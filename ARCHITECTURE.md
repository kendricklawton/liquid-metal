# Liquid Metal — Architecture

> Bare-metal hosting platform. Two products, no Kubernetes, ever.

> **Status**: Implementation in progress. Infrastructure described below is the target topology — not yet provisioned.

---

## Infrastructure Stack

All infrastructure runs in **Vultr Chicago (ORD)**. One vendor, one region, sub-millisecond between every layer. Backups replicate to GCP (GCS) for cross-cloud disaster recovery.

| Layer                 | Technology                                          | Notes                                                                              |
|-----------------------|-----------------------------------------------------|------------------------------------------------------------------------------------|
| **NAT VPS**           | Vultr VPS — Chicago                                 | Holds public IP permanently, HAProxy + Nomad server + NATS + observability stack   |
| **Compute (Metal)**   | Vultr Bare Metal — Chicago                          | node-a-01 — Firecracker microVMs, KVM isolation                                    |
| **Compute (Liquid)**  | Vultr Bare Metal — Chicago                          | node-b-01 — Wasmtime/WASI execution                                                |
| **Proxy**             | Pingora (Rust) on compute nodes                     | Slug → upstream routing, :8080 (behind HAProxy)                                    |
| **Load balancer**     | HAProxy on NAT VPS                                  | TLS termination (Let's Encrypt), rate limiting, health-checks, :80/:443            |
| **Private mesh**      | Tailscale                                           | Official Tailscale — VPS + both bare metal nodes                                   |
| **API**               | Axum (Rust), :7070                                  | Active/active on compute nodes                                                     |
| **Web**               | Axum + Askama + HTMX (Rust), :3000                  | Dashboard — server-rendered HTML, OIDC browser auth. *Planned.*                    |
| **Daemon**            | NATS consumer (Rust)                                | Each node owns its engine — Metal runs Firecracker, Liquid runs Wasmtime           |
| **CLI**               | `flux` binary (Rust)                                | login, init, deploy, status, logs, workspace, project                              |
| **Event bus**         | NATS JetStream                                      | Single server on NAT VPS, JetStream persistence to disk                            |
| **Database**          | Vultr Managed Postgres — Chicago                    | Managed HA + PgBouncer connection pooler on NAT VPS (:6432)                        |
| **Artifact store**    | Vultr Object Storage — Chicago                      | S3-compatible, rootfs images + .wasm binaries                                      |
| **DNS**               | Cloudflare                                          | Wildcard + apex → NAT VPS public IP                                                |
| **TLS**               | Let's Encrypt (certbot + Cloudflare DNS-01)         | Wildcard cert on HAProxy, auto-renewed via cron                                    |
| **eBPF isolation**    | Aya TC classifier per tap{n}                        | Tenant isolation at kernel level, no Cilium daemon                                 |
| **Observability**     | VictoriaMetrics + VictoriaLogs + Grafana            | Metrics scraping (:8428), log aggregation (:9428), dashboards (:3001)              |
| **Log shipping**      | Promtail on each compute node                       | Tails Nomad allocation logs + systemd journal → VictoriaLogs                       |
| **Node metrics**      | node_exporter on each compute node                  | Hardware metrics (:9100) scraped by VictoriaMetrics                                |
| **Connection pooling**| PgBouncer on NAT VPS                                | Transaction-mode pooler (:6432), all services connect here                         |
| **Backups**           | GCS (5 buckets) + rclone                            | Postgres, VictoriaMetrics, VictoriaLogs, Nomad, S3 artifacts → GCP daily           |
| **Infra provisioning**| Terraform (Vultr + Cloudflare + Tailscale + Google) | Everything declared as code in `infra/terraform/`                                  |
| **Process scheduling**| Nomad (HashiCorp)                                   | Schedules api/daemon/proxy on bare metal nodes — no K8s                            |
| **CI/CD**             | GitHub Actions                                      | ci.yml (check/test), release.yml (cargo-dist), deploy.yml (Nomad job update)       |
---

## Vultr Setup

> **Planned** — not yet provisioned.

### Region: Chicago (ORD)

Everything lives in Vultr Chicago. Bare metal, VPS, Managed Postgres, and Object Storage in the same datacenter — latency between any two services is <1ms.

### Services Used

| Vultr Service            | Purpose                                                         |
|--------------------------|-----------------------------------------------------------------|
| **Bare Metal** (×1)      | node-a-01 — Firecracker/Metal tier                              |
| **Bare Metal** (×1)      | node-b-01 — Wasmtime/Liquid tier                                |
| **Cloud Compute VPS**    | NAT VPS — HAProxy, Nomad server, NATS, PgBouncer, observability |
| **Block Storage** (40GB) | Persistent volume for VictoriaMetrics, VictoriaLogs, Grafana    |
| **Managed Databases**    | PostgreSQL 16 — primary datastore                               |
| **Object Storage**       | S3-compatible — rootfs images + .wasm binaries                  |

### Object Storage Layout

```
liquid-metal-artifacts/          (bucket name)
├── base/
│   └── alpine-3.20-amd64.ext4  # Base rootfs, built by us once
├── metal/
│   └── {service_id}/
│       └── {deploy_id}/
│           └── app              # User's statically compiled Linux binary
└── wasm/
    └── {service_id}/
        └── {deploy_id}/
            └── main.wasm        # User's compiled Wasm module
```

`deploy_id` is a UUID v7 generated at deploy time — each deploy is immutable.

Lifecycle policy: delete artifacts 30 days after service deletion.

The Rust codebase uses `aws-sdk-s3` pointed at the Vultr endpoint:

```
OBJECT_STORAGE_ENDPOINT=https://ord1.vultrobjects.com
OBJECT_STORAGE_BUCKET=liquid-metal-artifacts
OBJECT_STORAGE_ACCESS_KEY=<vultr-access-key>
OBJECT_STORAGE_SECRET_KEY=<vultr-secret-key>
```

### Managed Postgres

- Engine: PostgreSQL 16
- Plan: Vultr Managed Database, Chicago
- HA: Vultr manages standby + failover
- Backups: Vultr daily automated (7-day retention) + pg_dump to GCS daily at 03:00 UTC
- Connection pooling: PgBouncer on NAT VPS (:6432, transaction mode)
- All services connect via PgBouncer, not directly to Postgres
- Trusted IPs: `100.64.0.0/10` (Tailscale CGNAT range only)

---

## High Availability

> **Planned** — target topology once infrastructure is provisioned.

### Topology

```
Internet
    │
    ▼ DNS: *.domain + domain → NAT VPS public IP
┌───────────────────────────────────────────────┐
│  NAT VPS — Vultr Chicago (Cloud Compute VPS)  │
│  HAProxy :80/:443 (TLS termination + rate limiting)
│  Nomad server (bootstrap_expect=1)            │
│  NATS server (JetStream, single node)         │
│  PgBouncer :6432 (transaction-mode pooler)    │
│  VictoriaMetrics :8428 (metrics)              │
│  VictoriaLogs :9428 (log aggregation)         │
│  Grafana :3001 (dashboards + alerting)        │
│  Block Storage at /mnt/observability (40GB)   │
└────────────┬──────────────────────────────────┘
             │ Tailscale mesh (100.x.x.x)
    ┌────────┴────────┐
    │                 │
  Metal tier      Liquid tier
    │                 │
    ▼                 ▼
  node-a-01        node-b-01
  Pingora :8080    Pingora :8080
  API :7070        API :7070
  Daemon           Daemon
  Nomad client     Nomad client
  Promtail         Promtail
  node_exporter    node_exporter
  KVM+Firecracker  Wasmtime
```

The NAT VPS holds the public IP permanently — it never moves. HAProxy health-checks both bare metal nodes and routes traffic to Pingora. Tailscale carries all internal traffic.

### Per-Layer HA Strategy

| Layer                 | Strategy                                                                      | Failover time |
|-----------------------|-------------------------------------------------------------------------------|---------------|
| **Public IP**         | Permanent on NAT VPS — never moves                                            | N/A           |
| **HAProxy**           | Single instance on NAT VPS, TLS termination + rate limiting                   | Manual        |
| **Pingora**           | Active on both compute nodes. HAProxy round-robins with health checks         | <5s           |
| **API**               | Active/active on both compute nodes                                           | <5s           |
| **NATS JetStream**    | Single server on NAT VPS with file-backed JetStream persistence               | Manual        |
| **Managed Postgres**  | Vultr-managed standby + automatic failover                                    | <60s          |
| **PgBouncer**         | Single instance on NAT VPS, transaction-mode pooling                          | Manual        |
| **Object Storage**    | Vultr-managed, redundant by default                                           | N/A           |
| **Observability**     | VictoriaMetrics + VictoriaLogs + Grafana on NAT VPS, block storage persists   | Manual        |
| **Backups**           | Daily crons to GCS (Postgres, metrics, logs, Nomad state, S3 artifacts)       | N/A           |

> **Note**: The NAT VPS is a single point of failure for control-plane services (NATS, Nomad, PgBouncer, observability). This is a deliberate tradeoff for a lean 3-node cluster. If the NAT VPS fails, running services continue operating (Pingora routes directly) but new deploys and observability are unavailable until it recovers.

### NATS JetStream

Single-node NATS server on the NAT VPS with JetStream enabled for durable stream persistence. Listens on `:4222` (Tailscale only — blocked from public by UFW).

```
# /etc/nats/nats.conf on NAT VPS
listen: 0.0.0.0:4222

jetstream {
  store_dir: /var/lib/nats/jetstream
  max_mem: 256MB
  max_file: 4GB
}
```

Both compute nodes connect to this single NATS server via Tailscale. JetStream persistence ensures events survive a NATS restart.

### Network Architecture

```
Internet
    │
    ▼ (*.domain + domain → NAT VPS public IP)
NAT VPS — Vultr Chicago
    HAProxy :80/:443 (TLS + rate limiting)
    Nomad server, NATS :4222, PgBouncer :6432
    VictoriaMetrics :8428, VictoriaLogs :9428, Grafana :3001
    │
    │ Tailscale mesh (100.x.x.x CGNAT range)
    ├──────────────────────┐
    │                      │
  Metal tier           Liquid tier
    │                      │
    ▼                      ▼
  node-a-01            node-b-01
  Pingora :8080        Pingora :8080
  API :7070            API :7070
  Daemon               Daemon
  Nomad client         Nomad client
  Promtail → VLogs     Promtail → VLogs
  node_exporter :9100  node_exporter :9100
  KVM+br0+TAP          Wasmtime
    │                      │
    └──────────┬───────────┘
               ▼
    Vultr Managed Postgres (Chicago)
    ├── via PgBouncer :6432 on NAT VPS
    Vultr Object Storage (Chicago)
    ├── backed up to GCS via rclone
```

**Internal mesh (Tailscale):**
- Official Tailscale — each node joins the same Tailscale network
- All inter-service traffic (API→NATS, daemon→NATS, services→PgBouncer) uses Tailscale IPs
- No manual key management — Tailscale handles it

**Exposed ports (NAT VPS only — bare metal nodes have no public ports):**
- `:80` — HAProxy (HTTP → HTTPS redirect)
- `:443` — HAProxy (TLS-terminated user traffic, per-IP rate limited at 50 req/s)
- `:3478` — STUN (UDP, Tailscale NAT traversal)

**Bare metal nodes — public firewall: all closed.**
SSH is via `tailscale ssh` — no public port 22.

**SSH hardening (all nodes):**
- fail2ban (5 retries, 1hr ban)
- Password auth disabled, key-only root, X11 forwarding disabled

---

## eBPF Tenant Isolation (Aya)

> Applies to **Metal tier only** (node-a-01). Liquid nodes run Wasmtime in-process — no TAP devices, no bridge, no eBPF needed.

### Why — The Multi-Tenant Bridge Problem

All Firecracker VMs on a Metal node share the same `br0` bridge. Without enforcement,
VM A can send packets directly to VM B's TAP IP (172.16.x.x). This is a
cross-tenant security hole.

The solution is a TC (Traffic Control) eBPF classifier attached to each VM's
`tap{n}` device at provision time. It runs inside the Linux kernel — the packet
never leaves the kernel stack, and it cannot be bypassed from inside the VM.

### Stack — No Kubernetes, No Cilium Daemon

| Component | Role |
|---|---|
| `crates/ebpf-programs/` | Kernel-side: TC classifier compiled to BPF bytecode (`bpfel-unknown-none`) |
| `crates/daemon/src/ebpf.rs` | Userspace: Aya loader — attaches/detaches programs per TAP |
| `crates/daemon/build.rs` | Compiles `ebpf-programs` at daemon build time, embeds via `include_bytes_aligned!` |

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

## Artifact Registry

There is no separate registry service. **Vultr Object Storage is the registry.**
Users ship compiled binaries — not container images. The artifact format is
simpler than OCI, so a flat object store with a keying convention is sufficient.

### Bucket Layout

```
liquid-metal-artifacts/
├── base/
│   └── alpine-3.20-amd64.ext4        ← base rootfs, built by us once
├── metal/
│   └── {service_id}/
│       └── {deploy_id}/
│           └── app                   ← user's statically compiled Linux binary
└── wasm/
    └── {service_id}/
        └── {deploy_id}/
            └── main.wasm             ← user's compiled Wasm module
```

`deploy_id` is a UUID v7 generated at deploy time. Each deploy is immutable —
rollback means re-provisioning with an older `deploy_id`.

### Why No Container Registry

- **Metal**: user ships a static Linux binary → daemon injects it into the base
  Alpine rootfs template → boots as a Firecracker VM. No Dockerfile, no layers.
- **Liquid**: user ships a `.wasm` file → daemon loads it into Wasmtime. No
  image format at all.

---

## Data Flow

### NATS Event Reference

All events live in `crates/common/src/events.rs`. JetStream events are durable (at-least-once delivery). Fire-and-forget events are plain NATS publishes — no ack, best-effort.

| Subject | Type | Transport | Publisher | Consumer |
|---|---|---|---|---|
| `platform.provision` | `ProvisionEvent` | JetStream | API outbox poller | Daemon |
| `platform.deprovision` | `DeprovisionEvent` | JetStream | API | Daemon |
| `platform.route_updated` | `RouteUpdatedEvent` | Fire-and-forget | Daemon | Proxy (all instances) |
| `platform.route_removed` | `RouteRemovedEvent` | Fire-and-forget | Daemon | Proxy (all instances) |
| `platform.traffic_pulse` | `TrafficPulseEvent` | Fire-and-forget | Proxy | Daemon |
| `platform.usage_metal` | `MetalUsageEvent` | Fire-and-forget | Daemon | API |
| `platform.usage_liquid` | `LiquidUsageEvent` | Fire-and-forget | Daemon | API |
| `platform.service_crashed` | `ServiceCrashedEvent` | Fire-and-forget | Daemon | API |
| `platform.suspend` | `SuspendEvent` | JetStream | API | Daemon |

`DeprovisionEvent` carries `slug` so the daemon can publish `RouteRemovedEvent` without a DB lookup.

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

```
flux deploy
  1. read liquid-metal.toml → engine, name, build command
  2. build locally:
       metal:  cargo build --target x86_64-unknown-linux-musl --release
       liquid: cargo build --target wasm32-wasip1 --release
  3. sha256(artifact) + generate deploy_id (uuid_v7)
  4. GET /upload-url → API returns pre-signed Object Storage PUT URL
  5. PUT artifact → Object Storage directly (no API in the upload path)
  6. POST /services { slug, engine, spec, artifact_key, deploy_id, sha256 }
       → API: capacity gate (hobby Metal) + advisory lock + INSERT services + INSERT outbox
       → COMMIT (atomic)
       → outbox poller publishes ProvisionEvent → NATS JetStream

NATS → daemon
  ├─ metal:  download base-alpine.ext4 from Object Storage (cached locally)
  │           download user binary from Object Storage
  │           inject binary into rootfs template
  │           attach eBPF TC filter (ebpf.rs)
  │           spawn Firecracker → boot VM
  │           UPDATE services SET upstream_addr, node_id, status='running'
  │           publish RouteUpdatedEvent → NATS (platform.route_updated)
  └─ liquid: download main.wasm from Object Storage
             load into Wasmtime gateway (fuel: 1B ops/invocation → OutOfFuel trap on runaway)
             UPDATE services SET upstream_addr, status='running'
             publish RouteUpdatedEvent → NATS (platform.route_updated)
```

`RouteUpdatedEvent` is consumed by every Pingora instance's background subscriber, which writes `slug → upstream_addr` into the in-process route cache. The proxy cache is warm within milliseconds of provisioning completing.

### Live Request

```
Browser → Pingora
  └─ slug lookup
      ├─ cache hit  → in-memory HashMap<slug, upstream_addr> (no DB round-trip)
      └─ cache miss → Managed Postgres → upstream_addr → populate cache
      ├─ metal:  proxy → TAP → Firecracker VM
      └─ liquid: dispatch → in-process Wasmtime executor → response
```

The route cache (`crates/proxy/src/cache.rs`) is an `Arc<RwLock<HashMap<String, String>>>` kept warm by a background NATS subscriber. It listens on two subjects simultaneously using `tokio::select!`:

| Subject | Event | Cache action |
|---|---|---|
| `platform.route_updated` | Service provisioned / upstream_addr set | `insert(slug, addr)` |
| `platform.route_removed` | Service stopped or deleted | `remove(slug)` |

On a cold start or cache miss it falls back to a DB lookup and then populates the cache. There is no Redis — the cache is in-process, kilobytes of data, and DB is always the authoritative source on restart. Memory is bounded by the number of **currently running** services.

### Stop / Delete

```
flux stop <id>  (or flux delete <id>)
  → DELETE/POST /services/:id/stop
  → API: UPDATE services SET status='stopped', upstream_addr=NULL
  → API publishes DeprovisionEvent { service_id, slug, engine } → NATS JetStream

NATS → daemon
  → halt VM (SIGTERM → SIGKILL) or unload Wasm module
  → detach eBPF TC filter + remove TAP device (Metal only)
  → cleanup cgroup + artifact cache (Metal only)
  → publish RouteRemovedEvent { slug } → NATS (platform.route_removed)

platform.route_removed → every Pingora instance
  → cache.remove(slug)   ← slug evicted immediately
```

After eviction, any request for that slug gets a cache miss → DB lookup → `upstream_addr` is NULL → falls back to `api_upstream`. The service is gone.

---

## Observability

All observability services run on the NAT VPS. Data is stored on persistent Vultr Block Storage mounted at `/mnt/observability` (survives VPS rebuilds). All ports are Tailscale-only — blocked from public by UFW.

| Service | Port | Purpose |
|---|---|---|
| **VictoriaMetrics** | :8428 | Time-series database. Scrapes node_exporter on each compute node every 15s. 30-day retention. |
| **VictoriaLogs** | :9428 | Log aggregation. Accepts Loki push protocol (`/insert/loki/api/v1/push`). 30-day retention. |
| **Grafana** | :3001 | Dashboards and alerting. VictoriaMetrics as Prometheus datasource, VictoriaLogs via plugin. |
| **Promtail** | :9080 (per node) | Log shipper on each compute node. Tails Nomad allocation logs + systemd journal. |
| **node_exporter** | :9100 (per node) | Hardware metrics from /proc and /sys on each compute node. |

### Provisioned Alerts (Grafana)

| Alert | Condition | For |
|---|---|---|
| Node Down | `up{job="node_exporter"} == 0` | 2m |
| Disk Space Critical | Root filesystem below 10% free | 5m |
| High CPU | CPU usage above 90% sustained | 10m |
| High Memory | Memory usage above 90% | 5m |
| Observability Volume Low | `/mnt/observability` below 15% free | 5m |

## Backups

All cluster data replicates daily to GCP (GCS) for cross-cloud disaster recovery. Each data type has its own bucket with lifecycle policies.

| Data | Schedule | Destination | Retention |
|---|---|---|---|
| **Postgres** | 03:00 UTC | `{project}-lm-{env}-postgres-backups` | 30d Standard → Nearline, 90d delete |
| **VictoriaMetrics** | 03:30 UTC | `{project}-lm-{env}-victoriametrics-backups` | 30d Standard → Nearline, 90d delete |
| **VictoriaLogs** | 04:00 UTC | `{project}-lm-{env}-victorialogs-backups` | 30d Standard → Nearline, 90d delete |
| **S3 Artifacts** | 04:30 UTC | `{project}-lm-{env}-artifacts-backups` | 30d Nearline, 90d Coldline, 180d delete |
| **Nomad state** | 03:00 UTC (per node) | `{project}-lm-{env}-nomad-backups` | 30d Standard → Nearline, 90d delete |

A single GCP service account (`lm-{env}-backup`) with `roles/storage.objectAdmin` on all 5 buckets is distributed to all machines via cloud-init.

---

## GitOps & Deployment

Two distinct layers — infra provisioning and workload scheduling are separate concerns.

### Terraform — Infrastructure Provisioning

`infra/terraform/` at repo root declares all Vultr resources as code. One `terraform apply` rebuilds the entire platform from scratch.

**State backend**: Google Cloud Storage (`backend "gcs" {}`). State is stored in a per-environment GCS bucket (`liquid-metal-tfstate-dev`, `liquid-metal-tfstate-prod`). A GCP service account key lives in `keys/` (gitignored) and is referenced via `GOOGLE_APPLICATION_CREDENTIALS`.

```
infra/
├── terraform/
│   ├── main.tf                    # GCS backend + providers (Vultr, Cloudflare, Tailscale, Google) + variables
│   ├── nodes.tf                   # Tailscale keys + 2× bare metal + NAT VPS + cloud-init templatefile calls
│   ├── services.tf                # Vultr Managed Postgres + Object Storage
│   ├── backups.tf                 # GCS backup buckets (5×) + SA + block storage
│   ├── dns.tf                     # Cloudflare wildcard + apex A records
│   ├── outputs.tf                 # IPs, DB URL, PgBouncer URL, observability URLs, storage keys
│   ├── terraform.tfvars.example   # Example variable values for new environments
│   └── templates/
│       ├── cloud-init-nat.yaml    # NAT VPS: SSH hardening, Nomad server, NATS, TLS/certbot, HAProxy,
│       │                          #   block storage, VictoriaMetrics, VictoriaLogs, Grafana (with alerts),
│       │                          #   PgBouncer, backup crons (Postgres, metrics, logs, artifacts)
│       └── cloud-init-node.yaml   # Bare metal: SSH hardening, Nomad client, Promtail, node_exporter,
│                                  #   Nomad state backup cron, Firecracker setup (Metal only)
└── jobs/
    ├── api.nomad.hcl              # API — active/active on compute nodes
    ├── daemon-metal.nomad.hcl     # Daemon — Metal tier only (node_class=metal)
    ├── daemon-liquid.nomad.hcl    # Daemon — Liquid tier only (node_class=liquid)
    ├── proxy.nomad.hcl            # Proxy — active/active on compute nodes
    └── migrate.nomad.hcl          # Batch job — applies pending SQL migrations via psql
```

Providers used:

| Provider | Purpose |
|---|---|
| `vultr/vultr` | Bare metal, VPS, Block Storage, Managed Postgres, Object Storage |
| `cloudflare/cloudflare` | DNS — wildcard + apex → NAT VPS public IP |
| `tailscale/tailscale` | Pre-auth keys for node join |
| `hashicorp/google` | GCS backup buckets + service account for backup uploads |

Taskfile operations:

```bash
task infra:plan    # CLOUD_ENV=dev terraform plan
task infra:apply   # CLOUD_ENV=dev terraform apply
task infra:destroy # tear down everything
task infra:output  # print IPs, DB URL, storage credentials
```

All `TF_VAR_*` values are sourced from `.env` — no `terraform.tfvars` file needed in CI or locally.

### Nomad — Process Scheduling

Nomad runs on both bare metal nodes (clients) and the NAT VPS (server, `bootstrap_expect=1`). It manages the lifecycle of `api`, `daemon`, and `proxy` — health checks, rolling deploys, restarts on crash. It does **not** schedule Firecracker VMs — that remains the daemon's job via NATS. Schema migrations are run via a separate `migrate.nomad.hcl` batch job dispatched before rolling deploys.

```
infra/jobs/
├── api.nomad.hcl              # api binary, :7070, active/active all nodes
├── daemon-metal.nomad.hcl     # daemon — Metal tier (node_class=metal)
├── daemon-liquid.nomad.hcl    # daemon — Liquid tier (node_class=liquid)
└── proxy.nomad.hcl            # proxy binary, :443, active/active all nodes
```

Nomad node classes map to engine tier:

```hcl
# node-a-01 — set at agent config
client {
  node_class = "metal"
}

# node-b-01
client {
  node_class = "liquid"
}
```

Daemon job spec pins to the correct tier:

```hcl
constraint {
  attribute = "${node.class}"
  value     = "metal"   # or "liquid"
}
```

### GitHub Actions — CI/CD

```
.github/workflows/
├── ci.yml        # cargo check + test on every push to dev/main
├── release.yml   # cargo-dist builds flux CLI binary on v* tag → GitHub Release + Homebrew
└── deploy.yml    # on merge to main → nomad job run infra/jobs/*.nomad.hcl via Tailscale
```

`deploy.yml` flow:

```
merge to main
  → build api/daemon/proxy release binaries
  → upload to GitHub Release (or Object Storage)
  → SSH into NAT VPS via Tailscale
  → nomad job run api.nomad.hcl   (rolling deploy, zero downtime)
  → nomad job run daemon.nomad.hcl
  → nomad job run proxy.nomad.hcl
```

Firecracker provisioning is unaffected — Nomad only restarts the daemon process, not the VMs it manages.

---

## Codebase Layout

```
liquid-metal/
├── crates/
│   ├── common/        Shared Rust types (Engine, ProvisionEvent, EngineSpec, slugify)
│   ├── api/           Axum — REST/JSON server :7070, publishes to NATS
│   ├── web/           Axum + Askama + HTMX dashboard :3000 — OIDC browser auth, calls API internally
│   │                    PLANNED — not yet built
│   ├── cli/           flux CLI — login, init, deploy, status, logs, workspace, project
│   ├── proxy/         Pingora edge router — slug → upstream_addr
│   ├── daemon/        NATS consumer — Firecracker (Metal) + Wasmtime (Liquid) provision loop
│   └── ebpf-programs/  TC egress classifier (Aya, bpfel target) — Metal tier only
│                          Excluded from workspace, compiled by daemon/build.rs
├── infra/
│   ├── terraform/     Terraform — Vultr nodes, Postgres, Object Storage, DNS, Tailscale + cloud-init templates
│   └── jobs/          Nomad job specs — api, daemon-metal, daemon-liquid, proxy
├── keys/              GCP service account keys for Terraform state (gitignored)
└── migrations/        PostgreSQL migrations (refinery, embedded in api)
```

---

## Environment Variables

### Runtime (services)

| Variable                      | Used by            | Description                                                 |
|-------------------------------|--------------------|-------------------------------------------------------------|
| `DATABASE_URL`                | api, proxy, daemon | Vultr Managed Postgres connection string                    |
| `NATS_URL`                    | api, daemon, proxy | NATS JetStream address (Tailscale IP)                       |
| `BIND_ADDR`                   | api, proxy         | Listen address                                              |
| `INTERNAL_SECRET`             | api                | Shared secret for internal provisioning route               |
| `OBJECT_STORAGE_ENDPOINT`     | api, daemon        | Vultr Object Storage endpoint (S3-compat)                   |
| `OBJECT_STORAGE_BUCKET`       | api, daemon        | Bucket name for artifacts                                   |
| `OBJECT_STORAGE_ACCESS_KEY`   | api, daemon        | Vultr Object Storage access key                             |
| `OBJECT_STORAGE_SECRET_KEY`   | api, daemon        | Vultr Object Storage secret key                             |
| `FLUX_API_KEY`                | cli                | Liquid Metal API key (X-Api-Key header)                     |
| `FLUX_API_URL`                | cli                | API base URL                                                |
| `NODE_ID`                     | daemon             | Identifies which bare metal node (e.g. `node-a-01`)         |
| `NODE_ENGINE`                 | daemon             | `metal` or `liquid` — which engine this node runs           |
| `METAL_CAPACITY_MB`           | api                | Total allocatable Metal RAM across all nodes (MB); used by hobby tier capacity gate. 0 = gate disabled |
| `IDLE_TIMEOUT_SECS`           | daemon             | Seconds of no traffic before a serverless Metal VM is stopped (default: 300; set to 0 to disable for always-on services) |
| `FC_BIN`                      | daemon             | Path to Firecracker binary (Linux, Metal nodes only)        |
| `FC_KERNEL_PATH`              | daemon             | Path to guest kernel vmlinux (Linux)                        |
| `BRIDGE`                      | daemon             | TAP bridge name, e.g. `br0` (Linux)                         |
| `ARTIFACT_DIR`                | daemon             | Local artifact cache directory                              |

### Infrastructure provisioning (Terraform via Taskfile)

| Variable                        | Used by   | Description                                              |
|---------------------------------|-----------|----------------------------------------------------------|
| `VULTR_API_KEY`                 | Terraform | Vultr API key                                            |
| `VULTR_SSH_KEY_ID`              | Terraform | SSH key ID to inject into nodes (from Vultr console)     |
| `VULTR_BARE_METAL_PLAN`         | Terraform | Bare metal plan ID, default `vbm-4c-32gb`                |
| `VULTR_VPS_PLAN`                | Terraform | VPS plan for NAT node, default `vc2-2c-4gb`              |
| `VULTR_OS_ID`                   | Terraform | OS image ID (Ubuntu 24.04 LTS)                           |
| `CLOUDFLARE_API_TOKEN`          | Terraform | Cloudflare API token (Zone:DNS:Edit + certbot DNS-01)    |
| `CLOUDFLARE_ZONE_ID`            | Terraform | Cloudflare zone ID for your domain                       |
| `DOMAIN`                        | Terraform | Root domain, e.g. `liquidmetal.dev`                      |
| `TAILSCALE_API_KEY`             | Terraform | Tailscale API key for generating pre-auth keys           |
| `TAILSCALE_TAILNET`             | Terraform | Tailscale tailnet name, e.g. `yourname.github`           |
| `GRAFANA_ADMIN_PASSWORD`        | Terraform | Grafana admin UI password                                |
| `GCP_PROJECT`                   | Terraform | GCP project ID for backup buckets                        |
| `GCP_REGION`                    | Terraform | GCS bucket region, default `us-central1`                 |
| `GOOGLE_APPLICATION_CREDENTIALS`| Terraform | Path to GCP service account JSON — `keys/*.json`         |

See `infra/terraform/terraform.tfvars.example` for a complete variable reference.

---

## Hard Constraints

- **No Kubernetes, K3s, or any container orchestrator** — Nomad for process scheduling only
- **No managed compute** — bare metal for execution (Vultr Managed Postgres + Object Storage are fine)
- **No ORMs** — `tokio-postgres` with raw SQL everywhere
- **No hardcoded addresses** — all config via env vars
- **Firecracker + TAP are Linux-only** — gated with `#[cfg(target_os = "linux")]`
- **Wasmtime runs on all platforms** — safe for local macOS dev
- **All Rust** — Zig used only as a cross-compilation linker for Windows CLI builds
- **Terraform owns infra** — no manual Vultr resource creation; everything declared in `infra/`
- **Nomad does not schedule VMs** — Firecracker lifecycle is owned exclusively by the daemon
