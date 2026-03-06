use crate::AppState;
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::proto::liquidmetal::v1::workspace_service_server::WorkspaceService;
use crate::proto::liquidmetal::v1::{CreateWorkspaceRequest, CreateWorkspaceResponse};

pub struct WorkspaceServiceImpl {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl WorkspaceService for WorkspaceServiceImpl {
    async fn create_workspace(
        &self,
        request: Request<CreateWorkspaceRequest>,
    ) -> Result<Response<CreateWorkspaceResponse>, Status> {
        let auth = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !auth.starts_with("Bearer ") {
            return Err(Status::unauthenticated("missing Bearer token"));
        }

        let user_id: Uuid = auth["Bearer ".len()..]
            .parse()
            .map_err(|_| Status::unauthenticated("invalid token format"))?;

        let req = request.into_inner();

        if req.name.is_empty() || req.slug.is_empty() {
            return Err(Status::invalid_argument("name and slug are required"));
        }

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let workspace_id = Uuid::now_v7();

        // Insert workspace + owner membership in a single transaction
        db.execute(
            "INSERT INTO workspaces (id, name, slug) VALUES ($1, $2, $3)",
            &[&workspace_id, &req.name, &req.slug],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "create_workspace insert failed");
            if e.to_string().contains("unique") {
                Status::already_exists("slug already taken")
            } else {
                Status::internal("db error")
            }
        })?;

        db.execute(
            "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, 'owner')",
            &[&workspace_id, &user_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "create_workspace membership insert failed");
            Status::internal("db error")
        })?;

        Ok(Response::new(CreateWorkspaceResponse {
            id:   workspace_id.to_string(),
            name: req.name,
            slug: req.slug,
        }))
    }
}
