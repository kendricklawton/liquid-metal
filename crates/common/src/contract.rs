//! Shared HTTP request/response types for the Liquid Metal API contract.
//!
//! These types are the canonical shapes exchanged between the API, CLI, and web
//! crates over REST/JSON. Keeping them in `common` eliminates duplication and
//! guarantees wire-format consistency across all three consumers.
//!
//! **Design rules:**
//! - All types derive both `Serialize` and `Deserialize` (producers and consumers
//!   share the same definition).
//! - Owned `String` fields everywhere — no lifetimes. CLI and web crates
//!   previously used borrowed `&str` in request types; those stay local as
//!   thin wrappers that serialize into these shapes.
//! - `Option<T>` for fields that may be absent in either direction.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ── Auth ────────────────────────────────────────────────────────────────────

/// POST /auth/provision, POST /auth/cli/provision request body.
///
/// Used by both the web BFF (browser OIDC callback) and CLI (device flow).
/// The `invite_code` field is only sent by the CLI for new-user registration;
/// the web BFF path is gated by `X-Internal-Secret` instead.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProvisionRequest {
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    #[serde(default)]
    pub oidc_sub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
}

/// Response from POST /auth/provision and POST /auth/cli/provision.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProvisionResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub tier: String,
    pub workspace_id: String,
    #[serde(default)]
    pub oidc_sub: Option<String>,
    /// Scoped API key (`lm_*`) for CLI auth. Only set on cli_provision responses.
    #[serde(default)]
    pub api_key: Option<String>,
}

// ── Users ───────────────────────────────────────────────────────────────────

/// GET /users/me response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub name: String,
}

// ── Workspaces ──────────────────────────────────────────────────────────────

/// Single workspace object returned by GET /workspaces.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub tier: String,
    /// Caller's role in this workspace: "owner" | "admin" | "viewer"
    pub role: String,
}

// ── Projects ────────────────────────────────────────────────────────────────

/// Single project object returned by GET /projects and POST /projects.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ProjectResponse {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub slug: String,
}

/// POST /projects request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    pub workspace_id: String,
    pub name: String,
    pub slug: String,
}

/// POST /projects response wrapper.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateProjectResponse {
    pub project: ProjectResponse,
}

// ── Services ────────────────────────────────────────────────────────────────

/// Single service object returned by GET /services.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ServiceResponse {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub engine: String,
    pub status: String,
    #[serde(default)]
    pub upstream_addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    /// Metal-only: VM tier (one/two/four).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metal_tier: Option<String>,
    /// Metal-only: monthly price in cents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly_price_cents: Option<i32>,
}

/// Single log line returned by GET /services/:id/logs.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LogLineResponse {
    #[serde(default)]
    pub ts: Option<String>,
    pub message: String,
}

// ── Deploy ──────────────────────────────────────────────────────────────────

/// POST /deployments/upload-url request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UploadUrlRequest {
    pub engine: String,
    pub deploy_id: String,
    pub project_id: String,
}

/// POST /deployments/upload-url response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UploadUrlResponse {
    pub upload_url: String,
    pub artifact_key: String,
}

/// POST /deployments request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeployRequest {
    pub name: String,
    pub slug: String,
    pub engine: String,
    pub project_id: String,
    pub artifact_key: String,
    pub sha256: String,
    /// Metal-only: the port the user's binary listens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u32>,
    /// Metal-only: VM tier — "one" (1 vCPU), "two" (2 vCPU), or "four" (4 vCPU).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

/// Inner service object in the deploy response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeployedService {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub status: String,
}

/// POST /deployments response wrapper.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeployResponse {
    pub service: DeployedService,
}

// ── Delete ──────────────────────────────────────────────────────────────────

/// POST /services/:id/delete response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeleteServiceResponse {
    pub id: String,
    pub slug: String,
    pub deleted: bool,
}

// ── Env Vars ────────────────────────────────────────────────────────────────

/// GET /services/:id/env response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvVarsResponse {
    pub vars: std::collections::HashMap<String, String>,
}

/// POST /services/:id/env request body (set/merge).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SetEnvVarsRequest {
    pub vars: std::collections::HashMap<String, String>,
}

/// POST /services/:id/env/unset request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UnsetEnvVarsRequest {
    pub keys: Vec<String>,
}

// ── Domains ─────────────────────────────────────────────────────────────────

/// Single domain object.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DomainResponse {
    pub id: String,
    pub domain: String,
    pub is_verified: bool,
    pub verification_type: String,
    pub verification_token: String,
    pub tls_status: String,
    pub created_at: String,
}

/// POST /services/:id/domains request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AddDomainRequest {
    pub domain: String,
}

/// POST /services/:id/domains/:domain_id/verify response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VerifyDomainResponse {
    pub domain: String,
    pub is_verified: bool,
    pub message: String,
}

// ── Deploy History ──────────────────────────────────────────────────────────

/// Single deployment history entry.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentHistoryEntry {
    pub id: String,
    pub slug: String,
    pub engine: String,
    pub artifact_key: String,
    pub commit_sha: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub is_active: Option<bool>,
}

/// GET /services/:id/deploys response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeploymentHistoryResponse {
    pub deploys: Vec<DeploymentHistoryEntry>,
}

/// POST /services/:id/rollback request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RollbackRequest {
    #[serde(default)]
    pub deploy_id: Option<String>,
}

// ── Invite codes ────────────────────────────────────────────────────────────

/// POST /admin/invites request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateInvitesRequest {
    #[serde(default)]
    pub count: Option<i64>,
}

/// POST /admin/invites response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateInvitesResponse {
    pub codes: Vec<String>,
}

// ── Billing ─────────────────────────────────────────────────────────────────

/// POST /billing/topup request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TopupRequest {
    pub amount_cents: u64,
    pub success_url: String,
    pub cancel_url: String,
}

/// Response from POST /billing/topup.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CheckoutResponse {
    pub checkout_url: String,
    pub session_id: String,
}

/// GET /billing/balance response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct BalanceResponse {
    /// Current balance in micro-credits (1 µcr = $0.000001).
    pub balance: i64,
    /// Free Liquid invocations used this month.
    pub free_invocations_used: i64,
    /// Free Liquid invocations limit per month (1M).
    pub free_invocations_limit: i64,
}

/// GET /billing/usage response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct UsageResponse {
    /// Total monthly Metal cost across all running services (cents).
    pub metal_monthly_total_cents: i64,
    /// Liquid invocation usage this period.
    pub liquid: LiquidUsage,
}

/// Liquid usage breakdown within the usage response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LiquidUsage {
    pub invocations: i64,
    pub cost_microcredits: i64,
}

/// GET /billing/ledger response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LedgerResponse {
    pub entries: Vec<LedgerEntry>,
}

/// Single ledger entry.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LedgerEntry {
    pub id: String,
    pub amount: i64,
    pub kind: String,
    pub description: Option<String>,
    pub reference_id: Option<String>,
    pub balance_after: i64,
    pub created_at: String,
}

// ── API Keys ─────────────────────────────────────────────────────────────────

/// POST /api-keys request body.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// Scopes: "read", "write", "admin". Defaults to ["read"].
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional expiry in days from now.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

/// POST /api-keys response — the only time the raw token is returned.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub id: String,
    pub name: String,
    pub token: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Single API key in GET /api-keys list (token is never returned again).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyResponse {
    pub id: String,
    pub name: String,
    pub token_prefix: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
    pub created_at: String,
}

/// GET /api-keys response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyListResponse {
    pub keys: Vec<ApiKeyResponse>,
}

/// DELETE /api-keys/:id response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DeleteApiKeyResponse {
    pub id: String,
    pub deleted: bool,
}

// ── Health ───────────────────────────────────────────────────────────────────

/// GET /healthz response.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub db: String,
    pub nats: String,
}
