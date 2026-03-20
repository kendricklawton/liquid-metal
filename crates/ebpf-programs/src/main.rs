//! # TC Egress Classifier — Tenant Network Isolation for Firecracker VMs
//!
//! ## What this is
//!
//! A single eBPF program that runs inside the Linux kernel, attached to each
//! Firecracker VM's virtual network device (TAP). It inspects every outbound
//! packet from the VM and decides: pass it through, or drop it.
//!
//! ## Why it exists
//!
//! Each Firecracker VM gets a TAP device (tap0, tap1, ...) connected to a
//! shared Linux bridge (br0). Without this program, VM-A could craft packets
//! addressed to VM-B's IP and send them directly over the bridge — bypassing
//! the proxy entirely. That would be a tenant isolation failure.
//!
//! This program makes cross-VM communication physically impossible at the
//! kernel level. The VM can't bypass it because eBPF runs in kernel space,
//! outside the VM's execution context.
//!
//! ## The one rule
//!
//!   IF destination IP is in 172.16.0.0/12 → DROP
//!   OTHERWISE → PASS
//!
//! 172.16.0.0/12 is the IP range assigned to all guest VMs on br0. So this
//! means: "a VM can talk to the internet, the gateway, DNS — anything except
//! another VM on this node." Inter-service traffic goes through Pingora proxy,
//! which routes by slug, not by IP.
//!
//! ## How it gets into the kernel
//!
//! This crate is NOT part of the normal cargo workspace. It compiles to a
//! completely different target: BPF bytecode (bpfel-unknown-none), not x86.
//!
//! The build chain:
//!
//!   1. `crates/daemon/build.rs` runs `cargo build --target bpfel-unknown-none`
//!      on this crate using the nightly toolchain (BPF needs `-Z build-std=core`)
//!
//!   2. The output is a raw ELF object containing BPF instructions — NOT a
//!      normal executable. Think of it like a .so that the kernel loads.
//!
//!   3. `build.rs` copies the ELF to OUT_DIR. The daemon embeds it at compile
//!      time via `include_bytes_aligned!` in `ebpf.rs`
//!
//!   4. At runtime, when a VM is provisioned, `ebpf.rs` loads the BPF bytecode
//!      into the kernel via the `bpf()` syscall (through the Aya library)
//!
//!   5. Aya attaches it to the VM's TAP device as a TC egress classifier
//!
//! ## What `#![no_std]` and `#![no_main]` mean
//!
//! BPF programs run inside the kernel — no libc, no heap, no threads, no
//! file I/O. `no_std` means we only have `core` (basic types, math).
//! `no_main` means there's no `fn main()` — the kernel calls our
//! `#[classifier]` function directly when a packet hits the TC hook.
//!
//! ## What TC (Traffic Control) is
//!
//! TC is the Linux kernel subsystem for packet queuing, shaping, and filtering.
//! It has hook points where BPF programs can run:
//!
//!   - **Egress** (outbound): packets leaving the device — we use this one
//!   - **Ingress** (inbound): packets arriving at the device
//!
//! A "classifier" returns an action code for each packet:
//!   - `TC_ACT_OK` (0) = pass the packet through
//!   - `TC_ACT_SHOT` (2) = drop the packet silently
//!
//! ## Relationship to tc.rs (bandwidth shaping)
//!
//! tc.rs also uses TC but for bandwidth limiting via Token Bucket Filter (tbf)
//! qdiscs. The two are independent and coexist on the same TAP:
//!
//!   - This eBPF program: "should this packet be allowed at all?"
//!   - tc.rs tbf qdiscs: "how fast can allowed packets flow?"
//!
//! ## Fail-open design
//!
//! If the BPF program can't parse a packet (too short, malformed), it returns
//! TC_ACT_OK (pass). A broken packet won't route to another VM anyway.
//!
//! ## Security properties
//!
//! - Runs in kernel space: the VM cannot disable, modify, or bypass it
//! - BPF verifier checks the program before loading — no infinite loops,
//!   no out-of-bounds access, no kernel crashes
//! - Attached before the VM boots: enforced from the first packet
//! - On daemon restart, `reattach_all()` re-loads filters for running VMs.
//!   Any VM where re-attach fails is killed — no isolation = no running.
#![no_std]
#![no_main]

use aya_ebpf::{macros::classifier, programs::TcContext};

// ── TC action codes ─────────────────────────────────────────────────────────
// Return values the kernel expects from a TC classifier.
// Defined in linux/pkt_cls.h. Hardcoded here because we're #![no_std].
const TC_ACT_OK:   i32 = 0; // pass packet through to the next qdisc/filter
const TC_ACT_SHOT: i32 = 2; // drop packet immediately, free the skb

// ── Ethernet frame layout ───────────────────────────────────────────────────
//
// An Ethernet frame on the wire:
//
//   Bytes 0-5:   Destination MAC (6 bytes)
//   Bytes 6-11:  Source MAC (6 bytes)
//   Bytes 12-13: EtherType (2 bytes) ← what protocol is inside
//   Bytes 14+:   Payload (IP packet, ARP, etc.)
//
const ETH_HDR_LEN: usize = 14;     // total ethernet header size
const ETH_OFFSET_TYPE: usize = 12;  // offset to the 2-byte ethertype field

// EtherType 0x0800 = "the payload is an IPv4 packet"
// Other common values: 0x0806 = ARP, 0x86DD = IPv6
const ETH_P_IP: u16 = 0x0800;

// ── IPv4 header layout ──────────────────────────────────────────────────────
//
// IPv4 header (minimum 20 bytes):
//
//   Byte  0:     Version (4 bits) + IHL (4 bits)
//   Byte  1:     DSCP + ECN
//   Bytes 2-3:   Total length
//   Bytes 4-5:   Identification
//   Bytes 6-7:   Flags + Fragment offset
//   Byte  8:     TTL
//   Byte  9:     Protocol (6=TCP, 17=UDP, 1=ICMP)
//   Bytes 10-11: Header checksum
//   Bytes 12-15: Source IP address (4 bytes)
//   Bytes 16-19: Destination IP address (4 bytes) ← this is what we check
//
const IP_OFFSET_DST: usize = 16;

// ── Guest subnet ────────────────────────────────────────────────────────────
//
// 172.16.0.0/12 covers 172.16.0.0 – 172.31.255.255 (1,048,576 IPs).
// Every Firecracker VM on this node gets an IP from this range.
//
// To check if an IP is in this range: (ip & MASK) == SUBNET
//
//   172.16.0.5  = 0xAC100005 → 0xAC100005 & 0xFFF00000 = 0xAC100000 ✓ DROP
//   8.8.8.8     = 0x08080808 → 0x08080808 & 0xFFF00000 = 0x08000000 ✗ PASS
//   172.17.3.10 = 0xAC11030A → 0xAC11030A & 0xFFF00000 = 0xAC100000 ✓ DROP
//
// All comparisons in host byte order (little-endian on x86) after from_be().
const VM_SUBNET: u32 = 0xAC10_0000; // 172.16.0.0
const VM_MASK:   u32 = 0xFFF0_0000; // /12 mask = 255.240.0.0

/// Entry point — the kernel calls this for every packet leaving a VM's TAP.
///
/// The `#[classifier]` attribute generates the ELF section name
/// `classifier/tc_egress` so the Aya userspace loader can find it.
/// The function name `tc_egress` matches `bpf.program_mut("tc_egress")`
/// in ebpf.rs.
#[classifier]
pub fn tc_egress(ctx: TcContext) -> i32 {
    match classify(&ctx) {
        Ok(action) => action,
        // Fail open: can't parse → let it through. A malformed packet
        // won't route to another VM's IP anyway.
        Err(_) => TC_ACT_OK,
    }
}

/// Packet inspection logic.
///
/// `#[inline(always)]` is required — BPF can't do function calls on older
/// kernels (pre-5.10), and even on newer kernels inlining is preferred
/// because BPF has a limited stack (512 bytes).
#[inline(always)]
fn classify(ctx: &TcContext) -> Result<i32, i64> {
    // Step 1: Is this IPv4?
    //
    // ctx.load::<u16>(offset) reads 2 bytes from the packet at the given
    // offset. This is a BPF helper (bpf_skb_load_bytes) — bounds-checked
    // by the kernel. Returns Err if the packet is too short.
    //
    // We only inspect IPv4. ARP passes through (needed for MAC resolution).
    // IPv6 passes through (we don't assign v6 to VMs, nothing to isolate).
    let ethertype = u16::from_be(ctx.load::<u16>(ETH_OFFSET_TYPE)?);
    if ethertype != ETH_P_IP {
        return Ok(TC_ACT_OK);
    }

    // Step 2: Read destination IP.
    //
    // IPv4 header starts at byte 14 (after ethernet header).
    // Destination IP is at offset 16 within the IPv4 header.
    // Absolute offset: 14 + 16 = 30.
    let dst_ip = u32::from_be(ctx.load::<u32>(ETH_HDR_LEN + IP_OFFSET_DST)?);

    // Step 3: The isolation rule.
    //
    // One AND, one comparison. If destination is any guest IP on this
    // node's br0 bridge → drop. VMs talk to each other through Pingora,
    // never directly.
    if dst_ip & VM_MASK == VM_SUBNET {
        return Ok(TC_ACT_SHOT);
    }

    // Internet, DNS, gateway — pass through.
    Ok(TC_ACT_OK)
}

/// Required by `#![no_std]`. The kernel has no panic runtime. In practice
/// this is never reached — the BPF verifier rejects programs that could
/// panic before they're loaded.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
