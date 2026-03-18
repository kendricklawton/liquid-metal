//! Artifact integrity utilities shared by the CLI, API, and daemon.
//!
//! The integrity chain works as follows:
//!   1. Whoever produces the artifact (CLI local build, or API server build)
//!      calls `sha256_hex()` or `sha256_file()` to compute the digest.
//!   2. The digest travels with the deploy request and into the NATS ProvisionEvent.
//!   3. The daemon calls `verify()` after downloading the artifact from S3 —
//!      any tampering in transit or storage is caught before the VM boots or
//!      the Wasm module executes.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Compute the SHA-256 hex digest of an in-memory byte slice.
/// Used by the CLI after reading the built artifact into memory.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compute the SHA-256 hex digest of a file on disk.
/// Used by the API build runner after a server-side build completes.
pub async fn sha256_file(path: &str) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading artifact for hashing: {path}"))?;
    Ok(sha256_hex(&bytes))
}

/// Verify that `path` hashes to `expected_hex` (lowercase SHA-256).
/// Called by the daemon before booting a VM or executing a Wasm module.
pub async fn verify(path: &str, expected_hex: &str) -> Result<()> {
    let actual = sha256_file(path).await?;
    if actual != expected_hex.to_lowercase() {
        bail!(
            "artifact integrity FAILED for {path}\n  expected: {expected_hex}\n  computed: {actual}"
        );
    }
    tracing::debug!(path, "artifact SHA-256 verified");
    Ok(())
}

// ── ELF compatibility check ────────────────────────────────────────────────

/// Result of inspecting an ELF binary's dynamic linker.
#[derive(Debug)]
pub enum ElfCompat {
    /// Not an ELF binary (e.g. Wasm module, shell script). Skip the check.
    NotElf,
    /// Statically linked — no PT_INTERP segment. Always safe.
    Static,
    /// Dynamically linked against musl. Compatible with Alpine rootfs.
    Musl,
    /// Dynamically linked against glibc. Will crash on Alpine rootfs.
    Glibc { interp: String },
    /// Dynamically linked against an unknown linker.
    Unknown { interp: String },
}

/// Inspect an in-memory ELF binary and determine its dynamic linker.
///
/// Metal VMs run Alpine Linux (musl libc). Binaries linked against glibc
/// will fail at startup with "No such file or directory" because the glibc
/// dynamic linker (`/lib64/ld-linux-x86-64.so.2`) doesn't exist in Alpine.
///
/// This function parses just enough of the ELF header to extract the
/// PT_INTERP path. No external crates required.
pub fn check_elf_compat(bytes: &[u8]) -> ElfCompat {
    // ELF magic: 0x7f 'E' 'L' 'F'
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" {
        return ElfCompat::NotElf;
    }

    let class = bytes[4]; // 1 = 32-bit, 2 = 64-bit
    let _data = bytes[5]; // 1 = LE, 2 = BE (we assume LE for x86_64)

    let (ph_off, ph_ent_size, ph_num) = match class {
        2 => {
            // 64-bit ELF
            if bytes.len() < 64 { return ElfCompat::NotElf; }
            let ph_off      = u64::from_le_bytes(bytes[32..40].try_into().unwrap()) as usize;
            let ph_ent_size = u16::from_le_bytes(bytes[54..56].try_into().unwrap()) as usize;
            let ph_num      = u16::from_le_bytes(bytes[56..58].try_into().unwrap()) as usize;
            (ph_off, ph_ent_size, ph_num)
        }
        1 => {
            // 32-bit ELF
            if bytes.len() < 52 { return ElfCompat::NotElf; }
            let ph_off      = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
            let ph_ent_size = u16::from_le_bytes(bytes[42..44].try_into().unwrap()) as usize;
            let ph_num      = u16::from_le_bytes(bytes[44..46].try_into().unwrap()) as usize;
            (ph_off, ph_ent_size, ph_num)
        }
        _ => return ElfCompat::NotElf,
    };

    // Walk program headers looking for PT_INTERP (type = 3)
    const PT_INTERP: u32 = 3;

    for i in 0..ph_num {
        let base = ph_off + i * ph_ent_size;
        if base + ph_ent_size > bytes.len() { return ElfCompat::Static; }

        let p_type = u32::from_le_bytes(bytes[base..base + 4].try_into().unwrap());
        if p_type != PT_INTERP { continue; }

        // Extract offset and size of the interpreter string
        let (seg_off, seg_size) = match class {
            2 => {
                let off  = u64::from_le_bytes(bytes[base + 8..base + 16].try_into().unwrap()) as usize;
                let size = u64::from_le_bytes(bytes[base + 32..base + 40].try_into().unwrap()) as usize;
                (off, size)
            }
            1 => {
                let off  = u32::from_le_bytes(bytes[base + 4..base + 8].try_into().unwrap()) as usize;
                let size = u32::from_le_bytes(bytes[base + 16..base + 20].try_into().unwrap()) as usize;
                (off, size)
            }
            _ => return ElfCompat::NotElf,
        };

        if seg_off + seg_size > bytes.len() { return ElfCompat::Static; }

        let interp_bytes = &bytes[seg_off..seg_off + seg_size];
        let interp = std::str::from_utf8(interp_bytes)
            .unwrap_or("")
            .trim_end_matches('\0')
            .to_string();

        if interp.contains("ld-musl") {
            return ElfCompat::Musl;
        }
        if interp.contains("ld-linux") {
            return ElfCompat::Glibc { interp };
        }
        return ElfCompat::Unknown { interp };
    }

    // No PT_INTERP found — statically linked
    ElfCompat::Static
}

/// Check in-memory bytes for glibc linkage and return a user-facing error if found.
/// Returns Ok(()) for static, musl, non-ELF, or unknown linkers (with a warning log).
pub fn check_elf_compat_for_metal(bytes: &[u8]) -> Result<()> {
    match check_elf_compat(bytes) {
        ElfCompat::NotElf | ElfCompat::Static | ElfCompat::Musl => Ok(()),
        ElfCompat::Glibc { interp } => {
            bail!(
                "Binary is dynamically linked against glibc ({interp}).\n\
                 Metal VMs use Alpine Linux (musl). This binary will crash on startup.\n\n\
                 Fix: Build with musl:\n\
                 \x20 cargo build --target x86_64-unknown-linux-musl --release\n\n\
                 Or link statically:\n\
                 \x20 RUSTFLAGS=\"-C target-feature=+crt-static\" cargo build --release\n\n\
                 Go:  CGO_ENABLED=0 go build ...\n\
                 Zig: zig build -Dtarget=x86_64-linux-musl\n\n\
                 To skip this check: flux deploy --skip-elf-check"
            );
        }
        ElfCompat::Unknown { interp } => {
            tracing::warn!(
                interp,
                "binary uses an unrecognized dynamic linker — may not work on Alpine"
            );
            Ok(())
        }
    }
}

/// File-based variant for daemon-side checking after S3 download.
pub async fn check_elf_compat_file(path: &str) -> Result<()> {
    // Read only the first 64 KiB — enough for all ELF headers + PT_INTERP.
    // Avoids loading a multi-GB rootfs into memory.
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening artifact for ELF check: {path}"))?;
    let mut buf = vec![0u8; 65536];
    let n = file.read(&mut buf).await.unwrap_or(0);
    buf.truncate(n);
    check_elf_compat_for_metal(&buf)
}
