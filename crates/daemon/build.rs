//! Build script for the daemon crate.
//!
//! On Linux hosts, compiles crates/ebpf-programs targeting bpfel-unknown-none
//! and copies the resulting BPF object to OUT_DIR so ebpf.rs can embed it with
//! include_bytes_aligned!.
//!
//! On macOS (dev), the eBPF build is skipped entirely. The #[cfg(target_os =
//! "linux")] gate in ebpf.rs ensures the embedded bytes are never referenced
//! in a non-Linux binary.
//!
//! Prerequisites (Linux only):
//!   rustup target add bpfel-unknown-none
//!   rustup component add rust-src

use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
};

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ebpf_dir     = manifest_dir.join("../../crates/ebpf-programs");
    let out_dir      = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Build all eBPF programs in the ebpf-programs crate
    let status = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--target", "bpfel-unknown-none",
        ])
        .current_dir(&ebpf_dir)
        // Forward nightly flags required by build-std
        .env("RUSTFLAGS", "-C panic=abort")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .expect("cargo build for ebpf-programs failed to start");

    assert!(
        status.success(),
        "eBPF program build failed — run: \
         rustup target add bpfel-unknown-none && rustup component add rust-src"
    );

    // Copy compiled BPF objects to OUT_DIR so include_bytes_aligned! can find them
    let bpf_release = ebpf_dir.join("target/bpfel-unknown-none/release");

    for prog in &["tc-filter"] {
        let src = bpf_release.join(prog);
        let dst = out_dir.join(prog);
        std::fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("failed to copy BPF object {prog}: {e}"));
    }

    println!("cargo:rerun-if-changed=../../crates/ebpf-programs/src");
    println!("cargo:rerun-if-changed=../../crates/ebpf-programs/Cargo.toml");
}
