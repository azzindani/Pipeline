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

## Development standards

All code, architecture, testing, security, and tooling decisions in this project follow the standards defined in:

> **https://github.com/azzindani/Standards**

This is the authoritative reference. When any decision is ambiguous, consult the relevant standard before proceeding.

### How to use Standards in this project

Pipeline loads standards at init and keeps a local cache in `.pipeline/standards/`. Standards are fetched by category on demand and updated on `pipeline update`.

```
pipeline standards fetch              # clone/pull Standards repo locally
pipeline standards list               # show all available standards
pipeline standards show <category>    # read a specific standard
pipeline standards apply <category>   # agent applies standard to current codebase
```

### Standards that apply to Pipeline itself

Every standard in the catalog applies to Pipeline's own codebase. Priority order for agents working on Pipeline:

| Priority | Standard | Path | Applies to |
|---|---|---|---|
| 1 | Architecture | `architecture/STANDARDS.md` | Overall system structure |
| 2 | Rust | `rust/STANDARDS.md` | All Rust crates |
| 3 | CI/CD | `cicd/STANDARDS.md` | Pipeline's own pipeline.yaml |
| 4 | Local MCP | `local_mcp/STANDARDS.md` | pipeline-mcp crate |
| 5 | CLI | `cli/STANDARDS.md` | pipeline-cli crate |
| 6 | Testing | `testing/STANDARDS.md` | All test suites |
| 7 | Error handling | `error_handling/STANDARDS.md` | All crates |
| 8 | Observability | `observability/STANDARDS.md` | pipeline-report, logging |
| 9 | Security | `security/STANDARDS.md` | Secret scanning, gates |
| 10 | Database | `database/STANDARDS.md` | pipeline-memory SQLite schema |
| 11 | Git | `git/STANDARDS.md` | Branching, commits, tags |
| 12 | Agent | `agent/STANDARDS.md` | This CLAUDE.md file itself |
| 13 | Directory | `directory/STANDARDS.md` | Project layout |
| 14 | Dependencies | `dependencies/STANDARDS.md` | Cargo.toml management |
| 15 | Performance | `performance/STANDARDS.md` | Stage runner performance |

### Standards for projects Pipeline manages

When Pipeline scaffolds or manages a target project, it selects and applies the relevant subset of standards based on that project's stack:

```
python-uv project  → architecture · python · testing · cicd · security · git · error_handling
bun/typescript     → architecture · typescript · testing · cicd · security · git · error_handling
rust project       → architecture · rust · testing · cicd · security · git · error_handling
go project         → architecture · go · testing · cicd · security · git · error_handling
web project        → architecture · web · api · database · security · testing
ml project         → architecture · ml · data_pipeline · python · testing
mcp server         → architecture · local_mcp · cli · error_handling · security
```

Pipeline's `pipeline_init` command reads the target stack and automatically fetches + applies the correct standard subset during scaffolding.

### Writing style rule (from Standards/CLAUDE.md)

All code comments, documentation, and agent outputs in this project follow the high-density writing rule from Standards:

- Strip: articles, weak modals, scaffolding phrases, hedging, restatements
- Operators: `→` leads-to · `·` co-required · `|` alternative · `✗` never · `;` except · `!` critical
- Structure over prose: comparisons → tables · workflows → `A → B → C`
- Never compress: negations · hard thresholds · exception clauses · code blocks

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
│   ├── pipeline-report/     # structured output, JSON
│   ├── pipeline-digest/     # repo digestion, capability extraction, license check
│   ├── pipeline-port/       # language translation, module porting, validation
│   ├── pipeline-re/         # reverse engineering: codebase, service, binary, docker
│   ├── pipeline-spec/       # specification generation, contract registry
│   └── pipeline-knowledge/ # export/import, knowledge packaging, llm context
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
| `pipeline-digest` | Repo clone, structural analysis, capability indexing, digest JSON |
| `pipeline-port` | Language translation planning, module mapping, porting validation |
| `pipeline-re` | RE targets: codebase, binary, service, Docker image, infra |
| `pipeline-spec` | OpenAPI, JSONSchema, Protobuf, AsyncAPI generation and registry |
| `pipeline-knowledge` | Knowledge export/import, LLM context packaging |

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
pipeline standards     # fetch, list, show, apply standards from Standards repo
pipeline repo digest   # ingest external repo, build capability index
pipeline repo port     # translate external repo to target language
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

## Build order

Phase ladder · per-milestone tool delivery · velocity targets · risk register live in **`PLAN.md`**. Update there, not here.

CLAUDE.md owns timeless project context (architecture · concepts · MCP surface concept · standards reference). PLAN.md owns execution.

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
| No standards enforcement | Enforces Standards repo across all projects |
| Cannot learn from other repos | Digests external repos, extracts capabilities |
| Cannot understand undocumented systems | Reverse engineers any target into structured digest |
| No spec generation | Reconstructs API, schema, Dockerfile specs from live systems |
| Knowledge lost between sessions | Knowledge exported, portable across agents and projects |
| Language-locked | Ports projects across languages with Standards compliance |

---

## Repo digestion

Pipeline can ingest any external repository and extract reusable capabilities, patterns, or logic — then apply them to the current project. This is distinct from copy-paste: Pipeline understands the source repo structurally and applies only what is relevant.

### What repo digestion means

```
Source repo (any language, any structure)
    ↓ pipeline_repo_digest()
Pipeline analyzes:
  - Directory structure and module boundaries
  - Core abstractions and their interfaces
  - Business logic patterns
  - Test strategies
  - Configuration patterns
  - Data flow and dependency graph
    ↓
Produces a digest: structured summary stored in .pipeline/digests/<repo-name>.json
    ↓
Agent queries the digest to replicate specific abilities into the current project
```

### Use cases

- Pull a retry/backoff pattern from a Go service into your Rust project
- Replicate an authentication flow from a reference implementation
- Extract a data validation pipeline from a Python project
- Copy a well-tested queue worker pattern into a new service
- Understand how a reference project handles observability, then apply it

### MCP tools for repo digestion

```
pipeline_repo_digest(url, branch?)
  → clones repo, analyzes structure, stores digest
  → returns: module map, pattern index, capability list

pipeline_repo_list_capabilities(repo_name)
  → lists extractable capabilities from a digested repo
  → example output: ["retry-with-backoff", "jwt-auth", "queue-worker", "rate-limiter"]

pipeline_repo_extract(repo_name, capability, target_path?)
  → agent-guided extraction of a specific capability
  → produces: implementation plan + relevant source references
  → does NOT blindly copy — agent adapts to current project's patterns

pipeline_repo_diff(repo_name, current_project)
  → compares digested repo against current project
  → surfaces: missing patterns, structural gaps, capability opportunities

pipeline_repo_apply_standards(repo_name)
  → checks digested repo against Standards
  → reports: which standards it follows, which it violates
  → useful before extracting — know what you're inheriting
```

### Digest storage

```
.pipeline/
└── digests/
    ├── my-reference-service.json     ← structured digest
    ├── auth-library.json
    └── data-pipeline-example.json
```

Each digest contains:

```json
{
  "repo": "https://github.com/org/repo",
  "digested_at": "2026-04-30T10:00:00Z",
  "language": "go",
  "structure": { ... },
  "capabilities": [
    {
      "name": "retry-with-backoff",
      "location": "pkg/retry/retry.go",
      "interface": "Retry(fn func() error, opts RetryOpts) error",
      "dependencies": ["context", "time"],
      "test_coverage": "pkg/retry/retry_test.go",
      "pattern": "exponential-backoff-with-jitter"
    }
  ],
  "standards_compliance": {
    "architecture": "partial",
    "error_handling": "compliant",
    "testing": "compliant"
  }
}
```

### Important boundaries

- Digestion does not copy code automatically — agent always mediates extraction
- License check runs before digest — GPL/AGPL flagged, extraction blocked unless user confirms
- Secrets scan runs on digested repo — flagged before any extraction
- Private repos supported — SSH key or token passed via Pipeline config, never stored in digest

---

## Repo porting (language translation)

Pipeline can break down a reference project and port its logic to a different programming language. This goes beyond digestion — it produces working translated code following the target language's standards.

### What porting means

```
Source repo (language A)
    ↓ pipeline_repo_port()
Pipeline decomposes:
  - Identifies language-agnostic logic units (pure functions, data transforms, state machines)
  - Maps language-specific constructs to equivalents in target language
  - Preserves: interfaces, contracts, test cases (translated), error handling patterns
  - Discards: language idioms that don't translate, runtime-specific details
    ↓
Agent produces translated implementation in target language
    ↓
Pipeline validates translated output through its own stage runner
```

### Supported translation paths (v1)

| From | To | Confidence |
|---|---|---|
| Python | Rust | High — type system improves on translation |
| Python | Go | High — concurrency model maps well |
| Python | TypeScript | High — dynamic → typed, good tooling |
| Go | Rust | High — ownership maps to borrow checker |
| Go | TypeScript | Medium — goroutines need adaptation |
| TypeScript | Rust | Medium — async model differs |
| TypeScript | Go | Medium — interface patterns differ |
| Rust | Go | Medium — ownership concepts simplify |
| Any | Python | High — Python is always the fallback |

### MCP tools for porting

```
pipeline_repo_port(url, target_language, scope?)
  → scope: "full" | "module:<name>" | "capability:<name>"
  → produces: porting plan with complexity estimate per module

pipeline_repo_port_module(repo_name, module_path, target_language)
  → translates a single module
  → returns: translated code + test translation + adaptation notes

pipeline_repo_port_validate(ported_path)
  → runs Pipeline's stage runner against ported code
  → confirms translated code passes static, unit, and integration stages
  → compares behavior against original test suite (if available)

pipeline_repo_port_report(repo_name, target_language)
  → full report: what translated cleanly, what needed adaptation, what was dropped
  → flags: patterns with no direct equivalent, manual review required items
```

### Porting process (agent-driven)

```
1. Digest source repo → build module map
2. Identify pure logic units (language-agnostic core)
3. Map each unit to target language equivalent
4. Translate module by module (not file by file)
5. Translate tests alongside implementation
6. Run Pipeline stages on each translated module
7. Fix failures → rerun → green → move to next module
8. Final integration test of full ported project
9. Generate porting report
```

### Standards applied during porting

Ported code is held to the target language's standards from the Standards repo, not the source language's. A Python project ported to Rust must comply with `rust/STANDARDS.md`, not `python/STANDARDS.md`. Pipeline enforces this automatically during the validation stage.

### What porting cannot do

- Translate runtime-specific behavior (e.g., Python GIL assumptions, Go scheduler behavior)
- Guarantee identical performance characteristics
- Handle external dependencies with no equivalent in target ecosystem — flagged for manual substitution
- Port UI/frontend code (scope: backend logic only in v1)

---

## Positioning

> Pipeline is the infrastructure layer that makes any coding agent — cloud or local — capable of delivering production-grade software end to end, without the agent ever needing to know Docker, GitHub, or deployment specifics.

---

*Last updated: session 1 — initial design + Standards + digest + port + reverse engineering + extended capabilities*
*Next: scaffold Rust workspace → pipeline-core → pipeline-mcp → pipeline-digest → pipeline-port → pipeline-re → pipeline-knowledge*

---

## Reverse engineering

Reverse engineering in Pipeline means taking an existing system — compiled binary, running service, undocumented codebase, API with no spec, or legacy monolith — and reconstructing its intent, structure, contracts, and logic into a form agents can work with.

In the AI agent era this is no longer a slow manual process. Agents can parallelize analysis across hundreds of files, correlate patterns across layers, and produce structured output that feeds directly into digest, port, or scaffold workflows.

### What reverse engineering targets

```
BINARY / COMPILED
  Executable with no source → reconstruct logic + data structures
  Closed-source library → extract interface + behavior contracts
  WASM module → reconstruct intent from bytecode

UNDOCUMENTED CODEBASE
  Repo with no docs, no tests, no comments → reconstruct architecture
  Spaghetti legacy code → identify module boundaries that don't exist yet
  Dead project → extract reusable patterns before it's abandoned

RUNNING SERVICE (black-box)
  API with no spec → reconstruct OpenAPI schema from observed traffic
  Database with no schema docs → reconstruct schema from queries + data
  Message queue with no contract → reconstruct message shapes from consumers

DEPLOYED SYSTEM
  Docker image → reconstruct Dockerfile + compose + environment
  Running container → extract config, env vars, runtime behavior
  Infrastructure (cloud or on-prem) → reconstruct IaC from live state
```

### Reverse engineering pipeline

```
Target (any of the above)
    ↓ pipeline_re_analyze()
Stage 1 — Surface extraction
  Static: file tree, binary strings, symbol tables, import/export maps
  Dynamic (if runnable): execution traces, syscall logs, network traffic
  Semantic: naming patterns, magic numbers, error messages

Stage 2 — Structure reconstruction
  Module boundary detection (clustering by coupling/cohesion)
  Data flow mapping (where does data come in, transform, go out)
  Dependency graph (internal + external)
  State machine reconstruction (for services with lifecycle)

Stage 3 — Intent reconstruction
  Agent reasons over extracted structure
  Names anonymous constructs based on behavior
  Identifies design patterns (retry, circuit breaker, repository, etc.)
  Cross-references against Standards repo for pattern recognition

Stage 4 — Contract generation
  OpenAPI spec from observed API behavior
  Type definitions from runtime data shapes
  Test suite from observed inputs/outputs
  Architecture diagram from module map

Stage 5 — Output (feeds into digest/port/scaffold)
  Structured digest (same format as pipeline_repo_digest output)
  Reconstructed source skeleton (stubs + interfaces, agent fills logic)
  Standards compliance gap report
  Recommended modernization path
```

### MCP tools for reverse engineering

```
pipeline_re_analyze(target, type?)
  → target: path | url | image | host:port
  → type: "codebase" | "binary" | "service" | "docker" | "infra" | auto-detect
  → starts analysis, returns job_id for async tracking

pipeline_re_status(job_id)
  → progress per stage, ETA, partial results available

pipeline_re_report(job_id)
  → full structured output: module map, contracts, patterns, gaps
  → same digest format as pipeline_repo_digest — feeds into port/scaffold

pipeline_re_reconstruct_api(target_url, sample_count?)
  → fires observed requests, collects responses
  → produces OpenAPI 3.1 spec from traffic analysis
  → agent fills in descriptions, validates edge cases

pipeline_re_reconstruct_schema(connection_string)
  → connects to live DB (read-only)
  → reconstructs schema: tables, relations, constraints, indexes
  → infers missing foreign keys from naming + data patterns
  → produces migration files + schema diagram

pipeline_re_reconstruct_dockerfile(image_name)
  → pulls image, inspects layers, env, entrypoint, labels
  → reconstructs best-guess Dockerfile + compose
  → applies docker/STANDARDS.md best practices to reconstruction
  → flags: hardcoded secrets, root user, missing healthcheck

pipeline_re_modernize(job_id, target_stack?)
  → takes RE output → produces modernization plan
  → breaks monolith into bounded services
  → maps to target stack (default: current project's stack)
  → produces: phased migration plan, risk assessment, effort estimate
```

### AI acceleration in reverse engineering

Without agents, RE is bottlenecked by human reading speed and pattern recognition. With agents:

| Task | Manual | Agent-accelerated |
|---|---|---|
| Read 50k LOC legacy codebase | Days | Minutes |
| Identify module boundaries | Weeks | Hours |
| Reconstruct undocumented API | Days of traffic analysis | Minutes of observed calls |
| Name anonymous functions | Hours | Seconds (behavior-based naming) |
| Detect design patterns | Expert knowledge required | Cross-reference against Standards |
| Produce architecture diagram | Half a day | Automatic from module map |
| Generate test suite from behavior | Days | Hours (inputs/outputs observed) |
| Produce modernization plan | Consultant engagement | Structured agent output |

### Boundaries and ethics

- RE of third-party software: user is responsible for license and legal compliance
- Pipeline flags GPL/proprietary licenses before any RE output is used in a new project
- Secrets found during RE are never stored — flagged and discarded immediately
- Binary RE (decompilation) produces reconstructed intent, not decompiled source — avoids direct IP reproduction

---

## Extended capabilities roadmap

Reverse engineering, digestion, and porting unlock a family of higher-order capabilities. These are the natural next steps once those foundations exist.

### 1. Architecture synthesis

From multiple digested or RE'd repos, Pipeline synthesizes a new architecture combining the best patterns from each source.

```
pipeline_arch_synthesize(sources[], target_stack, constraints?)
  → ingests N digests/RE outputs
  → identifies: best-in-class pattern per concern (auth, data, queue, API, etc.)
  → produces: composite architecture spec + rationale
  → agent generates scaffold from spec
```

Example: synthesize auth from repo A, queue handling from repo B, API design from repo C → new project inherits the best of all three.

### 2. Dependency archaeology

Understand what your dependencies actually do — not what their docs say.

```
pipeline_dep_archeology(package_name, version?)
  → RE the dependency itself
  → produces: actual behavior contract, what it really does at runtime
  → flags: hidden behaviors, undocumented side effects, security surface
  → compares: documented API vs actual behavior
```

Critical when adopting a new library — especially in AI-generated code where agents pull in packages they "know" but haven't verified.

### 3. Specification generation

From any source (codebase, running service, RE output) → produce formal specifications agents can validate against.

```
pipeline_spec_generate(source, format?)
  → format: "openapi" | "jsonschema" | "protobuf" | "asyncapi" | "typespec"
  → produces: machine-readable spec from observed behavior
  → registers spec in .pipeline/specs/ — used by contract testing stage
  → feeds into pipeline_re_reconstruct_api as structured output
```

Once a spec exists, Pipeline's Stage 3 (integration) runs contract tests against it automatically.

### 4. Vulnerability surface mapping

RE-powered security analysis beyond what static scanners catch.

```
pipeline_security_map(target)
  → RE the attack surface: exposed endpoints, input boundaries, trust boundaries
  → maps: data flows that cross trust boundaries
  → identifies: injection points, auth gaps, missing validation
  → produces: threat model skeleton (STRIDE) for agent to complete
  → feeds into security gate in Pipeline stages
```

This is distinct from CVE scanning (Trivy already does that). This is structural security analysis.

### 5. Test generation from behavior

Without writing a single test manually — observe behavior, generate tests that encode it.

```
pipeline_testgen_behavioral(target, duration?)
  → runs target (or observes live system) for duration
  → records: inputs, outputs, state transitions, error conditions
  → generates: test suite that encodes observed behavior as assertions
  → agent reviews + promotes to permanent test suite
  → runs through Pipeline stage 1 (unit) for validation
```

Particularly powerful for legacy systems with no tests — RE the behavior, encode it, then refactor safely.

### 6. Migration planning

From RE output → phased, executable migration plan.

```
pipeline_migration_plan(source_digest, target_stack, constraints?)
  → constraints: "no_downtime" | "incremental" | "big_bang" | "strangler_fig"
  → produces:
      Phase map with dependencies between phases
      Risk assessment per phase
      Rollback plan per phase
      Effort estimate per phase
      Pipeline stage configuration per phase
  → each phase is itself a pipeline.yaml-compatible task
```

The migration plan is executable — each phase runs through Pipeline's full stage suite before the next begins.

### 7. Knowledge export

Everything Pipeline learns about a project — memory, digests, RE outputs, specs — exported as a portable knowledge package.

```
pipeline_knowledge_export(project, format?)
  → format: "markdown" | "json" | "vector_db" | "llm_context"
  → produces: compressed knowledge dump any agent can ingest
  → use case: onboard new team member, switch agents, archive project
  → llm_context format: pre-chunked, pre-embedded, ready to inject into any model's context
```

Reverse: `pipeline_knowledge_import` — new project inherits knowledge from a related project.

### 8. Compliance checking

Given a codebase + a compliance framework → gap analysis.

```
pipeline_compliance_check(target, framework)
  → framework: "standards" | "owasp" | "pci_dss" | "gdpr" | "hipaa" | "iso27001"
  → RE the codebase against the framework's requirements
  → produces: compliant items · gaps · critical violations · remediation plan
  → agent can auto-remediate gaps that have known fixes
  → feeds into Pipeline's quality gate as a compliance score
```

For "standards" framework — checks against your own Standards repo. Most immediate use case.

---

## Capability relationships

How all Pipeline capabilities connect:

```
ANALYZE                    TRANSFORM                  VALIDATE
───────────────────────    ─────────────────────────  ──────────────────────
pipeline_re_analyze    →   pipeline_repo_port      →  pipeline stages (CI)
pipeline_repo_digest   →   pipeline_arch_synthesize→  pipeline_compliance_check
pipeline_dep_archeology→   pipeline_migration_plan  →  pipeline_testgen_behavioral
pipeline_re_reconstruct→   pipeline_spec_generate   →  pipeline_security_map
                                    ↓
                           pipeline_knowledge_export
                                    ↓
                           Any agent, any session, any project
```

Everything in the analyze layer feeds the transform layer. Everything in the transform layer is validated by Pipeline's own stage runner. Knowledge export makes every output portable across agents and sessions.

---
