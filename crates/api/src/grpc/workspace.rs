use crate::AppState;
use crate::grpc::auth::{extract_user_id, map_db_error};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::proto::liquidmetal::v1::workspace_service_server::WorkspaceService;
use crate::proto::liquidmetal::v1::{
    BillingTier, CreateWorkspaceRequest, CreateWorkspaceResponse, DeleteWorkspaceRequest,
    DeleteWorkspaceResponse, GetWorkspaceRequest, GetWorkspaceResponse, ListWorkspacesRequest,
    ListWorkspacesResponse, Workspace,
};

pub struct WorkspaceServiceImpl {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl WorkspaceService for WorkspaceServiceImpl {
    // ─── CREATE WORKSPACE ───────────────────────────────────────────────────

    async fn create_workspace(
        &self,
        request: Request<CreateWorkspaceRequest>,
    ) -> Result<Response<CreateWorkspaceResponse>, Status> {
        let user_id = extract_user_id(&request)?;
        let req     = request.into_inner();

        if req.name.is_empty() || req.slug.is_empty() {
            return Err(Status::invalid_argument("name and slug are required"));
        }

        let mut db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let workspace_id = Uuid::now_v7();

        // Insert workspace + membership atomically so we never get an orphaned workspace.
        let txn = db.transaction().await.map_err(|e| {
            tracing::error!(error = %e, "begin transaction failed");
            Status::internal("db error")
        })?;

        txn.execute(
            "INSERT INTO workspaces (id, name, slug, tier) VALUES ($1, $2, $3, 'hobby')",
            &[&workspace_id, &req.name, &req.slug],
        )
        .await
        .map_err(|e| map_db_error(e, "slug already taken"))?;

        txn.execute(
            "INSERT INTO workspace_members (workspace_id, user_id, role) \
             VALUES ($1, $2, 'owner')",
            &[&workspace_id, &user_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "membership insert failed");
            Status::internal("db error")
        })?;

        txn.commit().await.map_err(|e| {
            tracing::error!(error = %e, "commit failed");
            Status::internal("db error")
        })?;

        Ok(Response::new(CreateWorkspaceResponse {
            workspace: Some(Workspace {
                id:         workspace_id.to_string(),
                name:       req.name,
                slug:       req.slug,
                tier:       BillingTier::Hobby as i32,
                created_at: None,
                updated_at: None,
            }),
        }))
    }

    // ─── GET WORKSPACE ──────────────────────────────────────────────────────

    async fn get_workspace(
        &self,
        request: Request<GetWorkspaceRequest>,
    ) -> Result<Response<GetWorkspaceResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let id: Uuid = request
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid workspace id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Join workspace_members to enforce membership — returns nothing if caller has no access.
        let row = db
            .query_opt(
                "SELECT w.id, w.name, w.slug, w.tier \
                 FROM workspaces w \
                 JOIN workspace_members wm ON wm.workspace_id = w.id AND wm.user_id = $2 \
                 WHERE w.id = $1 AND w.deleted_at IS NULL",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "get_workspace query failed");
                Status::internal("query error")
            })?
            .ok_or_else(|| Status::not_found("workspace not found"))?;

        Ok(Response::new(GetWorkspaceResponse {
            workspace: Some(Workspace {
                id:         row.get::<_, Uuid>("id").to_string(),
                name:       row.get("name"),
                slug:       row.get("slug"),
                tier:       BillingTier::Hobby as i32,
                created_at: None,
                updated_at: None,
            }),
        }))
    }

    // ─── LIST WORKSPACES ────────────────────────────────────────────────────

    async fn list_workspaces(
        &self,
        _request: Request<ListWorkspacesRequest>,
    ) -> Result<Response<ListWorkspacesResponse>, Status> {
        Ok(Response::new(ListWorkspacesResponse {
            workspaces:      vec![],
            next_page_token: String::new(),
        }))
    }

    // ─── DELETE WORKSPACE ───────────────────────────────────────────────────

    async fn delete_workspace(
        &self,
        request: Request<DeleteWorkspaceRequest>,
    ) -> Result<Response<DeleteWorkspaceResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let id: Uuid = request
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid workspace id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Soft-delete: only the workspace owner may delete.
        let updated = db
            .execute(
                "UPDATE workspaces \
                 SET deleted_at = NOW() \
                 WHERE id = $1 \
                   AND deleted_at IS NULL \
                   AND EXISTS (\
                     SELECT 1 FROM workspace_members \
                     WHERE workspace_id = $1 AND user_id = $2 AND role = 'owner'\
                   )",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "delete_workspace update failed");
                Status::internal("delete error")
            })?;

        if updated == 0 {
            return Err(Status::not_found("workspace not found"));
        }

        Ok(Response::new(DeleteWorkspaceResponse {}))
    }
}
