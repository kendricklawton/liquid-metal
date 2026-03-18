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
| `crates/common` | Shared types — `Engine`, `ProvisionEvent`, artifact integrity, networking primitives |
| `crates/api` | Axum REST/JSON — `:7070`, writes Postgres, publishes to NATS |
| `crates/web` | Axum + Askama + HTMX + Alpine.js dashboard — `:3000` |
| `crates/cli` | `flux` CLI — clap + reqwest, calls API over HTTP |
| `crates/proxy` | Pingora edge router — `slug → upstream_addr` via DB lookup |
| `crates/daemon` | NATS consumer — Firecracker VMs (Metal) + Wasmtime (Liquid) |

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
task dev:daemon    # NATS consumer (Firecracker skipped on macOS)

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

## What We Don't Use

- No Kubernetes, K3s, or any container orchestrator
- No AWS, Azure, Vercel, or Heroku for compute (GCP used only for KMS, Terraform state, and disaster recovery backups)
- No ORMs (raw SQL via `tokio-postgres`)
- No container registry (Vultr Object Storage is the registry)
- No gRPC / protobuf (plain REST/JSON between all services)
- No Prometheus (VictoriaMetrics for metrics, VictoriaLogs for logs)

---

> For local dev and deployment see [RUNBOOK.md](RUNBOOK.md). For infrastructure topology and data flow see [ARCHITECTURE.md](ARCHITECTURE.md).
