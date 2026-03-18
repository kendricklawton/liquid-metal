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
    h.send_expect(req, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn provision_rejects_missing_secret() {
    let h = harness!();
    let body = json!({ "email": "test@example.com", "first_name": "A", "last_name": "B" });
    // post_json doesn't attach internal secret
    h.send_expect(h.post_json("/auth/provision", &body), StatusCode::FORBIDDEN).await;
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
    assert!(v1["slug"].as_str().unwrap().ends_with("-workspace"));
    assert_eq!(v1["tier"], "hobby");

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
    assert_eq!(ws["tier"], "hobby");
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
    let body = h.send_ok(h.authed_get("/projects", user.api_key())).await;
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
async fn billing_balance_returns_hobby_defaults() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/balance", user.api_key())).await;
    assert_eq!(body["tier"], "hobby");
    // Hobby tier should have some included credits
    assert!(body["total_credits"].as_i64().is_some());
    assert!(body["plan"]["max_services"].as_i64().unwrap() > 0);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_usage_returns_zero_for_new_user() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/usage", user.api_key())).await;
    assert_eq!(body["metal"]["vcpu_minutes"], 0);
    assert_eq!(body["liquid"]["invocations"], 0);
    assert_eq!(body["total_cost_microcredits"], 0);
}

#[tokio::test]
#[ignore = "requires DATABASE_URL + NATS_URL"]
async fn billing_ledger_returns_entries() {
    let h = harness!();
    let user = h.provision_user().await;

    let body = h.send_ok(h.authed_get("/billing/ledger?limit=10", user.api_key())).await;
    // New user may have an initial credit entry from provisioning
    assert!(body["entries"].is_array());
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

    // Stop
    let stop_resp = h.send_ok(
        h.authed_post_empty(&format!("/services/{}/stop", service_id), user.api_key()),
    ).await;
    assert_eq!(stop_resp["status"].as_str().unwrap_or(""), "stopped");

    // Restart
    let restart_resp = h.send_ok(
        h.authed_post_empty(&format!("/services/{}/restart", service_id), user.api_key()),
    ).await;
    // After restart, status should not be "stopped"
    assert_ne!(restart_resp["status"].as_str().unwrap_or("stopped"), "stopped");

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

    // Remove domain
    h.send_ok(h.authed_post_empty(
        &format!("/services/{}/domains/{}/remove", service_id, domain),
        user.api_key(),
    )).await;

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
