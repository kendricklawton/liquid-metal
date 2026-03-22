# Liquid Metal

> **Experimental** — work in progress. Built to learn by doing. Not production-ready.

Bare-metal hosting platform. No Kubernetes. No managed cloud. Two products built on proven isolation primitives. All Rust.

---

## Products

### Metal — Firecracker MicroVMs

Run any Linux binary in a hardware-isolated VM. KVM-backed, dedicated rootfs, TAP networking. Ship a Rust binary, a compiled app, anything that targets Linux.

- **Isolation**: AWS Firecracker + KVM
- **Cold start**: ~100–250ms
- **Networking**: TAP device → br0 bridge → Pingora proxy
- **Storage**: Dedicated ext4 rootfs per service
- **Egress filtering**: eBPF TC classifiers prevent direct inter-VM traffic — all routing goes through Pingora

```toml
# liquid-metal.toml
[service]
name   = "my-app"
engine = "metal"
port   = 8080

[build]
command = "cargo build --target x86_64-unknown-linux-musl --release"
output  = "target/x86_64-unknown-linux-musl/release/my-app"
```

### Liquid — WebAssembly

Run Wasm modules via Wasmtime/WASI. Sub-millisecond cold starts, in-process execution, memory-only. No disk, no TAP, no VM overhead.

- **Isolation**: Wasmtime Wasm sandbox
- **Cold start**: <1ms
- **Execution**: In-process, WASI VFS

```toml
# liquid-metal.toml
[service]
name   = "my-fn"
engine = "liquid"

[build]
command = "cargo build --target wasm32-wasip1 --release"
output  = "target/wasm32-wasip1/release/my-fn.wasm"
```

---

## Stack

All Rust — platform services and CLI.

| Crate | Role |
|---|---|
| `crates/common` | Shared types — `Engine`, `ProvisionEvent`, contracts, pricing, feature flags, config |
| `crates/api` | Axum REST/JSON — `:7070`, writes Postgres, publishes to NATS, Stripe billing |
| `crates/web` | Axum + Askama + HTMX + Alpine.js dashboard — `:3000` |
| `crates/cli` | `flux` CLI — clap + reqwest, calls API over HTTP |
| `crates/proxy` | Pingora edge router — `slug → upstream_addr` via DB lookup, custom domain TLS |
| `crates/daemon` | NATS consumer — Firecracker VMs (Metal) + Wasmtime (Liquid) |
| `crates/ebpf-programs` | TC egress classifiers (Aya) — tenant network isolation for Metal VMs |

---

## Key Features

### Auth

- **CLI**: OIDC device flow via Zitadel → scoped `lm_*` API key (SHA-256 hashed, supports expiration and scopes)
- **Web**: OIDC authorization code + PKCE → AES-GCM encrypted session cookie
- **Internal**: `X-Internal-Secret` + `X-On-Behalf-Of` for web BFF → API calls

### Billing

- **Metal**: Fixed monthly pricing per VM tier ($20/$40/$80 for 1/2/4 vCPU)
- **Liquid**: $0.30 per 1M invocations, 1M free per workspace per month
- **Credits**: Prepaid balance (micro-credits), top-up via Stripe Checkout
- **Invoices**: Auto-generated monthly via Stripe Invoices API (PDF + hosted page)
- **Suspension**: Services paused when balance hits zero, auto-restored on top-up

### Custom Domains & TLS

- Add custom domains to any service via `flux domains add`
- Automatic TLS via Let's Encrypt (ACME HTTP-01 challenge)
- Pingora proxy handles SNI-based routing

### Rate Limiting

- Auth routes: 10 req/min per IP
- API routes: 60 req/min per API key
- BFF routes: 120 req/min per user

### Feature Flags

Environment-variable-driven flags evaluated at request time: `REQUIRE_INVITE`, `ENABLE_METAL`, `ENABLE_LIQUID`, `ENFORCE_QUOTAS`, `MAINTENANCE_MODE`.

---

## Getting Started

```bash
flux login      # authenticate via Zitadel OIDC device flow
flux init       # auto-detects language, creates project, writes liquid-metal.toml
flux deploy     # build locally → upload to S3 → provision (live SSE progress stream)
flux services   # list services in the active workspace (shows failure reasons)
# → live at <name>.liquidmetal.dev
```

---

## Local Dev

```bash
# Start infrastructure
task up            # Postgres + NATS + MinIO (docker compose)
task dev:api       # Rust API on :7070
task dev:web       # Web dashboard on :3000
task dev:proxy     # Pingora on :8080
sudo -E task dev:daemon  # NATS consumer (requires root for KVM/TAP/cgroups)
                         # Wasm-only: DAEMON_PID_FILE=/tmp/lm.pid NODE_ENGINE=liquid task dev:daemon

# Install the CLI
task install:cli   # cargo install → flux lands in ~/.cargo/bin
flux login
flux init          # run from your service directory
flux deploy
flux services
```

### Linux (bare metal, one-time setup)

```bash
task metal:setup     # br0 bridge, /run/firecracker, Firecracker binary
task security:setup  # jailer user, cgroup v2 controllers, eBPF policy
```

---

## Architecture

```
User → flux CLI → API (:7070) → Postgres + NATS
                                       ↓
                              Daemon (NATS consumer)
                              ├── Metal: Firecracker VM
                              └── Liquid: Wasmtime
                                       ↓
                              Proxy (Pingora) ← User traffic
```

- **Data flow invariant**: All mutations go API → Postgres → NATS (outbox) → Daemon. The API is the only Postgres writer. The daemon is the only thing that touches Firecracker or Wasmtime.
- **Outbox pattern**: Events written to an `outbox` table in the same Postgres transaction as the mutation, then published to NATS by a background poller. At-least-once delivery without distributed transactions.
- **UUID v7**: All primary keys are time-ordered UUIDs.

---

## What We Don't Use

- No Kubernetes, K3s, or any container orchestrator
- No AWS, Azure, Vercel, or Heroku for compute (GCP used only for KMS, Terraform state, and disaster recovery backups)
- No Docker in production — Nomad `raw_exec` runs binaries directly
- No ORMs (raw SQL via `tokio-postgres`)
- No gRPC / protobuf (plain REST/JSON between all services)
- No Prometheus (VictoriaMetrics for metrics, VictoriaLogs for logs)

---

> For local dev and deployment see [RUNBOOK.md](RUNBOOK.md). For infrastructure topology and data flow see [ARCHITECTURE.md](ARCHITECTURE.md).
