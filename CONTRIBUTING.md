# Contributing to Liquid Metal

> **Experimental** — work in progress. Not production-ready.

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
├── web/            Go dashboard — chi + Templ + HTMX, ConnectRPC client, :3000
├── mcp/            MCP server (Go) — exposes Liquid Metal ops as tools for Claude agents
├── gen/go/         buf-generated Go protobuf + connect stubs (shared via go.work)
├── proto/          Protobuf definitions — buf generates Rust (tonic) + Go (connect-go) stubs
└── migrations/     PostgreSQL migrations (refinery, embedded in api)
```

**Rust workspace**: `crates/` — all Rust crates share a root `Cargo.toml`.

**Go workspace**: `go.work` at repo root links `cli/`, `web/`, `mcp/`, and `gen/go/`. Each has its own `go.mod`.

---

## Tech Stack

| Layer       | Technology                                      |
|-------------|-------------------------------------------------|
| Rust API    | Axum + tonic (ConnectRPC), tokio-postgres       |
| Go web      | chi, Templ, HTMX, Alpine.js                     |
| Go CLI      | Cobra, Viper                                    |
| Go MCP      | MCP server, ConnectRPC client                   |
| Database    | PostgreSQL — raw SQL, no ORM                    |
| Messaging   | NATS JetStream                                  |
| Proxy       | Pingora (Rust)                                  |
| Isolation   | Firecracker + KVM + eBPF (Aya)                  |
| Wasm        | Wasmtime / WASI                                 |

---

## Rules of the House

1. **Lib/Main split** — All Rust crates use `lib.rs` for logic and `main.rs` as the entry point. This keeps crates integration-testable.

2. **Internal communication** — ConnectRPC over h2c (HTTP/2 cleartext). No TLS between internal services. The proto definitions in `proto/` are the only contract between Go and Rust.

3. **No hardcoded addresses** — All config via env vars (`NATS_URL`, `DATABASE_URL`, `API_URL`, etc.).

4. **Go web never touches Postgres or NATS** — All data flows through the Rust API via ConnectRPC. Zero exceptions.

5. **Artifact storage** — Compiled binaries and Wasm modules go to Vultr Object Storage (S3-compatible). `deploy_id` is a UUID v7 — each deploy is immutable.

6. **Linux-only gates** — Firecracker and TAP networking are wrapped in `#[cfg(target_os = "linux")]`. Wasmtime runs on all platforms, including macOS dev machines.

7. **No ORMs** — Raw SQL everywhere. Rust uses `tokio-postgres` directly.

8. **No SPA frameworks** — HTMX + Templ only. Alpine.js sparingly for client-side state.

9. **No rounded corners** — `rounded-none` in all UI components. Strict utilitarianism.

10. **MCP tools require `confirm: true`** for any destructive operation (delete service, deprovision). An agent cannot accidentally tear down a running service.

---

## Development Setup

```bash
# Start local infrastructure
task up              # Postgres + NATS + RustFS (S3 mock) via docker compose

# Start services
task dev:api         # Rust API on :7070
task dev:web         # Go dashboard on :3000 (air hot reload)
task dev:proxy       # Pingora on :8080
task dev:daemon      # NATS consumer (Firecracker skipped on macOS)
task dev:mcp         # MCP server (stdio — for Claude Desktop / agent dev)

# CLI
task dev:cli -- login
task dev:cli -- init
task dev:cli -- deploy
task dev:cli -- status
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
cd web && go test ./...   # Go web suite
cd cli && go test ./...   # CLI suite
```

---

## Proto / ConnectRPC

Protobuf definitions live in `proto/`. `buf` generates:
- Rust stubs (tonic) → consumed by `crates/api`
- Go stubs (connect-go) → consumed by `web/`, `cli/`, and `mcp/`

```bash
buf generate    # regenerate gen/go/ and crates/api/src/gen/
```

---

## Compute Isolation Model

- **Metal (Firecracker)**: 5-minute idle timeout for serverless tier. Always-on available on Pro/Team.
- **Liquid (Wasm)**: Per-invocation billing. Stateless, no persistent disk.

For the full infrastructure topology, eBPF tenant isolation details, HA strategy, and data flow diagrams see [ARCHITECTURE.md](ARCHITECTURE.md).
