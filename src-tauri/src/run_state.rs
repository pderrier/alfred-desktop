use std::{fs, io::Write};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::helpers::{
    new_run_id, now_iso_string, parse_timestamp_millis, read_json_env_override,
    safe_portfolio_source, safe_trimmed_option,
};
use crate::local_http::{parse_http_url, request_http_json};
use crate::paths::{
    resolve_audit_log_path, resolve_control_plane_base_url, resolve_latest_report_path,
    resolve_report_history_dir, resolve_runtime_state_dir,
    resolve_source_snapshot_store_path,
};
use crate::storage::{read_json_file, write_json_file};

fn resolve_run_updated_at(payload: &serde_json::Value, metadata: &fs::Metadata) -> String {
    payload
        .get("updated_at")
        .and_then(|v| v.as_str())
        .or_else(|| {
            payload
                .get("orchestration")
                .and_then(|v| v.get("finished_at"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            payload
                .get("orchestration")
                .and_then(|v| v.get("started_at"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| payload.get("created_at").and_then(|v| v.as_str()))
        .map(|v| v.to_string())
        .or_else(|| {
            metadata
                .modified()
                .ok()
                .and_then(|ts| chrono::DateTime::<chrono::Utc>::from(ts).to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into())
        })
        .unwrap_or_default()
}

pub fn build_run_summary(payload: &serde_json::Value, metadata: Option<&fs::Metadata>) -> Option<serde_json::Value> {
    let run_id = payload.get("run_id").and_then(|v| v.as_str())?;
    let orchestration = payload
        .get("orchestration")
        .and_then(|v| v.as_object());
    let pending_recommendations = payload
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    let collected_positions = payload
        .get("portfolio")
        .and_then(|v| v.get("positions"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    let has_partial_artifacts = pending_recommendations > 0
        || collected_positions > 0
        || payload
            .get("composed_payload")
            .map(|v| v.is_object())
            .unwrap_or(false);
    let updated_at = metadata
        .map(|stats| resolve_run_updated_at(payload, stats))
        .or_else(|| {
            payload
                .get("updated_at")
                .and_then(|v| v.as_str())
                .map(|v| v.to_string())
        })
        .unwrap_or_default();
    Some(json!({
        "run_id": run_id,
        "account": payload.get("account").cloned().unwrap_or(serde_json::Value::Null),
        "status": orchestration
            .and_then(|obj| obj.get("status"))
            .and_then(|v| v.as_str())
            .or_else(|| payload.get("status").and_then(|v| v.as_str()))
            .unwrap_or("unknown"),
        "stage": orchestration
            .and_then(|obj| obj.get("stage"))
            .and_then(|v| v.as_str()),
        "updated_at": updated_at,
        "partial_artifacts_available": has_partial_artifacts,
        "collected_positions_count": collected_positions,
        "pending_recommendations_count": pending_recommendations,
        "collection_progress": orchestration.and_then(|obj| obj.get("collection_progress")).cloned(),
        "line_progress": orchestration.and_then(|obj| obj.get("line_progress")).cloned()
    }))
}

pub fn load_run_by_id(run_id: &str) -> Result<serde_json::Value> {
    let safe_run_id = run_id.trim();
    if safe_run_id.is_empty() {
        return Err(anyhow!("run_id_required"));
    }
    // Read from disk — the MCP server (separate process) writes recommendations
    // directly to the file. An in-memory cache would miss those writes and
    // its background flush would overwrite them with stale data.
    let path = resolve_runtime_state_dir().join(format!("{safe_run_id}.json"));
    if !path.exists() {
        return Err(anyhow!("run_not_found"));
    }
    read_json_file(&path)
}

/// Mark any orphaned "running" runs as "aborted". Called once at app startup.
///
/// Uses the in-memory run index to identify orphans (instant), then only reads
/// the specific run files that need patching — avoids scanning 200+ large JSON files.
pub fn cleanup_orphaned_runs() {
    let state_dir = resolve_runtime_state_dir();
    if !state_dir.exists() || !state_dir.is_dir() {
        return;
    }

    // Use the run index to find which runs are still marked "running".
    let index = crate::run_index::load_index();
    let orphan_ids: Vec<String> = index
        .iter()
        .filter(|entry| {
            let status = entry.get("status").and_then(|v| v.as_str()).unwrap_or("");
            // Catch explicitly "running" AND missing/empty status (never set)
            status == "running" || status.is_empty()
        })
        .filter_map(|entry| entry.get("run_id").and_then(|v| v.as_str()).map(String::from))
        .collect();

    if orphan_ids.is_empty() {
        return;
    }

    crate::debug_log(&format!(
        "[cleanup] patching {} orphaned running runs",
        orphan_ids.len()
    ));

    for run_id in &orphan_ids {
        let path = state_dir.join(format!("{run_id}.json"));
        let mut payload = match read_json_file(&path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(object) = payload.as_object_mut() {
            let orch = object.entry("orchestration").or_insert_with(|| json!({}));
            if let Some(orch_obj) = orch.as_object_mut() {
                // Only patch if not already completed
                let current = orch_obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if current != "completed" && current != "completed_degraded" {
                    orch_obj.insert("status".to_string(), json!("aborted"));
                    orch_obj.insert("error_code".to_string(), json!("run_aborted"));
                    orch_obj.insert("error_message".to_string(), json!("Run was interrupted and did not complete."));
                }
            }
            if let Some(ls) = object.get_mut("line_status").and_then(|v| v.as_object_mut()) {
                let active_statuses = ["collecting", "analyzing", "repairing", "waiting"];
                for (_ticker, val) in ls.iter_mut() {
                    let s = val.as_str()
                        .or_else(|| val.get("status").and_then(|v| v.as_str()))
                        .unwrap_or_default();
                    if active_statuses.contains(&s) {
                        *val = json!("aborted");
                    }
                }
            }
            let _ = write_json_file(&path, &serde_json::Value::Object(object.clone()));
        }
        // Update the index entry too
        crate::run_index::upsert(run_id, &crate::run_index::summary_from_run_state(&payload));
    }
}

/// Load pending_recommandations + line_status from the most recent completed run
/// (excluding current_run_id). Used by refresh_synthesis and retry_failed modes.
pub fn load_previous_run_data(current_run_id: &str) -> (Vec<serde_json::Value>, serde_json::Value) {
    // Find the current run's account to filter by
    let current_account = load_run_by_id(current_run_id).ok()
        .and_then(|r| r.get("account").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();

    let runs = load_run_history(20).unwrap_or_default();
    for run_summary in &runs {
        let rid = run_summary.get("run_id").and_then(|v| v.as_str()).unwrap_or_default();
        if rid == current_run_id || rid.is_empty() { continue; }
        // Filter by same account
        let summary_account = run_summary.get("account").and_then(|v| v.as_str()).unwrap_or("");
        if !current_account.is_empty() && summary_account != current_account { continue; }
        if let Ok(full_run) = load_run_by_id(rid) {
            let recs = full_run.get("pending_recommandations")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if !recs.is_empty() {
                crate::debug_log(&format!(
                    "[load_previous] found {} recs from run {rid} (account={summary_account})",
                    recs.len()
                ));
                let line_status = full_run.get("line_status").cloned().unwrap_or_else(|| json!({}));
                return (recs, line_status);
            }
        }
    }
    crate::debug_log(&format!(
        "[load_previous] no previous run with recs found for account={current_account}"
    ));
    (Vec::new(), json!({}))
}

pub fn load_run_history(limit: usize) -> Result<Vec<serde_json::Value>> {
    // In production: use the lightweight in-memory run index.
    // In tests: skip index (tests use temp dirs, static index doesn't track them).
    #[cfg(not(test))]
    {
        let index = crate::run_index::load_index();
        if !index.is_empty() {
            return Ok(index.into_iter().take(limit).collect());
        }
        crate::debug_log("[run-history] index empty, rebuilding from disk...");
        let _ = crate::run_index::rebuild_from_disk();
        let index = crate::run_index::load_index();
        if !index.is_empty() {
            return Ok(index.into_iter().take(limit).collect());
        }
    }

    // Fallback / test path: scan files directly
    let state_dir = resolve_runtime_state_dir();
    if !state_dir.exists() || !state_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in fs::read_dir(&state_dir)? {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if stem.contains("memory") || stem.contains("settings") || stem.contains("index")
            || stem.contains("cache") || stem.contains("preferences")
        {
            continue;
        }
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let mut runs = Vec::new();
    for (_, path) in candidates.into_iter().take(limit) {
        let payload = match read_json_file(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(run_id) = payload.get("run_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let metadata = fs::metadata(&path).ok();
        let updated_at = metadata.as_ref().map(|m| resolve_run_updated_at(&payload, m))
            .or_else(|| payload.get("updated_at").and_then(|v| v.as_str()).map(|v| v.to_string()))
            .unwrap_or_default();
        runs.push(json!({
            "run_id": run_id,
            "account": payload.get("account").cloned().unwrap_or(serde_json::Value::Null),
            "status": payload.get("orchestration").and_then(|v| v.get("status")).and_then(|v| v.as_str())
                .or_else(|| payload.get("status").and_then(|v| v.as_str()))
                .unwrap_or("unknown"),
            "stage": payload.get("orchestration").and_then(|v| v.get("stage")).and_then(|v| v.as_str()),
            "portfolio_source": payload.get("portfolio_source").and_then(|v| v.as_str()),
            "updated_at": updated_at
        }));
        if runs.len() >= limit {
            break;
        }
    }
    Ok(runs)
}

pub fn load_latest_report() -> Result<serde_json::Value> {
    let path = resolve_latest_report_path();
    if !path.exists() {
        return Err(anyhow!("latest_report_missing"));
    }
    read_json_file(&path)
}

fn compact_historical_run(report: &serde_json::Value) -> Option<serde_json::Value> {
    let run_id = report.get("run_id").and_then(|v| v.as_str())?;
    Some(json!({
        "run_id": run_id,
        "status": "completed",
        "stage": null,
        "portfolio_source": "report_history",
        "updated_at": report.get("saved_at").and_then(|v| v.as_str())
    }))
}

pub fn load_report_history(limit: usize) -> Result<Vec<serde_json::Value>> {
    let history_dir = resolve_report_history_dir();
    if !history_dir.exists() || !history_dir.is_dir() {
        return Err(anyhow!("latest_report_missing"));
    }
    let mut reports: Vec<(i64, String, serde_json::Value)> = Vec::new();
    for entry in fs::read_dir(history_dir)? {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let artifact = match read_json_file(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let filename = entry.file_name().to_string_lossy().to_string();
        let saved_at = artifact.get("saved_at").and_then(|v| v.as_str()).unwrap_or_default();
        reports.push((
            parse_timestamp_millis(Some(saved_at)),
            artifact
                .get("run_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            json!({
                "run_id": artifact.get("run_id").and_then(|v| v.as_str()),
                "saved_at": artifact.get("saved_at").and_then(|v| v.as_str()),
                "history_filename": filename,
                "payload": artifact.get("payload").cloned().unwrap_or(serde_json::Value::Null)
            }),
        ));
    }
    reports.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    Ok(reports.into_iter().take(limit).map(|(_, _, value)| value).collect())
}

fn summarize_latest_report(report: &serde_json::Value) -> serde_json::Value {
    let payload = report
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let repartition = payload
        .get("repartition")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let recommandations_count = payload
        .get("recommandations")
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    json!({
        "valeur_portefeuille": payload.get("valeur_portefeuille").cloned().unwrap_or(serde_json::Value::Null),
        "plus_value_totale": payload.get("plus_value_totale").cloned().unwrap_or(serde_json::Value::Null),
        "liquidites": payload.get("liquidites").cloned().unwrap_or(serde_json::Value::Null),
        "recommandations_count": recommandations_count,
        "allocation_classes": repartition.into_iter().map(|entry| json!({
            "classe": entry.get("classe").cloned().unwrap_or(serde_json::Value::Null),
            "pourcentage": entry.get("pourcentage").cloned().unwrap_or(serde_json::Value::Null)
        })).collect::<Vec<_>>()
    })
}

fn load_latest_finary_snapshot_meta() -> serde_json::Value {
    let path = resolve_source_snapshot_store_path();
    if !path.exists() {
        return json!({
            "available": false,
            "saved_at": serde_json::Value::Null,
            "positions_count": 0
        });
    }
    let store = match read_json_file(&path) {
        Ok(value) => value,
        Err(_) => {
            return json!({
                "available": false,
                "saved_at": serde_json::Value::Null,
                "positions_count": 0
            })
        }
    };
    let entry = store
        .get("latest_by_source")
        .and_then(|v| v.get("finary_local_default"));
    let positions_count = entry
        .and_then(|v| v.get("snapshot"))
        .and_then(|v| v.get("positions"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0);
    // Strip accounts to just name/cash/totals — don't send full position arrays
    let accounts = entry
        .and_then(|v| v.get("snapshot"))
        .and_then(|v| v.get("accounts"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|acct| {
                    let name = acct.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                    if name.is_empty() { return None; }
                    let positions_count = acct.get("positions").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                    Some(json!({
                        "name": name,
                        "cash": acct.get("cash").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "total_value": acct.get("total_value").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "total_gain": acct.get("total_gain").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "positions_count": positions_count
                    }))
                })
                .collect::<Vec<_>>()
        })
        .map(serde_json::Value::Array)
        .unwrap_or_else(|| json!([]));
    // Forward ambiguous_cash_groups so the dashboard can trigger the cash matching wizard
    let ambiguous_cash_groups = entry
        .and_then(|v| v.get("snapshot"))
        .and_then(|v| v.get("ambiguous_cash_groups"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let mut meta = json!({
        "available": entry.is_some(),
        "saved_at": entry.and_then(|v| v.get("saved_at")).cloned().unwrap_or(serde_json::Value::Null),
        "positions_count": positions_count,
        "accounts": accounts
    });
    if let Some(arr) = ambiguous_cash_groups.as_array() {
        if !arr.is_empty() {
            meta.as_object_mut().unwrap().insert(
                "ambiguous_cash_groups".to_string(),
                ambiguous_cash_groups,
            );
        }
    }
    meta
}

fn compact_audit_event(event: &serde_json::Value) -> serde_json::Value {
    let category = event.get("category").and_then(|v| v.as_str()).unwrap_or_default();
    let action = event.get("action").and_then(|v| v.as_str()).unwrap_or_default();
    let fallback_type = if !category.is_empty() && !action.is_empty() {
        Some(format!("{category}.{action}"))
    } else {
        None
    };
    json!({
        "ts": event.get("ts").or_else(|| event.get("timestamp")).or_else(|| event.get("at")).cloned().unwrap_or(serde_json::Value::Null),
        "type": event.get("type").cloned().unwrap_or_else(|| fallback_type.map(serde_json::Value::String).unwrap_or(serde_json::Value::Null)),
        "run_id": event.get("run_id").or_else(|| event.get("runId")).cloned().unwrap_or(serde_json::Value::Null),
        "status": event.get("status").cloned().unwrap_or(serde_json::Value::Null)
    })
}

fn load_recent_audit_events(limit: usize) -> Vec<serde_json::Value> {
    let path = resolve_audit_log_path();
    if !path.exists() {
        return Vec::new();
    }
    let raw = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let mut events = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .collect::<Vec<_>>();
    if events.len() > limit {
        let keep_from = events.len().saturating_sub(limit);
        events = events.split_off(keep_from);
    }
    events.into_iter().rev().map(|event| compact_audit_event(&event)).collect()
}

pub fn append_audit_event(event: &serde_json::Value) -> Result<()> {
    let path = resolve_audit_log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(event)?)?;
    Ok(())
}

pub fn load_dashboard_details(runs_limit: usize) -> Result<serde_json::Value> {
    let runs = load_run_history(runs_limit).unwrap_or_default();
    let report_history = match load_report_history(20) {
        Ok(value) => value,
        Err(error) if error.to_string().contains("latest_report_missing") => Vec::new(),
        Err(error) => return Err(error),
    };
    let latest_run = if let Some(run) = runs.first() {
        let run_id = run
            .get("run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if run_id.is_empty() {
            None
        } else {
            load_run_by_id(&run_id).ok()
        }
    } else {
        None
    };
    let effective_runs = if !runs.is_empty() {
        runs
    } else {
        report_history
            .iter()
            .filter_map(compact_historical_run)
            .take(runs_limit)
            .collect()
    };
    let latest_report = match load_latest_report() {
        Ok(value) => Some(value),
        Err(error) if error.to_string().contains("latest_report_missing") => None,
        Err(error) => return Err(error),
    };
    let latest_run_summary = latest_run
        .as_ref()
        .and_then(|run| {
            let path = resolve_runtime_state_dir().join(format!(
                "{}.json",
                run.get("run_id").and_then(|v| v.as_str()).unwrap_or_default()
            ));
            fs::metadata(path)
                .ok()
                .and_then(|meta| build_run_summary(run, Some(&meta)))
                .or_else(|| build_run_summary(run, None))
        });
    Ok(json!({
        "ok": true,
        "snapshot": {
            "runs": effective_runs,
            "latest_run_summary": latest_run_summary,
            "latest_run": latest_run,
            "latest_report": latest_report,
            "report_history": report_history
        }
    }))
}

pub fn load_dashboard_overview(runs_limit: usize, audit_limit: usize) -> Result<serde_json::Value> {
    let runs = load_run_history(runs_limit).unwrap_or_default();
    let latest_run = match runs.first() {
        Some(run) => run
            .get("run_id")
            .and_then(|v| v.as_str())
            .and_then(|run_id| load_run_by_id(run_id).ok()),
        None => None,
    };
    let report_history = match load_report_history(20) {
        Ok(value) => value,
        Err(error) if error.to_string().contains("latest_report_missing") => Vec::new(),
        Err(error) => return Err(error),
    };
    let effective_runs = if !runs.is_empty() {
        runs
    } else {
        report_history
            .iter()
            .filter_map(compact_historical_run)
            .take(runs_limit)
            .collect()
    };
    let latest_report_summary = match load_latest_report() {
        Ok(value) => Some(summarize_latest_report(&value)),
        Err(error) if error.to_string().contains("latest_report_missing") => None,
        Err(error) => return Err(error),
    };
    Ok(json!({
        "ok": true,
        "snapshot": {
            "runs": effective_runs,
            "latest_run_summary": latest_run.as_ref().and_then(|run| {
                let path = resolve_runtime_state_dir().join(format!("{}.json", run.get("run_id").and_then(|v| v.as_str()).unwrap_or_default()));
                fs::metadata(path).ok().and_then(|meta| build_run_summary(run, Some(&meta))).or_else(|| build_run_summary(run, None))
            }),
            "latest_report_summary": latest_report_summary,
            "latest_finary_snapshot": load_latest_finary_snapshot_meta(),
            "stack_health": crate::health::collect_stack_health(),
            "audit_events": load_recent_audit_events(audit_limit)
        }
    }))
}

pub fn load_dashboard_snapshot(runs_limit: usize, audit_limit: usize) -> Result<serde_json::Value> {
    // Single-pass: load runs from index, then load only the latest run's full state
    let runs = load_run_history(runs_limit).unwrap_or_default();

    let latest_run = runs.first()
        .and_then(|r| r.get("run_id"))
        .and_then(|v| v.as_str())
        .and_then(|rid| load_run_by_id(rid).ok());

    let latest_run_summary = latest_run.as_ref()
        .and_then(|r| build_run_summary(r, None))
        .or_else(|| runs.first().and_then(|r| build_run_summary(r, None)));

    let latest_report = load_latest_report().ok();
    let report_history = load_report_history(20).unwrap_or_default();
    let finary_snapshot = load_latest_finary_snapshot_meta();
    let audit_events = load_recent_audit_events(audit_limit);

    let effective_runs = if !runs.is_empty() {
        runs
    } else {
        report_history.iter()
            .filter_map(compact_historical_run)
            .collect()
    };

    Ok(json!({
        "ok": true,
        "snapshot": {
            "runs": effective_runs,
            "latest_run_summary": latest_run_summary,
            "latest_report_summary": latest_report.as_ref().and_then(|r| {
                Some(json!({
                    "run_id": r.get("run_id"),
                    "saved_at": r.get("saved_at"),
                }))
            }),
            "latest_finary_snapshot": finary_snapshot,
            "stack_health": serde_json::Value::Null,
            "audit_events": audit_events,
            "latest_run": latest_run,
            "latest_report": latest_report,
            "report_history": report_history
        }
    }))
}

pub fn patch_run_state_with<F>(run_id: &str, mutator: F) -> Result<serde_json::Value>
where
    F: FnOnce(&mut serde_json::Value),
{
    // Patch through the in-memory cache so background flush doesn't clobber.
    let data_dir = resolve_runtime_state_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| resolve_runtime_state_dir());
    let run_state = crate::run_state_cache::patch(&data_dir, run_id, mutator)?;
    // Update lightweight run index (non-blocking, best-effort)
    crate::run_index::upsert(run_id, &crate::run_index::summary_from_run_state(&run_state));
    Ok(run_state)
}

pub fn set_native_run_stage(
    run_id: &str,
    stage: &str,
    collection_progress: Option<serde_json::Value>,
    line_progress: Option<serde_json::Value>,
) -> Result<()> {
    // Push stage change to frontend immediately
    crate::emit_event("alfred://run-stage", serde_json::json!({
        "run_id": run_id,
        "stage": stage,
        "collection_progress": collection_progress,
        "line_progress": line_progress,
    }));
    patch_run_state_with(run_id, |run_state| {
        let mut orch = crate::models::RunOrchestration::from_run_state(run_state);
        match stage {
            "completed" => orch.set_completed(),
            "completed_degraded" => {
                orch.set_completed();
                orch.status = "completed_degraded".to_string();
                orch.stage = "completed_degraded".to_string();
                orch.degraded = true;
            }
            "failed" => { orch.status = "failed".to_string(); orch.stage = "failed".to_string(); }
            _ => orch.set_running(stage, collection_progress.clone(), line_progress.clone()),
        }
        orch.apply_to(run_state);
    })?;
    Ok(())
}

pub fn update_line_status(run_id: &str, ticker: &str, status: &str) -> Result<()> {
    update_line_status_with_error(run_id, ticker, status, None)
}

pub fn update_line_status_with_progress(run_id: &str, ticker: &str, status: &str, progress: &str) -> Result<()> {
    let payload = serde_json::json!({ "status": status, "progress": progress });
    // Write to in-memory cache
    crate::run_state_cache::cache_line_status(run_id, ticker, payload.clone());
    // Push to frontend immediately via Tauri event
    crate::emit_event("alfred://line-progress", serde_json::json!({
        "run_id": run_id,
        "ticker": ticker,
        "line_status": payload,
    }));
    Ok(())
}

pub fn update_line_status_with_error(run_id: &str, ticker: &str, status: &str, error: Option<&str>) -> Result<()> {
    let value = if let Some(err_msg) = error {
        json!({ "status": status, "error": err_msg })
    } else {
        json!(status)
    };
    crate::run_state_cache::cache_line_status(run_id, ticker, value.clone());
    // Push to frontend immediately
    crate::emit_event("alfred://line-progress", json!({
        "run_id": run_id,
        "ticker": ticker,
        "line_status": value,
    }));
    Ok(())
}

pub fn initialize_with_control_plane_with<F>(
    options: Option<&serde_json::Value>,
    request_fn: F,
) -> Result<serde_json::Value>
where
    F: Fn(&str, &str, u16, &str, Option<&str>, Option<u64>) -> Result<serde_json::Value>,
{
    let safe_options = options.cloned().unwrap_or_else(|| json!({}));
    let portfolio_source = safe_portfolio_source(safe_options.get("portfolio_source"))?;
    let latest_export = safe_trimmed_option(safe_options.get("latest_export"));
    let agent_guidelines = safe_trimmed_option(safe_options.get("agent_guidelines"));
    let account = safe_trimmed_option(safe_options.get("account"));
    let run_mode = safe_options.get("run_mode")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("full_run");
    let bootstrap = if let Some(payload) = read_json_env_override("ALFRED_CONTROL_PLANE_BOOTSTRAP_JSON")? {
        payload
    } else {
        let base_url = resolve_control_plane_base_url();
        let (host, port, base_path) = parse_http_url(&base_url)?;
        let bootstrap_path = if base_path == "/" {
            "/bootstrap".to_string()
        } else {
            format!("{}{}", base_path.trim_end_matches('/'), "/bootstrap")
        };
        request_fn("GET", &host, port, &bootstrap_path, None, None)
            .map_err(|error| anyhow!("control_plane_bootstrap_failed:{error}"))?
    };
    let llm = if let Some(payload) = read_json_env_override("ALFRED_CONTROL_PLANE_LLM_SESSION_JSON")? {
        payload
    } else {
        let base_url = resolve_control_plane_base_url();
        let (host, port, base_path) = parse_http_url(&base_url)?;
        let llm_session_path = if base_path == "/" {
            "/llm/session".to_string()
        } else {
            format!("{}{}", base_path.trim_end_matches('/'), "/llm/session")
        };
        request_fn("POST", &host, port, &llm_session_path, Some("{}"), None)
            .map_err(|error| anyhow!("control_plane_llm_session_failed:{error}"))?
    };
    let run_id = new_run_id();
    let created_at = now_iso_string();
    let run_state = json!({
        "run_id": run_id,
        "created_at": created_at,
        "updated_at": created_at,
        "portfolio_source": portfolio_source,
        "account": account,
        "latest_export": latest_export,
        "agent_guidelines": agent_guidelines,
        "run_mode": run_mode,
        "market": {},
        "news": {},
        "codex_enrichments": {},
        "pending_recommandations": [],
        "validation_corrections": serde_json::Value::Null,
        "composed_payload": serde_json::Value::Null,
        "composed_payload_updated_at": serde_json::Value::Null,
        "report_artifacts": serde_json::Value::Null,
        "control_plane": {
            "user_id": bootstrap.get("user").and_then(|v| v.get("id")).cloned().unwrap_or(serde_json::Value::Null),
            "device_id": bootstrap.get("device").and_then(|v| v.get("id")).cloned().unwrap_or(serde_json::Value::Null),
            "entitlements": bootstrap.get("entitlements").cloned().unwrap_or_else(|| json!({}))
        },
        "runtime_llm": {
            "provider_base_url": llm.get("provider_base_url").cloned().unwrap_or(serde_json::Value::Null),
            "allowed_models": llm.get("allowed_models").cloned().unwrap_or_else(|| json!([])),
            "expires_at": llm.get("expires_at").cloned().unwrap_or(serde_json::Value::Null)
        }
    });
    let run_path = resolve_runtime_state_dir().join(format!("{run_id}.json"));
    write_json_file(&run_path, &run_state)?;
    let user_id = bootstrap.get("user").and_then(|v| v.get("id")).cloned().unwrap_or(serde_json::Value::Null);
    let device_id = bootstrap.get("device").and_then(|v| v.get("id")).cloned().unwrap_or(serde_json::Value::Null);
    let platform = bootstrap.get("device").and_then(|v| v.get("platform")).cloned().unwrap_or(serde_json::Value::Null);
    let audit_events = [
        json!({
            "ts": created_at,
            "category": "auth",
            "action": "bootstrap_loaded",
            "user_id": user_id,
            "status": "ok",
            "data": { "device_id": device_id }
        }),
        json!({
            "ts": created_at,
            "category": "device",
            "action": "session_initialized",
            "user_id": user_id,
            "status": "ok",
            "data": { "device_id": device_id, "platform": platform }
        }),
        json!({
            "ts": created_at,
            "category": "run",
            "action": "created",
            "user_id": user_id,
            "run_id": run_id,
            "status": "ok",
            "data": { "portfolio_source": portfolio_source }
        }),
    ];
    for event in audit_events {
        let _ = append_audit_event(&event);
    }
    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "bootstrap": bootstrap,
        "llm": llm
    }))
}

fn local_bootstrap_payload() -> serde_json::Value {
    json!({
        "user": { "id": "usr_local_001", "email": "local@example.com", "display_name": "Local User" },
        "device": { "id": "dev_local_001", "platform": std::env::consts::OS },
        "entitlements": { "plan": "dev", "features": ["analysis", "reports", "sources"] }
    })
}

fn local_llm_session_payload() -> serde_json::Value {
    json!({
        "provider_base_url": std::env::var("ALFRED_LITELLM_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:4401".to_string()),
        "allowed_models": ["gpt-5-mini"],
        "expires_at": now_iso_string()
    })
}

pub fn initialize_with_control_plane(
    options: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    initialize_with_control_plane_with(options, |method, host, port, path, body, timeout| {
        // Try HTTP first (if control-plane-api sidecar is running), fall back to local payloads
        match request_http_json(method, host, port, path, body, timeout) {
            Ok(response) => Ok(response),
            Err(_) => {
                if method == "GET" && path.ends_with("/bootstrap") {
                    Ok(local_bootstrap_payload())
                } else if method == "POST" && path.ends_with("/llm/session") {
                    Ok(local_llm_session_payload())
                } else {
                    Ok(json!({}))
                }
            }
        }
    })
}

pub fn discover_running_stage(
    run_id: Option<&String>,
) -> Option<(
    String,
    String,
    String,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
    Option<String>,
    Option<String>,
    Option<serde_json::Value>,
)> {
    let state_dir = resolve_runtime_state_dir();
    if !state_dir.exists() || !state_dir.is_dir() {
        return None;
    }
    let mut candidates: Vec<(
        std::time::SystemTime,
        String,
        String,
        String,
        Option<serde_json::Value>,
        Option<serde_json::Value>,
        Option<String>,
        Option<String>,
        Option<serde_json::Value>,
    )> = Vec::new();
    let entries = fs::read_dir(state_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let payload: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let payload_run_id = payload
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        if run_id.is_some() && payload_run_id.as_ref() != run_id {
            continue;
        }
        let orchestration = payload.get("orchestration").and_then(|v| v.as_object());
        let status = orchestration
            .and_then(|obj| obj.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if status != "running" && status != "completed" && status != "failed" {
            continue;
        }
        let stage = orchestration
            .and_then(|obj| obj.get("stage"))
            .and_then(|v| v.as_str())
            .unwrap_or(status)
            .to_string();
        let collection_progress = orchestration
            .and_then(|obj| obj.get("collection_progress"))
            .cloned();
        // Compute line_progress from actual line_status counts (not stale orchestration field)
        let line_progress = {
            let ls = payload.get("line_status").and_then(|v| v.as_object());
            let cached = crate::run_state_cache::read_cached_line_status(
                &payload_run_id.clone().unwrap_or_default(),
            );
            let cached_obj = cached.as_ref().and_then(|v| v.as_object());

            // Merge disk + cache statuses
            let mut done_count = 0u64;
            let mut total_count = 0u64;
            let count_from = |map: &serde_json::Map<String, serde_json::Value>| -> (u64, u64) {
                let mut done = 0u64;
                let total = map.len() as u64;
                for (_, v) in map {
                    let s = v.as_str()
                        .or_else(|| v.get("status").and_then(|s| s.as_str()))
                        .unwrap_or("");
                    if s == "done" || s == "completed" || s == "failed" || s == "aborted" {
                        done += 1;
                    }
                }
                (done, total)
            };
            if let Some(disk) = ls {
                let (d, t) = count_from(disk);
                done_count = d;
                total_count = t;
            }
            if let Some(cache) = cached_obj {
                // Cache may have more recent statuses
                for (ticker, v) in cache {
                    let s = v.as_str()
                        .or_else(|| v.get("status").and_then(|s| s.as_str()))
                        .unwrap_or("");
                    if s == "done" || s == "completed" || s == "failed" || s == "aborted" {
                        // Only count if not already counted from disk
                        if ls.map(|m| !m.contains_key(ticker) || {
                            let ds = m.get(ticker).and_then(|dv| dv.as_str()
                                .or_else(|| dv.get("status").and_then(|x| x.as_str())))
                                .unwrap_or("");
                            ds != "done" && ds != "completed" && ds != "failed" && ds != "aborted"
                        }).unwrap_or(true) {
                            done_count += 1;
                        }
                    }
                    if ls.map(|m| !m.contains_key(ticker)).unwrap_or(true) {
                        total_count += 1;
                    }
                }
            }
            if total_count > 0 {
                Some(json!({"completed": done_count, "total": total_count}))
            } else {
                orchestration.and_then(|obj| obj.get("line_progress")).cloned()
            }
        };
        let error_code = orchestration
            .and_then(|obj| obj.get("error_code"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        let error_message = orchestration
            .and_then(|obj| obj.get("error_message"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string());
        // Merge disk line_status with in-memory cache for fresh data
        let mut line_status = payload.get("line_status").cloned().unwrap_or_else(|| json!({}));
        let run_id_str = payload_run_id.clone().unwrap_or_default();
        if let Some(cached_ls) = crate::run_state_cache::read_cached_line_status(&run_id_str) {
            if let (Some(disk_map), Some(cache_map)) = (line_status.as_object_mut(), cached_ls.as_object()) {
                for (ticker, value) in cache_map {
                    disk_map.insert(ticker.clone(), value.clone());
                }
            }
        }
        let eff_stage = stage;
        let eff_cp = collection_progress;
        let eff_lp = line_progress;
        let modified = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((
            modified,
            run_id_str,
            status.to_string(),
            eff_stage,
            eff_cp,
            eff_lp,
            error_code,
            error_message,
            Some(line_status),
        ));
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, run_id, status, stage, cp, lp, ec, em, ls)| {
        (run_id, status, stage, cp, lp, ec, em, ls)
    })
}
