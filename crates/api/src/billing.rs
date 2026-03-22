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
    SUBJECT_PROVISION, SUBJECT_SUSPEND, SUBJECT_USAGE_LIQUID, SUBJECT_USAGE_METAL,
};
use common::pricing::{FREE_LIQUID_INVOCATIONS, LIQUID_PRICE_PER_MILLION};

use common::contract;
use crate::{envelope, routes};

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
                let balance_after = deduct_balance(&txn, &wid, cost, "usage_liquid", "Wasm invocations").await?;
                if balance_after <= 0 {
                    insert_suspend_outbox(&txn, &wid).await?;
                }
            }
        }

        txn.execute(
            "UPDATE usage_events SET billed = true WHERE id = ANY($1)",
            &[&ids],
        ).await.context("mark liquid billed")?;
    }

    txn.commit().await.context("billing aggregator commit")?;

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
            let balance_after = deduct_balance(&txn, &workspace_id, cost, "metal_monthly",
                &format!("Metal monthly (service {})", service_id)).await?;
            if balance_after <= 0 {
                insert_suspend_outbox(&txn, &workspace_id).await?;
            }
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
) -> Result<i64> {
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

    Ok(balance_after)
}

/// Insert a SuspendEvent into the outbox table within the same transaction as
/// the billing charge. The outbox poller publishes to NATS, ensuring the event
/// is not lost even if NATS is temporarily down.
async fn insert_suspend_outbox(
    txn: &deadpool_postgres::Transaction<'_>,
    workspace_id: &uuid::Uuid,
) -> Result<()> {
    // Only enqueue if the workspace still has active services to suspend.
    let has_active: bool = txn.query_one(
        "SELECT EXISTS(SELECT 1 FROM services WHERE workspace_id = $1 \
         AND status IN ('running', 'provisioning') AND deleted_at IS NULL) AS active",
        &[workspace_id],
    ).await.context("check active services")?.get("active");

    if !has_active {
        return Ok(());
    }

    let event = SuspendEvent {
        workspace_id: workspace_id.to_string(),
        reason: "balance depleted".to_string(),
    };
    let payload = serde_json::to_value(&event).context("serialize SuspendEvent")?;
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
        &[&SUBJECT_SUSPEND, &payload],
    ).await.context("insert suspend outbox")?;

    tracing::warn!(target: "audit", workspace_id = %workspace_id, action = "suspend_queued", "billing: workspace queued for suspension — balance depleted");
    Ok(())
}

/// Re-provision all suspended services for a workspace by inserting
/// ProvisionEvents into the outbox. Uses the existing daemon provision
/// pipeline — no special daemon-side unsuspend handler needed.
async fn enqueue_unsuspend(
    state: &AppState,
    txn: &deadpool_postgres::Transaction<'_>,
    workspace_id: uuid::Uuid,
) -> Result<(), ApiError> {
    // Query suspended services + latest deployment artifact for each.
    let rows = txn.query(
        "SELECT s.id, s.slug, s.name, s.engine, s.port, s.vcpu, s.memory_mb, \
                s.workspace_id, s.project_id, \
                d.artifact_key \
         FROM services s \
         LEFT JOIN LATERAL ( \
             SELECT artifact_key \
             FROM deployments \
             WHERE service_id = s.id \
             ORDER BY created_at DESC \
             LIMIT 1 \
         ) d ON true \
         WHERE s.workspace_id = $1 AND s.status = 'suspended' AND s.deleted_at IS NULL",
        &[&workspace_id],
    ).await.map_err(|e| ApiError::internal(format!("unsuspend query: {e}")))?;

    if rows.is_empty() {
        return Ok(());
    }

    for row in &rows {
        let service_id: uuid::Uuid = row.get("id");
        let slug: String = row.get("slug");
        let name: String = row.get("name");
        let engine_str: String = row.get("engine");
        let port: i32 = row.get("port");
        let vcpu: i32 = row.get("vcpu");
        let memory_mb: i32 = row.get("memory_mb");
        let wid: uuid::Uuid = row.get("workspace_id");
        let artifact_key: Option<String> = row.get("artifact_key");

        let Some(artifact_key) = artifact_key else {
            tracing::warn!(%service_id, "unsuspend: no deployment found — skipping");
            continue;
        };

        let engine = match engine_str.as_str() {
            "metal" => common::Engine::Metal,
            "liquid" => common::Engine::Liquid,
            other => {
                tracing::warn!(engine = other, %service_id, "unsuspend: unknown engine — skipping");
                continue;
            }
        };

        let engine_spec = match engine {
            common::Engine::Metal => common::EngineSpec::Metal(common::MetalSpec {
                vcpu: vcpu as u32,
                memory_mb: memory_mb as u32,
                port: port as u16,
                artifact_key: artifact_key.clone(),
                artifact_sha256: None,
                quota: state.default_quota.clone(),
            }),
            common::Engine::Liquid => common::EngineSpec::Liquid(common::LiquidSpec {
                artifact_key: artifact_key.clone(),
                artifact_sha256: None,
            }),
        };

        // Merge env vars: project → service (same as deploy flow).
        let pid: uuid::Uuid = row.get("project_id");
        let mut env_vars = envelope::read_project_env_vars(&state.vault, wid, pid)
            .await
            .unwrap_or_default();
        let service_env = envelope::read_env_vars(&state.vault, wid, service_id)
            .await
            .unwrap_or_default();
        env_vars.extend(service_env);

        let event = common::ProvisionEvent {
            tenant_id: wid.to_string(),
            service_id: service_id.to_string(),
            app_name: name,
            slug,
            engine,
            spec: engine_spec,
            env_vars,
        };

        let payload = serde_json::to_value(&event)
            .map_err(|e| ApiError::internal(format!("serialize provision: {e}")))?;

        // Mark as provisioning + insert outbox event in the same transaction.
        txn.execute(
            "UPDATE services SET status = 'provisioning', updated_at = NOW() WHERE id = $1",
            &[&service_id],
        ).await.map_err(|e| ApiError::internal(format!("mark provisioning: {e}")))?;

        txn.execute(
            "INSERT INTO outbox (subject, payload) VALUES ($1, $2)",
            &[&SUBJECT_PROVISION, &payload],
        ).await.map_err(|e| ApiError::internal(format!("unsuspend outbox: {e}")))?;

        tracing::info!(target: "audit", action = "unsuspend_provision", %service_id, %workspace_id, "service queued for re-provision after top-up");
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
        "checkout.session.expired" => {
            let session_id = event.data.object.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
            tracing::info!(target: "audit", action = "checkout_expired", session_id, "Stripe checkout session expired");
        }
        "charge.refunded" => {
            handle_charge_refunded(&state, &event.data.object).await?;
        }
        "payment_intent.payment_failed" => {
            handle_payment_failed(&state, &event.data.object).await?;
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
    // RETURNING balance + id avoids a separate SELECT round-trip.
    let row = txn.query_one(
        "UPDATE workspaces SET balance = balance + $1, updated_at = NOW() \
         WHERE stripe_customer_id = $2 AND deleted_at IS NULL \
         RETURNING id, balance",
        &[&credits, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("topup: {e}")))?;
    let workspace_id: uuid::Uuid = row.get("id");
    let balance_after: i64 = row.get("balance");

    // Patch the ledger row with the real balance_after.
    txn.execute(
        "UPDATE credit_ledger SET balance_after = $1 WHERE stripe_session_id = $2",
        &[&balance_after, &session_id],
    ).await.map_err(|e| ApiError::internal(format!("ledger patch: {e}")))?;

    // If workspace was suspended and now has positive balance, re-provision
    // all suspended services by inserting ProvisionEvents into the outbox.
    // The normal daemon provision pipeline handles the rest.
    if balance_after > 0 {
        enqueue_unsuspend(state, &txn, workspace_id).await?;
    }

    txn.commit().await.map_err(|e| ApiError::internal(format!("commit: {e}")))?;

    tracing::info!(target: "audit", action = "topup", customer_id, session_id, credits, %workspace_id, "top-up credits applied");

    Ok(())
}

async fn handle_charge_refunded(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let amount_refunded = object.get("amount_refunded").and_then(|v| v.as_i64()).unwrap_or(0);
    if amount_refunded == 0 {
        return Ok(());
    }

    let customer_id = object.get("customer").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing customer in charge.refunded"))?;
    let charge_id = object.get("id").and_then(|v| v.as_str()).unwrap_or("");

    let credits = amount_refunded * MICROCREDITS_PER_CENT;

    let mut db = db_conn(&state.db).await?;
    let txn = db.build_transaction().start().await
        .map_err(|e| ApiError::internal(format!("txn: {e}")))?;

    // Reverse the refund amount from workspace balance.
    let row = txn.query_one(
        "UPDATE workspaces SET balance = balance - $1, updated_at = NOW() \
         WHERE stripe_customer_id = $2 AND deleted_at IS NULL \
         RETURNING id, balance",
        &[&credits, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("refund deduct: {e}")))?;

    let workspace_id: uuid::Uuid = row.get("id");
    let balance_after: i64 = row.get("balance");

    txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, reference_id, balance_after) \
         VALUES ($1, $2, 'refund', 'Stripe refund', $3, $4)",
        &[&workspace_id, &(-credits), &charge_id, &balance_after],
    ).await.map_err(|e| ApiError::internal(format!("refund ledger: {e}")))?;

    if balance_after <= 0 {
        insert_suspend_outbox(&txn, &workspace_id).await
            .map_err(|e| ApiError::internal(format!("suspend outbox: {e}")))?;
    }

    txn.commit().await.map_err(|e| ApiError::internal(format!("commit: {e}")))?;

    tracing::info!(target: "audit", action = "refund", customer_id, charge_id, credits, %workspace_id, "Stripe refund applied");
    Ok(())
}

async fn handle_payment_failed(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let pi_id = object.get("id").and_then(|v| v.as_str()).unwrap_or("");

    let db = db_conn(&state.db).await?;

    // Check if checkout.session.completed already credited this payment_intent.
    let row = db.query_opt(
        "SELECT id, workspace_id, amount FROM credit_ledger \
         WHERE reference_id = $1 AND kind = 'topup'",
        &[&pi_id],
    ).await.map_err(|e| ApiError::internal(format!("ledger lookup: {e}")))?;

    let Some(row) = row else {
        // No matching credit — checkout.session.completed didn't fire or used a
        // different payment method that succeeded. Nothing to reverse.
        tracing::info!(target: "audit", action = "payment_failed_no_match", payment_intent = pi_id, "payment failed — no matching credit to reverse");
        return Ok(());
    };

    let workspace_id: uuid::Uuid = row.get("workspace_id");
    let original_amount: i64 = row.get("amount"); // positive (was a credit)

    // Reverse the credited amount.
    let mut db = db_conn(&state.db).await?;
    let txn = db.build_transaction().start().await
        .map_err(|e| ApiError::internal(format!("txn: {e}")))?;

    let row = txn.query_one(
        "UPDATE workspaces SET balance = balance - $1, updated_at = NOW() \
         WHERE id = $2 \
         RETURNING balance",
        &[&original_amount, &workspace_id],
    ).await.map_err(|e| ApiError::internal(format!("reverse credit: {e}")))?;
    let balance_after: i64 = row.get("balance");

    txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, reference_id, balance_after) \
         VALUES ($1, $2, 'refund', 'payment failed — credit reversed', $3, $4)",
        &[&workspace_id, &(-original_amount), &pi_id, &balance_after],
    ).await.map_err(|e| ApiError::internal(format!("reversal ledger: {e}")))?;

    if balance_after <= 0 {
        insert_suspend_outbox(&txn, &workspace_id).await
            .map_err(|e| ApiError::internal(format!("suspend outbox: {e}")))?;
    }

    txn.commit().await.map_err(|e| ApiError::internal(format!("commit: {e}")))?;

    tracing::warn!(target: "audit", action = "payment_failed_reversed", payment_intent = pi_id, %workspace_id, reversed = original_amount, "payment failed — credit reversed");
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

    // Single query: metal sum (correlated subquery), liquid sum, free_used.
    let row = db.query_one(
        "SELECT \
             COALESCE(w.free_invocations_used, 0) AS free_used, \
             COALESCE((SELECT SUM(monthly_price_cents) FROM services \
                       WHERE workspace_id = $1 AND engine = 'metal' \
                       AND status IN ('running', 'provisioning') \
                       AND deleted_at IS NULL), 0) AS metal_total, \
             COALESCE((SELECT SUM(ue.quantity) FROM usage_events ue \
                       WHERE ue.workspace_id = $1 AND ue.engine = 'liquid' \
                       AND ue.created_at >= COALESCE(w.free_invocations_reset_at, w.created_at)), 0)::bigint AS liquid_invocations \
         FROM workspaces w \
         WHERE w.id = $1",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("usage query: {e}")))?;

    let metal_monthly_total_cents: i64 = row.get("metal_total");
    let liquid_invocations: i64 = row.get("liquid_invocations");
    let free_used: i64 = row.get("free_used");

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
        user_id = %caller.user_id,
        ip = ?caller.ip,
        workspace_id = %wid,
        amount_cents = body.amount_cents,
        session_id = session.id,
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
