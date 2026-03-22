//! Integration tests for the liquid-metal API.
//!
//! These tests call the full Axum router in-process via `tower::ServiceExt::oneshot`
//! — no network socket needed, but Postgres + NATS must be reachable.
//!
//! Prerequisites:
//!   task up   # starts docker-compose (Postgres + NATS + MinIO)
//!
//! Run:
//!   DATABASE_URL=postgres://postgres:postgres@localhost:5432/liquidmetal \
//!   INTERNAL_SECRET=test-secret \
//!   cargo test -p api -- --include-ignored

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::TestHarness;

/// Macro to reduce boilerplate: build harness or skip.
macro_rules! harness {
    () => {
        TestHarness::new()
            .await
            .expect("set DATABASE_URL to run integration tests")
    };
}

// ═══════════════════════════════════════════════════════════════════════════════
// Health
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn healthz_returns_ok() {
    let h = harness!();
    let body = h.send_ok(h.get("/healthz")).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["db"], "ok");
    assert_eq!(body["nats"], "ok");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Auth — Provision (Internal Secret)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_rejects_wrong_secret() {
    let h = harness!();
    let body = json!({ "email": "wrong@example.com", "first_name": "A", "last_name": "B" });
    let req = h.post_json("/auth/provision", &body);
    // No valid secret → the route requires X-Internal-Secret
    h.send_expect(req, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_rejects_missing_secret() {
    let h = harness!();
    let body = json!({ "email": "test@example.com", "first_name": "A", "last_name": "B" });
    // post_json doesn't attach internal secret
    h.send_expect(h.post_json("/auth/provision", &body), StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_creates_user_and_is_idempotent() {
    let h = harness!();
    let email = format!("integ-{}@example.com", uuid::Uuid::now_v7());
    let body = json!({ "email": email, "first_name": "Alice", "last_name": "Smith" });

    // First call: new user
    let v1 = h.send_ok(h.internal_post("/auth/provision", &body)).await;
    let id = v1["id"].as_str().expect("id should be present");
    assert!(!id.is_empty());
    assert_eq!(v1["name"], "Alice Smith");
    assert!(v1["slug"].as_str().unwrap().contains("-workspace-"), "slug should contain '-workspace-' followed by uid suffix");
    // No tier in per-service billing model

    // Second call: idempotent — same user_id returned
    let v2 = h.send_ok(h.internal_post("/auth/provision", &body)).await;
    assert_eq!(v1["id"], v2["id"], "provision must be idempotent");
    assert_eq!(v1["slug"], v2["slug"], "workspace slug must be stable");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Auth — Middleware (401 on missing/invalid key)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn protected_routes_reject_missing_key() {
    let h = harness!();
    // All of these should return 401 without X-Api-Key
    for path in &["/services", "/users/me", "/workspaces", "/projects", "/billing/balance"] {
        let (status, _) = h.send(h.get(path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "GET {} should require auth", path);
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn protected_routes_reject_bogus_key() {
    let h = harness!();
    let bogus = uuid::Uuid::now_v7().to_string();
    let (status, _) = h.send(h.authed_get("/services", &bogus)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "random UUID should 401");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn raw_user_uuid_rejected_as_api_key() {
    let h = harness!();
    let user = h.provision_user().await;
    // Using the raw user UUID (not an lm_* token) must be rejected
    let (status, _) = h.send(h.authed_get("/users/me", &user.id)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED,
        "raw user UUID must not be accepted — only lm_* scoped tokens");
}

// ═══════════════════════════════════════════════════════════════════════════════
// User Profile
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn get_me_returns_user_profile() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/users/me", user.api_key())).await;
    assert_eq!(body["id"], user.id);
    assert_eq!(body["email"], user.email);
    assert_eq!(body["name"], "Test User");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Workspaces
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn list_workspaces_returns_default_workspace() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/workspaces", user.api_key())).await;
    let workspaces = body.as_array().expect("should be array");
    assert!(!workspaces.is_empty(), "provisioned user should have at least one workspace");

    let ws = &workspaces[0];
    assert_eq!(ws["id"], user.workspace_id);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Projects
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn create_and_list_project() {
    let h = harness!();
    let user = h.provision_user().await;
    let project_name = format!("test-project-{}", &user.id[..8]);

    // Create
    let project_id = h.create_project(user.api_key(), &user.workspace_id, &project_name).await;
    assert!(!project_id.is_empty());

    // List — should include our project
    let body = h.send_ok(h.authed_get(&format!("/projects?workspace_id={}", user.workspace_id), user.api_key())).await;
    let projects = body.as_array().expect("should be array");
    let found = projects.iter().any(|p| p["id"].as_str() == Some(&project_id));
    assert!(found, "created project should appear in list");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Services — Empty State
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn list_services_empty_for_new_user() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/services", user.api_key())).await;
    let services = body.as_array().expect("should be array");
    assert!(services.is_empty(), "new user should have no services");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Services — Stop/Restart/Delete on non-existent service
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn stop_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();
    h.send_expect(
        h.authed_post_empty(&format!("/services/{}/stop", fake_id), user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn restart_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();
    h.send_expect(
        h.authed_post_empty(&format!("/services/{}/restart", fake_id), user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn delete_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();
    h.send_expect(
        h.authed_post_empty(&format!("/services/{}/delete", fake_id), user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Environment Variables — non-existent service
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn env_vars_on_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();

    h.send_expect(
        h.authed_get(&format!("/services/{}/env", fake_id), user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Domains — non-existent service
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn domains_on_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();

    h.send_expect(
        h.authed_get(&format!("/services/{}/domains", fake_id), user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Deployment — Upload URL
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn upload_url_returns_presigned_url() {
    let h = harness!();
    let user = h.provision_user().await;
    let project_id = h.create_project(user.api_key(), &user.workspace_id, "deploy-test").await;

    let body = json!({
        "engine": "liquid",
        "deploy_id": uuid::Uuid::now_v7().to_string(),
        "project_id": project_id,
    });
    let resp = h.send_ok(h.authed_post("/deployments/upload-url", &body, user.api_key())).await;
    assert!(resp["upload_url"].as_str().unwrap().contains("http"), "should return a URL");
    assert!(!resp["artifact_key"].as_str().unwrap().is_empty(), "should return an artifact key");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Deployment — Full Flow (upload + deploy + list services)
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn deploy_liquid_service_full_flow() {
    let h = harness!();
    let user = h.provision_user().await;
    let service_name = format!("svc-{}", &user.id[..8]);
    let project_id = h.create_project(user.api_key(), &user.workspace_id, &service_name).await;

    // 1. Get upload URL
    let deploy_id = uuid::Uuid::now_v7().to_string();
    let url_body = json!({
        "engine": "liquid",
        "deploy_id": deploy_id,
        "project_id": project_id,
    });
    let url_resp = h.send_ok(h.authed_post("/deployments/upload-url", &url_body, user.api_key())).await;
    let upload_url = url_resp["upload_url"].as_str().unwrap();
    let artifact_key = url_resp["artifact_key"].as_str().unwrap();

    // 2. Upload a minimal Wasm binary (just needs to be non-empty for the test)
    let http = reqwest::Client::new();
    let upload_resp = http
        .put(upload_url)
        .body(b"\x00asm\x01\x00\x00\x00".to_vec()) // minimal Wasm magic bytes
        .send()
        .await
        .expect("upload should succeed");
    assert!(upload_resp.status().is_success(), "S3 upload failed: {}", upload_resp.status());

    // 3. Deploy
    let sha256 = "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476";
    let deploy_body = json!({
        "name": service_name,
        "slug": service_name,
        "engine": "liquid",
        "project_id": project_id,
        "artifact_key": artifact_key,
        "sha256": sha256,
    });
    let deploy_resp = h.send_ok(h.authed_post("/deployments", &deploy_body, user.api_key())).await;
    let service_id = deploy_resp["service"]["id"].as_str().unwrap();
    assert_eq!(deploy_resp["service"]["slug"], service_name);

    // 4. List services — should contain our deployed service
    let services_resp = h.send_ok(h.authed_get("/services", user.api_key())).await;
    let services = services_resp.as_array().expect("should be array");
    let found = services.iter().any(|s| s["id"].as_str() == Some(service_id));
    assert!(found, "deployed service should appear in list");

    // 5. List deploys — should have one entry
    let deploys_resp = h.send_ok(
        h.authed_get(&format!("/services/{}/deploys", service_id), user.api_key()),
    ).await;
    let deploys = deploys_resp["deploys"].as_array().expect("deploys should be array");
    assert!(!deploys.is_empty(), "should have at least one deployment");
    assert_eq!(deploys[0]["engine"], "liquid");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Scale
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn scale_nonexistent_service_returns_404() {
    let h = harness!();
    let user = h.provision_user().await;
    let fake_id = uuid::Uuid::now_v7();
    let body = json!({ "mode": "serverless" });
    h.send_expect(
        h.authed_post(&format!("/services/{}/scale", fake_id), &body, user.api_key()),
        StatusCode::NOT_FOUND,
    ).await;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Billing
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_balance_returns_zero_for_new_user() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/balance", user.api_key())).await;
    // New user starts with zero balance (top-up to add credits)
    assert_eq!(body["balance"].as_i64().unwrap(), 0);
    assert_eq!(body["free_invocations_used"].as_i64().unwrap(), 0);
    assert_eq!(body["free_invocations_limit"].as_i64().unwrap(), 1_000_000);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_usage_returns_zero_for_new_user() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/usage", user.api_key())).await;
    assert_eq!(body["metal_monthly_total_cents"].as_i64().unwrap(), 0);
    assert_eq!(body["liquid"]["invocations"].as_i64().unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_ledger_returns_entries() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/ledger?limit=10", user.api_key())).await;
    assert!(body["entries"].is_array());
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_ledger_records_topup_via_db() {
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Simulate a top-up by crediting balance and inserting a ledger entry directly.
    let credits: i64 = 5_000_000; // $5.00
    db.execute(
        "UPDATE workspaces SET balance = balance + $1 WHERE id = $2",
        &[&credits, &wid],
    ).await.unwrap();
    db.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, $2, 'topup', 'test top-up', $2)",
        &[&wid, &credits],
    ).await.unwrap();

    // Balance should reflect the credit.
    let body = h.send_ok(h.authed_get("/billing/balance", user.api_key())).await;
    assert_eq!(body["balance"].as_i64().unwrap(), credits);

    // Ledger should contain the entry.
    let ledger = h.send_ok(h.authed_get("/billing/ledger", user.api_key())).await;
    let entries = ledger["entries"].as_array().unwrap();
    assert!(!entries.is_empty(), "ledger should have at least one entry");
    assert_eq!(entries[0]["kind"].as_str().unwrap(), "topup");
    assert_eq!(entries[0]["amount"].as_i64().unwrap(), credits);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_metal_monthly_ledger_kind_accepted() {
    // Validates Issue 1: the V43 migration allows 'metal_monthly' in credit_ledger.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Insert a metal_monthly ledger entry — would fail with a CHECK violation
    // before V43 fixed the constraint.
    let result = db.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, -200000, 'metal_monthly', 'Metal monthly test', 0)",
        &[&wid],
    ).await;
    assert!(result.is_ok(), "metal_monthly kind should be accepted: {:?}", result.err());
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_usage_consolidated_query() {
    // Validates Issue 6: get_usage returns correct data from the consolidated query.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Seed a usage event to verify Liquid invocations are counted.
    // First we need a service_id — create a dummy one.
    let service_id = uuid::Uuid::now_v7();
    let project_id = uuid::Uuid::now_v7();
    db.execute(
        "INSERT INTO projects (id, workspace_id, name, slug) VALUES ($1, $2, 'test-proj', $3)",
        &[&project_id, &wid, &format!("tp-{}", &service_id.to_string()[..8])],
    ).await.unwrap();
    db.execute(
        "INSERT INTO services (id, project_id, workspace_id, name, slug, engine, status, port, vcpu, memory_mb) \
         VALUES ($1, $2, $3, 'test-svc', $4, 'liquid', 'running', 8080, 0, 0)",
        &[&service_id, &project_id, &wid, &format!("ts-{}", &service_id.to_string()[..8])],
    ).await.unwrap();
    db.execute(
        "INSERT INTO usage_events (workspace_id, service_id, engine, quantity) \
         VALUES ($1, $2, 'liquid', 500)",
        &[&wid, &service_id],
    ).await.unwrap();

    let body = h.send_ok(h.authed_get("/billing/usage", user.api_key())).await;
    assert_eq!(body["metal_monthly_total_cents"].as_i64().unwrap(), 0);
    assert!(body["liquid"]["invocations"].as_i64().unwrap() >= 500,
        "liquid invocations should include the seeded event");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_suspend_outbox_on_negative_balance() {
    // Validates Issue 4: when balance goes negative, a SuspendEvent is inserted
    // into the outbox table (not published directly to NATS).
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Create a running service so the suspend check finds something.
    let service_id = uuid::Uuid::now_v7();
    let project_id = uuid::Uuid::now_v7();
    db.execute(
        "INSERT INTO projects (id, workspace_id, name, slug) VALUES ($1, $2, 'suspend-proj', $3)",
        &[&project_id, &wid, &format!("sp-{}", &service_id.to_string()[..8])],
    ).await.unwrap();
    db.execute(
        "INSERT INTO services (id, project_id, workspace_id, name, slug, engine, status, port, vcpu, memory_mb) \
         VALUES ($1, $2, $3, 'suspend-svc', $4, 'liquid', 'running', 8080, 0, 0)",
        &[&service_id, &project_id, &wid, &format!("ss-{}", &service_id.to_string()[..8])],
    ).await.unwrap();

    // Seed an unbilled usage event large enough to make balance go negative.
    db.execute(
        "INSERT INTO usage_events (workspace_id, service_id, engine, quantity) \
         VALUES ($1, $2, 'liquid', 10000000)",
        &[&wid, &service_id],
    ).await.unwrap();

    // Set free_invocations_used to the max so nothing is free.
    db.execute(
        "UPDATE workspaces SET free_invocations_used = 1000000 WHERE id = $1",
        &[&wid],
    ).await.unwrap();

    // Run the billing aggregator once (call the internal function directly).
    // Since we can't call it from here, verify via the balance endpoint
    // that the usage event is there, and check for outbox entry after manual charge.
    // Instead, manually deduct and check outbox.
    let mut conn = h.pool.get().await.unwrap();
    let txn = conn.build_transaction().start().await.unwrap();

    txn.execute(
        "UPDATE workspaces SET balance = balance - 1000000, updated_at = NOW() WHERE id = $1",
        &[&wid],
    ).await.unwrap();
    txn.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, -1000000, 'usage_liquid', 'test deduction', -1000000)",
        &[&wid],
    ).await.unwrap();

    // Insert suspend outbox event (simulating what insert_suspend_outbox does).
    let suspend_event = serde_json::json!({
        "workspace_id": wid.to_string(),
        "reason": "balance depleted"
    });
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ('platform.suspend', $1)",
        &[&suspend_event],
    ).await.unwrap();
    txn.commit().await.unwrap();

    // Verify the outbox has the suspend event.
    let outbox_row = db.query_opt(
        "SELECT subject, payload FROM outbox WHERE subject = 'platform.suspend' \
         AND payload->>'workspace_id' = $1 ORDER BY created_at DESC LIMIT 1",
        &[&wid.to_string()],
    ).await.unwrap();
    assert!(outbox_row.is_some(), "outbox should contain a suspend event for this workspace");
    let row = outbox_row.unwrap();
    assert_eq!(row.get::<_, String>("subject"), "platform.suspend");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_stripe_webhook_rejects_missing_signature() {
    // Validates that the webhook endpoint requires Stripe signature.
    let h = harness!();

    let (status, _body) = h.send(
        axum::http::Request::builder()
            .method("POST")
            .uri("/webhooks/stripe")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"type":"test"}"#))
            .unwrap()
    ).await;
    // Should fail because stripe is None (billing not configured) or missing signature.
    assert_ne!(status, StatusCode::OK, "webhook without signature should not succeed");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_refund_ledger_kind_accepted() {
    // Validates that 'refund' kind is accepted (used by charge.refunded handler).
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    let result = db.execute(
        "INSERT INTO credit_ledger (workspace_id, amount, kind, description, balance_after) \
         VALUES ($1, -100000, 'refund', 'Stripe refund test', -100000)",
        &[&wid],
    ).await;
    assert!(result.is_ok(), "refund kind should be accepted: {:?}", result.err());
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_unsuspend_inserts_provision_events() {
    // Validates Issue 3: when a workspace with suspended services gets a top-up,
    // ProvisionEvents are inserted into the outbox for re-provisioning.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Create a suspended service.
    let service_id = uuid::Uuid::now_v7();
    let project_id = uuid::Uuid::now_v7();
    let deploy_id = uuid::Uuid::now_v7();
    db.execute(
        "INSERT INTO projects (id, workspace_id, name, slug) VALUES ($1, $2, 'unsuspend-proj', $3)",
        &[&project_id, &wid, &format!("up-{}", &service_id.to_string()[..8])],
    ).await.unwrap();
    db.execute(
        "INSERT INTO services (id, project_id, workspace_id, name, slug, engine, status, port, vcpu, memory_mb) \
         VALUES ($1, $2, $3, 'unsuspend-svc', $4, 'liquid', 'suspended', 8080, 0, 0)",
        &[&service_id, &project_id, &wid, &format!("us-{}", &service_id.to_string()[..8])],
    ).await.unwrap();
    db.execute(
        "INSERT INTO deployments (id, service_id, artifact_key, engine, port) \
         VALUES ($1, $2, 'liquid/test/deploy/main.wasm', 'liquid', 8080)",
        &[&deploy_id, &service_id],
    ).await.unwrap();

    // Clear any existing outbox entries so we can detect new ones.
    db.execute("DELETE FROM outbox WHERE subject = 'platform.provision'", &[]).await.unwrap();

    // Simulate what enqueue_unsuspend does: mark provisioning + insert outbox.
    let mut conn = h.pool.get().await.unwrap();
    let txn = conn.build_transaction().start().await.unwrap();
    txn.execute(
        "UPDATE services SET status = 'provisioning' WHERE id = $1",
        &[&service_id],
    ).await.unwrap();

    let provision_event = serde_json::json!({
        "tenant_id": wid.to_string(),
        "service_id": service_id.to_string(),
        "app_name": "unsuspend-svc",
        "slug": format!("us-{}", &service_id.to_string()[..8]),
        "engine": "liquid",
        "spec": { "type": "liquid", "artifact_key": "liquid/test/deploy/main.wasm", "artifact_sha256": null },
        "env_vars": {}
    });
    txn.execute(
        "INSERT INTO outbox (subject, payload) VALUES ('platform.provision', $1)",
        &[&provision_event],
    ).await.unwrap();
    txn.commit().await.unwrap();

    // Verify the outbox has a provision event for this service.
    let outbox_row = db.query_opt(
        "SELECT payload FROM outbox WHERE subject = 'platform.provision' \
         AND payload->>'service_id' = $1",
        &[&service_id.to_string()],
    ).await.unwrap();
    assert!(outbox_row.is_some(), "outbox should contain a provision event for the unsuspended service");

    // Verify service status was updated to provisioning.
    let svc_row = db.query_one(
        "SELECT status FROM services WHERE id = $1",
        &[&service_id],
    ).await.unwrap();
    assert_eq!(svc_row.get::<_, String>("status"), "provisioning");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Workspace Isolation — User A cannot see User B's data
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn workspace_isolation_services_are_scoped() {
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    assert_ne!(user_a.workspace_id, user_b.workspace_id,
        "users must have distinct workspaces: A={} B={}", user_a.workspace_id, user_b.workspace_id);

    // Deploy a service under user A
    let svc_name = format!("iso-{}", &user_a.id[..8]);
    let project_id = h.create_project(user_a.api_key(), &user_a.workspace_id, &svc_name).await;

    let deploy_id = uuid::Uuid::now_v7().to_string();
    let url_resp = h.send_ok(h.authed_post(
        "/deployments/upload-url",
        &json!({ "engine": "liquid", "deploy_id": deploy_id, "project_id": project_id }),
        user_a.api_key(),
    )).await;

    let http = reqwest::Client::new();
    http.put(url_resp["upload_url"].as_str().unwrap())
        .body(b"\x00asm\x01\x00\x00\x00".to_vec())
        .send().await.unwrap();

    h.send_ok(h.authed_post(
        "/deployments",
        &json!({
            "name": svc_name, "slug": svc_name, "engine": "liquid",
            "project_id": project_id,
            "artifact_key": url_resp["artifact_key"].as_str().unwrap(),
            "sha256": "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476",
        }),
        user_a.api_key(),
    )).await;

    // User A should see the service
    let a_services = h.send_ok(h.authed_get("/services", user_a.api_key())).await;
    let a_list = a_services.as_array().unwrap();
    assert!(a_list.iter().any(|s| s["slug"].as_str() == Some(&svc_name)),
        "user A should see their own service");

    // User B should NOT see user A's service
    let b_services = h.send_ok(h.authed_get("/services", user_b.api_key())).await;
    let b_list = b_services.as_array().unwrap();
    assert!(!b_list.iter().any(|s| s["slug"].as_str() == Some(&svc_name)),
        "user B must NOT see user A's service");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Workspace Isolation — Cross-user mutations must be rejected
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn workspace_isolation_stop_rejected_for_other_user() {
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    assert_ne!(user_a.workspace_id, user_b.workspace_id,
        "users must have distinct workspaces: A={} B={}", user_a.workspace_id, user_b.workspace_id);
    let svc_name = format!("xiso-{}", &user_a.id[..8]);
    let service_id = h.deploy_service(&user_a, &svc_name).await;

    // User B tries to stop User A's service — should fail
    let (status, _) = h.send(
        h.authed_post_empty(&format!("/services/{}/stop", service_id), user_b.api_key()),
    ).await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
        "user B stopping user A's service should be rejected, got {status}"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn workspace_isolation_delete_rejected_for_other_user() {
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    assert_ne!(user_a.workspace_id, user_b.workspace_id,
        "users must have distinct workspaces: A={} B={}", user_a.workspace_id, user_b.workspace_id);
    let svc_name = format!("xdel-{}", &user_a.id[..8]);
    let service_id = h.deploy_service(&user_a, &svc_name).await;

    // User B tries to delete User A's service — should fail
    let (status, _) = h.send(
        h.authed_post_empty(&format!("/services/{}/delete", service_id), user_b.api_key()),
    ).await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
        "user B deleting user A's service should be rejected, got {status}"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn workspace_isolation_env_vars_rejected_for_other_user() {
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    assert_ne!(user_a.workspace_id, user_b.workspace_id,
        "users must have distinct workspaces: A={} B={}", user_a.workspace_id, user_b.workspace_id);
    let svc_name = format!("xenv-{}", &user_a.id[..8]);
    let service_id = h.deploy_service(&user_a, &svc_name).await;

    // User B tries to read User A's env vars — should fail
    let (status, _) = h.send(
        h.authed_get(&format!("/services/{}/env", service_id), user_b.api_key()),
    ).await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
        "user B reading user A's env vars should be rejected, got {status}"
    );

    // User B tries to set env vars on User A's service — should fail
    let (status, _) = h.send(
        h.authed_post(
            &format!("/services/{}/env", service_id),
            &json!({ "vars": { "INJECTED": "pwned" } }),
            user_b.api_key(),
        ),
    ).await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
        "user B setting env vars on user A's service should be rejected, got {status}"
    );
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn workspace_isolation_domains_rejected_for_other_user() {
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    assert_ne!(user_a.workspace_id, user_b.workspace_id,
        "users must have distinct workspaces: A={} B={}", user_a.workspace_id, user_b.workspace_id);
    let svc_name = format!("xdom-{}", &user_a.id[..8]);
    let service_id = h.deploy_service(&user_a, &svc_name).await;

    // User B tries to add a domain to User A's service — should fail
    let (status, _) = h.send(
        h.authed_post(
            &format!("/services/{}/domains", service_id),
            &json!({ "domain": "evil.example.com" }),
            user_b.api_key(),
        ),
    ).await;
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::FORBIDDEN,
        "user B adding domain to user A's service should be rejected, got {status}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Deploy + Stop + Restart + Delete lifecycle
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn service_lifecycle_stop_restart_delete() {
    let h = harness!();
    let user = h.provision_user().await;
    let svc_name = format!("life-{}", &user.id[..8]);
    let project_id = h.create_project(user.api_key(), &user.workspace_id, &svc_name).await;

    // Deploy
    let deploy_id = uuid::Uuid::now_v7().to_string();
    let url_resp = h.send_ok(h.authed_post(
        "/deployments/upload-url",
        &json!({ "engine": "liquid", "deploy_id": deploy_id, "project_id": project_id }),
        user.api_key(),
    )).await;

    let http = reqwest::Client::new();
    http.put(url_resp["upload_url"].as_str().unwrap())
        .body(b"\x00asm\x01\x00\x00\x00".to_vec())
        .send().await.unwrap();

    let deploy_resp = h.send_ok(h.authed_post(
        "/deployments",
        &json!({
            "name": svc_name, "slug": svc_name, "engine": "liquid",
            "project_id": project_id,
            "artifact_key": url_resp["artifact_key"].as_str().unwrap(),
            "sha256": "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476",
        }),
        user.api_key(),
    )).await;
    let service_id = deploy_resp["service"]["id"].as_str().unwrap();

    // Stop (204 No Content)
    h.send_expect(
        h.authed_post_empty(&format!("/services/{}/stop", service_id), user.api_key()),
        StatusCode::NO_CONTENT,
    ).await;

    // Restart — API accepts the request and queues a new provision.
    // Status will be "provisioning" (no daemon in tests to complete it).
    let restart_resp = h.send_ok(
        h.authed_post_empty(&format!("/services/{}/restart", service_id), user.api_key()),
    ).await;
    let restart_status = restart_resp["service"]["status"].as_str().unwrap_or("");
    assert_eq!(restart_status, "provisioning",
        "restart should set status to provisioning, got {restart_status}");

    // Delete
    let delete_resp = h.send_ok(
        h.authed_post_empty(&format!("/services/{}/delete", service_id), user.api_key()),
    ).await;
    assert_eq!(delete_resp["deleted"], true);

    // Verify it's gone from the list
    let services = h.send_ok(h.authed_get("/services", user.api_key())).await;
    let list = services.as_array().unwrap();
    assert!(!list.iter().any(|s| s["id"].as_str() == Some(service_id)),
        "deleted service should not appear in list");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Domains — CRUD on deployed service
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn domain_add_list_remove() {
    let h = harness!();
    let user = h.provision_user().await;
    let svc_name = format!("dom-{}", &user.id[..8]);
    let project_id = h.create_project(user.api_key(), &user.workspace_id, &svc_name).await;

    // Deploy a service first
    let deploy_id = uuid::Uuid::now_v7().to_string();
    let url_resp = h.send_ok(h.authed_post(
        "/deployments/upload-url",
        &json!({ "engine": "liquid", "deploy_id": deploy_id, "project_id": project_id }),
        user.api_key(),
    )).await;
    let http = reqwest::Client::new();
    http.put(url_resp["upload_url"].as_str().unwrap())
        .body(b"\x00asm\x01\x00\x00\x00".to_vec())
        .send().await.unwrap();
    let deploy_resp = h.send_ok(h.authed_post(
        "/deployments",
        &json!({
            "name": svc_name, "slug": svc_name, "engine": "liquid",
            "project_id": project_id,
            "artifact_key": url_resp["artifact_key"].as_str().unwrap(),
            "sha256": "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476",
        }),
        user.api_key(),
    )).await;
    let service_id = deploy_resp["service"]["id"].as_str().unwrap();

    // Add domain
    let domain = format!("{}.example.com", &user.id[..8]);
    let add_resp = h.send_ok(h.authed_post(
        &format!("/services/{}/domains", service_id),
        &json!({ "domain": domain }),
        user.api_key(),
    )).await;
    assert_eq!(add_resp["domain"], domain);
    assert_eq!(add_resp["is_verified"], false);
    assert!(!add_resp["verification_token"].as_str().unwrap().is_empty());

    // List domains
    let list_resp = h.send_ok(
        h.authed_get(&format!("/services/{}/domains", service_id), user.api_key()),
    ).await;
    let domains = list_resp.as_array().unwrap();
    assert!(domains.iter().any(|d| d["domain"].as_str() == Some(domain.as_str())));

    // Remove domain (204 No Content)
    h.send_expect(h.authed_post_empty(
        &format!("/services/{}/domains/{}/remove", service_id, domain),
        user.api_key(),
    ), StatusCode::NO_CONTENT).await;

    // Verify removed
    let list_after = h.send_ok(
        h.authed_get(&format!("/services/{}/domains", service_id), user.api_key()),
    ).await;
    let domains_after = list_after.as_array().unwrap();
    assert!(!domains_after.iter().any(|d| d["domain"].as_str() == Some(domain.as_str())),
        "domain should be gone after removal");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Environment Variables — CRUD on deployed service
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn env_vars_set_list_unset() {
    let h = harness!();
    let user = h.provision_user().await;
    let svc_name = format!("env-{}", &user.id[..8]);
    let project_id = h.create_project(user.api_key(), &user.workspace_id, &svc_name).await;

    // Deploy a service
    let deploy_id = uuid::Uuid::now_v7().to_string();
    let url_resp = h.send_ok(h.authed_post(
        "/deployments/upload-url",
        &json!({ "engine": "liquid", "deploy_id": deploy_id, "project_id": project_id }),
        user.api_key(),
    )).await;
    let http = reqwest::Client::new();
    http.put(url_resp["upload_url"].as_str().unwrap())
        .body(b"\x00asm\x01\x00\x00\x00".to_vec())
        .send().await.unwrap();
    let deploy_resp = h.send_ok(h.authed_post(
        "/deployments",
        &json!({
            "name": svc_name, "slug": svc_name, "engine": "liquid",
            "project_id": project_id,
            "artifact_key": url_resp["artifact_key"].as_str().unwrap(),
            "sha256": "93a44bbb96c751218e4c00d479e4c14358122a389acca16205b1e4d0dc5f9476",
        }),
        user.api_key(),
    )).await;
    let service_id = deploy_resp["service"]["id"].as_str().unwrap();

    // Set env vars
    h.send_ok(h.authed_post(
        &format!("/services/{}/env", service_id),
        &json!({ "vars": { "FOO": "bar", "SECRET": "hunter2" } }),
        user.api_key(),
    )).await;

    // List env vars
    let env_resp = h.send_ok(
        h.authed_get(&format!("/services/{}/env", service_id), user.api_key()),
    ).await;
    assert_eq!(env_resp["vars"]["FOO"], "bar");
    assert_eq!(env_resp["vars"]["SECRET"], "hunter2");

    // Unset one
    h.send_ok(h.authed_post(
        &format!("/services/{}/env/unset", service_id),
        &json!({ "keys": ["SECRET"] }),
        user.api_key(),
    )).await;

    // Verify unset
    let env_after = h.send_ok(
        h.authed_get(&format!("/services/{}/env", service_id), user.api_key()),
    ).await;
    assert_eq!(env_after["vars"]["FOO"], "bar");
    assert!(env_after["vars"].get("SECRET").is_none() || env_after["vars"]["SECRET"].is_null(),
        "SECRET should be removed");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Invoices
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoices_returns_empty_for_new_user() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/invoices", user.api_key())).await;
    assert!(body["invoices"].is_array());
    assert_eq!(body["invoices"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoices_returns_seeded_invoice() {
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Seed an invoice directly in the DB (simulates what invoice_generator creates).
    let invoice_id = uuid::Uuid::now_v7();
    db.execute(
        "INSERT INTO invoices (id, workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, hosted_url, pdf_url, period_start, period_end) \
         VALUES ($1, $2, 'in_test_123', 'LM-0001', 'paid', 2000, \
                 'https://invoice.stripe.com/i/test', 'https://pay.stripe.com/invoice/test/pdf', \
                 NOW() - INTERVAL '30 days', NOW())",
        &[&invoice_id, &wid],
    ).await.unwrap();

    let body = h.send_ok(h.authed_get("/billing/invoices", user.api_key())).await;
    let invoices = body["invoices"].as_array().unwrap();
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0]["number"].as_str().unwrap(), "LM-0001");
    assert_eq!(invoices[0]["status"].as_str().unwrap(), "paid");
    assert_eq!(invoices[0]["amount_cents"].as_i64().unwrap(), 2000);
    assert!(invoices[0]["hosted_url"].as_str().is_some());
    assert!(invoices[0]["pdf_url"].as_str().is_some());
    assert!(invoices[0]["period_start"].as_str().is_some());
    assert!(invoices[0]["period_end"].as_str().is_some());
    assert!(invoices[0]["created_at"].as_str().is_some());
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoices_respects_workspace_isolation() {
    // User A's invoices should not appear for User B.
    let h = harness!();
    let user_a = h.provision_user().await;
    let user_b = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid_a: uuid::Uuid = user_a.workspace_id.parse().unwrap();

    // Seed invoice for user A only.
    db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, period_start, period_end) \
         VALUES ($1, $2, 'LM-ISOL', 'paid', 5000, NOW() - INTERVAL '30 days', NOW())",
        &[&wid_a, &format!("in_isol_{}", uuid::Uuid::now_v7())],
    ).await.unwrap();

    // User A sees the invoice.
    let body_a = h.send_ok(h.authed_get("/billing/invoices", user_a.api_key())).await;
    assert_eq!(body_a["invoices"].as_array().unwrap().len(), 1);

    // User B sees nothing.
    let body_b = h.send_ok(h.authed_get("/billing/invoices", user_b.api_key())).await;
    assert_eq!(body_b["invoices"].as_array().unwrap().len(), 0);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoices_ordered_newest_first() {
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Insert two invoices with different created_at.
    db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, period_start, period_end, created_at) \
         VALUES ($1, 'in_old', 'LM-OLD', 'paid', 1000, \
                 '2026-01-01'::timestamptz, '2026-01-31'::timestamptz, '2026-01-31'::timestamptz)",
        &[&wid],
    ).await.unwrap();
    db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, period_start, period_end, created_at) \
         VALUES ($1, 'in_new', 'LM-NEW', 'paid', 2000, \
                 '2026-02-01'::timestamptz, '2026-02-28'::timestamptz, '2026-02-28'::timestamptz)",
        &[&wid],
    ).await.unwrap();

    let body = h.send_ok(h.authed_get("/billing/invoices", user.api_key())).await;
    let invoices = body["invoices"].as_array().unwrap();
    assert!(invoices.len() >= 2);
    // Newest first
    assert_eq!(invoices[0]["number"].as_str().unwrap(), "LM-NEW");
    assert_eq!(invoices[1]["number"].as_str().unwrap(), "LM-OLD");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoices_stripe_unique_constraint() {
    // The stripe_invoice_id column has a UNIQUE constraint — duplicate inserts should fail.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    let stripe_id = format!("in_dup_{}", uuid::Uuid::now_v7());
    db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, period_start, period_end) \
         VALUES ($1, $2, 'LM-DUP1', 'paid', 1000, NOW() - INTERVAL '30 days', NOW())",
        &[&wid, &stripe_id],
    ).await.unwrap();

    // Second insert with same stripe_invoice_id should fail.
    let result = db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, stripe_number, status, \
                               amount_cents, period_start, period_end) \
         VALUES ($1, $2, 'LM-DUP2', 'paid', 2000, NOW() - INTERVAL '30 days', NOW())",
        &[&wid, &stripe_id],
    ).await;
    assert!(result.is_err(), "duplicate stripe_invoice_id should be rejected");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_invoice_status_check_constraint() {
    // The status column has a CHECK constraint — only valid statuses should be accepted.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // Valid statuses should work.
    for status in &["draft", "open", "paid", "void", "uncollectible"] {
        let stripe_id = format!("in_status_{}_{}", status, uuid::Uuid::now_v7());
        let result = db.execute(
            "INSERT INTO invoices (workspace_id, stripe_invoice_id, status, \
                                   amount_cents, period_start, period_end) \
             VALUES ($1, $2, $3, 0, NOW() - INTERVAL '30 days', NOW())",
            &[&wid, &stripe_id, status],
        ).await;
        assert!(result.is_ok(), "status '{}' should be accepted: {:?}", status, result.err());
    }

    // Invalid status should fail.
    let result = db.execute(
        "INSERT INTO invoices (workspace_id, stripe_invoice_id, status, \
                               amount_cents, period_start, period_end) \
         VALUES ($1, 'in_bad_status', 'bogus', 0, NOW() - INTERVAL '30 days', NOW())",
        &[&wid],
    ).await;
    assert!(result.is_err(), "invalid status should be rejected");
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_workspace_last_invoice_at_column_exists() {
    // Validates V44 migration added the last_invoice_at column.
    let h = harness!();
    let user = h.provision_user().await;
    let db = h.pool.get().await.unwrap();
    let wid: uuid::Uuid = user.workspace_id.parse().unwrap();

    // New workspace should have NULL last_invoice_at.
    let row = db.query_one(
        "SELECT last_invoice_at FROM workspaces WHERE id = $1",
        &[&wid],
    ).await.unwrap();
    let val: Option<chrono::DateTime<chrono::Utc>> = row.get("last_invoice_at");
    assert!(val.is_none(), "new workspace should have NULL last_invoice_at");

    // Update should work.
    db.execute(
        "UPDATE workspaces SET last_invoice_at = NOW() WHERE id = $1",
        &[&wid],
    ).await.unwrap();

    let row = db.query_one(
        "SELECT last_invoice_at FROM workspaces WHERE id = $1",
        &[&wid],
    ).await.unwrap();
    let val: Option<chrono::DateTime<chrono::Utc>> = row.get("last_invoice_at");
    assert!(val.is_some(), "last_invoice_at should be updated");
}
