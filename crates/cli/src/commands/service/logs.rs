use anyhow::Result;

use common::contract::LogLineResponse;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_data};

pub async fn run(config: &Config, service_ref: &str, limit: i32, follow: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let service_id = ctx.client.resolve_service(service_ref).await?;

    let lines: Vec<LogLineResponse> = ctx.client
        .get(&format!("/services/{}/logs?limit={}", service_id, limit))
        .await?;

    if !follow {
        print_data(ctx.output, &lines, |lines| {
            if lines.is_empty() {
                println!("no log lines found");
                return;
            }
            for l in lines {
                print_log_line(l);
            }
        });
        return Ok(());
    }

    // In follow mode: print initial batch, then stream
    for l in &lines {
        print_log_line_mode(l, ctx.output);
    }

    // Track last-seen (timestamp, message) pair for dedup across polls
    let mut last_seen: Option<(String, String)> = lines.last().and_then(|l| {
        l.ts.as_ref().map(|ts| (ts.clone(), l.message.clone()))
    });

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let new_lines: Vec<LogLineResponse> = ctx.client
            .get(&format!("/services/{}/logs?limit=50", service_id))
            .await?;

        let mut printing = last_seen.is_none();
        for l in &new_lines {
            if !printing {
                if let (Some(ts), Some((prev_ts, prev_msg))) = (&l.ts, &last_seen) {
                    if ts == prev_ts && l.message == *prev_msg {
                        printing = true;
                    }
                }
                continue;
            }
            print_log_line_mode(l, ctx.output);
        }

        if let Some(l) = new_lines.last() {
            if let Some(ts) = &l.ts {
                last_seen = Some((ts.clone(), l.message.clone()));
            }
        }
    }
}

fn print_log_line(l: &LogLineResponse) {
    use std::io::IsTerminal;
    let colorize = std::io::stdout().is_terminal();
    let msg = if colorize {
        crate::output::colorize_log(&l.message)
    } else {
        l.message.clone()
    };
    match &l.ts {
        Some(ts) => println!("[{}] {}", ts, msg),
        None => println!("{}", msg),
    }
}

/// In JSON mode, emit NDJSON (one JSON object per line). In human mode, use formatted output.
fn print_log_line_mode(l: &LogLineResponse, mode: OutputMode) {
    match mode {
        OutputMode::Json => println!("{}", serde_json::to_string(l).unwrap()),
        OutputMode::Human => print_log_line(l),
    }
}
