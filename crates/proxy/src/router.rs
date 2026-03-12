use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use deadpool_postgres::Pool;
use pingora::prelude::*;
use tracing::{debug, info, warn};

use crate::cache::RouteCache;
use crate::db::lookup_route;

/// Minimum seconds between traffic pulse publishes for the same slug.
/// Keeps NATS volume low — one pulse per 30s per active service is enough
/// for the daemon's idle checker (which runs every 60s).
const PULSE_DEBOUNCE_SECS: u64 = 30;

/// Per-request context — carries the resolved slug so downstream hooks
/// (e.g. `fail_to_connect`) can act on it without re-parsing headers.
pub struct RequestCtx {
    slug: Option<String>,
}

pub struct MachineRouter {
    pub pool:         Arc<Pool>,
    pub api_upstream: String,
    pub cache:        RouteCache,
    /// Optional NATS client for publishing traffic pulses.
    /// None in environments where NATS is unavailable — routing still works.
    pub nats:         Option<Arc<async_nats::Client>>,
    /// Per-slug debounce map: last time a pulse was published for this slug.
    pub pulse_cache:  Arc<RwLock<HashMap<String, Instant>>>,
}

#[async_trait]
impl ProxyHttp for MachineRouter {
    type CTX = RequestCtx;
    fn new_ctx(&self) -> RequestCtx {
        RequestCtx { slug: None }
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut RequestCtx,
    ) -> Result<Box<HttpPeer>> {
        let host = session
            .get_header("Host")
            .and_then(|h| h.to_str().ok())
            .or_else(|| session.req_header().uri.host())
            .unwrap_or("unknown");

        let host_clean = host.split(':').next().unwrap_or(host);
        // "myapp.liquidmetal.dev" → "myapp"
        let slug = host_clean.split('.').next().unwrap_or(host_clean);

        // Stash slug in context for fail_to_connect.
        ctx.slug = Some(slug.to_string());

        // Publish a traffic pulse (debounced) so the daemon can track
        // last_request_at and enforce the Metal idle timeout.
        self.maybe_publish_pulse(slug);

        // Fast path: in-memory cache hit — no DB round-trip.
        if let Some(addr) = self.cache.read().unwrap_or_else(|e| e.into_inner()).get(slug).cloned() {
            debug!(slug, addr, "routing (cache)");
            return Ok(Box::new(HttpPeer::new(addr, false, String::new())));
        }

        // Slow path: DB lookup, then populate cache for subsequent requests.
        let addr = match lookup_route(&self.pool, slug).await {
            Ok(Some(record)) => match record.upstream_addr {
                Some(addr) => {
                    info!(slug, addr, engine = record.engine, "routing (db)");
                    self.cache.write().unwrap_or_else(|e| e.into_inner()).insert(slug.to_string(), addr.clone());
                    addr
                }
                None => {
                    warn!(slug, "no upstream_addr — service still provisioning, falling back to API");
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

    /// Called when the proxy fails to connect to the upstream.
    /// Evicts the slug from the route cache so the next request does a fresh
    /// DB lookup (which will reflect status='crashed' / cleared upstream_addr
    /// set by the daemon's crash watcher).
    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut RequestCtx,
        e: Box<Error>,
    ) -> Box<Error> {
        if let Some(slug) = &ctx.slug {
            warn!(slug, error = %e, "upstream connect failed — evicting route cache");
            self.cache.write().unwrap_or_else(|e| e.into_inner()).remove(slug);
        }
        e
    }
}

/// Sweep interval for the pulse debounce cache. Entries older than
/// 2× the debounce interval are evicted to prevent unbounded growth
/// from deleted services accumulating stale entries.
const PULSE_SWEEP_INTERVAL_SECS: u64 = 300; // 5 minutes
const PULSE_STALE_THRESHOLD_SECS: u64 = PULSE_DEBOUNCE_SECS * 2;

/// Spawn a background thread that periodically sweeps stale entries
/// from the pulse debounce cache. Runs on its own thread since
/// Pingora's main loop is synchronous.
pub fn start_pulse_sweeper(pulse_cache: Arc<RwLock<HashMap<String, Instant>>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(PULSE_SWEEP_INTERVAL_SECS));
            let mut map = pulse_cache.write().unwrap_or_else(|e| e.into_inner());
            let before = map.len();
            map.retain(|_, ts| ts.elapsed().as_secs() < PULSE_STALE_THRESHOLD_SECS);
            let evicted = before - map.len();
            if evicted > 0 {
                tracing::debug!(evicted, remaining = map.len(), "pulse debounce cache swept");
            }
        }
    });
}

impl MachineRouter {
    /// Publish a `platform.traffic_pulse` event for `slug`, but at most once
    /// every `PULSE_DEBOUNCE_SECS` seconds per slug. Spawns a fire-and-forget
    /// task so it never blocks the routing hot path.
    fn maybe_publish_pulse(&self, slug: &str) {
        let Some(nats) = &self.nats else { return };

        // Single write lock for atomic check-and-update (avoids TOCTOU race).
        let should_publish = {
            let mut map = self.pulse_cache.write().unwrap_or_else(|e| e.into_inner());
            let stale = map.get(slug)
                .map(|t| t.elapsed().as_secs() >= PULSE_DEBOUNCE_SECS)
                .unwrap_or(true);
            if stale {
                map.insert(slug.to_string(), Instant::now());
            }
            stale
        };

        if !should_publish {
            return;
        }

        let nats  = nats.clone();
        let slug  = slug.to_string();
        tokio::spawn(async move {
            use common::events::{TrafficPulseEvent, SUBJECT_TRAFFIC_PULSE};
            if let Ok(payload) = serde_json::to_vec(&TrafficPulseEvent { slug }) {
                nats.publish(SUBJECT_TRAFFIC_PULSE, payload.into()).await.ok();
            }
        });
    }
}
