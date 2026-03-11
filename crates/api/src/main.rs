// anyhow::Context allows us to attach ".context('msg')" to any Result.
// If it fails, the error message will be "msg: [underlying error]", making debugging trivial.
use anyhow::{Context, Result};
use api::{AppState, build_router};
use common::{
    Features,
    config::{env_or, require_env},
};
// Arc (Atomic Reference Counted) is CRUCIAL in Rust web servers. We'll explain it below.
use std::sync::Arc;
// tracing is the standard for structured logging in Rust (replacing println!)
use tracing_subscriber::EnvFilter;

// #[tokio::main] is a macro that sets up the Async Runtime.
// Rust does NOT have a built-in async runtime like C# or Go.
// You have to bring your own (Tokio is the industry standard).
// This macro rewrites your `main` function to boot up a multi-threaded executor
// before running your code.
#[tokio::main]
async fn main() -> Result<()> {
    // ── Logging Setup ────────────────────────────────────────────────────────
    // Initializes the logging system. It reads the RUST_LOG environment variable.
    // If not set, it defaults to showing "info" level logs for this specific API crate.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "api=info".into()))
        .init();

    // ── Migrate-only mode ────────────────────────────────────────────────────
    // If the user runs `liquid-metal-api --migrate`, we execute this block.
    if std::env::args().any(|a| a == "--migrate") {
        let db_url = require_env("DATABASE_URL")?;
        let migrate_url = env_or("MIGRATIONS_DATABASE_URL", &db_url);

        // The .await here means: "Pause this specific Tokio task, let the CPU do other things,
        // and wake this task back up when the database responds."
        api::migrations::run_with_url(&migrate_url)
            .await
            .context("running migrations")?;

        tracing::info!("migrations complete — exiting");
        // We return early. The web server never boots.
        return Ok(());
    }

    let features = Features::from_env();
    features.log_summary();

    let db_url = require_env("DATABASE_URL")?;
    let internal_secret = require_env("INTERNAL_SECRET")?;

    // ── OIDC Configuration (Block Expression) ────────────────────────────────
    // This is a "block expression". It runs the code inside the `{ }` and assigns
    // the final returned tuple to the 5 variables on the left.
    // This is a great Rust pattern for keeping temporary variables (like `issuer`)
    // from polluting the rest of the function.
    let (oidc_client_id, oidc_device_auth_url, oidc_token_url, oidc_userinfo_url, oidc_revoke_url) = {
        let issuer = std::env::var("OIDC_ISSUER")
            .or_else(|_| std::env::var("ZITADEL_DOMAIN"))
            .ok();

        let client_id = std::env::var("OIDC_CLIENT_ID")
            .or_else(|_| std::env::var("ZITADEL_CLIENT_ID"))
            .ok();

        // If an issuer URL was provided, we hit their API to automatically discover
        // the rest of the required URLs (this is standard OIDC behavior).
        if let Some(issuer) = issuer {
            let client_id = client_id.context(
                "OIDC_CLIENT_ID (or ZITADEL_CLIENT_ID) is required when using OIDC_ISSUER",
            )?;

            // .await pauses the thread while the HTTP request happens.
            let disc = oidc_discover(&issuer)
                .await
                .with_context(|| format!("OIDC discovery failed for {issuer}"))?;

            tracing::info!(issuer, "OIDC endpoints discovered");
            // This tuple is the final value of the block expression
            (
                client_id,
                disc.device_authorization_endpoint,
                disc.token_endpoint,
                disc.userinfo_endpoint,
                disc.revocation_endpoint,
            )
        } else {
            // If no issuer, the user MUST provide all URLs manually via environment variables.
            (
                require_env("OIDC_CLIENT_ID")?,
                require_env("OIDC_DEVICE_AUTH_URL")?,
                require_env("OIDC_TOKEN_URL")?,
                require_env("OIDC_USERINFO_URL")?,
                std::env::var("OIDC_REVOKE_URL").ok(),
            )
        }
    };

    let migrate_url = env_or("MIGRATIONS_DATABASE_URL", &db_url);
    // NATS is a hyper-fast message broker (often used instead of Kafka or RabbitMQ)
    let nats_url = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let bind = env_or("BIND_ADDR", "0.0.0.0:7070");
    let bucket = env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts");

    // ── Run migrations on startup ────────────────────────────────────────────
    api::migrations::run_with_url(&migrate_url)
        .await
        .context("running migrations")?;

    // ── App Postgres pool (deadpool) ─────────────────────────────────────────
    // In Rust web servers, you do NOT create a new database connection for every HTTP request.
    // That is too slow. Instead, you create a "Pool" of connections (here, max 16) when the server starts.
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(mgr)
        .max_size(16)
        .build()
        .context("building postgres pool")?;

    // ── Connect to NATS JetStream ────────────────────────────────────────────
    let nc = async_nats::connect(&nats_url)
        .await
        .context("connecting to NATS")?;
    // nc.clone() does NOT copy the network connection. It just creates another
    // reference to the same underlying connection pool.
    let js = async_nats::jetstream::new(nc.clone());
    api::nats::ensure_stream(&js).await?;

    // ── Connect to S3 ────────────────────────────────────────────────────────
    let s3 = api::storage::build_client().await;

    // ── The AppState and Arc (CRITICAL RUST CONCEPT) ─────────────────────────
    // A web server receives thousands of concurrent HTTP requests.
    // Each request is handled by a separate Tokio task (think of it like a thread).
    // Every single one of those tasks needs access to the Database Pool, the NATS client, etc.
    //
    // BUT Rust's ownership rules say: "Only ONE variable can own data at a time."
    //
    // Arc stands for "Atomic Reference Counted".
    // It places the `AppState` on the Heap, and gives us a cheap pointer to it.
    // When a new HTTP request comes in, Axum will run `state.clone()`.
    // Because it's wrapped in an Arc, it doesn't copy the database pool; it just
    // increments a counter saying "Now 2 tasks are looking at this data."
    // When a request finishes, the counter drops. When it hits 0, the memory is freed.
    // This allows MULTIPLE threads to safely share the same memory.
    let state = Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secret,
        oidc_client_id,
        oidc_device_auth_url,
        oidc_token_url,
        oidc_userinfo_url,
        oidc_revoke_url,
        features,
    });

    // build_router takes our Arc<AppState> and attaches it to our HTTP endpoints.
    let app = build_router(state);

    tracing::info!(%bind, "api listening");
    // We ask the OS to open a TCP port (7070) to listen for traffic.
    let listener = tokio::net::TcpListener::bind(&bind).await?;

    // axum::serve starts the actual web server loop.
    // .with_graceful_shutdown() is a best practice. It tells Axum:
    // "Keep running until the shutdown_signal() function finishes."
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("api exited cleanly");
    Ok(())
}

// ── OIDC Discovery Helper ────────────────────────────────────────────────────
// This function demonstrates how to use `reqwest` (the standard HTTP client in Rust).
async fn oidc_discover(issuer: &str) -> Result<OidcDiscovery> {
    let issuer = issuer.trim_end_matches('/');
    // Prepend https:// if the user omitted the scheme
    let issuer = if issuer.starts_with("http://") || issuer.starts_with("https://") {
        issuer.to_string()
    } else {
        format!("https://{issuer}")
    };
    let url = format!("{issuer}/.well-known/openid-configuration");

    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .context("HTTP request to OIDC discovery URL")?
        // error_for_status() converts 404s or 500s into actual Rust Errors.
        .error_for_status()
        .context("OIDC discovery returned non-2xx")?
        // .json() automatically deserializes the response body into our OidcDiscovery struct!
        .json::<OidcDiscovery>()
        .await
        .context("deserializing OIDC discovery document")?;

    Ok(resp)
}

// ── The OIDC Struct ──────────────────────────────────────────────────────────
// Notice we only define the 4 fields we actually care about.
// The real OIDC discovery document has ~50 fields, but `serde::Deserialize`
// will safely ignore all the ones we didn't list here.
#[derive(serde::Deserialize)]
struct OidcDiscovery {
    device_authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    // #[serde(default)] means: "If the server doesn't send a revocation_endpoint,
    // don't crash. Just set this Option to None."
    #[serde(default)]
    revocation_endpoint: Option<String>,
}

// ── Graceful Shutdown Handler ────────────────────────────────────────────────
// This async function just sits here and waits for the OS to tell it to die.
// When you press Ctrl+C, or Docker sends a SIGTERM, this function completes,
// which signals Axum to stop accepting new HTTP requests and shut down cleanly.
async fn shutdown_signal() {
    #[cfg(unix)] // This block only compiles if we are on Linux/macOS
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");

        // tokio::select! waits for MULTIPLE async things at once.
        // Whichever one finishes FIRST "wins", and the others are cancelled.
        tokio::select! {
            _ = sigterm.recv()          => tracing::info!("SIGTERM received"),
            _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C received"),
        }
    }

    #[cfg(not(unix))] // This block compiles on Windows
    tokio::signal::ctrl_c().await.expect("Ctrl-C handler");
}
