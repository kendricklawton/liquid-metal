# liquid-metal

> **Experimental** — work in progress. Built to learn by doing. Not production-ready.

Bare-metal hosting platform. No Kubernetes. No managed cloud. Two products built on proven isolation primitives.

---

## Products

### Metal — Firecracker MicroVMs
Run any Linux binary in a hardware-isolated VM. KVM-backed, dedicated rootfs, TAP networking. Ship a Go binary, an HTMX app, anything that compiles.

- **Isolation**: AWS Firecracker + KVM
- **Cold start**: ~100–250ms
- **Networking**: TAP device → br0 bridge → Pingora proxy
- **Storage**: Dedicated ext4 rootfs per service

```toml
# machine.toml
[service]
name   = "my-app"
engine = "metal"
port   = 8080

[metal]
vcpu      = 1
memory_mb = 128
```

### Liquid — WebAssembly
Run Wasm modules via Wasmtime/WASI. Sub-millisecond cold starts, in-process execution, memory-only. No disk, no TAP, no VM overhead.

- **Isolation**: Wasmtime Wasm sandbox
- **Cold start**: <1ms
- **Execution**: In-process, WASI VFS

```toml
# machine.toml
[service]
name   = "my-fn"
engine = "liquid"

[liquid]
wasm = "main.wasm"
```

---

## Architecture

```
Internet
    │
    ▼
Pingora Edge Proxy  (:80/:443)
*.machinename.dev → slug lookup → upstream_addr
    │
    ├──────────────────────────┐
    ▼                          ▼
engine: metal             engine: liquid
Firecracker microVM       Wasmtime executor
TAP → br0                 in-process, <1ms
~100–250ms cold start     no disk, no TAP
```

**Stack:**
- **Proxy**: Pingora (Rust) — slug → upstream_addr routing
- **API**: Axum + tonic (Rust) — service CRUD, ConnectRPC over h2c
- **Daemon**: NATS consumer (Rust) — provisions VMs and Wasm
- **Web**: chi + Templ + HTMX (Go) — dashboard UI on :3000
- **CLI**: Cobra + Viper (Go) — `flux` developer tool
- **Event bus**: NATS JetStream 3-node Raft
- **State**: PostgreSQL — raw SQL, refinery migrations
- **Artifacts**: Vultr Object Storage (S3-compatible) — rootfs images + wasm binaries

---

## Repository Layout

```
liquid-metal/
├── go.work             Go workspace (web, cli, gen/go)
├── Cargo.toml          Rust workspace
│
├── crates/
│   ├── common/         Shared types: Engine, ProvisionEvent, config helpers
│   ├── api/            Axum + tonic — ConnectRPC server :7070, publishes to NATS
│   ├── proxy/          Pingora edge router — slug → upstream_addr
│   ├── daemon/         NATS consumer — Firecracker + Wasmtime provision loop
│   └── ebpf-programs/  TC egress classifier — cross-tenant VM isolation (BPF)
│
├── web/                Go module — dashboard (chi + Templ + HTMX) on :3000
│   ├── cmd/web/        Server entry point
│   └── internal/
│       ├── config/     Config loading from env vars
│       ├── handler/    HTMX request handlers
│       └── ui/         Templ components + pages
│
├── cli/                Go module — flux CLI (Cobra + Viper)
│   ├── main.go         Entry point
│   └── cmd/            Commands: login, whoami, deploy, status, logs
│
├── proto/              Protobuf definitions
├── gen/go/             buf-generated Go stubs (ConnectRPC)
└── migrations/         PostgreSQL migrations (refinery)
```

---

## Deploy in 3 Commands

```bash
flux login      # authenticate via browser (WorkOS)
flux deploy     # reads machine.toml → ships to node
flux status     # list your services
# → live at <name>.machinename.dev
```

---

## Local Dev

```bash
task up           # Postgres + NATS + RustFS (docker compose)
task dev:api      # Rust API on :7070
task dev:web      # Go dashboard on :3000 (air hot reload)
task dev:proxy    # Pingora on :8080
task dev:daemon   # NATS consumer (Firecracker skipped on macOS)
task dev:cli -- status   # run flux CLI
```

### Linux (bare metal, one-time setup)
```bash
task metal:setup     # br0 bridge, /run/firecracker, Firecracker binary
task security:setup  # jailer user, cgroup v2 controllers, eBPF policy
```

---

## What We Don't Use

- No Kubernetes, K3s, or any orchestrator
- No AWS, GCP, Azure, Vercel, or Heroku
- No ORMs (raw SQL everywhere)
- No SPA frameworks (HTMX + Templ only)
- No container registry (Object Storage is the registry)
