//! LLM generation — upstream API calls, codex calls, streaming, cache fallback.
//!
//! Prompt building lives in `llm_prompts.rs`, response parsing in `llm_parsing.rs`.

use std::env;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::llm_parsing::{
    extract_draft_from_response, extract_recommendation_from_response,
    extract_watchlist_from_response, is_repair_pass,
    persist_deep_news_if_present,
    persist_extracted_fundamentals_if_present, persist_shared_insights_if_present,
};
use crate::llm_prompts::{build_line_analysis_prompt, build_repair_prompt, build_report_prompt, build_watchlist_prompt};
use crate::paths::{resolve_report_history_dir, resolve_reports_dir};
use crate::storage::read_json_file;

// ── Mode resolution ────────────────────────────────────────────────

fn resolve_generation_mode() -> String {
    env::var("LITELLM_GENERATION_MODE")
        .unwrap_or_else(|_| {
            if env::var("ALFRED_LLM_TOKEN")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            {
                "live".to_string()
            } else {
                "codex_proxy".to_string()
            }
        })
        .trim()
        .to_lowercase()
}

fn resolve_model() -> String {
    env::var("ALFRED_MODEL").unwrap_or_else(|_| "gpt-5.4".to_string())
}

fn resolve_upstream_base_url() -> String {
    env::var("LITELLM_UPSTREAM_BASE_URL")
        .or_else(|_| env::var("ALFRED_LITELLM_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4401".to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}

fn resolve_chat_path() -> String {
    env::var("LITELLM_UPSTREAM_CHAT_PATH")
        .unwrap_or_else(|_| "/chat/completions".to_string())
}

// ── Report draft generation ────────────────────────────────────────

pub fn generate_report_draft(run_state: &Value, run_id: &str) -> Result<Value> {
    let mode = resolve_generation_mode();
    match mode.as_str() {
        "live" => generate_report_live(run_state, run_id),
        "codex_proxy" => generate_report_codex(run_state, run_id),
        "mock_cache" => generate_report_from_cache(),
        "mock_past" => generate_report_from_history(),
        _ => generate_report_codex(run_state, run_id),
    }
}

pub fn generate_line_analysis(
    line_context: &Value,
    run_state: &Value,
    agent_guidelines: Option<&str>,
    validation_context: Option<&Value>,
) -> Result<Value> {
    // Enrich line_context with sector + COT data if not already present
    let enriched_context = enrich_line_context_with_sector(line_context);
    let ctx = &enriched_context;

    let mode = resolve_generation_mode();
    match mode.as_str() {
        "live" => generate_line_live(ctx, run_state, agent_guidelines, validation_context),
        "codex_proxy" => generate_line_codex(ctx, run_state, agent_guidelines, validation_context),
        "mock_cache" | "mock_past" => generate_line_from_cache(ctx),
        _ => generate_line_codex(ctx, run_state, agent_guidelines, validation_context),
    }
}

/// Inject sector_cot into line_context if missing (for JS-originating contexts).
fn enrich_line_context_with_sector(line_context: &Value) -> Value {
    // Skip if already enriched
    if line_context.get("sector_cot").is_some() {
        return line_context.clone();
    }

    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
    let isin = line_context
        .get("row").and_then(|r| r.get("isin")).and_then(|v| v.as_str())
        .or_else(|| line_context.get("isin").and_then(|v| v.as_str()))
        .unwrap_or(ticker);
    let name = line_context.get("nom").and_then(|v| v.as_str()).unwrap_or("");

    if ticker.is_empty() && isin.is_empty() {
        return line_context.clone();
    }

    let sector_resp = crate::enrichment::fetch_sector(ticker, name, isin).ok();
    let sector_slug = sector_resp.as_ref()
        .and_then(|r| r.get("sector").and_then(|v| v.as_str()))
        .unwrap_or("");
    let sector_analysis = sector_resp.as_ref()
        .and_then(|r| r.get("sector_analysis").cloned())
        .unwrap_or(Value::Null);
    let cot_data = if !sector_slug.is_empty() {
        crate::enrichment::fetch_cot(ticker, isin)
            .ok()
            .and_then(|r| r.get("cot").cloned())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    let mut enriched = line_context.clone();
    if let Some(obj) = enriched.as_object_mut() {
        obj.insert("sector".to_string(), serde_json::json!(sector_slug));
        obj.insert("sector_cot".to_string(), serde_json::json!({
            "sector": sector_slug,
            "sector_analysis": sector_analysis,
            "cot": cot_data,
        }));
    }
    enriched
}

fn update_synthesis_progress(run_id: &str, progress: &str) {
    if run_id.is_empty() {
        return;
    }
    let payload = serde_json::json!({ "status": "generating", "progress": progress });
    // Write to run_state_cache for polling fallback
    crate::run_state_cache::cache_line_status(run_id, "__synthesis__", payload.clone());
    // Push to frontend immediately
    crate::emit_event("alfred://synthesis-progress", serde_json::json!({
        "run_id": run_id,
        "progress": progress,
    }));
}

// ── Live mode (upstream LLM API) ───────────────────────────────────

fn generate_report_live(run_state: &Value, run_id: &str) -> Result<Value> {
    let model = resolve_model();
    let prompt = build_report_prompt(run_state);
    let rid = run_id.to_string();
    let progress_fn: Option<Box<dyn Fn(usize, usize) + Send>> = if !rid.is_empty() {
        let rid2 = rid.clone();
        Some(Box::new(move |chunks: usize, bytes: usize| {
            let progress = if bytes < 1024 {
                format!("{bytes}B, {chunks} chunks")
            } else {
                format!("{:.1}kB, {chunks} chunks", bytes as f64 / 1024.0)
            };
            update_synthesis_progress(&rid2, &progress);
        }))
    } else {
        None
    };
    update_synthesis_progress(&rid, "LLM request sent\u{2026}");
    let response = call_upstream_llm_streamed(&model, &prompt, progress_fn)?;
    update_synthesis_progress(&rid, "parsing response\u{2026}");
    let draft = extract_draft_from_response(&response)?;
    Ok(json!({
        "ok": true,
        "model": model,
        "draft": draft,
        "llm_utilise": "litellm"
    }))
}

fn generate_line_live(
    line_context: &Value,
    run_state: &Value,
    agent_guidelines: Option<&str>,
    validation_context: Option<&Value>,
) -> Result<Value> {
    let model = resolve_model();
    let run_id = run_state.get("run_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let prompt = if is_repair_pass(validation_context) {
        build_repair_prompt(line_context, run_state, agent_guidelines, validation_context.unwrap())
    } else {
        build_line_analysis_prompt(line_context, run_state, agent_guidelines)
    };

    let progress_fn: Option<Box<dyn Fn(usize, usize) + Send>> =
        if !run_id.is_empty() && !ticker.is_empty() {
            let rid = run_id.clone();
            let tk = ticker.clone();
            Some(Box::new(move |chunks: usize, bytes: usize| {
                let progress = if bytes < 1024 {
                    format!("{bytes}B, {chunks} chunks")
                } else {
                    format!("{:.1}kB, {chunks} chunks", bytes as f64 / 1024.0)
                };
                let _ = crate::run_state::update_line_status_with_progress(
                    &rid, &tk, "analyzing", &progress,
                );
            }))
        } else {
            None
        };

    let response = call_upstream_llm_streamed(&model, &prompt, progress_fn)?;
    let recommendation = extract_recommendation_from_response(&response)?;

    // Persist extracted data back to API — on both first pass and repair
    // (repair often produces better/more complete fields)
    persist_extracted_fundamentals_if_present(&recommendation, line_context);
    persist_shared_insights_if_present(&recommendation, line_context);
    persist_deep_news_if_present(&recommendation, line_context);

    Ok(json!({
        "ok": true,
        "model": model,
        "recommendation": recommendation
    }))
}

/// Call the upstream LLM with streaming enabled. Reads SSE chunks and
/// reassembles the full content, calling on_progress with (chunk_count, total_bytes).
fn call_upstream_llm_streamed(
    model: &str,
    prompt: &str,
    on_progress: Option<Box<dyn Fn(usize, usize) + Send>>,
) -> Result<Value> {
    let base_url = resolve_upstream_base_url();
    let chat_path = resolve_chat_path();
    let url = format!("{base_url}{chat_path}");
    let timeout_ms: u64 = env::var("LITELLM_GENERATION_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120_000);

    let body = json!({
        "model": model,
        "temperature": 0.2,
        "stream": true,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": "Tu es un conseiller financier qui s'adresse a des investisseurs particuliers non-experts. Tu expliques les choses simplement, avec des exemples concrets et des chiffres. Tu produis uniquement du JSON valide, sans texte supplementaire." },
            { "role": "user", "content": prompt }
        ]
    });

    let mut request = ureq::post(&url)
        .timeout(Duration::from_millis(timeout_ms));

    if let Ok(token) = env::var("ALFRED_LLM_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            request = request.set("Authorization", &format!("Bearer {trimmed}"));
        }
    }

    let response = request
        .send_json(&body)
        .map_err(|e| anyhow!("llm_upstream_request_failed:{e}"))?;

    // Read response — try SSE streaming first, fall back to regular JSON
    let reader = std::io::BufReader::new(response.into_reader());
    let mut full_content = String::new();
    let mut raw_body = String::new();
    let mut chunk_count = 0usize;
    let mut total_bytes = 0usize;
    let mut finish_reason: Option<String> = None;
    let mut found_sse = false;

    use std::io::BufRead;
    for line in reader.lines() {
        let line = line.map_err(|e| anyhow!("llm_stream_read_failed:{e}"))?;
        raw_body.push_str(&line);
        raw_body.push('\n');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("data: ") {
            found_sse = true;
            let data = &trimmed[6..];
            if data == "[DONE]" {
                break;
            }
            if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                if let Some(delta_content) = chunk
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("content"))
                    .and_then(|c| c.as_str())
                {
                    full_content.push_str(delta_content);
                    chunk_count += 1;
                    total_bytes += delta_content.len();
                    if let Some(ref cb) = on_progress {
                        cb(chunk_count, total_bytes);
                    }
                }
                if let Some(fr) = chunk
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|choice| choice.get("finish_reason"))
                    .and_then(|v| v.as_str())
                {
                    finish_reason = Some(fr.to_string());
                }
            }
        }
    }

    // If SSE streaming worked, return the assembled content
    if found_sse && !full_content.is_empty() {
        return Ok(json!({
            "choices": [{
                "message": { "content": full_content },
                "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string())
            }]
        }));
    }

    // Fallback: server returned regular JSON (no SSE) — parse the raw body
    if let Some(ref cb) = on_progress {
        cb(1, raw_body.len());
    }
    let parsed: Value = serde_json::from_str(raw_body.trim())
        .map_err(|e| anyhow!("llm_response_parse_failed:{e}"))?;
    // Already in OpenAI format
    if parsed.get("choices").is_some() {
        return Ok(parsed);
    }
    // Wrapped in an envelope (e.g., codex proxy response)
    if let Some(content) = parsed.get("content").and_then(|v| v.as_str()) {
        return Ok(json!({
            "choices": [{
                "message": { "content": content },
                "finish_reason": "stop"
            }]
        }));
    }
    Err(anyhow!("llm_response_no_content"))
}

// ── Codex mode ─────────────────────────────────────────────────────

fn generate_report_codex(run_state: &Value, run_id: &str) -> Result<Value> {
    let prompt = build_report_prompt(run_state);
    let timeout_ms: u64 = env::var("CODEX_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180_000);
    let rid = run_id.to_string();
    let progress_cb: Option<crate::llm_backend::ProgressFn> = if !rid.is_empty() {
        let rid2 = rid.clone();
        Some(Box::new(move |_bytes, _lines, latest| {
            update_synthesis_progress(&rid2, latest);
        }))
    } else {
        None
    };
    update_synthesis_progress(&rid, "codex process started\u{2026}");
    let result = crate::llm_backend::run_prompt(&prompt, timeout_ms, progress_cb)?;
    update_synthesis_progress(&rid, "parsing response\u{2026}");
    let draft = if result.get("draft").is_some() {
        result.get("draft").unwrap().clone()
    } else {
        result
    };
    Ok(json!({
        "ok": true,
        "model": "codex",
        "draft": draft,
        "llm_utilise": "codex"
    }))
}

fn generate_line_codex(
    line_context: &Value,
    run_state: &Value,
    agent_guidelines: Option<&str>,
    validation_context: Option<&Value>,
) -> Result<Value> {
    let prompt = if is_repair_pass(validation_context) {
        build_repair_prompt(line_context, run_state, agent_guidelines, validation_context.unwrap())
    } else {
        build_line_analysis_prompt(line_context, run_state, agent_guidelines)
    };
    let timeout_ms: u64 = env::var("CODEX_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180_000);

    let run_id = run_state.get("run_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    let status_label = if is_repair_pass(validation_context) { "repairing" } else { "analyzing" };
    let progress_cb: Option<crate::llm_backend::ProgressFn> = if !run_id.is_empty() && !ticker.is_empty() {
        let rid = run_id.clone();
        let tk = ticker.clone();
        let label = status_label.to_string();
        Some(Box::new(move |_bytes, _lines, latest_line| {
            let _ = crate::run_state::update_line_status_with_progress(
                &rid, &tk, &label, latest_line
            );
        }))
    } else {
        None
    };

    let result = crate::llm_backend::run_prompt(&prompt, timeout_ms, progress_cb)?;
    let recommendation = if result.get("recommendation").is_some() {
        result.get("recommendation").unwrap().clone()
    } else {
        result
    };

    // Persist extracted data back to API — on both first pass and repair
    // (repair often produces better/more complete fields)
    persist_extracted_fundamentals_if_present(&recommendation, line_context);
    persist_shared_insights_if_present(&recommendation, line_context);
    persist_deep_news_if_present(&recommendation, line_context);

    Ok(json!({
        "ok": true,
        "model": "codex",
        "recommendation": recommendation
    }))
}

// ── Cache modes ────────────────────────────────────────────────────

fn generate_report_from_cache() -> Result<Value> {
    let latest_path = resolve_reports_dir().join("latest.json");
    if latest_path.exists() {
        let report = read_json_file(&latest_path)?;
        if let Some(payload) = report.get("payload") {
            return Ok(json!({
                "ok": true,
                "model": "cache",
                "draft": payload,
                "llm_utilise": "cache",
                "cache_source": "latest",
                "cache_saved_at": report.get("saved_at")
            }));
        }
    }
    generate_report_from_history()
}

fn generate_report_from_history() -> Result<Value> {
    let history_dir = resolve_report_history_dir();
    if !history_dir.exists() {
        return Err(anyhow!("litellm_mock_cache_empty"));
    }
    let mut entries: Vec<_> = std::fs::read_dir(&history_dir)?
        .flatten()
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    entries.sort_by(|a, b| {
        b.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
            .cmp(
                &a.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
            )
    });
    for entry in entries {
        if let Ok(report) = read_json_file(&entry.path()) {
            if let Some(payload) = report.get("payload") {
                return Ok(json!({
                    "ok": true,
                    "model": "cache",
                    "draft": payload,
                    "llm_utilise": "cache",
                    "cache_source": "history",
                    "cache_saved_at": report.get("saved_at")
                }));
            }
        }
    }
    Err(anyhow!("litellm_mock_cache_empty"))
}

fn generate_line_from_cache(line_context: &Value) -> Result<Value> {
    let ticker = line_context
        .get("ticker")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    // Try to find a cached recommendation for this ticker
    let latest_path = resolve_reports_dir().join("latest.json");
    if latest_path.exists() {
        if let Ok(report) = read_json_file(&latest_path) {
            if let Some(recs) = report
                .get("payload")
                .and_then(|p| p.get("recommandations"))
                .and_then(|v| v.as_array())
            {
                for rec in recs {
                    let rec_ticker = rec.get("ticker").and_then(|v| v.as_str()).unwrap_or_default();
                    if rec_ticker.eq_ignore_ascii_case(ticker) {
                        return Ok(json!({
                            "ok": true,
                            "model": "cache",
                            "recommendation": rec
                        }));
                    }
                }
            }
        }
    }

    // Fallback: generate a surveillance recommendation
    Ok(json!({
        "ok": true,
        "model": "cache",
        "recommendation": {
            "ticker": ticker,
            "signal": "SURVEILLANCE",
            "conviction": "faible",
            "synthese": "Donnees insuffisantes pour analyse complete.",
            "action_recommandee": "Surveiller"
        }
    }))
}

// ── Watchlist generation ─────────────────────────────────────────

/// Generate watchlist suggestions via LLM based on current portfolio.
pub fn generate_watchlist_suggestions(
    positions: &[Value],
    portfolio: &Value,
    guidelines: &str,
    account: &str,
) -> Result<Vec<Value>> {
    let mode = resolve_generation_mode();
    let prompt = build_watchlist_prompt(positions, portfolio, guidelines, account);
    let timeout_ms: u64 = env::var("CODEX_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);

    let result = match mode.as_str() {
        "live" => {
            let model = resolve_model();
            call_upstream_llm_streamed(&model, &prompt, None)?
        }
        _ => {
            crate::llm_backend::run_prompt(&prompt, timeout_ms, None)?
        }
    };

    // Extract the watchlist array from the response
    let watchlist = extract_watchlist_from_response(&result)?;
    Ok(watchlist)
}

// ── Universal CSV format analysis ─────────────────────────────────

/// Ask the LLM to analyze a CSV format and return a CsvParsingSpec.
/// The spec describes how to deterministically parse the CSV.
pub fn analyze_csv_format(
    headers: &[String],
    sample_rows: &[Vec<String>],
    delimiter: char,
    row_count: usize,
) -> Result<crate::native_collection::CsvParsingSpec> {
    let prompt = crate::llm_prompts::build_universal_csv_parsing_prompt(
        headers, sample_rows, delimiter, row_count,
    );
    let mode = resolve_generation_mode();

    crate::debug_log(&format!(
        "[csv-analyze] requesting LLM CSV analysis for {} headers, mode={mode}",
        headers.len()
    ));

    let response = match mode.as_str() {
        "live" => {
            let model = resolve_model();
            call_upstream_llm_streamed(&model, &prompt, None)?
        }
        _ => {
            let timeout_ms: u64 = env::var("CODEX_PROXY_TIMEOUT_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60_000);
            crate::llm_backend::run_prompt(&prompt, timeout_ms, None)?
        }
    };

    let draft = crate::llm_parsing::extract_draft_from_response(&response)?;
    crate::debug_log(&format!("[csv-analyze] LLM response: {draft}"));

    // Validate required fields
    let format_type = draft.get("format_type").and_then(|v| v.as_str()).unwrap_or_default();
    if format_type != "transaction_history" && format_type != "position_snapshot" {
        return Err(anyhow!("csv_analyze_invalid_format_type:{format_type}"));
    }
    if draft.get("columns").is_none() {
        return Err(anyhow!("csv_analyze_missing_columns"));
    }

    // Validate column indices are within range
    let max_col = headers.len() as i64;
    if let Some(columns) = draft.get("columns").and_then(|v| v.as_object()) {
        for (field, col_val) in columns {
            if let Some(idx) = col_val.get("index").and_then(|v| v.as_i64()) {
                if idx < 0 || idx >= max_col {
                    crate::debug_log(&format!(
                        "[csv-analyze] warning: column '{field}' index {idx} out of range (0..{max_col})"
                    ));
                }
            }
        }
    }

    let spec: crate::native_collection::CsvParsingSpec = serde_json::from_value(draft.clone())
        .map_err(|e| anyhow!("csv_analyze_deserialize_failed:{e}"))?;

    Ok(spec)
}
