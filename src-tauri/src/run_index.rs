//! Run index — in-memory cache of run summaries for instant UI access.
//!
//! Keeps a compact summary per run in memory. Flushed to disk periodically.
//! No need to read 170+ JSON files (2-3MB each) for the sidebar.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use crate::paths::resolve_runtime_state_dir;

static INDEX: std::sync::OnceLock<Mutex<Vec<Value>>> = std::sync::OnceLock::new();
const MAX_ENTRIES: usize = 50;

fn index_path() -> PathBuf {
    resolve_runtime_state_dir().join("run-index.json")
}

fn get_index() -> &'static Mutex<Vec<Value>> {
    INDEX.get_or_init(|| {
        // Load from disk on first access
        let entries = load_from_disk().unwrap_or_default();
        Mutex::new(entries)
    })
}

fn load_from_disk() -> Option<Vec<Value>> {
    let path = index_path();
    if !path.exists() { return None; }
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn flush_to_disk(entries: &[Value]) {
    let path = index_path();
    if let Ok(content) = serde_json::to_string(entries) {
        let _ = fs::write(&path, content);
    }
}

/// Get all run summaries (from memory, instant).
pub fn load_index() -> Vec<Value> {
    get_index().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Update or insert a run summary. Deduplicates by run_id, caps at MAX_ENTRIES.
/// Updates in memory immediately, flushes to disk.
pub fn upsert(run_id: &str, summary: &Value) {
    let mut entries = get_index().lock().unwrap_or_else(|e| e.into_inner());
    entries.retain(|e| e.get("run_id").and_then(|v| v.as_str()) != Some(run_id));
    entries.insert(0, summary.clone());
    entries.truncate(MAX_ENTRIES);
    flush_to_disk(&entries);
}

/// Build a compact summary from a run_state Value.
pub fn summary_from_run_state(run_state: &Value) -> Value {
    let run_id = run_state.get("run_id").and_then(|v| v.as_str()).unwrap_or_default();
    let orch = run_state.get("orchestration").cloned().unwrap_or(json!({}));
    let portfolio = run_state.get("portfolio").cloned().unwrap_or(json!({}));
    let reco_count = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    json!({
        "run_id": run_id,
        "account": run_state.get("account").and_then(|v| v.as_str()).unwrap_or(""),
        "status": orch.get("status").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "stage": orch.get("stage").and_then(|v| v.as_str()),
        "portfolio_source": run_state.get("portfolio_source").and_then(|v| v.as_str()),
        "updated_at": run_state.get("updated_at").or_else(|| orch.get("updated_at"))
            .and_then(|v| v.as_str()).unwrap_or(""),
        "finished_at": orch.get("finished_at").and_then(|v| v.as_str()),
        "positions_count": portfolio.get("positions")
            .and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
        "recommendation_count": reco_count,
        "valeur_totale": portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        "error_code": orch.get("error_code").and_then(|v| v.as_str()),
        "error_message": orch.get("error_message").and_then(|v| v.as_str()),
    })
}

/// Rebuild the index from existing run state files.
/// Called once on startup if index is missing or empty.
pub fn rebuild_from_disk() -> Result<()> {
    let state_dir = resolve_runtime_state_dir();
    if !state_dir.exists() { return Ok(()); }

    let mut candidates: Vec<(std::time::SystemTime, Value)> = Vec::new();

    for entry in fs::read_dir(&state_dir)? {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        if stem.contains("memory") || stem.contains("settings") || stem.contains("index")
            || stem.contains("cache") || stem.contains("preferences")
        { continue; }

        let modified = entry.metadata().ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);

        let run_state: Value = match fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        {
            Some(v) if v.get("run_id").is_some() => v,
            _ => continue,
        };

        candidates.push((modified, summary_from_run_state(&run_state)));
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let summaries: Vec<Value> = candidates.into_iter().take(MAX_ENTRIES).map(|(_, s)| s).collect();

    // Update both in-memory and disk
    {
        let mut entries = get_index().lock().unwrap_or_else(|e| e.into_inner());
        *entries = summaries.clone();
    }
    flush_to_disk(&summaries);

    crate::debug_log(&format!("[run-index] rebuilt with {} entries", summaries.len()));
    Ok(())
}
