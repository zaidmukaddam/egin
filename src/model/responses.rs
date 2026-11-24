//! OpenAI's Responses API, for models that take function tools only there (OpenAI's GPT models on
//! Amazon Bedrock). egin keeps conversations in its chat-completions shape; this module converts
//! them on the way out and back.
//!
//! An assistant turn is stored with the output items the model returned (`openai_output`) and sent
//! back exactly as it came, so encrypted reasoning survives the tool calls that follow it.

use super::{Endpoint, Fatal, Limited, Reply, Usage};
use crate::App;
use anyhow::{bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn user_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("text") => Some(json!({"type": "input_text", "text": p["text"]})),
                Some("image_url") => Some(json!({"type": "input_image", "image_url": p["image_url"]["url"]})),
                _ => None,
            })
            .collect(),
        other => vec![json!({"type": "input_text", "text": text_of(other)})],
    }
}

fn request(body: &Value) -> Value {
    let (mut system, mut input) = (vec![], vec![]);
    for m in body["messages"].as_array().into_iter().flatten() {
        match m["role"].as_str().unwrap_or("") {
            "system" => system.push(text_of(&m["content"])),
            "tool" => input.push(json!({"type": "function_call_output", "call_id": m["tool_call_id"], "output": text_of(&m["content"])})),
            "assistant" => match m["openai_output"].as_array() {
                Some(items) => input.extend(items.iter().cloned()),
                None => {
                    let text = text_of(&m["content"]);
                    if !text.is_empty() {
                        input.push(json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}));
                    }
                    for c in m["tool_calls"].as_array().into_iter().flatten() {
                        input
                            .push(json!({"type": "function_call", "call_id": c["id"], "name": c["function"]["name"], "arguments": c["function"]["arguments"]}));
                    }
                }
            },
            _ => input.push(json!({"role": "user", "content": user_parts(&m["content"])})),
        }
    }
    let mut req = json!({
        "model": body["model"],
        "input": input,
        "stream": true,
        // Nothing is kept on the provider's side, so reasoning comes back encrypted to be replayed.
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if !system.is_empty() {
        req["instructions"] = json!(system.join("\n\n"));
    }
    if let Some(n) = body["max_tokens"].as_u64() {
        req["max_output_tokens"] = json!(n);
    }
    if let Some(e) = body["reasoning_effort"].as_str() {
        req["reasoning"] = json!({"effort": e});
    }
    // A wrap-up call (tool_choice "none") sends no tools; tool-call history needs none declared.
    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "function" && body["tool_choice"] != "none")
        .map(|t| json!({"type": "function", "name": t["function"]["name"], "description": t["function"]["description"], "parameters": t["function"]["parameters"], "strict": false}))
        .collect();
    if !tools.is_empty() {
        req["tools"] = json!(tools);
        if body["parallel_tool_calls"] == json!(false) {
            req["parallel_tool_calls"] = json!(false);
        }
    }
    req
}

/// The stored assistant message: text and tool calls in egin's shape, the raw output items beside them.
fn assistant(output: &[Value]) -> Value {
    let text: String = output
        .iter()
        .filter(|i| i["type"] == "message")
        .flat_map(|i| i["content"].as_array().into_iter().flatten())
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let calls: Vec<Value> = output
        .iter()
        .filter(|i| i["type"] == "function_call")
        .map(|i| json!({"id": i["call_id"], "type": "function", "function": {"name": i["name"], "arguments": i["arguments"]}}))
        .collect();
    let mut m = json!({"role": "assistant", "content": text, "openai_output": output});
    if !calls.is_empty() {
        m["tool_calls"] = json!(calls);
    }
    m
}

pub(super) async fn chat_once(app: &App, ep: &Endpoint, body: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Reply> {
    let resp = app.http.post(format!("{}/responses", ep.url)).bearer_auth(&ep.key).json(&request(body)).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let msg = format!("model call failed: {status}: {}", resp.text().await.unwrap_or_default());
        if status.as_u16() == 429 {
            return Err(Limited(msg, retry_after).into());
        }
        if super::fatal_status(status) {
            return Err(Fatal(msg).into());
        }
        bail!(msg);
    }

    let mut done: Option<Value> = None;
    let mut buf: Vec<u8> = vec![];
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let raw: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let Some(data) = line.trim_end().strip_prefix("data:").map(str::trim) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            let delta = v["delta"].as_str().filter(|s| !s.is_empty());
            match (v["type"].as_str().unwrap_or(""), delta) {
                ("response.output_text.delta", Some(s)) => on("token", json!({"text": s})),
                ("response.reasoning_summary_text.delta", Some(s)) => on("reasoning", json!({"text": s})),
                // `incomplete` is a reply cut at the output limit: what arrived still counts.
                ("response.completed" | "response.incomplete", _) => done = Some(v["response"].clone()),
                ("response.failed", _) => bail!("model stream error: {}", v["response"]["error"]),
                ("error", _) => bail!("model stream error: {v}"),
                _ => {}
            }
        }
    }
    let Some(r) = done else { bail!("model stream ended early") };
    let u = &r["usage"];
    let usage = Usage {
        prompt: u["input_tokens"].as_u64().unwrap_or(0),
        cached: u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        completion: u["output_tokens"].as_u64().unwrap_or(0),
        reasoning: u["output_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0),
    };
    Ok(Reply { message: assistant(r["output"].as_array().map(Vec::as_slice).unwrap_or(&[])), usage, web_results: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_become_responses_requests_and_stored_output_is_echoed() {
        let stored = json!([{"type": "reasoning", "id": "rs_1", "encrypted_content": "x"}, {"type": "function_call", "call_id": "c2", "name": "bash", "arguments": "{}"}]);
        let body = json!({
            "model": "openai.gpt-6-luna",
            "max_tokens": 100,
            "reasoning_effort": "low",
            "tools": [{"type": "function", "function": {"name": "bash", "description": "run", "parameters": {"type": "object"}}}, {"type": "web_search"}],
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": [{"type": "text", "text": "hi"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}}]},
                {"role": "assistant", "content": "ok", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "a.txt"},
                {"role": "assistant", "content": "", "openai_output": stored, "tool_calls": [{"id": "c2"}]},
                {"role": "tool", "tool_call_id": "c2", "content": "done"},
            ],
        });
        let req = request(&body);
        assert_eq!(req["instructions"], "be brief");
        assert_eq!((&req["max_output_tokens"], &req["reasoning"]["effort"], &req["store"]), (&json!(100), &json!("low"), &json!(false)));
        assert_eq!(req["tools"], json!([{"type": "function", "name": "bash", "description": "run", "parameters": {"type": "object"}, "strict": false}]));
        let input = req["input"].as_array().unwrap();
        assert_eq!(input[0]["content"], json!([{"type": "input_text", "text": "hi"}, {"type": "input_image", "image_url": "data:image/png;base64,AA"}]));
        assert_eq!(input[1], json!({"role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}));
        assert_eq!(input[2], json!({"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"}));
        assert_eq!(input[3], json!({"type": "function_call_output", "call_id": "c1", "output": "a.txt"}));
        assert_eq!((&input[4], &input[5]), (&stored[0], &stored[1]), "a stored turn goes back as it came");
        assert_eq!(input[6]["call_id"], "c2");

        let mut wrap_up = body.clone();
        wrap_up["tool_choice"] = json!("none");
        assert!(request(&wrap_up).get("tools").is_none(), "a wrap-up call declares no tools");
    }

    #[test]
    fn output_items_become_an_assistant_message() {
        let out = json!([
            {"type": "reasoning", "id": "rs_1"},
            {"type": "message", "content": [{"type": "output_text", "text": "Listing."}]},
            {"type": "function_call", "call_id": "c1", "name": "bash", "arguments": "{\"command\":\"ls\"}"},
        ]);
        let m = assistant(out.as_array().unwrap());
        assert_eq!((m["content"].as_str(), &m["openai_output"]), (Some("Listing."), &out));
        assert_eq!(m["tool_calls"], json!([{"id": "c1", "type": "function", "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}}]));
    }
}
