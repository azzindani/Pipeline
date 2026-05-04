//! `pipeline_deploy` handler · CI/CD generation · deploy · health · release.
//!
//! Day-6 wires: cicd_generate, target, smoke, health, release_create.
//! Other 4 (rollback, canary, blue_green, diff) return not_implemented.

#![allow(clippy::doc_markdown)]

use crate::server::ServerState;
use crate::tools::{ToolName, ToolRequest, ToolResponse};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::process::Command;

pub async fn handle(req: ToolRequest, _state: Arc<ServerState>) -> ToolResponse {
    match req.action.as_str() {
        "cicd_generate" => cicd_generate(&req.args).await,
        "target" => target(&req.args).await,
        "smoke" | "health" => health(&req.args).await,
        "release_create" => release_create(&req.args).await,
        "rollback" | "canary" | "blue_green" | "diff" => {
            ToolResponse::not_implemented(ToolName::Deploy, &req.action)
        }
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

async fn target(args: &Value) -> ToolResponse {
    let env = args.get("env").and_then(Value::as_str).unwrap_or("staging");
    let image = args.get("image").and_then(Value::as_str);
    if let Some(img) = image {
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
        return ToolResponse {
            ok,
            data: json!({
                "env": env,
                "image": img,
                "exit_code": output.status.code().unwrap_or(-1),
                "stderr": String::from_utf8_lossy(&output.stderr).trim().to_owned(),
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
        };
    }
    // No image · placeholder: ssh+compose deploy lands in MVP.
    ToolResponse {
        ok: true,
        data: json!({"env": env, "note": "image not provided · ssh+compose deploy lands MVP week 5"}),
        next_suggested: vec!["pipeline_docker.image_promote".into()],
        memory_refs: vec![],
        error: None,
    }
}

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
