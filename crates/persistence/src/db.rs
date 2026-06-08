use anyhow::Result;
use sqlx::postgres::{PgPoolOptions, PgPool as InnerPool};

pub type PgPool = InnerPool;

pub async fn connect(database_url: &str) -> Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect(database_url)
        .await?;
    Ok(pool)
}

/// Apply all migrations under the workspace `migrations/` dir.
///
/// Embedded at build time so the binary is self-contained.
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}
