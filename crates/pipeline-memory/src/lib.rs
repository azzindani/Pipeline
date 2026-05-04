//! Pipeline memory · `SQLite`-backed structured + KV store.
//!
//! Schema in `schema.sql` · CLAUDE.md §"Memory architecture" describes the
//! three logical layers (structured · semantic · working). Day-2 ships the
//! structured layer end-to-end; semantic (sqlite-vec) lands in MVP.

#![allow(clippy::doc_markdown)] // SQL/SQLite/MCP/etc. read better unbacked in domain prose

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
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
        })
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

/// Canonical handover packet · CLAUDE.md §"Handover protocol".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoverPacket {
    pub project: ProjectInfo,
    pub active_session: Option<SessionLock>,
    pub last_run: Option<RunRecord>,
    pub recent_failures: Vec<FailureRecord>,
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
}
