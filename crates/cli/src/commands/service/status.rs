use anyhow::Result;

use common::contract::ServiceResponse;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_data};
use crate::table::print_table;

pub async fn run(config: &Config, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let services: Vec<ServiceResponse> = ctx.client.get("/services").await?;

    print_data(ctx.output, &services, |services| {
        if services.is_empty() {
            println!("no services found\n");
            println!("  to deploy your first service:");
            println!("    flux init      — set up a project in the current directory");
            println!("    flux deploy    — build and deploy to get a live URL");
            return;
        }

        let domain = ctx.config.platform_domain();
        let rows: Vec<Vec<String>> = services
            .iter()
            .map(|s| {
                let short_id = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                let url = if s.status == "running" {
                    format!("https://{}.{}", s.slug, domain)
                } else {
                    "--".to_string()
                };
                let age = s.created_at.as_deref()
                    .map(crate::output::relative_time)
                    .unwrap_or_default();
                vec![
                    short_id.to_string(),
                    s.name.clone(),
                    s.engine.clone(),
                    s.status.clone(),
                    url,
                    age,
                ]
            })
            .collect();
        print_table(
            &["ID", "NAME", "ENGINE", "STATUS", "URL", "AGE"],
            &rows,
            &[],
            None,
        );

        // Show failure reason for any failed services
        for s in services {
            if s.status == "failed" {
                if let Some(reason) = &s.failure_reason {
                    let short = reason.lines().next().unwrap_or(reason);
                    println!("\n  {} ({}): {}", s.name, s.slug, short);
                }
            }
        }
    });
    Ok(())
}
