# Machine Name — System Protocol

## Architecture: Single-Node, Two Engines

No Kubernetes. One bare-metal node. Two micro-runtimes. Everything is Rust.

```
Internet
    │
    ▼
┌─────────────────────────────────────┐
│  Pingora Edge Proxy  (proxy crate)  │  ← binds to public IP :80/:443
│  *.machinename.dev → slug lookup    │
└──────────────┬──────────────────────┘
               │
    ┌──────────┴──────────┐
    │                     │
    ▼                     ▼
engine: metal        engine: flash
Firecracker VM       Wasmtime executor
TAP → br0            in-process, <1ms
~100ms cold start    no disk, no TAP
```

## The Two Engines

| Feature       | Metal (Firecracker)              | Flash (Wasmtime/WASI)          |
|---------------|----------------------------------|--------------------------------|
| Isolation     | Hardware KVM                     | Wasm sandbox                   |
| Cold start    | ~100–250ms                       | <1ms                           |
| Filesystem    | Dedicated ext4 rootfs            | Memory-only (WASI VFS)         |
| Networking    | TAP device → br0 bridge          | Host port via proxy            |
| Best for      | Go APIs, HTMX apps, any binary   | Functions, fast middleware     |
| User builds   | `go build -o main` → ext4 image  | `GOOS=wasip1 go build -o main.wasm` |

## Crate Map

```
crates/
├── common/     Shared types: ProvisionEvent, Engine enum, config helpers
├── api/        Axum REST API — service CRUD, publishes to NATS
├── proxy/      Pingora edge router — slug → upstream_addr DB lookup
├── daemon/     Provision loop — Firecracker + Wasmtime execution
└── cli/        `plat` binary — init, deploy, status, logs
```

## Key Data Flow

```
plat deploy
  → POST /services  (api crate, Axum)
  → publish platform.provision  (NATS JetStream)
  → daemon consumes event
      engine=metal → spawn firecracker → configure VM → boot
      engine=flash → wasmtime::execute(_start)
  → upstream_addr written to Postgres services table
  → proxy routes *.machinename.dev/slug → upstream_addr
```

## User Workflow (3 commands)

```bash
plat init               # creates machine.toml
plat deploy             # builds + ships to the node
# → your app is live at <name>.machinename.dev
plat status             # check health
```

### machine.toml (Metal — Go API, HTMX, anything)
```toml
[service]
name   = "my-go-api"
engine = "metal"
port   = 8080

[metal]
vcpu      = 1
memory_mb = 128
```

### machine.toml (Flash — Wasm function)
```toml
[service]
name   = "my-handler"
engine = "flash"

[flash]
wasm = "main.wasm"
```

## Infrastructure

- **Node**: Hivelocity bare metal, Dallas hub
- **Network**: Floating IP → Pingora → TAP/br0 (metal) or in-process (flash)
- **Persistence**: NATS JetStream (event bus) + PostgreSQL (state)
- **Object storage**: RustFS (S3-compat) for rootfs and wasm artifact staging
- **No K3s. No Kubernetes. No managed cloud.**

## Constraints

- No rounded corners in UI (`rounded-none` everywhere)
- No AWS/GCP/Azure/Vercel/Heroku suggestions
- No GORM/ORMs — raw SQL via tokio-postgres
- All env config via env vars, no hardcoded addresses
- Firecracker and TAP/netlink are Linux-only — gated with `#[cfg(target_os = "linux")]`
- `wasmtime` runs on all platforms (good for local dev on macOS)

## Dev Workflow

```bash
task up           # start Postgres + NATS via docker compose
task dev:api      # Axum API on :3000
task dev:proxy    # Pingora on :8080
task dev:daemon   # NATS consumer (TAP/FC skipped on macOS)
task dev:cli -- deploy
```

### T480 Linux Setup (once, as root)
```bash
task metal:setup  # br0, /run/firecracker, Firecracker binary
```
