# ADR 0001 — SQLite for project memory

**Status** Accepted · **Date** 2026-09-14
**Standard** [`database/ENGINES.md`](https://github.com/azzindani/Standards/blob/main/database/ENGINES.md) §2 production default · §5 embedded and local-first

## Context

`database/engines` §2 makes PostgreSQL the production default for every project and requires an ADR naming the measured reason for anything else. Pipeline stores project memory — runs, stages, failures, sessions, tasks, memory keys — in `.pipeline/memory.db`, a SQLite file inside the project it describes.

## Decision

SQLite, under the §5 embedded exception. Every §5 condition holds:

| Condition | How Pipeline meets it |
|---|---|
| Single writer | One `pipeline dev` process per project owns the file · MCP server and watcher are two Tokio tasks sharing one pool, ✗ two processes |
| Data is local to its owner | The database describes exactly one project and lives inside it · ✗ shared across hosts |
| Network access ✗ required | Local-first is the product thesis · a required database server would put a service between an agent and its own project |
| Dataset fits local storage | Run history for one project, retention-bounded |

## Consequences

- Pipeline ships as a single binary with no database to install. An agent connecting to a fresh project gets memory with zero setup.
- Required pragmas at every connection: `foreign_keys = ON` · `journal_mode = WAL` · `busy_timeout` → `sql/STANDARDS.md`.
- Memory is per project, ✗ central. Shared team memory means committing `.pipeline/` — a user decision, ✗ a server.
- The §5 exit condition is concurrent remote writers. Team-shared memory with several machines writing one store migrates to the production default under §10 — ✗ grows a network layer in front of SQLite.

## What this ADR does not cover

Projects Pipeline **manages** are unaffected: a scaffolded or adopted project follows §2 and gets PostgreSQL. This ADR is about Pipeline's own memory file.
