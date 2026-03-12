# Edge Cases — Liquid Metal

Status legend: 📋 Planned | 🔧 Infra-only (no code needed yet) | ⚠️ Monitor | ✅ Done

Scale bands represent cumulative active users at time of concern — not new signups per day.

---

## 0 – 99 Users: Operational Stability

*The platform is still finding its footing. Problems here are correctness bugs, missing recovery paths, and security hardening — not load.*

---

### ✅ Rate Limiting on Auth Endpoints

**Problem:** No rate limiting on `/auth/cli/provision`, `/admin/invites`, or any API endpoint. An attacker can brute-force invite codes, flood the OIDC device flow, or DoS the deploy path. With users on the platform and SLOs to maintain, this is the highest-priority security gap.

**Fix (done):** Per-IP token-bucket rate limiting via `governor` crate, applied as Axum `route_layer` middleware on three route groups: auth/internal (10 req/min default via `RATE_LIMIT_AUTH_RPM`), CLI auth (same), and protected REST (60 req/min default via `RATE_LIMIT_API_RPM`). Returns `429 Too Many Requests` with `Retry-After` header and JSON error body.

**Where:** `crates/api/src/rate_limit.rs` (middleware + `RateLimit` type), `crates/api/src/lib.rs` (route group layering), `crates/api/src/main.rs` (env var parsing).

---

### ✅ Constant-Time Internal Secret Comparison

**Problem:** Comparing `INTERNAL_SECRET` using `!=` is vulnerable to timing attacks. An attacker can measure response times to leak the secret prefix byte-by-byte. This secret gates user provisioning and invite generation.

**Fix (done):** `verify_internal_secret()` uses `subtle::ConstantTimeEq` (`ct_eq`) to compare the header value against each valid secret. Supports zero-downtime rotation via comma-separated secrets. Also used in Stripe webhook signature verification.

**Where:** `crates/api/src/routes.rs:95-106` — `verify_internal_secret()`. `crates/api/src/stripe.rs:167` — webhook sig check.

---

### ✅ NATS Authentication + Per-Subject ACLs

**Problem:** `async_nats::connect(&nats_url)` uses no credentials. Any machine on the Tailnet can connect to NATS and publish a `ProvisionEvent`, spinning up arbitrary Firecracker VMs on bare metal nodes. Even with authentication, a single shared user means any compromised service can publish/subscribe to all subjects.

**Fix (done):** Two layers: (1) `common::config::nats_connect()` authenticates with `NATS_USER`/`NATS_PASSWORD` from env vars — all crates use this shared helper, falls back to unauthenticated for local dev. (2) NATS server config defines three users with least-privilege per-subject ACLs: **API** (`liquidmetal`) can publish provision/deprovision/suspend and subscribe to usage events; **Daemon** (`daemon`) can subscribe to provision/deprovision/suspend/traffic_pulse and publish route updates + usage; **Proxy** (`proxy`) can publish traffic pulses and subscribe to route updates only. Each role gets its own password via Nomad Variables. 12-factor: all credentials come from env vars, no application code changes needed.

**Where:** `crates/common/src/config.rs:13-36` — `nats_connect()`. `infra/terraform/templates/cloud-init-nat.yaml` — NATS server config with multi-user ACLs. `infra/terraform/main.tf` — `nats_password_daemon`/`nats_password_proxy` variables. `infra/nomad/*.nomad.hcl` — per-role Nomad Variable secrets.

---

### ✅ TLS on Postgres Connections

**Problem:** The API connects to Postgres with `tokio_postgres::NoTls`. In production, queries cross the Tailscale tunnel to Vultr Managed Postgres. While Tailscale encrypts the tunnel, defense-in-depth requires TLS at the application layer — especially for a managed database that provides TLS certificates.

**Fix (done):** `common::config::pg_tls()` reads the `POSTGRES_TLS_CA` env var (path to PEM-encoded CA certificate), builds a `rustls::ClientConfig` with the Vultr-provided CA, and returns a `MakeRustlsConnect` connector. When `POSTGRES_TLS_CA` is unset (local dev), returns `None` and all crates fall back to `tokio_postgres::NoTls`. 12-factor: TLS is toggled entirely by the presence of an env var, no code changes between dev and prod.

**Where:** `crates/common/src/config.rs:40-70` — `pg_tls()` helper. `crates/api/src/main.rs`, `crates/daemon/src/main.rs`, `crates/proxy/src/db.rs` — pool builders branch on `pg_tls()`. `crates/api/src/migrations.rs` — direct `tokio_postgres::connect()` also branches on TLS. Dependencies: `tokio-postgres-rustls`, `rustls` (ring backend), `rustls-pemfile` in workspace `Cargo.toml`.

---

### ✅ Audit Logging for Sensitive Operations

**Problem:** No structured audit trail for security-relevant operations: user provisioning, service creation/deletion, invite generation, API key usage. Application logs exist but aren't queryable as an audit stream. When something goes wrong, there's no "who did what when" record.

**Fix:** All sensitive handlers emit structured `tracing::info!(target: "audit", ...)` / `tracing::warn!(target: "audit", ...)` events with `action`, `user_id`, `resource_id`, and `result` fields. Covers: user provisioning, CLI provisioning, invite creation, service deploy, service stop, and auth failures. The `target: "audit"` label routes these to a dedicated VictoriaLogs stream for querying.

**Where:** `crates/api/src/routes.rs` — all auth, provisioning, invite, deploy, and stop handlers.

---

### ✅ S3 Bucket Access Policy

**Problem:** The Vultr Object Storage bucket has no explicit private ACL — it relies on Vultr's default behavior. If that default changes or is misconfigured, user artifacts (rootfs images, Wasm binaries) become publicly readable. Locally, the bucket may not even exist on first startup.

**Fix:** `api::storage::ensure_bucket()` runs on every API startup. First creates the bucket if it doesn't exist (idempotent — ignores `BucketAlreadyOwnedByYou`). Then calls `PutBucketAcl` with `private`. If either call fails, logs a warning and continues — defense-in-depth, not a hard gate.

**Where:** `crates/api/src/storage.rs`, called from `crates/api/src/main.rs` at startup.

---

### ✅ Stuck Provisioning (Daemon Crash Mid-Deploy)

**Problem:** If the daemon crashes after consuming a `ProvisionEvent` from JetStream but before writing `status='running'` back to Postgres, the service stays stuck in `provisioning` forever. The outbox row has already been deleted (JetStream acked), so no retry occurs. The user sees their deploy hang indefinitely.

**Fix:** Background watchdog task in the API runs every 60s. Marks services stuck in `provisioning` for >10 minutes as `failed`, surfacing the error so the user can redeploy.

**Where:** `crates/api/src/main.rs` — spawned at startup.

---

### ✅ Outbox Accumulation During Extended NATS Outage

**Problem:** The outbox poller retries every second but never purges old rows. If NATS is down for hours, the `outbox` table grows unboundedly. When NATS recovers, it replays events for services the user may have since deleted — provisioning ghost services.

**Fix:** Two mechanisms: (1) `stop_service` deletes pending outbox rows for the stopped service (`DELETE FROM outbox WHERE payload->>'service_id' = $1`). (2) The outbox poller runs a cleanup pass every ~60s, purging rows older than 30 minutes (`DELETE FROM outbox WHERE created_at < NOW() - interval '30 minutes'`). Events that old indicate a permanent failure, and the user may have since deleted the service.

**Where:** `crates/api/src/routes.rs` stop handler + `crates/api/src/outbox.rs` `cleanup_stale()`.

---

### ✅ Firecracker Process Leak on Spawn Failure

**Problem:** `spawn_firecracker_direct` in `provision.rs` spawns a Firecracker child process and waits for the API socket to appear. If the socket never appears (permission denied, binary crash, disk full), the function returns an error but the child process is left running. No process group management means the orphaned Firecracker process holds its TAP device and consumes CPU/memory indefinitely.

**Fix (done):** Both `spawn_firecracker_direct` and `jailer::spawn` now use `.process_group(0)` to put the child in its own process group (pgid = pid). On socket timeout, `kill(-pgid, SIGKILL)` terminates the entire group (Firecracker + any children) before returning the error. The `child.wait()` call reaps the zombie to avoid PID table leaks.

**Where:** `crates/daemon/src/provision.rs:390-411` — `spawn_firecracker_direct`. `crates/daemon/src/jailer.rs:100-110` — `spawn`.

---

### ✅ Provision Partial Failure — VM Running, DB Write Lost

**Problem:** If `provision_metal` succeeds through Firecracker start but the downstream DB write (`UPDATE services SET status='running', upstream_addr=...`) fails (connection drop, pool exhausted), the VM is running and consuming resources but the service stays in `provisioning`. The stuck-provisioning watchdog eventually marks it `failed`, but the VM process is never killed because the daemon has no handle to it after the task exits.

**Fix (done):** `provision()` catches DB write failures and calls `rollback_provision()` before returning the error. For Metal: removes the VM handle from the in-memory registry and runs the full `deprovision::metal()` teardown (SIGTERM → SIGKILL, eBPF detach, TAP delete, cgroup cleanup, SMT re-online, artifact cache delete, jailer chroot cleanup). For Liquid: removes from the billing registry — the orphaned localhost HTTP listener is harmless since no `upstream_addr` was written, so no traffic can reach it.

**Where:** `crates/daemon/src/provision.rs` — `provision()` + `rollback_provision()`.

---

### ✅ Deprovision After Daemon Restart — No VM Handle

**Problem:** The daemon keeps Firecracker process handles in an in-memory `HashMap<service_id, ChildHandle>`. If the daemon restarts, this map is empty. When a `DeprovisionEvent` arrives for a service provisioned by the previous daemon instance, the handler finds no handle, logs a warning, and skips cleanup. The Firecracker process, TAP device, and cgroup resources are leaked.

**Fix (done):** Two parts: (1) `provision_metal` now persists `fc_pid`, `cpu_core`, and `vm_id` to the `services` table alongside `tap_name` during provisioning. (2) On daemon startup, a new registry rebuild step queries the DB for all running Metal services on this node and populates the in-memory `VmRegistry` with `VmHandle` entries reconstructed from the stored metadata. Deprovision events that arrive after a restart now find valid handles and run the full teardown.

**Where:** `migrations/V24__vm_metadata.sql` — adds `fc_pid`, `cpu_core`, `vm_id` columns. `crates/daemon/src/provision.rs` — persists all four fields. `crates/daemon/src/main.rs` — startup registry rebuild.

---

### ✅ Partial Artifact Upload — Corrupt Object in S3

**Problem:** If the CLI's network drops mid-PUT to the presigned S3 URL, MinIO/Vultr may store a truncated object. The daemon downloads this partial file, the SHA256 check fails, and provisioning errors out. But the `artifact_key` in the DB still points to the corrupt object. The user must redeploy to generate a new presigned URL and re-upload — but the old corrupt object is never cleaned up, wasting storage.

**Fix (done):** The `deploy_service` handler now soft-deletes old stopped/failed services with the same slug (inside the deploy transaction) and fire-and-forget deletes the old S3 artifact after commit. A periodic S3 garbage-collection job (for orphaned presigned uploads that were never finalized) is still planned.

**Where:** `crates/api/src/routes.rs` — `deploy_service` handler. Separate GC task in `crates/api/src/main.rs` (planned).

---

### ✅ OIDC Token Expiry Mid-Deploy

**Problem:** The CLI's OIDC access token has a short lifetime (typically 5–15 minutes depending on provider). If a user starts `flux deploy` with a Metal rootfs image on a slow connection, the token may expire between the initial API call (which succeeds) and the confirmation call after upload completes. The user sees a confusing `401 Unauthorized` after a successful upload.

**Fix (done — by design):** Not applicable. The CLI authenticates all API calls with `X-Api-Key` (the user's persistent UUID from the `users` table), not the OIDC access token. The OIDC token is only used during the device-flow login to fetch userinfo and provision the account — it's never sent to the Liquid Metal API after login completes. Since the API key has no expiry, deploys of any duration work without token refresh.

**Where:** `crates/cli/src/client.rs` — `ApiClient` sends `X-Api-Key` header on all requests. `crates/api/src/routes.rs:163-190` — auth middleware validates UUID against `users` table.

---

### ✅ Artifact Cache Disk Full

**Problem:** The daemon downloads artifacts (rootfs images, Wasm modules) to `ARTIFACT_DIR` (`/var/lib/liquid-metal/artifacts`). Old artifacts from deleted services are never cleaned up. On a Metal node with many deploys over time, the partition fills. The next `provision_metal` fails with an opaque I/O error when writing the rootfs — the user sees "deploy failed" with no indication of the root cause.

**Fix (done):** Three mechanisms: (1) `cleanup_orphaned_artifacts()` runs on daemon startup — lists all directories in `ARTIFACT_DIR`, queries DB for running/provisioning services on this node, deletes any directory not referenced. (2) Both deprovision paths now delete artifacts: `deprovision::metal()` already had step 7 (artifact cache delete); the Liquid deprovision handler now also deletes the service's artifact directory. (3) `check_disk_space()` runs on startup — calls `statvfs` on the artifact partition and logs a warning if usage exceeds 80%.

**Where:** `crates/daemon/src/provision.rs` — `cleanup_orphaned_artifacts()` + `check_disk_space()`. `crates/daemon/src/main.rs` — startup calls + Liquid deprovision artifact cleanup.

---

### ✅ Bridge IP Exhaustion

**Problem:** Each Firecracker VM gets a TAP device with a unique IP in the `172.16.0.0/12` subnet. The daemon assigns IPs sequentially from a counter initialized at startup. Over time with many create/delete cycles, the counter only goes up — it never reclaims IPs from deleted VMs. The IP scheme supports 3969 concurrent VMs per node, but a monotonic counter exhausts the address space after 3969 cumulative deploys regardless of how many VMs are actually running.

**Fix (done):** Replaced the `AtomicU32` monotonic counter with a `Mutex<BTreeSet<u32>>` tracking in-use TAP indices. `init_tap_indices()` populates the set from DB on startup. `allocate_tap_index()` finds the smallest unused index (first gap in the sorted set). `release_tap_index()` returns an index to the pool on deprovision (called in both the normal deprovision path and `rollback_provision`). Indices are now recycled — 3969 is the concurrent limit, not the cumulative limit.

**Where:** `crates/daemon/src/provision.rs` — `TAP_INDICES` set, `init_tap_indices()`, `allocate_tap_index()`, `release_tap_index()`. `crates/daemon/src/main.rs` — calls `release_tap_index` in the deprovision handler.

---

### ✅ Migration Race on Multi-Instance Startup

**Problem:** Refinery migrations run on API startup before the HTTP server binds. If two API instances start simultaneously (e.g. Nomad rolling update with `stagger=0`), both run `refinery::Runner::run_async`. Refinery uses an advisory lock internally, but if the lock wait times out or the first instance crashes mid-migration, the second instance may see a partially-applied migration and fail to start.

**Fix (done):** Migrations are now separated from the API service startup. (1) The `migrate` Nomad batch job (`nomad job dispatch migrate`) runs the API binary with `--migrate`, applying pending migrations via refinery with owner-privilege credentials. (2) Normal API startup no longer runs DDL — `migrations::verify()` queries `refinery_schema_history` and compares the DB version against the binary's embedded migrations. If behind, the API refuses to start with a clear error message pointing the operator to `nomad job dispatch migrate`. This also means the API's runtime `DATABASE_URL` no longer needs DDL privileges.

**Where:** `crates/api/src/migrations.rs` — `verify()` + `expected_version()`. `crates/api/src/main.rs` — `--migrate` mode (lines 18-29) and verify-on-startup. `infra/nomad/migrate.nomad.hcl` — batch job using API binary.

---

### ✅ Workspace Deletion with Running Services

**Problem:** If a user deletes their workspace (soft-delete via `deleted_at`), any running services in that workspace continue running. The daemon doesn't know the workspace was deleted — it only acts on explicit `DeprovisionEvent` messages. The idle timeout checker may also skip these services because the workspace query returns no rows. VMs run indefinitely, consuming resources, with no billing because the workspace ledger is gone.

**Fix:** Both approaches implemented:
1. **API `DELETE /workspaces/{id}`** — enumerates all running/provisioning services, publishes `DeprovisionEvent` for each, then soft-deletes services → projects → workspace. Owner-only.
2. **Daemon orphan sweep** — runs every 60s, finds services on this node whose `workspace.deleted_at IS NOT NULL`, marks them stopped and publishes `DeprovisionEvent`. Safety net for partial NATS publish failures.

**Where:** `crates/api/src/routes.rs` — `delete_workspace`. `crates/api/src/lib.rs` — `DELETE /workspaces/{id}` route. `crates/daemon/src/main.rs` — deleted-workspace orphan sweep task.

---

### ✅ Clock Skew Between Nodes

**Problem:** The idle timeout check (`last_request_at + 5 minutes < NOW()`) runs on the daemon node, but `last_request_at` is written by the traffic pulse handler which uses Postgres `NOW()`. If the daemon node's clock drifts ahead of Postgres (or the NAT VPS running Postgres), services are suspended prematurely. A 30-second clock skew causes services to idle-timeout 30 seconds early — annoying but not catastrophic. A 5-minute skew causes immediate suspension of all services.

**Fix:** Both infra and daemon-side:
1. **Cloud-init** — `chrony` added to packages, enabled on boot. All nodes NTP-synced.
2. **Daemon startup** — compares `EXTRACT(EPOCH FROM NOW())` from Postgres against local `SystemTime::now()`. Logs warning at >5s drift, error at >60s.

**Where:** `infra/terraform/templates/cloud-init-node.yaml` — chrony package + service. `crates/daemon/src/main.rs` — clock drift check at startup.

---

### ✅ Tailscale Key Expiry

**Problem:** Bare metal nodes authenticate to Tailscale with a one-time auth key injected via cloud-init. Tailscale node keys expire after 180 days by default. When a key expires, the node silently drops off the Tailnet. NATS, Postgres, and inter-node communication all fail simultaneously. The node appears healthy locally (systemd services running) but is completely isolated.

**Fix:** Done by design — cloud-init already passes `--advertise-tags=tag:liquid-metal` to `tailscale up`. **Tagged nodes never expire** in Tailscale (they're owned by the tailnet, not a user). Auth keys are Terraform-managed (`tailscale_tailnet_key` resources, 1-hour expiry) and only used once during initial provisioning — their expiry is irrelevant after `tailscale up` completes.

**Prerequisite:** The Tailscale ACL policy must define `tag:liquid-metal` with appropriate `tagOwners`. This is a Tailscale admin console setting, not Terraform-managed.

**Where:** `infra/terraform/nodes.tf` — `tailscale_tailnet_key` resources. `infra/terraform/templates/cloud-init-node.yaml` — `--advertise-tags=tag:liquid-metal`.

---

### ✅ Backup Cron Silent Failure

**Problem:** Backup cron jobs fail silently — the only evidence is a missing log line. No alerting, no retry. Operator discovers the gap only after needing a restore.

**Fix:** Dead man's switch. Every backup script curls a heartbeat URL on success (`curl -fsS --retry 3`). If the ping doesn't arrive within the expected window, the SaaS (Healthchecks.io, Cronitor, etc.) fires an alert. Heartbeat URLs are injected via `secrets.env` with `[ -n "$URL" ] &&` guard so empty = no-op (safe for local dev).

- 5 Terraform variables (`heartbeat_url_nomad`, `heartbeat_url_postgres`, `heartbeat_url_victoriametrics`, `heartbeat_url_victorialogs`, `heartbeat_url_artifacts`) with `default = ""`.
- Variables passed through `templatefile()` in `nodes.tf` to cloud-init secrets.env.
- All 5 backup scripts (1 node, 4 NAT) curl their heartbeat URL as the final line after the success log entry. Scripts already use `set -euo pipefail`, so the heartbeat only fires on full success.

**Where:** `infra/terraform/main.tf` — variables. `infra/terraform/nodes.tf` — templatefile args. `infra/terraform/templates/cloud-init-node.yaml` and `cloud-init-nat.yaml` — secrets.env + backup scripts.

---

## 100 – 999 Users: First Real Load

*Concurrent deploys, real traffic, and multiple API instances become the norm. Race conditions and resource limits surface.*

---

### ✅ Secrets Rotation Mechanism

**Problem:** S3 credentials, `INTERNAL_SECRET`, NATS credentials, and the GCS backup SA key have no rotation mechanism. Rotating any secret requires redeploying all services that use it. If a secret is compromised, there's no way to rotate without downtime.

**Fix:** `INTERNAL_SECRET` now accepts a comma-separated list of valid secrets. During rotation, set `INTERNAL_SECRET=new-secret,old-secret` — both are accepted via constant-time comparison. After all callers update, remove the old value. S3 and NATS credential rotation is handled via Terraform + rolling restarts (infra-level, no code change needed).

**Where:** `crates/api/src/main.rs` (comma-split parsing), `crates/api/src/routes.rs` (`verify_internal_secret` checks all values).

---

### ✅ VictoriaLogs Query Escaping

**Problem:** `routes.rs` builds a VictoriaLogs query with `format!("task:{slug}")`. While slugs are restricted to alphanumeric + dashes by `slugify()`, there's no explicit escaping for VictoriaLogs LogsQL syntax. A slug containing LogsQL operators could inject query logic.

**Fix:** Slug is now double-quoted in the LogsQL query (`task:"{slug}"`) with embedded double quotes stripped. Combined with `slugify()` restricting input to alphanumeric + dashes, this is two layers of defense.

**Where:** `crates/api/src/routes.rs` — the VictoriaLogs query builder.

---

### ✅ Duplicate Outbox Processing (Multi-Instance API)

**Problem:** The current `SELECT * FROM outbox LIMIT 50` has no row locking. When two API instances run simultaneously, both pollers read the same rows, both publish to NATS, and the daemon receives duplicate `ProvisionEvent` messages. While the daemon should be idempotent on `service_id`, double-provisioning can leave orphaned TAP devices and Firecracker sockets if the second attempt runs against an already-running VM.

**Fix:** The outbox poller uses `FOR UPDATE SKIP LOCKED` inside a transaction. Each API instance locks only the rows it's processing — other instances skip locked rows entirely.

**Where:** `crates/api/src/outbox.rs:poll_once`.

---

### ✅ Postgres Connection Pool Exhaustion

**Problem:** Each API instance holds a `deadpool` pool capped at `max_size=16`. Under a burst of concurrent deploys (each holding a connection open for the full transaction + outbox insert duration), all 16 slots fill. New requests queue inside deadpool. If they queue longer than the default timeout, they return a 500. Under high concurrency this cascades — every request fails until the burst subsides.

**Fix:** All route handlers now acquire connections via `db_conn()`, which wraps `pool.get()` with a 5-second timeout. On timeout, returns `503 Service Unavailable` instead of queuing indefinitely. Pool size remains configurable via deadpool's `max_size`.

**Where:** `crates/api/src/routes.rs` — `db_conn()` helper used by all handlers.

---

### ✅ Presigned Upload URL Expiry for Large Artifacts

**Problem:** The API issues a 5-minute presigned S3 PUT URL. A large Metal rootfs image (500MB+) on a slow connection can exceed this window — the PUT returns `403 Forbidden` from Vultr Object Storage. The CLI surfaces a confusing auth error rather than "upload timed out, try again."

**Fix:** Presign expiry now branches on engine: 30 minutes for Metal (large rootfs images), 5 minutes for Liquid (small Wasm modules). Longer-term: switch to multipart upload for Metal rootfs.

**Where:** `crates/api/src/routes.rs:get_upload_url`.

---

### ✅ TAP Device Leak on Daemon Crash

**Problem:** If the daemon crashes between spawning a Firecracker VM and writing `tap_name` to the `services` table, the TAP device is left attached to `br0` but untracked.

**Fix:** Already implemented. On daemon startup, `cleanup_orphaned_taps()` reads `/sys/class/net/{bridge}/brif/` to enumerate all TAP devices on the bridge, cross-references against DB (`status IN ('running', 'provisioning')` with non-NULL `tap_name`), and deletes any untracked `tap*` device via `ip link del`. Gated to Linux via `#[cfg(target_os = "linux")]`.

**Where:** `crates/daemon/src/provision.rs` — `cleanup_orphaned_taps()`. Called from `crates/daemon/src/main.rs` at startup.

---

### ✅ Stripe Webhook Double Top-Up

**Problem:** If two webhook deliveries arrive simultaneously (Stripe retries on timeout), both can read the same state before either commits, double-crediting the customer.

**Fix:** Added `stripe_session_id` column to `credit_ledger` with a partial unique index (`WHERE stripe_session_id IS NOT NULL`). `handle_checkout_completed` now extracts the checkout session `id` and passes it to the ledger insert. On duplicate, the unique constraint fires `UNIQUE_VIOLATION`, the handler catches it, lets the transaction roll back (undoing the `topup_credits` bump), and returns `Ok(())` — standard Stripe idempotent webhook pattern.

**Where:** `migrations/V25__ledger_idempotency.sql`. `crates/api/src/billing.rs` — `handle_checkout_completed`.

---

### ✅ Wasm Execution Timeout

**Problem:** The Wasm HTTP shim had no wall-clock timeout. Infinite loops in user Wasm modules would block `spawn_blocking` threads indefinitely, eventually exhausting the blocking thread pool.

**Fix:** Wrapped the `spawn_blocking` call in `dispatch()` with `tokio::time::timeout`. Default 30s, configurable via `WASM_TIMEOUT_SECS` env var (read once at `serve()` startup). On timeout, the Wasmtime `Store` is dropped (killing execution) and the client receives `504 Gateway Timeout`. Fuel (1B units) still provides CPU-bound protection; the wall-clock timeout catches I/O waits and real-time stalls.

**Where:** `crates/daemon/src/wasm_http.rs` — `WasmService.timeout` field, `serve()` env var read, `dispatch()` timeout wrapper.

---

### ✅ Upstream Connection Refused — Stale Running Status

**Problem:** If a Firecracker VM crashes, the DB status remains `running` and the proxy routes to a dead upstream, returning 502s indefinitely.

**Fix:** Two complementary mechanisms:

1. **Daemon crash watcher** (`#[cfg(target_os = "linux")]`): Every 10s, checks if tracked Firecracker PIDs are still alive via `kill(pid, 0)`. Dead processes trigger: DB update to `status='crashed'` + clear `upstream_addr`, `RouteRemovedEvent` to evict proxy cache, `ServiceCrashedEvent` for observability, and full kernel resource cleanup (TAP, cgroup, CPU pin).

2. **Proxy cache eviction on connect failure**: Overrides Pingora's `fail_to_connect` hook. On any upstream connection error, the slug is evicted from the in-memory route cache. The next request does a fresh DB lookup, which reflects the `crashed` status (or cleared `upstream_addr`). `CTX` changed from `()` to `RequestCtx { slug }` to carry the resolved slug into the error hook.

New events: `ServiceCrashedEvent` (service_id, slug, exit_code) + `SUBJECT_SERVICE_CRASHED` in `common/events.rs`.

**Where:** `crates/daemon/src/main.rs` — VM crash watcher task. `crates/proxy/src/router.rs` — `fail_to_connect` + `RequestCtx`. `crates/common/src/events.rs` — `ServiceCrashedEvent`.

---

### ✅ Concurrent Stop and Deploy Race

**Problem:** A user runs `flux stop my-service` and immediately runs `flux deploy` for the same service. The stop handler publishes a `DeprovisionEvent` to NATS and sets `status='stopped'`. The deploy handler (running concurrently) sees status is not `running`, allows the deploy, inserts a new outbox row, and sets `status='provisioning'`. The deprovision event arrives at the daemon and kills the VM — but the provision event is already queued. The daemon provisions a new VM, but the service may end up in an inconsistent state if both events target the same service row.

**Fix:** The deploy handler's check transaction (under the workspace advisory lock) now queries for any active service with the same slug in a non-terminal state (`status IN ('running', 'provisioning')`). If found, returns 409 Conflict: "a service with this slug is currently running or provisioning — stop it first". This prevents the race at the API level — the user must wait for the stop to fully complete (status transitions to 'stopped') before redeploying. The daemon already uses `service_id` (not slug) for VM registry lookups, so even if both events are in-flight, they target different resources. The advisory lock serializes concurrent deploys for the same workspace, ensuring the check is atomic.

**Where:** `crates/api/src/routes.rs` — `deploy_service` handler, check transaction block.

---

### ✅ Billing Aggregation Crash — Double Billing

**Problem:** The billing aggregator runs in a transaction: read `usage_events` where `billed=false`, calculate costs, deduct from workspace balance, insert ledger entry, mark events `billed=true`, commit. If the API crashes between `commit` and the next aggregation cycle, the events are correctly billed. But if it crashes between the balance deduction and the ledger insert *within* the same transaction (unlikely but possible via Postgres connection loss mid-commit), the balance is deducted but the ledger has no record of why.

**Fix:** The aggregator already uses a single transaction, so partial commits can't happen. Added a defensive `verify_billing_consistency()` check that runs after each aggregation cycle: compares `SUM(ledger debits)` against computed costs from billed `usage_events` for each workspace. Logs a warning with exact drift on mismatch. Does not fail the cycle — purely observational so it can be wired to alerting.

**Where:** `crates/api/src/billing.rs` — `verify_billing_consistency()` called after `aggregate_once` commit.

---

### ✅ Route Publish Fire-and-Forget Loss

**Problem:** After provisioning a VM, the daemon publishes `RouteUpdatedEvent` to NATS as a fire-and-forget message (not via JetStream). If the daemon crashes or NATS is briefly unreachable between writing `status='running'` to the DB and the NATS publish, the proxy never learns the route. The service is "running" in the DB but unreachable — requests return 502 until someone manually triggers a route update.

**Fix:** Added periodic cache reconciliation in the proxy. Every 60s, `start_reconciler` queries `SELECT slug, upstream_addr FROM services WHERE status='running'` and diffs against the in-memory cache — adding missing routes and removing stale ones. This catches any route the NATS subscriber missed, regardless of cause. The NATS subscriber remains the primary path for sub-second updates; the reconciler is a safety net with a worst-case 60s convergence window.

**Where:** `crates/proxy/src/cache.rs` — `start_reconciler()`, `crates/proxy/src/main.rs`.

---

## 1,000 – 9,999 Users: Multi-Tenant Pressure

*Multiple busy workspaces, high deploy frequency, and log volume that starts to matter.*

---

### ✅ NATS Consumer Lag Under Burst Provisioning

**Problem:** A flash sale or viral moment sends 200 deploys in 30 seconds. The JetStream consumer queue depth grows faster than the daemon can process. Firecracker boot takes 100–250ms. Users see deploy queuing measured in minutes rather than seconds.

**Fix:** Concurrency cap is now tunable via `NODE_MAX_CONCURRENT_PROVISIONS` env var (default 8, was hardcoded 16). A 32-core node can set this higher; a 4-core dev box can set it to 2. Additionally, a consumer lag monitor task polls `consumer.info()` every 30s and warns when pending messages exceed 50 — gives operators a clear signal to scale out nodes or increase the concurrency cap.

**Where:** `crates/daemon/src/main.rs` — semaphore initialization + consumer lag monitor task.

---

### ✅ Advisory Lock Hot Workspace

**Problem:** The deploy handler acquires `pg_advisory_xact_lock(workspace_key)` before inserting a service, serializing all concurrent deploys for a single workspace. A CI/CD pipeline deploying 20 microservices in parallel turns a theoretically-parallel operation into a sequential one.

**Fix:** Split into two transactions: (1) a short `check_txn` that acquires the advisory lock, runs the capacity and service count checks, then commits (releasing the lock); (2) a separate `txn` for the soft-delete, INSERT, and outbox write (no lock held). The lock now serializes only the ~1ms checks, not the full deploy. Trade-off: between lock release and INSERT, another deploy could exceed the limit by 1 — acceptable since the limit is a soft gate, not a billing boundary.

**Where:** `crates/api/src/routes.rs` — `deploy_service` handler.

---

### ✅ Traffic Pulse NATS Flood

**Problem:** The proxy publishes `platform.traffic_pulse { slug }` debounced at 30s per slug. At 10,000 concurrent services, a proxy restart cold-clears the debounce cache, causing a burst of up to 10,000 simultaneous NATS publishes on first request per slug. The daemon's pulse handler then fires 10,000 individual `UPDATE services SET last_request_at` queries simultaneously.

**Fix:** Daemon pulse subscriber now batches: accumulates slugs in a `HashSet` over a 5s window, then flushes with a single `UPDATE services SET last_request_at = NOW() WHERE slug = ANY($1)`. A 10k burst collapses into ~1 query (all slugs deduplicated in the set). Proxy-side debounce (30s per slug) remains unchanged — the burst only happens on cold restart and the daemon absorbs it.

**Where:** `crates/daemon/src/main.rs` — traffic pulse subscriber, `tokio::select!` with flush ticker.

---

### ✅ Pulse Debounce Cache Unbounded Growth

**Problem:** The proxy's `pulse_cache` is a `HashMap<String, Instant>` that tracks the last time a traffic pulse was sent per slug. Entries are inserted on first request but never evicted. If services are created and deleted over time, the map accumulates entries for slugs that no longer exist. Over weeks/months on a long-running proxy, this is a slow memory leak.

**Fix:** Background sweeper thread runs every 5 minutes, evicting entries older than 2× the debounce interval (60s). Uses `retain()` under the write lock — O(n) but runs infrequently and the map is small relative to total services. Spawned from `main.rs` before Pingora starts accepting traffic.

**Where:** `crates/proxy/src/router.rs` — `start_pulse_sweeper()`, `crates/proxy/src/main.rs`.

---

### ✅ Hobby Capacity Check Race

**Problem:** The capacity check ran before the advisory lock, allowing two concurrent deploys to both pass and both insert — exceeding hobby-tier capacity.

**Fix:** Moved the capacity check (`SUM(memory_mb)` query) inside the transaction, after `pg_advisory_xact_lock`. Now the second concurrent deploy blocks on the lock, and when it runs the check it sees the first deploy's inserted row. The query also runs on `txn` instead of the plain `db` connection, so it reads within the transaction's snapshot.

**Where:** `crates/api/src/routes.rs` — `deploy_service` handler, capacity check now between advisory lock acquisition and service INSERT.

---

### ✅ Slug Reuse After Service Deletion

**Problem:** Stale route cache entries after slug reuse could route traffic to the old dead upstream.

**Fix:** This race cannot occur under the current schema. `services.slug` has a plain `UNIQUE NOT NULL` constraint (V1, line 169) — not a partial index on `deleted_at IS NULL`. A soft-deleted service still holds the unique lock on its slug, so no new service can claim the same slug until the row is hard-deleted. The described race requires two services with the same slug to exist simultaneously, which the DB prevents.

Additionally, the event ordering is already correct: deprovision publishes `RouteRemovedEvent` (cache evict) before provision publishes `RouteUpdatedEvent` (cache insert). Both use plain NATS on the same connection, which preserves publish order.

**Future consideration:** If slug reuse is needed, change the constraint to a partial unique index (`UNIQUE WHERE deleted_at IS NULL`). At that point, add a monotonic `route_version` column to `services` and include it in route events so the proxy can discard out-of-order updates.

**Where:** `migrations/V1__platform.sql` — `services.slug UNIQUE`. `crates/daemon/src/provision.rs` — `RouteUpdatedEvent` publish. `crates/daemon/src/main.rs` — `RouteRemovedEvent` publish on deprovision.

---

### ✅ VictoriaLogs Ingestion Backpressure

**Problem:** High-volume log writers could cause Promtail to buffer unboundedly and OOM, killing log ingestion for all services on the node.

**Fix:** Three layers of protection in Promtail config:

1. **Client backpressure**: `batchsize=1MiB`, `batchwait=3s`, `backoff_config` with exponential backoff (500ms→30s, max 10 retries). Caps per-push payload and prevents retry storms.

2. **Global rate limit**: `limits_config.readline_rate=5000` lines/sec with `readline_rate_drop=true`. Excess lines are dropped instead of buffered — prevents OOM under any log volume.

3. **Per-job pipeline rate limit**: `limit` stage on the `nomad` scrape job caps at 1000 lines/sec (burst 2000) with `drop=true`. Prevents a single noisy service from consuming the global budget.

**Where:** `infra/terraform/templates/cloud-init-node.yaml` — Promtail `clients`, `limits_config`, and `pipeline_stages.limit`.

---

### ✅ Cgroup Cleanup on VM Termination

**Problem:** Empty cgroup directories accumulate under `/sys/fs/cgroup/liquid-metal/` over thousands of provision/deprovision cycles, degrading kernel cgroup operations.

**Fix:** Two mechanisms:

1. **Per-VM cleanup** (already existed): `cgroup::cleanup(service_id)` is called in `deprovision::metal()` after killing the FC process. Uses `fs::remove_dir()` (not `_all()`) — only succeeds if the cgroup is empty (process exited).

2. **Startup sweep** (new): `cleanup_orphaned_cgroups()` runs on daemon startup, enumerates all directories under `/sys/fs/cgroup/liquid-metal/`, cross-references against DB (`status IN ('running', 'provisioning')`), and `rmdir`s any that don't belong to active services. Follows the same pattern as `cleanup_orphaned_taps()` and `cleanup_orphaned_artifacts()`.

**Where:** `crates/daemon/src/cgroup.rs` — `cleanup()`. `crates/daemon/src/provision.rs` — `cleanup_orphaned_cgroups()`. `crates/daemon/src/main.rs` — startup call.

---

## 10,000 – 99,999 Users: Serious Scale

*Single-node Postgres and a single Object Storage region start to show strain. Architectural changes are required.*

---

### ✅ Route Cache Cold-Start Storm

**Problem:** On Pingora restart, empty route cache causes all active slugs to miss simultaneously, firing thousands of parallel DB queries.

**Fix:** `cache::warm()` bulk-populates the route cache from a single DB query before the server accepts traffic. Runs synchronously in `main()` (own Tokio runtime) with a 5s timeout — if DB is slow/unreachable, falls back gracefully to on-demand population. Query: `SELECT slug, upstream_addr FROM services WHERE status = 'running' AND upstream_addr IS NOT NULL AND deleted_at IS NULL`.

**Where:** `crates/proxy/src/cache.rs` — `warm()`. `crates/proxy/src/main.rs` — called before `cache::start_subscriber()` and `server.run_forever()`.

---

### ✅ Migration Downtime on Large Tables

**Problem:** Blocking DDL migrations on large tables cause full outage for all queries touching that table.

**Fix:** Already mitigated by design on two fronts:

1. **Migrations are decoupled from API startup.** The API calls `migrations::verify()` (read-only version check), not `run()`. Migrations are applied separately via `nomad job dispatch migrate` or `api --migrate` before rolling out new API instances. This means a slow migration doesn't block API boot.

2. **All existing column additions are already safe.** Every `ADD COLUMN` in the migration history uses nullable or `DEFAULT` values (instant in Postgres 11+ — no table rewrite, no long lock). No `ALTER COLUMN SET NOT NULL` on existing large tables.

**Rules for future migrations:** (a) `ADD COLUMN` must be nullable or have a `DEFAULT` (instant). (b) `NOT NULL` constraints on backfilled columns use `ADD CONSTRAINT ... NOT VALID` + separate `VALIDATE CONSTRAINT` (ShareUpdateExclusiveLock, not AccessExclusive). (c) `CREATE INDEX CONCURRENTLY` requires running outside Refinery's transaction wrapper — use a separate `nomad job dispatch` step. (d) Never `ALTER COLUMN TYPE` on a large table without expand-contract.

**Where:** `crates/api/src/migrations.rs` — `verify()` on startup, `run_with_url()` for explicit migration runs. `migrations/` — all existing DDL follows safe patterns.

---

### ✅ Outbox Table Bloat Under High Deploy Frequency

**Problem:** The outbox table has rapid INSERT/DELETE churn (rows live <1s). Default autovacuum can't keep up at high deploy frequency, causing dead-tuple accumulation and index bloat.

**Fix:** Migration sets table-level autovacuum overrides: `scale_factor=0.01` (vacuum at 1% dead rows vs default 20%), `cost_delay=0` (no throttling — outbox is tiny). This ensures autovacuum runs frequently and completes fast for this specific table without affecting other tables' vacuum behavior.

**Where:** `migrations/V26__outbox_autovacuum.sql`.

---

### ✅ Postgres max_connections Ceiling

**Problem:** Vultr Managed Postgres caps `max_connections` by plan (100–400). Multiple API/proxy/daemon instances can exhaust the limit, cascading into 500s.

**Fix:** Already implemented. PgBouncer runs on the NAT VPS in transaction-pooling mode (`:6432`). Config: `max_client_conn=400`, `default_pool_size=20`, `min_pool_size=5`, `reserve_pool_size=5`, `server_tls_sslmode=require`. Terraform outputs both `database_url` (direct Postgres) and `pgbouncer_url` (`:6432`). All Nomad job files are templated to read `DATABASE_URL` from Nomad variables — operator sets it to the PgBouncer URL. Application code uses `DATABASE_URL` with no protocol awareness, so no code changes needed.

**Where:** `infra/terraform/templates/cloud-init-nat.yaml` — PgBouncer config template + systemd. `infra/terraform/outputs.tf` — `pgbouncer_url` output. `infra/nomad/*.nomad.hcl` — `DATABASE_URL` templating.

---

## 100,000 – 999,999 Users: Hyper Scale

*Single-region, single-database architecture hits fundamental limits. Platform topology must change.*

---

### 📋 Single-Region Object Storage Latency

**Problem:** All artifacts are stored in Vultr Chicago (`ord1.vultrobjects.com`). Daemon nodes in other regions downloading base images and user artifacts add 50–200ms to every cold-start. At 100k+ users across multiple geographies, this compounds into a meaningful SLA gap vs. providers with CDN-backed artifact distribution.

**Fix (planned):** Two layers: (1) Enable Vultr Object Storage CDN for the artifact bucket — this adds a global CDN edge with no architectural change. (2) Pre-cache base images (`alpine-3.20-amd64.ext4`) on each Metal node at provisioning time via cloud-init so the daemon never fetches the base image over the network — only the per-deploy user artifact.

---

### 📋 NATS Cluster Throughput Ceiling

**Problem:** The 3-node JetStream cluster handles all subjects including high-volume fire-and-forget events (`traffic_pulse`, `route_updated`, `route_removed`). At 100k+ active services the aggregate message rate approaches the throughput ceiling of a 3-node Raft cluster.

**Fix (planned):** Separate transport by durability requirement. Only `platform.provision` and `platform.deprovision` need JetStream persistence. Move `traffic_pulse`, `route_updated`, and `route_removed` to a plain NATS cluster (no Raft, no persistence) with a separate set of subjects. The durability cluster handles low-volume, high-importance events; the plain cluster handles high-volume, best-effort events.

---

### 📋 Postgres Write Scalability

**Problem:** A single Postgres primary handles all writes. At 100k users with high deploy frequency, the primary WAL throughput becomes the platform-wide ceiling. Vultr Managed Postgres offers read replicas but not write sharding.

**Fix (planned):** Two-phase: (1) Short-term — move read-heavy queries (service listing, workspace lookups) onto read replicas via a separate `read_pool` in `AppState`. (2) Long-term — shard high-write tables (`outbox`) by workspace ID onto separate Postgres instances, or migrate to a horizontally scalable store.

---

### 📋 Managed Postgres Failover — Connection Storm

**Problem:** Vultr Managed Postgres performs automatic failover on primary failure. The failover promotes a replica, which gets a new internal IP. All existing `deadpool` connections in the API, daemon, and proxy become invalid simultaneously. Every in-flight query fails. The pools attempt to reconnect, but all instances hit the new primary at the same instant — a thundering herd that may exceed `max_connections` on the fresh primary and cause cascading 500s for 10–30 seconds.

**Fix (planned):** Configure deadpool's `recycling_method` to test connections before use (`RecyclingMethod::Verified`). Add exponential backoff with jitter on pool reconnection. Use PgBouncer (see max_connections entry) as an intermediate layer that absorbs the reconnection storm and serializes new connections to Postgres.

**Where:** `crates/common/` — pool configuration. `infra/` — PgBouncer deployment.

---

### 🔧 Multi-Region Expansion

**Problem:** All compute, the NATS cluster, and Postgres are in a single Vultr Chicago datacenter. Any regional Vultr outage takes down the entire platform. Users in EU and APAC experience 80–150ms baseline latency on all API calls.

**Fix (infra-only):** Expand to a second Vultr region (Amsterdam or Tokyo) with its own Metal/Liquid nodes, a regional NATS cluster replicating durable subjects from Chicago, and a Postgres read replica. A NATS supercluster (hub-and-spoke) federates the two regions without requiring global Raft consensus on every message. The API and Proxy route deploys to the nearest region based on `X-Region` header or workspace preference.

**When:** When a customer explicitly requires a specific region or SLA demands < 50ms API latency globally.
