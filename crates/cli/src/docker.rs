use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};

/// Guard that cleans up Docker artifacts on drop (even on error).
struct DockerCleanup {
    container_name: String,
    image_tag: String,
    temp_path: Option<PathBuf>,
}

impl Drop for DockerCleanup {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.container_name])
            .output();
        let _ = Command::new("docker")
            .args(["rmi", &self.image_tag])
            .output();
        if let Some(path) = &self.temp_path {
            let _ = fs::remove_file(path);
        }
    }
}

/// Build a binary via Dockerfile and extract it from the image.
///
/// Docker is used as a build tool only — no containers at runtime.
pub fn build_via_dockerfile(slug: &str, dockerfile: &str, output: &str) -> Result<Vec<u8>> {
    // Check Docker is available
    let version_check = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output();

    match version_check {
        Ok(out) if out.status.success() => {}
        _ => bail!(
            "Docker is not installed or not running.\n\n\
             Dockerfile builds require Docker. Install it from:\n\
             https://docs.docker.com/get-docker/\n\n\
             Alternatively, replace [build].dockerfile with a [build].command\n\
             that builds natively."
        ),
    }

    let image_tag = format!("lm-build-{slug}");
    let container_name = format!("lm-extract-{slug}");

    // Set up cleanup guard — runs on drop regardless of success/failure
    let temp_path = std::env::temp_dir().join(format!("lm-extract-{slug}-{}", std::process::id()));
    let _cleanup = DockerCleanup {
        container_name: container_name.clone(),
        image_tag: image_tag.clone(),
        temp_path: Some(temp_path.clone()),
    };

    // Build the image
    println!("=> Building via Dockerfile (build tool only — no containers at runtime)");
    let build_status = Command::new("docker")
        .args(["build", "-t", &image_tag, "-f", dockerfile, "."])
        .status()?;

    if !build_status.success() {
        bail!("Dockerfile build failed");
    }

    // Create temporary container to extract the binary
    println!("=> Extracting binary from {output}...");
    let create_output = Command::new("docker")
        .args(["create", "--name", &container_name, &image_tag])
        .output()?;

    if !create_output.status.success() {
        let stderr = String::from_utf8_lossy(&create_output.stderr);
        bail!("failed to create container for extraction: {stderr}");
    }

    // Copy binary out of the container
    let cp_status = Command::new("docker")
        .args(["cp", &format!("{container_name}:{output}"), &temp_path.to_string_lossy()])
        .status()?;

    if !cp_status.success() {
        bail!(
            "Binary not found at '{}' inside the Docker image.\n\
             Make sure your Dockerfile copies the compiled binary to this path.",
            output
        );
    }

    // Read the extracted binary
    let bytes = fs::read(&temp_path)?;
    if bytes.is_empty() {
        bail!("Extracted binary is empty (0 bytes) — check your Dockerfile build");
    }

    println!("=> Cleaning up Docker artifacts...");
    // _cleanup Drop handles the actual cleanup

    Ok(bytes)
}
