# CLAUDE.md — Liquid Metal

> Single source of truth for AI-assisted development. If this contradicts what you infer from code, this file wins.

---

## What This Is

Liquid Metal is a **deployment platform for compiled static binaries**. Currently in beta, running on bare metal in Dallas. `flux deploy` → URL. No instance types, no load balancers, no Dockerfiles. Two engines, one platform:

- **Metal** — Dedicated Firecracker microVMs on bare metal. Real Linux, KVM isolation, dedicated vCPU, NVMe storage. Always-on, fixed monthly pricing. For `x86_64-unknown-linux-musl` binaries. Full kernel, network stack, connects to databases, brokers, anything with a socket.
- **Liquid** — Serverless Wasm. Wasmtime/WASI, sub-millisecond cold start, scale to zero, per-invocation billing. For `.wasm` modules targeting `wasm32-wasip1`. Best for stateless APIs, webhooks, data transforms.

Metal is dedicated VMs. Liquid is serverless Wasm. Same CLI, same deploy flow, different tools for different jobs. Don't blur them.

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

Five binaries. One database. One message bus. If you're about to add a sixth binary or a second database, stop.

### Crates

| Crate | Port | Does | Doesn't |
|-------|------|------|---------|
| `common` | — | Shared types, contracts, config, pricing, feature flags | Execute anything |
| `api` | `:7070` | REST/JSON brain. Postgres writes, NATS publishes, OpenAPI via utoipa | Talk to Firecracker, run Wasm |
| `web` | `:3000` | Askama + HTMX dashboard. OIDC browser auth. AES-GCM session cookies | Touch Postgres or NATS — it's a BFF that calls the API |
| `cli` | — | `flux` binary. Reads `liquid-metal.toml`, calls API | Make infrastructure decisions |
| `daemon` | — | NATS consumer. Provisions VMs, runs Wasm, manages cgroups + eBPF | Serve HTTP to users |
| `proxy` | `:8443` | Pingora edge router. Slug → upstream, custom domain TLS | Write to the database |
| `ebpf-programs` | — | TC egress classifiers (Aya) | Exist in the workspace |

### Data Flow Invariant

All mutations: **API → Postgres → NATS (outbox) → Daemon**. The API is the only Postgres writer. The daemon is the only thing that touches Firecracker or Wasmtime. The proxy is read-only. Violate this and you get split-brain bugs.

---

## Non-Negotiable Constraints

### 1. Lib/Main Split
Every crate: `lib.rs` for logic, `main.rs` as thin entry point. Tests in `crates/api/tests/` import `api::app()` and drive it with `tower::ServiceExt`. Logic in `main.rs` = untestable.

### 2. REST/JSON Only
No gRPC. No protobuf. No WebSockets. `curl` is a valid debugging tool for every endpoint. OpenAPI docs via utoipa.

### 3. Contracts in `common::contract`
All request/response types live in `crates/common/src/contract.rs`. CLI, web, and API all import from there. Never define response structs locally in consumer crates.

### 4. Raw SQL
`tokio-postgres` with hand-written SQL. No Diesel. No SeaORM. No SQLx macros. Queries go in `psql`.

### 5. No Kubernetes
No K8s. No K3s. Nomad schedules binaries via `raw_exec`. Daemons talk to Firecracker directly.

### 6. Server-Side HTML
Askama templates. HTMX for partial updates. Alpine.js for local UI state. No React. No Vue. No SPA. No client-side routing.

### 7. Zero Hardcoded Addresses
Every connection string, URL, and port comes from environment variables. See `.env.example`.

### 8. No Docker in Production
Nomad runs binaries directly via `raw_exec`. Docker is local dev only (Postgres, NATS, MinIO).

---

## Auth Model

### Scoped API Keys (`lm_*` prefix)
SHA-256 hashed, stored in `api_keys` table. Supports expiration, scopes, audit trails. CLI gets one on `flux login` via device flow.

### Internal Service Auth (`X-Internal-Secret` + `X-On-Behalf-Of`)
Web BFF → API calls. Shared secret from `INTERNAL_SECRET` env var. Supports zero-downtime rotation via comma-separated values.

No third path. No raw UUID tokens.

---

## Infrastructure

Four Hivelocity bare metal nodes in Dallas (DAL1), private VLAN. $419/mo total.

| Node | Hardware | Role |
|------|----------|------|
| **gateway** | E3-1230 v6 3.5GHz, 4c/8t, 32GB, 960GB SSD | HAProxy, Pingora, API, Web, NATS, Vault, observability |
| **node-metal** | EPYC 7452 2.35GHz, 32c/64t, 384GB, 1TB NVMe | Firecracker microVMs (60 sellable vCPUs) |
| **node-liquid** | E3-1230 v6 3.5GHz, 4c/8t, 32GB, 960GB SSD | Wasmtime/WASI |
| **node-db** | E3-1230 v6 3.5GHz, 4c/8t, 32GB, 960GB SSD | Postgres 16 + PgBouncer |

Observability: VictoriaMetrics + VictoriaLogs + Grafana. Backups to GCS. DNS on Cloudflare. Ops access via Tailscale. TLS via Let's Encrypt (certbot + DNS-01).

---

## Development

```bash
task up              # Postgres + NATS + MinIO via docker compose
task dev:api         # API on :7070
task dev:web         # Dashboard on :3000
task dev:proxy       # Pingora on :8080
sudo -E task dev:daemon  # Requires root for KVM/TAP/cgroups
                         # Wasm-only: DAEMON_PID_FILE=/tmp/lm.pid NODE_ENGINE=liquid task dev:daemon
```

### CLI
```bash
task install:cli     # cargo install → flux
flux login           # OIDC device flow → lm_* API key
flux init            # detect language, create project, write liquid-metal.toml
flux deploy          # build → upload to S3 → provision via NATS
```

### Tests
Integration tests need `DATABASE_URL` and `NATS_URL`. Real database, not mocks.
```bash
task test                  # unit tests (no infra needed)
task test:api:integration  # integration (needs task up + task migrate)
```

### Infrastructure
```bash
task infra:plan      # Terraform plan
task infra:apply     # Terraform apply
task nomad:deploy    # Deploy all Nomad jobs
```

---

## Patterns

- **Outbox**: API writes events to `outbox` table in same Postgres tx as mutation. Background poller publishes to NATS. At-least-once delivery without distributed transactions.
- **Feature Flags**: `crates/common/src/features.rs`. Env vars: `REQUIRE_INVITE`, `ENABLE_METAL`, `ENABLE_LIQUID`, `ENFORCE_QUOTAS`, `MAINTENANCE_MODE`. Evaluated at request time.
- **UUID v7**: All primary keys. Time-ordered, globally unique, no sequences.
- **Engine Gating**: `#[cfg(target_os = "linux")]` gates Firecracker code. Set `NODE_ENGINE=liquid` on non-Linux.

---

## Don't

- Add middleware to `web` that talks to Postgres — it's a BFF
- Create response types outside `common::contract`
- Mock the database in tests
- Add a new message broker — NATS does everything
- Run `cargo build`, `cargo check`, or `cargo test` without asking — limited RAM
- Add Docker to deployment — Nomad `raw_exec` only
- Introduce client-side routing in the dashboard
- Add backward-compatibility shims — zero production users, clean breaks only

---

## File Map

```
crates/
├── api/           # REST/JSON brain, route handlers, integration tests
├── cli/           # flux CLI, command implementations
├── common/        # contract.rs (canonical schema), events, features, pricing, config
├── daemon/        # NATS consumer, Firecracker + Wasmtime provisioning
├── proxy/         # Pingora edge router, slug lookup, TLS
├── web/           # Askama + HTMX dashboard, OIDC browser auth
└── ebpf-programs/ # TC classifiers (excluded from workspace)
infra/
├── nomad/         # Job files (api, proxy, vault, daemon-metal, daemon-liquid, migrate)
└── terraform/     # Hivelocity, Cloudflare, Tailscale, GCP
migrations/        # 40 SQL migrations, applied via task migrate
.env.example       # Every env var, documented
Taskfile.yml       # All dev/ops commands
RUNBOOK.md         # Dev setup, operations, and incident response
```
