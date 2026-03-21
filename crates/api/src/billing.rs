//! Billing: usage aggregation, credit deduction, Stripe webhooks, and billing endpoints.
//!
//! ## Billing model
//!
//! - Metal: fixed monthly price per service ($17/$33/$66 for 1/2/4 vCPU).
//!   30-day billing cycle per service. Charged at cycle renewal.
//! - Liquid: per-invocation at $0.30/1M. 1M free invocations per workspace per month.
//! - No tiers, no subscriptions. Top-up credits only.
//! - Single `balance` column on workspaces.
//!
//! ## Data flow
//!
//! Daemon (60s tick) ──LiquidUsageEvent──► NATS ──► `usage_subscriber` ──► usage_events table
//!                                                       │
//!                                          billing_aggregator (60s) ──► credit_ledger (debit)
//!                                                       │                workspaces.balance -= cost
//!                                                       ▼ (balance <= 0)
//!                                                 SuspendEvent ──► NATS ──► Daemon
//!
//! Metal billing: `metal_billing_cycle` runs hourly. Finds Metal services whose
//! 30-day billing cycle has elapsed, deducts monthly_price_cents from workspace
//! balance, advances billing_cycle_start.
//!
//! Liquid billing: `billing_aggregator` runs every 60s, sums unbilled Liquid
//! usage_events per workspace, applies free invocation allowance, deducts excess
//! at $0.30/1M from workspace balance.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Extension;
use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use futures::StreamExt;

use common::events::{
    LiquidUsageEvent, MetalUsageEvent, SuspendEvent,
    SUBJECT_SUSPEND, SUBJECT_USAGE_LIQUID, SUBJECT_USAGE_METAL,
};
use common::pricing::{FREE_LIQUID_INVOCATIONS, LIQUID_PRICE_PER_MILLION};

use common::contract;
use crate::routes;

use crate::AppState;
use crate::routes::{ApiError, db_conn};

/// 1 cent = 10,000 micro-credits. $1 = 1,000,000 micro-credits.
const MICROCREDITS_PER_CENT: i64 = 10_000;

// ── Public entry point ─────────────────────────────────────────────────────

/// Spawn all billing background tasks. Called once from main.rs.
pub fn spawn_billing_tasks(state: Arc<AppState>) {
    tokio::spawn(usage_subscriber(state.clone()));
    tokio::spawn(billing_aggregator(state.clone()));
    tokio::spawn(metal_billing_cycle(state.clone()));
    tokio::spawn(free_invocations_reset(state.clone()));
}

// ── Usage subscriber ────────────────────────────────────────────────────────

/// Subscribe to usage NATS events and insert into usage_events table.
/// Runs as a background task, reconnects on failure.
pub async fn usage_subscriber(state: Arc<AppState>) {
    loop {
        match usage_subscribe_loop(&state).await {
            Ok(()) => break,
            Err(e) => {
                tracing::warn!(error = %e, "billing usage subscriber disconnected — reconnecting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn usage_subscribe_loop(state: &AppState) -> Result<()> {
    // Queue group ensures only one API instance processes each usage event,
    // preventing duplicate inserts when scaling to multiple instances.
    let mut metal_sub  = state.nats_client.queue_subscribe(SUBJECT_USAGE_METAL, "api-billing".into()).await?;
    let mut liquid_sub = state.nats_client.queue_subscribe(SUBJECT_USAGE_LIQUID, "api-billing".into()).await?;

    tracing::info!("billing: usage subscriber connected");

    loop {
        tokio::select! {
            Some(msg) = metal_sub.next() => {
                // Metal usage events are still consumed for observability (usage_events table)
                // but Metal billing is handled by the fixed monthly cycle, not per-invocation.
                match serde_json::from_slice::<MetalUsageEvent>(&msg.payload) {
                    Ok(ev) => {
                        if let Err(e) = insert_metal_usage(state, &ev).await {
                            tracing::error!(error = %e, service_id = ev.service_id, "billing: failed to insert metal usage");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "billing: failed to parse MetalUsageEvent"),
                }
            }
            Some(msg) = liquid_sub.next() => {
                match serde_json::from_slice::<LiquidUsageEvent>(&msg.payload) {
                    Ok(ev) => {
                        if let Err(e) = insert_liquid_usage(state, &ev).await {
                            tracing::error!(error = %e, service_id = ev.service_id, "billing: failed to insert liquid usage");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "billing: failed to parse LiquidUsageEvent"),
                }
            }
            else => anyhow::bail!("usage NATS subscriptions ended"),
        }
    }
}

async fn insert_metal_usage(state: &AppState, ev: &MetalUsageEvent) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;
    let wid: uuid::Uuid = ev.workspace_id.parse().context("invalid workspace_id")?;
    let sid: uuid::Uuid = ev.service_id.parse().context("invalid service_id")?;
    db.execute(
        "INSERT INTO usage_events (workspace_id, service_id, engine, quantity, duration_ms) \
         VALUES ($1, $2, 'metal', $3, $4)",
        &[&wid, &sid, &(ev.invocations as i64), &(ev.duration_ms as i64)],
    ).await.context("insert metal usage")?;
    Ok(())
}

async fn insert_liquid_usage(state: &AppState, ev: &LiquidUsageEvent) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;
    let wid: uuid::Uuid = ev.workspace_id.parse().context("invalid workspace_id")?;
    let sid: uuid::Uuid = ev.service_id.parse().context("invalid service_id")?;
    db.execute(
        "INSERT INTO usage_events (workspace_id, service_id, engine, quantity) \
         VALUES ($1, $2, 'liquid', $3)",
        &[&wid, &sid, &(ev.invocations as i64)],
    ).await.context("insert liquid usage")?;
    Ok(())
}

// ── Billing aggregator (Liquid only) ──────────────────────────────────────

/// Aggregates unbilled Liquid usage_events into credit deductions.
/// Applies free invocation allowance, then bills excess at $0.30/1M.
pub async fn billing_aggregator(state: Arc<AppState>) {
    let secs: u64 = common::config::env_or("BILLING_INTERVAL_SECS", "60").parse().unwrap_or(60);
    tracing::info!(billing_interval_secs = secs, "billing aggregator configured");
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        if let Err(e) = aggregate_once(&state).await {
            tracing::error!(error = %e, "billing aggregator failed");
        }
    }
}

async fn aggregate_once(state: &AppState) -> Result<()> {
    let mut db = state.db.get().await.context("db pool")?;
    let txn = db.build_transaction().start().await?;

    // Aggregate unbilled Liquid usage per workspace.
    // FOR UPDATE SKIP LOCKED prevents double-billing when multiple API instances run.
    let liquid_rows = txn.query(
        "WITH locked AS ( \
             SELECT id, workspace_id, quantity \
             FROM usage_events \
             WHERE billed = false AND engine = 'liquid' \
             FOR UPDATE SKIP LOCKED \
         ) \
         SELECT l.workspace_id, \
                SUM(l.quantity) AS total_invocations, \
                array_agg(l.id) AS ids, \
                w.free_invocations_used \
         FROM locked l \
         LEFT JOIN workspaces w ON w.id = l.workspace_id \
         GROUP BY l.workspace_id, w.free_invocations_used",
        &[],
    ).await.context("aggregate liquid usage")?;

    for row in &liquid_rows {
        let wid:          uuid::Uuid     = row.get("workspace_id");
        let invocations:  i64            = row.get("total_invocations");
        let ids:          Vec<uuid::Uuid> = row.get("ids");
        let free_used:    i64            = row.get::<_, Option<i64>>("free_invocations_used").unwrap_or(0);

        // Calculate how many of these invocations are covered by the free allowance.
        let free_remaining = (FREE_LIQUID_INVOCATIONS - free_used).max(0);
        let free_consumed = invocations.min(free_remaining);
        let billable = invocations - free_consumed;

        // Update free_invocations_used on the workspace.
        if free_consumed > 0 {
            txn.execute(
                "UPDATE workspaces SET free_invocations_used = free_invocations_used + $1, \
                    updated_at = NOW() \
                 WHERE id = $2",
                &[&free_consumed, &wid],
            ).await.context("update free_invocations_used")?;
        }

        // Bill the excess at $0.30/1M = 300,000 µcr per 1M invocations.
        // Integer math: billable * LIQUID_PRICE_PER_MILLION / 1_000_000.
        if billable > 0 {
            let cost = billable * LIQUID_PRICE_PER_MILLION / 1_000_000;
            if cost > 0 {
                deduct_balance(&txn, &wid, cost, "usage_liquid", "Wasm invocations").await?;
            }
        }

        txn.execute(
            "UPDATE usage_events SET billed = true WHERE id = ANY($1)",
            &[&ids],
        ).await.context("mark liquid billed")?;
    }

    txn.commit().await.context("billing aggregator commit")?;

    // Check for zero-balance workspaces and publish SuspendEvents.
    check_suspensions(state).await?;

    Ok(())
}

// ── Metal billing cycle (fixed monthly) ──────────────────────────────────

/// Background task that runs hourly. Finds Metal services whose 30-day billing
/// cycle has elapsed and charges the fixed monthly price to the workspace balance.
pub async fn metal_billing_cycle(state: Arc<AppState>) {
    let secs: u64 = common::config::env_or("METAL_BILLING_INTERVAL_SECS", "3600").parse().unwrap_or(3600);
    tracing::info!(metal_billing_interval_secs = secs, "metal billing cycle configured");
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        if let Err(e) = metal_billing_once(&state).await {
            tracing::error!(error = %e, "metal billing cycle failed");
        }
    }
}

async fn metal_billing_once(state: &AppState) -> Result<()> {
    let mut db = state.db.get().await.context("db pool")?;
    let txn = db.build_transaction().start().await?;

    // Find Metal services whose 30-day cycle has elapsed.
    let rows = txn.query(
        "SELECT s.id AS service_id, s.workspace_id, s.monthly_price_cents, s.billing_cycle_start \
         FROM services s \
         WHERE s.engine = 'metal' \
           AND s.billing_cycle_start + INTERVAL '30 days' <= NOW() \
           AND s.status IN ('running', 'provisioning') \
           AND s.deleted_at IS NULL \
         FOR UPDATE OF s SKIP LOCKED",
        &[],
    ).await.context("find metal services due for billing")?;

    for row in &rows {
        let service_id:        uuid::Uuid = row.get("service_id");
        let workspace_id:      uuid::Uuid = row.get("workspace_id");
        let monthly_price_cents: i64      = row.get("monthly_price_cents");

        // Convert cents to micro-credits.
        let cost = monthly_price_cents * MICROCREDITS_PER_CENT;

        if cost > 0 {
            deduct_balance(&txn, &workspace_id, cost, "metal_monthly",
                &format!("Metal monthly (service {})", service_id)).await?;
        }

        // Advance billing_cycle_start by 30 days.
        txn.execute(
            "UPDATE services SET billing_cycle_start = billing_cycle_start + INTERVAL '30 days', \
                updated_at = NOW() \
             WHERE id = $1",
            &[&service_id],
        ).await.context("advance billing_cycle_start")?;

        tracing::info!(
            service_id = %service_id,
            workspace_id = %workspace_id,
            cost_microcredits = cost,
            "billing: metal monthly charge applied"
        );
    }

    txn.commit().await.context("metal billing cycle commit")?;

    // Check for zero-balance workspaces after Metal charges.
    check_suspensions(state).await?;

    Ok(())
}

// ── Free invocations reset ──────────────────────────────────────────────

/// Background task that runs hourly. Resets the free Liquid invocation counter
/// for workspaces whose monthly reset window has elapsed.
pub async fn free_invocations_reset(state: Arc<AppState>) {
    let secs: u64 = common::config::env_or("FREE_INV_RESET_INTERVAL_SECS", "3600").parse().unwrap_or(3600);
    tracing::info!(free_inv_reset_interval_secs = secs, "free invocations reset configured");
    let mut interval = tokio::time::interval(Duration::from_secs(secs));
    loop {
        interval.tick().await;
        if let Err(e) = free_invocations_reset_once(&state).await {
            tracing::error!(error = %e, "free invocations reset failed");
        }
    }
}

async fn free_invocations_reset_once(state: &AppState) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;

    let n = db.execute(
        "UPDATE workspaces \
         SET free_invocations_used = 0, \
             free_invocations_reset_at = free_invocations_reset_at + INTERVAL '1 month', \
             updated_at = NOW() \
         WHERE free_invocations_reset_at + INTERVAL '1 month' <= NOW() \
           AND deleted_at IS NULL",
        &[],
    ).await.context("reset free invocations")?;

    if n > 0 {
        tracing::info!(workspaces_reset = n, "billing: free invocations reset");
    }

    Ok(())
}

// ── Credit deduction ────────────────────────────────────────────────────

/// Atomically deduct from the workspace's single balance pool.
/// Balance can go negative — overage is tracked as debt rather than silently
/// absorbed. The billing tick + suspend drain period means a few seconds of
/// usage may accrue after balance hits zero.
async fn deduct_balance(
    txn: &deadpool_postgres::Transaction<'_>,
    workspace_id: &uuid::Uuid,
    cost: i64,
    kind: &str,
    description: &str,
) -> Result<()> {
    // Deduct and return new balance in one round-trip.
    let row = txn.query_one(
        "UPDATE workspaces SET balance = balance - $1, updated_at = NOW() \
         WHERE id = $2 \
         RETURNING balance",
        &[&cost, workspace_id],
    ).await.context("deduct workspace balance")?;

    let balance_after: i64 = row.get("balance");

    // Insert ledger entry.
    txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, $2, $3, $4, $5)",
        &[workspace_id, &(-cost), &kind, &description, &balance_after],
    ).await.context("insert credit ledger")?;

    Ok(())
}

/// Find workspaces with zero or negative balance and running services, publish SuspendEvent.
async fn check_suspensions(state: &AppState) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;

    let rows = db.query(
        "SELECT DISTINCT w.id \
         FROM workspaces w \
         JOIN services s ON s.workspace_id = w.id \
         WHERE w.balance <= 0 \
           AND s.status IN ('running', 'provisioning') \
           AND s.deleted_at IS NULL \
           AND w.deleted_at IS NULL",
        &[],
    ).await.context("check suspensions")?;

    for row in &rows {
        let wid: uuid::Uuid = row.get(0);
        let event = SuspendEvent {
            workspace_id: wid.to_string(),
            reason: "balance depleted".to_string(),
        };
        let payload = serde_json::to_vec(&event)?;
        state.nats_client.publish(SUBJECT_SUSPEND, payload.into()).await?;
        tracing::warn!(target: "audit", workspace_id = %wid, action = "suspend", "billing: workspace suspended — balance depleted");
    }

    Ok(())
}

// ── Stripe webhook handler ──────────────────────────────────────────────

#[utoipa::path(post, path = "/webhooks/stripe", request_body(content = String, description = "Raw Stripe webhook payload"), responses(
    (status = 200, description = "Webhook processed"),
    (status = 400, description = "Invalid payload or signature"),
), tag = "Webhooks")]
/// POST /webhooks/stripe — public route, verified by Stripe signature.
pub async fn stripe_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, ApiError> {
    let stripe = state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable("billing not configured")
    })?;

    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("missing_signature", "missing Stripe-Signature header"))?;

    let webhook_secret = state.stripe_webhook_secret.as_deref().ok_or_else(|| {
        ApiError::unavailable("webhook secret not configured")
    })?;

    let event = stripe.verify_webhook(&body, sig, webhook_secret)
        .map_err(|e| {
            tracing::warn!(error = %e, "Stripe webhook signature verification failed");
            ApiError::forbidden("invalid webhook signature")
        })?;

    tracing::info!(target: "audit", action = "stripe_webhook", event_type = event.event_type, event_id = event.id, "Stripe webhook received");

    match event.event_type.as_str() {
        "checkout.session.completed" => {
            handle_checkout_completed(&state, &event.data.object).await?;
        }
        other => {
            tracing::debug!(event_type = other, "Stripe webhook: unhandled event type");
        }
    }

    Ok(StatusCode::OK)
}

async fn handle_checkout_completed(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let mode = object.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if mode != "payment" {
        return Ok(());
    }

    let metadata_type = object
        .pointer("/metadata/type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if metadata_type != "topup" {
        return Ok(());
    }

    let session_id = object.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing session id"))?;
    let customer_id = object.get("customer").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing customer"))?;
    let amount_total = object.get("amount_total").and_then(|v| v.as_i64()).unwrap_or(0);
    let payment_intent = object.get("payment_intent").and_then(|v| v.as_str()).unwrap_or("");

    // Convert cents to micro-credits.
    let credits = amount_total * MICROCREDITS_PER_CENT;

    let mut db = db_conn(&state.db).await?;
    let txn = db.build_transaction().start().await
        .map_err(|e| ApiError::internal(format!("txn: {e}")))?;

    // Idempotent guard — attempt the ledger insert FIRST. stripe_session_id has
    // a UNIQUE index, so duplicate webhook deliveries hit the constraint before
    // we touch balance. On conflict the txn rolls back cleanly.
    //
    // We use balance_after = 0 as a placeholder here and patch it after the
    // UPDATE below, keeping the ledger accurate without a second round-trip.
    let result = txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, reference_id, balance_after, stripe_session_id) \
         SELECT id, $1, 'topup', 'credit top-up', $2, 0, $4 \
         FROM workspaces WHERE stripe_customer_id = $3 AND deleted_at IS NULL",
        &[&credits, &payment_intent, &customer_id, &session_id],
    ).await;

    match result {
        Ok(_) => {}
        Err(e) if e.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) => {
            // Duplicate webhook — nothing was mutated, txn rolls back on drop.
            tracing::info!(target: "audit", action = "topup_dedup", session_id, customer_id, "duplicate checkout webhook, skipping");
            return Ok(());
        }
        Err(e) => return Err(ApiError::internal(format!("ledger: {e}"))),
    }

    // Now safe to credit — we know this session_id is unique.
    txn.execute(
        "UPDATE workspaces SET balance = balance + $1, updated_at = NOW() \
         WHERE stripe_customer_id = $2 AND deleted_at IS NULL",
        &[&credits, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("topup: {e}")))?;

    // Patch the ledger row with the real balance_after.
    let row = txn.query_one(
        "SELECT balance FROM workspaces WHERE stripe_customer_id = $1",
        &[&customer_id],
    ).await.map_err(|e| ApiError::internal(format!("balance query: {e}")))?;
    let balance_after: i64 = row.get("balance");

    txn.execute(
        "UPDATE credit_ledger SET balance_after = $1 WHERE stripe_session_id = $2",
        &[&balance_after, &session_id],
    ).await.map_err(|e| ApiError::internal(format!("ledger patch: {e}")))?;

    txn.commit().await.map_err(|e| ApiError::internal(format!("commit: {e}")))?;

    tracing::info!(target: "audit", action = "topup", customer_id, session_id, credits, "top-up credits applied");

    Ok(())
}

// ── Billing API endpoints ───────────────────────────────────────────────────

#[utoipa::path(get, path = "/billing/balance", responses(
    (status = 200, description = "Current credit balance", body = contract::BalanceResponse),
), tag = "Billing", security(("api_key" = [])))]
/// GET /billing/balance — current balance and free invocation usage.
pub async fn get_balance(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<routes::Caller>,
) -> Result<Json<contract::BalanceResponse>, ApiError> {
    routes::require_scope(&caller, "read")?;
    let wid = extract_workspace_id(&state, caller.user_id).await?;
    let db = db_conn(&state.db).await?;

    let row = db.query_one(
        "SELECT balance, COALESCE(free_invocations_used, 0) AS free_invocations_used \
         FROM workspaces WHERE id = $1",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("balance query: {e}")))?;

    Ok(Json(contract::BalanceResponse {
        balance: row.get("balance"),
        free_invocations_used: row.get("free_invocations_used"),
        free_invocations_limit: FREE_LIQUID_INVOCATIONS,
    }))
}

#[utoipa::path(get, path = "/billing/usage", responses(
    (status = 200, description = "Current period usage breakdown", body = contract::UsageResponse),
), tag = "Billing", security(("api_key" = [])))]
/// GET /billing/usage — Metal monthly total + Liquid invocation usage.
pub async fn get_usage(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<routes::Caller>,
) -> Result<Json<contract::UsageResponse>, ApiError> {
    routes::require_scope(&caller, "read")?;
    let wid = extract_workspace_id(&state, caller.user_id).await?;
    let db = db_conn(&state.db).await?;

    // Metal: sum of monthly_price_cents for all running Metal services in this workspace.
    let metal_row = db.query_one(
        "SELECT COALESCE(SUM(monthly_price_cents), 0) AS total_cents \
         FROM services \
         WHERE workspace_id = $1 AND engine = 'metal' \
           AND status IN ('running', 'provisioning') \
           AND deleted_at IS NULL",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("metal usage: {e}")))?;

    let metal_monthly_total_cents: i64 = metal_row.get("total_cents");

    // Liquid: total invocations this billing period (since last free_invocations_reset_at).
    let liquid_row = db.query_one(
        "SELECT COALESCE(SUM(ue.quantity), 0) AS total_invocations \
         FROM usage_events ue \
         JOIN workspaces w ON w.id = ue.workspace_id \
         WHERE ue.workspace_id = $1 AND ue.engine = 'liquid' \
           AND ue.created_at >= COALESCE(w.free_invocations_reset_at, w.created_at)",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("liquid usage: {e}")))?;

    let liquid_invocations: i64 = liquid_row.get("total_invocations");

    // Cost of billable invocations (those beyond the free allowance).
    let free_used: i64 = db.query_one(
        "SELECT COALESCE(free_invocations_used, 0) AS free_used FROM workspaces WHERE id = $1",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("free used: {e}")))?.get("free_used");

    let billable = (liquid_invocations - free_used.min(liquid_invocations)).max(0);
    let liquid_cost = billable * LIQUID_PRICE_PER_MILLION / 1_000_000;

    Ok(Json(contract::UsageResponse {
        metal_monthly_total_cents,
        liquid: contract::LiquidUsage {
            invocations: liquid_invocations,
            cost_microcredits: liquid_cost,
        },
    }))
}

#[utoipa::path(get, path = "/billing/ledger", responses(
    (status = 200, description = "Credit transaction history", body = contract::LedgerResponse),
), tag = "Billing", security(("api_key" = [])))]
/// GET /billing/ledger — credit transaction history.
pub async fn get_ledger(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<routes::Caller>,
) -> Result<Json<contract::LedgerResponse>, ApiError> {
    routes::require_scope(&caller, "read")?;
    let wid = extract_workspace_id(&state, caller.user_id).await?;
    let db = db_conn(&state.db).await?;

    let rows = db.query(
        "SELECT id, amount, kind, description, reference_id, balance_after, \
                to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at \
         FROM credit_ledger \
         WHERE workspace_id = $1 \
         ORDER BY created_at DESC \
         LIMIT 100",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("ledger query: {e}")))?;

    let entries = rows.iter().map(|r| contract::LedgerEntry {
        id: r.get::<_, uuid::Uuid>("id").to_string(),
        amount: r.get("amount"),
        kind: r.get("kind"),
        description: r.get("description"),
        reference_id: r.get("reference_id"),
        balance_after: r.get("balance_after"),
        created_at: r.get("created_at"),
    }).collect();

    Ok(Json(contract::LedgerResponse { entries }))
}

#[utoipa::path(post, path = "/billing/topup", request_body = contract::TopupRequest, responses(
    (status = 200, description = "Stripe checkout session created", body = contract::CheckoutResponse),
    (status = 400, description = "Invalid amount"),
), tag = "Billing", security(("api_key" = [])))]
/// POST /billing/topup — initiate a Stripe checkout for credit top-up.
pub async fn create_topup(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<routes::Caller>,
    Json(body): Json<contract::TopupRequest>,
) -> Result<Json<contract::CheckoutResponse>, ApiError> {
    routes::require_scope(&caller, "admin")?;
    let stripe = state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable("billing not configured")
    })?;

    let wid = extract_workspace_id(&state, caller.user_id).await?;
    let db = db_conn(&state.db).await?;

    let customer_id = get_or_create_stripe_customer(&state, &db, &wid).await?;

    if body.amount_cents < 100 {
        return Err(ApiError::bad_request("invalid_amount", "minimum top-up is $1.00"));
    }

    let session = stripe.create_topup_session(
        &customer_id,
        body.amount_cents,
        &body.success_url,
        &body.cancel_url,
    ).await.map_err(|e| ApiError::bad_gateway(format!("Stripe: {e}")))?;

    tracing::info!(
        target: "audit",
        action = "create_topup",
        workspace_id = %wid,
        amount_cents = body.amount_cents,
        session_id = session.id,
        result = "ok",
    );

    Ok(Json(contract::CheckoutResponse {
        checkout_url: session.url.unwrap_or_default(),
        session_id: session.id,
    }))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async fn get_or_create_stripe_customer(
    state: &AppState,
    db: &deadpool_postgres::Object,
    workspace_id: &uuid::Uuid,
) -> Result<String, ApiError> {
    let row = db.query_one(
        "SELECT w.stripe_customer_id, w.name, u.email \
         FROM workspaces w \
         JOIN workspace_members wm ON wm.workspace_id = w.id AND wm.role = 'owner' \
         JOIN users u ON u.id = wm.user_id \
         WHERE w.id = $1",
        &[workspace_id],
    ).await.map_err(|e| ApiError::internal(format!("workspace query: {e}")))?;

    let existing: Option<String> = row.get("stripe_customer_id");
    if let Some(cid) = existing {
        return Ok(cid);
    }

    let stripe = state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable("billing not configured")
    })?;

    let name: String  = row.get("name");
    let email: String = row.get("email");

    let customer = stripe.create_customer(&name, &email, &workspace_id.to_string())
        .await
        .map_err(|e| ApiError::bad_gateway(format!("Stripe create customer: {e}")))?;

    db.execute(
        "UPDATE workspaces SET stripe_customer_id = $1, updated_at = NOW() WHERE id = $2",
        &[&customer.id, workspace_id],
    ).await.map_err(|e| ApiError::internal(format!("update stripe_customer_id: {e}")))?;

    Ok(customer.id)
}

/// Look up the primary (owner) workspace for a given user.
/// Deterministic: picks the oldest owned workspace, matching the pattern
/// used by `get_or_create_stripe_customer`.
async fn extract_workspace_id(state: &AppState, user_id: uuid::Uuid) -> Result<uuid::Uuid, ApiError> {
    let db = db_conn(&state.db).await?;

    let row = db.query_opt(
        "SELECT wm.workspace_id FROM workspace_members wm \
         WHERE wm.user_id = $1 AND wm.role = 'owner' \
         ORDER BY wm.created_at ASC \
         LIMIT 1",
        &[&user_id],
    ).await.map_err(|e| ApiError::internal(format!("workspace lookup: {e}")))?;

    row.map(|r| r.get("workspace_id"))
        .ok_or_else(|| ApiError::not_found("workspace not found for this user"))
}
