use anyhow::Result;

#[derive(Debug, Default)]
pub struct AuthService;

impl AuthService {
    pub fn new() -> Self {
        Self
    }

    pub fn current_user_id(&self) -> Result<Option<String>> {
        // Phase 2 placeholder: integrate with secure session storage.
        Ok(Some("usr_local_001".to_string()))
    }
}
