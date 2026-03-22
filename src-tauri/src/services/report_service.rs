use anyhow::Result;

#[derive(Debug, Default)]
pub struct ReportService;

impl ReportService {
    pub fn new() -> Self {
        Self
    }

    pub fn generate_placeholder(&self, analysis_id: &str) -> Result<String> {
        // Phase 2 placeholder: wire markdown/html/pdf generation.
        Ok(format!("report-for-{analysis_id}"))
    }
}
