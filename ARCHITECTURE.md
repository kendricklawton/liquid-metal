# Liquid Metal — Architecture

> **Status**: Beta — running on Hivelocity bare metal in Dallas (DAL1).

---

## Infrastructure Stack

All infrastructure runs in **Hivelocity Dallas (DAL1)**. Four dedicated bare metal nodes on a private VLAN — no shared resources, no managed services. Backups replicate to GCP (GCS) for cross-cloud disaster recovery.

### Node Inventory

| Node | Hardware | Cost | Role |
|------|----------|------|------|
| **gateway** | E3-1230 v6 3.5GHz Kaby Lake, 4c/8t, 32GB RAM, 960GB SSD, 20TB/1Gbps | $68/mo | Public ingress, API, NATS, observability |
| **node-metal** | EPYC 7452 2.35GHz, 32c/64t, 384GB RAM, 1TB NVMe, 20TB/10Gbps | $215/mo | Firecracker microVMs — tenant Metal workloads |
| **node-liquid** | E3-1230 v6 3.5GHz Kaby Lake, 4c/8t, 32GB RAM, 960GB SSD, 20TB/1Gbps | $68/mo | Wasmtime/WASI execution |
| **node-db** | E3-1230 v6 3.5GHz Kaby Lake, 4c/8t, 32GB RAM, 960GB SSD, 20TB/1Gbps | $68/mo | Postgres 16 + PgBouncer |

**Total: ~$419/mo** — fully self-hosted, one provider, one bill, one portal.

### Why These Boxes

- **gateway** — I/O bound, not compute bound. 32GB gives breathing room for NATS JetStream, VictoriaMetrics, VictoriaLogs, and Grafana running side by side. 4 cores is plenty — everything is async/event-driven.
- **node-metal** — The revenue machine. 64 threads = 60 sellable vCPUs (4 reserved for host). 384GB RAM. 1TB NVMe. Each Firecracker VM gets a dedicated thread pinned via cgroup cpuset — no noisy neighbors, no overcommit.
- **node-liquid** — Wasmtime is lightweight. Each invocation uses a few MB of memory and a fraction of a millisecond of CPU. This box could handle thousands of concurrent invocations before breaking a sweat. 32GB is future-proofing.
- **node-db** — Postgres performance is about RAM. 32GB means ~8GB shared_buffers + 24GB OS page cache. The dataset will live in memory for a long time. 4 cores is fine — Postgres is single-threaded per query, PgBouncer keeps connection count low.

### Service Layer

| Layer                 | Technology                                          | Notes                                                                                                |
|-----------------------|-----------------------------------------------------|------------------------------------------------------------------------------------------------------|
| **Gateway node**      | Hivelocity E3-1230 v6 — Dallas DAL1                 | Public IP, HAProxy + Pingora + API + Web + NATS + observability                                      |
| **Compute (Metal)**   | Hivelocity EPYC 7452 — Dallas DAL1                  | node-metal — Firecracker microVMs, KVM isolation, 60 sellable vCPUs (threads)                        |
| **Compute (Liquid)**  | Hivelocity E3-1230 v6 — Dallas DAL1                 | node-liquid — Wasmtime/WASI execution                                                                |
| **Database**          | Hivelocity E3-1230 v6 — Dallas DAL1                 | node-db — self-hosted Postgres 16 (8GB shared_buffers) + PgBouncer (:6432)                           |
| **Proxy**             | Pingora (Rust) on gateway node                      | Slug → upstream routing, :8443 (HAProxy forwards to localhost:8443)                                  |
| **Load balancer**     | HAProxy on gateway node                             | TLS termination (Let's Encrypt), rate limiting, health-checks, :80/:443                              |
| **Internal network**  | Hivelocity private VLAN          Not done / discussed but not implemented:
CLAUDE.md recreation (you updated it manually but the Infrastructure section still shows old E-2136 specs)
Changelog page (template exists at changelog.html but no content system)
OpenStatus integration (decided on it, didn’t set up)
Vault integration (plan exists at .claude/plans/, not implemented)
Blue-green deploy (added to TODO.md as future work)
TODO.md with the 5-year roadmap (discussed, may need updating)
Workload Identity Federation for GitHub Actions
gcloud auth application-default login for local dev (just a command to run)
Web content readability improvements (started with font, more may be needed)
What do you want to tackle next?                   | Unmetered internal traffic — DB, NATS, daemon ↔ API all on VLAN                                     |
| **Ops access**        | Tailscale                                           | SSH access to all nodes from operator laptops — no public port 22                                    |
| **API**               | Axum (Rust), :7070                                  | Single instance on gateway node                                                                      |
| **Web**               | Axum + Askama + HTMX (Rust), :3000                  | Dashboard — server-rendered HTML, OIDC browser auth                                                  |
| **Daemon**            | NATS consumer (Rust)                                | Each node owns its engine — Metal runs Firecracker, Liquid runs Wasmtime                             |
| **CLI**               | `flux` binary (Rust)                                | auth, service (deploy/stop/restart/delete/logs/env/domains/scale/rollback), init, billing, workspace, project, invite |
| **Secret management** | HashiCorp Vault (Raft storage, GCP KMS auto-unseal) | KV v2 — user env vars, TLS certs, internal secrets. Audit log on every access.                       |
| **Event bus**         | NATS JetStream                                      | Single server on gateway node, JetStream persistence to disk                                         |
| **Artifact store**    | Wasabi Object Storage (S3-compatible via Hivelocity) | $6.99/TB/mo, no egress fees — rootfs images + .wasm binaries                                        |
| **DNS**               | Cloudflare                                          | Wildcard + apex → gateway node public IP                                                             |
| **TLS**               | Let's Encrypt (certbot + Cloudflare DNS-01)         | Wildcard cert on HAProxy, auto-renewed via cron                                                      |
| **eBPF isolation**    | Aya TC classifier per tap{n}                        | Tenant isolation at kernel level, no Cilium daemon                                                   |
| **Observability**     | VictoriaMetrics + VictoriaLogs + Grafana            | Metrics scraping (:8428), log aggregation (:9428), dashboards (:3001)                                |
| **Log shipping**      | Promtail on every node                              | Tails Nomad allocation logs + systemd journal → VictoriaLogs                                         |
| **Node metrics**      | node_exporter on every node                         | Hardware metrics (:9100) scraped by VictoriaMetrics                                                  |
| **Connection pooling**| PgBouncer on node-db                                | Transaction-mode pooler (:6432), all services connect here                                           |
| **Backups**           | GCS (buckets) + pg_dump + rclone                    | Postgres (from node-db), VictoriaMetrics, VictoriaLogs, Nomad, Wasabi artifacts → GCP daily          |
| **Infra provisioning**| Terraform (Hivelocity + Cloudflare + Tailscale + Google) | Everything declared as code in `infra/terraform/`                                               |
| **Process scheduling**| Nomad (HashiCorp)                                   | Schedules api/proxy on gateway, daemons on compute nodes — no K8s                                   |
| **CI/CD**             | GitHub Actions                                      | ci.yml (check/test), release.yml (cargo-dist), deploy.yml (Nomad job update)                         |
---

## Topology

### Node Layout

```
Internet
    │
    ▼ DNS: *.domain + domain → gateway public IP (Cloudflare, not proxied)
┌────────────────────────────────────────────────────────────┐
│  gateway — E3-1230 v6, 4c/8t, 32GB — Hivelocity DAL1      │
│  HAProxy :80/:443 (TCP pass-through for 443)               │
│  Pingora :8443 (TLS termination, SNI routing)              │
│  API :7070 · Web :3000                                     │
│  NATS :4222 (JetStream) · Vault :8200 (KV v2)              │
│  Nomad server + client (node_class=gateway)                │
│  VictoriaMetrics :8428 · VictoriaLogs :9428 · Grafana :3001│
└──────────────────────┬─────────────────────────────────────┘
                       │ Hivelocity private VLAN (unmetered)
    ┌──────────────────┼──────────────────┬──────────────────┐
    │                  │                  │                  │
  node-metal        node-liquid        node-db
  EPYC 7452         E3-1230 v6         E3-1230 v6
  32c/64t, 384GB    4c/8t, 32GB        4c/8t, 32GB
  $215/mo           $68/mo             $68/mo
    │                  │                  │
  Daemon             Daemon            Postgres 16
  Nomad client       Nomad client      PgBouncer :6432
  Promtail           Promtail          Promtail
  node_exporter      node_exporter     node_exporter
  KVM + br0 + TAP   Wasmtime          pg_dump → GCS
  Firecracker
  eBPF TC isolation
```

### How a Request Flows (whiteboard version)

```
User → DNS (Cloudflare) → gateway public IP
  → HAProxy :443 (TCP pass-through)
    → Pingora :8443 (TLS termination, reads SNI hostname)
      → slug lookup (in-memory cache, backed by Postgres)
        → Metal:  proxy to 172.16.x.x:{port} on node-metal (Firecracker VM, always running)
        → Liquid: proxy to 127.0.0.1:{port} on node-liquid (Wasmtime shim, may need cold start)
```

### How a Deploy Flows (whiteboard version)

```
Developer → flux deploy (CLI)
  → builds binary locally (go build / cargo build / zig build)
  → uploads artifact to Wasabi S3 (pre-signed URL from API)
  → POST /services to API (:7070)
    → API writes to Postgres (INSERT services + INSERT outbox, single transaction)
    → outbox poller picks up event, publishes to NATS JetStream
      → daemon on node-metal or node-liquid receives ProvisionEvent
        → Metal:  download binary → inject into rootfs → create TAP → attach eBPF → boot Firecracker VM → health check → publish RouteUpdatedEvent
        → Liquid: download .wasm → compile with Wasmtime → bind HTTP shim → health check → publish RouteUpdatedEvent
      → proxy receives RouteUpdatedEvent → caches slug → upstream_addr mapping
  → service is live at slug.domain
```

DNS points at the gateway node's static public IP. HAProxy on `:443` does TCP pass-through to Pingora on `:8443`, which terminates TLS with SNI-based cert selection. All internal traffic (API→NATS, daemon→API, services→Postgres) travels over the Hivelocity private VLAN — unmetered, never touching the public internet. Tailscale is used for operator SSH access only.

### Availability

This is a single-region, no-redundancy setup appropriate for beta. Each layer has one instance.

| Layer | Strategy | Recovery |
|-------|----------|----------|
| **Public IP** | Static IP on gateway node | Manual DNS update if node replaced |
| **HAProxy + Pingora** | Single instance on gateway | Restart via Nomad systemd |
| **API + NATS** | Single instance on gateway | Restart via Nomad |
| **Postgres** | Single instance on node-db, daily GCS backup | Restore from backup (~RTO 30min) |
| **Object Storage** | Wasabi — managed redundancy | N/A |
| **Metal/Liquid daemons** | Single instance per node | Restart via Nomad — running VMs survive daemon restart |
| **Backups** | Daily crons to GCS | N/A |

> Running Firecracker VMs and Wasm modules survive a daemon restart — the daemon re-registers them from the database on startup. A gateway node failure interrupts new deploys and control-plane operations but does not kill running tenant workloads.

### NATS JetStream

Single-node NATS server on the gateway with JetStream enabled for durable stream persistence. Listens on `:4222` (Tailscale/VLAN only — blocked from public by UFW).

```
# /etc/nats/nats.conf on gateway
listen: 0.0.0.0:4222

jetstream {
  store_dir: /var/lib/nats/jetstream
  max_mem: 256MB
  max_file: 4GB
}
```

Both compute nodes connect to this single NATS server via the private VLAN. JetStream persistence ensures events survive a NATS restart.

### Network Architecture

```
Internet
    │
    ▼ (*.domain + domain → gateway public IP)
gateway — Hivelocity DAL1
    HAProxy :80/:443 (TCP pass-through for 443)
    Pingora :8443 (TLS termination, SNI cert selection)
    API :7070, Web :3000, Nomad server + client (gateway)
    NATS :4222
    VictoriaMetrics :8428, VictoriaLogs :9428, Grafana :3001
    │
    │ Hivelocity private VLAN (unmetered — no public internet)
    ├──────────────────────┬──────────────────┐
    │                      │                  │
  node-metal           node-liquid         node-db
  Daemon               Daemon              Postgres 16
  Nomad client         Nomad client        PgBouncer :6432
  Promtail → VLogs     Promtail → VLogs    Promtail → VLogs
  node_exporter :9100  node_exporter :9100 node_exporter :9100
  KVM+br0+TAP          Wasmtime
    │                      │
    └──────────────────────┘
               │
    Wasabi Object Storage (S3-compatible, $6.99/TB, no egress fees)
    ├── rootfs images + .wasm binaries
    ├── backed up to GCS via rclone
```

**Internal network (Hivelocity private VLAN):**
- All four nodes share a private VLAN — traffic is unmetered and never leaves the datacenter
- All inter-service traffic (API→NATS, daemon→NATS, services→PgBouncer→Postgres) uses VLAN IPs
- Provisioned via Terraform (`hivelocity_vlan` resource)

**Ops access (Tailscale):**
- Tailscale installed on all nodes for operator SSH access from laptops
- `tailscale ssh node-metal` — no public port 22 on any node
- Does NOT carry production traffic — VLAN handles all inter-node communication

**Exposed ports (gateway only — all other nodes: public firewall closed):**
- `:80` — HAProxy (HTTP → Pingora for ACME challenges + HTTPS redirect)
- `:443` — HAProxy (TCP pass-through → Pingora TLS termination)

**SSH hardening (all nodes):**
- fail2ban (5 retries, 1hr ban)
- Password auth disabled, key-only root, X11 forwarding disabled

---

## eBPF Tenant Isolation (Aya)

> Applies to **Metal tier only** (node-metal). Liquid nodes run Wasmtime in-process — no TAP devices, no bridge, no eBPF needed.

### Why — The Multi-Tenant Bridge Problem

All Firecracker VMs on a Metal node share the same `br0` bridge. Without enforcement,
VM A can send packets directly to VM B's TAP IP (172.16.x.x). This is a
cross-tenant security hole.

The solution is a TC (Traffic Control) eBPF classifier attached to each VM's
`tap{n}` device at provision time. It runs inside the Linux kernel — the packet
never leaves the kernel stack, and it cannot be bypassed from inside the VM.

### Stack — No Kubernetes, No Cilium Daemon

| Component                       | Role                                                                        |
|---------------------------------|-----------------------------------------------------------------------------|
| `crates/ebpf-programs/`         | Kernel-side: TC classifier compiled to BPF bytecode (`bpfel-unknown-none`)  |
| `crates/daemon/src/ebpf.rs`     | Userspace: Aya loader — attaches/detaches programs per TAP                  |
| `crates/daemon/build.rs`        | Compiles `ebpf-programs` at daemon build time, embeds via `include_bytes_aligned!` |

No external daemon. No Cilium CLI. No CNI plugin. The compiled BPF object is
embedded directly in the daemon binary and loaded into the kernel at runtime
using [Aya](https://aya-rs.dev/).

### What the TC Classifier Does

```
VM tap{n} egress hook (tc_egress in ebpf-programs/src/main.rs)

for every packet leaving the VM:
  if ethertype != IPv4 → TC_ACT_OK (pass ARP, IPv6, etc.)
  read dst_ip from IPv4 header
  if dst_ip & 0xFFF00000 == 172.16.0.0 → TC_ACT_SHOT (drop — other VM)
  else → TC_ACT_OK (pass — internet, gateway, DNS)
```

The `172.16.0.0/12` range covers all possible guest IPs assigned by the
`guest_ip()` function in `provision.rs`. A VM legitimately never needs to
address another VM directly — all inter-service traffic must go through
the Pingora proxy.

### Bandwidth (tc.rs) + Isolation (ebpf.rs) Coexist

```
tap{n} egress:
  1. tbf qdisc (tc.rs)       → rate limit to net_egress_kbps
  2. TC classifier (ebpf.rs) → drop if dst is another VM
  3. packet exits to br0 → NAT → internet
```

### Lifecycle

```
provision_metal():
  netlink::create_tap(tap)           # create tap{n}
  netlink::attach_to_bridge(tap)     # join br0
  tc::apply(tap, quota)              # bandwidth qdiscs
  ebpf::attach(tap, service_id)      # TC isolation classifier ← Aya

deprovision:
  ebpf::detach(tap)                  # unload BPF program
  tc::remove(tap)                    # remove qdiscs
  netlink::remove_tap(tap)           # delete tap{n}
```

### TAP IPAM — Index Recycling

Each `tap{n}` device gets an index from a recycling pool tracked by a `Mutex<BTreeSet<u32>>` in the daemon. On startup, `init_tap_pool()` loads all currently-allocated indices from the database and builds the set of free indices:

```sql
SELECT tap_name FROM services
WHERE node_id = $1 AND status = 'running' AND engine = 'metal'
  AND deleted_at IS NULL AND tap_name IS NOT NULL
```

`allocate_tap_index()` pops the lowest available index from the free set (or extends past the current max if none are free). `release_tap_index()` returns an index to the pool on deprovision. This replaces the earlier monotonic `AtomicU32` counter which could only grow — leaked indices from deleted services were never reclaimed.

### Build Requirements (Linux only)

```bash
rustup target add bpfel-unknown-none
rustup component add rust-src
cargo build -p daemon
```

On macOS, the eBPF build is skipped entirely. `#[cfg(target_os = "linux")]`
gates ensure the embedded bytes are never referenced in a macOS binary.

---

## Data Flow

### NATS Event Reference

All events live in `crates/common/src/events.rs`. JetStream events are durable (at-least-once delivery). Fire-and-forget events are plain NATS publishes — no ack, best-effort.

| Subject                         | Type | Transport       | Publisher       | Consumer          |
|---------------------------------|------|-----------------|-----------------|-------------------|
| `platform.provision`            | `ProvisionEvent`       | JetStream       | API outbox poller | Daemon
| `platform.deprovision`          | `DeprovisionEvent`     | JetStream       | API               | Daemon
| `platform.route_updated`        | `RouteUpdatedEvent`    | Fire-and-forget | Daemon            | Proxy (all instances)
| `platform.route_removed`        | `RouteRemovedEvent`    | Fire-and-forget | Daemon            | Proxy (all instances)
| `platform.traffic_pulse`        | `TrafficPulseEvent`    | Fire-and-forget | Proxy             | Daemon
| `platform.usage_metal`          | `MetalUsageEvent`      | Fire-and-forget | Daemon            | API
| `platform.usage_liquid`         | `LiquidUsageEvent`     | Fire-and-forget | Daemon            | API
| `platform.service_crashed`      | `ServiceCrashedEvent`  | Fire-and-forget | Daemon            | API
| `platform.suspend`              | `SuspendEvent`         | JetStream       | API               | Daemon
| `platform.deploy_progress.{id}` | `DeployProgressEvent`  | Fire-and-forget | Daemon            | API (SSE stream)
| `platform.cert_provisioned`     | `CertProvisionedEvent` | Fire-and-forget | API cert_manager  | Proxy (all instances)

`DeprovisionEvent` carries `slug` so the daemon can publish `RouteRemovedEvent` without a DB lookup. `DeployProgressEvent` carries a `DeployStep` enum (`Queued`, `Downloading`, `Verifying`, `Booting`/`Starting`, `HealthCheck`, `Running`, `Failed`) and a human-readable message. The `FailureKind` enum (`Transient`, `Permanent`) determines whether the daemon retries (max 3 attempts with 15s/30s backoff) or ACKs immediately.

### Transactional Outbox

`platform.provision` events are delivered via the **transactional outbox pattern** (`crates/api/src/outbox.rs`, `migrations/V16__outbox.sql`). This eliminates the race where a DB commit succeeds but the NATS publish fails, leaving a service stuck in `provisioning` forever.

```
deploy handler (single DB transaction):
  INSERT INTO services (...)
  INSERT INTO outbox (subject='platform.provision', payload={...})
  COMMIT   ← both rows land, or neither does

outbox poller (background task, 1s interval):
  BEGIN
  SELECT * FROM outbox ORDER BY created_at ASC LIMIT 50 FOR UPDATE SKIP LOCKED
  for each row:
    js.publish(subject, payload)  ← JetStream ack awaited
    DELETE FROM outbox WHERE id = $1
  COMMIT
```

If NATS is temporarily unreachable the publish returns an error — the row is not deleted and will be retried next poll. If the API crashes mid-publish, the row survives the restart. The daemon's provisioning logic is idempotent on `service_id`.

### Deploy

Two models, one deploy command. Metal is **dedicated** — the VM runs 24/7. Liquid is **serverless** — the Wasm shim scales to zero on idle and wakes in <10ms on the next request. No vCPU/RAM configuration, no Dockerfiles, no infrastructure decisions.

```
flux deploy
  1. read liquid-metal.toml → engine, name, build command
  2. build locally:
       metal:  cargo build --target x86_64-unknown-linux-musl --release
       liquid: cargo build --target wasm32-wasip1 --release
  3. sha256(artifact) + generate deploy_id (uuid_v7)
  4. GET /upload-url → API returns pre-signed Wasabi S3 PUT URL
  5. PUT artifact → Wasabi directly (no API in the upload path)
  6. POST /services { slug, engine, artifact_key, deploy_id, sha256 }
       → API: advisory lock + INSERT services + INSERT outbox
       → COMMIT (atomic)
       → outbox poller publishes ProvisionEvent → NATS JetStream

NATS → daemon (provision)
  ├─ metal:  download base-alpine.ext4 template (cached locally)
  │           download user binary from Wasabi S3
  │           inject binary + env vars into rootfs (loop mount)
  │           create TAP, attach to bridge, apply eBPF TC filter + tc bandwidth
  │           spawn Firecracker (direct or via jailer) → apply cgroup limits
  │           configure + boot VM via Firecracker REST API
  │           startup probe (HTTP GET / until any response)
  │           register VmHandle in-memory (for crash watcher + deprovision)
  │           UPDATE services SET status='running', upstream_addr, tap_name, fc_pid, vm_id
  │           publish RouteUpdatedEvent → NATS
  │           VM stays running permanently until user deletes the service
  └─ liquid: download main.wasm from Wasabi S3
             verify SHA-256 integrity
             compile Wasmtime module (cached to disk — <10ms deserialize on wake)
             save metadata.json (app_name, env_vars) for scale-to-zero recovery
             bind HTTP shim on random port (WAGI per-request dispatch)
             startup probe (HTTP GET / to verify shim responds)
             register in LiquidRegistry (for billing usage reporting)
             UPDATE services SET status='running', upstream_addr=127.0.0.1:{port}
             publish RouteUpdatedEvent → NATS
             Wasm shim runs until idle timeout (LIQUID_IDLE_TIMEOUT_SECS, default 300s)

On failure:
  → classify error as Transient or Permanent (FailureKind)
  → UPDATE services SET status='failed', failure_reason=<error>, provision_attempts += 1
  → Permanent (SHA mismatch, startup probe timeout, invalid ELF): ACK — no retry
  → Transient (S3 timeout, DB blip): NAK with backoff (15s, 30s) — max 3 attempts
```

After deploy, the service is `running` with a live `upstream_addr`. Metal VMs stay alive permanently. Liquid shims scale to zero after 5 minutes of inactivity and wake on the next request.

### Live Request

```
Browser → Pingora
  └─ slug lookup
      ├─ cache hit (warm)  → upstream_addr known → proxy to backend
      └─ cache miss        → DB lookup
          ├─ metal (always warm) → upstream_addr set → proxy to backend
          └─ liquid
              ├─ warm (upstream_addr set) → proxy to backend
              └─ cold (status='stopped', no upstream_addr)
                  → publish WakeEvent { service_id, slug, engine=Liquid }
                  → hold the request (queue with timeout)
                  → daemon deserializes cached Wasm module (<10ms)
                  → daemon starts HTTP shim, publishes RouteUpdatedEvent
                  → proxy receives event, forwards held request

  ├─ metal:  proxy → TAP → Firecracker VM (always running)
  └─ liquid: proxy → 127.0.0.1:{port} → Wasm HTTP shim → fresh Wasmtime instance per request
```

The route cache (`crates/proxy/src/cache.rs`) is an `Arc<RwLock<HashMap<String, String>>>` kept warm by a background NATS subscriber:

| Subject                  | Event                                   | Cache action         |
|--------------------------|----------------------------------------|----------------------|
| `platform.route_updated` | Service provisioned, restarted, or woke | `insert(slug, addr)` |
| `platform.route_removed` | Service stopped, deleted, or suspended  | `remove(slug)`       |

No Redis — in-process, kilobytes of data, DB is authoritative on restart.

### Liquid Scale to Zero

Liquid services are serverless — they scale to zero after `LIQUID_IDLE_TIMEOUT_SECS` (default 300s = 5 minutes) of no traffic, and wake on the next request.

```
Idle checker (daemon, every 60s):
  → query liquid services WHERE last_request_at < NOW() - liquid_idle_timeout
  → for each idle Liquid service:
      remove from LiquidRegistry (drops Wasm shim + listener, frees memory)
      UPDATE services SET status='stopped', upstream_addr=NULL
      publish RouteRemovedEvent → proxy evicts from cache
      artifacts stay on disk: main.wasm, .compiled cache, metadata.json

First request to a cold Liquid service:
  → proxy detects status='stopped', engine='liquid'
  → publish WakeEvent → NATS
  → hold the request with timeout
  → daemon reads metadata.json (app_name, env_vars)
  → daemon deserializes cached .compiled module (<10ms vs 60-180s full compile)
  → daemon calls wasm_http::serve() → bind port → register in LiquidRegistry
  → UPDATE services SET status='running', upstream_addr=127.0.0.1:{port}
  → publish RouteUpdatedEvent → proxy unblocks held request
```

The user never sees this. From their perspective, the URL always works. Cold starts add <50ms of latency (module deserialize + port bind + route event).

### Metal Scale to Zero (future)

> Not active in the current dedicated model. Metal VMs run 24/7. The infrastructure (snapshot/wake in `wake.rs`, `snapshot.rs`) exists for a future serverless Metal tier if needed. Set `IDLE_TIMEOUT_SECS > 0` to enable.

### Stop / Delete

```
flux stop <id>  (or flux delete <id>)
  → POST /services/:id/stop
  → API: UPDATE services SET status='stopped', upstream_addr=NULL
  → API publishes DeprovisionEvent → NATS JetStream

NATS → daemon
  ├─ metal:  remove VmHandle from registry
  │           SIGTERM → wait 500ms → SIGKILL the Firecracker process
  │           detach eBPF TC filter, remove tc qdiscs
  │           delete TAP device, cleanup cgroup
  │           remove jailer chroot (if jailed)
  │           delete local artifact cache
  └─ liquid: remove from LiquidRegistry
  │           delete local artifact cache (wasm module + compiled cache)
  │           (HTTP shim stops when the daemon restarts — harmless until then)
  │
  └─ both:   publish RouteRemovedEvent → proxy evicts from cache
```

After stop, the service has no running instance. `flux deploy` is required to create a new one.

---

## Observability

All observability services run on the gateway node. Data is stored on the local 960GB SSD at `/mnt/observability`. All ports are Tailscale-only — blocked from public by UFW.

| Service              | Port             | Purpose                                                                                      |
|----------------------|------------------|----------------------------------------------------------------------------------------------|
| **VictoriaMetrics**  | :8428            | Time-series database. Scrapes node_exporter on all 4 nodes every 15s. 30-day retention       |
| **VictoriaLogs**     | :9428            | Log aggregation. Accepts Loki push protocol (`/insert/loki/api/v1/push`). 30-day retention   |
| **Grafana**          | :3001            | Dashboards and alerting. VictoriaMetrics as Prometheus datasource, VictoriaLogs via plugin    |
| **Promtail**         | :9080 (per node) | Log shipper on every node. Tails Nomad allocation logs + systemd journal                     |
| **node_exporter**    | :9100 (per node) | Hardware metrics from /proc and /sys on every node                                           |

### Provisioned Alerts (Grafana)

| Alert                    | Condition                              | For  |
|--------------------------|----------------------------------------|------|
| Node Down                | `up{job="node_exporter"} == 0`         | 2m   |
| Disk Space Critical      | Root filesystem below 10% free         | 5m   |
| High CPU                 | CPU usage above 90% sustained          | 10m  |
| High Memory              | Memory usage above 90%                 | 5m   |

## Backups

All cluster data replicates daily to GCP (GCS) for cross-cloud disaster recovery. Each data type has its own bucket with lifecycle policies.

| Data                 | Runs on    | Schedule   | Destination                                    | Retention                                |
|----------------------|------------|------------|------------------------------------------------|------------------------------------------|
| **Vault**            | gateway    | 02:30 UTC  | `{project}-lm-{env}-vault-backups`             | 30d Standard → Nearline, 90d delete      |
| **Postgres**         | node-db    | 03:00 UTC  | `{project}-lm-{env}-postgres-backups`          | 30d Standard → Nearline, 90d delete      |
| **VictoriaMetrics**  | gateway    | 03:30 UTC  | `{project}-lm-{env}-victoriametrics-backups`   | 30d Standard → Nearline, 90d delete      |
| **VictoriaLogs**     | gateway    | 04:00 UTC  | `{project}-lm-{env}-victorialogs-backups`      | 30d Standard → Nearline, 90d delete      |
| **S3 Artifacts**     | gateway    | 04:30 UTC  | `{project}-lm-{env}-artifacts-backups`         | 30d Nearline, 90d Coldline, 180d delete  |
| **Nomad state**      | each node  | 03:00 UTC  | `{project}-lm-{env}-nomad-backups`             | 30d Standard → Nearline, 90d delete      |

A single GCP service account (`lm-{env}-backup`) with `roles/storage.objectAdmin` on all 5 buckets is distributed to all machines via cloud-init.

---

## GitOps & Deployment

Two distinct layers — infra provisioning and workload scheduling are separate concerns.

### Terraform — Infrastructure Provisioning

`infra/terraform/` at repo root declares all Hivelocity resources as code. One `terraform apply` rebuilds the entire platform from scratch.

**State backend**: Google Cloud Storage (`backend "gcs" {}`). State is stored in a per-environment GCS bucket (`liquid-metal-tfstate-dev`, `liquid-metal-tfstate-prod`). A GCP service account key lives in `keys/` (gitignored) and is referenced via `GOOGLE_APPLICATION_CREDENTIALS`.

```
infra/
├── terraform/
│   ├── main.tf                    # GCS backend + providers (Hivelocity, Cloudflare, Tailscale, Google) + variables
│   ├── nodes.tf                   # 4× Hivelocity bare metal devices + VLAN + Tailscale keys + cloud-init
│   ├── dns.tf                     # Cloudflare wildcard + apex A records → gateway public IP
│   ├── services.tf                # (empty — Postgres + S3 are self-hosted, not managed)
│   ├── backups.tf                 # GCS backup buckets + SA
│   ├── outputs.tf                 # IPs, observability URLs, Wasabi credentials
│   ├── terraform.tfvars.example   # Example variable values for new environments
│   └── templates/
│       ├── cloud-init-gateway.yaml  # gateway: SSH hardening, Nomad server, NATS, HAProxy,
│       │                            #   Pingora, VictoriaMetrics, VictoriaLogs, Grafana, backup crons
│       ├── cloud-init-db.yaml       # node-db: Postgres 16 (32GB tuning), PgBouncer, pg_dump → GCS
│       ├── cloud-init-liquid.yaml   # node-liquid: SSH hardening, Nomad client, Promtail, node_exporter
│       └── cloud-init-metal.yaml    # node-metal: SSH hardening, Nomad client, Promtail, node_exporter,
│                                    #   Firecracker + jailer, KVM, br0 bridge, eBPF, NAT rules
└── nomad/
    ├── api.nomad.hcl              # API — single instance on gateway (node_class=gateway)
    ├── daemon-metal.nomad.hcl     # Daemon — Metal tier only (node_class=metal)
    ├── daemon-liquid.nomad.hcl    # Daemon — Liquid tier only (node_class=liquid)
    ├── proxy.nomad.hcl            # Proxy — single instance on gateway (node_class=gateway)
    └── migrate.nomad.hcl          # Batch job — applies pending SQL migrations via psql
```

Providers used:

| Provider                | Purpose                                                          |
|-------------------------|------------------------------------------------------------------|
| `hivelocity/hivelocity` | 4× dedicated bare metal servers + private VLAN                   |
| `cloudflare/cloudflare` | DNS — wildcard + apex → gateway public IP                        |
| `tailscale/tailscale`   | Pre-auth keys for operator SSH access                            |
| `hashicorp/google`      | GCS backup buckets + service account for backup uploads          |

Taskfile operations:

```bash
task infra:plan    # CLOUD_ENV=dev terraform plan
task infra:apply   # CLOUD_ENV=dev terraform apply
task infra:destroy # tear down everything
task infra:output  # print IPs, DB URL, storage credentials
```

All `TF_VAR_*` values are sourced from `.env` — no `terraform.tfvars` file needed in CI or locally.

### Nomad — Process Scheduling

Nomad runs on all nodes except node-db. The gateway runs both server and client (`bootstrap_expect=1`). Compute nodes run Nomad clients. It manages the lifecycle of `api`, `web`, `daemon`, and `proxy` — health checks, rolling deploys, restarts on crash. It does **not** schedule Firecracker VMs — that remains the daemon's job via NATS. Schema migrations are run via a separate `migrate.nomad.hcl` batch job dispatched before rolling deploys.

```
infra/nomad/
├── api.nomad.hcl              # api binary, :7070, single instance on gateway (node_class=gateway)
├── daemon-metal.nomad.hcl     # daemon — Metal tier (node_class=metal)
├── daemon-liquid.nomad.hcl    # daemon — Liquid tier (node_class=liquid)
└── proxy.nomad.hcl            # proxy binary, :8443, system job on gateway (node_class=gateway)
```

Nomad node classes map to role:

```hcl
# gateway node
client {
  node_class = "gateway"
}

# node-metal
client {
  node_class = "metal"
}

# node-liquid
client {
  node_class = "liquid"
}
```

Job specs are constrained to the correct node class. API and proxy run on `gateway`, daemons run on their respective compute tiers:

```hcl
constraint {
  attribute = "${node.class}"
  value     = "gateway"   # or "metal" / "liquid"
}
```

### GitHub Actions — CI/CD

```
.github/workflows/
├── ci.yml        # cargo check + test on every push to dev/main
├── release.yml   # cargo-dist builds flux CLI binary on v* tag → GitHub Release + Homebrew
└── deploy.yml    # on merge to main → nomad job run infra/nomad/*.nomad.hcl via Tailscale
```

`deploy.yml` flow:

```
merge to main
  → build api/daemon/proxy release binaries
  → upload to GitHub Release (or Wasabi S3)
  → SSH into gateway via Tailscale
  → nomad job run api.nomad.hcl   (rolling deploy, zero downtime)
  → nomad job run daemon.nomad.hcl
  → nomad job run proxy.nomad.hcl
```

Firecracker provisioning is unaffected — Nomad only restarts the daemon process, not the VMs it manages.
