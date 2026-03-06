# liquid-metal

Bare-metal hosting platform. No Kubernetes. No managed cloud. Two products built on proven isolation primitives.

---

## Products



### Liquid — WebAssembly
Run Wasm modules via Wasmtime/WASI. Sub-millisecond cold starts, in-process execution, memory-only. No disk, no TAP, no VM overhead.

- **Isolation**: Wasmtime Wasm sandbox
- **Cold start**: <1ms
- **Execution**: In-process, WASI VFS
- **Build**: `GOOS=wasip1 go build -o main.wasm`

```toml
# machine.toml
[service]
name   = "my-function"
engine = "flash"

[flash]
wasm = "main.wasm"
```

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

---

## Architecture

```
Internet
    │
    ▼
Pingora Edge Proxy  (:80/:443)
*.machinename.dev → slug lookup
    │
    ├──────────────────────────┐
    ▼                          ▼
engine: metal             engine: flash
Firecracker microVM       Wasmtime executor
TAP → br0                 in-process, <1ms
~100–250ms cold start     no disk, no TAP
```

**Stack:**
- **Proxy**: Pingora (Rust) — slug → upstream_addr routing
- **API**: Axum + tonic (Rust) — service CRUD, ConnectRPC
- **Daemon**: NATS consumer (Rust) — provisions VMs and Wasm
- **Web**: chi + Templ + HTMX (Go) — dashboard UI
- **Event bus**: NATS JetStream
- **State**: PostgreSQL (raw SQL, no ORM)
- **Artifacts**: RustFS (S3-compatible) — rootfs images + wasm binaries

---

## Repository Layout

```
liquid-metal/
├── crates/
│   ├── common/     Shared Rust types (Engine, ProvisionEvent, config)
│   ├── api/        Axum + tonic — REST + ConnectRPC, publishes to NATS
│   ├── proxy/      Pingora edge router
│   ├── daemon/     Firecracker + Wasmtime provision loop
│   └── cli/        plat binary (init, deploy, status, logs)
├── web/            Go dashboard (Templ + HTMX + ConnectRPC client)
├── proto/          Protobuf definitions → buf generates Rust + Go stubs
└── migrations/     PostgreSQL migrations (refinery)
```

---

## Deploy in 3 Commands

```bash
plat init       # create machine.toml
plat deploy     # build + ship to node
plat status     # check health
# → live at <name>.machinename.dev
```

---

## Local Dev

```bash
task up           # Postgres + NATS via docker compose
task dev:api      # Rust API on :7070
task dev:web      # Go dashboard on :3000
task dev:proxy    # Pingora on :8080
task dev:daemon   # NATS consumer (Firecracker skipped on macOS)
```

### Linux (bare metal, one-time setup)
```bash
task metal:setup  # br0 bridge, /run/firecracker, Firecracker binary
```

---

## What We Don't Use

- No Kubernetes, K3s, or any orchestrator
- No AWS, GCP, Azure, Vercel, or Heroku
- No ORMs (raw SQL everywhere)
- No SPA frameworks (HTMX + Templ only)
