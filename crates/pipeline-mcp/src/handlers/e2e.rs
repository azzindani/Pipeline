//! `pipeline_e2e` handler · Playwright in Docker.
//!
//! Every action here spawns a real container and derives `ok` from its exit
//! status. Three invariants hold the surface honest:
//!
//! - **No unbounded spawn.** `run_cmd` is the only process launcher · it takes a
//!   non-optional `Duration` and sets `kill_on_drop`, so ✗ action can wedge the
//!   MCP server. `e2e.record` hung the conformance suite before this existed.
//! - **No self-baselining gate.** `visual_regression` compares by default · a
//!   missing baseline is reported as a missing baseline, ✗ as a pass.
//! - **No guessed origin.** `against_env` resolves an environment to a real URL
//!   or refuses and names why · it ✗ runs against an environment *name*.
//!
//! Two ways caller data reaches a script, both quote-safe:
//!
//! - **Session + a11y scripts** are written to `.pipeline/e2e/*.cjs` and read
//!   their inputs from the environment · a file path has no quoting surface at
//!   all, so `PIPELINE_URL` can hold anything.
//! - **One-shot `screenshot` · `devtools_eval`** still build a `node -e` script,
//!   so every interpolated value goes through `js_quote` · an unescaped `'` in
//!   a URL used to terminate the literal and corrupt the script.
//!
//! ! No script may `require('playwright')` directly — see `PW_PRELUDE`. The base
//! image ships browsers, ✗ the npm package.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.49.0-noble";

/// Whole-suite runs · slowest legitimate case.
const SUITE_LIMIT: Duration = Duration::from_secs(900);
/// One page: launch · navigate · act.
const PAGE_LIMIT: Duration = Duration::from_secs(180);
/// `npm i` then one page · install dominates.
const INSTALL_LIMIT: Duration = Duration::from_secs(300);
/// Docker bookkeeping — `rm` · `ps` · `logs` · `inspect`.
const QUICK_LIMIT: Duration = Duration::from_secs(60);
/// How long `browser_launch` waits for the in-container handshake · must cover
/// a cold vendoring step, ✗ only the navigation.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(300);

/// Marks a machine-readable line inside otherwise human-readable container
/// output · ✗ parse bare stdout, Playwright writes banners to it.
const SESSION_MARKER: &str = "__PIPELINE_SESSION__";
const A11Y_MARKER: &str = "__PIPELINE_A11Y__";
const EVAL_MARKER: &str = "__PIPELINE_EVAL__";
const SHOT_MARKER: &str = "__PIPELINE_SHOT__";

/// Label stamped on session containers so `browser_close` can find every
/// session without being told a name.
const SESSION_LABEL: &str = "pipeline.e2e=session";

/// CDP endpoint inside the session container. `docker exec` shares the network
/// namespace, so loopback is enough — ✗ published port, ✗ host exposure.
const SESSION_CDP: &str = "http://127.0.0.1:9222";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "run" => run(&req.args).await,
        "record" => record(&req.args),
        "browser_launch" => browser_launch(&req.args).await,
        "browser_close" => browser_close(&req.args).await,
        "trace" => trace(&req.args).await,
        "screenshot" => screenshot(&req.args).await,
        "visual_regression" => visual_regression(&req.args).await,
        "a11y_check" => a11y_check(&req.args).await,
        "against_env" => against_env(&req.args).await,
        "devtools_eval" => devtools_eval(&req.args).await,
        other => err(format!("unknown action 'pipeline_e2e.{other}'")),
    }
}

// ── process plumbing ────────────────────────────────────────────────────────

struct Captured {
    ok: bool,
    code: i32,
    stdout: String,
    stderr: String,
}

impl Captured {
    /// stdout + stderr · Playwright splits diagnostics across both and callers
    /// scanning for one marker should ✗ care which stream carried it.
    fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

/// The only process launcher in this module.
///
/// `limit` is non-optional by design: making the timeout structural is what
/// makes "✗ e2e action can hang the server" a property of the code rather than
/// a habit each new action has to remember.
async fn run_cmd(
    program: &str,
    args: &[&str],
    cwd: &Path,
    label: &str,
    limit: Duration,
) -> Result<Captured, String> {
    // ! kill_on_drop · the timeout arm drops the child. Without this the
    // container outlives the MCP call that started it and leaks silently.
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program} spawn: {e}"))?;

    let output = match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("{program} wait: {e}")),
        Err(_) => {
            return Err(format!(
                "{label} timed out after {}s · process killed",
                limit.as_secs()
            ));
        }
    };
    Ok(Captured {
        ok: output.status.success(),
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

async fn capture(
    program: &str,
    args: &[&str],
    cwd: &Path,
    label: &str,
    limit: Duration,
) -> ToolResponse {
    match run_cmd(program, args, cwd, label, limit).await {
        Ok(c) => to_response(&c, label),
        Err(e) => err(e),
    }
}

fn to_response(c: &Captured, label: &str) -> ToolResponse {
    ToolResponse {
        ok: c.ok,
        data: json!({
            "command": label,
            "exit_code": c.code,
            "stdout": truncate(&c.stdout, 8_000),
            "stderr": truncate(&c.stderr, 8_000),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if c.ok {
            None
        } else {
            Some(format!("{label} exit {}", c.code))
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        let cut = (0..=max)
            .rev()
            .find(|i| s.is_char_boundary(*i))
            .unwrap_or(0);
        format!(
            "{}\n... [truncated · {} more bytes]",
            &s[..cut],
            s.len() - cut
        )
    }
}

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

/// Fold extra keys into a response's `data` without disturbing what `capture`
/// already put there (`command` · `exit_code` · `stdout` · `stderr`).
fn merge(mut r: ToolResponse, extra: Value) -> ToolResponse {
    if let (Some(dst), Value::Object(src)) = (r.data.as_object_mut(), extra) {
        for (k, v) in src {
            dst.insert(k, v);
        }
    }
    r
}

// ── argument + command-line helpers ─────────────────────────────────────────

fn cwd() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cwd: {e}"))
}

fn mount_for(cwd: &Path) -> String {
    format!("{}:/work", cwd.display())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Escape for embedding inside a single-quoted JS literal.
///
/// ! Regression guard: `screenshot` interpolated a raw URL into `node -e`, so a
/// `'` anywhere in the URL closed the literal and the script became a syntax
/// error — or, with a crafted URL, different code entirely.
fn js_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// `docker run --rm` argv for a one-shot Playwright container.
///
/// ! `-e` pairs must precede the image name · docker treats everything after
/// the image as the container command, so a trailing flag is silently handed to
/// the program instead of to docker.
fn one_shot_argv(mount: &str, envs: &[String], cmd: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount.to_owned(),
        "-w".into(),
        "/work".into(),
    ];
    for e in envs {
        v.push("-e".into());
        v.push(e.clone());
    }
    v.push(PLAYWRIGHT_IMAGE.into());
    v.extend_from_slice(cmd);
    v
}

async fn docker(cmdline: &[String], cwd: &Path, label: &str, limit: Duration) -> ToolResponse {
    let arr: Vec<&str> = cmdline.iter().map(String::as_str).collect();
    capture("docker", &arr, cwd, label, limit).await
}

/// Scripts carrying caller data live on disk under Pipeline-owned `.pipeline/`.
/// A file path has no quoting surface · the data travels by environment.
fn write_script(cwd: &Path, name: &str, body: &str) -> Result<String, String> {
    let dir = cwd.join(".pipeline").join("e2e");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir .pipeline/e2e: {e}"))?;
    std::fs::write(dir.join(name), body).map_err(|e| format!("write {name}: {e}"))?;
    Ok(format!("/work/.pipeline/e2e/{name}"))
}

/// Pull the JSON payload off a marker line.
fn marker_payload(output: &str, marker: &str) -> Option<Value> {
    output
        .lines()
        .rev()
        .find_map(|l| l.split_once(marker))
        .and_then(|(_, rest)| serde_json::from_str(rest.trim()).ok())
}

// ── in-container scripts ────────────────────────────────────────────────────

/// Resolve a node module or vendor it on the spot.
///
/// ! Verified against the image, ✗ assumed: `mcr.microsoft.com/playwright`
/// ships the **browser binaries** under `/ms-playwright` but **not** the
/// `playwright` npm package, and `NODE_PATH` is empty. So a bare
/// `require('playwright')` resolves only when the mounted project happens to
/// carry `node_modules` — on a Rust repo, or any project that has not run
/// `npm i`, it is `MODULE_NOT_FOUND`. That is the same defect `a11y_check` was
/// flagged for, sitting unnoticed under `screenshot` and `devtools_eval`.
///
/// Cascade: project copy → sibling → vendored → install. `playwright-core`
/// drives the browsers already in the image, so the install is small and needs
/// ✗ browser download.
///
/// npm stdout is discarded · stderr kept · the marker line must stay the only
/// machine-readable thing on stdout.
const PW_PRELUDE: &str = r"const cp = require('child_process');
function need(cands, spec, prefix) {
  for (const c of cands) { try { return require(c); } catch (e) { /* next */ } }
  cp.execSync('npm i --no-save --no-audit --no-fund --loglevel=error --prefix ' + prefix + ' ' + spec,
    { stdio: ['ignore', 'ignore', 'inherit'] });
  return require(prefix + '/node_modules/' + spec.split('@')[0]);
}
const { chromium } = need(
  ['playwright', 'playwright-core', '/tmp/pw/node_modules/playwright-core'],
  'playwright-core@1.49.0', '/tmp/pw');
";

/// Holds a real chromium open for later `docker exec` calls.
///
/// ! CDP, ✗ `launchServer`: a `connect()` client owns the contexts it creates
/// and Playwright tears them down on disconnect, so the page this script
/// navigates would die the moment the launch call returned. `connectOverCDP`
/// attaches to a browser that outlives every client.
const SESSION_BODY: &str = r"(async () => {
  const url = process.env.PIPELINE_URL;
  const ctx = await chromium.launchPersistentContext('/tmp/pipeline-profile', {
    headless: true,
    args: ['--remote-debugging-port=9222'],
  });
  const page = ctx.pages()[0] || (await ctx.newPage());
  let status = null;
  try {
    const resp = await page.goto(url, { waitUntil: 'load', timeout: 60000 });
    status = resp ? resp.status() : null;
  } catch (e) {
    console.log('__PIPELINE_SESSION__' + JSON.stringify({ error: String((e && e.message) || e) }));
    await ctx.close();
    process.exit(1);
  }
  console.log('__PIPELINE_SESSION__' + JSON.stringify({
    url: page.url(),
    title: await page.title(),
    status: status,
    cdp: 'http://127.0.0.1:9222',
  }));
  await new Promise(() => {});
})();
";

const SESSION_EVAL_BODY: &str = r"(async () => {
  const b = await chromium.connectOverCDP(process.env.PIPELINE_CDP);
  const page = b.contexts()[0].pages()[0];
  const r = await page.evaluate('(() => {' + process.env.PIPELINE_JS + '})()');
  console.log('__PIPELINE_EVAL__' + JSON.stringify({ result: r === undefined ? null : r, url: page.url() }));
  await b.close();
})();
";

const SESSION_SHOT_BODY: &str = r"(async () => {
  const b = await chromium.connectOverCDP(process.env.PIPELINE_CDP);
  const page = b.contexts()[0].pages()[0];
  await page.screenshot({ path: process.env.PIPELINE_OUT, fullPage: true });
  console.log('__PIPELINE_SHOT__' + JSON.stringify({ path: process.env.PIPELINE_OUT, url: page.url() }));
  await b.close();
})();
";

/// axe is injected as source into the page · ✗ `@axe-core/playwright`, which is
/// absent from both this repo and the base image. `axe.source` is the whole
/// library as a string, so one `addScriptTag` is the entire integration.
const A11Y_BODY: &str = r"const axe = need(['axe-core', '/tmp/axe/node_modules/axe-core'],
  'axe-core@4.10.2', '/tmp/axe');
const ORDER = { minor: 1, moderate: 2, serious: 3, critical: 4 };
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage();
  await p.goto(process.env.PIPELINE_URL, { waitUntil: 'load', timeout: 60000 });
  await p.addScriptTag({ content: axe.source });
  const tags = JSON.parse(process.env.PIPELINE_A11Y_TAGS || '[]');
  const opts = tags.length ? { runOnly: { type: 'tag', values: tags } } : {};
  const r = await p.evaluate((o) => window.axe.run(document, o), opts);
  const violations = r.violations.map((v) => ({
    id: v.id,
    impact: v.impact,
    help: v.help,
    help_url: v.helpUrl,
    count: v.nodes.length,
    selectors: v.nodes.slice(0, 5).map((n) => n.target.join(' ')),
  }));
  console.log('__PIPELINE_A11Y__' + JSON.stringify({
    url: p.url(),
    violations: violations,
    passes: r.passes.length,
    incomplete: r.incomplete.length,
  }));
  await b.close();
  const floor = ORDER[process.env.PIPELINE_A11Y_FAIL_ON] || 0;
  const failing = floor === 0 ? 0 : violations.filter((v) => (ORDER[v.impact] || 0) >= floor).length;
  process.exit(failing > 0 ? 1 : 0);
})();
";

/// Every script gets the resolution cascade · ✗ script may assume `chromium`
/// is already requireable.
fn script(body: &str) -> String {
    format!("{PW_PRELUDE}{body}")
}

// ── run · trace ─────────────────────────────────────────────────────────────

async fn run(args: &Value) -> ToolResponse {
    let suite = str_arg(args, "suite").unwrap_or_default().to_owned();
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mut cmd: Vec<String> = vec!["npx".into(), "playwright".into(), "test".into()];
    if !suite.is_empty() {
        cmd.push(suite);
    }
    let cmdline = one_shot_argv(&mount_for(&cwd), &[], &cmd);
    docker(&cmdline, &cwd, "e2e_run", SUITE_LIMIT).await
}

async fn trace(args: &Value) -> ToolResponse {
    let Some(test_name) = str_arg(args, "test") else {
        return err("missing 'test'".into());
    };
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let cmd: Vec<String> = vec![
        "npx".into(),
        "playwright".into(),
        "test".into(),
        "--trace".into(),
        "on".into(),
        "-g".into(),
        test_name.to_owned(),
    ];
    let cmdline = one_shot_argv(&mount_for(&cwd), &[], &cmd);
    docker(&cmdline, &cwd, "e2e_trace", SUITE_LIMIT).await
}

// ── record · refused ────────────────────────────────────────────────────────

/// Stays refused on purpose.
///
/// `playwright codegen` is a headed recorder: it needs a display and a human
/// driving the browser, and returns only when that human closes the window.
/// There is no honest synchronous MCP shape for "wait for a person".
fn record(_args: &Value) -> ToolResponse {
    err(
        "e2e.record ✗ implementable as a synchronous MCP call · `playwright codegen` is a headed, \
         interactive recorder: it needs a display and a human driving the browser, and returns \
         only when that human closes the window. Alternatives: run `npx playwright codegen <url>` \
         on your own machine and commit the result, or script the interaction directly with \
         e2e.browser_launch + e2e.devtools_eval + e2e.screenshot."
            .into(),
    )
}

// ── browser session ─────────────────────────────────────────────────────────

fn generated_session_name() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("pipeline-browser-{n:x}")
}

/// `docker run -d` argv for a session container.
fn session_argv(mount: &str, name: &str, url: &str) -> Vec<String> {
    vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.to_owned(),
        "--label".into(),
        SESSION_LABEL.to_owned(),
        "--ipc=host".into(),
        "-v".into(),
        mount.to_owned(),
        "-w".into(),
        "/work".into(),
        "-e".into(),
        format!("PIPELINE_URL={url}"),
        PLAYWRIGHT_IMAGE.into(),
        "node".into(),
        "/work/.pipeline/e2e/session.cjs".into(),
    ]
}

/// Shape the launch outcome from what the *container* reported.
///
/// ! `url` comes from `page.url()` after redirects — never from the requested
/// argument. Echoing the request back would make an unreachable page and a
/// successful navigation indistinguishable, which is exactly the lie the old
/// `browser_launch(<url>)` log label told.
fn launch_response(name: &str, requested: &str, logs: &str) -> ToolResponse {
    let Some(hs) = marker_payload(logs, SESSION_MARKER) else {
        return err(format!(
            "browser session '{name}' produced no handshake · container logs: {}",
            truncate(logs, 4_000)
        ));
    };
    if let Some(e) = hs.get("error").and_then(Value::as_str) {
        return err(format!("navigation to '{requested}' failed: {e}"));
    }
    let Some(reached) = hs.get("url").and_then(Value::as_str) else {
        return err(format!(
            "browser session '{name}' handshake carried no url · cannot confirm navigation"
        ));
    };
    ToolResponse {
        ok: true,
        data: json!({
            "command": "browser_launch",
            "session": name,
            "container": name,
            "requested_url": requested,
            "url": reached,
            "redirected": reached != requested,
            "title": hs.get("title").cloned().unwrap_or(Value::Null),
            "http_status": hs.get("status").cloned().unwrap_or(Value::Null),
            "cdp": SESSION_CDP,
        }),
        next_suggested: vec![
            format!("pipeline_e2e.devtools_eval(session=\"{name}\", js=\"...\")"),
            format!("pipeline_e2e.screenshot(session=\"{name}\")"),
            format!("pipeline_e2e.browser_close(session=\"{name}\")"),
        ],
        memory_refs: vec![],
        error: None,
    }
}

async fn container_exists(name: &str, cwd: &Path) -> bool {
    run_cmd(
        "docker",
        &["inspect", "--type", "container", name],
        cwd,
        "inspect",
        QUICK_LIMIT,
    )
    .await
    .is_ok_and(|c| c.ok)
}

/// Poll container logs for the handshake · bounded, and gives up if the
/// container has already exited (a crash would otherwise burn the whole budget).
async fn await_handshake(name: &str, cwd: &Path) -> Result<String, String> {
    let deadline = std::time::Instant::now() + HANDSHAKE_LIMIT;
    loop {
        let logs = run_cmd("docker", &["logs", name], cwd, "logs", QUICK_LIMIT)
            .await
            .map(|c| c.combined())
            .unwrap_or_default();
        if logs.contains(SESSION_MARKER) {
            return Ok(logs);
        }
        if !container_exists(name, cwd).await {
            return Err(format!(
                "session container '{name}' exited · logs: {}",
                truncate(&logs, 4_000)
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "browser session '{name}' did not report navigation within {}s · logs: {}",
                HANDSHAKE_LIMIT.as_secs(),
                truncate(&logs, 4_000)
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn browser_launch(args: &Value) -> ToolResponse {
    let Some(url) = str_arg(args, "url") else {
        return err("missing 'url' · browser_launch navigates, so it needs a page".into());
    };
    let reuse = args.get("reuse").and_then(Value::as_bool).unwrap_or(false);
    // Generated names by default · the old hardcoded `pipeline-browser` made a
    // second launch fail on a name conflict with no way to run two sessions.
    let name = str_arg(args, "session").map_or_else(generated_session_name, str::to_owned);
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };

    if container_exists(&name, &cwd).await {
        if !reuse {
            return err(format!(
                "container '{name}' already exists · pass reuse=true to adopt it, a different \
                 'session' name, or omit 'session' for a generated one"
            ));
        }
        return match await_handshake(&name, &cwd).await {
            Ok(logs) => merge(
                launch_response(&name, url, &logs),
                json!({ "reused": true }),
            ),
            Err(e) => err(e),
        };
    }

    if let Err(e) = write_script(&cwd, "session.cjs", &script(SESSION_BODY)) {
        return err(e);
    }
    let cmdline = session_argv(&mount_for(&cwd), &name, url);
    let started = docker(&cmdline, &cwd, "browser_launch", QUICK_LIMIT).await;
    if !started.ok {
        return started;
    }
    match await_handshake(&name, &cwd).await {
        Ok(logs) => merge(
            launch_response(&name, url, &logs),
            json!({ "reused": false }),
        ),
        Err(e) => err(e),
    }
}

async fn browser_close(args: &Value) -> ToolResponse {
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    if let Some(name) = str_arg(args, "session") {
        return merge(
            docker(
                &["rm".into(), "-f".into(), name.to_owned()],
                &cwd,
                "browser_close",
                QUICK_LIMIT,
            )
            .await,
            json!({ "closed": [name] }),
        );
    }
    // No name → close every labelled session. Keeps the historical no-argument
    // call meaningful now that names are generated.
    let listed = match run_cmd(
        "docker",
        &["ps", "-aq", "--filter", &format!("label={SESSION_LABEL}")],
        &cwd,
        "browser_close_list",
        QUICK_LIMIT,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return err(e),
    };
    let ids: Vec<String> = listed
        .stdout
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        return merge(
            ToolResponse::ok(json!({ "command": "browser_close" })),
            json!({ "closed": Vec::<String>::new(), "note": "no labelled e2e sessions were running" }),
        );
    }
    let mut cmdline: Vec<String> = vec!["rm".into(), "-f".into()];
    cmdline.extend(ids.iter().cloned());
    merge(
        docker(&cmdline, &cwd, "browser_close", QUICK_LIMIT).await,
        json!({ "closed": ids }),
    )
}

// ── screenshot ──────────────────────────────────────────────────────────────

fn screenshot_outfile() -> String {
    format!(
        "{}.png",
        pipeline_memory::now_rfc3339().replace([':', '.', '+'], "_")
    )
}

/// One-shot chromium · URL is escaped, ✗ interpolated raw.
fn screenshot_script(url: &str, outfile: &str) -> String {
    script(&format!(
        "(async () => {{\n  const b = await chromium.launch();\n  const p = await b.newPage();\n  await p.goto('{}');\n  await p.screenshot({{ path: '{}', fullPage: true }});\n  console.log('{SHOT_MARKER}' + JSON.stringify({{ path: '{}', url: p.url() }}));\n  await b.close();\n}})();",
        js_quote(url),
        js_quote(outfile),
        js_quote(outfile)
    ))
}

async fn screenshot(args: &Value) -> ToolResponse {
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let dir = cwd.join(".pipeline").join("screenshots");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let file = screenshot_outfile();
    let in_container = format!("/work/.pipeline/screenshots/{file}");
    let on_host = dir.join(&file);
    // The path was computed and thrown away before · the agent got ok:true and
    // no way to find the artifact it had just been told existed.
    let paths = json!({
        "path": on_host.display().to_string(),
        "container_path": in_container,
    });

    if let Some(session) = str_arg(args, "session") {
        return screenshot_in_session(&cwd, session, &in_container, paths).await;
    }
    let Some(url) = str_arg(args, "url") else {
        return err("missing 'url' · pass a url, or 'session' to shoot a live browser".into());
    };
    let cmd = vec![
        "node".into(),
        "-e".into(),
        screenshot_script(url, &in_container),
    ];
    let cmdline = one_shot_argv(&mount_for(&cwd), &[], &cmd);
    let r = docker(&cmdline, &cwd, "screenshot", INSTALL_LIMIT).await;
    // Prefer the URL the page actually settled on · fall back to the request
    // only when the marker is absent (i.e. the shot did not happen).
    let reached = marker_url(&r, SHOT_MARKER).unwrap_or_else(|| url.to_owned());
    merge(r, merge_url(paths, &reached))
}

fn merge_url(mut paths: Value, url: &str) -> Value {
    if let Some(o) = paths.as_object_mut() {
        o.insert("url".into(), Value::String(url.to_owned()));
    }
    paths
}

/// Marker payload from a captured response's stdout.
fn response_marker(r: &ToolResponse, marker: &str) -> Option<Value> {
    r.data
        .get("stdout")
        .and_then(Value::as_str)
        .and_then(|s| marker_payload(s, marker))
}

fn marker_url(r: &ToolResponse, marker: &str) -> Option<String> {
    response_marker(r, marker).and_then(|p| p.get("url").and_then(Value::as_str).map(str::to_owned))
}

async fn screenshot_in_session(
    cwd: &Path,
    session: &str,
    outfile: &str,
    paths: Value,
) -> ToolResponse {
    if let Err(e) = write_script(cwd, "shot.cjs", &script(SESSION_SHOT_BODY)) {
        return err(e);
    }
    let cmdline: Vec<String> = vec![
        "exec".into(),
        "-e".into(),
        format!("PIPELINE_CDP={SESSION_CDP}"),
        "-e".into(),
        format!("PIPELINE_OUT={outfile}"),
        session.to_owned(),
        "node".into(),
        "/work/.pipeline/e2e/shot.cjs".into(),
    ];
    let r = docker(&cmdline, cwd, "screenshot", PAGE_LIMIT).await;
    let shot_url = marker_url(&r, SHOT_MARKER);
    let mut extra = shot_url.map_or(paths.clone(), |u| merge_url(paths, &u));
    if let Some(o) = extra.as_object_mut() {
        o.insert("session".into(), Value::String(session.to_owned()));
    }
    merge(r, extra)
}

// ── visual regression ───────────────────────────────────────────────────────

/// Build the suite argv.
///
/// ! `--update-snapshots` appears **only** when the caller explicitly asked to
/// regenerate. The old code passed `--update-snapshots=missing` unconditionally,
/// which wrote the baselines it was supposed to compare against — so on a repo
/// with no committed snapshots the gate created its own baseline and went green
/// every time, including the first run, which is the only one that matters.
fn visual_regression_argv(
    mount: &str,
    suite: &str,
    url: &str,
    threshold: Option<f64>,
    update_baseline: bool,
) -> Vec<String> {
    let mut envs: Vec<String> = Vec::new();
    if !url.is_empty() {
        envs.push(format!("BASE_URL={url}"));
    }
    if let Some(t) = threshold {
        envs.push(format!("PIPELINE_VISUAL_THRESHOLD={t}"));
    }
    let mut cmd: Vec<String> = vec!["npx".into(), "playwright".into(), "test".into()];
    if !suite.is_empty() {
        cmd.push(suite.to_owned());
    }
    if update_baseline {
        cmd.push("--update-snapshots".into());
    }
    one_shot_argv(mount, &envs, &cmd)
}

fn baseline_missing(output: &str) -> bool {
    let l = output.to_ascii_lowercase();
    l.contains("snapshot doesn't exist") || l.contains("snapshot does not exist")
}

fn no_tests_ran(output: &str) -> bool {
    let l = output.to_ascii_lowercase();
    l.contains("no tests found") || l.contains("nothing to run")
}

/// A visual gate has two ways to be green without comparing anything: no
/// baseline, and no tests. Both are named here rather than left as a pass.
fn shape_visual_result(r: ToolResponse, update_baseline: bool) -> ToolResponse {
    let combined = format!(
        "{}{}",
        r.data.get("stdout").and_then(Value::as_str).unwrap_or(""),
        r.data.get("stderr").and_then(Value::as_str).unwrap_or("")
    );
    if update_baseline {
        return merge(
            r,
            json!({
                "mode": "update_baseline",
                "compared": false,
                "note": "baselines were regenerated · ✗ comparison was performed",
            }),
        );
    }
    let missing = baseline_missing(&combined);
    let empty = no_tests_ran(&combined);
    let mut out = merge(
        r,
        json!({
            "mode": "compare",
            "compared": !missing && !empty,
            "baseline_missing": missing,
            "no_tests": empty,
        }),
    );
    if missing {
        out.ok = false;
        out.error = Some(
            "visual_regression: baseline snapshot missing · nothing was compared. Review the page, \
             then call visual_regression(update_baseline=true) to record a baseline deliberately."
                .into(),
        );
    } else if empty {
        out.ok = false;
        out.error = Some(
            "visual_regression: no tests matched · nothing was compared. A visual gate with no \
             visual tests is not a pass."
                .into(),
        );
    }
    out
}

async fn visual_regression(args: &Value) -> ToolResponse {
    let suite = str_arg(args, "suite").unwrap_or_default();
    let url = str_arg(args, "url").unwrap_or_default();
    let threshold = args.get("threshold").and_then(Value::as_f64);
    let update_baseline = args
        .get("update_baseline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let cmdline = visual_regression_argv(&mount_for(&cwd), suite, url, threshold, update_baseline);
    let r = docker(&cmdline, &cwd, "visual_regression", SUITE_LIMIT).await;
    let mut out = shape_visual_result(r, update_baseline);
    if let Some(t) = threshold {
        out = merge(
            out,
            json!({
                "threshold": t,
                "threshold_note": "exported as PIPELINE_VISUAL_THRESHOLD · effective only if \
                                   playwright.config reads it into expect.toHaveScreenshot",
            }),
        );
    }
    out
}

// ── a11y ────────────────────────────────────────────────────────────────────

const IMPACTS: [&str; 5] = ["none", "minor", "moderate", "serious", "critical"];

fn a11y_argv(mount: &str, url: &str, tags: &str, fail_on: &str) -> Vec<String> {
    // ✗ `sh -c` · the vendoring lives inside the script (see PW_PRELUDE), so the
    // container command stays a plain argv with no nested shell quoting. axe
    // installs into /tmp, ✗ into the mounted project — a gate must not leave
    // node_modules behind in the repo it audits.
    let envs = vec![
        format!("PIPELINE_URL={url}"),
        format!("PIPELINE_A11Y_TAGS={tags}"),
        format!("PIPELINE_A11Y_FAIL_ON={fail_on}"),
    ];
    one_shot_argv(
        mount,
        &envs,
        &["node".into(), "/work/.pipeline/e2e/a11y.cjs".into()],
    )
}

fn shape_a11y_result(r: ToolResponse, fail_on: &str) -> ToolResponse {
    let stdout = r.data.get("stdout").and_then(Value::as_str).unwrap_or("");
    let Some(p) = marker_payload(stdout, A11Y_MARKER) else {
        // ✗ report "no violations" · the scan never produced a result.
        let mut out = r;
        out.ok = false;
        out.error = Some(
            "a11y_check: axe produced no report · the audit did not run, so the page is \
             unassessed — this is not a pass. See stderr."
                .into(),
        );
        return out;
    };
    let violations = p.get("violations").cloned().unwrap_or(json!([]));
    let n = violations.as_array().map_or(0, Vec::len);
    let failed = !r.ok;
    let mut out = merge(
        r,
        json!({
            "url": p.get("url").cloned().unwrap_or(Value::Null),
            "violations": violations,
            "violation_count": n,
            "passes": p.get("passes").cloned().unwrap_or(Value::Null),
            "incomplete": p.get("incomplete").cloned().unwrap_or(Value::Null),
            "fail_on": fail_on,
        }),
    );
    out.error = if failed {
        Some(format!(
            "a11y_check: {n} accessibility violation(s) · at least one at or above impact '{fail_on}'"
        ))
    } else {
        None
    };
    out
}

async fn a11y_check(args: &Value) -> ToolResponse {
    let Some(url) = str_arg(args, "url") else {
        return err("missing 'url'".into());
    };
    let fail_on = str_arg(args, "fail_on").unwrap_or("critical");
    if !IMPACTS.contains(&fail_on) {
        return err(format!(
            "unknown fail_on '{fail_on}' · accepted: {}",
            IMPACTS.join(" · ")
        ));
    }
    let tags = match args.get("tags") {
        None | Some(Value::Null) => "[]".to_owned(),
        Some(v @ Value::Array(_)) => v.to_string(),
        Some(_) => {
            return err("'tags' must be an array of axe tag strings, e.g. [\"wcag2aa\"]".into());
        }
    };
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    if let Err(e) = write_script(&cwd, "a11y.cjs", &script(A11Y_BODY)) {
        return err(e);
    }
    let cmdline = a11y_argv(&mount_for(&cwd), url, &tags, fail_on);
    let r = docker(&cmdline, &cwd, "a11y_check", INSTALL_LIMIT).await;
    shape_a11y_result(r, fail_on)
}

// ── against_env ─────────────────────────────────────────────────────────────

/// Resolve an environment to a real origin, or refuse and name why.
///
/// ! The old code set `BASE_URL` to the environment *name* ("staging"), so the
/// suite ran against a nonsense origin and every failure read as an application
/// bug rather than a tool bug. A guess is worse than a refusal here: it moves
/// the blame somewhere the agent cannot check.
fn resolve_env_url(
    args: &Value,
    cfg: Option<&pipeline_config::PipelineConfig>,
) -> Result<(String, String), String> {
    if let Some(u) = str_arg(args, "url") {
        if !is_http_origin(u) {
            return Err(format!(
                "url '{u}' is not an http(s) origin · e2e needs a URL the browser can open"
            ));
        }
        return Ok((u.to_owned(), "arg:url".into()));
    }
    let Some(env) = str_arg(args, "env") else {
        return Err(
            "missing 'env' · pass env=<name> to resolve from pipeline.yaml, or url=<origin> \
             directly. ✗ default is assumed — running the suite against a guessed origin \
             reports tool failure as application failure."
                .into(),
        );
    };
    let Some(cfg) = cfg else {
        return Err(format!(
            "cannot resolve env '{env}' · no readable pipeline.yaml in cwd · pass url=<origin>"
        ));
    };
    let Some(deploy) = cfg.deploy.as_ref() else {
        return Err(format!(
            "cannot resolve env '{env}' · pipeline.yaml has no deploy section · pass url=<origin>"
        ));
    };
    let Some(target) = deploy.targets.get(env) else {
        let known: Vec<&str> = deploy.targets.keys().map(String::as_str).collect();
        return Err(format!(
            "unknown env '{env}' · deploy.targets declares: {}",
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(" · ")
            }
        ));
    };
    if !is_http_origin(&target.host) {
        return Err(format!(
            "deploy.targets.{env}.host is '{}' · a deploy target, not an http(s) origin the \
             browser can open · pass url=<origin> for this run",
            target.host
        ));
    }
    Ok((
        target.host.clone(),
        format!("pipeline.yaml deploy.targets.{env}.host"),
    ))
}

fn is_http_origin(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

async fn against_env(args: &Value) -> ToolResponse {
    let cfg = crate::handlers::load_config_in_cwd().ok();
    let (url, source) = match resolve_env_url(args, cfg.as_ref()) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let suite = str_arg(args, "suite").unwrap_or_default();
    let env = str_arg(args, "env").unwrap_or("-");
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    let mut cmd: Vec<String> = vec!["npx".into(), "playwright".into(), "test".into()];
    if !suite.is_empty() {
        cmd.push(suite.to_owned());
    }
    let cmdline = one_shot_argv(&mount_for(&cwd), &[format!("BASE_URL={url}")], &cmd);
    merge(
        docker(&cmdline, &cwd, &format!("e2e_against({env})"), SUITE_LIMIT).await,
        json!({ "env": env, "base_url": url, "resolved_from": source }),
    )
}

// ── devtools_eval ───────────────────────────────────────────────────────────

/// ! Marker-wrapped · the vendoring step can print to stdout, so a bare
/// `JSON.stringify(r)` on its own line is no longer safely parseable.
fn devtools_script(url: &str, js: &str) -> String {
    script(&format!(
        "(async () => {{\n  const b = await chromium.launch();\n  const p = await b.newPage();\n  await p.goto('{}');\n  const r = await p.evaluate(() => {{ {js} }});\n  console.log('{EVAL_MARKER}' + JSON.stringify({{ result: r === undefined ? null : r, url: p.url() }}));\n  await b.close();\n}})();",
        js_quote(url)
    ))
}

async fn devtools_eval(args: &Value) -> ToolResponse {
    let Some(js) = str_arg(args, "js") else {
        return err("missing 'js'".into());
    };
    let cwd = match cwd() {
        Ok(p) => p,
        Err(e) => return err(e),
    };
    if let Some(session) = str_arg(args, "session") {
        return devtools_eval_in_session(&cwd, session, js).await;
    }
    let Some(url) = str_arg(args, "url") else {
        return err(
            "missing 'url' · pass a url, or 'session' to evaluate in a live browser".into(),
        );
    };
    let cmd = vec!["node".into(), "-e".into(), devtools_script(url, js)];
    let cmdline = one_shot_argv(&mount_for(&cwd), &[], &cmd);
    let r = docker(&cmdline, &cwd, "devtools_eval", INSTALL_LIMIT).await;
    shape_eval_result(r, None)
}

async fn devtools_eval_in_session(cwd: &Path, session: &str, js: &str) -> ToolResponse {
    if let Err(e) = write_script(cwd, "eval.cjs", &script(SESSION_EVAL_BODY)) {
        return err(e);
    }
    let cmdline: Vec<String> = vec![
        "exec".into(),
        "-e".into(),
        format!("PIPELINE_CDP={SESSION_CDP}"),
        "-e".into(),
        format!("PIPELINE_JS={js}"),
        session.to_owned(),
        "node".into(),
        "/work/.pipeline/e2e/eval.cjs".into(),
    ];
    let r = docker(&cmdline, cwd, "devtools_eval", PAGE_LIMIT).await;
    shape_eval_result(r, Some(session))
}

/// Lift the evaluated value out of stdout into `data.result`.
///
/// ! Marker-scoped · the vendoring step and Playwright banners share stdout, so
/// parsing the whole stream would hand the agent noise typed as a result.
fn shape_eval_result(r: ToolResponse, session: Option<&str>) -> ToolResponse {
    let payload = response_marker(&r, EVAL_MARKER);
    let mut extra = json!({});
    if let Some(o) = extra.as_object_mut() {
        if let Some(s) = session {
            o.insert("session".into(), Value::String(s.to_owned()));
        }
        if let Some(p) = payload {
            o.insert(
                "result".into(),
                p.get("result").cloned().unwrap_or(Value::Null),
            );
            o.insert("url".into(), p.get("url").cloned().unwrap_or(Value::Null));
        }
    }
    merge(r, extra)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(host: &str) -> pipeline_config::PipelineConfig {
        pipeline_config::PipelineConfig::parse(&format!(
            "project: p\nversion: 0.1.0\nstack:\n  runtime: rust\ndeploy:\n  registry: ghcr.io/x\n  targets:\n    staging:\n      type: compose\n      host: {host}\n"
        ))
        .expect("fixture parses")
    }

    // ── the timeout invariant ───────────────────────────────────────────────

    #[test]
    fn no_e2e_action_can_run_without_a_timeout() {
        // ! Structural, not conventional: `run_cmd` is the only spawner in this
        // module and its `limit` is a plain Duration, so there is no code path
        // that starts a process without a bound. e2e.record hung the whole
        // conformance suite when this was not true.
        let src = include_str!("e2e.rs");
        // Needle assembled at runtime · a literal would match itself in this
        // very file and inflate the count.
        let spawn = format!(".{}()", "spawn");
        assert_eq!(
            src.matches(spawn.as_str()).count(),
            1,
            "exactly one spawn site · a second one would bypass the timeout"
        );
        assert!(
            src.contains("tokio::time::timeout(limit, child.wait_with_output())"),
            "the single spawn must be awaited under a timeout"
        );
        assert!(
            src.contains("kill_on_drop(true)"),
            "the timeout arm drops the child · without kill_on_drop the container leaks"
        );
        assert!(
            !src.contains(".output()\n"),
            "the unbounded .output() path must not come back"
        );
        for d in [
            SUITE_LIMIT,
            PAGE_LIMIT,
            INSTALL_LIMIT,
            QUICK_LIMIT,
            HANDSHAKE_LIMIT,
        ] {
            assert!(d.as_secs() > 0, "a zero timeout is not a timeout");
        }
    }

    // ── visual regression ───────────────────────────────────────────────────

    #[test]
    fn a_missing_baseline_is_visible_not_a_silent_green() {
        // Regression: --update-snapshots=missing wrote the baseline it was meant
        // to compare against, so run #1 on a fresh repo always passed.
        let cmdline = visual_regression_argv("/w:/work", "", "", None, false);
        assert!(
            !cmdline.iter().any(|a| a.starts_with("--update-snapshots")),
            "comparison is the default · {cmdline:?}"
        );

        // And when Playwright says the baseline was absent, that surfaces as a
        // named failure rather than an exit-0 pass.
        let raw = ToolResponse {
            ok: true,
            data: json!({"stdout": "Error: A snapshot doesn't exist at /work/x.png, writing actual.", "stderr": ""}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: None,
        };
        let shaped = shape_visual_result(raw, false);
        assert!(!shaped.ok, "missing baseline must not report success");
        assert_eq!(shaped.data["baseline_missing"], json!(true));
        assert_eq!(shaped.data["compared"], json!(false));
        assert!(shaped.error.unwrap().contains("baseline snapshot missing"));
    }

    #[test]
    fn regenerating_baselines_is_opt_in_and_says_it_did_not_compare() {
        let cmdline = visual_regression_argv("/w:/work", "", "", None, true);
        assert!(
            cmdline.iter().any(|a| a == "--update-snapshots"),
            "{cmdline:?}"
        );
        let raw = ToolResponse::ok(json!({"stdout": "", "stderr": ""}));
        let shaped = shape_visual_result(raw, true);
        assert_eq!(shaped.data["compared"], json!(false));
        assert_eq!(shaped.data["mode"], json!("update_baseline"));
    }

    #[test]
    fn an_empty_visual_suite_is_not_a_pass() {
        let raw = ToolResponse::ok(json!({"stdout": "Error: no tests found.", "stderr": ""}));
        let shaped = shape_visual_result(raw, false);
        assert!(!shaped.ok);
        assert_eq!(shaped.data["no_tests"], json!(true));
    }

    #[test]
    fn visual_regression_targeting_reaches_the_argv() {
        let cmdline = visual_regression_argv(
            "/w:/work",
            "tests/vis.spec.ts",
            "https://x.test",
            Some(0.02),
            false,
        );
        assert!(
            cmdline.contains(&"BASE_URL=https://x.test".to_owned()),
            "{cmdline:?}"
        );
        assert!(
            cmdline.contains(&"PIPELINE_VISUAL_THRESHOLD=0.02".to_owned()),
            "{cmdline:?}"
        );
        assert!(
            cmdline.contains(&"tests/vis.spec.ts".to_owned()),
            "{cmdline:?}"
        );
        // ! env flags must precede the image · after it they are container args.
        let img = cmdline.iter().position(|a| a == PLAYWRIGHT_IMAGE).unwrap();
        let base = cmdline
            .iter()
            .position(|a| a.starts_with("BASE_URL="))
            .unwrap();
        assert!(base < img, "-e pairs must come before the image");
    }

    // ── against_env ─────────────────────────────────────────────────────────

    #[test]
    fn an_unresolvable_environment_is_refused_not_guessed() {
        // No config at all.
        let e = resolve_env_url(&json!({"env": "staging"}), None).unwrap_err();
        assert!(e.contains("no readable pipeline.yaml"), "{e}");

        // Env absent from the config · the refusal names what does exist.
        let cfg = cfg_with("https://staging.example.test");
        let e = resolve_env_url(&json!({"env": "prod"}), Some(&cfg)).unwrap_err();
        assert!(e.contains("unknown env 'prod'"), "{e}");
        assert!(e.contains("staging"), "must list the known targets · {e}");

        // Target exists but is an SSH host · ✗ usable as a browser origin, and
        // ✗ silently turned into one.
        let cfg = cfg_with("ssh://staging-server");
        let e = resolve_env_url(&json!({"env": "staging"}), Some(&cfg)).unwrap_err();
        assert!(e.contains("ssh://staging-server"), "{e}");
        assert!(e.contains("not an http(s) origin"), "{e}");

        // ✗ default environment · the old code assumed "staging".
        let e = resolve_env_url(&json!({}), None).unwrap_err();
        assert!(e.contains("missing 'env'"), "{e}");
    }

    #[test]
    fn a_resolvable_environment_yields_a_real_origin() {
        let cfg = cfg_with("https://staging.example.test");
        let (url, src) = resolve_env_url(&json!({"env": "staging"}), Some(&cfg)).unwrap();
        assert_eq!(url, "https://staging.example.test");
        assert!(src.contains("deploy.targets.staging.host"), "{src}");

        // Explicit url wins and needs no config.
        let (url, src) = resolve_env_url(&json!({"url": "http://localhost:3000"}), None).unwrap();
        assert_eq!(url, "http://localhost:3000");
        assert_eq!(src, "arg:url");
    }

    #[test]
    fn an_environment_name_is_never_used_as_a_url() {
        // The exact defect: BASE_URL=staging.
        let cfg = cfg_with("https://staging.example.test");
        let (url, _) = resolve_env_url(&json!({"env": "staging"}), Some(&cfg)).unwrap();
        assert_ne!(url, "staging");
        assert!(is_http_origin(&url));
    }

    // ── browser_launch ──────────────────────────────────────────────────────

    #[test]
    fn a_launched_browser_reports_the_url_it_actually_reached() {
        // ! The reported url comes from page.url() in the container, ✗ from the
        // requested argument. Redirects, canonicalisation and login walls are
        // invisible otherwise.
        let logs = format!(
            "some playwright banner\n{SESSION_MARKER}{{\"url\":\"https://example.test/home\",\"title\":\"Home\",\"status\":200}}\n"
        );
        let r = launch_response("pipeline-browser-a", "https://example.test/", &logs);
        assert!(r.ok);
        assert_eq!(r.data["url"], json!("https://example.test/home"));
        assert_eq!(r.data["requested_url"], json!("https://example.test/"));
        assert_eq!(r.data["redirected"], json!(true));
        assert_eq!(r.data["title"], json!("Home"));
        assert_eq!(r.data["http_status"], json!(200));
    }

    #[test]
    fn a_browser_that_never_navigated_does_not_report_success() {
        // The old handler ran `sleep 3600` and echoed browser_launch(<url>),
        // which read as confirmation the page had loaded.
        let r = launch_response("s", "https://x.test", "container started, nothing else\n");
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("no handshake"));

        let logs = format!("{SESSION_MARKER}{{\"error\":\"net::ERR_CONNECTION_REFUSED\"}}");
        let r = launch_response("s", "https://x.test", &logs);
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("ERR_CONNECTION_REFUSED"));
    }

    #[test]
    fn session_names_are_unique_so_a_second_launch_does_not_collide() {
        // Regression: the container name was hardcoded to "pipeline-browser".
        let a = generated_session_name();
        let b = generated_session_name();
        assert_ne!(a, b);
        assert!(a.starts_with("pipeline-browser-"));
        let cmdline = session_argv("/w:/work", &a, "https://x.test");
        assert!(cmdline.contains(&a));
        assert!(
            cmdline.contains(&"PIPELINE_URL=https://x.test".to_owned()),
            "{cmdline:?}"
        );
        // ✗ sleep · the container must run the browser, not idle.
        assert!(!cmdline.iter().any(|s| s == "sleep"), "{cmdline:?}");
        assert!(
            cmdline.iter().any(|s| s.ends_with("session.cjs")),
            "{cmdline:?}"
        );
    }

    // ── escaping ────────────────────────────────────────────────────────────

    #[test]
    fn a_url_containing_a_quote_does_not_break_the_script() {
        let url = "https://x.test/a'b";
        let s = screenshot_script(url, "/work/out.png");
        assert!(s.contains("https://x.test/a\\'b"), "{s}");
        // The goto literal must still be exactly one balanced pair of quotes.
        let line = s.lines().find(|l| l.contains("p.goto(")).unwrap();
        assert_eq!(
            line.matches('\'').count() - line.matches("\\'").count(),
            2,
            "unescaped quote would terminate the literal · {line}"
        );
        // Same flaw, same fix, in devtools_eval.
        let d = devtools_script(url, "return 1");
        assert!(d.contains("https://x.test/a\\'b"), "{d}");
    }

    #[test]
    fn js_quote_neutralises_backslashes_and_newlines() {
        assert_eq!(js_quote(r"a\b"), r"a\\b");
        assert_eq!(js_quote("a\nb"), "a\\nb");
        assert_eq!(js_quote("it's"), r"it\'s");
    }

    #[test]
    fn a_screenshot_returns_the_path_it_wrote() {
        // Regression: outfile was computed and never returned · ok:true with no
        // way to find the artifact.
        let f = screenshot_outfile();
        assert_eq!(
            std::path::Path::new(&f)
                .extension()
                .and_then(|e| e.to_str()),
            Some("png")
        );
        assert!(!f.contains(':'), "path must survive a filesystem · {f}");
        let paths = merge_url(
            json!({"path": "/host/.pipeline/screenshots/x.png", "container_path": "/work/x.png"}),
            "https://x.test",
        );
        let r = merge(ToolResponse::ok(json!({"command": "screenshot"})), paths);
        assert_eq!(r.data["path"], json!("/host/.pipeline/screenshots/x.png"));
        assert_eq!(r.data["url"], json!("https://x.test"));
    }

    // ── a11y ────────────────────────────────────────────────────────────────

    #[test]
    fn a11y_installs_axe_instead_of_requiring_an_absent_module() {
        let cmdline = a11y_argv("/w:/work", "https://x.test", "[]", "critical");
        assert!(cmdline.contains(&"PIPELINE_URL=https://x.test".to_owned()));
        assert!(cmdline.contains(&"PIPELINE_A11Y_FAIL_ON=critical".to_owned()));

        let body = script(A11Y_BODY);
        // The old script required @axe-core/playwright, which exists in neither
        // the repo nor the base image · it could only ever exit MODULE_NOT_FOUND.
        assert!(!body.contains("@axe-core/playwright"), "{body}");
        assert!(body.contains("axe-core@4.10.2"), "pinned · {body}");
        assert!(
            body.contains("/tmp/axe"),
            "✗ install into the audited repo · {body}"
        );
    }

    #[test]
    fn every_script_can_resolve_playwright_in_the_base_image() {
        // ! Verified against mcr.microsoft.com/playwright:v1.49.0-noble: the
        // image carries browser binaries under /ms-playwright but NOT the
        // playwright npm package, and NODE_PATH is empty. A bare
        // require('playwright') is MODULE_NOT_FOUND on any project without
        // node_modules — the exact defect a11y_check was flagged for, which was
        // also sitting under screenshot and devtools_eval.
        for body in [
            A11Y_BODY,
            SESSION_BODY,
            SESSION_EVAL_BODY,
            SESSION_SHOT_BODY,
        ] {
            let s = script(body);
            assert!(
                s.contains("playwright-core@1.49.0"),
                "every script must be able to vendor playwright · {body}"
            );
            assert!(
                !s.contains("require('playwright')"),
                "a bare require would be MODULE_NOT_FOUND · {body}"
            );
        }
        // The one-shot `node -e` scripts carry the same cascade.
        assert!(screenshot_script("https://x.test", "/o.png").contains("playwright-core@1.49.0"));
        assert!(devtools_script("https://x.test", "return 1").contains("playwright-core@1.49.0"));
        // Pin must track the image · mismatched core cannot drive those browsers.
        assert!(PLAYWRIGHT_IMAGE.contains("v1.49.0"));
    }

    #[test]
    fn a11y_reports_rule_impact_and_selector() {
        let stdout = format!(
            "{A11Y_MARKER}{{\"url\":\"https://x.test/\",\"violations\":[{{\"id\":\"color-contrast\",\"impact\":\"serious\",\"selectors\":[\"#hero > p\"]}}],\"passes\":42,\"incomplete\":1}}"
        );
        let raw = ToolResponse {
            ok: false,
            data: json!({"stdout": stdout, "stderr": ""}),
            next_suggested: vec![],
            memory_refs: vec![],
            error: Some("a11y_check exit 1".into()),
        };
        let shaped = shape_a11y_result(raw, "serious");
        assert_eq!(shaped.data["violation_count"], json!(1));
        assert_eq!(shaped.data["violations"][0]["id"], json!("color-contrast"));
        assert_eq!(shaped.data["violations"][0]["impact"], json!("serious"));
        assert_eq!(
            shaped.data["violations"][0]["selectors"][0],
            json!("#hero > p")
        );
        assert!(shaped.error.unwrap().contains("1 accessibility violation"));
    }

    #[test]
    fn an_a11y_scan_that_did_not_run_is_not_a_clean_page() {
        // ! No report → "unassessed", ✗ "no violations". The whole defect class.
        let raw = ToolResponse::ok(json!({"stdout": "", "stderr": "npm ERR! offline"}));
        let shaped = shape_a11y_result(raw, "critical");
        assert!(!shaped.ok);
        assert!(shaped.error.unwrap().contains("unassessed"));
    }

    // ── record ──────────────────────────────────────────────────────────────

    #[test]
    fn record_refuses_instead_of_blocking_forever() {
        let r = record(&json!({"url": "https://x.test"}));
        assert!(!r.ok);
        let e = r.error.unwrap();
        assert!(e.contains("interactive"), "{e}");
        assert!(e.contains("codegen"), "{e}");
    }

    // ── misc ────────────────────────────────────────────────────────────────

    #[test]
    fn marker_payload_ignores_surrounding_banner_output() {
        let out = format!("noise\n{EVAL_MARKER}{{\"result\":7}}\nmore noise");
        let p = marker_payload(&out, EVAL_MARKER).unwrap();
        assert_eq!(p["result"], json!(7));
        assert!(marker_payload("nothing here", EVAL_MARKER).is_none());
    }

    #[test]
    fn truncate_does_not_split_a_multibyte_character() {
        let s = "é".repeat(50);
        let t = truncate(&s, 11);
        assert!(t.contains("truncated"));
        assert!(t.is_char_boundary(0));
    }
}
