//! Alfred CLI — standalone headless binary (no Tauri/GTK/WebKit dependencies).
//!
//! Shares all domain modules with the Tauri desktop app via `#[path]` includes.
//! The only difference: no GUI, no Tauri event emission, CLI-only entry point.

// ── Domain modules (shared with src-tauri via path includes) ─────────────────

#[path = "../../src-tauri/src/models/mod.rs"]
mod models;

#[path = "../../src-tauri/src/services/local_db_service.rs"]
mod local_db_service;
#[path = "../../src-tauri/src/services/local_http.rs"]
mod local_http;
#[path = "../../src-tauri/src/services/native_collection.rs"]
mod native_collection;
#[path = "../../src-tauri/src/services/native_collection_modes.rs"]
mod native_collection_modes;
#[path = "../../src-tauri/src/services/native_collection_dispatch.rs"]
mod native_collection_dispatch;
#[path = "../../src-tauri/src/services/native_collection_helpers.rs"]
mod native_collection_helpers;
#[path = "../../src-tauri/src/services/native_line_analysis.rs"]
mod native_line_analysis;
#[path = "../../src-tauri/src/repositories/sqlite/migrations.rs"]
mod sqlite_migrations;
#[path = "../../src-tauri/src/services/native_mcp_analysis.rs"]
mod native_mcp_analysis;

#[path = "../../src-tauri/src/storage.rs"]
mod storage;
#[path = "../../src-tauri/src/paths.rs"]
mod paths;
#[path = "../../src-tauri/src/helpers.rs"]
mod helpers;
#[path = "../../src-tauri/src/runtime_settings.rs"]
mod runtime_settings;
#[path = "../../src-tauri/src/health.rs"]
mod health;
#[path = "../../src-tauri/src/run_state.rs"]
mod run_state;
#[path = "../../src-tauri/src/run_state_cache.rs"]
mod run_state_cache;
#[path = "../../src-tauri/src/finary.rs"]
mod finary;
#[path = "../../src-tauri/src/report.rs"]
mod report;
#[path = "../../src-tauri/src/analysis_ops.rs"]
mod analysis_ops;
#[path = "../../src-tauri/src/command_handlers.rs"]
mod command_handlers;
#[path = "../../src-tauri/src/cli.rs"]
mod cli;
#[path = "../../src-tauri/src/alfred_api_client.rs"]
mod alfred_api_client;
#[path = "../../src-tauri/src/codex.rs"]
mod codex;
#[path = "../../src-tauri/src/enrichment.rs"]
mod enrichment;
#[path = "../../src-tauri/src/llm.rs"]
mod llm;
#[path = "../../src-tauri/src/llm_prompts.rs"]
mod llm_prompts;
#[path = "../../src-tauri/src/llm_parsing.rs"]
mod llm_parsing;
#[path = "../../src-tauri/src/mcp_server.rs"]
mod mcp_server;
#[path = "../../src-tauri/src/mcp_progress_relay.rs"]
mod mcp_progress_relay;
#[path = "../../src-tauri/src/run_stats.rs"]
mod run_stats;
#[path = "../../src-tauri/src/storage_cleanup.rs"]
mod storage_cleanup;
#[path = "../../src-tauri/src/run_index.rs"]
mod run_index;
#[path = "../../src-tauri/src/updater.rs"]
mod updater;
#[path = "../../src-tauri/src/llm_backend.rs"]
mod llm_backend;
#[path = "../../src-tauri/src/openai_client.rs"]
mod openai_client;
#[path = "../../src-tauri/src/chat_wizard.rs"]
mod chat_wizard;

use std::env;

// ── No-op event emission (replaces Tauri AppHandle event bus) ────────────────

/// In the CLI there is no GUI window to emit events to.
/// This is a silent no-op so domain modules that call `crate::emit_event()`
/// compile and run without any Tauri dependency.
pub fn emit_event(_event: &str, _payload: serde_json::Value) {
    // no-op: no GUI window to receive events
}

// ── Re-exports expected by domain modules via `crate::*` ─────────────────────

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

// ── CLI entry point ──────────────────────────────────────────────────────────

/// Load env vars from `.alfred.local.env` (repo root) if it exists.
/// Only sets vars that are not already set in the environment.
fn load_local_env() {
    let candidates = [
        std::env::var("APP_ROOT").ok().map(std::path::PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .and_then(|p| p.parent().map(|d| d.to_path_buf())),
        std::env::current_dir().ok(),
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

fn main() {
    load_local_env();

    let args: Vec<String> = env::args().collect();

    // MCP server mode
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

    // CLI dispatch — always CLI mode (no GUI fallback)
    let result = if args.len() > 1 {
        cli::run(&args)
    } else {
        // No args: show help
        let help_args = vec!["alfred".to_string(), "help".to_string()];
        cli::run(&help_args)
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
