# Liquid Metal — Architecture

> Bare-metal hosting platform. Two products, no Kubernetes, ever.

---

## Infrastructure Stack

All infrastructure runs in **Vultr Chicago (ORD)**. One vendor, one region, sub-millisecond between every layer.

| Layer              | Technology                              | Notes                                              |
|--------------------|-----------------------------------------|----------------------------------------------------|
| **Floating IP**    | Vultr VPS — Chicago                     | Holds public IP permanently, HAProxy + NATS tiebreaker |
| **Compute**        | 2× Vultr Bare Metal — Chicago           | node-a (primary) + node-b (standby)                |
| **Proxy**          | Pingora (Rust) on both nodes            | TLS termination, slug → upstream routing           |
| **Load balancer**  | HAProxy on NAT VPS                      | Health-checks both nodes, routes :80/:443          |
| **Private mesh**   | Tailscale                               | Official Tailscale — VPS + node-a + node-b         |
| **API**            | Axum + tonic (Rust), :7070              | Active/active on both nodes                        |
| **Daemon**         | NATS consumer (Rust)                    | Active on both nodes, each owns local KVM          |
| **Web UI**         | chi + Templ + HTMX (Go), :3000          | Active/active on both nodes                        |
| **CLI**            | `flux` binary (Go)                      | login, init, deploy, status, logs, workspace, project |
| **Event bus**      | NATS JetStream — 3-node Raft            | node-a + node-b + NAT VPS, survives 1 failure      |
| **Database**       | Vultr Managed Postgres — Chicago        | Managed HA, daily backups, standard pg conn string |
| **Artifact store** | Vultr Object Storage — Chicago          | S3-compatible, rootfs images + .wasm binaries      |
| **DNS**            | Cloudflare                              | Wildcard `*.liquidmetal.dev` → NAT VPS floating IP |
| **TLS**            | Let's Encrypt via Pingora               | Wildcard cert, auto-renewed                        |
| **eBPF isolation** | Aya TC classifier per tap{n}            | Tenant isolation at kernel level, no Cilium daemon |
| **Observability**  | Structured JSON logs → stdout           | Metrics: Prometheus + Grafana (future)             |

---

## Vultr Setup

### Region: Chicago (ORD)

Everything lives in Vultr Chicago. Bare metal, VPS, Managed Postgres, and Object Storage are all in the same datacenter — latency between any two services is <1ms.

### Services Used

| Vultr Service            | Purpose                                        |
|--------------------------|------------------------------------------------|
| **Bare Metal** (×2)      | Firecracker + Wasmtime compute nodes           |
| **Cloud Compute VPS**    | NAT box — floating IP, HAProxy, NATS tiebreaker|
| **Managed Databases**    | PostgreSQL 16 — primary datastore              |
| **Object Storage**       | S3-compatible — rootfs images + .wasm binaries |

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

Vultr Object Storage is S3-compatible. The Rust codebase uses the `aws-sdk-s3` crate pointed at the Vultr endpoint:

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
- Connection string: `postgresql://user:pass@<vultr-db-host>:5432/liquidmetal?sslmode=require`
- No auth proxy needed — standard TLS connection direct to Vultr endpoint

---

## High Availability

### Topology

```
Internet
    │
    ▼ DNS: *.liquidmetal.dev → Floating IP (on NAT VPS)
┌──────────────────────────────┐
│  NAT VPS — Vultr Chicago     │  Cloud Compute VPS, ~$6/mo
│  Holds the public IP         │  HAProxy: :80/:443 → active node
│  NATS node (Raft member 3)   │  Tailscale coordination node
└────────────┬─────────────────┘
             │ Tailscale mesh (100.x.x.x)
    ┌────────┴────────┐
    ▼                 ▼
node-a (primary)   node-b (standby)
Pingora :443       Pingora :443
API :7070          API :7070
Daemon             Daemon
NATS (Raft 1)      NATS (Raft 2)
KVM + Firecracker  KVM + Firecracker
```

The NAT VPS holds the public IP permanently — it never moves. HAProxy health-checks both bare metal nodes and routes to whichever is alive. Tailscale carries all internal traffic between nodes.

### Per-Layer HA Strategy

| Layer              | Strategy                                                        | Failover time |
|--------------------|-----------------------------------------------------------------|---------------|
| **Floating IP**    | Permanent on NAT VPS — never moves                              | N/A           |
| **Pingora**        | Active on node-a, standby on node-b. HAProxy switches          | <5s           |
| **API**            | Active/active on both nodes. HAProxy round-robins              | Instant       |
| **Web UI**         | Active/active on both nodes. HAProxy round-robins              | Instant       |
| **NATS JetStream** | 3-node Raft: node-a + node-b + NAT VPS. Survives 1 failure     | <10s          |
| **Managed Postgres**| Vultr-managed standby + automatic failover                    | <60s          |
| **Object Storage** | Vultr-managed, redundant by default                            | N/A           |
| **Daemon**         | Both nodes run daemon, each owns its local KVM                 | N/A           |

### Metal HA (Warm Standby)

Firecracker VMs are pinned to the node that provisioned them — KVM and TAP are local to each host. There is no live migration. Failover is re-provisioning:

```
node-a dies
  → HAProxy health check detects failure (~5s)
  → HAProxy stops routing to node-a
  → NATS (still quorate on node-b + VPS) re-delivers ProvisionEvent
  → daemon on node-b pulls rootfs.ext4 from Vultr Object Storage
  → boots new Firecracker VM on node-b
  → UPDATE services SET upstream_addr = node-b VM IP, node_id = 'node-b'
  → Pingora (now on node-b) resumes routing
Total: ~30–60s outage for Metal services
```

The `services` table stores `node_id` so the system tracks which node owns each VM. A watchdog in the daemon periodically checks peer health and re-queues provision events for stranded services.

### Liquid HA (Active/Active)

Wasm modules are stateless. The same module runs on both nodes simultaneously:

```
Both nodes run the same .wasm module
HAProxy load-balances across both upstream_addrs
node-a dies → HAProxy removes it → all traffic to node-b
Total: <5s, no re-provisioning needed
```

The `services` table for Liquid stores multiple `upstream_addr` entries. Pingora uses all healthy ones.

### NATS JetStream Cluster

```
# /etc/nats/nats.conf on each node
cluster {
  name: liquid-metal
  listen: 0.0.0.0:6222
  routes: [
    nats://10.0.0.1:6222   # NAT VPS
    nats://10.0.0.2:6222   # node-a
    nats://10.0.0.3:6222   # node-b
  ]
}

jetstream {
  store_dir: /var/lib/nats
}
```

3-node cluster tolerates 1 failure. The NAT VPS is the tiebreaker — it runs NATS only, no daemon or API.

### Network Architecture (HA)

```
Internet
    │
    ▼ (*.liquidmetal.dev → 1 floating IP on NAT VPS)
NAT VPS — Vultr Chicago (HAProxy :80/:443, Tailscale node)
    │ Tailscale mesh (100.x.x.x CGNAT range)
    ├──────────────────────────────────┐
    ▼                                  ▼
node-a                              node-b
Pingora (:443)                      Pingora (:443)
API (:7070)                         API (:7070)
Web (:3000)                         Web (:3000)
NATS (:4222/:6222)                  NATS (:4222/:6222)
Daemon                              Daemon
KVM + br0 + TAP devices             KVM + br0 + TAP devices
    │                                   │
    └───────────────┬───────────────────┘
                    ▼
        Vultr Managed Postgres (Chicago)
        Vultr Object Storage (Chicago)
```

**Internal mesh (Tailscale):**
- Official Tailscale — each node joins the same Tailscale network
- Tailscale assigns 100.x.x.x addresses (CGNAT range) to each node
- All inter-service traffic (API→NATS, daemon→NATS, Postgres) uses Tailscale IPs
- No manual key management, no peer config files — Tailscale handles it

**Exposed ports (NAT VPS only — bare metal nodes have no public ports):**
- `:80` — HAProxy (redirect to HTTPS)
- `:443` — HAProxy (user traffic)
- `:3478` — STUN (UDP, Tailscale NAT traversal)

**Bare metal nodes — public firewall: all closed.**
Tailscale handles all connectivity. SSH is via `tailscale ssh` — no public port 22.

---

## eBPF Tenant Isolation (Aya)

### Why — The Multi-Tenant Bridge Problem

All Firecracker VMs on a node share the same `br0` bridge. Without enforcement,
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

`tc.rs` manages tbf qdiscs for rate limiting (L3 bandwidth).
`ebpf.rs` manages the TC classifier for identity enforcement (L3 isolation).
Both attach to the same `tap{n}` device and operate independently.

```
tap{n} egress:
  1. tbf qdisc (tc.rs)      → rate limit to net_egress_kbps
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

deprovision (future):
  ebpf::detach(tap)                  # unload BPF program
  tc::remove(tap)                    # remove qdiscs
  netlink::remove_tap(tap)           # delete tap{n}
```

### Build Requirements (Linux only)

```bash
rustup target add bpfel-unknown-none
rustup component add rust-src
# daemon build.rs handles the rest automatically
cargo build -p daemon
```

On macOS, the eBPF build is skipped entirely. The `#[cfg(target_os = "linux")]`
gate in `ebpf.rs` ensures the embedded bytes are never referenced in a macOS
binary.

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
the artifact at `{service_id}/{deploy_id}/` never changes. This makes rollback
trivial: re-provision with an older `deploy_id`.

### Why No Container Registry

Container registries (Docker Hub, GHCR, ECR) exist for OCI image layers. Liquid
Metal uses a different model:

- **Metal**: user ships a static Linux binary → daemon injects it into the base
  Alpine rootfs template → boots as a Firecracker VM. No Dockerfile, no layers.
- **Liquid**: user ships a `.wasm` file → daemon loads it into Wasmtime. No
  image format at all.

Object Storage provides everything needed: immutable versioned storage, pre-signed
upload URLs (so the large artifact bypasses the API server), and lifecycle
policies for cleanup.

---

## Data Flow

### Deploy

```
flux deploy
  1. read liquid-metal.toml → engine, name, build command
  2. build locally:
       metal:  go build -o app . / cargo build --release
       liquid: GOOS=wasip1 go build -o main.wasm .
  3. sha256(artifact) + generate deploy_id (uuid_v7)
  4. GetUploadUrl RPC → API returns pre-signed Object Storage PUT URL
  5. PUT artifact → Object Storage directly (no API in the upload path)
  6. Deploy RPC { slug, engine, spec, artifact_key, deploy_id, sha256 }
       → API inserts service row → Managed Postgres
       → API publishes ProvisionEvent → NATS JetStream

NATS → daemon
  ├─ metal:  download base-alpine.ext4 from Object Storage (cached locally)
  │           download user binary from Object Storage
  │           loop-mount base → inject binary → unmount → rootfs.ext4
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

### Web UI

```
Browser (HTMX)
  └─→ Go web :3000 (chi + Templ)
        └─→ ConnectRPC / h2c (protobuf)
              └─→ Rust API :7070 (tonic + Axum)
                    └─→ Managed Postgres
```

Go web has **zero direct database or NATS access**. All data flows through the Rust API via ConnectRPC.

---

## Codebase Layout

```
liquid-metal/
├── crates/
│   ├── common/     Shared Rust types (Engine, ProvisionEvent, EngineSpec, slugify)
│   ├── api/        Axum + tonic — ConnectRPC server :7070, publishes to NATS
│   ├── proxy/      Pingora edge router — slug → upstream_addr
│   └── daemon/     NATS consumer — Firecracker + Wasmtime provision loop
├── cli/            flux CLI (Go) — login, init, deploy, status, logs, workspace, project
│   ├── main.go
│   └── cmd/        Cobra commands
├── web/            Go dashboard — chi + Templ + HTMX, ConnectRPC client, :3000
│   ├── cmd/web/    chi server entry point
│   └── internal/
│       ├── handler/ HTMX request handlers
│       └── ui/      Templ components + pages
├── mcp/            MCP server (Go) — exposes Liquid Metal ops as tools for Claude agents
├── gen/go/         buf-generated Go protobuf + connect stubs (shared via go.work)
├── proto/          Protobuf definitions — buf generates Rust (tonic) + Go (connect-go) stubs
├── migrations/     PostgreSQL migrations (refinery, embedded in api)
└── ARCHITECTURE.md This file
```

---

## Proto / ConnectRPC

- Definitions in `proto/` — the only contract between Go and Rust
- `buf` generates:
  - Rust stubs (tonic) → consumed by `crates/api`
  - Go stubs (connect-go) → consumed by `web/` and `cli/`
- Transport: ConnectRPC over h2c (HTTP/2 cleartext, internal Tailscale mesh only)

---

## Environment Variables

| Variable                      | Used by            | Description                                   |
|-------------------------------|--------------------|-----------------------------------------------|
| `DATABASE_URL`                | api, proxy, daemon | Vultr Managed Postgres connection string       |
| `NATS_URL`                    | api, daemon        | NATS JetStream address (Tailscale IP)          |
| `BIND_ADDR`                   | api, proxy, web    | Listen address                                 |
| `API_URL`                     | web                | Rust API ConnectRPC endpoint                   |
| `OBJECT_STORAGE_ENDPOINT`     | api, daemon        | Vultr Object Storage endpoint (S3-compat)      |
| `OBJECT_STORAGE_BUCKET`       | api, daemon        | Bucket name for artifacts                      |
| `OBJECT_STORAGE_ACCESS_KEY`   | api, daemon        | Vultr Object Storage access key                |
| `OBJECT_STORAGE_SECRET_KEY`   | api, daemon        | Vultr Object Storage secret key                |
| `ANTHROPIC_API_KEY`           | mcp                | Anthropic API key for Claude agent features    |
| `FLUX_API_KEY`                | mcp, cli           | Liquid Metal API key (X-Api-Key header)        |

---

## MCP Server

The `mcp/` service exposes Liquid Metal operations as tools for Claude and other MCP-compatible agents. It follows the same architectural constraint as `web/` — no direct database or NATS access, all data flows through the Rust API via ConnectRPC.

### Data Flow

```
Claude / Agent SDK client
    │ MCP (JSON-RPC over stdio or SSE)
    ▼
mcp/        Go MCP server — tool definitions + ConnectRPC client
    │ ConnectRPC / h2c
    ▼
Rust API :7070
    │
    ├─→ Managed Postgres
    └─→ NATS JetStream
```

### Transport

| Mode   | Use case                                          |
|--------|---------------------------------------------------|
| stdio  | Local dev — Claude Desktop, Claude Code           |
| SSE    | Production — hosted agent pipelines               |

### Auth

The MCP server authenticates to the Rust API using `X-Api-Key: $FLUX_API_KEY` — the same header used by the CLI. No separate auth layer.

### Constraint: No Destructive Tools Without Confirmation

Tools that modify or delete resources require an explicit `confirm: true` argument. An agent cannot accidentally deprovision a running service.

---

## Hard Constraints

- **No Kubernetes, K3s, or any orchestrator**
- **No managed compute** — bare metal for all execution (Vultr Managed Postgres + Object Storage are fine)
- **No ORMs** — tokio-postgres with raw SQL (Rust); Go web has zero DB access
- **No SPA frameworks** — HTMX + Templ only, Alpine.js sparingly
- **No hardcoded addresses** — all config via env vars
- **No rounded corners** — `rounded-none` everywhere in UI
- **Firecracker + TAP are Linux-only** — gated with `#[cfg(target_os = "linux")]`
- **Wasmtime runs on all platforms** — safe for local macOS dev
- **Go web never touches Postgres or NATS directly**
