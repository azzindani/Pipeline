//! `pipeline_deploy` handler · CI/CD generation · deploy · health · release.
//!
//! Real: `target` (pushes · records the deployment) · `health` · `release_create` ·
//! `rollback` (re-points a tag at a previously recorded digest) · `diff`.
//! Scaffold: `cicd_generate` · `smoke`.
//! Planned: `canary` · `blue_green` — both need a traffic router Pipeline
//! neither owns nor can discover. ! They refuse rather than return a split.

#![allow(clippy::doc_markdown)]

use crate::handlers::{ensure_memory, load_config_in_cwd};
use crate::server::ServerState;
use crate::tools::{ToolRequest, ToolResponse};
use pipeline_memory::{DeploymentRecord, NewDeployment};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "cicd_generate" => cicd_generate(&req.args).await,
        "target" => target(&req.args, &state).await,
        "smoke" | "health" => health(&req.args).await,
        "release_create" => release_create(&req.args).await,
        "rollback" => rollback(&req.args, &state).await,
        "canary" | "blue_green" => no_router(&req.action),
        "diff" => diff(&req.args).await,
        other => err(format!("unknown action 'pipeline_deploy.{other}'")),
    }
}

async fn cicd_generate(args: &Value) -> ToolResponse {
    let provider = args
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("github");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let (path, content) = match provider {
        "github" => (
            cwd.join(".github/workflows/deploy.yml"),
            GITHUB_DEPLOY.to_owned(),
        ),
        "gitlab" => (cwd.join(".gitlab-ci.yml"), GITLAB_CI.to_owned()),
        other => return err(format!("unsupported provider '{other}' · github|gitlab")),
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return err(format!("mkdir: {e}"));
        }
    }
    if path.exists() {
        return err(format!("refusing to overwrite {}", path.display()));
    }
    if let Err(e) = tokio::fs::write(&path, content).await {
        return err(format!("write: {e}"));
    }
    ToolResponse {
        ok: true,
        data: json!({"provider": provider, "path": path.display().to_string()}),
        next_suggested: vec!["pipeline_deploy.target".into()],
        memory_refs: vec![],
        error: None,
    }
}

// ---------- target ----------

/// Push an image · record what was pushed so `rollback` has a prior state.
///
/// ! The recording is the load-bearing half. A push that is not recorded is a
/// deployment nothing can revert.
async fn target(args: &Value, state: &Arc<ServerState>) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("staging");
    let Some(img) = args.get("image").and_then(Value::as_str) else {
        return err("missing 'image' · pass a fully qualified ref (ghcr.io/org/app:tag)".into());
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let output = match Command::new("docker")
        .args(["push", img])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("docker push spawn: {e}")),
    };
    let ok = output.status.success();
    let status = if ok { "success" } else { "failed" };
    // ✗ resolve a digest for a push that failed · docker inspect would report
    // whatever the tag meant before, which would pin the wrong image.
    let digest = if ok {
        resolve_digest(&cwd, img).await
    } else {
        None
    };
    let commit = resolve_commit(&cwd).await;
    let recorded = write_deployment(
        state,
        &NewDeployment {
            project_id: "", // filled by write_deployment from config/session
            env,
            kind: "deploy",
            image_ref: img,
            image_digest: digest.as_deref(),
            commit_sha: commit.as_deref(),
            status,
            health_json: None,
        },
    )
    .await;
    ToolResponse {
        ok,
        data: json!({
            "env": env,
            "image": img,
            "digest": digest,
            "commit_sha": commit,
            "exit_code": output.status.code().unwrap_or(-1),
            "stderr": String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            "recorded": recorded.is_ok(),
            "record_error": recorded.as_ref().err(),
        }),
        next_suggested: vec![format!("pipeline_deploy.smoke(env={env})")],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "docker push exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

/// Pin the immutable digest for `image`. `None` when docker cannot resolve one —
/// ! a record without a digest is deliberately kept (history stays honest) but
/// `rollback` will not target it.
async fn resolve_digest(cwd: &std::path::Path, image: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["inspect", "--format={{index .RepoDigests 0}}", image])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() || !s.contains("@sha256:") {
        return None;
    }
    Some(s)
}

async fn resolve_commit(cwd: &std::path::Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

async fn project_id(state: &Arc<ServerState>) -> Result<String, String> {
    if let Some(p) = state.project_id.lock().await.clone() {
        return Ok(p);
    }
    load_config_in_cwd().map(|c| c.project)
}

async fn write_deployment(
    state: &Arc<ServerState>,
    d: &NewDeployment<'_>,
) -> Result<String, String> {
    let pid = project_id(state).await?;
    let mem = ensure_memory(state).await?;
    mem.record_deployment(&NewDeployment {
        project_id: &pid,
        ..*d
    })
    .await
    .map_err(|e| e.to_string())
}

// ---------- health ----------

async fn health(args: &Value) -> ToolResponse {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_owned(),
        None => return err("missing 'url' · e.g. http://staging.example.com/health".into()),
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let output = match Command::new("curl")
        .args(["-fsS", "-o", "/dev/null", "-w", "%{http_code}", &url])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("curl spawn: {e}")),
    };
    let code_str = String::from_utf8_lossy(&output.stdout).into_owned();
    let ok = output.status.success() && code_str.starts_with('2');
    ToolResponse {
        ok,
        data: json!({
            "url": url,
            "http_code": code_str,
            "exit_code": output.status.code().unwrap_or(-1),
        }),
        next_suggested: vec![],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!("health http={code_str}"))
        },
    }
}

async fn release_create(args: &Value) -> ToolResponse {
    let tag = match args.get("tag").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => return err("missing 'tag' (e.g. v0.1.0)".into()),
    };
    let notes = args.get("notes").and_then(Value::as_str).unwrap_or("");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let output = match Command::new("git")
        .args(["tag", "-a", &tag, "-m", notes])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git tag spawn: {e}")),
    };
    let ok = output.status.success();
    ToolResponse {
        ok,
        data: json!({
            "tag": tag,
            "notes": notes,
            "stderr": String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }),
        next_suggested: vec!["git push --tags".into()],
        memory_refs: vec![],
        error: if ok {
            None
        } else {
            Some(format!(
                "git tag exit {}",
                output.status.code().unwrap_or(-1)
            ))
        },
    }
}

// ---------- rollback ----------

/// What a rollback will do, resolved entirely from recorded history.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RollbackPlan {
    /// Live deployment being replaced · `None` only on the explicit-`to` path.
    from_ref: Option<String>,
    from_digest: Option<String>,
    /// ! Always a `repo@sha256:...` ref · a rollback to a mutable tag resolves
    /// to whatever that tag means now, which is not a rollback.
    to_digest: String,
    /// Mutable tag re-pointed at `to_digest` · this is the actual revert.
    retag: String,
}

/// Choose what to roll back to from `history` (newest first).
///
/// Semantics: the newest successful deployment is live; the target is the next
/// successful one carrying a *distinct pinned digest*. ! Repeated rollback
/// therefore alternates between the last two distinct releases.
/// ✗ any success path when nothing prior was recorded — "nothing to roll back
/// to" is the answer, not `ok:true`.
fn plan_rollback(
    history: &[DeploymentRecord],
    explicit_to: Option<&str>,
    explicit_tag: Option<&str>,
) -> Result<RollbackPlan, String> {
    let successes: Vec<&DeploymentRecord> =
        history.iter().filter(|d| d.status == "success").collect();
    let current = successes.first().copied();
    let to_digest = match explicit_to {
        Some(t) => {
            if !t.contains("@sha256:") {
                return Err(format!(
                    "'to' must pin a digest (repo@sha256:...) · got '{t}' · rolling back to a mutable tag is not a rollback"
                ));
            }
            t.to_owned()
        }
        None => prior_digest(&successes)?,
    };
    let retag = match explicit_tag {
        Some(t) => t.to_owned(),
        None => match current {
            Some(c) if !c.image_ref.contains('@') => c.image_ref.clone(),
            Some(c) => {
                return Err(format!(
                    "live deployment '{}' was pushed by digest · no mutable tag to re-point · pass 'tag'",
                    c.image_ref
                ));
            }
            None => {
                return Err(
                    "no recorded deployment for this env · pass 'tag' alongside 'to'".into(),
                );
            }
        },
    };
    Ok(RollbackPlan {
        from_ref: current.map(|c| c.image_ref.clone()),
        from_digest: current.and_then(|c| c.image_digest.clone()),
        to_digest,
        retag,
    })
}

/// First successful deployment older than the live one with a different pinned
/// digest. Every failure mode names itself · ✗ collapse into a generic error.
fn prior_digest(successes: &[&DeploymentRecord]) -> Result<String, String> {
    let Some(current) = successes.first() else {
        return Err("no successful deployment recorded for this env · nothing to roll back to · deploy.target records one on every successful push".into());
    };
    if successes.len() == 1 {
        return Err(format!(
            "only one successful deployment recorded ('{}') · nothing earlier to roll back to",
            current.image_ref
        ));
    }
    successes
        .iter()
        .skip(1)
        .find_map(|d| {
            d.image_digest
                .as_ref()
                .filter(|g| Some(g.as_str()) != current.image_digest.as_deref())
                .cloned()
        })
        .ok_or_else(|| {
            format!(
                "{} earlier successful deployment(s) recorded but none carries a distinct pinned digest · docker inspect resolved no RepoDigests at deploy time, so there is nothing immutable to return to",
                successes.len() - 1
            )
        })
}

/// docker argv for a rollback · pull the pinned digest, re-point the tag, push.
fn rollback_argv(plan: &RollbackPlan) -> Vec<Vec<String>> {
    vec![
        vec!["pull".to_owned(), plan.to_digest.clone()],
        vec!["tag".to_owned(), plan.to_digest.clone(), plan.retag.clone()],
        vec!["push".to_owned(), plan.retag.clone()],
    ]
}

async fn rollback(args: &Value, state: &Arc<ServerState>) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("staging");
    let explicit_to = args.get("to").and_then(Value::as_str);
    let explicit_tag = args.get("tag").and_then(Value::as_str);
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let history = match read_history(state, env).await {
        Ok(h) => h,
        Err(e) => return err(format!("rollback[{env}]: {e}")),
    };
    let plan = match plan_rollback(&history, explicit_to, explicit_tag) {
        Ok(p) => p,
        Err(e) => return err(format!("rollback[{env}]: {e}")),
    };
    if dry_run {
        return ToolResponse::ok(json!({
            "env": env, "executed": false, "plan": plan_json(&plan),
            "commands": rollback_argv(&plan),
        }));
    }
    execute_rollback(state, env, &plan).await
}

async fn read_history(
    state: &Arc<ServerState>,
    env: &str,
) -> Result<Vec<DeploymentRecord>, String> {
    let pid = project_id(state).await?;
    let mem = ensure_memory(state).await?;
    mem.deployment_history(&pid, env, 50)
        .await
        .map_err(|e| e.to_string())
}

fn plan_json(plan: &RollbackPlan) -> Value {
    json!({
        "from_image": plan.from_ref,
        "from_digest": plan.from_digest,
        "to_digest": plan.to_digest,
        "retagged": plan.retag,
    })
}

async fn execute_rollback(
    state: &Arc<ServerState>,
    env: &str,
    plan: &RollbackPlan,
) -> ToolResponse {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let mut failure: Option<String> = None;
    for argv in rollback_argv(plan) {
        let out = match Command::new("docker")
            .args(&argv)
            .current_dir(&cwd)
            .output()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                failure = Some(format!("docker {} spawn: {e}", argv[0]));
                break;
            }
        };
        if !out.status.success() {
            failure = Some(format!(
                "docker {} exit {} · {}",
                argv[0],
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
            break;
        }
    }
    let pushed = failure.is_none();
    let recorded = write_deployment(
        state,
        &NewDeployment {
            project_id: "",
            env,
            kind: "rollback",
            image_ref: &plan.retag,
            image_digest: Some(&plan.to_digest),
            commit_sha: None,
            status: if pushed { "success" } else { "failed" },
            health_json: None,
        },
    )
    .await;
    rollback_response(env, plan, pushed, failure, &recorded)
}

/// ! `ok` requires both halves: an unrecorded rollback leaves history claiming
/// the old image is still live, which corrupts the *next* rollback.
fn rollback_response(
    env: &str,
    plan: &RollbackPlan,
    pushed: bool,
    failure: Option<String>,
    recorded: &Result<String, String>,
) -> ToolResponse {
    let error = match failure {
        Some(f) => Some(f),
        None => match recorded {
            Err(e) => Some(format!(
                "rolled back to {} but failed to record it: {e} · history is now stale",
                plan.to_digest
            )),
            Ok(_) => None,
        },
    };
    ToolResponse {
        ok: pushed && recorded.is_ok(),
        data: json!({
            "env": env,
            "executed": true,
            "pushed": pushed,
            "recorded": recorded.is_ok(),
            "plan": plan_json(plan),
        }),
        next_suggested: vec![format!("pipeline_deploy.health(url=...) · verify {env}")],
        memory_refs: vec![],
        error,
    }
}

// ---------- planned ----------

/// ✗ implemented · defence in depth behind the dispatch guard.
///
/// Both need a traffic router (nginx/Traefik/k8s Service/LB) that Pipeline does
/// not own, does not scaffold, and cannot discover from the repo. Returning a
/// percentage split or a slot name while touching no router is the exact defect
/// class this surface exists to remove.
fn no_router(action: &str) -> ToolResponse {
    err(format!(
        "pipeline_deploy.{action} not implemented · requires a traffic router Pipeline neither owns nor can discover · use deploy.target + deploy.health, and shift traffic with your own router"
    ))
}

// ---------- diff ----------

/// Read `git describe`'s outcome. ! An untagged repo exits 128 — a distinct
/// state from "no commits since the tag" and ✗ collapse into an empty log.
fn describe_outcome(success: bool, stdout: &str, stderr: &str) -> Result<String, String> {
    if success {
        let tag = stdout.trim();
        if tag.is_empty() {
            return Err("git describe succeeded but printed no tag".into());
        }
        return Ok(tag.to_owned());
    }
    if stderr.to_ascii_lowercase().contains("no names found")
        || stderr.to_ascii_lowercase().contains("no tags can describe")
    {
        return Err(
            "no git tag found · nothing has been released, so there is no baseline to diff against · create one with deploy.release_create".into(),
        );
    }
    Err(format!("git describe failed · {}", stderr.trim()))
}

/// ! Exit status is checked before the log is read. The previous version never
/// did, so a git failure was reported as `ok:true` with an empty commit list.
fn log_outcome(
    success: bool,
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<Vec<String>, String> {
    if !success {
        return Err(format!(
            "git log exit {} · {}",
            code.unwrap_or(-1),
            stderr.trim()
        ));
    }
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

async fn diff(args: &Value) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("staging");
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => return err(format!("cwd: {e}")),
    };
    let base = match args.get("base").and_then(Value::as_str) {
        Some(b) => b.to_owned(),
        None => match resolve_base_tag(&cwd).await {
            Ok(t) => t,
            Err(e) => return err(format!("diff[{env}]: {e}")),
        },
    };
    // ! `<base>..HEAD` is one argv element built here · ✗ shell substitution:
    // the previous version passed `$(git describe ...)..HEAD` literally, so git
    // exited 128 on every call.
    let range = format!("{base}..HEAD");
    let output = match Command::new("git")
        .args(["log", "--oneline", "--no-decorate", &range])
        .current_dir(&cwd)
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return err(format!("git log spawn: {e}")),
    };
    let commits = match log_outcome(
        output.status.success(),
        output.status.code(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    ) {
        Ok(c) => c,
        Err(e) => return err(format!("diff[{env}]: {e}")),
    };
    ToolResponse::ok(json!({
        "env": env,
        "base": base,
        "range": range,
        "undeployed_commits": commits.len(),
        "commits": commits,
    }))
}

async fn resolve_base_tag(cwd: &std::path::Path) -> Result<String, String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--abbrev=0"])
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| format!("git describe spawn: {e}"))?;
    describe_outcome(
        out.status.success(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

const GITHUB_DEPLOY: &str = "name: deploy\n\non:\n  push:\n    tags:\n      - 'v*'\n  workflow_dispatch:\n    inputs:\n      env:\n        description: 'staging | production'\n        required: true\n        default: 'staging'\n\njobs:\n  deploy:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - name: docker login ghcr\n        uses: docker/login-action@v3\n        with:\n          registry: ghcr.io\n          username: ${{ github.actor }}\n          password: ${{ secrets.GITHUB_TOKEN }}\n      - name: build + push\n        run: |\n          IMAGE=ghcr.io/${{ github.repository }}:${{ github.sha }}\n          docker build -t \"$IMAGE\" .\n          docker push \"$IMAGE\"\n      - name: deploy\n        run: echo \"TODO: ssh+compose deploy to ${{ inputs.env || 'staging' }}\"\n";

const GITLAB_CI: &str = "stages:\n  - build\n  - deploy\n\nbuild:\n  stage: build\n  image: docker:25\n  services:\n    - docker:25-dind\n  script:\n    - docker build -t $CI_REGISTRY_IMAGE:$CI_COMMIT_SHA .\n    - docker push $CI_REGISTRY_IMAGE:$CI_COMMIT_SHA\n\ndeploy:\n  stage: deploy\n  script:\n    - echo TODO ssh+compose deploy\n  only:\n    - tags\n";

fn err(msg: String) -> ToolResponse {
    ToolResponse {
        ok: false,
        data: json!({}),
        next_suggested: vec![],
        memory_refs: vec![],
        error: Some(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(image: &str, digest: Option<&str>, status: &str, at: &str) -> DeploymentRecord {
        DeploymentRecord {
            id: format!("{image}-{at}"),
            project_id: "p1".into(),
            env: "staging".into(),
            kind: "deploy".into(),
            image_ref: image.into(),
            image_digest: digest.map(ToOwned::to_owned),
            commit_sha: None,
            status: status.into(),
            deployed_at: at.into(),
            health_json: None,
        }
    }

    const D1: &str =
        "ghcr.io/org/app@sha256:1111111111111111111111111111111111111111111111111111111111111111";
    const D2: &str =
        "ghcr.io/org/app@sha256:2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn a_rollback_with_no_prior_deployment_refuses() {
        let e = plan_rollback(&[], None, None).expect_err("empty history must refuse");
        assert!(e.contains("nothing to roll back to"), "{e}");
    }

    #[test]
    fn a_rollback_with_only_one_deployment_refuses() {
        let h = vec![rec("app:v1", Some(D1), "success", "2026-01-01T00:00:00Z")];
        let e = plan_rollback(&h, None, None).expect_err("single deployment must refuse");
        assert!(e.contains("nothing earlier"), "{e}");
    }

    #[test]
    fn a_failed_deployment_is_never_a_rollback_target() {
        // Only one *successful* row · the failed push never went live.
        let h = vec![
            rec("app:v2", Some(D2), "success", "2026-01-02T00:00:00Z"),
            rec("app:v1", Some(D1), "failed", "2026-01-01T00:00:00Z"),
        ];
        let e = plan_rollback(&h, None, None).expect_err("failed rows are not targets");
        assert!(e.contains("nothing earlier"), "{e}");
    }

    #[test]
    fn a_rollback_targets_a_digest_not_a_mutable_tag() {
        let h = vec![
            rec("app:latest", Some(D2), "success", "2026-01-02T00:00:00Z"),
            rec("app:latest", Some(D1), "success", "2026-01-01T00:00:00Z"),
        ];
        let plan = plan_rollback(&h, None, None).expect("plan");
        assert_eq!(plan.to_digest, D1);
        assert_eq!(plan.from_digest.as_deref(), Some(D2));
        assert_eq!(plan.retag, "app:latest");
        let argv = rollback_argv(&plan);
        assert_eq!(argv[0], vec!["pull".to_owned(), D1.to_owned()]);
        assert!(
            argv[0][1].contains("@sha256:"),
            "pull source must be immutable · got {:?}",
            argv[0]
        );
        assert_eq!(argv[2], vec!["push".to_owned(), "app:latest".to_owned()]);
    }

    #[test]
    fn a_deployment_recorded_without_a_digest_is_never_rolled_back_to() {
        // docker inspect found no RepoDigests · nothing immutable to return to.
        let h = vec![
            rec("app:latest", Some(D2), "success", "2026-01-02T00:00:00Z"),
            rec("app:latest", None, "success", "2026-01-01T00:00:00Z"),
        ];
        let e = plan_rollback(&h, None, None).expect_err("digest-less row must refuse");
        assert!(e.contains("distinct pinned digest"), "{e}");
    }

    #[test]
    fn a_redeploy_of_the_same_digest_is_skipped_when_choosing_a_target() {
        let h = vec![
            rec("app:latest", Some(D2), "success", "2026-01-03T00:00:00Z"),
            rec("app:latest", Some(D2), "success", "2026-01-02T00:00:00Z"),
            rec("app:latest", Some(D1), "success", "2026-01-01T00:00:00Z"),
        ];
        let plan = plan_rollback(&h, None, None).expect("plan");
        assert_eq!(
            plan.to_digest, D1,
            "rolling back to the live digest is a no-op"
        );
    }

    #[test]
    fn an_explicit_rollback_target_must_still_be_a_digest() {
        let h = vec![rec(
            "app:latest",
            Some(D2),
            "success",
            "2026-01-02T00:00:00Z",
        )];
        let e = plan_rollback(&h, Some("app:v1"), None).expect_err("mutable tag must refuse");
        assert!(e.contains("must pin a digest"), "{e}");
        let plan = plan_rollback(&h, Some(D1), None).expect("digest ref accepted");
        assert_eq!(plan.to_digest, D1);
    }

    #[test]
    fn a_live_deployment_pushed_by_digest_has_no_tag_to_repoint() {
        let h = vec![rec(D2, Some(D2), "success", "2026-01-02T00:00:00Z")];
        let e = plan_rollback(&h, Some(D1), None).expect_err("no mutable pointer exists");
        assert!(e.contains("pass 'tag'"), "{e}");
        let plan = plan_rollback(&h, Some(D1), Some("app:staging")).expect("explicit tag");
        assert_eq!(plan.retag, "app:staging");
    }

    #[test]
    fn a_diff_with_no_prior_tag_says_so_rather_than_returning_empty() {
        let e = describe_outcome(
            false,
            "",
            "fatal: No names found, cannot describe anything.",
        )
        .expect_err("untagged repo must refuse");
        assert!(e.contains("no git tag found"), "{e}");
        assert!(
            e.contains("release_create"),
            "must name the way forward · {e}"
        );
    }

    #[test]
    fn a_failed_git_invocation_is_never_reported_as_an_empty_diff() {
        let e = log_outcome(false, Some(128), "", "fatal: bad revision 'v9..HEAD'")
            .expect_err("non-zero git exit must refuse");
        assert!(e.contains("128"), "{e}");
        assert!(e.contains("bad revision"), "{e}");
        // Regression guard: success with no output is the only empty commit list.
        assert_eq!(
            log_outcome(true, Some(0), "", "").expect("clean"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_diff_parses_one_commit_per_line() {
        let out = "abc123 feat: one\ndef456 fix: two\n";
        let commits = log_outcome(true, Some(0), out, "").expect("parse");
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], "abc123 feat: one");
    }

    #[test]
    fn a_describe_failure_that_is_not_about_tags_is_not_relabelled() {
        let e = describe_outcome(false, "", "fatal: not a git repository").expect_err("refuse");
        assert!(e.contains("not a git repository"), "{e}");
    }

    #[test]
    fn canary_and_blue_green_refuse_rather_than_report_a_split() {
        for a in ["canary", "blue_green"] {
            let r = no_router(a);
            assert!(!r.ok, "{a} must not report success");
            assert!(r.error.unwrap_or_default().contains("traffic router"));
        }
    }
}
