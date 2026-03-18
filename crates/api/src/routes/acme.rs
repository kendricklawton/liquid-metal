use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::sync::Arc;

use crate::AppState;
use super::{ApiError, db_conn};

/// Serve ACME HTTP-01 challenge responses.
///
/// Let's Encrypt validates domain ownership by requesting:
///   GET /.well-known/acme-challenge/{token}
///
/// The cert_manager writes the token → key_authorization mapping to
/// `acme_challenges` before signaling the challenge as ready. This handler
/// reads it and responds with the key_authorization as plain text.
///
/// HAProxy routes `/.well-known/acme-challenge/*` on port 80 to this API;
/// all other HTTP requests on port 80 are redirected to HTTPS by HAProxy.
///
/// This route is public (no auth) — the token is the authorization credential.
pub async fn acme_challenge(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // Reject obviously invalid tokens early (tokens are base64url, ~43 chars).
    if token.is_empty() || token.len() > 128 {
        return Err(ApiError::not_found("token not found"));
    }

    let db = db_conn(&state.db).await?;
    let row = db
        .query_opt(
            "SELECT key_authorization FROM acme_challenges
             WHERE token = $1 AND created_at > NOW() - INTERVAL '10 minutes'",
            &[&token],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "acme_challenge: db error");
            ApiError::internal("db error")
        })?
        .ok_or_else(|| ApiError::not_found("token not found"))?;

    let key_auth: String = row.get("key_authorization");

    Ok((
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        key_auth,
    ))
}
