#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub app_name: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            app_name: "alfred".to_string(),
        }
    }
}
