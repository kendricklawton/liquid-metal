# Liquid Metal — Architecture

> Bare-metal hosting platform. Two products, no Kubernetes, ever.

> **Status**: Implementation in progress. Infrastructure described below is the target topology — not yet provisioned.

---

## Infrastructure Stack

All infrastructure will run in **Vultr Chicago (ORD)**. One vendor, one region, sub-millisecond between every layer.

| Layer                 | Technology                              | Notes                                                                              |
|-----------------------|-----------------------------------------|------------------------------------------------------------------------------------|
| **Floating IP**       | Vultr VPS — Chicago                     | Holds public IP permanently, HAProxy + NATS tiebreaker                             |
| **Compute (Metal)**   | 2× Vultr Bare Metal — Chicago           | node-a-01 (primary) + node-a-02 (standby) — Firecracker only                       |
| **Compute (Liquid)**  | 2× Vultr Bare Metal — Chicago           | node-b-01 (primary) + node-b-02 (standby) — Wasmtime only                          |
| **Proxy**             | Pingora (Rust) on all compute nodes     | TLS termination, slug → upstream routing                                           |
| **Load balancer**     | HAProxy on NAT VPS                      | Health-checks all nodes, routes :80/:443                                           |
| **Private mesh**      | Tailscale                               | Official Tailscale — VPS + all 4 bare metal nodes                                  |
| **API**               | Axum (Rust), :7070                      | Active/active on all nodes                                                         |
| **Web**               | Axum + Askama + HTMX (Rust), :3000      | Dashboard — server-rendered HTML, OIDC browser auth. *Planned.*                    |
| **Daemon**            | NATS consumer (Rust)                    | Each node owns its engine — Metal nodes run Firecracker, Liquid nodes run Wasmtime |
| **CLI**               | `flux` binary (Rust)                    | login, init, deploy, status, logs, workspace, project                              |
| **Event bus**         | NATS JetStream — 5-node Raft            | All 4 nodes + NAT VPS, survives 2 failures                                         |
| **Database**          | Vultr Managed Postgres — Chicago        | Managed HA, daily backups, standard pg conn string                                 |
| **Artifact store**    | Vultr Object Storage — Chicago          | S3-compatible, rootfs images + .wasm binaries                                      |
| **DNS**               | Cloudflare                              | Wildcard `*.tobedetermined.dev` → NAT VPS floating IP                                 |
| **TLS**               | Let's Encrypt via Pingora               | Wildcard cert, auto-renewed                                                        |
| **eBPF isolation**    | Aya TC classifier per tap{n}            | Tenant isolation at kernel level, no Cilium daemon                                 |
| **Observability**     | Structured JSON logs → stdout           | Metrics: Prometheus + Grafana (future)                                             |
---

## Vultr Setup

> **Planned** — not yet provisioned.

### Region: Chicago (ORD)

Everything lives in Vultr Chicago. Bare metal, VPS, Managed Postgres, and Object Storage in the same datacenter — latency between any two services is <1ms.

### Services Used

| Vultr Service            | Purpose                                                       |
|--------------------------|---------------------------------------------------------------|
| **Bare Metal** (×2)      | node-a-01 + node-a-02 — Firecracker/Metal tier                |
| **Bare Metal** (×2)      | node-b-01 + node-b-02 — Wasmtime/Liquid tier                  |
| **Cloud Compute VPS**    | NAT box — floating IP, HAProxy, NATS tiebreaker               |
| **Managed Databases**    | PostgreSQL 16 — primary datastore                             |
| **Object Storage**       | S3-compatible — rootfs images + .wasm binaries                |

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
- Backups: daily automated, 7-day retention
- Connection string: `postgresql://user:pass@<vultr-db-host>:5432/tobedetermined?sslmode=require`

---

## High Availability

> **Planned** — target topology once infrastructure is provisioned.

### Topology

```
Internet
    │
    ▼ DNS: *.tobedetermined.dev → Floating IP (on NAT VPS)
┌──────────────────────────────────┐
│  NAT VPS — Vultr Chicago         │  Cloud Compute VPS
│  Holds the public IP permanently │  HAProxy: :80/:443 → healthy nodes
│  NATS (Raft tiebreaker)          │  Tailscale coordination node
└────────────┬─────────────────────┘
             │ Tailscale mesh (100.x.x.x)
    ┌────────┴────────────────────────────┐
    │                                     │
  Metal tier                          Liquid tier
    │                                     │
  ┌─┴──────────────┐             ┌────────┴────────┐
  ▼                ▼             ▼                  ▼
node-a-01        node-a-02    node-b-01          node-b-02
(primary)        (standby)    (active)           (active)
Pingora :443     Pingora :443 Pingora :443       Pingora :443
API :7070        API :7070    API :7070           API :7070
Daemon           Daemon       Daemon              Daemon
NATS (Raft 1)    NATS (Raft 2) NATS (Raft 3)    NATS (Raft 4)
KVM + Firecracker KVM + Firecracker  Wasmtime    Wasmtime
```

The NAT VPS holds the public IP permanently — it never moves. HAProxy health-checks all four bare metal nodes and routes Metal deploys to the node-a tier, Liquid deploys to the node-b tier. Tailscale carries all internal traffic.

### Per-Layer HA Strategy

| Layer                 | Strategy                                                                      | Failover time |
|-----------------------|-------------------------------------------------------------------------------|---------------|
| **Floating IP**       | Permanent on NAT VPS — never moves                                            | N/A           |
| **Pingora**           | Active/standby per tier. HAProxy switches within tier on failure              | <5s           |
| **API**               | Active/active on all 4 nodes. HAProxy round-robins per tier                   | Instant       |
| **NATS JetStream**    | 5-node Raft: all 4 bare metal + NAT VPS. Survives 2 failures                  | <10s          |
| **Managed Postgres**  | Vultr-managed standby + automatic failover                                    | <60s          |
| **Object Storage**    | Vultr-managed, redundant by default                                           | N/A           |
| **Metal Daemon**      | node-a-01 primary, node-a-02 standby. Re-provision on failover (~30–60s)      | ~30–60s       |
| **Liquid Daemon**     | node-b-01 + node-b-02 active/active. Stateless, no re-provision needed        | <5s           |

### Metal HA (Warm Standby)

Firecracker VMs are pinned to the node that provisioned them — KVM and TAP are local to each host. There is no live migration. Failover is re-provisioning within the Metal tier:

```
node-a-01 dies
  → HAProxy health check detects failure (~5s)
  → HAProxy stops routing Metal traffic to node-a-01
  → NATS (still quorate — 4 remaining nodes) re-delivers ProvisionEvent
  → daemon on node-a-02 pulls rootfs.ext4 from Vultr Object Storage
  → boots new Firecracker VM on node-a-02
  → UPDATE services SET upstream_addr = node-a-02 VM IP, node_id = 'node-a-02'
  → Pingora resumes routing
Total: ~30–60s outage for Metal services
```

The `services` table stores `node_id` so the system tracks which node owns each VM. node-b-01/02 (Liquid tier) are unaffected by Metal node failures.

### Liquid HA (Active/Active)

Wasm modules are stateless. The same module runs on both Liquid nodes simultaneously:

```
node-b-01 + node-b-02 both run the same .wasm module
HAProxy load-balances across both upstream_addrs
node-b-01 dies → HAProxy removes it → all traffic to node-b-02
Total: <5s, no re-provisioning needed
```

node-a-01/02 (Metal tier) are unaffected by Liquid node failures.

### NATS JetStream Cluster

```
# /etc/nats/nats.conf on each node
cluster {
  name: liquid-metal
  listen: 0.0.0.0:6222
  routes: [
    nats://10.0.0.1:6222   # NAT VPS      (NATS only — no daemon/API)
    nats://10.0.0.2:6222   # node-a-01    (Metal primary)
    nats://10.0.0.3:6222   # node-a-02    (Metal standby)
    nats://10.0.0.4:6222   # node-b-01    (Liquid primary)
    nats://10.0.0.5:6222   # node-b-02    (Liquid standby)
  ]
}

jetstream {
  store_dir: /var/lib/nats
}
```

5-node cluster tolerates 2 simultaneous failures. The NAT VPS is the tiebreaker — it runs NATS only, no daemon or API.

### Network Architecture (HA)

```
Internet
    │
    ▼ (*.tobedetermined.dev → 1 floating IP on NAT VPS)
NAT VPS — Vultr Chicago (HAProxy :80/:443, NATS tiebreaker, Tailscale node)
    │ Tailscale mesh (100.x.x.x CGNAT range)
    ├──────────────────────┬───────────────────────────┐
    │                      │                           │
  Metal tier           Liquid tier                     │
    │                      │                           │
  ┌─┴──────────┐     ┌─────┴──────┐                    │
  ▼            ▼     ▼            ▼                    │
node-a-01  node-a-02  node-b-01  node-b-02             │ 
Pingora    Pingora    Pingora    Pingora               │
API :7070  API :7070  API :7070  API :7070             │
NATS       NATS       NATS       NATS                  │
Daemon     Daemon     Daemon     Daemon                │
KVM+br0    KVM+br0    Wasmtime   Wasmtime              │
TAP devs   TAP devs                                    │
    │            │         │           │               │
    └────────────┴─────────┴───────────┴───────────────┘
                                ▼
                    Vultr Managed Postgres (Chicago)
                    Vultr Object Storage (Chicago)
```

**Internal mesh (Tailscale):**
- Official Tailscale — each node joins the same Tailscale network
- All inter-service traffic (API→NATS, daemon→NATS, Postgres) uses Tailscale IPs
- No manual key management — Tailscale handles it

**Exposed ports (NAT VPS only — bare metal nodes have no public ports):**
- `:80` — HAProxy (redirect to HTTPS)
- `:443` — HAProxy (user traffic)
- `:3478` — STUN (UDP, Tailscale NAT traversal)

**Bare metal nodes — public firewall: all closed.**
SSH is via `tailscale ssh` — no public port 22.

---

## eBPF Tenant Isolation (Aya)

> Applies to **Metal tier only** (node-a-01 + node-a-02). Liquid nodes run Wasmtime in-process — no TAP devices, no bridge, no eBPF needed.

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
       → API inserts service row → Managed Postgres
       → API publishes ProvisionEvent → NATS JetStream

NATS → daemon
  ├─ metal:  download base-alpine.ext4 from Object Storage (cached locally)
  │           download user binary from Object Storage
  │           inject binary into rootfs template
  │           attach eBPF TC filter (ebpf.rs)
  │           spawn Firecracker → boot VM
  │           UPDATE services SET upstream_addr, node_id, status='running'
  └─ liquid: download main.wasm from Object Storage
             load into Wasmtime gateway
             UPDATE services SET upstream_addr, status='running'
```

### Live Request

```
Browser → Pingora
  └─ slug lookup → Managed Postgres → upstream_addr
      ├─ metal:  proxy → TAP → Firecracker VM
      └─ liquid: dispatch → in-process Wasmtime executor → response
```

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
└── migrations/        PostgreSQL migrations (refinery, embedded in api)
```

---

## Environment Variables

| Variable                      | Used by            | Description                                                 |
|-------------------------------|--------------------|-------------------------------------------------------------|
| `DATABASE_URL`                | api, proxy, daemon | Vultr Managed Postgres connection string                    |
| `NATS_URL`                    | api, daemon        | NATS JetStream address (Tailscale IP)                       |
| `BIND_ADDR`                   | api, proxy         | Listen address                                              |
| `INTERNAL_SECRET`             | api                | Shared secret for internal provisioning route               |
| `OBJECT_STORAGE_ENDPOINT`     | api, daemon        | Vultr Object Storage endpoint (S3-compat)                   |
| `OBJECT_STORAGE_BUCKET`       | api, daemon        | Bucket name for artifacts                                   |
| `OBJECT_STORAGE_ACCESS_KEY`   | api, daemon        | Vultr Object Storage access key                             |  
| `OBJECT_STORAGE_SECRET_KEY`   | api, daemon        | Vultr Object Storage secret key                             |
| `FLUX_API_KEY`                | cli                | Liquid Metal API key (X-Api-Key header)                     |
| `FLUX_API_URL`                | cli                | API base URL                                                |
| `NODE_ID`                     | daemon             | Identifies which bare metal node this is (e.g. `node-a-01`) |
| `NODE_ENGINE`                 | daemon             | `metal` or `liquid` — which engine this node runs           |
| `FC_BIN`                      | daemon             | Path to Firecracker binary (Linux, Metal nodes only)        |
| `FC_KERNEL_PATH`              | daemon             | Path to guest kernel vmlinux (Linux)                        |
| `BRIDGE`                      | daemon             | TAP bridge name, e.g. `br0` (Linux)                         |
| `ARTIFACT_DIR`                | daemon             | Local artifact cache directory                              |

---

## Hard Constraints

- **No Kubernetes, K3s, or any orchestrator**
- **No managed compute** — bare metal for execution (Vultr Managed Postgres + Object Storage are fine)
- **No ORMs** — `tokio-postgres` with raw SQL everywhere
- **No hardcoded addresses** — all config via env vars
- **Firecracker + TAP are Linux-only** — gated with `#[cfg(target_os = "linux")]`
- **Wasmtime runs on all platforms** — safe for local macOS dev
- **Rust-only codebase** — Zig used only as a cross-compilation linker for Windows CLI builds
