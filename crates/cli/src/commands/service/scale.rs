// Scale command removed — all services are serverless.
// The run_mode concept (serverless vs always-on) no longer exists.

use anyhow::{Result, bail};

use crate::output::OutputMode;
use crate::config::Config;

pub async fn run(_config: &Config, _service_ref: &str, _mode: &str, _skip_confirm: bool, _output: OutputMode) -> Result<()> {
    bail!("The scale command has been removed. All services are serverless and scale to zero automatically.")
}
