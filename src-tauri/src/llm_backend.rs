//! LLM backend abstraction — dispatches between Codex app-server and native
//! OpenAI API client. Both backends share the same prompts, tools, and parsing.
//!
//! Backend selection: runtime setting `llm_backend` ("codex" | "native").
//! Default: "codex" (preserves existing behavior + free tier).

use anyhow::Result;
use serde_json::Value;
use std::env;

/// Progress callback: `(bytes_received, line_count, latest_label)`.
pub type ProgressFn = Box<dyn Fn(usize, usize, &str) + Send>;

/// Resolve which backend to use. Priority:
/// 1. `ALFRED_LLM_BACKEND` env var
/// 2. `llm_backend` runtime setting
/// 3. Default: "codex"
fn resolve_backend() -> String {
    if let Ok(v) = env::var("ALFRED_LLM_BACKEND") {
        let t = v.trim().to_lowercase();
        if !t.is_empty() {
            return t;
        }
    }
    let setting = crate::runtime_setting_integer_direct("llm_backend_id", 0);
    if setting == 1 {
        return "native".to_string();
    }
    // Check string setting
    if let Ok(raw) = crate::runtime_settings::string_direct("llm_backend") {
        let t = raw.trim().to_lowercase();
        if t == "native" || t == "openai" {
            return "native".to_string();
        }
    }
    "codex".to_string()
}

/// Execute a prompt through the active backend.
/// Drop-in replacement for `codex::run_codex_prompt_with_progress`.
pub fn run_prompt(
    prompt: &str,
    timeout_ms: u64,
    on_progress: Option<ProgressFn>,
) -> Result<Value> {
    let backend = resolve_backend();
    crate::debug_log(&format!("llm_backend: using {backend}"));

    match backend.as_str() {
        "native" | "openai" => {
            crate::openai_client::run_prompt(prompt, timeout_ms, on_progress)
        }
        _ => {
            // Codex backend — existing path
            crate::codex::ensure_mcp_config();
            crate::codex::run_codex_prompt_with_progress(prompt, timeout_ms, on_progress)
        }
    }
}

/// Ensure the selected backend is ready (binary exists / API key valid).
/// Called from the splash screen on startup.
pub fn ensure_backend_available() -> Result<Value> {
    let backend = resolve_backend();
    match backend.as_str() {
        "native" | "openai" => crate::openai_client::validate_api_key(),
        _ => crate::codex::ensure_codex_available(),
    }
}

/// Get current backend name for UI display.
pub fn current_backend_name() -> String {
    resolve_backend()
}
