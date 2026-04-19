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

pub fn run_finary_login() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:login-local",
        "result": finary::session_browser_reuse()?
    }))
}

pub fn run_finary_snapshot() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:snapshot-local",
        "result": finary::fetch_snapshot()?
    }))
}

pub fn run_finary_accounts() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "finary:accounts-local",
        "result": finary::list_accounts()?
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
        "finary:login-local" | "finary_login_local" => run_finary_login(),
        "finary:snapshot-local" | "finary_snapshot_local" => run_finary_snapshot(),
        "finary:accounts-local" | "finary_accounts_local" => run_finary_accounts(),
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

// ── Stale Reanalysis Alerts (Phase 1b) ──

pub fn run_get_stale_positions() -> Result<serde_json::Value> {
    // C-1: Read from in-memory cache if loaded, else fall back to disk
    let store = {
        let cached = crate::native_mcp_analysis::line_memory_read();
        if cached.get("by_ticker").is_some() {
            cached
        } else {
            let path = crate::resolve_runtime_state_dir().join("line-memory.json");
            if !path.exists() {
                return Ok(json!({ "stale_count": 0, "stale_tickers": [] }));
            }
            crate::storage::read_json_file(&path)?
        }
    };
    let by_ticker = match store.get("by_ticker").and_then(|v| v.as_object()) {
        Some(bt) => bt,
        None => return Ok(json!({ "stale_count": 0, "stale_tickers": [] })),
    };

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut stale: Vec<serde_json::Value> = Vec::new();

    for (ticker, entry) in by_ticker {
        // Skip synthetic keys (e.g. _PORTFOLIO used for portfolio-level insights)
        if ticker.starts_with('_') { continue; }
        let reanalyse_after = entry
            .get("reanalyse_after")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if reanalyse_after.is_empty() || reanalyse_after.len() < 10 {
            continue;
        }
        // Compare date strings lexicographically (ISO format — this works correctly)
        // Safety: validate ASCII before byte-slicing to avoid UTF-8 boundary panic (W-1)
        let date_part: String = if reanalyse_after.is_ascii() {
            reanalyse_after[..10].to_string()
        } else {
            reanalyse_after.chars().take(10).collect()
        };
        if date_part.as_str() <= today.as_str() {
            let reason = entry
                .get("reanalyse_reason")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            stale.push(json!({
                "ticker": ticker,
                "reanalyse_after": &date_part,
                "reanalyse_reason": reason,
            }));
        }
    }

    let count = stale.len();
    Ok(json!({ "stale_count": count, "stale_tickers": stale }))
}

// ── Signal Scorecard (Phase 3b) ──

pub fn run_get_signal_scorecard(ticker: String) -> Result<serde_json::Value> {
    let ticker = ticker.trim().to_uppercase();
    if ticker.is_empty() {
        return Ok(json!({ "ticker": "", "signals": [], "overall_accuracy_pct": 0, "scored_count": 0, "correct_count": 0, "trend": "stable" }));
    }
    // C-1: Read from in-memory cache if loaded, else fall back to disk
    let store = {
        let cached = crate::native_mcp_analysis::line_memory_read();
        if cached.get("by_ticker").is_some() {
            cached
        } else {
            let path = crate::resolve_runtime_state_dir().join("line-memory.json");
            if !path.exists() {
                return Ok(json!({ "ticker": ticker, "signals": [], "overall_accuracy_pct": 0, "scored_count": 0, "correct_count": 0, "trend": "stable" }));
            }
            crate::storage::read_json_file(&path)?
        }
    };
    let entry = store.get("by_ticker")
        .and_then(|bt| bt.get(&ticker));
    let entry = match entry {
        Some(e) => e,
        None => return Ok(json!({ "ticker": ticker, "signals": [], "overall_accuracy_pct": 0, "scored_count": 0, "correct_count": 0, "trend": "stable" })),
    };

    let history = entry.get("signal_history").and_then(|v| v.as_array());
    let history = match history {
        Some(h) if !h.is_empty() => h,
        _ => return Ok(json!({ "ticker": ticker, "signals": [], "overall_accuracy_pct": 0, "scored_count": 0, "correct_count": 0, "trend": "stable" })),
    };

    // W-2: Sort history by date descending to ensure newest-first ordering
    let mut sorted_history = history.clone();
    sorted_history.sort_by(|a, b| {
        let da = a.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let db = b.get("date").and_then(|v| v.as_str()).unwrap_or("");
        db.cmp(da)
    });
    let history = &sorted_history;

    // Current price from most recent signal or price_tracking
    let current_price = entry.get("price_tracking")
        .and_then(|pt| pt.get("current_price").or_else(|| pt.get("price_at_signal")))
        .and_then(|v| v.as_f64())
        .or_else(|| history.first()
            .and_then(|h| h.get("price_at_signal"))
            .and_then(|v| v.as_f64()))
        .unwrap_or(0.0);

    let buy_signals = ["ACHAT", "ACHAT_FORT", "RENFORCEMENT"];
    let sell_signals = ["VENTE", "ALLEGEMENT"];

    let mut signals = Vec::new();
    let mut correct = 0usize;
    let mut incorrect = 0usize;

    for sig in history.iter() {
        let signal = sig.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let conviction = sig.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let date = sig.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let price_at = sig.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let return_pct = if price_at > 0.0 { (current_price - price_at) / price_at * 100.0 } else { 0.0 };
        let upper = signal.to_uppercase();
        let accuracy = if buy_signals.contains(&upper.as_str()) {
            if return_pct > 0.0 { correct += 1; "correct" } else { incorrect += 1; "incorrect" }
        } else if sell_signals.contains(&upper.as_str()) {
            if return_pct < 0.0 { correct += 1; "correct" } else { incorrect += 1; "incorrect" }
        } else {
            "neutral"
        };
        signals.push(json!({
            "date": date,
            "signal": signal,
            "conviction": conviction,
            "price_at_signal": price_at,
            "current_price": current_price,
            "return_pct": (return_pct * 10.0).round() / 10.0,
            "accuracy": accuracy,
        }));
    }

    let scored = correct + incorrect;
    let accuracy_pct = if scored > 0 { (correct as f64 / scored as f64 * 100.0).round() } else { 0.0 };

    // Trend: compare recent 3 vs older
    let recent_correct = signals.iter().take(3).filter(|s| s.get("accuracy").and_then(|v| v.as_str()) == Some("correct")).count();
    let recent_scored = signals.iter().take(3).filter(|s| s.get("accuracy").and_then(|v| v.as_str()) != Some("neutral")).count();
    let older_correct = signals.iter().skip(3).filter(|s| s.get("accuracy").and_then(|v| v.as_str()) == Some("correct")).count();
    let older_scored = signals.iter().skip(3).filter(|s| s.get("accuracy").and_then(|v| v.as_str()) != Some("neutral")).count();
    let trend = if recent_scored < 2 || older_scored < 2 { "stable" }
    else {
        let recent_rate = recent_correct as f64 / recent_scored as f64;
        let older_rate = older_correct as f64 / older_scored as f64;
        if recent_rate > older_rate + 0.15 { "improving" }
        else if recent_rate < older_rate - 0.15 { "declining" }
        else { "stable" }
    };

    Ok(json!({
        "ticker": ticker,
        "signals": signals,
        "overall_accuracy_pct": accuracy_pct,
        "scored_count": scored,
        "correct_count": correct,
        "trend": trend,
    }))
}

// ── Run Diff (Phase 4a) ──

pub fn run_get_run_diff() -> Result<serde_json::Value> {
    // C-1: Read from in-memory cache if loaded, else fall back to disk
    let store = {
        let cached = crate::native_mcp_analysis::line_memory_read();
        if cached.get("by_ticker").is_some() {
            cached
        } else {
            let path = crate::resolve_runtime_state_dir().join("line-memory.json");
            if !path.exists() {
                return Ok(json!({ "has_previous": false, "changes": [], "summary": { "signal_changes": 0, "upgrades": 0, "downgrades": 0, "significant_moves": 0, "total_positions": 0 } }));
            }
            crate::storage::read_json_file(&path)?
        }
    };
    let by_ticker = match store.get("by_ticker").and_then(|v| v.as_object()) {
        Some(bt) => bt,
        None => return Ok(json!({ "has_previous": false, "changes": [], "summary": {} })),
    };

    let buy_strength = |s: &str| -> i32 {
        match s.to_uppercase().as_str() {
            "VENTE" => 1, "ALLEGEMENT" => 2, "SURVEILLANCE" => 3,
            "CONSERVER" => 4, "RENFORCEMENT" => 5, "ACHAT" => 6, "ACHAT_FORT" => 7,
            _ => 3,
        }
    };

    let mut changes = Vec::new();
    let mut signal_changes = 0usize;
    let mut upgrades = 0usize;
    let mut downgrades = 0usize;
    let mut significant_moves = 0usize;
    let mut total = 0usize;

    for (ticker, entry) in by_ticker {
        // Skip synthetic keys (e.g. _PORTFOLIO used for portfolio-level insights)
        if ticker.starts_with('_') { continue; }
        let raw_history = entry.get("signal_history").and_then(|v| v.as_array());
        let raw_history = match raw_history {
            Some(h) if h.len() >= 2 => h,
            _ => continue,
        };
        // W-2: Sort by date descending to ensure newest-first ordering
        let mut history = raw_history.clone();
        history.sort_by(|a, b| {
            let da = a.get("date").and_then(|v| v.as_str()).unwrap_or("");
            let db = b.get("date").and_then(|v| v.as_str()).unwrap_or("");
            db.cmp(da)
        });
        total += 1;
        let curr = &history[0];
        let prev = &history[1];
        let curr_signal = curr.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let prev_signal = prev.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let curr_conv = curr.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let prev_conv = prev.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let curr_price = curr.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let prev_price = prev.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let price_change = if prev_price > 0.0 { (curr_price - prev_price) / prev_price * 100.0 } else { 0.0 };
        let sig_changed = curr_signal != prev_signal;
        let conv_changed = curr_conv != prev_conv;
        let big_move = price_change.abs() > 5.0;

        if sig_changed || conv_changed || big_move {
            if sig_changed {
                signal_changes += 1;
                // W-3: explicit equality check — equal-strength signals are not upgrades or downgrades
                if buy_strength(curr_signal) > buy_strength(prev_signal) { upgrades += 1; }
                else if buy_strength(curr_signal) < buy_strength(prev_signal) { downgrades += 1; }
            }
            if big_move { significant_moves += 1; }
            changes.push(json!({
                "ticker": ticker,
                "signal_changed": sig_changed,
                "prev_signal": prev_signal,
                "curr_signal": curr_signal,
                "conviction_changed": conv_changed,
                "prev_conviction": prev_conv,
                "curr_conviction": curr_conv,
                "price_change_pct": (price_change * 10.0).round() / 10.0,
                "significant_price_move": big_move,
            }));
        }
    }

    Ok(json!({
        "has_previous": total > 0,
        "changes": changes,
        "summary": {
            "signal_changes": signal_changes,
            "upgrades": upgrades,
            "downgrades": downgrades,
            "significant_moves": significant_moves,
            "total_positions": total,
        }
    }))
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

// ── Alfred Session State (Phase D) ──

pub fn run_save_alfred_state(state: serde_json::Value) -> Result<serde_json::Value> {
    let path = crate::resolve_runtime_state_dir().join("alfred-session.json");
    crate::storage::write_json_file(&path, &state)?;
    Ok(json!({"ok": true}))
}

pub fn run_load_alfred_state() -> Result<serde_json::Value> {
    let path = crate::resolve_runtime_state_dir().join("alfred-session.json");
    if path.exists() {
        crate::storage::read_json_file(&path)
    } else {
        Ok(json!({}))
    }
}

// ── Export Report as Markdown (Item 12) ──

pub fn run_export_report_markdown(payload: serde_json::Value) -> Result<serde_json::Value> {
    // Build a default export path in the data dir
    let data_dir = crate::paths::default_data_dir();
    let exports_dir = data_dir.join("exports");
    std::fs::create_dir_all(&exports_dir)
        .map_err(|e| anyhow!("export_mkdir_failed:{e}"))?;

    let account = payload.get("account")
        .and_then(|v| v.as_str())
        .unwrap_or("portfolio");
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let safe_account = account.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
    let filename = format!("alfred-report-{}-{}.md", safe_account, date);
    let path = exports_dir.join(&filename);

    let md = format_report_markdown(&payload);
    std::fs::write(&path, md.as_bytes())
        .map_err(|e| anyhow!("export_write_failed:{e}"))?;

    Ok(json!({
        "ok": true,
        "path": path.to_string_lossy().to_string(),
        "filename": filename,
    }))
}

fn format_report_markdown(payload: &serde_json::Value) -> String {
    let mut md = String::new();

    // Frontmatter
    let account = payload.get("account").and_then(|v| v.as_str()).unwrap_or("N/A");
    let date = payload.get("lastUpdate").and_then(|v| v.as_str()).unwrap_or("N/A");
    let value = payload.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let gain = payload.get("gain").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cash = payload.get("cash").and_then(|v| v.as_f64()).unwrap_or(0.0);

    md.push_str("---\n");
    md.push_str(&format!("account: {}\n", account));
    md.push_str(&format!("date: {}\n", date));
    md.push_str(&format!("portfolio_value: {:.0}\n", value));
    md.push_str(&format!("gain: {:.0}\n", gain));
    md.push_str(&format!("cash: {:.0}\n", cash));
    md.push_str("---\n\n");

    md.push_str(&format!("# Alfred Report — {}\n\n", account));
    md.push_str(&format!("**Date**: {} | **Value**: {:.0}\u{00a0}\u{20ac} | **Gain**: {:.0}\u{00a0}\u{20ac} | **Cash**: {:.0}\u{00a0}\u{20ac}\n\n", date, value, gain, cash));

    // Synthesis
    let synthesis = payload.get("synthesis").and_then(|v| v.as_str()).unwrap_or("");
    if !synthesis.is_empty() {
        md.push_str("## Synthesis\n\n");
        md.push_str(synthesis);
        md.push_str("\n\n");
    }

    // Action table
    let actions = payload.get("actionsNow").and_then(|v| v.as_array());
    if let Some(actions) = actions {
        if !actions.is_empty() {
            md.push_str("## Immediate Actions\n\n");
            md.push_str("| # | Ticker | Action | Type | Rationale |\n");
            md.push_str("|---|--------|--------|------|----------|\n");
            for action in actions {
                let priority = action.get("priority").and_then(|v| v.as_u64()).unwrap_or(0);
                let ticker = action.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
                let signal = action.get("action").and_then(|v| v.as_str()).unwrap_or("?");
                let order = action.get("orderType").and_then(|v| v.as_str()).unwrap_or("MARKET");
                let rationale = action.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
                // Truncate rationale for table readability
                let short_rationale = if rationale.len() > 120 {
                    format!("{}...", &rationale[..rationale.char_indices().nth(120).map(|(i,_)| i).unwrap_or(rationale.len())])
                } else {
                    rationale.to_string()
                };
                md.push_str(&format!("| {} | {} | {} | {} | {} |\n",
                    priority, ticker, signal, order, short_rationale.replace('|', "\\|")));
            }
            md.push_str("\n");
        }
    }

    // Positions summary
    let recommendations = payload.get("recommendations").and_then(|v| v.as_array());
    if let Some(recs) = recommendations {
        if !recs.is_empty() {
            md.push_str("## Positions\n\n");
            md.push_str("| Ticker | Name | Signal | Conviction | Summary |\n");
            md.push_str("|--------|------|--------|------------|--------|\n");
            for rec in recs {
                let ticker = rec.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
                let name = rec.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let signal = rec.get("signal").and_then(|v| v.as_str()).unwrap_or("");
                let conviction = rec.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
                let summary = rec.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                let short_summary = if summary.len() > 100 {
                    format!("{}...", &summary[..summary.char_indices().nth(100).map(|(i,_)| i).unwrap_or(summary.len())])
                } else {
                    summary.to_string()
                };
                md.push_str(&format!("| {} | {} | {} | {} | {} |\n",
                    ticker, name, signal, conviction, short_summary.replace('|', "\\|")));
            }
            md.push_str("\n");
        }
    }

    md.push_str("---\n*Generated by Alfred Desktop*\n");
    md
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

