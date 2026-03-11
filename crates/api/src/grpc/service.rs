use crate::AppState;
use crate::grpc::auth::{assert_workspace_member, extract_user_id};
use crate::proto::liquidmetal::v1::{
    DeleteServiceRequest, DeleteServiceResponse, DeployRequest, DeployResponse, Engine,
    GetServiceLogsRequest, GetServiceLogsResponse, GetServiceRequest, GetServiceResponse,
    GetUploadUrlRequest, GetUploadUrlResponse, ListServicesRequest, ListServicesResponse, LogLine,
    RestartServiceRequest, RestartServiceResponse, Service, ServiceStatus, StopServiceRequest,
    StopServiceResponse, service_service_server::ServiceService,
};
use common::{EngineSpec, LiquidSpec, MetalSpec, ProvisionEvent, slugify};
use common::events::{DeprovisionEvent, Engine as CommonEngine};
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


#[tonic::async_trait]
impl ServiceService for ServiceServiceImpl {
    // ─── GET UPLOAD URL ──────────────────────────────────────────────────────

    async fn get_upload_url(
        &self,
        request: Request<GetUploadUrlRequest>,
    ) -> Result<Response<GetUploadUrlResponse>, Status> {
        let caller = extract_user_id(&request)?;
        let req    = request.into_inner();

        let pid: Uuid = req
            .project_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid project_id"))?;

        if req.deploy_id.is_empty() {
            return Err(Status::invalid_argument("deploy_id is required"));
        }

        let engine_prefix = match Engine::try_from(req.engine).unwrap_or(Engine::Unspecified) {
            Engine::Metal  => "metal",
            Engine::Liquid => "wasm",
            _              => return Err(Status::invalid_argument("invalid engine type")),
        };

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db pool error")
        })?;

        // Verify caller owns the project before issuing a write URL.
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
        assert_workspace_member(&db, workspace_id, caller).await?;

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

        // ── Input validation ─────────────────────────────────────────────────
        if req.name.is_empty() {
            return Err(Status::invalid_argument("name is required"));
        }
        if req.name.len() > 63 {
            return Err(Status::invalid_argument("name must be 63 characters or fewer"));
        }
        if req.artifact_key.is_empty() {
            return Err(Status::invalid_argument("artifact_key is required"));
        }

        let pid: Uuid = req
            .project_id
            .parse()
            .map_err(|_| Status::invalid_argument("invalid project_id"))?;

        let mut db = self.state.db.get().await.map_err(|e| {
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
        assert_workspace_member(&db, workspace_id, caller).await?;

        // ── Tier + quota enforcement ─────────────────────────────────────────
        let tier_row = db
            .query_one(
                "SELECT tier FROM workspaces WHERE id = $1",
                &[&workspace_id],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "workspace tier lookup failed");
                Status::internal("database error")
            })?;

        let tier: String = tier_row.get("tier");
        let limits = crate::quota::limits_for(&tier);

        // ── Engine + spec validation ─────────────────────────────────────────
        let engine_str = match Engine::try_from(req.engine).unwrap_or(Engine::Unspecified) {
            Engine::Metal  => "metal",
            Engine::Liquid => "liquid",
            _              => return Err(Status::invalid_argument("invalid engine")),
        };

        let engine_spec = match (engine_str, req.spec) {
            ("metal", Some(crate::proto::liquidmetal::v1::deploy_request::Spec::Metal(m))) => {
                let vcpu      = m.vcpu as u32;
                let memory_mb = m.memory_mb as u32;
                let port      = m.port as u16;

                if vcpu == 0 {
                    return Err(Status::invalid_argument("metal: vcpu must be at least 1"));
                }
                if memory_mb < 64 {
                    return Err(Status::invalid_argument("metal: memory_mb must be at least 64"));
                }
                if port == 0 {
                    return Err(Status::invalid_argument("metal: port is required"));
                }
                if vcpu > limits.max_vcpu {
                    return Err(Status::resource_exhausted(format!(
                        "{} tier allows a maximum of {} vCPU(s) per service",
                        tier, limits.max_vcpu
                    )));
                }
                if memory_mb > limits.max_memory_mb {
                    return Err(Status::resource_exhausted(format!(
                        "{} tier allows a maximum of {} MB memory per service",
                        tier, limits.max_memory_mb
                    )));
                }

                EngineSpec::Metal(MetalSpec {
                    vcpu,
                    memory_mb,
                    port,
                    artifact_key:    req.artifact_key.clone(),
                    artifact_sha256: if req.sha256.is_empty() { None } else { Some(req.sha256.clone()) },
                    quota:           Default::default(),
                })
            }
            ("liquid", Some(crate::proto::liquidmetal::v1::deploy_request::Spec::Liquid(_)) | None) => {
                EngineSpec::Liquid(LiquidSpec {
                    artifact_key:    req.artifact_key.clone(),
                    artifact_sha256: if req.sha256.is_empty() { None } else { Some(req.sha256.clone()) },
                })
            }
            ("metal", _) => {
                return Err(Status::invalid_argument("metal engine requires a [metal] spec"));
            }
            _ => {
                return Err(Status::invalid_argument("engine and spec are mismatched"));
            }
        };

        let (vcpu, memory_mb, port) = match &engine_spec {
            EngineSpec::Metal(m) => (m.vcpu, m.memory_mb, m.port as i32),
            EngineSpec::Liquid(_) => (0, 0, 0),
        };

        let service_id = Uuid::now_v7();
        let slug = if req.slug.is_empty() { slugify(&req.name) } else { req.slug.clone() };

        // ── Atomic service count + insert ────────────────────────────────────
        // Advisory lock serializes concurrent deploys for the same workspace,
        // eliminating the TOCTOU race between the service count check and INSERT.
        // The lock is automatically released when the transaction commits or rolls back.
        let lock_key = {
            let b = workspace_id.as_bytes();
            i64::from_be_bytes(b[0..8].try_into().unwrap())
                ^ i64::from_be_bytes(b[8..16].try_into().unwrap())
        };

        let txn = db.build_transaction().start().await.map_err(|e| {
            tracing::error!(error = %e, "transaction start failed");
            Status::internal("database error")
        })?;

        txn.execute("SELECT pg_advisory_xact_lock($1)", &[&lock_key])
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "advisory lock failed");
                Status::internal("database error")
            })?;

        let svc_count: i64 = txn
            .query_one(
                "SELECT COUNT(*) FROM services WHERE workspace_id = $1 AND deleted_at IS NULL",
                &[&workspace_id],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "service count query failed");
                Status::internal("database error")
            })?
            .get(0);

        if svc_count >= limits.max_services {
            return Err(Status::resource_exhausted(format!(
                "{} tier allows a maximum of {} service(s)",
                tier, limits.max_services
            )));
        }

        txn.execute(
            "INSERT INTO services \
             (id, project_id, workspace_id, name, slug, engine, status, commit_sha, vcpu, memory_mb, port) \
             VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', $7, $8, $9, $10)",
            &[
                &service_id, &pid, &workspace_id,
                &req.name, &slug, &engine_str,
                &req.sha256, &(vcpu as i32), &(memory_mb as i32), &port,
            ],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "service insert failed");
            Status::internal("database error")
        })?;

        txn.commit().await.map_err(|e| {
            tracing::error!(error = %e, "transaction commit failed");
            Status::internal("database error")
        })?;

        let event = ProvisionEvent {
            tenant_id:  workspace_id.to_string(),
            service_id: service_id.to_string(),
            app_name:   req.name.clone(),
            engine:     match Engine::try_from(req.engine).unwrap_or(Engine::Unspecified) {
                Engine::Metal => common::Engine::Metal,
                _             => common::Engine::Liquid,
            },
            spec: engine_spec,
        };

        crate::nats::publish_provision(&self.state.nats, &event)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "nats publish failed");
                Status::internal("failed to queue provisioning")
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

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db pool error")
        })?;

        // Membership is enforced inside the query via JOIN — returns empty set if not a member.
        // When project_id is empty, list all services across all of the caller's workspaces.
        let rows = if req.project_id.is_empty() {
            db.query(
                "SELECT s.id, s.name, s.slug, s.engine, s.status, \
                        s.upstream_addr, s.commit_sha, s.project_id \
                 FROM services s \
                 JOIN projects p ON p.id = s.project_id \
                 JOIN workspace_members wm \
                   ON wm.workspace_id = p.workspace_id AND wm.user_id = $1 \
                 WHERE s.deleted_at IS NULL \
                 ORDER BY s.created_at DESC \
                 LIMIT $2",
                &[&caller, &LIST_SERVICES_LIMIT],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "list_services query failed");
                Status::internal("query error")
            })?
        } else {
            let pid: Uuid = req
                .project_id
                .parse()
                .map_err(|_| Status::invalid_argument("invalid project_id"))?;
            db.query(
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
            })?
        };

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
                engine:       if row.get::<_, String>("engine") == "metal" {
                                  Engine::Metal as i32
                              } else {
                                  Engine::Liquid as i32
                              },
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
        // Returns engine so we can publish a DeprovisionEvent to the daemon.
        let row = db
            .query_opt(
                "UPDATE services s \
                 SET deleted_at = NOW() \
                 FROM projects p, workspace_members wm \
                 WHERE s.id = $1 \
                   AND s.deleted_at IS NULL \
                   AND s.project_id = p.id \
                   AND wm.workspace_id = p.workspace_id \
                   AND wm.user_id = $2 \
                 RETURNING s.engine",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "delete_service update failed");
                Status::internal("delete error")
            })?;

        let row = row.ok_or_else(|| Status::not_found("service not found"))?;
        let engine_str: String = row.get("engine");

        let event = DeprovisionEvent {
            service_id: id.to_string(),
            engine: if engine_str == "metal" { CommonEngine::Metal } else { CommonEngine::Liquid },
        };
        crate::nats::publish_deprovision(&self.state.nats, &event)
            .await
            .map_err(|e| tracing::error!(error = %e, "nats deprovision publish failed"))
            .ok(); // non-fatal: service is already soft-deleted

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
                "SELECT content, created_at \
                 FROM build_log_lines \
                 WHERE service_id = $1 \
                   AND created_at > NOW() - INTERVAL '30 days' \
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
            .map(|row| LogLine { message: row.get("content"), ts: None })
            .collect();

        Ok(Response::new(GetServiceLogsResponse {
            lines,
            next_page_token: String::new(),
        }))
    }

    // ─── STOP SERVICE ────────────────────────────────────────────────────────

    async fn stop_service(
        &self,
        request: Request<StopServiceRequest>,
    ) -> Result<Response<StopServiceResponse>, Status> {
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

        // Set status=stopped (not deleted) — caller must be a workspace member.
        let row = db
            .query_opt(
                "UPDATE services s \
                 SET status = 'stopped', upstream_addr = NULL \
                 FROM projects p, workspace_members wm \
                 WHERE s.id = $1 \
                   AND s.deleted_at IS NULL \
                   AND s.status != 'stopped' \
                   AND s.project_id = p.id \
                   AND wm.workspace_id = p.workspace_id \
                   AND wm.user_id = $2 \
                 RETURNING s.engine",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "stop_service update failed");
                Status::internal("stop error")
            })?;

        let row = row.ok_or_else(|| Status::not_found("service not found or already stopped"))?;
        let engine_str: String = row.get("engine");

        let event = DeprovisionEvent {
            service_id: id.to_string(),
            engine: if engine_str == "metal" { CommonEngine::Metal } else { CommonEngine::Liquid },
        };
        crate::nats::publish_deprovision(&self.state.nats, &event)
            .await
            .map_err(|e| tracing::error!(error = %e, "nats deprovision publish failed"))
            .ok();

        Ok(Response::new(StopServiceResponse {}))
    }

    // ─── RESTART SERVICE ─────────────────────────────────────────────────────

    async fn restart_service(
        &self,
        request: Request<RestartServiceRequest>,
    ) -> Result<Response<RestartServiceResponse>, Status> {
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

        // Fetch service — must be stopped or failed, caller must be a member.
        let row = db
            .query_opt(
                "SELECT s.id, s.name, s.slug, s.engine, s.workspace_id, \
                        s.vcpu, s.memory_mb, s.port, s.wasm_path, s.rootfs_path, s.commit_sha \
                 FROM services s \
                 JOIN projects p ON p.id = s.project_id \
                 JOIN workspace_members wm \
                   ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
                 WHERE s.id = $1 \
                   AND s.deleted_at IS NULL \
                   AND s.status IN ('stopped', 'failed')",
                &[&id, &caller],
            )
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "restart_service fetch failed");
                Status::internal("query error")
            })?
            .ok_or_else(|| Status::not_found("service not found or not in a restartable state"))?;

        let engine_str: String = row.get("engine");
        let workspace_id: Uuid  = row.get("workspace_id");
        let name: String        = row.get("name");
        let slug: String        = row.get("slug");
        let commit_sha: Option<String> = row.get("commit_sha");

        let spec = match engine_str.as_str() {
            "metal" => {
                let artifact_key: Option<String> = row.get("rootfs_path");
                EngineSpec::Metal(MetalSpec {
                    vcpu:            row.get::<_, i32>("vcpu") as u32,
                    memory_mb:       row.get::<_, i32>("memory_mb") as u32,
                    port:            row.get::<_, i32>("port") as u16,
                    artifact_key:    artifact_key.unwrap_or_default(),
                    artifact_sha256: None,
                    quota:           Default::default(),
                })
            }
            _ => {
                let artifact_key: Option<String> = row.get("wasm_path");
                EngineSpec::Liquid(LiquidSpec {
                    artifact_key:    artifact_key.unwrap_or_default(),
                    artifact_sha256: None,
                })
            }
        };

        // Reset to provisioning.
        db.execute(
            "UPDATE services SET status = 'provisioning', upstream_addr = NULL \
             WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "restart_service status update failed");
            Status::internal("database error")
        })?;

        let event = ProvisionEvent {
            tenant_id:  workspace_id.to_string(),
            service_id: id.to_string(),
            app_name:   name.clone(),
            engine: if engine_str == "metal" { CommonEngine::Metal } else { CommonEngine::Liquid },
            spec,
        };
        crate::nats::publish_provision(&self.state.nats, &event)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "nats provision publish failed");
                Status::internal("failed to queue restart")
            })?;

        Ok(Response::new(RestartServiceResponse {
            service: Some(Service {
                id:         id.to_string(),
                name,
                slug,
                engine:     if engine_str == "metal" { Engine::Metal as i32 } else { Engine::Liquid as i32 },
                status:     ServiceStatus::Provisioning as i32,
                commit_sha: commit_sha.unwrap_or_default(),
                ..Default::default()
            }),
        }))
    }
}
