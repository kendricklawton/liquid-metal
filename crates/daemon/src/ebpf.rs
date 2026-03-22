//! Aya eBPF tenant isolation for Firecracker VMs.
//!
//! When a Metal VM is provisioned, `attach()` loads the compiled TC classifier
//! from crates/ebpf-programs into the kernel and pins it to the VM's tap{n}
//! device as a TC egress hook. The program enforces:
//!
//!   ISOLATION   — drops packets destined to 172.16.0.0/12 (other guest IPs)
//!                 at the kernel level, before they can reach br0's forwarding
//!                 table. Zero userspace overhead, cannot be bypassed from
//!                 inside the VM.
//!
//!   PASSTHROUGH — all other egress (internet, gateway, DNS) passes unmodified.
//!                 Bandwidth shaping lives in tc.rs and is unaffected.
//!
//! When the VM is deprovisioned, `detach()` drops the `Ebpf` handle, which
//! causes Aya to detach all associated TC hooks and unload the BPF programs.
//!
//! Linux only. On macOS the entire module is compiled away by the cfg gate.
#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use aya::{
    include_bytes_aligned,
    programs::{tc::TcAttachType, SchedClassifier},
    Ebpf,
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

// ── Compiled BPF object embedded at build time ────────────────────────────────
//
// build.rs compiles crates/ebpf-programs targeting bpfel-unknown-none and
// copies the result to OUT_DIR. include_bytes_aligned! ensures the bytes are
// aligned to the 8-byte boundary that aya::Ebpf::load requires.
static TC_FILTER_BPF: &[u8] =
    include_bytes_aligned!(concat!(env!("OUT_DIR"), "/tc-filter"));

// ── Active eBPF handles ───────────────────────────────────────────────────────
//
// We keep the Ebpf struct alive for each TAP — dropping it detaches all
// programs and maps associated with that load. Keyed by tap name ("tap0", etc).
//
// Bounded by MAX_TAP_INDEX (the TAP index pool) — cannot exceed the number
// of active Metal VMs. detach() reliably removes entries on deprovision,
// crash_watcher cleanup, and daemon restart (map starts empty, reattach_all
// repopulates only from DB).
fn active() -> &'static Mutex<HashMap<String, Ebpf>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, Ebpf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Attach the TC isolation filter to `tap_name`.
///
/// Called immediately after TAP creation and tc bandwidth setup in
/// provision.rs, before Firecracker boots. The kernel starts enforcing
/// the isolation policy before the VM can send a single packet.
pub fn attach(tap_name: &str, service_id: &str) -> Result<()> {
    let mut bpf = Ebpf::load(TC_FILTER_BPF)
        .context("loading TC filter BPF object")?;

    // Load the tc_egress classifier into the kernel
    let prog: &mut SchedClassifier = bpf
        .program_mut("tc_egress")
        .context("BPF program 'tc_egress' not found in object")?
        .try_into()
        .context("tc_egress is not a SchedClassifier")?;

    prog.load()
        .context("loading tc_egress program into kernel")?;

    // Attach as TC egress hook on the TAP device.
    // Egress = packets leaving the VM (VM → br0 → internet or other VMs).
    prog.attach(tap_name, TcAttachType::Egress)
        .with_context(|| format!("attaching tc_egress to {tap_name}"))?;

    // Store handle — keeps the program alive in the kernel
    active()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(tap_name.to_string(), bpf);

    tracing::info!(
        tap       = tap_name,
        service_id,
        "eBPF TC isolation attached — 172.16.0.0/12 egress blocked"
    );

    Ok(())
}

/// Detach the TC filter from `tap_name`.
///
/// Called during deprovision after the VM process has been stopped and before
/// the TAP device is removed. Dropping the Ebpf handle causes Aya to detach
/// the TC hook and unload the BPF program from the kernel.
pub fn detach(tap_name: &str) {
    if active().lock().unwrap_or_else(|e| e.into_inner()).remove(tap_name).is_some() {
        tracing::info!(tap = tap_name, "eBPF TC filter detached");
    } else {
        tracing::warn!(tap = tap_name, "eBPF detach called but no active filter found");
    }
}

/// Re-attach filters for all active Metal services on daemon startup.
///
/// If the daemon restarts while VMs are still running, their eBPF programs
/// were unloaded when the previous process exited. This function queries the
/// database for running Metal services on this node and re-attaches the TC
/// filter to each TAP, restoring the isolation policy.
///
/// Returns a list of (tap_name, service_id) pairs where re-attach **failed**.
/// The caller MUST kill these VMs — running without isolation is not acceptable.
pub fn reattach_all(tap_names: &[(String, String)]) -> Vec<(String, String)> {
    let mut failed = Vec::new();
    for (tap_name, service_id) in tap_names {
        if let Err(e) = attach(tap_name, service_id) {
            tracing::error!(
                tap        = tap_name.as_str(),
                service_id = service_id.as_str(),
                error      = %e,
                "CRITICAL: failed to re-attach eBPF filter — VM will be killed to prevent isolation breach"
            );
            failed.push((tap_name.clone(), service_id.clone()));
        }
    }
    failed
}

// ── Runtime filter verification ─────────────────────────────────────────────
//
// The attach-time guarantee is necessary but not sufficient. Filters can
// disappear at runtime if:
//
//   1. Someone runs `tc filter del` manually (ops mistake, rogue script)
//   2. The TAP device is deleted out from under us (kernel driver bug)
//   3. A kernel upgrade or module reload clears TC state
//   4. The Ebpf handle is dropped due to a bug in our code
//
// Any of these silently removes the isolation barrier. The VM keeps running,
// but packets to 172.16.0.0/12 now pass through — full tenant breach.
//
// `audit_filters()` catches this by asking the kernel directly: "is there
// a BPF classifier on this TAP's egress?" If not, the caller kills the VM.
//
// Both functions are async — they use tokio::process::Command so the `tc`
// subprocess doesn't block a tokio worker thread. This matters when checking
// many TAPs in sequence; a blocking call per TAP would stall whatever else
// is scheduled on that thread (pulse flushes, NATS acks, etc).

/// Check whether a BPF TC egress filter is attached to `tap_name`.
///
/// Runs `tc filter show dev {tap} egress` asynchronously and looks for "bpf"
/// in the output. This is the same check an operator would run manually.
///
/// Returns `false` if:
///   - The TAP device doesn't exist (deleted underneath us)
///   - No BPF filter is attached to egress (filter was removed)
///   - The `tc` command itself fails (binary missing, permission denied)
///
/// Uses `tokio::process::Command` (non-blocking) instead of
/// `std::process::Command` to avoid stalling the tokio runtime. The `tc`
/// binary finishes in <1ms typically, but we're called once per active TAP
/// every 30s — blocking would serialize those checks on a single worker
/// thread and delay other async work sharing that thread.
async fn verify_filter(tap_name: &str) -> bool {
    match tokio::process::Command::new("tc")
        .args(["filter", "show", "dev", tap_name, "egress"])
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // `tc filter show` output includes "bpf" when a BPF classifier
            // is attached. Example line:
            //   filter protocol all pref 49152 bpf chain 0 handle 0x1 tc-filter ...
            output.status.success() && stdout.contains("bpf")
        }
        Err(e) => {
            tracing::error!(
                tap = tap_name,
                error = %e,
                "tc command failed — cannot verify eBPF filter"
            );
            false
        }
    }
}

/// Audit all active eBPF filters. Returns TAP names where the TC egress
/// classifier is missing — these VMs are running without tenant isolation.
///
/// Called periodically by the health checker in main.rs. The caller MUST
/// kill every VM in the returned list. No exceptions.
///
/// The mutex is held only long enough to snapshot the TAP names, then
/// released before spawning any `tc` subprocesses. This prevents the lock
/// from being held across await points (which would block attach/detach
/// from other tasks).
pub async fn audit_filters() -> Vec<String> {
    // Snapshot keys under the lock, then release immediately.
    // Holding a std::sync::Mutex across .await is undefined behaviour
    // (can deadlock the runtime), so we copy and drop.
    let tap_names: Vec<String> = {
        let map = active().lock().unwrap_or_else(|e| e.into_inner());
        map.keys().cloned().collect()
    };

    let mut missing = Vec::new();
    for tap_name in &tap_names {
        if !verify_filter(tap_name).await {
            tracing::error!(
                tap = tap_name.as_str(),
                "ISOLATION BREACH: eBPF TC filter missing from running VM"
            );
            missing.push(tap_name.clone());
        }
    }
    missing
}
