use anyhow::Result;

refinery::embed_migrations!("../../migrations");

pub async fn run(pool: &deadpool_postgres::Pool) -> Result<()> {
    let mut conn = pool.get().await?;
    migrations::runner().run_async(&mut **conn).await?;
    Ok(())
}
