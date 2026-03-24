//! Run statistics — collected from MCP progress JSONL + Codex notifications.
//! Aggregated at run end and persisted to run_state.

use std::path::PathBuf;

use serde_json::{json, Value};

/// Aggregate statistics from the MCP progress JSONL file.
pub fn aggregate_from_progress_file(data_dir: &str, run_id: &str) -> Value {
    let path = PathBuf::from(data_dir)
        .join("runtime-state")
        .join(format!("{run_id}_mcp_progress.jsonl"));

    if !path.exists() {
        return json!({"error": "no_progress_file"});
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return json!({"error": "read_failed"}),
    };

    let mut lines_analyzed = 0u32;
    let mut validation_passes = 0u32;
    let mut validation_failures = 0u32;
    let mut deep_news_persisted = 0u32;
    let mut deep_news_banned = 0u32;
    let mut fundamentals_persisted = 0u32;
    let mut insights_persisted = 0u32;
    let mut web_searches = 0u32;
    let mut reasoning_steps = 0u32;
    let mut total_progress_events = 0u32;
    let mut tickers_done: Vec<String> = Vec::new();
    let mut signals: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut first_event_ts = String::new();
    let mut last_event_ts = String::new();
    let mut max_total_tokens = 0u64;
    let mut max_input_tokens = 0u64;
    let mut max_output_tokens = 0u64;
    let mut token_updates = 0u32;
    let mut last_rate_limit_pct = 0u64;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let event: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        total_progress_events += 1;

        let ts = event.get("at").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !ts.is_empty() {
            if first_event_ts.is_empty() { first_event_ts = ts.clone(); }
            last_event_ts = ts;
        }

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let status = event.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let progress = event.get("progress").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "line_done" => {
                lines_analyzed += 1;
                validation_passes += 1;
                let ticker = event.get("ticker").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if !ticker.is_empty() { tickers_done.push(ticker); }
                let signal = event.get("recommendation")
                    .and_then(|r| r.get("signal"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                *signals.entry(signal).or_insert(0) += 1;
            }
            "line_progress" => {
                if status == "repairing" {
                    validation_failures += 1;
                }
                // Count tool calls from progress text (Codex + native patterns)
                if progress.contains("persisting fundamentals") || progress.contains("persist_extracted_fundamentals") { fundamentals_persisted += 1; }
                if progress.contains("sharing insights") || progress.contains("persist_shared_insights") { insights_persisted += 1; }
                if progress.contains("caching deep news") || progress.contains("persist_deep_news") { deep_news_persisted += 1; }
                if progress.contains("banning noise") || progress.contains("ban_deep_news") { deep_news_banned += 1; }
                // Count activity from streaming progress
                if progress.contains("searching:") || progress.contains("web search") || progress.contains("searching the web") { web_searches += 1; }
                if progress.contains("analyzing data") || progress.contains("evaluating") || progress.contains("assessing") || progress.contains("thinking:") {
                    reasoning_steps += 1;
                }
            }
            "token_usage" => {
                let total = event.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let input = event.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
                let output = event.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
                // Token usage is cumulative per thread — take the max seen
                if total > max_total_tokens { max_total_tokens = total; }
                if input > max_input_tokens { max_input_tokens = input; }
                if output > max_output_tokens { max_output_tokens = output; }
                token_updates += 1;
            }
            "rate_limit" => {
                let pct = event.get("used_pct").and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                if pct > last_rate_limit_pct { last_rate_limit_pct = pct; }
            }
            _ => {}
        }
    }

    json!({
        "lines_analyzed": lines_analyzed,
        "tickers_done": tickers_done,
        "validation": {
            "passes": validation_passes,
            "failures": validation_failures,
            "retry_rate_pct": if validation_passes + validation_failures > 0 {
                validation_failures * 100 / (validation_passes + validation_failures)
            } else { 0 },
        },
        "collective_memory": {
            "deep_news_persisted": deep_news_persisted,
            "deep_news_banned": deep_news_banned,
            "fundamentals_persisted": fundamentals_persisted,
            "insights_persisted": insights_persisted,
        },
        "codex_activity": {
            "web_searches": web_searches,
            "reasoning_steps": reasoning_steps,
            "total_progress_events": total_progress_events,
        },
        "token_usage": {
            "total_tokens": max_total_tokens,
            "input_tokens": max_input_tokens,
            "output_tokens": max_output_tokens,
            "token_updates": token_updates,
            "rate_limit_pct": last_rate_limit_pct,
        },
        "signals": signals,
        "timing": {
            "first_event": first_event_ts,
            "last_event": last_event_ts,
        },
    })
}

/// Persist stats to run_state. Called at run end (success or failure).
pub fn persist_to_run_state(run_id: &str, stats: &Value) {
    let _ = crate::patch_run_state_direct_with(run_id, |rs| {
        if let Some(obj) = rs.as_object_mut() {
            obj.insert("run_statistics".to_string(), stats.clone());
        }
    });
}

/// Collect and persist all stats for a run. Call from analysis_ops on finalization.
pub fn collect_and_persist(run_id: &str, data_dir: &str) {
    let stats = aggregate_from_progress_file(data_dir, run_id);
    crate::debug_log(&format!(
        "[run-stats] {}: lines={} validation={}/{}({}% retry) deep_news=+{} banned=+{} web={}",
        run_id,
        stats.get("lines_analyzed").and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("validation").and_then(|v| v.get("passes")).and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("validation").and_then(|v| v.get("failures")).and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("validation").and_then(|v| v.get("retry_rate_pct")).and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("collective_memory").and_then(|v| v.get("deep_news_persisted")).and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("collective_memory").and_then(|v| v.get("deep_news_banned")).and_then(|v| v.as_u64()).unwrap_or(0),
        stats.get("codex_activity").and_then(|v| v.get("web_searches")).and_then(|v| v.as_u64()).unwrap_or(0),
    ));
    persist_to_run_state(run_id, &stats);
}
