use std::{fs, path::PathBuf};

use anyhow::Result;

use crate::sqlite_migrations::apply_migrations;

#[derive(Debug, Clone)]
pub struct LocalDbService {
    pub db_path: PathBuf,
}

impl LocalDbService {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn init(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)?;
        }
        apply_migrations()?;
        Ok(())
    }
}
