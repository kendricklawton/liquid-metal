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
        .unwrap()
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
    if active().lock().unwrap().remove(tap_name).is_some() {
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
pub fn reattach_all(tap_names: &[(String, String)]) {
    for (tap_name, service_id) in tap_names {
        if let Err(e) = attach(tap_name, service_id) {
            tracing::error!(
                tap        = tap_name.as_str(),
                service_id = service_id.as_str(),
                error      = %e,
                "failed to re-attach eBPF filter on daemon restart"
            );
        }
    }
}
