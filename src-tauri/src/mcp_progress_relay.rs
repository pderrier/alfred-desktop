//! MCP progress relay — reads JSONL events from the MCP server and emits Tauri events.
//!
//! The MCP server runs as a child of Codex (not Tauri), so it cannot emit
//! Tauri events directly. Instead it writes JSONL to a progress file, and
//! this relay thread reads new lines and calls `crate::emit_event()`.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

use crate::models::{LineDoneEvent, LineProgress, LineRecommendationSummary};

/// Start a background thread that relays MCP progress events to Tauri.
/// Returns a handle and a stop flag.
pub fn start_relay(run_id: &str, data_dir: &str) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let progress_path = PathBuf::from(data_dir)
        .join("runtime-state")
        .join(format!("{run_id}_mcp_progress.jsonl"));
    let run_id = run_id.to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    let handle = thread::spawn(move || {
        relay_loop(&progress_path, &run_id, &stop_clone);
    });

    (handle, stop)
}

fn relay_loop(path: &PathBuf, run_id: &str, stop: &AtomicBool) {
    // Wait for the file to appear (MCP server creates it on first event)
    let mut attempts = 0;
    while !path.exists() && attempts < 300 && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(100));
        attempts += 1;
    }
    if !path.exists() {
        return;
    }

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    // Seek to end — we only want new events
    let _ = reader.seek(SeekFrom::End(0));

    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // No new data — wait a bit
                thread::sleep(Duration::from_millis(100));
            }
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<Value>(trimmed) {
                    dispatch_event(run_id, &event);
                    // Check for terminal event
                    if event.get("type").and_then(|v| v.as_str()) == Some("done") {
                        break;
                    }
                }
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn dispatch_event(run_id: &str, event: &Value) {
    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match event_type {
        "line_progress" => {
            let ticker = event.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
            let status = event.get("status").and_then(|v| v.as_str()).unwrap_or("analyzing");
            let progress = event.get("progress").and_then(|v| v.as_str()).unwrap_or("");
            // Update run_state cache for polling fallback
            crate::run_state_cache::cache_line_status(
                run_id,
                ticker,
                json!({"status": status, "progress": progress}),
            );
            // Push to frontend immediately
            crate::emit_event(
                "alfred://line-progress",
                json!({"ticker": ticker, "line_status": {"status": status, "progress": progress}}),
            );
        }
        "line_done" => {
            let ticker = event.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
            let recommendation = event.get("recommendation").cloned().unwrap_or(json!({}));
            let completed = event.get("completed").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let total = event.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            // Update line status to done
            crate::run_state_cache::cache_line_status(
                run_id,
                ticker,
                json!({"status": "done"}),
            );

            // Push line-done event with recommendation summary
            let rec_summary = LineRecommendationSummary::from_json(&recommendation);
            let line_progress = LineProgress::new(completed, total);
            crate::emit_event(
                "alfred://line-done",
                serde_json::to_value(LineDoneEvent {
                    run_id: run_id.to_string(),
                    ticker: ticker.to_string(),
                    recommendation: rec_summary,
                    line_progress,
                })
                .unwrap_or(json!({})),
            );
        }
        "synthesis_progress" => {
            let progress = event.get("progress").and_then(|v| v.as_str()).unwrap_or("");
            crate::run_state_cache::cache_line_status(
                run_id,
                "__synthesis__",
                json!({"status": "generating", "progress": progress}),
            );
            crate::emit_event(
                "alfred://synthesis-progress",
                json!({"run_id": run_id, "progress": progress}),
            );
        }
        "stage" => {
            let stage = event.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            let line_progress = event.get("line_progress").cloned();
            crate::emit_event(
                "alfred://run-stage",
                json!({
                    "stage": stage,
                    "line_progress": line_progress,
                }),
            );
        }
        "done" => {
            // Terminal — relay thread will exit
        }
        _ => {
            // Unknown event type — ignore
        }
    }
}

/// Write a progress event to the JSONL file (called from Codex streaming callback).
pub fn write_progress_event(data_dir: &std::path::Path, run_id: &str, ticker: &str, status: &str, progress: &str) {
    let path = data_dir
        .join("runtime-state")
        .join(format!("{run_id}_mcp_progress.jsonl"));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let event = serde_json::json!({
            "type": "line_progress",
            "ticker": ticker,
            "status": status,
            "progress": progress,
        });
        let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
    }
}

