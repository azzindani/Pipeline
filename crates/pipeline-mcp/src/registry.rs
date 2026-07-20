//! Static tool registry · one [`ActionSpec`] per action.
//!
//! Each entry declares what the action takes and **how much of its own name it
//! delivers** ([`Fidelity`]). Both are agent-facing: the action list is inlined
//! into each tool description so an agent picks correctly without a second call.
//!
//! ! Fidelity is a claim the conformance suite checks, ✗ a comment. A live audit
//! found ~60 actions returning `ok: true` while fabricating, so the registry now
//! carries the verdict and `tests/registry_conformance.rs` enforces it:
//! - `Planned` → refused in `dispatch` before the handler runs · ✗ fabricate
//! - declared arg → must actually be read by the handler
//! - specified args → unknown keys rejected at the transport boundary
//!
//! Adding an action without a spec entry fails the build; adding a spec whose
//! args the handler ignores fails the test.

use crate::spec::{
    ActionSpec,
    ArgSet::{None as NoArgs, Of, Unspecified},
    ArgType::{Bool, Int, List, Obj, Str},
    opt, req,
};
use crate::tools::ToolName;

#[derive(Debug, Clone, Copy)]
pub struct ToolDescriptor {
    pub name: ToolName,
    pub summary: &'static str,
    pub actions: &'static [ActionSpec],
}

impl ToolDescriptor {
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    /// Look up one action by name.
    pub fn action(&self, name: &str) -> Option<&'static ActionSpec> {
        self.actions.iter().find(|a| a.name == name)
    }

    /// Agent-facing description · summary plus one line per action, fidelity
    /// badge included so a scaffold is never mistaken for analysis.
    pub fn describe(&self) -> String {
        let lines: Vec<String> = self.actions.iter().map(ActionSpec::describe).collect();
        format!("{}\n\nActions:\n- {}", self.summary, lines.join("\n- "))
    }

    /// Published JSON Schema for this tool's `tools/call` input.
    ///
    /// `action` is an enum; per-action argument shapes ride in an `allOf` of
    /// `if`/`then` clauses, which is how JSON Schema expresses "the valid args
    /// depend on the discriminant". Actions with an unspecified arg set emit no
    /// clause and stay permissive.
    ///
    /// ! Publication is advisory — clients are not obliged to enforce it, which
    /// is why [`Self::validate`] re-checks server-side. This exists so an agent
    /// can *see* the arguments before calling.
    pub fn input_schema(&self) -> serde_json::Map<String, serde_json::Value> {
        use serde_json::{Map, Value, json};

        let names: Vec<&str> = self.actions.iter().map(|a| a.name).collect();
        let mut props = Map::new();
        props.insert("action".into(), json!({"type": "string", "enum": names}));
        props.insert(
            "args".into(),
            json!({"type": "object", "additionalProperties": true}),
        );

        let clauses: Vec<Value> = self
            .actions
            .iter()
            .filter(|a| a.args.specified())
            .map(|a| {
                json!({
                    "if":   {"properties": {"action": {"const": a.name}}, "required": ["action"]},
                    "then": {"properties": {"args": a.args_schema()}},
                })
            })
            .collect();

        let mut schema = Map::new();
        schema.insert("type".into(), json!("object"));
        schema.insert("properties".into(), Value::Object(props));
        schema.insert("required".into(), json!(["action"]));
        schema.insert("additionalProperties".into(), Value::Bool(false));
        if !clauses.is_empty() {
            schema.insert("allOf".into(), Value::Array(clauses));
        }
        schema
    }

    /// Server-side argument check · the enforcement that actually binds.
    ///
    /// ! An unknown argument is an **error**, ✗ ignored. Silently dropping
    /// `packages` is precisely how `deps_install` reported success while
    /// installing nothing — a typo must be visible immediately, at the
    /// boundary, not inferred later from a wrong result.
    ///
    /// Unspecified arg sets are skipped: unaudited actions keep working rather
    /// than hard-failing on arguments the registry has not caught up with.
    ///
    /// # Errors
    /// Unknown action · unknown argument (with a did-you-mean) · missing
    /// required argument · wrong JSON type.
    pub fn validate(&self, action: &str, args: &serde_json::Value) -> Result<(), String> {
        let Some(spec) = self.action(action) else {
            let known: Vec<&str> = self.actions.iter().map(|a| a.name).collect();
            return Err(format!(
                "unknown action '{}.{action}' · known: {}",
                self.name.as_str(),
                known.join(" · ")
            ));
        };
        if !spec.args.specified() {
            return Ok(());
        }
        let declared = spec.args.args();
        let obj = match args {
            serde_json::Value::Object(m) => m,
            serde_json::Value::Null => return missing_required(self.name.as_str(), spec, &[]),
            other => {
                return Err(format!(
                    "{}.{action}: 'args' must be an object, got {}",
                    self.name.as_str(),
                    kind_of(other)
                ));
            }
        };

        for key in obj.keys() {
            if declared.iter().any(|d| d.name == *key) {
                continue;
            }
            let names: Vec<&str> = declared.iter().map(|d| d.name).collect();
            let hint = nearest(key, &names)
                .map(|n| format!(" · did you mean '{n}'?"))
                .unwrap_or_default();
            return Err(format!(
                "{}.{action}: unknown argument '{key}'{hint} · accepted: {}",
                self.name.as_str(),
                if names.is_empty() {
                    "(none)".to_owned()
                } else {
                    names.join(" · ")
                }
            ));
        }

        for d in declared {
            match obj.get(d.name) {
                None | Some(serde_json::Value::Null) => {}
                Some(v) if type_matches(d.ty, v) => {}
                Some(v) => {
                    return Err(format!(
                        "{}.{action}: '{}' must be {}, got {} · {}",
                        self.name.as_str(),
                        d.name,
                        d.ty.as_json_type(),
                        kind_of(v),
                        d.help
                    ));
                }
            }
        }

        let present: Vec<&str> = obj.keys().map(String::as_str).collect();
        missing_required(self.name.as_str(), spec, &present)
    }
}

fn missing_required(tool: &str, spec: &ActionSpec, present: &[&str]) -> Result<(), String> {
    let missing: Vec<&str> = spec
        .args
        .args()
        .iter()
        .filter(|d| d.required && !present.contains(&d.name))
        .map(|d| d.name)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let detail: Vec<String> = spec
        .args
        .args()
        .iter()
        .filter(|d| missing.contains(&d.name))
        .map(|d| format!("{} ({})", d.name, d.help))
        .collect();
    Err(format!(
        "{tool}.{}: missing required argument{} · {}",
        spec.name,
        if missing.len() == 1 { "" } else { "s" },
        detail.join(" · ")
    ))
}

fn type_matches(ty: crate::spec::ArgType, v: &serde_json::Value) -> bool {
    use crate::spec::ArgType;
    match ty {
        // Accept a JSON string for Int/Bool · agents routinely quote scalars and
        // refusing that is pedantry, ✗ safety. Confusion that changes behaviour
        // (list vs object) is what this check is for.
        ArgType::Str => v.is_string(),
        ArgType::Int => v.is_number() || v.as_str().is_some_and(|s| s.parse::<i64>().is_ok()),
        ArgType::Bool => v.is_boolean() || v.as_str().is_some_and(|s| s.parse::<bool>().is_ok()),
        ArgType::List => v.is_array(),
        ArgType::Obj => v.is_object(),
    }
}

fn kind_of(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Closest candidate by edit distance · only when it is clearly the intended
/// word, so the hint never sends an agent after an unrelated argument.
fn nearest<'a>(typo: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let budget = match typo.len() {
        0..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };
    candidates
        .iter()
        .map(|c| (edit_distance(typo, c), *c))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The canonical surface · 19 tools.
///
/// `static` rather than a builder fn so every `&[...]` inside is a constant
/// expression with `'static` lifetime · const-fn calls do not rvalue-promote.
static REGISTRY: [ToolDescriptor; 19] = [
    ToolDescriptor {
        name: ToolName::Session,
        summary: "Session lock · context · handover.",
        actions: &[
            ActionSpec::real(
                "lock",
                "Acquire the exclusive project lock · SQLite-backed, survives process restart · contention → ok:false.",
                Of(&[
                    opt(
                        "agent_id",
                        Str,
                        "caller identity · defaults to the registered agent",
                    ),
                    opt(
                        "goal",
                        Str,
                        "what this session intends to do · stored on the lock",
                    ),
                ]),
            ),
            ActionSpec::real(
                "unlock",
                "Release the lock held by this connection · ! gates on in-process state, ✗ releases a lock taken elsewhere.",
                NoArgs,
            ),
            ActionSpec::real(
                "steal",
                "Force-release another agent's lock · ! ✗ checks a lock existed; orphans the victim's session row.",
                Of(&[opt(
                    "project_id",
                    Str,
                    "project to unlock · defaults to pipeline.yaml in cwd",
                )]),
            ),
            ActionSpec::planned(
                "start",
                "Open a session without the lock · ✗ implemented: persists no session row, returns no session_id, handover reports active_session:null. Use lock.",
            ),
            ActionSpec::real(
                "checkpoint",
                "Persist a free-form progress note to memory under scope 'checkpoint'.",
                Of(&[opt("note", Str, "note body · empty when omitted")]),
            ),
            ActionSpec::real(
                "end",
                "Close a session by id · writes outcome + summary.",
                Of(&[
                    req("session_id", Str, "id returned by session.lock"),
                    opt("outcome", Str, "ok | failed | abandoned · defaults to ok"),
                    opt("summary", Str, "what the session accomplished"),
                ]),
            ),
            ActionSpec::real(
                "handover",
                "Replay stored project state: PRD · features · milestones · last run · recent failures.",
                NoArgs,
            ),
            ActionSpec::real(
                "context",
                "Handover packet plus one memory scope read.",
                Of(&[
                    opt(
                        "scope",
                        Str,
                        "plan | feature | milestone | decision | risk | research_note | checkpoint",
                    ),
                    opt("limit", Int, "max scope entries · defaults to 20"),
                ]),
            ),
            ActionSpec::real(
                "file_context",
                "Substring-match a path against the last 20 runs' stdout/stderr · ✗ a file→test graph.",
                Of(&[req("path", Str, "path as it would appear in run output")]),
            ),
            ActionSpec::planned(
                "task_context",
                "Find prior work related to a task · ✗ implemented: matches the whole description as one substring, so it returns empty and reads as 'no prior work'.",
            ),
            ActionSpec::real(
                "agent_register",
                "Bind an agent identity to this MCP connection · later session ops inherit it.",
                Of(&[req("agent_id", Str, "stable caller identity")]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Plan,
        summary: "Idea intake · feasibility · PRD · features · milestones · ADRs · risks.",
        actions: &[
            ActionSpec::real(
                "idea_capture",
                "Persist an idea · ! single fixed key: a second capture overwrites the first.",
                Of(&[req("text", Str, "idea body")]),
            ),
            ActionSpec::real(
                "link_ingest",
                "Fetch URLs, extract title + text, persist as research notes · ! 4xx/5xx and DNS failures still count as ingested.",
                Of(&[req("urls", List, "URLs to fetch · strings")]),
            ),
            ActionSpec::real(
                "research_notes_list",
                "List stored research notes · id · url · kind · title · ts.",
                NoArgs,
            ),
            ActionSpec::real(
                "research_notes_show",
                "Read one research note in full, excerpt included.",
                Of(&[req("id", Str, "note id from research_notes_list")]),
            ),
            ActionSpec::planned(
                "feasibility",
                "Assess feasibility · ✗ implemented: effort in weeks is keyword-count arithmetic with no disclosed model, verdict 'yes' means only 'a digest file existed', links accepted and never fetched.",
            ),
            ActionSpec::real(
                "create",
                "Open a plan · records project type, seeds an empty PRD when absent.",
                Of(&[opt("type", Str, "project type label · defaults to custom")]),
            ),
            ActionSpec::real(
                "prd_write",
                "Write the PRD · overwrites any existing document.",
                Of(&[
                    opt("goals", List, "goal statements"),
                    opt("non_goals", List, "explicit exclusions"),
                    opt("users", List, "user or persona names"),
                    opt("summary", Str, "one-paragraph product summary"),
                ]),
            ),
            ActionSpec::real("prd_read", "Read the stored PRD.", NoArgs),
            ActionSpec::real(
                "prd_update",
                "Shallow-merge keys into the PRD · ! every top-level key is merged unvalidated.",
                Unspecified,
            ),
            ActionSpec::real(
                "features_add",
                "Add a feature · status todo, uuid assigned.",
                Of(&[
                    req("name", Str, "feature name"),
                    opt("description", Str, "what the feature does"),
                    opt("ac", List, "acceptance criteria · strings"),
                ]),
            ),
            ActionSpec::real(
                "features_list",
                "List features with counts by status.",
                NoArgs,
            ),
            ActionSpec::real(
                "features_update",
                "Merge a patch into one feature · id is immutable.",
                Of(&[
                    req("id", Str, "feature id"),
                    opt(
                        "patch",
                        Obj,
                        "fields to merge · omitted → remaining args are the patch",
                    ),
                ]),
            ),
            ActionSpec::real(
                "features_track",
                "Set a feature's status.",
                Of(&[
                    req("id", Str, "feature id"),
                    req("status", Str, "todo | in_progress | blocked | done"),
                ]),
            ),
            ActionSpec::real(
                "acceptance_define",
                "Replace a feature's acceptance criteria.",
                Of(&[
                    req("feature_id", Str, "feature id"),
                    opt(
                        "criteria",
                        List,
                        "acceptance criteria · strings · omitted → cleared",
                    ),
                ]),
            ),
            ActionSpec::real(
                "milestone_create",
                "Create a milestone keyed by name · status planned.",
                Of(&[
                    req("name", Str, "milestone name · also its key"),
                    opt("exit_criteria", List, "conditions that close the milestone"),
                    opt("feature_ids", List, "feature ids counted for progress"),
                ]),
            ),
            ActionSpec::real(
                "milestone_progress",
                "Re-read each member feature from storage and compute done/total percent.",
                Of(&[req("name", Str, "milestone name")]),
            ),
            ActionSpec::real(
                "progress",
                "Aggregate counts: features by status · milestones · decisions · risks.",
                NoArgs,
            ),
            ActionSpec::real(
                "decision_log",
                "Record an ADR: context · decision · alternatives.",
                Of(&[
                    req("title", Str, "decision title"),
                    opt("context", Str, "forces in play"),
                    opt("decision", Str, "what was decided"),
                    opt("alternatives", List, "options rejected"),
                ]),
            ),
            ActionSpec::real(
                "risk_add",
                "Record a risk with likelihood · impact · mitigation.",
                Of(&[
                    req("title", Str, "risk title"),
                    opt(
                        "likelihood",
                        Str,
                        "low | medium | high · defaults to medium",
                    ),
                    opt("impact", Str, "low | medium | high · defaults to medium"),
                    opt("mitigation", Str, "planned mitigation"),
                ]),
            ),
            ActionSpec::real("risk_list", "List recorded risks.", NoArgs),
            ActionSpec::planned(
                "estimate",
                "Estimate effort from features · ✗ implemented: base = ac_count × 4h annihilates complexity at zero ACs, and features_add never stores complexity, so every feature returns 2.0h.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Standards,
        summary: "Standards brief · route · show · checklist · pin · check. \
                  Sourced from the external Standards repo, pinned by commit SHA.",
        actions: &[
            ActionSpec::real(
                "brief",
                "L0 packet · routed set + drift/pin health · inject at session start.",
                NoArgs,
            ),
            ActionSpec::real(
                "list",
                "Full catalog · every standard in the corpus, bound flag per entry.",
                NoArgs,
            ),
            ActionSpec::real(
                "show",
                "L1 · one standard in full, read from the resolved corpus.",
                Of(&[
                    opt(
                        "id",
                        Str,
                        "standard id · rust | testing/pressure · required unless category given",
                    ),
                    opt("category", Str, "alias for id"),
                ]),
            ),
            ActionSpec::real(
                "checklist",
                "L2 · every checklist item the routed set imposes.",
                NoArgs,
            ),
            ActionSpec::real(
                "route",
                "Why each standard binds · runtime · project_type · surfaces · provenance.",
                NoArgs,
            ),
            ActionSpec::real(
                "fetch",
                "Populate the shared cache from upstream · the only networked action.",
                NoArgs,
            ),
            ActionSpec::real(
                "update",
                "Move cache to upstream HEAD · report commits touching bound standards.",
                NoArgs,
            ),
            ActionSpec::real(
                "pin",
                "Write the resolved SHA into pipeline.yaml · refuses a result that would not parse.",
                NoArgs,
            ),
            ActionSpec::real(
                "check",
                "Compliance gate · emits obligations + blocking drift/pin/route signals · ✗ a score it did not compute.",
                NoArgs,
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Project,
        summary: "Project init · scaffold · templates.",
        actions: &[
            ActionSpec::real(
                "init",
                "Scaffold a project from a template · reports files_written + files_skipped · refuses a non-empty target unless adopt.",
                Of(&[
                    req(
                        "name",
                        Str,
                        "project name · also the directory created under parent",
                    ),
                    opt(
                        "type",
                        Str,
                        "template name · defaults to custom · see template_list",
                    ),
                    opt("template", Str, "alias for type"),
                    opt("stack", Str, "python-uv | bun | node | rust | go"),
                    opt(
                        "adopt",
                        Bool,
                        "bring an existing repo under Pipeline · writes only missing files",
                    ),
                    opt(
                        "parent",
                        Str,
                        "directory to create the project in · defaults to cwd",
                    ),
                ]),
            ),
            ActionSpec::real(
                "scaffold",
                "Add a component · kind=crate writes manifest + source and registers the member in the root Cargo.toml.",
                Of(&[
                    req(
                        "component",
                        Str,
                        "component name · file, module, or crate name",
                    ),
                    opt(
                        "kind",
                        Str,
                        "module | test | bin | crate · defaults to module",
                    ),
                    opt(
                        "bin",
                        Bool,
                        "kind=crate only · emit src/main.rs instead of src/lib.rs",
                    ),
                    opt(
                        "description",
                        Str,
                        "kind=crate only · manifest description field",
                    ),
                ]),
            ),
            ActionSpec::real(
                "template_list",
                "List the built-in templates · ! static set only, ✗ reads the template_register registry.",
                NoArgs,
            ),
            ActionSpec::planned(
                "template_register",
                "Register a user template · ✗ implemented: writes a registry file nothing reads — init cannot use it, template_list ignores it, source is never validated.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Env,
        summary: "Environment · deps · runtime · tooling · secrets.",
        actions: &[
            ActionSpec::scaffold(
                "create",
                "Write .env.example + devcontainer.json templates · content hardcoded, profile echoed but unused · skips files that exist.",
                Of(&[opt(
                    "profile",
                    Str,
                    "echoed back · ✗ effect on generated content",
                )]),
            ),
            ActionSpec::real(
                "deps_install",
                "Add packages to the manifest · empty list syncs from the lockfile.",
                Of(&[
                    req(
                        "stack",
                        Str,
                        "rust | node | ts | typescript | bun | python | python-uv | uv | go | golang",
                    ),
                    opt("packages", List, "names · empty → sync from lockfile"),
                    opt("dev", Bool, "true → dev/test dependency section"),
                    opt("features", List, "cargo features · rust only"),
                    opt(
                        "manifest",
                        Str,
                        "workspace member manifest path · rust only",
                    ),
                ]),
            ),
            ActionSpec::real(
                "deps_audit",
                "Run cargo audit in cwd · ! cargo only, ignores project stack · takes no arguments.",
                NoArgs,
            ),
            ActionSpec::real(
                "deps_update",
                "Upgrade dependencies to latest allowed versions per stack · unknown stack rejected.",
                Of(&[req(
                    "stack",
                    Str,
                    "rust | node | ts | typescript | bun | python | python-uv | uv | go | golang",
                )]),
            ),
            ActionSpec::real(
                "deps_lock",
                "Regenerate the lockfile per stack · unknown stack rejected.",
                Of(&[req(
                    "stack",
                    Str,
                    "rust | node | ts | typescript | bun | python | python-uv | uv | go | golang",
                )]),
            ),
            ActionSpec::planned(
                "runtime_provision",
                "Install a language runtime at a version · ✗ implemented: appends a .tool-versions line, installs nothing; an existing entry at another version is skipped yet ok:true still echoes the requested version.",
            ),
            ActionSpec::real(
                "tooling_install",
                "Install linter · formatter · lsp · coverage toolchain · rust mapped, every other stack errors.",
                Of(&[
                    req("kind", Str, "linter | formatter | lsp | coverage"),
                    opt("stack", Str, "rust only · anything else errors"),
                ]),
            ),
            ActionSpec::scaffold(
                "secrets_setup",
                "Write .env.example with __SET_ME__ stubs · ! truncates any existing file, no existence check.",
                Of(&[opt(
                    "keys",
                    List,
                    "variable names · default DATABASE_URL, API_KEY",
                )]),
            ),
            ActionSpec::real(
                "secrets_inject",
                "Copy the stage's env template to .env · refuses to overwrite .env.",
                Of(&[opt("stage", Str, "template suffix · default dev")]),
            ),
            ActionSpec::planned(
                "devcontainer_open",
                "Open the project in its devcontainer · ✗ implemented: passes a fabricated container URI VS Code cannot resolve (hex-encoded JSON required).",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Docker,
        summary: "Docker · compose · image.",
        actions: &[
            ActionSpec::real(
                "build",
                "Build an image from a Dockerfile · propagates docker build exit code.",
                Of(&[
                    req("tag", Str, "image tag · name:tag"),
                    opt("context", Str, "build context dir · default cwd"),
                    opt(
                        "dockerfile",
                        Str,
                        "Dockerfile path · default <context>/Dockerfile",
                    ),
                    opt("target", Str, "multi-stage target stage name"),
                ]),
            ),
            ActionSpec::real(
                "run",
                "Run a container to completion · propagates the container's exit code.",
                Of(&[
                    req("image", Str, "image ref"),
                    opt("cmd", List, "argv strings · empty → image CMD"),
                    opt("env", Obj, "env vars · string values only"),
                ]),
            ),
            ActionSpec::real(
                "exec",
                "Exec argv in a running container · propagates exit code.",
                Of(&[
                    req("container", Str, "container name | id"),
                    req("cmd", List, "argv strings · non-empty"),
                ]),
            ),
            ActionSpec::real(
                "logs",
                "Fetch container logs · one shot; ✗ streams.",
                Of(&[
                    req("container", Str, "container name | id"),
                    opt("tail", Int, "last N lines · omit → all"),
                ]),
            ),
            ActionSpec::real(
                "inspect",
                "Return docker inspect JSON for a container | image.",
                Of(&[req("name", Str, "container | image name | id")]),
            ),
            ActionSpec::real(
                "rm",
                "Remove a container · propagates exit code.",
                Of(&[
                    req("name", Str, "container name | id"),
                    opt(
                        "force",
                        Bool,
                        "kill running container first · default false",
                    ),
                ]),
            ),
            ActionSpec::real(
                "compose_up",
                "Start compose services · propagates exit code.",
                Of(&[
                    opt(
                        "file",
                        Str,
                        "compose file, relative to cwd · default docker-compose.yml",
                    ),
                    opt("compose_file", Str, "alias for file"),
                    opt("services", List, "service names · empty → all"),
                ]),
            ),
            ActionSpec::real(
                "compose_down",
                "Stop compose services · propagates exit code.",
                Of(&[
                    opt(
                        "file",
                        Str,
                        "compose file, relative to cwd · default docker-compose.yml",
                    ),
                    opt("compose_file", Str, "alias for file"),
                ]),
            ),
            ActionSpec::real(
                "compose_ps",
                "List compose service state · propagates exit code.",
                Of(&[
                    opt(
                        "file",
                        Str,
                        "compose file, relative to cwd · default docker-compose.yml",
                    ),
                    opt("compose_file", Str, "alias for file"),
                ]),
            ),
            ActionSpec::real(
                "compose_logs",
                "Fetch compose logs · one shot; ✗ streams.",
                Of(&[
                    opt(
                        "file",
                        Str,
                        "compose file, relative to cwd · default docker-compose.yml",
                    ),
                    opt("compose_file", Str, "alias for file"),
                    opt("service", Str, "single service · omit → all"),
                ]),
            ),
            ActionSpec::real(
                "image_scan",
                "Scan an image with Trivy · findings at or above severity fail the call.",
                Of(&[
                    req("image", Str, "image ref"),
                    opt("severity", Str, "comma list · default CRITICAL,HIGH"),
                ]),
            ),
            ActionSpec::real(
                "image_promote",
                "Retag an image into a registry and push · aborts if the tag step fails.",
                Of(&[
                    req("image", Str, "source image ref"),
                    req("registry", Str, "destination registry · ghcr.io/owner"),
                    opt("tag", Str, "destination tag · default latest"),
                ]),
            ),
            ActionSpec::real(
                "image_push",
                "Push an image to its registry · propagates exit code.",
                Of(&[req("image", Str, "fully qualified image ref")]),
            ),
            ActionSpec::real(
                "image_pull",
                "Pull an image · propagates exit code.",
                Of(&[req("image", Str, "image ref")]),
            ),
            ActionSpec::scaffold(
                "dockerfile_generate",
                "Write a multi-stage Dockerfile template for a stack · refuses to overwrite · ✗ reads your project.",
                Of(&[opt(
                    "stack",
                    Str,
                    "rust | node | ts | typescript | bun | python | python-uv | uv | go | golang · default rust",
                )]),
            ),
            ActionSpec::real(
                "dockerfile_lint",
                "Lint a Dockerfile with hadolint · findings fail the call.",
                Of(&[opt(
                    "path",
                    Str,
                    "Dockerfile path relative to cwd · default Dockerfile",
                )]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Run,
        summary: "Stage execution · preflight · commit · push.",
        actions: &[
            ActionSpec::real(
                "stage",
                "Execute a stage profile · spawns fmt · clippy · test · a real failure returns ok:false.",
                Of(&[
                    opt("profile", Str, "fast | full | preflight · default fast"),
                    opt("name", Str, "alias for profile"),
                ]),
            ),
            ActionSpec::real(
                "status",
                "Report handover state · active session · last status · recent runs.",
                NoArgs,
            ),
            ActionSpec::real(
                "logs",
                "Query stored stdout/stderr of past runs from memory.",
                Of(&[
                    opt("stage", Str, "filter by stage name · omit → all stages"),
                    opt("tail", Int, "runs to fetch · default 20"),
                ]),
            ),
            ActionSpec::real(
                "fix_suggestion",
                "Surface prior fixes that worked for the most recent failure.",
                Of(&[opt(
                    "stage",
                    Str,
                    "restrict to failures on this stage · omit → newest failure",
                )]),
            ),
            ActionSpec::real(
                "preflight",
                "Run the preflight profile · every stage must genuinely execute; a skip fails the gate.",
                NoArgs,
            ),
            ActionSpec::real(
                "fmt",
                "Apply formatting and the lint fixes the toolchain can make automatically.",
                Of(&[opt("check", Bool, "report without writing · default false")]),
            ),
            ActionSpec::real(
                "commit",
                "git add -A + git commit · refuses on a red gate unless force.",
                Of(&[
                    req("message", Str, "commit message"),
                    opt(
                        "force",
                        Bool,
                        "commit despite a failing last run · default false",
                    ),
                ]),
            ),
            ActionSpec::real(
                "push",
                "git push -u · refuses on a red gate unless force.",
                Of(&[
                    opt("remote", Str, "default origin"),
                    opt("branch", Str, "default current HEAD branch"),
                    opt(
                        "force",
                        Bool,
                        "push despite a failing last run · default false",
                    ),
                ]),
            ),
            ActionSpec::scaffold(
                "explain",
                "Print stage prose · ✗ reads your pipeline.yaml · hardcoded text.",
                Of(&[opt(
                    "stage",
                    Str,
                    "static | unit | container | integration | security",
                )]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Test,
        summary: "Test generate · run · coverage · mutation · property.",
        actions: &[
            ActionSpec::scaffold(
                "generate",
                "Write a test file · body is a trivial assertion · ✗ reads your code.",
                Of(&[
                    req("target", Str, "path under tests/ or a module name"),
                    opt("kind", Str, "unit | integration · default unit"),
                ]),
            ),
            ActionSpec::real(
                "run",
                "Spawn cargo test · filter passed through · exit code is the verdict.",
                Of(&[
                    opt("filter", Str, "test name substring passed to cargo test"),
                    opt(
                        "suite",
                        Str,
                        "workspace → --workspace · anything else → current crate",
                    ),
                ]),
            ),
            ActionSpec::real(
                "coverage",
                "Run cargo llvm-cov · fails below threshold · defaults to gates.coverage from pipeline.yaml.",
                Of(&[opt(
                    "threshold",
                    Int,
                    "minimum line coverage percent · omit → gates.coverage",
                )]),
            ),
            ActionSpec::real(
                "mutation_run",
                "Spawn the stack's mutation runner · cargo mutants | mutmut | stryker.",
                Of(&[opt(
                    "stack",
                    Str,
                    "rust | python | python-uv | node | ts | typescript · default rust",
                )]),
            ),
            ActionSpec::scaffold(
                "property_generate",
                "Write a proptest scaffold · ✗ derives properties from your code.",
                Of(&[req("target", Str, "function name · drives file + fn name")]),
            ),
            ActionSpec::scaffold(
                "validation_create",
                "Write a validation shell script · body is a stub that always exits 0.",
                Of(&[opt("spec", Str, "spec name · default contract")]),
            ),
            ActionSpec::real(
                "ac_to_test",
                "Read the stored feature · emit one ignored test per acceptance criterion.",
                Of(&[
                    opt(
                        "feature_id",
                        Str,
                        "feature key in memory · required unless id given",
                    ),
                    opt("id", Str, "alias for feature_id"),
                ]),
            ),
            ActionSpec::planned(
                "flake_detect",
                "Detect flaky tests · ✗ implemented: reruns nothing, discards every argument · pass+fail in history is not flakiness, a fixed regression scores the same.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::E2e,
        summary: "Playwright · browser control · visual · a11y.",
        actions: &[
            ActionSpec::real(
                "run",
                "Run the Playwright suite in a container against cwd · exit status propagated.",
                Of(&[opt(
                    "suite",
                    Str,
                    "test file or path filter · empty → whole suite",
                )]),
            ),
            ActionSpec::planned(
                "record",
                "Record a browser session into test code · ✗ usable: codegen is headed and interactive with no display and no timeout, so the call blocks indefinitely.",
            ),
            ActionSpec::planned(
                "browser_launch",
                "Launch a browser at a URL · ✗ implemented: the container sleeps, no browser starts, url only labels the log line.",
            ),
            ActionSpec::real(
                "browser_close",
                "Remove the browser container · takes no arguments.",
                NoArgs,
            ),
            ActionSpec::real(
                "trace",
                "Run one test with Playwright tracing on · trace written to the project.",
                Of(&[req("test", Str, "test name · matched with -g")]),
            ),
            ActionSpec::real(
                "screenshot",
                "Launch chromium, navigate, write a full-page PNG · returns the output path.",
                Of(&[req("url", Str, "page to capture")]),
            ),
            ActionSpec::planned(
                "visual_regression",
                "Compare screenshots against baselines · ✗ implemented: writes the baselines it should compare against, so it cannot fail on a fresh project.",
            ),
            ActionSpec::planned(
                "a11y_check",
                "Audit a page for accessibility violations · ✗ implemented: requires an axe dependency absent from repo and base image · can only exit MODULE_NOT_FOUND.",
            ),
            ActionSpec::planned(
                "against_env",
                "Run the suite against a deployed environment · ✗ implemented: sets the base URL to an environment name where a URL belongs.",
            ),
            ActionSpec::real(
                "devtools_eval",
                "Navigate chromium to a URL and evaluate JS in page context · result returned as JSON.",
                Of(&[
                    req("url", Str, "page to open"),
                    req(
                        "js",
                        Str,
                        "statement body evaluated in the page · must return a value",
                    ),
                ]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Simulate,
        summary: "Persona · journey · use case · load · chaos.",
        actions: &[
            ActionSpec::real(
                "persona_create",
                "Persist a persona to project memory · returns its id.",
                Of(&[
                    req("role", Str, "persona role"),
                    opt("goals", List, "goal strings · default []"),
                ]),
            ),
            ActionSpec::real(
                "journey_define",
                "Persist a persona's step sequence to project memory · returns its id.",
                Of(&[
                    opt("persona", Str, "persona id · default anonymous"),
                    opt("steps", List, "ordered step descriptions · default []"),
                ]),
            ),
            ActionSpec::planned(
                "journey_simulate",
                "Execute a stored journey and report per-step outcomes · ✗ implemented: multiplies count by step length, issues no request.",
            ),
            ActionSpec::real(
                "use_case_define",
                "Persist an actor + intent + flow to project memory · returns its id.",
                Of(&[
                    req("actor", Str, "actor name"),
                    opt("intent", Str, "what the actor wants"),
                    opt("flow", List, "ordered flow steps · default []"),
                ]),
            ),
            ActionSpec::real(
                "load",
                "Drive a k6 load profile against a target · script mounted into the container.",
                Of(&[
                    req("target", Str, "URL under test"),
                    opt("profile", Str, "smoke | load | stress · default smoke"),
                ]),
            ),
            ActionSpec::real(
                "chaos_inject",
                "Inject a container fault via docker kill/pause/unpause/stop.",
                Of(&[
                    req("service", Str, "container name"),
                    opt("fault", Str, "kill | pause | unpause | stop · default kill"),
                ]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Deploy,
        summary: "CI/CD generate · deploy · rollback · canary · health.",
        actions: &[
            ActionSpec::scaffold(
                "cicd_generate",
                "Write a CI/CD workflow template · refuses to overwrite · ! the deploy job is a placeholder.",
                Of(&[opt("provider", Str, "github | gitlab · default github")]),
            ),
            ActionSpec::real(
                "target",
                "Push the image for an env · propagates docker push exit code.",
                Of(&[
                    req("image", Str, "fully qualified image ref"),
                    opt(
                        "env",
                        Str,
                        "staging | production · default staging · label only",
                    ),
                ]),
            ),
            ActionSpec::planned(
                "rollback",
                "Roll back a deployment · ✗ implemented: resolves the previous tag, changes nothing.",
            ),
            ActionSpec::scaffold(
                "smoke",
                "Alias of health · curls one url · ✗ runs a smoke suite · ignores env.",
                Of(&[req("url", Str, "health endpoint")]),
            ),
            ActionSpec::real(
                "health",
                "Curl an endpoint · ok gated on curl success AND 2xx.",
                Of(&[req("url", Str, "health endpoint")]),
            ),
            ActionSpec::real(
                "release_create",
                "Create an annotated git tag · propagates git exit code · ✗ pushes it.",
                Of(&[
                    req("tag", Str, "tag name · v0.1.0"),
                    opt("notes", Str, "annotation message · default empty"),
                ]),
            ),
            ActionSpec::planned(
                "canary",
                "Shift traffic to a canary · ✗ implemented: returns 100 - percent, touches no router.",
            ),
            ActionSpec::planned(
                "blue_green",
                "Swap blue/green slots · ✗ implemented: flips a string, reads no live state.",
            ),
            ActionSpec::planned(
                "diff",
                "Diff deployed vs HEAD · ✗ implemented: the revision range is passed unexpanded, so git always fails and an empty log is reported as success.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Repo,
        summary: "Multi-repo · digest · port · compare · reverse engineer.",
        actions: &[
            ActionSpec::real(
                "register",
                "Add a repo to the registry · alias inferred from url when omitted.",
                Of(&[
                    req("url", Str, "git url | file:// | absolute or ./ local path"),
                    opt("alias", Str, "registry key · default inferred from url"),
                ]),
            ),
            ActionSpec::real(
                "list",
                "List registered repos from the registry file.",
                NoArgs,
            ),
            ActionSpec::real(
                "remove",
                "Drop a repo from the registry · optionally delete its clone directory.",
                Of(&[
                    req("alias", Str, "registry key"),
                    opt(
                        "delete_clone",
                        Bool,
                        "also remove the clone dir · default false",
                    ),
                ]),
            ),
            ActionSpec::real(
                "digest",
                "Clone (or read local path) · walk the tree · write the digest JSON.",
                Of(&[req("alias", Str, "registered alias")]),
            ),
            ActionSpec::scaffold(
                "list_capabilities",
                "List top-level source dirs + filename keyword hits · filename substrings only.",
                Of(&[req("alias", Str, "digested alias")]),
            ),
            ActionSpec::real(
                "extract",
                "Recursively copy a path out of a digested repo · refuses to overwrite the target.",
                Of(&[
                    req("alias", Str, "digested alias"),
                    opt(
                        "capability",
                        Str,
                        "path within the source repo · required unless source given",
                    ),
                    opt("source", Str, "alias for capability"),
                    opt(
                        "target_path",
                        Str,
                        "destination · default ./extracted/<capability>",
                    ),
                ]),
            ),
            ActionSpec::scaffold(
                "compare",
                "Emit side-by-side language histograms · ✗ branches on axis · every axis returns the same payload.",
                Of(&[
                    req("a", Str, "first digested alias"),
                    req("b", Str, "second digested alias"),
                    opt(
                        "axis",
                        Str,
                        "echoed only · features | arch | standards · default arch",
                    ),
                ]),
            ),
            ActionSpec::planned(
                "port",
                "Port a repo to another language · ✗ implemented: returns a language histogram plus confidence from a hardcoded table · translates no code.",
            ),
            ActionSpec::real(
                "port_validate",
                "Spawn the pipeline binary · run the fast gate inside the ported path.",
                Of(&[req("path", Str, "directory containing pipeline.yaml")]),
            ),
            ActionSpec::planned(
                "apply_standards",
                "Standards compliance for a digested repo · ✗ implemented: the score is the fraction of four file-existence booleans, not compliance.",
            ),
            ActionSpec::real(
                "capability_graph",
                "Build nodes + edges by reading every digest on disk.",
                NoArgs,
            ),
            ActionSpec::planned(
                "re_analyze",
                "Reverse-engineering analysis · ✗ implemented: writes a queued job no worker will ever process.",
            ),
            ActionSpec::planned(
                "re_status",
                "RE job status · ✗ implemented: echoes a job file nothing processes · status never advances.",
            ),
            ActionSpec::planned(
                "re_report",
                "RE report · ✗ implemented: overwrites status to complete and returns empty module map · contracts · patterns.",
            ),
            ActionSpec::planned(
                "re_reconstruct",
                "Reconstruct api | schema | dockerfile · ✗ implemented: writes fixed template text · target appears only in a comment.",
            ),
            ActionSpec::planned(
                "re_modernize",
                "Modernization plan · ✗ implemented: hardcoded phase list and fixed risk level for every input.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Docs,
        summary: "Docs · changelog · diagram · spec generation.",
        actions: &[
            ActionSpec::scaffold(
                "generate",
                "Write a doc skeleton of empty headings · ✗ reads source · refuses to overwrite.",
                Of(&[opt(
                    "kind",
                    Str,
                    "readme | runbook | onboarding | api | arch · default readme",
                )]),
            ),
            ActionSpec::real(
                "update_from_code",
                "Run cargo doc · exit code propagated · ✗ writes or merges any doc file.",
                NoArgs,
            ),
            ActionSpec::real(
                "changelog",
                "Read the commit log over a range · commit count derived, exit status gated.",
                Of(&[
                    opt("from", Str, "start ref · omit → full history to `to`"),
                    opt("to", Str, "end ref · default HEAD"),
                ]),
            ),
            ActionSpec::planned(
                "diagram",
                "Diagram the caller's architecture · ✗ implemented: writes a hardcoded diagram of Pipeline's own architecture for every project, parses nothing.",
            ),
            ActionSpec::planned(
                "publish",
                "Publish docs to a hosting target · ✗ implemented: nothing is ever published, target echoed and unused, a missing toolchain is swallowed into success.",
            ),
            ActionSpec::planned(
                "spec_generate",
                "Derive an API spec from source · ✗ implemented: source echoed but never opened; emits a fabricated endpoint that then feeds contract tests as a fabricated gate.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Data,
        summary: "DB · schema · migrate · seed · ETL · quality.",
        actions: &[
            ActionSpec::scaffold(
                "db_provision",
                "Write a compose file for a DB service · ✗ starts it → pipeline_docker.compose_up.",
                Of(&[
                    opt(
                        "engine",
                        Str,
                        "postgres | mysql | redis | mongo | clickhouse | sqlite · default postgres",
                    ),
                    opt(
                        "version",
                        Str,
                        "image version tag · engine-specific default",
                    ),
                    opt(
                        "extensions",
                        List,
                        "postgres ONLY · refused for other engines · vector → pgvector image · contrib names ship in the stock image · anything else refused, ✗ silently downgraded",
                    ),
                ]),
            ),
            ActionSpec::real(
                "schema_generate",
                "Render a structured table spec to SQL, exactly as given · refuses without tables · refuses to overwrite.",
                Of(&[
                    req(
                        "tables",
                        List,
                        "non-empty · each {name, columns:[{name, type, not_null?, default?, unique?, primary_key?, references?, generated?}], primary_key?, partition_by?, partitions?, indexes?, comment?}",
                    ),
                    opt(
                        "extensions",
                        List,
                        "names → CREATE EXTENSION IF NOT EXISTS, in order",
                    ),
                    opt(
                        "path",
                        Str,
                        "output file · default migrations/0001_init.sql",
                    ),
                    opt("comment", Str, "header comment · one line per input line"),
                ]),
            ),
            ActionSpec::real(
                "schema_migrate",
                "Apply pending migrations via the stack's tool · propagates its exit code.",
                Of(&[opt(
                    "stack",
                    Str,
                    "rust → sqlx | python-uv → alembic | node | bun → prisma · default rust",
                )]),
            ),
            ActionSpec::scaffold(
                "seed",
                "Write a seeds fixture of fixed-shape records · ✗ reads your schema · ✗ touches a DB.",
                Of(&[
                    opt("persona", Str, "fixture name · default user"),
                    opt("count", Int, "record count · default 10"),
                ]),
            ),
            ActionSpec::planned(
                "etl_create",
                "Scaffold an ETL job · ✗ implemented: query and sink table are hardcoded regardless of input.",
            ),
            ActionSpec::planned(
                "quality_check",
                "Check data quality · ✗ implemented: writes two fixed SQL strings, never connects, never executes, checks nothing.",
            ),
            ActionSpec::planned(
                "db_diff",
                "Diff two database schemas · ✗ implemented: connection strings are passed unexpanded so the tool can never connect, and a spawn failure is reported as success.",
            ),
            ActionSpec::planned(
                "anonymize",
                "Anonymize a dump · ✗ implemented: writes a rules file and echoes source/target · neither file is ever opened.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Observe,
        summary: "Metrics · logs · traces · perf · optimize.",
        actions: &[
            ActionSpec::scaffold(
                "metrics_setup",
                "Write prometheus + otel collector templates · ✗ reads your stack.",
                Of(&[opt(
                    "stack",
                    Str,
                    "echoed in the response · ✗ alters output",
                )]),
            ),
            ActionSpec::real(
                "logs_aggregate",
                "Tail recent lines from every compose service · exit status gated.",
                Of(&[opt("env", Str, "label echoed back · default dev")]),
            ),
            ActionSpec::scaffold(
                "traces_setup",
                "Write a jaeger compose template · refuses to overwrite.",
                NoArgs,
            ),
            ActionSpec::scaffold(
                "alerts_define",
                "Append a Prometheus rule to the alerts file · ✗ validates expr.",
                Of(&[req("rule", Str, "PromQL expression")]),
            ),
            ActionSpec::planned(
                "perf_baseline",
                "Measure and record a performance baseline · ✗ implemented: stores whatever metrics the caller passes, defaulting to an empty object, so no regression can ever be detected.",
            ),
            ActionSpec::real(
                "perf_compare",
                "Diff supplied metrics against the stored baseline · per-key delta + pct.",
                Of(&[
                    opt("suite", Str, "baseline key · default default"),
                    opt(
                        "metrics",
                        Obj,
                        "current numeric readings · compared per shared key",
                    ),
                ]),
            ),
            ActionSpec::real(
                "optimize_suggest",
                "Derive inner-loop speedups from recorded stage runs · zero runs is insufficient data, ✗ fast.",
                NoArgs,
            ),
            ActionSpec::real(
                "image_size_optimize",
                "List image layers by size · exit status gated.",
                Of(&[req("image", Str, "image tag to inspect")]),
            ),
            ActionSpec::real(
                "query_optimize",
                "Run EXPLAIN ANALYZE against a postgres DSN · ! executes the statement.",
                Of(&[
                    req("sql", Str, "statement to explain"),
                    opt(
                        "dsn",
                        Str,
                        "postgres connection string · required to execute",
                    ),
                ]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Security,
        summary: "Secrets · vulns · audit · threat · compliance.",
        actions: &[
            ActionSpec::real(
                "secret_scan",
                "Scan for committed secrets via trufflehog · findings fail the call · locations only, ✗ values.",
                Of(&[opt(
                    "scope",
                    Str,
                    "filesystem | git · git scans history · default filesystem",
                )]),
            ),
            ActionSpec::real(
                "vuln_scan",
                "Scan image or working tree for CVEs via Trivy · findings fail the call.",
                Of(&[
                    opt(
                        "target",
                        Str,
                        "image tag → image mode · omit → filesystem scan of cwd",
                    ),
                    opt("severity", Str, "comma list · default CRITICAL,HIGH"),
                ]),
            ),
            ActionSpec::real(
                "dep_audit",
                "Run the stack's dependency audit binary · a missing scanner is UNKNOWN, ✗ pass.",
                Of(&[opt(
                    "stack",
                    Str,
                    "rust | node | ts | typescript | bun | python | python-uv · default rust",
                )]),
            ),
            ActionSpec::scaffold(
                "threat_model",
                "Write a STRIDE threat-model skeleton · ✗ reads your code.",
                Of(&[opt("scope", Str, "heading text only · default application")]),
            ),
            ActionSpec::planned(
                "compliance_check",
                "Gap-analyse against a compliance framework · ✗ implemented: framework is never branched on, so hipaa | pci_dss | gdpr all score identically from five file-existence checks.",
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Memory,
        summary: "Remember · recall · history · patterns · export.",
        actions: &[
            ActionSpec::real(
                "remember",
                "Upsert a key/value into project memory · DB errors propagate.",
                Of(&[
                    req("key", Str, "lookup key · exact match on recall"),
                    req(
                        "value",
                        Str,
                        "stored verbatim · JSON must be pre-serialized",
                    ),
                    opt("scope", Str, "namespace · default \"default\""),
                ]),
            ),
            ActionSpec::real(
                "recall",
                "Fetch one value by exact key equality · ! ✗ semantic search · a miss reports found:false.",
                Of(&[
                    req("key", Str, "exact key · ✗ substring, ✗ similarity"),
                    opt("scope", Str, "namespace · default \"default\""),
                ]),
            ),
            ActionSpec::real(
                "history",
                "List recent pipeline runs newest-first · limit honoured.",
                Of(&[opt("limit", Int, "max rows · default 10")]),
            ),
            ActionSpec::real(
                "known_issues",
                "Count recorded failures grouped by stage · a read error is an error, ✗ zero.",
                NoArgs,
            ),
            ActionSpec::scaffold(
                "suggest_fix",
                "Keyword LIKE search over past failure messages · ! ✗ vector search · ranking is not meaningful.",
                Of(&[
                    req("error", Str, "error text · matched as OR'd keywords"),
                    opt("limit", Int, "max candidates · default 5"),
                ]),
            ),
            ActionSpec::real(
                "pattern_report",
                "Failure totals per stage · distinguishes no-runs-yet from measured-green.",
                NoArgs,
            ),
            ActionSpec::real(
                "export",
                "Write a memory bundle · reports what was written and what was skipped.",
                Of(&[opt(
                    "format",
                    Str,
                    "json | markdown | llm_context · only json is re-importable",
                )]),
            ),
            ActionSpec::real(
                "import",
                "Load a JSON export back into memory · reports imported · overwritten · skipped · failed.",
                Of(&[req(
                    "path",
                    Str,
                    "path to a json export · other formats rejected",
                )]),
            ),
        ],
    },
    ToolDescriptor {
        name: ToolName::Report,
        summary: "Dashboard · velocity · burndown · last.",
        actions: &[
            ActionSpec::real(
                "dashboard",
                "Computed handover packet: project state · last run · active work · errors propagate.",
                NoArgs,
            ),
            ActionSpec::real(
                "velocity_metrics",
                "Pass rate · median inner-loop ms · failures by stage, computed from recorded runs.",
                NoArgs,
            ),
            ActionSpec::real(
                "burndown",
                "Feature counts done · in_progress · blocked · remaining · an unknown milestone is an error, ✗ whole-project numbers.",
                Of(&[opt("milestone", Str, "milestone key · omit → all features")]),
            ),
            ActionSpec::real("last", "Alias of dashboard · identical payload.", NoArgs),
            ActionSpec::real("summary", "Alias of dashboard · identical payload.", NoArgs),
        ],
    },
    ToolDescriptor {
        name: ToolName::Meta,
        summary: "Explain · config · self-check · version.",
        actions: &[
            ActionSpec::scaffold(
                "explain",
                "Return a canned blurb from a hardcoded string table · ✗ reads the project.",
                Of(&[opt("topic", Str, "pipeline | stages | memory | tools")]),
            ),
            ActionSpec::real(
                "config_get",
                "Read pipeline.yaml gates and stack · omit key → whole config.",
                Of(&[opt("key", Str, "single key · omit → entire config")]),
            ),
            ActionSpec::planned(
                "config_set",
                "Change project configuration · ✗ effective: writes a file nothing reads; the live config is pipeline.yaml, so every change alters no gate, stage, or deploy.",
            ),
            ActionSpec::real(
                "self_check",
                "Probe cargo · rustc · docker · git via --version · takes no arguments.",
                NoArgs,
            ),
            ActionSpec::real(
                "version",
                "Compile-time versions of every pipeline crate · takes no arguments.",
                NoArgs,
            ),
        ],
    },
];

/// Build the canonical 19-tool descriptor list.
pub fn registry() -> Vec<ToolDescriptor> {
    REGISTRY.to_vec()
}

/// Look up one tool's descriptor by MCP tool name (`pipeline_run`, …).
pub fn descriptor_for(tool: &str) -> Option<&'static ToolDescriptor> {
    REGISTRY.iter().find(|d| d.name.as_str() == tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::Fidelity;

    #[test]
    fn registry_has_nineteen_tools() {
        assert_eq!(registry().len(), 19);
    }

    #[test]
    fn every_tool_has_at_least_one_action() {
        for t in registry() {
            assert!(t.action_count() > 0, "{} has zero actions", t.name.as_str());
        }
    }

    #[test]
    fn action_names_are_unique_within_a_tool() {
        for t in registry() {
            let mut names: Vec<&str> = t.actions.iter().map(|a| a.name).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(
                before,
                names.len(),
                "duplicate action in {}",
                t.name.as_str()
            );
        }
    }

    #[test]
    fn every_action_declares_a_non_empty_summary() {
        for t in registry() {
            for a in t.actions {
                assert!(
                    !a.summary.trim().is_empty(),
                    "{}.{} has no summary",
                    t.name.as_str(),
                    a.name
                );
            }
        }
    }

    #[test]
    fn a_planned_action_says_what_is_missing() {
        // ! `Planned` is a promise to the agent that the action will refuse.
        // The summary must explain the gap · "not implemented" alone sends the
        // agent looking for a workaround it cannot evaluate.
        for t in registry() {
            for a in t.actions {
                if a.fidelity == Fidelity::Planned {
                    assert!(
                        a.summary.contains('✗'),
                        "{}.{} is Planned but does not state what is missing",
                        t.name.as_str(),
                        a.name
                    );
                }
            }
        }
    }

    #[test]
    fn a_scaffold_never_claims_to_analyze() {
        // A scaffold writes boilerplate. Words implying derivation from the
        // caller's project are exactly the confusion Fidelity exists to stop.
        for t in registry() {
            for a in t.actions {
                if a.fidelity == Fidelity::Scaffold {
                    let s = a.summary.to_lowercase();
                    for banned in ["analyzes your", "derives from your", "inferred from your"] {
                        assert!(
                            !s.contains(banned),
                            "{}.{} is Scaffold but claims '{banned}'",
                            t.name.as_str(),
                            a.name
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn descriptor_lookup_finds_every_tool() {
        for t in registry() {
            assert!(descriptor_for(t.name.as_str()).is_some());
        }
        assert!(descriptor_for("pipeline_nope").is_none());
    }

    #[test]
    fn an_unknown_argument_is_rejected_with_a_suggestion() {
        let d = descriptor_for("pipeline_env").expect("env");
        let err = d
            .validate(
                "deps_install",
                &serde_json::json!({"stack": "rust", "pacakges": []}),
            )
            .unwrap_err();
        assert!(err.contains("pacakges"), "{err}");
        assert!(err.contains("did you mean 'packages'"), "{err}");
    }

    #[test]
    fn a_missing_required_argument_carries_its_help_text() {
        let d = descriptor_for("pipeline_docker").expect("docker");
        let err = d.validate("build", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("tag"), "{err}");
        assert!(err.contains("name:tag"), "must include help: {err}");
    }

    #[test]
    fn an_unspecified_action_accepts_anything() {
        // prd_update merges every top-level key · declaring a closed set would
        // be a fabrication, so validation must stay out of its way.
        let d = descriptor_for("pipeline_plan").expect("plan");
        assert!(
            d.validate("prd_update", &serde_json::json!({"whatever": 1}))
                .is_ok()
        );
    }

    #[test]
    fn a_quoted_scalar_is_accepted_for_int_and_bool() {
        // Agents routinely quote scalars · refusing that is pedantry, ✗ safety.
        let d = descriptor_for("pipeline_memory").expect("memory");
        assert!(
            d.validate("history", &serde_json::json!({"limit": "5"}))
                .is_ok()
        );
        let d = descriptor_for("pipeline_run").expect("run");
        assert!(
            d.validate("fmt", &serde_json::json!({"check": "true"}))
                .is_ok()
        );
    }

    #[test]
    fn a_type_confusion_that_changes_behaviour_is_rejected() {
        let d = descriptor_for("pipeline_env").expect("env");
        let err = d
            .validate(
                "deps_install",
                &serde_json::json!({"stack": "rust", "packages": "serde"}),
            )
            .unwrap_err();
        assert!(err.contains("must be array"), "{err}");
    }

    #[test]
    fn the_fidelity_split_is_recorded() {
        // Not a threshold — a tripwire. If this moves, the surface changed and
        // docs/usecases/tool-fidelity.md needs the same edit.
        let (mut real, mut scaffold, mut planned) = (0, 0, 0);
        for t in registry() {
            for a in t.actions {
                match a.fidelity {
                    Fidelity::Real => real += 1,
                    Fidelity::Scaffold => scaffold += 1,
                    Fidelity::Planned => planned += 1,
                }
            }
        }
        assert_eq!(
            (real, scaffold, planned),
            (120, 20, 35),
            "fidelity split moved · update the fidelity doc too"
        );
        assert_eq!(real + scaffold + planned, 175, "action count drift");
    }
}
