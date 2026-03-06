use anyhow::Result;
use proxy::db;
use proxy::router;
use common::config::{env_or, require_env};
use pingora::prelude::*;
use std::sync::Arc;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "proxy=info".into()),
        )
        .init();

    let db_url      = require_env("DATABASE_URL")?;
    let api_upstream = env_or("API_UPSTREAM", "127.0.0.1:3000");
    let bind_addr   = env_or("BIND_ADDR", "0.0.0.0:80");

    let pool = Arc::new(db::build_pool(&db_url)?);

    let mut server = Server::new(None).unwrap();
    server.bootstrap();

    let r = router::MachineRouter { pool, api_upstream };
    let mut svc = http_proxy_service(&server.configuration, r);
    svc.add_tcp(&bind_addr);
    server.add_service(svc);

    tracing::info!(%bind_addr, "machinename-proxy listening");
    server.run_forever();
}
