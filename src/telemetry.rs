//! Trace spans, exported over OTLP/HTTP JSON when an endpoint is configured.
//! The trace id is derived from the session id, so every run of a session shares one trace.

use crate::{app::new_id, App};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Somewhere spans go: an OTLP/HTTP traces endpoint and the headers it needs, from the `OTEL_*`
/// environment or from `instrumentation/<name>.toml` in the agent folder. `${VAR}` in the endpoint
/// or a header value reads the environment when a span is sent, so the file holds no secret.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exporter {
    /// The file's name, or `env` for the OTEL_* environment.
    #[serde(skip)]
    pub name: String,
    pub endpoint: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Exporter {
    /// The standard OpenTelemetry variables: the traces endpoint (or the base endpoint plus
    /// /v1/traces) and `key=value,…` headers, values percent-encoded as the spec says.
    pub fn from_env() -> Option<Exporter> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let endpoint = var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
            .or_else(|| var("OTEL_EXPORTER_OTLP_ENDPOINT").map(|e| format!("{}/v1/traces", e.trim_end_matches('/'))))?;
        let headers = var("OTEL_EXPORTER_OTLP_TRACES_HEADERS").or_else(|| var("OTEL_EXPORTER_OTLP_HEADERS")).unwrap_or_default();
        let headers = headers.split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.trim().to_string(), percent_decode(v.trim()))).collect();
        Some(Exporter { name: "env".into(), endpoint, headers })
    }

    fn send(&self, app: &App, body: Value) {
        let mut req = app.http.post(crate::tools::mcp::expand(&self.endpoint)).json(&body);
        for (k, v) in &self.headers {
            req = req.header(k, crate::tools::mcp::expand(v));
        }
        tokio::spawn(async move {
            let _ = req.send().await;
        });
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Span {
    pub id: String,
    trace: String,
    parent: Option<String>,
    name: &'static str,
    start: u128,
}

pub(crate) fn nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

/// A deterministic hex id of `len` characters derived from `seed`.
fn hex_id(seed: &str, len: usize) -> String {
    use std::hash::{Hash, Hasher};
    let mut out = String::new();
    let mut i = 0u64;
    while out.len() < len {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (seed, i).hash(&mut h);
        out += &format!("{:016x}", h.finish());
        i += 1;
    }
    out[..len].to_string()
}

impl Span {
    pub fn start(sid: &str, parent: Option<&str>, name: &'static str) -> Span {
        Span { id: hex_id(&new_id(), 16), trace: hex_id(sid, 32), parent: parent.map(String::from), name, start: nanos() }
    }

    /// Close the span and send it (fire and forget) to the environment's exporter and the agent's
    /// `instrumentation/` exporters, if there are any.
    pub fn end(self, app: &App, exporters: &[Exporter], attrs: Value) {
        if app.otlp.is_none() && exporters.is_empty() {
            return;
        }
        let attributes: Vec<Value> = attrs
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let value = match v {
                    Value::Bool(b) => json!({"boolValue": b}),
                    Value::Number(n) if n.is_f64() => json!({"doubleValue": n}),
                    Value::Number(n) => json!({"intValue": n.to_string()}),
                    Value::String(s) => json!({"stringValue": s}),
                    other => json!({"stringValue": other.to_string()}),
                };
                json!({"key": k, "value": value})
            })
            .collect();
        let mut span = json!({"traceId": self.trace, "spanId": self.id, "name": self.name, "kind": 1,
            "startTimeUnixNano": self.start.to_string(), "endTimeUnixNano": nanos().to_string(), "attributes": attributes});
        if let Some(p) = self.parent {
            span["parentSpanId"] = json!(p);
        }
        let body = json!({"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "egin"}}]},
            "scopeSpans": [{"scope": {"name": "egin"}, "spans": [span]}]}]});
        // ponytail: one POST per span; batch if a collector complains about volume.
        for e in app.otlp.iter().chain(exporters) {
            e.send(app, body.clone());
        }
    }
}
