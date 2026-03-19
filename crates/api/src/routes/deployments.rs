use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::StreamExt as _;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use super::{ApiError, Caller, db_conn, require_scope};
use crate::AppState;
use crate::envelope;
use common::contract;
use common::events::{DeployProgressEvent, DeployStep, SUBJECT_DEPLOY_PROGRESS};

#[utoipa::path(post, path = "/deployments/upload-url", request_body = contract::UploadUrlRequest, responses(
    (status = 200, description = "Presigned S3 upload URL", body = contract::UploadUrlResponse),
), tag = "Deployments", security(("api_key" = [])))]
pub async fn get_upload_url(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<contract::UploadUrlRequest>,
) -> Result<Json<contract::UploadUrlResponse>, ApiError> {
    require_scope(&caller, "write")?;

    if body.deploy_id.is_empty() || body.project_id.is_empty() {
        return Err(ApiError::bad_request(
            "missing_fields",
            "deploy_id and project_id are required",
        ));
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| {
        ApiError::bad_request("invalid_project_id", "project_id must be a valid UUID")
    })?;
    let db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| ApiError::internal("project lookup failed"))?
        .ok_or_else(|| ApiError::not_found("project not found"))?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?;

    if !member.get::<_, bool>(0) {
        return Err(ApiError::forbidden("not a member of this workspace"));
    }

    let (engine_prefix, artifact_name) = match body.engine.as_str() {
        "liquid" => ("wasm", "main.wasm"),
        "metal" => ("metal", "app"),
        _ => {
            return Err(ApiError::bad_request(
                "invalid_engine",
                "engine must be 'metal' or 'liquid'",
            ));
        }
    };

    let artifact_key = format!(
        "{}/{}/{}/{}",
        engine_prefix, body.project_id, body.deploy_id, artifact_name
    );

    let expiry_secs = if body.engine == "metal" { 1800 } else { 300 };
    let expires = aws_sdk_s3::presigning::PresigningConfig::expires_in(
        std::time::Duration::from_secs(expiry_secs),
    )
    .map_err(|_| ApiError::internal("presign config error"))?;

    let presigned = state
        .s3
        .put_object()
        .bucket(&state.bucket)
        .key(&artifact_key)
        .presigned(expires)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "S3 presign failed");
            ApiError::internal("failed to generate upload URL")
        })?;

    Ok(Json(contract::UploadUrlResponse {
        upload_url: presigned.uri().to_string(),
        artifact_key,
    }))
}

#[utoipa::path(post, path = "/deployments", request_body = contract::DeployRequest, responses(
    (status = 200, description = "Service deployed", body = contract::DeployResponse),
    (status = 409, description = "Service slug already active"),
    (status = 422, description = "Tier limit exceeded"),
), tag = "Deployments", security(("api_key" = [])))]
pub async fn deploy_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(body): Json<contract::DeployRequest>,
) -> Result<Json<contract::DeployResponse>, ApiError> {
    require_scope(&caller, "write")?;

    if body.name.is_empty() {
        return Err(ApiError::bad_request(
            "missing_name",
            "service name is required",
        ));
    }
    if body.name.len() > 63 {
        return Err(ApiError::bad_request(
            "name_too_long",
            "service name must be 63 characters or fewer",
        ));
    }
    if body.artifact_key.is_empty() {
        return Err(ApiError::bad_request(
            "missing_artifact_key",
            "artifact_key is required",
        ));
    }

    let pid: Uuid = body.project_id.parse().map_err(|_| {
        ApiError::bad_request("invalid_project_id", "project_id must be a valid UUID")
    })?;
    let mut db = db_conn(&state.db).await?;

    let row = db
        .query_opt(
            "SELECT workspace_id FROM projects WHERE id = $1 AND deleted_at IS NULL",
            &[&pid],
        )
        .await
        .map_err(|_| ApiError::internal("project lookup failed"))?
        .ok_or_else(|| ApiError::not_found("project not found"))?;

    let wid: Uuid = row.get("workspace_id");

    let member = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = $1 AND user_id = $2)",
            &[&wid, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("workspace membership check failed"))?;

    if !member.get::<_, bool>(0) {
        return Err(ApiError::forbidden("not a member of this workspace"));
    }

    let tier_row = db
        .query_one("SELECT tier FROM workspaces WHERE id = $1", &[&wid])
        .await
        .map_err(|_| ApiError::internal("workspace tier lookup failed"))?;
    let tier: String = tier_row.get("tier");
    let limits = crate::quota::limits_for(&tier);

    let engine: common::Engine = body.engine.parse().map_err(|_| {
        ApiError::bad_request("invalid_engine", "engine must be 'metal' or 'liquid'")
    })?;

    let engine_spec = match engine {
        common::Engine::Metal => {
            // Platform-managed: 1 vCPU, 128 MB for all Metal VMs.
            // Users don't pick resources — serverless model.
            let port = body.port.unwrap_or(0) as u16;
            if port == 0 {
                return Err(ApiError::bad_request(
                    "invalid_metal_spec",
                    "metal deploys require port >= 1",
                ));
            }
            common::EngineSpec::Metal(common::MetalSpec {
                vcpu: 1,
                memory_mb: 128,
                port,
                artifact_key: body.artifact_key.clone(),
                artifact_sha256: if body.sha256.is_empty() {
                    None
                } else {
                    Some(body.sha256.clone())
                },
                quota: state.default_quota.clone(),
            })
        }
        common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key: body.artifact_key.clone(),
            artifact_sha256: if body.sha256.is_empty() {
                None
            } else {
                Some(body.sha256.clone())
            },
        }),
    };

    let (vcpu_i, memory_mb_i, port_i) = match &engine_spec {
        common::EngineSpec::Metal(m) => (m.vcpu as i32, m.memory_mb as i32, m.port as i32),
        common::EngineSpec::Liquid(_) => (0i32, 0i32, 0i32),
    };

    let engine_str = engine.as_str();

    let service_id = Uuid::now_v7();
    let slug = if body.slug.is_empty() {
        common::slugify(&body.name)
    } else {
        body.slug.clone()
    };
    if slug.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "service name must produce a non-empty slug (use ASCII alphanumeric characters)",
        ));
    }
    if !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError::bad_request(
            "invalid_slug",
            "slug must contain only lowercase alphanumeric characters and dashes",
        ));
    }

    // ── Capacity & limit checks (short-lived advisory lock) ────────────────
    let lock_key = {
        let b = wid.as_bytes();
        i64::from_be_bytes(b[0..8].try_into().unwrap())
            ^ i64::from_be_bytes(b[8..16].try_into().unwrap())
    };

    {
        let check_txn = db
            .build_transaction()
            .start()
            .await
            .map_err(|_| ApiError::internal("failed to start check transaction"))?;

        check_txn
            .execute("SELECT pg_advisory_xact_lock($1)", &[&lock_key])
            .await
            .map_err(|_| ApiError::internal("failed to acquire workspace lock"))?;

        let svc_count: i64 = check_txn
            .query_one(
                "SELECT COUNT(*) FROM services WHERE workspace_id = $1 AND deleted_at IS NULL",
                &[&wid],
            )
            .await
            .map_err(|_| ApiError::internal("service count query failed"))?
            .get(0);

        if svc_count >= limits.max_services {
            return Err(ApiError::unprocessable(
                "service_limit_exceeded",
                format!(
                    "workspace has {} services, {} tier limit is {}",
                    svc_count, tier, limits.max_services
                ),
            ));
        }

        let active_slug: bool = check_txn
            .query_one(
                "SELECT EXISTS(\
                   SELECT 1 FROM services \
                   WHERE workspace_id = $1 AND slug = $2 \
                     AND status IN ('running', 'provisioning') \
                     AND deleted_at IS NULL\
                 )",
                &[&wid, &slug],
            )
            .await
            .map_err(|_| ApiError::internal("active slug check failed"))?
            .get(0);

        if active_slug {
            return Err(ApiError::conflict(
                "a service with this slug is currently running or provisioning — stop it first",
            ));
        }

        check_txn
            .commit()
            .await
            .map_err(|_| ApiError::internal("check transaction commit failed"))?;
    }

    // ── Deploy transaction ──────────────────────────────────────────────────
    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    txn.execute(
        "UPDATE services \
         SET deleted_at = NOW() \
         WHERE workspace_id = $1 AND slug = $2 \
           AND deleted_at IS NULL AND status IN ('stopped', 'failed')",
        &[&wid, &slug],
    )
    .await
    .map_err(|_| ApiError::internal("old service cleanup failed"))?;

    txn.execute(
        "INSERT INTO services \
         (id, project_id, workspace_id, name, slug, engine, status, \
          artifact_key, commit_sha, vcpu, memory_mb, port) \
         VALUES ($1, $2, $3, $4, $5, $6, 'provisioning', $7, $8, $9, $10, $11)",
        &[
            &service_id,
            &pid,
            &wid,
            &body.name,
            &slug,
            &engine_str,
            &body.artifact_key,
            &body.sha256,
            &vcpu_i,
            &memory_mb_i,
            &port_i,
        ],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "service insert failed");
        ApiError::internal("failed to create service")
    })?;

    let event = common::ProvisionEvent {
        tenant_id: wid.to_string(),
        service_id: service_id.to_string(),
        app_name: body.name.clone(),
        slug: slug.clone(),
        engine: engine.clone(),
        spec: engine_spec,
        env_vars: Default::default(),
    };

    let payload = serde_json::to_value(&event).map_err(|e| {
        tracing::error!(error = %e, "serializing provision event");
        ApiError::internal("failed to serialize provision event")
    })?;
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&common::events::SUBJECT_PROVISION, &payload],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "outbox insert failed");
        ApiError::internal("failed to queue provision event")
    })?;

    txn.execute(
        "INSERT INTO deployments (service_id, workspace_id, slug, engine, artifact_key, commit_sha, port, deployed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[
            &service_id, &wid, &slug, &engine_str,
            &body.artifact_key, &body.sha256, &port_i, &caller.user_id,
        ],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "deployments insert failed");
        ApiError::internal("failed to record deployment")
    })?;

    txn.commit()
        .await
        .map_err(|_| ApiError::internal("failed to commit deploy transaction"))?;

    tracing::info!(
        target: "audit",
        action = "deploy_service",
        user_id = %caller.user_id,
        service_id = %service_id,
        slug,
        engine = engine_str,
        workspace_id = %wid,
        result = "ok",
    );

    Ok(Json(contract::DeployResponse {
        service: contract::DeployedService {
            id: service_id.to_string(),
            name: body.name,
            slug,
            status: "provisioning".to_string(),
        },
    }))
}

#[utoipa::path(get, path = "/services/{id}/deploys", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Deployment history", body = contract::DeploymentHistoryResponse),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn list_deploys(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<contract::DeploymentHistoryResponse>, ApiError> {
    require_scope(&caller, "read")?;
    let service_id: Uuid = id.parse().map_err(|_| {
        ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID")
    })?;
    let db = db_conn(&state.db).await?;

    db.query_opt(
        "SELECT 1 FROM services s \
         JOIN projects p ON p.id = s.project_id \
         JOIN workspace_members wm ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
         WHERE s.id = $1 AND s.deleted_at IS NULL",
        &[&service_id, &caller.user_id],
    )
    .await
    .map_err(|_| ApiError::internal("service lookup failed"))?
    .ok_or_else(|| ApiError::not_found("service not found"))?;

    let rows = db
        .query(
            "SELECT d.id::text, d.slug, d.engine, d.artifact_key, d.commit_sha, d.created_at::text, \
                    (d.artifact_key = s.artifact_key) AS is_active \
             FROM deployments d \
             JOIN services s ON s.id = d.service_id \
             WHERE d.service_id = $1 ORDER BY d.created_at DESC LIMIT 20",
            &[&service_id],
        )
        .await
        .map_err(|_| ApiError::internal("deploys query failed"))?;

    let deploys: Vec<contract::DeploymentHistoryEntry> = rows
        .iter()
        .map(|r| contract::DeploymentHistoryEntry {
            id: r.get("id"),
            slug: r.get("slug"),
            engine: r.get("engine"),
            artifact_key: r.get("artifact_key"),
            commit_sha: r.get("commit_sha"),
            created_at: r.get("created_at"),
            is_active: r.get("is_active"),
        })
        .collect();

    Ok(Json(contract::DeploymentHistoryResponse { deploys }))
}

#[utoipa::path(post, path = "/services/{id}/rollback", request_body = contract::RollbackRequest, params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Rollback initiated", body = contract::DeployResponse),
    (status = 404, description = "Service or deploy not found"),
    (status = 422, description = "No previous deployment to rollback to"),
), tag = "Services", security(("api_key" = [])))]
pub async fn rollback_service(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<contract::RollbackRequest>,
) -> Result<Json<contract::DeployResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| {
        ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID")
    })?;
    let mut db = db_conn(&state.db).await?;

    let svc = db
        .query_opt(
            "SELECT s.id, s.name, s.slug, s.engine, s.workspace_id, s.project_id, \
                    s.vcpu, s.memory_mb, s.port \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let slug: String = svc.get("slug");
    let name: String = svc.get("name");
    let wid: Uuid = svc.get("workspace_id");

    let target = if let Some(deploy_id) = &body.deploy_id {
        let did: Uuid = deploy_id.parse().map_err(|_| {
            ApiError::bad_request("invalid_deploy_id", "deploy ID must be a valid UUID")
        })?;
        db.query_opt(
            "SELECT artifact_key, commit_sha, engine, port FROM deployments WHERE id = $1 AND service_id = $2",
            &[&did, &service_id],
        )
        .await
        .map_err(|_| ApiError::internal("deploy lookup failed"))?
        .ok_or_else(|| ApiError::not_found("deployment not found"))?
    } else {
        db.query_opt(
            "SELECT artifact_key, commit_sha, engine, port FROM deployments \
             WHERE service_id = $1 ORDER BY created_at DESC OFFSET 1 LIMIT 1",
            &[&service_id],
        )
        .await
        .map_err(|_| ApiError::internal("deploy lookup failed"))?
        .ok_or_else(|| {
            ApiError::unprocessable(
                "no_previous_deploy",
                "no previous deployment to rollback to".to_string(),
            )
        })?
    };

    let artifact_key: String = target.get("artifact_key");
    let commit_sha: Option<String> = target.get("commit_sha");
    let target_engine: String = target.get("engine");
    let port: Option<i32> = target.get("port");

    let engine: common::Engine = target_engine
        .parse()
        .map_err(|e: String| ApiError::internal(&e))?;
    let engine_spec = match &engine {
        common::Engine::Metal => common::EngineSpec::Metal(common::MetalSpec {
            vcpu: svc.get::<_, i32>("vcpu") as u32,
            memory_mb: svc.get::<_, i32>("memory_mb") as u32,
            port: port.unwrap_or(8080) as u16,
            artifact_key: artifact_key.clone(),
            artifact_sha256: None,
            quota: state.default_quota.clone(),
        }),
        common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
            artifact_key: artifact_key.clone(),
            artifact_sha256: None,
        }),
    };

    // Decrypt env vars from the encrypted columns (not the legacy plaintext column).
    let env_row = db
        .query_one(
            "SELECT env_ciphertext, env_nonce FROM services WHERE id = $1",
            &[&service_id],
        )
        .await
        .map_err(|_| ApiError::internal("env vars lookup failed"))?;
    let ciphertext: Option<Vec<u8>> = env_row.get("env_ciphertext");
    let nonce: Option<Vec<u8>> = env_row.get("env_nonce");
    let env_vars = match (ciphertext, nonce) {
        (Some(ct), Some(n)) => envelope::decrypt_env_vars(&db, &*state.kms, wid, &ct, &n)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, %service_id, "env var decryption failed");
                ApiError::internal("failed to decrypt environment variables")
            })?,
        _ => std::collections::HashMap::new(),
    };

    let event = common::ProvisionEvent {
        tenant_id: wid.to_string(),
        service_id: service_id.to_string(),
        app_name: name.clone(),
        slug: slug.clone(),
        engine: engine,
        spec: engine_spec,
        env_vars,
    };

    // Use the outbox pattern: status update + outbox + deployments in one transaction.
    let payload = serde_json::to_value(&event).map_err(|e| {
        tracing::error!(error = %e, "serializing provision event");
        ApiError::internal("failed to serialize provision event")
    })?;

    let txn = db
        .build_transaction()
        .start()
        .await
        .map_err(|_| ApiError::internal("failed to start transaction"))?;

    txn.execute(
        "UPDATE services SET status = 'provisioning', upstream_addr = NULL, \
         artifact_key = $2, commit_sha = $3 WHERE id = $1",
        &[&service_id, &artifact_key, &commit_sha],
    )
    .await
    .map_err(|_| ApiError::internal("failed to update service for rollback"))?;

    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&common::events::SUBJECT_PROVISION, &payload],
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "outbox insert failed");
        ApiError::internal("failed to queue rollback event")
    })?;

    txn.execute(
        "INSERT INTO deployments (service_id, workspace_id, slug, engine, artifact_key, commit_sha, port, deployed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&service_id, &wid, &slug, &target_engine, &artifact_key, &commit_sha, &port, &caller.user_id],
    ).await.map_err(|e| {
        tracing::error!(error = %e, "deployments insert failed");
        ApiError::internal("failed to record deployment")
    })?;

    txn.commit()
        .await
        .map_err(|_| ApiError::internal("failed to commit rollback transaction"))?;

    tracing::info!(
        target: "audit",
        action = "rollback_service",
        user_id = %caller.user_id,
        service_id = %service_id,
        slug,
        result = "ok",
    );

    Ok(Json(contract::DeployResponse {
        service: contract::DeployedService {
            id: service_id.to_string(),
            name,
            slug,
            status: "provisioning".to_string(),
        },
    }))
}

/// Stream live provisioning progress for a service as Server-Sent Events.
///
/// Opens a NATS subscription on `platform.deploy_progress.{id}` and forwards
/// each step to the client. The stream closes automatically when the daemon
/// publishes a terminal step (`running` or `failed`), or after a 5-minute timeout.
/// If the service is already in a terminal state when the client connects, a
/// single final event is sent immediately.
pub async fn stream_deploy(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(service_id): Path<Uuid>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let db = db_conn(&state.db).await?;

    // Verify the caller is a member of the workspace that owns this service.
    let row = db
        .query_opt(
            "SELECT s.status \
             FROM services s \
             JOIN projects p ON p.id = s.project_id \
             JOIN workspace_members wm ON wm.workspace_id = p.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let current_status: String = row.get("status");

    // Subscribe to NATS BEFORE reading status to close the race window where
    // the daemon finishes between the status check and the subscribe call.
    let nats_subject = format!("{}.{}", SUBJECT_DEPLOY_PROGRESS, service_id);
    let sub = state
        .nats_client
        .subscribe(nats_subject)
        .await
        .map_err(|_| ApiError::internal("subscribe failed"))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(32);

    tokio::spawn(async move {
        // Already in a terminal state — send one event and close.
        if current_status == "running" {
            tx.send(Ok(progress_sse_event(
                DeployStep::Running,
                "Service is already running",
            )))
            .await
            .ok();
            return;
        }
        if current_status == "failed" {
            tx.send(Ok(progress_sse_event(
                DeployStep::Failed,
                "Service failed to provision",
            )))
            .await
            .ok();
            return;
        }

        let mut sub = sub;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(300);

        loop {
            tokio::select! {
                biased;
                msg = sub.next() => {
                    let Some(msg) = msg else { break };
                    let Ok(ev) = serde_json::from_slice::<DeployProgressEvent>(&msg.payload) else {
                        continue;
                    };
                    let terminal = matches!(ev.step, DeployStep::Running | DeployStep::Ready | DeployStep::Failed);
                    tx.send(Ok(progress_sse_event(ev.step, &ev.message))).await.ok();
                    if terminal { break; }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    tx.send(Ok(progress_sse_event(DeployStep::Failed, "Deploy timed out — run `flux services` to check status"))).await.ok();
                    break;
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

fn progress_sse_event(step: DeployStep, message: &str) -> Event {
    let step_str = match step {
        DeployStep::Queued => "queued",
        DeployStep::Downloading => "downloading",
        DeployStep::Verifying => "verifying",
        DeployStep::Building => "building",
        DeployStep::Booting => "booting",
        DeployStep::Starting => "starting",
        DeployStep::HealthCheck => "health_check",
        DeployStep::Snapshotting => "snapshotting",
        DeployStep::Ready => "ready",
        DeployStep::Running => "running",
        DeployStep::Failed => "failed",
    };
    Event::default().data(format!(
        r#"{{"step":"{step_str}","message":{}}}"#,
        serde_json::json!(message)
    ))
}
