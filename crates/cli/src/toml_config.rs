use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LiquidMetalConfig {
    pub service: ServiceConfig,
    pub build: Option<BuildConfig>,
}

#[derive(Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub engine: String,
    #[serde(default)]
    pub project_id: Option<String>,
    pub port: Option<u32>,
    /// Metal-only: VM tier — "one" (1 vCPU), "two" (2 vCPU), or "four" (4 vCPU).
    pub tier: Option<String>,
}

#[derive(Deserialize)]
pub struct BuildConfig {
    pub command: Option<String>,
    pub output: Option<String>,
    /// `true` uses `./Dockerfile`, a string uses a custom path (e.g. `"Dockerfile.prod"`).
    pub dockerfile: Option<toml::Value>,
}

impl BuildConfig {
    /// Returns the Dockerfile path if dockerfile builds are configured.
    pub fn dockerfile_path(&self) -> Option<String> {
        match &self.dockerfile {
            Some(toml::Value::Boolean(true)) => Some("Dockerfile".to_string()),
            Some(toml::Value::String(path)) => Some(path.clone()),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct BuildResult {
    pub artifact_path: String,
    pub sha256_hex: String,
}

/// Load and parse liquid-metal.toml from the current directory.
pub fn load_config() -> Result<LiquidMetalConfig> {
    let raw = std::fs::read_to_string("liquid-metal.toml").map_err(|_| {
        anyhow::anyhow!(
            "no liquid-metal.toml found\n\nRun `flux init` to set up this directory as a Liquid Metal service."
        )
    })?;
    Ok(toml::from_str(&raw)?)
}

/// Validate engine, run build command, read artifact, compute SHA256.
pub fn run_build(cfg: &LiquidMetalConfig) -> Result<BuildResult> {
    let engine = cfg.service.engine.to_lowercase();
    match engine.as_str() {
        "liquid" | "metal" => {}
        other => bail!("unknown engine {:?} — expected \"liquid\" or \"metal\"", other),
    }

    let build = cfg.build.as_ref();
    let engine_display = if engine == "metal" { "Metal" } else { "Liquid" };
    println!(
        "=> Building {} (Engine: {})...",
        cfg.service.name, engine_display
    );

    // Dockerfile build path
    if let Some(dockerfile_path) = build.and_then(|b| b.dockerfile_path()) {
        let output = build
            .and_then(|b| b.output.as_deref())
            .ok_or_else(|| anyhow::anyhow!(
                "[build].output is required with dockerfile builds — \
                 it specifies the binary path inside the container (e.g. \"/app/myapp\")"
            ))?;

        if build.and_then(|b| b.command.as_ref()).is_some() {
            bail!("[build].dockerfile and [build].command are mutually exclusive");
        }

        let slug = common::slugify(&cfg.service.name);
        let extracted_path = crate::docker::build_via_dockerfile(&slug, &dockerfile_path, output)?;

        let sha256_hex = {
            use sha2::{Digest, Sha256};
            let mut file = std::fs::File::open(&extracted_path)?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            format!("{:x}", hasher.finalize())
        };

        println!(
            "=> Artifact: {} (SHA256: {}...)",
            output,
            &sha256_hex[..8]
        );

        return Ok(BuildResult {
            artifact_path: extracted_path,
            sha256_hex,
        });
    }

    // Native build path (existing flow)
    let build_cmd = build.and_then(|b| b.command.as_deref());
    let artifact_path = build
        .and_then(|b| b.output.as_deref())
        .unwrap_or(if engine == "metal" { "app" } else { "main.wasm" })
        .to_string();

    if let Some(cmd) = build_cmd {
        println!("=> Running: {}", cmd);
        let status = std::process::Command::new("sh")
            .args(["-c", cmd])
            .status()?;
        if !status.success() {
            bail!("build failed");
        }
    } else if engine == "liquid" {
        bail!(
            "liquid deploys require an explicit [build].command in liquid-metal.toml\n\n\
             Run `flux init --engine liquid` to generate one, or add it manually:\n\n\
             [build]\n\
             command = \"cargo build --target wasm32-wasip1 --release\"\n\
             output  = \"target/wasm32-wasip1/release/your-binary.wasm\""
        );
    } else {
        bail!(
            "metal deploys require an explicit [build].command in liquid-metal.toml\n\n\
             Run `flux init --engine metal` to generate one, or add it manually:\n\n\
             [build]\n\
             command = \"cargo build --target x86_64-unknown-linux-musl --release\"\n\
             output  = \"target/x86_64-unknown-linux-musl/release/your-binary\""
        );
    }

    if !std::path::Path::new(&artifact_path).exists() {
        bail!("build succeeded but artifact not found at: {}", artifact_path);
    }

    let sha256_hex = {
        use sha2::{Digest, Sha256};
        let mut file = std::fs::File::open(&artifact_path)?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher)?;
        format!("{:x}", hasher.finalize())
    };

    println!(
        "=> Artifact: {} (SHA256: {}...)",
        artifact_path,
        &sha256_hex[..8]
    );

    Ok(BuildResult {
        artifact_path,
        sha256_hex,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_path_true() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = true
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.dockerfile_path(), Some("Dockerfile".to_string()));
    }

    #[test]
    fn dockerfile_path_custom() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = "Dockerfile.prod"
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(
            build.dockerfile_path(),
            Some("Dockerfile.prod".to_string())
        );
    }

    #[test]
    fn dockerfile_path_false() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = false
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.dockerfile_path(), None);
    }

    #[test]
    fn dockerfile_path_absent() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            command = "cargo build --release"
            output = "target/release/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.dockerfile_path(), None);
    }

    #[test]
    fn dockerfile_and_command_mutually_exclusive() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = true
            command = "cargo build --release"
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let err = run_build(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "expected mutually exclusive error, got: {err}"
        );
    }

    #[test]
    fn dockerfile_requires_output() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = true
            "#,
        )
        .unwrap();
        let err = run_build(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("[build].output is required"),
            "expected output required error, got: {err}"
        );
    }

    #[test]
    fn native_build_config_unchanged() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            command = "echo hello"
            output = "app"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.command, Some("echo hello".to_string()));
        assert_eq!(build.output, Some("app".to_string()));
        assert_eq!(build.dockerfile_path(), None);
    }

    #[test]
    fn dockerfile_wrong_type_ignored() {
        // An integer value should not match any dockerfile_path() branch
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = 123
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(build.dockerfile_path(), None);
    }

    #[test]
    fn dockerfile_empty_string() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = ""
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        // Empty string still resolves — docker will fail with a clear error at build time
        assert_eq!(build.dockerfile_path(), Some("".to_string()));
    }

    #[test]
    fn no_build_section_parses() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"
            "#,
        )
        .unwrap();
        assert!(cfg.build.is_none());
    }

    #[test]
    fn no_build_section_metal_errors() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"
            "#,
        )
        .unwrap();
        let err = run_build(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("metal deploys require"),
            "expected metal build hint, got: {err}"
        );
    }

    #[test]
    fn no_build_section_liquid_errors() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "liquid"
            "#,
        )
        .unwrap();
        let err = run_build(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("liquid deploys require"),
            "expected liquid build hint, got: {err}"
        );
    }

    #[test]
    fn unknown_engine_rejected() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "docker"

            [build]
            command = "echo hi"
            "#,
        )
        .unwrap();
        let err = run_build(&cfg).unwrap_err();
        assert!(
            err.to_string().contains("unknown engine"),
            "expected unknown engine error, got: {err}"
        );
    }

    #[test]
    fn dockerfile_with_spaces_in_path() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"

            [build]
            dockerfile = "path/to/Docker file.prod"
            output = "/app/myapp"
            "#,
        )
        .unwrap();
        let build = cfg.build.unwrap();
        assert_eq!(
            build.dockerfile_path(),
            Some("path/to/Docker file.prod".to_string())
        );
    }

    #[test]
    fn minimal_service_config() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.service.name, "myapp");
        assert_eq!(cfg.service.engine, "metal");
        assert!(cfg.service.project_id.is_none());
        assert!(cfg.service.port.is_none());
    }

    #[test]
    fn full_service_config() {
        let cfg: LiquidMetalConfig = toml::from_str(
            r#"
            [service]
            name = "myapp"
            engine = "metal"
            project_id = "550e8400-e29b-41d4-a716-446655440000"
            port = 8080

            [build]
            command = "cargo build --release"
            output = "target/release/myapp"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.service.port, Some(8080));
        assert_eq!(
            cfg.service.project_id,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }
}
