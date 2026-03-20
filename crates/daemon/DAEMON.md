# Daemon Internals

> How the daemon works, file by file.

The daemon is a NATS consumer that provisions and manages Firecracker microVMs (Metal) and Wasmtime instances (Liquid). It runs on bare metal compute nodes with KVM access. This document explains every file, how they connect, and the flows that tie them together.

---

## Architecture at a Glance

```
NATS JetStream
  │
  ├── platform.provision  ──→  run.rs (select loop)  ──→  provision.rs
  ├── platform.deprovision ─→  run.rs (select loop)  ──→  deprovision.rs
  └── platform.wake  ───────→  run.rs (select loop)  ──→  wake.rs
                                    │
                                    ├── tasks/pulse.rs        (NATS core sub)
                                    ├── tasks/idle.rs         (timer)
                                    ├── tasks/orphan_sweep.rs (timer)
                                    ├── tasks/crash_watcher.rs(timer, Linux)
                                    ├── tasks/ebpf_audit.rs   (timer, Linux)
                                    ├── tasks/usage.rs        (timer)
                                    ├── tasks/suspend.rs      (NATS core sub)
                                    ├── tasks/lag_monitor.rs  (timer)
                                    └── tasks/health.rs       (TCP server)
```

---

## File Map (28 files)

### Entry Point

| File | Lines | What It Does |
|------|-------|-------------|
| `main.rs` | 7 | Thin entry point. Inits tracing, calls `run::run()`. |
| `lib.rs` | 25 | Module declarations. `pub mod` for every module below. |

### Orchestration

| File | Lines | What It Does |
|------|-------|-------------|
| `run.rs` | ~380 | **The brain.** Startup sequence, spawns all background tasks, runs the main `select!` event loop (provision/deprovision/wake consumers), handles graceful shutdown. |
| `startup.rs` | ~240 | One-time initialization before any tasks spawn: PID lock, config from env, Postgres pool, S3 client, cleanup of stale state from previous daemon, NATS connect, VM registry rebuild. |

### Core Operations

| File | Lines | What It Does |
|------|-------|-------------|
| `provision.rs` | ~1027 | **Largest file.** Orchestrates the full provision flow for both engines. Downloads artifacts from S3, builds rootfs (Metal), compiles Wasm (Liquid), boots VMs, runs startup probes, writes to DB, publishes route events. Also contains cleanup functions called by startup.rs (orphaned TAPs, artifacts, cgroups, stale provisioning). |
| `deprovision.rs` | ~114 | Tears down a running service. Metal: SIGTERM/SIGKILL the FC process, detach eBPF, remove tc qdiscs, delete TAP, cleanup cgroup, remove artifacts, cleanup jailer chroot. Liquid: just removes the registry entry. Also defines `VmRegistry`, `LiquidRegistry`, and `VmHandle`. |
| `wake.rs` | ~155 | Restores a Metal VM from a Firecracker snapshot. Downloads snapshot files from S3, creates TAP, spawns FC, loads snapshot, health checks, publishes route event. Called when the proxy detects a cold service. |

### Kernel Resource Modules (Linux only)

These modules each own one kernel subsystem. They are called by `provision.rs`, `deprovision.rs`, and `wake.rs` — never directly from the event loop.

| File | Lines | What It Does |
|------|-------|-------------|
| `firecracker.rs` | ~145 | REST client for Firecracker's Unix socket API. `start_vm()` configures and boots a microVM (machine-config, boot-source, drives, network, InstanceStart). `load_snapshot()` restores from snapshot. Talks HTTP/1.1 over UDS via hyper. |
| `netlink.rs` | ~65 | TAP device lifecycle via rtnetlink. `create_tap()` (ioctl TUNSETIFF), `attach_to_bridge()`, `delete_tap()`. Raw kernel interface — no shell commands. |
| `ebpf.rs` | ~217 | Aya eBPF tenant isolation. Loads a TC egress classifier onto each VM's TAP device that drops packets to 172.16.0.0/12 (other guest IPs). `attach()`, `detach()`, `reattach_all()` (daemon restart), `audit_filters()` (runtime verification via `tc filter show`). |
| `tc.rs` | ~145 | Bandwidth shaping via iproute2 `tc` command. Applies Token Bucket Filter (tbf) qdiscs for egress, uses IFB devices for ingress shaping. |
| `cgroup.rs` | ~128 | cgroup v2 enforcement. Moves FC process into `/sys/fs/cgroup/liquid-metal/{service_id}`, sets `memory.max`, `pids.max`, `cpu.weight`, `io.max`. |
| `jailer.rs` | ~231 | Firecracker Jailer integration. Spawns FC inside PID namespace + mount namespace + chroot + uid remap + seccomp. Hard-links artifacts into chroot. Handles cleanup including stale bind mount removal. |
| `rootfs.rs` | ~310 | Builds bootable ext4 rootfs images for Metal VMs. Downloads base Alpine template from S3, copies it, loop-mounts, injects user binary at `/app` and env vars at `/etc/lm-env`, unmounts. |
| `snapshot.rs` | ~103 | Snapshot restore support. Downloads vmstate.snap + memory.snap from S3 (with local cache), spawns a bare Firecracker process, calls `firecracker::load_snapshot()`. |

### Infrastructure Modules (cross-platform)

| File | Lines | What It Does |
|------|-------|-------------|
| `storage.rs` | ~154 | S3-compatible object storage client (MinIO locally, Vultr in prod). `build_client()`, `upload()`, `download()` with atomic write-then-rename and configurable timeouts. |
| `verify.rs` | ~12 | SHA-256 artifact integrity verification. Thin wrapper around `common::artifact::verify()`. |
| `wasm_http.rs` | ~530 | WAGI (WebAssembly Gateway Interface) HTTP shim for Liquid services. Compiles Wasm module once (with disk cache), spawns a hyper HTTP server on a random port, dispatches each request as a fresh Wasm instance with CGI env vars. Includes fuel metering, memory limits, concurrency control, and wall-clock timeouts. |

### Background Tasks

Each file exports a single `spawn()` function that calls `tokio::spawn` internally. They run for the lifetime of the daemon.

| File | Interval | What It Does |
|------|----------|-------------|
| `tasks/pulse.rs` | 5s batch window | Subscribes to `platform.traffic_pulse` from Pingora. Batches slug updates and flushes `services.last_request_at` in a single SQL UPDATE. Prevents NATS flood from hammering Postgres. |
| `tasks/idle.rs` | 60s | Finds Metal services idle longer than `IDLE_TIMEOUT_SECS`. Marks them stopped and publishes `DeprovisionEvent`. Retries NATS publish 3x, reverts to running on failure. |
| `tasks/orphan_sweep.rs` | 60s | Safety net for workspace deletion. Finds services still running whose workspace was soft-deleted, publishes `DeprovisionEvent` for each. |
| `tasks/crash_watcher.rs` | 10s (Linux) | Checks if tracked Firecracker PIDs are still alive via `waitpid`/`/proc`. On crash: marks DB as crashed, evicts proxy route, publishes `ServiceCrashedEvent`, cleans up kernel resources. |
| `tasks/ebpf_audit.rs` | 30s (Linux) | Asks the kernel if each active TAP still has a BPF TC egress classifier. If missing (filter was removed by operator, bug, or kernel event), SIGKILLs the VM immediately — running without tenant isolation is not acceptable. |
| `tasks/usage.rs` | 60s | Drains atomic invocation counters from the Liquid registry, publishes `LiquidUsageEvent` per service for billing. Maintains a per-service backlog for failed publishes. |
| `tasks/suspend.rs` | event-driven | Subscribes to `platform.suspend` (workspace balance depleted). Three-phase: (1) mark draining + evict routes, (2) wait `SUSPEND_DRAIN_SECS` for in-flight requests, (3) kill VMs / remove Wasm handlers. |
| `tasks/lag_monitor.rs` | 30s | Checks JetStream provision consumer pending count. Warns when lag > 50 — means the daemon can't keep up with deploy volume. |
| `tasks/health.rs` | on-demand | HTTP health endpoint on `HEALTH_PORT` (default 9090). Returns JSON with node_id, uptime, VM count, Wasm service count. Used by Nomad health checks. |

---

## Lifecycle Flows

### Metal Provision (dedicated VM)

```
ProvisionEvent arrives via NATS
  │
  ├── provision.rs::provision()
  │     ├── rootfs::ensure_template()     — download base Alpine from S3 (once)
  │     ├── rootfs::build_rootfs()        — copy template, download binary, inject into ext4
  │     ├── netlink::create_tap()         — create TAP device
  │     ├── netlink::attach_to_bridge()   — attach TAP to br0
  │     ├── tc::apply()                   — bandwidth shaping
  │     ├── ebpf::attach()                — tenant isolation filter
  │     ├── jailer::spawn() OR spawn_firecracker_direct()
  │     ├── cgroup::apply()               — memory, PIDs, IO limits
  │     ├── firecracker::start_vm()       — configure + boot via API socket
  │     ├── startup_probe()               — HTTP GET / until response
  │     ├── registry.insert()             — track handle for deprovision/crash
  │     └── UPDATE services SET status='running', upstream_addr=...
  │
  └── publish RouteUpdatedEvent → proxy caches the route
```

### Liquid Provision (Wasm)

```
ProvisionEvent arrives via NATS
  │
  ├── provision.rs::provision_liquid()
  │     ├── storage::download()           — download .wasm from S3
  │     ├── verify::artifact()            — SHA-256 check
  │     ├── wasm_http::serve()            — compile module, bind random port, start HTTP shim
  │     ├── startup_probe()               — HTTP GET / to verify shim responds
  │     ├── liquid_registry.insert()      — track for billing + deprovision
  │     └── UPDATE services SET status='running', upstream_addr=127.0.0.1:{port}
  │
  └── publish RouteUpdatedEvent → proxy caches the route
```

### Metal Deprovision

```
DeprovisionEvent arrives via NATS
  │
  ├── registry.remove()                   — take handle atomically
  ├── provision::release_tap_index()      — return index to pool
  └── deprovision::metal()
        ├── SIGTERM → wait 500ms → SIGKILL
        ├── ebpf::detach()
        ├── tc::remove()
        ├── netlink::delete_tap()
        ├── cgroup::cleanup()
        ├── rm -rf artifact dir
        └── jailer::cleanup() (if jailed)
```

### Wake from Snapshot (cold start)

```
WakeEvent arrives via NATS (proxy detected cold service)
  │
  ├── wake.rs::wake_from_snapshot()
  │     ├── snapshot::ensure_snapshot()   — download vmstate + memory from S3
  │     ├── netlink::create_tap()
  │     ├── netlink::attach_to_bridge()
  │     ├── tc::apply()
  │     ├── ebpf::attach()
  │     ├── snapshot::restore_vm()        — spawn FC + load_snapshot()
  │     ├── cgroup::apply()
  │     ├── TCP connect probe (5s)
  │     ├── registry.insert()
  │     └── UPDATE services SET status='running'
  │
  └── publish RouteUpdatedEvent → proxy unblocks held request
```

### Daemon Startup

```
main.rs → run::run()
  │
  ├── startup::acquire_pid_lock()         — flock to prevent double-daemon
  ├── startup::build_config()             — ProvisionConfig from env vars
  ├── startup::build_pool()               — Postgres connection pool
  ├── storage::build_client()             — S3 client
  │
  ├── startup::run_cleanup()
  │     ├── Finalize orphaned 'draining' → 'suspended'
  │     ├── Mark stale Liquid services as 'stopped'
  │     ├── provision::init_tap_indices()
  │     ├── provision::cleanup_orphaned_taps()
  │     ├── provision::cleanup_orphaned_artifacts()
  │     ├── provision::cleanup_orphaned_cgroups()
  │     ├── provision::cleanup_stale_provisioning()
  │     └── provision::check_disk_space()
  │
  ├── startup::check_clock_drift()        — compare local vs Postgres time
  ├── startup::reattach_ebpf()            — re-attach filters for running VMs
  ├── startup::rebuild_registry()         — VmHandle map from DB
  ├── startup::connect_nats_and_evict()   — NATS connect + evict stale routes
  │
  ├── Spawn all background tasks
  ├── Create JetStream consumers (provision, deprovision, wake)
  ├── Bind health check listener
  │
  └── Enter select! event loop (runs until SIGTERM/Ctrl-C)
```

### Graceful Shutdown

```
SIGTERM / Ctrl-C
  │
  ├── Break out of select! loop
  ├── Drain in-flight JoinSet tasks (30s timeout)
  ├── For each VM in registry:
  │     ├── UPDATE services SET status='stopped'
  │     ├── publish RouteRemovedEvent
  │     └── deprovision::metal()
  └── Exit
```
pub mod wasm_http;
---pub mod wasm_http;

## Shared Statepub mod wasm_http;

All background tasks and the event loop share state through `Arc`:

| Type | Defined In | Purpose |
|------|-----------|---------|
| `VmRegistry` (`Arc<Mutex<HashMap<String, VmHandle>>>`) | `deprovision.rs` | Tracks running Metal VMs. Written by provision/wake, read by crash_watcher/ebpf_audit, drained by deprovision/shutdown. |
| `LiquidRegistry` (`Arc<Mutex<HashMap<String, LiquidHandle>>>`) | `deprovision.rs` | Tracks running Liquid services. Written by provision, drained by usage reporter, removed by deprovision/suspend. |
| `ProvisionCtx` | `provision.rs` | Bundle of pool + config + S3 + bucket + registries + NATS. Passed to provision/deprovision/wake tasks. |
| `ProvisionConfig` | `provision.rs` | All env-var-sourced paths and settings (FC binary, kernel, bridge, jailer, node_id, artifact_dir). |

---

## Security Layers (Metal)

Each Metal VM has four independent isolation layers, applied in order during provision:

```
Layer 0: KVM             — hardware virtualization (Firecracker)
Layer 1: eBPF            — TC egress filter drops 172.16.0.0/12 (ebpf.rs)
Layer 2: tc              — bandwidth caps via Token Bucket Filter (tc.rs)
Layer 3: cgroup v2       — memory, PIDs, IO, CPU weight (cgroup.rs)
Layer 4: Jailer          — PID ns, mount ns, chroot, uid remap, seccomp (jailer.rs)
```

The eBPF audit task (`tasks/ebpf_audit.rs`) continuously verifies Layer 1 at runtime. If a filter disappears, the VM is killed immediately — there are no second chances for isolation breaches.

---

## Security Layers (Liquid)

Liquid services run in-process via Wasmtime with:

- **Linear memory isolation** — Wasm has no pointers to host memory
- **Fuel metering** — 1B instruction budget per request, deterministic trap on exhaustion
- **Memory limits** — `ResourceLimiter` caps linear memory at 128 MiB
- **Concurrency control** — semaphore limits parallel Wasm instances per service
- **Wall-clock timeout** — 30s default per request

---

## Key Invariants

1. **The daemon never writes to Postgres outside of provision/deprovision/wake flows and background task maintenance queries.** It is a NATS consumer, not an API server.

2. **Every kernel resource allocated during provision has a cleanup path.** TAP indices are tracked in a `BTreeSet`, VM handles in the registry, cgroup dirs by service_id. Startup cleanup catches anything leaked by a crash.

3. **eBPF filters are non-negotiable.** If `reattach_all` fails on startup, the VM is killed. If `audit_filters` detects a missing filter at runtime, the VM is killed. No exceptions.

4. **The PID file lock prevents two daemons on the same node.** Without this, two instances would double-provision and corrupt the TAP index space.

5. **TAP indices are gap-filled, not monotonic.** The `BTreeSet` allocator reuses the lowest available index, preventing IP address space exhaustion after many create/delete cycles.

6. **Wasm modules are compiled once and cached to disk.** The `.compiled` file includes a SHA-256 prefix for integrity. Cache hits deserialize in <10ms vs 60-180s for full compilation.
