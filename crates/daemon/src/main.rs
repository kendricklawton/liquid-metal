use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let _tracer_provider = common::config::init_tracing("daemon");
    daemon::run::run().await
}
