use crate::{AppState, nats};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use common::events::{Engine, EngineSpec, FlashSpec, MetalSpec, ProvisionEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

pub async fn health() -> StatusCode {
    StatusCode::OK
}

pub fn services_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/",        post(create_service))
        .route("/:id",     get(get_service))
        .route("/:id",     delete(delete_service))
}

#[derive(Debug, Deserialize)]
pub struct CreateServiceRequest {
    pub workspace_id: String,
    pub name: String,
    pub engine: String,        // "metal" | "flash"
    pub vcpu: Option<u32>,
    pub memory_mb: Option<u32>,
    pub rootfs_path: Option<String>,
    pub wasm_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceResponse {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub status: String,
}

pub async fn create_service(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateServiceRequest>,
) -> Result<(StatusCode, Json<ServiceResponse>), StatusCode> {
    let service_id = Uuid::new_v4().to_string();

    let spec = match req.engine.as_str() {
        "metal" => EngineSpec::Metal(MetalSpec {
            vcpu: req.vcpu.unwrap_or(1),
            memory_mb: req.memory_mb.unwrap_or(128),
            rootfs_path: req.rootfs_path.unwrap_or_default(),
        }),
        "flash" => EngineSpec::Flash(FlashSpec {
            wasm_path: req.wasm_path.unwrap_or_default(),
        }),
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let engine = match req.engine.as_str() {
        "metal" => Engine::Metal,
        "flash" => Engine::Flash,
        _ => return Err(StatusCode::BAD_REQUEST),
    };

    let event = ProvisionEvent {
        tenant_id: req.workspace_id,
        service_id: service_id.clone(),
        app_name: req.name.clone(),
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
            id: service_id,
            name: req.name,
            engine: req.engine,
            status: "provisioning".to_string(),
        }),
    ))
}

pub async fn get_service(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<ServiceResponse> {
    // TODO: query services table
    Json(ServiceResponse {
        id,
        name: "unknown".to_string(),
        engine: "metal".to_string(),
        status: "running".to_string(),
    })
}

pub async fn delete_service(
    State(_state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> StatusCode {
    tracing::info!(service_id = id, "delete requested");
    // TODO: publish deprovision event
    StatusCode::NO_CONTENT
}
