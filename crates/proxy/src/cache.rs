use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// In-process route cache: slug → upstream_addr.
///
/// Warmed by NATS `platform.route_updated` events published by the daemon
/// immediately after provisioning sets upstream_addr in the DB.
/// Falls back to a DB lookup on miss — e.g. on first request after cold start
/// or after a Pingora restart.
pub type RouteCache = Arc<RwLock<HashMap<String, String>>>;

/// In-process custom domain cache: domain → slug.
/// Populated on miss from the domains table. Reconciled every 60s alongside
/// the route cache.
pub type DomainCache = Arc<RwLock<HashMap<String, String>>>;

pub fn new() -> RouteCache {
    Arc::new(RwLock::new(HashMap::new()))
}

pub fn new_domain_cache() -> DomainCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Bulk-populate the route cache from DB on startup.
/// Prevents a cold-start storm where every slug misses the cache simultaneously
/// and fires a parallel DB query. Times out after 5s and falls back to
/// on-demand population if the DB is slow or unreachable.
pub fn warm(cache: &RouteCache, pool: &deadpool_postgres::Pool) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for cache warm");
    let result = rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let db = pool.get().await?;
            let rows = db
                .query(
                    "SELECT slug, upstream_addr FROM services \
                     WHERE status = 'running' AND upstream_addr IS NOT NULL AND deleted_at IS NULL",
                    &[],
                )
                .await?;
            let mut map = cache.write().unwrap_or_else(|e| e.into_inner());
            for row in &rows {
                let slug: String = row.get(0);
                let addr: String = row.get(1);
                map.insert(slug, addr);
            }
            Ok::<usize, anyhow::Error>(map.len())
        })
        .await
    });
    match result {
        Ok(Ok(n)) => tracing::info!(routes = n, "route cache warmed from DB"),
        Ok(Err(e)) => tracing::warn!(error = %e, "route cache warm failed — falling back to on-demand"),
        Err(_) => tracing::warn!("route cache warm timed out after 5s — falling back to on-demand"),
    }
}

/// Bulk-populate the domain cache from DB on startup.
pub fn warm_domains(domain_cache: &DomainCache, pool: &deadpool_postgres::Pool) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for domain cache warm");
    let result = rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let db = pool.get().await?;
            let rows = db
                .query(
                    "SELECT d.domain, s.slug FROM domains d \
                     JOIN services s ON s.id = d.service_id \
                     WHERE d.is_verified = true AND s.deleted_at IS NULL",
                    &[],
                )
                .await?;
            let mut map = domain_cache.write().unwrap_or_else(|e| e.into_inner());
            for row in &rows {
                let domain: String = row.get(0);
                let slug: String = row.get(1);
                map.insert(domain, slug);
            }
            Ok::<usize, anyhow::Error>(map.len())
        })
        .await
    });
    match result {
        Ok(Ok(n)) => tracing::info!(domains = n, "domain cache warmed from DB"),
        Ok(Err(e)) => tracing::warn!(error = %e, "domain cache warm failed"),
        Err(_) => tracing::warn!("domain cache warm timed out after 5s"),
    }
}

/// Spawn a background thread that periodically reconciles the route cache
/// against the DB. Catches routes that were missed due to daemon crash,
/// NATS blip, or proxy restart while NATS was down. Runs every 60s.
pub fn start_reconciler(cache: RouteCache, pool: std::sync::Arc<deadpool_postgres::Pool>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for cache reconciler");
        rt.block_on(async move {
            use std::collections::HashMap;
            use tokio::time::{Duration, interval};

            let reconcile_secs: u64 = common::config::env_or("ROUTE_CACHE_RECONCILE_SECS", "60").parse().unwrap_or(60);
            let mut ticker = interval(Duration::from_secs(reconcile_secs));
            // Skip the first immediate tick — warm() already ran at startup.
            ticker.tick().await;

            loop {
                ticker.tick().await;

                let db = match pool.get().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "cache reconciler: db pool error");
                        continue;
                    }
                };

                let rows = match db
                    .query(
                        "SELECT slug, upstream_addr FROM services \
                         WHERE status = 'running' AND upstream_addr IS NOT NULL AND deleted_at IS NULL",
                        &[],
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "cache reconciler: query failed");
                        continue;
                    }
                };

                // Build the authoritative map from DB.
                let db_map: HashMap<String, String> = rows
                    .iter()
                    .map(|r| (r.get::<_, String>(0), r.get::<_, String>(1)))
                    .collect();

                let mut map = cache.write().unwrap_or_else(|e| e.into_inner());

                // Add/update routes present in DB but missing/stale in cache.
                let mut added = 0usize;
                for (slug, addr) in &db_map {
                    if map.get(slug) != Some(addr) {
                        map.insert(slug.clone(), addr.clone());
                        added += 1;
                    }
                }

                // Remove routes in cache that are no longer running in DB.
                let before = map.len();
                map.retain(|slug, _| db_map.contains_key(slug));
                let removed = before - map.len();

                if added > 0 || removed > 0 {
                    tracing::info!(added, removed, total = map.len(), "cache reconciler: synced with DB");
                }
            }
        });
    });
}

/// Spawn a background thread that periodically reconciles the domain cache and
/// cert cache against Postgres. Removes entries for domains that are no longer
/// verified or whose service has been deleted. Mirrors `start_reconciler` — same
/// interval, same retain pattern. Also evicts stale entries from `cert_cache` so
/// deleted domains don't hold SslContext objects in memory indefinitely.
pub fn start_domain_reconciler(
    domain_cache: DomainCache,
    cert_cache: crate::tls::CertCache,
    pool: std::sync::Arc<deadpool_postgres::Pool>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for domain reconciler");
        rt.block_on(async move {
            use std::collections::HashSet;
            use tokio::time::{Duration, interval};

            let reconcile_secs: u64 = common::config::env_or("ROUTE_CACHE_RECONCILE_SECS", "60")
                .parse().unwrap_or(60);
            let mut ticker = interval(Duration::from_secs(reconcile_secs));
            // Skip the first immediate tick — warm_domains() already ran at startup.
            ticker.tick().await;

            loop {
                ticker.tick().await;

                let db = match pool.get().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "domain reconciler: db pool error");
                        continue;
                    }
                };

                let rows = match db
                    .query(
                        "SELECT d.domain FROM domains d \
                         JOIN services s ON s.id = d.service_id \
                         WHERE d.is_verified = true AND s.deleted_at IS NULL",
                        &[],
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "domain reconciler: query failed");
                        continue;
                    }
                };

                // Authoritative set of valid verified domains.
                let db_domains: HashSet<String> = rows
                    .iter()
                    .map(|r| r.get::<_, String>(0))
                    .collect();

                // Prune domain cache.
                let mut dmap = domain_cache.write().unwrap_or_else(|e| e.into_inner());
                let before = dmap.len();
                dmap.retain(|domain, _| db_domains.contains(domain));
                let removed = before - dmap.len();
                drop(dmap);

                // Evict cert cache entries for removed domains.
                if removed > 0 {
                    let mut cmap = cert_cache.write().unwrap_or_else(|e| e.into_inner());
                    cmap.retain(|domain, _| db_domains.contains(domain));
                    tracing::info!(
                        domains_removed = removed,
                        "domain reconciler: pruned stale domains + certs"
                    );
                }
            }
        });
    });
}

/// Spawn a background thread that subscribes to NATS route update events
/// and keeps the cache warm. Runs on its own Tokio runtime, independent
/// of Pingora's internal thread pool. Reconnects automatically on disconnect.
pub fn start_subscriber(cache: RouteCache, nats_url: String) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for route cache subscriber");
        rt.block_on(async move {
            loop {
                match subscribe_loop(&cache, &nats_url).await {
                    Ok(()) => break,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "route cache NATS subscriber disconnected — reconnecting in 5s"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });
    });
}

async fn subscribe_loop(cache: &RouteCache, nats_url: &str) -> anyhow::Result<()> {
    use common::events::{RouteRemovedEvent, RouteUpdatedEvent, SUBJECT_ROUTE_REMOVED, SUBJECT_ROUTE_UPDATED};
    use futures::StreamExt;

    let nc = common::config::nats_connect(nats_url).await?;
    let mut updated = nc.subscribe(SUBJECT_ROUTE_UPDATED).await?;
    let mut removed = nc.subscribe(SUBJECT_ROUTE_REMOVED).await?;

    tracing::info!(%nats_url, "route cache: NATS subscriber connected");

    loop {
        tokio::select! {
            Some(msg) = updated.next() => {
                match serde_json::from_slice::<RouteUpdatedEvent>(&msg.payload) {
                    Ok(event) => {
                        cache.write().unwrap_or_else(|e| e.into_inner()).insert(event.slug.clone(), event.upstream_addr.clone());
                        tracing::debug!(slug = event.slug, upstream = event.upstream_addr, "route cache updated");
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to parse RouteUpdatedEvent"),
                }
            }
            Some(msg) = removed.next() => {
                match serde_json::from_slice::<RouteRemovedEvent>(&msg.payload) {
                    Ok(event) => {
                        cache.write().unwrap_or_else(|e| e.into_inner()).remove(&event.slug);
                        tracing::debug!(slug = event.slug, "route cache evicted");
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to parse RouteRemovedEvent"),
                }
            }
            else => anyhow::bail!("NATS subscription streams ended"),
        }
    }
}
