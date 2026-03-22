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

/// Per-IP rate limiter.
type IpRateLimiter = RateLimiter<std::net::IpAddr, DashMapStateStore<std::net::IpAddr>, DefaultClock>;

/// Per-user rate limiter (keyed by user ID string from X-On-Behalf-Of).
type UserIdRateLimiter = RateLimiter<String, DashMapStateStore<String>, DefaultClock>;

/// Shared per-IP rate limiter handle.
#[derive(Clone)]
pub struct RateLimit(pub Arc<IpRateLimiter>);

impl RateLimit {
    pub fn per_minute(rpm: u32) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(rpm).expect("RPM must be > 0"));
        Self(Arc::new(RateLimiter::dashmap(quota)))
    }
}

/// Shared per-user rate limiter handle (for BFF calls via X-On-Behalf-Of).
#[derive(Clone)]
pub struct UserRateLimit(pub Arc<UserIdRateLimiter>);

impl UserRateLimit {
    pub fn per_minute(rpm: u32) -> Self {
        let quota = Quota::per_minute(NonZeroU32::new(rpm).expect("RPM must be > 0"));
        Self(Arc::new(RateLimiter::dashmap(quota)))
    }
}

/// Reusable 429 response builder.
fn too_many_requests(retry_secs: String, context: &str) -> Response {
    tracing::warn!(retry_after_secs = %retry_secs, context, "rate limited");
    let body = serde_json::json!({
        "error": "rate_limited",
        "message": "too many requests — slow down",
        "retry_after": retry_secs,
    });
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", retry_secs)],
        Json(body),
    ).into_response()
}

/// Rate limit by IP address. Used for public/CLI endpoints.
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
            return Err(too_many_requests(retry_secs, &format!("ip:{ip}")));
        }
    }

    Ok(next.run(req).await)
}

/// Rate limit by user ID (X-On-Behalf-Of) when present, otherwise by IP.
///
/// BFF calls include X-On-Behalf-Of with the dashboard user's ID — each user
/// gets their own bucket at `user_limit` RPM. Calls without the header (direct
/// internal calls, brute force attempts) fall back to IP-based limiting at
/// `ip_fallback` RPM.
pub async fn rate_limit_by_user_or_ip_middleware(
    user_limit: UserRateLimit,
    ip_fallback: RateLimit,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    // BFF calls carry X-On-Behalf-Of — rate limit per user.
    if let Some(user_id) = req.headers().get("x-on-behalf-of").and_then(|v| v.to_str().ok()) {
        let key = user_id.to_string();
        if let Err(not_until) = user_limit.0.check_key(&key) {
            let wait = not_until.wait_time_from(DefaultClock::default().now());
            let retry_secs = wait.as_secs().max(1).to_string();
            return Err(too_many_requests(retry_secs, &format!("user:{key}")));
        }
        return Ok(next.run(req).await);
    }

    // No X-On-Behalf-Of — fall back to IP-based limiting.
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());

    if let Some(ip) = ip {
        if let Err(not_until) = ip_fallback.0.check_key(&ip) {
            let wait = not_until.wait_time_from(DefaultClock::default().now());
            let retry_secs = wait.as_secs().max(1).to_string();
            return Err(too_many_requests(retry_secs, &format!("ip:{ip}")));
        }
    }

    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_rate_limit_allows_within_quota() {
        let rl = RateLimit::per_minute(5);
        let ip: std::net::IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..5 {
            assert!(rl.0.check_key(&ip).is_ok());
        }
    }

    #[test]
    fn ip_rate_limit_rejects_over_quota() {
        let rl = RateLimit::per_minute(2);
        let ip: std::net::IpAddr = "10.0.0.2".parse().unwrap();
        assert!(rl.0.check_key(&ip).is_ok());
        assert!(rl.0.check_key(&ip).is_ok());
        assert!(rl.0.check_key(&ip).is_err());
    }

    #[test]
    fn ip_rate_limit_separate_buckets_per_ip() {
        let rl = RateLimit::per_minute(1);
        let ip_a: std::net::IpAddr = "10.0.0.3".parse().unwrap();
        let ip_b: std::net::IpAddr = "10.0.0.4".parse().unwrap();
        assert!(rl.0.check_key(&ip_a).is_ok());
        assert!(rl.0.check_key(&ip_a).is_err(), "ip_a should be exhausted");
        assert!(rl.0.check_key(&ip_b).is_ok(), "ip_b should have its own bucket");
    }

    #[test]
    fn user_rate_limit_allows_within_quota() {
        let rl = UserRateLimit::per_minute(5);
        let user = "user-abc-123".to_string();
        for _ in 0..5 {
            assert!(rl.0.check_key(&user).is_ok());
        }
    }

    #[test]
    fn user_rate_limit_rejects_over_quota() {
        let rl = UserRateLimit::per_minute(2);
        let user = "user-xyz-456".to_string();
        assert!(rl.0.check_key(&user).is_ok());
        assert!(rl.0.check_key(&user).is_ok());
        assert!(rl.0.check_key(&user).is_err());
    }

    #[test]
    fn user_rate_limit_separate_buckets_per_user() {
        let rl = UserRateLimit::per_minute(1);
        let user_a = "user-a".to_string();
        let user_b = "user-b".to_string();
        assert!(rl.0.check_key(&user_a).is_ok());
        assert!(rl.0.check_key(&user_a).is_err(), "user_a should be exhausted");
        assert!(rl.0.check_key(&user_b).is_ok(), "user_b should have its own bucket");
    }

    #[test]
    fn user_and_ip_limiters_are_independent() {
        let ip_rl = RateLimit::per_minute(1);
        let user_rl = UserRateLimit::per_minute(1);
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        let user = "user-123".to_string();

        assert!(ip_rl.0.check_key(&ip).is_ok());
        assert!(ip_rl.0.check_key(&ip).is_err());

        // User bucket unaffected by IP exhaustion
        assert!(user_rl.0.check_key(&user).is_ok());
    }

    #[test]
    #[should_panic(expected = "RPM must be > 0")]
    fn ip_rate_limit_zero_rpm_panics() {
        RateLimit::per_minute(0);
    }

    #[test]
    #[should_panic(expected = "RPM must be > 0")]
    fn user_rate_limit_zero_rpm_panics() {
        UserRateLimit::per_minute(0);
    }
}
