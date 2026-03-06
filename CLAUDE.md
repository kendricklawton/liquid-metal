# liquid-metal — Agent Protocol

## Mission: Two Products, No Kubernetes, Ever

### Product 1 — Metal (Firecracker)
Hardware-isolated microVMs via AWS Firecracker + KVM. Users ship a Linux binary, we boot it in a VM with a dedicated ext4 rootfs and TAP networking. ~100–250ms cold start.

### Product 2 — Liquid (WebAssembly)
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
engine: metal            engine: liquid
Firecracker microVM      Wasmtime executor
TAP → br0                in-process, <1ms
~100–250ms cold start    no disk, no TAP
```

---

## Crate Map (Rust)

All Rust crates follow the `lib.rs` + `main.rs` split — `lib.rs` declares `pub mod` for all modules, `main.rs` contains only `main()` and imports from the lib. This makes every crate testable via integration tests.

```
crates/
├── common/         Engine enum, EngineSpec, ProvisionEvent, config helpers
├── api/            Axum + tonic — ConnectRPC server :7070, publishes ProvisionEvent to NATS
├── proxy/          Pingora — slug → upstream_addr DB lookup → route
├── daemon/         NATS consumer — Firecracker provisioner + Wasmtime executor
└── ebpf-programs/  TC egress classifier (BPF, bpfel-unknown-none target) — NOT in workspace
```

## Go Modules

Two Go modules, one `go.work` at workspace root linking them with `gen/go` (proto stubs).

```
web/                    go.mod: github.com/kendricklawton/liquid-metal/web
├── cmd/web/main.go     chi server entry point — :3000
└── internal/
    ├── config/         env var loading
    ├── handler/        HTMX handlers (HX-Request/HX-Target detection for partial swaps)
    └── ui/             Templ components + pages

cli/                    go.mod: github.com/kendricklawton/liquid-metal/cli
├── main.go             calls cmd.Execute()
└── cmd/                package cmd — Cobra commands, importable for tests
    ├── root.go         rootCmd, Execute(), initConfig(), shared helpers
    ├── login.go        flux login  (browser OAuth → localhost callback → save token)
    ├── whoami.go       flux whoami (GetMe)
    ├── status.go       flux status (ListServices, tabwriter output)
    ├── logs.go         flux logs <id> [--limit N]
    └── deploy.go       flux deploy (reads machine.toml → CreateService)
```

**CLI auth**: `flux login` opens browser to Go web `/auth/cli/login`, captures token via localhost callback, stores in `~/.config/flux/config.yaml`. All subsequent CLI calls go **directly to the Rust API** with `X-Api-Key: {token}` — no Go web involvement.

**Go web never touches Postgres or NATS directly.** All data flows through the Rust API via ConnectRPC over h2c.

---

## Data Flow

```
flux deploy
  → reads machine.toml
  → build binary / .wasm locally
  → upload artifact to Object Storage (pre-signed URL from GET /upload-url)
  → POST /services via ConnectRPC (CreateService)
  → API inserts service row (status=provisioning), publishes ProvisionEvent to NATS
  → daemon consumes
      metal  → download artifact → inject into rootfs → boot Firecracker VM → write upstream_addr
      liquid → download .wasm → wasmtime::execute(_start) → write upstream_addr
  → proxy routes slug → upstream_addr

Browser (HTMX)
  → Go web handler (chi)
  → ConnectRPC call (protobuf/h2c)
  → Rust API (tonic + Axum)
  → tokio-postgres
  → Postgres
```

---

## Auth (WorkOS AuthKit)

**Go web** handles all OAuth flows:
- Browser login: `/auth/login` → WorkOS → `/auth/callback` → sets `lm_session` cookie (userUUID)
- CLI login: `/auth/cli/login?redirect_uri=http://localhost:{port}/callback` → WorkOS → `/auth/cli/callback` → redirects to CLI's local server with `?token={userUUID}`
- After OAuth: calls `POST /auth/provision` on Rust API (X-Internal-Secret) to upsert user + workspace

**Rust API** validates tokens:
- Public: `/healthz`
- Internal: `/auth/provision` — `X-Internal-Secret` header, called by Go web only
- CLI: `/services`, `/upload-url` — `X-Api-Key: {userUUID}` header
- gRPC (ConnectRPC): `Authorization: Bearer {userUUID}` header

**Cookies** (HttpOnly, Go web only — never sent to Rust API):
`lm_session`, `lm_name`, `lm_slug`, `lm_email`, `lm_workos_uid`, `lm_workos_sid`, `lm_tier`

---

## Proto / ConnectRPC

- Definitions in `proto/liquidmetal/v1/` — `service.proto`, `user.proto`
- `buf` generates:
  - Rust stubs → `crates/api` (tonic)
  - Go stubs → `gen/go/` (connectrpc/connect-go) — shared by both `web/` and `cli/` via `go.work`
- ConnectRPC over h2c (HTTP/2 cleartext) — no TLS between internal services

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
# Liquid
[service]
name   = "my-fn"
engine = "liquid"

[liquid]
wasm = "main.wasm"
```

---

## Infrastructure

- **Compute**: 2× Vultr Bare Metal, Chicago (ORD)
- **NAT VPS**: Vultr Cloud Compute, Chicago — holds floating IP, HAProxy, Headscale, NATS tiebreaker
- **Network**: Tailscale mesh via Headscale (self-hosted on NAT VPS) — bare metal has zero public ports; SSH via `tailscale ssh`
- **Event bus**: NATS JetStream 3-node Raft (node-a + node-b + NAT VPS)
- **State**: Vultr Managed Postgres, Chicago — raw SQL, refinery migrations in `migrations/`
- **Artifacts**: Vultr Object Storage, Chicago (S3-compatible) — base rootfs + user binaries + wasm modules; `deploy_id` (uuid_v7) per deploy for immutable versioning
- **Local dev**: RustFS (docker compose) on :9000 replaces Vultr Object Storage

---

## Hard Constraints

- **No Kubernetes, K3s, or any orchestrator**
- **No Vercel/Heroku/managed compute** — bare metal for all execution
- **Vultr Managed Postgres + Object Storage are allowed** — no GCP, no AWS, no managed compute
- **No ORMs** — tokio-postgres (Rust), no sqlc yet (raw queries)
- **No SPA frameworks** — HTMX + Templ only, Alpine.js sparingly
- **No rounded corners** — `rounded-none` everywhere in UI
- **No hardcoded addresses** — all config via env vars (`DATABASE_URL`, `NATS_URL`, `BIND_ADDR`, `API_URL`)
- **Firecracker + TAP + eBPF are Linux-only** — gate with `#[cfg(target_os = "linux")]`
- **eBPF tenant isolation uses Aya** — `crates/ebpf-programs` (kernel) + `crates/daemon/src/ebpf.rs` (loader). No Cilium, no CNI, no K8s dependency.
- **wasmtime runs on all platforms** — safe for local macOS dev
- **Go web and CLI never touch Postgres or NATS directly**

---

## Dev Workflow

```bash
task up              # Postgres + NATS + RustFS (docker compose)
task dev:api         # Rust API :7070
task dev:web         # Go dashboard :3000 (air hot reload)
task dev:proxy       # Pingora :8080
task dev:daemon      # NATS consumer (Firecracker skipped on macOS)
task dev:cli -- login    # flux login
task dev:cli -- deploy   # flux deploy (reads ./machine.toml)
task dev:cli -- status   # flux status
```

### Linux bare-metal setup (once, as root)
```bash
task metal:setup     # br0, /run/firecracker, Firecracker binary
task security:setup  # jailer user, cgroup v2 controllers, IFB module
```

### Running tests
```bash
cargo test --workspace                        # Rust unit + integration tests
cd web && go test ./...                       # Go web tests
cd cli && go test ./...                       # CLI tests
```
