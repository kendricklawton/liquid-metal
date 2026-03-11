## Liquid Metal: Engineering Protocol

### Product Thesis

Liquid Metal is a **PaaS (not IaaS)** delivering hardware-isolated compute with infrastructure transparency. We provide predictable performance (vCPU/RAM) without the "mystery resource pool" abstraction of Vercel or the management overhead of Kubernetes.

* **Metal**: Firecracker microVMs. Linux binaries, ext4 rootfs, TAP networking. ~100-250ms cold start.
* **Liquid**: Wasmtime/WASI execution. In-process, memory-only, no disk/TAP. <1ms startup.
* **Zero-Trust Infrastructure**: No K8s. No managed orchestrators. Bare metal or death.

---

### Tech Stack & Constraints

* **Languages**: Rust (everything). Zig may be used to cross-compile the CLI for Windows targets.
* **Database**: PostgreSQL (Raw SQL via `tokio-postgres`, no ORMs).
* **Messaging**: NATS JetStream (The source of truth for provisioning).
* **Proxy**: Pingora (Cloudflare's framework) for edge routing.
* **Security**: Firecracker + Jailer + eBPF (Aya) for tenant isolation.

---

### System Architecture & Data Flow

1. **CLI (`flux`, `crates/cli`)**: Reads `liquid-metal.toml`, uploads artifacts to S3, calls Rust API via ConnectRPC.
2. **API (`axum/tonic`)**: The Brain. Writes to Postgres, publishes `ProvisionEvent` to NATS.
3. **Daemon**: NATS Consumer. Orchestrates Firecracker (Linux) or Wasmtime (Multi-platform).
4. **Proxy (`pingora`)**: Maps `slug -> upstream_addr` via DB lookup.

---

### Crate & Module Map

#### Rust Workspace (`/crates`)

* `common`: Shared `Engine` enums, `ProvisionEvent`, and config logic.
* `api`: Axum + ConnectRPC. Port `:7070`. Validates `X-Api-Key`.
* `cli`: `flux` CLI. Reads `liquid-metal.toml`, calls Rust API via ConnectRPC. Cross-compiled for Windows via Zig if needed.
* `proxy`: Pingora-based edge router.
* `daemon`: The worker. Firecracker/Wasmtime logic. Linux-only code gated via `#[cfg(target_os = "linux")]`.
* `ebpf-programs`: TC egress classifiers (Aya). *Excluded from workspace.*

---

### Rules of the House

1. **The Lib/Main Split**: All Rust crates must use `lib.rs` for logic and `main.rs` for the entry point to ensure integration testability.
2. **Internal Communication**: Use ConnectRPC over h2c (HTTP/2 cleartext). No TLS between internal services.
3. **Configuration**: Zero hardcoded addresses. Use env vars (`NATS_URL`, `DATABASE_URL`, etc.).
4. **Deployment**: Binaries/Wasm modules are stored in S3-compatible storage using `uuid_v7` for immutable versioning.
5. **Compute Isolation**:
* **Serverless (Metal)**: 5-minute idle timeout.
* **Always-on (Metal)**: Continuous drain (Pro/Team only).
* **Liquid**: Per-invocation billing.

---

### Development Commands

```bash
# Infrastructure
task up                   # Spin up Postgres, NATS, RustFS (S3 mock)
task metal:setup          # (Linux Root) Setup br0, jailer, cgroups

# Development
task dev:api              # Start Rust API
task dev:daemon           # Start NATS consumer

# Testing
cargo test --workspace    # Rust suite
```
