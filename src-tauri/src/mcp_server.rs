//! MCP (Model Context Protocol) server — stdio-based JSON-RPC 2.0.
//!
//! Entry point: `run_stdio_server(data_dir)`.
//! Reads newline-delimited JSON-RPC from stdin, writes responses to stdout.
//! Logs to stderr for debugging.
//!
//! Self-contained: no imports from Tauri-dependent modules.

use std::collections::HashSet;

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};


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

/// Insert `sector` into `run_state.market[ticker]`. Pure mutator — no I/O.
///
/// Used by `tool_get_line_data` to persist the GICS slug computed during per-
/// line enrichment so the UI can surface it (P1-57). Behaviour :
///   - Idempotent : repeated calls with the same slug are a no-op.
///   - No-op when `sector_slug` is empty.
///   - No-op when `market` is missing or not an object (caller path can't
///     surface sector without market data anyway).
///   - No-op when `market[ticker]` is missing or not an object (graceful : the
///     ticker may not be in market yet — sector will be re-persisted next
///     enrichment cycle).
/// Additive per snapshot UI contract : never touches existing market keys.
pub(crate) fn apply_sector_to_market(rs: &mut Value, ticker: &str, sector_slug: &str) {
    if sector_slug.is_empty() || ticker.is_empty() {
        return;
    }
    let Some(rs_obj) = rs.as_object_mut() else { return; };
    let Some(market) = rs_obj.get_mut("market").and_then(|v| v.as_object_mut()) else { return; };
    let Some(ticker_entry) = market.get_mut(ticker).and_then(|v| v.as_object_mut()) else { return; };
    ticker_entry.insert("sector".to_string(), Value::String(sector_slug.to_string()));
}

/// Walk `pending_recommandations[]` and inject `sector` from
/// `run_state.market[ticker].sector` for each reco. Returns the count of recos
/// that were enriched (newly populated `sector`).
///
/// Used by `report::persist_retry_global_synthesis` as the post-processing step
/// (P1-57 — option B in the audit). The LLM does not reliably copy `sector`
/// from its tool context into reco output (0/15 in production run
/// `019e3c8cce63`), so we backfill from authoritative run_state data.
/// Pure function — no I/O, no global state. Easy to unit-test.
///
/// Behaviour :
///   - Preserves existing `sector` on a reco (never overwrites — caller
///     wins if the LLM did populate it).
///   - Skips recos with no `ticker` (no key to look up).
///   - Skips recos when no `sector` exists in `market[ticker]`.
///   - Returns 0 when `recos` or `market` are not arrays/objects.
pub(crate) fn enrich_recommendations_with_sector(
    recos: &mut Value,
    market: &Value,
) -> usize {
    let Some(market_obj) = market.as_object() else { return 0; };
    let Some(recos_arr) = recos.as_array_mut() else { return 0; };
    let mut count = 0usize;
    for reco in recos_arr.iter_mut() {
        let Some(reco_obj) = reco.as_object_mut() else { continue; };
        // Preserve existing sector — never overwrite.
        if reco_obj.get("sector").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some() {
            continue;
        }
        let ticker = reco_obj.get("ticker").and_then(|v| v.as_str()).unwrap_or("").to_ascii_uppercase();
        if ticker.is_empty() {
            continue;
        }
        let sector_opt = market_obj
            .get(&ticker)
            .or_else(|| market_obj.get(&ticker.to_lowercase()))
            .and_then(|t| t.get("sector"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        if let Some(slug) = sector_opt {
            reco_obj.insert("sector".to_string(), Value::String(slug.to_string()));
            count += 1;
        }
    }
    count
}

/// P1-61 — enrich each recommendation with `current_weight_pct` (computed
/// from `positions`) and `weight_delta_pct` (computed against the LLM-supplied
/// `target_weight_pct`).
///
/// **Always-additive** per `feedback_snapshot_ui_contract`: never touches
/// `target_weight_pct` (LLM output), only ADDS `current_weight_pct` and (when
/// computable) `weight_delta_pct`.
///
/// Behaviour:
///   - Recos for watchlist tickers (no matching position): `current_weight_pct = 0.0`
///     and `weight_delta_pct = target_weight_pct` (delta == target).
///   - Recos where `target_weight_pct` is absent or null: `current_weight_pct`
///     is still attached; `weight_delta_pct` stays absent (CONSERVER /
///     SURVEILLANCE case — no allocation intent to compare against).
///   - Returns the number of recommendations that received any weight field
///     (useful for logging).
///
/// Reuses `compute_weight_pct` from `services::native_collection_helpers`
/// so the formula has exactly one source of truth — see the helper docstring.
pub(crate) fn enrich_recommendations_with_weight(
    recos: &mut Value,
    positions: &[Value],
) -> usize {
    let Some(recos_arr) = recos.as_array_mut() else { return 0; };

    // Compute total portfolio value once. Positions without a `valeur_actuelle`
    // contribute 0 — matches `top_positions_for` semantics.
    let total_value: f64 = positions
        .iter()
        .map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum();

    let mut count = 0usize;
    for reco in recos_arr.iter_mut() {
        let Some(reco_obj) = reco.as_object_mut() else { continue; };
        let ticker = reco_obj
            .get("ticker")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if ticker.is_empty() {
            continue;
        }
        // Look up the position with the matching ticker (case-insensitive).
        // Recos for watchlist tickers have no position → current = 0.0.
        let position_value: f64 = positions
            .iter()
            .find(|p| {
                p.get("ticker")
                    .and_then(|v| v.as_str())
                    .map(|t| t.eq_ignore_ascii_case(&ticker))
                    .unwrap_or(false)
            })
            .and_then(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);

        let current = crate::native_collection_helpers::compute_weight_pct(
            position_value,
            total_value,
        );
        reco_obj.insert("current_weight_pct".to_string(), json!(current));

        // weight_delta_pct only when target_weight_pct is a finite number.
        // The LLM emits `null` for CONSERVER/SURVEILLANCE — we skip delta then.
        if let Some(target) = reco_obj
            .get("target_weight_pct")
            .and_then(|v| v.as_f64())
            .filter(|f| f.is_finite())
        {
            let raw_delta = target - current;
            let delta = (raw_delta * 10.0).round() / 10.0;
            reco_obj.insert("weight_delta_pct".to_string(), json!(delta));
        }
        count += 1;
    }
    count
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
/// Merge two recommendations: never overwrite non-empty with empty.
pub fn merge_recommendation(existing: &Value, new: &Value) -> Value {
    let mut merged = existing.clone();
    if let (Some(base), Some(update)) = (merged.as_object_mut(), new.as_object()) {
        for (key, new_val) in update {
            let is_empty = new_val.is_null()
                || (new_val.is_string() && new_val.as_str().unwrap_or_default().trim().is_empty())
                || (new_val.as_array().is_some_and(|a| a.is_empty()));
            let existing_has_value = base.get(key).is_some_and(|v| {
                !v.is_null()
                    && !(v.is_string() && v.as_str().unwrap_or_default().trim().is_empty())
                    && !(v.as_array().is_some_and(|a| a.is_empty()))
            });
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
                        let obj = match state.as_object_mut() {
                            Some(o) => o,
                            None => continue,
                        };
                        let pending = obj
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

/// Number of DISTINCT lines covered so far, counted from `pending_recommandations`.
///
/// This is the cumulative progress source of truth for the live `line_done`
/// counter and the run narrator's ProgressSnapshot. Deduplicates by `line_id`
/// (reconstructed via `make_line_id` when the recommendation omits it) so a line
/// re-analyzed by the coverage-reprise sweep is counted once, not twice. Pure:
/// no I/O, takes an already-loaded run state.
pub(crate) fn count_covered_lines(run_state: &Value) -> usize {
    let mut covered: HashSet<String> = HashSet::new();
    if let Some(arr) = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
    {
        for rec in arr {
            let lid = make_line_id(rec);
            if !lid.is_empty() {
                covered.insert(lid);
            }
        }
    }
    covered.len()
}

// The expected-line-id set is derived by the single source of truth in
// `report.rs` (`crate::report::derive_expected_line_ids`), which computes the
// same position+watchlist line-id set and additionally sorts it for
// deterministic ordering. The duplicate that previously lived here was removed
// to keep the coverage contract DRY — `tool_check_coverage` now delegates to
// the shared helper.

// ── Alfred API client (delegates to alfred_api_client with HMAC auth) ───────

fn api_persist_extracted_fundamentals(ticker: &str, isin: &str, extracted: &Value) {
    crate::alfred_api_client::persist_extracted_fundamentals(ticker, isin, extracted);
}

fn api_persist_shared_insights(ticker: &str, isin: &str, insights: &Value, sector: Option<&str>, sector_analysis: Option<&str>) {
    crate::alfred_api_client::persist_shared_insights(ticker, isin, insights, sector, sector_analysis);
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
            "description": "Persist shared analysis insights to the Alfred API cache. Optionally includes sector classification and sector analysis memo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ticker": { "type": "string" },
                    "isin": { "type": "string" },
                    "insights": { "type": "string", "description": "JSON string of insights object" },
                    "sector": { "type": "string", "description": "GICS sector slug (e.g. energy, tech, financials)" },
                    "sector_analysis": { "type": "string", "description": "2-3 phrases sur le positionnement sectoriel (COT, tendances macro)" }
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

    // Phase 1: forward cross_account_context so the synthesis turn (codex MCP)
    // sees the same cross-account block as the native/native-oauth paths.
    // Themes are freshly aggregated from line_memory so the LLM sees the
    // most recent state, not the one frozen at collection time.
    let mut cross_account_context = run_state
        .get("cross_account_context")
        .cloned()
        .unwrap_or(Value::Null);
    if cross_account_context.is_object() {
        let themes = crate::native_mcp_analysis::aggregate_cross_account_themes(&run_state);
        if let Some(obj) = cross_account_context.as_object_mut() {
            obj.insert("cross_account_themes".to_string(), themes);
        }
    }

    Ok(json!({
        "portfolio_summary": {
            "valeur_totale": portfolio.get("valeur_totale").cloned().unwrap_or(Value::Null),
            "plus_value_totale": portfolio.get("plus_value_totale").cloned().unwrap_or(Value::Null),
            "liquidites": portfolio.get("liquidites").cloned().unwrap_or(Value::Null),
            // P0-81: parity — the codex MCP agent must see the same known/unknown
            // distinction the native prompt builders render. false → the agent
            // must not treat liquidites as 0 (the tool description spells out the
            // rule). Defaults to false (unknown) when absent from run_state.
            "liquidites_known": portfolio.get("liquidites_known").and_then(|v| v.as_bool()).unwrap_or(false),
            "position_count": positions.len(),
        },
        "lines": all_lines,
        "agent_guidelines": as_text(run_state.get("agent_guidelines")),
        "watchlist": watchlist_items,
        "cross_account_context": cross_account_context,
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

/// Extract recent transactions and orders for a specific ticker from the run snapshot.
/// Only includes transactions belonging to the account being analyzed.
/// Matches by ticker, ISIN, or position name (e.g. ticker=ALSTI, name="STIF SA" matches "ACHAT COMPTANT - STIF").
/// Returns a compact array of {date, action, amount, name} — max 10 items, newest first.
fn build_line_activity(run_state: &Value, ticker: &str, isin: &str, position_name: &str) -> Value {
    let mut items: Vec<Value> = Vec::new();
    let ticker_lower = ticker.to_lowercase();
    let isin_lower = isin.to_lowercase();
    // Extract significant words from position name (>= 3 chars) for fuzzy matching
    let name_words: Vec<String> = position_name.to_lowercase()
        .split_whitespace()
        .filter(|w| w.len() >= 3 && !["sa", "se", "nv", "plc", "inc", "spa", "group", "groupe"].contains(w))
        .map(String::from)
        .collect();

    // The account being analyzed (e.g. "Plan Epargne en Action")
    let run_account = run_state.get("account").and_then(|v| v.as_str()).unwrap_or("");
    let run_account_lower = run_account.to_lowercase();

    // Transactions are at top-level run_state.transactions (Finary format)
    if let Some(txs) = run_state.get("transactions").and_then(|v| v.as_array()) {
        for tx in txs {
            // Filter by account: transaction.account.display_name or .name must match run account
            let tx_account = tx.get("account")
                .and_then(|a| a.get("display_name").or(a.get("name")))
                .and_then(|v| v.as_str()).unwrap_or("");
            if !run_account_lower.is_empty() && !tx_account.to_lowercase().contains(&run_account_lower) {
                continue;
            }

            let name = tx.get("name").or(tx.get("display_name"))
                .and_then(|v| v.as_str()).unwrap_or("");
            let simplified = tx.get("simplified_name").and_then(|v| v.as_str()).unwrap_or("");
            let name_lower = format!("{name} {simplified}").to_lowercase();

            // Match by ticker, ISIN, or position name words in the transaction name
            let matches = (!ticker_lower.is_empty() && name_lower.contains(&ticker_lower))
                || (!isin_lower.is_empty() && name_lower.contains(&isin_lower))
                || name_words.iter().any(|w| {
                    // Check if a name word appears as a distinct token in the transaction
                    name_lower.split(&['-', ' ', ',', '(', ')'][..])
                        .any(|part| part.trim() == w.as_str())
                })
                || name_lower.split(&['-', ' ', ','][..])
                    .any(|part| {
                        let p = part.trim();
                        (!ticker_lower.is_empty() && p == ticker_lower)
                            || (!isin_lower.is_empty() && p == isin_lower)
                    });

            if matches {
                let date = tx.get("display_date").or(tx.get("date"))
                    .and_then(|v| v.as_str()).unwrap_or("").to_string();
                let value = tx.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let action = if value > 0.0 { "vente" } else { "achat" };
                items.push(json!({
                    "date": date,
                    "action": action,
                    "amount_eur": value.abs(),
                    "name": truncate_activity_name(name, 60),
                }));
            }
        }
    }

    // Also check run_state.orders (securities orders from Finary)
    if let Some(orders) = run_state.get("orders").and_then(|v| v.as_array()) {
        for order in orders {
            // Filter by account
            let order_account = order.get("account")
                .and_then(|a| a.get("display_name").or(a.get("name")))
                .and_then(|v| v.as_str()).unwrap_or("");
            if !run_account_lower.is_empty() && !order_account.to_lowercase().contains(&run_account_lower) {
                continue;
            }

            let security_name = order.get("security")
                .and_then(|s| s.get("name").or(s.get("symbol")))
                .and_then(|v| v.as_str()).unwrap_or("");
            let security_lower = security_name.to_lowercase();
            if security_lower.contains(&ticker_lower) || security_lower.contains(&isin_lower) {
                let date = order.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let side = order.get("side").and_then(|v| v.as_str()).unwrap_or("buy");
                let quantity = order.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let price = order.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                items.push(json!({
                    "date": date,
                    "action": side,
                    "quantity": quantity,
                    "price": price,
                    "amount_eur": quantity * price,
                    "name": truncate_activity_name(security_name, 60),
                }));
            }
        }
    }

    // Sort by date descending, cap at 10
    items.sort_by(|a, b| {
        let da = a.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let db = b.get("date").and_then(|v| v.as_str()).unwrap_or("");
        db.cmp(da)
    });
    items.truncate(10);

    json!(items)
}

fn truncate_activity_name(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
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

    // Technical snapshot (250d SMA/RSI/MACD/ATR) — populated by the collection
    // worker. The server endpoint may not be deployed yet → expect Null in
    // that case. The prompt builders render "TECHNIQUE : non disponible"
    // when this is missing.
    let technical_snapshot = run_state
        .get("technicals")
        .and_then(|t| t.get(&ticker).or_else(|| t.get(&ticker.to_lowercase())))
        .cloned()
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

    // Sector classification + COT positioning
    let name = position_row.get("nom").and_then(|v| v.as_str()).unwrap_or("");
    // Canonical Yahoo symbol — resolved during collection and stashed on the
    // position row. Forwarded so the server can route on it for sector/cot
    // lookups (cross-account dedup: PEA STMPA + CTO STM share canonical).
    let canonical_opt = position_row
        .get("resolved_symbol")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let sector_resp = crate::enrichment::fetch_sector(&ticker, name, isin, canonical_opt).ok();
    let sector_slug = sector_resp.as_ref()
        .and_then(|r| r.get("sector").and_then(|v| v.as_str()))
        .unwrap_or("");
    let sector_analysis = sector_resp.as_ref()
        .and_then(|r| r.get("sector_analysis").cloned())
        .unwrap_or(Value::Null);
    let cot_data = if !sector_slug.is_empty() {
        crate::enrichment::fetch_cot(&ticker, isin, canonical_opt)
            .ok()
            .and_then(|r| r.get("cot").cloned())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    // P1-57 — persist sector slug into `run_state.market[ticker].sector` so the
    // UI (home allocation widget, line-modal chip) can read it without round-
    // tripping through the LLM. Additive parallel field per the snapshot UI
    // contract (`feedback_snapshot_ui_contract`): we never modify existing
    // market keys (price, pe_ratio, etc.), only insert a new `sector` key.
    if !sector_slug.is_empty() {
        let _ = crate::run_state_cache::patch(data_dir, &run_id, |rs| {
            apply_sector_to_market(rs, &ticker, sector_slug);
        });
    }

    // Line memory (cross-run) — schema: { "by_ticker": { "AAPL": {...} } }.
    // v0.3 (#22): honour canonical_line_memory_key so a cross-account dup
    // surfaces the same persistent history regardless of which broker ticker
    // the LLM queried for. Legacy fallback to raw ticker key keeps pre-v0.3
    // entries readable.
    let line_memory = {
        let mem_path = line_memory_path(data_dir);
        if mem_path.exists() {
            read_json(&mem_path)
                .ok()
                .map(|mem| crate::native_mcp_analysis::read_line_memory_entry(&mem, &ticker, canonical_opt))
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    };
    // P1-82 Layer-1 ban safety net (parity with native/oauth's
    // build_memory_for_prompt): strip banned-run signal_history entries and
    // recompute derived fields before the codex agent sees them.
    let line_memory = {
        let banned = crate::run_deletion::load_banned_run_ids();
        crate::native_mcp_analysis::sanitize_entry_for_banned_runs(&line_memory, &banned)
    };

    // Quality indicators — per-run `run_state.quality.by_ticker` map, NOT
    // line-memory. Ticker key is the raw broker ticker (no canonical dedup
    // applies — quality is scoped to the active run only).
    let quality = run_state
        .get("quality")
        .and_then(|q| q.get("by_ticker"))
        .and_then(|bt| bt.get(&ticker)) // LINT-ALLOW: run_state.quality map, not line-memory
        .cloned()
        .unwrap_or(Value::Null);

    // Recent transactions/orders for this ticker (from snapshot)
    let position_name = position_row.get("nom").and_then(|v| v.as_str()).unwrap_or("");
    let activity = build_line_activity(&run_state, &ticker, isin, position_name);

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

    // Build per-block collection_quality sub-object (additive — never modifies
    // existing fields per the snapshot UI contract). The UI reads this to
    // render per-block freshness badges (✓ frais / ⚠ stale / ✗ indispo).
    // The LLM also reads this to weight evidence quality.
    let collection_quality = build_collection_quality(
        &market_data,
        &technical_snapshot,
        &news,
        &shared_insights,
        &sector_slug,
        &cot_data,
        &line_memory,
    );

    Ok(json!({
        "line_id": line_id,
        "line_type": line_type,
        "ticker": ticker,
        "sector": sector_slug,
        "position": position_row,
        "market_data": market_data,
        "news": news,
        "shared_insights": shared_insights,
        "sector_cot": {
            "sector": sector_slug,
            "sector_analysis": sector_analysis,
            "cot": cot_data,
        },
        "activity": activity,
        "line_memory": line_memory,
        "quality": quality,
        // ── Additive fields (snapshot UI contract: never replaces existing) ──
        "technical_snapshot": technical_snapshot,
        "collection_quality": collection_quality,
    }))
}

/// Build the per-block `collection_quality` sub-object surfaced in the line
/// modal UI and consumed by the LLM. Each block reports `{source, as_of,
/// quality}` (+ optional extras like `samples` for technical or `count` for
/// news). Missing data → `quality: "unavailable"`.
///
/// Quality enum: `"fresh" | "stale" | "degraded" | "unavailable"`.
/// - `fresh`     — server reports it directly, OR data is present and recent
/// - `stale`     — present but older than the freshness window
/// - `degraded`  — present but partial (e.g. < 60 OHLC samples)
/// - `unavailable` — block returned no usable data
fn build_collection_quality(
    market: &Value,
    technical: &Value,
    news: &Value,
    insights: &Value,
    sector_slug: &str,
    cot: &Value,
    line_memory: &Value,
) -> Value {
    let now = now_iso();

    // Spot: derive from market_data
    let spot_source = market.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let has_price = market.get("prix_actuel").and_then(|v| v.as_f64()).is_some();
    let spot = json!({
        "source": if spot_source.is_empty() { Value::Null } else { json!(spot_source) },
        "as_of": now.clone(),
        "quality": if has_price { "fresh" } else { "unavailable" },
    });

    // Fundamentals: derive from market_data (same row exposes PER, margin, etc.)
    let has_pe = market.get("pe_ratio").and_then(|v| v.as_f64()).is_some();
    let has_margin = market.get("profit_margin").and_then(|v| v.as_f64()).is_some();
    let has_growth = market.get("revenue_growth").and_then(|v| v.as_f64()).is_some();
    let has_debt = market.get("debt_to_equity").and_then(|v| v.as_f64()).is_some();
    let fundamentals_count = [has_pe, has_margin, has_growth, has_debt]
        .iter()
        .filter(|b| **b)
        .count();
    let fundamentals_quality = match fundamentals_count {
        4 => "fresh",
        2..=3 => "degraded",
        1 => "degraded",
        _ => "unavailable",
    };
    let fundamentals = json!({
        "source": if spot_source.is_empty() { Value::Null } else { json!(spot_source) },
        "as_of": now.clone(),
        "quality": fundamentals_quality,
    });

    // Technical: from technical_snapshot envelope itself
    let technical_block = if technical.is_object() {
        let server_quality = technical.get("quality").and_then(|v| v.as_str());
        let server_source = technical.get("source").and_then(|v| v.as_str()).unwrap_or("alphavantage:daily");
        let server_as_of = technical.get("as_of").and_then(|v| v.as_str()).unwrap_or(&now);
        let samples = technical.get("samples").and_then(|v| v.as_u64()).unwrap_or(0);
        json!({
            "source": server_source,
            "as_of": server_as_of,
            "quality": server_quality.unwrap_or(if samples >= 200 { "fresh" } else if samples >= 60 { "degraded" } else { "unavailable" }),
            "samples": samples,
        })
    } else {
        json!({
            "source": Value::Null,
            "as_of": Value::Null,
            "quality": "unavailable",
        })
    };

    // News: from articles count
    let articles_count = news
        .get("articles")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let news_quality = if articles_count == 0 {
        "unavailable"
    } else if articles_count < 2 {
        "degraded"
    } else {
        "fresh"
    };
    let news_block = json!({
        "source": "searxng",
        "as_of": now.clone(),
        "quality": news_quality,
        "count": articles_count,
    });

    // Sector + COT: derived from sector_slug presence
    let sector_quality = if sector_slug.is_empty() {
        "unavailable"
    } else if cot.is_object() {
        "fresh"
    } else {
        "degraded"
    };
    let sector_cot = json!({
        "source": "cached",
        "as_of": now.clone(),
        "quality": sector_quality,
    });

    // Insights: derived from shared_insights presence
    let insights_block = json!({
        "source": "shared",
        "as_of": now.clone(),
        "quality": if insights.is_object() && !insights.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            "fresh"
        } else {
            "unavailable"
        },
    });

    // Memory: line_memory presence
    let memory_block = json!({
        "source": "local",
        "as_of": now.clone(),
        "quality": if line_memory.is_object() && !line_memory.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            "fresh"
        } else {
            "unavailable"
        },
    });

    json!({
        "spot": spot,
        "fundamentals": fundamentals,
        "technical": technical_block,
        "news": news_block,
        "sector_cot": sector_cot,
        "insights": insights_block,
        "memory": memory_block,
    })
}

/// Pure classifier — returns `(hard_issues, soft_warnings)` for a recommendation.
///
/// Extracted from `tool_validate_recommendation` per
/// `feedback_pure_helper_for_async_testability` so the validation rules can be
/// unit-tested without spinning up a temp `data_dir`, run state, or progress
/// sidecar. The MCP tool wraps this helper and adds I/O concerns (retry
/// bookkeeping, sidecar writes, progress events).
///
/// Rules:
///   - Hard (rejects + retries up to `MAX_VALIDATION_RETRIES`):
///       `synthese_too_short` — under 80 chars
///       `invalid_conviction` — not in {faible, moderee, forte}
///       `invalid_signal`     — not in the 7-signal enum
///       `invalid_line_id_format` — missing or no `:` separator
///       `invalid_target_weight_pct` — present, non-null, outside `[0, 100]`
///   - Soft (stored, surfaced in `warnings`):
///       `analyse_technique_empty` / `analyse_fondamentale_empty` / `analyse_sentiment_empty`
///       `raisons_principales_insufficient` — under 2 entries
///       `action_recommandee_empty`
///       `deep_news_summary_empty`
///       `target_weight_too_high` — > 30 (P1-61, overridable via synthese)
pub(crate) fn classify_recommendation_issues(rec: &Value) -> (Vec<String>, Vec<String>) {
    let mut hard_issues: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    // ── Hard blockers ──
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

    let line_id = as_text(rec.get("line_id"));
    if line_id.is_empty() || !line_id.contains(':') {
        hard_issues.push("invalid_line_id_format".to_string());
    }

    // P1-61 — target_weight_pct range validation.
    // Acceptable: absent (key missing) OR explicit `null` (LLM marked
    // CONSERVER/SURVEILLANCE). When present and not null:
    //   - must parse as a finite f64
    //   - must be in [0, 100]   → hard issue otherwise
    //   - >30 emits a SOFT warning (overridable via synthese justification)
    if let Some(target_value) = rec.get("target_weight_pct") {
        if !target_value.is_null() {
            match target_value.as_f64() {
                Some(f) if f.is_finite() && (0.0..=100.0).contains(&f) => {
                    if f > 30.0 {
                        warnings.push("target_weight_too_high".to_string());
                    }
                }
                _ => hard_issues.push("invalid_target_weight_pct".to_string()),
            }
        }
    }

    // ── Soft warnings ──
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

    (hard_issues, warnings)
}

fn tool_validate_recommendation(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let rec_str = as_text(params.get("recommendation"));
    let mut rec: Value =
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
    let _is_watchlist = line_id.starts_with("watchlist:");

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

    let (hard_issues, warnings) = classify_recommendation_issues(&rec);

    // Re-derive these locals — they're used by progress events below.
    let synthese = as_text(rec.get("synthese"));
    let signal = as_text(rec.get("signal")).to_ascii_uppercase();

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

    // P2-6 — apply data-quality post-processing AFTER validation so the
    // validator sees the LLM's actual conviction value, but BEFORE persist
    // so the downgrade is visible in every downstream consumer (sidecar
    // JSONL → run_state → line_memory → synthesis cross-check → UI). The
    // guard runs at this single funnel point shared by all 3 LLM modes
    // (codex / native / native-oauth) per the parity contract — see
    // `docs/llm-mode-parity-contract.md`.
    if crate::llm_post_processing::enforce_data_quality_guards(&mut rec) {
        let ticker_for_log = as_text(rec.get("ticker"));
        let signal_for_log = as_text(rec.get("signal"));
        crate::debug_log(&format!(
            "[p2-6] data_quality=fundamentals_missing — downgraded conviction to degradee \
             for {signal_for_log} {ticker_for_log} (run {run_id})"
        ));
    }

    // Valid — write to sidecar file (main process merges after each batch)
    append_mcp_result(data_dir, &run_id, &json!({
        "type": "recommendation",
        "line_id": line_id,
        "recommendation": rec,
        "at": now_iso(),
    }));

    // Progress for the `line_done` event (consumed by the run narrator's
    // ProgressSnapshot and the live UI counter).
    //
    // `completed` is the number of DISTINCT lines covered so far. It is derived
    // from `pending_recommandations` (disk + sidecar overlay, deduped by line_id
    // inside `load_run_state`) — the cumulative source of truth — NOT from a raw
    // line count of the `_mcp_results.jsonl` sidecar. The sidecar is renamed away
    // by `merge_mcp_results` after every batch, so counting its lines reset the
    // count to ~1 each batch in native/native-oauth mode (the narrator then
    // reported "1/35" and concluded Alfred was stuck/looping). `pending_recommandations`
    // accumulates across batches and is the same set the coverage gate counts.
    let run_state = load_run_state(data_dir, &run_id)?;
    let completed = count_covered_lines(&run_state);
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
    // P2-6 — propagate the data_quality flag into live events so the UI can
    // render the warning badge during the run (not only on report re-open).
    // Null when absent so the JS side can treat it as a tri-state without
    // sniffing for missing keys.
    let data_quality_flag = rec.get("data_quality").cloned().unwrap_or(Value::Null);

    append_progress(
        data_dir,
        &run_id,
        &json!({
            "type": "line_done",
            "ticker": ticker_part,
            "recommendation": {
                "signal": signal,
                "conviction": conviction_text,
                "synthese": synthese_short,
                "data_quality": data_quality_flag,
            },
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
        "recommendation": {
            "signal": signal,
            "conviction": conviction_text,
            "synthese": synthese_short,
            "data_quality": data_quality_flag,
        },
        "line_progress": { "completed": completed, "total": total },
    }));

    let mut result = json!({
        "ok": true,
        "stored": true,
        "issues": [],
    });
    if !warnings.is_empty() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("warnings".to_string(), json!(warnings));
        }
    }
    Ok(result)
}

fn tool_validate_synthesis(data_dir: &Path, params: &Value) -> Result<Value> {
    let run_id = as_text(params.get("run_id"));
    let synthese_marche = as_text(params.get("synthese_marche"));
    let actions_str = as_text(params.get("actions_immediates"));
    let prochaine_analyse = as_text(params.get("prochaine_analyse"));
    let opportunites_watchlist = as_text(params.get("opportunites_watchlist"));

    let mut actions: Vec<Value> = serde_json::from_str(&actions_str)
        .map_err(|e| anyhow!("invalid_actions_immediates_json:{e}"))?;

    // P2-7 — backfill `limit_price` and `estimated_amount_eur` from the
    // rationale string when the LLM left them null. Runs at this single
    // funnel point shared by all 3 LLM modes (codex / native / native-oauth)
    // per the parity contract. Pure post-processing: never overwrites a
    // populated value; never invents data when the rationale has no EUR
    // price token.
    {
        let mut actions_value = Value::Array(actions);
        let mutated_count =
            crate::llm_post_processing::backfill_actions_immediates(&mut actions_value);
        if mutated_count > 0 {
            crate::debug_log(&format!(
                "[p2-7] backfilled {mutated_count} action_immediate(s) for run {run_id}"
            ));
        }
        actions = actions_value
            .as_array()
            .cloned()
            .unwrap_or_default();
    }

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

    // Expected set + missing gap come from the single source of truth in
    // `report.rs` so the synthesis-turn re-analysis loop and this tool agree
    // on coverage byte-for-byte. `make_line_id` reconstructs `type:ticker`
    // when the LLM omits `line_id` — identical to `report::as_line_id`.
    let expected = crate::report::derive_expected_line_ids(&run_state);
    let expected_set: HashSet<String> = expected.iter().cloned().collect();
    let missing = crate::report::missing_line_ids(&run_state);

    let pending = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut covered = HashSet::new();
    let mut duplicates = Vec::new();
    let mut unexpected = Vec::new();

    for rec in &pending {
        let lid = make_line_id(rec);
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

    // Evict (flush dirty cache to disk, then DROP the in-memory entry) BEFORE
    // composing the report. `persist_retry_global_synthesis` reads run_state
    // from disk and writes the composed report back to disk, bypassing the
    // cache entirely. If the cache entry were still resident, the 2s background
    // flush (or a later evict) would overwrite the disk file with the now-stale
    // cache snapshot — clobbering composed_payload, validation_corrections, and
    // the completed orchestration status (the "Partial latest-run artifact" bug
    // in native / native-oauth mode). Evicting first makes disk the single
    // source of truth for the whole compose step: the read sees the freshly
    // flushed state (positions + watchlist.items + recommendations + done
    // line_status, all written through the cache), and nothing flushes over the
    // write afterwards. (Same fix independently reached by bugs #1 and #4.)
    crate::run_state_cache::evict(&run_id);

    // Delegate to the same function the legacy path uses — ensures identical format,
    // validation, composed_payload writing, report artifacts, orchestration status.
    let result = crate::report::persist_retry_global_synthesis(&run_id, &draft)?;

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

    // Extract optional sector fields from params
    let sector = params.get("sector").and_then(|v| v.as_str());
    let sector_analysis = params.get("sector_analysis").and_then(|v| v.as_str());
    api_persist_shared_insights(&ticker, &isin, &insights, sector, sector_analysis);

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

fn handle_request(data_dir: &Path, msg: &Value, tool_filter: Option<&[String]>) -> Option<Value> {
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
            let all = tool_definitions();
            let filtered = if let Some(filter) = tool_filter {
                all.as_array()
                    .map(|arr| arr.iter()
                        .filter(|t| t.get("name").and_then(|v| v.as_str()).map(|n| filter.iter().any(|f| f == n)).unwrap_or(false))
                        .cloned().collect::<Vec<_>>())
                    .map(|v| json!(v))
                    .unwrap_or(all)
            } else {
                all
            };
            Some(rpc_ok(
                &id,
                json!({
                    "tools": filtered,
                }),
            ))
        }

        "tools/call" => {
            let tool_name = as_text(params.get("name"));
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            // Reject tools not in the filter
            if let Some(filter) = tool_filter {
                if !filter.iter().any(|f| f == &tool_name) {
                    log(&format!("tools/call: {tool_name} REJECTED (not in filter)"));
                    return Some(rpc_ok(&id, json!({
                        "content": [{"type": "text", "text": format!("Tool '{tool_name}' not available in this turn.")}],
                        "isError": true
                    })));
                }
            }
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
    run_stdio_server_with_filter(data_dir, None)
}

pub fn run_stdio_server_filtered(data_dir: PathBuf, allowed_tools: Vec<String>) -> anyhow::Result<()> {
    run_stdio_server_with_filter(data_dir, Some(allowed_tools))
}

fn run_stdio_server_with_filter(data_dir: PathBuf, tool_filter: Option<Vec<String>>) -> anyhow::Result<()> {
    log(&format!("starting MCP server, data_dir={}, filter={:?}", data_dir.display(), tool_filter));

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

        if let Some(response) = handle_request(&data_dir, &msg, tool_filter.as_deref()) {
            let response_str = serde_json::to_string(&response).unwrap_or_default();
            let _ = writeln!(stdout_lock, "{}", response_str);
            let _ = stdout_lock.flush();
        }
    }

    log("stdin closed, shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── apply_sector_to_market ─────────────────────────────────────

    #[test]
    fn apply_sector_writes_to_market_ticker() {
        let mut rs = json!({
            "market": {
                "AAPL": { "prix_actuel": 150.0, "pe_ratio": 28.0 }
            }
        });
        apply_sector_to_market(&mut rs, "AAPL", "tech");
        assert_eq!(rs["market"]["AAPL"]["sector"], "tech");
        // Existing keys preserved (snapshot UI contract).
        assert_eq!(rs["market"]["AAPL"]["prix_actuel"], 150.0);
        assert_eq!(rs["market"]["AAPL"]["pe_ratio"], 28.0);
    }

    #[test]
    fn apply_sector_is_idempotent_on_repeat_call() {
        let mut rs = json!({
            "market": {
                "AAPL": { "prix_actuel": 150.0 }
            }
        });
        apply_sector_to_market(&mut rs, "AAPL", "tech");
        apply_sector_to_market(&mut rs, "AAPL", "tech");
        // Second call must be a no-op (no duplication, no panic).
        assert_eq!(rs["market"]["AAPL"]["sector"], "tech");
        // Object shape unchanged.
        let market_obj = rs["market"]["AAPL"].as_object().unwrap();
        assert_eq!(market_obj.len(), 2); // prix_actuel + sector
    }

    #[test]
    fn apply_sector_overwrites_stale_slug() {
        // A later enrichment call must update the slug — alfred-api may
        // re-classify a position after a cache refresh.
        let mut rs = json!({
            "market": {
                "AAPL": { "prix_actuel": 150.0, "sector": "consumer_discretionary" }
            }
        });
        apply_sector_to_market(&mut rs, "AAPL", "tech");
        assert_eq!(rs["market"]["AAPL"]["sector"], "tech");
    }

    #[test]
    fn apply_sector_skips_when_ticker_absent_from_market() {
        // Ticker not in market → graceful no-op (no panic). Sector will be
        // re-persisted on the next enrichment cycle once market data lands.
        let mut rs = json!({
            "market": {
                "AAPL": { "prix_actuel": 150.0 }
            }
        });
        apply_sector_to_market(&mut rs, "GOOG", "tech");
        assert!(rs["market"].get("GOOG").is_none(),
            "absent ticker must not be auto-created (sector requires market data)");
        // Original entry untouched.
        assert!(rs["market"]["AAPL"].get("sector").is_none());
    }

    #[test]
    fn apply_sector_skips_when_market_missing() {
        let mut rs = json!({ "portfolio": {} });
        apply_sector_to_market(&mut rs, "AAPL", "tech");
        assert!(rs.get("market").is_none(), "must not create market obj");
    }

    #[test]
    fn apply_sector_skips_on_empty_inputs() {
        let mut rs = json!({ "market": { "AAPL": { "prix_actuel": 150.0 } } });
        apply_sector_to_market(&mut rs, "", "tech");
        apply_sector_to_market(&mut rs, "AAPL", "");
        // Neither call mutated state.
        assert!(rs["market"]["AAPL"].get("sector").is_none());
    }

    // ── enrich_recommendations_with_sector ─────────────────────────

    #[test]
    fn enrich_recommendations_backfills_sector_from_market() {
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "ACHAT" },
            { "ticker": "TTE",  "signal": "VENTE" },
            { "ticker": "UNKNOWN", "signal": "HOLD" }
        ]);
        let market = json!({
            "AAPL": { "sector": "tech" },
            "TTE":  { "sector": "energy" }
            // UNKNOWN absent → reco passes through without sector
        });
        let count = enrich_recommendations_with_sector(&mut recos, &market);
        assert_eq!(count, 2, "two recos must be enriched");
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["sector"], "tech");
        assert_eq!(arr[1]["sector"], "energy");
        assert!(arr[2].get("sector").is_none(),
            "reco without market entry stays sectorless");
    }

    #[test]
    fn enrich_recommendations_preserves_existing_sector() {
        // If the LLM did populate sector (rare but possible), don't clobber.
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "ACHAT", "sector": "llm_picked" }
        ]);
        let market = json!({
            "AAPL": { "sector": "tech" }
        });
        let count = enrich_recommendations_with_sector(&mut recos, &market);
        assert_eq!(count, 0, "existing sector preserved → no enrichment");
        assert_eq!(recos[0]["sector"], "llm_picked");
    }

    #[test]
    fn enrich_recommendations_uppercases_ticker_lookup() {
        // Recos sometimes carry lowercase tickers — market keys are uppercase.
        let mut recos = json!([
            { "ticker": "aapl", "signal": "ACHAT" }
        ]);
        let market = json!({
            "AAPL": { "sector": "tech" }
        });
        let count = enrich_recommendations_with_sector(&mut recos, &market);
        assert_eq!(count, 1);
        assert_eq!(recos[0]["sector"], "tech");
    }

    #[test]
    fn enrich_recommendations_handles_empty_inputs() {
        let mut empty_recos = json!([]);
        let market = json!({ "AAPL": { "sector": "tech" } });
        assert_eq!(enrich_recommendations_with_sector(&mut empty_recos, &market), 0);

        let mut recos = json!([{ "ticker": "AAPL" }]);
        let no_market = json!(null);
        assert_eq!(enrich_recommendations_with_sector(&mut recos, &no_market), 0);
        assert!(recos[0].get("sector").is_none());

        let mut not_array = json!({});
        assert_eq!(enrich_recommendations_with_sector(&mut not_array, &market), 0);
    }

    #[test]
    fn enrich_recommendations_skips_recos_without_ticker() {
        let mut recos = json!([
            { "signal": "ACHAT" },           // no ticker
            { "ticker": "", "signal": "X" }, // empty ticker
            { "ticker": "AAPL" }
        ]);
        let market = json!({ "AAPL": { "sector": "tech" } });
        let count = enrich_recommendations_with_sector(&mut recos, &market);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert!(arr[0].get("sector").is_none());
        assert!(arr[1].get("sector").is_none());
        assert_eq!(arr[2]["sector"], "tech");
    }

    #[test]
    fn enrich_recommendations_skips_when_market_sector_empty() {
        // market entry exists but sector is empty → reco stays sectorless.
        let mut recos = json!([{ "ticker": "AAPL" }]);
        let market = json!({ "AAPL": { "sector": "" } });
        let count = enrich_recommendations_with_sector(&mut recos, &market);
        assert_eq!(count, 0);
        assert!(recos[0].get("sector").is_none());
    }

    // ── P1-61 — classify_recommendation_issues (target_weight_pct rules) ──

    /// Build a baseline-valid reco — all required fields populated so the
    /// only thing under test is the target_weight_pct branch.
    fn baseline_valid_rec() -> Value {
        json!({
            "line_id": "position:aapl",
            "ticker": "AAPL",
            "signal": "ACHAT",
            "conviction": "moderee",
            "synthese": "Une synthese suffisamment longue pour passer la validation de 80 caracteres minimum sans probleme.",
            "analyse_technique": "tech",
            "analyse_fondamentale": "fund",
            "analyse_sentiment": "sent",
            "raisons_principales": ["a", "b"],
            "action_recommandee": "Acheter 3 titres",
            "deep_news_summary": "news",
        })
    }

    #[test]
    fn validator_accepts_null_target_weight_pct() {
        // null target → no hard issues, no target-related warnings.
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = Value::Null;
        let (hard, soft) = classify_recommendation_issues(&rec);
        assert!(hard.is_empty(), "null target must not produce hard issues: {:?}", hard);
        assert!(!soft.contains(&"target_weight_too_high".to_string()));
        assert!(!soft.contains(&"invalid_target_weight_pct".to_string()));
    }

    #[test]
    fn validator_accepts_absent_target_weight_pct() {
        // Absent key (LLM omitted it entirely) → also accepted, no warnings.
        let rec = baseline_valid_rec();
        assert!(rec.get("target_weight_pct").is_none(), "fixture sanity");
        let (hard, soft) = classify_recommendation_issues(&rec);
        assert!(hard.is_empty(), "absent target must not produce hard issues: {:?}", hard);
        assert!(!soft.contains(&"target_weight_too_high".to_string()));
    }

    #[test]
    fn validator_rejects_out_of_range_target_weight_pct() {
        // Negative → hard issue.
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = json!(-2.0);
        let (hard, _) = classify_recommendation_issues(&rec);
        assert!(hard.contains(&"invalid_target_weight_pct".to_string()),
            "negative target must hard-fail; got: {:?}", hard);

        // > 100 → hard issue.
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = json!(150.0);
        let (hard, _) = classify_recommendation_issues(&rec);
        assert!(hard.contains(&"invalid_target_weight_pct".to_string()),
            "> 100 target must hard-fail; got: {:?}", hard);

        // Non-numeric (string) → hard issue.
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = json!("forty");
        let (hard, _) = classify_recommendation_issues(&rec);
        assert!(hard.contains(&"invalid_target_weight_pct".to_string()),
            "non-numeric target must hard-fail; got: {:?}", hard);
    }

    #[test]
    fn validator_emits_soft_issue_when_target_weight_above_30() {
        // 35.0 → soft warning, no hard issue (LLM may justify in synthese).
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = json!(35.0);
        let (hard, soft) = classify_recommendation_issues(&rec);
        assert!(hard.is_empty(), "above-30 target must NOT hard-fail; got: {:?}", hard);
        assert!(soft.contains(&"target_weight_too_high".to_string()),
            "above-30 target must emit soft warning; got: {:?}", soft);
    }

    #[test]
    fn validator_accepts_in_range_target_weight_pct() {
        // Sane in-range values (0, mid, 30) accepted without any target warning.
        for &value in &[0.0_f64, 8.0, 15.5, 30.0] {
            let mut rec = baseline_valid_rec();
            rec["target_weight_pct"] = json!(value);
            let (hard, soft) = classify_recommendation_issues(&rec);
            assert!(hard.is_empty(), "in-range {value} must not hard-fail; got: {:?}", hard);
            assert!(!soft.contains(&"target_weight_too_high".to_string()),
                "in-range {value} must not warn; got: {:?}", soft);
        }
    }

    #[test]
    fn validator_target_weight_at_100_is_accepted() {
        // Boundary: exactly 100 is in-range; emits the >30 soft warning.
        let mut rec = baseline_valid_rec();
        rec["target_weight_pct"] = json!(100.0);
        let (hard, soft) = classify_recommendation_issues(&rec);
        assert!(hard.is_empty(), "100 must be in-range; got: {:?}", hard);
        assert!(soft.contains(&"target_weight_too_high".to_string()),
            "100 must emit soft warning; got: {:?}", soft);
    }

    // ── P1-61 — enrich_recommendations_with_weight ──

    #[test]
    fn attach_weight_fields_computes_current_and_delta() {
        // Portfolio total = 100 000 ; AAPL = 8 000 → current = 8.0 %
        // Target 12 → delta = +4.0
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "ACHAT", "target_weight_pct": 12.0 }
        ]);
        let positions = vec![
            json!({ "ticker": "AAPL", "valeur_actuelle": 8000.0 }),
            json!({ "ticker": "MSFT", "valeur_actuelle": 12000.0 }),
            json!({ "ticker": "GOOG", "valeur_actuelle": 80000.0 }),
        ];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(8.0));
        assert_eq!(arr[0]["weight_delta_pct"].as_f64(), Some(4.0));
        // Target is unmutated.
        assert_eq!(arr[0]["target_weight_pct"].as_f64(), Some(12.0));
    }

    #[test]
    fn attach_weight_fields_handles_watchlist_zero_current() {
        // Reco for a watchlist ticker (no matching position) → current = 0,
        // delta = target (positive only — we never go negative on a fresh entry).
        let mut recos = json!([
            { "ticker": "NVDA", "signal": "ACHAT", "target_weight_pct": 5.0 }
        ]);
        let positions = vec![
            json!({ "ticker": "AAPL", "valeur_actuelle": 8000.0 }),
        ];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(0.0));
        assert_eq!(arr[0]["weight_delta_pct"].as_f64(), Some(5.0));
    }

    #[test]
    fn attach_weight_fields_handles_null_target() {
        // Reco with null target_weight_pct (CONSERVER / SURVEILLANCE) →
        // current is still attached, delta is OMITTED.
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "CONSERVER", "target_weight_pct": null }
        ]);
        let positions = vec![
            json!({ "ticker": "AAPL", "valeur_actuelle": 5000.0 }),
            json!({ "ticker": "MSFT", "valeur_actuelle": 5000.0 }),
        ];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(50.0));
        assert!(arr[0].get("weight_delta_pct").is_none(),
            "delta must be absent when target is null");
    }

    #[test]
    fn attach_weight_fields_handles_missing_target_key() {
        // Reco with target_weight_pct entirely absent (e.g. legacy LLM that
        // never learned the field) — should NOT crash, just attach current
        // and skip delta.
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "CONSERVER" }
        ]);
        let positions = vec![
            json!({ "ticker": "AAPL", "valeur_actuelle": 1000.0 }),
            json!({ "ticker": "MSFT", "valeur_actuelle": 1000.0 }),
        ];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(50.0));
        assert!(arr[0].get("weight_delta_pct").is_none());
    }

    #[test]
    fn attach_weight_fields_handles_empty_portfolio() {
        // No positions → total = 0 → current = 0.0 (no division-by-zero panic).
        // delta = target (since current is 0).
        let mut recos = json!([
            { "ticker": "AAPL", "signal": "ACHAT", "target_weight_pct": 10.0 }
        ]);
        let positions: Vec<Value> = vec![];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(0.0));
        assert_eq!(arr[0]["weight_delta_pct"].as_f64(), Some(10.0));
    }

    #[test]
    fn attach_weight_fields_ticker_case_insensitive() {
        // LLM may emit lowercase ticker; positions use uppercase. Match must succeed.
        let mut recos = json!([
            { "ticker": "aapl", "signal": "ACHAT", "target_weight_pct": 10.0 }
        ]);
        let positions = vec![
            json!({ "ticker": "AAPL", "valeur_actuelle": 4000.0 }),
            json!({ "ticker": "MSFT", "valeur_actuelle": 6000.0 }),
        ];
        let count = enrich_recommendations_with_weight(&mut recos, &positions);
        assert_eq!(count, 1);
        let arr = recos.as_array().unwrap();
        assert_eq!(arr[0]["current_weight_pct"].as_f64(), Some(40.0));
        assert_eq!(arr[0]["weight_delta_pct"].as_f64(), Some(-30.0));
    }

    #[test]
    fn attach_weight_fields_handles_empty_inputs() {
        let mut empty_recos = json!([]);
        let positions = vec![json!({ "ticker": "AAPL", "valeur_actuelle": 1000.0 })];
        assert_eq!(enrich_recommendations_with_weight(&mut empty_recos, &positions), 0);

        let mut not_array = json!({ "foo": "bar" });
        assert_eq!(enrich_recommendations_with_weight(&mut not_array, &positions), 0);
    }

    // ── P1-61 — schema parity across the 3 line-analysis prompt builders ──

    /// BINDING `product_llm_mode_parity_2026_04`: the verbatim schema line
    /// for `target_weight_pct` must appear identically in all 3 line builders.
    /// Pinned via a single source-of-truth constant in `llm_prompts.rs`.
    #[test]
    fn recommandation_schema_parity_across_three_line_builders() {
        use crate::llm_prompts::TARGET_WEIGHT_PCT_SCHEMA_LINE;

        // build_line_analysis_prompt (codex MCP).
        let line_context = json!({
            "ticker": "AAPL",
            "type": "position",
            "row": { "nom": "Apple", "quantite": 10.0, "pru": 100.0, "valeur": 1500.0 },
            "market": {},
            "news": [],
            "shared_insights": null,
            "line_memory": null,
            "technical_snapshot": null,
            "sector_cot": null,
            "activity": [],
        });
        let run_state = json!({
            "portfolio": { "valeur_totale": 10000.0, "liquidites": 1000.0, "plus_value_totale": 100.0 },
            "run_id": "test-run",
        });
        let codex_prompt = crate::llm_prompts::build_line_analysis_prompt(
            &line_context, &run_state, None, None,
        );

        // build_native_line_prompt (native / native-oauth).
        let native_line_data = json!({
            "position": {},
            "market_data": {},
            "news": [],
            "shared_insights": null,
            "quality": {},
            "line_memory": null,
            "technical_snapshot": null,
            "sector_cot": {},
            "activity": [],
        });
        let native_prompt = crate::native_mcp_analysis::build_native_line_prompt(
            "test-run", "AAPL", "Apple", "position", &native_line_data,
        );

        // build_repair_prompt (all modes — repair pass).
        let repair_ctx = json!({
            "validation_issues": ["synthese_too_short"],
            "recommendation_to_fix": { "signal": "ACHAT" },
        });
        let repair_prompt = crate::llm_prompts::build_repair_prompt(
            &line_context, &run_state, None, &repair_ctx, None,
        );

        // Verbatim byte-equality assertion — the constant text must show up
        // unchanged in all 3 prompts. A render-time mutation (e.g. someone
        // re-wraps it) would break this guard.
        for (name, prompt) in [
            ("build_line_analysis_prompt", &codex_prompt),
            ("build_native_line_prompt", &native_prompt),
            ("build_repair_prompt", &repair_prompt),
        ] {
            assert!(
                prompt.contains(TARGET_WEIGHT_PCT_SCHEMA_LINE),
                "{name} is missing the verbatim target_weight_pct schema line.\n\
                 Expected: {TARGET_WEIGHT_PCT_SCHEMA_LINE}\n\
                 Got prompt prefix: {}",
                &prompt.chars().take(200).collect::<String>(),
            );
        }
    }
}

