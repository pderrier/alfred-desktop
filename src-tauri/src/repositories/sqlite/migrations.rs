use anyhow::{ensure, Result};

pub const INITIAL_MIGRATION_SQL: &str = include_str!("../../../migrations/0001_initial.sql");

pub fn apply_migrations() -> Result<()> {
    ensure!(
        INITIAL_MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS sources"),
        "initial migration missing sources table"
    );
    ensure!(
        INITIAL_MIGRATION_SQL.contains("CREATE TABLE IF NOT EXISTS snapshots"),
        "initial migration missing snapshots table"
    );
    Ok(())
}
