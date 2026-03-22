//! In-memory run state cache with batched disk flush.
//!
//! Line status updates are the most frequent writes during analysis (6 workers
//! × N lines). Instead of acquiring a global mutex + reading/writing JSON for
//! each update, we accumulate changes in memory and flush to disk periodically.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{json, Value};

/// Pending line-status updates keyed by (run_id, ticker).
struct CacheState {
    /// run_id → (ticker → status_value)
    line_status: HashMap<String, HashMap<String, Value>>,
    /// Last time we flushed to disk.
    last_flush: Instant,
}

static CACHE: OnceLock<Mutex<CacheState>> = OnceLock::new();

const FLUSH_INTERVAL_MS: u64 = 300;

fn cache() -> &'static Mutex<CacheState> {
    CACHE.get_or_init(|| {
        Mutex::new(CacheState {
            line_status: HashMap::new(),
            last_flush: Instant::now(),
        })
    })
}

/// Update a line status in the in-memory cache (no disk I/O).
pub fn cache_line_status(run_id: &str, ticker: &str, value: Value) {
    if let Ok(mut state) = cache().lock() {
        state
            .line_status
            .entry(run_id.to_string())
            .or_default()
            .insert(ticker.to_string(), value);
    }
}

/// Read the cached line_status map for a run (for polling without disk I/O).
pub fn read_cached_line_status(run_id: &str) -> Option<Value> {
    let state = cache().lock().ok()?;
    state
        .line_status
        .get(run_id)
        .map(|map| Value::Object(map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
}

/// Check if enough time has passed to warrant a disk flush.
pub fn should_flush() -> bool {
    if let Ok(state) = cache().lock() {
        let has_data = !state.line_status.is_empty();
        let elapsed = state.last_flush.elapsed().as_millis() as u64;
        has_data && elapsed >= FLUSH_INTERVAL_MS
    } else {
        false
    }
}

/// Drain all pending updates and return them for flushing to disk.
/// Returns (run_id → line_status_map, run_id → orchestration).
pub fn drain_pending() -> HashMap<String, HashMap<String, Value>> {
    if let Ok(mut state) = cache().lock() {
        state.last_flush = Instant::now();
        std::mem::take(&mut state.line_status)
    } else {
        HashMap::new()
    }
}

/// Flush all pending cache entries to disk in a single write per run.
/// Called from the analysis worker thread periodically.
pub fn flush_to_disk() {
    let line_status_updates = drain_pending();
    if line_status_updates.is_empty() {
        return;
    }

    for (run_id, status_updates) in &line_status_updates {
        let _ = crate::run_state::patch_run_state_with(run_id, |run_state| {
            if let Some(object) = run_state.as_object_mut() {
                let mut line_status = object
                    .get("line_status")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                for (ticker, value) in status_updates {
                    line_status.insert(ticker.clone(), value.clone());
                }
                object.insert(
                    "line_status".to_string(),
                    Value::Object(line_status),
                );
                object.insert(
                    "updated_at".to_string(),
                    json!(crate::helpers::now_iso_string()),
                );
            }
        });
    }
}

/// Remove all cached data for a run (call after run completes).
pub fn clear_run(run_id: &str) {
    if let Ok(mut state) = cache().lock() {
        state.line_status.remove(run_id);
    }
}
