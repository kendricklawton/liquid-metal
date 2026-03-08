//! Shared authentication and authorization helpers for gRPC handlers.

use tokio_postgres::error::SqlState;
use tonic::Status;
use uuid::Uuid;

/// Extract the authenticated user's UUID from a tonic request.
///
/// Accepts either:
/// - `Authorization: Bearer {uuid}` — browser / ConnectRPC web path
/// - `x-api-key: {uuid}` — CLI path
pub fn extract_user_id<T>(req: &tonic::Request<T>) -> Result<Uuid, Status> {
    let meta = req.metadata();

    if let Some(bearer) = meta.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(token) = bearer.strip_prefix("Bearer ") {
            return token
                .parse()
                .map_err(|_| Status::unauthenticated("invalid token format"));
        }
    }

    if let Some(key) = meta.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return key
            .parse()
            .map_err(|_| Status::unauthenticated("invalid api key format"));
    }

    Err(Status::unauthenticated("missing authentication"))
}

/// Assert the user is a member of the given workspace.
/// Returns `permission_denied` if not.
pub async fn assert_workspace_member(
    db: &tokio_postgres::Client,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<(), Status> {
    let row = db
        .query_one(
            "SELECT EXISTS(\
               SELECT 1 FROM workspace_members \
               WHERE workspace_id = $1 AND user_id = $2\
             )",
            &[&workspace_id, &user_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "membership check query failed");
            Status::internal("authorization check failed")
        })?;

    if !row.get::<_, bool>(0) {
        return Err(Status::permission_denied("access denied"));
    }

    Ok(())
}

/// Map a postgres error to a gRPC Status.
/// `UNIQUE_VIOLATION` → `already_exists`; everything else → `internal`.
pub fn map_db_error(e: tokio_postgres::Error, already_exists_msg: &'static str) -> Status {
    if e.as_db_error()
        .map(|db| db.code() == &SqlState::UNIQUE_VIOLATION)
        .unwrap_or(false)
    {
        Status::already_exists(already_exists_msg)
    } else {
        tracing::error!(error = %e, "database error");
        Status::internal("database error")
    }
}
