//! Billing: usage aggregation, credit deduction, Stripe webhooks, and billing endpoints.
//!
//! ## Data flow
//!
//! Daemon (60s tick) ──MetalUsageEvent──► NATS ──► `usage_subscriber` ──► usage_events table
//!                                                       │
//! Daemon (60s tick) ──LiquidUsageEvent─► NATS ──►       ├──► credit_ledger (debit)
//!                                                       ├──► workspaces.balance_credits -= cost
//!                                                       │
//!                                                       ▼ (balance <= 0)
//!                                                 SuspendEvent ──► NATS ──► Daemon
//!
//! The `billing_aggregator` runs every 60s, sums unbilled usage_events per workspace,
//! converts to micro-credits, deducts from balance, and publishes SuspendEvent on zero.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use futures::StreamExt;

use common::events::{
    LiquidUsageEvent, MetalUsageEvent, SuspendEvent,
    SUBJECT_SUSPEND, SUBJECT_USAGE_LIQUID, SUBJECT_USAGE_METAL,
};

use crate::AppState;
use crate::routes::{ApiError, db_conn};

// ── Cost constants (micro-credits) ──────────────────────────────────────────
// $0.10/vCPU-hr = 100,000 micro-credits/vCPU-hr = 1,667 micro-credits/vCPU-min
const METAL_VCPU_PER_MIN: i64 = 1_667;
// $0.01/GB-hr = 10,000 micro-credits/GB-hr. Per MB-min: 10,000 / 1024 / 60 ≈ 0.163
// We store as fixed-point: (memory_mb * 163) / 1000 per minute
const METAL_MB_PER_MIN_NUM: i64 = 163;
const METAL_MB_PER_MIN_DEN: i64 = 1000;
// $0.000001/invocation = 1 micro-credit
const LIQUID_PER_INVOCATION: i64 = 1;
// 1 cent = 10,000 micro-credits ($1 = 100 cents = 1,000,000 micro-credits)
const MICROCREDITS_PER_CENT: i64 = 10_000;

/// Compute the cost in micro-credits for Metal compute usage.
fn metal_cost(vcpu_mins: i64, mb_mins: i64) -> i64 {
    vcpu_mins * METAL_VCPU_PER_MIN
        + mb_mins * METAL_MB_PER_MIN_NUM / METAL_MB_PER_MIN_DEN
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
    let mut metal_sub  = state.nats_client.subscribe(SUBJECT_USAGE_METAL).await?;
    let mut liquid_sub = state.nats_client.subscribe(SUBJECT_USAGE_LIQUID).await?;

    tracing::info!("billing: usage subscriber connected");

    loop {
        tokio::select! {
            Some(msg) = metal_sub.next() => {
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
        "INSERT INTO usage_events (workspace_id, service_id, engine, quantity, vcpu, memory_mb) \
         VALUES ($1, $2, 'metal', $3, $4, $5)",
        &[&wid, &sid, &(ev.duration_secs as i64), &(ev.vcpu as i32), &(ev.memory_mb as i32)],
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

// ── Billing aggregator ──────────────────────────────────────────────────────

/// Runs every 60s: aggregates unbilled usage_events into credit deductions.
pub async fn billing_aggregator(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
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

    // Aggregate unbilled Metal usage per workspace.
    let metal_rows = txn.query(
        "SELECT workspace_id, \
                SUM(((quantity + 59) / 60) * vcpu)    AS vcpu_minutes, \
                SUM(((quantity + 59) / 60) * memory_mb) AS mb_minutes, \
                array_agg(id) AS ids \
         FROM usage_events \
         WHERE billed = false AND engine = 'metal' \
         GROUP BY workspace_id",
        &[],
    ).await.context("aggregate metal usage")?;

    for row in &metal_rows {
        let wid:         uuid::Uuid    = row.get("workspace_id");
        let vcpu_mins:   i64           = row.get("vcpu_minutes");
        let mb_mins:     i64           = row.get("mb_minutes");
        let ids:         Vec<uuid::Uuid> = row.get("ids");

        let cost = metal_cost(vcpu_mins, mb_mins);

        if cost > 0 {
            deduct_credits(&txn, &wid, cost, "usage_metal", "Metal compute").await?;
        }

        // Mark as billed.
        txn.execute(
            "UPDATE usage_events SET billed = true WHERE id = ANY($1)",
            &[&ids],
        ).await.context("mark metal billed")?;
    }

    // Aggregate unbilled Liquid usage per workspace (join tier to avoid N+1).
    let liquid_rows = txn.query(
        "SELECT ue.workspace_id, \
                SUM(ue.quantity) AS total_invocations, \
                array_agg(ue.id) AS ids, \
                COALESCE(w.tier, 'hobby') AS tier \
         FROM usage_events ue \
         LEFT JOIN workspaces w ON w.id = ue.workspace_id \
         WHERE ue.billed = false AND ue.engine = 'liquid' \
         GROUP BY ue.workspace_id, w.tier",
        &[],
    ).await.context("aggregate liquid usage")?;

    for row in &liquid_rows {
        let wid:          uuid::Uuid    = row.get("workspace_id");
        let invocations:  i64           = row.get("total_invocations");
        let ids:          Vec<uuid::Uuid> = row.get("ids");
        let tier:         String         = row.get("tier");

        let cost = if tier == "hobby" {
            0 // Hobby: free invocations, hard-capped at deploy time
        } else {
            invocations * LIQUID_PER_INVOCATION
        };

        if cost > 0 {
            deduct_credits(&txn, &wid, cost, "usage_liquid", "Wasm invocations").await?;
        }

        txn.execute(
            "UPDATE usage_events SET billed = true WHERE id = ANY($1)",
            &[&ids],
        ).await.context("mark liquid billed")?;
    }

    txn.commit().await.context("billing aggregator commit")?;

    // Defensive reconciliation: verify that the sum of ledger debits matches
    // the sum of billed usage costs for each workspace. This catches drift if
    // the aggregator is ever refactored to use multiple transactions.
    verify_billing_consistency(state).await;

    // Check for zero-balance workspaces and publish SuspendEvents.
    check_suspensions(state).await?;

    Ok(())
}

/// Compare total ledger debits against total billed usage costs per workspace.
/// Logs a warning on mismatch — does not fail the aggregation cycle.
async fn verify_billing_consistency(state: &AppState) {
    let db = match state.db.get().await {
        Ok(d) => d,
        Err(_) => return,
    };

    // For each workspace, compare:
    //   ledger debits  = SUM(-amount) WHERE kind IN ('usage_metal', 'usage_liquid')
    //   billed costs   = computed from billed usage_events
    let result = db.query(
        "WITH ledger_debits AS ( \
             SELECT workspace_id, COALESCE(SUM(-amount), 0) AS total_debited \
             FROM credit_ledger \
             WHERE kind IN ('usage_metal', 'usage_liquid') \
             GROUP BY workspace_id \
         ), \
         billed_costs AS ( \
             SELECT workspace_id, \
                    COALESCE(SUM(CASE WHEN engine = 'metal' THEN \
                        ((quantity + 59) / 60) * vcpu * $1 + \
                        ((quantity + 59) / 60) * memory_mb * $2 / $3 \
                    ELSE 0 END), 0) + \
                    COALESCE(SUM(CASE WHEN engine = 'liquid' THEN quantity * $4 ELSE 0 END), 0) \
                    AS total_cost \
             FROM usage_events \
             WHERE billed = true \
             GROUP BY workspace_id \
         ) \
         SELECT ld.workspace_id, ld.total_debited, bc.total_cost \
         FROM ledger_debits ld \
         JOIN billed_costs bc ON bc.workspace_id = ld.workspace_id \
         WHERE ld.total_debited != bc.total_cost",
        &[&METAL_VCPU_PER_MIN, &METAL_MB_PER_MIN_NUM, &METAL_MB_PER_MIN_DEN, &LIQUID_PER_INVOCATION],
    ).await;

    match result {
        Ok(rows) if !rows.is_empty() => {
            for row in &rows {
                let wid: uuid::Uuid = row.get("workspace_id");
                let debited: i64 = row.get("total_debited");
                let cost: i64 = row.get("total_cost");
                tracing::warn!(
                    workspace_id = %wid,
                    ledger_debited = debited,
                    usage_cost = cost,
                    drift = debited - cost,
                    "billing reconciliation mismatch — ledger debits != billed usage costs"
                );
            }
        }
        Ok(_) => {} // no mismatches
        Err(e) => tracing::warn!(error = %e, "billing reconciliation check failed"),
    }
}

/// Atomically deduct credits from a workspace. Subscription credits first, then top-up.
async fn deduct_credits(
    txn: &deadpool_postgres::Transaction<'_>,
    workspace_id: &uuid::Uuid,
    cost: i64,
    kind: &str,
    description: &str,
) -> Result<()> {
    // Read current balances.
    let row = txn.query_one(
        "SELECT balance_credits, topup_credits FROM workspaces WHERE id = $1 FOR UPDATE",
        &[workspace_id],
    ).await.context("read workspace balance")?;

    let mut sub_balance: i64   = row.get("balance_credits");
    let mut topup_balance: i64 = row.get("topup_credits");

    // Deduct from subscription credits first, then top-up.
    let mut remaining = cost;
    let sub_deduction = remaining.min(sub_balance);
    sub_balance -= sub_deduction;
    remaining -= sub_deduction;

    let topup_deduction = remaining.min(topup_balance);
    topup_balance -= topup_deduction;

    // Update balances.
    txn.execute(
        "UPDATE workspaces SET balance_credits = $1, topup_credits = $2, updated_at = NOW() WHERE id = $3",
        &[&sub_balance, &topup_balance, workspace_id],
    ).await.context("update workspace balance")?;

    let balance_after = sub_balance + topup_balance;

    // Insert ledger entry.
    txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, $2, $3, $4, $5)",
        &[workspace_id, &(-cost), &kind, &description, &balance_after],
    ).await.context("insert credit ledger")?;

    Ok(())
}

/// Find workspaces with zero or negative balance and running services → publish SuspendEvent.
async fn check_suspensions(state: &AppState) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;

    let rows = db.query(
        "SELECT DISTINCT w.id \
         FROM workspaces w \
         JOIN services s ON s.workspace_id = w.id \
         WHERE w.balance_credits + w.topup_credits <= 0 \
           AND w.tier != 'hobby' \
           AND s.status = 'running' \
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

// ── Monthly credit reset ────────────────────────────────────────────────────

/// Runs daily. Checks for workspaces whose billing period has ended,
/// expires remaining subscription credits, and applies new monthly credits.
pub async fn monthly_credit_reset(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(3600)); // hourly check
    loop {
        interval.tick().await;
        if let Err(e) = reset_once(&state).await {
            tracing::error!(error = %e, "monthly credit reset failed");
        }
    }
}

async fn reset_once(state: &AppState) -> Result<()> {
    let mut db = state.db.get().await.context("db pool")?;
    let txn = db.build_transaction().start().await?;

    // Find workspaces whose billing period has ended.
    let rows = txn.query(
        "SELECT w.id, w.balance_credits, w.tier, p.credit_cents \
         FROM workspaces w \
         JOIN plans p ON p.id = w.tier \
         WHERE w.billing_period_end IS NOT NULL \
           AND w.billing_period_end <= CURRENT_DATE \
           AND w.deleted_at IS NULL \
           AND w.tier != 'hobby' \
         FOR UPDATE OF w",
        &[],
    ).await.context("find expired billing periods")?;

    for row in &rows {
        let wid:            uuid::Uuid = row.get("id");
        let old_balance:    i64        = row.get("balance_credits");
        let credit_cents:   i32        = row.get("credit_cents");

        // Convert cents to micro-credits: $1 = 100 cents = 1,000,000 micro-credits
        // So 1 cent = 10,000 micro-credits.
        let new_credits: i64 = credit_cents as i64 * MICROCREDITS_PER_CENT;

        // Expire remaining subscription credits (not top-up — those roll over).
        if old_balance > 0 {
            txn.execute(
                "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
                 VALUES ($1, $2, 'expiry', 'monthly subscription credit expiry', 0)",
                &[&wid, &(-old_balance)],
            ).await?;
        }

        // Apply new monthly credits and return topup_credits in one round-trip.
        let updated = txn.query_one(
            "UPDATE workspaces SET balance_credits = $1, \
                    billing_period_start = billing_period_end, \
                    billing_period_end = billing_period_end + INTERVAL '1 month', \
                    updated_at = NOW() \
             WHERE id = $2 \
             RETURNING topup_credits",
            &[&new_credits, &wid],
        ).await?;

        let topup: i64 = updated.get("topup_credits");

        txn.execute(
            "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
             VALUES ($1, $2, 'subscription_credit', 'monthly credit reset', $3)",
            &[&wid, &new_credits, &(new_credits + topup)],
        ).await?;

        tracing::info!(workspace_id = %wid, credits = new_credits, "billing: monthly credit reset applied");
    }

    txn.commit().await?;
    Ok(())
}

// ── Stripe webhook handler ──────────────────────────────────────────────────

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
        "customer.subscription.created" | "customer.subscription.updated" => {
            handle_subscription_change(&state, &event.data.object).await?;
        }
        "customer.subscription.deleted" => {
            handle_subscription_deleted(&state, &event.data.object).await?;
        }
        "checkout.session.completed" => {
            handle_checkout_completed(&state, &event.data.object).await?;
        }
        other => {
            tracing::debug!(event_type = other, "Stripe webhook: unhandled event type");
        }
    }

    Ok(StatusCode::OK)
}

async fn handle_subscription_change(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let customer_id = object.get("customer").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing customer"))?;
    let subscription_id = object.get("id").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing subscription id"))?;
    let status = object.get("status").and_then(|v| v.as_str()).unwrap_or("");

    if status != "active" {
        return Ok(());
    }

    // Determine tier from price metadata or price ID.
    let tier = resolve_tier_from_subscription(state, object).unwrap_or_else(|_| "pro".to_string());

    let period_start = object.get("current_period_start")
        .and_then(|v| v.as_i64());
    let period_end = object.get("current_period_end")
        .and_then(|v| v.as_i64());

    let db = db_conn(&state.db).await?;

    // Update workspace tier and billing period.
    let n = db.execute(
        "UPDATE workspaces SET \
            tier = $1, \
            stripe_subscription_id = $2, \
            billing_period_start = CASE WHEN $3::BIGINT IS NOT NULL THEN to_timestamp($3)::DATE ELSE billing_period_start END, \
            billing_period_end = CASE WHEN $4::BIGINT IS NOT NULL THEN to_timestamp($4)::DATE ELSE billing_period_end END, \
            updated_at = NOW() \
         WHERE stripe_customer_id = $5 AND deleted_at IS NULL",
        &[&tier, &subscription_id, &period_start, &period_end, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("update workspace: {e}")))?;

    if n == 0 {
        tracing::warn!(customer_id, "Stripe webhook: no workspace found for customer");
    } else {
        tracing::info!(target: "audit", action = "subscription_updated", customer_id, tier, "workspace tier updated via Stripe");

        // Apply initial credits if this is a new subscription.
        apply_initial_credits(state, customer_id, &tier).await?;
    }

    Ok(())
}

async fn handle_subscription_deleted(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let customer_id = object.get("customer").and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("invalid_payload", "missing customer"))?;

    let db = db_conn(&state.db).await?;

    db.execute(
        "UPDATE workspaces SET tier = 'hobby', balance_credits = 0, \
            stripe_subscription_id = NULL, updated_at = NOW() \
         WHERE stripe_customer_id = $1 AND deleted_at IS NULL",
        &[&customer_id],
    ).await.map_err(|e| ApiError::internal(format!("downgrade workspace: {e}")))?;

    tracing::info!(target: "audit", action = "subscription_deleted", customer_id, "workspace downgraded to hobby");

    Ok(())
}

async fn handle_checkout_completed(state: &AppState, object: &serde_json::Value) -> Result<(), ApiError> {
    let mode = object.get("mode").and_then(|v| v.as_str()).unwrap_or("");
    if mode != "payment" {
        return Ok(()); // Subscription checkouts handled by subscription.created
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

    // Add to topup_credits (these roll over).
    txn.execute(
        "UPDATE workspaces SET topup_credits = topup_credits + $1, updated_at = NOW() \
         WHERE stripe_customer_id = $2 AND deleted_at IS NULL",
        &[&credits, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("topup: {e}")))?;

    // Get new total for ledger entry.
    let row = txn.query_one(
        "SELECT balance_credits + topup_credits AS total FROM workspaces WHERE stripe_customer_id = $1",
        &[&customer_id],
    ).await.map_err(|e| ApiError::internal(format!("balance query: {e}")))?;
    let balance_after: i64 = row.get("total");

    // Idempotent insert — stripe_session_id has a UNIQUE index. Duplicate webhook
    // deliveries hit the constraint and we return OK (already processed).
    let result = txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, reference_id, balance_after, stripe_session_id) \
         SELECT id, $1, 'topup', 'credit top-up', $2, $3, $5 \
         FROM workspaces WHERE stripe_customer_id = $4 AND deleted_at IS NULL",
        &[&credits, &payment_intent, &balance_after, &customer_id, &session_id],
    ).await;

    match result {
        Ok(_) => {}
        Err(e) if e.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) => {
            // Duplicate webhook — roll back the topup_credits update and return OK.
            tracing::info!(target: "audit", action = "topup_dedup", session_id, customer_id, "duplicate checkout webhook, skipping");
            // txn will be rolled back on drop.
            return Ok(());
        }
        Err(e) => return Err(ApiError::internal(format!("ledger: {e}"))),
    }

    txn.commit().await.map_err(|e| ApiError::internal(format!("commit: {e}")))?;

    tracing::info!(target: "audit", action = "topup", customer_id, session_id, credits, "top-up credits applied");

    Ok(())
}

fn resolve_tier_from_subscription(state: &AppState, object: &serde_json::Value) -> Result<String> {
    // Try to resolve from price ID in the subscription items.
    let price_id = object
        .pointer("/items/data/0/price/id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let pro_price = state.stripe_price_pro.as_deref().unwrap_or("");
    let team_price = state.stripe_price_team.as_deref().unwrap_or("");

    if !price_id.is_empty() {
        if price_id == pro_price {
            return Ok("pro".to_string());
        }
        if price_id == team_price {
            return Ok("team".to_string());
        }
    }

    // Fallback: check metadata.
    let tier = object
        .pointer("/metadata/tier")
        .and_then(|v| v.as_str())
        .unwrap_or("pro");

    Ok(tier.to_string())
}

async fn apply_initial_credits(state: &AppState, customer_id: &str, tier: &str) -> Result<(), ApiError> {
    let db = db_conn(&state.db).await?;

    // Look up plan credits.
    let credit_cents: i32 = db.query_one(
        "SELECT credit_cents FROM plans WHERE id = $1", &[&tier],
    ).await.map(|r| r.get("credit_cents"))
        .unwrap_or(0);

    if credit_cents == 0 {
        return Ok(());
    }

    let credits: i64 = credit_cents as i64 * MICROCREDITS_PER_CENT;

    // Only apply if balance is currently 0 (avoid double-crediting on webhook replays).
    let n = db.execute(
        "UPDATE workspaces SET balance_credits = $1, updated_at = NOW() \
         WHERE stripe_customer_id = $2 AND balance_credits = 0 AND deleted_at IS NULL",
        &[&credits, &customer_id],
    ).await.map_err(|e| ApiError::internal(format!("apply credits: {e}")))?;

    if n > 0 {
        tracing::info!(customer_id, credits, "billing: initial subscription credits applied");
    }

    Ok(())
}

// ── Billing API endpoints ───────────────────────────────────────────────────

/// GET /billing/balance — current credits, plan, and usage summary.
pub async fn get_balance(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wid = extract_workspace_id(&state, &headers).await?;
    let db = db_conn(&state.db).await?;

    let row = db.query_one(
        "SELECT w.tier, w.balance_credits, w.topup_credits, \
                w.billing_period_start::text, w.billing_period_end::text, \
                p.price_cents, p.credit_cents, p.max_services, \
                p.max_vcpu, p.max_memory_mb, p.allows_always_on, p.max_wasm_invocations \
         FROM workspaces w \
         JOIN plans p ON p.id = w.tier \
         WHERE w.id = $1",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("balance query: {e}")))?;

    let balance_credits: i64 = row.get("balance_credits");
    let topup_credits: i64   = row.get("topup_credits");

    Ok(Json(serde_json::json!({
        "tier": row.get::<_, String>("tier"),
        "balance_credits": balance_credits,
        "topup_credits": topup_credits,
        "total_credits": balance_credits + topup_credits,
        "billing_period_start": row.get::<_, Option<String>>("billing_period_start"),
        "billing_period_end": row.get::<_, Option<String>>("billing_period_end"),
        "plan": {
            "price_cents": row.get::<_, i32>("price_cents"),
            "credit_cents": row.get::<_, i32>("credit_cents"),
            "max_services": row.get::<_, i32>("max_services"),
            "max_vcpu": row.get::<_, i32>("max_vcpu"),
            "max_memory_mb": row.get::<_, i32>("max_memory_mb"),
            "allows_always_on": row.get::<_, bool>("allows_always_on"),
            "max_wasm_invocations": row.get::<_, i64>("max_wasm_invocations"),
        }
    })))
}

/// GET /billing/usage — usage events for the current billing period.
pub async fn get_usage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wid = extract_workspace_id(&state, &headers).await?;
    let db = db_conn(&state.db).await?;

    // Get the workspace's billing period start (fall back to start of calendar month).
    let period_start: String = db.query_one(
        "SELECT COALESCE(billing_period_start, date_trunc('month', NOW())::date)::text AS period_start \
         FROM workspaces WHERE id = $1",
        &[&wid],
    ).await.map_err(|e| ApiError::internal(format!("billing period: {e}")))?.get("period_start");

    // Metal: total compute-minutes and cost for current billing period.
    let metal = db.query_one(
        "SELECT COALESCE(SUM(((quantity + 59) / 60) * vcpu), 0)    AS vcpu_minutes, \
                COALESCE(SUM(((quantity + 59) / 60) * memory_mb), 0) AS mb_minutes \
         FROM usage_events \
         WHERE workspace_id = $1 AND engine = 'metal' \
           AND created_at >= $2::date",
        &[&wid, &period_start],
    ).await.map_err(|e| ApiError::internal(format!("metal usage: {e}")))?;

    let vcpu_mins: i64 = metal.get("vcpu_minutes");
    let mb_mins: i64   = metal.get("mb_minutes");
    let metal_cost_credits = metal_cost(vcpu_mins, mb_mins);

    // Liquid: total invocations for current billing period.
    let liquid = db.query_one(
        "SELECT COALESCE(SUM(quantity), 0) AS total_invocations \
         FROM usage_events \
         WHERE workspace_id = $1 AND engine = 'liquid' \
           AND created_at >= $2::date",
        &[&wid, &period_start],
    ).await.map_err(|e| ApiError::internal(format!("liquid usage: {e}")))?;

    let invocations: i64 = liquid.get("total_invocations");

    Ok(Json(serde_json::json!({
        "period_start": period_start,
        "metal": {
            "vcpu_minutes": vcpu_mins,
            "memory_mb_minutes": mb_mins,
            "cost_microcredits": metal_cost_credits,
        },
        "liquid": {
            "invocations": invocations,
            "cost_microcredits": invocations * LIQUID_PER_INVOCATION,
        },
        "total_cost_microcredits": metal_cost_credits + invocations * LIQUID_PER_INVOCATION,
    })))
}

/// GET /billing/ledger — credit transaction history.
pub async fn get_ledger(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wid = extract_workspace_id(&state, &headers).await?;
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

    let entries: Vec<serde_json::Value> = rows.iter().map(|r| {
        serde_json::json!({
            "id": r.get::<_, uuid::Uuid>("id").to_string(),
            "amount": r.get::<_, i64>("amount"),
            "kind": r.get::<_, String>("kind"),
            "description": r.get::<_, Option<String>>("description"),
            "reference_id": r.get::<_, Option<String>>("reference_id"),
            "balance_after": r.get::<_, i64>("balance_after"),
            "created_at": r.get::<_, String>("created_at"),
        })
    }).collect();

    Ok(Json(serde_json::json!({ "entries": entries })))
}

/// POST /billing/topup — initiate a Stripe checkout for credit top-up.
pub async fn create_topup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TopupRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let stripe = state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable("billing not configured")
    })?;

    let wid = extract_workspace_id(&state, &headers).await?;
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

    Ok(Json(serde_json::json!({
        "checkout_url": session.url,
        "session_id": session.id,
    })))
}

/// POST /billing/subscribe — initiate a Stripe checkout for plan subscription.
pub async fn create_subscription(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SubscribeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let stripe = state.stripe.as_ref().ok_or_else(|| {
        ApiError::unavailable("billing not configured")
    })?;

    let wid = extract_workspace_id(&state, &headers).await?;
    let db = db_conn(&state.db).await?;

    let customer_id = get_or_create_stripe_customer(&state, &db, &wid).await?;

    let price_id = match body.tier.as_str() {
        "pro" => state.stripe_price_pro.as_deref()
            .ok_or_else(|| ApiError::unavailable("pro pricing not configured"))?,
        "team" => state.stripe_price_team.as_deref()
            .ok_or_else(|| ApiError::unavailable("team pricing not configured"))?,
        _ => return Err(ApiError::bad_request("invalid_tier", "tier must be 'pro' or 'team'")),
    };

    let session = stripe.create_checkout_session(
        &customer_id,
        price_id,
        &body.success_url,
        &body.cancel_url,
    ).await.map_err(|e| ApiError::bad_gateway(format!("Stripe: {e}")))?;

    Ok(Json(serde_json::json!({
        "checkout_url": session.url,
        "session_id": session.id,
    })))
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

/// Extract workspace_id from the request extensions (set by auth_middleware).
async fn extract_workspace_id(state: &AppState, headers: &HeaderMap) -> Result<uuid::Uuid, ApiError> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing X-Api-Key header"))?;

    let db = db_conn(&state.db).await?;

    let row = db.query_opt(
        "SELECT wm.workspace_id FROM users u \
         JOIN workspace_members wm ON wm.user_id = u.id \
         WHERE u.id::text = $1 AND u.deleted_at IS NULL \
         LIMIT 1",
        &[&api_key],
    ).await.map_err(|e| ApiError::internal(format!("workspace lookup: {e}")))?;

    row.map(|r| r.get("workspace_id"))
        .ok_or_else(|| ApiError::not_found("workspace not found for this API key"))
}

// ── Request types ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct TopupRequest {
    pub amount_cents: u64,
    pub success_url:  String,
    pub cancel_url:   String,
}

#[derive(serde::Deserialize)]
pub struct SubscribeRequest {
    pub tier:        String,
    pub success_url: String,
    pub cancel_url:  String,
}
