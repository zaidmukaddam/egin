//! Claude through the Messages API: the Claude API for `claude-*` models, Claude in Amazon Bedrock
//! (the bedrock-mantle endpoint) for `anthropic.claude-*` ones. egin keeps conversations in its
//! chat-completions shape; this module converts them on the way out and back.
//!
//! An assistant turn is stored with the content blocks Claude returned (`anthropic_content`) and
//! sent back exactly as it came, so signed thinking blocks and server-tool results survive replay.

use super::{Endpoint, Fatal, Limited, ModelInfo, Protocol, Reply, Usage};
use crate::App;
use anyhow::{bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

const API_VERSION: &str = "2026-10-21";
/// Server-tool turns that stop with `pause_turn` are resumed at most this many times.
const MAX_CONTINUATIONS: usize = 5;

pub fn is_claude(model: &str) -> bool {
    bare(model).starts_with("claude-")
}

/// The model id without Bedrock's prefixes: `us.anthropic.claude-x` and `anthropic.claude-x` are `claude-x`.
fn bare(model: &str) -> &str {
    model.rsplit_once("anthropic.").map_or(model, |(_, id)| id)
}

/// Context, prices and inputs. Neither endpoint lists prices, so they are kept here
/// (USD per million tokens, first-party rates; Bedrock's global endpoint charges the same).
pub fn info(model: &str) -> Option<ModelInfo> {
    let id = bare(model);
    let (input, cached, output) = match id {
        "claude-fable-5-2" | "claude-mythos-5-2" => (8.0, 0.20, 40.0),
        "claude-fable-5-1" | "claude-mythos-5-1" | "claude-fable-5" | "claude-mythos-5" => (10.0, 1.0, 50.0),
        "claude-opus-5-5" => (4.0, 0.20, 20.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => (5.0, 0.50, 25.0),
        "claude-sonnet-5-5" => (2.0, 0.10, 10.0),
        "claude-sonnet-5" => (2.0, 0.20, 10.0),
        "claude-sonnet-4-6" => (3.0, 0.30, 15.0),
        "claude-haiku-5-5" => (0.10, 0.01, 0.50),
        "claude-haiku-4-5" => (1.0, 0.10, 5.0),
        _ => return None,
    };
    let context = if id == "claude-haiku-4-5" { 200_000 } else { 1_000_000 };
    Some(ModelInfo { context, input, cached, output, inputs: vec!["image".into(), "document".into()] })
}

/// Models that take `thinking: {type: "adaptive"}` (Haiku 4.5 and older take a token budget instead,
/// so egin leaves thinking off there).
fn adaptive(id: &str) -> bool {
    !id.starts_with("claude-haiku-4") && !id.starts_with("claude-sonnet-4-5") && !id.starts_with("claude-opus-4-5")
}

/// Models whose refusals the Claude API can hand to another model inside the same call.
fn has_fallbacks(id: &str) -> bool {
    matches!(id, "claude-fable-5-2" | "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5")
}

/// `data:<mime>;base64,<data>` to an image block.
fn image(url: &str) -> Option<Value> {
    let rest = url.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    Some(json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}}))
}

fn user_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("text") => p["text"].as_str().filter(|t| !t.is_empty()).map(|t| json!({"type": "text", "text": t})),
                Some("image_url") => image(p["image_url"]["url"].as_str().unwrap_or("")),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// egin's messages to the Messages API's `system` and `messages`. Consecutive turns of one role
/// are merged, so a turn's tool results arrive in a single user message, as the API expects.
fn push(out: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last["role"] == role => last["content"].as_array_mut().unwrap().extend(blocks),
        _ => out.push(json!({"role": role, "content": blocks})),
    }
}

/// Put each image into the tool_result of the call that read it (`owners`, one call id per image),
/// in the last message. Anything after the results would end the assistant turn, which breaks a
/// turn that is still waiting on a server tool.
fn attach_images(out: &mut [Value], images: Vec<Value>, owners: &[Value]) {
    let Some(blocks) = out.last_mut().and_then(|l| l["content"].as_array_mut()) else { return };
    for (img, owner) in images.into_iter().zip(owners) {
        if let Some(r) = blocks.iter_mut().find(|b| b["type"] == "tool_result" && b["tool_use_id"] == *owner) {
            let content = r["content"].take();
            r["content"] = match content {
                Value::Array(mut parts) => {
                    parts.push(img);
                    json!(parts)
                }
                text => json!([{"type": "text", "text": text}, img]),
            };
        }
    }
}

fn convert(msgs: &[Value]) -> (String, Vec<Value>) {
    let mut system = vec![];
    let mut out: Vec<Value> = vec![];
    for m in msgs {
        match m["role"].as_str() {
            Some("system") => system.push(m["content"].as_str().unwrap_or("").to_string()),
            Some("user") if m["for_calls"].is_array() => {
                let images = m["content"].as_array().into_iter().flatten().filter_map(|p| image(p["image_url"]["url"].as_str().unwrap_or(""))).collect();
                attach_images(&mut out, images, m["for_calls"].as_array().unwrap());
            }
            Some("user") => push(&mut out, "user", user_parts(&m["content"])),
            Some("tool") => {
                let text = m["content"].as_str().filter(|t| !t.is_empty()).unwrap_or("(no output)");
                push(&mut out, "user", vec![json!({"type": "tool_result", "tool_use_id": m["tool_call_id"], "content": text})]);
            }
            Some("assistant") => {
                if let Some(blocks) = m["anthropic_content"].as_array() {
                    push(&mut out, "assistant", blocks.clone());
                    continue;
                }
                // A turn another model wrote earlier in the session.
                let mut blocks = vec![];
                if let Some(t) = m["content"].as_str().filter(|t| !t.trim().is_empty()) {
                    blocks.push(json!({"type": "text", "text": t}));
                }
                for c in m["tool_calls"].as_array().into_iter().flatten() {
                    let input: Value = serde_json::from_str(c["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                    blocks.push(json!({"type": "tool_use", "id": c["id"], "name": c["function"]["name"], "input": input}));
                }
                push(&mut out, "assistant", blocks);
            }
            _ => {}
        }
    }
    (system.join("\n\n"), out)
}

/// egin's tool declarations to Messages API tools. The router's web tools become Claude's own
/// web search and web fetch, which the Claude API runs server-side.
fn tools(specs: &[Value], bedrock: bool) -> Result<Vec<Value>> {
    let mut out = vec![];
    for t in specs {
        match t["type"].as_str() {
            Some("function") => {
                let f = &t["function"];
                out.push(json!({"name": f["name"], "description": f["description"], "input_schema": f["parameters"]}));
            }
            Some("web_search") | Some("web_extract") if bedrock => {
                bail!("web = \"server\" needs the Claude API: Claude in Amazon Bedrock has no server-side web tools. Use web = \"client\".")
            }
            Some("web_search") => {
                let mut s = json!({"type": "web_search_20260209", "name": "web_search"});
                if let Some(d) = t["include_domains"].as_array() {
                    s["allowed_domains"] = json!(d);
                } else if let Some(d) = t["exclude_domains"].as_array() {
                    s["blocked_domains"] = json!(d);
                }
                out.push(s);
            }
            Some("web_extract") => out.push(json!({"type": "web_fetch_20260209", "name": "web_fetch"})),
            _ => {}
        }
    }
    Ok(out)
}

/// The Messages API request for a chat-completions body built by the run loop.
fn request(body: &Value, bedrock: bool) -> Result<(Value, Vec<&'static str>)> {
    let model = body["model"].as_str().unwrap_or("");
    let id = bare(model);
    let (system, messages) = convert(body["messages"].as_array().map(Vec::as_slice).unwrap_or(&[]));
    let mut req = json!({
        "model": model,
        "max_tokens": body["max_tokens"].as_u64().unwrap_or(64_000),
        "stream": true,
        "messages": messages,
        // Caches the longest stable prefix: system prompt, tools, and the history so far.
        "cache_control": {"type": "ephemeral"},
    });
    if !system.is_empty() {
        req["system"] = json!(system);
    }
    // The latest code execution container, while it lives: a turn held for client tool results
    // resumes its server tools there.
    let latest = body["messages"].as_array().into_iter().flatten().rev().find_map(|m| m.get("anthropic_container")).cloned();
    if let Some(c) = latest {
        let alive = c["expires_at"].as_str().and_then(|e| chrono::DateTime::parse_from_rfc3339(e).ok()).is_none_or(|e| e > chrono::Utc::now());
        if let (true, Some(id)) = (alive, c["id"].as_str()) {
            req["container"] = json!(id);
        }
    }
    let mut betas = vec![];
    if adaptive(id) {
        // egin's context trimming rewrites old tool results. Thinking blocks are bound to the history
        // they were written against, so a trimmed history drops them instead of failing the request.
        req["thinking"] = json!({"type": "adaptive", "block_binding": {"prefix_mismatch_behavior": "drop_block"}});
        betas.push("thinking-binding-controls-2026-08-01");
    }
    if let Some(e) = body["reasoning_effort"].as_str() {
        req["output_config"] = json!({"effort": e});
    }
    let tools = tools(body["tools"].as_array().map(Vec::as_slice).unwrap_or(&[]), bedrock)?;
    if !tools.is_empty() {
        req["tools"] = json!(tools);
        if body["tool_choice"] == "none" {
            req["tool_choice"] = json!({"type": "none"});
        } else if body["parallel_tool_calls"] == json!(false) {
            req["tool_choice"] = json!({"type": "auto", "disable_parallel_tool_use": true});
        }
    }
    if !bedrock && has_fallbacks(id) {
        // A refusal is retried on a model chosen by the refusal's category, in the same call.
        req["fallbacks"] = json!("default");
        betas.push("server-side-fallback-2026-07-01");
    }
    Ok((req, betas))
}

pub(super) async fn chat_once(app: &App, ep: &Endpoint, body: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Reply> {
    let (mut req, betas) = request(body, ep.protocol == Protocol::Bedrock).map_err(|e| Fatal(format!("{e:#}")))?;
    let (url, key) = (format!("{}/v1/messages", ep.url), ep.key.clone());
    let mut blocks: Vec<Value> = vec![];
    let mut usage = Usage::default();
    let mut container = None;
    for _ in 0..=MAX_CONTINUATIONS {
        let turn =
            if ep.url.contains("bedrock-runtime") { invoke(app, ep, &betas, &req, on).await? } else { stream(app, &url, &key, &betas, &req, on).await? };
        usage.prompt += turn.usage.prompt;
        usage.cached += turn.usage.cached;
        usage.completion += turn.usage.completion;
        blocks.extend(turn.blocks);
        container = turn.container.or(container);
        match turn.stop.as_str() {
            // A server-tool loop hit its iteration limit: send the turn back, in the same container,
            // and it resumes.
            "pause_turn" => {
                if let Some(id) = container.as_ref().and_then(|c| c["id"].as_str()) {
                    req["container"] = json!(id);
                }
                let msgs = req["messages"].as_array_mut().unwrap();
                match msgs.last_mut() {
                    Some(last) if last["role"] == "assistant" => last["content"] = json!(blocks),
                    _ => msgs.push(json!({"role": "assistant", "content": blocks})),
                }
            }
            "refusal" => {
                let why = turn.refusal.unwrap_or_else(|| "no category given".into());
                let message = json!({"role": "assistant", "content": format!("[The model declined this request ({why}).]")});
                return Ok(Reply { message, usage, web_results: None });
            }
            _ => {
                let mut message = assistant(blocks);
                if let Some(c) = container {
                    message["anthropic_container"] = c;
                }
                return Ok(Reply { message, usage, web_results: None });
            }
        }
    }
    bail!("the model paused {MAX_CONTINUATIONS} times in one turn")
}

/// The stored assistant message: text and tool calls in egin's shape, the raw blocks beside them.
fn assistant(blocks: Vec<Value>) -> Value {
    let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("");
    let calls: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| json!({"id": b["id"], "type": "function", "function": {"name": b["name"], "arguments": b["input"].to_string()}}))
        .collect();
    let mut m = json!({"role": "assistant", "content": text, "anthropic_content": blocks});
    if !calls.is_empty() {
        m["tool_calls"] = json!(calls);
    }
    m
}

struct Turn {
    blocks: Vec<Value>,
    usage: Usage,
    stop: String,
    refusal: Option<String>,
    /// The code execution container this turn ran in (`{id, expires_at}`), if any.
    container: Option<Value>,
}

/// One turn through Bedrock's InvokeModel, for Claude models served only by an inference profile.
// ponytail: the whole reply arrives at once; InvokeModelWithResponseStream frames events in AWS's
// binary event stream, so add a decoder for it if these models need live tokens.
async fn invoke(app: &App, ep: &Endpoint, betas: &[&str], req: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Turn> {
    let mut req = req.clone();
    let o = req.as_object_mut().unwrap();
    o.remove("model");
    o.remove("stream");
    o.insert("anthropic_version".into(), json!("bedrock-2023-05-31"));
    if !betas.is_empty() {
        o.insert("anthropic_beta".into(), json!(betas));
    }
    let resp = app.http.post(format!("{}/model/{}/invoke", ep.url, ep.model)).bearer_auth(&ep.key).json(&req).send().await?;
    let status = resp.status();
    let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
    let text = resp.text().await?;
    if !status.is_success() {
        let msg = format!("model call failed: {status}: {text}");
        if status.as_u16() == 429 {
            return Err(Limited(msg, retry_after).into());
        }
        if super::fatal_status(status) {
            return Err(Fatal(msg).into());
        }
        bail!(msg);
    }
    let v: Value = serde_json::from_str(&text)?;
    let u = &v["usage"];
    let read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let prompt = u["input_tokens"].as_u64().unwrap_or(0) + read + u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let blocks: Vec<Value> = v["content"].as_array().cloned().unwrap_or_default();
    for b in &blocks {
        match (b["type"].as_str(), b["text"].as_str(), b["thinking"].as_str()) {
            (Some("text"), Some(s), _) if !s.is_empty() => on("token", json!({"text": s})),
            (Some("thinking"), _, Some(s)) if !s.is_empty() => on("reasoning", json!({"text": s})),
            _ => {}
        }
    }
    Ok(Turn {
        blocks,
        usage: Usage { prompt, cached: read, completion: u["output_tokens"].as_u64().unwrap_or(0), reasoning: 0 },
        stop: v["stop_reason"].as_str().unwrap_or("").to_string(),
        refusal: v["stop_details"]["category"].as_str().map(String::from),
        container: v["container"].is_object().then(|| v["container"].clone()),
    })
}

async fn stream(app: &App, url: &str, key: &str, betas: &[&str], req: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Turn> {
    let mut r = app.http.post(url).header("x-api-key", key).header("anthropic-version", API_VERSION).json(req);
    if !betas.is_empty() {
        r = r.header("anthropic-beta", betas.join(","));
    }
    let resp = r.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let msg = format!("model call failed: {status}: {}", resp.text().await.unwrap_or_default());
        // 529 is "overloaded": retry like a server error.
        if status.as_u16() == 429 {
            return Err(Limited(msg, retry_after).into());
        }
        if super::fatal_status(status) {
            return Err(Fatal(msg).into());
        }
        bail!(msg);
    }

    let mut t = Turn { blocks: vec![], usage: Usage::default(), stop: String::new(), refusal: None, container: None };
    let mut json_bufs: Vec<String> = vec![];
    let (mut buf, mut done) = (Vec::<u8>::new(), false);
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let Some(data) = line.trim_end().strip_prefix("data:").map(str::trim) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            let i = v["index"].as_u64().unwrap_or(0) as usize;
            match v["type"].as_str().unwrap_or("") {
                "message_start" => {
                    if v["message"]["container"].is_object() {
                        t.container = Some(v["message"]["container"].clone());
                    }
                    let u = &v["message"]["usage"];
                    let read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    let written = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                    t.usage.prompt = u["input_tokens"].as_u64().unwrap_or(0) + read + written;
                    t.usage.cached = read;
                }
                "content_block_start" => {
                    while t.blocks.len() <= i {
                        t.blocks.push(Value::Null);
                        json_bufs.push(String::new());
                    }
                    t.blocks[i] = v["content_block"].clone();
                }
                "content_block_delta" if i < t.blocks.len() => {
                    let d = &v["delta"];
                    let b = &mut t.blocks[i];
                    match d["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            let s = d["text"].as_str().unwrap_or("");
                            b["text"] = json!(format!("{}{s}", b["text"].as_str().unwrap_or("")));
                            if !s.is_empty() {
                                on("token", json!({"text": s}));
                            }
                        }
                        "thinking_delta" => {
                            let s = d["thinking"].as_str().unwrap_or("");
                            b["thinking"] = json!(format!("{}{s}", b["thinking"].as_str().unwrap_or("")));
                            if !s.is_empty() {
                                on("reasoning", json!({"text": s}));
                            }
                        }
                        "signature_delta" => b["signature"] = d["signature"].clone(),
                        "input_json_delta" => json_bufs[i].push_str(d["partial_json"].as_str().unwrap_or("")),
                        "citations_delta" => match b["citations"].as_array_mut() {
                            Some(c) => c.push(d["citation"].clone()),
                            None => b["citations"] = json!([d["citation"]]),
                        },
                        _ => {}
                    }
                }
                "content_block_stop" if i < t.blocks.len() => {
                    let b = &mut t.blocks[i];
                    if matches!(b["type"].as_str(), Some("tool_use") | Some("server_tool_use")) {
                        let raw = std::mem::take(&mut json_bufs[i]);
                        b["input"] = if raw.trim().is_empty() { json!({}) } else { serde_json::from_str(&raw).unwrap_or(json!({})) };
                        if b["type"] == "server_tool_use" {
                            on(b["name"].as_str().unwrap_or("server_tool"), b["input"].clone());
                        }
                    }
                }
                "message_delta" => {
                    if v["delta"]["container"].is_object() {
                        t.container = Some(v["delta"]["container"].clone());
                    }
                    if let Some(s) = v["delta"]["stop_reason"].as_str() {
                        t.stop = s.to_string();
                    }
                    if let Some(c) = v["delta"]["stop_details"]["category"].as_str() {
                        t.refusal = Some(c.to_string());
                    }
                    if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                        t.usage.completion = n;
                    }
                }
                "message_stop" => done = true,
                "error" => {
                    let e = &v["error"];
                    // Overloaded mid-stream: the caller retries the whole call.
                    bail!("model stream error: {}: {}", e["type"].as_str().unwrap_or("error"), e["message"].as_str().unwrap_or(""));
                }
                _ => {}
            }
        }
    }
    if !done {
        bail!("model stream ended early");
    }
    t.blocks.retain(|b| !b.is_null());
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_become_messages_api_requests() {
        let body = json!({
            "model": "claude-opus-5-5",
            "reasoning_effort": "low",
            "parallel_tool_calls": false,
            "tools": [
                {"type": "function", "function": {"name": "bash", "description": "Run", "parameters": {"type": "object"}}},
                {"type": "web_search", "count": 5},
                {"type": "web_extract", "formats": ["markdown"]},
            ],
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "list files"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]},
                {"role": "tool", "tool_call_id": "t1", "content": "a.txt"},
                {"role": "user", "content": [{"type": "text", "text": "Images:"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}}], "for_calls": ["t1"]},
                {"role": "user", "content": "and this?"},
            ],
        });
        let (req, betas) = request(&body, false).unwrap();
        assert_eq!(req["system"], "Be brief.");
        let m = req["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3, "the tool result and the next user text share one user message");
        assert_eq!(m[1]["content"][0]["input"]["command"], "ls");
        let result = &m[2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["content"][0]["text"], "a.txt");
        assert_eq!(
            result["content"][1]["source"],
            json!({"type": "base64", "media_type": "image/png", "data": "QUJD"}),
            "the image rides inside its tool_result"
        );
        assert_eq!(m[2]["content"][1]["text"], "and this?");
        assert_eq!(req["tools"][1]["type"], "web_search_20260209");
        assert_eq!(req["tools"][2]["type"], "web_fetch_20260209");
        assert_eq!(req["tool_choice"]["disable_parallel_tool_use"], true);
        assert_eq!(req["output_config"]["effort"], "low");
        assert_eq!(req["fallbacks"], "default");
        assert!(betas.contains(&"thinking-binding-controls-2026-08-01") && betas.contains(&"server-side-fallback-2026-07-01"));
    }

    #[test]
    fn a_stored_claude_turn_is_echoed_unchanged() {
        let blocks = json!([{"type": "thinking", "thinking": "", "signature": "sig"}, {"type": "text", "text": "hi"}]);
        let body = json!({"model": "claude-opus-5-5", "messages": [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi", "anthropic_content": blocks},
            {"role": "user", "content": "again"},
        ]});
        assert_eq!(request(&body, false).unwrap().0["messages"][1]["content"], blocks);
    }

    #[test]
    fn bedrock_has_no_fallbacks_or_server_web_tools() {
        let body = json!({"model": "anthropic.claude-opus-5-5", "messages": [{"role": "user", "content": "hi"}]});
        let (req, betas) = request(&body, true).unwrap();
        assert!(req.get("fallbacks").is_none() && !betas.contains(&"server-side-fallback-2026-07-01"));
        let web = json!({"model": "anthropic.claude-opus-5-5", "messages": [], "tools": [{"type": "web_search"}]});
        assert!(request(&web, true).is_err());
    }

    #[test]
    fn prices_and_context_for_both_endpoints() {
        assert_eq!(info("anthropic.claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(info("claude-haiku-4-5").unwrap().context, 200_000);
        assert!(info("claude-unknown").is_none());
    }
}
