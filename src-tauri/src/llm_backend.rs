//! LLM backend abstraction — dispatches between Codex app-server and native
//! OpenAI API client. Both backends share the same prompts, tools, and parsing.
//!
//! Backend selection: runtime setting `llm_backend` ("codex" | "native" | "native-oauth").
//! Default: "codex" (preserves existing behavior + free tier).

use anyhow::Result;
use serde_json::{json, Value};
use std::env;
use std::time::Instant;

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
    if setting == 2 {
        return "native-oauth".to_string();
    }
    // Check string setting
    if let Ok(raw) = crate::runtime_settings::string_direct("llm_backend") {
        let t = raw.trim().to_lowercase();
        if t == "native" || t == "openai" {
            return "native".to_string();
        }
        if t == "native-oauth" {
            return "native-oauth".to_string();
        }
    }
    "codex".to_string()
}

/// Execute a prompt through the active backend.
/// Drop-in replacement for `codex::run_codex_prompt_with_progress`.
pub fn run_prompt(prompt: &str, timeout_ms: u64, on_progress: Option<ProgressFn>) -> Result<Value> {
    let backend = resolve_backend();
    let run_id = format!("llm-{}", crate::helpers::new_run_id());
    let started = Instant::now();
    let artifacts = crate::agentos_artifacts::start_run(
        &run_id,
        "llm.run_prompt",
        json!({ "timeout_ms": timeout_ms }),
    );
    crate::debug_log(&format!("llm_backend: using {backend}"));
    crate::agentos_artifacts::record_decision(
        &artifacts,
        "llm.backend.route",
        "llm.backend.route",
        json!({
            "backend": backend.clone(),
            "decision_type": "script",
        }),
        None,
        Some(json!({ "compilation_candidate": true })),
    );

    let result = match backend.as_str() {
        "native" | "openai" => {
            crate::openai_client::run_prompt(prompt, timeout_ms, on_progress, Some(&artifacts))
        }
        "native-oauth" => crate::openai_client::run_prompt_oauth(
            prompt,
            timeout_ms,
            on_progress,
            Some(&artifacts),
        ),
        _ => {
            // Codex backend — existing path
            crate::codex::ensure_mcp_config();
            crate::codex::run_codex_prompt_with_progress(prompt, timeout_ms, on_progress)
        }
    };

    let latency_ms = started.elapsed().as_millis() as u64;
    let mut outcome_data = json!({
        "latency_ms": latency_ms,
        "backend": backend.clone(),
    });
    if let Some(obj) = outcome_data.as_object_mut() {
        if let Some(runtime_obj) = crate::agentos_artifacts::runtime_data(&artifacts).as_object() {
            for (key, value) in runtime_obj {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
    let status = match &result {
        Ok(_) => "success",
        Err(error) if error.to_string().contains("timeout") => "timeout",
        Err(_) => "failure",
    };
    crate::agentos_artifacts::record_outcome(&artifacts, status, outcome_data);
    result
}

/// Get current backend name for UI display.
pub fn current_backend_name() -> String {
    resolve_backend()
}
