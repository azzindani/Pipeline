# CLAUDE.md — Pipeline Project

> This file is the primary context document for any coding agent working on Pipeline.
> Read this before touching any code. Update it when decisions change.

---

## What is Pipeline

Pipeline is a local-first, end-to-end software development and deployment acceleration tool built in Rust. It is designed to support AI-assisted software development by providing a structured, MCP-native infrastructure layer that any coding agent — cloud or local — can use to deliver production-grade software without needing to know Docker, GitHub, or deployment specifics directly.

### Core philosophy

> **Local CI is the source of truth. GitHub Actions is confirmation. MCP makes any agent a first-class citizen.**

Pipeline is not just a CI/CD tool. It is the infrastructure layer that sits between agent intent and real-world execution — from project initialization through development, testing, deployment, and long-term maintenance.

### The problem Pipeline solves

AI coding agents generate code fast. The bottleneck is now validation. Current workflows look like:

```
AI generates code → developer eyeballs it → pushes → CI catches problems → fix → repeat
```

This feedback loop is too slow and too manual. Pipeline changes it to:

```
Agent generates code → Pipeline validates locally → agent fixes → green → push → confirm → deploy
```

Everything meaningful happens before the push. GitHub Actions is a trust signal, not the primary gate.

### Why this matters for local models

Local models (Ollama, llama.cpp, LM Studio) are weaker at long-context reasoning, multi-step planning, and knowing infrastructure specifics. Pipeline removes that burden entirely. A local model does not need to know Docker — it calls `pipeline_run`. It does not need to know GitHub API — it calls `pipeline_push`. Pipeline makes local models viable for real software delivery by hiding infrastructure complexity behind clean MCP tools.

---

## Project language and runtime

- **Language:** Rust
- **Reason:** Single binary, zero runtime dependency, cross-platform, fast process orchestration, safe long-running daemon, excellent async ecosystem via Tokio
- **Key crates:** `tokio`, `clap`, `bollard`, `notify`, `octocrab`, `rmcp`, `serde`, `sqlx`, `ratatui`, `indicatif`, `wasmtime` (v2)
- **Plugin interface:** MCP (Model Context Protocol) — primary extension point
- **Binary modes:** `init`, `run`, `watch`, `deploy`, `monitor`, `mcp`, `dev`, `report`

---

---

## Full lifecycle

Pipeline handles the entire software lifecycle. Each phase is a distinct responsibility.

### Phase 1 — Init

Agent calls `pipeline_init`. Pipeline handles:

- Project structure scaffolding
- Dockerfile (multi-stage, best practice)
- docker-compose.yml
- pipeline.yaml (project-specific config)
- GitHub Actions thin YAML
- Git init + initial commit
- GitHub repo creation via API
- Branch protection rules (require Pipeline green before merge)

### Phase 2 — Development loop

```
Agent generates code
    ↓
pipeline_run(stage="fast")     ← static + unit only, seconds
    ↓
Pipeline returns structured result + memory context
    ↓
Agent reads → fixes → reruns
    ↓
pipeline_run(stage="full")     ← all stages, Docker
    ↓
Agent commits if green
```

The fast stage runs in seconds. Full stage runs in Docker clean environment. Agent stays in a tight loop without ever touching Docker directly.

### Phase 3 — Pre-push (preflight)

```
pipeline_preflight()
  - Clean Docker environment (no cache, from scratch)
  - Full stage run
  - Image build + vulnerability scan
  - Integration tests
  - Coverage gate check
  - Security gate check
  - All green → push allowed
```

### Phase 4 — GitHub Actions (thin confirmation)

GitHub Actions reruns Stage 0 (static) and Stage 1 (unit) only. Purpose:

- Confirms local/CI environment parity
- Posts status checks to PR
- Triggers deployment pipeline if on main branch
- Does NOT rerun Docker-heavy stages — those already passed locally

### Phase 5 — Deployment

```
pipeline_deploy(env="production")
  - Push image to GHCR (free)
  - Run smoke tests against staging
  - Deploy to target (SSH + compose, or k8s)
  - Health check confirmation
  - Automatic rollback if health check fails
```

### Phase 6 — Maintenance

```
pipeline_monitor()   ← runs as daemon
  - Watches deployment health continuously
  - Scheduled dependency updates (cron)
  - Runs full pipeline on update
  - Auto-creates PR if green
  - Notifies agent if red
  - Agent invoked to fix if configured
```

---

## Architecture overview

```
┌─────────────────────────────────────────────────────┐
│                  LOCAL (primary)                     │
│                                                      │
│  Coding Agent (any — Claude Code, Ollama, custom)   │
│       ↓ MCP protocol                                │
│  Pipeline MCP Server (Rust)                         │
│       ↓ orchestrates                                │
│  Pipeline Watch Daemon (filesystem, notify crate)   │
│       ↓ both read/write                             │
│  SQLite Memory (.pipeline/memory.db)                │
│       ↓ Pipeline executes                           │
│  Docker (ephemeral clean environments)              │
│       ↓ all green                                   │
│  Git commit + push to GitHub                        │
│                                                     │
└─────────────────────────────────────────────────────┘
           ↓ push triggers
┌─────────────────────────────────────────────────────┐
│              GITHUB ACTIONS (confirmation)           │
│  Stage 0 + Stage 1 only (fast, cheap)              │
│  Deploy trigger on main branch                      │
└─────────────────────────────────────────────────────┘
           ↓ green
┌─────────────────────────────────────────────────────┐
│                 DEPLOYMENT TARGET                    │
│  GHCR image registry                               │
│  SSH + compose / Kubernetes                         │
│  Health checks + rollback                           │
└─────────────────────────────────────────────────────┘
```

### Dev mode (primary command during development)

```bash
pipeline dev    # starts MCP server + filesystem watcher as single process
```

Two async Tokio tasks, one process, one SQLite database.

---

## Pipeline stages

### Stage 0 — Static (instant, no execution)

- Linting + formatting
- Type checking
- Secret scanning (trufflehog)
- Dependency audit (pip audit / npm audit)
- Dockerfile linting (hadolint)

### Stage 1 — Unit (fast, no containers)

- Unit tests with coverage threshold enforcement
- Mutation testing sample on critical paths
- Property-based test suite

### Stage 2 — Container (medium, Docker required)

- Multi-stage Docker image build (cached)
- Image vulnerability scan (Trivy)
- Image size gate (fail if above threshold)
- Container structure test

### Stage 3 — Integration (slower, compose up)

- Services start via docker-compose (fresh network)
- Health checks pass
- Integration tests run inside network
- API contract tests (schemathesis)
- Compose down + volume cleanup

### Stage 4 — Quality gate (decision point)

- Coverage report
- Mutation score
- Performance baseline comparison
- Structured PASS / FAIL with explainable report → JSON → design tool

### Stage profiles

| Profile | Stages | When |
|---|---|---|
| `fast` | 0 + 1 | Agent inner loop, every change |
| `full` | 0 + 1 + 2 + 3 | Pre-commit |
| `preflight` | 0 + 1 + 2 + 3 + security | Pre-push, clean environment |
| `confirm` | 0 + 1 | GitHub Actions only |

---

## pipeline.yaml schema

```yaml
project: my-app
version: 1.0.0
stack:
  runtime: python-uv      # python-uv | bun | node | rust | go
  services:
    - postgres:16
    - redis:7

stages:
  fast:
    - static
    - unit
  full:
    - static
    - unit
    - container
    - integration
  preflight:
    - static
    - unit
    - container
    - integration
    - security

gates:
  coverage: 80            # fail if below
  image_size_mb: 500      # fail if above
  critical_vulns: 0       # fail if any critical CVE

deploy:
  registry: ghcr.io/username
  targets:
    staging:
      type: compose
      host: ssh://staging-server
    production:
      type: compose
      host: ssh://prod-server
      requires: manual_approval

maintenance:
  schedule: "0 9 * * 1"  # every Monday 9am
  auto_merge: true        # if pipeline green
  notify_on_fail: true
```

---

## MCP tool surface

MCP is the primary interface. Any agent that speaks MCP can use Pipeline with zero configuration.

### Init and scaffold

```
pipeline_init(name, type, stack, github_repo?)
pipeline_scaffold(component)
```

### Development

```
pipeline_run(stage?, watch?)
pipeline_status()
pipeline_logs(stage, tail?)
pipeline_fix_suggestion(stage)
```

### Commit and push

```
pipeline_preflight()
pipeline_commit(message)
pipeline_push()
```

### Deployment

```
pipeline_deploy(env)
pipeline_rollback(env)
pipeline_smoke_test(env)
```

### Maintenance

```
pipeline_update_deps()
pipeline_health(env)
pipeline_diff(env)
```

### Session and context

```
pipeline_session_start()        → returns full handover packet
pipeline_session_checkpoint(note?)
pipeline_session_end(outcome, summary?)
pipeline_context()
pipeline_file_context(path)
pipeline_task_context(description)
```

### Memory

```
pipeline_remember(key, value, scope)
pipeline_recall(query)
pipeline_history(entity, limit?)
pipeline_known_issues()
pipeline_suggest_fix(error)
pipeline_pattern_report()
```

### Filesystem

```
pipeline_watch_start()
pipeline_changed_files(since?)
pipeline_scaffold(component, type)
```

### Meta

```
pipeline_config_get()
pipeline_config_set(key, value)
pipeline_explain(stage)
```

---

## Memory architecture

### Three logical layers, one SQLite file

Pipeline is stateless on the surface (any agent connects fresh) but stateful underneath (everything persists in SQLite).

```
STRUCTURED MEMORY    (sqlx + SQLite)
  pipeline runs, stages, results, deployments
  project config, state, history, sessions
  Query: "last 10 failures on unit stage"

SEMANTIC MEMORY      (SQLite + sqlite-vec)
  error messages + their fixes
  failure patterns + solutions
  agent reasoning + outcomes
  Query: "similar error to this one before?"

WORKING MEMORY       (in-memory, session scoped)
  current task context
  active stage outputs
  agent's current plan
  flushed to SQLite on session end
```

### Memory file location

```
my-project/
├── src/
├── Dockerfile
├── pipeline.yaml
└── .pipeline/
    ├── memory.db        ← SQLite (structured + vector)
    ├── sessions/        ← session logs
    └── reports/         ← rendered reports
```

Memory is part of the project, not a central server. Add `.pipeline/` to `.gitignore` or commit it for shared team memory.

### SQLite schema (core tables)

```sql
CREATE TABLE project (
  id TEXT PRIMARY KEY,
  name TEXT, stack TEXT,
  created_at DATETIME, last_active DATETIME, config JSON
);

CREATE TABLE pipeline_runs (
  id TEXT PRIMARY KEY, project_id TEXT,
  stage TEXT, status TEXT,
  duration_ms INTEGER, triggered_by TEXT,
  commit_sha TEXT, created_at DATETIME, output JSON
);

CREATE TABLE failures (
  id TEXT PRIMARY KEY, run_id TEXT, stage TEXT,
  error_message TEXT, error_embedding BLOB,
  fix_applied TEXT, fix_worked BOOLEAN, created_at DATETIME
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY, project_id TEXT, agent_id TEXT,
  started_at DATETIME, ended_at DATETIME,
  goal TEXT, decisions JSON, files_touched JSON, outcome TEXT
);

CREATE TABLE deployments (
  id TEXT PRIMARY KEY, project_id TEXT, env TEXT,
  image_digest TEXT, commit_sha TEXT,
  status TEXT, deployed_at DATETIME, health_checks JSON
);

CREATE TABLE memory (
  id TEXT PRIMARY KEY, project_id TEXT,
  scope TEXT, key TEXT, value TEXT,
  embedding BLOB, created_at DATETIME, expires_at DATETIME
);
```

### The learning loop

```
Run 1:  test_auth fails → agent fixes → Pipeline stores (error, fix, outcome)
Run 2:  similar error → Pipeline surfaces previous fix → agent applies faster
Run 10: Pipeline detects pattern → "auth tests fail when JWT_SECRET missing"
Run 11: Pipeline pre-checks JWT_SECRET before running auth tests
```

Project-specific institutional knowledge accumulates automatically.

---

## Handover protocol

Pipeline owns all persistent context. Agents are stateless consumers of that context.

### Handover packet (generated by Pipeline at every session start)

```json
{
  "project": {
    "name": "my-app",
    "stack": "python-uv",
    "current_branch": "feat/auth",
    "last_good_commit": "abc123"
  },
  "pipeline_state": {
    "last_run": "2 hours ago",
    "fast_stages": "passing",
    "full_stages": "failing",
    "failing_stage": "integration",
    "failing_test": "test_auth.py::test_login",
    "consecutive_failures": 3
  },
  "active_work": {
    "goal": "fix JWT authentication flow",
    "files_in_progress": ["src/auth.py", "tests/test_auth.py"],
    "last_action": "modified JWT validation logic",
    "blockers": ["JWT_SECRET not set in test environment"]
  },
  "relevant_memory": [
    "this test failed before — fixed by setting JWT_SECRET in .env.test",
    "auth service depends on redis being up before tests run"
  ],
  "suggested_next": "check .env.test for JWT_SECRET, then rerun integration stage"
}
```

Pipeline builds this entirely from its own structured data. No agent input required. Any agent connects and immediately knows where things stand.

### Filesystem ownership boundary

```
AGENT OWNS:
  src/, tests/, and all application source code
  Reading, writing, refactoring code

PIPELINE OWNS:
  .pipeline/ directory entirely
  Dockerfile, docker-compose.yml, .github/workflows/
  File change tracking (via notify crate)
  File → test → failure relationship graph

SHARED (Pipeline tracks, agent acts):
  Files changed since last green run
  Files related to failing tests
  Files not touched but referenced by failing tests
```

---

## Rust project structure

```
pipeline/
├── crates/
│   ├── pipeline-core/       # stage engine, Docker orchestration
│   ├── pipeline-cli/        # clap CLI, entry point
│   ├── pipeline-mcp/        # MCP server (rmcp)
│   ├── pipeline-lsp/        # LSP server (v2)
│   ├── pipeline-github/     # GitHub API, Actions, status checks
│   ├── pipeline-docker/     # Docker API client (bollard)
│   ├── pipeline-stages/     # built-in stages
│   ├── pipeline-config/     # pipeline.yaml schema, serde
│   ├── pipeline-memory/     # SQLite + sqlite-vec, session, handover
│   └── pipeline-report/     # structured output, JSON
├── plugins/                 # WASM plugins (v2)
├── pipeline.yaml            # Pipeline dogfoods itself
├── CLAUDE.md                # this file
└── .github/workflows/
    └── confirm.yml          # thin confirmation only
```

### Key crate responsibilities

| Crate | Responsibility |
|---|---|
| `pipeline-core` | Stage runner, orchestration, result types |
| `pipeline-cli` | All CLI commands, arg parsing via clap |
| `pipeline-mcp` | MCP server, tool registration, session management |
| `pipeline-docker` | bollard wrapper, image build, compose, cleanup |
| `pipeline-memory` | SQLite schema, queries, vector search, handover packet |
| `pipeline-github` | octocrab wrapper, PR status, Actions trigger, GHCR push |
| `pipeline-stages` | Static, unit, container, integration stage implementations |
| `pipeline-config` | pipeline.yaml deserialization, validation, defaults |
| `pipeline-report` | JSON report generation, design tool integration |

---

## CLI commands

```bash
pipeline init          # scaffold new project, create GitHub repo
pipeline run           # execute stage(s)
pipeline watch         # daemon, watch files + run on change
pipeline deploy        # build, push, deploy to target env
pipeline monitor       # maintenance daemon
pipeline mcp           # start MCP server (agents connect here)
pipeline dev           # start mcp + watch together (primary dev command)
pipeline report        # open last report
```

---

## External tools used (all run as Docker containers, no host install required)

| Purpose | Tool |
|---|---|
| Image vulnerability scan | Trivy |
| Secret scanning | trufflehog |
| Dockerfile lint | hadolint |
| API contract test | schemathesis |
| Mutation testing (Python) | mutmut |
| Mutation testing (JS/TS) | Stryker |

---

## Build order (incremental, useful at every stage)

```
Week 1 — Core + MCP skeleton
  pipeline_run (static + unit stages)
  pipeline_status, pipeline_logs
  MCP server running, agent can call 3 tools
  Already useful for development

Week 2 — Docker integration
  pipeline_run (container + integration stages)
  bollard integration, clean environment guarantee
  pipeline_fix_suggestion with memory context

Week 3 — Init + scaffold
  pipeline_init, pipeline_scaffold
  Project templates (python-uv, bun, rust, go)
  pipeline.yaml schema + validation

Week 4 — GitHub integration
  pipeline_preflight, pipeline_commit, pipeline_push
  GitHub Actions YAML generation
  PR status checks, branch protection

Week 5 — Deployment
  pipeline_deploy, pipeline_rollback
  pipeline_smoke_test
  GHCR image push, SSH + compose targets

Week 6 — Maintenance + memory maturity
  pipeline_monitor daemon
  pipeline_update_deps, pipeline_health
  Auto-PR on green update
  Full handover protocol, semantic memory, learning loop

v2 — LSP integration, WASM plugins
```

---

## Dogfooding rule

Pipeline must run itself from Week 1. The `pipeline.yaml` at the root of this repo is the primary test of Pipeline's own capabilities. If Pipeline cannot CI/CD itself, it is not ready.

---

## What makes Pipeline different

| Existing CI tools | Pipeline |
|---|---|
| CI is a push-time gate | CI is agent's inner loop |
| Pipeline config is YAML | Pipeline is MCP-callable Rust |
| Local ≠ CI environment | Local IS CI via Docker |
| Agent and pipeline are separate | Agent drives pipeline natively |
| No project memory | Accumulates institutional knowledge |
| Cloud-first | Local-first, cloud for confirmation |
| Works with one agent | Works with any agent via MCP |

---

## Positioning

> Pipeline is the infrastructure layer that makes any coding agent — cloud or local — capable of delivering production-grade software end to end, without the agent ever needing to know Docker, GitHub, or deployment specifics.

---

*Last updated: session 1 — initial design*
*Next: scaffold Rust workspace, implement pipeline-core + pipeline-mcp*
