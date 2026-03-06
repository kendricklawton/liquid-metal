use crate::{AppState, nats};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use common::events::{Engine, EngineSpec, FlashSpec, MetalSpec, ProvisionEvent, ResourceQuota};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub async fn health() -> StatusCode {
    StatusCode::OK
}

pub fn services_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/",    post(create_service))
        .route("/:id", get(get_service))
        .route("/:id", delete(delete_service))
}

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub workspace_id: Uuid,
    pub project_id:   Option<Uuid>,
    pub name:         String,
    pub engine:       String,   // "metal" | "flash"
    // Git context (populated by plat deploy)
    pub branch:         Option<String>,
    pub commit_sha:     Option<String>,
    pub commit_message: Option<String>,
    // Metal config
    pub port:        Option<u16>,
    pub vcpu:        Option<u32>,
    pub memory_mb:   Option<u32>,
    pub rootfs_path: Option<String>,
    // Flash config
    pub wasm_path: Option<String>,
    // Triple-Lock: Layer 2 (Kernel IO) + Layer 3 (Network)
    // Layer 1 (Hypervisor) is vcpu + memory_mb above
    #[serde(default)]
    pub quota: ResourceQuota,
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub engine: String,
    pub status: String,
    pub upstream_addr: Option<String>,
}

pub async fn create_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateServiceRequest>,
) -> Result<(StatusCode, Json<ServiceResponse>), StatusCode> {
    let service_id = Uuid::now_v7();
    let slug = slugify(&req.name);
    let port = req.port.unwrap_or(8080);

    // Extract before any moves
    let vcpu        = req.vcpu.unwrap_or(1);
    let memory_mb   = req.memory_mb.unwrap_or(128);
    let rootfs_path = req.rootfs_path.unwrap_or_default();
    let wasm_path   = req.wasm_path.unwrap_or_default();

    let spec = match req.engine.as_str() {
        "metal" => EngineSpec::Metal(MetalSpec {
            vcpu, memory_mb, port,
            rootfs_path:     rootfs_path.clone(),
            artifact_sha256: None,   // TODO: set from upload hash
            quota:           req.quota.clone(),
        }),
        "flash" => EngineSpec::Flash(FlashSpec {
            wasm_path:       wasm_path.clone(),
            artifact_sha256: None,   // TODO: set from upload hash
        }),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let engine = match req.engine.as_str() {
        "metal" => Engine::Metal,
        "flash" => Engine::Flash,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    // Persist service row before publishing — daemon updates it after boot
    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    db.execute(
        "INSERT INTO services \
         (id, workspace_id, project_id, name, slug, engine, \
          branch, commit_sha, commit_message, \
          vcpu, memory_mb, port, rootfs_path, wasm_path, \
          disk_read_bps, disk_write_bps, disk_read_iops, disk_write_iops, \
          net_ingress_kbps, net_egress_kbps) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
        &[
            &service_id,
            &req.workspace_id,
            &req.project_id,
            &req.name,
            &slug,
            &req.engine,
            &req.branch,
            &req.commit_sha,
            &req.commit_message,
            &(vcpu as i32),
            &(memory_mb as i32),
            &(port as i32),
            &rootfs_path,
            &wasm_path,
            &req.quota.disk_read_bps.map(|v| v as i64),
            &req.quota.disk_write_bps.map(|v| v as i64),
            &req.quota.disk_read_iops.map(|v| v as i32),
            &req.quota.disk_write_iops.map(|v| v as i32),
            &req.quota.net_ingress_kbps.map(|v| v as i32),
            &req.quota.net_egress_kbps.map(|v| v as i32),
        ],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "insert service failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let event = ProvisionEvent {
        tenant_id:  req.workspace_id.to_string(),
        service_id: service_id.to_string(),
        app_name:   req.name.clone(),
        engine,
        spec,
    };

    nats::publish_provision(&state.nats, &event)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to publish provision event");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok((
        StatusCode::ACCEPTED,
        Json(ServiceResponse {
            id:           service_id.to_string(),
            name:         req.name,
            slug,
            engine:       req.engine,
            status:       "provisioning".to_string(),
            upstream_addr: None,
        }),
    ))
}

pub async fn get_service(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ServiceResponse>, StatusCode> {
    let svc_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let row = db
        .query_opt(
            "SELECT id, name, slug, engine, status, upstream_addr \
             FROM services WHERE id = $1 AND deleted_at IS NULL",
            &[&svc_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "query service failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ServiceResponse {
        id:           row.get::<_, Uuid>("id").to_string(),
        name:         row.get("name"),
        slug:         row.get("slug"),
        engine:       row.get("engine"),
        status:       row.get("status"),
        upstream_addr: row.get("upstream_addr"),
    }))
}

pub async fn delete_service(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let svc_id: Uuid = id.parse().map_err(|_| StatusCode::BAD_REQUEST)?;

    let db = state.db.get().await.map_err(|e| {
        tracing::error!(error = %e, "db pool error");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let n = db
        .execute(
            "UPDATE services SET deleted_at = NOW() \
             WHERE id = $1 AND deleted_at IS NULL",
            &[&svc_id],
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "soft-delete service failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if n == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    // TODO: publish deprovision event so daemon tears down VM/Wasm
    tracing::info!(service_id = %svc_id, "service soft-deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// Lowercase, replace non-alphanumeric with `-`, collapse runs, trim edges.
fn slugify(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // collapse consecutive dashes
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
