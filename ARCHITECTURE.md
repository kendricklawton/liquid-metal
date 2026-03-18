# Liquid Metal — Architecture

> **Status**: Infrastructure provisioned and operational.

---

## Infrastructure Stack

All infrastructure runs in **Vultr Chicago (ORD)**. One vendor, one region, sub-millisecond between every layer. Backups replicate to GCP (GCS) for cross-cloud disaster recovery.
| Layer                 | Technology                                          | Notes                                                                                                |
|-----------------------|-----------------------------------------------------|------------------------------------------------------------------------------------------------------|
| **NAT VPS**           | Vultr VPS — Chicago                                 | Holds public IP permanently, HAProxy + Nomad server + NATS + observability stack                     |
| **Compute (Metal)**   | Vultr Bare Metal — Chicago                          | node-a-01 — Firecracker microVMs, KVM isolation                                                      |
| **Compute (Liquid)**  | Vultr Bare Metal — Chicago                          | node-b-01 — Wasmtime/WASI execution                                                                  |
| **Proxy**             | Pingora (Rust) on NAT VPS                           | Slug → upstream routing, :8443 (HAProxy forwards to localhost:8443)                                  |
| **Load balancer**     | HAProxy on NAT VPS                                  | TLS termination (Let's Encrypt), rate limiting, health-checks, :80/:443                              |
| **Private mesh**      | Tailscale                                           | Official Tailscale — VPS + both bare metal nodes                                                     |
| **API**               | Axum (Rust), :7070                                  | Single instance on NAT VPS                                                                           |
| **Web**               | Axum + Askama + HTMX (Rust), :3000                  | Dashboard — server-rendered HTML, OIDC browser auth                                                  |
| **Daemon**            | NATS consumer (Rust)                                | Each node owns its engine — Metal runs Firecracker, Liquid runs Wasmtime                             |
| **CLI**               | `flux` binary (Rust)                                | auth, service (deploy/stop/restart/delete/logs/env/domains/scale/rollback), init, billing, workspace, project, invite |
| **Event bus**         | NATS JetStream                                      | Single server on NAT VPS, JetStream persistence to disk                                              |
| **Database**          | Vultr Managed Postgres — Chicago                    | Managed HA + PgBouncer connection pooler on NAT VPS (:6432)                                          |
| **Artifact store**    | Vultr Object Storage — Chicago                      | S3-compatible, rootfs images + .wasm binaries                                                        |
| **DNS**               | Cloudflare                                          | Wildcard + apex → NAT VPS public IP                                                                  |
| **TLS**               | Let's Encrypt (certbot + Cloudflare DNS-01)         | Wildcard cert on HAProxy, auto-renewed via cron                                                      |
| **eBPF isolation**    | Aya TC classifier per tap{n}                        | Tenant isolation at kernel level, no Cilium daemon                                                   |
| **Observability**     | VictoriaMetrics + VictoriaLogs + Grafana            | Metrics scraping (:8428), log aggregation (:9428), dashboards (:3001)                                |
| **Log shipping**      | Promtail on each compute node                       | Tails Nomad allocation logs + systemd journal → VictoriaLogs                                         |
| **Node metrics**      | node_exporter on each compute node                  | Hardware metrics (:9100) scraped by VictoriaMetrics                                                  |
| **Connection pooling**| PgBouncer on NAT VPS                                | Transaction-mode pooler (:6432), all services connect here                                           |
| **Backups**           | GCS (5 buckets) + rclone                            | Postgres, VictoriaMetrics, VictoriaLogs, Nomad, S3 artifacts → GCP daily                             |
| **Infra provisioning**| Terraform (Vultr + Cloudflare + Tailscale + Google) | Everything declared as code in `infra/terraform/`                                                    |
| **Process scheduling**| Nomad (HashiCorp)                                   | Schedules api/proxy on NAT VPS (gateway), daemons on bare metal — no K8s                             |
| **CI/CD**             | GitHub Actions                                      | ci.yml (check/test), release.yml (cargo-dist), deploy.yml (Nomad job update)                         |
---

## High Availability

### Topology

```
Internet
    │
    ▼ DNS: *.domain + domain → Vultr floating IP (reserved IP)
┌───────────────────────────────────────────────┐
│  Gateway A (NAT VPS) — primary                │
│  HAProxy :80/:443 (TCP pass-through for 443)  │
│  Nomad server + client (node_class=gateway)   │
│  NATS server (JetStream, single node)         │
│  PgBouncer :6432 (transaction-mode pooler)    │
│  API :7070 (single instance)                  │
│  Pingora :8443 (TLS termination, SNI routing) │
│  VictoriaMetrics :8428 (metrics)              │
│  VictoriaLogs :9428 (log aggregation)         │
│  Grafana :3001 (dashboards + alerting)        │
│  Block Storage at /mnt/observability (40GB)   │
└────────────┬──────────────────────────────────┘
             │ Tailscale mesh (100.x.x.x)
    ┌────────┼────────┐
    │        │        │
  Metal    Liquid   Gateway B
  tier     tier     (standby)
    │        │        │
    ▼        ▼        ▼
  node-a-01  node-b-01  gateway-b
  Daemon     Daemon      HAProxy + Pingora
  Nomad      Nomad       Nomad client
  Promtail   Promtail    PgBouncer
  node_exp   node_exp    Failover script
  KVM+FC     Wasmtime
```

DNS points at a Vultr floating IP (reserved IP) attached to gateway-a by default. HAProxy on port 443 does TCP pass-through to Pingora on `:8443`, which terminates TLS with SNI-based cert selection — wildcard cert for platform subdomains, per-domain certs for custom domains. Gateway-b is a standby that runs a health check script every 30s; after 3 consecutive failures it calls the Vultr API to move the floating IP to itself. Tailscale carries all internal traffic.

### Per-Layer HA Strategy

| Layer                 | Strategy                                                                      | Failover time   |
|-----------------------|-------------------------------------------------------------------------------|-----------------|
| **Public IP**         | Vultr floating IP — moves to gateway-b on gateway-a failure                   | ~90s            |
| **HAProxy**           | Runs on both gateways (TCP pass-through for :443, HTTP proxy for :80)         | ~90s (failover) |
| **Pingora**           | System job on all gateway nodes (TLS termination, SNI routing)                | ~90s (failover) |
| **API**               | Single instance on gateway-a (primary)                                        | Manual          |
| **NATS JetStream**    | Single server on gateway-a with file-backed JetStream persistence             | Manual          |
| **Managed Postgres**  | Vultr-managed standby + automatic failover                                    | <60s            |
| **PgBouncer**         | Runs on both gateways, transaction-mode pooling                               | ~90s (failover) |
| **Object Storage**    | Vultr-managed, redundant by default                                           | N/A             |
| **Observability**     | VictoriaMetrics + VictoriaLogs + Grafana on gateway-a, block storage persists | Manual          |
| **Backups**           | Daily crons to GCS (Postgres, metrics, logs, Nomad state, S3 artifacts)       | N/A             |

> **Note**: Gateway-b provides automatic failover for the ingress path (HAProxy + Pingora + PgBouncer). If gateway-a fails, existing traffic routes through gateway-b within ~90s. However, control-plane services (API, NATS, Nomad server, observability) only run on gateway-a — new deploys and observability are unavailable until gateway-a recovers. Running VMs/Wasm modules continue operating throughout.

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
    ▼ (*.domain + domain → Vultr floating IP)
Gateway A (NAT VPS) — Vultr Chicago
    HAProxy :80/:443 (TCP pass-through for 443)
    Pingora :8443 (TLS termination, SNI cert selection)
    API :7070, Nomad server + client (gateway)
    NATS :4222, PgBouncer :6432
    VictoriaMetrics :8428, VictoriaLogs :9428, Grafana :3001
    │
    │ Tailscale mesh (100.x.x.x CGNAT range)
    ├──────────────────────┬──────────────────┐
    │                      │                  │
  Metal tier           Liquid tier        Gateway B
    │                      │              (standby)
    ▼                      ▼                  ▼
  node-a-01            node-b-01          gateway-b
  Daemon               Daemon             HAProxy + Pingora
  Nomad client         Nomad client       Nomad client
  Promtail → VLogs     Promtail → VLogs   PgBouncer
  node_exporter :9100  node_exporter :9100 Failover script
  KVM+br0+TAP          Wasmtime
    │                      │
    └──────────┬───────────┘
               ▼
    Vultr Managed Postgres (Chicago)
    ├── via PgBouncer :6432 on gateway-a + gateway-b
    Vultr Object Storage (Chicago)
    ├── backed up to GCS via rclone
```

**Internal mesh (Tailscale):**
- Official Tailscale — each node joins the same Tailscale network
- All inter-service traffic (API→NATS, daemon→NATS, services→PgBouncer) uses Tailscale IPs
- No manual key management — Tailscale handles it

**Exposed ports (gateways only — bare metal nodes have no public ports):**
- `:80` — HAProxy (HTTP → Pingora for ACME challenges + HTTPS redirect)
- `:443` — HAProxy (TCP pass-through → Pingora TLS termination)
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
       → API: advisory lock + INSERT services + INSERT outbox
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

On failure:
  → classify error as Transient or Permanent (FailureKind)
  → UPDATE services SET status='failed', failure_reason=<error>, provision_attempts += 1
  → Permanent (SHA mismatch, startup probe timeout): ACK — no retry
  → Transient (S3 timeout, DB blip): NAK with backoff (15s, 30s) — max 3 attempts
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

| Subject                  | Event                                   | Cache action         |
|--------------------------|----------------------------------------|----------------------|
| `platform.route_updated` | Service provisioned / upstream_addr set | `insert(slug, addr)` |
| `platform.route_removed` | Service stopped or deleted              | `remove(slug)`       |

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

| Service              | Port             | Purpose                                                                                      |
|----------------------|------------------|----------------------------------------------------------------------------------------------|
| **VictoriaMetrics**  | :8428            | Time-series database. Scrapes node_exporter on each compute node every 15s. 30-day retention |
| **VictoriaLogs**     | :9428            | Log aggregation. Accepts Loki push protocol (`/insert/loki/api/v1/push`). 30-day retention   |
| **Grafana**          | :3001            | Dashboards and alerting. VictoriaMetrics as Prometheus datasource, VictoriaLogs via plugin    |
| **Promtail**         | :9080 (per node) | Log shipper on each compute node. Tails Nomad allocation logs + systemd journal              |
| **node_exporter**    | :9100 (per node) | Hardware metrics from /proc and /sys on each compute node                                    |

### Provisioned Alerts (Grafana)

| Alert                    | Condition                              | For  |
|--------------------------|----------------------------------------|------|
| Node Down                | `up{job="node_exporter"} == 0`         | 2m   |
| Disk Space Critical      | Root filesystem below 10% free         | 5m   |
| High CPU                 | CPU usage above 90% sustained          | 10m  |
| High Memory              | Memory usage above 90%                 | 5m   |
| Observability Volume Low | `/mnt/observability` below 15% free    | 5m   |

## Backups

All cluster data replicates daily to GCP (GCS) for cross-cloud disaster recovery. Each data type has its own bucket with lifecycle policies.

| Data                 | Schedule             | Destination                                    | Retention                                |
|----------------------|----------------------|------------------------------------------------|------------------------------------------|
| **Postgres**         | 03:00 UTC            | `{project}-lm-{env}-postgres-backups`          | 30d Standard → Nearline, 90d delete      |
| **VictoriaMetrics**  | 03:30 UTC            | `{project}-lm-{env}-victoriametrics-backups`   | 30d Standard → Nearline, 90d delete      |
| **VictoriaLogs**     | 04:00 UTC            | `{project}-lm-{env}-victorialogs-backups`      | 30d Standard → Nearline, 90d delete      |
| **S3 Artifacts**     | 04:30 UTC            | `{project}-lm-{env}-artifacts-backups`         | 30d Nearline, 90d Coldline, 180d delete  |
| **Nomad state**      | 03:00 UTC (per node) | `{project}-lm-{env}-nomad-backups`             | 30d Standard → Nearline, 90d delete      |

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
│       ├── cloud-init-nat.yaml      # NAT VPS: SSH hardening, Nomad server, NATS, TLS/certbot, HAProxy,
│       │                            #   block storage, VictoriaMetrics, VictoriaLogs, Grafana (with alerts),
│       │                            #   PgBouncer, backup crons (Postgres, metrics, logs, artifacts)
│       ├── cloud-init-gateway.yaml  # Gateway B standby: HAProxy, Pingora, PgBouncer, failover script
│       └── cloud-init-node.yaml     # Bare metal: SSH hardening, Nomad client, Promtail, node_exporter,
│                                    #   Nomad state backup cron, Firecracker setup (Metal only)
└── nomad/
    ├── api.nomad.hcl              # API — single instance on NAT VPS (node_class=gateway)
    ├── daemon-metal.nomad.hcl     # Daemon — Metal tier only (node_class=metal)
    ├── daemon-liquid.nomad.hcl    # Daemon — Liquid tier only (node_class=liquid)
    ├── proxy.nomad.hcl            # Proxy — single instance on NAT VPS (node_class=gateway)
    └── migrate.nomad.hcl          # Batch job — applies pending SQL migrations via psql
```

Providers used:

| Provider                | Purpose                                                          |
|-------------------------|------------------------------------------------------------------|
| `vultr/vultr`           | Bare metal, VPS, Block Storage, Managed Postgres, Object Storage |
| `cloudflare/cloudflare` | DNS — wildcard + apex → NAT VPS public IP                        |
| `tailscale/tailscale`   | Pre-auth keys for node join                                      |
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

Nomad runs on both bare metal nodes (clients) and the NAT VPS (server + client, `bootstrap_expect=1`). It manages the lifecycle of `api`, `daemon`, and `proxy` — health checks, rolling deploys, restarts on crash. It does **not** schedule Firecracker VMs — that remains the daemon's job via NATS. Schema migrations are run via a separate `migrate.nomad.hcl` batch job dispatched before rolling deploys.

```
infra/nomad/
├── api.nomad.hcl              # api binary, :7070, single instance on NAT VPS (node_class=gateway)
├── daemon-metal.nomad.hcl     # daemon — Metal tier (node_class=metal)
├── daemon-liquid.nomad.hcl    # daemon — Liquid tier (node_class=liquid)
└── proxy.nomad.hcl            # proxy binary, :8443, system job on all gateway nodes (node_class=gateway)
```

Nomad node classes map to role:

```hcl
# NAT VPS — set at agent config
client {
  node_class = "gateway"
}

# node-a-01
client {
  node_class = "metal"
}

# node-b-01
client {
  node_class = "liquid"
}
```

Job specs are constrained to the correct node class. API and proxy run on `gateway` (NAT VPS), daemons run on their respective compute tiers:

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
  → upload to GitHub Release (or Object Storage)
  → SSH into NAT VPS via Tailscale
  → nomad job run api.nomad.hcl   (rolling deploy, zero downtime)
  → nomad job run daemon.nomad.hcl
  → nomad job run proxy.nomad.hcl
```

Firecracker provisioning is unaffected — Nomad only restarts the daemon process, not the VMs it manages.

