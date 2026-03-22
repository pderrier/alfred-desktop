use anyhow::Result;

#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub user_id: String,
    pub device_id: String,
    pub default_model: String,
}

#[derive(Debug, Default)]
pub struct BootstrapService;

impl BootstrapService {
    pub fn new() -> Self {
        Self
    }

    pub fn fetch_bootstrap(&self) -> Result<BootstrapConfig> {
        // Phase 2 placeholder: replace with control-plane HTTP client.
        Ok(BootstrapConfig {
            user_id: "usr_local_001".to_string(),
            device_id: "dev_local_001".to_string(),
            default_model: "gpt-5-mini".to_string(),
        })
    }
}
