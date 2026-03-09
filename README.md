# Liquid Metal

> **Experimental** — work in progress. Built to learn by doing. Not production-ready.

Bare-metal hosting platform. No Kubernetes. No managed cloud. Two products built on proven isolation primitives.

---

## Products

### Metal — Firecracker MicroVMs
Run any Linux binary in a hardware-isolated VM. KVM-backed, dedicated rootfs, TAP networking. Ship a Go binary, an HTMX app, anything that compiles.

- **Isolation**: AWS Firecracker + KVM
- **Cold start**: ~100–250ms
- **Networking**: TAP device → br0 bridge → Pingora proxy
- **Storage**: Dedicated ext4 rootfs per service

```toml
# liquid-metal.toml
[service]
name   = "my-app"
engine = "metal"
port   = 8080

[metal]
vcpu      = 1
memory_mb = 128

```

### Liquid — WebAssembly

Run Wasm modules via Wasmtime/WASI. Sub-millisecond cold starts, in-process execution, memory-only. No disk, no TAP, no VM overhead.

* **Isolation**: Wasmtime Wasm sandbox
* **Cold start**: <1ms
* **Execution**: In-process, WASI VFS

```toml
# liquid-metal.toml
[service]
name   = "my-fn"
engine = "liquid"

[build]
command = "GOOS=wasip1 GOARCH=wasm go build -o main.wasm ."
output  = "main.wasm"

```

---

## Getting Started

```bash
flux login      # authenticate via browser (WorkOS)
flux init       # create project + write liquid-metal.toml
flux deploy     # build locally → upload → provision
flux status     # list your services
# → live at <name>.liquidmetal.dev

```

---

## Local Dev

```bash
task up           # Postgres + NATS + RustFS (docker compose)
task dev:api      # Rust API on :7070
task dev:web      # Go dashboard on :3000 (air hot reload)
task dev:proxy    # Pingora on :8080
task dev:daemon   # NATS consumer (Firecracker skipped on macOS)
task dev:cli -- login    # flux login
task dev:cli -- init     # flux init
task dev:cli -- deploy   # flux deploy
task dev:cli -- status   # flux status

```

### Linux (bare metal, one-time setup)

```bash
task metal:setup     # br0 bridge, /run/firecracker, Firecracker binary
task security:setup  # jailer user, cgroup v2 controllers, eBPF policy

```

---

## What We Don't Use

* No Kubernetes, K3s, or any orchestrator
* No AWS, GCP, Azure, Vercel, or Heroku
* No ORMs (raw SQL everywhere)
* No SPA frameworks (HTMX + Templ only)
* No container registry (Object Storage is the registry)

> For deep-dives into infrastructure topology, eBPF tenant isolation, HA strategy, and data flow see [ARCHITECTURE.md](ARCHITECTURE.md). For codebase layout and contribution rules see [CLAUDE.md](CLAUDE.md).

```
