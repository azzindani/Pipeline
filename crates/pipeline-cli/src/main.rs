//! `pipeline` binary · single entry point for CLI · MCP server · dev daemon.

use clap::{Parser, Subcommand};
use pipeline_core::{StageProfile, StageStatus};
use pipeline_memory::{Memory, NewRun};
use pipeline_stages::Runner;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "pipeline",
    version,
    about = "Local-first CI/CD + MCP for any coding agent",
    long_about = "Pipeline runs the entire software lifecycle (init · code · test · deploy · maintain) \
                  for any coding agent that speaks MCP. See CLAUDE.md and PLAN.md for design."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the MCP server on stdio · agents connect here
    Mcp,
    /// Run a stage profile against the current project
    Run {
        /// Profile: fast · full · preflight · confirm
        #[arg(default_value = "fast")]
        profile: String,
    },
    /// Start MCP server + filesystem watcher together (primary dev command)
    Dev,
    /// Watch filesystem and re-run stages on change
    Watch,
    /// Initialize a new project (scaffold + GitHub repo + branch protection)
    Init {
        name: String,
        /// Project type · web-spa · mcp-server-rust · cli-rust · etc. · see PLAN.md §4.4
        #[arg(long = "type")]
        kind: Option<String>,
    },
    /// Print the latest run + handover packet
    Report,
    /// Print the loaded pipeline.yaml config (debug)
    Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Mcp => pipeline_mcp::serve_stdio().await?,
        Command::Run { profile } => run_profile(&profile).await?,
        Command::Dev => println!("[stub] dev (mcp + watch) · POC week 1"),
        Command::Watch => println!("[stub] watch · POC week 1"),
        Command::Init { name, kind } => {
            println!("[stub] init project '{name}' (type: {kind:?}) · MVP week 4");
        }
        Command::Report => report().await?,
        Command::Config => print_config()?,
    }
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

async fn run_profile(profile_arg: &str) -> anyhow::Result<()> {
    let Some(profile) = StageProfile::parse(profile_arg) else {
        anyhow::bail!("unknown profile '{profile_arg}' · valid: fast · full · preflight · confirm");
    };

    let project_root = std::env::current_dir()?;
    let cfg = load_config(&project_root)?;
    let project_id = cfg.project.clone();
    let project_name = cfg.project.clone();
    let stack = cfg.stack.runtime.clone();

    let mem = open_memory(&project_root).await?;
    mem.upsert_project(&project_id, &project_name, &stack)
        .await?;

    let session = mem
        .lock_session(
            &project_id,
            Some("pipeline-cli"),
            Some(&format!("run {profile_arg}")),
        )
        .await
        .or_else(|e| match e {
            pipeline_memory::MemoryError::LockHeld(_) => {
                eprintln!("warn: session lock held; continuing without new lock");
                Ok::<_, pipeline_memory::MemoryError>(pipeline_memory::SessionLock {
                    project_id: project_id.clone(),
                    session_id: String::new(),
                    locked_at: pipeline_memory::now_rfc3339(),
                    agent_id: None,
                })
            }
            other => Err(other),
        })?;

    let ctx = pipeline_core::StageContext {
        project_root,
        config: cfg,
    };
    println!("running profile '{profile_arg}'");
    let summary = Runner::run_profile(profile, &ctx).await;

    for r in &summary.results {
        println!(
            "  {} · {} · {} ms",
            r.stage.as_str(),
            status_label(r.status),
            r.duration.as_millis()
        );
        let session_ref = if session.session_id.is_empty() {
            None
        } else {
            Some(session.session_id.as_str())
        };
        let failure_json = r
            .failure
            .as_ref()
            .and_then(|f| serde_json::to_string(f).ok());
        mem.log_run(&NewRun {
            project_id: &project_id,
            session_id: session_ref,
            profile: profile_arg,
            stage: r.stage.as_str(),
            status: status_label(r.status),
            duration_ms: r.duration.as_millis(),
            triggered_by: Some("pipeline-cli"),
            commit_sha: None,
            stdout: Some(&r.stdout),
            stderr: Some(&r.stderr),
            failure_json: failure_json.as_deref(),
        })
        .await?;
    }

    println!(
        "overall: {} · {} ms",
        status_label(summary.overall),
        summary.total_duration_ms
    );

    if !session.session_id.is_empty() {
        let outcome = match summary.overall {
            StageStatus::Pass => "pass",
            _ => "fail",
        };
        let _ = mem.end_session(&session.session_id, outcome, None).await;
    }

    if matches!(summary.overall, StageStatus::Fail | StageStatus::Error) {
        std::process::exit(1);
    }
    Ok(())
}

async fn report() -> anyhow::Result<()> {
    let project_root = std::env::current_dir()?;
    let cfg = load_config(&project_root)?;
    let mem = open_memory(&project_root).await?;
    let pack = mem.handover(&cfg.project).await?;
    let json = serde_json::to_string_pretty(&pack)?;
    println!("{json}");
    Ok(())
}

fn print_config() -> anyhow::Result<()> {
    let cfg = load_config(&std::env::current_dir()?)?;
    println!("{cfg:#?}");
    Ok(())
}

fn load_config(root: &Path) -> anyhow::Result<pipeline_config::PipelineConfig> {
    let path = root.join("pipeline.yaml");
    if !path.exists() {
        anyhow::bail!("pipeline.yaml not found in {}", root.display());
    }
    Ok(pipeline_config::PipelineConfig::load(&path)?)
}

async fn open_memory(root: &Path) -> anyhow::Result<Memory> {
    let db_path: PathBuf = root.join(".pipeline").join("memory.db");
    Ok(Memory::open(&db_path).await?)
}

const fn status_label(s: StageStatus) -> &'static str {
    match s {
        StageStatus::Pass => "pass",
        StageStatus::Fail => "fail",
        StageStatus::Skipped => "skipped",
        StageStatus::Error => "error",
    }
}
