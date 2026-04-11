// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Alfred Desktop Backend — Tauri application entry point.
//!
//! This file is a thin orchestrator: module declarations, Tauri async command
//! wrappers (required by `tauri::generate_handler![]`), and `fn main()`.
//! All domain logic lives in the extracted modules below.

mod models;

// ── Pre-existing service modules (path-redirected) ──────────────────────────
#[path = "services/local_db_service.rs"]
mod local_db_service;
#[path = "services/local_http.rs"]
mod local_http;
#[path = "services/native_collection.rs"]
mod native_collection;
#[path = "services/native_collection_modes.rs"]
mod native_collection_modes;
#[path = "services/native_collection_dispatch.rs"]
mod native_collection_dispatch;
#[path = "services/native_collection_helpers.rs"]
mod native_collection_helpers;
#[path = "services/native_line_analysis.rs"]
mod native_line_analysis;
// node_bridge_support removed — Playwright/Node replaced by native Rust CDP
#[path = "repositories/sqlite/migrations.rs"]
mod sqlite_migrations;

// ── Extracted domain modules ────────────────────────────────────────────────
mod storage;
mod paths;
mod helpers;
mod runtime_settings;
mod health;
mod run_state;
mod run_state_cache;
mod finary;
mod report;
mod analysis_ops;
mod command_handlers;
mod cli;
mod alfred_api_client;
mod codex;
mod enrichment;
mod llm;
mod llm_prompts;
mod llm_parsing;
mod mcp_server;
mod mcp_progress_relay;
mod run_stats;
mod storage_cleanup;
mod run_index;
mod updater;
mod llm_backend;
mod openai_client;
mod chat_wizard;
#[path = "services/native_mcp_analysis.rs"]
mod native_mcp_analysis;

use std::env;

use anyhow::anyhow;

// ── Global AppHandle for event emission from worker threads ───────────────
static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

pub fn emit_event(event: &str, payload: serde_json::Value) {
    if let Some(handle) = APP_HANDLE.get() {
        use tauri::Emitter;
        let _ = handle.emit(event, payload);
    }
}

// ── Re-exports for service modules that still use `crate::function_name` ─────
pub use helpers::{debug_log, now_epoch_ms, now_iso_string, pick_json_fields};
pub use local_http::request_http_json;
pub use paths::{resolve_runtime_state_dir, resolve_source_snapshot_store_path};
pub use run_state::{
    load_run_by_id as load_run_by_id_direct,
    patch_run_state_with as patch_run_state_direct_with,
    set_native_run_stage,
    update_line_status,
};
pub use runtime_settings::integer_direct as runtime_setting_integer_direct;
pub use storage::write_json_file;

// ── Tauri async command wrappers ────────────────────────────────────────────
// These must live here because `tauri::generate_handler![]` resolves function
// paths at compile time relative to the invoking module.

#[tauri::command]
async fn analysis_run_start_local(options: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_analysis_start(options))
        .await
        .map_err(|e| format!("analysis_run_start_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn retry_global_synthesis_local(run_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_retry_global_synthesis(run_id))
        .await
        .map_err(|e| format!("retry_global_synthesis_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn analysis_stop_local(operation_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || analysis_ops::request_cancellation(&operation_id))
        .await
        .map_err(|e| format!("analysis_stop_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn analysis_run_status_local(operation_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_analysis_status(operation_id))
        .await
        .map_err(|e| format!("analysis_run_status_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn dashboard_snapshot_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_dashboard_snapshot)
        .await
        .map_err(|e| format!("dashboard_snapshot_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn dashboard_overview_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_dashboard_overview)
        .await
        .map_err(|e| format!("dashboard_overview_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn dashboard_details_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_dashboard_details)
        .await
        .map_err(|e| format!("dashboard_details_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn runtime_settings_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_runtime_settings)
        .await
        .map_err(|e| format!("runtime_settings_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn runtime_settings_update_local(settings: serde_json::Value) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_runtime_settings_update(settings))
        .await
        .map_err(|e| format!("runtime_settings_update_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn runtime_settings_reset_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_runtime_settings_reset)
        .await
        .map_err(|e| format!("runtime_settings_reset_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_by_id_local(run_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_by_id(run_id))
        .await
        .map_err(|e| format!("run_by_id_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn stack_health_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_stack_health)
        .await
        .map_err(|e| format!("stack_health_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_status_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_status)
        .await
        .map_err(|e| format!("finary_session_status_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_connect_local(payload: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_finary_session_connect(payload))
        .await
        .map_err(|e| format!("finary_session_connect_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_refresh_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_refresh)
        .await
        .map_err(|e| format!("finary_session_refresh_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_browser_start_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_browser_start)
        .await
        .map_err(|e| format!("finary_session_browser_start_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_browser_complete_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_browser_complete)
        .await
        .map_err(|e| format!("finary_session_browser_complete_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_browser_playwright_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_browser_playwright)
        .await
        .map_err(|e| format!("finary_session_browser_playwright_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn finary_session_browser_reuse_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_finary_session_browser_reuse)
        .await
        .map_err(|e| format!("finary_session_browser_reuse_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn desktop_open_external_url(url: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::open_external_url(&url))
        .await
        .map_err(|e| format!("desktop_open_external_url_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_user_preferences_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_get_user_preferences)
        .await
        .map_err(|e| format!("get_user_preferences_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_user_preferences_local(prefs: serde_json::Value) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_save_user_preferences(prefs))
        .await
        .map_err(|e| format!("save_user_preferences_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn account_positions_local(account: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_account_positions(account))
        .await
        .map_err(|e| format!("account_positions_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn storage_usage_local() -> Result<serde_json::Value, String> {
    Ok(storage_cleanup::get_storage_usage())
}

#[tauri::command]
async fn storage_prune_local(keep: Option<usize>) -> Result<serde_json::Value, String> {
    let keep = keep.unwrap_or(10);
    tauri::async_runtime::spawn_blocking(move || storage_cleanup::prune_old_runs(keep))
        .await
        .map_err(|e| format!("storage_prune_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn storage_clear_log_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(storage_cleanup::clear_debug_log)
        .await
        .map_err(|e| format!("storage_clear_log_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn ensure_codex_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_ensure_codex)
        .await
        .map_err(|e| format!("ensure_codex_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn codex_session_status_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_codex_session_status)
        .await
        .map_err(|e| format!("codex_session_status_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn codex_session_login_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_codex_session_login)
        .await
        .map_err(|e| format!("codex_session_login_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn codex_session_logout_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_codex_session_logout)
        .await
        .map_err(|e| format!("codex_session_logout_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

// ── Application bootstrap ───────────────────────────────────────────────────

/// Load env vars from `.alfred.local.env` (repo root) if it exists.
/// Only sets vars that are not already set in the environment.
fn load_local_env() {
    // Try repo root (two levels up from src-tauri, or APP_ROOT)
    let candidates = [
        std::env::var("APP_ROOT").ok().map(std::path::PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
        std::env::current_dir().ok(),
        // Common dev layout: apps/desktop-ui/src-tauri -> repo root
        std::env::current_dir()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
    ];
    for candidate in candidates.into_iter().flatten() {
        let env_path = candidate.join(".alfred.local.env");
        if env_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&env_path) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        continue;
                    }
                    if let Some((key, value)) = trimmed.split_once('=') {
                        let key = key.trim();
                        let value = value.trim();
                        if !key.is_empty() && std::env::var(key).is_err() {
                            std::env::set_var(key, value);
                        }
                    }
                }
                helpers::debug_log(&format!("loaded env from {}", env_path.display()));
            }
            return;
        }
    }
}

// ── API auth check ──────────────────────────────────────────────────────

#[tauri::command]
async fn check_api_auth_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| Ok(health::check_api_auth()))
        .await
        .map_err(|e| format!("check_api_auth_failed:join:{e}"))?
}

// ── Finary sync commands ─────────────────────────────────────────────────

#[tauri::command]
async fn finary_sync_snapshot_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(native_collection::fetch_finary_snapshot_standalone)
        .await
        .map_err(|e| format!("finary_sync_snapshot_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

// ── LLM backend commands ─────────────────────────────────────────────────

#[tauri::command]
async fn check_openai_api_key_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(openai_client::validate_api_key)
        .await
        .map_err(|e| format!("check_openai_api_key_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

// ── Updater commands ──────────────────────────────────────────────────────

#[tauri::command]
async fn check_for_update_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(updater::check_for_update)
        .await
        .map_err(|e| format!("check_for_update_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn download_update_local(url: String, sha256: Option<String>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        updater::download_update(&url, sha256.as_deref())
    })
    .await
    .map_err(|e| format!("download_update_local_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn install_update_local(path: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || updater::install_update(&path))
        .await
        .map_err(|e| format!("install_update_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn js_log_local(message: String) {
    helpers::debug_log(&format!("[js] {message}"));
}

#[tauri::command]
async fn chat_wizard_send_local(
    context: String,
    history: Vec<serde_json::Value>,
    user_message: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        chat_wizard::chat_wizard_send_impl(context, history, user_message)
    })
    .await
    .map_err(|e| format!("chat_wizard_send_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_line_memory_local(
    ticker: String,
    key_reasoning: Option<String>,
    user_note: Option<String>,
    news_themes: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        native_mcp_analysis::update_line_memory_fields(
            &ticker,
            key_reasoning.as_deref(),
            user_note.as_deref(),
            news_themes,
        )?;
        Ok(serde_json::json!({"ok": true}))
    })
    .await
    .map_err(|e| format!("update_line_memory_local_failed:join:{e}"))?
    .map_err(|e: anyhow::Error| e.to_string())
}

fn run_tauri_app() -> anyhow::Result<()> {
    load_local_env();

    tauri::Builder::default()
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());

            // Ensure critical data directories exist on fresh install.
            let data_dir = paths::default_data_dir();
            for sub in &["finary-session", "reports", "report-history", "runtime-state"] {
                let _ = std::fs::create_dir_all(data_dir.join(sub));
            }

            // Ensure the window is visible and focused on startup
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
            // Cleanup orphaned runs — uses the in-memory index so it's instant.
            run_state::cleanup_orphaned_runs();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            analysis_run_start_local,
            analysis_stop_local,
            retry_global_synthesis_local,
            analysis_run_status_local,
            dashboard_snapshot_local,
            dashboard_overview_local,
            dashboard_details_local,
            runtime_settings_local,
            runtime_settings_update_local,
            runtime_settings_reset_local,
            run_by_id_local,
            stack_health_local,
            finary_session_status_local,
            finary_session_connect_local,
            finary_session_refresh_local,
            finary_session_browser_start_local,
            finary_session_browser_complete_local,
            finary_session_browser_playwright_local,
            finary_session_browser_reuse_local,
            desktop_open_external_url,
            ensure_codex_local,
            codex_session_status_local,
            codex_session_login_local,
            codex_session_logout_local,
            account_positions_local,
            get_user_preferences_local,
            save_user_preferences_local,
            storage_usage_local,
            storage_prune_local,
            storage_clear_log_local,
            check_api_auth_local,
            finary_sync_snapshot_local,
            check_openai_api_key_local,
            check_for_update_local,
            download_update_local,
            install_update_local,
            js_log_local,
            chat_wizard_send_local,
            update_line_memory_local
        ])
        .run(tauri::generate_context!())
        .map_err(|e| anyhow!("tauri_app_launch_failed:{e}"))?;
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // MCP server mode — run as stdio JSON-RPC server for Codex tool calls
    if args.iter().any(|a| a == "--mcp-server") {
        let data_dir = args
            .windows(2)
            .find(|w| w[0] == "--data-dir")
            .map(|w| std::path::PathBuf::from(&w[1]))
            .unwrap_or_else(|| {
                paths::resolve_runtime_state_dir()
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_path_buf()
            });
        let tool_filter: Option<Vec<String>> = args
            .windows(2)
            .find(|w| w[0] == "--tools")
            .map(|w| w[1].split(',').map(|s| s.trim().to_string()).collect());
        let server_result = if let Some(filter) = tool_filter {
            mcp_server::run_stdio_server_filtered(data_dir, filter)
        } else {
            mcp_server::run_stdio_server(data_dir)
        };
        if let Err(error) = server_result {
            eprintln!("mcp_server_failed:{error}");
            std::process::exit(1);
        }
        return;
    }

    let result = if cli::should_run_cli(&args) {
        cli::run(&args)
    } else {
        run_tauri_app()
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
