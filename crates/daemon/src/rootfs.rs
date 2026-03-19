//! Rootfs builder for Metal (Firecracker) services.
//!
//! The Metal engine boots user binaries inside Firecracker microVMs. The user
//! uploads a static musl binary; the daemon assembles a bootable ext4 rootfs by
//! injecting that binary into a cached Alpine template.
//!
//! Flow:
//!   1. `ensure_template()` — download + cache base Alpine ext4 from S3 (once)
//!   2. `build_rootfs()`    — copy template, inject binary + env vars, return path
//!
//! The template contains Alpine base (busybox, musl libc), an init script at
//! `/sbin/init` that sources `/etc/lm-env` and exec's `/app`, and placeholder
//! files at both paths. `build_rootfs` overwrites `/app` with the user binary
//! and `/etc/lm-env` with the service's environment variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::sync::OnceCell;

use crate::{storage, verify};

/// Guards concurrent template downloads on a fresh node. The first provision
/// to call `ensure_template` downloads; all others wait on this cell.
static TEMPLATE_READY: OnceCell<PathBuf> = OnceCell::const_new();

/// Template cache directory name (dot-prefixed so orphan cleanup skips it).
const TEMPLATE_DIR: &str = ".templates";
const TEMPLATE_FILENAME: &str = "base-alpine.ext4";

/// Ensure the base Alpine ext4 template exists locally.
///
/// Downloads from S3 on first call, then returns the cached path for all
/// subsequent calls. Thread-safe via `OnceCell` — concurrent provisions
/// on a fresh node block until the single download completes.
pub async fn ensure_template(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    base_image_key: &str,
    artifact_dir: &str,
    expected_sha: Option<&str>,
) -> Result<PathBuf> {
    TEMPLATE_READY
        .get_or_try_init(|| async {
            let template_dir = PathBuf::from(artifact_dir).join(TEMPLATE_DIR);
            let template_path = template_dir.join(TEMPLATE_FILENAME);

            if template_path.exists() {
                // Verify integrity if SHA is configured.
                if let Some(sha) = expected_sha {
                    verify::artifact(template_path.to_str().unwrap_or(""), sha)
                        .await
                        .context("base template integrity check failed — delete the cached file and retry")?;
                }
                tracing::info!(path = %template_path.display(), "base template cached");
                return Ok(template_path);
            }

            tracing::info!(key = base_image_key, "downloading base Alpine template from Object Storage");
            storage::download(s3, bucket, base_image_key, &template_path)
                .await
                .context("downloading base Alpine template")?;

            if let Some(sha) = expected_sha {
                verify::artifact(template_path.to_str().unwrap_or(""), sha)
                    .await
                    .context("base template integrity check after download")?;
            }

            tracing::info!(path = %template_path.display(), "base template ready");
            Ok(template_path)
        })
        .await
        .cloned()
}

/// Build a bootable rootfs for a Metal service.
///
/// 1. Copy the cached base template to `{artifact_dir}/{service_id}/rootfs.ext4`
/// 2. Download the user binary from S3 to `{artifact_dir}/{service_id}/app.bin`
/// 3. Verify SHA-256 + ELF compatibility of the binary
/// 4. Loop-mount the rootfs copy
/// 5. Copy binary → `/app`, write env vars → `/etc/lm-env`
/// 6. Unmount
///
/// Returns the path to the assembled rootfs.
pub async fn build_rootfs(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    artifact_key: &str,
    artifact_sha256: Option<&str>,
    service_id: &str,
    artifact_dir: &str,
    env_vars: &HashMap<String, String>,
    template_path: &Path,
    port: u16,
    node_id: &str,
) -> Result<PathBuf> {
    let service_dir = PathBuf::from(artifact_dir).join(service_id);
    let rootfs_path = service_dir.join("rootfs.ext4");
    let binary_path = service_dir.join("app.bin");
    let mount_dir = service_dir.join("mnt");

    tokio::fs::create_dir_all(&service_dir)
        .await
        .context("creating service artifact dir")?;

    // 1. Copy template → per-service rootfs
    tokio::fs::copy(template_path, &rootfs_path)
        .await
        .with_context(|| format!("copying template to {}", rootfs_path.display()))?;

    // 2. Download user binary from S3
    storage::download(s3, bucket, artifact_key, &binary_path)
        .await
        .context("downloading user binary from Object Storage")?;

    // 3. Verify integrity
    if let Some(expected) = artifact_sha256 {
        verify::artifact(binary_path.to_str().unwrap_or(""), expected)
            .await
            .context("user binary integrity check")?;
    } else {
        tracing::warn!(service_id, "no artifact_sha256 provided — skipping binary integrity check");
    }

    // 4. ELF compatibility check (catch glibc binaries before we boot a VM)
    common::artifact::check_elf_compat_file(binary_path.to_str().unwrap_or(""))
        .await
        .context("ELF compatibility check")?;

    // 5. Loop-mount, inject binary + env vars, unmount
    tokio::fs::create_dir_all(&mount_dir)
        .await
        .context("creating mount dir")?;

    // Mount — the daemon already runs as root (required for TAP/cgroup/jailer).
    let result = inject_into_rootfs(
        &rootfs_path, &binary_path, &mount_dir, env_vars, service_id, port, node_id,
    )
    .await;

    // Always try to unmount + clean up mount dir, even on failure.
    let _ = run_cmd("umount", &[mount_dir.to_str().unwrap_or("")]).await;
    let _ = tokio::fs::remove_dir(&mount_dir).await;

    result?;

    // Clean up the raw binary — it's now inside the rootfs.
    let _ = tokio::fs::remove_file(&binary_path).await;

    tracing::info!(
        service_id,
        path = %rootfs_path.display(),
        "rootfs assembled"
    );
    Ok(rootfs_path)
}

/// Mount the rootfs, copy the binary in, write env vars.
async fn inject_into_rootfs(
    rootfs_path: &Path,
    binary_path: &Path,
    mount_dir: &Path,
    env_vars: &HashMap<String, String>,
    service_id: &str,
    port: u16,
    node_id: &str,
) -> Result<()> {
    let rootfs_str = rootfs_path.to_str().unwrap_or("");
    let mount_str = mount_dir.to_str().unwrap_or("");

    run_cmd("mount", &["-o", "loop", rootfs_str, mount_str])
        .await
        .context("loop-mounting rootfs")?;

    // Copy binary → /app inside the rootfs
    let app_path = mount_dir.join("app");
    tokio::fs::copy(binary_path, &app_path)
        .await
        .context("copying binary into rootfs")?;

    // chmod +x
    run_cmd("chmod", &["+x", app_path.to_str().unwrap_or("")])
        .await
        .context("chmod +x /app")?;

    // Write env vars to /etc/lm-env
    let env_path = mount_dir.join("etc").join("lm-env");
    let env_content = build_env_file(env_vars, service_id, port, node_id);
    tokio::fs::write(&env_path, env_content.as_bytes())
        .await
        .context("writing /etc/lm-env")?;

    Ok(())
}

/// Build the `/etc/lm-env` file content.
///
/// Platform vars are written first, then user vars. User vars take precedence
/// (later `set -a; . /etc/lm-env` lines overwrite earlier ones).
///
/// Values are single-quoted to prevent shell expansion. Single quotes within
/// values are escaped as `'\''` (end quote, escaped quote, start quote).
fn build_env_file(
    env_vars: &HashMap<String, String>,
    service_id: &str,
    port: u16,
    node_id: &str,
) -> String {
    let mut lines = Vec::new();

    // Platform-injected vars
    lines.push(format!("PORT='{}'", port));
    lines.push(format!("LM_SERVICE_ID='{}'", escape_single_quote(service_id)));
    lines.push(format!("LM_NODE_ID='{}'", escape_single_quote(node_id)));

    // User-defined vars (override platform vars if same key)
    for (k, v) in env_vars {
        if !is_valid_env_key(k) {
            tracing::warn!(key = k, "skipping env var with invalid key (must match [A-Za-z_][A-Za-z0-9_]*)");
            continue;
        }
        lines.push(format!("{}='{}'", k, escape_single_quote(v)));
    }

    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// Escape single quotes for shell: `it's` → `it'\''s`
fn escape_single_quote(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Validate that an env var key is a legal POSIX shell variable name.
/// Rejects keys that would produce broken or injectable shell.
fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Run a command, returning an error if it exits non-zero.
async fn run_cmd(cmd: &str, args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .with_context(|| format!("spawning {cmd}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{cmd} failed ({}): {stderr}", output.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_format() {
        let mut vars = HashMap::new();
        vars.insert("DATABASE_URL".into(), "postgres://localhost/db".into());
        vars.insert("MY_SECRET".into(), "it's a secret".into());

        let content = build_env_file(&vars, "svc-123", 8080, "node-a");

        assert!(content.contains("PORT='8080'"));
        assert!(content.contains("LM_SERVICE_ID='svc-123'"));
        assert!(content.contains("LM_NODE_ID='node-a'"));
        assert!(content.contains("DATABASE_URL='postgres://localhost/db'"));
        assert!(content.contains("MY_SECRET='it'\\''s a secret'"));
    }

    #[test]
    fn escape_single_quotes() {
        assert_eq!(escape_single_quote("hello"), "hello");
        assert_eq!(escape_single_quote("it's"), "it'\\''s");
        assert_eq!(escape_single_quote("a'b'c"), "a'\\''b'\\''c");
    }

    #[test]
    fn valid_env_keys() {
        assert!(is_valid_env_key("FOO"));
        assert!(is_valid_env_key("_FOO"));
        assert!(is_valid_env_key("FOO_BAR"));
        assert!(is_valid_env_key("FOO123"));
        assert!(is_valid_env_key("_"));
    }

    #[test]
    fn invalid_env_keys() {
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("1FOO"));       // starts with digit
        assert!(!is_valid_env_key("FOO BAR"));     // space
        assert!(!is_valid_env_key("FOO;rm"));      // semicolon
        assert!(!is_valid_env_key("FOO=BAR"));     // equals
        assert!(!is_valid_env_key("FOO'BAR"));     // quote
    }
}
