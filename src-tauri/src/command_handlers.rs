//! Tauri command handlers — thin async wrappers around domain logic.

use anyhow::{anyhow, Result};
use serde_json::json;
use std::process::Command;

use crate::analysis_ops;
use crate::finary;
use crate::report;
use crate::run_state;
use crate::runtime_settings;

// ── Synchronous implementations (shared between Tauri + CLI) ──

pub fn run_analysis_start(options: Option<serde_json::Value>) -> Result<serde_json::Value> {
    analysis_ops::start_analysis(options)
}

pub fn run_retry_global_synthesis(run_id: String) -> Result<serde_json::Value> {
    let safe_run_id = run_id.trim().to_string();
    if safe_run_id.is_empty() {
        return Err(anyhow!("run_id_required"));
    }

    let is_native = crate::llm_backend::current_backend_name() != "codex";
    if is_native {
        // Native backend: use direct synthesis (same as run_synthesis_turn)
        let data_dir = crate::paths::resolve_runtime_state_dir()
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());
        let result = crate::native_mcp_analysis::run_synthesis_turn(&safe_run_id, &data_dir)?;
        return Ok(json!({
            "ok": true,
            "action": "analysis:retry-global-synthesis-local",
            "result": result
        }));
    }

    // Codex backend: generate draft via LLM + persist
    let run_state = run_state::load_run_by_id(&safe_run_id)?;
    let generated_draft = report::generate_draft_via_litellm(&run_state, &safe_run_id)?;
    let result = report::persist_retry_global_synthesis(&safe_run_id, &generated_draft)?;
    Ok(json!({
        "ok": true,
        "action": "analysis:retry-global-synthesis-local",
        "result": result
    }))
}

pub fn run_analysis_status(operation_id: String) -> Result<serde_json::Value> {
    analysis_ops::poll_analysis_status(operation_id)
}

pub fn run_dashboard_snapshot() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "dashboard:snapshot-local",
        "result": run_state::load_dashboard_snapshot(20, 20)?
    }))
}

pub fn run_dashboard_overview() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "dashboard:overview-local",
        "result": run_state::load_dashboard_overview(20, 20)?
    }))
}

pub fn run_dashboard_details() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "dashboard:details-local",
        "result": run_state::load_dashboard_details(20)?
    }))
}

pub fn run_runtime_settings() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "runtime:settings-local",
        "result": {
            "ok": true,
            "settings": runtime_settings::get_payload()?
        }
    }))
}

pub fn run_runtime_settings_update(settings: serde_json::Value) -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "runtime:settings-update-local",
        "result": {
            "ok": true,
            "settings": runtime_settings::patch(&settings)?
        }
    }))
}

pub fn run_runtime_settings_reset() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "runtime:settings-reset-local",
        "result": {
            "ok": true,
            "settings": runtime_settings::reset()?
        }
    }))
}

pub fn run_by_id(run_id: String) -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "run:by-id-local",
        "result": {
            "ok": true,
            "run": run_state::load_run_by_id(&run_id)?
        }
    }))
}

pub fn run_stack_health() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "stack:health-local",
        "result": crate::health::collect_stack_health()
    }))
}

pub fn run_finary_session_status() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-status-local",
        "result": finary::session_status()?
    }))
}

pub fn run_finary_session_connect(payload: Option<serde_json::Value>) -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-connect-local",
        "result": finary::session_connect(payload)?
    }))
}

pub fn run_finary_session_refresh() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-refresh-local",
        "result": finary::session_refresh()?
    }))
}

pub fn run_finary_session_browser_start() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-browser-start-local",
        "result": finary::session_browser_start()?
    }))
}

pub fn run_finary_session_browser_complete() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-browser-complete-local",
        "result": finary::session_browser_complete()?
    }))
}

pub fn run_finary_session_browser_playwright() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-browser-playwright-local",
        "result": finary::session_browser_playwright()?
    }))
}

pub fn run_finary_session_browser_reuse() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:session-browser-reuse-local",
        "result": finary::session_browser_reuse()?
    }))
}

// ── External URL ──

pub(crate) fn validate_external_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("external_url_invalid"));
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(anyhow!("external_url_invalid"));
    }
    Ok(trimmed.to_string())
}

pub fn open_external_url(url: &str) -> Result<serde_json::Value> {
    let safe_url = validate_external_url(url)?;
    #[cfg(target_os = "windows")]
    let status = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg("start").arg("").arg(&safe_url);
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        cmd.status().map_err(|error| anyhow!("external_url_open_failed:{error}"))?
    };
    #[cfg(target_os = "macos")]
    let status = Command::new("open")
        .arg(&safe_url)
        .status()
        .map_err(|error| anyhow!("external_url_open_failed:{error}"))?;
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let status = Command::new("xdg-open")
        .arg(&safe_url)
        .status()
        .map_err(|error| anyhow!("external_url_open_failed:{error}"))?;

    if !status.success() {
        return Err(anyhow!("external_url_open_failed:exit"));
    }
    Ok(json!({
        "ok": true,
        "action": "desktop:open-external-url",
        "result": {
            "ok": true,
            "opened": true,
            "url": safe_url
        }
    }))
}

// ── Invoke dispatch (used by CLI and tests) ──

pub fn invoke_command(command: &str) -> Result<serde_json::Value> {
    match command {
        "analysis:run-start-local" | "analysis_run_start_local" => run_analysis_start(None),
        "analysis:retry-global-synthesis-local" | "retry_global_synthesis_local" => {
            Err(anyhow!("run_id_required"))
        }
        "dashboard:snapshot-local" | "dashboard_snapshot_local" => run_dashboard_snapshot(),
        "dashboard:overview-local" | "dashboard_overview_local" => run_dashboard_overview(),
        "dashboard:details-local" | "dashboard_details_local" => run_dashboard_details(),
        "runtime:settings-local" | "runtime_settings_local" => run_runtime_settings(),
        "runtime:settings-update-local" | "runtime_settings_update_local" => {
            Err(anyhow!("runtime_settings_payload_required"))
        }
        "runtime:settings-reset-local" | "runtime_settings_reset_local" => run_runtime_settings_reset(),
        "stack:health-local" | "stack_health_local" => run_stack_health(),
        "finary:session-status-local" | "finary_session_status_local" => run_finary_session_status(),
        "finary:session-connect-local" | "finary_session_connect_local" => run_finary_session_connect(None),
        "finary:session-refresh-local" | "finary_session_refresh_local" => run_finary_session_refresh(),
        "finary:session-browser-start-local" | "finary_session_browser_start_local" => run_finary_session_browser_start(),
        "finary:session-browser-complete-local" | "finary_session_browser_complete_local" => run_finary_session_browser_complete(),
        "finary:session-browser-playwright-local" | "finary_session_browser_playwright_local" => run_finary_session_browser_playwright(),
        "finary:session-browser-reuse-local" | "finary_session_browser_reuse_local" => run_finary_session_browser_reuse(),
        "codex:ensure-local" | "ensure_codex_local" => run_ensure_codex(),
        "codex:session-status-local" | "codex_session_status_local" => run_codex_session_status(),
        "codex:session-login-local" | "codex_session_login_local" => run_codex_session_login(),
        "codex:session-logout-local" | "codex_session_logout_local" => run_codex_session_logout(),
        other => Err(anyhow!("unknown_invoke_command:{other}")),
    }
}

pub fn run_get_user_preferences() -> Result<serde_json::Value> {
    Ok(runtime_settings::get_user_preferences())
}

pub fn run_save_user_preferences(prefs: serde_json::Value) -> Result<serde_json::Value> {
    runtime_settings::save_user_preferences(&prefs)?;
    Ok(json!({ "ok": true }))
}

pub fn run_account_positions(account: String) -> Result<serde_json::Value> {
    // 1. Try Finary snapshot store
    let snapshot_path = crate::paths::resolve_source_snapshot_store_path();
    if snapshot_path.exists() {
        if let Ok(store) = crate::storage::read_json_file(&snapshot_path) {
            let positions: Vec<_> = store
                .get("latest_by_source")
                .and_then(|v| v.get("finary_local_default"))
                .and_then(|v| v.get("snapshot"))
                .and_then(|v| v.get("positions"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|p| p.get("compte").and_then(|v| v.as_str()).unwrap_or_default() == account)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if !positions.is_empty() {
                return Ok(json!({ "positions": positions, "source": "finary_snapshot" }));
            }
        }
    }

    // 2. Try most recent run state with positions for this account
    let history = run_state::load_run_history(20)?;
    for run_summary in &history {
        let run_id = run_summary.get("run_id").and_then(|v| v.as_str()).unwrap_or_default();
        if run_id.is_empty() { continue; }
        if let Ok(run) = run_state::load_run_by_id(run_id) {
            let positions: Vec<_> = run
                .get("portfolio")
                .and_then(|v| v.get("positions"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|p| p.get("compte").and_then(|v| v.as_str()).unwrap_or_default() == account)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if !positions.is_empty() {
                return Ok(json!({ "positions": positions, "source": "run_history" }));
            }
        }
    }

    Ok(json!({ "positions": [], "source": "none" }))
}

pub fn run_ensure_codex() -> Result<serde_json::Value> {
    crate::codex::ensure_codex_available()
}

pub fn run_codex_session_status() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:session-status-local",
        "result": crate::codex::session_status()?
    }))
}

pub fn run_codex_session_login() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:session-login-local",
        "result": crate::codex::session_login()?
    }))
}

pub fn run_codex_session_logout() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:session-logout-local",
        "result": crate::codex::session_logout()?
    }))
}

