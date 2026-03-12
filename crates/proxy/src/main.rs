use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use pingora::prelude::*;

use common::config::{env_or, require_env};
use proxy::{cache, db, router::{self, MachineRouter}};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "proxy=info".into()),
        )
        .init();

    let db_url       = require_env("DATABASE_URL")?;
    let nats_url     = env_or("NATS_URL",     "nats://127.0.0.1:4222");
    let api_upstream = env_or("API_UPSTREAM", "127.0.0.1:3000");
    let bind_addr    = env_or("BIND_ADDR",    "0.0.0.0:80");

    let pool        = Arc::new(db::build_pool(&db_url)?);
    let route_cache = cache::new();

    // Bulk-populate route cache before accepting traffic. Prevents cold-start
    // storm where 10k slugs all miss cache simultaneously.
    cache::warm(&route_cache, &pool);

    // NATS subscriber keeps the route cache warm and handles route eviction
    // on service stop. Reconnects automatically on disconnect.
    cache::start_subscriber(route_cache.clone(), nats_url.clone());

    // Periodic DB reconciliation catches routes missed by NATS (daemon crash,
    // NATS blip, proxy restart while NATS was down). Runs every 60s.
    cache::start_reconciler(route_cache.clone(), pool.clone());

    // Connect NATS for outbound traffic pulse publishes. Uses a dedicated tokio
    // runtime since Pingora's main() is synchronous. Optional — routing degrades
    // gracefully (no idle timeout) if NATS is unavailable.
    let nats_client = {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { common::config::nats_connect(&nats_url).await.ok() })
            .map(Arc::new)
    };
    if nats_client.is_none() {
        tracing::warn!(%nats_url, "proxy NATS connect failed — idle timeout pulses disabled");
    }

    let pulse_cache = Arc::new(RwLock::new(HashMap::new()));

    // Sweep stale entries from the pulse debounce cache every 5 minutes
    // to prevent unbounded growth from deleted services.
    router::start_pulse_sweeper(pulse_cache.clone());

    let mut server = Server::new(None).context("creating Pingora server")?;
    server.bootstrap();

    let r = MachineRouter {
        pool,
        api_upstream,
        cache:       route_cache,
        nats:        nats_client,
        pulse_cache,
    };
    let mut svc = http_proxy_service(&server.configuration, r);
    svc.add_tcp(&bind_addr);
    server.add_service(svc);

    tracing::info!(%bind_addr, "liquid-metal-proxy listening");
    server.run_forever();
}
