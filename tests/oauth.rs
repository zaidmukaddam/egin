//! `egin connect` against a fake MCP server and authorization server: discovery from the 401,
//! dynamic registration, the code flow with PKCE, the token on MCP requests, and refresh.

mod common;

use axum::{
    extract::{Form, Query},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::fx;
use egin::Approve;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Fake {
    /// The only access token the MCP endpoint accepts right now.
    valid: String,
    issued: u32,
    challenge: String,
    log: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

/// With `cimd`, the authorization server takes client metadata documents and expects egin's.
async fn fake_server(cimd: bool) -> (String, Shared) {
    let s: Shared = Arc::default();
    let client: &'static str = if cimd { egin::tools::oauth::CLIENT_DOCUMENT } else { "client-1" };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = format!("{base}/auth");

    let (st, b) = (s.clone(), base.clone());
    let mcp = post(move |headers: HeaderMap, Json(msg): Json<Value>| async move {
        let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let valid = st.lock().unwrap().valid.clone();
        if valid.is_empty() || auth != format!("Bearer {valid}") {
            let challenge = format!("Bearer resource_metadata=\"{b}/.well-known/oauth-protected-resource/mcp\", scope=\"read\"");
            return (StatusCode::UNAUTHORIZED, [("www-authenticate", challenge)], "").into_response();
        }
        let result = match msg["method"].as_str() {
            Some("tools/list") => json!({"tools": [{"name": "whoami", "description": "Which token called", "inputSchema": {"type": "object"}}]}),
            Some("tools/call") => json!({"content": [{"type": "text", "text": valid}]}),
            Some(_) if msg.get("id").is_none() => return StatusCode::ACCEPTED.into_response(),
            _ => json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake"}}),
        };
        Json(json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})).into_response()
    });

    let b = base.clone();
    // A trailing slash, as Vercel's has: the client sends the resource as the server names it.
    let prm = get(move || async move {
        Json(json!({"resource": format!("{b}/mcp/"),"authorization_servers": [format!("{b}/auth")], "scopes_supported": ["read", "write"]}))
    });

    let iss = issuer.clone();
    let meta = get(move || async move {
        Json(json!({"issuer": iss, "authorization_endpoint": format!("{iss}/authorize"), "token_endpoint": format!("{iss}/token"),
            "registration_endpoint": format!("{iss}/register"), "code_challenge_methods_supported": ["S256"], "scopes_supported": ["openid", "offline_access"],
            "authorization_response_iss_parameter_supported": true, "client_id_metadata_document_supported": cimd}))
    });

    let st = s.clone();
    let register = post(move |Json(b): Json<Value>| async move {
        st.lock().unwrap().log.push(format!("register {}", b["redirect_uris"][0].as_str().unwrap_or("").starts_with("http://127.0.0.1:")));
        assert_eq!(b["token_endpoint_auth_method"], "none");
        (StatusCode::CREATED, Json(json!({"client_id": "client-1"})))
    });

    let (st, b, iss) = (s.clone(), base.clone(), issuer.clone());
    let authorize = get(move |Query(q): Query<HashMap<String, String>>| async move {
        let mut f = st.lock().unwrap();
        assert_eq!(q["client_id"], client);
        assert_eq!(q["redirect_uri"], egin::tools::oauth::redirect_uri(), "the redirect the client document lists");
        assert_eq!((q["response_type"].as_str(), q["code_challenge_method"].as_str()), ("code", "S256"));
        assert_eq!(q["resource"], format!("{b}/mcp/"), "the server's resource goes with the authorization request");
        assert_eq!(q["scope"], "read offline_access", "the 401's scope wins over scopes_supported, plus a refresh token");
        f.challenge = q["code_challenge"].clone();
        f.log.push("authorize".into());
        let back = format!("{}?code=code-1&state={}&iss={}", q["redirect_uri"], q["state"], iss);
        Redirect::to(&back)
    });

    let (st, b) = (s.clone(), base.clone());
    let token = post(move |Form(f): Form<HashMap<String, String>>| async move {
        let mut s = st.lock().unwrap();
        assert_eq!(f["resource"], format!("{b}/mcp/"), "and with the token request");
        assert_eq!(f["client_id"], client);
        let ok = match f["grant_type"].as_str() {
            "authorization_code" => f["code"] == "code-1" && URL_SAFE_NO_PAD.encode(Sha256::digest(f["code_verifier"].as_bytes())) == s.challenge,
            "refresh_token" => f["refresh_token"] == "refresh-1",
            _ => false,
        };
        if !ok {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"}))).into_response();
        }
        s.issued += 1;
        s.valid = format!("token-{}", s.issued);
        s.log.push(f["grant_type"].clone());
        Json(json!({"access_token": s.valid, "token_type": "Bearer", "expires_in": 3600, "refresh_token": "refresh-1"})).into_response()
    });

    let router: Router = Router::new()
        .route("/mcp", mcp)
        .route("/.well-known/oauth-protected-resource/mcp", prm)
        .route("/.well-known/oauth-authorization-server/auth", meta)
        .route("/auth/register", register)
        .route("/auth/authorize", authorize)
        .route("/auth/token", token);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, s)
}

#[tokio::test]
async fn connect_signs_in_and_the_mcp_client_uses_and_refreshes_the_token() {
    let (base, fake) = fake_server(false).await;
    let f = fx(vec![], Approve::Auto).await;
    f.write("connections/remote.toml", &format!("url = \"{base}/mcp\"\nauth = \"oauth\"\n"), false);
    let agent = f.agent("");
    let file = egin::tools::oauth::file(&agent.dir, "remote");

    // Before signing in, the connection says what to run.
    let err = egin::tools::oauth::access_token(&reqwest::Client::new(), &file, false).await.unwrap_err().to_string();
    assert!(err.contains("egin connect remote"), "{err}");

    // The "browser" follows the authorization URL and its redirect back to the local listener.
    let http = reqwest::Client::new();
    let t = egin::tools::oauth::login(&http, &file, &format!("{base}/mcp"), None, |link| {
        let link = link.to_string();
        tokio::spawn(async move { reqwest::get(link).await.unwrap() });
    })
    .await
    .unwrap();
    assert_eq!((t.access_token.as_str(), t.issuer), ("token-1", format!("{base}/auth")));
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600, "only the owner can read the token");
    }
    assert!(file.starts_with(agent.dir.join(".egin")), "tokens live in .egin/, not connections/");

    // The MCP client sends the token.
    assert_eq!(egin::tools::mcp::call(&f.app, &agent, "remote__whoami", &json!({})).await.as_deref(), Some("token-1"));

    // The server stops taking it: the 401 is answered by a refresh and one retry.
    fake.lock().unwrap().valid = "revoked".into();
    fake.lock().unwrap().issued = 1;
    assert_eq!(egin::tools::mcp::call(&f.app, &agent, "remote__whoami", &json!({})).await.as_deref(), Some("token-2"));
    assert_eq!(egin::tools::oauth::load(&file).unwrap().unwrap().access_token, "token-2", "the new token is stored");

    // A token about to expire is refreshed before it is sent.
    let mut stored = egin::tools::oauth::load(&file).unwrap().unwrap();
    stored.expires_at = Some(0);
    std::fs::write(&file, serde_json::to_string(&stored).unwrap()).unwrap();
    assert_eq!(egin::tools::oauth::access_token(&http, &file, false).await.unwrap(), "token-3");

    assert_eq!(fake.lock().unwrap().log, ["register true", "authorize", "authorization_code", "refresh_token", "refresh_token"]);

    // A server that takes client metadata documents gets egin's URL as the client id, with no registration.
    // (In this test, not its own: the redirect port is fixed, so two sign-ins can't overlap.)
    let (base2, fake2) = fake_server(true).await;
    let file2 = egin::tools::oauth::file(&agent.dir, "cimd");
    egin::tools::oauth::login(&http, &file2, &format!("{base2}/mcp"), None, |link| {
        let link = link.to_string();
        tokio::spawn(async move { reqwest::get(link).await.unwrap() });
    })
    .await
    .unwrap();
    assert_eq!(fake2.lock().unwrap().log, ["authorize", "authorization_code"]);
    assert_eq!(egin::tools::oauth::load(&file2).unwrap().unwrap().client_id, egin::tools::oauth::CLIENT_DOCUMENT);
}

#[test]
fn auth_must_be_oauth_and_needs_a_url() {
    let dir = std::env::temp_dir().join(format!("egin-oauth-{}", egin::new_id()));
    egin::agent::init(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), "sandbox = \"local\"\n").unwrap();
    std::fs::create_dir_all(dir.join("connections")).unwrap();
    std::fs::write(dir.join("connections/x.toml"), "command = [\"x\"]\nauth = \"oauth\"\n").unwrap();
    let err = egin::agent::Agent::load(&dir, &toml::Table::new()).err().unwrap().to_string();
    assert!(err.contains("needs a `url`"), "{err}");
    std::fs::write(dir.join("connections/x.toml"), "url = \"https://h/mcp\"\nauth = \"basic\"\n").unwrap();
    let err = egin::agent::Agent::load(&dir, &toml::Table::new()).err().unwrap().to_string();
    assert!(err.contains("unknown auth"), "{err}");
}
