//! OAuth sign-in for MCP servers over HTTP, as the MCP authorization spec lays it out: an
//! unauthenticated request draws a 401 whose `WWW-Authenticate` names the protected-resource
//! metadata (RFC 9728), that names the authorization server, whose metadata (RFC 8414 or OpenID
//! discovery) gives the endpoints. egin identifies itself with a client metadata document where the
//! server takes one, else registers itself (RFC 7591), unless the connection has a `client_id`;
//! then it signs in with the authorization code flow and PKCE on a localhost redirect, and
//! sends `resource` (RFC 8707) with every authorization and token request.
//!
//! `egin connect <name>` runs [`login`]; tokens land in `.egin/oauth/<name>.json`, readable only by
//! the owner, never in `connections/`. The MCP client calls [`access_token`] before each request,
//! which refreshes a token that is about to expire, or that the server just refused.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// egin's Client ID Metadata Document, served by the site (site/public/oauth/client.json). It lists
/// [`redirect_uri`]. `EGIN_OAUTH_CLIENT` points at another one.
pub const CLIENT_DOCUMENT: &str = "https://egin.dev/oauth/client.json";
pub const REDIRECT_PORT: u16 = 33418;

pub fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}/callback")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: Option<i64>,
    pub token_endpoint: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub resource: String,
    pub issuer: String,
}

/// Where a connection's tokens are kept.
pub fn file(agent_dir: &Path, connection: &str) -> PathBuf {
    crate::state_dir(agent_dir).join("oauth").join(format!("{connection}.json"))
}

fn connection_name(file: &Path) -> String {
    file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn load(file: &Path) -> Result<Option<Tokens>> {
    match std::fs::read_to_string(file) {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| file.display().to_string())?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn save(file: &Path, t: &Tokens) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = file.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // Write a fresh 0600 file and rename it over the old one, so a token is never world-readable.
    let tmp = file.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    std::io::Write::write_all(&mut f, serde_json::to_string_pretty(t)?.as_bytes())?;
    std::fs::rename(&tmp, file)?;
    Ok(())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A bearer token for the connection whose tokens are in `file`: the stored one, refreshed first
/// when it expires within a minute or `refused` says the server just turned it down.
pub async fn access_token(http: &reqwest::Client, file: &Path, refused: bool) -> Result<String> {
    let name = connection_name(file);
    let again = || anyhow!("connection {name} needs a new sign-in: run `egin connect {name}`");
    let Some(mut t) = load(file)? else { bail!("connection {name} needs a sign-in: run `egin connect {name}`") };
    let expiring = t.expires_at.is_some_and(|e| e - 60 <= now());
    if !(refused || expiring) {
        return Ok(t.access_token);
    }
    let Some(refresh) = t.refresh_token.clone() else { return Err(again()) };
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &t.client_id), ("resource", &t.resource)];
    let secret = t.client_secret.clone().unwrap_or_default();
    if !secret.is_empty() {
        form.push(("client_secret", &secret));
    }
    let got = token_request(http, &t.token_endpoint, &form).await.map_err(|e| again().context(e))?;
    t.access_token = got.0;
    t.refresh_token = got.1.or(t.refresh_token);
    t.expires_at = got.2;
    save(file, &t)?;
    Ok(t.access_token)
}

/// POST a token request; returns (access token, refresh token, expiry).
async fn token_request(http: &reqwest::Client, endpoint: &str, form: &[(&str, &str)]) -> Result<(String, Option<String>, Option<i64>)> {
    let r = http.post(endpoint).header("accept", "application/json").form(form).send().await?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    let Some(access) = v["access_token"].as_str().filter(|_| status.is_success()) else { bail!("token endpoint answered {status}: {v}") };
    let expires = v["expires_in"].as_i64().map(|s| now() + s);
    Ok((access.into(), v["refresh_token"].as_str().map(String::from), expires))
}

/// The value of `key` in a `WWW-Authenticate: Bearer k="v", ...` header.
fn challenge_param(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let mut from = 0;
    while let Some(i) = header[from..].find(&needle).map(|i| i + from) {
        let starts_word = i == 0 || !header.as_bytes()[i - 1].is_ascii_alphanumeric() && header.as_bytes()[i - 1] != b'_';
        let rest = &header[i + needle.len()..];
        if starts_word {
            return rest.find('"').map(|j| rest[..j].to_string());
        }
        from = i + needle.len();
    }
    None
}

async fn get_json(http: &reqwest::Client, url: &str) -> Option<Value> {
    let r = http.get(url).header("accept", "application/json").send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

/// `origin + /.well-known/<suffix> + path`, the RFC 8414 / 9728 path insertion.
fn well_known(u: &Url, suffix: &str) -> String {
    format!("{}/.well-known/{suffix}{}", u.origin().ascii_serialization(), u.path().trim_end_matches('/'))
}

/// The authorization server's metadata, from the well-known URLs the spec lists, in its order.
/// A document whose `issuer` is not exactly the one asked for is not used.
async fn server_metadata(http: &reqwest::Client, issuer: &str) -> Result<Value> {
    let u = Url::parse(issuer).with_context(|| format!("authorization server {issuer}"))?;
    let path = u.path().trim_end_matches('/');
    let origin = u.origin().ascii_serialization();
    let candidates = if path.is_empty() {
        vec![format!("{origin}/.well-known/oauth-authorization-server"), format!("{origin}/.well-known/openid-configuration")]
    } else {
        vec![well_known(&u, "oauth-authorization-server"), well_known(&u, "openid-configuration"), format!("{origin}{path}/.well-known/openid-configuration")]
    };
    for c in &candidates {
        if let Some(m) = get_json(http, c).await {
            if m["issuer"].as_str() == Some(issuer) {
                return Ok(m);
            }
            eprintln!("egin: ignoring {c}: its issuer is {} rather than {issuer}", m["issuer"]);
        }
    }
    bail!("no authorization server metadata for {issuer} (tried {})", candidates.join(", "))
}

fn random(n: usize) -> String {
    let mut b = vec![0u8; n];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("system randomness");
    URL_SAFE_NO_PAD.encode(b)
}

/// Sign in to the MCP server at `url` and store the tokens in `file`. `show` gets the URL to open
/// in a browser; the redirect comes back to a listener on 127.0.0.1.
pub async fn login(http: &reqwest::Client, file: &Path, url: &str, client_id: Option<&str>, show: impl FnOnce(&str)) -> Result<Tokens> {
    let resource = url.trim_end_matches('/').to_string();
    let server = Url::parse(&resource).with_context(|| format!("connection url {url}"))?;

    // 1. Ask without a token; the 401 says where the protected-resource metadata is and what scope to ask for.
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "egin", "version": env!("CARGO_PKG_VERSION")}}});
    let probe = http.post(url).header("accept", "application/json, text/event-stream").json(&init).send().await.with_context(|| format!("reaching {url}"))?;
    let header = probe.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if probe.status() != reqwest::StatusCode::UNAUTHORIZED {
        bail!("{url} answered {} without a token, so it does not ask for a sign-in", probe.status());
    }

    // 2. Protected-resource metadata: from the header, else the well-known URLs, path first.
    let mut tried = vec![];
    let prm = match challenge_param(&header, "resource_metadata") {
        Some(u) => {
            tried.push(u.clone());
            get_json(http, &u).await
        }
        None => {
            let mut found = None;
            for u in
                [well_known(&server, "oauth-protected-resource"), format!("{}/.well-known/oauth-protected-resource", server.origin().ascii_serialization())]
            {
                tried.push(u.clone());
                if let Some(m) = get_json(http, &u).await {
                    found = Some(m);
                    break;
                }
            }
            found
        }
    };
    let prm = prm.ok_or_else(|| anyhow!("no protected-resource metadata for {url} (tried {})", tried.join(", ")))?;
    // The resource as the server names it (Vercel's has a trailing slash), when it is this server.
    let resource = match prm["resource"].as_str() {
        Some(r) if resource.starts_with(r.trim_end_matches('/')) => r.to_string(),
        Some(r) => bail!("the metadata at {} is for {r}, not {url}", tried.last().unwrap()),
        None => resource,
    };
    let issuer =
        prm["authorization_servers"][0].as_str().ok_or_else(|| anyhow!("the protected-resource metadata names no authorization server: {prm}"))?.to_string();

    // 3. The authorization server, which must do PKCE with S256.
    let meta = server_metadata(http, &issuer).await?;
    let s256 = meta["code_challenge_methods_supported"].as_array().is_some_and(|m| m.iter().any(|x| x == "S256"));
    if !s256 {
        bail!("{issuer} does not advertise PKCE with S256, which the MCP spec requires");
    }
    let endpoint = |k: &str| meta[k].as_str().map(String::from).ok_or_else(|| anyhow!("{issuer} metadata has no {k}"));
    let (authorize, token_endpoint) = (endpoint("authorization_endpoint")?, endpoint("token_endpoint")?);

    // 4. The redirect listener, on a fixed port so a client document or a hand registration can
    // name it; then the client: pre-registered, egin's metadata document (CIMD) where the server
    // takes one, or registered now (RFC 7591).
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .with_context(|| format!("listening for the sign-in on 127.0.0.1:{REDIRECT_PORT}; is another `egin connect` running?"))?;
    let redirect = redirect_uri();
    let cimd = meta["client_id_metadata_document_supported"] == true;
    let (client_id, client_secret) = match client_id {
        Some(id) => (id.to_string(), None),
        None if cimd => (crate::setting("OAUTH_CLIENT").unwrap_or_else(|| CLIENT_DOCUMENT.into()), None),
        None => {
            let reg = endpoint("registration_endpoint")
                .map_err(|_| anyhow!("{issuer} has no dynamic client registration; register an app there and set client_id in the connection file"))?;
            let body = json!({"client_name": "egin", "redirect_uris": [redirect], "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"], "token_endpoint_auth_method": "none"});
            let r = http.post(&reg).json(&body).send().await?;
            let status = r.status();
            let v: Value = r.json().await.unwrap_or_default();
            let id = v["client_id"].as_str().filter(|_| status.is_success()).ok_or_else(|| anyhow!("client registration at {reg} answered {status}: {v}"))?;
            (id.to_string(), v["client_secret"].as_str().map(String::from))
        }
    };

    // 5. Authorization request with PKCE, state and resource; the scope the 401 asked for, else the metadata's.
    let verifier = random(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random(16);
    let mut scope = challenge_param(&header, "scope").or_else(|| {
        let s: Vec<&str> = prm["scopes_supported"].as_array()?.iter().filter_map(Value::as_str).collect();
        (!s.is_empty()).then(|| s.join(" "))
    });
    // Ask for a refresh token where the server offers one, so the sign-in lasts.
    if meta["scopes_supported"].as_array().is_some_and(|s| s.iter().any(|x| x == "offline_access")) {
        scope = Some(scope.map_or("offline_access".into(), |s| format!("{s} offline_access")));
    }
    let mut auth_url = Url::parse(&authorize)?;
    {
        let mut q = auth_url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &resource);
        if let Some(s) = &scope {
            q.append_pair("scope", s);
        }
    }
    show(auth_url.as_str());

    // 6. The browser comes back with the code.
    let params = tokio::time::timeout(Duration::from_secs(600), callback(&listener)).await.map_err(|_| anyhow!("no sign-in within 10 minutes"))??;
    let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    if get("state").as_deref() != Some(&state) {
        bail!("the sign-in came back with the wrong state; try again");
    }
    // RFC 9207: a returned issuer must be the one we asked; one that promised to send it must.
    match get("iss") {
        Some(iss) if iss != issuer => bail!("the sign-in came back from {iss}, not {issuer}; refusing it"),
        None if meta["authorization_response_iss_parameter_supported"] == true => {
            bail!("{issuer} promised an iss parameter and sent none; refusing the sign-in")
        }
        _ => {}
    }
    if let Some(e) = get("error") {
        bail!("sign-in refused: {e} {}", get("error_description").unwrap_or_default());
    }
    let code = get("code").ok_or_else(|| anyhow!("the sign-in came back without a code"))?;

    // 7. Code for tokens.
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("resource", resource.as_str()),
    ];
    if let Some(s) = &client_secret {
        form.push(("client_secret", s));
    }
    let (access_token, refresh_token, expires_at) = token_request(http, &token_endpoint, &form).await?;
    let t = Tokens { access_token, refresh_token, expires_at, token_endpoint, client_id, client_secret, resource, issuer };
    save(file, &t)?;
    Ok(t)
}

/// Serve requests on the redirect listener until one reaches /callback; return its query.
async fn callback(listener: &tokio::net::TcpListener) -> Result<Vec<(String, String)>> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let mut n = 0;
        while n < buf.len() {
            let got = sock.read(&mut buf[n..]).await?;
            n += got;
            if got == 0 || buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf[..n]);
        let target = head.split_whitespace().nth(1).unwrap_or("/");
        let u = Url::parse(&format!("http://127.0.0.1{target}"))?;
        if u.path() != "/callback" {
            sock.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await?;
            continue;
        }
        let page = "<!doctype html><meta charset=utf-8><title>egin</title><p style=\"font:16px system-ui;margin:3rem\">Signed in. You can close this tab and go back to the terminal.</p>";
        let resp = format!("HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}", page.len());
        sock.write_all(resp.as_bytes()).await?;
        return Ok(u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_challenge_parameters() {
        let h = r#"Bearer error="insufficient_scope", scope="files:read files:write", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource""#;
        assert_eq!(challenge_param(h, "scope").as_deref(), Some("files:read files:write"));
        assert_eq!(challenge_param(h, "resource_metadata").as_deref(), Some("https://mcp.example.com/.well-known/oauth-protected-resource"));
        assert_eq!(challenge_param(r#"Bearer error="insufficient_scope""#, "scope"), None, "not the end of another name");
        let u = Url::parse("https://h.example/tenant1/").unwrap();
        assert_eq!(well_known(&u, "oauth-authorization-server"), "https://h.example/.well-known/oauth-authorization-server/tenant1");
    }
}
