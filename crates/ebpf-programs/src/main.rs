//! TC classifier — tenant isolation for Liquid Metal Firecracker VMs.
//!
//! Attached to each VM's tap{n} device as a TC egress hook. Enforces two rules
//! at the kernel level with zero userspace overhead:
//!
//!   ISOLATION: Drop any packet from this VM whose destination falls inside
//!              172.16.0.0/12 — the range used for guest IPs on br0. This
//!              prevents cross-tenant communication regardless of what runs
//!              inside the VM.
//!
//!   PASSTHROUGH: All other packets (internet, gateway, DNS) pass unmodified.
//!                Bandwidth shaping is handled separately by tc.rs tbf qdiscs.
//!
//! This program is compiled to BPF bytecode by crates/daemon/build.rs and
//! embedded into the daemon binary. The Aya userspace loader in ebpf.rs
//! attaches it per-TAP when a VM is provisioned and detaches on deprovision.
#![no_std]
#![no_main]

use aya_ebpf::{macros::classifier, programs::TcContext};

// TC action codes
const TC_ACT_OK:   i32 = 0; // pass packet
const TC_ACT_SHOT: i32 = 2; // drop packet

// Ethernet header is 14 bytes: 6 dst MAC + 6 src MAC + 2 ethertype
const ETH_HDR_LEN: usize = 14;

// EtherType offset within ethernet header
const ETH_OFFSET_TYPE: usize = 12;

// IPv4 ethertype (big-endian)
const ETH_P_IP: u16 = 0x0800;

// Offset of destination IP within IPv4 header (from IP header start):
//   0: ver+ihl (1), 1: dscp (1), 2: tot_len (2), 4: id (2),
//   6: frag_off (2), 8: ttl (1), 9: protocol (1), 10: checksum (2),
//  12: src_addr (4), 16: dst_addr (4)
const IP_OFFSET_DST: usize = 16;

// 172.16.0.0/12 — the entire guest IP range on br0.
// All comparisons are in host byte order after from_be().
const VM_SUBNET: u32 = 0xAC10_0000; // 172.16.0.0
const VM_MASK:   u32 = 0xFFF0_0000; // 255.240.0.0 (/12)

/// TC egress classifier — called for every packet leaving a VM tap device.
#[classifier]
pub fn tc_egress(ctx: TcContext) -> i32 {
    match classify(&ctx) {
        Ok(action) => action,
        Err(_)     => TC_ACT_OK, // fail open: never silently drop on error
    }
}

#[inline(always)]
fn classify(ctx: &TcContext) -> Result<i32, i64> {
    // Only inspect IPv4 — pass ARP, IPv6, etc. unmodified
    let ethertype = u16::from_be(ctx.load::<u16>(ETH_OFFSET_TYPE)?);
    if ethertype != ETH_P_IP {
        return Ok(TC_ACT_OK);
    }

    // Load dst IP from IPv4 header (network byte order → host order)
    let dst_ip = u32::from_be(ctx.load::<u32>(ETH_HDR_LEN + IP_OFFSET_DST)?);

    // Drop if destination is any guest IP on this node's br0 bridge.
    // A VM should never need to address another VM directly — all legitimate
    // inter-service traffic goes through the Pingora proxy.
    if dst_ip & VM_MASK == VM_SUBNET {
        return Ok(TC_ACT_SHOT);
    }

    Ok(TC_ACT_OK)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
