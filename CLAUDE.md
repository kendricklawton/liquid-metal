# CLAUDE.md — Liquid Metal

> This file is the single source of truth for AI-assisted development on this codebase.
> If something here contradicts what you infer from the code, this file wins.

---

## What This Is

Liquid Metal is a **serverless platform for compiled static binaries**. Not IaaS. Not "infrastructure building blocks." Serverless.

Users run `flux deploy` and get a URL. They don't pick instance types, configure load balancers, or write Dockerfiles. We decide how their code runs. We own the opinions. Two serverless engines, one platform:

- **Metal** — Firecracker microVMs restored from snapshot. Real Linux, real isolation, real networking. KVM-backed, ~15ms cold start via snapshot restore. For anything that targets `x86_64-unknown-linux-musl`. Billed per invocation + GB-sec compute.
- **Liquid** — Wasmtime/WASI. In-process, memory-only, sub-millisecond cold start. For `.wasm` modules. No disk, no TAP, no VM. Billed per invocation.

Both engines scale to zero. Both wake on first request. The user never thinks about VMs, cores, or infrastructure. They deploy a binary and get a URL.

The distinction matters. Metal gives you a kernel. Liquid gives you a sandbox. Don't blur them.

---

## Architecture — The Short Version

```
User → flux CLI → API (:7070) → Postgres + NATS
                                       ↓
                              Daemon (NATS consumer)
                              ├── Metal: Firecracker VM
                              └── Liquid: Wasmtime
                                       ↓
                              Proxy (Pingora) ← User traffic
```

Five binaries. One database. One message bus. That's the whole system. If you're about to add a sixth binary or a second database, stop and reconsider.

### The Crates

| Crate | Port | What It Does | What It Doesn't Do |
|-------|------|-------------|-------------------|
| `common` | — | Shared types, contracts, config | Execute anything |
| `api` | `:7070` | REST/JSON brain. Postgres writes, NATS publishes | Talk to Firecracker, run Wasm |
| `web` | `:3000` | Askama + HTMX dashboard. OIDC browser auth | Touch Postgres or NATS directly |
| `cli` | — | `flux` binary. Reads `liquid-metal.toml`, calls API | Make infrastructure decisions |
| `daemon` | — | NATS consumer. Provisions VMs, runs Wasm | Serve HTTP to users |
| `proxy` | `:8443` | Pingora edge router. Slug → upstream | Write to the database |
| `ebpf-programs` | — | TC egress classifiers (Aya) | Exist in the workspace |

The `web` crate is a stateless BFF. Sessions live in AES-GCM encrypted cookies. It calls the API over HTTP using `X-Internal-Secret` + `X-On-Behalf-Of` headers. It never imports `tokio-postgres`. If you find yourself adding a database connection to `web`, you've made a wrong turn.

### Data Flow Invariant

All mutations flow through one path: **API → Postgres → NATS → Daemon**. The API is the only writer to Postgres. The daemon is the only thing that touches Firecracker or Wasmtime. The proxy is read-only against the database. Violate this and you'll create split-brain bugs that are impossible to debug in production.

---

## Constraints — Non-Negotiable

These aren't preferences. They're load-bearing walls.

### 1. The Lib/Main Split

Every crate uses `lib.rs` for logic and `main.rs` as a thin entry point. This is how we get integration testability without Docker. The test harness in `crates/api/tests/` imports `api::app()` and drives it with `tower::ServiceExt`. If you put logic in `main.rs`, you've made it untestable.

### 2. REST/JSON Only

CLI and web talk to the API over plain HTTP. No gRPC. No protobuf. No ConnectRPC. No WebSockets (yet). The simplicity is the feature. A `curl` command is a valid debugging tool for every endpoint.

### 3. Contracts Live in `common::contract`

All API request and response types are defined in `crates/common/src/contract.rs`. CLI, web, and API all import from here. Never define a response struct locally in a consumer crate. If you need a new field, add it to the contract. One source of truth, no drift.

### 4. Raw SQL, No ORM

We use `tokio-postgres` with hand-written SQL. No Diesel. No SeaORM. No SQLx macros. Queries are readable, debuggable, and you can paste them into `psql`. If a query is getting complex, it's the query that needs simplifying, not the abstraction layer that needs thickening.

### 5. No Kubernetes, Ever

No K8s. No K3s. No container orchestrators. Nomad schedules our binaries via `raw_exec`. The daemons talk to Firecracker directly. If you catch yourself thinking "this would be easier with a CRD," you're solving the wrong problem.

### 6. Server-Side HTML

The web dashboard renders HTML on the server with Askama templates. HTMX handles partial page updates (log tailing, status polling). Alpine.js handles local UI state (toggles, modals). No React. No Vue. No Leptos. No client-side routing. No SPA. The `rounded-none` CSS class is used unironically.

### 7. Zero Hardcoded Addresses

Every connection string, every URL, every port comes from an environment variable. See `.env.example` for the full list. If you add a new external dependency, add its config to `.env.example` with a comment explaining what it is and how to get one.

---

## Auth Model

Two authentication paths exist. Understand both before touching auth code.

### Scoped API Keys (`lm_*` prefix)

The production auth path. SHA-256 hashed, stored in `api_keys` table. Support expiration, scopes, and audit trails. The CLI gets one automatically on `flux login` (via `cli_provision`). Every API call from the CLI sends it as `X-Api-Key: lm_...`.

### Internal Service Auth (`X-Internal-Secret` + `X-On-Behalf-Of`)

For trusted service-to-service calls. The web BFF uses this to call the API on behalf of a logged-in user. The secret is a shared value from `INTERNAL_SECRET` env var. Supports zero-downtime rotation via comma-separated values.

There is no third path. Raw UUID tokens are rejected. If middleware sees a token that doesn't start with `lm_` and isn't an internal service call, it returns 401.

---

## Infrastructure Topology (Beta)

Three machines. One region. That's it.

| Node | Role | Nomad Class | Runs |
|------|------|------------|------|
| NAT VPS | Gateway | `gateway` | API, Proxy, NATS, HAProxy, PgBouncer, Nomad server+client |
| node-a-01 | Compute | `metal` | daemon-metal (Firecracker) |
| node-b-01 | Compute | `liquid` | daemon-liquid (Wasmtime) |

HAProxy on the NAT VPS owns `:443` and `:80`. It terminates TLS and forwards to Pingora on `localhost:8443`. Everything talks over Tailscale. Postgres is Vultr Managed, accessed via PgBouncer on the NAT VPS (`:6432`).

Nomad job files live in `infra/nomad/`. Terraform lives in `infra/terraform/`. Node classes constrain which jobs land where. Don't add `spread` or `distinct_hosts` constraints — there's one instance of everything.

---

## Development

### Local Setup

```bash
task up              # Postgres + NATS + MinIO (S3 mock) via docker compose
task dev:api         # API on :7070
task dev:web         # Dashboard on :3000
task dev:proxy       # Pingora on :8080
sudo -E task dev:daemon  # NATS consumer — requires root for KVM/TAP/cgroups
                         # Wasm-only (no sudo): DAEMON_PID_FILE=/tmp/lm.pid NODE_ENGINE=liquid task dev:daemon
```

### Linux-Only (Bare Metal Dev)

```bash
sudo task metal:setup      # KVM check, br0 bridge, NAT, Firecracker + jailer, artifact dir
sudo task security:setup   # jailer user, cgroup v2, IFB module
```

### CLI

```bash
task install:cli     # cargo install → flux in ~/.cargo/bin
flux login           # OIDC device flow → gets lm_* API key
flux init            # detect language, create project, write liquid-metal.toml
flux deploy          # build → upload to S3 → provision via NATS
flux services        # list services in active workspace
```

### Tests

Integration tests in `crates/api/tests/` require `DATABASE_URL` and `NATS_URL`. They use a real database, not mocks. The test harness provisions real users and creates real API keys.

```bash
cargo test -p api -- --include-ignored   # integration tests (needs infra)
cargo test --workspace                   # unit tests only
```

### Infrastructure

```bash
task infra:plan              # Terraform plan (CLOUD_ENV=dev)
task infra:apply             # Terraform apply
task nomad:deploy            # Deploy all Nomad jobs
task nomad:status            # Check job status
```

---

## Patterns You'll See

### The Outbox Pattern

The API doesn't publish to NATS directly in request handlers. It writes events to an `outbox` table in the same Postgres transaction as the mutation, then a background poller picks them up and publishes to NATS. This guarantees at-least-once delivery without distributed transactions.

### Feature Flags

`crates/common/src/features.rs` defines feature flags read from env vars: `REQUIRE_INVITE`, `ENABLE_METAL`, `ENABLE_LIQUID`, `ENFORCE_QUOTAS`, `MAINTENANCE_MODE`. All default to the safe production value when unset. They're evaluated at request time, not compile time.

### UUID v7

All primary keys are UUID v7 — time-ordered, globally unique, no sequences. Deploy IDs are UUID v7 too, making each artifact immutable and naturally sorted.

### Engine Gating

The daemon uses `#[cfg(target_os = "linux")]` to gate Firecracker code. On non-Linux systems, only Liquid (Wasm) works. Set `NODE_ENGINE=liquid` to skip Metal entirely during local dev.

---

## What Not To Do

- **Don't add middleware to `web` that talks to Postgres.** Web is a BFF. It calls the API.
- **Don't create response types outside `common::contract`.** That's how you get deserialization mismatches between CLI and API at 2am.
- **Don't mock the database in tests.** We've been burned. Use the real thing.
- **Don't add a new message broker.** NATS does pub/sub, queues, and persistence. One bus.
- **Don't run `cargo build`, `cargo check`, or `cargo test` without asking first.** This machine has limited RAM. Always confirm before kicking off a build.
- **Don't add Docker to the deployment path.** Nomad runs our binaries directly via `raw_exec`. No containers in production.
- **Don't introduce client-side routing in the web dashboard.** Server-rendered HTML with HTMX partial swaps. That's the architecture.
- **Don't add backward-compatibility shims.** Zero production users. Clean breaks only.

---

## Deployment

Binaries are built by GitHub Actions (`release.yml` via `cargo-dist`), uploaded as release artifacts, and deployed via `deploy.yml` which updates Nomad jobs. User artifacts (compiled binaries and `.wasm` modules) go to Vultr Object Storage (S3-compatible) under `liquid-metal-artifacts/`.

The deploy flow for user services: `flux deploy` → local build → upload to S3 → API writes to Postgres + outbox → NATS event → daemon provisions (Firecracker VM or Wasmtime instance) → proxy routes traffic via slug lookup.

---

## File Map

```
/
├── crates/
│   ├── api/           # REST/JSON brain
│   │   ├── src/routes/  # Route handlers (auth, services, api_keys, billing, etc.)
│   │   └── tests/       # Integration tests with real Postgres
│   ├── cli/           # flux CLI
│   │   └── src/commands/  # Command implementations (auth/, service/, workspace, etc.)
│   ├── common/        # Shared types — contract.rs is the canonical schema
│   ├── daemon/        # NATS consumer — Firecracker + Wasmtime
│   ├── proxy/         # Pingora edge router
│   ├── web/           # Askama + HTMX dashboard
│   └── ebpf-programs/ # TC classifiers (excluded from workspace)
├── infra/
│   ├── nomad/         # Job files (api, proxy, daemon-metal, daemon-liquid)
│   └── terraform/     # Vultr + Cloudflare + Tailscale + GCP
├── migrations/        # SQL migrations (V1..VN), run via `task migrate`
├── .env.example       # Every env var the system uses, documented
├── Taskfile.yml       # All dev/ops commands
├── ARCHITECTURE.md    # Infrastructure topology and data flow
└── RUNBOOK.md         # Operational procedures
```

---

## Reference

- **Docs**: `ARCHITECTURE.md` (topology), `RUNBOOK.md` (operations)
- **Config**: `.env.example` has every env var with inline docs
- **Migrations**: `migrations/` directory, applied via `cargo run -p api -- --migrate`
- **Nomad jobs**: `infra/nomad/*.nomad.hcl`
- **Terraform**: `infra/terraform/` — Vultr, Cloudflare, Tailscale, GCP (KMS + state bucket)
