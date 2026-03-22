use anyhow::Result;

#[derive(Debug, Default)]
pub struct SidecarService;

impl SidecarService {
    pub fn new() -> Self {
        Self
    }

    pub fn healthcheck(&self) -> Result<Vec<(String, String)>> {
        // Phase 2 placeholder: check agent-runner/local-mcp/finary-connector processes.
        Ok(vec![
            ("agent-runner".to_string(), "unknown".to_string()),
            ("portfolio-local-mcp".to_string(), "unknown".to_string()),
            ("finary-connector".to_string(), "unknown".to_string()),
        ])
    }
}
