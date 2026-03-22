use anyhow::Result;

use crate::repositories::sqlite::source_repository::{SourceRecord, SourceRepository};

#[derive(Debug, Default)]
pub struct SourceService {
    repo: SourceRepository,
}

impl SourceService {
    pub fn new() -> Self {
        Self {
            repo: SourceRepository::new(),
        }
    }

    pub fn list_sources(&self, user_id: &str) -> Result<Vec<SourceRecord>> {
        self.repo.list_sources(user_id)
    }
}
