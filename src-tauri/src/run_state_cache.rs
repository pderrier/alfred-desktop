//! In-memory run state cache — eliminates disk I/O during parallel analysis.
//!
//! The full run state is held in memory. All reads and mutations go through
//! this cache. A background thread flushes dirty entries to disk every 2s.
//!
//! Usage:
//! - `load(data_dir, run_id)` — load from disk into cache (first access)
//! - `read(run_id)` — read cached state (no disk I/O)
//! - `patch(data_dir, run_id, mutator)` — mutate in memory, mark dirty
//! - `flush_now(run_id)` — force immediate disk write (run completion)
//! - `evict(run_id)` — flush + remove from cache

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::Value;

struct CacheEntry {
    state: Value,
    dirty: bool,
    last_flush: Instant,
    data_dir: PathBuf,
}

struct Cache {
    entries: HashMap<String, CacheEntry>,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
static FLUSH_STARTED: OnceLock<()> = OnceLock::new();

const FLUSH_INTERVAL: Duration = Duration::from_secs(2);

fn cache() -> &'static Mutex<Cache> {
    CACHE.get_or_init(|| {
        Mutex::new(Cache {
            entries: HashMap::new(),
        })
    })
}

fn run_state_path(data_dir: &Path, run_id: &str) -> PathBuf {
    data_dir
        .join("runtime-state")
        .join(format!("{run_id}.json"))
}

/// Start the background flush thread (idempotent).
fn ensure_flush_thread() {
    FLUSH_STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("run-state-flush".into())
            .spawn(|| loop {
                std::thread::sleep(FLUSH_INTERVAL);
                flush_dirty();
            })
            .ok();
    });
}

/// Load a run state into the cache. Returns the cached copy.
pub fn load(data_dir: &Path, run_id: &str) -> Result<Value> {
    ensure_flush_thread();
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = guard.entries.get(run_id) {
        return Ok(entry.state.clone());
    }
    let path = run_state_path(data_dir, run_id);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| anyhow!("run_not_found:{run_id}:{e}"))?;
    let state: Value =
        serde_json::from_str(&raw).map_err(|e| anyhow!("run_state_parse_failed:{run_id}:{e}"))?;
    guard.entries.insert(
        run_id.to_string(),
        CacheEntry {
            state: state.clone(),
            dirty: false,
            last_flush: Instant::now(),
            data_dir: data_dir.to_path_buf(),
        },
    );
    Ok(state)
}

/// Mutate the cached run state. No disk I/O — flushed by background thread.
pub fn patch<F>(data_dir: &Path, run_id: &str, mutator: F) -> Result<Value>
where
    F: FnOnce(&mut Value),
{
    ensure_flush_thread();
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    if !guard.entries.contains_key(run_id) {
        // Load from disk on first access
        let path = run_state_path(data_dir, run_id);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow!("run_not_found:{run_id}:{e}"))?;
        let state: Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow!("run_state_parse_failed:{run_id}:{e}"))?;
        guard.entries.insert(
            run_id.to_string(),
            CacheEntry {
                state,
                dirty: false,
                last_flush: Instant::now(),
                data_dir: data_dir.to_path_buf(),
            },
        );
    }
    let entry = match guard.entries.get_mut(run_id) {
        Some(e) => e,
        None => return Err(anyhow!("cache_entry_missing_after_load:{}", run_id)),
    };
    mutator(&mut entry.state);
    entry.dirty = true;
    Ok(entry.state.clone())
}

/// Flush all dirty entries to disk. Called by background thread.
fn flush_dirty() {
    let to_flush: Vec<(String, PathBuf, String)> = {
        let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
        let mut batch = Vec::new();
        for (run_id, entry) in guard.entries.iter_mut() {
            if entry.dirty {
                if let Ok(s) = serde_json::to_string_pretty(&entry.state) {
                    batch.push((run_id.clone(), entry.data_dir.clone(), s));
                }
                entry.dirty = false;
                entry.last_flush = Instant::now();
            }
        }
        batch
    };
    // Write outside the lock — no contention with tool threads
    for (run_id, data_dir, serialized) in &to_flush {
        let path = run_state_path(data_dir, run_id);
        if let Err(e) = std::fs::write(&path, serialized) {
            crate::debug_log(&format!("run_state_cache: flush failed for {run_id}: {e}"));
        }
        // Update run index
        if let Ok(state) = serde_json::from_str::<Value>(serialized) {
            crate::run_index::upsert(run_id, &crate::run_index::summary_from_run_state(&state));
        }
    }
}

/// Force flush a specific run to disk immediately.
pub fn flush_now(run_id: &str) {
    let to_write = {
        let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
        guard.entries.get_mut(run_id).and_then(|entry| {
            entry.dirty = false;
            entry.last_flush = Instant::now();
            serde_json::to_string_pretty(&entry.state)
                .ok()
                .map(|s| (entry.data_dir.clone(), s))
        })
    };
    if let Some((data_dir, serialized)) = to_write {
        let path = run_state_path(&data_dir, run_id);
        if let Err(e) = std::fs::write(&path, &serialized) {
            crate::debug_log(&format!("run_state_cache: flush_now failed for {run_id}: {e}"));
        }
        if let Ok(state) = serde_json::from_str::<Value>(&serialized) {
            crate::run_index::upsert(run_id, &crate::run_index::summary_from_run_state(&state));
        }
    }
}

/// Evict a run from cache. Flushes if dirty.
pub fn evict(run_id: &str) {
    flush_now(run_id);
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    guard.entries.remove(run_id);
}

/// Drop a run from the cache WITHOUT flushing to disk.
///
/// Use this when an authoritative write has already happened directly to the
/// disk file from outside this cache (e.g. the separate MCP-server process'
/// `finalize_report` wrote the completed report + validation_corrections +
/// completed orchestration). In that case the resident cache entry is strictly
/// staler than disk, and `evict` (which flushes first) would clobber the disk
/// state with the stale snapshot — the exact bug this avoids. After dropping,
/// the next `load`/`load_run_by_id` reads the authoritative on-disk state.
pub fn drop_without_flush(run_id: &str) {
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    guard.entries.remove(run_id);
}

// ── Legacy compatibility (used by line_status updates from non-MCP code) ──

/// Cache a line status update (same as patch but specialized for line_status).
pub fn cache_line_status(run_id: &str, ticker: &str, value: Value) {
    // Best-effort: if cache has the run, update in memory
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = guard.entries.get_mut(run_id) {
        if let Some(obj) = entry.state.as_object_mut() {
            let ls = obj
                .entry("line_status")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(ls_obj) = ls.as_object_mut() {
                ls_obj.insert(ticker.to_string(), value);
            }
            entry.dirty = true;
        }
    }
    // If not cached, the old flush_to_disk path handles it
}

/// Read cached line_status for a run (for polling).
pub fn read_cached_line_status(run_id: &str) -> Option<Value> {
    let guard = cache().lock().ok()?;
    guard
        .entries
        .get(run_id)
        .and_then(|e| e.state.get("line_status").cloned())
}

/// Flush pending line status updates (legacy path — delegates to flush_dirty).
pub fn flush_to_disk() {
    flush_dirty();
}

/// Remove cached data for a run (legacy alias for evict).
pub fn clear_run(run_id: &str) {
    evict(run_id);
}

/// Check if dirty entries need flushing (legacy compat).
pub fn should_flush() -> bool {
    if let Ok(guard) = cache().lock() {
        guard.entries.values().any(|e| e.dirty)
    } else {
        false
    }
}

/// Clear all in-memory cache entries. For test isolation only — do not call in production.
#[cfg(test)]
pub fn reset_cache() {
    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    guard.entries.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unique_run_id() -> String {
        format!(
            "test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )
    }

    /// `drop_without_flush` must NOT write the stale cache snapshot over an
    /// authoritative direct-to-disk write made after the snapshot. This is the
    /// production clobber: the MCP-server process writes the finalized report to
    /// disk, then the desktop process must discard its (older) cache entry
    /// without flushing. Contrast with `evict`, which flushes first.
    #[test]
    fn drop_without_flush_preserves_direct_disk_write() {
        let dir = std::env::temp_dir().join(unique_run_id());
        std::fs::create_dir_all(dir.join("runtime-state")).unwrap();
        let run_id = unique_run_id();
        let path = run_state_path(&dir, &run_id);

        // Seed disk + load into cache (status: running).
        std::fs::write(&path, json!({ "status": "running" }).to_string()).unwrap();
        load(&dir, &run_id).unwrap();
        // Mutate in cache → dirty (the stale snapshot).
        patch(&dir, &run_id, |s| {
            s.as_object_mut().unwrap().insert("stale".into(), json!(true));
        })
        .unwrap();

        // Authoritative direct-to-disk write (simulates the finalize compose).
        std::fs::write(&path, json!({ "status": "completed" }).to_string()).unwrap();

        // Drop without flush → disk must keep the completed write.
        drop_without_flush(&run_id);
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.get("status").and_then(|v| v.as_str()),
            Some("completed"),
            "drop_without_flush must not clobber the direct-to-disk write"
        );
        assert!(
            on_disk.get("stale").is_none(),
            "the stale cache snapshot must not have been flushed"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sanity contrast: `evict` DOES flush the dirty cache (and thus would
    /// clobber a later direct-disk write — which is exactly why finalize uses
    /// `drop_without_flush` instead).
    #[test]
    fn evict_flushes_dirty_cache_to_disk() {
        let dir = std::env::temp_dir().join(unique_run_id());
        std::fs::create_dir_all(dir.join("runtime-state")).unwrap();
        let run_id = unique_run_id();
        let path = run_state_path(&dir, &run_id);

        std::fs::write(&path, json!({ "status": "running" }).to_string()).unwrap();
        load(&dir, &run_id).unwrap();
        patch(&dir, &run_id, |s| {
            s.as_object_mut().unwrap().insert("cached".into(), json!(true));
        })
        .unwrap();

        evict(&run_id);
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.get("cached").and_then(|v| v.as_bool()), Some(true));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

