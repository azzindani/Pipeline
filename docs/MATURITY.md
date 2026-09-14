# Maturity Capabilities — design note

> What separates a repo that compiles from a repo that is proven. Maps eleven field learnings onto Pipeline's existing 19-tool surface as new actions.

Status: design · §10 decided · implementation tracked in the task list. Baseline it builds on → [ASSESSMENT.md](ASSESSMENT.md).

---

## 1. Thesis

Token cost is dominated by **iteration rounds**, ✗ by prose density. A wrong architecture found at round 12 costs more than every compression rule ever saved. Accelerate phase 1 (scratch build) with a standardized scaffold → recursive improvement runs against a correct shape from round 1.

Caveat: `tools/list` is a fixed per-session tax (82,636 B ≈ 24k tokens), independent of scaffold quality. Separate budget · ✗ conflate.

---

## 2. Maturity model

Software maturity = **breadth of proof**, ✗ coverage percentage. Coverage proves lines executed. Maturity proves real-world conditions survived.

| Level | Proof available | Typical state |
|---|---|---|
| 0 | Compiles · lints | Scaffold |
| 1 | Unit tests pass | Most repos stop here |
| 2 | Container + integration green | Deployable |
| 3 | E2E · visual · a11y · endpoint · fixtures | Observable in a browser |
| 4 | Load · stress · chaos · evals on real data | Behavior known under pressure |
| 5 | Motion + latency measured numerically · budgets enforced in CI | Regression-proof |

Coverage is one row at level 1. A repo at 95% coverage and level 1 is less mature than one at 70% and level 4.

---

## 3. Learnings → surface map

Eleven field learnings. Every one lands as an **action on an existing tool**. Tool count stays 19 (§7).

| # | Learning | Tool | Status |
|---|---|---|---|
| 1 | Test breadth: e2e · playwright · UI/UX · screenshot · video · fixtures · stress · evals on real data · endpoint · docker | `e2e` · `test` · `simulate` | Partial — gaps below |
| 2 | Key metrics: speed · performance · efficiency · hardware · throttle | `observe` | Partial — `perf_baseline` · `perf_compare` exist |
| 3 | Public simulated endpoint bound to a persistent domain | `deploy` | Missing |
| 4 | `dev_tools` — per-project reusable custom tooling | `project` | Missing |
| 5 | Motion measured as numbers, ✗ video | `e2e` | Missing |
| 6 | New capability = action, ✗ new tool | registry-wide | Rule, §7 |
| 7 | Concept spans both repos | Standards + Pipeline | §8 |
| 8 | Scaffold protocol · progress tracker for long runs | `session` · `plan` | Partial |
| 9 | Agents know git; remind ✗ rebuild | `standards` · `session` | Rule, §9 |
| 10 | Three environments: dev · staging · production | `deploy` · `env` | Partial |
| 11 | Two project processes: scratch build · maintenance | `plan` · `project` | Missing |

---

## 4. Existing coverage — do not rebuild

| Tool | Already real |
|---|---|
| `e2e` | run · browser_launch · browser_close · trace · screenshot · visual_regression · a11y_check · against_env · devtools_eval |
| `observe` | logs_aggregate · perf_baseline · perf_compare · optimize_suggest · image_size_optimize · query_optimize |
| `simulate` | persona_create · journey_define · use_case_define · load · chaos_inject |
| `deploy` | target · rollback · health · release_create · diff |
| `session` | 11 actions — lock · context · handover |
| `plan` | 21 actions — idea → feasibility → PRD → features → milestones · ADRs · risks |

`e2e.record` is Planned · `test.generate` · `test.property_generate` · `test.validation_create` are Scaffold. Promote these before adding neighbors — a Scaffold action next to a new real one misleads the agent.

---

## 5. Proposed actions

Fifteen actions. Estimated surface cost +6–8k B on `tools/list`.

### 5.1 Motion measurement — `e2e`

Models are 2D-static vision. Live video is unaffordable per frame and unnecessary: motion reduces to scalars an agent reasons over directly. Same reduction extends to spatial and 3D — perspective, trajectory, and depth are numbers before they are pictures.

| Action | Produces |
|---|---|
| `motion_measure` | Frame times · dropped frames · jank count · p50/p95/p99 input latency · time-to-first-paint · cumulative layout shift · animation duration vs declared |
| `motion_baseline` | Stores the above as the committed baseline for a route |
| `motion_compare` | Diffs a run against baseline · fails on regression beyond budget |

Rules: every value is a number with an explicit unit · ✗ return video | image as primary evidence · screenshots are supporting artifacts only · capture via CDP through the existing `devtools_eval` path. Target is a descriptor, ✗ a URL — v1 resolves web targets and refuses others with the backend named (§10.2).

### 5.2 Real-condition testing — `test` · `simulate`

| Action | Tool | Produces |
|---|---|---|
| `fixture_create` | `test` | Versioned fixture set from real data, secrets stripped |
| `eval_run` | `test` | Eval suite against real data · scored, ✗ pass/fail only |
| `endpoint_probe` | `test` | Every declared endpoint exercised · status · latency · schema conformance |
| `docker_verify` | `test` | Image runs · healthcheck passes · declared ports answer |
| `video_capture` | `e2e` | Session recording as a supporting artifact · numbers stay primary |
| `stress` | `simulate` | Ramp to failure · records the breaking point, ✗ only pass at target load |

### 5.3 Metrics and hardware — `observe`

| Action | Produces |
|---|---|
| `resource_measure` | CPU · memory · disk I/O · network per stage |
| `throttle_test` | Behavior under constrained CPU · memory · network |
| `efficiency_report` | Work per unit resource · cost per request |

### 5.4 Public endpoint — `deploy`

| Action | Produces |
|---|---|
| `tunnel_open` | Binds running container to a persistent public domain · returns URL |
| `tunnel_close` | Tears down · releases the name |

Rules: Cloudflare **named** tunnel, ✗ quick tunnel (§10.3) — the domain is persistent per project and survives restarts. A changing URL breaks the human review loop this exists to serve. Tunnel targets dev | staging only. ✗ expose production through a tunnel.

### 5.5 Dev tools — `project`

| Action | Produces |
|---|---|
| `devtool_add` | Registers a project-local tool · name · entry point · contract |
| `devtool_list` | Lists registered tools for this project |
| `devtool_run` | Executes one with arguments |

Rules: dev tools are project-local and committed, ✗ global. Pipeline hosts and executes them; the agent authors them (§10.1) — Pipeline validates the contract, ✗ the logic. A dev tool used by a second project is promoted per the primitives standard, ✗ copied.

### 5.6 Maintenance mode — `plan`

| Action | Produces |
|---|---|
| `mode_set` | Declares `build` | `maintain` · changes which gates apply |

Build mode optimizes for scaffold speed and shape correctness. Maintain mode optimizes for regression safety: baselines frozen, motion and perf compares mandatory.

---

## 6. Environments

Three, fixed: `dev` → `staging` → `production`.

| Env | Purpose | Gates to enter |
|---|---|---|
| dev | Agent inner loop | Stage 0 + 1 |
| staging | Human-visible · tunnel-bound · real-ish data | Stage 0–3 · e2e · motion baseline recorded |
| production | Users | Preflight · security · motion compare within budget · manual approval |

Environment is first-class in `pipeline.yaml`, ✗ a deploy argument. `e2e.against_env` and `deploy.target` resolve against it.

Implemented in `pipeline-config` as `Environments` · `Environment { name · requires · tunnel · approval }`. Omitting the block yields exactly these three rungs with these gates — defaults live in code, ✗ in every project's YAML. An absent block is never an empty ladder: that would read as "no gates" and let anything promote straight to production.

---

## 7. Surface rule

New capability = new **action** on an existing tool. ✗ new tool.

Rationale is measured, ✗ aesthetic: `tools/list` cost is driven by per-action schema clauses. 19 tools · 175 actions = 82,636 B, of which inputSchema is 62,311 B (75%). Merging | splitting tools moves the same text; it does not reduce it. Tool count also stays well under the ~40–50 where clients degrade.

Ceiling: 19 tools · ≤ 200 actions. Crossing 200 → drop Scaffold and Planned actions first (27 available), ✗ add a tool.

---

## 8. Split across repos

| Repo | Owns |
|---|---|
| Standards | The rules — what maturity level demands, which metrics are mandatory, environment promotion policy, dev-tool conventions, primitive-based construction |
| Pipeline | Enforcement and execution — the actions that measure, gate, and report against those rules |

Standards targets: `testing/` (+ `REALITY.md`, `PRESSURE.md`) own test breadth · `observability/` owns metrics · `performance/` owns budgets · `devops/` owns environment promotion · `primitives/` owns reusable-unit construction including dev tools.

✗ restate a Standards rule inside Pipeline. Pipeline cites the standard by id and fails the gate; the rule text lives in one place.

---

## 9. Agent scaffold protocol

Agents already use git correctly — branch · PR · issue · commit. Remind, ✗ rebuild.

Two real gaps remain, both owned by `session`:

| Gap | Detail |
|---|---|
| Worktrees | Understood less reliably than branches. Pipeline states when a worktree is required, ✗ assumes |
| Long-run progress | Agents lose the thread across a context reset. `session` handover already carries state — extend it with an explicit progress tracker: declared goal · completed steps · remaining steps · current blocker |

A progress tracker is re-read after every reset. This is also where the primitives registry is re-queried — stale in-memory knowledge of the unit tree is a primary source of duplication.

---

## 10. Decisions

Recorded rather than left open · each names the alternative rejected and why.

### 10.1 Dev tools — Pipeline hosts, ✗ generates

Agent authors the tool · Pipeline registers, executes, and tracks it.

| Considered | Verdict |
|---|---|
| Pipeline generates tools from templates | ✗ — generation requires domain knowledge Pipeline does not have. A generated tool is a scaffold, and §4 already shows scaffolds are the weakest output |
| Pipeline hosts agent-authored tools | ✓ — authoring is the agent's strength · registry · execution · contract enforcement are Pipeline's |

Consequence: `devtool_add` registers a contract pointing at an entry point the agent wrote. Pipeline validates the contract, ✗ the logic. A dev tool reaching a second project is promoted per [primitives](https://github.com/azzindani/Standards/blob/main/primitives/STANDARDS.md), ✗ copied.

### 10.2 Motion — web/CDP first, numeric contract transport-agnostic

| Considered | Verdict |
|---|---|
| Web + native + 3D in v1 | ✗ — three capture backends before one metric schema is proven |
| Web/CDP only, schema closed to web | ✗ — locks out the spatial case the model exists to serve |
| Web/CDP first, schema defined independently of transport | ✓ |

The metric schema is the deliverable, ✗ the capture path. Frame interval · dropped frames · latency percentiles · displacement per frame are all transport-neutral. Native and 3D land as additional capture backends emitting the same schema — ✗ new actions, ✗ a changed contract.

Consequence: `motion_measure` takes a target descriptor, ✗ a URL. v1 resolves web targets only and refuses others with the backend named.

### 10.3 Tunnel — Cloudflare named tunnel

| Considered | Verdict |
|---|---|
| Cloudflare quick tunnel (`trycloudflare.com`) | ✗ — random hostname per start. A URL that moves breaks the human review loop the feature exists to serve |
| Cloudflare named tunnel | ✓ — hostname is allocated once per project and survives restarts |
| ngrok | ✗ — certificate-pinned client; unusable behind an inspecting proxy, which is exactly the environment agents run in |

Credentials resolve through [configuration](https://github.com/azzindani/Standards/blob/main/configuration/STANDARDS.md) cascade · ✗ stored in `.pipeline/`, ✗ echoed into run output or digests. Name allocation is recorded in project config so it is recoverable after a machine change.

---

## 11. Order of work

1. Promote the 7 Planned + 20 Scaffold actions | delete them. ✗ build new actions beside misleading ones.
2. `e2e.motion_*` — highest novelty, unblocks the numeric-evidence model.
3. `observe.resource_measure` · `throttle_test` — feeds motion budgets.
4. `deploy.tunnel_*` — unblocks human review of staging.
5. `test.fixture_create` · `eval_run` · `endpoint_probe` · `docker_verify`.
6. `project.devtool_*` · `plan.mode_set`.

Gate: each lands with standalone tests and a real run against Pipeline itself, per the dogfooding rule.
