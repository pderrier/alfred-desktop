use anyhow::Result;

#[derive(Debug, Clone)]
pub struct RuntimeLlmSession {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Default)]
pub struct TokenService;

impl TokenService {
    pub fn new() -> Self {
        Self
    }

    pub fn get_llm_session(&self) -> Result<RuntimeLlmSession> {
        // Phase 2 placeholder: replace with control-plane llm/session call.
        Ok(RuntimeLlmSession {
            token: "dev-token".to_string(),
            expires_at: "2099-01-01T00:00:00Z".to_string(),
        })
    }
}
