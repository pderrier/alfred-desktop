//! MCP (Model Context Protocol) server — stdio-based JSON-RPC 2.0.
//!
//! Entry point: `run_stdio_server(data_dir)`.
//! Reads newline-delimited JSON-RPC from stdin, writes responses to stdout.
//! Logs to stderr for debugging.
//!
//! Self-contained: no imports from Tauri-dependent modules.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

// ── Helpers (local, no crate:: dependencies) ────────────────────────────────

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn as_text(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn as_upper(v: Option<&Value>) -> String {
    as_text(v).to_ascii_uppercase()
}

fn log(msg: &str) {
    eprintln!("[mcp-server {}] {}", now_iso(), msg);
}

// ── File I/O with simple lock ───────────────────────────────────────────────

fn read_json(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| anyhow!("read_failed:{}:{e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| anyhow!("parse_failed:{}:{e}", path.display()))
}

/// Append a JSONL line to a progress file.
fn append_progress(data_dir: &Path, run_id: &str, event: &Value) {
    let path = data_dir
        .join("runtime-state")
        .join(format!("{run_id}_mcp_progress.jsonl"));
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let line = serde_json::to_string(event).unwrap_or_default();
        let _ = writeln!(f, "{}", line);
    }
}

/// Append a result entry to the MCP results sidecar file.
/// The main process merges these into the run state after each batch.
fn append_mcp_result(data_dir: &Path, run_id: &str, result: &Value) {
    let path = data_dir
        .join("runtime-state")
        .join(format!("{run_id}_mcp_results.jsonl"));
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let line = serde_json::to_string(result).unwrap_or_default();
        let _ = writeln!(f, "{}", line);
    }
}

/// Merge new recommendation into existing: new values win UNLESS they are empty/null
/// and the existing value is non-empty. Prevents overwriting good data with blanks.
fn merge_recommendation(existing: &Value, new: &Value) -> Value {
    let mut merged = existing.clone();
    if let (Some(base), Some(update)) = (merged.as_object_mut(), new.as_object()) {
        for (key, new_val) in update {
            let is_empty = new_val.is_null()
                || (new_val.is_string() && new_val.as_str().unwrap_or_default().trim().is_empty())
                || (new_val.is_array() && new_val.as_array().unwrap().is_empty());
            let existing_val = base.get(key);
            let existing_has_value = existing_val.is_some_and(|v| {
                !v.is_null()
                    && !(v.is_string() && v.as_str().unwrap_or_default().trim().is_empty())
                    && !(v.is_array() && v.as_array().unwrap().is_empty())
            });
            // Only skip if new is empty AND existing has real data
            if is_empty && existing_has_value {
                continue;
            }
            base.insert(key.clone(), new_val.clone());
        }
    }
    merged
}

// ── Run state (via in-memory cache) ──────────────────────────────────────────

fn load_run_state(data_dir: &Path, run_id: &str) -> Result<Value> {
    let mut state = crate::run_state_cache::load(data_dir, run_id)?;

    // Overlay sidecar results so tools like finalize_report see recommendations
    let results_path = data_dir.join("runtime-state").join(format!("{run_id}_mcp_results.jsonl"));
    if let Ok(content) = std::fs::read_to_string(&results_path) {
        for line in content.lines() {
            let entry: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let entry_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match entry_type {
                "recommendation" => {
                    let line_id = entry.get("line_id").and_then(|v| v.as_str()).unwrap_or("");
                    let rec = entry.get("recommendation").cloned().unwrap_or(json!({}));
                    if !line_id.is_empty() {
                        let pending = state.as_object_mut().unwrap()
                            .entry("pending_recommandations")
                            .or_insert_with(|| json!([]));
                        if let Some(arr) = pending.as_array_mut() {
                            let lid = line_id.to_string();
                            arr.retain(|r| r.get("line_id").and_then(|v| v.as_str()).unwrap_or("") != lid);
                            arr.push(rec);
                        }
                    }
                }
                "synthesis" => {
                    if let Some(obj) = state.as_object_mut() {
                        if let Some(cp) = entry.get("composed_payload") {
                            obj.insert("composed_payload".to_string(), cp.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(state)
}


fn line_memory_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime-state").join("line-memory.json")
}

// ── Line-id helpers ─────────────────────────────────────────────────────────

fn make_line_id(value: &Value) -> String {
    let direct = as_text(value.get("line_id"));
    if !direct.is_empty() {
        return direct;
    }
    let ticker = as_upper(value.get("ticker"));
    if ticker.is_empty() {
        return String::new();
    }
    let line_type = {
        let raw = as_text(value.get("type")).to_ascii_lowercase();
        if raw.is_empty() {
            "position".to_string()
        } else {
            raw
        }
    };
    format!("{line_type}:{ticker}")
}

fn derive_expected_line_ids(run_state: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    for (line_type, rows) in [
        (
            "position",
            run_state
                .get("portfolio")
                .and_then(|v| v.get("positions"))
                .and_then(|v| v.as_array()),
        ),
        (
            "watchlist",
            run_state
                .get("watchlist")
                .and_then(|v| v.get("items"))
                .and_then(|v| v.as_array()),
        ),
    ] {
        for row in rows.unwrap_or(&Vec::new()) {
            let ticker = as_upper(row.get("ticker"));
            if ticker.is_empty() {
                continue;
            }
            let line_id = format!("{line_type}:{ticker}");
            if seen.insert(line_id.clone()) {
                ids.push(line_id);
            }
        }
    }
    ids
}

// ── Alfred API client (fire-and-forget, self-contained) ─────────────────────

const DEFAULT_API_URL: &str = "https://vps-c5793aab.vps.ovh.net/alfred/api";

fn api_url() -> Option<String> {
    let enabled = env::var("ALFRED_API_ENABLED")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    Some(
        env::var("ALFRED_API_URL")
            .unwrap_or_else(|_| DEFAULT_API_URL.to_string())
            .trim_end_matches('/')
            .to_string(),
    )
}

// Delegate to alfred_api_client (HMAC-signed requests).
fn api_persist_extracted_fundamentals(ticker: &str, isin: &str, extracted: &Value) {
    crate::alfred_api_client::persist_extracted_fundamentals(ticker, isin, extracted);
}

fn api_persist_shared_insights(ticker: &str, isin: &str, insights: &Value) {
    crate::alfred_api_client::persist_shared_insights(ticker, isin, insights);
}

// Kept for backward compat — but no longer used directly.
fn _api_persist_shared_insights_legacy(ticker: &str, isin: &str, insights: &Value) {
    let base = match api_url() { Some(u) => u, None => return };
    let url = format!("{base}/api/insights");
    let body = json!({ "ticker": ticker, "isin": isin, "insights": insights });
    match ureq::post(&url)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&serde_json::to_string(&body).unwrap_or_default())
    {
        Ok(_) => log(&format!("api: persisted shared insights for {ticker}")),
        Err(e) => log(&format!(
            "api: failed to persist shared insights for {ticker}: {e}"
        )),
    }
}

// ── JSON-RPC 2.0 helpers ────────────────────────────────────────────────────

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

// ── MCP Tool Definitions ────────────────────────────────────────────────────

fn tool_definitions() -> Value {
    json!([
        {
            "name": "get_run_context",
            "description": "Get the current run context: portfolio summary, line IDs, agent guidelines, watchlist.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" }
                },
                "required": ["run_id"]
            }
        },
        {
            "name": "get_line_data",
            "description": "Get all data for a specific position/watchlist line: position row, market data, news, shared insights, line memory, quality indicators.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" },
                    "line_id": { "type": "string", "description": "Line ID in format type:TICKER (e.g. position:MC, watchlist:ASML)" }
                },
                "required": ["run_id", "line_id"]
            }
        },
        {
            "name": "validate_recommendation",
            "description": "Validate and persist a line recommendation. Returns validation issues or confirms storage.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" },
                    "recommendation": { "type": "string", "description": "JSON string of the recommendation object" }
                },
                "required": ["run_id", "recommendation"]
            }
        },
        {
            "name": "validate_synthesis",
            "description": "Validate and store the global synthesis (market summary, immediate actions, next analysis, watchlist opportunities).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" },
                    "synthese_marche": { "type": "string", "description": "Market synthesis text (>= 100 chars)" },
                    "actions_immediates": { "type": "string", "description": "JSON string of actions array (max 5)" },
                    "prochaine_analyse": { "type": "string", "description": "Next analysis schedule/notes" },
                    "opportunites_watchlist": { "type": "string", "description": "Watchlist opportunities summary" }
                },
                "required": ["run_id", "synthese_marche", "actions_immediates", "prochaine_analyse", "opportunites_watchlist"]
            }
        },
        {
            "name": "check_coverage",
            "description": "Check recommendation coverage: which lines are missing, duplicated, or unexpected.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" }
                },
                "required": ["run_id"]
            }
        },
        {
            "name": "finalize_report",
            "description": "Compose the final report from pending recommendations and synthesis, persist to disk, mark run as completed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "The run ID" }
                },
                "required": ["run_id"]
            }
        },
        {
            "name": "persist_extracted_fundamentals",
            "description": "Persist LLM-extracted fundamental values to the Alfred API cache.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "isin": { "type": "string" },
                    "fundamentals": { "type": "string", "description": "JSON string of fundamentals object" }
                },
                "required": ["ticker", "isin", "fundamentals"]
            }
        },
        {
            "name": "persist_shared_insights",
            "description": "Persist shared analysis insights to the Alfred API cache.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "isin": { "type": "string" },
                    "insights": { "type": "string", "description": "JSON string of insights object" }
                },
                "required": ["ticker", "isin", "insights"]
            }
        },
        {
            "name": "persist_deep_news",
            "description": "Persist a deep news summary for a specific article URL. This caches the summary so future runs reuse it instead of re-reading the article.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "isin": { "type": "string" },
                    "url": { "type": "string", "description": "The article URL" },
                    "title": { "type": "string" },
                    "summary": { "type": "string", "description": "Deep summary of the article (100-800 chars)" },
                    "quality_score": { "type": "integer", "description": "News quality score 0-100" },
                    "relevance": { "type": "string", "description": "high|medium|low" },
                    "staleness": { "type": "string", "description": "fresh|recent|stale" }
                },
                "required": ["ticker", "url", "summary"]
            }
        },
        {
            "name": "ban_deep_news",
            "description": "Ban a news URL as noise for a ticker. Banned articles are filtered out in future runs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "isin": { "type": "string" },
                    "url": { "type": "string", "description": "The article URL to ban" },
                    "reason": { "type": "string", "description": "Why: noise, not_relevant, stale, duplicate" }
                },
                "required": ["ticker", "url"]
            }
        }
    ])
}

// ── Tool Implementations ────────────────────────────────────────────────────

fn tool_get_run_context(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let run_state = load_run_state(data_dir, &run_id)?;

    let portfolio = run_state.get("portfolio").cloned().unwrap_or(json!({}));
    let positions = portfolio
        .get("positions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let line_ids: Vec<Value> = positions
        .iter()
        .map(|p| {
            json!({
                "line_id": make_line_id(p),
                "type": as_text(p.get("type")).to_ascii_lowercase().replace("", "").trim().to_string(),
                "ticker": as_upper(p.get("ticker")),
                "nom": as_text(p.get("nom")),
            })
        })
        .collect();

    let watchlist_items = run_state
        .get("watchlist")
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let watchlist_line_ids: Vec<Value> = watchlist_items
        .iter()
        .map(|w| {
            json!({
                "line_id": format!("watchlist:{}", as_upper(w.get("ticker"))),
                "type": "watchlist",
                "ticker": as_upper(w.get("ticker")),
                "nom": as_text(w.get("nom")),
            })
        })
        .collect();

    let mut all_lines = line_ids;
    all_lines.extend(watchlist_line_ids);

    Ok(json!({
        "portfolio_summary": {
            "valeur_totale": portfolio.get("valeur_totale").cloned().unwrap_or(Value::Null),
            "plus_value_totale": portfolio.get("plus_value_totale").cloned().unwrap_or(Value::Null),
            "liquidites": portfolio.get("liquidites").cloned().unwrap_or(Value::Null),
            "position_count": positions.len(),
        },
        "lines": all_lines,
        "agent_guidelines": as_text(run_state.get("agent_guidelines")),
        "watchlist": watchlist_items,
    }))
}

/// Cap news articles: keep cached deep summaries first, then most recent, up to max.
fn cap_news_articles(news: &Value, max: usize) -> Value {
    let articles_key = if news.get("items").is_some() { "items" }
        else if news.get("articles").is_some() { "articles" }
        else { return news.clone(); };

    let articles = match news.get(articles_key).and_then(|v| v.as_array()) {
        Some(arr) if arr.len() <= max => return news.clone(),
        Some(arr) => arr,
        None => return news.clone(),
    };

    // Score each article: cached deep summaries ranked by quality_score + relevance + freshness
    fn article_sort_key(a: &Value) -> (i64, i64, String) {
        let is_cached = a.get("deep_summary_cached").and_then(|v| v.as_bool()).unwrap_or(false);
        let quality = a.get("deep_quality_score").and_then(|v| v.as_i64()).unwrap_or(0);
        let relevance_score = match a.get("relevance").or_else(|| a.get("deep_relevance"))
            .and_then(|v| v.as_str()).unwrap_or("") {
            "high" => 3, "medium" => 2, "low" => 1, _ => if is_cached { 2 } else { 0 },
        };
        let staleness_score = match a.get("staleness").or_else(|| a.get("deep_staleness"))
            .and_then(|v| v.as_str()).unwrap_or("") {
            "fresh" => 3, "recent" => 2, "stale" => 1, _ => 2,
        };
        let date = a.get("date").or_else(|| a.get("published_at"))
            .and_then(|v| v.as_str()).unwrap_or("").to_string();
        // Composite: cached first (boost 1000), then relevance×staleness×quality, then date
        let boost = if is_cached { 1000 } else { 0 };
        let score = boost + (relevance_score * staleness_score * 10) + quality;
        (score, relevance_score, date)
    }

    let mut scored: Vec<(i64, &Value)> = articles.iter()
        .map(|a| { let (s, _, _) = article_sort_key(a); (s, a) })
        .collect();
    // Sort descending by score
    scored.sort_by(|a, b| b.0.cmp(&a.0));

    let selected: Vec<Value> = scored.into_iter()
        .take(max)
        .map(|(_, a)| a.clone())
        .collect();

    let mut result = news.clone();
    if let Some(obj) = result.as_object_mut() {
        obj.insert(articles_key.to_string(), json!(selected));
    }
    result
}

fn tool_get_line_data(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let line_id = as_text(params.get("line_id"));
    let run_state = load_run_state(data_dir, &run_id)?;

    // Parse line_id → type + ticker
    let (line_type, ticker) = if let Some(idx) = line_id.find(':') {
        (
            line_id[..idx].to_string(),
            line_id[idx + 1..].to_ascii_uppercase(),
        )
    } else {
        ("position".to_string(), line_id.to_ascii_uppercase())
    };

    // Find position row
    let positions = run_state
        .get("portfolio")
        .and_then(|v| v.get("positions"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let watchlist_items = run_state
        .get("watchlist")
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let position_row = if line_type == "watchlist" {
        watchlist_items
            .iter()
            .find(|w| as_upper(w.get("ticker")) == ticker)
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        positions
            .iter()
            .find(|p| as_upper(p.get("ticker")) == ticker)
            .cloned()
            .unwrap_or(Value::Null)
    };

    // Market data for this ticker
    let market_data = run_state
        .get("market")
        .and_then(|m| m.get(&ticker))
        .cloned()
        .or_else(|| {
            // Try lowercase key
            run_state
                .get("market")
                .and_then(|m| m.get(&ticker.to_lowercase()))
                .cloned()
        })
        .unwrap_or(Value::Null);

    // News for this ticker — cap at 5 articles, prioritize cached deep summaries
    let news = {
        let raw = run_state
            .get("news")
            .and_then(|n| n.get(&ticker).or_else(|| n.get(&ticker.to_lowercase())))
            .cloned()
            .unwrap_or(Value::Null);
        cap_news_articles(&raw, 5)
    };

    // Shared insights — fetch from API (always fresh, not from run_state)
    let isin = position_row.get("isin").and_then(|v| v.as_str()).unwrap_or(&ticker);
    let shared_insights = crate::enrichment::fetch_shared_insights(&ticker, isin)
        .ok()
        .and_then(|r| r.get("insights").cloned())
        .unwrap_or(Value::Null);

    // Line memory (cross-run)
    let line_memory = {
        let mem_path = line_memory_path(data_dir);
        if mem_path.exists() {
            read_json(&mem_path)
                .ok()
                .and_then(|mem| mem.get(&ticker).cloned())
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    };

    // Quality indicators
    let quality = run_state
        .get("quality")
        .and_then(|q| q.get("by_ticker"))
        .and_then(|bt| bt.get(&ticker))
        .cloned()
        .unwrap_or(Value::Null);

    // Write progress event — "collecting context" step
    append_progress(
        data_dir,
        &run_id,
        &json!({
            "type": "line_progress",
            "ticker": ticker,
            "status": "analyzing",
            "progress": "loading context\u{2026}",
            "at": now_iso(),
        }),
    );

    Ok(json!({
        "line_id": line_id,
        "line_type": line_type,
        "ticker": ticker,
        "position": position_row,
        "market_data": market_data,
        "news": news,
        "shared_insights": shared_insights,
        "line_memory": line_memory,
        "quality": quality,
    }))
}

fn tool_validate_recommendation(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let rec_str = as_text(params.get("recommendation"));
    let rec: Value =
        serde_json::from_str(&rec_str).map_err(|e| anyhow!("invalid_recommendation_json:{e}"))?;

    // Progress: validating
    let ticker_for_progress = as_text(rec.get("ticker"));
    if !ticker_for_progress.is_empty() {
        append_progress(data_dir, &run_id, &json!({
            "type": "line_progress",
            "ticker": ticker_for_progress,
            "status": "analyzing",
            "progress": "validating recommendation\u{2026}",
        }));
    }

    let line_id = as_text(rec.get("line_id"));
    let is_watchlist = line_id.starts_with("watchlist:");

    // Track validation attempts per line — accept with warnings after max retries
    static ATTEMPT_COUNTS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u32>>> = std::sync::OnceLock::new();
    let attempts = {
        let map = ATTEMPT_COUNTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
        let key = format!("{run_id}:{line_id}");
        let count = guard.entry(key).or_insert(0);
        *count += 1;
        *count
    };
    const MAX_VALIDATION_RETRIES: u32 = 2;

    let mut hard_issues: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // ── Hard blockers (reject + retry) ──
    let synthese = as_text(rec.get("synthese"));
    if synthese.chars().count() < 80 {
        hard_issues.push("synthese_too_short".to_string());
    }

    let conviction = as_text(rec.get("conviction"))
        .to_lowercase()
        .replace('é', "e")
        .replace('è', "e");
    if !["faible", "moderee", "forte"].contains(&conviction.as_str()) {
        hard_issues.push("invalid_conviction".to_string());
    }

    let signal = as_text(rec.get("signal")).to_ascii_uppercase();
    let valid_signals = [
        "ACHAT_FORT", "ACHAT", "RENFORCEMENT", "CONSERVER",
        "ALLEGEMENT", "VENTE", "SURVEILLANCE",
    ];
    if !valid_signals.contains(&signal.as_str()) {
        hard_issues.push("invalid_signal".to_string());
    }

    if line_id.is_empty() || !line_id.contains(':') {
        hard_issues.push("invalid_line_id_format".to_string());
    }

    // ── Soft warnings (store anyway, reported but don't block) ──
    if as_text(rec.get("analyse_technique")).is_empty() {
        warnings.push("analyse_technique_empty".to_string());
    }
    if as_text(rec.get("analyse_fondamentale")).is_empty() {
        warnings.push("analyse_fondamentale_empty".to_string());
    }
    if as_text(rec.get("analyse_sentiment")).is_empty() {
        warnings.push("analyse_sentiment_empty".to_string());
    }
    let raisons = rec.get("raisons_principales")
        .and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    if raisons < 2 {
        warnings.push("raisons_principales_insufficient".to_string());
    }
    if as_text(rec.get("action_recommandee")).is_empty() {
        warnings.push("action_recommandee_empty".to_string());
    }
    if as_text(rec.get("deep_news_summary")).is_empty() {
        warnings.push("deep_news_summary_empty".to_string());
    }

    // Only reject on hard issues (or accept after max retries)
    let issues: Vec<String> = hard_issues.iter().chain(warnings.iter()).cloned().collect();
    if !hard_issues.is_empty() {
        if attempts > MAX_VALIDATION_RETRIES {
            log(&format!("validation: accepting {} after {attempts} attempts (hard issues: {})", line_id, hard_issues.join(", ")));
            // Fall through to storage below
        } else {
            // Reject — model will retry
            if !ticker_for_progress.is_empty() {
                append_progress(data_dir, &run_id, &json!({
                    "type": "line_progress",
                    "ticker": ticker_for_progress,
                    "status": "repairing",
                    "progress": format!("fixing: {}", issues.join(", ")),
                }));
            }
            return Ok(json!({
                "ok": false,
                "stored": false,
                "issues": issues,
                "attempt": attempts,
                "max_retries": MAX_VALIDATION_RETRIES,
            }));
        }
    }

    // Valid — write to sidecar file (main process merges after each batch)
    append_mcp_result(data_dir, &run_id, &json!({
        "type": "recommendation",
        "line_id": line_id,
        "recommendation": rec,
        "at": now_iso(),
    }));

    // Count progress from sidecar file
    let results_path = data_dir.join("runtime-state").join(format!("{run_id}_mcp_results.jsonl"));
    let completed = std::fs::read_to_string(&results_path).ok()
        .map(|s| s.lines().filter(|l| l.contains("\"recommendation\"")).count())
        .unwrap_or(0);
    let run_state = load_run_state(data_dir, &run_id)?;
    let total = {
        let pos_count = run_state.get("portfolio")
            .and_then(|p| p.get("positions")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let wl_count = run_state.get("watchlist")
            .and_then(|w| w.get("items")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        pos_count + wl_count
    };

    // Emit progress — both JSONL (for relay) and direct Tauri event (immediate UI update)
    let ticker_part = line_id.split(':').last().unwrap_or_default();
    let conviction_text = as_text(rec.get("conviction"));
    let synthese_short: String = synthese.chars().take(120).collect();

    append_progress(
        data_dir,
        &run_id,
        &json!({
            "type": "line_done",
            "ticker": ticker_part,
            "recommendation": { "signal": signal, "conviction": conviction_text, "synthese": synthese_short },
            "completed": completed,
            "total": total,
            "at": now_iso(),
        }),
    );

    // Emit events for UI — the main process will merge results from the sidecar
    crate::emit_event("alfred://line-progress", json!({
        "run_id": run_id,
        "ticker": ticker_part,
        "line_status": {"status": "done"},
    }));
    crate::emit_event("alfred://line-done", json!({
        "run_id": run_id,
        "ticker": ticker_part,
        "recommendation": { "signal": signal, "conviction": conviction_text, "synthese": synthese_short },
        "line_progress": { "completed": completed, "total": total },
    }));

    let mut result = json!({
        "ok": true,
        "stored": true,
        "issues": [],
    });
    if !warnings.is_empty() {
        result
            .as_object_mut()
            .unwrap()
            .insert("warnings".to_string(), json!(warnings));
    }
    Ok(result)
}

fn tool_validate_synthesis(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let synthese_marche = as_text(params.get("synthese_marche"));
    let actions_str = as_text(params.get("actions_immediates"));
    let prochaine_analyse = as_text(params.get("prochaine_analyse"));
    let opportunites_watchlist = as_text(params.get("opportunites_watchlist"));

    let actions: Vec<Value> = serde_json::from_str(&actions_str)
        .map_err(|e| anyhow!("invalid_actions_immediates_json:{e}"))?;

    let mut issues: Vec<String> = Vec::new();

    // synthese_marche >= 100 chars
    if synthese_marche.chars().count() < 100 {
        issues.push("synthese_marche_too_short".to_string());
    }

    // actions_immediates max 5
    if actions.len() > 5 {
        issues.push("actions_immediates_too_many".to_string());
    }

    // Each action needs ticker/action/rationale/priority
    for (i, action) in actions.iter().enumerate() {
        if as_text(action.get("ticker")).is_empty() {
            issues.push(format!("action_{}_missing_ticker", i));
        }
        if as_text(action.get("action")).is_empty() {
            issues.push(format!("action_{}_missing_action", i));
        }
        if as_text(action.get("rationale")).is_empty() {
            issues.push(format!("action_{}_missing_rationale", i));
        }
        if action.get("priority").and_then(|v| v.as_u64()).is_none() {
            issues.push(format!("action_{}_missing_priority", i));
        }
    }

    // Cross-check: actions must be consistent with per-line signals
    let run_state = load_run_state(data_dir, &run_id)?;
    let recs = run_state.get("pending_recommandations")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let actionable_signals = ["ACHAT_FORT", "ACHAT", "VENTE", "ALLEGEMENT", "RENFORCEMENT"];
    for (i, action) in actions.iter().enumerate() {
        let action_ticker = as_text(action.get("ticker")).to_uppercase();
        if action_ticker.is_empty() { continue; }
        let line_rec = recs.iter().find(|r|
            as_text(r.get("ticker")).to_uppercase() == action_ticker
        );
        if let Some(rec) = line_rec {
            let line_signal = as_text(rec.get("signal")).to_uppercase();
            if !actionable_signals.contains(&line_signal.as_str()) {
                issues.push(format!(
                    "action_{i}_{action_ticker}_conflicts_with_line_signal_{line_signal}"
                ));
            }
        }
    }

    // Priorities 1-5, unique
    let priorities: Vec<u64> = actions
        .iter()
        .filter_map(|a| a.get("priority").and_then(|v| v.as_u64()))
        .collect();
    let priority_set: HashSet<u64> = priorities.iter().copied().collect();
    if priority_set.len() != priorities.len() {
        issues.push("priorities_not_unique".to_string());
    }
    for p in &priorities {
        if *p < 1 || *p > 5 {
            issues.push(format!("priority_{}_out_of_range", p));
        }
    }

    if !issues.is_empty() {
        return Ok(json!({
            "ok": false,
            "issues": issues,
        }));
    }

    // Valid — write to sidecar file (main process merges after batch)
    let composed = json!({
        "synthese_marche": synthese_marche,
        "actions_immediates": actions,
        "prochaine_analyse": prochaine_analyse,
        "opportunites_watchlist": opportunites_watchlist,
    });
    append_mcp_result(data_dir, &run_id, &json!({
        "type": "synthesis",
        "composed_payload": composed,
        "at": now_iso(),
    }));

    append_progress(
        data_dir,
        &run_id,
        &json!({
            "type": "synthesis_progress",
            "progress": "synthesis validated — composing report\u{2026}",
            "at": now_iso(),
        }),
    );

    Ok(json!({
        "ok": true,
        "issues": [],
    }))
}

fn tool_check_coverage(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));

    append_progress(data_dir, &run_id, &json!({
        "type": "synthesis_progress",
        "progress": "checking coverage\u{2026}",
    }));

    let run_state = load_run_state(data_dir, &run_id)?;

    let expected = derive_expected_line_ids(&run_state);
    let expected_set: HashSet<String> = expected.iter().cloned().collect();

    let pending = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut covered = HashSet::new();
    let mut duplicates = Vec::new();
    let mut unexpected = Vec::new();

    for rec in &pending {
        let lid = as_text(rec.get("line_id"));
        if lid.is_empty() {
            continue;
        }
        if !covered.insert(lid.clone()) {
            duplicates.push(lid.clone());
        }
        if !expected_set.contains(&lid) {
            unexpected.push(lid);
        }
    }

    let missing: Vec<String> = expected
        .iter()
        .filter(|id| !covered.contains(*id))
        .cloned()
        .collect();

    let ok = missing.is_empty() && duplicates.is_empty();

    Ok(json!({
        "ok": ok,
        "expected_count": expected.len(),
        "covered_count": covered.len(),
        "missing": missing,
        "duplicates": duplicates,
        "unexpected": unexpected,
    }))
}

fn tool_finalize_report(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));

    let run_state = load_run_state(data_dir, &run_id)?;
    let composed = run_state.get("composed_payload").cloned().unwrap_or(json!({}));
    let pending = run_state.get("pending_recommandations")
        .and_then(|v| v.as_array()).cloned().unwrap_or_default();

    if pending.is_empty() {
        return Err(anyhow!("no_recommendations_to_finalize"));
    }

    // Build a draft in the same format that persist_retry_global_synthesis expects
    let draft = json!({
        "ok": true,
        "synthese_marche": composed.get("synthese_marche").cloned().unwrap_or(json!("")),
        "actions_immediates": composed.get("actions_immediates").cloned().unwrap_or(json!([])),
        "prochaine_analyse": composed.get("prochaine_analyse").cloned().unwrap_or(json!("")),
        "opportunites_watchlist": composed.get("opportunites_watchlist").cloned().unwrap_or(json!("")),
        "llm_utilise": "codex-mcp",
    });

    // Flush cache to disk before report finalization — report module reads from disk
    crate::run_state_cache::flush_now(&run_id);

    // Delegate to the same function the legacy path uses — ensures identical format,
    // validation, composed_payload writing, report artifacts, orchestration status.
    let result = crate::report::persist_retry_global_synthesis(&run_id, &draft)?;

    // Evict from cache — run is done, disk is the source of truth now
    crate::run_state_cache::evict(&run_id);

    // Write progress event
    append_progress(
        data_dir,
        &run_id,
        &json!({
            "type": "stage",
            "stage": "completed",
            "at": now_iso(),
        }),
    );

    let reco_count = result.get("report")
        .and_then(|r| r.get("num_recommandations"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let report_path = result.get("report")
        .and_then(|r| r.get("latest_report_path"))
        .and_then(|v| v.as_str()).unwrap_or("");

    Ok(json!({
        "ok": true,
        "report_path": report_path,
        "recommendation_count": reco_count,
    }))
}

fn tool_persist_extracted_fundamentals(data_dir: &Path, params: &Value) -> Result<Value> {
    let ticker = as_text(params.get("ticker"));
    let isin = as_text(params.get("isin"));
    let run_id = as_text(params.get("run_id"));
    let fundamentals_str = as_text(params.get("fundamentals"));
    let fundamentals: Value = serde_json::from_str(&fundamentals_str)
        .map_err(|e| anyhow!("invalid_fundamentals_json:{e}"))?;

    if !ticker.is_empty() && !run_id.is_empty() {
        append_progress(data_dir, &run_id, &json!({
            "type": "line_progress", "ticker": ticker,
            "status": "analyzing", "progress": "persisting fundamentals\u{2026}",
        }));
    }

    api_persist_extracted_fundamentals(&ticker, &isin, &fundamentals);

    Ok(json!({ "ok": true }))
}

fn tool_persist_shared_insights(data_dir: &Path, params: &Value) -> Result<Value> {
    let ticker = as_text(params.get("ticker"));
    let isin = as_text(params.get("isin"));
    let run_id = as_text(params.get("run_id"));
    let insights_str = as_text(params.get("insights"));
    let insights: Value =
        serde_json::from_str(&insights_str).map_err(|e| anyhow!("invalid_insights_json:{e}"))?;

    if !ticker.is_empty() && !run_id.is_empty() {
        append_progress(data_dir, &run_id, &json!({
            "type": "line_progress", "ticker": ticker,
            "status": "analyzing", "progress": "sharing insights\u{2026}",
        }));
    }

    api_persist_shared_insights(&ticker, &isin, &insights);

    Ok(json!({ "ok": true }))
}

fn tool_persist_deep_news(data_dir: &Path, arguments: &Value) -> Result<Value> {
    let ticker = as_text(arguments.get("ticker"));
    let isin = as_text(arguments.get("isin"));
    let run_id = as_text(arguments.get("run_id"));
    let url = as_text(arguments.get("url"));
    let title = as_text(arguments.get("title"));
    let summary = as_text(arguments.get("summary"));
    let quality_score = arguments.get("quality_score").and_then(|v| v.as_u64()).unwrap_or(50);
    let relevance = as_text(arguments.get("relevance"));
    let staleness = as_text(arguments.get("staleness"));

    if ticker.is_empty() || url.is_empty() || summary.is_empty() {
        return Err(anyhow!("ticker + url + summary required"));
    }

    if !run_id.is_empty() {
        append_progress(data_dir, &run_id, &json!({
            "type": "line_progress", "ticker": ticker,
            "status": "analyzing", "progress": "caching deep news\u{2026}",
        }));
    }

    crate::alfred_api_client::persist_deep_news_summary(
        &ticker, &isin, &url, &title, &summary,
        quality_score,
        if relevance.is_empty() { "medium" } else { &relevance },
        if staleness.is_empty() { "recent" } else { &staleness },
    );
    Ok(json!({ "ok": true }))
}

fn tool_ban_deep_news(data_dir: &Path, arguments: &Value) -> Result<Value> {
    let ticker = as_text(arguments.get("ticker"));
    let isin = as_text(arguments.get("isin"));
    let run_id = as_text(arguments.get("run_id"));
    let url = as_text(arguments.get("url"));
    let reason = as_text(arguments.get("reason"));

    if ticker.is_empty() || url.is_empty() {
        return Err(anyhow!("ticker + url required"));
    }

    let ban_reason = if reason.is_empty() { "noise" } else { &reason };

    if !run_id.is_empty() {
        append_progress(data_dir, &run_id, &json!({
            "type": "line_progress", "ticker": ticker,
            "status": "analyzing", "progress": format!("banning noise: {ban_reason}"),
        }));
    }

    crate::alfred_api_client::ban_deep_news_url(&ticker, &isin, &url, ban_reason);
    Ok(json!({ "ok": true }))
}

// ── Tool dispatch ───────────────────────────────────────────────────────────

fn dispatch_tool(data_dir: &Path, name: &str, arguments: &Value) -> Value {
    let result = match name {
        "get_run_context" => tool_get_run_context(data_dir, arguments),
        "get_line_data" => tool_get_line_data(data_dir, arguments),
        "validate_recommendation" => tool_validate_recommendation(data_dir, arguments),
        "validate_synthesis" => tool_validate_synthesis(data_dir, arguments),
        "check_coverage" => tool_check_coverage(data_dir, arguments),
        "finalize_report" => tool_finalize_report(data_dir, arguments),
        "persist_extracted_fundamentals" => tool_persist_extracted_fundamentals(data_dir, arguments),
        "persist_shared_insights" => tool_persist_shared_insights(data_dir, arguments),
        "persist_deep_news" => tool_persist_deep_news(data_dir, arguments),
        "ban_deep_news" => tool_ban_deep_news(data_dir, arguments),
        _ => Err(anyhow!("unknown_tool:{name}")),
    };

    match result {
        Ok(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&value).unwrap_or_default(),
            }],
            "isError": false,
        }),
        Err(e) => json!({
            "content": [{
                "type": "text",
                "text": format!("Error: {e}"),
            }],
            "isError": true,
        }),
    }
}

// ── Public API for native backend (no IPC) ─────────────────────────────────

/// Execute a tool directly without JSON-RPC wrapping. Used by `openai_client.rs`.
/// Returns the raw tool result (not wrapped in MCP content/isError envelope).
pub fn dispatch_tool_direct(data_dir: &std::path::Path, name: &str, arguments: &Value) -> Value {
    let result = match name {
        "get_run_context" => tool_get_run_context(data_dir, arguments),
        "get_line_data" => tool_get_line_data(data_dir, arguments),
        "validate_recommendation" => tool_validate_recommendation(data_dir, arguments),
        "validate_synthesis" => tool_validate_synthesis(data_dir, arguments),
        "check_coverage" => tool_check_coverage(data_dir, arguments),
        "finalize_report" => tool_finalize_report(data_dir, arguments),
        "persist_extracted_fundamentals" => tool_persist_extracted_fundamentals(data_dir, arguments),
        "persist_shared_insights" => tool_persist_shared_insights(data_dir, arguments),
        "persist_deep_news" => tool_persist_deep_news(data_dir, arguments),
        "ban_deep_news" => tool_ban_deep_news(data_dir, arguments),
        _ => Err(anyhow!("unknown_tool:{name}")),
    };
    match result {
        Ok(v) => v,
        Err(e) => json!({ "error": e.to_string() }),
    }
}

/// Tool definitions in OpenAI function-calling format (for native backend).
pub fn tool_definitions_openai() -> Vec<Value> {
    let mcp_tools = tool_definitions();
    mcp_tools
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|t| {
            json!({
                "name": t.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                "description": t.get("description").and_then(|v| v.as_str()).unwrap_or_default(),
                "parameters": t.get("inputSchema").cloned().unwrap_or(json!({"type": "object", "properties": {}})),
            })
        })
        .collect()
}

// ── MCP Request Handler ─────────────────────────────────────────────────────

fn handle_request(data_dir: &Path, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    // Notifications have no id — no response needed
    if id.is_null() && !method.is_empty() {
        log(&format!("notification: {method}"));
        return None;
    }

    match method {
        "initialize" => {
            log("initialize");
            Some(rpc_ok(
                &id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "alfred-mcp-server",
                        "version": "0.1.0"
                    }
                }),
            ))
        }

        "tools/list" => {
            log("tools/list");
            Some(rpc_ok(
                &id,
                json!({
                    "tools": tool_definitions(),
                }),
            ))
        }

        "tools/call" => {
            let tool_name = as_text(params.get("name"));
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            log(&format!("tools/call: {tool_name}"));
            let result = dispatch_tool(data_dir, &tool_name, &arguments);
            Some(rpc_ok(&id, result))
        }

        "" => {
            // No method — possibly malformed
            log("received message with no method");
            Some(rpc_error(&id, -32600, "Invalid Request: no method"))
        }

        other => {
            log(&format!("unknown method: {other}"));
            Some(rpc_error(&id, -32601, &format!("Method not found: {other}")))
        }
    }
}

// ── Entry Point ─────────────────────────────────────────────────────────────

/// Run the MCP stdio server. Blocks until stdin is closed.
pub fn run_stdio_server(data_dir: PathBuf) -> anyhow::Result<()> {
    log(&format!("starting MCP server, data_dir={}", data_dir.display()));

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout_lock = stdout.lock();

    for line_result in stdin.lock().lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                log(&format!("stdin read error: {e}"));
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                log(&format!("JSON parse error: {e}"));
                let err = rpc_error(&Value::Null, -32700, &format!("Parse error: {e}"));
                let _ = writeln!(stdout_lock, "{}", serde_json::to_string(&err).unwrap_or_default());
                let _ = stdout_lock.flush();
                continue;
            }
        };

        if let Some(response) = handle_request(&data_dir, &msg) {
            let response_str = serde_json::to_string(&response).unwrap_or_default();
            let _ = writeln!(stdout_lock, "{}", response_str);
            let _ = stdout_lock.flush();
        }
    }

    log("stdin closed, shutting down");
    Ok(())
}
