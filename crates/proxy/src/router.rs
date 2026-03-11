use crate::db::lookup_route;
use async_trait::async_trait;
use deadpool_postgres::Pool;
use pingora::prelude::*;
use std::sync::Arc;
use tracing::{info, warn};

pub struct MachineRouter {
    pub pool: Arc<Pool>,
    /// Fallback upstream when no DB record exists (e.g. API itself)
    pub api_upstream: String,
}

#[async_trait]
impl ProxyHttp for MachineRouter {
    type CTX = ();
    fn new_ctx(&self) {}

    async fn upstream_peer(
        &self,
        session: &mut Session,
        _ctx: &mut (),
    ) -> Result<Box<HttpPeer>> {
        let host = session
            .get_header("Host")
            .and_then(|h| h.to_str().ok())
            .or_else(|| session.req_header().uri.host())
            .unwrap_or("unknown");

        let host_clean = host.split(':').next().unwrap_or(host);
        // "myapp.machinename.dev" → "myapp"
        let slug = host_clean.split('.').next().unwrap_or(host_clean);

        let addr = match lookup_route(&self.pool, slug).await {
            Ok(Some(record)) => match record.upstream_addr {
                Some(addr) => {
                    info!(slug, addr, engine = record.engine, "routing");
                    addr
                }
                None => {
                    warn!(slug, "no upstream_addr, falling back to API");
                    self.api_upstream.clone()
                }
            },
            Ok(None) => {
                warn!(slug, "unknown slug, falling back to API");
                self.api_upstream.clone()
            }
            Err(e) => {
                warn!(error = %e, slug, "db error, falling back to API");
                self.api_upstream.clone()
            }
        };

        Ok(Box::new(HttpPeer::new(addr, false, String::new())))
    }
}
