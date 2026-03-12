# Contributing to Liquid Metal

> **Experimental** — work in progress. Not production-ready.

---

## Codebase Layout

```
liquid-metal/
├── crates/
│   ├── common/          Shared types — Engine, ProvisionEvent, artifact, networking
│   ├── api/             Axum REST/JSON API server :7070, publishes to NATS
│   ├── web/             Axum + Askama + HTMX + Alpine.js dashboard :3000 — PLANNED
│   ├── cli/             flux CLI — login, init, deploy, status, logs, workspace, project
│   ├── proxy/           Pingora edge router — slug → upstream_addr
│   └── daemon/          NATS consumer — Firecracker (Metal) + Wasmtime (Liquid) provision loop
├── ebpf-programs/       TC egress classifiers (Aya) — Metal only. Excluded from workspace.
└── migrations/          PostgreSQL migrations (refinery, embedded in api)
```

**Rust workspace**: `crates/` — all crates share a root `Cargo.toml`. Everything is Rust.

The CLI (`crates/cli`) targets Linux/macOS natively. For building locally on Windows, use `cargo-zigbuild` — Zig acts as the C linker only, no Zig code involved:

```bash
cargo install cargo-zigbuild
cargo zigbuild --target x86_64-pc-windows-msvc
```

CI releases use `cargo-dist` with native GitHub Actions runners per platform.

---

## Tech Stack

| Layer     | Technology                                                                  |
|-----------|-----------------------------------------------------------------------------|
| API       | Axum, tokio-postgres — REST/JSON on :7070                                   |
| Web       | Axum + Askama templates + HTMX + Alpine.js — dashboard on :3000 *(planned)* |
| CLI       | clap, reqwest — calls API over HTTP                                         |
| Database  | PostgreSQL — raw SQL, no ORM                                                |
| Messaging | NATS JetStream                                                              |
| Proxy     | Pingora (Rust)                                                              |
| Isolation | Firecracker + KVM + eBPF (Aya) — Metal only                                 |
| Wasm      | Wasmtime / WASI                                                             |

---

## Rules of the House

1. **Rust only** — No Go, Node, Python, or any second runtime. One language, one toolchain. No context switching.

2. **Lib/Main split** — All crates use `lib.rs` for logic and `main.rs` as the entry point. Keeps crates integration-testable.

3. **REST/JSON only** — CLI and web communicate with the API over plain HTTP. No gRPC, no ConnectRPC, no protobuf.

4. **No hardcoded addresses** — All config via env vars (`NATS_URL`, `DATABASE_URL`, `BIND_ADDR`, etc.).

5. **Artifact storage** — Compiled binaries and Wasm modules go to Vultr Object Storage (S3-compatible). `deploy_id` is a UUID v7 — each deploy is immutable and time-sortable.

6. **Linux-only gates** — Firecracker, TAP networking, and eBPF are wrapped in `#[cfg(target_os = "linux")]`. Wasmtime runs on all platforms including macOS dev machines.

7. **No ORMs** — Raw SQL everywhere via `tokio-postgres`.

8. **UI** — Axum + Askama (server-side templates) + HTMX + Alpine.js. No SPAs. No React. `rounded-none`.

---

## Development Setup

```bash
# Start local infrastructure
task up              # Postgres + NATS + RustFS (S3 mock) via docker compose

# Start services
task dev:api         # Rust API on :7070
task dev:web         # Web dashboard on :3000 (once crates/web is built)
task dev:proxy       # Pingora on :8080
task dev:daemon      # NATS consumer (Firecracker skipped on macOS)

# Install the CLI
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

> **Note**: Production infrastructure is not yet provisioned. The planned topology (bare metal nodes, NATS cluster, Vultr Object Storage) is documented in [ARCHITECTURE.md](ARCHITECTURE.md).
