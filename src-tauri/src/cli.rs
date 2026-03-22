//! CLI mode: parse arguments and dispatch to command handlers.

use std::{env, path::PathBuf};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::command_handlers;
use crate::local_db_service::LocalDbService;
use crate::paths::default_db_path;

pub fn should_run_cli(args: &[String]) -> bool {
    args.get(1).is_some()
}

pub fn run(args: &[String]) -> Result<()> {
    let payload = dispatch(args)?;
    println!("{payload}");
    Ok(())
}

pub(crate) fn dispatch(args: &[String]) -> Result<serde_json::Value> {
    let command = args.get(1).map(|v| v.as_str()).unwrap_or("health");
    match command {
        "health" => Ok(json!({
            "ok": true,
            "service": "alfred-desktop-backend",
        })),
        "db:init" => {
            let db_path = env::var("ALFRED_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| default_db_path());
            let db = LocalDbService::new(db_path.clone());
            db.init()?;
            Ok(json!({
                "ok": true,
                "action": "db:init",
                "db_path": db_path.display().to_string()
            }))
        }
        "analysis:run-start-local" | "analysis_run_start_local" => command_handlers::invoke_command(command),
        "analysis:retry-global-synthesis-local" | "retry_global_synthesis_local" => {
            let run_id = args
                .get(2)
                .ok_or_else(|| anyhow!("run_id_required"))?
                .to_string();
            command_handlers::run_retry_global_synthesis(run_id)
        }
        "analysis:run-status-local" | "analysis_run_status_local" => {
            let operation_id = args
                .get(2)
                .ok_or_else(|| anyhow!("analysis_operation_id_required"))?
                .to_string();
            command_handlers::run_analysis_status(operation_id)
        }
        "run:by-id-local" | "run_by_id_local" => {
            let run_id = args
                .get(2)
                .ok_or_else(|| anyhow!("run_id_required"))?
                .to_string();
            command_handlers::run_by_id(run_id)
        }
        "dashboard:snapshot-local" | "dashboard_snapshot_local" => command_handlers::invoke_command(command),
        "dashboard:overview-local" | "dashboard_overview_local" => command_handlers::invoke_command(command),
        "dashboard:details-local" | "dashboard_details_local" => command_handlers::invoke_command(command),
        "runtime:settings-local" | "runtime_settings_local" => command_handlers::invoke_command(command),
        "runtime:settings-update-local" | "runtime_settings_update_local" => {
            let settings = args
                .get(2)
                .ok_or_else(|| anyhow!("runtime_settings_payload_required"))?;
            let patch = serde_json::from_str::<serde_json::Value>(settings)
                .map_err(|error| anyhow!("runtime_settings_payload_invalid:{error}"))?;
            command_handlers::run_runtime_settings_update(patch)
        }
        "runtime:settings-reset-local" | "runtime_settings_reset_local" => command_handlers::invoke_command(command),
        "stack:health-local" | "stack_health_local" => command_handlers::invoke_command(command),
        "finary:session-status-local" | "finary_session_status_local" => command_handlers::invoke_command(command),
        "finary:session-connect-local" | "finary_session_connect_local" => command_handlers::invoke_command(command),
        "finary:session-refresh-local" | "finary_session_refresh_local" => command_handlers::invoke_command(command),
        "finary:session-browser-start-local" | "finary_session_browser_start_local" => command_handlers::invoke_command(command),
        "finary:session-browser-complete-local" | "finary_session_browser_complete_local" => command_handlers::invoke_command(command),
        "finary:session-browser-playwright-local" | "finary_session_browser_playwright_local" => command_handlers::invoke_command(command),
        "finary:session-browser-reuse-local" | "finary_session_browser_reuse_local" => command_handlers::invoke_command(command),
        "codex:session-status-local" | "codex_session_status_local" => command_handlers::invoke_command(command),
        "codex:session-login-local" | "codex_session_login_local" => command_handlers::invoke_command(command),
        "codex:session-logout-local" | "codex_session_logout_local" => command_handlers::invoke_command(command),
        "desktop:open-external-url" | "desktop_open_external_url" => {
            let url = args
                .get(2)
                .ok_or_else(|| anyhow!("external_url_required"))?
                .to_string();
            command_handlers::open_external_url(&url)
        }
        "help" => Ok(json!({
            "ok": true,
            "commands": [
                "health",
                "db:init",
                "analysis:run-start-local",
                "analysis:retry-global-synthesis-local <run_id>",
                "analysis:run-status-local <operation_id>",
                "dashboard:snapshot-local",
                "dashboard:overview-local",
                "dashboard:details-local",
                "runtime:settings-local",
                "runtime:settings-update-local <settings_json>",
                "runtime:settings-reset-local",
                "stack:health-local",
                "analysis_run_start_local",
                "retry_global_synthesis_local <run_id>",
                "analysis_run_status_local <operation_id>",
                "dashboard_snapshot_local",
                "dashboard_overview_local",
                "dashboard_details_local",
                "runtime_settings_local",
                "runtime_settings_update_local <settings_json>",
                "runtime_settings_reset_local",
                "stack_health_local",
                "finary:session-status-local",
                "finary:session-connect-local",
                "finary:session-refresh-local",
                "finary:session-browser-start-local",
                "finary:session-browser-complete-local",
                "finary:session-browser-playwright-local",
                "finary:session-browser-reuse-local",
                "finary_session_status_local",
                "finary_session_connect_local",
                "finary_session_refresh_local",
                "finary_session_browser_start_local",
                "finary_session_browser_complete_local",
                "finary_session_browser_playwright_local",
                "finary_session_browser_reuse_local",
                "codex:session-status-local",
                "codex:session-login-local",
                "codex:session-logout-local",
                "desktop:open-external-url <url>",
                "desktop_open_external_url <url>",
                "help"
            ]
        })),
        other => Err(anyhow!("unknown_command:{other}")),
    }
}
