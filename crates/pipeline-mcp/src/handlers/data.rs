//! `pipeline_data` handler · DB provision · schema migrate · seed.
//!
//! Day-6 wires: db_provision (writes compose service stub), schema_migrate
//! (sqlx/alembic), seed (writes fixture JSON). Other 5 actions return
//! not_implemented (MVP).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "db_provision" => db_provision(&req.args).await,
        "schema_migrate" => schema_migrate(&req.args).await,
        "seed" => seed(&req.args).await,
        "schema_generate" | "etl_create" | "quality_check" | "db_diff" | "anonymize" => {
            ToolResponse::not_implemented(ToolName::Data, &req.action)
        }
        other => err(format!("unknown action 'pipeline_data.{other}'")),
    }
}

async fn db_provision(args: &Value) -> ToolResponse {
    let engine = args
        .get("engine")
        .and_then(Value::as_str)
        .unwrap_or("postgres");
    let version = args.get("version").and_then(Value::as_str);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join("docker-compose.db.yml");
    let body = match engine {
        "postgres" => compose_postgres(version.unwrap_or("16")),
        "mysql" => compose_mysql(version.unwrap_or("8")),
        "redis" => compose_redis(version.unwrap_or("7")),
        "mongo" => compose_mongo(version.unwrap_or("7")),
        "clickhouse" => compose_clickhouse(version.unwrap_or("24")),
        "sqlite" => {
            return ToolResponse::ok(json!({
                "engine": "sqlite",
                "note": "no compose service needed · sqlite is file-backed",
            }));
        }
        other => return err(format!("unsupported engine '{other}'")),
    };
    if let Err(e) = tokio::fs::write(&path, body).await {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "engine": engine,
            "version": version.unwrap_or("default"),
            "compose_file": path.display().to_string(),
        }),
        next_suggested: vec!["pipeline_docker.compose_up".into()],
        memory_refs: vec![],
        error: None,
    }
}

async fn schema_migrate(args: &Value) -> ToolResponse {
    let stack = args.get("stack").and_then(Value::as_str).unwrap_or("rust");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (program, cmd_args): (&str, Vec<&str>) = match stack {
        "rust" => ("sqlx", vec!["migrate", "run"]),
        "python" | "python-uv" => ("alembic", vec!["upgrade", "head"]),
        "node" | "ts" | "typescript" | "bun" => ("npx", vec!["prisma", "migrate", "deploy"]),
        other => return err(format!("unsupported stack '{other}'")),
    };
    let output = match Command::new(program)
        .args(&cmd_args)
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("{program}: {e} · is it installed?")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "stack": stack,
            "exit_code": output.status.code().unwrap_or(-1),
            "stderr": String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "schema_migrate exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

async fn seed(args: &Value) -> ToolResponse {
    let persona = args
        .get("persona")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let count = args.get("count").and_then(Value::as_u64).unwrap_or(10);
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("seeds");
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join(format!("{persona}.json"));
    let cap = usize::try_from(count).unwrap_or(usize::MAX);
    let mut records: Vec<Value> = Vec::with_capacity(cap);
    for i in 0..count {
        records.push(json!({
            "id": i + 1,
            "persona": persona,
            "name": format!("{persona}_{:04}", i + 1),
            "email": format!("{persona}{:04}@example.test", i + 1),
        }));
    }
    let text = match serde_json::to_string_pretty(&records) {
        Ok(s) => s,
        Err(e) => return err(format!("serialize: {e}")),
    };
    if let Err(e) = tokio::fs::write(&path, text).await {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({"persona": persona, "count": count, "path": path.display().to_string()}),
        next_suggested: vec!["pipeline_data.schema_migrate".into()],
        memory_refs: vec![],
        error: None,
    }
}

fn compose_postgres(v: &str) -> String {
    format!(
        "services:\n  postgres:\n    image: postgres:{v}\n    environment:\n      POSTGRES_USER: app\n      POSTGRES_PASSWORD: app\n      POSTGRES_DB: app\n    ports:\n      - \"5432:5432\"\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U app\"]\n      interval: 5s\n      timeout: 2s\n      retries: 5\n"
    )
}
fn compose_mysql(v: &str) -> String {
    format!(
        "services:\n  mysql:\n    image: mysql:{v}\n    environment:\n      MYSQL_ROOT_PASSWORD: app\n      MYSQL_DATABASE: app\n    ports:\n      - \"3306:3306\"\n"
    )
}
fn compose_redis(v: &str) -> String {
    format!("services:\n  redis:\n    image: redis:{v}-alpine\n    ports:\n      - \"6379:6379\"\n")
}
fn compose_mongo(v: &str) -> String {
    format!("services:\n  mongo:\n    image: mongo:{v}\n    ports:\n      - \"27017:27017\"\n")
}
fn compose_clickhouse(v: &str) -> String {
    format!(
        "services:\n  clickhouse:\n    image: clickhouse/clickhouse-server:{v}\n    ports:\n      - \"8123:8123\"\n      - \"9000:9000\"\n"
    )
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
