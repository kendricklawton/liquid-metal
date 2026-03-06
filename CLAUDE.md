# liquid-metal — Agent Protocol

## Mission: Two Products, No Kubernetes, Ever

### Product 1 — Metal (Firecracker)
Hardware-isolated microVMs via AWS Firecracker + KVM. Users ship a Linux binary, we boot it in a VM with a dedicated ext4 rootfs and TAP networking. ~100–250ms cold start.

### Product 2 — Flash (WebAssembly)
In-process Wasm execution via Wasmtime/WASI. Users ship a `.wasm` file, we execute it directly. Sub-millisecond cold starts, memory-only, no disk, no TAP.

**No Kubernetes. No K3s. No managed cloud. No exceptions.**

---

## Architecture

```
Internet
    │
    ▼
Pingora Edge Proxy  (proxy crate, :80/:443)
*.machinename.dev → slug → upstream_addr
    │
    ├─────────────────────────┐
    ▼                         ▼
engine: metal            engine: flash
Firecracker microVM      Wasmtime executor
TAP → br0                in-process, <1ms
~100–250ms cold start    no disk, no TAP
```

---

## Crate Map (Rust)

```
crates/
├── common/   Engine enum, EngineSpec, ProvisionEvent, config helpers
├── api/      Axum + tonic — ConnectRPC server, publishes ProvisionEvent to NATS
├── proxy/    Pingora — slug → upstream_addr DB lookup → route
├── daemon/   NATS consumer — Firecracker provisioner + Wasmtime executor
└── cli/      plat binary — init, deploy, status, logs
```

## Web (Go)

```
web/
├── cmd/web/          chi server, ConnectRPC client, :3000
└── internal/
    ├── handler/      HTMX request handlers (HX-Request/HX-Target detection)
    └── ui/           Templ components + pages
```

---

## Data Flow

```
plat deploy
  → POST /services          (api crate — Axum)
  → publish ProvisionEvent  (NATS JetStream)
  → daemon consumes
      metal → spawn Firecracker → boot VM → write upstream_addr
      flash → wasmtime::execute(_start) → write upstream_addr
  → proxy routes slug → upstream_addr
```

## Web Data Flow

```
Browser (HTMX)
  → Go web handler (chi)
  → ConnectRPC call (protobuf/h2c)
  → Rust API (tonic + Axum)
  → tokio-postgres
  → Postgres
```

Go web has **zero direct DB or NATS access**. All data flows through the Rust API via ConnectRPC.

---

## Proto / ConnectRPC

- Definitions live in `proto/` at workspace root
- `buf` generates:
  - Rust stubs → consumed by `crates/api` (tonic)
  - Go stubs → consumed by `web/` (connectrpc/connect-go)
- The proto contract is the boundary between Go and Rust

---

## machine.toml (user config)

```toml
# Metal
[service]
name   = "my-app"
engine = "metal"
port   = 8080

[metal]
vcpu      = 1
memory_mb = 128
```

```toml
# Flash
[service]
name   = "my-fn"
engine = "flash"

[flash]
wasm = "main.wasm"
```

---

## Infrastructure

- **Node**: Hivelocity bare metal, Dallas
- **Network**: Floating IP → Pingora → TAP/br0 (metal) or in-process (flash)
- **Event bus**: NATS JetStream
- **State**: PostgreSQL — raw SQL, refinery migrations in `migrations/`
- **Artifacts**: RustFS (S3-compat) — rootfs images + wasm binaries

---

## Hard Constraints

- **No Kubernetes, K3s, or any orchestrator**
- **No AWS/GCP/Azure/Vercel/Heroku** — bare metal only
- **No ORMs** — tokio-postgres (Rust), sqlc + pgx (Go)
- **No SPA frameworks** — HTMX + Templ only, Alpine.js sparingly
- **No rounded corners** — `rounded-none` everywhere in UI
- **No hardcoded addresses** — all config via env vars (`DATABASE_URL`, `NATS_URL`, `BIND_ADDR`, `API_URL`)
- **Firecracker + TAP are Linux-only** — gate with `#[cfg(target_os = "linux")]`
- **wasmtime runs on all platforms** — safe for local macOS dev
- **Go web never touches Postgres or NATS directly**

---

## Dev Workflow

```bash
task up           # Postgres + NATS (docker compose)
task dev:api      # Rust API :7070
task dev:web      # Go web :3000
task dev:proxy    # Pingora :8080
task dev:daemon   # NATS consumer (Firecracker skipped on macOS)
task dev:cli -- deploy
```

### Linux bare-metal setup (once, as root)
```bash
task metal:setup  # br0, /run/firecracker, Firecracker binary
```
