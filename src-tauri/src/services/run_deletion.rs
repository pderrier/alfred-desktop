//! Run deletion / ban — P1-82.
//!
//! When an analysis run produces poisoned data (bad CSV mapping, garbage
//! prices), it contaminates future analyses through two channels Pierre
//! flagged as the real targets:
//!   1. accumulated `signal_history[]` in line-memory (run_id-tagged), and
//!   2. the LLM synthesis fed forward via narrative / run_history.
//!
//! This module deletes such a run in two layers:
//!
//! **Layer 1 — ban (guaranteed):** the run_id is added to a persistent
//! `banned-runs.json` set. Every line-memory reader skips banned
//! `signal_history` entries through the single shared funnel
//! `native_mcp_analysis::filter_signal_history_by_banned_runs` (applied in
//! `build_memory_for_prompt` for native/oauth and `get_line_data` for
//! codex). Even if Layer 2's surgical recompute is imperfect, a banned run
//! never feeds the next prompt.
//!
//! **Layer 2 — surgical cleanup (best-effort, the real purge):** physically
//! removes the run's `signal_history` entries from `line-memory.json`,
//! recomputes each touched entry's derived fields from the new head (reusing
//! `recompute_entry_derived_from_history`), best-effort prunes `run_history`
//! by created-at date-match (run_history is not run_id-tagged), and deletes
//! the run's state file + MCP sidecars + run-index entry.
//!
//! `memory_narrative` cannot be cleanly un-written (it is cumulative prose
//! the LLM regenerates each run from prior context). It is left intact and
//! the next run reconstructs it from the now-purged context — the poison
//! drains. The summary surfaces `residual_narrative_warning: true` whenever
//! a touched entry still carries a non-empty narrative so the UI can be
//! honest about the best-effort boundary.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::native_mcp_analysis::{
    filter_signal_history_by_banned_runs, line_memory_flush_now, line_memory_patch,
    recompute_entry_derived_from_history,
};
use crate::paths::resolve_runtime_state_dir;

const BANNED_RUNS_FILE: &str = "banned-runs.json";

fn banned_runs_path() -> PathBuf {
    resolve_runtime_state_dir().join(BANNED_RUNS_FILE)
}

/// Load the persisted set of banned run_ids. Missing / malformed file → empty
/// set (a ban file that can't be read must never crash an analysis run).
pub fn load_banned_run_ids() -> HashSet<String> {
    let path = banned_runs_path();
    parse_banned_run_ids(
        &fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(|| json!({})),
    )
}

/// Pure parse of the banned-runs document → set. Tolerates a missing key,
/// non-array values, and non-string / blank entries.
pub(crate) fn parse_banned_run_ids(doc: &Value) -> HashSet<String> {
    doc.get("banned_run_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Persist a banned run_id (idempotent). Returns true when it was newly added.
fn ban_run_id(run_id: &str) -> Result<bool> {
    let mut set = load_banned_run_ids();
    let newly = set.insert(run_id.to_string());
    if newly {
        let mut sorted: Vec<&String> = set.iter().collect();
        sorted.sort();
        let doc = json!({ "banned_run_ids": sorted });
        crate::storage::write_json_file(&banned_runs_path(), &doc)?;
    }
    Ok(newly)
}

/// Counters surfaced to the UI after a deletion. Separates the guaranteed
/// ban from the best-effort surgical cleanup so the operator sees exactly
/// what happened.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RunDeletionOutcome {
    pub banned: bool,
    pub signal_entries_purged: usize,
    pub tickers_affected: usize,
    pub tickers_reset: usize,
    pub run_history_entries_pruned: usize,
    pub residual_narrative_warning: bool,
    pub files_removed: usize,
    pub index_entry_removed: bool,
}

impl RunDeletionOutcome {
    pub fn to_summary(&self) -> Value {
        json!({
            "deleted": true,
            "banned": self.banned,
            "signal_entries_purged": self.signal_entries_purged,
            "tickers_affected": self.tickers_affected,
            "tickers_reset": self.tickers_reset,
            "run_history_entries_pruned": self.run_history_entries_pruned,
            "residual_narrative_warning": self.residual_narrative_warning,
            "files_removed": self.files_removed,
            "index_entry_removed": self.index_entry_removed,
        })
    }
}

/// Pure Layer-2 transform on the line-memory store. Purges the deleted run's
/// `signal_history` entries from every `by_ticker` entry, recomputes derived
/// fields from the surviving head, and best-effort prunes `run_history` by
/// the run's created-at date (run_history is not run_id-tagged).
///
/// `run_created_date` is the `YYYY-MM-DD` prefix of the deleted run's
/// `created_at` (empty string = skip run_history pruning). The mutation is
/// driven by a single-element banned set so the same funnel
/// (`filter_signal_history_by_banned_runs`) the read-time safety net uses is
/// reused here — no duplicate filter logic.
///
/// An entry whose `signal_history` becomes empty after the purge is reset to
/// a first-analysis state (history cleared, derived fields neutralised) rather
/// than dropped, so cross-account dedup keys and deep-news caches survive.
pub(crate) fn apply_run_deletion_to_store(
    store: &mut Value,
    run_id: &str,
    run_created_date: &str,
) -> RunDeletionOutcome {
    let mut outcome = RunDeletionOutcome::default();
    let banned: HashSet<String> = std::iter::once(run_id.to_string()).collect();

    let by_ticker = match store.get_mut("by_ticker").and_then(|v| v.as_object_mut()) {
        Some(m) => m,
        None => return outcome,
    };

    for (_, entry) in by_ticker.iter_mut() {
        let history: Vec<Value> = entry
            .get("signal_history")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let (filtered, removed) = filter_signal_history_by_banned_runs(&history, &banned);

        // Best-effort run_history prune by created-at date prefix. Tracked
        // independently of signal_history so an entry with only a run_history
        // hit still counts as affected.
        let run_history_pruned = prune_run_history_by_date(entry, run_created_date);

        if removed == 0 && run_history_pruned == 0 {
            continue;
        }

        outcome.signal_entries_purged += removed;
        outcome.run_history_entries_pruned += run_history_pruned;
        outcome.tickers_affected += 1;

        if removed > 0 {
            if filtered.is_empty() {
                reset_entry_to_first_analysis(entry);
                outcome.tickers_reset += 1;
            } else {
                recompute_entry_derived_from_history(entry, &filtered);
            }
        }

        // Honest best-effort flag: the cumulative narrative can't be
        // un-written, so warn if a touched entry still carries one.
        let narrative_present = entry
            .get("memory_narrative")
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if narrative_present {
            outcome.residual_narrative_warning = true;
        }
    }

    outcome
}

/// Reset a `by_ticker` entry whose entire history belonged to the deleted
/// run. Keeps identity + deep-news caches; clears the signal/accuracy state
/// so the next run treats the ticker as a first analysis. Reuses the shared
/// `reset_entry_signal_state` and additionally drops the persisted
/// `run_history` + `run_id_last_update` (no surviving run to anchor them).
fn reset_entry_to_first_analysis(entry: &mut Value) {
    crate::native_mcp_analysis::reset_entry_signal_state(entry);
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("run_history".to_string(), json!([]));
        obj.remove("run_id_last_update");
    }
}

/// Best-effort prune of `run_history` entries whose `date` falls on the
/// deleted run's created-at day. run_history is NOT run_id-tagged, so this is
/// an imprecise date-match (documented limitation): a different run that
/// completed the same UTC day on the same ticker would also be pruned. Empty
/// `run_created_date` → no-op. Returns the number of entries removed.
fn prune_run_history_by_date(entry: &mut Value, run_created_date: &str) -> usize {
    if run_created_date.is_empty() {
        return 0;
    }
    let obj = match entry.as_object_mut() {
        Some(o) => o,
        None => return 0,
    };
    let history = match obj.get("run_history").and_then(|v| v.as_array()) {
        Some(arr) => arr.clone(),
        None => return 0,
    };
    let before = history.len();
    let kept: Vec<Value> = history
        .into_iter()
        .filter(|item| {
            let item_date = item.get("date").and_then(|v| v.as_str()).unwrap_or("");
            // `date` may be a full RFC3339 timestamp; match on the day prefix.
            !item_date.starts_with(run_created_date)
        })
        .collect();
    let removed = before - kept.len();
    if removed > 0 {
        obj.insert("run_history".to_string(), json!(kept));
    }
    removed
}

/// Delete the run's state file + MCP sidecars from disk. Returns the number
/// of files actually removed. Missing files are not errors (a run can be
/// banned after its files were already pruned).
fn remove_run_files(run_id: &str) -> usize {
    let state_dir = resolve_runtime_state_dir();
    let mut removed = 0usize;
    let candidates = [
        format!("{run_id}.json"),
        format!("{run_id}_mcp_results.jsonl"),
        format!("{run_id}_mcp_results.merging"),
        format!("{run_id}_mcp_progress.jsonl"),
    ];
    for name in candidates {
        let path = state_dir.join(&name);
        if path.exists() && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Read the deleted run's `created_at` day prefix (`YYYY-MM-DD`) from its
/// state file, falling back to `updated_at`. Empty string when the file is
/// gone / unreadable — run_history pruning is then skipped.
fn read_run_created_date(run_id: &str) -> String {
    let path = resolve_runtime_state_dir().join(format!("{run_id}.json"));
    let doc = match crate::storage::read_json_file(&path) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let ts = doc
        .get("created_at")
        .and_then(|v| v.as_str())
        .or_else(|| doc.get("updated_at").and_then(|v| v.as_str()))
        .unwrap_or("");
    // Day prefix only (ASCII-safe slice of an RFC3339 timestamp).
    ts.get(..10).unwrap_or("").to_string()
}

/// Orchestrate a full run deletion: ban → surgical line-memory purge →
/// file/index removal. Returns the outcome summary. Best-effort throughout —
/// the ban (Layer 1) is the only step that must succeed for the guarantee to
/// hold; a failure there propagates as an error.
pub fn delete_run(run_id: &str) -> Result<RunDeletionOutcome> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err(anyhow::anyhow!("run_id_required"));
    }

    // Read the created-at BEFORE removing files so run_history date-match works.
    let run_created_date = read_run_created_date(run_id);

    // Layer 1 — ban (guaranteed safety net).
    let banned = ban_run_id(run_id)?;

    // Layer 2 — surgical line-memory purge + recompute.
    let mut outcome = RunDeletionOutcome::default();
    line_memory_patch(|store| {
        outcome = apply_run_deletion_to_store(store, run_id, &run_created_date);
    });
    line_memory_flush_now();
    outcome.banned = banned;

    // Evict any cached run state so a stale copy can't be re-flushed after we
    // delete its file on disk.
    crate::run_state_cache::evict(run_id);

    // Files + index.
    outcome.files_removed = remove_run_files(run_id);
    outcome.index_entry_removed = crate::run_index::remove(run_id);

    crate::debug_log(&format!(
        "delete_run {run_id}: banned={} purged={} tickers={} reset={} run_hist_pruned={} files={} index={} residual_narrative={}",
        outcome.banned,
        outcome.signal_entries_purged,
        outcome.tickers_affected,
        outcome.tickers_reset,
        outcome.run_history_entries_pruned,
        outcome.files_removed,
        outcome.index_entry_removed,
        outcome.residual_narrative_warning,
    ));

    Ok(outcome)
}

/// Test-only seam: drive `line_memory_read` → purge → flush without faking
/// the disk-backed run state. Mirrors the production `line_memory_patch`
/// flow but lets tests assert the persisted store directly.
#[cfg(test)]
pub fn apply_run_deletion_for_test(run_id: &str, run_created_date: &str) -> RunDeletionOutcome {
    let mut outcome = RunDeletionOutcome::default();
    line_memory_patch(|store| {
        outcome = apply_run_deletion_to_store(store, run_id, run_created_date);
    });
    line_memory_flush_now();
    let _ = crate::native_mcp_analysis::line_memory_read();
    outcome
}
