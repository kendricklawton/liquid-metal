mod client;
mod commands;
mod config;
mod context;
mod docker;
mod oidc;
mod output;
mod table;
mod toml_config;

use clap::{Args, CommandFactory, Parser, Subcommand};
use config::Config;
use output::OutputMode;

#[derive(Parser)]
#[command(name = "flux", about = "flux — liquid-metal CLI", version)]
struct Cli {
    #[arg(long, hide = true, env = "API_URL")]
    api_url: Option<String>,
    #[arg(long, hide = true, env = "FLUX_TOKEN")]
    token: Option<String>,
    /// Output as JSON (for scripting/CI)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Log in via browser
    Login {
        /// Invite code
        #[arg(long, short)]
        invite: Option<String>,
    },
    /// Log out
    Logout,
    /// Show current user and workspaces
    Whoami,
    /// List services
    Services,
    /// Stream logs: flux logs <SERVICE>
    Logs {
        /// Service slug or UUID (see: flux services)
        service: String,
        /// Number of recent log lines to fetch
        #[arg(long, default_value_t = 100)]
        limit: i32,
        /// Follow log output (poll every 2s)
        #[arg(long, short)]
        follow: bool,
    },
    /// Stop a service: flux stop <SERVICE>
    Stop {
        /// Service slug or UUID (see: flux services)
        service: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Restart a service: flux restart <SERVICE>
    Restart {
        /// Service slug or UUID (see: flux services)
        service: String,
    },
    /// Delete a service: flux delete <SERVICE>
    Delete {
        /// Service slug or UUID (see: flux services)
        service: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Init a project in current dir (writes liquid-metal.toml)
    Init {
        /// Override the detected service name
        #[arg(long)]
        name: Option<String>,
        /// Engine: "liquid" (Wasm) or "metal" (Firecracker microVM)
        #[arg(long)]
        engine: Option<String>,
    },
    /// Build locally without deploying
    Build,
    /// Build and deploy — get a live URL
    Deploy {
        /// Skip ELF compatibility check (glibc vs musl detection)
        #[arg(long)]
        skip_elf_check: bool,
    },
    /// Switch workspace: flux switch <WORKSPACE>
    Switch {
        /// Workspace slug or UUID (omit to list workspaces)
        workspace: Option<String>,
    },
    /// Manage env vars on a service
    Env(EnvArgs),
    /// Manage custom domains
    Domains(DomainsArgs),
    /// Open service URL: flux open <SERVICE>
    Open {
        /// Service slug or UUID (see: flux services)
        service: String,
    },
    /// Set run mode: flux scale <SERVICE> --mode <MODE>
    Scale {
        /// Service slug or UUID (see: flux services)
        service: String,
        /// Run mode: "serverless" or "always-on"
        #[arg(long)]
        mode: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Release history: flux releases <SERVICE>
    #[command(name = "releases")]
    Releases {
        /// Service slug or UUID (see: flux services)
        service: String,
    },
    /// Rollback a deploy: flux rollback <SERVICE>
    Rollback {
        /// Service slug or UUID (see: flux services)
        service: String,
        /// Specific deploy ID to rollback to (default: previous)
        #[arg(long)]
        deploy_id: Option<String>,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
    /// Shell completions (bash, zsh, fish)
    Completions {
        /// Target shell
        shell: clap_complete::Shell,
    },
}

#[derive(Args)]
struct EnvArgs {
    #[command(subcommand)]
    command: EnvCommands,
}

#[derive(Subcommand)]
enum EnvCommands {
    /// List environment variables
    List {
        /// Service slug or UUID
        service: String,
    },
    /// Set environment variables (KEY=VALUE pairs)
    Set {
        /// Service slug or UUID
        service: String,
        /// KEY=VALUE pairs
        #[arg(required = true, num_args = 1..)]
        vars: Vec<String>,
    },
    /// Remove environment variables
    Unset {
        /// Service slug or UUID
        service: String,
        /// Keys to remove
        #[arg(required = true, num_args = 1..)]
        keys: Vec<String>,
    },
}

#[derive(Args)]
struct DomainsArgs {
    #[command(subcommand)]
    command: DomainsCommands,
}

#[derive(Subcommand)]
enum DomainsCommands {
    /// List custom domains for a service
    List {
        /// Service slug or UUID
        service: String,
    },
    /// Add a custom domain to a service
    Add {
        /// Service slug or UUID
        service: String,
        /// Domain name (e.g. example.com)
        domain: String,
    },
    /// Verify DNS for a custom domain
    Verify {
        /// Service slug or UUID
        service: String,
        /// Domain name
        domain: String,
    },
    /// Remove a custom domain
    Remove {
        /// Service slug or UUID
        service: String,
        /// Domain name
        domain: String,
        /// Skip confirmation prompt
        #[arg(long, short)]
        yes: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Let clap handle --help and --version normally.
            if e.kind() == clap::error::ErrorKind::DisplayHelp
                || e.kind() == clap::error::ErrorKind::DisplayVersion
            {
                e.exit();
            }
            // Improve clap's "required arguments not provided" for <SERVICE> args.
            let msg = e.to_string();
            if msg.contains("<SERVICE>") {
                eprintln!("error: missing <SERVICE> — pass a service slug or UUID\n");
                eprintln!("  run `flux services` to see your services and their slugs");
                std::process::exit(1);
            }
            e.exit();
        }
    };
    let mut config = Config::load().unwrap_or_default();
    let output = if cli.json {
        OutputMode::Json
    } else {
        OutputMode::Human
    };

    if let Some(url) = cli.api_url {
        config.api_url = Some(url);
    }
    if let Some(token) = cli.token {
        config.token = Some(token);
    }

    let result = match cli.command {
        Commands::Login { invite } => commands::auth::login::run(&mut config, invite, output).await,
        Commands::Logout => commands::auth::logout::run(&mut config, output).await,
        Commands::Whoami => commands::auth::whoami::run(&config, output).await,
        Commands::Services => commands::service::status::run(&config, output).await,
        Commands::Logs {
            service,
            limit,
            follow,
        } => commands::service::logs::run(&config, &service, limit, follow, output).await,
        Commands::Stop { service, yes } => {
            commands::service::stop::run(&config, &service, yes, output).await
        }
        Commands::Restart { service } => {
            commands::service::restart::run(&config, &service, output).await
        }
        Commands::Delete { service, yes } => {
            commands::service::delete::run(&config, &service, yes, output).await
        }
        Commands::Init { name, engine } => commands::init::run(&config, name, engine, output).await,
        Commands::Build => commands::service::build::run(output).await,
        Commands::Deploy { skip_elf_check } => {
            commands::service::deploy::run(&config, output, skip_elf_check).await
        }
        Commands::Switch { workspace } => match workspace {
            Some(slug_or_id) => {
                commands::workspace::run_use(&mut config, &slug_or_id, output).await
            }
            None => commands::workspace::run_list(&config, output).await,
        },
        Commands::Env(args) => match args.command {
            EnvCommands::List { service } => {
                commands::service::env::run_list(&config, &service, output).await
            }
            EnvCommands::Set { service, vars } => {
                commands::service::env::run_set(&config, &service, &vars, output).await
            }
            EnvCommands::Unset { service, keys } => {
                commands::service::env::run_unset(&config, &service, &keys, output).await
            }
        },
        Commands::Domains(args) => match args.command {
            DomainsCommands::List { service } => {
                commands::service::domains::run_list(&config, &service, output).await
            }
            DomainsCommands::Add { service, domain } => {
                commands::service::domains::run_add(&config, &service, &domain, output).await
            }
            DomainsCommands::Verify { service, domain } => {
                commands::service::domains::run_verify(&config, &service, &domain, output).await
            }
            DomainsCommands::Remove {
                service,
                domain,
                yes,
            } => {
                commands::service::domains::run_remove(&config, &service, &domain, yes, output)
                    .await
            }
        },
        Commands::Open { service } => commands::service::open::run(&config, &service, output).await,
        Commands::Scale { service, mode, yes } => {
            commands::service::scale::run(&config, &service, &mode, yes, output).await
        }
        Commands::Releases { service } => {
            commands::service::deploys::run_list(&config, &service, output).await
        }
        Commands::Rollback {
            service,
            deploy_id,
            yes,
        } => {
            commands::service::rollback::run(&config, &service, deploy_id.as_deref(), yes, output)
                .await
        }
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "flux", &mut std::io::stdout());
            Ok(())
        }
    };

    if let Err(e) = result {
        output::print_error(output, &e);
        std::process::exit(1);
    }
}
