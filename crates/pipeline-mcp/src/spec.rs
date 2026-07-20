//! Per-action specification · the registry's unit of truth.
//!
//! Every action declares what it takes and **how much of its name it actually
//! delivers**. Both halves exist because of a live dogfooding audit that found
//! ~60 actions returning `ok: true` while doing nothing, doing the wrong thing,
//! or emitting fabricated data (`docs/tool-fidelity.md`).
//!
//! ! The rule this module enforces: *a tool that refuses is more useful than a
//! tool that lies.* An agent cannot verify a result it did not compute — the
//! success flag is the only signal it has, so that flag must be load-bearing.
//!
//! Two enabling conditions for that defect class, both closed here:
//! - args were never declared → "accepted, echoed, silently dropped" was
//!   invisible to the caller *and* untestable ([`ArgSpec`] · [`ArgSet`])
//! - nothing distinguished "ran a real scan" from "wrote a template" →
//!   scaffolds read as analysis ([`Fidelity`])

use serde_json::{Value, json};

/// How much of its own name an action delivers.
///
/// ! Declared per action and asserted in `registry_conformance` · the marker is
/// a claim the test suite checks, ✗ a comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Does the work its name claims: spawns the process, writes the file,
    /// queries the database, calls the API — and reports the true outcome,
    /// *including failure*. `ok: true` here means the work happened.
    Real,

    /// Writes a template, skeleton, or fixture. ✗ reads · ✗ analyzes the
    /// caller's project. Useful as a starting point, worthless as a finding.
    ///
    /// ! Responses carry `scaffold: true` so an agent never mistakes generated
    /// boilerplate for a derived result. `docs.diagram` emitting Pipeline's own
    /// architecture for every project is the failure this prevents.
    Scaffold,

    /// Not implemented. Returns `ok: false` via `ToolResponse::not_implemented`.
    ///
    /// ! This is the *correct* state for unbuilt work — `Planned` is honest,
    /// a fabricated payload is not. Conformance asserts these never return ok.
    Planned,
}

impl Fidelity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Scaffold => "scaffold",
            Self::Planned => "planned",
        }
    }

    /// Prefix shown to the agent in `tools/list`, so fidelity is visible at
    /// selection time rather than after a misleading success.
    pub const fn badge(self) -> &'static str {
        match self {
            Self::Real => "",
            Self::Scaffold => "[scaffold] ",
            Self::Planned => "[planned] ",
        }
    }
}

/// JSON type of a declared argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Str,
    Int,
    Bool,
    List,
    Obj,
}

impl ArgType {
    pub const fn as_json_type(self) -> &'static str {
        match self {
            Self::Str => "string",
            Self::Int => "integer",
            Self::Bool => "boolean",
            Self::List => "array",
            Self::Obj => "object",
        }
    }
}

/// One declared argument.
#[derive(Debug, Clone, Copy)]
pub struct ArgSpec {
    pub name: &'static str,
    pub ty: ArgType,
    pub required: bool,
    /// One line, agent-facing. Enumerate accepted values where finite —
    /// `db_provision` silently dropping `extensions` for four of five engines
    /// was invisible precisely because nothing stated the accepted set.
    pub help: &'static str,
}

/// Required argument.
pub const fn req(name: &'static str, ty: ArgType, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        ty,
        required: true,
        help,
    }
}

/// Optional argument.
pub const fn opt(name: &'static str, ty: ArgType, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        ty,
        required: false,
        help,
    }
}

/// An action's argument surface.
///
/// ! `None` and `Unspecified` are deliberately distinct. Collapsing them would
/// make the registry commit the same sin it exists to prevent: claiming an
/// action takes no arguments when nobody has checked is a fabrication.
#[derive(Debug, Clone, Copy)]
pub enum ArgSet {
    /// Verified to take no arguments.
    None,
    /// Not yet specified. Publishes a permissive schema; conformance skips it.
    Unspecified,
    /// Fully declared · unknown keys are rejected at the transport boundary.
    Of(&'static [ArgSpec]),
}

impl ArgSet {
    pub const fn specified(self) -> bool {
        !matches!(self, Self::Unspecified)
    }

    pub const fn args(self) -> &'static [ArgSpec] {
        match self {
            Self::Of(a) => a,
            Self::None | Self::Unspecified => &[],
        }
    }
}

/// A single action on a super tool.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub fidelity: Fidelity,
    pub args: ArgSet,
}

impl ActionSpec {
    pub const fn real(name: &'static str, summary: &'static str, args: ArgSet) -> Self {
        Self {
            name,
            summary,
            fidelity: Fidelity::Real,
            args,
        }
    }

    pub const fn scaffold(name: &'static str, summary: &'static str, args: ArgSet) -> Self {
        Self {
            name,
            summary,
            fidelity: Fidelity::Scaffold,
            args,
        }
    }

    pub const fn planned(name: &'static str, summary: &'static str) -> Self {
        Self {
            name,
            summary,
            fidelity: Fidelity::Planned,
            args: ArgSet::Unspecified,
        }
    }

    /// JSON Schema for this action's `args` object.
    ///
    /// Unspecified → permissive, so an unaudited action keeps working rather
    /// than hard-failing on arguments the registry has not caught up with.
    /// Specified → `additionalProperties: false`, which turns a silently
    /// dropped argument into a visible protocol error.
    pub fn args_schema(&self) -> Value {
        if !self.args.specified() {
            return json!({"type": "object", "additionalProperties": true});
        }
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for a in self.args.args() {
            props.insert(
                a.name.to_owned(),
                json!({"type": a.ty.as_json_type(), "description": a.help}),
            );
            if a.required {
                required.push(Value::String(a.name.to_owned()));
            }
        }
        json!({
            "type": "object",
            "properties": Value::Object(props),
            "required": Value::Array(required),
            "additionalProperties": false,
        })
    }

    /// One agent-facing line for `tools/list`.
    pub fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut s = format!("{}{}: {}", self.fidelity.badge(), self.name, self.summary);
        if let ArgSet::Of(args) = self.args {
            if !args.is_empty() {
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| {
                        if a.required {
                            a.name.to_owned()
                        } else {
                            format!("{}?", a.name)
                        }
                    })
                    .collect();
                let _ = write!(s, " ({})", rendered.join(", "));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[ArgSpec] = &[
        req("stack", ArgType::Str, "python-uv | bun | node | rust | go"),
        opt(
            "packages",
            ArgType::List,
            "names · empty → sync from lockfile",
        ),
    ];

    #[test]
    fn a_specified_action_rejects_unknown_arguments() {
        // ! The whole point: `additionalProperties: false` is what turns a
        // silently-dropped arg into a visible error at the boundary.
        let a = ActionSpec::real("deps_install", "add packages", ArgSet::Of(SAMPLE));
        let s = a.args_schema();
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(s["required"], json!(["stack"]));
        assert_eq!(s["properties"]["packages"]["type"], json!("array"));
    }

    #[test]
    fn an_unspecified_action_stays_permissive() {
        // Unaudited actions keep working · they do not hard-fail on args the
        // registry has not caught up with yet.
        let a = ActionSpec::real("mystery", "?", ArgSet::Unspecified);
        assert_eq!(a.args_schema()["additionalProperties"], json!(true));
    }

    #[test]
    fn no_args_and_unspecified_are_not_the_same_claim() {
        // ! Collapsing these would let the registry fabricate: asserting "takes
        // no arguments" when nobody checked is exactly the defect class this
        // module exists to close.
        assert!(ArgSet::None.specified());
        assert!(!ArgSet::Unspecified.specified());
        let none = ActionSpec::real("x", "y", ArgSet::None).args_schema();
        assert_eq!(none["additionalProperties"], json!(false));
    }

    #[test]
    fn fidelity_is_visible_in_the_description() {
        // An agent must see "scaffold" before it calls, not infer it after.
        let s = ActionSpec::scaffold("diagram", "writes a template", ArgSet::None);
        assert!(s.describe().starts_with("[scaffold] "));
        let p = ActionSpec::planned("re_modernize", "not built");
        assert!(p.describe().starts_with("[planned] "));
        let r = ActionSpec::real("build", "builds", ArgSet::None);
        assert!(!r.describe().contains('['));
    }

    #[test]
    fn describe_marks_optional_arguments() {
        let a = ActionSpec::real("deps_install", "add packages", ArgSet::Of(SAMPLE));
        assert_eq!(
            a.describe(),
            "deps_install: add packages (stack, packages?)"
        );
    }
}
