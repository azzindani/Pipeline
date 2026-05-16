//! `pipeline_e2e` handler · Playwright in Docker.
//!
//! Day-6 wires: run, record. Other 8 actions return not_implemented.
//! Browser_launch/close need persistent containers · MVP work.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.49.0-noble";

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "run" => run(&req.args).await,
        "record" => record(&req.args).await,
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

async fn run(args: &Value) -> ToolResponse {
    let suite = args.get("suite").and_then(Value::as_str).unwrap_or("");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let mut docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
    ];
    if !suite.is_empty() {
        docker_args.push(suite.into());
    }
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "e2e_run").await
}

async fn record(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "codegen".into(),
        url,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "e2e_record").await
}

async fn capture(program: &str, args: &[&str], cwd: &std::path::Path, label: &str) -> ToolResponse {
    let output = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("{program} spawn: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "command": label,
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": truncate(&String::from_utf8_lossy(&output.stdout), 8_000),
            "stderr": truncate(&String::from_utf8_lossy(&output.stderr), 8_000),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "{label} exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_owned()
    } else {
        format!(
            "{}\n... [truncated · {} more bytes]",
            &s[..max],
            s.len() - max
        )
    }
}

async fn browser_launch(args: &Value) -> ToolResponse {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("about:blank");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        "pipeline-browser".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        PLAYWRIGHT_IMAGE.into(),
        "sleep".into(),
        "3600".into(),
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, &format!("browser_launch({url})")).await
}

async fn browser_close(args: &Value) -> ToolResponse {
    let _ = args;
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    capture(
        "docker",
        &["rm", "-f", "pipeline-browser"],
        &cwd,
        "browser_close",
    )
    .await
}

async fn trace(args: &Value) -> ToolResponse {
    let test_name = match args.get("test").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'test'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
        "--trace".into(),
        "on".into(),
        "-g".into(),
        test_name,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "e2e_trace").await
}

async fn screenshot(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join(".pipeline/screenshots");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let outfile = format!(
        "/work/.pipeline/screenshots/{}.png",
        pipeline_memory::now_rfc3339().replace([':', '.', '+'], "_")
    );
    let mount = format!("{}:/work", cwd.display());
    let inline_script = format!(
        "const {{ chromium }} = require('playwright');\n(async () => {{\n  const b = await chromium.launch();\n  const p = await b.newPage();\n  await p.goto('{url}');\n  await p.screenshot({{ path: '{outfile}', fullPage: true }});\n  await b.close();\n}})();"
    );
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "node".into(),
        "-e".into(),
        inline_script,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "screenshot").await
}

async fn visual_regression(args: &Value) -> ToolResponse {
    let _ = args;
    // Ride on Playwright's built-in toMatchSnapshot · runs the existing suite
    // with snapshot comparison enabled.
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
        "--update-snapshots=missing".into(),
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "visual_regression").await
}

async fn a11y_check(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let inline_script = format!(
        "const {{ chromium }} = require('playwright');\nconst {{ AxeBuilder }} = require('@axe-core/playwright');\n(async () => {{\n  const b = await chromium.launch();\n  const p = await b.newPage();\n  await p.goto('{url}');\n  const r = await new AxeBuilder({{ page: p }}).analyze();\n  console.log(JSON.stringify({{ violations: r.violations.length, passes: r.passes.length }}));\n  await b.close();\n}})();"
    );
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "node".into(),
        "-e".into(),
        inline_script,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "a11y_check").await
}

async fn against_env(args: &Value) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("staging");
    let suite = args.get("suite").and_then(Value::as_str).unwrap_or("");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let env_var = format!("BASE_URL={env}");
    let mut docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        "-e".into(),
        env_var,
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
    ];
    if !suite.is_empty() {
        docker_args.push(suite.to_owned());
    }
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, &format!("e2e_against({env})")).await
}

async fn devtools_eval(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url'".into()),
    };
    let js = match args.get("js").and_then(Value::as_str) {
        Some(j) => j.to_owned(),
        None => return err("missing 'js'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mount = format!("{}:/work", cwd.display());
    let inline = format!(
        "const {{ chromium }} = require('playwright');\n(async () => {{\n  const b = await chromium.launch();\n  const p = await b.newPage();\n  await p.goto('{url}');\n  const r = await p.evaluate(() => {{ {js} }});\n  console.log(JSON.stringify(r));\n  await b.close();\n}})();"
    );
    let docker_args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "-v".into(),
        mount,
        "-w".into(),
        "/work".into(),
        PLAYWRIGHT_IMAGE.into(),
        "node".into(),
        "-e".into(),
        inline,
    ];
    let arr: Vec<&str> = docker_args.iter().map(String::as_str).collect();
    capture("docker", &arr, &cwd, "devtools_eval").await
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
