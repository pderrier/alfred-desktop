//! Tauri command handlers — thin async wrappers around domain logic.

use anyhow::{anyhow, Result};
use serde_json::json;
use std::process::Command;

use crate::analysis_ops;
use crate::finary;
use crate::report;
use crate::run_state;
use crate::runtime_settings;
use crate::storage_cleanup;

// ── Bridge envelope helper ──
//
// The JS bridge (`bridge-client.js::normalizeTauriPayload`) requires every
// Tauri command response to follow the shape
// `{ ok: true, action: "<command>", result: <value> }`. Returning a flat
// payload triggers `bridge_payload_invalid` in the UI. Use this helper for
// any handler whose result is the only data being forwarded — it pins the
// envelope shape once instead of repeating the json! literal at every
// call site (which is exactly how the v0.3.0 storage handlers drifted).
pub fn bridge_envelope(action: &str, result: serde_json::Value) -> serde_json::Value {
    json!({
        "ok": true,
        "action": action,
        "result": result,
    })
}

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

/// P1-82 — delete/ban a poisoned analysis run. Bans the run_id (guaranteed
/// safety net) then surgically purges its line-memory signal_history +
/// recomputes derived fields + removes the run's files and index entry.
pub fn run_delete_run(run_id: String) -> Result<serde_json::Value> {
    let outcome = crate::run_deletion::delete_run(&run_id)?;
    Ok(json!({
        "ok": true,
        "action": "run:delete-local",
        "result": outcome.to_summary()
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

pub fn run_finary_refresh_token() -> Result<serde_json::Value> {
    let token = crate::finary::refresh_clerk_token()?;
    Ok(json!({
        "ok": true,
        "action": "finary:refresh-token-local",
        "token": token
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
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:"))
    {
        return Err(anyhow!("external_url_invalid"));
    }
    // Reject control characters and encoded CR/LF to reduce shell/launcher injection risk.
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(anyhow!("external_url_invalid"));
    }
    if lower.contains("%0d") || lower.contains("%0a") {
        return Err(anyhow!("external_url_invalid"));
    }
    Ok(trimmed.to_string())
}

pub fn open_external_url(url: &str) -> Result<serde_json::Value> {
    let safe_url = validate_external_url(url)?;
    #[cfg(target_os = "windows")]
    let status = Command::new("rundll32")
        .arg("url.dll,FileProtocolHandler")
        .arg(&safe_url)
        .status()
        .map_err(|error| anyhow!("external_url_open_failed:{error}"))?;
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

// ── Storage cleanup handlers ──
//
// Wrap the `storage_cleanup::*` helpers in the bridge envelope expected by
// `bridge-client.js::normalizeTauriPayload`. Without the envelope the UI
// surfaces `bridge_payload_invalid` and the Settings → Storage panel shows
// "Could not read storage usage" / "Prune failed". See P0-6 in
// `docs/plans/po-plan-2026-05.md`.

pub fn run_storage_usage() -> Result<serde_json::Value> {
    Ok(bridge_envelope("storage_usage_local", storage_cleanup::get_storage_usage()))
}

pub fn run_storage_prune(keep: usize) -> Result<serde_json::Value> {
    let result = storage_cleanup::prune_old_runs(keep)?;
    Ok(bridge_envelope("storage_prune_local", result))
}

pub fn run_storage_clear_log() -> Result<serde_json::Value> {
    let result = storage_cleanup::clear_debug_log()?;
    Ok(bridge_envelope("storage_clear_log_local", result))
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
        "finary:refresh-token-local" | "finary_refresh_token_local" => run_finary_refresh_token(),
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
        "codex:probe-quota-local" | "probe_codex_quota_local" => run_probe_codex_quota(),
        "codex:auth-mode-local" | "codex_auth_mode_local" => run_codex_auth_mode(),
        "storage_usage_local" => run_storage_usage(),
        "storage_prune_local" => run_storage_prune(10),
        "storage_clear_log_local" => run_storage_clear_log(),
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

/// Watchlist Curation v2 (D2/D3/D4) — resume command for the mid-run
/// confirmation modal. The frontend calls this when the user confirms the
/// watchlist checklist. It:
///   1. resolves any user-added tickers carrying an ISIN via `/api/resolve`
///      (parity with `resolve_watchlist_items`),
///   2. persists the confirmed set as `watchlist_by_account[account]` and the
///      free-text directive as `watchlist_feedback_by_account[account]`
///      (deep-merged so sibling accounts survive),
///   3. signals the worker's watchlist gate with the confirmed item list so the
///      run proceeds with exactly what the user kept.
///
/// `confirmed_items` is the final checklist (kept candidates the user left
/// checked). `added_tickers` is a list of `{ticker, nom?, isin?}` objects the
/// user typed in. They are merged + deduped by upper-cased ticker, kept first.
pub fn run_watchlist_confirm(
    run_id: String,
    account: String,
    confirmed_items: serde_json::Value,
    added_tickers: serde_json::Value,
    feedback: Option<String>,
) -> Result<serde_json::Value> {
    let run_id = run_id.trim().to_string();
    if run_id.is_empty() {
        return Err(anyhow::anyhow!("watchlist_confirm_run_id_required"));
    }

    let confirmed = confirmed_items.as_array().cloned().unwrap_or_default();
    let added = added_tickers.as_array().cloned().unwrap_or_default();

    // Resolve added items (ISIN-keyed; a no-op for bare-ticker entries — the
    // collection pipeline routes those by ticker as usual).
    let resolved_added = crate::native_collection::resolve_watchlist_items(added);

    // Merge confirmed + added, dedupe by upper-cased ticker (kept first so a
    // user re-adding a kept ticker doesn't duplicate it).
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut merged: Vec<serde_json::Value> = Vec::new();
    for item in confirmed.into_iter().chain(resolved_added.into_iter()) {
        let ticker = item
            .get("ticker")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_uppercase();
        if ticker.is_empty() || !seen.insert(ticker) {
            continue;
        }
        merged.push(item);
    }

    // Persist the confirmed list + feedback (deep-merged by account).
    let mut prefs = json!({
        "watchlist_by_account": { account.clone(): merged.clone() }
    });
    if let Some(fb) = feedback.as_ref() {
        if let Some(obj) = prefs.as_object_mut() {
            obj.insert(
                "watchlist_feedback_by_account".to_string(),
                json!({ account.clone(): fb }),
            );
        }
    }
    runtime_settings::save_user_preferences(&prefs)?;

    // Signal the gate so the worker proceeds with the confirmed set.
    let waited = crate::analysis_ops::submit_watchlist_confirmation(
        &run_id,
        json!({ "items": merged, "account": account }),
    );
    Ok(json!({ "ok": true, "gate_signalled": waited, "count": merged.len() }))
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

// ── Signal Scorecard (Phase 3b — v0.3.3 P0-8) ──
//
// Scorecard tuning constants. Each is documented with the "why" so future
// edits don't drift away from product intent:
//   * SCOREABLE_AGE_DAYS: a signal needs market time to play out. Anything
//     < 5d old with near-zero drift is "too early to call" → "pending".
//   * RECENT_NEAR_ZERO_RETURN_TOL: 2%. Within this band AND under the age
//     window, the signal stays pending.
//   * HOLD_NEUTRAL_TOL: 1%. CONSERVER between -1%/+1% is "noise", not
//     a thesis pass or fail.
//   * WATCH_ALERT_THRESHOLD: 5%. SURVEILLANCE that misses a drop deeper
//     than 5% earns a `watch_missed_drop` flag — visible regret marker but
//     not counted as incorrect (no action was taken).
const SCOREABLE_AGE_DAYS: i64 = 5;
const RECENT_NEAR_ZERO_RETURN_TOL: f64 = 2.0; // %
const HOLD_NEUTRAL_TOL: f64 = 1.0; // %
const WATCH_ALERT_THRESHOLD: f64 = 5.0; // %

/// Maps an LLM signal string to a coarse semantic kind.
///
/// v0.3.3 P0-8: introduced to separate CONSERVER (Hold) from SURVEILLANCE
/// (Watch) — previously both fell through to "neutral" and were never scored.
/// The mapping is single-sourced here so the scorecard, the audit tools, and
/// any future consumer all read the same intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Active long: ACHAT / ACHAT_FORT / RENFORCEMENT / BUY.
    Buy,
    /// Active short or trim: VENTE / VENDRE / ALLEGEMENT / SELL.
    Sell,
    /// Active hold: CONSERVER / MAINTIEN / HOLD — analyst confirmed the
    /// position; thesis is validated by appreciation, invalidated by drop.
    Hold,
    /// Watch-only: SURVEILLANCE / MONITORING / WATCH / SURVEILLER — analyst
    /// did NOT take a position. Never counts toward accuracy; deep drops earn
    /// a regret flag but not an "incorrect" mark.
    Watch,
    /// Watchlist Curation v2 (D1): ECARTER — the LLM proposal was VALIDATED
    /// as a non-opportunity and rejected. Like Watch it represents "no
    /// position taken", but it is an explicit *rejection* of a proposed entry,
    /// not an ongoing observation. Never scored; never surfaced as an action.
    Discard,
    /// Unknown / unmapped (or empty).
    Other,
}

/// Maps an LLM signal string to a coarse semantic kind.
///
/// Watchlist Curation v2 (D1): the watchlist verdict vocabulary
/// (`ENTRER | ACHAT_SUR_REPLI | SURVEILLER | ECARTER`) is folded into the same
/// kinds so every consumer (scorecard, actions enrichment, synthesis tallies)
/// reads one mapping. Entry verdicts are Buy-tier; SURVEILLER is Watch; ECARTER
/// is the dedicated Discard kind (never Sell — there is no held position to
/// sell). The order matters: ECARTER is checked before the generic SELL match
/// would ever apply (it shares no substring anyway, but the explicit branch
/// documents intent).
pub fn classify_signal(raw: &str) -> SignalKind {
    let upper = raw.trim().to_uppercase();
    if upper.is_empty() {
        return SignalKind::Other;
    }
    // Watchlist entry verdicts → Buy-tier. ACHAT_SUR_REPLI already matches the
    // generic ACHAT branch below; ENTRER needs its own branch.
    if upper == "ENTRER" {
        return SignalKind::Buy;
    }
    // Watchlist rejection — dedicated kind, never Sell.
    if upper == "ECARTER" {
        return SignalKind::Discard;
    }
    if upper.contains("ACHAT") || upper.contains("RENFORC") || upper == "BUY" {
        return SignalKind::Buy;
    }
    if upper.contains("VENTE")
        || upper.contains("VENDR")
        || upper.contains("ALLEG")
        || upper == "SELL"
    {
        return SignalKind::Sell;
    }
    if upper.contains("CONSERV") || upper.contains("MAINTIEN") || upper == "HOLD" {
        return SignalKind::Hold;
    }
    if upper.contains("SURVEILL")
        || upper.contains("MONITOR")
        || upper == "WATCH"
    {
        return SignalKind::Watch;
    }
    SignalKind::Other
}

#[cfg(test)]
pub fn classify_signal_for_test(raw: &str) -> SignalKind {
    classify_signal(raw)
}

pub fn run_get_signal_scorecard(ticker: String) -> Result<serde_json::Value> {
    fn as_f64_loose(v: Option<&serde_json::Value>) -> Option<f64> {
        match v {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            Some(serde_json::Value::String(s)) => s.trim().replace(',', ".").parse::<f64>().ok(),
            _ => None,
        }
    }
    fn days_between(today: &str, date: &str) -> Option<i64> {
        let d_today = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d").ok()?;
        let d_signal = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        Some(d_today.signed_duration_since(d_signal).num_days())
    }

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
    // v0.3.2 (P0-4 / P0-5): resolve the line-memory entry via the canonical-aware
    // helper. Raw `by_ticker.get(&ticker)` would miss any entry written under
    // the canonical Yahoo symbol by `sync_line_memory` (v0.3 #22 dedup), which
    // is the root cause of the v0.3.0 scorecard regression — the entry exists
    // but is keyed `STMPA.PA`, not `STMPA`. See
    // `feedback_contract_tests_regression_audit` (3rd occurrence of the same
    // class).
    let entry = crate::native_mcp_analysis::resolve_line_memory_key(&store, &ticker);
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

    // v0.3.3 P0-8: only the freshly-resolved `price_tracking.current_price`
    // is acceptable as the "today" anchor. Falling back to
    // `history.first().price_at_signal` would re-introduce the bug where a
    // signal's drift is measured against itself — every recent signal would
    // then read "0% drift, no thesis evolved" and stay incorrectly marked.
    // When `current_price` is missing or <= 0, every signal stays pending
    // and we log a warning (silent skip masked the production bug for weeks).
    let current_price_opt = entry.get("price_tracking")
        .and_then(|pt| as_f64_loose(pt.get("current_price")))
        .filter(|p| p.is_finite() && *p > 0.0);
    let price_is_stale = entry.get("price_tracking")
        .and_then(|pt| pt.get("stale"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if current_price_opt.is_none() {
        crate::debug_log(&format!(
            "scorecard: {ticker} has no usable current_price — all signals will read as pending. \
             This indicates `sync_line_memory` could not resolve a fresh or stale price."
        ));
    }

    // Fix 5.2/5.3: collapse contaminated legacy histories (multiple same-day
    // same-signal entries from before sync_line_memory dedup-on-write) into
    // one representative per (date, signal). The history is already sorted
    // newest-first, so the head — i.e. the freshest price for that day —
    // wins. Counting/trend then operates on distinct dates, matching the UI
    // semantics ("2/8 scored" reflects 8 distinct decision points, not 10
    // duplicated rows).
    let bucketed_history =
        crate::native_mcp_analysis::dedupe_signal_history_by_date_signal(history);

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut signals = Vec::new();
    let mut correct_dates: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut scored_dates: std::collections::HashSet<String> = std::collections::HashSet::new();

    for sig in bucketed_history.iter() {
        let signal = sig.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let conviction = sig.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let date = sig.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let price_at = as_f64_loose(sig.get("price_at_signal")).unwrap_or(0.0);
        // Measurable return requires both a non-zero anchor price AND a usable
        // current_price. price_at_signal == 0 is the "indisponible" repair
        // flag, not a real datapoint.
        let has_measurable_return = price_at > 0.0 && current_price_opt.is_some();
        let return_pct = if has_measurable_return {
            (current_price_opt.unwrap() - price_at) / price_at * 100.0
        } else {
            0.0
        };
        let kind = classify_signal(signal);
        let age_days = days_between(&today, date).unwrap_or(i64::MAX);

        // Time-window guard: signals < SCOREABLE_AGE_DAYS old AND with
        // near-zero drift stay "pending" — they haven't had time to play out.
        let in_pending_window = has_measurable_return
            && age_days < SCOREABLE_AGE_DAYS
            && return_pct.abs() < RECENT_NEAR_ZERO_RETURN_TOL;

        let mut flag: Option<&'static str> = None;
        let accuracy: &'static str = if !has_measurable_return || in_pending_window {
            "pending"
        } else {
            match kind {
                SignalKind::Buy => if return_pct > 0.0 { "correct" } else { "incorrect" },
                SignalKind::Sell => if return_pct < 0.0 { "correct" } else { "incorrect" },
                SignalKind::Hold => {
                    if return_pct.abs() < HOLD_NEUTRAL_TOL {
                        "neutral"
                    } else if return_pct > 0.0 {
                        "correct"
                    } else {
                        "incorrect"
                    }
                }
                SignalKind::Watch => {
                    if return_pct < -WATCH_ALERT_THRESHOLD {
                        flag = Some("watch_missed_drop");
                    }
                    "neutral"
                }
                // Watchlist Curation v2: a rejected proposal took no position,
                // so it never scores — like an un-acted Watch but without the
                // missed-drop regret flag (we did not propose holding it).
                SignalKind::Discard => "neutral",
                SignalKind::Other => "neutral",
            }
        };

        // Count distinct dates only — defensive even though bucketed_history
        // is already deduped by (date, signal): two entries on the same date
        // with different signals (rare, allowed by Fix 5.1) still collapse to
        // one decision point for accuracy purposes.
        if accuracy == "correct" || accuracy == "incorrect" {
            scored_dates.insert(date.to_string());
            if accuracy == "correct" {
                correct_dates.insert(date.to_string());
            }
        }

        let kind_str = match kind {
            SignalKind::Buy => "buy",
            SignalKind::Sell => "sell",
            SignalKind::Hold => "hold",
            SignalKind::Watch => "watch",
            SignalKind::Discard => "discard",
            SignalKind::Other => "other",
        };

        let mut signal_obj = json!({
            "date": date,
            "signal": signal,
            "kind": kind_str,
            "conviction": conviction,
            "price_at_signal": price_at,
            "current_price": current_price_opt,
            "stale": price_is_stale,
            "return_pct": (return_pct * 10.0).round() / 10.0,
            "accuracy": accuracy,
            "age_days": age_days,
        });
        if accuracy == "pending" {
            let days_remaining = (SCOREABLE_AGE_DAYS - age_days).max(0);
            signal_obj["pending_days_remaining"] = json!(days_remaining);
        }
        if let Some(f) = flag {
            signal_obj["flag"] = json!(f);
        }
        signals.push(signal_obj);
    }

    let scored = scored_dates.len();
    let correct = correct_dates.len();
    let accuracy_pct = if scored > 0 { (correct as f64 / scored as f64 * 100.0).round() } else { 0.0 };

    // Trend: compare recent 3 distinct-date buckets vs older. Only signals
    // that scored (correct/incorrect) count toward the trend — pending and
    // neutral are excluded so a streak of watch/hold signals doesn't muddy
    // the badge.
    let mut seen_recent_dates: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut recent_correct = 0usize;
    let mut recent_scored = 0usize;
    let mut older_correct = 0usize;
    let mut older_scored = 0usize;
    for sig in signals.iter() {
        let date = sig.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let accuracy = sig.get("accuracy").and_then(|v| v.as_str()).unwrap_or("neutral");
        let is_recent = seen_recent_dates.len() < 3 || seen_recent_dates.contains(date);
        let contributes = accuracy == "correct" || accuracy == "incorrect";
        if is_recent {
            seen_recent_dates.insert(date);
            if accuracy == "correct" { recent_correct += 1; }
            if contributes { recent_scored += 1; }
        } else {
            if accuracy == "correct" { older_correct += 1; }
            if contributes { older_scored += 1; }
        }
    }
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
        "price_stale": price_is_stale,
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

// ── Reports CLI ──

pub fn run_reports_list() -> Result<serde_json::Value> {
    let history = run_state::load_report_history(50)?;
    let rows: Vec<serde_json::Value> = history
        .iter()
        .map(|entry| {
            json!({
                "run_id": entry.get("run_id").and_then(|v| v.as_str()).unwrap_or(""),
                "saved_at": entry.get("saved_at").and_then(|v| v.as_str()).unwrap_or(""),
                "filename": entry.get("history_filename").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();
    Ok(json!({
        "ok": true,
        "action": "reports:list",
        "count": rows.len(),
        "reports": rows,
    }))
}

pub fn run_reports_latest() -> Result<serde_json::Value> {
    let report = run_state::load_latest_report()?;
    Ok(json!({
        "ok": true,
        "action": "reports:latest",
        "report": report,
    }))
}

pub fn run_reports_show(path_or_run_id: &str) -> Result<serde_json::Value> {
    let trimmed = path_or_run_id.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("path_or_run_id_required"));
    }
    // Try as a file path first
    let as_path = std::path::PathBuf::from(trimmed);
    if as_path.is_file() {
        let report = crate::storage::read_json_file(&as_path)?;
        return Ok(json!({
            "ok": true,
            "action": "reports:show",
            "source": "file",
            "report": report,
        }));
    }
    // Try inside reports dir
    let reports_dir = crate::paths::resolve_reports_dir();
    let in_reports = reports_dir.join(trimmed);
    if in_reports.is_file() {
        let report = crate::storage::read_json_file(&in_reports)?;
        return Ok(json!({
            "ok": true,
            "action": "reports:show",
            "source": "reports_dir",
            "report": report,
        }));
    }
    // Try as run_id in history
    let history_dir = crate::paths::resolve_report_history_dir();
    if history_dir.is_dir() {
        for entry in std::fs::read_dir(&history_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let artifact = match crate::storage::read_json_file(&path) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if artifact.get("run_id").and_then(|v| v.as_str()) == Some(trimmed) {
                return Ok(json!({
                    "ok": true,
                    "action": "reports:show",
                    "source": "history",
                    "report": artifact,
                }));
            }
        }
    }
    Err(anyhow!("report_not_found:{trimmed}"))
}

// ── Line CLI ──

fn load_line_memory_from_disk_or_cache() -> serde_json::Value {
    let cached = crate::native_mcp_analysis::line_memory_read();
    let has_real_data = cached
        .get("by_ticker")
        .and_then(|v| v.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if has_real_data {
        return cached;
    }
    let path = crate::resolve_runtime_state_dir().join("line-memory.json");
    if !path.exists() {
        return json!({});
    }
    crate::storage::read_json_file(&path).unwrap_or_else(|_| json!({}))
}

pub fn run_line_list() -> Result<serde_json::Value> {
    let report = run_state::load_latest_report().ok();
    let recommendations = report
        .as_ref()
        .and_then(|r| r.get("payload"))
        .and_then(|p| p.get("recommandations"))
        .and_then(|v| v.as_array());

    let rows: Vec<serde_json::Value> = if let Some(recs) = recommendations {
        recs.iter()
            .map(|rec| {
                json!({
                    "ticker": rec.get("ticker").and_then(|v| v.as_str()).unwrap_or("?"),
                    "name": rec.get("nom").or_else(|| rec.get("name")).and_then(|v| v.as_str()).unwrap_or(""),
                    "account": rec.get("compte").or_else(|| rec.get("account")).and_then(|v| v.as_str()).unwrap_or(""),
                    "signal": rec.get("signal").and_then(|v| v.as_str()).unwrap_or(""),
                    "conviction": rec.get("conviction").and_then(|v| v.as_str()).unwrap_or(""),
                    "last_price": rec.get("cours_actuel").or_else(|| rec.get("last_price")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                    "pru": rec.get("pru").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    "pv_pct": rec.get("pv_pct").and_then(|v| v.as_f64()).unwrap_or(0.0),
                })
            })
            .collect()
    } else {
        let store = load_line_memory_from_disk_or_cache();
        let by_ticker = store.get("by_ticker").and_then(|v| v.as_object());
        match by_ticker {
            Some(bt) => bt
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .map(|(ticker, entry)| {
                    let latest_signal = entry
                        .get("signal_history")
                        .and_then(|v| v.as_array())
                        .and_then(|arr| arr.last());
                    json!({
                        "ticker": ticker,
                        "name": entry.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "signal": latest_signal.and_then(|s| s.get("signal")).and_then(|v| v.as_str()).unwrap_or(""),
                        "conviction": latest_signal.and_then(|s| s.get("conviction")).and_then(|v| v.as_str()).unwrap_or(""),
                        "last_price": entry.get("price_tracking").and_then(|pt| pt.get("current_price")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                    })
                })
                .collect(),
            None => Vec::new(),
        }
    };
    Ok(json!({
        "ok": true,
        "action": "line:list",
        "count": rows.len(),
        "lines": rows,
    }))
}

pub fn run_line_show(ticker: &str) -> Result<serde_json::Value> {
    let ticker = ticker.trim().to_uppercase();
    if ticker.is_empty() {
        return Err(anyhow!("ticker_required"));
    }
    let report = run_state::load_latest_report().ok();
    let rec = report
        .as_ref()
        .and_then(|r| r.get("payload"))
        .and_then(|p| p.get("recommandations"))
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|r| {
                r.get("ticker")
                    .and_then(|v| v.as_str())
                    .map(|t| t.to_uppercase() == ticker)
                    .unwrap_or(false)
            })
        })
        .cloned();

    let store = load_line_memory_from_disk_or_cache();
    // v0.3.2 (P0-5): canonical-aware lookup so a CLI `line:show STMPA` resolves
    // an entry actually keyed `STMPA.PA`. Raw lookup would miss the v0.3 dedup
    // write path.
    let memory = crate::native_mcp_analysis::resolve_line_memory_key(&store, &ticker).cloned();

    if rec.is_none() && memory.is_none() {
        return Err(anyhow!("ticker_not_found:{ticker}"));
    }

    Ok(json!({
        "ok": true,
        "action": "line:show",
        "ticker": ticker,
        "recommendation": rec,
        "line_memory": memory,
    }))
}

pub fn run_line_memory_show(ticker: Option<&str>) -> Result<serde_json::Value> {
    let store = load_line_memory_from_disk_or_cache();
    let by_ticker = store.get("by_ticker").and_then(|v| v.as_object());

    if let Some(t) = ticker {
        let t = t.trim().to_uppercase();
        if t.is_empty() {
            return Err(anyhow!("ticker_required"));
        }
        // v0.3.2 (P0-5): canonical-aware lookup so the CLI returns the v0.3-
        // keyed entry when present (e.g. `line:memory STMPA` finds the entry
        // stored under `STMPA.PA`).
        let entry = crate::native_mcp_analysis::resolve_line_memory_key(&store, &t)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if entry.is_null() {
            return Err(anyhow!("ticker_not_found:{t}"));
        }
        Ok(json!({
            "ok": true,
            "action": "line:memory-show",
            "ticker": t,
            "memory": entry,
        }))
    } else {
        let entries: Vec<serde_json::Value> = by_ticker
            .map(|bt| {
                bt.iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(ticker, entry)| {
                        let signal_count = entry
                            .get("signal_history")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let trend = entry
                            .get("trend")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let reanalyse_after = entry
                            .get("reanalyse_after")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        json!({
                            "ticker": ticker,
                            "signal_count": signal_count,
                            "trend": trend,
                            "reanalyse_after": reanalyse_after,
                            "has_memory_narrative": entry.get("memory_narrative").or(entry.get("key_reasoning")).is_some(),
                            "has_news_themes": entry.get("news_themes").is_some(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "ok": true,
            "action": "line:memory-show",
            "count": entries.len(),
            "entries": entries,
        }))
    }
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

pub fn run_probe_codex_quota() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:probe-quota-local",
        "result": crate::codex::probe_quota()?
    }))
}

pub fn run_codex_auth_mode() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:auth-mode-local",
        "result": { "mode": crate::codex::auth_mode()? }
    }))
}

pub fn run_swap_codex_to_apikey(api_key: String) -> Result<serde_json::Value> {
    crate::codex::swap_to_apikey(&api_key)?;
    Ok(json!({
        "ok": true,
        "action": "codex:swap-to-apikey-local",
        "result": { "mode": crate::codex::auth_mode().unwrap_or("apikey") }
    }))
}

pub fn run_swap_codex_to_oauth() -> Result<serde_json::Value> {
    let restored = crate::codex::swap_to_oauth()?;
    Ok(json!({
        "ok": true,
        "action": "codex:swap-to-oauth-local",
        "result": {
            "restored": restored,
            "mode": crate::codex::auth_mode().unwrap_or("none")
        }
    }))
}

pub fn run_codex_has_oauth_backup() -> Result<serde_json::Value> {
    Ok(json!({
        "ok": true,
        "action": "codex:has-oauth-backup-local",
        "result": { "has_backup": crate::codex::has_oauth_backup() }
    }))
}

// ── Admin observability (v0.4.0 P0-14, server-driven gate P0-16) ────

/// Proxy `/admin/usage`. Server 403s for non-admins; the desktop UI hides
/// the Admin tab unless `admin_check_local` returned `{is_admin: true}`,
/// so the typical 403 path is unreachable in normal use.
pub fn run_get_admin_usage() -> Result<serde_json::Value> {
    let payload = crate::alfred_api_client::get_admin_usage()?;
    Ok(bridge_envelope("admin:usage-local", payload))
}

/// Proxy `/admin/vps-stats`. Same auth contract as `run_get_admin_usage`.
pub fn run_get_admin_vps_stats() -> Result<serde_json::Value> {
    let payload = crate::alfred_api_client::get_admin_vps_stats()?;
    Ok(bridge_envelope("admin:vps-stats-local", payload))
}

/// Return the current user's client hash (FNV-1a of OpenAI JWT). Kept for
/// diagnostic display (Pierre prints his own hash when adding it to the
/// server `ALFRED_ADMIN_HASHES` env). Returns `{user_hash: null}` when
/// no JWT is available.
pub fn run_current_user_hash() -> Result<serde_json::Value> {
    let hash = crate::alfred_api_client::current_user_hash();
    Ok(bridge_envelope(
        "admin:current-user-hash-local",
        json!({ "user_hash": hash }),
    ))
}

/// P0-20 (2026-05-23) — count runs whose `updated_at` falls in the last
/// 7 days, for the home page tier+quota header strip. Used by the JS
/// helper `quota-local-counter.js`. Returns
/// `{count: <integer>, limit: 3, period: "rolling_7d"}` so the JS can
/// render `Gratuit · X/3 cette semaine` without further math. The
/// `limit` is reported by the desktop (matches the server default
/// `FREE_TIER_RUNS_PER_WEEK = 3`).
///
/// v0.4.7 P3-31: this is now the FALLBACK source — the home strip reads
/// the authoritative `run_quota_status` (`GET /quota/status`) first and
/// only falls back to this local count on network failure. The local
/// count can drift ±1 vs the server ZSET, so it's a degraded-mode
/// approximation, not the primary.
pub fn run_runs_count_last_7d() -> Result<serde_json::Value> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let count = crate::run_index::count_runs_last_7d(now_ms);
    Ok(bridge_envelope(
        "home:runs-count-last-7d-local",
        json!({
            "count": count,
            "limit": 3,
            "period": "rolling_7d",
        }),
    ))
}

/// P3-31 (v0.4.7) — authoritative rolling-7d quota for the home strip.
/// Proxies `GET /quota/status`, which returns the SAME count the
/// enforcement gate (`quota::start_run`) sees, killing the ±1 drift of
/// the local `run-index.json` count (`run_runs_count_last_7d`). The JS
/// helper `quota-local-counter.js` uses this as the PRIMARY source and
/// falls back to the local count on network failure.
///
/// Returns the server envelope `{count, limit, period, reset_at}`
/// verbatim. `reset_at` (epoch secs, or null for an empty window) lets
/// the home strip render "réinitialisation le JJ/MM". Read-only — never
/// consumes quota. Errors propagate so the JS layer can fall back to the
/// local count rather than rendering a stale/empty strip.
pub fn run_quota_status() -> Result<serde_json::Value> {
    let payload = crate::alfred_api_client::get_quota_status()?;
    Ok(bridge_envelope("home:quota-status-local", payload))
}

/// P2-24 (2026-05-23) — cross-portfolio signal accuracy. Aggregates
/// the pre-computed `price_tracking.signal_accuracy` field across all
/// line-memory entries and returns {total_signals, correct,
/// incorrect, accuracy_pct, best_pick, worst_pick}. Used by the home
/// Section 8 "Rétro-précision Alfred" — section gated on
/// total_signals >= 5 by the JS renderer (sub-5 = noise).
pub fn run_compute_signal_accuracy() -> Result<serde_json::Value> {
    let stats = crate::signal_accuracy::compute_signal_accuracy()
        .map_err(|e| anyhow::anyhow!("signal_accuracy_failed:{e}"))?;
    Ok(bridge_envelope(
        "home:signal-accuracy-local",
        crate::signal_accuracy::stats_to_json(&stats),
    ))
}

/// P2-70 (2026-05-24) — surface the macro briefing snapshot on the home
/// page pre-run. Thin wrapper around `enrichment::fetch_macro_briefing`
/// which already hits the 30-min server-cached `/api/macro` endpoint and
/// silently degrades on transport/auth/5xx failures (returns
/// `{ok: true, macro: null}` rather than propagating an error). The JS
/// renderer treats a null briefing as "hide the tile" — matches the
/// existing `build_macro_briefing_section` contract on the LLM side, so
/// the tile and the synthesis prompt either both render or both fall
/// silent. Read-only, no quota consumption, no run state mutation.
pub fn run_macro_briefing() -> Result<serde_json::Value> {
    let envelope = crate::enrichment::fetch_macro_briefing()
        .map_err(|e| anyhow::anyhow!("macro_briefing_failed:{e}"))?;
    Ok(bridge_envelope("home:macro-briefing-local", envelope))
}

/// P0-16 (2026-05-23) — server-driven admin tab visibility probe.
/// Calls `GET /admin/check` ; returns `{is_admin: true}` on 204 (server
/// confirmed admin), `{is_admin: false}` on 403 or any error (fail-safe
/// to "not admin" — never spuriously show the tab). Errors are logged
/// via debug_log so a misconfigured deploy is diagnosable without
/// flipping the tab on.
pub fn run_admin_check() -> Result<serde_json::Value> {
    let is_admin = match crate::alfred_api_client::admin_check() {
        Ok(value) => value,
        Err(e) => {
            crate::debug_log(&format!(
                "[admin-check] probe failed, defaulting to is_admin=false: {e}"
            ));
            false
        }
    };
    Ok(bridge_envelope(
        "admin:check-local",
        json!({ "is_admin": is_admin }),
    ))
}

// ── License flow (v0.4.0 P0-15) ──────────────────────────────────────

/// Default instance name forwarded to Lemon Squeezy on activation. LS
/// displays this string in the user's account dashboard
/// ("Activated from Alfred on Pierre's MacBook Pro"). We default to the
/// machine's hostname when available, otherwise the constant fallback —
/// the server-side `activate_handler` accepts an empty value too and
/// substitutes its own default, but sending a meaningful value here is
/// better UX.
fn resolve_instance_name() -> String {
    std::env::var("ALFRED_LICENSE_INSTANCE_NAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Best-effort hostname lookup. The `hostname` env var is the
            // most portable signal across the OSes we ship; on Linux/macOS
            // `HOSTNAME` (lowercase via /etc/hostname read) and on Windows
            // `COMPUTERNAME` are the canonical sources. Try both before
            // giving up.
            std::env::var("HOSTNAME")
                .ok()
                .or_else(|| std::env::var("COMPUTERNAME").ok())
                .filter(|s| !s.is_empty())
                .map(|h| format!("alfred-desktop@{h}"))
        })
        .unwrap_or_else(|| "alfred-desktop".to_string())
}

/// `POST /license/activate` — activate a Lemon Squeezy license key.
///
/// On success the server has already written `tier:<hash> = paid` to
/// Redis and returned the typed envelope; we forward it verbatim through
/// the bridge so the JS layer can persist `license_key`,
/// `license_validated_at`, and `tier` to user-preferences.
///
/// `instance_name` is auto-derived from hostname when blank — the user
/// never has to fill it in. Pass through any explicit value the caller
/// supplied (e.g. an admin override during testing).
pub fn run_license_activate(
    license_key: String,
    instance_name: Option<String>,
) -> Result<serde_json::Value> {
    let trimmed_key = license_key.trim();
    if trimmed_key.is_empty() {
        return Err(anyhow!("license_key_required"));
    }
    let name = instance_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(resolve_instance_name);
    let resp = crate::alfred_api_client::activate_license(trimmed_key, &name)?;

    // P3-51: persist `{tier, license_key, license_validated_at}` to
    // user-preferences.json so the desktop has a local record of being
    // paid. The server's Redis `tier:<hash>` is authoritative (TTL 24h),
    // but if Redis evicts the key OR the server is unreachable on next
    // cold start, the desktop would silently fall back to the free tier.
    // The local cache covers that gap.
    //
    // Failure to persist is logged + swallowed — the activation itself
    // succeeded server-side, and the user will see their paid status on
    // the next `/license/status` round-trip. Don't fail the whole
    // activation flow over a local-file write hiccup.
    persist_license_to_user_prefs(trimmed_key, &resp);

    let payload = serde_json::to_value(resp)
        .map_err(|e| anyhow!("license_activate_serialize_failed:{e}"))?;
    Ok(bridge_envelope("license:activate-local", payload))
}

/// Build the user-preferences patch for a successful activation and call
/// `save_user_preferences` with merge-only semantics. Extracted into its
/// own fn so the unit test can drive the prefs serialisation contract
/// without needing the full LS HTTP round-trip.
///
/// On any error (serde, file I/O), log to debug.log and return — the
/// caller treats this as best-effort. Tier remains authoritative on the
/// server.
fn persist_license_to_user_prefs(
    license_key: &str,
    resp: &crate::alfred_api_client::LicenseActivationResponse,
) {
    let patch = build_license_prefs_patch(license_key, resp);
    if let Err(e) = crate::runtime_settings::save_user_preferences(&patch) {
        crate::debug_log(&format!(
            "license: failed to persist tier to user-preferences: {e} (server-side tier authoritative, cold-start may need /license/status round-trip)"
        ));
    }
}

/// Build the `{tier, license_key, license_validated_at}` JSON patch from
/// an `LicenseActivationResponse`. Pure helper so the contract can be
/// pinned by a unit test without any I/O.
fn build_license_prefs_patch(
    license_key: &str,
    resp: &crate::alfred_api_client::LicenseActivationResponse,
) -> serde_json::Value {
    // `validated_at` is epoch seconds from the server; convert to ISO
    // 8601 so the user-preferences schema stays self-describing (other
    // timestamp prefs use ISO 8601). Falls back to "" if the conversion
    // ever fails — safer than dropping the key entirely.
    let iso_ts = if resp.validated_at == 0 {
        String::new()
    } else {
        chrono::DateTime::<chrono::Utc>::from_timestamp(resp.validated_at as i64, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    };
    serde_json::json!({
        "tier": resp.tier.clone(),
        "license_key": license_key,
        "license_validated_at": iso_ts,
    })
}

/// `POST /license/validate` — revalidate an already-activated key against
/// LS. Today thin proxy (full P1-6 cold-start orchestration deferred);
/// returns the raw server response so the desktop API client has a stable
/// surface when P1-6 lands.
pub fn run_license_validate(license_key: String) -> Result<serde_json::Value> {
    let trimmed = license_key.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("license_key_required"));
    }
    let raw = crate::alfred_api_client::validate_license(trimmed)?;
    Ok(bridge_envelope("license:validate-local", raw))
}

/// `GET /license/status` — read the cached tier + pending notice for
/// the current user. No LS round-trip.
pub fn run_license_status() -> Result<serde_json::Value> {
    let resp = crate::alfred_api_client::get_license_status()?;
    let payload = serde_json::to_value(resp)
        .map_err(|e| anyhow!("license_status_serialize_failed:{e}"))?;
    Ok(bridge_envelope("license:status-local", payload))
}

/// MON-A — register a server-issued device identity on first run. No-op
/// when one already exists. Fail-soft: a transport / 404 error is swallowed
/// (the desktop keeps working on legacy `X-Client-Hash` auth; registration
/// retries on the next launch) so this never blocks startup. Returns
/// `{registered: bool}` — `true` when a fresh identity was minted.
pub fn run_register_device() -> Result<serde_json::Value> {
    let registered = match crate::alfred_api_client::ensure_device_registered() {
        Ok(r) => r,
        Err(e) => {
            crate::debug_log(&format!(
                "alfred-api: device registration deferred (legacy auth still active): {e}"
            ));
            false
        }
    };
    Ok(bridge_envelope(
        "device:register-local",
        serde_json::json!({ "registered": registered }),
    ))
}

/// MON-C — redeem an activation (comp) code via `POST /redeem`. On success
/// the server binds the code to this device and returns `{tier, expires_at}`;
/// the UI refreshes tier from the result. Structured error codes
/// (`alfred_redeem_invalid`, `alfred_redeem_already_used`) propagate so the
/// JS layer can route to the right state (invalid vs already-used/expired).
pub fn run_redeem_code(code: String) -> Result<serde_json::Value> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("alfred_redeem_invalid"));
    }
    let resp = crate::alfred_api_client::redeem_code(trimmed)?;

    // Mirror the activation path: persist the paid tier locally so a cold
    // start before the next `/license/status` round-trip still shows the
    // user as paid. Best-effort — server `tier:<id>` is authoritative.
    persist_redeem_to_user_prefs(&resp);

    Ok(bridge_envelope("redeem:code-local", resp))
}

/// Build the user-preferences patch from a successful `/redeem` response and
/// persist it (merge-only). Pure-ish split so the prefs contract is testable.
/// Best-effort: a write failure is logged + swallowed.
fn persist_redeem_to_user_prefs(resp: &serde_json::Value) {
    let patch = build_redeem_prefs_patch(resp);
    if let Err(e) = crate::runtime_settings::save_user_preferences(&patch) {
        crate::debug_log(&format!(
            "redeem: failed to persist tier to user-preferences: {e} (server-side tier authoritative)"
        ));
    }
}

/// Pure helper: build the `{tier, license_expires_at}` patch from a
/// `/redeem` response. `expires_at` (epoch secs) is stored as ISO 8601 to
/// match the rest of the timestamp prefs. Pinned by a unit test.
fn build_redeem_prefs_patch(resp: &serde_json::Value) -> serde_json::Value {
    let tier = resp.get("tier").and_then(|v| v.as_str()).unwrap_or("paid");
    let iso_ts = resp
        .get("expires_at")
        .and_then(|v| v.as_u64())
        .and_then(|secs| chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0))
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default();
    serde_json::json!({
        "tier": tier,
        "license_expires_at": iso_ts,
    })
}

/// Return the Lemon Squeezy checkout URL the desktop overlay opens via
/// `LemonSqueezy.Url.Open(...)`. v0.4.0 P0-15.
///
/// The actual URL is sourced from a compile-time env var
/// (`ALFRED_LS_CHECKOUT_URL`, baked at build time) so:
///   1. It is never checked into the public submodule repo.
///   2. Pierre can rebuild with a different URL for sandbox vs prod
///      without touching JS code.
///   3. The desktop never hard-codes a Lemon Squeezy product identifier.
///
/// When the env var is empty (the public-repo default, before Pierre
/// wires his LS dashboard) the function returns `null` for the URL +
/// a structured `not_configured` flag so the JS layer can surface a
/// clean "Activation Premium temporairement indisponible — réessaie
/// dans quelques heures." banner instead of opening a bogus checkout.
///
/// Why not query the alfred-api server for this? Because the LS
/// checkout URL has nothing user-specific — it's a static product URL
/// shared by all customers. Routing through the server would add an
/// HTTP round-trip on every Upgrade click with no benefit, and the URL
/// belongs to the Tauri build (rebuild to change), matching how we
/// handle `ALFRED_ADMIN_HASHES` whitelist.
pub fn run_license_checkout_url() -> Result<serde_json::Value> {
    // Compile-time first so the URL ships baked into Pierre's private
    // builds; runtime env fallback for local dev / CI.
    const COMPILE_TIME_URL: Option<&str> = option_env!("ALFRED_LS_CHECKOUT_URL");
    let runtime = std::env::var("ALFRED_LS_CHECKOUT_URL").ok();
    let url = COMPILE_TIME_URL
        .map(|s| s.to_string())
        .or(runtime)
        .filter(|s| !s.trim().is_empty());
    let configured = url.is_some();
    Ok(bridge_envelope(
        "license:checkout-url-local",
        json!({
            "url": url,
            "configured": configured,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_instance_name_uses_override_when_set() {
        // ALFRED_LICENSE_INSTANCE_NAME wins over hostname inference so
        // tests + admin overrides don't drift between OSes.
        std::env::set_var("ALFRED_LICENSE_INSTANCE_NAME", "ci-fixture-machine");
        assert_eq!(resolve_instance_name(), "ci-fixture-machine");
        std::env::remove_var("ALFRED_LICENSE_INSTANCE_NAME");
    }

    #[test]
    fn resolve_instance_name_falls_back_to_constant_when_no_hostname() {
        // With no override and no HOSTNAME/COMPUTERNAME, the constant
        // fallback must apply — never an empty string. Defensive
        // because LS rejects empty instance_name on some plans.
        std::env::remove_var("ALFRED_LICENSE_INSTANCE_NAME");
        std::env::remove_var("HOSTNAME");
        std::env::remove_var("COMPUTERNAME");
        let name = resolve_instance_name();
        assert!(!name.is_empty(), "instance name must never be empty");
    }

    #[test]
    fn run_license_activate_rejects_empty_key() {
        let err = run_license_activate("".into(), None).unwrap_err();
        assert!(
            err.to_string().contains("license_key_required"),
            "empty license key must surface a clear error code, got: {err}",
        );
        let err = run_license_activate("   ".into(), None).unwrap_err();
        assert!(
            err.to_string().contains("license_key_required"),
            "whitespace-only key must also fail, got: {err}",
        );
    }

    #[test]
    fn run_license_validate_rejects_empty_key() {
        let err = run_license_validate("".into()).unwrap_err();
        assert!(err.to_string().contains("license_key_required"));
        let err = run_license_validate("   ".into()).unwrap_err();
        assert!(err.to_string().contains("license_key_required"));
    }

    // ── P3-51: user-preferences persistence contract ─────────────────

    #[test]
    fn build_license_prefs_patch_shapes_payload_correctly() {
        // Contract: the patch sent to save_user_preferences must contain
        // exactly { tier, license_key, license_validated_at }. Any extra
        // key would merge into user-prefs and clutter the JSON. Any
        // missing key would defeat the cold-start cache fallback purpose.
        let resp = crate::alfred_api_client::LicenseActivationResponse {
            ok: true,
            tier: "paid".to_string(),
            instance_id: Some("inst-123".to_string()),
            expires_at: Some("2027-05-18T22:00:00Z".to_string()),
            validated_at: 1_777_000_000,
        };
        let patch = build_license_prefs_patch("ABCD-1234", &resp);
        let obj = patch.as_object().expect("patch must be a JSON object");
        assert_eq!(obj.get("tier").and_then(|v| v.as_str()), Some("paid"));
        assert_eq!(obj.get("license_key").and_then(|v| v.as_str()), Some("ABCD-1234"));
        let iso = obj.get("license_validated_at").and_then(|v| v.as_str()).unwrap();
        assert!(iso.starts_with("2026-"), "ISO 8601 starts with year, got: {iso}");
        // No other keys leak — the patch is merge-only into user-prefs.
        assert_eq!(obj.len(), 3, "patch must contain exactly 3 keys, got: {obj:?}");
    }

    #[test]
    fn build_license_prefs_patch_handles_zero_validated_at() {
        // Edge case: a misconfigured server returning validated_at=0
        // (e.g. fresh deploy where the field wasn't populated). Falling
        // back to "" rather than dropping the key keeps the patch shape
        // stable for the merge-only writer.
        let resp = crate::alfred_api_client::LicenseActivationResponse {
            ok: true,
            tier: "paid".to_string(),
            instance_id: None,
            expires_at: None,
            validated_at: 0,
        };
        let patch = build_license_prefs_patch("KEY-NO-TS", &resp);
        let iso = patch
            .get("license_validated_at")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(iso, "", "validated_at=0 maps to empty string sentinel");
    }

    #[test]
    fn build_redeem_prefs_patch_converts_expiry_to_iso() {
        // MON-C: a /redeem response {tier:paid, expires_at:<epoch>} maps to
        // {tier, license_expires_at:<ISO8601>} for the cold-start cache.
        let resp = serde_json::json!({
            "ok": true,
            "tier": "paid",
            "expires_at": 1_800_000_000u64
        });
        let patch = build_redeem_prefs_patch(&resp);
        let obj = patch.as_object().unwrap();
        assert_eq!(obj.get("tier").and_then(|v| v.as_str()), Some("paid"));
        let iso = obj.get("license_expires_at").and_then(|v| v.as_str()).unwrap();
        assert!(iso.starts_with("20"), "ISO 8601 timestamp, got: {iso}");
        assert_eq!(obj.len(), 2, "patch is merge-only minimal, got: {obj:?}");
    }

    #[test]
    fn build_redeem_prefs_patch_defaults_tier_and_empty_iso() {
        // Defensive: a malformed/partial response (no tier, no expires_at)
        // still yields a stable patch shape — tier defaults to "paid"
        // (we only persist on a successful redeem) and the ISO is empty.
        let patch = build_redeem_prefs_patch(&serde_json::json!({}));
        assert_eq!(patch.get("tier").and_then(|v| v.as_str()), Some("paid"));
        assert_eq!(
            patch.get("license_expires_at").and_then(|v| v.as_str()),
            Some("")
        );
    }

    #[test]
    fn build_license_prefs_patch_preserves_original_license_key() {
        // The license_key field in the patch is the RAW key sent to
        // activate_license, not anything derived from the server response.
        // This matters because the server doesn't echo the key back (it
        // would be redundant) — the desktop must remember what it sent.
        let resp = crate::alfred_api_client::LicenseActivationResponse {
            ok: true,
            tier: "paid".to_string(),
            instance_id: None,
            expires_at: None,
            validated_at: 1_777_000_000,
        };
        let patch = build_license_prefs_patch("XYZ-VERY-LONG-KEY-9999", &resp);
        assert_eq!(
            patch.get("license_key").and_then(|v| v.as_str()),
            Some("XYZ-VERY-LONG-KEY-9999"),
        );
    }

    #[test]
    fn run_license_checkout_url_returns_not_configured_when_unset() {
        // Public-repo default: no compile-time URL, no runtime override.
        // The desktop must receive `configured: false` so the JS layer
        // shows the "temporarily unavailable" banner instead of opening
        // a bogus checkout.
        //
        // Note: this test runs against the compile-time
        // option_env!("ALFRED_LS_CHECKOUT_URL") which IS None for the
        // public repo's CI build. If Pierre rebuilds with the env var
        // set, this test will fail at his build time — expected, and
        // a clear signal to add his URL to a .env or build.rs override
        // for `cargo test` runs.
        std::env::remove_var("ALFRED_LS_CHECKOUT_URL");
        let payload = run_license_checkout_url().unwrap();
        let result = payload.get("result").unwrap();
        let configured = result.get("configured").unwrap().as_bool().unwrap();
        let url = result.get("url").unwrap();
        // The public-repo build has no compile-time URL → both must
        // reflect "not configured".
        assert!(
            !configured,
            "checkout URL should be unconfigured in the public-repo build",
        );
        assert!(
            url.is_null(),
            "url must be null when not configured, got: {url:?}",
        );
    }

    #[test]
    fn run_license_checkout_url_uses_runtime_env_when_compile_time_absent() {
        // When Pierre runs `ALFRED_LS_CHECKOUT_URL=https://...
        // cargo run` locally without recompiling, the runtime env
        // fallback kicks in. Verify the path is wired.
        //
        // Skipped if a compile-time URL is baked in (Pierre's private
        // build), because compile-time wins and the test premise
        // doesn't hold.
        if option_env!("ALFRED_LS_CHECKOUT_URL").is_some() {
            return;
        }
        std::env::set_var(
            "ALFRED_LS_CHECKOUT_URL",
            "https://example.lemonsqueezy.com/buy/test-product",
        );
        let payload = run_license_checkout_url().unwrap();
        let result = payload.get("result").unwrap();
        assert!(result.get("configured").unwrap().as_bool().unwrap());
        assert_eq!(
            result.get("url").unwrap().as_str().unwrap(),
            "https://example.lemonsqueezy.com/buy/test-product",
        );
        std::env::remove_var("ALFRED_LS_CHECKOUT_URL");
    }
}
