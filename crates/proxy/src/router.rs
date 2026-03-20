use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use deadpool_postgres::Pool;
use pingora::prelude::*;
use tracing::{debug, info, warn};

use crate::cache::{DomainCache, RouteCache};
use crate::db::{lookup_domain, lookup_route};

/// Per-service Metal usage counter.
/// Stored in a shared map keyed by slug and drained every 60s by the usage flusher.
/// Tracks both invocation count and accumulated compute duration (GB-seconds × 1000
/// for millisecond precision without floating point).
pub struct MetalCounter {
    pub service_id:    String,
    pub workspace_id:  String,
    pub invocations:   AtomicU64,
    /// Accumulated compute duration in milliseconds. Converted to GB-sec at flush time.
    /// Using millis avoids floating point in atomic operations.
    pub duration_ms:   AtomicU64,
}

/// Metal invocation counters — slug → MetalCounter.
pub type MetalCounters = Arc<RwLock<HashMap<String, Arc<MetalCounter>>>>;

/// Per-slug concurrency limiter. Limits how many simultaneous requests
/// a single service can handle. Prevents one service from saturating
/// the node's connection/memory budget. Returns 503 when at capacity.
pub type SlugSemaphores = Arc<RwLock<HashMap<String, Arc<tokio::sync::Semaphore>>>>;

/// Max concurrent requests per service. Override via MAX_CONCURRENT_PER_SERVICE.
static MAX_CONCURRENT_PER_SERVICE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("MAX_CONCURRENT_PER_SERVICE", "200")
        .parse()
        .unwrap_or(200)
});

/// Maximum upstream response time before the proxy kills the request.
/// Protects against runaway compute. Override via UPSTREAM_TIMEOUT_SECS.
static UPSTREAM_TIMEOUT_SECS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    common::config::env_or("UPSTREAM_TIMEOUT_SECS", "30").parse().unwrap_or(30)
});

/// Maximum time to wait for a cold Metal service to wake from snapshot.
/// If the VM doesn't come up within this window, the proxy returns 503.
static WAKE_TIMEOUT_MS: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    common::config::env_or("WAKE_TIMEOUT_MS", "10000").parse().unwrap_or(10_000)
});

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

/// Per-request context — carries state across Pingora hooks.
pub struct RequestCtx {
    slug: Option<String>,
    /// Set when routing to a Metal service. Used by `logging()` to measure
    /// request duration for GB-sec billing.
    metal_start: Option<Instant>,
    /// Held for the request's lifetime — released when `logging()` runs.
    /// Limits per-service concurrency.
    _concurrency_permit: Option<tokio::sync::OwnedSemaphorePermit>,
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
    /// Per-service Metal invocation counters. Drained every 60s by the usage flusher.
    pub metal_counters: MetalCounters,
    /// Per-slug concurrency semaphores. Limits simultaneous requests per service.
    pub slug_semaphores: SlugSemaphores,
}

#[async_trait]
impl ProxyHttp for MachineRouter {
    type CTX = RequestCtx;
    fn new_ctx(&self) -> RequestCtx {
        RequestCtx { slug: None, metal_start: None, _concurrency_permit: None }
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

        // Acquire a per-slug concurrency permit. Returns 503 if the service
        // is at capacity (MAX_CONCURRENT_PER_SERVICE simultaneous requests).
        ctx._concurrency_permit = self.try_acquire_concurrency(&slug);
        if ctx._concurrency_permit.is_none() {
            warn!(%slug, max = *MAX_CONCURRENT_PER_SERVICE, "service at concurrency limit — returning 503");
            return Err(Error::new(pingora::ErrorType::HTTPStatus(503)));
        }

        // Fast path: in-memory cache hit — no DB round-trip.
        if let Some(addr) = self.cache.read().unwrap_or_else(|e| e.into_inner()).get(&slug).cloned() {
            debug!(%slug, addr, "routing (cache)");
            if self.try_count_metal(&slug) {
                ctx.metal_start = Some(Instant::now());
            }
            return Ok(Self::make_peer(addr));
        }

        // Slow path: DB lookup, then populate cache for subsequent requests.
        let addr = match lookup_route(&self.pool, &slug).await {
            Ok(Some(record)) => match record.upstream_addr {
                Some(addr) => {
                    info!(%slug, addr, engine = record.engine, "routing (db)");
                    self.cache.write().unwrap_or_else(|e| e.into_inner()).insert(slug.clone(), addr.clone());
                    if record.engine == "metal" {
                        self.count_metal_invocation(&slug, &record.service_id, &record.workspace_id);
                        ctx.metal_start = Some(Instant::now());
                    }
                    addr
                }
                None if record.status == "ready" && record.snapshot_key.is_some()
                    || record.status == "stopped" && record.engine == "liquid" =>
                {
                    // Cold service — Metal: has a snapshot but no running VM.
                    // Liquid: stopped but artifacts cached on disk (serverless scale-to-zero).
                    // Publish a wake event, then poll the route cache until the
                    // daemon restores the service and publishes RouteUpdatedEvent.
                    info!(%slug, engine = record.engine, "cold service — requesting wake");
                    self.publish_wake(&slug, &record.service_id, &record.engine, record.snapshot_key.as_deref());

                    match self.wait_for_warm(&slug).await {
                        Some(addr) => {
                            info!(%slug, %addr, "service woke from snapshot");
                            self.count_metal_invocation(&slug, &record.service_id, &record.workspace_id);
                            ctx.metal_start = Some(Instant::now());
                            addr
                        }
                        None => {
                            warn!(%slug, "wake timeout — service did not start within {}ms", *WAKE_TIMEOUT_MS);
                            return Err(Error::new(pingora::ErrorType::HTTPStatus(503)));
                        }
                    }
                }
                None => {
                    warn!(%slug, status = record.status, "no upstream_addr, falling back to API");
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

        Ok(Self::make_peer(addr))
    }

    /// Called after the full request/response cycle completes.
    /// Records Metal request duration for GB-sec billing.
    async fn logging(&self, _session: &mut Session, _e: Option<&Error>, ctx: &mut RequestCtx) {
        if let (Some(start), Some(slug)) = (ctx.metal_start.take(), ctx.slug.as_ref()) {
            let elapsed_ms = start.elapsed().as_millis() as u64;
            let counters = self.metal_counters.read().unwrap_or_else(|e| e.into_inner());
            if let Some(counter) = counters.get(slug) {
                counter.duration_ms.fetch_add(elapsed_ms, Ordering::Relaxed);
            }
        }
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
/// Spawn a background thread that drains Metal invocation counters every 60s
/// and publishes MetalUsageEvent to NATS for billing.
pub fn start_metal_usage_flusher(counters: MetalCounters, nats: Arc<async_nats::Client>) {
    let flush_secs: u64 = common::config::env_or("METAL_USAGE_FLUSH_SECS", "60")
        .parse().unwrap_or(60);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("metal usage flusher runtime");
        rt.block_on(async move {
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(flush_secs));
            loop {
                ticker.tick().await;
                let snapshot: Vec<(String, String, String, u64, u64)> = {
                    let map = counters.read().unwrap_or_else(|e| e.into_inner());
                    map.iter()
                        .filter_map(|(slug, c)| {
                            let inv = c.invocations.swap(0, Ordering::Relaxed);
                            let dur = c.duration_ms.swap(0, Ordering::Relaxed);
                            if inv > 0 || dur > 0 {
                                Some((slug.clone(), c.service_id.clone(), c.workspace_id.clone(), inv, dur))
                            } else {
                                None
                            }
                        })
                        .collect()
                };

                for (_slug, service_id, workspace_id, invocations, duration_ms) in &snapshot {
                    let ev = common::events::MetalUsageEvent {
                        service_id:   service_id.clone(),
                        workspace_id: workspace_id.clone(),
                        invocations:  *invocations,
                        duration_ms:  *duration_ms,
                    };
                    if let Ok(payload) = serde_json::to_vec(&ev) {
                        if let Err(e) = nats.publish(common::events::SUBJECT_USAGE_METAL, payload.into()).await {
                            tracing::warn!(error = %e, service_id, "failed to publish MetalUsageEvent");
                            // TODO: backlog retry (for now, lost invocations on NATS failure)
                        }
                    }
                }

                if !snapshot.is_empty() {
                    tracing::debug!(services = snapshot.len(), "flushed metal invocation counters");
                }
            }
        });
    });
}

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
    /// Build an HttpPeer with the configured upstream timeout.
    fn make_peer(addr: String) -> Box<HttpPeer> {
        let mut peer = HttpPeer::new(addr, false, String::new());
        peer.options.read_timeout = Some(std::time::Duration::from_secs(*UPSTREAM_TIMEOUT_SECS));
        peer.options.write_timeout = Some(std::time::Duration::from_secs(*UPSTREAM_TIMEOUT_SECS));
        Box::new(peer)
    }

    /// Try to acquire a concurrency permit for a slug.
    /// Returns None (and the caller returns 503) if at capacity.
    fn try_acquire_concurrency(&self, slug: &str) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let sem = {
            let sems = self.slug_semaphores.read().unwrap_or_else(|e| e.into_inner());
            if let Some(sem) = sems.get(slug) {
                sem.clone()
            } else {
                drop(sems);
                let sem = Arc::new(tokio::sync::Semaphore::new(*MAX_CONCURRENT_PER_SERVICE));
                self.slug_semaphores.write().unwrap_or_else(|e| e.into_inner())
                    .insert(slug.to_string(), sem.clone());
                sem
            }
        };
        sem.try_acquire_owned().ok()
    }

    /// Increment the Metal invocation counter for a service.
    /// Called on every successfully routed request. On cache hits, looks up
    /// by slug — if no counter exists, the service is Liquid (daemon counts those).
    fn count_metal_invocation(&self, slug: &str, service_id: &str, workspace_id: &str) {
        let counters = self.metal_counters.read().unwrap_or_else(|e| e.into_inner());
        if let Some(counter) = counters.get(slug) {
            counter.invocations.fetch_add(1, Ordering::Relaxed);
            return;
        }
        drop(counters);
        // First time seeing this Metal service — create a counter.
        let counter = Arc::new(MetalCounter {
            service_id:   service_id.to_string(),
            workspace_id: workspace_id.to_string(),
            invocations:  AtomicU64::new(1),
            duration_ms:  AtomicU64::new(0),
        });
        self.metal_counters.write().unwrap_or_else(|e| e.into_inner())
            .insert(slug.to_string(), counter);
    }

    /// Try to increment a Metal counter by slug (fast path — no DB needed).
    /// Returns true if the slug had a counter, false if not (Liquid or unknown).
    fn try_count_metal(&self, slug: &str) -> bool {
        let counters = self.metal_counters.read().unwrap_or_else(|e| e.into_inner());
        if let Some(counter) = counters.get(slug) {
            counter.invocations.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Publish a `platform.wake` event to trigger cold-start restore.
    /// Metal: snapshot restore. Liquid: re-compile cached Wasm module.
    /// Fire-and-forget — the daemon deduplicates if multiple requests arrive simultaneously.
    fn publish_wake(&self, slug: &str, service_id: &str, engine: &str, snapshot_key: Option<&str>) {
        let Some(nats) = &self.nats else { return };
        let nats = nats.clone();
        let engine_enum = if engine == "metal" {
            common::events::Engine::Metal
        } else {
            common::events::Engine::Liquid
        };
        let event = common::events::WakeEvent {
            service_id:   service_id.to_string(),
            slug:         slug.to_string(),
            engine:       engine_enum,
            snapshot_key: snapshot_key.unwrap_or("").to_string(),
        };
        tokio::spawn(async move {
            if let Ok(payload) = serde_json::to_vec(&event) {
                if let Err(e) = nats.publish(common::events::SUBJECT_WAKE, payload.into()).await {
                    tracing::warn!(error = %e, "failed to publish WakeEvent");
                }
            }
        });
    }

    /// Poll the route cache until the daemon populates it via RouteUpdatedEvent,
    /// or until the wake timeout expires. Returns the upstream_addr if the
    /// service woke in time, None on timeout.
    async fn wait_for_warm(&self, slug: &str) -> Option<String> {
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_millis(*WAKE_TIMEOUT_MS);
        let poll_interval = tokio::time::Duration::from_millis(50);

        loop {
            if let Some(addr) = self.cache.read().unwrap_or_else(|e| e.into_inner()).get(slug).cloned() {
                return Some(addr);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

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
