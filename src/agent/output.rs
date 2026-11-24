//! Structured output: a session given a JSON schema (`output_schema` on the HTTP API, `--schema` on
//! `egin run`) ends its runs by calling a `final_answer` tool whose parameters are that schema. The
//! value is checked against the schema (types, required fields, enums, recursively); a value that
//! doesn't fit goes back to the model to fix. The run's answer is then the value as JSON, and the
//! `done` event carries it as `output`.

use crate::App;
use anyhow::Result;
use serde_json::{json, Value};

pub const TOOL: &str = "final_answer";

/// The session's schema, if it has one.
pub fn schema(app: &App, sid: &str) -> Option<Value> {
    let s = app.store.settings(sid).ok()??;
    serde_json::from_str(s.get("output_schema")?.as_str()?).ok()
}

/// Set or clear the session's schema. A schema that isn't a JSON object is refused.
pub fn set(app: &App, sid: &str, schema: Option<&Value>) -> Result<()> {
    let mut s = app.store.settings(sid)?.unwrap_or_default();
    match schema {
        Some(v) if v.is_object() => {
            s.insert("output_schema".into(), v.to_string().into());
        }
        Some(_) => anyhow::bail!("output_schema must be a JSON Schema object"),
        None => {
            s.remove("output_schema");
        }
    }
    app.store.set_settings(sid, &s)
}

/// A tool's parameters must be an object; any other schema is wrapped as `{"value": ...}`.
fn wrapped(schema: &Value) -> (Value, bool) {
    if schema["type"] == "object" {
        (schema.clone(), false)
    } else {
        (json!({"type": "object", "properties": {"value": schema}, "required": ["value"]}), true)
    }
}

pub fn spec(schema: &Value) -> Value {
    let description = "Give your final answer, as the structured value the task asks for. Call this once you are done, instead of answering in text.";
    json!({"type": "function", "function": {"name": TOOL, "description": description, "parameters": wrapped(schema).0}})
}

/// The answer from a `final_answer` call, or what is wrong with it.
pub fn take(schema: &Value, args: &Value) -> Result<Value, String> {
    let (params, wrap) = wrapped(schema);
    check(&params, args, "$")?;
    Ok(if wrap { args["value"].clone() } else { args.clone() })
}

fn check(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    if let Some(choices) = schema["enum"].as_array() {
        if !choices.contains(v) {
            return Err(format!("{at} must be one of {}", Value::Array(choices.clone())));
        }
    }
    let ok = match schema["type"].as_str() {
        Some("object") => v.is_object(),
        Some("array") => v.is_array(),
        Some("string") => v.is_string(),
        Some("integer") => v.is_i64() || v.is_u64(),
        Some("number") => v.is_number(),
        Some("boolean") => v.is_boolean(),
        Some("null") => v.is_null(),
        _ => true,
    };
    if !ok {
        return Err(format!("{at} must be of type {}", schema["type"]));
    }
    if let Some(obj) = v.as_object() {
        for r in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !obj.contains_key(r) {
                return Err(format!("{at}.{r} is required"));
            }
        }
        if let Some(props) = schema["properties"].as_object() {
            for (k, sub) in props {
                if let Some(val) = obj.get(k) {
                    check(sub, val, &format!("{at}.{k}"))?;
                }
            }
        }
    }
    if let (Some(items), Some(list)) = (schema.get("items"), v.as_array()) {
        for (i, val) in list.iter().enumerate() {
            check(items, val, &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_types_required_enums_and_nesting() {
        let s = json!({"type": "object", "required": ["name", "tags"], "properties": {
            "name": {"type": "string"},
            "level": {"enum": ["low", "high"]},
            "tags": {"type": "array", "items": {"type": "string"}}}});
        assert_eq!(take(&s, &json!({"name": "x", "tags": ["a"], "level": "low"})).unwrap()["name"], "x");
        assert_eq!(take(&s, &json!({"tags": []})).unwrap_err(), "$.name is required");
        assert_eq!(take(&s, &json!({"name": 1, "tags": []})).unwrap_err(), "$.name must be of type \"string\"");
        assert!(take(&s, &json!({"name": "x", "tags": [1]})).unwrap_err().starts_with("$.tags[0]"));
        assert!(take(&s, &json!({"name": "x", "tags": [], "level": "mid"})).unwrap_err().contains("one of"));
        // A schema that isn't an object is wrapped, and unwrapped again.
        let list = json!({"type": "array", "items": {"type": "integer"}});
        assert_eq!(spec(&list)["function"]["parameters"]["required"], json!(["value"]));
        assert_eq!(take(&list, &json!({"value": [1, 2]})).unwrap(), json!([1, 2]));
    }
}
