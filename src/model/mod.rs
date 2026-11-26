//! Model calls, always streaming. `providers` maps a model to its provider, key and endpoint. Each
//! provider speaks one of three wire protocols, one file each:
//! - `chat_completions`: the You.com router, OpenAI's older models, the AI Gateway, Gemini, Bedrock's
//!   non-Claude models, and anything OpenAI-compatible (OpenRouter, Groq, vLLM, Ollama, LiteLLM).
//! - `claude`: the Messages API, on the Claude API and on Amazon Bedrock.
//! - `responses`: OpenAI's Responses API, for GPT models on OpenAI and on Bedrock.

mod chat_completions;
mod claude;
mod providers;
mod responses;

pub use claude::{info as claude_info, is_claude};
pub use providers::{has_key, key_envs, missing_key, reasoning_efforts, route, Endpoint};

use crate::App;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

/// Prices are USD per million tokens.
#[derive(Clone, Debug, Default)]
pub struct ModelInfo {
    pub context: u64,
    pub input: f64,
    pub cached: f64,
    pub output: f64,
    /// What the model accepts besides text, from the router: "image", "document", "audio", "video".
    pub inputs: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub prompt: u64,
    pub cached: u64,
    pub completion: u64,
    pub reasoning: u64,
}

pub struct Reply {
    /// The assistant message exactly as it must be echoed back (reasoning_content included).
    pub message: Value,
    pub usage: Usage,
    /// Sources from router-side web_search / web_extract, when they ran.
    pub web_results: Option<Value>,
}

/// The wire protocol a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Protocol {
    /// `POST {url}/chat/completions` with a bearer key.
    OpenAi,
    /// `POST {url}/v1/messages` with `x-api-key`: the Claude API.
    Anthropic,
    /// The same on Claude in Amazon Bedrock, which has no server-side tools or fallbacks.
    Bedrock,
    /// `POST {url}/responses` with a bearer key: OpenAI's Responses API.
    Responses,
}

pub fn cost(info: &ModelInfo, u: &Usage) -> f64 {
    (u.prompt.saturating_sub(u.cached) as f64 * info.input + u.cached as f64 * info.cached + u.completion as f64 * info.output) / 1e6
}

pub async fn list_models(app: &App) -> Result<Value> {
    let r = app.http.get(format!("{}/models", app.base_url)).bearer_auth(&app.key).send().await?;
    let (status, text) = (r.status(), r.text().await?);
    if !status.is_success() {
        bail!("models: {status}: {text}");
    }
    Ok(serde_json::from_str(&text)?)
}

pub async fn fetch_models(app: &App) -> Result<HashMap<String, ModelInfo>> {
    let v = list_models(app).await?;
    // The router sends prices as strings.
    let price = |m: &Value, k: &str| {
        let p = &m["pricing"][k];
        p.as_str().and_then(|s| s.parse().ok()).or(p.as_f64()).unwrap_or(0.0)
    };
    let mut out = HashMap::new();
    for m in v["data"].as_array().into_iter().flatten() {
        let info = ModelInfo {
            context: m["max_input_tokens"].as_u64().or(m["context_size"].as_u64()).unwrap_or(0),
            input: price(m, "input_per_mtok"),
            cached: price(m, "cached_input_per_mtok"),
            output: price(m, "output_per_mtok"),
            inputs: m["capabilities"]["input"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(String::from)).collect(),
        };
        out.insert(m["id"].as_str().unwrap_or("").to_string(), info);
    }
    Ok(out)
}

/// The AI Gateway's model list. It needs no key; prices come as USD per token in strings.
pub async fn fetch_gateway_models(app: &App) -> Result<HashMap<String, ModelInfo>> {
    let v: Value = app.http.get("https://ai-gateway.vercel.sh/v1/models").send().await?.json().await?;
    let per_m = |p: &Value| p.as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) * 1e6;
    let mut out = HashMap::new();
    for m in v["data"].as_array().into_iter().flatten() {
        let mut inputs: Vec<String> = m["modalities"]["input"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
        if inputs.iter().any(|x| x == "pdf") {
            inputs.push("document".into());
        }
        let info = ModelInfo {
            context: m["context_window"].as_u64().unwrap_or(0),
            input: per_m(&m["pricing"]["input"]),
            // Without a cache price, cached tokens bill as ordinary input.
            cached: if m["pricing"]["input_cache_read"].is_string() { per_m(&m["pricing"]["input_cache_read"]) } else { per_m(&m["pricing"]["input"]) },
            output: per_m(&m["pricing"]["output"]),
            inputs,
        };
        out.insert(m["id"].as_str().unwrap_or("").to_string(), info);
    }
    Ok(out)
}

/// Whether a failed status is the request's own fault, so retrying cannot help. 408, 409 and 425 are 4xx that say
/// "try again": a router 408 after a hung call once ended a DeepSWE run that a retry would have saved.
fn fatal_status(status: reqwest::StatusCode) -> bool {
    status.is_client_error() && !matches!(status.as_u16(), 408 | 409 | 425)
}

/// A 4xx the request itself caused: retrying would not help.
#[derive(Debug)]
struct Fatal(String);
impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Fatal {}

/// Rate limited; `Retry-After` seconds when the router sent one.
#[derive(Debug)]
struct Limited(String, Option<u64>);
impl std::fmt::Display for Limited {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Limited {}

/// Process-wide circuit breaker for the router's rate limit. Per-request backoff alone lets many sessions
/// that were limited together retry together, a new spike. Instead a 429 pauses every model call in the
/// process, new or retried; consecutive 429s lengthen the pause (2 s up to 60 s, or Retry-After if longer),
/// any success resets it, and calls released when a pause ends are spread over the next half pause.
pub(crate) mod breaker {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::time::Instant;

    static UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
    static STRIKES: AtomicU32 = AtomicU32::new(0);

    /// 0..1, different per call: enough to spread callers out, no RNG dependency.
    fn unit() -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_nanos() as f64 / 1e9
    }

    /// Wait out the current pause, if any, then a random part of it so released callers trickle in.
    pub async fn gate() {
        loop {
            let until = *UNTIL.lock().unwrap();
            match until {
                Some(t) if t > Instant::now() => {
                    let left = t - Instant::now();
                    tokio::time::sleep(left + left.mul_f64(0.5 * unit())).await;
                }
                _ => return,
            }
        }
    }

    /// A 429: open the breaker for the process.
    pub fn trip(retry_after: Option<u64>) {
        let n = STRIKES.fetch_add(1, Ordering::Relaxed).min(5);
        let pause = Duration::from_secs((2u64 << n).min(60)).max(Duration::from_secs(retry_after.unwrap_or(0)));
        let t = Instant::now() + pause;
        let mut until = UNTIL.lock().unwrap();
        if until.is_none_or(|u| u < t) {
            *until = Some(t);
        }
    }

    pub fn ok() {
        STRIKES.store(0, Ordering::Relaxed);
    }

    /// Held by unit tests that make model calls and by the breaker's own test, whose paused clock leaves a pause
    /// that real-time callers would wait out.
    #[cfg(test)]
    pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[cfg(test)]
    #[tokio::test(start_paused = true)]
    async fn pauses_grow_with_consecutive_429s_and_reset_on_success() {
        let _one = TEST_LOCK.lock().await;
        let waited = |t: Instant| Instant::now() - t;
        let t = Instant::now();
        gate().await;
        assert_eq!(waited(t), Duration::ZERO, "closed: no wait");
        trip(None);
        let t = Instant::now();
        gate().await;
        assert!((Duration::from_secs(2)..=Duration::from_secs(3)).contains(&waited(t)), "first 429: 2 s plus up to half again");
        trip(None);
        let t = Instant::now();
        gate().await;
        assert!(waited(t) >= Duration::from_secs(4), "second in a row: 4 s");
        ok();
        trip(Some(30));
        let t = Instant::now();
        gate().await;
        assert!(waited(t) >= Duration::from_secs(30), "after a reset Retry-After still wins over the 2 s step");
        let t = Instant::now();
        gate().await;
        assert_eq!(waited(t), Duration::ZERO, "closed again once the pause has passed");
        *UNTIL.lock().unwrap() = None;
        ok();
    }
}

/// How long a model call keeps retrying network errors, 5xx and 429s.
const RETRY_FOR: Duration = Duration::from_secs(600);

/// One model call. `on` receives ("token" | "reasoning" | router event name, data) as the stream arrives.
/// Network errors and 5xx are retried with exponential backoff; 429s trip the process-wide breaker above
/// and are retried once it closes. Both for up to `RETRY_FOR`: a run can outlast a provider outage
/// ("Upstream capacity is temporarily unavailable" for minutes) more cheaply than it can be rerun.
// ponytail: one breaker for every provider, so a 429 from one pauses calls to the others; key it by
// endpoint if agents mix providers under load.
pub async fn chat(app: &App, ep: &Endpoint, body: &Value, on: &mut (dyn FnMut(&str, Value) + Send)) -> Result<Reply> {
    let mut body = body.clone();
    body["model"] = json!(ep.model);
    let mut attempt = 0u32;
    let first = std::time::Instant::now();
    loop {
        breaker::gate().await;
        let result = match ep.protocol {
            Protocol::OpenAi => chat_completions::chat_once(app, ep, &body, on).await,
            Protocol::Anthropic | Protocol::Bedrock => claude::chat_once(app, ep, &body, on).await,
            Protocol::Responses => responses::chat_once(app, ep, &body, on).await,
        };
        match result {
            Ok(r) => {
                breaker::ok();
                return Ok(r);
            }
            Err(e) if e.is::<Fatal>() || first.elapsed() >= RETRY_FOR => return Err(e),
            Err(e) => {
                if let Some(l) = e.downcast_ref::<Limited>() {
                    breaker::trip(l.1);
                } else {
                    tokio::time::sleep(Duration::from_secs((1u64 << attempt.min(6)).min(45))).await;
                }
                attempt += 1;
            }
        }
    }
}
