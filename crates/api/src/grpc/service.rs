use crate::AppState;
use crate::grpc::auth::{assert_workspace_member, extract_user_id};
use crate::proto::liquidmetal::v1::{
    DeleteServiceRequest, DeleteServiceResponse, DeployRequest, DeployResponse, Engine,
    GetServiceLogsRequest, GetServiceLogsResponse, GetServiceRequest, GetServiceResponse,
    GetUploadUrlRequest, GetUploadUrlResponse, ListServicesRequest, ListServicesResponse, LogLine,
    Service, ServiceStatus, service_service_server::ServiceService,
};
use aws_sdk_s3::presigning::PresigningConfig;
use std::sync::Arc;
use std::time::Duration;
use tonic::{Request, Response, Status};
use uuid::Uuid;

/// Hard cap on rows returned by ListServices. Prevents memory exhaustion.
const LIST_SERVICES_LIMIT: i64 = 500;
/// Hard cap on log lines returned by GetServiceLogs.
const LOGS_MAX_LIMIT: i64 = 1_000;

pub struct ServiceServiceImpl {
    pub state: Arc<AppState>,
}

fn slugify(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash {
                slug.push(c);
            }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
    }
    slug.trim_matches('-').to_string()
}

#[tonic::async_trait]
impl ServiceService for ServiceServiceImpl {
    // ─── GET UPLOAD URL ──────────────────────────────────────────────────────

    async fn get_upload_url(
        &self,
        request: Request<GetUploadUrlRequest>,
    ) -> Result<Response<GetUploadUrlResponse>, Status> {
        // Auth is validated by the axum auth_middleware; no per-resource check needed here
        // since the key is parameterised by project_id supplied by the caller.
        let req = request.into_inner();

        let engine_prefix = match Engine::try_from(req.engine).unwrap_or(Engine::Unspecified) {
            Engine::Metal  => "metal",
            Engine::Liquid => "wasm",
            _              => return Err(Status::invalid_argument("invalid engine type")),
        };

        let artifact_name = if req.engine == Engine::Liquid as i32 { "main.wasm" } else { "app" };
        let artifact_key  = format!(
            "{}/{}/{}/{}",
            engine_prefix, req.project_id, req.deploy_id, artifact_name
        );

        let expires_in = PresigningConfig::expires_in(Duration::from_secs(300))
            .map_err(|_| Status::internal("presign config error"))?;

        let presigned = self
            .state
            .s3
            .put_object()
            .bucket(&self.state.bucket)
            .key(&artifact_key)
            .presigned(expires_in)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "S3 presign failed");
                Status::internal("failed to generate upload URL")
            })?;

        Ok(Response::new(GetUploadUrlResponse {
            upload_url: presigned.uri().to_string(),
            artifact_key,
        }))
    }

    // ─── DEPLOY ──────────────────────────────────────────────────────────────

    async fn deploy(
        &self,
        request: Request<DeployRequest>,
    ) -> Result<Response<DeployResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        let pid: Uuid = req
            .project_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid project_id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db pool error")
        })?;

        // Verify caller is a member of the workspace that owns this project.
        let row = db
            .query_opt(
                "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
                &[&pid],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "project lookup failed");
                Status::internal("database error")
            })?
            .ok_or_else(|| Status::not_found("project not found"))?;

        let workspace_id: Uuid = row.get("workspace_id");
        assert_workspace_member(&*db, workspace_id, caller).await?;

        let service_id = Uuid::now_v7();
        let slug = if req.slug.is_empty() { slugify(&req.name) } else { req.slug.clone() };

        let engine_str = match Engine::try_from(req.engine).unwrap_or(Engine::Unspecified) {
            Engine::Metal  => "metal",
            Engine::Liquid => "liquid",
            _              => return Err(Status::invalid_argument("invalid engine")),
        };

        let (vcpu, memory_mb, port) = match req.spec {
            Some(crate::proto::liquidmetal::v1::deploy_request::Spec::Metal(m)) => {
                (m.vcpu, m.memory_mb, m.port)
            }
            _ => (0, 0, 0),
        };

        db.execute(
            "INSERT INTO services \
             (id, project_id, workspace_id, name, slug, engine, status, commit_sha, vcpu, memory_mb, port) \
             VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', $7, $8, $9, $10)",
            &[
                &service_id, &pid, &workspace_id,
                &req.name, &slug, &engine_str,
                &req.sha256, &vcpu, &memory_mb, &port,
            ],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "service insert failed");
            Status::internal("database error")
        })?;

        Ok(Response::new(DeployResponse {
            service: Some(Service {
                id:         service_id.to_string(),
                name:       req.name,
                slug,
                engine:     req.engine,
                status:     ServiceStatus::Provisioning as i32,
                project_id: req.project_id,
                commit_sha: req.sha256,
                ..Default::default()
            }),
        }))
    }

    // ─── LIST SERVICES ───────────────────────────────────────────────────────

    async fn list_services(
        &self,
        request: Request<ListServicesRequest>,
    ) -> Result<Response<ListServicesResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        let pid: Uuid = req
            .project_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid project_id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db pool error")
        })?;

        // Membership is enforced inside the query via JOIN — returns empty set if not a member.
        let rows = db
            .query(
                "SELECT s.id, s.name, s.slug, s.engine, s.status, \
                        s.upstream_addr, s.commit_sha, s.project_id \
                 FROM services s \
                 JOIN projects p ON p.id = s.project_id \
                 JOIN workspace_members wm \
                   ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
                 WHERE s.project_id = $1 AND s.deleted_at IS NULL \
                 ORDER BY s.created_at DESC \
                 LIMIT $3",
                &[&pid, &caller, &LIST_SERVICES_LIMIT],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "list_services query failed");
                Status::internal("query error")
            })?;

        let services = rows
            .iter()
            .map(|row| {
                let engine_str: String = row.get("engine");
                let status_str: String = row.get("status");
                Service {
                    id:         row.get::<_, Uuid>("id").to_string(),
                    project_id: row.get::<_, Uuid>("project_id").to_string(),
                    name:       row.get("name"),
                    slug:       row.get("slug"),
                    engine: if engine_str == "metal" {
                        Engine::Metal as i32
                    } else {
                        Engine::Liquid as i32
                    },
                    status: match status_str.as_str() {
                        "running"  => ServiceStatus::Running as i32,
                        "failed"   => ServiceStatus::Failed as i32,
                        _          => ServiceStatus::Provisioning as i32,
                    },
                    upstream_addr: row
                        .get::<_, Option<String>>("upstream_addr")
                        .unwrap_or_default(),
                    commit_sha: row
                        .get::<_, Option<String>>("commit_sha")
                        .unwrap_or_default(),
                    ..Default::default()
                }
            })
            .collect();

        Ok(Response::new(ListServicesResponse {
            services,
            next_page_token: String::new(),
        }))
    }

    // ─── GET SERVICE ─────────────────────────────────────────────────────────

    async fn get_service(
        &self,
        request: Request<GetServiceRequest>,
    ) -> Result<Response<GetServiceResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let id: Uuid = request
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Join through project → workspace_members to enforce ownership.
        let row = db
            .query_opt(
                "SELECT s.id, s.name, s.slug, s.engine, s.status, \
                        s.upstream_addr, s.project_id \
                 FROM services s \
                 JOIN projects p ON p.id = s.project_id \
                 JOIN workspace_members wm \
                   ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
                 WHERE s.id = $1 AND s.deleted_at IS NULL",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "get_service query failed");
                Status::internal("query error")
            })?
            .ok_or_else(|| Status::not_found("service not found"))?;

        Ok(Response::new(GetServiceResponse {
            service: Some(Service {
                id:           row.get::<_, Uuid>("id").to_string(),
                name:         row.get("name"),
                slug:         row.get("slug"),
                engine:       row.get::<_, String>("engine")
                                  .eq("metal")
                                  .then_some(Engine::Metal as i32)
                                  .unwrap_or(Engine::Liquid as i32),
                status:       match row.get::<_, String>("status").as_str() {
                    "running" => ServiceStatus::Running as i32,
                    "failed"  => ServiceStatus::Failed as i32,
                    _         => ServiceStatus::Provisioning as i32,
                },
                upstream_addr: row
                    .get::<_, Option<String>>("upstream_addr")
                    .unwrap_or_default(),
                project_id:   row.get::<_, Uuid>("project_id").to_string(),
                ..Default::default()
            }),
        }))
    }

    // ─── DELETE SERVICE ──────────────────────────────────────────────────────

    async fn delete_service(
        &self,
        request: Request<DeleteServiceRequest>,
    ) -> Result<Response<DeleteServiceResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let id: Uuid = request
            .into_inner()
            .id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Soft-delete only if caller is a member of the owning workspace.
        let updated = db
            .execute(
                "UPDATE services s \
                 SET deleted_at = NOW() \
                 FROM projects p, workspace_members wm \
                 WHERE s.id = $1 \
                   AND s.deleted_at IS NULL \
                   AND s.project_id = p.id \
                   AND wm.workspace_id = p.workspace_id \
                   AND wm.user_id = $2",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "delete_service update failed");
                Status::internal("delete error")
            })?;

        if updated == 0 {
            return Err(Status::not_found("service not found"));
        }

        Ok(Response::new(DeleteServiceResponse {}))
    }

    // ─── GET SERVICE LOGS ────────────────────────────────────────────────────

    async fn get_service_logs(
        &self,
        request: Request<GetServiceLogsRequest>,
    ) -> Result<Response<GetServiceLogsResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        // Clamp limit: default 100, hard cap 1000.
        let limit = match req.limit {
            n if n <= 0      => 100i64,
            n if n as i64 > LOGS_MAX_LIMIT => LOGS_MAX_LIMIT,
            n                => n as i64,
        };

        let service_id: Uuid = req
            .service_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid service_id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        // Verify caller has access to the service's workspace.
        let ok = db
            .query_one(
                "SELECT EXISTS(\
                   SELECT 1 FROM services s \
                   JOIN projects p ON p.id = s.project_id \
                   JOIN workspace_members wm \
                     ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
                   WHERE s.id = $1 AND s.deleted_at IS NULL\
                 )",
                &[&service_id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "logs access check failed");
                Status::internal("authorization check failed")
            })?;

        if !ok.get::<_, bool>(0) {
            return Err(Status::not_found("service not found"));
        }

        let rows = db
            .query(
                "SELECT message, created_at \
                 FROM build_log_lines \
                 WHERE service_id = $1 \
                 ORDER BY created_at DESC \
                 LIMIT $2",
                &[&service_id, &limit],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "logs query failed");
                Status::internal("failed to fetch logs")
            })?;

        let lines = rows
            .iter()
            .map(|row| LogLine { message: row.get("message"), ts: None })
            .collect();

        Ok(Response::new(GetServiceLogsResponse {
            lines,
            next_page_token: String::new(),
        }))
    }
}
