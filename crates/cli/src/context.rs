use anyhow::Result;

use crate::client::ApiClient;
use crate::config::Config;
use crate::output::OutputMode;

pub struct CommandContext<'a> {
    pub config: &'a Config,
    pub client: ApiClient,
    pub output: OutputMode,
}

impl<'a> CommandContext<'a> {
    pub fn new(config: &'a Config, output: OutputMode) -> Result<Self> {
        let token = config.require_token()?;
        let client = ApiClient::new(config.api_url(), Some(token));
        Ok(Self { config, client, output })
    }
}
