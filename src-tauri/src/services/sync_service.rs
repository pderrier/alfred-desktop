use anyhow::Result;

use crate::repositories::sqlite::sync_repository::{SyncJobRecord, SyncRepository};

#[derive(Debug, Default)]
pub struct SyncService {
    repo: SyncRepository,
}

impl SyncService {
    pub fn new() -> Self {
        Self {
            repo: SyncRepository::new(),
        }
    }

    pub fn queue_sync(&self, source_id: &str, job_type: &str) -> Result<SyncJobRecord> {
        self.repo.enqueue(source_id, job_type)
    }
}
