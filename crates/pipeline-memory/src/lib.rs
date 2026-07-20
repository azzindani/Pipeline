//! Pipeline memory · `SQLite`-backed structured + KV store.
//!
//! Schema in `schema.sql` · CLAUDE.md §"Memory architecture" describes the
//! three logical layers (structured · semantic · working). Day-2 ships the
//! structured layer end-to-end; semantic (sqlite-vec) lands in MVP.

#![allow(clippy::doc_markdown)] // SQL/SQLite/MCP/etc. read better unbacked in domain prose

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use uuid::Uuid;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("sqlite: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("session lock held by {0}")]
    LockHeld(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("project not found: {0}")]
    ProjectNotFound(String),
}

/// Owned handle to the project's SQLite memory store.
#[derive(Debug, Clone)]
pub struct Memory {
    pool: SqlitePool,
}

impl Memory {
    /// Open or create a SQLite database at `path` (file is created if missing).
    pub async fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(opts)
            .await?;
        let mem = Self { pool };
        mem.migrate().await?;
        Ok(mem)
    }

    /// In-memory database · used for tests.
    pub async fn open_in_memory() -> Result<Self, MemoryError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        let mem = Self { pool };
        mem.migrate().await?;
        Ok(mem)
    }

    async fn migrate(&self) -> Result<(), MemoryError> {
        for stmt in split_sql(SCHEMA) {
            if stmt.trim().is_empty() {
                continue;
            }
            sqlx::query(&stmt).execute(&self.pool).await?;
        }
        Ok(())
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ---------- projects ----------

    pub async fn upsert_project(
        &self,
        id: &str,
        name: &str,
        stack: &str,
    ) -> Result<(), MemoryError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO projects (id, name, stack, created_at, last_active)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name,
                                            stack = excluded.stack,
                                            last_active = excluded.last_active",
        )
        .bind(id)
        .bind(name)
        .bind(stack)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---------- sessions ----------

    /// Acquire an exclusive session lock for `project_id`.
    /// Fails with `LockHeld` if another session is active.
    pub async fn lock_session(
        &self,
        project_id: &str,
        agent_id: Option<&str>,
        goal: Option<&str>,
    ) -> Result<SessionLock, MemoryError> {
        if let Some(existing) = self.current_lock(project_id).await? {
            return Err(MemoryError::LockHeld(existing.session_id));
        }

        let session_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO sessions (id, project_id, agent_id, started_at, goal)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&session_id)
        .bind(project_id)
        .bind(agent_id)
        .bind(&now)
        .bind(goal)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO session_locks (project_id, session_id, locked_at, agent_id)
             VALUES (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(&session_id)
        .bind(&now)
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(SessionLock {
            session_id,
            project_id: project_id.into(),
            agent_id: agent_id.map(str::to_owned),
            locked_at: now,
        })
    }

    pub async fn current_lock(&self, project_id: &str) -> Result<Option<SessionLock>, MemoryError> {
        let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT project_id, session_id, locked_at, agent_id
             FROM session_locks WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(p, s, t, a)| SessionLock {
            project_id: p,
            session_id: s,
            locked_at: t,
            agent_id: a,
        }))
    }

    /// Release the lock and mark the session ended.
    pub async fn end_session(
        &self,
        session_id: &str,
        outcome: &str,
        summary: Option<&str>,
    ) -> Result<(), MemoryError> {
        let ended_at = Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        let result =
            sqlx::query("UPDATE sessions SET ended_at = ?, outcome = ?, summary = ? WHERE id = ?")
                .bind(&ended_at)
                .bind(outcome)
                .bind(summary)
                .bind(session_id)
                .execute(&mut *tx)
                .await?;
        if result.rows_affected() == 0 {
            return Err(MemoryError::SessionNotFound(session_id.into()));
        }
        sqlx::query("DELETE FROM session_locks WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Force-release a lock without ending its session.
    /// Use only for `pipeline_session.steal`.
    pub async fn force_unlock(&self, project_id: &str) -> Result<(), MemoryError> {
        sqlx::query("DELETE FROM session_locks WHERE project_id = ?")
            .bind(project_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Build the canonical handover packet for `project_id`.
    pub async fn handover(&self, project_id: &str) -> Result<HandoverPacket, MemoryError> {
        let project: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT id, name, stack FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some((id, name, stack)) = project else {
            return Err(MemoryError::ProjectNotFound(project_id.into()));
        };

        let last_run: Option<RunRecord> = sqlx::query_as::<_, RunRecord>(
            "SELECT id, project_id, session_id, profile, stage, status, duration_ms,
                    created_at, stdout, stderr, failure_json
             FROM pipeline_runs WHERE project_id = ?
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;

        let recent_failures: Vec<FailureRecord> = sqlx::query_as::<_, FailureRecord>(
            "SELECT f.id, f.run_id, f.stage, f.error_message, f.file, f.line,
                    f.fix_applied, f.fix_worked, f.created_at
             FROM failures f
             JOIN pipeline_runs r ON r.id = f.run_id
             WHERE r.project_id = ?
             ORDER BY f.created_at DESC LIMIT 5",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        let active_lock = self.current_lock(project_id).await?;

        Ok(HandoverPacket {
            project: ProjectInfo {
                id,
                name,
                stack: stack.unwrap_or_default(),
            },
            active_session: active_lock,
            last_run,
            recent_failures,
            active_work: self.active_work(project_id).await?,
        })
    }

    /// Rebuild the planning context from what `pipeline_plan.*` stored.
    ///
    /// Reads rather than requires: a project with no plan yields a default
    /// `ActiveWork`, ✗ an error — handover must answer on a bare project too.
    async fn active_work(&self, project_id: &str) -> Result<ActiveWork, MemoryError> {
        let mut work = ActiveWork::default();

        if let Some(raw) = self.recall(project_id, "plan", "prd").await? {
            if let Ok(prd) = serde_json::from_str::<serde_json::Value>(&raw) {
                work.goal = prd
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
                work.goals = string_list(prd.get("goals"));
                work.non_goals = string_list(prd.get("non_goals"));
            }
        }

        // ! Plan order, ✗ recency order. list_scope is created_at DESC, which would
        // hand the agent the LAST milestone as the next thing to build.
        for (id, raw) in in_plan_order(self.list_scope(project_id, "feature").await?) {
            let Ok(f) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            let status = f
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("todo")
                .to_owned();
            work.features_total += 1;
            *work.features_by_status.entry(status.clone()).or_insert(0) += 1;
            // Only unfinished work belongs in "what's next" · a done feature is history.
            if status != "done" {
                work.next_features.push(FeatureBrief {
                    id,
                    name: f
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    description: f
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_owned(),
                    status,
                    acceptance_criteria: string_list(f.get("ac")),
                });
            }
        }

        for (name, raw) in in_plan_order(self.list_scope(project_id, "milestone").await?) {
            let m = serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default();
            work.milestones.push(MilestoneBrief {
                name: m
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&name)
                    .to_owned(),
                status: m
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("planned")
                    .to_owned(),
                exit_criteria: string_list(m.get("exit_criteria")),
            });
        }

        work.open_risks = self.list_scope(project_id, "risk").await?.len();
        Ok(work)
    }

    // ---------- runs ----------

    pub async fn log_run(&self, run: &NewRun<'_>) -> Result<String, MemoryError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO pipeline_runs
             (id, project_id, session_id, profile, stage, status, duration_ms,
              triggered_by, commit_sha, created_at, stdout, stderr, failure_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(run.project_id)
        .bind(run.session_id)
        .bind(run.profile)
        .bind(run.stage)
        .bind(run.status)
        .bind(i64::try_from(run.duration_ms).unwrap_or(i64::MAX))
        .bind(run.triggered_by)
        .bind(run.commit_sha)
        .bind(&now)
        .bind(run.stdout)
        .bind(run.stderr)
        .bind(run.failure_json)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn log_failure(&self, failure: &NewFailure<'_>) -> Result<String, MemoryError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO failures (id, run_id, stage, error_message, file, line, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(failure.run_id)
        .bind(failure.stage)
        .bind(failure.error_message)
        .bind(failure.file)
        .bind(failure.line)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn run_history(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<RunRecord>, MemoryError> {
        let rows: Vec<RunRecord> = sqlx::query_as::<_, RunRecord>(
            "SELECT id, project_id, session_id, profile, stage, status, duration_ms,
                    created_at, stdout, stderr, failure_json
             FROM pipeline_runs WHERE project_id = ?
             ORDER BY created_at DESC LIMIT ?",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    // ---------- generic memory KV ----------

    pub async fn remember(
        &self,
        project_id: &str,
        scope: &str,
        key: &str,
        value: &str,
    ) -> Result<(), MemoryError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO memory_kv (id, project_id, scope, key, value, created_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(project_id, scope, key) DO UPDATE SET
               value = excluded.value, created_at = excluded.created_at",
        )
        .bind(&id)
        .bind(project_id)
        .bind(scope)
        .bind(key)
        .bind(value)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recall(
        &self,
        project_id: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<String>, MemoryError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT value FROM memory_kv WHERE project_id = ? AND scope = ? AND key = ?",
        )
        .bind(project_id)
        .bind(scope)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(v,)| v))
    }

    /// Enumerate every `(key, value)` pair for `(project_id, scope)`,
    /// ordered by `created_at DESC`.
    pub async fn list_scope(
        &self,
        project_id: &str,
        scope: &str,
    ) -> Result<Vec<(String, String)>, MemoryError> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT key, value FROM memory_kv
             WHERE project_id = ? AND scope = ?
             ORDER BY created_at DESC",
        )
        .bind(project_id)
        .bind(scope)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Enumerate every scope that actually holds a row for `project_id`,
    /// alphabetically.
    ///
    /// ! Callers must never hardcode a scope list: `remember` writes to
    /// `"default"` when none is given, and any caller may invent a scope. A
    /// fixed list silently omits whatever it did not predict.
    pub async fn scopes(&self, project_id: &str) -> Result<Vec<String>, MemoryError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT scope FROM memory_kv WHERE project_id = ? ORDER BY scope",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    /// Delete a single `(scope, key)` entry. Returns `true` if a row was removed.
    pub async fn forget(
        &self,
        project_id: &str,
        scope: &str,
        key: &str,
    ) -> Result<bool, MemoryError> {
        let result =
            sqlx::query("DELETE FROM memory_kv WHERE project_id = ? AND scope = ? AND key = ?")
                .bind(project_id)
                .bind(scope)
                .bind(key)
                .execute(&self.pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Find failures whose `error_message` contains any of `keywords`,
    /// scoped to `project_id`. Cheap substring match; sqlite-vec lands at MVP.
    pub async fn find_similar_failures(
        &self,
        project_id: &str,
        error_message: &str,
        limit: i64,
    ) -> Result<Vec<FailureRecord>, MemoryError> {
        let keywords = top_keywords(error_message, 4);
        if keywords.is_empty() {
            return Ok(Vec::new());
        }
        // Build a dynamic query: one LIKE clause per keyword joined with OR.
        let mut sql = String::from(
            "SELECT f.id, f.run_id, f.stage, f.error_message, f.file, f.line,
                    f.fix_applied, f.fix_worked, f.created_at
             FROM failures f
             JOIN pipeline_runs r ON r.id = f.run_id
             WHERE r.project_id = ? AND (",
        );
        let conditions: Vec<&str> = keywords.iter().map(|_| "f.error_message LIKE ?").collect();
        sql.push_str(&conditions.join(" OR "));
        sql.push_str(") ORDER BY f.created_at DESC LIMIT ?");

        let mut q = sqlx::query_as::<_, FailureRecord>(&sql).bind(project_id);
        for kw in &keywords {
            q = q.bind(format!("%{kw}%"));
        }
        q = q.bind(limit);
        Ok(q.fetch_all(&self.pool).await?)
    }

    /// Group failures by stage and return `(stage, count)` ordered by count desc.
    pub async fn failure_patterns(
        &self,
        project_id: &str,
    ) -> Result<Vec<(String, i64)>, MemoryError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT f.stage, COUNT(*) as n
             FROM failures f
             JOIN pipeline_runs r ON r.id = f.run_id
             WHERE r.project_id = ?
             GROUP BY f.stage
             ORDER BY n DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

/// Extract up to `n` distinctive keywords from an error message · strips
/// short stop words and common Rust/CI noise.
fn top_keywords(text: &str, n: usize) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "but", "with", "from", "into", "this", "that", "have", "has", "had",
        "was", "were", "are", "is", "be", "been", "of", "on", "in", "at", "to", "or", "an", "a",
        "as", "by", "it", "if", "not", "no", "yes", "do", "does", "did", "so", "use", "used",
        "uses", "see", "via", "error", "failed", "fail", "exit", "code",
    ];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
        if raw.len() < 4 {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if STOP.contains(&lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            out.push(lower);
            if out.len() >= n {
                break;
            }
        }
    }
    out
}

/// Naive SQL splitter · strips line comments · splits on `;`.
/// Works for our schema (no string literals containing `;`).
fn split_sql(text: &str) -> Vec<String> {
    text.split(';')
        .map(|stmt| {
            stmt.lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------- types ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLock {
    pub project_id: String,
    pub session_id: String,
    pub locked_at: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct NewRun<'a> {
    pub project_id: &'a str,
    pub session_id: Option<&'a str>,
    pub profile: &'a str,
    pub stage: &'a str,
    pub status: &'a str,
    pub duration_ms: u128,
    pub triggered_by: Option<&'a str>,
    pub commit_sha: Option<&'a str>,
    pub stdout: Option<&'a str>,
    pub stderr: Option<&'a str>,
    pub failure_json: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct NewFailure<'a> {
    pub run_id: &'a str,
    pub stage: &'a str,
    pub error_message: &'a str,
    pub file: Option<&'a str>,
    pub line: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RunRecord {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub profile: String,
    pub stage: String,
    pub status: String,
    pub duration_ms: i64,
    pub created_at: String,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub failure_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct FailureRecord {
    pub id: String,
    pub run_id: String,
    pub stage: String,
    pub error_message: String,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub fix_applied: Option<String>,
    pub fix_worked: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub stack: String,
}

/// What the project is *trying to do* · reconstructed from the stored plan.
///
/// ! Without this the packet answers "what broke" but not "what are we building",
/// so an agent that reconnects knows the last failure and nothing about the goal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveWork {
    /// PRD summary · the one-paragraph statement of intent.
    pub goal: Option<String>,
    pub goals: Vec<String>,
    pub non_goals: Vec<String>,
    pub features_total: usize,
    /// status → count, e.g. `{"todo": 11}`.
    pub features_by_status: BTreeMap<String, usize>,
    /// The work actually up next · todo features with their acceptance criteria.
    pub next_features: Vec<FeatureBrief>,
    pub milestones: Vec<MilestoneBrief>,
    pub open_risks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureBrief {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestoneBrief {
    pub name: String,
    pub status: String,
    pub exit_criteria: Vec<String>,
}

/// Canonical handover packet · CLAUDE.md §"Handover protocol".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoverPacket {
    pub project: ProjectInfo,
    pub active_session: Option<SessionLock>,
    pub last_run: Option<RunRecord>,
    pub recent_failures: Vec<FailureRecord>,
    pub active_work: ActiveWork,
}

/// Re-sort scope rows oldest-first on the payload's own `created_at`.
///
/// Plan artifacts are authored in dependency order — M1 before M5, schema before
/// delivery — so handover must replay them in that order. Stable sort keeps rows
/// that share a timestamp (a scripted planning pass writes many in the same
/// second) in their original insertion order.
fn in_plan_order(mut rows: Vec<(String, String)>) -> Vec<(String, String)> {
    rows.reverse(); // list_scope is created_at DESC → oldest-first
    rows.sort_by_key(|(_, raw)| {
        serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| {
                v.get("created_at")
                    .and_then(|c| c.as_str())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default()
    });
    rows
}

/// JSON array → `Vec<String>`, dropping non-strings. Absent field → empty.
fn string_list(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> Memory {
        let m = Memory::open_in_memory().await.expect("open");
        m.upsert_project("p1", "pipeline", "rust")
            .await
            .expect("upsert");
        m
    }

    #[tokio::test]
    async fn lock_and_unlock_session() {
        let m = fresh().await;
        let lock = m
            .lock_session("p1", Some("agent-A"), Some("test"))
            .await
            .unwrap();
        assert_eq!(lock.project_id, "p1");

        let conflict = m.lock_session("p1", Some("agent-B"), None).await;
        assert!(matches!(conflict, Err(MemoryError::LockHeld(_))));

        m.end_session(&lock.session_id, "ok", Some("done"))
            .await
            .unwrap();
        let new_lock = m.lock_session("p1", Some("agent-C"), None).await.unwrap();
        assert_ne!(new_lock.session_id, lock.session_id);
    }

    #[tokio::test]
    async fn log_run_and_history() {
        let m = fresh().await;
        let lock = m.lock_session("p1", None, None).await.unwrap();
        let run_id = m
            .log_run(&NewRun {
                project_id: "p1",
                session_id: Some(&lock.session_id),
                profile: "fast",
                stage: "static",
                status: "pass",
                duration_ms: 1234,
                triggered_by: Some("test"),
                commit_sha: None,
                stdout: Some("ok"),
                stderr: None,
                failure_json: None,
            })
            .await
            .unwrap();
        assert!(!run_id.is_empty());

        let history = m.run_history("p1", 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].stage, "static");
        assert_eq!(history[0].status, "pass");
    }

    #[tokio::test]
    async fn handover_carries_the_plan_not_just_the_failures() {
        // Regression: the packet used to answer "what broke" and never "what are we
        // building", so a reconnecting agent got a project name and nothing else.
        let m = fresh().await;
        m.remember(
            "p1",
            "plan",
            "prd",
            r#"{"summary":"deliver Vera","goals":["cited results"],"non_goals":["never call an LLM"]}"#,
        )
        .await
        .unwrap();
        m.remember(
            "p1",
            "feature",
            "f1",
            r#"{"name":"pg-schema","description":"halfvec","status":"todo","ac":["migrates clean"]}"#,
        )
        .await
        .unwrap();
        m.remember(
            "p1",
            "feature",
            "f2",
            r#"{"name":"shipped","description":"done thing","status":"done","ac":[]}"#,
        )
        .await
        .unwrap();
        m.remember(
            "p1",
            "milestone",
            "M1",
            r#"{"name":"M1 · foundation","status":"planned","exit_criteria":["schema migrates"]}"#,
        )
        .await
        .unwrap();

        let w = m.handover("p1").await.unwrap().active_work;
        assert_eq!(w.goal.as_deref(), Some("deliver Vera"));
        assert_eq!(w.goals, vec!["cited results".to_owned()]);
        assert_eq!(w.non_goals, vec!["never call an LLM".to_owned()]);
        assert_eq!(w.features_total, 2);
        assert_eq!(w.features_by_status.get("todo"), Some(&1));
        assert_eq!(w.features_by_status.get("done"), Some(&1));
        // Done work is history · only unfinished features are "what's next".
        assert_eq!(w.next_features.len(), 1);
        assert_eq!(w.next_features[0].name, "pg-schema");
        assert_eq!(
            w.next_features[0].acceptance_criteria,
            vec!["migrates clean".to_owned()]
        );
        assert_eq!(w.milestones.len(), 1);
        assert_eq!(w.milestones[0].exit_criteria.len(), 1);
    }

    #[tokio::test]
    async fn handover_replays_the_plan_in_authoring_order() {
        // Regression: list_scope is created_at DESC, so handover handed back the LAST
        // milestone as the next thing to build — M5 before M1, delivery before schema.
        let m = fresh().await;
        for (i, name) in ["M1", "M2", "M3"].iter().enumerate() {
            m.remember(
                "p1",
                "milestone",
                name,
                &format!(
                    r#"{{"name":"{name}","status":"planned","created_at":"2026-07-20T00:0{i}:00Z"}}"#
                ),
            )
            .await
            .unwrap();
            m.remember(
                "p1",
                "feature",
                &format!("f{i}"),
                &format!(
                    r#"{{"name":"feat-{name}","status":"todo","created_at":"2026-07-20T00:0{i}:00Z"}}"#
                ),
            )
            .await
            .unwrap();
        }
        let w = m.handover("p1").await.unwrap().active_work;
        let ms: Vec<&str> = w.milestones.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            ms,
            vec!["M1", "M2", "M3"],
            "milestones must replay M1 first"
        );
        let fs: Vec<&str> = w.next_features.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(fs, vec!["feat-M1", "feat-M2", "feat-M3"]);
    }

    #[tokio::test]
    async fn handover_on_a_project_with_no_plan_is_empty_not_an_error() {
        let m = fresh().await;
        let w = m.handover("p1").await.unwrap().active_work;
        assert!(w.goal.is_none());
        assert_eq!(w.features_total, 0);
        assert!(w.next_features.is_empty());
    }

    #[tokio::test]
    async fn handover_returns_last_run() {
        let m = fresh().await;
        let lock = m.lock_session("p1", Some("a"), Some("g")).await.unwrap();
        m.log_run(&NewRun {
            project_id: "p1",
            session_id: Some(&lock.session_id),
            profile: "fast",
            stage: "unit",
            status: "fail",
            duration_ms: 999,
            triggered_by: None,
            commit_sha: None,
            stdout: None,
            stderr: Some("test_x failed"),
            failure_json: None,
        })
        .await
        .unwrap();
        let pack = m.handover("p1").await.unwrap();
        assert_eq!(pack.project.name, "pipeline");
        assert!(pack.active_session.is_some());
        assert_eq!(pack.last_run.as_ref().unwrap().stage, "unit");
    }

    #[tokio::test]
    async fn remember_and_recall() {
        let m = fresh().await;
        m.remember("p1", "config", "JWT_SECRET_REF", "vault://secret/jwt")
            .await
            .unwrap();
        let v = m.recall("p1", "config", "JWT_SECRET_REF").await.unwrap();
        assert_eq!(v.as_deref(), Some("vault://secret/jwt"));
        m.remember("p1", "config", "JWT_SECRET_REF", "vault://v2")
            .await
            .unwrap();
        let v2 = m.recall("p1", "config", "JWT_SECRET_REF").await.unwrap();
        assert_eq!(v2.as_deref(), Some("vault://v2"));
    }

    #[tokio::test]
    async fn similar_failures_substring_matches() {
        let m = fresh().await;
        let lock = m.lock_session("p1", None, None).await.unwrap();
        let run_id = m
            .log_run(&NewRun {
                project_id: "p1",
                session_id: Some(&lock.session_id),
                profile: "fast",
                stage: "unit",
                status: "fail",
                duration_ms: 1,
                triggered_by: None,
                commit_sha: None,
                stdout: None,
                stderr: None,
                failure_json: None,
            })
            .await
            .unwrap();
        m.log_failure(&NewFailure {
            run_id: &run_id,
            stage: "unit",
            error_message: "JWT_SECRET environment variable is not set",
            file: None,
            line: None,
        })
        .await
        .unwrap();
        m.log_failure(&NewFailure {
            run_id: &run_id,
            stage: "static",
            error_message: "clippy lint borrow_deref_ref triggered",
            file: None,
            line: None,
        })
        .await
        .unwrap();

        let hits = m
            .find_similar_failures("p1", "missing JWT_SECRET token", 5)
            .await
            .unwrap();
        assert!(hits.iter().any(|f| f.error_message.contains("JWT_SECRET")));
        let other = m
            .find_similar_failures("p1", "no such thing zzz", 5)
            .await
            .unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn failure_patterns_groups_by_stage() {
        let m = fresh().await;
        let lock = m.lock_session("p1", None, None).await.unwrap();
        let run_id = m
            .log_run(&NewRun {
                project_id: "p1",
                session_id: Some(&lock.session_id),
                profile: "fast",
                stage: "unit",
                status: "fail",
                duration_ms: 1,
                triggered_by: None,
                commit_sha: None,
                stdout: None,
                stderr: None,
                failure_json: None,
            })
            .await
            .unwrap();
        for s in ["unit", "unit", "static"] {
            m.log_failure(&NewFailure {
                run_id: &run_id,
                stage: s,
                error_message: "x",
                file: None,
                line: None,
            })
            .await
            .unwrap();
        }
        let patterns = m.failure_patterns("p1").await.unwrap();
        assert_eq!(patterns[0], ("unit".to_owned(), 2));
        assert_eq!(patterns[1], ("static".to_owned(), 1));
    }

    #[tokio::test]
    async fn scopes_enumerates_whatever_was_written_including_default() {
        // Callers used to hardcode a scope list that omitted "default" — the scope
        // `remember` uses when none is given — so those memories were invisible.
        let m = fresh().await;
        m.remember("p1", "default", "k", "v").await.unwrap();
        m.remember("p1", "feature", "f1", "{}").await.unwrap();
        m.remember("p1", "invented", "x", "y").await.unwrap();
        m.remember("other", "not_mine", "x", "y").await.unwrap();
        assert_eq!(
            m.scopes("p1").await.unwrap(),
            vec![
                "default".to_owned(),
                "feature".to_owned(),
                "invented".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn list_scope_and_forget() {
        let m = fresh().await;
        m.remember("p1", "feature", "f1", r#"{"name":"login"}"#)
            .await
            .unwrap();
        m.remember("p1", "feature", "f2", r#"{"name":"signup"}"#)
            .await
            .unwrap();
        m.remember("p1", "milestone", "POC", r#"{"status":"in_progress"}"#)
            .await
            .unwrap();

        let features = m.list_scope("p1", "feature").await.unwrap();
        assert_eq!(features.len(), 2);
        let keys: Vec<&str> = features.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"f1"));
        assert!(keys.contains(&"f2"));

        let milestones = m.list_scope("p1", "milestone").await.unwrap();
        assert_eq!(milestones.len(), 1);

        assert!(m.forget("p1", "feature", "f1").await.unwrap());
        assert!(!m.forget("p1", "feature", "f1").await.unwrap()); // already gone
        assert_eq!(m.list_scope("p1", "feature").await.unwrap().len(), 1);
    }
}
