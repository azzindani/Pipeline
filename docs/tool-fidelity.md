# Tool fidelity

> Every action declares how much of its own name it delivers. The declaration is
> checked by `crates/pipeline-mcp/tests/registry_conformance.rs`, not trusted.

## Why this exists

A live dogfooding audit of all 19 tools / 175 actions found ~60 returning
`ok: true` while doing nothing, doing the wrong thing, or emitting fabricated
data. Three examples, all verified against live infrastructure:

| Action | Reported | Actually did |
|---|---|---|
| `security.secret_scan` | `ok:true, error:null` | ran trufflehog **and found secrets** — `--fail` was never passed, so the scanner exits 0 on findings |
| `security.compliance_check(framework="hipaa")` | `score_percent: 100` | five file-existence checks · `framework` was read and never branched on |
| `memory.pattern_report` | "no failures yet · this project has been green" | a **database read error**, `unwrap_or_default()`'d into an empty list |

The governing rule:

> **A tool that refuses is more useful than a tool that lies.**

An agent cannot verify a result it did not compute. The success flag is the only
signal it has, so that flag must be load-bearing — and "I could not determine
this" must never collapse into "I determined it is fine."

## The three levels

| Fidelity | Meaning | Contract |
|---|---|---|
| **Real** | Does the work its name claims — spawns the process, writes the file, queries the DB — and reports the true outcome *including failure* | `ok:true` means the work happened |
| **Scaffold** | Writes a template, skeleton, or fixture · ✗ reads · ✗ analyzes your project | Useful as a starting point, worthless as a finding. Badged `[scaffold]` in `tools/list` |
| **Planned** | Not implemented | Refused in `dispatch` **before the handler runs**. Badged `[planned]`, and the summary states what is missing |

Current split: **148 Real · 20 Scaffold · 7 Planned**. The counts are pinned by
`registry::tests::the_fidelity_split_is_recorded` — a tripwire, not a target. If
it moves, this table needs the same edit.

## Why `Planned` is refused centrally

The refusal lives in `dispatch::call_tool`, not in 35 handlers. Three reasons:

- **Fixing handlers one at a time only refills.** Nothing stopped the next one
  being written. A central guard makes the marker self-enforcing: flipping an
  action to `Real` is the only thing that lets its handler execute, so the
  marker cannot drift from behaviour.
- **Some handlers are worse than useless when reached.** `e2e.record` spawned an
  interactive tool with no timeout and blocked forever — the conformance test
  hung on it before the guard existed. `docs.publish` swallowed a spawn failure
  into `ok:true`.
- **It is testable in one place.** `a_planned_action_never_reports_success`
  covers all 35 at once, in 0.01s, without spawning anything.

## Arguments

Before this, `args` was published as `{"type":"object","additionalProperties":true}`.
Nothing declared any argument, so "accepted, echoed back, silently dropped" was
invisible to the caller **and** untestable. That was the enabling condition for
the whole defect class — `deps_install` ignored `packages`, `compliance_check`
ignored `framework`, `repo.compare` ignored `axis`.

Now each action declares an [`ArgSet`]:

- `Of(&[...])` — fully declared. Published as JSON Schema with
  `additionalProperties: false`, and **re-checked server-side** in `dispatch`,
  because MCP clients are not obliged to enforce a published schema.
- `None` — verified to take no arguments.
- `Unspecified` — not yet audited. Stays permissive.

`None` and `Unspecified` are deliberately distinct. Collapsing them would make
the registry commit the same sin it exists to prevent: asserting an action takes
no arguments when nobody checked is a fabrication.

A rejected argument names itself and suggests the intended one:

```
pipeline_env.deps_install: unknown argument 'pacakges' · did you mean 'packages'?
  · accepted: stack · packages · dev · features · manifest
```

## What conformance enforces

`crates/pipeline-mcp/tests/registry_conformance.rs`:

| Invariant | Defect it prevents |
|---|---|
| every declared arg appears in its handler source | `deps_install` accepted `packages`, never read it, reported success |
| `Planned` never returns `ok:true` | `re_report` overwrote status to "complete" and returned empty findings |
| every registry action dispatches | a listed-but-unmatched action reads as a version mismatch |
| every dispatched action is in the registry | an undeclared action carries no fidelity and no schema — invisible to every other check |
| specified args publish a closed schema | an unknown key would otherwise pass silently |

The arg check is grep-level on purpose. It is the cheapest test that would have
caught `deps_install`, `compliance_check`, `repo.compare`, and `metrics_setup`
on the day each was written.

## Adding an action

1. Add an `ActionSpec` to `registry.rs`. Start at `Planned` — it is honest, and
   the guard will refuse calls until the work exists.
2. Write the handler.
3. Flip to `Real` (or `Scaffold` if it writes a template without reading the
   project). Conformance will now check the declared args are actually read.

Declaring `Real` before the handler works fails the suite, which is the intent.

## Known gaps

Seven actions remain `Planned`. Each was examined and refused deliberately —
these are honest boundaries, not backlog.

| Action | Why it stays refused |
|---|---|
| `deploy.canary` · `deploy.blue_green` | Need a traffic router Pipeline neither owns nor can discover. Compose DNS round-robin only splits across replicas of the *same* service, so it cannot split between two *versions*; blue/green additionally requires reading live state from a router whose identity is unknown. Implementing either means inventing a topology and assuming the user has it |
| `e2e.record` | `playwright codegen` is headed and interactive. An interactive recorder needs a display and a human at it — there is no honest synchronous MCP shape for it |
| `env.devcontainer_open` | The VS Code URI authority is `dev-container+<hex-encoded-json>`, and that JSON is an internal structure of the Dev Containers extension whose fields change across releases. A guessed payload would resolve on one machine and fail silently everywhere else |
| `repo.port` | Language translation is an agent task, not a tool task. A `Real` marker on an action *named* `port` would invite an agent to believe code was translated. Use `re_analyze` + `re_modernize` for a plan grounded in real modules |
| `repo.re_reconstruct` | Introspects no target. OpenAPI needs observed traffic, schema needs a live DB connection, Dockerfile needs image-layer inspection — none are wired up |
| `simulate.journey_simulate` | Executing a stored journey requires driving a real target; the current shape returns arithmetic over step counts |

`re_analyze` is `Real` for `type=codebase` and **refuses `binary` / `service` /
`infra` by name**, listing what each would require. Decompilation, live traffic
capture, and cloud-state introspection are each a project, not a function.

## Boundaries that are not gaps

Some `Real` actions are deliberately narrower than their name. Each says so in
its own summary, which is the contract:

- `docs.publish` builds the site locally and **pushes nowhere**. `ok:true` means
  "built at this path", and the payload carries `published: false`.
- `data.etl_create` is `Scaffold`: it renders exactly the caller's spec, but no
  runner consumes the file, so Pipeline executes nothing.
- `security.compliance_check` assesses `standards` only and emits **no score at
  all** — prose obligations are unscored by construction, and a number would be
  the same fabrication in a new shape. `hipaa` · `pci_dss` · `gdpr` · `iso27001`
  · `soc2` · `owasp` are refused by name with a reason.
- `data.anonymize`'s `hash` strategy is pseudonymisation, not anonymisation —
  deterministic so joins survive, which leaves low-entropy columns
  re-identifiable. `redact` / `null` are the honest choices there.
- `repo.apply_standards` returns binding standards and their obligations and
  **scores nothing**, replacing a `score_percent` that was four file-existence
  booleans labelled "compliance".
