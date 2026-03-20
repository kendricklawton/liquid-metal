use std::sync::Arc;

use axum::{Extension, Json, extract::State};
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope};

#[utoipa::path(get, path = "/users/me", responses(
    (status = 200, description = "Current user profile", body = contract::UserResponse),
    (status = 401, description = "Unauthorized"),
), tag = "Users", security(("api_key" = [])))]
pub async fn get_me(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<contract::UserResponse>, ApiError> {
    require_scope(&caller, "read")?;
    let db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT id, email, name FROM users WHERE id = $1 AND deleted_at IS NULL",
            &[&caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("user lookup failed"))?
        .ok_or_else(|| ApiError::not_found("user not found"))?;

    Ok(Json(contract::UserResponse {
        id: row.get::<_, Uuid>("id").to_string(),
        email: row.get("email"),
        name: row.get("name"),
    }))
}
