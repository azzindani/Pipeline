# Field report · adopting Vera

> The first time Pipeline was pointed at a project that was not Pipeline.
> Four gaps surfaced in the first twenty minutes. All four are fixed; this
> is the log, kept because the *findings* are the deliverable, not the code.

**Target.** [`azzindani/Vera`](https://github.com/azzindani/Vera) — an agentic retrieval
MCP server (Rust · Postgres + pgvector). At adoption time: 11 design documents, a git
history, **zero lines of code**. A written spec with no implementation is the ideal
dogfood: every lifecycle phase is exercised, and nothing can pass by accident.

Chosen because it is close enough to route the same standards as Pipeline
(`local_mcp` · `rust` · `architecture`) and far enough that nothing works "because it's
the same repo."

---

## What broke, and why it mattered

### 1 · The MCP server had no notion of a target project

88 `current_dir()` calls across the handlers; `ServerState` carries no project root;
`pipeline mcp` had no way to say *which* project. One server = whatever directory it
happened to be spawned in. An agent working in repo A could not drive repo B — which is
the entire premise of "any agent, any project, over MCP."

**Fix.** `pipeline mcp --project <dir>`, a single `set_current_dir` at startup.
Deliberately *not* a second source of truth: handlers keep resolving from the cwd, and
the flag just decides what the cwd is.

```bash
pipeline mcp --transport stdio --project /root/Vera
```

### 2 · Pipeline could only ever create greenfield projects

```
pipeline_project.init → ok=false · target '/root/Vera' is non-empty
```

`init` refused any directory with content in it. But greenfield is the rare case — a
real project has a git history, docs, and source before Pipeline ever sees it. Pipeline
could scaffold, and could not **adopt**.

**Fix.** `pipeline_project.init(adopt: true)` — writes only what is missing, records
what it skipped, and never overwrites a file that already exists.

```json
{ "adopted": true,
  "files_written": ["/root/Vera/pipeline.yaml", "/root/Vera/Cargo.toml", "/root/Vera/src/main.rs"],
  "files_skipped": ["/root/Vera/.gitignore", "/root/Vera/README.md"] }
```

`git diff --stat` on Vera afterwards: **empty**. Not one tracked file modified. The
non-clobber guarantee is pinned by `adopt_writes_the_gaps_and_never_clobbers`, which
asserts on file *contents*, not just on the skip list.

Adopt is idempotent — the second run wrote the one missing file and skipped the other four.

### 3 · Every scaffolded project was born unable to use standards

The one that mattered most. On the freshly adopted Vera:

```
pipeline_standards.route → ok=false · no standards cache · set standards.source in pipeline.yaml
```

The template emitted **no `standards:` block at all**. Standards injection — Pipeline's
headline capability — worked on exactly one project in the world: Pipeline, because that
`pipeline.yaml` was written by hand. Every project Pipeline created was born without it,
and nothing in the test suite noticed, because the suite only ever checked that the yaml
*file* appeared.

**Fix.** `standards_block()` seeds the routing keys it can infer per template:

| Template | `project_type` | `surfaces` |
|---|---|---|
| `mcp-server-rust` | MCP server | Command line |
| `microservice-rust` | REST/gRPC service (Go/Rust) | HTTP / gRPC API · Deployed service |
| `lib-rust` | Library / SDK | Public package |
| `cli-rust` · `custom` | — (no matching corpus key) | Command line · — |

! Those values are ROUTER keys owned by the **corpus**, ✗ by Pipeline. They are seeded
defaults, not a restatement of the routing rules — if the corpus renames one, `route`
reports it under `unknown_routes` rather than silently dropping the binding. The
vocabulary was read out of `index.json` rather than invented.

Two regression tests now stand behind this: one asserts the emitted config *parses and
carries the routing keys*, the other asserts **every** template emits a config
`pipeline_config` can deserialize. The old suite would have passed with an empty file.

### 4 · A dangling comment after the first pin

Cosmetic, caught only by doing it for real. The template wrote a `# pin: <commit sha>`
placeholder; `pipeline_standards.pin` appends the live key *below* it, leaving a comment
describing a line that now exists twice over. Replaced with a hint that reads correctly
in both states.

### 5 · Pipeline's own `.gitignore` did not ignore `.pipeline/`

Found by tripping over it: committing this work swept `.pipeline/` — SQLite WAL, a repo
registry full of absolute machine paths — into the commit. The rule was there:

```gitignore
# Pipeline runtime data (ignore by default; uncomment to commit shared memory)
# .pipeline/
```

The comment says the directory is ignored by default. The rule beneath it is commented
out, so it never was. The scaffolding template has always emitted this correctly —
only Pipeline's own hand-written file was wrong, which is the same class of bug as
finding 3: **the paths Pipeline generates were tested; the paths Pipeline was born with
were not.**

### 6 · The handover packet dropped the entire plan

Planning Vera through `pipeline_plan.*` succeeded on all 24 calls — PRD, 11 features with
acceptance criteria, 5 milestones. Then `pipeline_session.handover` returned:

```json
{ "project": {"id":"Vera","name":"Vera","stack":"rust"},
  "active_session": null, "last_run": null, "recent_failures": [] }
```

Pipeline had just stored the plan and then declined to hand it over. `HandoverPacket`
carried four fields — project · active_session · last_run · recent_failures — all of
which answer *what broke*, and none of which answer *what are we building*. A
reconnecting agent got a project name.

This is the load-bearing claim in CLAUDE.md ("Pipeline owns all persistent context;
agents are stateless consumers") failing at exactly the tool that exists to deliver it.
It is also directly on the critical path for this exercise: handover **is** the mechanism
that makes session N+1 work.

**Fix.** `HandoverPacket.active_work` — goal · goals · non_goals · feature counts by
status · unfinished features *with their acceptance criteria* · milestones with exit
criteria · open risk count. Reads rather than requires: a project with no plan yields an
empty `ActiveWork`, ✗ an error.

### 7 · …and then replayed it backwards

The fix worked and immediately exposed a second bug, visible only on real data with
meaningful ordering:

```
milestones: ['M5 · delivery', 'M4 · ingest', 'M3 · tool surface', 'M2 · retrieval core', 'M1 · foundation']
next up   : delivery · eval-harness · ingest-pipeline …
```

`list_scope` orders `created_at DESC`. Correct for "recent failures", wrong for a plan:
an agent reading `next_features` would have started with `delivery` — the last thing to
build — and worked toward the schema.

**Fix.** `in_plan_order()` re-sorts on the payload's own `created_at`, oldest-first,
with a stable sort so a scripted planning pass writing many rows in the same second
keeps its insertion order. Scoped to handover; `list_scope`'s recency ordering is left
alone for the callers that want it.

A unit test would not have caught this. Two fixtures sort the same either way; it took
five milestones with real names before the reversal was legible.

---

## Vera, after

```
route  ok=true   runtime=rust  type=MCP server  surfaces=[Command line]
       bound (22): architecture, code_writing, design, directory, configuration,
       dependencies, documentation, error_handling, observability, performance,
       security, testing, cicd, code_review, git, workflow, cli, local_mcp,
       local_mcp/delivery, local_mcp/runtime, local_mcp/tools, rust

check  ok=false  blocking: no standards.pin in pipeline.yaml · gates are unversioned
       → pipeline_standards.pin  → 5a5b5d0f35dfd75f040215bc1b7a73b57236fbf8
check  ok=true   bound=22  obligations=529  blocking=[]
```

22 standards · 529 obligations · gate green. Vera differs from Pipeline by three
standards (`testing/pressure` · `api` · `local_mcp` surface routes), which is the routing
doing real work rather than echoing a fixture.

Note the pin step: for Vera this was a **first** pin, not a move — there was no prior SHA
to overwrite, so no human judgement was owed. Compare
[`standards-guided-loop.md`](standards-guided-loop.md), where a *re*-pin was surfaced and
deliberately left for review.

---

## What this says about the roadmap

The exercise reordered the build queue by demand rather than by plan:

- **Vera needs Postgres + pgvector**, and `pipeline_data.db_provision` ·
  `schema_generate` · `schema_migrate` are all `not_implemented`. That is the next real
  wall, and it is now a requirement with a named consumer instead of a roadmap row.
- **Adopt is the common path, scaffold is the rare one.** The tool surface was built the
  other way round. Worth re-examining wherever else Pipeline assumes it created the thing
  it is looking at.
- **The agent skipped the gate and CI caught it.** This work was validated with
  `cargo clippy` + `cargo test` run directly — and the first push went red on
  `cargo fmt --check`. `pipeline run fast` runs fmt as its *first* static check and
  would have caught it in 6 seconds. The inner loop only works if the agent actually
  uses it; reaching for the underlying tool is the failure mode Pipeline exists to
  remove, and it took one push to fall into it.
- **Pipeline had never been consumed as an MCP server by an agent** — `mcpServers` was
  `{}` in every Claude Code scope. Its dogfooding covered its own CI, not its own
  protocol. Three of the four findings above were invisible from inside the test suite
  and obvious within minutes of a real client connecting.
