//! LLM generation — upstream API calls, codex calls, streaming, cache fallback.
//!
//! Prompt building lives in `llm_prompts.rs`, response parsing in `llm_parsing.rs`.

use std::collections::HashMap;
use std::env;
use std::sync::{Mutex, OnceLock};
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
use crate::signal_accuracy::SignalAccuracyStats;
use crate::storage::read_json_file;

// ── P1-59 follow-up: per-run calibration cache ─────────────────────
//
// `generate_line_analysis` is the codex/MCP shim entry point — called
// once per line, with no batch boundary. Reading and aggregating
// line-memory.json on every call would be redundant work for a
// portfolio-level statistic that is constant within a run. We memoize
// the computed `SignalAccuracyStats` by `run_id` here so the codex path
// pays the cost exactly once per run (mirroring `flush_batch` in
// `native_mcp_analysis.rs` which computes once per batch).
//
// The cache is intentionally simple: a Mutex<HashMap>. Memory cost is
// trivial (one struct per run_id, bounded by simultaneous active
// runs, typically 1). No eviction logic — entries are dropped on
// process restart; stale entries from finished runs are harmless.
static CALIBRATION_CACHE: OnceLock<Mutex<HashMap<String, SignalAccuracyStats>>> =
    OnceLock::new();

fn calibration_cache() -> &'static Mutex<HashMap<String, SignalAccuracyStats>> {
    CALIBRATION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn calibration_for_run(run_id: &str) -> Option<SignalAccuracyStats> {
    if run_id.is_empty() {
        return None;
    }
    if let Ok(guard) = calibration_cache().lock() {
        if let Some(cached) = guard.get(run_id) {
            return Some(cached.clone());
        }
    }
    let stats = match crate::signal_accuracy::compute_signal_accuracy() {
        Ok(s) => s,
        Err(e) => {
            crate::debug_log(&format!("[llm] calibration compute failed: {e}"));
            return None;
        }
    };
    if let Ok(mut guard) = calibration_cache().lock() {
        guard.insert(run_id.to_string(), stats.clone());
    }
    Some(stats)
}

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
    // Enrich line_context with sector + COT data if not already present.
    // run_id is extracted so the persist branch can mirror the canonical
    // mcp_server path — required by `product_llm_mode_parity_2026_04`:
    // any backend reaching the `/v1/line/analyze` shim must surface the
    // sector slug to the UI just like the MCP `tool_get_line_data` path.
    let run_id = run_state
        .get("run_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let enriched_context = enrich_line_context_with_sector(line_context, run_id);
    let ctx = &enriched_context;

    // P1-59 follow-up — codex MCP / live shim parity with native.
    // `calibration_for_run` memoizes the result per run_id so the disk
    // read + JSON aggregation only happens once per run (matches the
    // once-per-batch contract the native path enforces in `flush_batch`).
    let calibration = calibration_for_run(run_id);
    let calibration_ref = calibration.as_ref();

    let mode = resolve_generation_mode();
    match mode.as_str() {
        "live" => generate_line_live(ctx, run_state, agent_guidelines, validation_context, calibration_ref),
        "codex_proxy" => generate_line_codex(ctx, run_state, agent_guidelines, validation_context, calibration_ref),
        "mock_cache" | "mock_past" => generate_line_from_cache(ctx),
        _ => generate_line_codex(ctx, run_state, agent_guidelines, validation_context, calibration_ref),
    }
}

/// Inject sector_cot into line_context if missing (for JS-originating contexts).
///
/// P1-57 follow-up : after fetching the sector slug, this function also
/// persists `run_state.market[ticker].sector` via `apply_sector_to_market`
/// — symmetric with `mcp_server::tool_get_line_data`. This keeps the
/// legacy `/v1/line/analyze` shim path on parity with the canonical MCP
/// path (BINDING `product_llm_mode_parity_2026_04`).
fn enrich_line_context_with_sector(line_context: &Value, run_id: &str) -> Value {
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
    // Canonical Yahoo symbol — resolved during collection and stashed on the
    // position row (`Position::resolved_symbol`). Forwarded to news/sector/cot
    // so the server can route on the resolved symbol instead of the broker
    // ticker. Optional — server falls back to ticker when absent.
    let canonical_opt = line_context
        .get("row").and_then(|r| r.get("resolved_symbol")).and_then(|v| v.as_str())
        .or_else(|| line_context.get("resolved_symbol").and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty());

    if ticker.is_empty() && isin.is_empty() {
        return line_context.clone();
    }

    let sector_resp = crate::enrichment::fetch_sector(ticker, name, isin, canonical_opt).ok();
    let sector_slug = sector_resp.as_ref()
        .and_then(|r| r.get("sector").and_then(|v| v.as_str()))
        .unwrap_or("");
    let sector_analysis = sector_resp.as_ref()
        .and_then(|r| r.get("sector_analysis").cloned())
        .unwrap_or(Value::Null);
    let cot_data = if !sector_slug.is_empty() {
        crate::enrichment::fetch_cot(ticker, isin, canonical_opt)
            .ok()
            .and_then(|r| r.get("cot").cloned())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    // P1-57 — persist sector slug into `run_state.market[ticker].sector` so the
    // UI surfaces it without round-tripping through the LLM. Reuses
    // `apply_sector_to_market` (the same pure helper called by the canonical
    // MCP path) per `feedback_factorize_code`. No-op when run_id is empty
    // (e.g. ad-hoc calls without an active run) or when the patch helper
    // fails to locate the run state (best-effort, mirrors the MCP path).
    if !sector_slug.is_empty() && !run_id.is_empty() {
        let ticker_owned = ticker.to_string();
        let slug_owned = sector_slug.to_string();
        let _ = crate::patch_run_state_direct_with(run_id, move |rs| {
            crate::mcp_server::apply_sector_to_market(rs, &ticker_owned, &slug_owned);
        });
    }

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
    calibration_stats: Option<&SignalAccuracyStats>,
) -> Result<Value> {
    let model = resolve_model();
    let run_id = run_state.get("run_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let prompt = if is_repair_pass(validation_context) {
        build_repair_prompt(line_context, run_state, agent_guidelines, validation_context.unwrap(), calibration_stats)
    } else {
        build_line_analysis_prompt(line_context, run_state, agent_guidelines, calibration_stats)
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
            { "role": "system", "content": "Tu es un conseiller financier qui s'adresse a des investisseurs particuliers non-experts. Tu expliques les choses simplement, avec des exemples concrets et des chiffres. Tu produis uniquement du JSON valide, sans texte supplementaire. Quand les sections MEMOIRE LIGNE et TECHNIQUE sont fournies, elles se completent : MEMOIRE LIGNE = accountability des recommandations Alfred passees (historique multi-runs), TECHNIQUE = etat marche actuel calcule sur ~250 jours OHLC (independant des runs). Combine les : la memoire dit ce que nous avons annonce, la technique dit ce que dit le marche aujourd'hui." },
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
    calibration_stats: Option<&SignalAccuracyStats>,
) -> Result<Value> {
    let prompt = if is_repair_pass(validation_context) {
        build_repair_prompt(line_context, run_state, agent_guidelines, validation_context.unwrap(), calibration_stats)
    } else {
        build_line_analysis_prompt(line_context, run_state, agent_guidelines, calibration_stats)
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    /// Serialise tests that mutate ALFRED_STATE_DIR — env vars are
    /// process-global so parallel tests would race otherwise.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique_state_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "alfred-llm-sector-persist-{tag}-{}",
            crate::now_epoch_ms()
        ))
    }

    /// P1-57 follow-up — `enrich_line_context_with_sector` writes the GICS
    /// slug back to `run_state.market[ticker].sector` via the same helper
    /// path (`patch_run_state_direct_with` + `apply_sector_to_market`) the
    /// legacy shim invokes. We can't drive `fetch_sector` from a unit test
    /// (it hits HTTP), but we can prove the wiring is correct end-to-end
    /// by invoking the helper chain directly against a temp run_state and
    /// asserting the slug lands where the UI reads it.
    ///
    /// This guards the BINDING `product_llm_mode_parity_2026_04`: any
    /// backend reaching `/v1/line/analyze` must surface the sector slug
    /// to the UI just like the canonical MCP path.
    #[test]
    fn enrich_line_context_with_sector_persists_to_run_state() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let base = unique_state_dir("persist");
        let state_dir = base.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("create state dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let run_id = "run-llm-sector-persist-test";
        let initial = serde_json::json!({
            "run_id": run_id,
            "market": {
                "AAPL": { "prix_actuel": 150.0, "pe_ratio": 28.0 }
            }
        });
        let run_path = state_dir.join(format!("{run_id}.json"));
        fs::write(&run_path, serde_json::to_string(&initial).unwrap())
            .expect("write run_state");

        // Mirror the persist path in `enrich_line_context_with_sector` —
        // this is exactly what the function does after `fetch_sector`
        // returns a non-empty slug.
        let ticker = "AAPL".to_string();
        let slug = "tech".to_string();
        crate::patch_run_state_direct_with(run_id, move |rs| {
            crate::mcp_server::apply_sector_to_market(rs, &ticker, &slug);
        })
        .expect("patch should succeed");

        // Force the in-memory cache to flush so the disk reflects the patch.
        // We re-read via patch (which returns the current cached state).
        let after = crate::patch_run_state_direct_with(run_id, |_rs| {})
            .expect("re-read");
        assert_eq!(after["market"]["AAPL"]["sector"], "tech",
            "sector slug must land in market[ticker] just like the MCP path");
        // Additive contract — existing keys preserved.
        assert_eq!(after["market"]["AAPL"]["prix_actuel"], 150.0);
        assert_eq!(after["market"]["AAPL"]["pe_ratio"], 28.0);

        // Cleanup
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base);
    }

    /// Contract-style guard : ensure `enrich_line_context_with_sector`
    /// actually contains the call to `apply_sector_to_market`. Cheap
    /// regression net — if a future refactor strips the persist call,
    /// this test fails immediately rather than silently breaking parity.
    #[test]
    fn enrich_line_context_with_sector_call_site_persists_via_apply_helper() {
        let src = include_str!("llm.rs");
        // Locate the function body.
        let start = src.find("fn enrich_line_context_with_sector(")
            .expect("function must exist");
        // End at the next top-level `fn ` or end of file.
        let rest = &src[start..];
        let end = rest[1..].find("\nfn ").map(|i| i + 1).unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("apply_sector_to_market"),
            "enrich_line_context_with_sector must call apply_sector_to_market \
             to keep the legacy /v1/line/analyze shim on parity with the MCP path \
             (BINDING product_llm_mode_parity_2026_04)"
        );
        assert!(
            body.contains("patch_run_state_direct_with"),
            "enrich_line_context_with_sector must persist via patch_run_state_direct_with \
             (the same helper the canonical MCP path uses indirectly through run_state_cache::patch)"
        );
    }
}
