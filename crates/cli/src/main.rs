mod client;
mod commands;
mod config;

use clap::{Args, Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(name = "flux", about = "flux — liquid-metal CLI")]
struct Cli {
    #[arg(long, hide = true)]
    api_url: Option<String>,
    #[arg(long)]
    token: Option<String>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate via Zitadel OIDC device flow
    Login {
        #[arg(long, short)]
        invite: Option<String>,
    },
    /// Clear stored credentials
    Logout,
    /// Show the authenticated user
    Whoami,
    /// List services in the active workspace
    Status,
    /// Stream logs for a service
    Logs {
        /// Service slug or UUID
        service: String,
        #[arg(long, default_value_t = 100)]
        limit: i32,
    },
    /// Stop a running service
    Stop {
        /// Service slug or UUID
        service: String,
    },
    /// Restart a service
    Restart {
        /// Service slug or UUID
        service: String,
    },
    /// Create a project and write liquid-metal.toml (auto-detects language)
    Init {
        /// Override the detected service name
        #[arg(long)]
        name: Option<String>,
        /// Engine: "liquid" (Wasm) or "metal" (Firecracker microVM)
        #[arg(long)]
        engine: Option<String>,
    },
    /// Build and deploy the current service
    Deploy,
    /// Manage workspaces
    Workspace(WorkspaceArgs),
    /// Manage projects
    Project(ProjectArgs),
    /// Manage invite codes
    Invite(InviteArgs),
}

#[derive(Args)]
struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceCommands,
}

#[derive(Subcommand)]
enum WorkspaceCommands {
    List,
    Use { slug_or_id: String },
}

#[derive(Args)]
struct ProjectArgs {
    #[command(subcommand)]
    command: ProjectCommands,
}

#[derive(Subcommand)]
enum ProjectCommands {
    List,
    Use { slug_or_id: String },
}

#[derive(Args)]
struct InviteArgs {
    #[command(subcommand)]
    command: InviteCommands,
}

#[derive(Subcommand)]
enum InviteCommands {
    Generate {
        #[arg(long, short, default_value_t = 1)]
        count: u32,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let mut config = Config::load().unwrap_or_default();

    if let Some(url) = cli.api_url {
        config.api_url = Some(url);
    }
    if let Some(token) = cli.token {
        config.token = Some(token);
    }

    let result = match cli.command {
        Commands::Login { invite } => commands::login::run(&mut config, invite).await,
        Commands::Logout => commands::logout::run(&mut config).await,
        Commands::Whoami => commands::whoami::run(&config).await,
        Commands::Status => commands::status::run(&config).await,
        Commands::Logs { service, limit } => {
            commands::logs::run(&config, &service, limit).await
        }
        Commands::Stop { service } => commands::stop::run(&config, &service).await,
        Commands::Restart { service } => commands::restart::run(&config, &service).await,
        Commands::Init { name, engine } => commands::init::run(&config, name, engine).await,
        Commands::Deploy => commands::deploy::run(&config).await,
        Commands::Workspace(args) => match args.command {
            WorkspaceCommands::List => commands::workspace::run_list(&config).await,
            WorkspaceCommands::Use { slug_or_id } => {
                commands::workspace::run_use(&mut config, &slug_or_id).await
            }
        },
        Commands::Project(args) => match args.command {
            ProjectCommands::List => commands::project::run_list(&config).await,
            ProjectCommands::Use { slug_or_id } => {
                commands::project::run_use(&config, &slug_or_id).await
            }
        },
        Commands::Invite(args) => match args.command {
            InviteCommands::Generate { count } => {
                commands::invite::run_generate(&config, count).await
            }
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
