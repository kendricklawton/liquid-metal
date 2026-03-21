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
pub(crate) async fn sha256_file(path: &str) -> Result<String> {
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
pub(crate) enum ElfCompat {
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
pub(crate) fn check_elf_compat(bytes: &[u8]) -> ElfCompat {
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
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── SHA-256 ──────────────────────────────────────────────────────────

    #[test]
    fn sha256_hex_empty() {
        // SHA-256 of empty input is a known constant.
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_hello() {
        let hash = sha256_hex(b"hello");
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn sha256_hex_deterministic() {
        let a = sha256_hex(b"some binary content");
        let b = sha256_hex(b"some binary content");
        assert_eq!(a, b);
    }

    #[test]
    fn sha256_hex_different_inputs() {
        let a = sha256_hex(b"binary-v1");
        let b = sha256_hex(b"binary-v2");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn sha256_file_matches_hex() {
        let dir = std::env::temp_dir().join("lm-test-sha256");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.bin");
        std::fs::write(&path, b"test content").unwrap();

        let file_hash = sha256_file(path.to_str().unwrap()).await.unwrap();
        let mem_hash = sha256_hex(b"test content");
        assert_eq!(file_hash, mem_hash);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn sha256_file_not_found() {
        let result = sha256_file("/tmp/lm-does-not-exist-ever.bin").await;
        assert!(result.is_err());
    }

    // ── verify() ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn verify_matching_hash() {
        let dir = std::env::temp_dir().join("lm-test-verify-ok");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("good.bin");
        std::fs::write(&path, b"good binary").unwrap();

        let expected = sha256_hex(b"good binary");
        verify(path.to_str().unwrap(), &expected).await.unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn verify_mismatched_hash() {
        let dir = std::env::temp_dir().join("lm-test-verify-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.bin");
        std::fs::write(&path, b"tampered binary").unwrap();

        let err = verify(path.to_str().unwrap(), "0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("integrity FAILED"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn verify_case_insensitive() {
        let dir = std::env::temp_dir().join("lm-test-verify-case");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("case.bin");
        std::fs::write(&path, b"case test").unwrap();

        let expected = sha256_hex(b"case test").to_uppercase();
        verify(path.to_str().unwrap(), &expected).await.unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn verify_file_not_found() {
        let result = verify("/tmp/lm-no-such-file.bin", "abc123").await;
        assert!(result.is_err());
    }

    // ── ELF compatibility ────────────────────────────────────────────────

    #[test]
    fn elf_not_elf_empty() {
        assert!(matches!(check_elf_compat(b""), ElfCompat::NotElf));
    }

    #[test]
    fn elf_not_elf_short() {
        assert!(matches!(check_elf_compat(b"hello world"), ElfCompat::NotElf));
    }

    #[test]
    fn elf_not_elf_wasm_magic() {
        // Wasm magic: \0asm
        assert!(matches!(check_elf_compat(b"\x00asm\x01\x00\x00\x00"), ElfCompat::NotElf));
    }

    #[test]
    fn elf_not_elf_wrong_magic() {
        let mut fake = vec![0u8; 128];
        fake[0..4].copy_from_slice(b"\x7fFOO");
        assert!(matches!(check_elf_compat(&fake), ElfCompat::NotElf));
    }

    #[test]
    fn elf_not_elf_bad_class() {
        // Valid ELF magic but invalid class byte (3)
        let mut fake = vec![0u8; 128];
        fake[0..4].copy_from_slice(b"\x7fELF");
        fake[4] = 3; // invalid class
        assert!(matches!(check_elf_compat(&fake), ElfCompat::NotElf));
    }

    #[test]
    fn elf_static_no_pt_interp() {
        // Minimal valid 64-bit ELF with 0 program headers → Static
        let mut elf = vec![0u8; 128];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // 64-bit
        elf[5] = 1; // little-endian
        // e_phoff = 64 (bytes 32..40)
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        // e_phentsize = 56 (bytes 54..56)
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        // e_phnum = 0 (bytes 56..58)
        elf[56..58].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(check_elf_compat(&elf), ElfCompat::Static));
    }

    #[test]
    fn elf_glibc_detected() {
        // 64-bit ELF with a PT_INTERP pointing to a glibc linker string
        let interp = b"/lib64/ld-linux-x86-64.so.2\0";
        let interp_offset: u64 = 120; // place interp string at byte 120
        let mut elf = vec![0u8; 120 + interp.len() + 16];

        // ELF header
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // 64-bit
        elf[5] = 1; // LE

        // Program header at offset 64
        let ph_off: u64 = 64;
        elf[32..40].copy_from_slice(&ph_off.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());  // e_phnum = 1

        // Program header: PT_INTERP (type = 3)
        let ph_base = ph_off as usize;
        elf[ph_base..ph_base + 4].copy_from_slice(&3u32.to_le_bytes()); // p_type = PT_INTERP
        // p_offset (bytes 8..16 in 64-bit phdr)
        elf[ph_base + 8..ph_base + 16].copy_from_slice(&interp_offset.to_le_bytes());
        // p_filesz (bytes 32..40 in 64-bit phdr)
        elf[ph_base + 32..ph_base + 40].copy_from_slice(&(interp.len() as u64).to_le_bytes());

        // Place the interpreter string
        elf[interp_offset as usize..interp_offset as usize + interp.len()]
            .copy_from_slice(interp);

        match check_elf_compat(&elf) {
            ElfCompat::Glibc { interp } => {
                assert!(interp.contains("ld-linux"));
            }
            other => panic!("expected Glibc, got {:?}", other),
        }
    }

    #[test]
    fn elf_musl_detected() {
        let interp = b"/lib/ld-musl-x86_64.so.1\0";
        let interp_offset: u64 = 120;
        let mut elf = vec![0u8; 120 + interp.len() + 16];

        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;

        let ph_off: u64 = 64;
        elf[32..40].copy_from_slice(&ph_off.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph_base = ph_off as usize;
        elf[ph_base..ph_base + 4].copy_from_slice(&3u32.to_le_bytes());
        elf[ph_base + 8..ph_base + 16].copy_from_slice(&interp_offset.to_le_bytes());
        elf[ph_base + 32..ph_base + 40].copy_from_slice(&(interp.len() as u64).to_le_bytes());

        elf[interp_offset as usize..interp_offset as usize + interp.len()]
            .copy_from_slice(interp);

        assert!(matches!(check_elf_compat(&elf), ElfCompat::Musl));
    }

    #[test]
    fn elf_unknown_linker() {
        let interp = b"/usr/lib/ld-freebsd.so.1\0";
        let interp_offset: u64 = 120;
        let mut elf = vec![0u8; 120 + interp.len() + 16];

        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;

        let ph_off: u64 = 64;
        elf[32..40].copy_from_slice(&ph_off.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph_base = ph_off as usize;
        elf[ph_base..ph_base + 4].copy_from_slice(&3u32.to_le_bytes());
        elf[ph_base + 8..ph_base + 16].copy_from_slice(&interp_offset.to_le_bytes());
        elf[ph_base + 32..ph_base + 40].copy_from_slice(&(interp.len() as u64).to_le_bytes());

        elf[interp_offset as usize..interp_offset as usize + interp.len()]
            .copy_from_slice(interp);

        match check_elf_compat(&elf) {
            ElfCompat::Unknown { interp } => {
                assert!(interp.contains("freebsd"));
            }
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn elf_32bit_static() {
        // 32-bit ELF with no PT_INTERP
        let mut elf = vec![0u8; 128];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // 32-bit
        elf[5] = 1; // LE
        // e_phoff = 52 (bytes 28..32 for 32-bit)
        elf[28..32].copy_from_slice(&52u32.to_le_bytes());
        // e_phentsize = 32 (bytes 42..44)
        elf[42..44].copy_from_slice(&32u16.to_le_bytes());
        // e_phnum = 0 (bytes 44..46)
        elf[44..46].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(check_elf_compat(&elf), ElfCompat::Static));
    }

    #[test]
    fn elf_truncated_header_64() {
        // ELF magic + class but too short for full header
        let mut elf = vec![0u8; 40];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // 64-bit
        // Only 40 bytes — not enough for 64-byte header
        // But check_elf_compat checks len < 64 at the top
        assert!(matches!(check_elf_compat(&elf), ElfCompat::NotElf));
    }

    #[test]
    fn elf_truncated_header_32() {
        let mut elf = vec![0u8; 30];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 1; // 32-bit
        // Only 30 bytes — not enough for 52-byte 32-bit header
        assert!(matches!(check_elf_compat(&elf), ElfCompat::NotElf));
    }

    // ── check_elf_compat_for_metal ───────────────────────────────────────

    #[test]
    fn metal_check_rejects_glibc() {
        let interp = b"/lib64/ld-linux-x86-64.so.2\0";
        let interp_offset: u64 = 120;
        let mut elf = vec![0u8; 120 + interp.len() + 16];

        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;

        let ph_off: u64 = 64;
        elf[32..40].copy_from_slice(&ph_off.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph_base = ph_off as usize;
        elf[ph_base..ph_base + 4].copy_from_slice(&3u32.to_le_bytes());
        elf[ph_base + 8..ph_base + 16].copy_from_slice(&interp_offset.to_le_bytes());
        elf[ph_base + 32..ph_base + 40].copy_from_slice(&(interp.len() as u64).to_le_bytes());

        elf[interp_offset as usize..interp_offset as usize + interp.len()]
            .copy_from_slice(interp);

        let err = check_elf_compat_for_metal(&elf).unwrap_err();
        assert!(err.to_string().contains("dynamically linked against glibc"));
    }

    #[test]
    fn metal_check_allows_static() {
        let mut elf = vec![0u8; 128];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&0u16.to_le_bytes());
        check_elf_compat_for_metal(&elf).unwrap();
    }

    #[test]
    fn metal_check_allows_musl() {
        let interp = b"/lib/ld-musl-x86_64.so.1\0";
        let interp_offset: u64 = 120;
        let mut elf = vec![0u8; 120 + interp.len() + 16];

        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        let ph_off: u64 = 64;
        elf[32..40].copy_from_slice(&ph_off.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());

        let ph_base = ph_off as usize;
        elf[ph_base..ph_base + 4].copy_from_slice(&3u32.to_le_bytes());
        elf[ph_base + 8..ph_base + 16].copy_from_slice(&interp_offset.to_le_bytes());
        elf[ph_base + 32..ph_base + 40].copy_from_slice(&(interp.len() as u64).to_le_bytes());
        elf[interp_offset as usize..interp_offset as usize + interp.len()]
            .copy_from_slice(interp);

        check_elf_compat_for_metal(&elf).unwrap();
    }

    #[test]
    fn metal_check_allows_non_elf() {
        check_elf_compat_for_metal(b"\x00asm\x01\x00\x00\x00").unwrap();
    }
}
