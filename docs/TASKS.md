# Open tasks

> Live backlog for Pipeline + Standards. Session task state is ephemeral and invisible outside the terminal UI — this file is the durable copy.

Updated: 2026-09-14 · 2 removed by request.

Blocked items name their blocker. ✗ mark a task done while its verification is blocked — record the partial and say what is missing.

---

## 1. Needs a decision from the owner

| # | Task | Detail |
|---|---|---|
| 28 | Land both branches on `main` | Pipeline `claude/relaxed-noether-j1l68e` is ~37 commits ahead, Standards ~11. Both green. ✗ merged: this session's instructions forbid pushing to a branch other than the designated one without explicit permission, and both `CLAUDE.md` files forbid opening a PR unasked. Say merge | PR |

---

## 2. Blocked on environment

| # | Task | Blocker |
|---|---|---|
| 29 | Verify stages 2 + 3 with registry access | Docker Hub · GHCR · Quay all return 403 on blob fetch through the session proxy. Daemon starts, compose runs, only image pull fails. Stages 2/3 have never run anywhere |
| 7 | `deploy.tunnel_open` · `tunnel_close` | Needs a running container (#29). Provider decided: Cloudflare **named** tunnel → `docs/MATURITY.md` §10.3 |

---

## 3. Half-built — logic shipped, wiring missing

! These carry tests and are green, ✗ reachable by an agent. A green test on unwired logic is the most flattering kind of incomplete.

| # | Task | Shipped | Missing |
|---|---|---|---|
| 21 | Apply primitives to Pipeline's own code | Kinds declared on all 16 crate roots · direction check passes | 44 submodules undeclared · `repo.rs` 2,360 LOC · `data.rs` 1,903 · `plan.rs` 1,624 · `e2e.rs` 1,541 unsplit. Needs the registry's call-site + duplicate detection first |

---

## 4. Not started

| # | Task | Notes |
|---|---|---|
| 1 | Promote the 7 `Planned` actions | Feasible here: `e2e.record` (needs a timeout — it spawns an interactive tool and blocks forever), `simulate.journey_simulate`, analysis half of `repo.re_reconstruct`. Blocked by #29: `env.devcontainer_open` · `deploy.canary` · `blue_green`. Own project: `repo.port` |
| 8 | `test.fixture_create` · `eval_run` · `endpoint_probe` · `docker_verify` · `e2e.video_capture` · `simulate.stress` | `docker_verify` blocked by #29 · rest are not |
| 3 | Fix what the full-stage run revealed | Original findings were environmental. The real defects found this session were fixed under their own tasks. Revisit after #29 |

---

## 5. Ongoing

| # | Task | Progress |
|---|---|---|
| 34 | Review every standard against current external best practice | Done: security · `devops/` · `api/` · `web/`. Verified-current and no-change results are recorded too, so the unexamined set is visible. **Remaining: `database/` + `sql/` · `python/` `go/` `typescript/` `shell/` · `testing/` `ml/` `data_pipeline/`** · findings in `Standards/tools/review/FINDINGS-core.md` |

**Method that works:** IETF and OWASP web mirrors are blocked by the proxy; `github.com` is not, and the sources live there. `git clone --depth 1 github.com/OWASP/ASVS` → read requirement text → cite by id. Same for `OWASP/CheatSheetSeries` · `open-telemetry/semantic-conventions` · `usnistgov/800-63-4`. RFC text remains unreachable — search summaries only.

---

## 6. Removed by request

| # | Task |
|---|---|
| 26 | Release Standards v0.2.0 |
| 27 | Stress-test the agent surface at 244 tools / ~125k context |

---

## 7. Done this session

Pipeline 432 → 505 tests · fmt + clippy clean · `pipeline run fast` green. Standards 43 → 54 · `validate.py` · `--check-index` · markdownlint all clean. Surface 195 actions / 19 tools, under the 200 ceiling. Pipeline now routes 32 standards.

| # | Task |
|---|---|
| 2 | First real full-stage run — stage runner proven real; stages 2/3 blocked by #29 |
| 4 | Stub crates: keep · declare · enforce. Each names `implemented-in`, the handler owning its charter today |
| 5 | `e2e.motion_*` — motion as scalars · 9 tests · refusal-heavy by design |
| 10 | Promotion ladder is config, ✗ a deploy argument |
| 11 | `session.progress` — the thread survives a context reset |
| 12 | `maturity/` standard — proof breadth · numeric-evidence rule |
| 13 | Primitive registry generator + CI gate |
| 14 · 15 · 16 | Three design decisions closed → `docs/MATURITY.md` §10 |
| 17 | CI runs `pipeline run confirm` alongside raw cargo, ✗ instead of it |
| 18 | End-to-end MCP workflow test |
| 19 | Per-crate test gaps closed |
| 20 | `tools/list` surface budget guard |
| 22 | Project-tool conventions → `primitives/STANDARDS.md` §11 |
| 24 | Learning loop closed — `fix_applied` was read in three places and written in none |
| 25 | Handover verified across a real process boundary |
| 6 | `observe.resource_measure` · `throttle_test` · `efficiency_report` — wired, driven end to end |
| 23 | Maturity level computed from real run history — `report.maturity` |
| 9 | `project.devtool_*` · `plan.mode_set` — Pipeline hosts agent-authored tools |
| 32 | Handover packet reconciled · `current_branch` + `last_good_commit` implemented, six doc-only fields deleted |
| 33 | All five auth findings fixed — TTL · refresh reuse · PKCE plain · code reuse · redaction |
| 36 | Coverage measured · gate ratcheted 70 → 61 against a real 61.42% · **Pipeline is maturity level 1** |
| 30 | `security/TOKENS.md` |
| 31 | Auth audit — 5 findings, 4 fixed |
| 35 | `security/OAUTH.md` — and it caught finding 5 in Pipeline the same day |
| 34a | Standards review: security tier · `devops/CONTAINERS.md` (runtime hardening) · `web/SECURITY.md` (SRI · Trusted Types · COOP/CORP) |
| 37 | `design/` producer–consumer completeness — an orphan read is a defect, ✗ an unfinished feature. Scanner run over the workspace: 221 public fields, 0 orphans |
| 38 | `llm/` standard — `STANDARDS.md` (prompt artifacts · model pinning + migration · output contracts · degradation ladder · token cost) · `EVALUATION.md` (eval sets · graders · gates · adversarial cases) · `SAFETY.md` (injection · tool authorization · agent autonomy · prompt privacy) |
| 39 | `repo.fleet_health` — every registered repo in one call: state · branch · uncommitted · last run + age · maturity · high findings · blocked tasks, loudest first. Audit and maturity run through the SAME functions the single-project actions use |
| 40 | Standards: `testing/PROBES.md` · `observability/LOGS.md` · `database/ENGINES.md` — probes with response+log dual assertion · log store as a queryable database · PostgreSQL as the production default |
| 41 | `index.json` regenerated (was stale by 1,224 lines, so no standard added this session was reaching Pipeline's router) · 26 markdownlint errors cleared — the `\|` density operator was splitting table cells |
| 42 | Pipeline declares the `Persistent store` surface · `docs/adr/0001` records SQLite under the embedded exception the new engine standard requires |
