# Contributing to Liquid Metal

> **Experimental** — work in progress. Not production-ready.

---

## Codebase Layout

```
liquid-metal/
├── crates/
│   ├── common/          Shared Rust types (Engine, ProvisionEvent, EngineSpec, slugify)
│   ├── api/             Axum — REST/JSON API server :7070, publishes to NATS
│   ├── web/             Axum + Askama + HTMX dashboard :3000 — PLANNED, not yet built
│   ├── cli/             flux CLI — login, init, deploy, status, logs, workspace, project
│   ├── proxy/           Pingora edge router — slug → upstream_addr
│   └── daemon/          NATS consumer — Firecracker (Metal) + Wasmtime (Liquid) provision loop
├── ebpf-programs/       TC egress classifiers (Aya) — Metal tier only. Excluded from workspace.
└── migrations/          PostgreSQL migrations (refinery, embedded in api)
```

**Rust workspace**: `crates/` — all Rust crates share a root `Cargo.toml`.

The CLI (`crates/cli`) targets Linux/macOS natively. For Windows releases, Zig is used as a cross-compilation linker.

---

## Tech Stack

| Layer       | Technology                                                                  |
|-------------|-----------------------------------------------------------------------------|
| Rust API    | Axum, tokio-postgres — REST/JSON on :7070                                   |
| Rust Web    | Axum + Askama templates + HTMX + Alpine.js — dashboard on :3000 *(planned)* |
| Rust CLI    | clap, reqwest — calls API over HTTP                                         |
| Database    | PostgreSQL — raw SQL, no ORM                                                |
| Messaging   | NATS JetStream                                                              |
| Proxy       | Pingora (Rust)                                                              |
| Isolation   | Firecracker + KVM + eBPF (Aya) — Metal tier only                            |
| Wasm        | Wasmtime / WASI                                                             |

---

## Rules of the House

1. **Lib/Main split** — All Rust crates use `lib.rs` for logic and `main.rs` as the entry point. This keeps crates integration-testable.

2. **REST/JSON only** — The CLI communicates with the API over plain HTTP using reqwest. No gRPC, no ConnectRPC, no protobuf.

3. **No hardcoded addresses** — All config via env vars (`NATS_URL`, `DATABASE_URL`, `API_URL`, `NODE_ENGINE`, etc.).

4. **Artifact storage** — Compiled binaries and Wasm modules go to Vultr Object Storage (S3-compatible). `deploy_id` is a UUID v7 — each deploy is immutable.

5. **Linux-only gates** — Firecracker, TAP networking, and eBPF are wrapped in `#[cfg(target_os = "linux")]`. Wasmtime runs on all platforms, including macOS dev machines.

6. **No ORMs** — Raw SQL everywhere. Rust uses `tokio-postgres` directly.

7. **Engine selection** — The daemon reads `NODE_ENGINE` (`metal` or `liquid`) at startup to determine which engine to run. Metal nodes handle Firecracker VMs; Liquid nodes handle Wasm invocations.

---

## Development Setup

```bash
# Start local infrastructure
task up              # Postgres + NATS + RustFS (S3 mock) via docker compose

# Start services
task dev:api         # Rust API on :7070
task dev:web         # Web dashboard on :3000 (once crates/web is built)
task dev:proxy       # Pingora on :8080
task dev:daemon      # NATS consumer (Firecracker skipped on macOS; set NODE_ENGINE=liquid for Wasm)

# Install the CLI once — then use flux from any directory
task install:cli     # cargo install --path crates/cli → flux lands in ~/.cargo/bin

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
# Compile check (no infra needed)
cargo check --workspace

# Unit tests (no infra needed)
cargo test --workspace

# Integration tests (requires task up + task dev:api)
cargo test -p api --test api
```

---

## Compute Isolation Model

- **Metal (Firecracker)**: 5-minute idle timeout for serverless tier. Always-on available on Pro/Team.
- **Liquid (Wasm)**: Per-invocation billing. Stateless, no persistent disk.

> **Note**: Production infrastructure is not yet provisioned. The planned topology (4 bare metal nodes, NATS cluster, Vultr Object Storage) is documented in [ARCHITECTURE.md](ARCHITECTURE.md).
