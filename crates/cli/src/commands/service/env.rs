use std::collections::HashMap;

use anyhow::{bail, Result};

use common::contract::{EnvVarsResponse, SetEnvVarsRequest, UnsetEnvVarsRequest};

use crate::context::CommandContext;
use crate::config::Config;
use crate::output::{OutputMode, print_data, print_ok};

pub async fn run_list(config: &Config, service_ref: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: EnvVarsResponse = ctx.client
        .get(&format!("/services/{}/env", service_id))
        .await?;

    print_data(ctx.output, &resp, |resp| {
        if resp.vars.is_empty() {
            println!("No environment variables set.");
        } else {
            let mut keys: Vec<_> = resp.vars.keys().collect();
            keys.sort();
            for key in keys {
                println!("{}={}", key, resp.vars[key]);
            }
        }
    });
    Ok(())
}

pub async fn run_set(config: &Config, service_ref: &str, pairs: &[String], output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let mut vars = HashMap::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid format: {:?} — expected KEY=VALUE", pair))?;
        if key.is_empty() {
            bail!("empty key in {:?}", pair);
        }
        vars.insert(key.to_string(), value.to_string());
    }

    let resp: EnvVarsResponse = ctx.client
        .post(
            &format!("/services/{}/env", service_id),
            &SetEnvVarsRequest { vars },
        )
        .await?;

    print_ok(output, &format!(
        "Set {} variable(s). Total: {}\n\nRestart the service to apply: flux restart {}",
        pairs.len(), resp.vars.len(), service_ref
    ));
    Ok(())
}

pub async fn run_unset(config: &Config, service_ref: &str, keys: &[String], output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: EnvVarsResponse = ctx.client
        .post(
            &format!("/services/{}/env/unset", service_id),
            &UnsetEnvVarsRequest { keys: keys.to_vec() },
        )
        .await?;

    print_ok(output, &format!(
        "Removed {} key(s). Remaining: {}\n\nRestart the service to apply: flux restart {}",
        keys.len(), resp.vars.len(), service_ref
    ));
    Ok(())
}
