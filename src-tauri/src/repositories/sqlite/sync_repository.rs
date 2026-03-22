use anyhow::Result;

#[derive(Debug, Clone)]
pub struct SyncJobRecord {
    pub id: String,
    pub source_id: String,
    pub job_type: String,
    pub status: String,
}

#[derive(Debug, Default)]
pub struct SyncRepository;

impl SyncRepository {
    pub fn new() -> Self {
        Self
    }

    pub fn enqueue(&self, source_id: &str, job_type: &str) -> Result<SyncJobRecord> {
        // Phase 2 placeholder: persist queued sync job and return record.
        Ok(SyncJobRecord {
            id: "sync_placeholder".to_string(),
            source_id: source_id.to_string(),
            job_type: job_type.to_string(),
            status: "queued".to_string(),
        })
    }

    pub fn update_status(&self, job_id: &str, status: &str) -> Result<()> {
        // Phase 2 placeholder: update status and timestamps.
        let _ = (job_id, status);
        Ok(())
    }
}
