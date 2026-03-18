use anyhow::Result;

use common::contract::{AddDomainRequest, DomainResponse, VerifyDomainResponse};

use crate::context::CommandContext;
use crate::config::Config;
use crate::output::{OutputMode, confirm, print_data, print_ok};

pub async fn run_list(config: &Config, service_ref: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let domains: Vec<DomainResponse> = ctx.client
        .get(&format!("/services/{}/domains", service_id))
        .await?;

    print_data(ctx.output, &domains, |domains| {
        if domains.is_empty() {
            println!("No custom domains configured.");
        } else {
            let rows: Vec<Vec<String>> = domains
                .iter()
                .map(|d| vec![
                    d.domain.clone(),
                    if d.is_verified { "yes" } else { "no" }.to_string(),
                    d.tls_status.clone(),
                ])
                .collect();
            crate::table::print_table(&["DOMAIN", "VERIFIED", "TLS"], &rows, &[], None);
        }
    });
    Ok(())
}

pub async fn run_add(config: &Config, service_ref: &str, domain: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: DomainResponse = ctx.client
        .post(
            &format!("/services/{}/domains", service_id),
            &AddDomainRequest { domain: domain.to_string() },
        )
        .await?;

    print_data(ctx.output, &resp, |resp| {
        println!("Domain \"{}\" added.", resp.domain);
        println!();
        println!("To verify ownership, create one of the following DNS records:");
        println!();
        println!("  Option 1 — CNAME (recommended):");
        println!("    {} → <service-slug>.liquidmetal.dev", resp.domain);
        println!();
        println!("  Option 2 — TXT:");
        println!("    _lm-verify.{} → {}", resp.domain, resp.verification_token);
        println!();
        println!("Then run: flux domains verify {} {}", service_ref, resp.domain);
    });
    Ok(())
}

pub async fn run_verify(config: &Config, service_ref: &str, domain: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: VerifyDomainResponse = ctx.client
        .post_no_body_with_response(&format!(
            "/services/{}/domains/{}/verify",
            service_id, domain
        ))
        .await?;

    print_data(ctx.output, &resp, |resp| {
        if resp.is_verified {
            println!("Domain \"{}\" verified!", resp.domain);
        } else {
            println!("Verification failed: {}", resp.message);
        }
    });
    Ok(())
}

pub async fn run_remove(config: &Config, service_ref: &str, domain: &str, skip_confirm: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    if !skip_confirm {
        confirm(output, &format!("Remove domain \"{}\" from \"{}\"?", domain, service_ref))?;
    }

    ctx.client
        .post_no_body(&format!("/services/{}/domains/{}/remove", service_id, domain))
        .await?;

    print_ok(output, &format!("Domain \"{}\" removed.", domain));
    Ok(())
}
