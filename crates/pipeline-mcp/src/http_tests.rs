//! End-to-end tests for the HTTP surface.
//!
//! # Why these exist, separately from the unit tests
//!
//! Every bug found while porting Folio this session was an INTEGRATION bug that the
//! unit tests could not have caught — because each lived in the wiring between
//! well-tested parts, not in any one part:
//!
//! - `/library/` (trailing slash) 404'd — a routing gap, not a `render` bug.
//! - the `?token=` hand-off redirected to a dead `/library/` — a handler/route mismatch.
//! - the gallery walked symlinks — a listing path that bypassed `resolve`.
//! - `client_ip` took the wrong forwarded hop — correct in isolation, wrong in the request.
//!
//! So these drive the REAL router — the exact one `serve_http` serves, via
//! [`crate::http_transport::build_router`] — with `tower::oneshot`. No socket, no
//! subprocess, no environment: the config is injected as an [`AppState`] value, which is
//! also why they are deterministic and safe to run in parallel. (This crate forbids
//! `unsafe`, so a test cannot set an env var anyway — the value-injection design is what
//! makes the surface testable at all.)
//!
//! What is asserted here, end to end: the public/gated split · the auth flows (Bearer,
//! the `?token=`→cookie hand-off, the no-`WWW-Authenticate` invariant) · path containment
//! at the HTTP layer · the MCP wire contract · rate limiting and its headers · every file
//! operation · the read-only and write-disabled gates · and the CSRF guard.

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
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt; // oneshot

// ── fixture ──────────────────────────────────────────────────────────────────────────

/// A router over a freshly-seeded temp library. The `TempDir` must be held for the life
/// of the test — dropping it deletes the record out from under the router.
struct Fx {
    router: Router,
    dir: tempfile::TempDir,
}

fn seed(root: &Path) {
    use std::fs;
    fs::create_dir_all(root.join("digests")).unwrap();
    fs::create_dir_all(root.join("reports")).unwrap();
    fs::create_dir_all(root.join(".oauth")).unwrap();
    fs::write(
        root.join("digests/folio.json"),
        r#"{"alias":"folio","summary":{"total_files":5263,"languages":{"typescript":492,"yaml":973}}}"#,
    )
    .unwrap();
    fs::write(root.join("reports/run-green.json"), r#"{"status":"pass"}"#).unwrap();
    fs::write(root.join("reports/run-red.json"), r#"{"status":"failed"}"#).unwrap();
    // Not on the inline whitelist → must never be served.
    fs::write(root.join("memory.db"), b"SQLite format 3\0").unwrap();
    // The live credential store — must never be listed, served, renamed, or deleted.
    fs::write(
        root.join(".oauth/access-tokens.json"),
        r#"{"sk-x":"alice"}"#,
    )
    .unwrap();
    // A symlink out of the root — a naive listing would follow it.
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
}

fn fixture(mode: RemoteMode, writable: bool, burst: u32, per_sec: f64) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let state = AppState {
        server: Arc::new(ServerState::new()),
        tokens: Arc::new(TokenRegistry::for_tests(&[
            ("alice", "sk-alice"),
            ("bob", "sk-bob"),
        ])),
        oauth: Arc::new(OAuth::new(dir.path().join(".oauth"))),
        limiter: Arc::new(RateLimiter::new(burst, per_sec)),
        library: Arc::new(dir.path().to_path_buf()),
        mode,
        writable: writable.then(crate::fsops::Writable::granted),
    };
    Fx {
        router: build_router(state),
        dir,
    }
}

/// A permissive default: read-only, writes off, rate limit effectively unlimited.
fn ro() -> Fx {
    fixture(RemoteMode::ReadOnly, false, 100_000, 100_000.0)
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

/// `POST /library/op` with a Bearer token and a JSON body.
async fn op_auth(fx: &Fx, tok: &str, body: serde_json::Value) -> (StatusCode, HeaderMap, String) {
    let (k, v) = bearer(tok);
    call(
        fx,
        build(
            "POST",
            "/library/op",
            &[(&k, &v), JSONH],
            Body::from(body.to_string()),
        ),
    )
    .await
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
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

// ══ the library gate + hand-off ═════════════════════════════════════════════════════════

#[tokio::test]
async fn the_library_gate_shows_a_page_and_never_a_browser_popup() {
    let fx = ro();
    let (s, h, body) = call(&fx, build("GET", "/library", &[], Body::empty())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // ! THE invariant: no WWW-Authenticate. A browser basic-auth popup cannot be satisfied
    // by an access token, so offering one is a dead end — we serve a gate page instead.
    assert!(
        header(&h, "www-authenticate").is_none(),
        "a WWW-Authenticate header would trigger the popup we exist to avoid"
    );
    assert!(!body.is_empty(), "the gate must render a page");
}

#[tokio::test]
async fn library_root_and_a_typed_trailing_slash_both_render() {
    // Regression: `/library/` matched neither `/library` nor `/library/{*path}` and 404'd.
    let fx = ro();
    for uri in ["/library", "/library/"] {
        let (s, _, body) = get_auth(&fx, uri, "sk-alice").await;
        assert_eq!(s, StatusCode::OK, "{uri} must render");
        assert!(body.contains("Pipeline"), "{uri} is not the file manager");
    }
}

#[tokio::test]
async fn the_token_handoff_becomes_a_cookie_and_drops_the_token() {
    let fx = ro();
    let (s, h, _) = call(
        &fx,
        build("GET", "/library?token=sk-alice", &[], Body::empty()),
    )
    .await;
    assert_eq!(s, StatusCode::FOUND, "?token= must redirect");

    // Regression: this Location used to carry a trailing slash and 404 on arrival.
    let loc = header(&h, "location").unwrap();
    assert_eq!(loc, "/library", "must land on a real route");
    assert!(
        !loc.contains("token"),
        "the token must be dropped from the URL"
    );

    let sc = header(&h, "set-cookie").expect("a session cookie");
    assert!(sc.contains(crate::oauth::SESSION_COOKIE));
    assert!(sc.contains("HttpOnly"), "cookie must be HttpOnly");
    assert!(sc.contains("SameSite=Lax"));
    // The cookie must NOT be the API key — it carries a minted session token.
    assert!(
        !sc.contains("sk-alice"),
        "the API key must never be the cookie value"
    );

    // And the cookie it minted actually authenticates the follow-up request.
    let cookie = sc.split(';').next().unwrap();
    let (s, _, body) = call(
        &fx,
        build("GET", "/library", &[("cookie", cookie)], Body::empty()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "the minted cookie must authenticate");
    assert!(body.contains("Pipeline"));
}

// ══ containment at the HTTP layer ═══════════════════════════════════════════════════════

#[tokio::test]
async fn a_whitelisted_file_is_served_and_others_are_refused() {
    let fx = ro();

    let (s, h, body) = get_auth(&fx, "/library/digests/folio.json", "sk-alice").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(header(&h, "content-type"), Some("application/json"));
    assert!(body.contains("folio"));

    // memory.db is not on the inline whitelist → 404, never handed over.
    let (s, _, _) = get_auth(&fx, "/library/memory.db", "sk-alice").await;
    assert_eq!(s, StatusCode::NOT_FOUND, "a non-whitelisted file must 404");

    // ?download=1 → save, not render.
    let (s, h, _) = get_auth(
        &fx,
        "/library/reports/run-green.json?download=1",
        "sk-alice",
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cd = header(&h, "content-disposition").unwrap_or("");
    assert!(
        cd.contains("attachment"),
        "download must set attachment: {cd}"
    );
    assert!(cd.contains("run-green.json"));
}

#[tokio::test]
async fn traversal_the_token_store_and_symlinks_are_all_refused() {
    let fx = ro();
    let mut cases = vec![
        "/library/reports/../../etc/passwd",         // raw ..
        "/library/reports/%2e%2e/%2e%2e/etc/passwd", // encoded ..
        "/library/.oauth/access-tokens.json",        // dotfile token store
        "/library/oauth/access-tokens.json",         // deny-listed name
    ];
    #[cfg(unix)]
    cases.push("/library/escape/passwd"); // symlink → /etc

    for uri in cases {
        let (s, _, _) = get_auth(&fx, uri, "sk-alice").await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri} must be refused");
    }

    // And the token store is not even mentioned in the root listing.
    let (_, _, body) = get_auth(&fx, "/library", "sk-alice").await;
    assert!(
        !body.contains(".oauth"),
        "token store leaked into the listing"
    );
    assert!(
        !body.contains("sk-x"),
        "a token value leaked into the listing"
    );
}

// ══ MCP wire contract ═══════════════════════════════════════════════════════════════════

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
    let fx = fixture(RemoteMode::ReadOnly, false, 100_000, 100_000.0);

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
    let fx = fixture(RemoteMode::ReadOnly, false, 3, 0.001);
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
    let fx = fixture(RemoteMode::ReadOnly, false, 1, 0.001);
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

// ══ file operations ═════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn writes_are_refused_when_the_switch_is_off() {
    let fx = ro(); // writable = false
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"mkdir","path":"","name":"x"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "op must be 403 with writes off");

    let up = bearer("sk-alice");
    let (s, _, _) = call(
        &fx,
        build(
            "POST",
            "/library/upload?dir=&name=x.json",
            &[(&up.0, &up.1)],
            Body::from("{}"),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "upload must be 403 with writes off"
    );
}

#[tokio::test]
async fn the_full_file_operation_cycle_works_and_delete_never_destroys() {
    let fx = fixture(RemoteMode::ReadOnly, true, 100_000, 100_000.0);
    let root = fx.dir.path().to_path_buf();

    // mkdir
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"mkdir","path":"","name":"archive"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(root.join("archive").is_dir());

    // upload
    let up = bearer("sk-alice");
    let (s, _, _) = call(
        &fx,
        build(
            "POST",
            "/library/upload?dir=archive&name=note.json",
            &[(&up.0, &up.1)],
            Body::from(r#"{"note":"hello"}"#),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(root.join("archive/note.json")).unwrap(),
        r#"{"note":"hello"}"#
    );

    // rename (in place)
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"rename","path":"archive/note.json","name":"renamed.json"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(root.join("archive/renamed.json").exists());
    assert!(!root.join("archive/note.json").exists());

    // move into another dir
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"move","path":"archive/renamed.json","dest":"reports"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(root.join("reports/renamed.json").exists());

    // delete → MUST be a move to trash/, and the bytes MUST survive.
    let (s, _, body) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"delete","path":"reports/renamed.json"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let trashed = v["trashed_to"].as_str().expect("trashed_to path");
    assert!(trashed.starts_with("trash/"));
    assert!(
        !root.join("reports/renamed.json").exists(),
        "must leave the original spot"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(trashed)).unwrap(),
        r#"{"note":"hello"}"#,
        "delete must preserve the bytes — it is a move, not an unlink"
    );
}

#[tokio::test]
async fn write_abuse_is_refused() {
    let fx = fixture(RemoteMode::ReadOnly, true, 100_000, 100_000.0);
    let root = fx.dir.path().to_path_buf();

    // A rename cannot relocate a file out of the root.
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"rename","path":"reports/run-green.json","name":"../../../etc/x"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // A rename cannot recreate/shadow the token store name.
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"rename","path":"reports/run-green.json","name":".oauth"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // The token store cannot be deleted.
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"delete","path":".oauth/access-tokens.json"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(
        root.join(".oauth/access-tokens.json").exists(),
        "the token store must be untouched"
    );

    // An unknown op is a client error, not a 500.
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"chmod","path":"x"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_csrf_guard_blocks_a_cross_origin_cookie_write() {
    let fx = fixture(RemoteMode::ReadOnly, true, 100_000, 100_000.0);
    let root = fx.dir.path().to_path_buf();

    // Get a real session cookie via the hand-off.
    let (_, h, _) = call(
        &fx,
        build("GET", "/library?token=sk-alice", &[], Body::empty()),
    )
    .await;
    let cookie = header(&h, "set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let mkdir = |name: &str| serde_json::json!({"op":"mkdir","path":"","name":name}).to_string();

    // Cookie + a cross-site Origin → refused. This is the attack SameSite is the first
    // line against and the Origin check is the backstop for.
    let (s, _, _) = call(
        &fx,
        build(
            "POST",
            "/library/op",
            &[
                ("cookie", &cookie),
                ("origin", "https://evil.example"),
                JSONH,
            ],
            Body::from(mkdir("csrf")),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a cross-origin cookie write must be refused"
    );
    assert!(
        !root.join("csrf").exists(),
        "the CSRF write must not have happened"
    );

    // Cookie + a matching Origin → allowed.
    let (s, _, _) = call(
        &fx,
        build(
            "POST",
            "/library/op",
            &[
                ("cookie", &cookie),
                ("host", "pipe.test"),
                ("origin", "https://pipe.test"),
                JSONH,
            ],
            Body::from(mkdir("csrf-ok")),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "a same-origin cookie write must be allowed"
    );
    assert!(root.join("csrf-ok").is_dir());

    // A Bearer script sends no Origin and needs none — it cannot be CSRF'd.
    let (s, _, _) = op_auth(
        &fx,
        "sk-alice",
        serde_json::json!({"op":"mkdir","path":"","name":"bearer-ok"}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "a Bearer write must be unaffected by the Origin rule"
    );
    assert!(root.join("bearer-ok").is_dir());
}
