## Liquid Metal: Engineering Protocol

### Product Thesis

Liquid Metal is a **PaaS (not IaaS)** delivering hardware-isolated compute with infrastructure transparency. We provide predictable performance (vCPU/RAM) without the "mystery resource pool" abstraction of Vercel or the management overhead of Kubernetes.

* **Metal**: Firecracker microVMs. Linux binaries, ext4 rootfs, TAP networking. ~100-250ms cold start.
* **Liquid**: Wasmtime/WASI execution. In-process, memory-only, no disk/TAP. <1ms startup.
* **Zero-Trust Infrastructure**: No K8s. No managed orchestrators. Bare metal or death.

---

### Tech Stack & Constraints

* **Languages**: Rust (everything). Zig may be used to cross-compile the CLI for Windows targets.
* **Web**: `crates/web` — Axum + Askama templates + HTMX + Alpine.js (minimal). **No SPAs. No Leptos. No JS framework.**
* **Database**: PostgreSQL (Raw SQL via `tokio-postgres`, no ORMs).
* **Messaging**: NATS JetStream (The source of truth for provisioning).
* **Proxy**: Pingora (Cloudflare's framework) for edge routing.
* **Security**: Firecracker + Jailer + eBPF (Aya) for tenant isolation.
* **UI Aesthetic**: `rounded-none`. Strict utilitarianism.

---

### System Architecture & Data Flow

1. **CLI (`flux`, `crates/cli`)**: Reads `liquid-metal.toml`, uploads artifacts to S3, calls Rust API via REST/JSON.
2. **Web (`crates/web`)**: Axum + Askama + HTMX dashboard on port `:3000`. Handles Zitadel OIDC browser auth. Calls Rust API internally via REST/JSON. **Never touches Postgres or NATS directly. Planned — not yet built.**
3. **API (`crates/api`)**: The Brain. Axum REST/JSON on port `:7070`. Validates `X-Api-Key`. Writes to Postgres, publishes `ProvisionEvent` to NATS.
4. **Daemon (`crates/daemon`)**: NATS Consumer. Orchestrates Firecracker (Linux) or Wasmtime (multi-platform).
5. **Proxy (`crates/proxy`)**: Pingora maps `slug → upstream_addr` via DB lookup.

---

### Crate & Module Map

#### Rust Workspace (`/crates`)

* `common`: Shared `Engine` enums, `ProvisionEvent`, config helpers, and slugify.
* `api`: Axum REST/JSON. Port `:7070`. Validates `X-Api-Key`. Writes to Postgres, publishes to NATS.
* `web`: Axum + Askama + HTMX dashboard. Port `:3000`. Browser OIDC auth (Zitadel). Calls API via HTTP. **Planned — not yet built.**
* `cli`: `flux` CLI. Reads `liquid-metal.toml`, calls API via REST/JSON with `reqwest`. Cross-compiled for Windows via Zig if needed.
* `proxy`: Pingora-based edge router.
* `daemon`: The worker. Firecracker/Wasmtime logic. Linux-only code gated via `#[cfg(target_os = "linux")]`.
* `ebpf-programs`: TC egress classifiers (Aya). *Excluded from workspace.*

---

### Rules of the House

1. **The Lib/Main Split**: All Rust crates must use `lib.rs` for logic and `main.rs` for the entry point to ensure integration testability.
2. **REST/JSON only**: CLI and web communicate with the API over plain HTTP. No gRPC, no ConnectRPC, no protobuf.
3. **Configuration**: Zero hardcoded addresses. Use env vars (`NATS_URL`, `DATABASE_URL`, etc.).
4. **Deployment**: Binaries/Wasm modules are stored in S3-compatible storage using `uuid_v7` for immutable versioning.
5. **Compute Isolation**:
   * **Serverless (Metal)**: 5-minute idle timeout.
   * **Always-on (Metal)**: Continuous drain (Pro/Team only).
   * **Liquid**: Per-invocation billing.
6. **Web rendering**: Server-side HTML via Askama templates. HTMX for partial swaps (log tailing, status polling). Alpine.js for local toggle/modal state only. No client-side routing. No SPAs.

---

### Development Commands

```bash
# Infrastructure
task up                   # Spin up Postgres, NATS, RustFS (S3 mock)
task metal:setup          # (Linux Root) Setup br0, jailer, cgroups

# Development
task dev:api              # Start Rust API on :7070
task dev:web              # Start web dashboard on :3000 (once built)
task dev:daemon           # Start NATS consumer (NODE_ENGINE=liquid for Wasm-only on macOS)

# Testing
cargo test --workspace    # Rust suite
```
