mod machine;

use anyhow::Result;
use clap::{Parser, Subcommand};
use common::config::env_or;

#[derive(Parser)]
#[command(name = "plat", about = "Machine Name CLI — deploy in three commands")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a machine.toml in the current directory
    Init {
        #[arg(long, default_value = "metal")]
        engine: String,
    },
    /// Deploy the current project to the platform
    Deploy,
    /// Show the status of the deployed service
    Status,
    /// Stream logs from the deployed service (stub)
    Logs,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "plat=info".into()),
        )
        .compact()
        .init();

    let api = env_or("MACHINENAME_API", "http://127.0.0.1:3000");
    let cli = Cli::parse();

    match cli.cmd {
        Command::Init { engine } => machine::init(&engine),
        Command::Deploy             => machine::deploy(&api).await,
        Command::Status             => machine::status(&api).await,
        Command::Logs               => machine::logs(&api).await,
    }
}
