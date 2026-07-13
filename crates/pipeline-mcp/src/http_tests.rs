//! End-to-end tests for the HTTP surface.
//!
//! # Why these exist, separately from the unit tests
//!
//! The bugs worth catching on this surface are INTEGRATION bugs — they live in the wiring
//! between well-tested parts, not in any one part:
//!
//! - `client_ip` took the wrong forwarded hop — correct in isolation, wrong in the request.
//! - a notification other than `initialized` fell through to "method not found" and killed
//!   strict clients — a routing gap, not a handler bug.
//! - a 405 that forgot its `Allow` header — visible only at the router level.
//!
//! So these drive the REAL router — the exact one `serve_http` serves, via
//! [`crate::http_transport::build_router`] — with `tower::oneshot`. No socket, no
//! subprocess, no environment: the config is injected as an [`AppState`] value, which is
//! also why they are deterministic and safe to run in parallel. (This crate forbids
//! `unsafe`, so a test cannot set an env var anyway — the value-injection design is what
//! makes the surface testable at all.)
//!
//! What is asserted here, end to end: the public/gated split · the auth flows (missing and
//! invalid bearers, `whoami`) · the MCP wire contract (initialize, tools/list, the whole
//! `notifications/*` namespace, GET→405) · the read-only capability gate · and rate
//! limiting with its published budget headers.

use crate::auth::TokenRegistry;
use crate::http_transport::{AppState, RemoteMode, build_router};
use crate::oauth::OAuth;
use crate::ratelimit::RateLimiter;
use crate::server::ServerState;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt; // oneshot

// ── fixture ──────────────────────────────────────────────────────────────────────────

/// A router over a temp OAuth-state dir. The `TempDir` must be held for the life of the
/// test — dropping it would pull the persisted token store out from under the router.
struct Fx {
    router: Router,
    #[allow(dead_code)] // held only to keep the temp dir alive for the router's lifetime
    dir: tempfile::TempDir,
}

fn fixture(mode: RemoteMode, burst: u32, per_sec: f64) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState {
        server: Arc::new(ServerState::new()),
        tokens: Arc::new(TokenRegistry::for_tests(&[
            ("alice", "sk-alice"),
            ("bob", "sk-bob"),
        ])),
        oauth: Arc::new(OAuth::new(dir.path().join(".oauth"))),
        limiter: Arc::new(RateLimiter::new(burst, per_sec)),
        mode,
    };
    Fx {
        router: build_router(state),
        dir,
    }
}

/// A permissive default: read-only, rate limit effectively unlimited.
fn ro() -> Fx {
    fixture(RemoteMode::ReadOnly, 100_000, 100_000.0)
}

// ── request/response helpers ───────────────────────────────────────────────────────────

const PEER: &str = "203.0.113.9:5000";

/// Build a request with `ConnectInfo` injected. `oneshot` does not set connect-info the
/// way `into_make_service_with_connect_info` does, so without this the `ConnectInfo`
/// extractor in every handler would 500 — inserting it is what makes the handler see a peer.
fn build(method: &str, uri: &str, headers: &[(&str, &str)], body: Body) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let mut r = b.body(body).unwrap();
    r.extensions_mut()
        .insert(ConnectInfo(PEER.parse::<SocketAddr>().unwrap()));
    r
}

const JSONH: (&str, &str) = ("content-type", "application/json");
fn bearer(tok: &str) -> (String, String) {
    ("authorization".into(), format!("Bearer {tok}"))
}

async fn call(fx: &Fx, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = fx.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// `GET uri` with a Bearer token.
async fn get_auth(fx: &Fx, uri: &str, tok: &str) -> (StatusCode, HeaderMap, String) {
    let (k, v) = bearer(tok);
    call(fx, build("GET", uri, &[(&k, &v)], Body::empty())).await
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

async fn mcp(fx: &Fx, tok: &str, payload: &str) -> (StatusCode, HeaderMap, String) {
    let (k, v) = bearer(tok);
    call(
        fx,
        build(
            "POST",
            "/mcp",
            &[(&k, &v), JSONH],
            Body::from(payload.to_owned()),
        ),
    )
    .await
}

// ══ public vs gated ════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn public_endpoints_need_no_token() {
    let fx = ro();
    let (s, _, body) = call(&fx, build("GET", "/health", &[], Body::empty())).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains(r#""name":"pipeline""#), "health body: {body}");
    assert!(body.contains(r#""version""#));

    let (s, _, _) = call(&fx, build("GET", "/version", &[], Body::empty())).await;
    assert_eq!(s, StatusCode::OK);

    // OAuth discovery MUST be reachable anonymously — claude.ai walks it before it holds
    // any token. A token check here would dead-end the connector handshake.
    let (s, _, body) = call(
        &fx,
        build(
            "GET",
            "/.well-known/oauth-authorization-server",
            &[],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("token_endpoint"), "metadata: {body}");
}

#[tokio::test]
async fn mcp_requires_a_valid_bearer() {
    let fx = ro();
    let list = || r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    let (s, _, _) = call(&fx, build("POST", "/mcp", &[JSONH], Body::from(list()))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "no token must be 401");

    let bad = bearer("sk-not-a-real-token");
    let (s, _, _) = call(
        &fx,
        build(
            "POST",
            "/mcp",
            &[(&bad.0, &bad.1), JSONH],
            Body::from(list()),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "bad token must be 401");

    let ok = bearer("sk-alice");
    let (s, _, _) = call(
        &fx,
        build("POST", "/mcp", &[(&ok.0, &ok.1), JSONH], Body::from(list())),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "valid token must pass");
}

#[tokio::test]
async fn whoami_names_the_principal() {
    let fx = ro();
    let (s, _, body) = get_auth(&fx, "/tokens/whoami", "sk-alice").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains(r#""token":"alice""#), "whoami: {body}");

    let (s, _, _) = call(&fx, build("GET", "/tokens/whoami", &[], Body::empty())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

// ══ MCP wire contract ═══════════════════════════════════════════════════════════════════

#[tokio::test]
async fn mcp_initialize_and_tools_list() {
    let fx = ro();
    let (s, _, body) = mcp(
        &fx,
        "sk-alice",
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("serverInfo") && body.contains("pipeline-mcp"));

    let (_, _, body) = mcp(
        &fx,
        "sk-alice",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let tools = v["result"]["tools"].as_array().expect("tools array");
    assert_eq!(
        tools.len(),
        19,
        "the advertised tool count is part of the contract"
    );
}

#[tokio::test]
async fn the_whole_notifications_namespace_takes_202_with_no_body() {
    let fx = ro();
    // Not just the one member we happened to special-case first.
    for method in [
        "notifications/initialized",
        "notifications/cancelled",
        "notifications/progress",
    ] {
        let (s, _, body) = mcp(
            &fx,
            "sk-alice",
            &format!(r#"{{"jsonrpc":"2.0","method":"{method}"}}"#),
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{method} must be 202");
        assert!(body.is_empty(), "{method} must have an empty body");
    }
}

#[tokio::test]
async fn get_on_mcp_is_405_with_allow() {
    // Routing-level, before auth — so no token needed.
    let fx = ro();
    let (s, h, _) = call(&fx, build("GET", "/mcp", &[], Body::empty())).await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    assert!(
        header(&h, "allow").unwrap_or("").contains("POST"),
        "405 must advertise POST in Allow"
    );
}

#[tokio::test]
async fn read_only_blocks_a_destructive_action_but_allows_a_safe_one() {
    let fx = fixture(RemoteMode::ReadOnly, 100_000, 100_000.0);

    let destructive = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"pipeline_docker","arguments":{"action":"build"}}}"#;
    let (s, _, body) = mcp(&fx, "sk-alice", destructive).await;
    assert_eq!(s, StatusCode::OK); // a JSON-RPC error rides inside a 200
    assert!(
        body.contains("blocked by PIPELINE_REMOTE_MODE=read_only"),
        "a destructive action must be blocked in read-only mode: {body}"
    );

    let safe = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"pipeline_meta","arguments":{"action":"version"}}}"#;
    let (s, _, body) = mcp(&fx, "sk-alice", safe).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !body.contains("blocked by PIPELINE_REMOTE_MODE"),
        "a safe action must not be blocked: {body}"
    );
}

// ══ rate limiting ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn the_rate_limit_bites_and_publishes_the_budget() {
    // burst 3, no meaningful refill during the test.
    let fx = fixture(RemoteMode::ReadOnly, 3, 0.001);
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    // First three succeed, and the budget is visible on SUCCESS (not only on the 429).
    for i in 1..=3 {
        let (s, h, _) = mcp(&fx, "sk-alice", list).await;
        assert_eq!(s, StatusCode::OK, "request {i} should pass");
        assert_eq!(header(&h, "x-ratelimit-limit"), Some("3"));
        assert!(header(&h, "x-ratelimit-remaining").is_some());
    }

    // Fourth is refused with an honest Retry-After.
    let (s, h, _) = mcp(&fx, "sk-alice", list).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        header(&h, "retry-after").is_some(),
        "429 must carry Retry-After"
    );
}

#[tokio::test]
async fn the_rate_limit_isolates_principals_on_a_shared_ip() {
    // burst 1 — alice's single token is spent immediately.
    let fx = fixture(RemoteMode::ReadOnly, 1, 0.001);
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    assert_eq!(mcp(&fx, "sk-alice", list).await.0, StatusCode::OK);
    assert_eq!(
        mcp(&fx, "sk-alice", list).await.0,
        StatusCode::TOO_MANY_REQUESTS,
        "alice is now over her limit"
    );
    // bob shares the same PEER ip but must have his own bucket.
    assert_eq!(
        mcp(&fx, "sk-bob", list).await.0,
        StatusCode::OK,
        "bob must not be starved by alice on the same ip"
    );
}
