//! `pipeline_data` handler · DB provision · schema migrate · seed.
//!
//! Day-6 wires: db_provision (writes compose service stub), schema_migrate
//! (sqlx/alembic), seed (writes fixture JSON). Other 5 actions return
//! not_implemented (MVP).

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "db_provision" => db_provision(&req.args).await,
        "schema_migrate" => schema_migrate(&req.args).await,
        "seed" => seed(&req.args).await,
        "schema_generate" => schema_generate(&req.args).await,
        "etl_create" => etl_create(&req.args).await,
        "quality_check" => quality_check(&req.args).await,
        "db_diff" => db_diff(&req.args).await,
        "anonymize" => anonymize(&req.args).await,
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

/// Render a **structured** schema spec to SQL.
///
/// ! This used to write a hardcoded `users` table for every input and report
/// `ok: true`, ignoring the caller's spec entirely — a silent wrong artifact,
/// which is worse than an honest refusal. Pipeline ✗ infers a schema from prose
/// (that needs a model, and Pipeline is not one); it renders what the agent
/// designed, deterministically, or it refuses.
async fn schema_generate(args: &Value) -> ToolResponse {
    let Some(tables) = args.get("tables").and_then(Value::as_array) else {
        return err(SCHEMA_SPEC_HELP.to_owned());
    };
    if tables.is_empty() {
        return err(SCHEMA_SPEC_HELP.to_owned());
    }

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let rel = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("migrations/0001_init.sql");
    let path = cwd.join(rel);
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }

    let sql = match render_schema(args, tables) {
        Ok(s) => s,
        Err(e) => return err(e),
    };

    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = tokio::fs::write(&path, &sql).await {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({
            "path": path.display().to_string(),
            "tables": tables.len(),
            "bytes": sql.len(),
        }),
        next_suggested: vec!["pipeline_data.schema_migrate".into()],
        memory_refs: vec![],
        error: None,
    }
}

const SCHEMA_SPEC_HELP: &str = "\
missing 'tables' · schema_generate renders a structured spec, it ✗ infers one from prose. \
Shape: {\"extensions\":[\"vector\"],\"tables\":[{\"name\":\"chunks\",\
\"columns\":[{\"name\":\"id\",\"type\":\"TEXT\",\"not_null\":true}],\
\"primary_key\":[\"cluster_id\",\"id\"],\"partition_by\":\"HASH (cluster_id)\",\
\"partitions\":64,\
\"indexes\":[{\"name\":\"chunks_tsv_idx\",\"columns\":[\"tsv\"],\"using\":\"GIN\"}]}]}";

/// Deterministic SQL rendering · no I/O, so it is unit-testable on its own.
fn render_schema(root: &Value, tables: &[Value]) -> Result<String, String> {
    use std::fmt::Write as _;
    let mut out = String::from("-- Generated by pipeline_data.schema_generate.\n");
    if let Some(note) = root.get("comment").and_then(Value::as_str) {
        for line in note.lines() {
            let _ = writeln!(out, "-- {line}");
        }
    }
    out.push('\n');

    for ext in root
        .get("extensions")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
    {
        let Some(name) = ext.as_str() else {
            return Err("extensions[] must be strings".into());
        };
        let _ = writeln!(out, "CREATE EXTENSION IF NOT EXISTS {name};");
    }
    if root.get("extensions").is_some() {
        out.push('\n');
    }

    for t in tables {
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or("every table needs a 'name'")?;
        let columns = t
            .get("columns")
            .and_then(Value::as_array)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| format!("table '{name}' needs a non-empty 'columns'"))?;

        if let Some(c) = t.get("comment").and_then(Value::as_str) {
            for line in c.lines() {
                let _ = writeln!(out, "-- {line}");
            }
        }
        let _ = writeln!(out, "CREATE TABLE {name} (");

        let mut parts: Vec<String> = Vec::new();
        for col in columns {
            parts.push(render_column(col, name)?);
        }
        if let Some(pk) = t.get("primary_key").and_then(Value::as_array) {
            let cols = string_list(pk).ok_or("primary_key[] must be strings")?;
            if !cols.is_empty() {
                parts.push(format!("PRIMARY KEY ({})", cols.join(", ")));
            }
        }
        let _ = write!(out, "    {}", parts.join(",\n    "));
        out.push('\n');

        match t.get("partition_by").and_then(Value::as_str) {
            Some(p) => {
                let _ = writeln!(out, ") PARTITION BY {p};");
            }
            None => out.push_str(");\n"),
        }

        // HASH partitioning needs the partitions declared · emit them rather than
        // leaving a partitioned table nothing can be inserted into.
        if let (Some(p), Some(n)) = (
            t.get("partition_by").and_then(Value::as_str),
            t.get("partitions").and_then(Value::as_u64),
        ) {
            if p.trim_start().to_ascii_uppercase().starts_with("HASH") && n > 0 {
                out.push('\n');
                let _ = writeln!(out, "DO $$\nBEGIN\n    FOR i IN 0..{} LOOP", n - 1);
                let _ = writeln!(
                    out,
                    "        EXECUTE format('CREATE TABLE {name}_p%s PARTITION OF {name} \
                     FOR VALUES WITH (MODULUS {n}, REMAINDER %s)', i, i);"
                );
                out.push_str("    END LOOP;\nEND $$;\n");
            }
        }

        for idx in t
            .get("indexes")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
        {
            out.push_str(&render_index(idx, name)?);
        }
        out.push('\n');
    }

    Ok(out)
}

fn render_column(col: &Value, table: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let name = col
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("table '{table}': every column needs a 'name'"))?;
    let ty = col
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("table '{table}', column '{name}': missing 'type'"))?;
    let mut s = format!("{name} {ty}");
    if let Some(expr) = col.get("generated").and_then(Value::as_str) {
        let _ = write!(s, " GENERATED ALWAYS AS ({expr}) STORED");
    }
    if col.get("not_null").and_then(Value::as_bool) == Some(true) {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = col.get("default").and_then(Value::as_str) {
        let _ = write!(s, " DEFAULT {d}");
    }
    if col.get("unique").and_then(Value::as_bool) == Some(true) {
        s.push_str(" UNIQUE");
    }
    if col.get("primary_key").and_then(Value::as_bool) == Some(true) {
        s.push_str(" PRIMARY KEY");
    }
    if let Some(r) = col.get("references").and_then(Value::as_str) {
        let _ = write!(s, " REFERENCES {r}");
    }
    Ok(s)
}

fn render_index(idx: &Value, table: &str) -> Result<String, String> {
    let cols = idx
        .get("columns")
        .and_then(Value::as_array)
        .and_then(|c| string_list(c))
        .filter(|c| !c.is_empty())
        .ok_or_else(|| format!("table '{table}': index needs 'columns'"))?;
    let name = idx.get("name").and_then(Value::as_str).map_or_else(
        || format!("{table}_{}_idx", cols.join("_")),
        ToOwned::to_owned,
    );
    let unique = if idx.get("unique").and_then(Value::as_bool) == Some(true) {
        "UNIQUE "
    } else {
        ""
    };
    let using = idx
        .get("using")
        .and_then(Value::as_str)
        .map(|u| format!(" USING {u}"))
        .unwrap_or_default();
    let filter = idx
        .get("where")
        .and_then(Value::as_str)
        .map(|w| format!(" WHERE {w}"))
        .unwrap_or_default();
    Ok(format!(
        "CREATE {unique}INDEX {name} ON {table}{using} ({}){filter};\n",
        cols.join(", ")
    ))
}

fn string_list(v: &[Value]) -> Option<Vec<String>> {
    v.iter()
        .map(|x| x.as_str().map(ToOwned::to_owned))
        .collect()
}

#[allow(clippy::unused_async)]
async fn etl_create(args: &Value) -> ToolResponse {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("postgres");
    let sink = args
        .get("sink")
        .and_then(Value::as_str)
        .unwrap_or("clickhouse");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("etl");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join("pipeline.yaml");
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = format!(
        "# ETL pipeline scaffolded by pipeline_data.etl_create\nsource:\n  type: {source}\n  url: $SOURCE_URL\n  query: SELECT * FROM events WHERE ts >= $1\nsink:\n  type: {sink}\n  url: $SINK_URL\n  table: events_warehouse\nschedule: \"0 * * * *\"\n"
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"source": source, "sink": sink, "path": path.display().to_string()}))
}

#[allow(clippy::unused_async)]
async fn quality_check(args: &Value) -> ToolResponse {
    let table = match args.get("table").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'table'".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("quality");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join(format!("{table}.checks.yaml"));
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    let body = format!(
        "# Data quality checks for {table} · Great Expectations style\nchecks:\n  - name: id_not_null\n    sql: SELECT COUNT(*) AS n FROM {table} WHERE id IS NULL\n    expect: n == 0\n  - name: row_count_positive\n    sql: SELECT COUNT(*) AS n FROM {table}\n    expect: n > 0\n"
    );
    if let Err(e) = std::fs::write(&path, body) {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({"table": table, "path": path.display().to_string()}))
}

async fn db_diff(args: &Value) -> ToolResponse {
    let env_a = args
        .get("env_a")
        .and_then(Value::as_str)
        .unwrap_or("staging");
    let env_b = args
        .get("env_b")
        .and_then(Value::as_str)
        .unwrap_or("production");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    // Use migra (Python) if available · else helpful note.
    let output = Command::new("migra")
        .args([
            &format!("$DATABASE_URL_{}", env_a.to_uppercase()),
            &format!("$DATABASE_URL_{}", env_b.to_uppercase()),
        ])
        .current_dir(&cwd)
        .output()
        .await;
    match output {
        Ok(o) => ToolResponse {
            ok: o.status.success(),
            data: json!({
                "env_a": env_a, "env_b": env_b,
                "diff": String::from_utf8_lossy(&o.stdout).into_owned(),
                "exit_code": o.status.code().unwrap_or(-1),
            }),
            next_suggested: vec![],
            memory_refs: vec![],
            error: if o.status.success() {
                None
            } else {
                Some("migra failed".into())
            },
        },
        Err(_) => ToolResponse::ok(json!({
            "env_a": env_a, "env_b": env_b,
            "note": "migra not installed · pip install migra · then set DATABASE_URL_<ENV> envs",
        })),
    }
}

#[allow(clippy::unused_async)]
async fn anonymize(args: &Value) -> ToolResponse {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("dump.sql");
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("dump.anon.sql");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let dir = cwd.join("data/anonymize");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return err(format!("mkdir: {e}"));
    }
    let path = dir.join("rules.yaml");
    if !path.exists() {
        let body = "# Anonymization rules · pipeline_data.anonymize\nfields:\n  email: faker.email\n  name: faker.name\n  phone: faker.phone_number\n  ssn: redact\n  password: redact\n";
        if let Err(e) = std::fs::write(&path, body) {
            return err(format!("write rules: {e}"));
        }
    }
    ToolResponse::ok(json!({
        "source": source,
        "target": target,
        "rules": path.display().to_string(),
        "note": "stub · run `pg_anonymize -c rules.yaml -i source -o target` to apply",
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Value {
        json!({
            "extensions": ["vector"],
            "tables": [{
                "name": "chunks",
                "columns": [
                    {"name": "id", "type": "TEXT", "not_null": true},
                    {"name": "cluster_id", "type": "INTEGER", "not_null": true},
                    {"name": "embedding", "type": "halfvec(4096)", "not_null": true},
                    {"name": "tsv", "type": "tsvector",
                     "generated": "to_tsvector('simple', body)"}
                ],
                "primary_key": ["cluster_id", "id"],
                "partition_by": "HASH (cluster_id)",
                "partitions": 4,
                "indexes": [
                    {"name": "chunks_tsv_idx", "columns": ["tsv"], "using": "GIN"},
                    {"name": "chunks_ident_idx", "columns": ["identifier"],
                     "where": "identifier IS NOT NULL"}
                ]
            }]
        })
    }

    #[test]
    fn renders_the_spec_it_was_given() {
        let s = spec();
        let sql = render_schema(&s, s["tables"].as_array().unwrap()).unwrap();
        assert!(sql.contains("CREATE EXTENSION IF NOT EXISTS vector;"));
        assert!(sql.contains("CREATE TABLE chunks ("));
        assert!(sql.contains("embedding halfvec(4096) NOT NULL"));
        assert!(
            sql.contains("tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', body)) STORED")
        );
        assert!(sql.contains("PRIMARY KEY (cluster_id, id)"));
        assert!(sql.contains(") PARTITION BY HASH (cluster_id);"));
        assert!(sql.contains("MODULUS 4"));
        assert!(sql.contains("CREATE INDEX chunks_tsv_idx ON chunks USING GIN (tsv);"));
        assert!(sql.contains("WHERE identifier IS NOT NULL"));
    }

    #[test]
    fn never_invents_a_users_table() {
        // Regression: schema_generate used to ignore the caller's spec entirely and
        // write a hardcoded `users` table while reporting ok — a silent wrong artifact.
        let s = spec();
        let sql = render_schema(&s, s["tables"].as_array().unwrap()).unwrap();
        assert!(
            !sql.contains("users"),
            "must render the given spec, not a fixture"
        );
    }

    #[tokio::test]
    async fn refuses_rather_than_fabricating_when_no_spec_is_given() {
        for args in [
            json!({}),
            json!({"tables": []}),
            json!({"spec": "some prose"}),
        ] {
            let r = schema_generate(&args).await;
            assert!(!r.ok, "must refuse without a structured spec: {args}");
            assert!(r.error.unwrap().contains("missing 'tables'"));
        }
    }

    #[test]
    fn a_table_without_columns_is_an_error_not_empty_sql() {
        let s = json!({"tables": [{"name": "t"}]});
        let e = render_schema(&s, s["tables"].as_array().unwrap()).unwrap_err();
        assert!(e.contains("non-empty 'columns'"));
    }
}
