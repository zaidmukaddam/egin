//! Chat completions, the protocol most providers speak: the You.com router, OpenAI's older models, the
//! AI Gateway, Gemini, Bedrock's non-Claude models, and anything OpenAI-compatible (OpenRouter, Groq,
//! vLLM, Ollama, LiteLLM) through `provider = "openai"` with a `base_url`.

use super::{Endpoint, Fatal, Limited, Reply, Usage};
use crate::App;
use anyhow::{bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

/// What one chat-completions provider needs changed in an otherwise standard request.
fn quirks(ep: &Endpoint, body: &mut Value) {
    // MiniMax M3 reads a missing max_tokens as its whole 524,288-token output budget, which leaves ~75k of its
    // 600k window for input, so long sessions fail. 131072 is MiniMax's recommended cap (per You.com support).
    // ponytail: a table of one; read the router's max output instead once /v1/models reports it.
    if body["max_tokens"].is_null() && ep.model.starts_with("minimax-m3") {
        body["max_tokens"] = json!(131_072);
    }
    // OpenAI deprecated max_tokens, and its reasoning models reject it.
    if ep.provider == "openai" {
        if let Some(n) = body.as_object_mut().and_then(|o| o.remove("max_tokens")) {
            body["max_completion_tokens"] = n;
        }
    }
    // A wrap-up call (tool_choice "none") sends no tools at all: some models behind the router 400 on
    // "none", and every chat-completions provider accepts tool-call history without tools declared.
    if body["tool_choice"] == "none" {
        if let Some(o) = body.as_object_mut() {
            o.remove("tools");
            o.remove("tool_choice");
        }
        return;
    }
    // The router's server-side web tools have no meaning elsewhere. On the AI Gateway, web search
    // becomes one of its own server tools; other providers have none, so the tools are dropped.
    // get_mut, not body["tools"]: indexing a JSON object mutably inserts a null "tools" key.
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else { return };
    let router = |t: &Value| matches!(t["type"].as_str(), Some("web_search") | Some("web_extract"));
    if ep.provider == "you" || !tools.iter().any(router) {
        return;
    }
    let search = tools.iter().find(|t| t["type"] == "web_search").cloned();
    tools.retain(|t| !router(t));
    if let (true, Some(s)) = (ep.provider == "gateway", search) {
        let mut config = json!({});
        if let Some(d) = s["include_domains"].as_array() {
            config["search_domain_filter"] = json!(d);
        } else if let Some(d) = s["exclude_domains"].as_array() {
            config["search_domain_filter"] = json!(d.iter().filter_map(Value::as_str).map(|d| format!("-{d}")).collect::<Vec<_>>());
        }
        if let Some(f) = s["freshness"].as_str().filter(|f| matches!(*f, "day" | "week" | "month" | "year")) {
            config["search_recency_filter"] = json!(f);
        }
        tools.push(json!({"type": "vercel:perplexity_search", "config": config}));
    }
    if tools.is_empty() {
        if let Some(o) = body.as_object_mut() {
            o.remove("tools");
            o.remove("parallel_tool_calls");
        }
    }
}

/// The router's content-policy refusal ("The request was blocked by the content policy.", a 400). It is
/// not a property of the request: the call that ended a DeepSWE run on a KaTeX test file went through on
/// each of three replays. So it is retried like a 5xx rather than ending the session.
fn policy_block(msg: &str) -> bool {
    msg.contains("blocked by the content policy")
}

pub(super) async fn chat_once(app: &App, ep: &Endpoint, body: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Reply> {
    // Turns Claude wrote earlier in the session carry their raw blocks; chat-completions endpoints
    // know nothing of them.
    let mut body = body.clone();
    for m in body["messages"].as_array_mut().into_iter().flatten() {
        if let Some(o) = m.as_object_mut() {
            for k in ["anthropic_content", "anthropic_container", "openai_output", "for_calls"] {
                o.remove(k);
            }
            // egin's own markers on the notes it adds (egin_deadline, egin_review, ...).
            o.retain(|k, _| !k.starts_with("egin_"));
        }
    }
    quirks(ep, &mut body);
    let resp = app.http.post(format!("{}/chat/completions", ep.url)).bearer_auth(&ep.key).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let msg = format!("model call failed: {status}: {}", resp.text().await.unwrap_or_default());
        if status.as_u16() == 429 {
            return Err(Limited(msg, retry_after).into());
        }
        if super::fatal_status(status) && !policy_block(&msg) {
            return Err(Fatal(msg).into());
        }
        bail!(msg);
    }

    let (mut content, mut reasoning) = (String::new(), String::new());
    let mut calls: Vec<Value> = vec![];
    let (mut usage, mut web_results, mut finished) = (Usage::default(), None, false);
    let mut event = String::new();
    let mut buf: Vec<u8> = vec![];
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end();
            if line.is_empty() {
                event.clear();
                continue;
            }
            if let Some(name) = line.strip_prefix("event:") {
                event = name.trim().to_string();
                continue;
            }
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            if data == "[DONE]" {
                finished = true;
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if !event.is_empty() && event != "message" {
                // Router progress events: web_search, web_extract.
                on(&event, v);
                continue;
            }
            if let Some(e) = v.get("error") {
                bail!("model stream error: {e}");
            }
            if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                usage = Usage {
                    prompt: u["prompt_tokens"].as_u64().unwrap_or(0),
                    cached: u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
                    completion: u["completion_tokens"].as_u64().unwrap_or(0),
                    reasoning: u["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
                };
            }
            if let Some(w) = v.get("web_results").filter(|w| w.is_object()) {
                web_results = Some(w.clone());
            }
            let choice = &v["choices"][0];
            if choice["finish_reason"].is_string() {
                finished = true;
            }
            let d = &choice["delta"];
            if let Some(s) = d["content"].as_str().filter(|s| !s.is_empty()) {
                content.push_str(s);
                on("token", json!({"text": s}));
            }
            // `reasoning_content` on most providers, `reasoning` on the AI Gateway.
            if let Some(s) = d["reasoning_content"].as_str().or(d["reasoning"].as_str()).filter(|s| !s.is_empty()) {
                reasoning.push_str(s);
                on("reasoning", json!({"text": s}));
            }
            for tc in d["tool_calls"].as_array().into_iter().flatten() {
                let i = tc["index"].as_u64().unwrap_or(0) as usize;
                while calls.len() <= i {
                    calls.push(json!({"id": "", "type": "function", "function": {"name": "", "arguments": ""}}));
                }
                let append = |slot: &mut Value, piece: &Value| {
                    if let Some(p) = piece.as_str() {
                        *slot = json!(format!("{}{p}", slot.as_str().unwrap_or("")));
                    }
                };
                if calls[i]["id"] == "" {
                    append(&mut calls[i]["id"], &tc["id"]);
                }
                append(&mut calls[i]["function"]["name"], &tc["function"]["name"]);
                append(&mut calls[i]["function"]["arguments"], &tc["function"]["arguments"]);
                // Fields a provider adds and expects back (Gemini's thought signatures), kept verbatim.
                for (k, v) in tc.as_object().into_iter().flatten() {
                    if !matches!(k.as_str(), "index" | "id" | "type" | "function") && !v.is_null() {
                        calls[i][k] = v.clone();
                    }
                }
            }
        }
    }
    if !finished {
        bail!("model stream ended early");
    }

    let mut message = json!({"role": "assistant", "content": content});
    if !reasoning.is_empty() {
        message["reasoning_content"] = json!(reasoning);
    }
    if !calls.is_empty() {
        for (i, c) in calls.iter_mut().enumerate() {
            if c["id"] == "" {
                c["id"] = json!(format!("call_{}_{i}", crate::new_id()));
            }
        }
        message["tool_calls"] = json!(calls);
    }
    Ok(Reply { message, usage, web_results })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Protocol;

    #[test]
    fn minimax_m3_gets_an_output_cap_unless_one_is_set() {
        let ep = |model: &str| Endpoint { provider: "you".into(), protocol: Protocol::OpenAi, url: String::new(), key: String::new(), model: model.into() };
        let mut body = json!({});
        quirks(&ep("minimax-m3"), &mut body);
        assert_eq!(body["max_tokens"], json!(131_072));
        let mut body = json!({"max_tokens": 32000});
        quirks(&ep("minimax-m3"), &mut body);
        assert_eq!(body["max_tokens"], json!(32000), "a configured cap wins");
        let mut body = json!({});
        quirks(&ep("kimi-k3"), &mut body);
        assert!(body.get("max_tokens").is_none(), "other models are left alone");
    }

    #[test]
    fn provider_quirks() {
        let ep =
            |provider: &str| Endpoint { provider: provider.into(), protocol: Protocol::OpenAi, url: String::new(), key: String::new(), model: String::new() };
        let mut body = json!({"max_tokens": 10, "tools": [{"type": "function"}, {"type": "web_search", "freshness": "week", "exclude_domains": ["a.com"]}, {"type": "web_extract"}]});
        quirks(&ep("openai"), &mut body);
        assert_eq!((body.get("max_tokens"), &body["max_completion_tokens"]), (None, &json!(10)));
        assert_eq!(body["tools"].as_array().unwrap().len(), 1, "router web tools dropped");

        let mut body = json!({"tools": [{"type": "web_search", "freshness": "week", "exclude_domains": ["a.com"]}, {"type": "web_extract"}]});
        quirks(&ep("gateway"), &mut body);
        assert_eq!(
            body["tools"],
            json!([{"type": "vercel:perplexity_search", "config": {"search_domain_filter": ["-a.com"], "search_recency_filter": "week"}}])
        );

        let mut body = json!({"tools": [{"type": "web_search"}], "parallel_tool_calls": true});
        quirks(&ep("gemini"), &mut body);
        assert!(body.get("tools").is_none() && body.get("parallel_tool_calls").is_none());

        let mut body = json!({"messages": []});
        quirks(&ep("you"), &mut body);
        assert_eq!(body, json!({"messages": []}), "a request without tools gains no tools key");
    }
}
