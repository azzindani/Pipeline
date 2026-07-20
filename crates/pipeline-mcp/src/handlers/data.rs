//! `pipeline_data` handler · DB provision · schema · seed · anonymize · quality.
//!
//! Every action here either does the work its name claims or refuses. The two
//! rules the whole module is built around:
//!
//! - ✗ infer structure from prose · `schema_generate`, `etl_create` and
//!   `anonymize` render the caller's structured spec or refuse. A guessed
//!   schema is a wrong artifact; a guessed PII column is a leak.
//! - ✗ collapse "I could not determine this" into "it is fine" · a missing
//!   `migra`, an unreachable database, or a rule that matched no column is
//!   reported as such, never as a clean result.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
    // Extensions decide the IMAGE, not just a startup step. `postgres:16` has no
    // pgvector, so a schema doing `CREATE EXTENSION vector` fails at migrate time
    // with a compose file that looked fine. Ask for the extension, get an image
    // that actually carries it.
    let extensions: Vec<&str> = args
        .get("extensions")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let path = cwd.join("docker-compose.db.yml");
    let body = match engine {
        "postgres" => match postgres_image(version.unwrap_or("16"), &extensions) {
            Ok(image) => compose_postgres(&image),
            Err(e) => return err(e),
        },
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

/// Pick the Postgres image that actually carries the requested extensions.
///
/// ! Refuses rather than silently handing back stock `postgres:N` for an
/// extension it cannot supply — a compose file that looks right and then fails
/// at `CREATE EXTENSION` is the expensive kind of wrong.
fn postgres_image(version: &str, extensions: &[&str]) -> Result<String, String> {
    // Bundled in the stock image · no special image needed.
    const CONTRIB: &[&str] = &[
        "pg_trgm",
        "btree_gin",
        "btree_gist",
        "hstore",
        "uuid-ossp",
        "pgcrypto",
        "unaccent",
        "citext",
        "ltree",
        "tablefunc",
    ];
    let needs_vector = extensions
        .iter()
        .any(|e| matches!(*e, "vector" | "pgvector" | "vectors"));
    let unknown: Vec<&str> = extensions
        .iter()
        .filter(|e| !CONTRIB.contains(*e) && !matches!(**e, "vector" | "pgvector" | "vectors"))
        .copied()
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "postgres extension(s) {} need an image Pipeline does not know · \
             pass an explicit image, or install them in your own Dockerfile",
            unknown.join(", ")
        ));
    }
    Ok(if needs_vector {
        format!("pgvector/pgvector:pg{version}")
    } else {
        format!("postgres:{version}")
    })
}

fn compose_postgres(image: &str) -> String {
    format!(
        "services:\n  postgres:\n    image: {image}\n    environment:\n      POSTGRES_USER: app\n      POSTGRES_PASSWORD: app\n      POSTGRES_DB: app\n    ports:\n      - \"5432:5432\"\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U app\"]\n      interval: 5s\n      timeout: 2s\n      retries: 5\n"
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

// ───────────────────────────────── ETL ─────────────────────────────────

const ETL_SPEC_HELP: &str = "\
missing 'source'/'sink' spec · etl_create renders a structured job spec, it ✗ invents one. \
Shape: {\"name\":\"events_to_warehouse\",\
\"source\":{\"type\":\"postgres\",\"url_env\":\"SOURCE_URL\",\
\"query\":\"SELECT * FROM events WHERE ts >= $1\"},\
\"sink\":{\"type\":\"clickhouse\",\"url_env\":\"SINK_URL\",\"table\":\"events_warehouse\"},\
\"transform\":[\"lowercase_email\"],\"schedule\":\"0 * * * *\"}";

/// Render a **structured** ETL job spec to YAML.
///
/// ! This used to emit `SELECT * FROM events` → `events_warehouse` for every
/// input, so the file described a job nobody asked for while reporting success.
/// A template that ignores its inputs is a wrong artifact wearing a green flag.
/// Scaffold, ✗ Real: the file is a job definition · Pipeline ✗ executes it.
async fn etl_create(args: &Value) -> ToolResponse {
    let body = match render_etl(args) {
        Ok(b) => b,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("etl_job");
    let rel = args
        .get("path")
        .and_then(Value::as_str)
        .map_or_else(|| format!("etl/{name}.yaml"), ToOwned::to_owned);
    let path = cwd.join(rel);
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = tokio::fs::write(&path, &body).await {
        return err(format!("write: {e}"));
    }
    ToolResponse::ok(json!({
        "name": name,
        "path": path.display().to_string(),
        "bytes": body.len(),
        "note": "job definition only · Pipeline ✗ runs it",
    }))
}

/// Deterministic YAML rendering · no I/O, so it is unit-testable on its own.
fn render_etl(root: &Value) -> Result<String, String> {
    use std::fmt::Write as _;
    let source = root
        .get("source")
        .and_then(Value::as_object)
        .ok_or_else(|| ETL_SPEC_HELP.to_owned())?;
    let sink = root
        .get("sink")
        .and_then(Value::as_object)
        .ok_or_else(|| ETL_SPEC_HELP.to_owned())?;
    let name = root
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("etl_job");
    let mut out = String::from(
        "# Generated by pipeline_data.etl_create · renders the given spec, ✗ a fixture.\n",
    );
    let _ = writeln!(out, "name: {name}");
    out.push_str("source:\n");
    out.push_str(&render_endpoint(source, "source")?);
    out.push_str("sink:\n");
    out.push_str(&render_endpoint(sink, "sink")?);
    match root.get("transform") {
        None | Some(Value::Null) => {}
        Some(Value::Array(steps)) => {
            out.push_str("transform:\n");
            for s in steps {
                let step = s.as_str().ok_or("'transform' entries must be strings")?;
                let _ = writeln!(out, "  - {step}");
            }
        }
        Some(Value::String(s)) => {
            let _ = writeln!(out, "transform: {s}");
        }
        Some(_) => return Err("'transform' must be a string or a list of strings".into()),
    }
    if let Some(s) = root.get("schedule") {
        let cron = s.as_str().ok_or("'schedule' must be a cron string")?;
        let _ = writeln!(out, "schedule: \"{cron}\"");
    }
    Ok(out)
}

fn render_endpoint(ep: &serde_json::Map<String, Value>, side: &str) -> Result<String, String> {
    use std::fmt::Write as _;
    let kind = ep
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("'{side}' needs a 'type' (postgres | clickhouse | s3 | file)"))?;
    let mut out = String::new();
    let _ = writeln!(out, "  type: {kind}");
    if let Some(u) = ep.get("url_env").and_then(Value::as_str) {
        let _ = writeln!(out, "  url: ${u}");
    }
    let table = ep.get("table").and_then(Value::as_str);
    let query = ep.get("query").and_then(Value::as_str);
    // ✗ default the table · "events_warehouse for everyone" is the bug this
    // action was rewritten to remove.
    if table.is_none() && query.is_none() {
        return Err(format!(
            "'{side}' needs a 'table' or a 'query' · ✗ defaulted"
        ));
    }
    if let Some(t) = table {
        let _ = writeln!(out, "  table: {t}");
    }
    if let Some(q) = query {
        let _ = writeln!(out, "  query: {q}");
    }
    Ok(out)
}

// ───────────────────────────── quality check ─────────────────────────────

const QUALITY_CHECKS_HELP: &str = "\
missing 'checks' · quality_check executes assertions, it ✗ invents them. \
Shape: {\"dsn\":\"postgres://…\",\"checks\":[\
{\"table\":\"users\",\"column\":\"email\",\"assert\":\"not_null\"},\
{\"table\":\"users\",\"column\":\"email\",\"assert\":\"unique\"},\
{\"table\":\"orders\",\"column\":\"total\",\"assert\":\"range\",\"min\":0,\"max\":1000000},\
{\"table\":\"orders\",\"column\":\"user_id\",\"assert\":\"referenced\",\"references\":\"users.id\"}]}";

/// One assertion, compiled to the SQL that counts its violations.
#[derive(Debug)]
struct CheckPlan {
    name: String,
    table: String,
    column: String,
    assertion: String,
    sql: String,
}

/// Execute data quality assertions against a live database.
///
/// ! This used to write two fixed SQL strings to a YAML file, never connect,
/// never execute, and return `ok:true` — so an agent gating on "did
/// quality_check pass" received an unconditional yes about a database nobody
/// had touched.
async fn quality_check(args: &Value) -> ToolResponse {
    let Some(dsn) = args.get("dsn").and_then(Value::as_str) else {
        return err(
            "missing 'dsn' · quality_check runs against a live database · ✗ ok for zero work"
                .into(),
        );
    };
    let Some(checks) = args
        .get("checks")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
    else {
        return err(QUALITY_CHECKS_HELP.to_owned());
    };
    let mut plans = Vec::with_capacity(checks.len());
    for c in checks {
        match build_check(c) {
            Ok(p) => plans.push(p),
            Err(e) => return err(e),
        }
    }
    let mut results = Vec::with_capacity(plans.len());
    let mut failed = 0_usize;
    for p in &plans {
        let r = match run_check(dsn, p).await {
            Ok(r) => r,
            // Spawn failure is infrastructural · ✗ a passing check.
            Err(e) => return err(e),
        };
        if r.get("pass").and_then(Value::as_bool) != Some(true) {
            failed += 1;
        }
        results.push(r);
    }
    ToolResponse {
        ok: failed == 0,
        data: json!({"checks": results, "total": plans.len(), "failed": failed}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if failed == 0 {
            None
        } else {
            Some(format!(
                "{failed}/{} data quality checks failed",
                plans.len()
            ))
        },
    }
}

/// Compile one assertion · pure, so the generated SQL is testable without a DB.
fn build_check(c: &Value) -> Result<CheckPlan, String> {
    let table = sql_ident(c.get("table").and_then(Value::as_str), "table")?;
    let column = sql_ident(c.get("column").and_then(Value::as_str), "column")?;
    let assertion = c
        .get("assert")
        .and_then(Value::as_str)
        .ok_or_else(|| QUALITY_CHECKS_HELP.to_owned())?
        .to_ascii_lowercase();
    let violations = match assertion.as_str() {
        "not_null" => format!("SELECT count(*) FROM {table} WHERE {column} IS NULL"),
        "unique" => format!(
            "SELECT count(*) FROM (SELECT {column} FROM {table} WHERE {column} IS NOT NULL \
             GROUP BY {column} HAVING count(*) > 1) dup"
        ),
        "range" => {
            let min = sql_number(c.get("min"), "min")?;
            let max = sql_number(c.get("max"), "max")?;
            format!(
                "SELECT count(*) FROM {table} WHERE {column} IS NOT NULL \
                 AND ({column} < {min} OR {column} > {max})"
            )
        }
        "referenced" => {
            let target = c
                .get("references")
                .and_then(Value::as_str)
                .ok_or("assert 'referenced' needs 'references' as \"table.column\"")?;
            let (rt, rc) = target
                .split_once('.')
                .ok_or("'references' must be \"table.column\"")?;
            let rt = sql_ident(Some(rt), "references table")?;
            let rc = sql_ident(Some(rc), "references column")?;
            format!(
                "SELECT count(*) FROM {table} WHERE {column} IS NOT NULL \
                 AND {column} NOT IN (SELECT {rc} FROM {rt})"
            )
        }
        other => {
            return Err(format!(
                "unknown assert '{other}' · valid: not_null | unique | range | referenced"
            ));
        }
    };
    Ok(CheckPlan {
        name: c.get("name").and_then(Value::as_str).map_or_else(
            || format!("{table}.{column} {assertion}"),
            ToOwned::to_owned,
        ),
        sql: format!("SELECT (SELECT count(*) FROM {table}), ({violations})"),
        table,
        column,
        assertion,
    })
}

/// ! Identifiers are interpolated into SQL we build by hand, so an unvalidated
/// one is an injection. Refuse anything that is not a bare (optionally
/// schema-qualified) identifier rather than quoting and hoping.
fn sql_ident(v: Option<&str>, what: &str) -> Result<String, String> {
    let s = v
        .ok_or_else(|| format!("every check needs a '{what}'"))?
        .trim();
    let valid = !s.is_empty()
        && s.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    if valid {
        Ok(s.to_owned())
    } else {
        Err(format!(
            "'{what}' = '{s}' is not a bare SQL identifier · refusing to interpolate it"
        ))
    }
}

fn sql_number(v: Option<&Value>, what: &str) -> Result<f64, String> {
    v.and_then(Value::as_f64)
        .ok_or_else(|| format!("assert 'range' needs a numeric '{what}'"))
}

async fn run_check(dsn: &str, plan: &CheckPlan) -> Result<Value, String> {
    let output = Command::new("psql")
        .args([dsn, "-At", "-F", "|", "-c", &plan.sql])
        .output()
        .await
        .map_err(|e| format!("psql: {e} · is the postgres client installed?"))?;
    let base = json!({
        "name": plan.name, "table": plan.table,
        "column": plan.column, "assert": plan.assertion,
    });
    let mut out = base.as_object().cloned().unwrap_or_default();
    if !output.status.success() {
        // ! Unreachable host / missing table / bad auth all exit non-zero. The
        // check did not run · that is a failure, ✗ a pass.
        out.insert("pass".into(), json!(false));
        out.insert(
            "error".into(),
            json!(format!(
                "psql exited {} · {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |c| c.to_string()),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        );
        return Ok(Value::Object(out));
    }
    let (rows, violations) = parse_check_counts(&String::from_utf8_lossy(&output.stdout))?;
    out.insert("rows".into(), json!(rows));
    out.insert("violations".into(), json!(violations));
    out.insert("pass".into(), json!(violations == 0));
    Ok(Value::Object(out))
}

/// `psql -At -F|` emits `total|violations` · anything else means the query did
/// not return what we asked for, which is an error, ✗ zero violations.
fn parse_check_counts(stdout: &str) -> Result<(i64, i64), String> {
    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or("psql returned no rows · the counts are UNKNOWN, ✗ zero")?;
    let (a, b) = line
        .trim()
        .split_once('|')
        .ok_or_else(|| format!("unparsable psql output '{}'", line.trim()))?;
    let rows = a
        .trim()
        .parse::<i64>()
        .map_err(|e| format!("row count: {e}"))?;
    let violations = b
        .trim()
        .parse::<i64>()
        .map_err(|e| format!("violation count: {e}"))?;
    Ok((rows, violations))
}

// ─────────────────────────────── db diff ───────────────────────────────

/// Diff two live schemas with `migra`.
///
/// ! Two defects lived here. The literal string `$DATABASE_URL_STAGING` was
/// passed as argv with no shell to expand it, so the tool could never connect;
/// and a spawn failure was converted into `ok:true` with a friendly note — i.e.
/// "migra is not installed" was reported as "the schemas match".
async fn db_diff(args: &Value) -> ToolResponse {
    let (dsn_a, from_a) = match resolve_dsn(args.get("dsn_a"), args.get("env_a"), "a") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let (dsn_b, from_b) = match resolve_dsn(args.get("dsn_b"), args.get("env_b"), "b") {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let output = match Command::new("migra")
        .args(["--unsafe", &dsn_a, &dsn_b])
        .output()
        .await
    {
        Ok(o) => o,
        // ! Scanner missing → the difference is UNKNOWN. ✗ ok.
        Err(e) => {
            return err(format!(
                "migra: {e} · pip install migra · schema difference is UNKNOWN, ✗ empty"
            ));
        }
    };
    // migra's contract: 0 → identical · 2 → differences found · anything else is
    // a real failure (bad DSN, unreachable host, unsupported object).
    let diff = String::from_utf8_lossy(&output.stdout).into_owned();
    let payload = json!({
        "a": from_a, "b": from_b,
        "identical": output.status.code() == Some(0),
        "diff": diff,
    });
    match output.status.code() {
        Some(0 | 2) => ToolResponse::ok(payload),
        code => err(format!(
            "migra exited {} · {}",
            code.map_or_else(|| "signal".to_owned(), |c| c.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// Resolve one side's connection string · explicit DSN, or an env var read here
/// with [`std::env::var`] rather than handed to a process that has no shell.
fn resolve_dsn(
    dsn: Option<&Value>,
    env: Option<&Value>,
    side: &str,
) -> Result<(String, String), String> {
    if let Some(d) = dsn.and_then(Value::as_str) {
        return Ok((d.to_owned(), format!("dsn_{side}")));
    }
    let Some(name) = env.and_then(Value::as_str) else {
        return Err(format!(
            "missing 'dsn_{side}' and 'env_{side}' · pass a DSN, or an environment name \
             whose DATABASE_URL_<ENV> Pipeline should read"
        ));
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "env_{side} = '{name}' is not a valid environment name"
        ));
    }
    let var = format!("DATABASE_URL_{}", name.to_ascii_uppercase());
    std::env::var(&var)
        .map(|v| (v, var.clone()))
        .map_err(|_| format!("{var} is not set · the schema difference is UNKNOWN, ✗ empty"))
}

// ─────────────────────────────── anonymize ───────────────────────────────

const ANONYMIZE_RULES_HELP: &str = "\
missing 'rules' · anonymize ✗ guesses which columns hold PII — guessing is how PII leaks. \
Shape: {\"source\":\"dump.sql\",\"target\":\"safe.sql\",\"rules\":{\
\"email\":\"fake_email\",\"full_name\":\"fake_name\",\"ssn\":\"redact\",\
\"user_id\":\"hash\",\"notes\":\"null\",\"created_at\":\"preserve\"}} \
· strategies: redact | hash | fake_email | fake_name | null | preserve";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Strategy {
    Redact,
    Hash,
    FakeEmail,
    FakeName,
    Null,
    Preserve,
}

impl Strategy {
    fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "redact" => Ok(Self::Redact),
            "hash" => Ok(Self::Hash),
            "fake_email" => Ok(Self::FakeEmail),
            "fake_name" => Ok(Self::FakeName),
            "null" => Ok(Self::Null),
            "preserve" => Ok(Self::Preserve),
            other => Err(format!(
                "unknown strategy '{other}' · valid: \
                 redact | hash | fake_email | fake_name | null | preserve"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Redact => "redact",
            Self::Hash => "hash",
            Self::FakeEmail => "fake_email",
            Self::FakeName => "fake_name",
            Self::Null => "null",
            Self::Preserve => "preserve",
        }
    }
}

/// `SHA-256` hex · deterministic, so joins on a hashed key survive.
///
/// ! Determinism preserves cardinality: a low-entropy column (postcode, birth
/// year, boolean) stays re-identifiable by dictionary attack. Use `redact` or
/// `null` for those — `hash` is pseudonymisation, ✗ anonymisation.
fn digest(value: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(value.as_bytes());
    format!("{:x}", h.finalize())
}

/// `None` → leave the field exactly as it was.
fn transform(strategy: Strategy, value: &str, null_token: &str) -> Option<String> {
    match strategy {
        Strategy::Preserve => None,
        Strategy::Redact => Some("[REDACTED]".to_owned()),
        Strategy::Null => Some(null_token.to_owned()),
        Strategy::Hash => Some(digest(value)[..32].to_owned()),
        Strategy::FakeEmail => Some(format!("user{}@example.invalid", &digest(value)[..12])),
        Strategy::FakeName => Some(format!("Person {}", &digest(value)[..8])),
    }
}

/// Column → strategy, plus the evidence that each rule did something.
///
/// ! `Debug` is safe here only because this struct holds column names and
/// counts · never a value read out of the source. Keep it that way.
#[derive(Debug)]
struct Anonymizer {
    rules: BTreeMap<String, Strategy>,
    /// Ruled columns that actually appeared in a header · a rule missing from
    /// this set is a typo, and must surface rather than pass as clean work.
    seen: BTreeSet<String>,
    transformed: BTreeMap<String, u64>,
}

/// Per-column slot: `None` → no rule, so the field is copied untouched.
type Plan = Vec<Option<(String, Strategy)>>;

impl Anonymizer {
    fn new(rules: &Value) -> Result<Self, String> {
        let obj = rules
            .as_object()
            .filter(|o| !o.is_empty())
            .ok_or_else(|| ANONYMIZE_RULES_HELP.to_owned())?;
        let mut map = BTreeMap::new();
        for (col, v) in obj {
            let s = v
                .as_str()
                .ok_or_else(|| format!("rule '{col}' must be a strategy string"))?;
            map.insert(col.trim().to_ascii_lowercase(), Strategy::parse(s)?);
        }
        Ok(Self {
            transformed: map.keys().map(|k| (k.clone(), 0)).collect(),
            rules: map,
            seen: BTreeSet::new(),
        })
    }

    /// Match a header row against the rules · records which rules this block hit.
    fn plan(&mut self, header: &[String]) -> Plan {
        header
            .iter()
            .map(|h| {
                let key = h.trim().trim_matches('"').to_ascii_lowercase();
                let s = self.rules.get(&key).copied()?;
                self.seen.insert(key.clone());
                Some((key, s))
            })
            .collect()
    }

    /// ! The original value is never returned, logged, or stored — only the
    /// count of how many times it changed.
    fn apply(&mut self, key: &str, s: Strategy, value: &str, null_token: &str) -> Option<String> {
        let out = transform(s, value, null_token)?;
        *self.transformed.entry(key.to_owned()).or_insert(0) += 1;
        Some(out)
    }

    /// ! Zero matched columns means the target is a byte-for-byte copy carrying
    /// every original value. Shipping that as "anonymized" is precisely the
    /// failure this action exists to prevent, so it is a refusal.
    fn verify(&self, rows: u64) -> Result<(), String> {
        if rows == 0 {
            return Err(
                "no data rows recognised in 'source' · nothing was anonymized · check 'format'"
                    .into(),
            );
        }
        if self.seen.is_empty() {
            let named: Vec<&str> = self.rules.keys().map(String::as_str).collect();
            return Err(format!(
                "no rule matched any column · rules name [{}] · nothing was anonymized",
                named.join(", ")
            ));
        }
        Ok(())
    }

    fn report(&self, rows: u64) -> Value {
        let per_rule: Vec<Value> = self
            .rules
            .iter()
            .map(|(col, s)| {
                json!({
                    "column": col,
                    "strategy": s.name(),
                    "column_found": self.seen.contains(col),
                    "values_transformed": self.transformed.get(col).copied().unwrap_or(0),
                })
            })
            .collect();
        // Two distinct typo signals · a column that was never in the data, and a
        // column that was there but whose rule changed nothing.
        let no_column: Vec<&String> = self
            .rules
            .keys()
            .filter(|c| !self.seen.contains(*c))
            .collect();
        let no_values: Vec<&String> = self
            .rules
            .iter()
            .filter(|(c, s)| {
                **s != Strategy::Preserve
                    && self.seen.contains(*c)
                    && self.transformed.get(*c).copied().unwrap_or(0) == 0
            })
            .map(|(c, _)| c)
            .collect();
        json!({
            "rows_processed": rows,
            "rules": per_rule,
            "rules_that_matched_no_column": no_column,
            "rules_that_transformed_nothing": no_values,
        })
    }
}

/// Anonymize a CSV or SQL dump into a new file.
///
/// ! This used to read `source` and `target` only to echo them back, opening
/// neither, and return `ok:true`. The dangerous failure is an agent shipping
/// `safe.sql` believing Pipeline scrubbed it.
async fn anonymize(args: &Value) -> ToolResponse {
    let Some(source) = args.get("source").and_then(Value::as_str) else {
        return err("missing 'source' · path to the dump or CSV to anonymize".into());
    };
    let Some(target) = args.get("target").and_then(Value::as_str) else {
        return err("missing 'target' · path to write the anonymized copy to".into());
    };
    let Some(rules) = args.get("rules") else {
        return err(ANONYMIZE_RULES_HELP.to_owned());
    };
    let mut anon = match Anonymizer::new(rules) {
        Ok(a) => a,
        Err(e) => return err(e),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (src, dst) = (cwd.join(source), cwd.join(target));
    if dst.exists() {
        return err(format!("refusing to overwrite {}", dst.display()));
    }
    let as_csv = match dump_is_csv(args.get("format").and_then(Value::as_str), source) {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    let input = match tokio::fs::read_to_string(&src).await {
        Ok(s) => s,
        Err(e) => return err(format!("read {}: {e}", src.display())),
    };
    let outcome = if as_csv {
        anonymize_csv(&input, &mut anon)
    } else {
        anonymize_sql(&input, &mut anon)
    };
    let (body, rows) = match outcome {
        Ok(v) => v,
        Err(e) => return err(e),
    };
    if let Err(e) = anon.verify(rows) {
        return err(e);
    }
    if let Some(parent) = dst.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if let Err(e) = tokio::fs::write(&dst, &body).await {
        return err(format!("write {}: {e}", dst.display()));
    }
    let mut data = anon.report(rows);
    data["target"] = json!(dst.display().to_string());
    data["format"] = json!(if as_csv { "csv" } else { "sql" });
    ToolResponse::ok(data)
}

fn dump_is_csv(explicit: Option<&str>, source: &str) -> Result<bool, String> {
    match explicit.map(str::to_ascii_lowercase).as_deref() {
        Some("csv") => return Ok(true),
        Some("sql") => return Ok(false),
        Some(other) => return Err(format!("unknown format '{other}' · valid: csv | sql")),
        None => {}
    }
    let ext = std::path::Path::new(source)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "csv" {
        Ok(true)
    } else if ext == "sql" || ext == "dump" {
        Ok(false)
    } else {
        // ✗ guess · the wrong parser silently copies the file through untouched.
        Err(format!(
            "cannot tell the format of '{source}' from its extension · pass 'format': csv | sql"
        ))
    }
}

fn split_csv(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            '"' => quoted = true,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if quoted {
        return None;
    }
    out.push(cur);
    Some(out)
}

fn join_csv(fields: &[String]) -> String {
    fields
        .iter()
        .map(|f| {
            if f.contains([',', '"', '\n']) {
                format!("\"{}\"", f.replace('"', "\"\""))
            } else {
                f.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Returns the rewritten file and the number of data rows processed.
fn anonymize_csv(input: &str, anon: &mut Anonymizer) -> Result<(String, u64), String> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    let mut plan: Option<Plan> = None;
    let mut rows = 0_u64;
    for (n, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            let _ = writeln!(out, "{line}");
            continue;
        }
        // ! ✗ echo the row into any error — that leaks the PII we were asked to
        // remove into a log the caller was told was safe.
        let fields = split_csv(line).ok_or_else(|| {
            format!(
                "line {}: unterminated quote · a field spanning lines is unsupported",
                n + 1
            )
        })?;
        if plan.is_none() {
            plan = Some(anon.plan(&fields));
            let _ = writeln!(out, "{line}");
            continue;
        }
        let p = plan
            .as_ref()
            .expect("header parsed on the first non-empty line");
        if fields.len() != p.len() {
            return Err(format!(
                "line {}: {} fields, header has {} · refusing to guess the alignment",
                n + 1,
                fields.len(),
                p.len()
            ));
        }
        let mut fields = fields;
        for (i, slot) in p.iter().enumerate() {
            if let Some((key, s)) = slot {
                if let Some(v) = anon.apply(key, *s, &fields[i], "") {
                    fields[i] = v;
                }
            }
        }
        let _ = writeln!(out, "{}", join_csv(&fields));
        rows += 1;
    }
    if plan.is_none() {
        return Err("no header row · nothing to anonymize".into());
    }
    Ok((out, rows))
}

/// Handles the two shapes `pg_dump` emits: `COPY … FROM stdin` blocks and
/// column-qualified `INSERT` statements. Everything else is copied verbatim.
fn anonymize_sql(input: &str, anon: &mut Anonymizer) -> Result<(String, u64), String> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    let mut copy: Option<Plan> = None;
    let mut rows = 0_u64;
    for (n, line) in input.lines().enumerate() {
        if let Some(plan) = copy.take() {
            if line.trim_end() == "\\." {
                let _ = writeln!(out, "{line}");
                continue; // block closed · `copy` stays None
            }
            let row = copy_row(line, &plan, anon).map_err(|e| format!("line {}: {e}", n + 1))?;
            let _ = writeln!(out, "{row}");
            rows += 1;
            copy = Some(plan);
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("COPY ") && t.trim_end().ends_with("FROM stdin;") {
            let cols = copy_columns(t).map_err(|e| format!("line {}: {e}", n + 1))?;
            copy = Some(anon.plan(&cols));
            let _ = writeln!(out, "{line}");
            continue;
        }
        // `get` ✗ slicing · a line opening with a multibyte char would panic on a
        // byte index that is not a char boundary.
        if t.get(..11)
            .is_some_and(|h| h.eq_ignore_ascii_case("INSERT INTO"))
        {
            let (stmt, k) =
                anonymize_insert(line, anon).map_err(|e| format!("line {}: {e}", n + 1))?;
            let _ = writeln!(out, "{stmt}");
            rows += k;
            continue;
        }
        let _ = writeln!(out, "{line}");
    }
    if copy.is_some() {
        return Err("unterminated COPY block · the dump is truncated".into());
    }
    Ok((out, rows))
}

fn copy_row(line: &str, plan: &Plan, anon: &mut Anonymizer) -> Result<String, String> {
    let mut fields: Vec<String> = line.split('\t').map(ToOwned::to_owned).collect();
    if fields.len() != plan.len() {
        return Err(format!(
            "COPY row has {} columns, the block declares {}",
            fields.len(),
            plan.len()
        ));
    }
    for (i, slot) in plan.iter().enumerate() {
        let Some((key, s)) = slot else { continue };
        // Already NULL · there is nothing to anonymize, and counting it would
        // overstate how much the rule did.
        if fields[i] == "\\N" {
            continue;
        }
        if let Some(v) = anon.apply(key, *s, &fields[i], "\\N") {
            fields[i] = v;
        }
    }
    Ok(fields.join("\t"))
}

/// `COPY public.users (id, email) FROM stdin;` → `["id", "email"]`.
fn copy_columns(stmt: &str) -> Result<Vec<String>, String> {
    let open = stmt.find('(').ok_or(
        "COPY without a column list · re-dump with explicit columns, otherwise rules ✗ map to fields",
    )?;
    let close = stmt[open..]
        .find(')')
        .map(|i| open + i)
        .ok_or("malformed COPY column list")?;
    if close <= open + 1 {
        return Err("empty COPY column list".into());
    }
    Ok(stmt[open + 1..close]
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_owned())
        .collect())
}

fn anonymize_insert(line: &str, anon: &mut Anonymizer) -> Result<(String, u64), String> {
    let (cols, after) = insert_columns(line)?;
    let plan = anon.plan(&cols);
    let rest = &line[after..];
    let vpos = rest
        .to_ascii_uppercase()
        .find("VALUES")
        .ok_or("INSERT without a VALUES clause")?;
    let (values, rows) = rewrite_values(&rest[vpos + 6..], &plan, anon)?;
    Ok((
        format!("{}{}{}", &line[..after], &rest[..vpos + 6], values),
        rows,
    ))
}

/// Column list plus the byte offset just past its closing paren.
fn insert_columns(line: &str) -> Result<(Vec<String>, usize), String> {
    const NO_LIST: &str = "INSERT without a column list · re-dump with --column-inserts, otherwise rules ✗ map to fields";
    let open = line.find('(').ok_or(NO_LIST)?;
    // `INSERT INTO t VALUES (…)` — the first paren is the tuple, not a column
    // list. Mapping rules positionally there would scramble the data.
    if line[..open].to_ascii_uppercase().contains("VALUES") {
        return Err(NO_LIST.into());
    }
    let close = line[open..]
        .find(')')
        .map(|i| open + i)
        .ok_or("malformed INSERT column list")?;
    let cols = line[open + 1..close]
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_owned())
        .collect();
    Ok((cols, close + 1))
}

/// Rewrite every top-level `(…)` tuple in a VALUES tail · quote- and
/// depth-aware, so a `,` or `)` inside a literal ✗ splits the row.
fn rewrite_values(tail: &str, plan: &Plan, anon: &mut Anonymizer) -> Result<(String, u64), String> {
    let mut out = String::with_capacity(tail.len());
    let mut rows = 0_u64;
    let mut chars = tail.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '(' {
            out.push(c);
            continue;
        }
        let mut inner = String::new();
        let (mut quoted, mut depth, mut closed) = (false, 0_u32, false);
        while let Some(d) = chars.next() {
            match d {
                '\'' if quoted && chars.peek() == Some(&'\'') => {
                    inner.push_str("''");
                    chars.next();
                }
                '\'' => {
                    quoted = !quoted;
                    inner.push(d);
                }
                '(' if !quoted => {
                    depth += 1;
                    inner.push(d);
                }
                ')' if !quoted && depth == 0 => {
                    closed = true;
                    break;
                }
                ')' if !quoted => {
                    depth -= 1;
                    inner.push(d);
                }
                _ => inner.push(d),
            }
        }
        if !closed {
            return Err("unterminated VALUES tuple · a multi-line INSERT is unsupported".into());
        }
        out.push('(');
        out.push_str(&rewrite_tuple(&inner, plan, anon)?);
        out.push(')');
        rows += 1;
    }
    Ok((out, rows))
}

fn rewrite_tuple(inner: &str, plan: &Plan, anon: &mut Anonymizer) -> Result<String, String> {
    let mut toks = split_sql_tuple(inner);
    if toks.len() != plan.len() {
        return Err(format!(
            "VALUES tuple has {} items, the column list has {}",
            toks.len(),
            plan.len()
        ));
    }
    for (i, slot) in plan.iter().enumerate() {
        let Some((key, s)) = slot else { continue };
        let raw = toks[i].trim().to_owned();
        if raw.eq_ignore_ascii_case("NULL") {
            continue;
        }
        let literal = unquote_sql(&raw);
        if let Some(v) = anon.apply(key, *s, &literal, "NULL") {
            toks[i] = if *s == Strategy::Null {
                v
            } else {
                format!("'{}'", v.replace('\'', "''"))
            };
        } else {
            toks[i] = raw;
        }
    }
    Ok(toks.join(", "))
}

fn split_sql_tuple(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut depth) = (false, 0_u32);
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if quoted && chars.peek() == Some(&'\'') => {
                cur.push_str("''");
                chars.next();
            }
            '\'' => {
                quoted = !quoted;
                cur.push(c);
            }
            '(' if !quoted => {
                depth += 1;
                cur.push(c);
            }
            ')' if !quoted => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if !quoted && depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn unquote_sql(tok: &str) -> String {
    let t = tok.trim();
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        t[1..t.len() - 1].replace("''", "'")
    } else {
        t.to_owned()
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
    fn pgvector_asks_for_an_image_that_actually_has_pgvector() {
        // Regression: db_provision emitted stock postgres:16 regardless, so a schema
        // doing CREATE EXTENSION vector failed at migrate time against a compose
        // file that looked correct.
        assert_eq!(
            postgres_image("16", &["vector"]).unwrap(),
            "pgvector/pgvector:pg16"
        );
        assert_eq!(postgres_image("16", &[]).unwrap(), "postgres:16");
        // Contrib extensions ship in the stock image.
        assert_eq!(postgres_image("16", &["pg_trgm"]).unwrap(), "postgres:16");
        // ✗ silently hand back an image that cannot supply the extension.
        let e = postgres_image("16", &["timescaledb"]).unwrap_err();
        assert!(e.contains("timescaledb"), "{e}");
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

    // ───────────────────────────── anonymize ─────────────────────────────

    const CSV: &str = "id,email,full_name,city\n1,ada@example.com,Ada Lovelace,London\n\
                       2,alan@example.com,Alan Turing,Wilmslow\n";

    fn rules() -> Value {
        json!({"email": "fake_email", "full_name": "redact", "id": "hash"})
    }

    fn run_csv(rules: &Value, csv: &str) -> (String, u64, Anonymizer) {
        let mut a = Anonymizer::new(rules).expect("rules parse");
        let (out, rows) = anonymize_csv(csv, &mut a).expect("csv anonymizes");
        (out, rows, a)
    }

    #[tokio::test]
    async fn anonymize_refuses_without_rules_rather_than_guessing() {
        // ! Guessing which columns hold PII is how PII leaks · a dump that looks
        // scrubbed and is not is the worst possible artifact to ship.
        for args in [
            json!({"source": "d.sql", "target": "s.sql"}),
            json!({"source": "d.sql", "target": "s.sql", "rules": {}}),
            json!({"source": "d.sql", "target": "s.sql", "rules": "email"}),
        ] {
            let r = anonymize(&args).await;
            assert!(!r.ok, "must refuse without structured rules: {args}");
            assert!(r.error.unwrap().contains("rules"));
        }
    }

    #[test]
    fn a_rule_that_matched_nothing_is_reported_not_silent() {
        // A rule naming a column the data does not have is a likely typo. Silence
        // there reads as "that column was scrubbed".
        let (_, rows, a) = run_csv(&json!({"emial": "fake_email", "city": "redact"}), CSV);
        let report = a.report(rows);
        let missed = report["rules_that_matched_no_column"].as_array().unwrap();
        assert_eq!(missed, &vec![json!("emial")], "the typo must surface");
        // The rule that did match must not be dragged down with it.
        let city = report["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["column"] == "city")
            .unwrap()
            .clone();
        assert_eq!(city["values_transformed"], 2);
        assert_eq!(city["column_found"], json!(true));
    }

    #[test]
    fn every_rule_missing_its_column_is_a_refusal_not_a_clean_copy() {
        // ! Zero matched columns → the target is a byte-for-byte copy of the
        // original, carrying every value it was asked to remove.
        let (_, rows, a) = run_csv(&json!({"nope": "redact"}), CSV);
        let e = a.verify(rows).unwrap_err();
        assert!(e.contains("no rule matched any column"), "{e}");
        assert!(e.contains("nothing was anonymized"), "{e}");
    }

    #[test]
    fn anonymize_never_echoes_an_original_value() {
        // ! The output file, the counts, and the report must contain no original
        // value — not in a sample, not in an error, not in a "before" field.
        let (out, rows, a) = run_csv(&rules(), CSV);
        let report = serde_json::to_string(&a.report(rows)).unwrap();
        for secret in [
            "ada@example.com",
            "alan@example.com",
            "Ada Lovelace",
            "Alan Turing",
        ] {
            assert!(!out.contains(secret), "output leaked {secret}");
            assert!(!report.contains(secret), "report leaked {secret}");
        }
        // Unruled columns pass through · anonymization ✗ destruction.
        assert!(out.contains("London") && out.contains("Wilmslow"));
    }

    #[test]
    fn counts_report_values_actually_transformed() {
        let (_, rows, a) = run_csv(&rules(), CSV);
        assert_eq!(rows, 2);
        let report = a.report(rows);
        for r in report["rules"].as_array().unwrap() {
            assert_eq!(r["values_transformed"], 2, "{r}");
        }
        assert!(
            report["rules_that_transformed_nothing"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn hashing_is_deterministic_so_joins_survive() {
        let (out, _, _) = run_csv(&json!({"id": "hash"}), "id,x\n7,a\n7,b\n");
        let ids: Vec<&str> = out
            .lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap())
            .collect();
        assert_eq!(ids[0], ids[1], "the same input must map to the same token");
        assert_ne!(ids[0], "7");
    }

    #[test]
    fn preserve_keeps_the_column_and_is_not_flagged_as_a_dead_rule() {
        let (out, rows, a) = run_csv(&json!({"city": "preserve"}), CSV);
        assert!(out.contains("London"));
        let report = a.report(rows);
        // Found, transformed nothing · by design, so ✗ in either typo list.
        assert!(
            report["rules_that_matched_no_column"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            report["rules_that_transformed_nothing"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_copy_block_is_anonymized_and_the_rest_of_the_dump_is_untouched() {
        let dump = "SET client_encoding = 'UTF8';\n\
                    COPY public.users (id, email, city) FROM stdin;\n\
                    1\tada@example.com\tLondon\n\
                    2\t\\N\tParis\n\
                    \\.\n";
        let mut a = Anonymizer::new(&json!({"email": "fake_email"})).unwrap();
        let (out, rows) = anonymize_sql(dump, &mut a).unwrap();
        assert_eq!(rows, 2);
        assert!(!out.contains("ada@example.com"));
        assert!(out.contains("SET client_encoding = 'UTF8';"));
        assert!(out.contains("\\.\n"), "the block terminator must survive");
        // An already-NULL field is nothing to anonymize · counting it would
        // overstate what the rule did.
        assert_eq!(a.transformed["email"], 1);
        assert!(out.contains("2\t\\N\tParis"));
    }

    #[test]
    fn column_inserts_are_rewritten_in_place() {
        let dump = "INSERT INTO public.users (id, email) VALUES (1, 'ada@example.com');\n";
        let mut a = Anonymizer::new(&json!({"email": "redact"})).unwrap();
        let (out, rows) = anonymize_sql(dump, &mut a).unwrap();
        assert_eq!(rows, 1);
        assert!(!out.contains("ada@example.com"));
        assert!(out.contains("'[REDACTED]'"), "{out}");
        assert!(out.starts_with("INSERT INTO public.users (id, email) VALUES ("));
    }

    #[test]
    fn a_valueless_insert_is_refused_rather_than_mapped_positionally() {
        // ! `INSERT INTO t VALUES (…)` has no column list · treating the tuple as
        // one would scramble every rule onto the wrong field.
        let mut a = Anonymizer::new(&json!({"email": "redact"})).unwrap();
        let e =
            anonymize_sql("INSERT INTO t VALUES (1, 'ada@example.com');\n", &mut a).unwrap_err();
        assert!(e.contains("column list"), "{e}");
    }

    #[test]
    fn a_quoted_comma_does_not_split_a_row() {
        let (out, rows, _) = run_csv(
            &json!({"email": "redact"}),
            "email,note\nada@example.com,\"hi, there\"\n",
        );
        assert_eq!(rows, 1);
        assert!(out.contains("\"hi, there\""), "{out}");
    }

    #[test]
    fn an_unknown_strategy_names_the_valid_ones() {
        let e = Anonymizer::new(&json!({"email": "faker.email"})).unwrap_err();
        assert!(e.contains("unknown strategy"), "{e}");
        assert!(e.contains("fake_email"), "{e}");
    }

    #[test]
    fn an_ambiguous_extension_is_refused_rather_than_parsed_with_the_wrong_reader() {
        // The wrong parser finds no rows and copies the file through untouched.
        assert!(dump_is_csv(None, "backup").is_err());
        assert!(dump_is_csv(None, "d.CSV").unwrap());
        assert!(!dump_is_csv(Some("sql"), "d.csv").unwrap());
    }

    // ─────────────────────────── quality_check ───────────────────────────

    #[tokio::test]
    async fn quality_check_without_a_dsn_is_a_refusal_not_a_pass() {
        // Regression: it wrote two fixed SQL strings to a file, never connected,
        // and returned ok:true — an unconditional yes to "did quality pass".
        let r = quality_check(&json!({"checks": [{"table": "t", "column": "c",
                                                  "assert": "not_null"}]}))
        .await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("dsn"));
    }

    #[tokio::test]
    async fn quality_check_without_checks_refuses_rather_than_inventing_them() {
        let r = quality_check(&json!({"dsn": "postgres://x"})).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("checks"));
    }

    #[test]
    fn each_assertion_compiles_to_sql_that_counts_its_own_violations() {
        let p = build_check(&json!({"table": "users", "column": "email",
                                    "assert": "not_null"}))
        .unwrap();
        assert!(p.sql.contains("SELECT count(*) FROM users"));
        assert!(p.sql.contains("WHERE email IS NULL"));

        let u =
            build_check(&json!({"table": "users", "column": "email", "assert": "unique"})).unwrap();
        assert!(u.sql.contains("HAVING count(*) > 1"));

        let r = build_check(&json!({"table": "o", "column": "total", "assert": "range",
                                    "min": 0, "max": 100}))
        .unwrap();
        assert!(r.sql.contains("total < 0 OR total > 100"), "{}", r.sql);

        let f = build_check(
            &json!({"table": "o", "column": "user_id", "assert": "referenced",
                                    "references": "users.id"}),
        )
        .unwrap();
        assert!(f.sql.contains("NOT IN (SELECT id FROM users)"), "{}", f.sql);
    }

    #[test]
    fn an_identifier_that_is_not_an_identifier_is_refused_not_interpolated() {
        // ! We build this SQL by hand · an unvalidated identifier is an injection.
        let e = build_check(&json!({"table": "users; DROP TABLE users", "column": "id",
                                    "assert": "not_null"}))
        .unwrap_err();
        assert!(e.contains("not a bare SQL identifier"), "{e}");
        assert!(build_check(&json!({"table": "s.t", "column": "c", "assert": "not_null"})).is_ok());
    }

    #[test]
    fn an_unknown_assertion_is_refused_rather_than_skipped() {
        // A silently skipped check is a check that always passes.
        let e = build_check(&json!({"table": "t", "column": "c", "assert": "vibes"})).unwrap_err();
        assert!(e.contains("unknown assert"), "{e}");
    }

    #[test]
    fn unreadable_psql_output_is_an_error_not_zero_violations() {
        assert_eq!(parse_check_counts("10|3\n").unwrap(), (10, 3));
        assert!(parse_check_counts("").is_err(), "no rows ✗ zero violations");
        assert!(parse_check_counts("ERROR: relation does not exist\n").is_err());
    }

    // ───────────────────────────── db_diff ─────────────────────────────

    #[test]
    fn a_connection_string_resolves_here_not_in_argv() {
        // Regression: the literal "$DATABASE_URL_STAGING" was passed as an
        // argument · no shell, so migra could never connect.
        let (dsn, from) = resolve_dsn(Some(&json!("postgres://a")), None, "a").unwrap();
        assert_eq!(dsn, "postgres://a");
        assert_eq!(from, "dsn_a");
    }

    #[test]
    fn a_missing_env_var_is_unknown_not_an_empty_diff() {
        let e = resolve_dsn(None, Some(&json!("nowhere_env_9f2")), "b").unwrap_err();
        assert!(e.contains("DATABASE_URL_NOWHERE_ENV_9F2"), "{e}");
        assert!(e.contains("UNKNOWN"), "{e}");
        // Neither form supplied · name both ways out.
        let e = resolve_dsn(None, None, "a").unwrap_err();
        assert!(e.contains("dsn_a") && e.contains("env_a"), "{e}");
    }

    // ───────────────────────────── etl_create ─────────────────────────────

    #[test]
    fn etl_renders_the_spec_it_was_given() {
        // Regression: source/sink were echoed into a body that always read
        // `SELECT * FROM events` → `events_warehouse`.
        let spec = json!({
            "name": "clicks_to_wh",
            "source": {"type": "postgres", "url_env": "SRC", "query": "SELECT * FROM clicks"},
            "sink": {"type": "clickhouse", "url_env": "DST", "table": "clicks_wh"},
            "transform": ["dedupe"],
            "schedule": "*/5 * * * *",
        });
        let y = render_etl(&spec).unwrap();
        assert!(y.contains("name: clicks_to_wh"));
        assert!(y.contains("query: SELECT * FROM clicks"));
        assert!(y.contains("table: clicks_wh"));
        assert!(y.contains("  - dedupe"));
        assert!(y.contains("schedule: \"*/5 * * * *\""));
        assert!(!y.contains("events_warehouse"), "✗ a fixture");
    }

    #[test]
    fn etl_refuses_a_spec_it_would_have_to_invent_the_tables_for() {
        assert!(render_etl(&json!({})).is_err());
        let e = render_etl(&json!({
            "source": {"type": "postgres"},
            "sink": {"type": "clickhouse", "table": "t"},
        }))
        .unwrap_err();
        assert!(e.contains("'source' needs a 'table' or a 'query'"), "{e}");
    }
}
