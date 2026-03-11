# Contributing to Liquid Metal

> **Experimental** — work in progress. Not production-ready.

---

## Codebase Layout

```
liquid-metal/
├── crates/
│   ├── common/     Shared Rust types (Engine, ProvisionEvent, EngineSpec, slugify)
│   ├── api/        Axum + tonic — ConnectRPC server :7070, publishes to NATS
│   ├── cli/        flux CLI — login, init, deploy, status, logs, workspace, project
│   ├── proxy/      Pingora edge router — slug → upstream_addr
│   └── daemon/     NATS consumer — Firecracker + Wasmtime provision loop
└── migrations/     PostgreSQL migrations (refinery, embedded in api)
```

**Rust workspace**: `crates/` — all Rust crates share a root `Cargo.toml`.

The CLI (`crates/cli`) targets Linux/macOS natively. For Windows releases, Zig is used as a cross-compilation linker.

---

## Tech Stack

| Layer       | Technology                                      |
|-------------|-------------------------------------------------|
| Rust API    | Axum + tonic (ConnectRPC), tokio-postgres       |
| Rust CLI    | clap, reqwest, ConnectRPC client                |
| Database    | PostgreSQL — raw SQL, no ORM                    |
| Messaging   | NATS JetStream                                  |
| Proxy       | Pingora (Rust)                                  |
| Isolation   | Firecracker + KVM + eBPF (Aya)                  |
| Wasm        | Wasmtime / WASI                                 |

---

## Rules of the House

1. **Lib/Main split** — All Rust crates use `lib.rs` for logic and `main.rs` as the entry point. This keeps crates integration-testable.

2. **Internal communication** — ConnectRPC over h2c (HTTP/2 cleartext). No TLS between internal services.

3. **No hardcoded addresses** — All config via env vars (`NATS_URL`, `DATABASE_URL`, `API_URL`, etc.).

4. **Artifact storage** — Compiled binaries and Wasm modules go to Vultr Object Storage (S3-compatible). `deploy_id` is a UUID v7 — each deploy is immutable.

5. **Linux-only gates** — Firecracker and TAP networking are wrapped in `#[cfg(target_os = "linux")]`. Wasmtime runs on all platforms, including macOS dev machines.

6. **No ORMs** — Raw SQL everywhere. Rust uses `tokio-postgres` directly.

---

## Development Setup

```bash
# Start local infrastructure
task up              # Postgres + NATS + RustFS (S3 mock) via docker compose

# Start services
task dev:api         # Rust API on :7070
task dev:proxy       # Pingora on :8080
task dev:daemon      # NATS consumer (Firecracker skipped on macOS)

# Install the CLI once — then use flux from any directory
task install:cli     # cargo install → flux lands in ~/.cargo/bin

# From your service directory (not the liquid-metal repo)
flux login
flux init
flux deploy
flux status
```

### Linux only (bare metal, one-time setup)

```bash
task metal:setup     # br0 bridge, /run/firecracker, Firecracker binary
task security:setup  # jailer user, cgroup v2 controllers, eBPF policy
```

---

## Running Tests

```bash
cargo test --workspace    # Rust suite
```

---

## Compute Isolation Model

- **Metal (Firecracker)**: 5-minute idle timeout for serverless tier. Always-on available on Pro/Team.
- **Liquid (Wasm)**: Per-invocation billing. Stateless, no persistent disk.

For the full infrastructure topology, eBPF tenant isolation details, HA strategy, and data flow diagrams see [ARCHITECTURE.md](ARCHITECTURE.md).
