use std::sync::Arc;

use axum::{Extension, Json, extract::{Path, State}, http::StatusCode};
use uuid::Uuid;

use crate::AppState;
use common::contract;
use super::{ApiError, Caller, db_conn, require_scope, require_workspace_role};

// ── GET /services/:id/domains ────────────────────────────────────────────────

#[utoipa::path(get, path = "/services/{id}/domains", params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "List of domains", body = Vec<contract::DomainResponse>),
    (status = 404, description = "Service not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn list_domains(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<Vec<contract::DomainResponse>>, ApiError> {
    require_scope(&caller, "read")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    // Verify ownership.
    db.query_opt(
        "SELECT 1 FROM services s \
         JOIN workspace_members wm ON wm.workspace_id = s.workspace_id AND wm.user_id = $2 \
         WHERE s.id = $1 AND s.deleted_at IS NULL",
        &[&service_id, &caller.user_id],
    )
    .await
    .map_err(|_| ApiError::internal("service lookup failed"))?
    .ok_or_else(|| ApiError::not_found("service not found"))?;

    let rows = db
        .query(
            "SELECT id::text, domain, is_verified, verification_type, verification_token, \
                    tls_status, created_at::text \
             FROM domains WHERE service_id = $1 ORDER BY created_at",
            &[&service_id],
        )
        .await
        .map_err(|_| ApiError::internal("domains query failed"))?;

    let domains: Vec<contract::DomainResponse> = rows
        .iter()
        .map(|r| contract::DomainResponse {
            id:                 r.get("id"),
            domain:             r.get("domain"),
            is_verified:        r.get("is_verified"),
            verification_type:  r.get("verification_type"),
            verification_token: r.get("verification_token"),
            tls_status:         r.get("tls_status"),
            created_at:         r.get("created_at"),
        })
        .collect();

    Ok(Json(domains))
}

// ── POST /services/:id/domains ──────────────────────────────────────────────

#[utoipa::path(post, path = "/services/{id}/domains", request_body = contract::AddDomainRequest, params(
    ("id" = String, Path, description = "Service UUID"),
), responses(
    (status = 200, description = "Domain added", body = contract::DomainResponse),
    (status = 404, description = "Service not found"),
    (status = 409, description = "Domain already exists"),
), tag = "Services", security(("api_key" = [])))]
pub async fn add_domain(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    Json(body): Json<contract::AddDomainRequest>,
) -> Result<Json<contract::DomainResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    // Verify ownership and get project_id.
    let svc = db
        .query_opt(
            "SELECT s.project_id, wm.role FROM services s \
             JOIN workspace_members wm ON wm.workspace_id = s.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;

    let role: String = svc.get("role");
    require_workspace_role(&role, "admin")?;

    let project_id: Uuid = svc.get("project_id");

    let domain = body.domain.to_lowercase().trim().to_string();
    if domain.is_empty() || !domain.contains('.') {
        return Err(ApiError::bad_request("invalid_domain", "provide a valid domain like example.com"));
    }

    let domain_id = Uuid::now_v7();
    let row = db
        .query_one(
            "INSERT INTO domains (id, project_id, service_id, domain) \
             VALUES ($1, $2, $3, $4) \
             RETURNING id::text, domain, is_verified, verification_type, verification_token, \
                       tls_status, created_at::text",
            &[&domain_id, &project_id, &service_id, &domain],
        )
        .await
        .map_err(|e| {
            if e.to_string().contains("unique") || e.to_string().contains("duplicate") {
                ApiError::conflict("domain already exists".to_string())
            } else {
                tracing::error!(error = %e, "domain insert failed");
                ApiError::internal("failed to add domain")
            }
        })?;

    tracing::info!(
        target: "audit",
        action = "add_domain",
        user_id = %caller.user_id,
        ip = ?caller.ip,
        service_id = %service_id,
        domain = %domain,
    );

    Ok(Json(contract::DomainResponse {
        id:                 row.get("id"),
        domain:             row.get("domain"),
        is_verified:        row.get("is_verified"),
        verification_type:  row.get("verification_type"),
        verification_token: row.get("verification_token"),
        tls_status:         row.get("tls_status"),
        created_at:         row.get("created_at"),
    }))
}

// ── POST /services/:id/domains/:domain/verify ──────────────────────────────

#[utoipa::path(post, path = "/services/{id}/domains/{domain}/verify", params(
    ("id" = String, Path, description = "Service UUID"),
    ("domain" = String, Path, description = "Domain name"),
), responses(
    (status = 200, description = "Verification result", body = contract::VerifyDomainResponse),
    (status = 404, description = "Domain not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn verify_domain(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, domain_name)): Path<(String, String)>,
) -> Result<Json<contract::VerifyDomainResponse>, ApiError> {
    require_scope(&caller, "write")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    // Get the domain record + service slug (for CNAME verification target).
    let row = db
        .query_opt(
            "SELECT d.id, d.domain, d.verification_type, d.verification_token, s.slug, wm.role \
             FROM domains d \
             JOIN services s ON s.id = d.service_id \
             JOIN workspace_members wm ON wm.workspace_id = s.workspace_id AND wm.user_id = $3 \
             WHERE d.service_id = $1 AND d.domain = $2 AND s.deleted_at IS NULL",
            &[&service_id, &domain_name, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("domain lookup failed"))?
        .ok_or_else(|| ApiError::not_found("domain not found"))?;

    let role: String = row.get("role");
    require_workspace_role(&role, "admin")?;

    let domain_id: Uuid = row.get("id");
    let token: String = row.get("verification_token");
    let slug: String = row.get("slug");
    let domain: String = row.get("domain");

    // DNS verification: try CNAME first, then TXT.
    let verified = verify_domain_dns(&domain, &slug, &token).await;

    if verified {
        db.execute(
            "UPDATE domains SET is_verified = true WHERE id = $1",
            &[&domain_id],
        )
        .await
        .map_err(|_| ApiError::internal("failed to update domain verification"))?;
    }

    let message = if verified {
        "Domain verified successfully.".to_string()
    } else {
        format!(
            "DNS verification failed. Ensure a CNAME record for {} points to {}.liquidmetal.dev, \
             or a TXT record at _lm-verify.{} contains {}",
            domain, slug, domain, token
        )
    };

    tracing::info!(
        target: "audit",
        action = "verify_domain",
        user_id = %caller.user_id,
        ip = ?caller.ip,
        service_id = %service_id,
        domain = %domain,
        verified,
    );

    Ok(Json(contract::VerifyDomainResponse {
        domain,
        is_verified: verified,
        message,
    }))
}

/// Check DNS records for domain ownership verification.
///
/// Two methods (either passing is sufficient):
/// 1. **CNAME**: `{domain}` has a CNAME record pointing to `{slug}.{platform_domain}.`
/// 2. **TXT**:   `_lm-verify.{domain}` has a TXT record containing the verification token.
async fn verify_domain_dns(domain: &str, slug: &str, token: &str) -> bool {
    use hickory_resolver::TokioResolver;
    use hickory_resolver::proto::rr::RecordType;

    let resolver = match TokioResolver::builder_tokio() {
        Ok(builder) => builder.build(),
        Err(e) => {
            tracing::error!(error = %e, "failed to create DNS resolver");
            return false;
        }
    };

    let platform_domain = common::config::env_or("PLATFORM_DOMAIN", "liquidmetal.dev");
    let expected_cname = format!("{slug}.{platform_domain}.");

    // 1. CNAME check: resolve the domain and verify the CNAME target.
    if let Ok(lookup) = resolver.lookup(domain, RecordType::CNAME).await {
        for rdata in lookup.iter() {
            let target = rdata.to_string();
            if target.trim_end_matches('.') == expected_cname.trim_end_matches('.') {
                tracing::info!(domain, %target, "domain CNAME verified");
                return true;
            }
        }
    }

    // 2. TXT check: look for the verification token at _lm-verify.{domain}.
    let txt_name = format!("_lm-verify.{domain}");
    if let Ok(lookup) = resolver.txt_lookup(&txt_name).await {
        for record in lookup.iter() {
            let txt_value = record.to_string();
            if txt_value.trim() == token {
                tracing::info!(domain, "domain TXT verified");
                return true;
            }
        }
    }

    false
}

// ── POST /services/:id/domains/:domain/remove ──────────────────────────────

#[utoipa::path(post, path = "/services/{id}/domains/{domain}/remove", params(
    ("id" = String, Path, description = "Service UUID"),
    ("domain" = String, Path, description = "Domain name"),
), responses(
    (status = 204, description = "Domain removed"),
    (status = 404, description = "Domain not found"),
), tag = "Services", security(("api_key" = [])))]
pub async fn remove_domain(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Path((id, domain_name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_scope(&caller, "admin")?;
    let service_id: Uuid = id.parse().map_err(|_| ApiError::bad_request("invalid_service_id", "service ID must be a valid UUID"))?;
    let db = db_conn(&state.db).await?;

    let svc = db
        .query_opt(
            "SELECT wm.role FROM services s \
             JOIN workspace_members wm ON wm.workspace_id = s.workspace_id AND wm.user_id = $2 \
             WHERE s.id = $1 AND s.deleted_at IS NULL",
            &[&service_id, &caller.user_id],
        )
        .await
        .map_err(|_| ApiError::internal("service lookup failed"))?
        .ok_or_else(|| ApiError::not_found("service not found"))?;
    let role: String = svc.get("role");
    require_workspace_role(&role, "admin")?;

    let count = db
        .execute(
            "DELETE FROM domains WHERE service_id = $1 AND domain = $2",
            &[&service_id, &domain_name],
        )
        .await
        .map_err(|_| ApiError::internal("domain delete failed"))?;

    if count == 0 {
        return Err(ApiError::not_found("domain not found"));
    }

    tracing::info!(
        target: "audit",
        action = "remove_domain",
        user_id = %caller.user_id,
        ip = ?caller.ip,
        service_id = %service_id,
        domain = %domain_name,
    );

    Ok(StatusCode::NO_CONTENT)
}
