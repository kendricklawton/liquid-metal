use axum::{
    Json,
    extract::{ConnectInfo, Request},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{
    Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    state::keyed::DashMapStateStore,
};
use std::{net::SocketAddr, num::NonZeroU32, sync::Arc};

/// Key type for the rate limiter — rate-limits by IP address.
type IpRateLimiter = RateLimiter<std::net::IpAddr, DashMapStateStore<std::net::IpAddr>, DefaultClock>;

/// Shared rate limiter handle, constructed once at startup.
#[derive(Clone)]
pub struct RateLimit(pub Arc<IpRateLimiter>);

impl RateLimit {
    /// Build a per-IP rate limiter from a requests-per-minute value.
    ///
    /// 12-factor: the RPM value comes from an env var parsed in `main.rs`.
    /// This function is pure — no env reads, no side effects.
    pub fn per_minute(rpm: u32) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(rpm).expect("RPM must be > 0"));
        Self(Arc::new(RateLimiter::dashmap(quota)))
    }
}

/// Axum middleware that rejects requests exceeding the rate limit.
///
/// Returns `429 Too Many Requests` with a `Retry-After` header (in seconds)
/// and a structured JSON error body.
/// If no `ConnectInfo` is available (e.g. in tests), the request passes through.
pub async fn rate_limit_middleware(
    limit: RateLimit,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());

    if let Some(ip) = ip {
        if let Err(not_until) = limit.0.check_key(&ip) {
            let wait = not_until.wait_time_from(DefaultClock::default().now());
            let retry_secs = wait.as_secs().max(1).to_string();
            tracing::warn!(%ip, retry_after_secs = %retry_secs, "rate limited");
            let body = serde_json::json!({
                "error": "rate_limited",
                "message": "too many requests — slow down",
                "retry_after": retry_secs,
            });
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", retry_secs)],
                Json(body),
            ).into_response());
        }
    }

    Ok(next.run(req).await)
}
