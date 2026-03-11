use crate::AppState;
use crate::grpc::auth::{assert_workspace_member, extract_user_id, map_db_error};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

use crate::proto::liquidmetal::v1::project_service_server::ProjectService;
use crate::proto::liquidmetal::v1::{
    CreateProjectRequest, CreateProjectResponse, ListProjectsRequest, ListProjectsResponse, Project,
};

/// Hard cap on rows returned by ListProjects.
const LIST_PROJECTS_LIMIT: i64 = 200;

pub struct ProjectServiceImpl {
    pub state: Arc<AppState>,
}

#[tonic::async_trait]
impl ProjectService for ProjectServiceImpl {
    // ─── CREATE PROJECT ─────────────────────────────────────────────────────

    async fn create_project(
        &self,
        request: Request<CreateProjectRequest>,
    ) -> Result<Response<CreateProjectResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        if req.name.is_empty() || req.slug.is_empty() || req.workspace_id.is_empty() {
            return Err(Status::invalid_argument(
                "name, slug, and workspace_id are required",
            ));
        }

        let workspace_id: Uuid = req
            .workspace_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid workspace_id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db pool error")
        })?;

        // Verify caller is a member of the workspace.
        assert_workspace_member(&db, workspace_id, caller).await?;

        let project_id = Uuid::now_v7();

        db.execute(
            "INSERT INTO projects (id, workspace_id, name, slug) VALUES ($1, $2, $3, $4)",
            &[&project_id, &workspace_id, &req.name, &req.slug],
        )
        .await
        .map_err(|e| map_db_error(e, "project slug already exists in this workspace"))?;

        info!(project_id = %project_id, "project created");

        Ok(Response::new(CreateProjectResponse {
            project: Some(Project {
                id:           project_id.to_string(),
                workspace_id: workspace_id.to_string(),
                name:         req.name,
                slug:         req.slug,
                created_at:   None,
                updated_at:   None,
            }),
        }))
    }

    // ─── LIST PROJECTS ──────────────────────────────────────────────────────

    async fn list_projects(
        &self,
        request: Request<ListProjectsRequest>,
    ) -> Result<Response<ListProjectsResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        let workspace_id: Uuid = req
            .workspace_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid workspace_id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Membership check is embedded in the JOIN — returns empty if not a member.
        let rows = db
            .query(
                "SELECT p.id, p.workspace_id, p.name, p.slug \
                 FROM projects p \
                 JOIN workspace_members wm \
                   ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
                 WHERE p.workspace_id = $1 AND p.deleted_at IS NULL \
                 ORDER BY p.created_at DESC \
                 LIMIT $3",
                &[&workspace_id, &caller, &LIST_PROJECTS_LIMIT],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "list_projects query failed");
                Status::internal("query error")
            })?;

        let projects = rows
            .iter()
            .map(|row| Project {
                id:           row.get::<_, Uuid>("id").to_string(),
                workspace_id: row.get::<_, Uuid>("workspace_id").to_string(),
                name:         row.get("name"),
                slug:         row.get("slug"),
                created_at:   None,
                updated_at:   None,
            })
            .collect();

        Ok(Response::new(ListProjectsResponse {
            projects,
            next_page_token: String::new(),
        }))
    }
}
