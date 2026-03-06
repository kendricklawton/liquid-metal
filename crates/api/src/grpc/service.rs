use crate::AppState;
use crate::nats;
use crate::proto::liquidmetal::v1::{
    CreateServiceRequest, CreateServiceResponse,
    DeleteServiceRequest, DeleteServiceResponse,
    Engine, GetServiceLogsRequest, GetServiceLogsResponse,
    GetServiceRequest, GetServiceResponse,
    ListServicesRequest, ListServicesResponse,
    Service, ServiceStatus,
};
use crate::proto::liquidmetal::v1::service_service_server::ServiceService;
use common::events::{Engine as EngineEnum, EngineSpec, LiquidSpec, MetalSpec, ProvisionEvent, ResourceQuota};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

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
            if !prev_dash { slug.push(c); }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
    }
    slug.trim_matches('-').to_string()
}

fn engine_from_proto(e: i32) -> Result<EngineEnum, Status> {
    match Engine::try_from(e).unwrap_or(Engine::Unspecified) {
        Engine::Metal    => Ok(EngineEnum::Metal),
        Engine::Liquid   => Ok(EngineEnum::Liquid),
        Engine::Unspecified => Err(Status::invalid_argument("engine must be METAL or LIQUID")),
    }
}

fn engine_to_proto_str(e: &EngineEnum) -> &'static str {
    match e {
        EngineEnum::Metal  => "metal",
        EngineEnum::Liquid => "liquid",
    }
}

#[tonic::async_trait]
impl ServiceService for ServiceServiceImpl {
    async fn list_services(
        &self,
        request: Request<ListServicesRequest>,
    ) -> Result<Response<ListServicesResponse>, Status> {
        let req = request.into_inner();
        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let rows = if req.project_id.is_empty() {
            db.query(
                "SELECT id, name, slug, engine, status, upstream_addr, \
                 commit_sha, commit_message \
                 FROM services WHERE deleted_at IS NULL ORDER BY created_at DESC",
                &[],
            ).await
        } else {
            let pid: Uuid = req.project_id.parse()
                .map_err(|_| Status::invalid_argument("invalid project_id"))?;
            db.query(
                "SELECT id, name, slug, engine, status, upstream_addr, \
                 commit_sha, commit_message \
                 FROM services WHERE project_id = $1 AND deleted_at IS NULL \
                 ORDER BY created_at DESC",
                &[&pid],
            ).await
        }.map_err(|e| {
            tracing::error!(error = %e, "list services failed");
            Status::internal("query error")
        })?;

        let services = rows.iter().map(|row| {
            let engine_str: String = row.get("engine");
            let engine_val = if engine_str == "metal" { Engine::Metal as i32 } else { Engine::Liquid as i32 };
            let status_str: String = row.get("status");
            let status_val = match status_str.as_str() {
                "provisioning" => ServiceStatus::Provisioning as i32,
                "running"      => ServiceStatus::Running as i32,
                "failed"       => ServiceStatus::Failed as i32,
                "stopped"      => ServiceStatus::Stopped as i32,
                _              => ServiceStatus::Unspecified as i32,
            };
            Service {
                id:             row.get::<_, Uuid>("id").to_string(),
                name:           row.get("name"),
                slug:           row.get("slug"),
                engine:         engine_val,
                status:         status_val,
                upstream_addr:  row.get::<_, Option<String>>("upstream_addr").unwrap_or_default(),
                project_id:     String::new(),
                commit_sha:     row.get::<_, Option<String>>("commit_sha").unwrap_or_default(),
                commit_message: row.get::<_, Option<String>>("commit_message").unwrap_or_default(),
                created_at:     None,
                provisioned_at: None,
            }
        }).collect();

        Ok(Response::new(ListServicesResponse { services }))
    }

    async fn get_service(
        &self,
        request: Request<GetServiceRequest>,
    ) -> Result<Response<GetServiceResponse>, Status> {
        let id: Uuid = request.into_inner().id.parse()
            .map_err(|_| Status::invalid_argument("invalid id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let row = db.query_opt(
            "SELECT id, name, slug, engine, status, upstream_addr, \
             commit_sha, commit_message \
             FROM services WHERE id = $1 AND deleted_at IS NULL",
            &[&id],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "get service failed");
            Status::internal("query error")
        })?.ok_or_else(|| Status::not_found("service not found"))?;

        let engine_str: String = row.get("engine");
        let engine_val = if engine_str == "metal" { Engine::Metal as i32 } else { Engine::Liquid as i32 };

        Ok(Response::new(GetServiceResponse {
            service: Some(Service {
                id:             row.get::<_, Uuid>("id").to_string(),
                name:           row.get("name"),
                slug:           row.get("slug"),
                engine:         engine_val,
                status:         ServiceStatus::Unspecified as i32,
                upstream_addr:  row.get::<_, Option<String>>("upstream_addr").unwrap_or_default(),
                project_id:     String::new(),
                commit_sha:     row.get::<_, Option<String>>("commit_sha").unwrap_or_default(),
                commit_message: row.get::<_, Option<String>>("commit_message").unwrap_or_default(),
                created_at:     None,
                provisioned_at: None,
            }),
        }))
    }

    async fn create_service(
        &self,
        request: Request<CreateServiceRequest>,
    ) -> Result<Response<CreateServiceResponse>, Status> {
        let req = request.into_inner();
        let engine = engine_from_proto(req.engine)?;
        let service_id = Uuid::now_v7();
        let slug = slugify(&req.name);
        let port = req.port as u16;
        let vcpu = req.vcpu as u32;
        let memory_mb = req.memory_mb as u32;

        let spec = match &engine {
            EngineEnum::Metal => EngineSpec::Metal(MetalSpec {
                vcpu, memory_mb, port,
                artifact_key:    String::new(),
                artifact_sha256: None,
                quota:           ResourceQuota::default(),
            }),
            EngineEnum::Liquid => EngineSpec::Liquid(LiquidSpec {
                artifact_key:    String::new(),
                artifact_sha256: None,
            }),
        };

        let engine_str = engine_to_proto_str(&engine);

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let project_id: Option<Uuid> = if req.project_id.is_empty() {
            None
        } else {
            Some(req.project_id.parse().map_err(|_| Status::invalid_argument("invalid project_id"))?)
        };

        db.execute(
            "INSERT INTO services \
             (id, project_id, name, slug, engine, \
              vcpu, memory_mb, port) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            &[
                &service_id,
                &project_id,
                &req.name,
                &slug,
                &engine_str,
                &(vcpu as i32),
                &(memory_mb as i32),
                &(port as i32),
            ],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "insert service failed");
            Status::internal("insert error")
        })?;

        let event = ProvisionEvent {
            tenant_id:  String::new(),
            service_id: service_id.to_string(),
            app_name:   req.name.clone(),
            engine:     engine.clone(),
            spec,
        };

        nats::publish_provision(&self.state.nats, &event).await.map_err(|e| {
            tracing::error!(error = %e, "NATS publish failed");
            Status::internal("event publish error")
        })?;

        Ok(Response::new(CreateServiceResponse {
            service: Some(Service {
                id:             service_id.to_string(),
                name:           req.name,
                slug,
                engine:         req.engine,
                status:         ServiceStatus::Provisioning as i32,
                upstream_addr:  String::new(),
                project_id:     req.project_id,
                commit_sha:     String::new(),
                commit_message: String::new(),
                created_at:     None,
                provisioned_at: None,
            }),
        }))
    }

    async fn delete_service(
        &self,
        request: Request<DeleteServiceRequest>,
    ) -> Result<Response<DeleteServiceResponse>, Status> {
        let id: Uuid = request.into_inner().id.parse()
            .map_err(|_| Status::invalid_argument("invalid id"))?;

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let n = db.execute(
            "UPDATE services SET deleted_at = NOW() WHERE id = $1 AND deleted_at IS NULL",
            &[&id],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "delete service failed");
            Status::internal("delete error")
        })?;

        if n == 0 {
            return Err(Status::not_found("service not found"));
        }

        Ok(Response::new(DeleteServiceResponse {}))
    }

    async fn get_service_logs(
        &self,
        request: Request<GetServiceLogsRequest>,
    ) -> Result<Response<GetServiceLogsResponse>, Status> {
        let req = request.into_inner();
        let service_id: Uuid = req.service_id.parse()
            .map_err(|_| Status::invalid_argument("invalid service_id"))?;
        let limit = if req.limit > 0 { req.limit } else { 100 };

        let db = self.state.db.get().await.map_err(|e| {
            tracing::error!(error = %e, "db pool error");
            Status::internal("db error")
        })?;

        let rows = db.query(
            "SELECT message FROM build_log_lines \
             WHERE service_id = $1 ORDER BY created_at DESC LIMIT $2",
            &[&service_id, &(limit as i64)],
        ).await.map_err(|e| {
            tracing::error!(error = %e, "get logs failed");
            Status::internal("query error")
        })?;

        let lines = rows.iter().map(|row| {
            crate::proto::liquidmetal::v1::LogLine {
                message: row.get("message"),
                ts: None,
            }
        }).collect();

        Ok(Response::new(GetServiceLogsResponse { lines }))
    }
}
