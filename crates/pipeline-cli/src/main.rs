//! `pipeline` binary · single entry point for CLI · MCP server · dev daemon.

use clap::{Parser, Subcommand};
use pipeline_core::StageProfile;

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
    /// Open the last report
    Report,
    /// Print the loaded pipeline.yaml config (debug)
    Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Mcp => {
            pipeline_mcp::serve_stdio().await?;
        }
        Command::Run { profile } => run_stub(&profile)?,
        Command::Dev => println!("[stub] dev (mcp + watch) · POC week 1"),
        Command::Watch => println!("[stub] watch · POC week 1"),
        Command::Init { name, kind } => {
            println!("[stub] init project '{name}' (type: {kind:?}) · MVP week 4");
        }
        Command::Report => println!("[stub] report · POC week 3"),
        Command::Config => print_config()?,
    }
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();
}

fn run_stub(profile: &str) -> anyhow::Result<()> {
    let Some(p) = StageProfile::parse(profile) else {
        anyhow::bail!("unknown profile '{profile}' · valid: fast · full · preflight · confirm");
    };
    println!("[stub] would run profile '{profile}' · stages:");
    for s in p.stages() {
        println!("  - {}", s.as_str());
    }
    println!("execution lands in pipeline-stages · see PLAN.md POC week");
    Ok(())
}

fn print_config() -> anyhow::Result<()> {
    let path = std::path::Path::new("pipeline.yaml");
    if !path.exists() {
        anyhow::bail!("pipeline.yaml not found in cwd");
    }
    let cfg = pipeline_config::PipelineConfig::load(path)?;
    println!("{cfg:#?}");
    Ok(())
}
