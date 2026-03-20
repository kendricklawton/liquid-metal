use anyhow::Result;

use crate::output::{OutputMode, print_ok};
use crate::toml_config;

pub async fn run(output: OutputMode) -> Result<()> {
    let cfg = toml_config::load_config()?;
    let result = toml_config::run_build(&cfg)?;

    print_ok(output, &format!(
        "\nBuild successful!\n  Artifact: {}\n  SHA256:   {}...\n\nRun `flux deploy` to deploy.",
        result.artifact_path, &result.sha256_hex[..8]
    ));
    Ok(())
}
