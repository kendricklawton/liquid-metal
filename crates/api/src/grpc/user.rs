use crate::AppState;
use crate::grpc::auth::extract_user_id;
use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::proto::liquidmetal::v1::user_service_server::UserService;
use crate::proto::liquidmetal::v1::{GetMeRequest, GetMeResponse, User};

pub struct UserServiceImpl {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl UserService for UserServiceImpl {
    async fn get_me(
        &self,
        request: Request<GetMeRequest>,
    ) -> Result<Response<GetMeResponse>, Status> {
        let user_id = extract_user_id(&request)?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let row = db
            .query_opt(
                "SELECT id, email, name FROM users WHERE id = $1 AND deleted_at IS NULL",
                &[&user_id],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "get_me query failed");
                Status::internal("query error")
            })?
            .ok_or_else(|| Status::not_found("user not found"))?;

        Ok(Response::new(GetMeResponse {
            user: Some(User {
                id:         row.get::<_, uuid::Uuid>("id").to_string(),
                email:      row.get("email"),
                name:       row.get("name"),
                created_at: None,
                updated_at: None,
            }),
        }))
    }
}
