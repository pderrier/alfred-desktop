use anyhow::Result;

#[derive(Debug, Clone)]
pub struct SourceRecord {
    pub id: String,
    pub user_id: String,
    pub source_type: String,
    pub status: String,
}

#[derive(Debug, Default)]
pub struct SourceRepository;

impl SourceRepository {
    pub fn new() -> Self {
        Self
    }

    pub fn list_sources(&self, _user_id: &str) -> Result<Vec<SourceRecord>> {
        // Phase 2 placeholder: implement SQLite query against `sources` table.
        Ok(Vec::new())
    }

    pub fn upsert_source(&self, source: SourceRecord) -> Result<SourceRecord> {
        // Phase 2 placeholder: implement upsert behavior.
        Ok(source)
    }
}
