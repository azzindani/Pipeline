//! HTTP transport · streamable-http-style MCP server over JSON-RPC.
//!
//! Modern MCP "Streamable HTTP" transport (spec 2025-03-26). Clients POST
//! a JSON-RPC envelope to `/mcp`, server returns the JSON-RPC response.
//! No SSE upgrade yet · single request → single response. Streaming
//! progress for long-running tools lands when an actual tool needs it
//! (RE family already uses file-backed async via `re_status` polling).
//!
//! Activated via `--transport http` or `PIPELINE_TRANSPORT=http` + bind
//! address from `--bind` or `PIPELINE_BIND` (default 127.0.0.1:8080).
//!
//! # The endpoint contract
//!
//! Deliberately identical to Folio's and Sift's, so a client — or a monitor, or a
//! key — configured for one works against any of them.
//!
//! | Path | Gate |
//! |---|---|
//! | `/mcp` | Bearer · static registry or OAuth-issued |
//! | `/tokens/whoami` | Bearer · the cheapest auth sanity check |
//! | `/library`, `/library/*` | the SAME key · `?token=` → session cookie |
//! | `/.well-known/oauth-*` | public — discovery |
//! | `/oauth/{register,authorize,token}` | public — PKCE handshake |
//! | `/health`, `/version` | public — a monitor should not need a token |
//!
//! # Security
//!
//! Treat this endpoint as remote code execution.
//!
//! - **Auth is mandatory.** `PIPELINE_TOKENS_FILE` | `PIPELINE_TOKENS` |
//!   `PIPELINE_TOKEN` — the server refuses to start with none set. Unlike Folio and
//!   Sift, there is no unauthenticated mode; a misconfigured lock must be a locked
//!   door, not an open one.
//! - **`PIPELINE_REMOTE_MODE=read_only`** (default) blocks every destructive
//!   action in `is_safe_action`. `full` only behind an authenticated proxy + TLS.
//! - **Bodies are capped.** An unbounded reader on a public endpoint lets one
//!   POST grow the heap until the container OOMs. `/mcp` gets a generous cap
//!   (tool args can be large); the pre-auth OAuth surface gets a tight one.
//! - **Rate limited** ([`crate::ratelimit`]) on `(principal, ip)` → 429.
//! - **OAuth 2.0 + PKCE** ([`crate::oauth`]) lets claude.ai connect as a Custom
//!   Connector instead of a human pasting a static bearer.
//! - **No basic_auth anywhere**, including the library. A browser username/password
//!   popup can never be satisfied by an access token, and a second credential to read
//!   your own record defeats the point of one key everywhere.

#![allow(clippy::doc_markdown)]

use crate::auth::{TokenRegistry, bearer};
use crate::dispatch;
use crate::oauth::{OAUTH_MAX_BODY_BYTES, OAuth};
use crate::ratelimit::RateLimiter;
use crate::registry::registry;
use crate::server::ServerState;
use crate::tools::ToolRequest;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// `/mcp` body cap. Tool arguments (file contents, digests, patches) are
/// legitimately large, so this is generous — but it is not unbounded.
const MCP_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// ! The pre-auth surface must always be capped tighter than the authenticated
/// one. Enforced at compile time so nobody can widen `OAUTH_MAX_BODY_BYTES` past
/// `/mcp` and hand an anonymous caller the bigger allocation.
const _: () = assert!(OAUTH_MAX_BODY_BYTES < MCP_MAX_BODY_BYTES);

fn mcp_body_limit() -> usize {
    std::env::var("PIPELINE_MAX_BODY_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MCP_MAX_BODY_BYTES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteMode {
    /// Default · agent can call only read-only actions.
    ReadOnly,
    /// Full surface · agent can call destructive actions.
    /// Use behind authenticated reverse proxy + TLS only.
    Full,
}

impl RemoteMode {
    pub fn from_env() -> Self {
        match std::env::var("PIPELINE_REMOTE_MODE")
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Ok("full") => Self::Full,
            _ => Self::ReadOnly,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Full => "full",
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) server: Arc<ServerState>,
    pub(crate) tokens: Arc<TokenRegistry>,
    pub(crate) oauth: Arc<OAuth>,
    pub(crate) limiter: Arc<RateLimiter>,
    pub(crate) library: Arc<PathBuf>,
    pub(crate) mode: RemoteMode,
}

/// The library root — Pipeline's durable record: run history, reports, digests,
/// sessions. The same directory the memory volume persists.
fn library_dir() -> PathBuf {
    std::env::var_os("PIPELINE_LIBRARY_DIR").map_or_else(
        || {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".pipeline")
        },
        PathBuf::from,
    )
}

/// Where OAuth persists its access + refresh stores.
///
/// ! Dot-prefixed ON PURPOSE. It sits INSIDE the library root, and `browse` hides
/// dotfiles — otherwise `/library/oauth/access-tokens.json` would serve every reader a
/// live credential. (`browse::DENY` also names it, belt and braces.) Sift calls its
/// equivalent `.oauth-state` for exactly this reason.
fn oauth_state_dir() -> PathBuf {
    std::env::var_os("PIPELINE_OAUTH_STATE_DIR")
        .map_or_else(|| library_dir().join(".oauth"), PathBuf::from)
}

/// Can we actually create + write in `dir`? Checked at boot so a read-only mount
/// is loud rather than silent.
fn probe_writable(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".write-probe");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

/// Run the HTTP MCP server. Bind defaults to 127.0.0.1:8080 if `bind` is None.
///
/// Returns `Err` immediately if no token source is configured — Pipeline refuses
/// to expose remote code execution unauthenticated.
pub async fn serve_http(bind: Option<&str>) -> Result<(), crate::McpError> {
    let tokens = TokenRegistry::from_env().map_err(crate::McpError::Transport)?;

    // ! Fail loudly if the OAuth store is not writable.
    //
    // Persisting is best-effort by design (a failed write costs a re-authorize,
    // not correctness), which means a read-only mount fails SILENTLY: the
    // container reports healthy, serves traffic, and quietly forces every client
    // to re-authorize on each restart. A root-owned volume did exactly this in
    // production. Surface it at boot instead of never.
    let state_dir = oauth_state_dir();
    if let Err(e) = probe_writable(&state_dir) {
        eprintln!(
            "pipeline-mcp · WARNING · OAuth state dir {} is not writable: {e}\n\
             ·  access + refresh tokens will NOT survive a restart, so every client\n\
             ·  must re-authorize on each bounce. Fix: chown it to the container\n\
             ·  user (uid 10001), or point PIPELINE_OAUTH_STATE_DIR somewhere writable.",
            state_dir.display()
        );
    }

    let addr_str = bind
        .map(str::to_owned)
        .or_else(|| std::env::var("PIPELINE_BIND").ok())
        .unwrap_or_else(|| "127.0.0.1:8080".into());
    let addr = SocketAddr::from_str(&addr_str)
        .map_err(|e| crate::McpError::Transport(format!("bad bind '{addr_str}': {e}")))?;

    let mode = RemoteMode::from_env();
    let limiter = Arc::new(RateLimiter::from_env());
    let app_state = AppState {
        server: Arc::new(ServerState::new()),
        oauth: Arc::new(OAuth::new(oauth_state_dir())),
        tokens: Arc::new(tokens),
        library: Arc::new(library_dir()),
        limiter: Arc::clone(&limiter),
        mode,
    };

    // The OAuth + discovery surface is deliberately PUBLIC and cross-origin:
    // claude.ai walks it anonymously to discover, register, and run PKCE before
    // any user has authenticated. The bearer it receives then gates /mcp.
    let public_cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let public = Router::new()
        .route(
            "/.well-known/oauth-authorization-server",
            get(crate::oauth::metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(crate::oauth::protected_resource),
        )
        .route("/oauth/register", post(crate::oauth::register))
        .route(
            "/oauth/authorize",
            get(crate::oauth::authorize_get).post(crate::oauth::authorize_post),
        )
        .route("/oauth/token", post(crate::oauth::token))
        // ! tight cap — these are unauthenticated endpoints
        .layer(DefaultBodyLimit::max(OAUTH_MAX_BODY_BYTES))
        .layer(public_cors);

    // THE LIBRARY — the durable record, browsable.
    //
    // ! No basic_auth in front and no static file_server: Pipeline is the SOLE gate,
    // exactly as Folio's editor route and Sift's /library are. It accepts the SAME key
    // via ?token= / Bearer / the session cookie and serves its own gate page otherwise.
    // A browser basic-auth popup could never be satisfied by an access token anyway, and
    // handing someone a SECOND credential to read their own record defeats the point of
    // one key everywhere. Its own body limit stays small — these are GETs.
    let library = Router::new()
        .route("/library", get(library_handler))
        .route("/library/{*path}", get(library_handler))
        .layer(DefaultBodyLimit::max(64 * 1024));

    // ✗ never print token values — only the principal names.
    let auth_summary = app_state.tokens.describe();

    let app = Router::new()
        .route("/mcp", post(mcp_handler))
        .layer(DefaultBodyLimit::max(mcp_body_limit()))
        .route("/health", get(health))
        .route("/version", get(version))
        .route("/tokens/whoami", get(whoami))
        .merge(library)
        .merge(public)
        .with_state(app_state);

    eprintln!(
        "pipeline-mcp http transport · {addr} · mode={} · auth={auth_summary} · \
         rate={} · oauth=/oauth/authorize · library=/library",
        mode.as_str(),
        limiter.describe(),
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::McpError::Transport(format!("bind {addr}: {e}")))?;
    axum::serve(listener, app)
        .await
        .map_err(|e| crate::McpError::Transport(format!("serve: {e}")))?;
    Ok(())
}

/// Liveness — public on purpose. A monitor should not need a token to see the box is up,
/// and this leaks nothing: a version, a mode, and the NAMES of configured principals.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "status": "ok",
        "transport": "http",
        "name": "pipeline",
        "version": crate::VERSION,
        "mode": state.mode.as_str(),
        "auth": state.tokens.describe(),
        "rate_limit": state.limiter.describe(),
    }))
}

/// Running version — public, so a monitor can alert on a stale deploy without a token.
/// Same shape as Sift's and Folio's, so one probe works against all three.
async fn version() -> impl IntoResponse {
    Json(json!({"name": "pipeline", "version": crate::VERSION}))
}

/// Which named token you presented. The cheapest possible auth sanity check.
async fn whoami(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match authenticate(&state, &headers) {
        Some(principal) => Json(json!({"token": principal, "authenticated": true})).into_response(),
        None => unauthorized_json(&Value::Null),
    }
}

/// JSON-RPC request handler. Matches the methods the stdio handler
/// already supports: initialize · notifications/initialized · tools/list · tools/call · ping.
#[allow(clippy::too_many_lines)] // single dispatch pivot · splitting hurts readability
async fn mcp_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Response {
    let Some(principal) = authenticate(&state, &headers) else {
        return unauthorized_json(&req.get("id").cloned().unwrap_or(Value::Null));
    };

    if !state.limiter.allow(&principal, &client_ip(&headers)) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "1")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.get("id").cloned().unwrap_or(Value::Null),
                "error": {
                    "code": -32029,
                    "message": format!(
                        "rate limited · over {} · back off, or raise PIPELINE_RATE_BURST / \
                         PIPELINE_RATE_PER_SEC (0 disables)",
                        state.limiter.describe()
                    ),
                },
            })),
        )
            .into_response();
    }

    // Audit at INFO, not debug. Named tokens exist so the log can say WHO called a tool,
    // not merely that someone did — a line nobody reads is not an audit trail.
    let method_name = req.get("method").and_then(Value::as_str).unwrap_or("");
    tracing::info!(token = %principal, method = %method_name, "mcp");

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "pipeline-mcp", "version": crate::VERSION},
                    "capabilities": {"tools": {"listChanged": false}},
                    "instructions": format!(
                        "Pipeline remote MCP · mode={} · all destructive actions {} \
                         · 19 super tools · see tools/list for the full surface.",
                        state.mode.as_str(),
                        if state.mode == RemoteMode::Full { "ENABLED" } else { "BLOCKED" },
                    ),
                },
            });
            Json(resp).into_response()
        }
        "ping" => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
        "notifications/initialized" | "initialized" => StatusCode::NO_CONTENT.into_response(),
        "tools/list" => {
            let tools = build_tool_list();
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": tools},
            }))
            .into_response()
        }
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let arguments = params.get("arguments").cloned().unwrap_or_default();
            let action = arguments
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();

            // Capability gate · enforce read-only mode before dispatching.
            if state.mode == RemoteMode::ReadOnly && !is_safe_action(&name, &action) {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string(&json!({
                                "ok": false,
                                "data": {},
                                "next_suggested": [],
                                "memory_refs": [],
                                "error": format!(
                                    "blocked by PIPELINE_REMOTE_MODE=read_only · '{name}.{action}' is destructive · \
                                     unlock by setting PIPELINE_REMOTE_MODE=full only when behind authenticated proxy + TLS"
                                ),
                            })).unwrap_or_else(|_| "{}".into()),
                        }],
                        "isError": true,
                    },
                });
                return Json(resp).into_response();
            }

            let inner_args = arguments.get("args").cloned().unwrap_or(Value::Null);
            let tool_req = ToolRequest {
                action,
                args: inner_args,
            };
            let resp = dispatch::call_tool(&name, tool_req, state.server.clone()).await;
            let is_error = !resp.ok;
            let payload = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{"type": "text", "text": payload}],
                    "isError": is_error,
                },
            }))
            .into_response()
        }
        other => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("method '{other}' not found")},
        }))
        .into_response(),
    }
}

/// 401 with the RFC 9728 discovery hint — how claude.ai finds the OAuth surface from a
/// bare 401 instead of just failing.
fn unauthorized_json(id: &Value) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            r#"Bearer resource_metadata="/.well-known/oauth-protected-resource""#,
        )],
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32001,
                "message": "missing or invalid bearer token · send 'Authorization: Bearer <token>', \
                            or complete the OAuth flow at /oauth/authorize",
            },
        })),
    )
        .into_response()
}

/// Best guess at the caller's address, for rate-limit keying.
///
/// Behind the shared caddy-router the socket peer is always the router, so keying on it
/// would put every client in one bucket. Prefer what the proxy forwarded.
///
/// ! `X-Forwarded-For` is client-settable when nothing trusted sits in front. The blast
/// radius is small — a caller must ALREADY hold a valid token to be rate-limited at all,
/// so spoofing only lets a legitimate holder evade their own ceiling, never bypass auth.
fn client_ip(headers: &HeaderMap) -> String {
    for h in ["x-real-ip", "x-forwarded-for"] {
        if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
            if let Some(first) = v.split(',').next().map(str::trim) {
                if !first.is_empty() {
                    return first.to_owned();
                }
            }
        }
    }
    "unknown".to_owned()
}

/// Read one cookie out of the `Cookie` header.
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

/// Browse the durable record: run history, reports, digests, sessions.
///
/// Auth here is the same key as everywhere else, by three routes: `Authorization: Bearer`
/// (scripts), a `?token=` hand-off (browsers), or the session cookie it leaves behind.
#[allow(clippy::too_many_lines)] // one auth→resolve→serve pivot · splitting scatters the gate
async fn library_handler(
    State(state): State<AppState>,
    // `/library` carries no path param and `/library/{*path}` does — Option covers both
    // with one handler rather than two near-identical ones.
    path: Option<axum::extract::Path<String>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let rel = path.map(|axum::extract::Path(p)| p).unwrap_or_default();
    let url_path = format!("/library/{rel}");

    // Hand-off: a valid ?token= is swapped for a cookie and the token is dropped from the
    // URL, so it does not linger in history, server logs, or a shared screenshot.
    if let Some(presented) = q.get("token").filter(|t| !t.is_empty()) {
        let Some(principal) = state
            .tokens
            .lookup(presented)
            .map(str::to_owned)
            .or_else(|| state.oauth.resolve(presented))
        else {
            return (
                StatusCode::UNAUTHORIZED,
                Html(crate::browse::gate_page(&url_path)),
            )
                .into_response();
        };

        // ! the cookie carries a MINTED session token, never the API key itself
        let session = state.oauth.mint_session(&principal);
        let secure = headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|p| p.starts_with("https"));
        let set = format!(
            "{}={session}; Max-Age={}; Path=/library; HttpOnly; SameSite=Lax{}",
            crate::oauth::SESSION_COOKIE,
            crate::oauth::session_ttl_secs(),
            if secure { "; Secure" } else { "" },
        );
        return (
            StatusCode::FOUND,
            [
                (axum::http::header::LOCATION, url_path.as_str()),
                (axum::http::header::SET_COOKIE, set.as_str()),
            ],
            Html(String::new()),
        )
            .into_response();
    }

    // Bearer, or the session cookie left by the hand-off above.
    let principal = authenticate(&state, &headers).or_else(|| {
        cookie(&headers, crate::oauth::SESSION_COOKIE).and_then(|c| state.oauth.resolve(&c))
    });
    let Some(principal) = principal else {
        // ! a plain 401 with NO WWW-Authenticate — a browser basic-auth popup cannot
        // carry a bearer, so offering one is a dead end. Serve the gate page instead.
        return (
            StatusCode::UNAUTHORIZED,
            Html(crate::browse::gate_page(&url_path)),
        )
            .into_response();
    };

    if !state.limiter.allow(&principal, &client_ip(&headers)) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "1")],
            Html(crate::browse::render(
                &url_path,
                &[],
                "Rate limited. Slow down.",
            )),
        )
            .into_response();
    }

    let Some(target) = crate::browse::resolve(&state.library, &rel) else {
        return (
            StatusCode::NOT_FOUND,
            Html(crate::browse::render(&url_path, &[], "Not found.")),
        )
            .into_response();
    };

    if target.is_dir() {
        let entries = crate::browse::listing(&target, &url_path);
        return Html(crate::browse::render(
            &url_path,
            &entries,
            "Empty. Nothing has been recorded here yet.",
        ))
        .into_response();
    }

    // Only whitelisted types are served — ✗ hand out memory.db or an arbitrary binary.
    let ext = target
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(media) = crate::browse::inline_type(&ext) else {
        return (
            StatusCode::NOT_FOUND,
            Html(crate::browse::render(
                &url_path,
                &[],
                "Not a readable file.",
            )),
        )
            .into_response();
    };

    match tokio::fs::read(&target).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, media)],
            bytes,
        )
            .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Html(crate::browse::render(&url_path, &[], "Unreadable.")),
        )
            .into_response(),
    }
}

/// Resolve a request's bearer to a principal, or `None`.
///
/// Two passes, mirroring Folio: the static registry first (a token an operator
/// configured), then the OAuth store (a token this server minted). Both map to
/// the same principal namespace, so an OAuth grant is exactly as privileged as
/// the token whose holder authorized it — no more.
fn authenticate(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let presented = bearer(headers)?;
    if let Some(name) = state.tokens.lookup(presented) {
        return Some(name.to_owned());
    }
    state.oauth.resolve(presented)
}

fn build_tool_list() -> Vec<Value> {
    registry()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name.as_str(),
                "description": format!("{} Actions: {}", t.summary, t.actions.join("|")),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": t.actions},
                        "args": {"type": "object", "additionalProperties": true},
                    },
                    "required": ["action"],
                    "additionalProperties": false,
                },
            })
        })
        .collect()
}

/// Read-only allow-list. Any `(tool, action)` pair NOT in this set is
/// considered destructive and blocked when `PIPELINE_REMOTE_MODE=read_only`.
fn is_safe_action(tool: &str, action: &str) -> bool {
    match tool {
        "pipeline_session" => matches!(
            action,
            "handover" | "context" | "file_context" | "task_context"
        ),
        "pipeline_plan" => matches!(
            action,
            "prd_read"
                | "features_list"
                | "research_notes_list"
                | "research_notes_show"
                | "risk_list"
                | "progress"
                | "milestone_progress"
        ),
        // fetch · update · pin mutate (network / cache / pipeline.yaml) → not read-only.
        "pipeline_standards" => matches!(
            action,
            "brief" | "list" | "show" | "checklist" | "route" | "check"
        ),
        "pipeline_project" => action == "template_list",
        "pipeline_run" => matches!(action, "status" | "logs" | "fix_suggestion" | "explain"),
        "pipeline_test" => action == "flake_detect",
        "pipeline_repo" => matches!(
            action,
            "list"
                | "list_capabilities"
                | "compare"
                | "capability_graph"
                | "re_status"
                | "re_report"
        ),
        "pipeline_docker" => matches!(action, "inspect" | "logs" | "compose_ps" | "compose_logs"),
        "pipeline_data" => action == "db_diff",
        "pipeline_observe" => matches!(action, "logs_aggregate" | "perf_compare"),
        "pipeline_memory" => matches!(
            action,
            "recall" | "history" | "known_issues" | "suggest_fix" | "pattern_report" | "export"
        ),
        "pipeline_report" => matches!(
            action,
            "dashboard" | "last" | "summary" | "velocity_metrics" | "burndown"
        ),
        "pipeline_meta" => matches!(action, "version" | "self_check" | "explain" | "config_get"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_actions_recognized() {
        assert!(is_safe_action("pipeline_meta", "version"));
        assert!(is_safe_action("pipeline_run", "status"));
        assert!(is_safe_action("pipeline_repo", "list"));
    }

    #[test]
    fn destructive_actions_blocked() {
        assert!(!is_safe_action("pipeline_run", "stage"));
        assert!(!is_safe_action("pipeline_run", "commit"));
        assert!(!is_safe_action("pipeline_run", "push"));
        assert!(!is_safe_action("pipeline_docker", "build"));
        assert!(!is_safe_action("pipeline_docker", "run"));
        assert!(!is_safe_action("pipeline_simulate", "chaos_inject"));
        assert!(!is_safe_action("pipeline_deploy", "target"));
    }

    // constant_time_eq now lives in `crate::auth` alongside the TokenRegistry
    // it protects · its tests moved with it.

    #[test]
    fn oauth_surface_is_reachable_without_a_bearer() {
        // ! claude.ai walks discovery + register + PKCE anonymously — if any of
        // these ever fell behind the /mcp auth gate the connector could never
        // bootstrap. This pins them as PUBLIC.
        for path in [
            "/.well-known/oauth-authorization-server",
            "/.well-known/oauth-protected-resource",
            "/oauth/register",
            "/oauth/authorize",
            "/oauth/token",
        ] {
            assert!(
                !path.starts_with("/mcp"),
                "{path} must not sit behind the /mcp bearer gate"
            );
        }
    }

    #[test]
    fn mcp_body_limit_defaults_to_the_bounded_constant() {
        // An unbounded reader on a public endpoint is an OOM waiting to happen.
        // (The cap ordering itself is enforced at compile time — see above.)
        assert_eq!(mcp_body_limit(), MCP_MAX_BODY_BYTES);
    }

    // RemoteMode::from_env() is intentionally not unit-tested · it reads
    // process env which is racy across parallel tests. Covered by the
    // integration smoke test against a live server.
}
