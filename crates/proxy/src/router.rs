use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use deadpool_postgres::Pool;
use pingora::prelude::*;
use tracing::{debug, info, warn};

use crate::cache::{DomainCache, RouteCache};
use crate::db::{lookup_domain, lookup_route};

/// Minimum seconds between traffic pulse publishes for the same slug.
/// Keeps NATS volume low — one pulse per interval per active service is enough
/// for the daemon's idle checker. Override via PULSE_DEBOUNCE_SECS env var.
static PULSE_DEBOUNCE_SECS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    common::config::env_or("PULSE_DEBOUNCE_SECS", "30").parse().unwrap_or(30)
});

/// Platform domain suffix (e.g. "liquidmetal.dev"). Hosts matching this pattern
/// use slug-based routing; all other hosts are looked up as custom domains.
static PLATFORM_DOMAIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    common::config::env_or("PLATFORM_DOMAIN", "liquidmetal.dev")
});

/// Maximum entries in the domain cache before new inserts are skipped.
/// Prevents memory exhaustion from requests with arbitrary hostnames.
/// The reconciler (every 60s) prunes stale entries, making room for new ones.
static DOMAIN_CACHE_MAX: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("DOMAIN_CACHE_MAX", "50000")
        .parse()
        .unwrap_or(50_000)
});

/// Per-request context — carries the resolved slug so downstream hooks
/// (e.g. `fail_to_connect`) can act on it without re-parsing headers.
pub struct RequestCtx {
    slug: Option<String>,
}

pub struct MachineRouter {
    pub pool:         Arc<Pool>,
    pub api_upstream: String,
    pub cache:        RouteCache,
    /// Custom domain → slug cache. Populated on miss from DB.
    pub domain_cache: DomainCache,
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

        // Strip port — handle IPv6 literals like [::1]:8080 by skipping
        // bracketed addresses entirely (they're never valid service slugs).
        let host_clean = if host.starts_with('[') {
            // IPv6 literal — no valid slug, fall through to API.
            ""
        } else {
            host.split(':').next().unwrap_or(host)
        };

        // Strip trailing dot from FQDN (e.g. "myapp.liquidmetal.dev." → "myapp.liquidmetal.dev")
        let host_clean = host_clean.trim_end_matches('.');

        // Determine slug: platform subdomain or custom domain lookup.
        let platform_suffix = PLATFORM_DOMAIN.as_str();
        let dotted_suffix = format!(".{}", platform_suffix);
        let slug = if host_clean == platform_suffix {
            // Bare platform domain (e.g. "liquidmetal.dev") → route to API/dashboard.
            debug!(%host, "bare platform domain, routing to API");
            return Ok(Box::new(HttpPeer::new(self.api_upstream.clone(), false, String::new())));
        } else if host_clean.ends_with(&dotted_suffix) {
            // Platform subdomain: "myapp.liquidmetal.dev" → "myapp"
            let slug = host_clean.split('.').next().unwrap_or(host_clean);
            slug.to_string()
        } else {
            // Custom domain: check domain cache, then DB.
            if let Some(s) = self.domain_cache.read().unwrap_or_else(|e| e.into_inner()).get(host_clean).cloned() {
                s
            } else {
                // DB lookup for custom domain → slug.
                match lookup_domain(&self.pool, host_clean).await {
                    Ok(Some((s, record))) => {
                        {
                            let mut dmap = self.domain_cache.write().unwrap_or_else(|e| e.into_inner());
                            if dmap.len() < *DOMAIN_CACHE_MAX {
                                dmap.insert(host_clean.to_string(), s.clone());
                            } else {
                                tracing::warn!(
                                    domain = host_clean,
                                    cap = *DOMAIN_CACHE_MAX,
                                    "domain cache at capacity — skipping insert (reconciler will make room)"
                                );
                            }
                        }
                        if let Some(addr) = &record.upstream_addr {
                            self.cache.write().unwrap_or_else(|e| e.into_inner()).insert(s.clone(), addr.clone());
                        }
                        s
                    }
                    Ok(None) => {
                        // Not a known custom domain — extract first label as slug (fallback).
                        host_clean.split('.').next().unwrap_or(host_clean).to_string()
                    }
                    Err(e) => {
                        warn!(error = %e, domain = host_clean, "custom domain db lookup failed");
                        host_clean.split('.').next().unwrap_or(host_clean).to_string()
                    }
                }
            }
        };

        // Validate slug: must be non-empty, alphanumeric + dashes only.
        // Invalid slugs skip cache/DB lookup and fall through to the API.
        let slug_valid = !slug.is_empty()
            && !slug.starts_with('-')
            && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');

        // Stash slug in context for fail_to_connect.
        ctx.slug = if slug_valid { Some(slug.clone()) } else { None };

        // Invalid or missing slug — skip cache/DB, go straight to API.
        if !slug_valid {
            debug!(%host, "invalid or empty slug, falling back to API");
            return Ok(Box::new(HttpPeer::new(self.api_upstream.clone(), false, String::new())));
        }

        // Publish a traffic pulse (debounced) so the daemon can track
        // last_request_at and enforce the Metal idle timeout.
        self.maybe_publish_pulse(&slug);

        // Fast path: in-memory cache hit — no DB round-trip.
        if let Some(addr) = self.cache.read().unwrap_or_else(|e| e.into_inner()).get(&slug).cloned() {
            debug!(%slug, addr, "routing (cache)");
            return Ok(Box::new(HttpPeer::new(addr, false, String::new())));
        }

        // Slow path: DB lookup, then populate cache for subsequent requests.
        let addr = match lookup_route(&self.pool, &slug).await {
            Ok(Some(record)) => match record.upstream_addr {
                Some(addr) => {
                    info!(%slug, addr, engine = record.engine, "routing (db)");
                    self.cache.write().unwrap_or_else(|e| e.into_inner()).insert(slug.clone(), addr.clone());
                    addr
                }
                None => {
                    warn!(%slug, "no upstream_addr — service still provisioning, falling back to API");
                    self.api_upstream.clone()
                }
            },
            Ok(None) => {
                warn!(%slug, "unknown slug, falling back to API");
                self.api_upstream.clone()
            }
            Err(e) => {
                warn!(error = %e, %slug, "db error, falling back to API");
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

/// Spawn a background thread that periodically sweeps stale entries
/// from the pulse debounce cache. Runs on its own thread since
/// Pingora's main loop is synchronous.
pub fn start_pulse_sweeper(pulse_cache: Arc<RwLock<HashMap<String, Instant>>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(PULSE_SWEEP_INTERVAL_SECS));
            let mut map = pulse_cache.write().unwrap_or_else(|e| e.into_inner());
            let before = map.len();
            map.retain(|_, ts| ts.elapsed().as_secs() < *PULSE_DEBOUNCE_SECS * 2);
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
                .map(|t| t.elapsed().as_secs() >= *PULSE_DEBOUNCE_SECS)
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
