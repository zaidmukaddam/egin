//! Which provider serves a model, which key it needs, and where its calls go. Each provider speaks one
//! of the wire protocols beside this file.

use super::{Fatal, Protocol};
use crate::App;
use anyhow::{bail, Result};

/// Where a model's calls go.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// The provider's name in `agent.toml`, for the quirks particular to one provider.
    pub provider: String,
    pub protocol: Protocol,
    pub url: String,
    pub key: String,
    /// The model id as the endpoint names it (Bedrock ids carry an `anthropic.` prefix).
    pub model: String,
}

const BEDROCK_KEYS: &[&str] = &["AWS_BEDROCK_API_KEY", "AWS_BEARER_TOKEN_BEDROCK", "AMAZON_BEDROCK_API_KEY"];

/// The env vars that may hold a provider's key, the first one set winning.
pub fn key_envs(provider: &str) -> &'static [&'static str] {
    match provider {
        "you" => &["YDC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        // A Vercel OIDC token works wherever an AI Gateway key does; the key wins when both are set.
        "gateway" => &["AI_GATEWAY_API_KEY", "VERCEL_OIDC_TOKEN"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "bedrock" => BEDROCK_KEYS,
        _ => &[],
    }
}

/// Whether any of a provider's keys is set.
pub fn has_key(provider: &str) -> bool {
    key_envs(provider).iter().any(|n| std::env::var(n).is_ok_and(|v| !v.is_empty()))
}

/// The provider a model id names, and the id that provider knows the model by.
/// - `anthropic/…`, `openai/…`, `gemini/…`, `bedrock/…` and `you/…` name the provider. `anthropic/…` and
///   `openai/…` go through the AI Gateway instead when only a Gateway key is set, as eve's ids do.
/// - Any other `vendor/model` is an AI Gateway id.
/// - Bedrock ids (`anthropic.…`, `us.anthropic.…`, `openai.gpt-…`) go to Bedrock, `claude-…` to the
///   Claude API, `gpt-…` (except the router's gpt-oss) and `o3…` to OpenAI, anything else to the router.
pub fn route(model: &str) -> (&'static str, String) {
    if let Some((vendor, id)) = model.split_once('/') {
        let named = match vendor {
            "anthropic" => Some("anthropic"),
            "openai" => Some("openai"),
            "gemini" | "google" => Some("gemini"),
            "bedrock" => Some("bedrock"),
            "you" => Some("you"),
            _ => None,
        };
        return match named {
            Some(p) if p == "anthropic" || p == "openai" => {
                if !has_key(p) && has_key("gateway") {
                    ("gateway", model.into())
                } else {
                    (p, id.into())
                }
            }
            Some(p) => (p, id.into()),
            None => ("gateway", model.into()),
        };
    }
    let p = if model.starts_with("anthropic.") || model.contains(".anthropic.") || model.starts_with("openai.") {
        "bedrock"
    } else if model.starts_with("claude-") {
        "anthropic"
    } else if (model.starts_with("gpt-") && !model.starts_with("gpt-oss")) || model.starts_with("o3") || model.starts_with("o4") {
        "openai"
    } else {
        "you"
    };
    (p, model.into())
}

/// The reasoning efforts a model accepts, as its provider names them (each provider's API lists them
/// in the error for a wrong one). Models egin cannot confirm get the three every provider takes.
pub fn reasoning_efforts(model: &str) -> &'static [&'static str] {
    const CLAUDE: &[&str] = &["low", "medium", "high", "xhigh", "max"];
    const GPT: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
    const COMMON: &[&str] = &["low", "medium", "high"];
    let (provider, id) = route(model);
    let id = id.rsplit('/').next().unwrap_or(&id);
    if super::is_claude(id) && provider != "gateway" {
        CLAUDE
    } else if matches!(provider, "openai" | "bedrock") && responses_only(id) {
        GPT
    } else {
        COMMON
    }
}

/// What to do when a provider's key is missing, in one line.
pub fn missing_key(model: &str, provider: &str, envs: &[&str]) -> String {
    let dir = std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_else(|_| ".".into());
    format!("{model} runs on {provider}, which needs {}: set it in your shell or in .env.local in {dir}", envs.join(" or "))
}

/// OpenAI's GPT models take function tools only through the Responses API, on OpenAI and on Bedrock.
fn responses_only(model: &str) -> bool {
    let id = model.strip_prefix("openai.").unwrap_or(model);
    id.starts_with("gpt-") && !id.starts_with("gpt-oss")
}

/// A named provider: its protocol, default endpoint, and the env vars that may hold its key (the
/// first one set wins).
fn preset(provider: &str, model: &str, app: &App) -> Result<(Protocol, String, &'static [&'static str])> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let region = || env("AWS_REGION").or_else(|| env("AWS_DEFAULT_REGION")).unwrap_or_else(|| "us-east-1".into());
    let keys = key_envs(provider);
    Ok(match provider {
        "you" => (Protocol::OpenAi, app.base_url.clone(), keys),
        "openai" if responses_only(model) => (Protocol::Responses, "https://api.openai.com/v1".into(), keys),
        "openai" => (Protocol::OpenAi, "https://api.openai.com/v1".into(), keys),
        "gateway" => (Protocol::OpenAi, "https://ai-gateway.vercel.sh/v1".into(), keys),
        "gemini" => (Protocol::OpenAi, "https://generativelanguage.googleapis.com/v1beta/openai".into(), keys),
        "anthropic" => (Protocol::Anthropic, env("ANTHROPIC_BASE_URL").unwrap_or_else(|| "https://api.anthropic.com".into()), keys),
        // Claude takes the Messages API on bedrock-mantle. A Claude model that Bedrock serves only
        // through an inference profile (`us.anthropic.claude-…`) is invoked on bedrock-runtime.
        // Every other Bedrock model takes OpenAI-compatible chat completions on bedrock-runtime.
        "bedrock" if model.contains(".anthropic.") => (Protocol::Bedrock, format!("https://bedrock-runtime.{}.amazonaws.com", region()), keys),
        "bedrock" if model.contains("anthropic.") || model.starts_with("claude-") => {
            (Protocol::Bedrock, format!("https://bedrock-mantle.{}.api.aws/anthropic", region()), keys)
        }
        "bedrock" if responses_only(model) => (Protocol::Responses, format!("https://bedrock-mantle.{}.api.aws/openai/v1", region()), keys),
        "bedrock" => (Protocol::OpenAi, format!("https://bedrock-runtime.{}.amazonaws.com/openai/v1", region()), keys),
        p => {
            bail!("unknown provider {p:?}: use you, openai, gateway, gemini, anthropic or bedrock (any other OpenAI-compatible endpoint: openai with base_url)")
        }
    })
}

impl Endpoint {
    /// The endpoint for `model`. `cfg` is the agent's config, whose `provider`, `base_url` and
    /// `api_key_env` apply only when `model` is the agent's own model; anything else (a judge, a
    /// proposer) is reached by its id alone.
    pub fn resolve(app: &App, model: &str, cfg: Option<&crate::agent::Config>) -> Result<Endpoint> {
        let cfg = cfg.filter(|c| c.model == model);
        let (provider, routed) = match cfg.and_then(|c| c.provider.clone()) {
            Some(p) => (p, model.to_string()),
            None => {
                let (p, id) = route(model);
                (p.to_string(), id)
            }
        };
        let model = routed.as_str();
        let (protocol, default_url, key_envs) = preset(&provider, model, app)?;
        // Read on every call: a Vercel OIDC token rotates, so it is never cached.
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let key = match cfg.and_then(|c| c.api_key_env.as_deref()) {
            Some(name) => env(name).ok_or_else(|| Fatal(format!("no API key for {model}: set {name}")))?,
            None => key_envs
                .iter()
                .find_map(|n| env(n))
                // The router's key is the app's own (YDC_API_KEY when the app comes from the environment).
                .or_else(|| (provider == "you" && !app.key.is_empty()).then(|| app.key.clone()))
                .ok_or_else(|| Fatal(missing_key(model, &provider, key_envs)))?,
        };
        let url = cfg.and_then(|c| c.base_url.clone()).unwrap_or(default_url).trim_end_matches('/').to_string();
        let model = match protocol {
            Protocol::Bedrock if !model.starts_with("anthropic.") && !model.contains(".anthropic.") => format!("anthropic.{model}"),
            _ => model.to_string(),
        };
        Ok(Endpoint { provider, protocol, url, key, model })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> std::sync::Arc<App> {
        let db = std::env::temp_dir().join(format!("egin-model-{}.db", crate::new_id()));
        App::with(&db, crate::Approve::Auto, "router-key", "https://router.example/v1", "https://search.example").unwrap()
    }

    fn cfg(toml: &str) -> crate::agent::Config {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn providers_resolve_to_their_endpoints() {
        let app = app();
        std::env::set_var("EGIN_TEST_PROVIDER_KEY", "k");
        let ep = |toml: &str| {
            let c = cfg(&format!("{toml}\napi_key_env = \"EGIN_TEST_PROVIDER_KEY\""));
            Endpoint::resolve(&app, &c.model.clone(), Some(&c)).unwrap()
        };
        let openai = ep("model = \"m\"\nprovider = \"openai\"");
        assert_eq!((openai.protocol, openai.url.as_str()), (Protocol::OpenAi, "https://api.openai.com/v1"));
        assert_eq!(ep("model = \"gpt-6-luna\"\nprovider = \"openai\"").protocol, Protocol::Responses, "GPT models take tools only there");
        assert_eq!(ep("model = \"openai/x\"\nprovider = \"gateway\"").url, "https://ai-gateway.vercel.sh/v1");
        assert_eq!(ep("model = \"g\"\nprovider = \"gemini\"").url, "https://generativelanguage.googleapis.com/v1beta/openai");
        let claude = ep("model = \"claude-opus-5-5\"\nprovider = \"bedrock\"");
        assert_eq!((claude.protocol, claude.model.as_str()), (Protocol::Bedrock, "anthropic.claude-opus-5-5"));
        let llama = ep("model = \"meta.llama4-maverick\"\nprovider = \"bedrock\"");
        assert_eq!(llama.protocol, Protocol::OpenAi);
        assert!(llama.url.ends_with(".amazonaws.com/openai/v1"), "{}", llama.url);
        assert_eq!(ep("model = \"m\"\nprovider = \"openai\"\nbase_url = \"http://localhost:11434/v1/\"").url, "http://localhost:11434/v1");
        // Unset: chosen from the model id.
        let c = cfg("model = \"kimi\"");
        assert_eq!(Endpoint::resolve(&app, "kimi", Some(&c)).unwrap().url, "https://router.example/v1");
    }

    #[test]
    fn model_ids_name_their_provider() {
        // No env is read for these: a provider named in the id with its own prefix.
        assert_eq!(route("gemini/gemini-3.8-flash"), ("gemini", "gemini-3.8-flash".into()));
        assert_eq!(route("bedrock/meta.llama4"), ("bedrock", "meta.llama4".into()));
        assert_eq!(route("xai/grok-4.7"), ("gateway", "xai/grok-4.7".into()), "an unknown vendor is a Gateway id");
        assert_eq!(route("us.anthropic.claude-sonnet-5-5").0, "bedrock");
        assert_eq!(route("openai.gpt-6-luna").0, "bedrock");
        assert_eq!(route("claude-sonnet-5-5").0, "anthropic");
        assert_eq!(route("gpt-6-luna").0, "openai");
        assert_eq!(route("gpt-oss-120b").0, "you", "the router serves gpt-oss");
        assert_eq!(route("kimi-k3.1-pro").0, "you");
    }

    #[test]
    fn each_provider_offers_its_own_reasoning_efforts() {
        assert_eq!(reasoning_efforts("claude-sonnet-5-5"), ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(reasoning_efforts("us.anthropic.claude-haiku-5-5"), ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(reasoning_efforts("gpt-6-luna")[0], "none");
        assert_eq!(reasoning_efforts("openai.gpt-6.1-sol").last(), Some(&"max"));
        assert_eq!(reasoning_efforts("kimi-k3.1-pro"), ["low", "medium", "high"]);
    }

    #[test]
    fn the_gateway_falls_back_to_a_vercel_oidc_token() {
        let app = app();
        // Only the OIDC token is set for this check, so it must be the one picked.
        std::env::remove_var("AI_GATEWAY_API_KEY");
        std::env::set_var("VERCEL_OIDC_TOKEN", "oidc");
        let c = cfg("model = \"anthropic/claude-opus-4.5\"\nprovider = \"gateway\"");
        assert_eq!(Endpoint::resolve(&app, &c.model.clone(), Some(&c)).unwrap().key, "oidc");
        std::env::remove_var("VERCEL_OIDC_TOKEN");
        assert!(Endpoint::resolve(&app, &c.model.clone(), Some(&c)).is_err());
    }
}
