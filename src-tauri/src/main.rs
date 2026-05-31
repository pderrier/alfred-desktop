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
#[path = "services/native_collection_dispatch.rs"]
mod native_collection_dispatch;
#[path = "services/native_collection_helpers.rs"]
mod native_collection_helpers;
#[path = "services/native_collection_modes.rs"]
mod native_collection_modes;
// P0-58 (2026-05-23) — re-analysis predicate (date / cold / drift / news).
#[path = "services/force_reanalyse.rs"]
mod force_reanalyse;
#[path = "services/native_line_analysis.rs"]
mod native_line_analysis;
// P2-24 (2026-05-23) — cross-portfolio signal accuracy aggregator.
#[path = "services/signal_accuracy.rs"]
mod signal_accuracy;
// node_bridge_support removed — Playwright/Node replaced by native Rust CDP
#[path = "repositories/sqlite/migrations.rs"]
mod sqlite_migrations;

// ── Extracted domain modules ────────────────────────────────────────────────
// P0-16 (2026-05-23) — `admin_config` deleted, admin visibility now
// uses a server probe (`admin_check_local`) backed by `/admin/check`.
mod agentos_artifacts;
mod alfred_api_client;
mod analysis_ops;
mod chat_wizard;
mod cli;
mod codex;
mod command_handlers;
mod enrichment;
mod finary;
mod health;
mod helpers;
mod llm;
mod llm_backend;
mod llm_parsing;
mod llm_prompts;
mod mcp_progress_relay;
mod mcp_server;
#[path = "services/macro_briefing.rs"]
mod macro_briefing;
#[path = "services/native_mcp_analysis.rs"]
mod native_mcp_analysis;
#[path = "services/llm_post_processing.rs"]
mod llm_post_processing;
#[path = "services/run_deletion.rs"]
mod run_deletion;
mod openai_client;
mod paths;
mod report;
mod run_index;
mod run_narrator;
mod run_state;
mod run_state_cache;
mod run_stats;
mod runtime_settings;
mod storage;
mod storage_cleanup;
mod updater;

use std::env;

use anyhow::anyhow;

// ── Global AppHandle for event emission from worker threads ───────────────
static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

pub fn emit_event(event: &str, payload: serde_json::Value) {
    #[cfg(test)]
    {
        // Mirror every emitted event into a test-only buffer so unit tests
        // can assert on the wire shape without a Tauri runtime. The buffer
        // is shared across parallel tests — callers must scope assertions
        // by a per-test field (run_id, ticker, …) when filtering. See
        // `feedback_emit_event_test_capture`.
        if let Ok(mut buf) = test_event_capture_buffer().lock() {
            buf.push((event.to_string(), payload.clone()));
        }
    }
    if let Some(handle) = APP_HANDLE.get() {
        use tauri::Emitter;
        let _ = handle.emit(event, payload);
    }
}

// ── Test-only event capture buffer ──────────────────────────────────────
//
// In unit tests there is no Tauri AppHandle, so `emit_event` would normally
// be a silent no-op. To let tests assert on events the production code
// emits (e.g. P0-2 narration-status, P0-7 ticker-collection-event), we
// mirror every emit into this buffer when compiled with `cfg(test)`. Tests
// reset it with `test_event_capture_reset()` and read it with
// `test_event_capture_drain()` (or the scoped `test_event_capture_drain_where`
// when running alongside parallel tests on the same buffer).
#[cfg(test)]
fn test_event_capture_buffer() -> &'static std::sync::Mutex<Vec<(String, serde_json::Value)>> {
    static BUFFER: std::sync::OnceLock<std::sync::Mutex<Vec<(String, serde_json::Value)>>> =
        std::sync::OnceLock::new();
    BUFFER.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg(test)]
pub fn test_event_capture_reset() {
    if let Ok(mut buf) = test_event_capture_buffer().lock() {
        buf.clear();
    }
}

/// Drain every captured event since the last drain. Tests should call this
/// at the start of a scenario to clear residual events from prior tests,
/// then assert on the returned vec. Filtering by a scoping field (ticker,
/// run_id) is REQUIRED — the buffer is global.
///
/// Prefer `test_event_capture_drain_where` in tests that run alongside other
/// tests touching the same buffer — an unscoped drain destroys events that
/// belong to parallel tests.
#[cfg(test)]
pub fn test_event_capture_drain() -> Vec<(String, serde_json::Value)> {
    test_event_capture_buffer()
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

/// Drain ONLY the events matching `predicate`, leaving the others in the
/// buffer for other parallel tests. This is the safe primitive for any
/// test that runs alongside other emit_event-aware tests: an unscoped
/// `test_event_capture_drain` race-deletes events between tests on the
/// same shared buffer.
#[cfg(test)]
pub fn test_event_capture_drain_where<P>(predicate: P) -> Vec<(String, serde_json::Value)>
where
    P: Fn(&str, &serde_json::Value) -> bool,
{
    let Ok(mut guard) = test_event_capture_buffer().lock() else {
        return Vec::new();
    };
    let mut matched = Vec::new();
    let mut retained = Vec::with_capacity(guard.len());
    for (name, payload) in guard.drain(..) {
        if predicate(&name, &payload) {
            matched.push((name, payload));
        } else {
            retained.push((name, payload));
        }
    }
    *guard = retained;
    matched
}

// ── Re-exports for service modules that still use `crate::function_name` ─────
pub use helpers::{debug_log, now_epoch_ms, now_iso_string, pick_json_fields};
pub use local_http::request_http_json;
pub use paths::{resolve_runtime_state_dir, resolve_source_snapshot_store_path};
pub use run_state::{
    load_run_by_id as load_run_by_id_direct, patch_run_state_with as patch_run_state_direct_with,
    set_native_run_stage, update_line_status,
};
pub use runtime_settings::integer_direct as runtime_setting_integer_direct;
pub use storage::write_json_file;

// ── Tauri async command wrappers ────────────────────────────────────────────
// These must live here because `tauri::generate_handler![]` resolves function
// paths at compile time relative to the invoking module.

#[tauri::command]
async fn analysis_run_start_local(
    options: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_analysis_start(options))
        .await
        .map_err(|e| format!("analysis_run_start_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn retry_global_synthesis_local(run_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_retry_global_synthesis(run_id)
    })
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
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_analysis_status(operation_id)
    })
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
async fn runtime_settings_update_local(
    settings: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_runtime_settings_update(settings)
    })
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
async fn finary_session_connect_local(
    payload: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_finary_session_connect(payload)
    })
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
async fn save_user_preferences_local(
    prefs: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_save_user_preferences(prefs))
        .await
        .map_err(|e| format!("save_user_preferences_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn watchlist_confirm_local(
    run_id: String,
    account: String,
    confirmed_items: serde_json::Value,
    added_tickers: serde_json::Value,
    feedback: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_watchlist_confirm(
            run_id,
            account,
            confirmed_items,
            added_tickers,
            feedback,
        )
    })
    .await
    .map_err(|e| format!("watchlist_confirm_failed:join:{e}"))?
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
    command_handlers::run_storage_usage().map_err(|e| e.to_string())
}

#[tauri::command]
async fn storage_prune_local(keep: Option<usize>) -> Result<serde_json::Value, String> {
    let keep = keep.unwrap_or(10);
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_storage_prune(keep))
        .await
        .map_err(|e| format!("storage_prune_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn storage_clear_log_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_storage_clear_log)
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

#[tauri::command]
async fn probe_codex_quota_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_probe_codex_quota)
        .await
        .map_err(|e| format!("probe_codex_quota_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn codex_auth_mode_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_codex_auth_mode)
        .await
        .map_err(|e| format!("codex_auth_mode_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn swap_codex_to_apikey_local(api_key: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_swap_codex_to_apikey(api_key))
        .await
        .map_err(|e| format!("swap_codex_to_apikey_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn swap_codex_to_oauth_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_swap_codex_to_oauth)
        .await
        .map_err(|e| format!("swap_codex_to_oauth_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn codex_has_oauth_backup_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_codex_has_oauth_backup)
        .await
        .map_err(|e| format!("codex_has_oauth_backup_local_failed:join:{e}"))?
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

#[tauri::command]
async fn finary_invalidate_snapshot_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| -> Result<serde_json::Value, anyhow::Error> {
        native_collection::invalidate_finary_snapshot_cache()?;
        Ok(serde_json::json!({ "ok": true }))
    })
        .await
        .map_err(|e| format!("finary_invalidate_snapshot_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

// ── CSV import preview ──────────────────────────────────────────────────

#[tauri::command]
async fn preview_csv_import_local(
    csv_text: String,
    account: String,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        native_collection::preview_csv_import(&csv_text, &account)
    })
    .await
    .map_err(|e| format!("preview_csv_import_local_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

/// P0-77b — validate an ISIN client-side via the strict ISO-6166 + Luhn
/// helper. Used by the CSV confirm modal to live-validate the manual ISIN
/// input as the user types.
#[tauri::command]
fn validate_isin_local(isin: String) -> bool {
    native_collection_helpers::validate_isin(&isin)
}

/// P0-77b — apply per-line corrections from the CSV confirm modal and
/// return the snapshot that should be passed to `analysis_run_start_local`
/// as `uploaded_snapshot`.
///
/// Corrections are a sparse list keyed by `position_index` from the
/// preview's `positions_needing_review`. Recognised actions:
///   - `accept_suggestion` — set the position's ticker+ISIN from the
///     historical_suggestion fields (modal validates suggestion exists).
///   - `manual_isin`       — set `isin` from the user input; ticker stays.
///   - `cash_equivalent`   — mark the position as `category: cash_equivalent`
///     so the enrichment pipeline skips market lookups for it.
///   - `skip`              — leave ticker/ISIN as-is; the existing P0-55
///     guard will skip enrichment on these rows.
#[tauri::command]
async fn csv_import_apply_corrections_local(
    csv_text: String,
    account: String,
    corrections: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<serde_json::Value, anyhow::Error> {
        let preview = native_collection::preview_csv_import(&csv_text, &account)?;
        let snapshot = native_collection::apply_csv_corrections(preview, &corrections)?;
        Ok(snapshot)
    })
    .await
    .map_err(|e| format!("csv_import_apply_corrections_local_failed:join:{e}"))?
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
async fn download_update_local(
    url: String,
    sha256: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || updater::download_update(&url, sha256.as_deref()))
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
    memory_narrative: Option<String>,
    user_note: Option<String>,
    news_themes: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        native_mcp_analysis::update_line_memory_fields(
            &ticker,
            memory_narrative.as_deref(),
            user_note.as_deref(),
            news_themes,
        )?;
        Ok(serde_json::json!({"ok": true}))
    })
    .await
    .map_err(|e| format!("update_line_memory_local_failed:join:{e}"))?
    .map_err(|e: anyhow::Error| e.to_string())
}

/// Bug B follow-up — one-shot maintenance: flag tickers in line-memory.json
/// whose price_tracking history is entirely 0 (signal that the price provider
/// was unreachable when those signals were recorded). The flag prevents
/// downstream prompt renderers from printing `prix: 0.00€` as if it were a
/// real price. Safe to invoke from devtools; idempotent — already-flagged
/// entries are left untouched, fresh non-zero history clears the flag on the
/// next sync.
#[tauri::command]
async fn repair_line_memory_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        native_mcp_analysis::repair_line_memory_zero_prices().map(|outcome| {
            serde_json::json!({
                "ok": true,
                "flagged_tickers": outcome.flagged,
                "cleared_tickers": outcome.cleared,
            })
        })
    })
    .await
    .map_err(|e| format!("repair_line_memory_local_failed:join:{e}"))?
    .map_err(|e: anyhow::Error| e.to_string())
}

#[tauri::command]
async fn get_stale_positions_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_get_stale_positions)
        .await
        .map_err(|e| format!("get_stale_positions_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_signal_scorecard_local(ticker: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_get_signal_scorecard(ticker))
        .await
        .map_err(|e| format!("get_signal_scorecard_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_run_diff_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_get_run_diff)
        .await
        .map_err(|e| format!("get_run_diff_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_run_local(run_id: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_delete_run(run_id))
        .await
        .map_err(|e| format!("delete_run_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_alfred_state_local(state: serde_json::Value) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_save_alfred_state(state))
        .await
        .map_err(|e| format!("save_alfred_state_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn load_alfred_state_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_load_alfred_state)
        .await
        .map_err(|e| format!("load_alfred_state_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn export_report_markdown_local(
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_export_report_markdown(payload)
    })
    .await
    .map_err(|e| format!("export_report_markdown_local_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

/// `get_admin_usage_local` — fetch `/admin/usage`. v0.4.0 P0-14.
///
/// Gated client-side by the `admin_check_local` cold-start probe
/// (P0-16, 2026-05-23) ; the desktop never invokes this for non-admin
/// users. The server is the source of truth (403 on miss) ; this
/// command just proxies the response envelope.
#[tauri::command]
async fn get_admin_usage_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_get_admin_usage)
        .await
        .map_err(|e| format!("get_admin_usage_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `get_admin_vps_stats_local` — fetch `/admin/vps-stats`. v0.4.0 P0-14.
#[tauri::command]
async fn get_admin_vps_stats_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_get_admin_vps_stats)
        .await
        .map_err(|e| format!("get_admin_vps_stats_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `current_user_hash_local` — return the FNV-1a hash of the local OpenAI
/// JWT. v0.4.0 P0-14. Diagnostic only since P0-16 (2026-05-23) — the
/// admin-tab visibility now uses a server probe (`admin_check_local`)
/// instead of comparing this hash against a baked-in whitelist. Kept
/// for debugging (Pierre may want to print his own hash when adding it
/// to `ALFRED_ADMIN_HASHES` server-side). Never returns the raw JWT.
#[tauri::command]
async fn current_user_hash_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_current_user_hash)
        .await
        .map_err(|e| format!("current_user_hash_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `compute_signal_accuracy_local` — P2-24 (2026-05-23) home Section 8
/// "Rétro-précision Alfred". Aggregates pre-computed signal accuracy
/// across line-memory and returns `{total_signals, correct,
/// incorrect, accuracy_pct, best_pick, worst_pick}`. The JS renderer
/// gates the section on `total_signals >= 5`.
#[tauri::command]
async fn compute_signal_accuracy_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_compute_signal_accuracy)
        .await
        .map_err(|e| format!("compute_signal_accuracy_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `macro_briefing_local` — P2-70 (2026-05-24) home macro context tile.
/// Returns the silent-degrade envelope `{ok, macro: {us_10y_yield, vix,
/// eur_usd, brent_usd}, cache_hit, source, as_of}` from `/api/macro`.
/// The JS tile hides itself when `macro` is null or every indicator is
/// null. Thin wrapper — see `command_handlers::run_macro_briefing` for
/// the rationale.
#[tauri::command]
async fn macro_briefing_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_macro_briefing)
        .await
        .map_err(|e| format!("macro_briefing_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `runs_count_last_7d_local` — P0-20 (2026-05-23) home tier+quota
/// header strip. Returns the number of runs in the rolling 7-day
/// window plus the desktop-side limit. Read-only against the in-memory
/// run-index, no network round-trip. v0.4.7 P3-31: this is the FALLBACK
/// source for the home strip — see `quota_status_local`.
#[tauri::command]
async fn runs_count_last_7d_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_runs_count_last_7d)
        .await
        .map_err(|e| format!("runs_count_last_7d_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `quota_status_local` — P3-31 (v0.4.7) authoritative rolling-7d quota
/// for the home strip. Proxies `GET /quota/status` (read-only, never
/// consumes quota). Returns `{count, limit, period, reset_at}` from the
/// server ZSET — the same count the enforcement gate sees, killing the
/// ±1 drift of the local `runs_count_last_7d_local`. The JS layer uses
/// this as PRIMARY and falls back to the local count on network failure.
#[tauri::command]
async fn quota_status_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_quota_status)
        .await
        .map_err(|e| format!("quota_status_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `admin_check_local` — server-driven admin tab visibility probe.
/// v0.4.1 P0-16 (2026-05-23) — replaces the baked-in
/// `ADMIN_HASHES_WHITELIST` constant. The desktop calls this at
/// cold-start to decide whether to render the Admin tab in the gear
/// panel. The server `/admin/check` endpoint returns 204 if the
/// current user's hash is in `ALFRED_ADMIN_HASHES`, 403 otherwise.
/// Returning `{is_admin: bool}` keeps the JS bridge shape stable so
/// existing callers (admin-panel.js, app.js) only see the value flip.
///
/// Adding an admin no longer requires a desktop rebuild — Pierre just
/// updates the server env var and restarts the container.
#[tauri::command]
async fn admin_check_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_admin_check)
        .await
        .map_err(|e| format!("admin_check_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

// ── License flow (v0.4.0 P0-15) ──────────────────────────────────────

/// `license_activate_local` — activate a Lemon Squeezy license key via
/// `POST /license/activate`. v0.4.0 P0-15.
///
/// Called by the upgrade-view.js module from two paths:
///   1. LS overlay success callback (primary) — `LemonSqueezy.Setup({...})`
///      eventHandler fires with the new license_key.
///   2. Deep-link fallback (`alfred://license-activated?key=...`) — the
///      Tauri deep-link listener forwards the key to JS via event, JS
///      calls this command.
///
/// `instance_name` is optional — when null the Rust side derives one
/// from the hostname so the LS dashboard shows "Activated from <host>".
#[tauri::command]
async fn license_activate_local(
    license_key: String,
    instance_name: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_license_activate(license_key, instance_name)
    })
    .await
    .map_err(|e| format!("license_activate_local_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

/// `license_validate_local` — revalidate an already-activated license
/// key. v0.4.0 P0-15. Wired today as a thin proxy; full integration
/// with cold-start cache refresh is P1-6.
#[tauri::command]
async fn license_validate_local(license_key: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        command_handlers::run_license_validate(license_key)
    })
    .await
    .map_err(|e| format!("license_validate_local_failed:join:{e}"))?
    .map_err(|e| e.to_string())
}

/// `license_status_local` — read the cached tier + pending-notice for
/// the current user. v0.4.0 P0-15. No LS round-trip.
#[tauri::command]
async fn license_status_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_license_status)
        .await
        .map_err(|e| format!("license_status_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `register_device_local` — MON-A. Register a server-issued device
/// identity on first run (no-op if one exists). Fail-soft. Called once at
/// startup by the JS bootstrap.
#[tauri::command]
async fn register_device_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_register_device)
        .await
        .map_err(|e| format!("register_device_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `redeem_code_local` — MON-C. Redeem an activation (comp) code via
/// `POST /redeem`. Returns `{tier, expires_at}` on success; structured
/// error codes (`alfred_redeem_invalid`, `alfred_redeem_already_used`)
/// propagate to the JS layer for state routing.
#[tauri::command]
async fn redeem_code_local(code: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || command_handlers::run_redeem_code(code))
        .await
        .map_err(|e| format!("redeem_code_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// `license_checkout_url_local` — return the Lemon Squeezy checkout URL
/// the upgrade-view overlay opens. v0.4.0 P0-15.
///
/// Returns `{url: string|null, configured: bool}`. When `configured` is
/// false (public-repo default — no `ALFRED_LS_CHECKOUT_URL` compile-time
/// or runtime env), the JS layer surfaces a clean "temporarily
/// unavailable" banner instead of opening a bogus checkout.
#[tauri::command]
async fn license_checkout_url_local() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(command_handlers::run_license_checkout_url)
        .await
        .map_err(|e| format!("license_checkout_url_local_failed:join:{e}"))?
        .map_err(|e| e.to_string())
}

/// Parse a `alfred://license-activated?key=...` deep-link URL and return
/// the extracted license key, when present.
///
/// Defensive parser — accepts trailing whitespace, alternate query
/// orderings, and missing key (returns None). Centralised so the deep-link
/// dispatch path is unit-testable without spinning up a Tauri runtime.
///
/// Why hand-rolled instead of `url` crate? Adds zero new dependencies,
/// and the URL shape is fixed by LS's `redirect_url` config — a tiny
/// substring scan is sufficient and easier to audit.
fn parse_license_activated_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let prefix = "alfred://license-activated";
    if !trimmed.starts_with(prefix) {
        return None;
    }
    let rest = &trimmed[prefix.len()..];
    let query = rest.strip_prefix('?').unwrap_or(rest);
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next()?.trim();
        let v = kv.next()?.trim();
        if k == "key" && !v.is_empty() {
            return Some(url_decode(v));
        }
    }
    None
}

/// Redact the `key=...` segment of a license-activated deep-link URL for
/// safe logging. Keeps the first 4 chars of the key as a debugging hint
/// (so we can correlate a log line with a specific LS license without
/// exposing the full key in the local debug.log). Non-license URLs are
/// passed through unchanged.
///
/// Local-file impact only — debug.log is never transmitted — but per the
/// stated security property "license key never logged in cleartext", we
/// redact at the source.
fn redact_license_key_in_url(raw: &str) -> String {
    let trimmed = raw.trim_start();
    let prefix = "alfred://license-activated";
    if !trimmed.starts_with(prefix) {
        return raw.to_string();
    }
    let rest = &trimmed[prefix.len()..];
    let query = rest.strip_prefix('?').unwrap_or(rest);
    let mut redacted_pairs: Vec<String> = Vec::new();
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("").trim();
        let v = kv.next().map(|s| s.trim()).unwrap_or("");
        if k == "key" && !v.is_empty() {
            let head: String = v.chars().take(4).collect();
            redacted_pairs.push(format!("key={head}<redacted>"));
        } else if !k.is_empty() {
            redacted_pairs.push(format!("{k}={v}"));
        }
    }
    if redacted_pairs.is_empty() {
        format!("{prefix}")
    } else {
        format!("{prefix}?{}", redacted_pairs.join("&"))
    }
}

/// Minimal URL percent-decode for the license-key extraction path. LS
/// license keys are conventionally alphanumeric-with-dashes — full
/// percent-encoding support isn't needed, but `%XX` sequences appear in
/// some edge cases (legacy LS sandbox keys) so handle them defensively.
///
/// Pure helper kept here (not `helpers.rs`) because it's only used by
/// the deep-link path and over-exposing it would invite reuse in
/// contexts that need the full URL crate semantics.
fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(a), Some(b)) = (h1, h2) {
                let mut hex = String::with_capacity(2);
                hex.push(a);
                hex.push(b);
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    out.push(byte as char);
                    continue;
                }
            }
            // Malformed — keep the literal `%` and any trailing chars
            // we already consumed.
            out.push('%');
            if let Some(a) = h1 { out.push(a); }
            if let Some(b) = h2 { out.push(b); }
        } else if c == '+' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn run_tauri_app() -> anyhow::Result<()> {
    load_local_env();

    tauri::Builder::default()
        // Deep-link plugin (v0.4.0 P0-15). Registers the `alfred://` URI
        // scheme on Win/macOS/Linux per the `plugins.deep-link.schemes`
        // entries in tauri.conf.json. The setup handler below subscribes
        // to the plugin's `on_open_url` event and forwards
        // `alfred://license-activated?key=...` payloads to the JS layer
        // via the Tauri `alfred://license-activated` event — same shape
        // the LS overlay success-callback uses, so the JS handler is one
        // pipeline.
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            let _ = APP_HANDLE.set(app.handle().clone());

            // Ensure critical data directories exist on fresh install.
            let data_dir = paths::default_data_dir();
            for sub in &[
                "finary-session",
                "reports",
                "report-history",
                "runtime-state",
            ] {
                let _ = std::fs::create_dir_all(data_dir.join(sub));
            }

            // Ensure the window is visible and focused on startup
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }

            // Deep-link listener (v0.4.0 P0-15). Fires when the OS hands
            // us an `alfred://...` URL — typically the LS checkout
            // fallback path: LS redirects to a server endpoint that 302s
            // to `alfred://license-activated?key=<key>` which the OS
            // routes to this running instance (after registering the
            // scheme via the plugin config).
            //
            // We parse the key here in Rust (single-source-of-truth for
            // the URL shape — same `parse_license_activated_url`
            // helper exercised by unit tests) and emit a Tauri event
            // the JS upgrade-view module consumes. The JS handler is
            // the same one wired to the LS overlay success callback,
            // so the activation flow is one path regardless of trigger.
            //
            // Window focus: pull the running window forward so the
            // user lands on the app immediately after the system browser
            // hands off — without focus, the activation toast would
            // fire silently behind the user's browser window.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    use tauri::Emitter;
                    for url in event.urls() {
                        let raw = url.as_str();
                        crate::debug_log(&format!(
                            "deep-link: received {}",
                            redact_license_key_in_url(raw)
                        ));
                        if let Some(key) = parse_license_activated_url(raw) {
                            if let Some(win) = handle.get_webview_window("main") {
                                let _ = win.show();
                                let _ = win.set_focus();
                                let _ = win.unminimize();
                            }
                            let payload = serde_json::json!({ "key": key });
                            if let Err(e) = handle.emit("alfred://license-activated", payload) {
                                crate::debug_log(&format!(
                                    "deep-link: emit alfred://license-activated failed: {e}"
                                ));
                            }
                        }
                    }
                });
            }

            // Cleanup orphaned runs — uses the in-memory index so it's instant.
            run_state::cleanup_orphaned_runs();
            // Bug B follow-up — one-shot line-memory zero-price repair, gated
            // by the `line_memory_repaired_v1` runtime setting so it runs once
            // per install. The repair is plain blocking I/O (reads/writes
            // `line-memory.json`, fsync, then a settings patch) that can take
            // hundreds of milliseconds on large memories or slow disks, so we
            // offload it to the blocking thread-pool to keep the setup hook —
            // and therefore window paint — non-blocking. The task is
            // idempotent (gated by `line_memory_repaired_v1`) and errors are
            // swallowed inside; the next launch retries on failure. App boot
            // must never wait on this.
            tauri::async_runtime::spawn_blocking(
                native_mcp_analysis::maybe_run_zero_price_repair_at_startup,
            );
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
            probe_codex_quota_local,
            codex_auth_mode_local,
            swap_codex_to_apikey_local,
            swap_codex_to_oauth_local,
            codex_has_oauth_backup_local,
            account_positions_local,
            get_user_preferences_local,
            save_user_preferences_local,
            watchlist_confirm_local,
            storage_usage_local,
            storage_prune_local,
            storage_clear_log_local,
            check_api_auth_local,
            finary_sync_snapshot_local,
            finary_invalidate_snapshot_local,
            preview_csv_import_local,
            validate_isin_local,
            csv_import_apply_corrections_local,
            check_openai_api_key_local,
            check_for_update_local,
            download_update_local,
            install_update_local,
            js_log_local,
            chat_wizard_send_local,
            update_line_memory_local,
            repair_line_memory_local,
            get_stale_positions_local,
            get_signal_scorecard_local,
            get_run_diff_local,
            delete_run_local,
            save_alfred_state_local,
            load_alfred_state_local,
            export_report_markdown_local,
            get_admin_usage_local,
            get_admin_vps_stats_local,
            current_user_hash_local,
            admin_check_local,
            runs_count_last_7d_local,
            quota_status_local,
            compute_signal_accuracy_local,
            macro_briefing_local,
            license_activate_local,
            license_validate_local,
            license_status_local,
            license_checkout_url_local,
            register_device_local,
            redeem_code_local
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

#[cfg(test)]
mod deep_link_tests {
    //! Unit tests for the `alfred://license-activated` deep-link parse path.
    //!
    //! Covers the v0.4.0 P0-15 fallback flow: LS overlay can't close
    //! cleanly on Linux WebKitGTK → LS redirects to a server endpoint
    //! that 302s to `alfred://license-activated?key=...` → OS dispatches
    //! to the running Alfred via the registered scheme → main.rs setup
    //! handler parses + emits a Tauri event. Exercising the parser
    //! directly is the cheapest way to pin the URL contract; the
    //! plugin / emit path requires a full Tauri runtime which we don't
    //! spin up in unit tests.
    use super::*;

    #[test]
    fn deep_link_handler_parses_alfred_license_activated_uri() {
        // Happy path: well-formed LS-style fallback URL.
        let key = parse_license_activated_url("alfred://license-activated?key=ABCD-1234-EFGH-5678");
        assert_eq!(key.as_deref(), Some("ABCD-1234-EFGH-5678"));
    }

    #[test]
    fn deep_link_handler_returns_none_for_other_alfred_paths() {
        // Future routes under the alfred:// scheme must NOT be silently
        // treated as activation URLs. Pin the host-segment check.
        assert!(parse_license_activated_url("alfred://something-else?key=ABC").is_none());
        assert!(parse_license_activated_url("alfred://upgrade?key=ABC").is_none());
        // Non-alfred schemes are out-of-scope entirely.
        assert!(parse_license_activated_url("https://example.com/license-activated?key=ABC").is_none());
        assert!(parse_license_activated_url("").is_none());
    }

    #[test]
    fn deep_link_handler_handles_extra_query_params() {
        // LS may add tracking params (`?token=...&key=...`) in addition
        // to `key`. The parser must extract `key` regardless of position.
        let key = parse_license_activated_url(
            "alfred://license-activated?token=xyz&key=THE-KEY&utm_source=ls",
        );
        assert_eq!(key.as_deref(), Some("THE-KEY"));
        let key = parse_license_activated_url("alfred://license-activated?key=K1&extra=2");
        assert_eq!(key.as_deref(), Some("K1"));
    }

    #[test]
    fn deep_link_handler_returns_none_when_key_missing() {
        // Defensive — empty / no-query URLs must NOT trigger activation.
        // Sending an empty license to /license/activate would just 400,
        // but bailing here keeps the contract clean.
        assert!(parse_license_activated_url("alfred://license-activated").is_none());
        assert!(parse_license_activated_url("alfred://license-activated?").is_none());
        assert!(parse_license_activated_url("alfred://license-activated?key=").is_none());
        assert!(parse_license_activated_url("alfred://license-activated?other=val").is_none());
    }

    #[test]
    fn deep_link_handler_percent_decodes_license_keys() {
        // LS sandbox keys occasionally include `%XX` sequences when the
        // redirect URL was URL-encoded twice. Decode them so the key
        // we forward to /license/activate matches what LS issued.
        let key = parse_license_activated_url("alfred://license-activated?key=A%2DB%2DC");
        assert_eq!(key.as_deref(), Some("A-B-C"));
        // `+` decodes to space (legacy form encoding).
        let key = parse_license_activated_url("alfred://license-activated?key=K%201");
        assert_eq!(key.as_deref(), Some("K 1"));
    }

    #[test]
    fn deep_link_handler_tolerates_whitespace_around_url() {
        // OS shells sometimes hand us URLs with a trailing newline /
        // surrounding spaces. Trim defensively.
        let key = parse_license_activated_url("  alfred://license-activated?key=ABC  ");
        assert_eq!(key.as_deref(), Some("ABC"));
    }

    // ── License key redaction (P3-47) ────────────────────────────────

    #[test]
    fn redact_license_key_preserves_4_char_head() {
        // Debug log must keep a short prefix for correlation but never
        // expose the full key. 4 chars is enough to distinguish runs
        // without making the redacted log a credential.
        let redacted = redact_license_key_in_url("alfred://license-activated?key=ABCD-1234-EFGH-5678");
        assert_eq!(redacted, "alfred://license-activated?key=ABCD<redacted>");
    }

    #[test]
    fn redact_license_key_handles_short_keys() {
        // Keys shorter than 4 chars are still partly visible, but the
        // <redacted> suffix is always appended so a grep for the marker
        // catches every leak attempt.
        let redacted = redact_license_key_in_url("alfred://license-activated?key=K1");
        assert_eq!(redacted, "alfred://license-activated?key=K1<redacted>");
    }

    #[test]
    fn redact_license_key_keeps_other_params_intact() {
        // Tracking params like utm_source aren't sensitive and helpful in
        // debug logs. Only the `key` field is redacted.
        let redacted = redact_license_key_in_url(
            "alfred://license-activated?utm_source=ls&key=THE-KEY&token=xyz",
        );
        assert!(redacted.contains("utm_source=ls"), "utm preserved: {redacted}");
        assert!(redacted.contains("token=xyz"), "token preserved: {redacted}");
        assert!(redacted.contains("key=THE-<redacted>"), "key redacted: {redacted}");
        assert!(!redacted.contains("THE-KEY"), "full key MUST be absent: {redacted}");
    }

    #[test]
    fn redact_license_key_passes_through_non_alfred_urls() {
        // Non-license deep-link URLs (or HTTPS noise) pass through
        // unchanged — redaction only applies to the activation path.
        let redacted = redact_license_key_in_url("https://example.com/something?key=ABC");
        assert_eq!(redacted, "https://example.com/something?key=ABC");
        let redacted = redact_license_key_in_url("alfred://upgrade?key=ABC");
        assert_eq!(redacted, "alfred://upgrade?key=ABC");
    }

    #[test]
    fn redact_license_key_handles_missing_key() {
        // Edge case: deep-link with no key field at all. Should not panic
        // and should not insert <redacted> spuriously.
        let redacted = redact_license_key_in_url("alfred://license-activated");
        assert_eq!(redacted, "alfred://license-activated");
        let redacted = redact_license_key_in_url("alfred://license-activated?other=val");
        assert_eq!(redacted, "alfred://license-activated?other=val");
    }
}
