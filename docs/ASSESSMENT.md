# Assessment — 2026-09-14

Baseline audit of Pipeline + Standards. State at Pipeline `c28f063` · Standards `53011d9`.

## Verified green

| Gate | Result |
|---|---|
| Toolchain | pinned 1.97.0 |
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo test --workspace` | 432 passed · 0 failed · 0 ignored |
| Binary | `pipeline --help` runs · 7 subcommands |
| Standards `tools/validate.py` | 42 standards · 0 errors · 0 warnings |
| CI (`confirm.yml`) | stage 0 static · stage 1 unit · secret scan · stdio+http smoke |

## MCP surface

19 tools · 194 actions — 167 real · 20 scaffold · 7 planned.
(175/148 at the time of the audit · `session.progress` added since.)
`tools/list` payload = 82,636 B ≈ 23–25k tokens (descriptions 20,325 B · inputSchema 62,311 B).

Tool count is comfortable; cost is driven by per-action `if`/`then` schema clauses, ✗ by tool count.
Collapsing 19 → 5 saves nothing · costs action-selection accuracy. Decision: keep 19.

Registry conformance (`tests/registry_conformance.rs`) enforces fidelity claims ·
`Planned` refused in `dispatch.rs` before handler runs → ✗ fabrication. Keep this property.

## Gaps — ranked

### 1. Never truly tested (highest value)

432 tests are unit tests inside `pipeline-mcp`. No test boots the server and drives a real
workflow. Stages 2 (container) · 3 (integration) have never run anywhere — CI runs 0+1 only.

### 2. Dogfooding rule violated

CLAUDE.md: "Pipeline must run itself from Week 1 ... if Pipeline cannot CI/CD itself, it is
not ready." `pipeline.yaml` exists; CI calls raw `cargo`, ✗ `pipeline run`.

### 3. Eight stub crates are dead weight

`digest` · `port` · `re` · `spec` · `knowledge` · `github` · `report` · `lsp` = 6 LOC each.
Nothing depends on them. Their functionality lives in `pipeline-mcp` handlers
(`repo.rs` 2,360 LOC carries digest/port/RE). Workspace layout advertises an architecture
that does not exist.

Distribution: `pipeline-mcp` 26,340 LOC (84% of workspace) · `pipeline-core` 191 LOC —
thin for what the handlers claim. 14 of 20 handlers shell out to real processes;
the 6 that do not (plan · session · memory · standards · report) are state management,
correctly file/DB-based.

## Agreed order of work

1. Run `pipeline run --stage full` against Pipeline itself → expect failure; that failure is
   the most valuable signal available.
2. Fix what it reveals — likely `pipeline-core`.
3. Decide the stubs: move `repo.rs` logic down into them, | delete them and accept a
   single-crate design. Current state is worst of both.
4. Tests last. More unit tests on unproven code cements assumptions.

## Maturity level — computed, ✗ asserted

First run of `pipeline_report.maturity` against real run history (22 runs):

First run, before coverage existed:

```text
level 0 · compiles
missing for next: coverage_gate
```

Level 0 with 495 passing tests was the correct answer, ✗ a model bug.
`gates.coverage: 70` was declared and never measured, so no coverage evidence
existed — and absent evidence is "not reached", ✗ a pass. A configured gate that
never runs provides nothing.

After wiring `cargo-llvm-cov` into the unit stage:

```text
level 1 · unit-proven
evidence present: build · coverage_gate · format · lint · typecheck · unit_tests
missing for next: image_build · services_healthy · integration_tests
```

! The three missing rows are the container and integration stages — the ones
blocked by the registry 403. The level is capped by a real blocker, ✗ by an
unwritten feature, and the report names it without being told.

**Measured line coverage: 61.42%.** The gate moved 70 → 61: 70 was never
measured, and a gate above the real number turns the build red for something no
single change caused. 61 is a ratchet — it catches a regression today and rises
deliberately.

---

## Repo state

Both repos: `main` only · no other remote branches · no open PRs · working trees clean.
Standards tagged `v0.1.0`; Pipeline untagged.
