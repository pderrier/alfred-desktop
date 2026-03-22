//! Storage cleanup — prune old run state files and clear debug log.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

use crate::paths::resolve_runtime_state_dir;

/// Get storage usage stats.
pub fn get_storage_usage() -> Value {
    let state_dir = resolve_runtime_state_dir();
    let debug_log = state_dir.parent()
        .map(|p| p.join("debug.log"))
        .unwrap_or_else(|| PathBuf::from("debug.log"));

    let mut run_files = 0u32;
    let mut run_bytes = 0u64;
    let mut progress_files = 0u32;
    let mut progress_bytes = 0u64;

    if let Ok(entries) = fs::read_dir(&state_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if name.ends_with("_mcp_progress.jsonl") {
                progress_files += 1;
                progress_bytes += size;
            } else if name.ends_with(".json")
                && !name.contains("memory") && !name.contains("settings")
                && !name.contains("index") && !name.contains("preferences")
            {
                run_files += 1;
                run_bytes += size;
            }
        }
    }

    let log_bytes = fs::metadata(&debug_log).map(|m| m.len()).unwrap_or(0);

    json!({
        "run_files": run_files,
        "run_bytes": run_bytes,
        "run_mb": format!("{:.1}", run_bytes as f64 / 1_048_576.0),
        "progress_files": progress_files,
        "progress_bytes": progress_bytes,
        "log_bytes": log_bytes,
        "log_mb": format!("{:.1}", log_bytes as f64 / 1_048_576.0),
        "total_mb": format!("{:.1}", (run_bytes + progress_bytes + log_bytes) as f64 / 1_048_576.0),
    })
}

/// Prune old run state files, keeping the N most recent.
/// Also removes orphaned MCP progress files.
pub fn prune_old_runs(keep: usize) -> Result<Value> {
    let state_dir = resolve_runtime_state_dir();

    // Collect run state files with modification time
    let mut run_files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&state_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".json")
                && !name.contains("memory") && !name.contains("settings")
                && !name.contains("index") && !name.contains("preferences")
                && !name.contains("cache")
            {
                let modified = entry.metadata().ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::UNIX_EPOCH);
                run_files.push((modified, path));
            }
        }
    }

    // Sort by modification time (newest first)
    run_files.sort_by(|a, b| b.0.cmp(&a.0));

    let total = run_files.len();
    let mut removed = 0u32;
    let mut freed_bytes = 0u64;

    // Remove files beyond the keep limit
    for (_, path) in run_files.iter().skip(keep) {
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if let Ok(()) = fs::remove_file(&path) {
            removed += 1;
            freed_bytes += size;

            // Also remove associated MCP progress file
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let progress_path = state_dir.join(format!("{stem}_mcp_progress.jsonl"));
            if progress_path.exists() {
                let psize = fs::metadata(&progress_path).map(|m| m.len()).unwrap_or(0);
                let _ = fs::remove_file(&progress_path);
                freed_bytes += psize;
            }
        }
    }

    // Rebuild the run index
    let _ = crate::run_index::rebuild_from_disk();

    crate::debug_log(&format!(
        "[cleanup] pruned {removed}/{total} run files, freed {:.1}MB",
        freed_bytes as f64 / 1_048_576.0
    ));

    Ok(json!({
        "ok": true,
        "total_before": total,
        "removed": removed,
        "kept": keep.min(total),
        "freed_mb": format!("{:.1}", freed_bytes as f64 / 1_048_576.0),
    }))
}

/// Clear the debug log file.
pub fn clear_debug_log() -> Result<Value> {
    let state_dir = resolve_runtime_state_dir();
    let debug_log = state_dir.parent()
        .map(|p| p.join("debug.log"))
        .unwrap_or_else(|| PathBuf::from("debug.log"));

    let size = fs::metadata(&debug_log).map(|m| m.len()).unwrap_or(0);
    fs::write(&debug_log, "")?;

    crate::debug_log("[cleanup] debug log cleared");

    Ok(json!({
        "ok": true,
        "freed_mb": format!("{:.1}", size as f64 / 1_048_576.0),
    }))
}
