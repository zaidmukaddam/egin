//! Web search and page fetching behind one interface, so an agent can use any provider:
//! You.com (default), Brave, Parallel, Firecrawl, context.dev, or a self-hosted
//! SearXNG. `web_search` and `web_fetch` in tools.rs apply the agent's exclusions to whatever comes
//! back, whichever provider it was.

use crate::{agent::Config, App};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub struct Hit {
    pub title: String,
    pub url: String,
    pub text: String,
}

pub struct Page {
    pub title: String,
    pub url: String,
    pub text: String,
}

fn key(name: &str) -> Result<String> {
    std::env::var(name).ok().filter(|k| !k.is_empty()).with_context(|| format!("web search needs {name}"))
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

async fn send(r: reqwest::RequestBuilder, what: &str) -> Result<Value> {
    let r = r.timeout(Duration::from_secs(60)).send().await?;
    let (status, text) = (r.status(), r.text().await?);
    if !status.is_success() {
        bail!("{what}: {status}: {}", text.chars().take(500).collect::<String>());
    }
    Ok(serde_json::from_str(&text)?)
}

/// egin's freshness ("day", "week", "month", "year" or "YYYY-MM-DDtoYYYY-MM-DD") as a date range.
fn since(freshness: &str) -> Option<(String, Option<String>)> {
    if let Some((a, b)) = freshness.split_once("to") {
        return Some((a.to_string(), Some(b.to_string())));
    }
    let days = match freshness {
        "day" => 1,
        "week" => 7,
        "month" => 31,
        "year" => 365,
        _ => return None,
    };
    Some(((chrono::Utc::now() - chrono::Duration::days(days)).format("%Y-%m-%d").to_string(), None))
}

pub async fn search(app: &App, cfg: &Config, query: &str, count: u64, freshness: Option<&str>) -> Result<Vec<Hit>> {
    match cfg.search.as_str() {
        "you" => you(app, cfg, query, count, freshness).await,
        "brave" => {
            let mut q = vec![("q", query.to_string()), ("count", count.min(20).to_string()), ("extra_snippets", "true".into())];
            if let Some(f) = freshness {
                let f = match f {
                    "day" => "pd",
                    "week" => "pw",
                    "month" => "pm",
                    "year" => "py",
                    range => range,
                };
                q.push(("freshness", f.to_string()));
            }
            let r = app.http.get("https://api.search.brave.com/res/v1/web/search").query(&q).header("X-Subscription-Token", key("BRAVE_API_KEY")?);
            let v = send(r, "brave search").await?;
            Ok(v["web"]["results"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let extra: Vec<String> = r["extra_snippets"].as_array().into_iter().flatten().map(s).collect();
                    let text = if extra.is_empty() { s(&r["description"]) } else { format!("{}\n{}", s(&r["description"]), extra.join("\n")) };
                    Hit { title: s(&r["title"]), url: s(&r["url"]), text }
                })
                .collect())
        }
        "searxng" => {
            let base = key("SEARXNG_URL")?;
            let mut q = vec![("q", query.to_string()), ("format", "json".into())];
            if let Some(f) = freshness.filter(|f| matches!(*f, "day" | "week" | "month" | "year")) {
                q.push(("time_range", f.to_string()));
            }
            let r = app.http.get(format!("{}/search", base.trim_end_matches('/'))).query(&q);
            let v = send(r, "searxng search").await?;
            let hits = v["results"].as_array().into_iter().flatten().take(count as usize);
            Ok(hits.map(|r| Hit { title: s(&r["title"]), url: s(&r["url"]), text: s(&r["content"]) }).collect())
        }
        "parallel" => {
            let mut policy = json!({});
            domains(cfg, &mut policy, "include_domains", "exclude_domains");
            if let Some((after, _)) = freshness.and_then(since) {
                policy["after_date"] = json!(after);
            }
            let body = json!({
                "objective": query,
                "search_queries": [query],
                "advanced_settings": {"max_results": count, "source_policy": policy},
            });
            let r = app.http.post("https://api.parallel.ai/v1/search").header("x-api-key", key("PARALLEL_API_KEY")?).json(&body);
            let v = send(r, "parallel search").await?;
            Ok(v["results"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let text = r["excerpts"].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join("\n");
                    Hit { title: s(&r["title"]), url: s(&r["url"]), text }
                })
                .collect())
        }
        "firecrawl" => {
            let mut body = json!({"query": query, "limit": count.clamp(1, 100)});
            domains(cfg, &mut body, "includeDomains", "excludeDomains");
            if let Some(f) = freshness {
                body["tbs"] = json!(match (f, since(f)) {
                    ("day", _) => "qdr:d".to_string(),
                    ("week", _) => "qdr:w".to_string(),
                    ("month", _) => "qdr:m".to_string(),
                    ("year", _) => "qdr:y".to_string(),
                    (_, Some((start, end))) => {
                        let us = |d: &str| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|d| d.format("%m/%d/%Y").to_string()).unwrap_or_default();
                        format!("cdr:1,cd_min:{},cd_max:{}", us(&start), us(&end.unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string())))
                    }
                    _ => String::new(),
                });
            }
            let r = app.http.post("https://api.firecrawl.dev/v2/search").bearer_auth(key("FIRECRAWL_API_KEY")?).json(&body);
            let v = send(r, "firecrawl search").await?;
            Ok(v["data"]["web"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| Hit { title: s(&r["title"]), url: s(&r["url"]), text: s(&r["description"]) })
                .collect())
        }
        "context.dev" | "contextdev" => {
            let mut body = json!({"query": query, "numResults": count.clamp(1, 100)});
            domains(cfg, &mut body, "includeDomains", "excludeDomains");
            if let Some(f) = freshness {
                let window = match f {
                    "day" => Some("last_24_hours"),
                    "week" => Some("last_week"),
                    "month" => Some("last_month"),
                    "year" => Some("last_year"),
                    _ => None, // no custom ranges on this API
                };
                if let Some(w) = window {
                    body["freshness"] = json!(w);
                }
            }
            if cfg.search_mode == "highlights" {
                body["highlightsOptions"] = json!({"enabled": true});
            }
            let r = app.http.post("https://api.context.dev/v1/web/search").bearer_auth(key("CONTEXT_DEV_API_KEY")?).json(&body);
            let v = send(r, "context.dev search").await?;
            Ok(v["results"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    let h = &r["highlights"]["highlights"];
                    let highlights = match h {
                        Value::Array(a) => a.iter().map(s).collect::<Vec<_>>().join("\n"),
                        other => s(other),
                    };
                    let text = if highlights.is_empty() { s(&r["description"]) } else { highlights };
                    Hit { title: s(&r["title"]), url: s(&r["url"]), text }
                })
                .collect())
        }
        other => bail!("unknown search provider {other:?}: use you, brave, parallel, firecrawl, context.dev or searxng"),
    }
}

/// The agent's domain lists, for providers that filter by domain themselves. The allowlist wins.
fn domains(cfg: &Config, body: &mut Value, include: &str, exclude: &str) {
    if !cfg.include_domains.is_empty() {
        body[include] = json!(cfg.include_domains);
    } else if !cfg.exclude_domains.is_empty() {
        body[exclude] = json!(cfg.exclude_domains);
    }
}

async fn you(app: &App, cfg: &Config, query: &str, count: u64, freshness: Option<&str>) -> Result<Vec<Hit>> {
    let mut body = json!({"query": query, "count": count});
    if cfg.search_mode == "highlights" {
        body["extraction"] = json!({"extraction_mode": "highlights"});
    }
    if let Some(f) = freshness {
        body["freshness"] = json!(f);
    }
    domains(cfg, &mut body, "include_domains", "exclude_domains");
    let r = app.http.post(format!("{}/search", app.search_url)).header("X-API-Key", key("YDC_API_KEY")?).json(&body);
    let v = send(r, "search").await?;
    let passages = |k: &Value| k.as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n")).filter(|s| !s.is_empty());
    Ok(["web", "news"]
        .iter()
        .flat_map(|k| v["results"][k].as_array().into_iter().flatten())
        .map(|r| {
            let text = passages(&r["contents"]["highlights"]).or(passages(&r["snippets"])).unwrap_or(s(&r["description"]));
            Hit { title: s(&r["title"]), url: s(&r["url"]), text }
        })
        .collect())
}

/// Pages as text, through the search provider's own extraction where it has one (You.com Contents,
/// Parallel Extract, Firecrawl Scrape, context.dev Scrape); every other provider fetches directly.
///
/// `html` skips extraction and returns each page's HTML as served. A page whose extraction comes
/// back empty (one the provider cannot render or will not serve) is fetched directly instead.
pub async fn fetch(app: &App, cfg: &Config, urls: &[&str], html: bool) -> Result<Vec<Page>> {
    let failed = |url: &str, e: anyhow::Error| Page { title: String::new(), url: url.to_string(), text: format!("could not fetch: {e:#}") };
    if html {
        let mut pages = vec![];
        for url in urls {
            pages.push(raw(app, url).await.unwrap_or_else(|e| failed(url, e)));
        }
        return Ok(pages);
    }
    let mut pages = extract(app, cfg, urls).await?;
    if pages.is_empty() {
        pages = urls.iter().map(|u| Page { title: String::new(), url: u.to_string(), text: String::new() }).collect();
    }
    for p in pages.iter_mut().filter(|p| p.text.trim().is_empty()) {
        if let Ok(d) = direct(app, &p.url).await {
            *p = d;
        }
    }
    pages.retain(|p| !p.text.trim().is_empty());
    Ok(pages)
}

/// A page's HTML as served.
async fn raw(app: &App, url: &str) -> Result<Page> {
    let r = app.http.get(url).header("user-agent", "egin/0.1 (+https://egin.dev)").timeout(Duration::from_secs(30)).send().await?;
    if !r.status().is_success() {
        bail!("{}", r.status());
    }
    let bytes = r.bytes().await?;
    if bytes.len() > MAX_PAGE {
        bail!("the page is over {} MB", MAX_PAGE / (1024 * 1024));
    }
    Ok(Page { title: String::new(), url: url.into(), text: String::from_utf8_lossy(&bytes).into_owned() })
}

async fn extract(app: &App, cfg: &Config, urls: &[&str]) -> Result<Vec<Page>> {
    match cfg.search.as_str() {
        "you" => {
            let r = app
                .http
                .post(format!("{}/contents", app.search_url))
                .header("X-API-Key", key("YDC_API_KEY")?)
                .json(&json!({"urls": urls, "formats": ["markdown"]}));
            let v = send(r, "contents").await?;
            return Ok(v.as_array().into_iter().flatten().map(|p| Page { title: s(&p["title"]), url: s(&p["url"]), text: s(&p["markdown"]) }).collect());
        }
        "parallel" => {
            let mut pages = vec![];
            for chunk in urls.chunks(20) {
                let body = json!({"urls": chunk, "advanced_settings": {"full_content": true}});
                let r = app.http.post("https://api.parallel.ai/v1/extract").header("x-api-key", key("PARALLEL_API_KEY")?).json(&body);
                let v = send(r, "parallel extract").await?;
                pages.extend(v["results"].as_array().into_iter().flatten().map(|p| {
                    let text = p["full_content"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| p["excerpts"].as_array().into_iter().flatten().map(s).collect::<Vec<_>>().join("\n"));
                    Page { title: s(&p["title"]), url: s(&p["url"]), text }
                }));
                pages.extend(v["errors"].as_array().into_iter().flatten().map(|e| Page {
                    title: String::new(),
                    url: s(&e["url"]),
                    text: format!("could not fetch: {} {}", s(&e["error_type"]), e["http_status_code"]),
                }));
            }
            return Ok(pages);
        }
        _ => {}
    }
    let mut pages = vec![];
    for url in urls {
        let page = match cfg.search.as_str() {
            "firecrawl" => firecrawl_scrape(app, url).await,
            "context.dev" | "contextdev" => context_scrape(app, url).await,
            _ => direct(app, url).await,
        };
        pages.push(page.unwrap_or_else(|e| Page { title: String::new(), url: url.to_string(), text: format!("could not fetch: {e:#}") }));
    }
    Ok(pages)
}

async fn firecrawl_scrape(app: &App, url: &str) -> Result<Page> {
    let r = app.http.post("https://api.firecrawl.dev/v2/scrape").bearer_auth(key("FIRECRAWL_API_KEY")?).json(&json!({"url": url, "formats": ["markdown"]}));
    let v = send(r, "firecrawl scrape").await?;
    let d = &v["data"];
    // `metadata.title` is a string, or an array when the page has several.
    let title = match &d["metadata"]["title"] {
        Value::Array(a) => a.first().map(s).unwrap_or_default(),
        t => s(t),
    };
    Ok(Page { title, url: url.into(), text: s(&d["markdown"]) })
}

async fn context_scrape(app: &App, url: &str) -> Result<Page> {
    let r = app
        .http
        .post("https://api.context.dev/v1/web/scrape")
        .bearer_auth(key("CONTEXT_DEV_API_KEY")?)
        .json(&json!({"url": url, "formats": {"markdown": true}}));
    let v = send(r, "context.dev scrape").await?;
    if v["markdown"]["success"] == json!(false) {
        bail!("context.dev could not read the page");
    }
    Ok(Page { title: s(&v["metadata"]["title"]), url: v["url"].as_str().unwrap_or(url).into(), text: s(&v["markdown"]["data"]) })
}

/// The largest page a direct fetch reads.
const MAX_PAGE: usize = 10 * 1024 * 1024;

async fn direct(app: &App, url: &str) -> Result<Page> {
    let r = app.http.get(url).header("user-agent", "egin/0.1 (+https://egin.dev)").timeout(Duration::from_secs(30)).send().await?;
    if !r.status().is_success() {
        bail!("{}", r.status());
    }
    let kind = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let bytes = r.bytes().await?;
    if bytes.len() > MAX_PAGE {
        bail!("the page is over {} MB", MAX_PAGE / (1024 * 1024));
    }
    if kind.contains("pdf") || url.to_lowercase().ends_with(".pdf") {
        let text = super::docs::extract(&super::docs::Kind::Pdf, &bytes)?;
        return Ok(Page { title: String::new(), url: url.into(), text });
    }
    let body = String::from_utf8_lossy(&bytes);
    if kind.contains("html") || body.trim_start().starts_with('<') {
        let (title, text) = html_text(&body);
        return Ok(Page { title, url: url.into(), text });
    }
    if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") || kind.is_empty() {
        return Ok(Page { title: String::new(), url: url.into(), text: body.into_owned() });
    }
    bail!("unsupported content type {kind}")
}

/// Readable text from HTML: the title, then the body with scripts, styles and tags removed and
/// block elements on their own lines.
// ponytail: a tag stripper, not a parser. Good enough for reading articles and docs; swap in a real
// readability pass if pages with heavy chrome start crowding out the content.
pub fn html_text(html: &str) -> (String, String) {
    // ASCII lowercasing keeps byte offsets identical to `html`'s.
    let lower = html.to_ascii_lowercase();
    let title = lower.find("<title").and_then(|a| {
        let start = a + lower[a..].find('>')? + 1;
        let end = start + lower[start..].find("</title")?;
        Some(entities(html[start..end].trim()))
    });
    // Drop whole elements whose content is not text.
    let skip: Vec<(String, String)> = ["script", "style", "noscript", "svg", "head", "template"].iter().map(|t| (format!("<{t}"), format!("</{t}>"))).collect();
    let mut kept = String::with_capacity(html.len());
    let mut i = 0;
    'scan: while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            for (open, close) in &skip {
                if lower[i..].starts_with(open.as_str()) {
                    if let Some(end) = lower[i..].find(close.as_str()) {
                        i += end + close.len();
                        continue 'scan;
                    }
                }
            }
        }
        let c = html[i..].chars().next().unwrap();
        kept.push(c);
        i += c.len_utf8();
    }
    let mut text = String::with_capacity(kept.len());
    let mut rest = kept.as_str();
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else { break };
        let tag = rest[open + 1..open + close].trim_start_matches('/').to_lowercase();
        let name = tag.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        if matches!(name, "p" | "div" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "article" | "pre" | "blockquote" | "table") {
            text.push('\n');
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let text = entities(&text);
    // One line per block; the blank lines markup leaves behind are noise to a model.
    let lines: Vec<String> = text.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    (title.unwrap_or_default(), lines.join("\n"))
}

fn entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let Some(semi) = tail.find(';').filter(|n| *n <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let name = &tail[1..semi];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_readable_text() {
        let html = r#"<html><head><title>Rust &amp; You</title><style>p{color:red}</style></head>
            <body><script>alert(1)</script><h1>Release</h1><p>Rust&nbsp;1.100 is <b>out</b> &#8212; today.</p>
            <ul><li>one</li><li>two</li></ul></body></html>"#;
        let (title, text) = html_text(html);
        assert_eq!(title, "Rust & You");
        assert_eq!(text, "Release\nRust 1.100 is out \u{2014} today.\none\ntwo");
    }

    #[test]
    fn freshness_ranges() {
        assert_eq!(since("2026-01-01to2026-02-01"), Some(("2026-01-01".into(), Some("2026-02-01".into()))));
        assert!(since("week").is_some_and(|(_, end)| end.is_none()));
        assert_eq!(since("someday"), None);
    }
}
