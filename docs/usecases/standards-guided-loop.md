# Use case · the standards-guided development loop

> A real, live walk through Pipeline's core thesis: **any agent connects fresh, has the
> ruling Standards injected as context, and adjudicates the one governance finding the
> compliance gate surfaces — without ever memorising which rules apply.**

This is not a mock. Every response below was captured live from Pipeline's own MCP server
driven against this repository. Reproduce it in seconds:

```bash
scripts/usecase-standards-loop.sh          # human-readable, sectioned
scripts/usecase-standards-loop.sh --raw    # the raw JSON-RPC responses, one per line
```

The whole loop is **read-only and offline**. Standards resolve from the local clone named by
`standards.source` in `pipeline.yaml` (here `/root/Standards`), so nothing touches the network
and nothing mutates the repo.

---

## The scenario

A new agent — any agent, cloud or local model — is dropped into a standards-governed Rust
project and told to pick up work. Before it writes a line it has to answer three questions
Pipeline exists to answer for it:

1. **Where do things stand?** → `pipeline_session.handover`
2. **Which rules govern this code, and why?** → `pipeline_standards.route` · `.brief` · `.show` · `.checklist`
3. **Is the project currently in compliance?** → `pipeline_standards.check`

The "real task" is the finding at the end: the compliance gate reports the project is **not**
compliant, for one concrete, actionable reason. The agent, now holding the ruling standards as
context, adjudicates it.

---

## What "standards as context / prompt injection" means here

Pipeline consumes an **external, separately-versioned** corpus
(`github.com/azzindani/Standards`) as a dependency — resolved, pinned by commit SHA, and
*routed* to the specific standards that bind this project's stack and surfaces. It then
**injects** them into the agent's context in three tiers:

| Tier | Tool call | What the agent gets |
|---|---|---|
| **L0** | `pipeline_standards.brief` | the always-on packet: every standard in force + why it binds |
| **L1** | `pipeline_standards.show(id)` | one standard, in full, on demand before touching its surface |
| **L2** | `pipeline_standards.checklist` | the enforcement surface: every concrete obligation the routed set imposes |

The agent never guesses which rules apply, never restates them from memory, and never drifts
from the pinned version. That is the injection: **the governing rules arrive as context, keyed
to this exact project, at the moment they are needed.**

---

## The walkthrough

The JSON-RPC envelope is identical on both transports — `params.name` carries the tool,
`params.arguments.{action,args}` carries the rest:

```json
{"jsonrpc":"2.0","id":5,"method":"tools/call",
 "params":{"name":"pipeline_standards","arguments":{"action":"brief"}}}
```

### 1 · `initialize` — the endpoint answers

```
server   pipeline-mcp v0.0.1
protocol 2024-11-05
```

### 2 · `pipeline_meta.version` — what am I talking to

```json
{ "pipeline_config": "0.0.1", "pipeline_core": "0.0.1", "pipeline_mcp": "0.0.1",
  "pipeline_memory": "0.0.1", "pipeline_stages": "0.0.1" }
```

### 3 · `pipeline_session.handover` — Pipeline reports **real** state, never invents it

```
ok       False
error    project not found: pipeline
```

This is a feature, not a gap. On a cold checkout no session has been recorded, so there is no
handover packet to hand over — and Pipeline **says so** rather than fabricating a plausible
summary. (Register the project and run a session, and this fills with branch, last run,
failing stage, and active work.) The standards surface below needs no registered project: it
reads `pipeline.yaml` plus the corpus, which is why it is the natural cold-start entry point.

### 4 · `pipeline_standards.route` — **why** these standards bind

```
runtime       rust
project_type  MCP server
surfaces      ['Command line', 'Deployed service', 'Public package']
sha           5a5b5d0f35dfd75f040215bc1b7a73b57236fbf8
bound (25):   architecture, code_writing, design, directory, configuration, dependencies,
              documentation, error_handling, observability, performance, security, testing,
              testing/pressure, cicd, code_review, devops, git, workflow, api, cli,
              local_mcp, local_mcp/delivery, local_mcp/runtime, local_mcp/tools, rust
```

Provenance, not a hardcoded map: the binding is computed from this project's declared runtime,
type, and surfaces against the corpus's own ROUTER rules. Change the surfaces and the routed
set changes with them.

### 5 · `pipeline_standards.brief` — ★ THE INJECTION (the L0 context packet)

The markdown Pipeline hands the agent (abridged — 25 rows, 3.9 KB in full):

```markdown
# Standards in force

Source `/root/Standards` @ `5a5b5d0` (config). 25 standards bind this project.

> ! DRIFT — the pinned commit and the checked-out corpus differ. Gates may have moved.
> Run `pipeline standards update` to move the pin deliberately, or restore the pin.

| Standard | Tier | Owns | Why |
|---|---|---|---|
| `architecture` | Foundation | layer model · dependency direction · function placement… | always-on |
| `error_handling` | Core | error taxonomy · result types · propagation + boundary… | always-on |
| `security` | Core | input-validation boundary · injection prevention · authn/authz… | always-on |
| `observability` | Core | structured logging · operation receipts · golden-signal metrics… | always-on, surface:Deployed service |
| `local_mcp` | Domain | MCP architecture · engine/server split · repo + tier structure… | type:MCP server |
| `rust` | Language | ownership · Result/Option/? · thiserror/anyhow · trait design… | language, type:MCP server |
| … 19 more … |

Pull a full standard with `pipeline_standards.show(id)` before working on the surface it owns.
```

Note the `Why` column — `always-on`, `surface:Deployed service`, `type:MCP server` — that is
the routing rationale travelling **with** each rule. And note the **DRIFT** banner: the packet
tells the agent up front that the ground may have moved.

### 6 · `pipeline_standards.show rust` — L1, one binding standard in full

```
id       rust   tier Language   v1.0
owns     ownership + borrowing · Result/Option/? · thiserror/anyhow · crate + module
         layout · trait design · type-state + newtype · unsafe rules · tokio async …
content  20456 chars   (the complete rust/STANDARDS.md, defers-to graph included)
```

On demand, the agent pulls the full 20 KB standard for the surface it is about to touch —
not before, not all 25 at once.

### 7 · `pipeline_standards.checklist` — L2, the enforcement surface

```
standards    25
total_items  604

  [architecture] 28 items — first 3:
    · Every architectural decision traces to ≥ 1 principle (§1)
    · Innermost layer contains zero I/O calls (§2)
    · All I/O confined to the outermost layer (§2)

  [code_writing] 30 items — first 3:
    · Every name reveals intent; no `data` · `info` · `temp` · `misc` (§2, §11)
    · No abbreviations outside the allowed set (§2)
    · One verb per concept across the project (§2, §11)
```

**604 concrete obligations** across the routed set — each traceable to a section of its source
standard. This is what "compliant" is measured against.

### 8 · `pipeline_standards.check` — the compliance gate

```
ok (compliant)   False
bound_standards  25
obligations      604
blocking (1):
    ✗ standards drift · pinned 0828bd8de09977166cb30172d26fdcb1113bd0fa
      but corpus is at 5a5b5d0 · gates may have moved
next_suggested   ['pipeline_standards.pin', 'pipeline_standards.route']
```

The gate is honest about its own reach. It does **not** claim to have scored 604 prose
obligations with a regex — those are the agent's to adjudicate against the code. What it
*mechanically* proves, it reports: the pinned SHA (`0828bd8`) no longer matches the corpus
(`5a5b5d0`), so the very gates being enforced may have shifted underfoot. That is a real,
blocking, versioning finding — and it points at the fix.

---

## The adjudication — completing the task

The agent now holds everything it needs to close the loop:

- **The finding is real.** `pipeline.yaml` pins standards at `0828bd8`; the checked-out corpus
  is at `5a5b5d0`. Enforcing a checklist while pinned to a different commit is enforcing gates
  you can't see.
- **The fix is one reviewable line.** `pipeline_standards.pin` writes the resolved SHA into
  `pipeline.yaml` — a surgical, comment-preserving edit. Demonstrated here on a **copy**, so
  the real file is untouched:

  ```json
  // pipeline_standards.pin  (run with cwd = a throwaway copy of the repo)
  { "file": "pipeline.yaml",
    "previous": "0828bd8de09977166cb30172d26fdcb1113bd0fa",
    "pin":      "5a5b5d0f35dfd75f040215bc1b7a73b57236fbf8" }
  ```
  ```diff
  -  pin: 0828bd8de09977166cb30172d26fdcb1113bd0fa
  +  pin: 5a5b5d0f35dfd75f040215bc1b7a73b57236fbf8
  ```
  Everything else in the file — every comment, every key, the ordering — is byte-for-byte
  identical. A serde round-trip would have eaten the comments; the pin writer edits one line.

- **But pinning is a decision, not a reflex.** Moving the gate the whole project is measured
  against is a deliberate, human-reviewed act — the tool proposes it (`next_suggested`), the
  agent surfaces it, and a person accepts it. Pipeline never re-pins unattended. So this use
  case *stops here*: it has taken the task from "fresh agent, no context" to "one specific,
  understood, one-line decision awaiting review." That is exactly the hand-off Pipeline is
  built to produce.

---

## Why this loop is the whole thesis

| Pipeline principle | Where it shows up above |
|---|---|
| **Stateless agent, Pipeline owns context** | `handover` — real state or an honest absence, never a fabrication |
| **Standards injected, not memorised** | `brief`/`show`/`checklist` — the ruling rules arrive as context, keyed to this project |
| **Any agent, cloud or local** | a local model needs zero knowledge of the corpus; it calls `pipeline_standards.brief` |
| **Governance is mechanical *and* adjudicated** | `check` reports only what it can prove and hands the prose obligations to the agent |
| **Deterministic · offline · versioned** | resolves from a SHA-pinned local corpus; the same call yields the same ruling set |

---

## Notes

- **Transport.** This walkthrough drives the **stdio** transport (`pipeline mcp --transport
  stdio`), which a local Claude Code / Ollama / LM Studio agent spawns directly. The identical
  envelope works over the **HTTP** transport at `/mcp` (e.g. the live `pipe.casava.space`
  deployment) — the only difference is a `Bearer` token and, in the default `read_only` mode,
  the destructive actions being gated. Every call in this loop is read-only, so it runs
  unchanged over either.
- **Offline by design.** `brief`/`route`/`show`/`checklist`/`check` never touch the network —
  only `fetch`/`update` do, and `update` refuses to modify a user-owned clone.
- **Reproduce or extend.** `scripts/usecase-standards-loop.sh` is the exact driver; add or
  reorder the JSON-RPC lines in its `REQUESTS` block to script your own scenario.
