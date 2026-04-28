//! MCP-driven batch analysis — multi-line Codex turns with self-validation.
//!
//! Drop-in replacement for `NativeLineDispatchQueue`. Collection stays the same;
//! instead of dispatching each line to a separate Codex turn, collected lines
//! are batched and sent as multi-line turns where Codex uses MCP tools to
//! fetch data, analyze, and self-validate each recommendation.

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use serde_json::{json, Value};

// ── Line Memory Cache (C-1 fix) ────────────────────────────────
// Same pattern as run_state_cache.rs: in-memory state, all writes go through
// cache, flushed to disk on demand. Eliminates concurrent file I/O entirely.

struct LineMemoryCache {
    store: Value,
    dirty: bool,
    loaded: bool,
}

static LINE_MEMORY_CACHE: std::sync::OnceLock<std::sync::Mutex<LineMemoryCache>> =
    std::sync::OnceLock::new();

fn lm_cache() -> &'static std::sync::Mutex<LineMemoryCache> {
    LINE_MEMORY_CACHE.get_or_init(|| {
        std::sync::Mutex::new(LineMemoryCache {
            store: json!({
                "by_ticker": {},
                "global_deep_news_banned_urls": [],
                "deep_news_rotation_cache": { "by_ticker": {} }
            }),
            dirty: false,
            loaded: false,
        })
    })
}

fn lm_default_store() -> Value {
    json!({
        "by_ticker": {},
        "global_deep_news_banned_urls": [],
        "deep_news_rotation_cache": { "by_ticker": {} }
    })
}

fn lm_path() -> std::path::PathBuf {
    crate::resolve_runtime_state_dir().join("line-memory.json")
}

/// Load line-memory.json from disk into cache (first access or reload).
fn line_memory_load() -> Value {
    let mut guard = lm_cache().lock().unwrap_or_else(|p| p.into_inner());
    if guard.loaded {
        return guard.store.clone();
    }
    let path = lm_path();
    guard.store = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .unwrap_or_else(lm_default_store)
    } else {
        lm_default_store()
    };
    guard.loaded = true;
    guard.dirty = false;
    guard.store.clone()
}

/// Read cached line-memory store. Falls back to disk if not yet loaded.
pub fn line_memory_read() -> Value {
    let guard = lm_cache().lock().unwrap_or_else(|p| p.into_inner());
    if guard.loaded {
        return guard.store.clone();
    }
    drop(guard); // release lock before disk I/O
    line_memory_load()
}

/// Mutate the cached line-memory store in memory. No disk I/O.
fn line_memory_patch<F>(mutator: F)
where
    F: FnOnce(&mut Value),
{
    let mut guard = lm_cache().lock().unwrap_or_else(|p| p.into_inner());
    if !guard.loaded {
        let path = lm_path();
        guard.store = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .unwrap_or_else(lm_default_store)
        } else {
            lm_default_store()
        };
        guard.loaded = true;
    }
    mutator(&mut guard.store);
    guard.dirty = true;
}

/// Flush cached line-memory to disk if dirty. Called at end of analysis run.
pub fn line_memory_flush_now() {
    let to_write = {
        let mut guard = lm_cache().lock().unwrap_or_else(|p| p.into_inner());
        if !guard.dirty || !guard.loaded {
            return;
        }
        guard.dirty = false;
        Some(guard.store.clone())
    };
    if let Some(store) = to_write {
        let pb = lm_path();
        if let Err(e) = crate::storage::write_json_file(&pb, &store) {
            crate::debug_log(&format!("line_memory_flush_now: write failed: {e}"));
        } else {
            crate::debug_log("line_memory_flush_now: flushed to disk");
        }
    }
}

// ── MCP results sidecar merge ───────────────────────────────────

/// Merge MCP results from the sidecar JSONL file into the run state.
/// Called by the main process after batches complete — single writer.
fn merge_mcp_results(run_id: &str, data_dir: &str) {
    let results_path = std::path::Path::new(data_dir)
        .join("runtime-state")
        .join(format!("{run_id}_mcp_results.jsonl"));

    // Atomic read: rename to .merging to prevent concurrent MCP writes from being lost
    let merging_path = results_path.with_extension("merging");
    if std::fs::rename(&results_path, &merging_path).is_err() {
        return; // No sidecar or already being merged
    }

    let content = match std::fs::read_to_string(&merging_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => { let _ = std::fs::remove_file(&merging_path); return; }
    };

    let data_path = std::path::Path::new(data_dir);
    let mut merged_count = 0;

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
                    let lid = line_id.to_string();
                    let rec_clone = rec.clone();
                    let _ = crate::run_state_cache::patch(data_path, run_id, |state| {
                        // C-2: safe access — avoid unwrap() panic on non-object state
                        let obj = match state.as_object_mut() {
                            Some(o) => o,
                            None => { crate::debug_log("merge_mcp_results: state is not an object"); return; }
                        };
                        let pending = obj
                            .entry("pending_recommandations")
                            .or_insert_with(|| json!([]));
                        if let Some(arr) = pending.as_array_mut() {
                            let existing = arr.iter().find(|r| {
                                r.get("line_id").and_then(|v| v.as_str()).unwrap_or("") == lid
                            }).cloned();
                            arr.retain(|r| {
                                r.get("line_id").and_then(|v| v.as_str()).unwrap_or("") != lid
                            });
                            let merged = if let Some(prev) = existing {
                                crate::mcp_server::merge_recommendation(&prev, &rec_clone)
                            } else {
                                rec_clone.clone()
                            };
                            arr.push(merged);
                        }
                    });
                    // Update line_status to done
                    let ticker = line_id.split(':').last().unwrap_or("");
                    if !ticker.is_empty() {
                        crate::run_state_cache::cache_line_status(run_id, ticker, json!({"status": "done"}));
                        // Sync line memory for codex mode (V2 schema)
                        let price = extract_market_price_from_run_state(data_path, run_id, ticker);
                        sync_line_memory(run_id, ticker, &rec, price);
                    }
                    merged_count += 1;
                }
            }
            "synthesis" => {
                let composed = entry.get("composed_payload").cloned().unwrap_or(json!({}));
                let _ = crate::run_state_cache::patch(data_path, run_id, |state| {
                    if let Some(obj) = state.as_object_mut() {
                        obj.insert("composed_payload".to_string(), composed.clone());
                    }
                });
                merged_count += 1;
            }
            _ => {}
        }
    }

    if merged_count > 0 {
        crate::debug_log(&format!(
            "[mcp-merge] merged {merged_count} results from sidecar for {run_id}"
        ));
        crate::run_state_cache::flush_now(run_id);
        // Clear the merged file (new results go to a fresh sidecar)
        let _ = std::fs::remove_file(&merging_path);
    }
}

// ── Batch prompt ─────────────────────────────────────────────────

/// Build a per-line prompt for the native backend. Data is pre-injected.
/// The model returns JSON only — we handle validation and persistence ourselves.
fn build_native_line_prompt(_run_id: &str, ticker: &str, nom: &str, line_type: &str, line_data: &Value) -> String {
    let position = serde_json::to_string_pretty(&line_data["position"]).unwrap_or_default();
    let market = serde_json::to_string_pretty(&line_data["market_data"]).unwrap_or_default();
    let news = serde_json::to_string_pretty(&line_data["news"]).unwrap_or_default();
    let insights = serde_json::to_string_pretty(&line_data["shared_insights"]).unwrap_or_default();
    let memory = serde_json::to_string_pretty(&line_data["line_memory"]).unwrap_or_default();
    let quality = serde_json::to_string_pretty(&line_data["quality"]).unwrap_or_default();

    // Build activity section (recent transactions/orders for this ticker)
    let activity_section = {
        let items = line_data.get("activity").and_then(|v| v.as_array());
        match items {
            Some(arr) if !arr.is_empty() => {
                let mut lines = vec!["\nHistorique des operations recentes:".to_string()];
                for item in arr.iter().take(10) {
                    let date = item.get("date").and_then(|v| v.as_str()).unwrap_or("?");
                    let action = item.get("action").and_then(|v| v.as_str()).unwrap_or("?");
                    let amount = item.get("amount_eur").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    lines.push(format!("- {date}: {action} — {amount:.0}€ ({name})"));
                }
                lines.join("\n")
            }
            _ => String::new(),
        }
    };

    // Build sector COT section (human-readable, not raw JSON)
    let sector_section = {
        let sc = &line_data["sector_cot"];
        let sector = sc.get("sector").and_then(|v| v.as_str()).unwrap_or("");
        if sector.is_empty() {
            String::new()
        } else {
            let mut lines = vec![format!("\nPositionnement sectoriel ({}):", sector.to_uppercase())];
            if let Some(cot_obj) = sc.get("cot") {
                if let Some(contracts) = cot_obj.get("contracts").and_then(|v| v.as_array()) {
                    for item in contracts {
                        let c = item.get("contract").and_then(|v| v.as_str()).unwrap_or("?");
                        let net = item.get("noncomm_net").and_then(|v| v.as_i64()).unwrap_or(0);
                        let chg = item.get("change_noncomm_net").and_then(|v| v.as_i64()).unwrap_or(0);
                        let sent = item.get("sentiment").and_then(|v| v.as_str()).unwrap_or("?");
                        let sign = if chg >= 0 { "+" } else { "" };
                        lines.push(format!("- {c}: net speculateurs = {net} ({sign}{chg}), sentiment = {sent}"));
                    }
                }
            }
            if let Some(sa) = sc.get("sector_analysis").and_then(|v| v.as_str()) {
                if !sa.is_empty() { lines.push(format!("- Memo sectoriel: {sa}")); }
            }
            lines.join("\n")
        }
    };

    format!(
        r#"Tu es Alfred, un conseiller financier bienveillant. Analyse cette ligne.

Ligne: {line_type}:{ticker} ({nom})

=== DONNEES ===

Position:
{position}

Donnees de marche:
{market}

Actualites:
{news}

Insights partages:
{insights}
{sector_section}
{activity_section}

Memoire precedente:
{memory}

Qualite des donnees:
{quality}

=== INSTRUCTIONS ===

Reponds UNIQUEMENT avec un objet JSON (pas de texte avant ou apres) contenant :
- line_id: "{line_type}:{ticker}"
- ticker: "{ticker}", type: "{line_type}", nom: "{nom}"
- signal: ACHAT_FORT | ACHAT | RENFORCEMENT | CONSERVER | ALLEGEMENT | VENTE | SURVEILLANCE
- conviction: faible | moderee | forte
- synthese: minimum 150 caracteres (explique comme a un ami)
- memory_narrative: 4-8 phrases, construites comme un FIL HISTORIQUE (ce qui a change depuis les derniers runs), en integrant les signaux precedents, les operations recentes (transactions/orders), et les implications pour la suite
- analyse_technique, analyse_fondamentale, analyse_sentiment
- raisons_principales: 3-5 raisons (array)
- risques, catalyseurs, badges_keywords: arrays
- action_recommandee: instruction CHIFFREE (nb titres, montant EUR, prix). Pour watchlist: prix d'entree ideal + montant suggere.
- deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
- deep_news_quality_score: 0-100
- deep_news_relevance: high|medium|low
- deep_news_staleness: fresh|recent|stale
- extracted_fundamentals: {{ "pe_ratio": ..., "revenue_growth": ..., "profit_margin": ..., "debt_to_equity": ... }} (si trouves via web ou calcules)
- shared_insights: {{ "analyse_technique": "...", "analyse_fondamentale": "...", "analyse_sentiment": "...", "risques": "...", "catalyseurs": "..." }}
- reanalyse_after: date ISO, reanalyse_reason

Si les donnees sont insuffisantes, tu peux faire UNE recherche web (pas plus) pour completer.
Sois concret: chiffres, montants, dates. Pas de generalites.
Les articles "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement."#,
        ticker = ticker,
        nom = nom,
        line_type = line_type,
        position = position,
        market = market,
        news = news,
        insights = insights,
        memory = memory,
        quality = quality,
    )
}

/// Run a single line analysis with native backend: LLM returns JSON, we validate+persist.
fn run_native_line_analysis(
    run_id: &str,
    ticker: &str,
    nom: &str,
    line_type: &str,
    line_data: &Value,
    data_dir: &std::path::Path,
    mut on_progress: Option<crate::llm_backend::ProgressFn>,
) -> Result<()> {
    let prompt = build_native_line_prompt(run_id, ticker, nom, line_type, line_data);
    let timeout_ms = 180_000u64;
    const MAX_RETRIES: u32 = 2;

    let mut last_issues: Vec<String> = Vec::new();
    let mut best_rec: Option<Value> = None;

    for attempt in 0..=MAX_RETRIES {
        let retry_prompt = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{}\n\n=== CORRECTION (tentative {}/{}) ===\nTa reponse precedente avait ces problemes: {}.\nCorrige-les et renvoie le JSON complet.",
                prompt,
                attempt + 1,
                MAX_RETRIES + 1,
                last_issues.join(", ")
            )
        };

        // Call LLM — model returns JSON (+ optional web search)
        // Progress callback only on first attempt
        let cb = if attempt == 0 { on_progress.take() } else { None };
        let result = crate::llm_backend::run_prompt(&retry_prompt, timeout_ms, cb)?;

        // Extract JSON from response
        let rec = match extract_recommendation_json(&result) {
            Some(r) => {
                best_rec = Some(r.clone());
                r
            }
            None => {
                crate::debug_log(&format!("native_line: {ticker} attempt {attempt}: no JSON in response"));
                last_issues = vec!["no_json_in_response".to_string()];
                continue;
            }
        };

        // Validate locally
        let validation = crate::mcp_server::dispatch_tool_direct(
            data_dir,
            "validate_recommendation",
            &json!({"run_id": run_id, "recommendation": serde_json::to_string(&rec).unwrap_or_default()}),
        );

        let ok = validation.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
            || validation.get("stored").and_then(|v| v.as_bool()).unwrap_or(false);

        if ok {
            // Persist extracted data + line memory
            persist_line_extras(data_dir, run_id, ticker, line_data, &rec);
            return Ok(());
        }

        // Validation failed
        last_issues = validation
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["validation_failed".to_string()]);

        crate::debug_log(&format!(
            "native_line: {ticker} attempt {}: issues: {:?}",
            attempt + 1,
            last_issues
        ));

        crate::mcp_progress_relay::write_progress_event(
            &data_dir.parent().unwrap_or(data_dir),
            run_id, ticker, "repairing",
            &format!("fixing: {}", last_issues.join(", ")),
        );
    }

    // Max retries exhausted — force-store best available recommendation
    if let Some(rec) = best_rec {
        crate::debug_log(&format!("native_line: {ticker} force-storing best rec after {MAX_RETRIES} retries"));
        // Call validate with high attempt count to trigger accept-with-warnings
        crate::mcp_server::dispatch_tool_direct(
            data_dir,
            "validate_recommendation",
            &json!({"run_id": run_id, "recommendation": serde_json::to_string(&rec).unwrap_or_default()}),
        );
        persist_line_extras(data_dir, run_id, ticker, line_data, &rec);
    } else {
        crate::debug_log(&format!("native_line: {ticker} no valid JSON after {MAX_RETRIES} retries"));
        let _ = crate::run_state::update_line_status_with_error(run_id, ticker, "failed", Some("no_valid_recommendation"));
    }
    Ok(())
}

/// Extract recommendation JSON from LLM response Value.
fn extract_recommendation_json(result: &Value) -> Option<Value> {
    // Result may be the JSON directly, or {"ok":true, "mcp_turn":true} with text
    if result.get("line_id").is_some() {
        return Some(result.clone());
    }
    if result.get("recommendation").is_some() {
        return result.get("recommendation").cloned();
    }
    // Try parsing from text content
    if let Some(text) = result.as_str() {
        return crate::llm_parsing::extract_json_object(text);
    }
    // The result itself might be the recommendation
    if result.get("signal").is_some() && result.get("ticker").is_some() {
        return Some(result.clone());
    }
    None
}

// ── Line memory sync (V2 schema) ────────────────────────────────

/// Sync a validated recommendation into the persistent line-memory.json store.
/// V2 schema — clean break from V1. Writes `schema_version: 2`,
/// `signal_history`, `memory_narrative`, `news_themes`, `trend`, `price_tracking`.
/// V1 fields (`llm_memory_summary`, `llm_strong_signals`, `llm_key_history`) are NOT written.
/// NOTE: `memory_narrative` replaces the former `key_reasoning` field name.
fn sync_line_memory(run_id: &str, ticker: &str, rec: &Value, current_price: f64) {
    let ticker = ticker.trim().to_uppercase();
    if ticker.is_empty() { return; }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let today = now[..10].to_string(); // "YYYY-MM-DD" — safe, ASCII only

    // Read current ticker entry from cache (for merge)
    let current = {
        let store = line_memory_read();
        store.get("by_ticker")
            .and_then(|bt| bt.get(&ticker))
            .cloned()
            .unwrap_or(json!({}))
    };

    // Extract fields from recommendation
    let signal = as_str(rec.get("signal"));
    let conviction = as_str(rec.get("conviction"));
    let synthese = as_str(rec.get("synthese"));

    // ── V2: signal_history (prepend, cap at 10) ────────────────────
    let new_signal_entry = json!({
        "date": today,
        "signal": signal,
        "conviction": conviction,
        "price_at_signal": current_price,
        "run_id": run_id,
    });

    let mut signal_history: Vec<Value> = vec![new_signal_entry];
    if let Some(arr) = current.get("signal_history").and_then(|v| v.as_array()) {
        for item in arr.iter().take(9) {
            signal_history.push(item.clone());
        }
    }

    // ── V2: memory_narrative (LLM-authored during analysis; fallback if missing) ─────
    let trend = compute_trend(&signal_history);
    let memory_narrative = non_empty_str(rec.get("memory_narrative"))
        .or_else(|| non_empty_str(rec.get("key_reasoning")))
        .unwrap_or_else(|| {
            build_memory_narrative(
                &current,
                &synthese,
                &signal,
                &conviction,
                &today,
                trend,
            )
        });

    // ── V2: news_themes (merge from badges_keywords, cap at 15) ────
    let news_themes = merge_string_list(
        json_str_array(rec.get("badges_keywords"))
            .chain(json_str_array(current.get("news_themes"))),
        15,
    );

    // ── V2: trend (computed from last 3 signal_history entries) ─────

    // ── V2: price_tracking (accuracy vs previous signal) ───────────
    let price_tracking = compute_price_tracking(&current, current_price, &signal);

    // ── Deep news fields (preserved across V1→V2) ──────────────────
    let deep_news_summary = non_empty_str(rec.get("deep_news_memory_summary"))
        .or_else(|| non_empty_str(rec.get("deep_news_summary")))
        .or_else(|| non_empty_str(current.get("deep_news_memory_summary")))
        .unwrap_or_default();

    // Run history: prepend this run (max 20)
    let mut run_history: Vec<Value> = Vec::with_capacity(21);
    run_history.push(json!({
        "date": &now,
        "signal": signal,
        "conviction": conviction,
        "synthese": truncate(synthese.as_str(), 420),
    }));
    if let Some(arr) = current.get("run_history").and_then(|v| v.as_array()) {
        for item in arr.iter().take(19) {
            run_history.push(item.clone());
        }
    }

    // Collect deep news banned URLs for merge
    let rec_banned: Vec<String> = json_str_array(rec.get("deep_news_banned_urls"))
        .map(|s| s.to_string())
        .collect();

    // Build the V2 ticker entry — NO V1 fields
    let entry = json!({
        "schema_version": 2,
        "ticker": ticker,
        "updated_at": &now,
        "run_id_last_update": run_id,
        "signal": signal,
        "conviction": conviction,
        "signal_history": signal_history,
        "memory_narrative": memory_narrative,
        "price_tracking": price_tracking,
        "news_themes": news_themes,
        "trend": trend,
        "deep_news_memory_summary": truncate(&deep_news_summary, 2400),
        "deep_news_selected_url": non_empty_str(rec.get("deep_news_selected_url"))
            .or_else(|| non_empty_str(current.get("deep_news_selected_url")))
            .unwrap_or_default(),
        "deep_news_quality_score": rec.get("deep_news_quality_score")
            .and_then(|v| v.as_u64())
            .or_else(|| current.get("deep_news_quality_score").and_then(|v| v.as_u64()))
            .unwrap_or(50),
        "deep_news_relevance": non_empty_str(rec.get("deep_news_relevance"))
            .or_else(|| non_empty_str(current.get("deep_news_relevance")))
            .unwrap_or_default(),
        "deep_news_staleness": non_empty_str(rec.get("deep_news_staleness"))
            .or_else(|| non_empty_str(current.get("deep_news_staleness")))
            .unwrap_or_default(),
        "deep_news_seen_urls": current.get("deep_news_seen_urls").cloned().unwrap_or(json!([])),
        "deep_news_banned_urls": current.get("deep_news_banned_urls").cloned().unwrap_or(json!([])),
        "action_recommandee": non_empty_str(rec.get("action_recommandee"))
            .or_else(|| non_empty_str(current.get("action_recommandee")))
            .unwrap_or_default(),
        "reanalyse_after": non_empty_str(rec.get("reanalyse_after"))
            .unwrap_or_default(),
        "reanalyse_reason": non_empty_str(rec.get("reanalyse_reason"))
            .unwrap_or_default(),
        "last_recommendation": {
            "date": &now,
            "signal": signal,
            "conviction": conviction,
            "synthese": truncate(synthese.as_str(), 420),
        },
        "run_history": run_history,
        // user_action preserved if it existed
        "user_action": current.get("user_action").cloned().unwrap_or(Value::Null),
    });

    // C-1: Write to in-memory cache (no direct file I/O, flushed at end of run)
    let ticker_key = ticker.clone();
    let run_id_log = run_id.to_string();
    line_memory_patch(move |store| {
        // Ensure by_ticker exists
        if !store.get("by_ticker").and_then(|v| v.as_object()).is_some() {
            store["by_ticker"] = json!({});
        }
        if let Some(bt) = store.get_mut("by_ticker").and_then(|v| v.as_object_mut()) {
            bt.insert(ticker_key.clone(), entry);
        }

        // Merge deep news banned URLs into global list
        if !rec_banned.is_empty() {
            if let Some(arr) = store.get_mut("global_deep_news_banned_urls")
                .and_then(|v| v.as_array_mut())
            {
                for url in &rec_banned {
                    let url_val = Value::String(url.to_string());
                    if !arr.contains(&url_val) {
                        arr.push(url_val);
                    }
                }
                while arr.len() > 2000 { arr.remove(0); }
            }
        }
    });
    crate::debug_log(&format!("sync_line_memory: updated V2 for {ticker} run {run_id_log} (cached)"));
}

// ── Theme concentration aggregation (Phase 2b) ─────────────────

/// Compute theme concentration from line memory V2 data.
/// Returns a JSON object with concentrated themes (3+ tickers sharing a theme).
pub(crate) fn compute_theme_concentration(_run_id: &str) -> serde_json::Value {
    // C-1: Read from in-memory cache (falls back to disk if cache not loaded)
    let store = line_memory_read();

    let by_ticker = match store.get("by_ticker").and_then(|v| v.as_object()) {
        Some(bt) => bt,
        None => return json!({ "themes": [], "total_concentrated": 0 }),
    };

    // Build map: theme_slug -> Vec<ticker>
    let mut theme_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for (ticker, entry) in by_ticker {
        // Skip synthetic keys (e.g. _PORTFOLIO used for portfolio-level insights)
        if ticker.starts_with('_') { continue; }
        if let Some(themes_arr) = entry.get("news_themes").and_then(|v| v.as_array()) {
            for theme_val in themes_arr {
                if let Some(slug) = theme_val.as_str() {
                    let slug = slug.trim().to_lowercase();
                    if slug.is_empty() { continue; }
                    let tickers = theme_map.entry(slug).or_default();
                    let upper = ticker.trim().to_uppercase();
                    if !tickers.contains(&upper) {
                        tickers.push(upper);
                    }
                }
            }
        }
    }

    // Filter to themes with 3+ tickers
    let mut concentrated: Vec<Value> = theme_map
        .into_iter()
        .filter(|(_, tickers)| tickers.len() >= 3)
        .map(|(theme, mut tickers)| {
            tickers.sort();
            let count = tickers.len();
            json!({
                "theme": theme,
                "tickers": tickers,
                "count": count,
            })
        })
        .collect();

    // Sort by count descending, then by theme name
    concentrated.sort_by(|a, b| {
        let ca = a.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        let cb = b.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        cb.cmp(&ca).then_with(|| {
            let ta = a.get("theme").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("theme").and_then(|v| v.as_str()).unwrap_or("");
            ta.cmp(tb)
        })
    });

    let total = concentrated.len();
    if total > 0 {
        crate::debug_log(&format!(
            "[theme-concentration] found {total} concentrated themes across portfolio"
        ));
    }

    json!({
        "themes": concentrated,
        "total_concentrated": total,
    })
}

/// Build a human-readable French text block for theme concentration.
/// Returns empty string when no concentrated themes exist.
pub(crate) fn build_theme_concentration_text(concentration: &Value) -> String {
    let themes = match concentration.get("themes").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return String::new(),
    };

    let mut lines = vec![
        "\nCONCENTRATION THEMATIQUE:".to_string(),
    ];
    for entry in themes {
        let theme = entry.get("theme").and_then(|v| v.as_str()).unwrap_or("?");
        let count = entry.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        let tickers = entry.get("tickers").and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        lines.push(format!("- \"{theme}\" ({count} positions): {tickers}"));
    }
    lines.push("Attention: risque de concentration thematique.".to_string());
    lines.join("\n")
}

// ── Chat-to-Memory: partial update of line memory fields ──────

/// Update specific fields in the line-memory.json store for a given ticker.
/// Only the fields that are `Some` are overwritten; others are left untouched.
/// Called from the "Save to Memory" panel after a Position Chat session.
pub fn update_line_memory_fields(
    ticker: &str,
    memory_narrative: Option<&str>,
    user_note: Option<&str>,
    news_themes: Option<Vec<String>>,
) -> Result<()> {
    let ticker = ticker.trim().to_uppercase();
    if ticker.is_empty() {
        return Err(anyhow::anyhow!("update_line_memory_fields: empty ticker"));
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let today = now[..10].to_string(); // safe — ASCII only

    // Prepare owned values for the closure (captures must be 'static-compatible)
    let memory_narrative_owned = memory_narrative.map(|s| s.to_string());
    let user_note_owned = user_note.map(|s| s.to_string());
    let ticker_key = ticker.clone();

    // C-1: Mutate via in-memory cache — flush is deferred or explicit
    line_memory_patch(move |store| {
        // Ensure by_ticker exists
        if !store.get("by_ticker").and_then(|v| v.as_object()).is_some() {
            store["by_ticker"] = json!({});
        }

        let entry = store
            .get_mut("by_ticker")
            .and_then(|v| v.as_object_mut())
            .and_then(|bt| bt.entry(&ticker_key).or_insert_with(|| json!({"schema_version": 2, "ticker": &ticker_key})).as_object_mut());

        let entry = match entry {
            Some(e) => e,
            None => { crate::debug_log("update_line_memory_fields: failed to access ticker entry"); return; }
        };

        if let Some(reasoning) = memory_narrative_owned {
            entry.insert("memory_narrative".to_string(), Value::String(reasoning));
        }

        if let Some(note) = user_note_owned {
            // Merge into user_action — preserve `followed` and `date` if they exist
            let mut action = entry.get("user_action")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            action.insert("note".to_string(), Value::String(note));
            if !action.contains_key("date") {
                action.insert("date".to_string(), Value::String(today));
            }
            entry.insert("user_action".to_string(), Value::Object(action));
        }

        if let Some(themes) = news_themes {
            let merged = merge_string_list(
                themes.iter().map(|s| s.as_str()),
                15,
            );
            entry.insert(
                "news_themes".to_string(),
                Value::Array(merged.into_iter().map(Value::String).collect()),
            );
        }

        entry.insert("updated_at".to_string(), Value::String(now));
    });

    // For UI-triggered writes (Save to Memory panel), flush immediately
    // so the user sees updated data on next read without waiting for run completion.
    line_memory_flush_now();
    crate::debug_log(&format!("update_line_memory_fields: updated {ticker}"));
    Ok(())
}

// ── V2 computation helpers ─────────────────────────────────────

/// Extract first N sentences from text (split on `. ` or period at end).
fn extract_first_sentences(text: &str, n: usize) -> String {
    let mut sentences = Vec::new();
    let mut remaining = text.trim();
    for _ in 0..n {
        if remaining.is_empty() { break; }
        // Find the first sentence-ending punctuation followed by a space or end
        let end = remaining.find(". ")
            .map(|i| i + 1) // include the period
            .or_else(|| remaining.find(".\n").map(|i| i + 1))
            .unwrap_or(remaining.len());
        let sentence = remaining[..end].trim();
        if !sentence.is_empty() {
            sentences.push(sentence.to_string());
        }
        remaining = remaining[end..].trim_start();
    }
    sentences.join(" ")
}

/// Build an evolving narrative across runs so line memory preserves history.
fn build_memory_narrative(
    current: &Value,
    synthese: &str,
    signal: &str,
    conviction: &str,
    today: &str,
    trend: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(previous_signal) = current.get("signal_history")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
    {
        let prev_date = as_str(previous_signal.get("date"));
        let prev_signal_name = as_str(previous_signal.get("signal"));
        let prev_conviction = as_str(previous_signal.get("conviction"));
        if !prev_signal_name.is_empty() {
            parts.push(format!(
                "Previous call ({}) was {}{}.",
                if prev_date.is_empty() { "unknown date" } else { prev_date.as_str() },
                prev_signal_name,
                if prev_conviction.is_empty() { "".to_string() } else { format!(" ({prev_conviction})") },
            ));
        }
    }

    parts.push(format!(
        "Current call ({today}) is {}{} with a {} trend.",
        if signal.is_empty() { "N/A" } else { signal },
        if conviction.is_empty() { "".to_string() } else { format!(" ({conviction})") },
        if trend.is_empty() { "stable" } else { trend },
    ));

    let latest_delta = extract_first_sentences(synthese, 2);
    if !latest_delta.is_empty() {
        parts.push(format!("Latest analytical update: {latest_delta}"));
    }

    if let Some(previous_narrative) = non_empty_str(current.get("memory_narrative")) {
        let prior_thesis = extract_first_sentences(&previous_narrative, 1);
        if !prior_thesis.is_empty() {
            parts.push(format!("Prior thesis snapshot: {prior_thesis}"));
        }
    }

    if let Some(previous_run_summary) = current.get("run_history")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|entry| entry.get("synthese"))
        .and_then(|v| v.as_str())
    {
        let prior_run = extract_first_sentences(previous_run_summary, 1);
        if !prior_run.is_empty() && !latest_delta.starts_with(&prior_run) {
            parts.push(format!("Previous run focus: {prior_run}"));
        }
    }

    truncate(&parts.join(" "), 1400)
}

/// Compute trend from last 3 signal_history entries.
/// - `upgrading`: signals move toward stronger buy
/// - `downgrading`: signals move toward sell
/// - `volatile`: alternates direction >= 2 times
/// - `stable`: same signal repeated
fn compute_trend(signal_history: &[Value]) -> &'static str {
    if signal_history.len() < 2 { return "stable"; }

    let signals: Vec<i8> = signal_history.iter()
        .take(3)
        .filter_map(|entry| entry.get("signal").and_then(|v| v.as_str()))
        .map(signal_strength)
        .collect();

    if signals.len() < 2 { return "stable"; }

    let mut ups = 0i32;
    let mut downs = 0i32;
    let mut direction_changes = 0i32;
    let mut last_dir: i8 = 0;

    for window in signals.windows(2) {
        let diff = window[0] - window[1]; // newest - older: positive = upgrading
        let dir = if diff > 0 { 1i8 } else if diff < 0 { -1 } else { 0 };
        if dir > 0 { ups += 1; }
        if dir < 0 { downs += 1; }
        if last_dir != 0 && dir != 0 && dir != last_dir { direction_changes += 1; }
        if dir != 0 { last_dir = dir; }
    }

    if direction_changes >= 2 { return "volatile"; }
    if ups > 0 && downs == 0 { return "upgrading"; }
    if downs > 0 && ups == 0 { return "downgrading"; }
    if ups == 0 && downs == 0 { return "stable"; }
    "volatile"
}

/// Map signal name to numeric strength for trend comparison.
fn signal_strength(signal: &str) -> i8 {
    match signal {
        "VENTE" => 1,
        "ALLEGEMENT" => 2,
        "SURVEILLANCE" => 3,
        "CONSERVER" => 4,
        "RENFORCEMENT" => 5,
        "ACHAT" => 6,
        "ACHAT_FORT" => 7,
        _ => 3, // unknown defaults to neutral
    }
}

/// Returns true if this signal expects a positive price move.
fn is_bullish_signal(signal: &str) -> bool {
    matches!(signal, "ACHAT_FORT" | "ACHAT" | "RENFORCEMENT")
}

/// Compute price_tracking from previous signal data and current price.
fn compute_price_tracking(current: &Value, current_price: f64, current_signal: &str) -> Value {
    // Get the most recent signal_history entry from the PREVIOUS run
    let prev_entry = current.get("signal_history")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());

    if let Some(prev) = prev_entry {
        let prev_signal = prev.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let prev_date = prev.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let price_at_signal = prev.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let return_pct = if price_at_signal > 0.0 && current_price > 0.0 {
            ((current_price - price_at_signal) / price_at_signal * 100.0 * 10.0).round() / 10.0
        } else {
            0.0
        };

        let accuracy = if price_at_signal > 0.0 && current_price > 0.0 {
            let positive_return = return_pct > 0.0;
            if is_bullish_signal(prev_signal) == positive_return { "correct" } else { "incorrect" }
        } else {
            "unknown"
        };

        json!({
            "last_signal": prev_signal,
            "last_signal_date": prev_date,
            "price_at_signal": price_at_signal,
            "current_price": current_price,
            "return_since_signal_pct": return_pct,
            "signal_accuracy": accuracy,
        })
    } else {
        // First analysis — no previous signal to compare against
        json!({
            "last_signal": current_signal,
            "last_signal_date": &chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "price_at_signal": current_price,
            "current_price": current_price,
            "return_since_signal_pct": 0.0,
            "signal_accuracy": "first_analysis",
        })
    }
}

// ── Line memory helpers ─────────────────────────────────────────

/// Extract market price for a ticker from the cached run state.
/// Used by codex path where line_data isn't directly available.
fn extract_market_price_from_run_state(data_dir: &std::path::Path, run_id: &str, ticker: &str) -> f64 {
    let ticker_upper = ticker.to_uppercase();
    crate::run_state_cache::load(data_dir, run_id)
        .ok()
        .and_then(|state| {
            state.get("market")
                .and_then(|m| m.get(&ticker_upper))
                .and_then(|md| md.get("price").or_else(|| md.get("last_price")).or_else(|| md.get("cours")))
                .and_then(|v| v.as_f64())
        })
        .unwrap_or(0.0)
}

fn as_str(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

fn non_empty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if s.is_char_boundary(max) {
        s[..max].to_string()
    } else {
        // Find the largest valid char boundary at or before max
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) { end -= 1; }
        s[..end].to_string()
    }
}

/// Collect string items from a JSON array, deduplicating and capping at max_items.
fn merge_string_list<'a>(items: impl Iterator<Item = &'a str>, max_items: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(max_items);
    for item in items {
        let trimmed = item.trim();
        if trimmed.is_empty() { continue; }
        if seen.insert(trimmed.to_lowercase()) {
            result.push(trimmed.to_string());
            if result.len() >= max_items { break; }
        }
    }
    result
}

/// Extract string items from a JSON array Value.
fn json_str_array(v: Option<&Value>) -> impl Iterator<Item = &str> {
    v.and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str())
        .filter(|s| !s.trim().is_empty())
}

/// Persist extracted fundamentals, shared insights, deep news, and line memory from the recommendation.
fn persist_line_extras(data_dir: &std::path::Path, run_id: &str, ticker: &str, line_data: &Value, rec: &Value) {
    // Extract current market price for V2 signal tracking
    let current_price = line_data.get("market_data")
        .and_then(|m| m.get("price").or_else(|| m.get("last_price")).or_else(|| m.get("cours")))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    // Sync line memory (cross-run persistent state, V2 schema)
    sync_line_memory(run_id, ticker, rec, current_price);
    let isin = line_data.get("position")
        .and_then(|p| p.get("isin"))
        .and_then(|v| v.as_str())
        .unwrap_or(ticker);

    // Persist shared insights (with optional sector analysis)
    if let Some(insights) = rec.get("shared_insights") {
        if !insights.is_null() {
            let mut params = json!({
                "ticker": ticker,
                "isin": isin,
                "insights": serde_json::to_string(insights).unwrap_or_default(),
            });
            if let Some(sector) = rec.get("sector").and_then(|v| v.as_str()) {
                params["sector"] = json!(sector);
            }
            if let Some(sa) = rec.get("sector_analysis").and_then(|v| v.as_str()) {
                params["sector_analysis"] = json!(sa);
            }
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_shared_insights",
                &params,
            );
        }
    }

    // Persist extracted fundamentals
    if let Some(fundamentals) = rec.get("extracted_fundamentals") {
        if !fundamentals.is_null() {
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_extracted_fundamentals",
                &json!({"ticker": ticker, "isin": isin, "fundamentals": serde_json::to_string(fundamentals).unwrap_or_default()}),
            );
        }
    }

    // Persist deep news summary to the per-URL API cache
    let deep_news_summary = rec.get("deep_news_summary")
        .or_else(|| rec.get("deep_news_memory_summary"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !deep_news_summary.is_empty() {
        // Build a minimal line_context for persist_deep_news_if_present
        let line_context = json!({
            "ticker": ticker,
            "row": { "isin": isin },
            "news": line_data.get("news").cloned().unwrap_or(Value::Null),
            "market": line_data.get("market_data").cloned().unwrap_or(Value::Null),
        });
        crate::llm_parsing::persist_deep_news_if_present(rec, &line_context);
    }
}

fn build_batch_prompt(run_id: &str, tickers: &[(String, String, String)]) -> String {
    let lines_list = tickers
        .iter()
        .map(|(t, n, lt)| format!("  - {lt}:{t} ({n})"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Tu es Alfred, un conseiller financier bienveillant. Analyse ces {count} lignes pour le run "{run_id}".

Lignes a analyser :
{lines_list}

Pour CHAQUE ligne ci-dessus :
1. Appelle `get_line_data(run_id="{run_id}", ticker="TICKER")` pour obtenir le contexte.
2. Analyse et produis un JSON de recommandation avec :
   - line_id: "type:ticker" (ex: "position:MC")
   - ticker, type, nom
   - signal: ACHAT_FORT | ACHAT | RENFORCEMENT | CONSERVER | ALLEGEMENT | VENTE | SURVEILLANCE
   - conviction: faible | moderee | forte
   - synthese: minimum 150 caracteres (explique comme a un ami)
   - memory_narrative: 4-8 phrases, narratif historique qui relie les runs precedents, les nouveaux signaux, et l'historique des operations (transactions/orders)
   - analyse_technique, analyse_fondamentale, analyse_sentiment
   - raisons_principales: 3-5 raisons (array)
   - risques, catalyseurs, badges_keywords: arrays
   - action_recommandee: instruction CHIFFREE (nb titres, montant €, prix)
   - deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
   - deep_news_quality_score: 0-100 (recence 0-35 + pertinence 0-35 + diversite 0-20 + utilite 0-10)
   - deep_news_relevance: high|medium|low
   - deep_news_staleness: fresh|recent|stale
   - reanalyse_after: date ISO, reanalyse_reason
   - sector_analysis: 2-3 phrases sur le positionnement sectoriel (COT, tendances macro)
3. Appelle `validate_recommendation(run_id="{run_id}", recommendation=...)`.
   Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.
4. BUDGET WEB: maximum 1 recherche web par ligne. Utilise-la uniquement si
   deep_news_quality_score < 30 et les donnees de get_line_data sont vraiment insuffisantes.
   Prefere TOUJOURS analyser avec les donnees disponibles.
5. Si tu fais une recherche web et lis un article, appelle `persist_deep_news` pour enrichir
   le cache collectif (les prochains runs reutiliseront ce resume sans recherche web).
6. Si un article dans les news est du bruit (pub, generique, non pertinent), appelle
   `ban_deep_news` pour le filtrer dans les prochains runs.
7. Si fondamentaux manquants trouves, appelle `persist_extracted_fundamentals`.
8. Appelle `persist_shared_insights` avec tes analyses generiques.

Regles :
- Analyse CHAQUE ligne. Ne saute aucune.
- Toujours appeler validate_recommendation — ne presume pas que ton output est valide.
- Sois concret sur les chiffres.
- Les articles marques "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement.
- Les articles marques "A APPROFONDIR" n'ont pas de resume — lis-les via recherche web.
- Si `activity` contient des operations recentes, commente les decisions passees (timing, prix d'achat vs cours actuel, renforcements pertinents ou non). Utilise-les pour calibrer ta recommandation."#,
        count = tickers.len(),
        run_id = run_id,
        lines_list = lines_list,
    )
}

fn build_synthesis_prompt(run_id: &str) -> String {
    let account = crate::load_run_by_id_direct(run_id).ok()
        .and_then(|r| r.get("account").and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_default();
    let previous_syntheses = crate::llm_prompts::build_previous_syntheses_section_public(&account);

    // Phase 2b: theme concentration
    let concentration = compute_theme_concentration(run_id);
    let concentration_section = build_theme_concentration_text(&concentration);

    format!(
        r#"Tu es Alfred, un gestionnaire de portefeuille qui conseille un investisseur particulier.
Tu dois produire la synthese globale du portefeuille pour le run "{run_id}".

REGLE FONDAMENTALE : les signaux par ligne (ACHAT, VENTE, CONSERVER, etc.)
sont des FAITS produits par l'analyse detaillee. Tu ne dois PAS les
re-evaluer, les contredire ou les changer. Ta synthese les RESUME et
les MET EN PERSPECTIVE — elle ne reinvente pas l'analyse.

WORKFLOW STRICT — suis ces etapes dans l'ordre :

1. Appelle `get_run_context(run_id="{run_id}")` pour obtenir le resume du portefeuille
   et la liste des lignes analysees.

2. Appelle `check_coverage(run_id="{run_id}")` pour verifier la couverture.
   Si des lignes manquent, note-les mais continue avec les recommandations disponibles.
   Une synthese partielle est mieux que pas de synthese.

3. Genere la synthese avec ces 4 champs :

   synthese_marche (minimum 300 caracteres):
   IMPORTANT — ne repete PAS les donnees ligne par ligne (l'utilisateur les voit
   deja dans le detail de chaque position). Concentre-toi sur :
   - La NARRATIVE : quel est le fil conducteur du portefeuille ? Quel profil de
     risque se dessine ? Est-ce coherent avec la strategie annoncee ?
   - Les DECISIONS A PRENDRE : quels arbitrages concrets, dans quel ordre, et
     pourquoi maintenant plutot que dans un mois ?
   - Les RISQUES CROISES : correlations entre lignes, exposition sectorielle ou
     geographique desequilibree, impact d'un scenario macro (hausse des taux,
     recession, change EUR/USD).
   - Le TON : parle comme un conseiller bienveillant a un particulier, pas a un pro.
     Pas de jargon financier inutile. Sois direct et opinione, pas descriptif.
     Exemple : "Vous avez trop mise sur la tech US sans protection. Avant
     de renforcer NVDA, securisez vos gains sur VZ et vendez les lignes
     qui ne bougent plus — ca libere du cash pour de vraies opportunites."
   - Si ecart entre execution et strategie, le mentionner explicitement.

   actions_immediates (JSON array, 1-5 actions):
   UNIQUEMENT les tickers dont le signal par ligne est ACHAT, ACHAT_FORT,
   VENTE, ALLEGEMENT ou RENFORCEMENT. Si le signal est CONSERVER ou
   SURVEILLANCE → PAS d'action pour ce ticker (meme si tu penses le contraire).
   OBLIGATOIRE si des recommandations ont un signal actionnable.
   Schema strict par action:
   {{
     "ticker": "MC",
     "nom": "LVMH",
     "action": "ACHAT|VENTE|RENFORCEMENT|ALLEGEMENT",
     "order_type": "MARKET|LIMIT",
     "limit_price": null,
     "quantity": 3,
     "estimated_amount_eur": 2400.0,
     "priority": 1,
     "rationale": "phrase courte et concrete"
   }}
   Regles: quantity > 0, estimated_amount_eur > 0, priorities 1-5 uniques,
   LIMIT => limit_price > 0, MARKET => limit_price = null.
   Si liquidites = 0: uniquement VENTE/ALLEGEMENT (ou 0 action).

   prochaine_analyse: date + catalyseurs justifiant cette date
   (ex: "Relancez apres le 15 avril — resultats T1 Schneider et LVMH")

   opportunites_watchlist: resume des 2-3 meilleures opportunites watchlist
   (si des lignes watchlist existent). Sinon, chaine vide.
   Ne presente PAS les watchlist comme deja detenues.

4. Appelle `validate_synthesis(run_id="{run_id}",
     synthese_marche="...", actions_immediates=[...],
     prochaine_analyse="...", opportunites_watchlist="...")`.
   Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.

5. Appelle `finalize_report(run_id="{run_id}")` pour composer et persister le rapport.

{previous_syntheses}
{concentration_section}
REGLES:
- Ne saute AUCUNE etape (get_run_context, check_coverage, validate_synthesis, finalize_report).
- N'appelle PAS get_line_data ni validate_recommendation — les analyses par ligne
  sont deja faites. Tu SYNTHETISES, tu ne re-analyses pas.
- Sois concret: chiffres, montants, dates. Pas de generalites.
- Ne presente PAS les watchlist comme deja detenues.

CRITIQUE — si tu ne fais pas les etapes 4 ET 5, le rapport est PERDU.
Le travail d'analyse de toutes les lignes sera gache. Tu DOIS appeler
validate_synthesis puis finalize_report. Pas d'exception."#,
        run_id = run_id,
        concentration_section = concentration_section,
    )
}

// ── Batch dispatch queue ─────────────────────────────────────────

pub struct McpLinePacket {
    pub ticker: String,
    pub nom: String,
    pub line_type: String,
}

pub struct McpBatchDispatchQueue {
    run_id: String,
    data_dir: String,
    batch_size: usize,
    pending: Vec<McpLinePacket>,
    result_tx: mpsc::Sender<Result<Vec<String>>>,
    result_rx: mpsc::Receiver<Result<Vec<String>>>,
    active_batches: usize,
    completed_tickers: Vec<String>,
    relay_stop_flags: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl McpBatchDispatchQueue {
    pub fn new(run_id: &str, data_dir: &str, batch_size: usize) -> Self {
        // Ensure MCP config is written before first analysis (codex backend only)
        if crate::llm_backend::current_backend_name() == "codex" {
            crate::codex::ensure_mcp_config();
        }

        let (result_tx, result_rx) = mpsc::channel();
        Self {
            run_id: run_id.to_string(),
            data_dir: data_dir.to_string(),
            batch_size: batch_size.max(1),
            pending: Vec::new(),
            result_tx,
            result_rx,
            active_batches: 0,
            completed_tickers: Vec::new(),
            relay_stop_flags: Vec::new(),
        }
    }

    pub fn push(&mut self, packet: McpLinePacket) -> Result<()> {
        self.pending.push(packet);
        if self.pending.len() >= self.batch_size {
            self.flush_batch()?;
        }
        Ok(())
    }

    pub fn flush_pending(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            self.flush_batch()?;
        }
        Ok(())
    }

    pub fn join_all(&mut self) -> Result<Vec<String>> {
        while self.active_batches > 0 {
            match self.result_rx.recv() {
                Ok(Ok(tickers)) => {
                    self.completed_tickers.extend(tickers);
                    self.active_batches -= 1;
                    // Merge MCP results after each batch completes
                    merge_mcp_results(&self.run_id, &self.data_dir);
                }
                Ok(Err(e)) => {
                    eprintln!("[mcp-batch] batch failed: {e}");
                    self.active_batches -= 1;
                    merge_mcp_results(&self.run_id, &self.data_dir);
                }
                Err(_) => break,
            }
        }
        // Final merge in case any results arrived after last batch
        merge_mcp_results(&self.run_id, &self.data_dir);
        for flag in &self.relay_stop_flags {
            flag.store(true, Ordering::Relaxed);
        }
        Ok(self.completed_tickers.clone())
    }

    fn flush_batch(&mut self) -> Result<()> {
        let batch: Vec<McpLinePacket> = self.pending.drain(..).collect();
        let tickers: Vec<(String, String, String)> = batch
            .iter()
            .map(|p| (p.ticker.clone(), p.nom.clone(), p.line_type.clone()))
            .collect();

        let run_id = self.run_id.clone();
        let data_dir = self.data_dir.clone();
        let tx = self.result_tx.clone();

        // Emit analyzing_lines stage on first batch dispatch
        if self.active_batches == 0 && self.completed_tickers.is_empty() {
            let _ = crate::run_state::set_native_run_stage(
                &self.run_id, "analyzing_lines", None, None,
            );
        }

        let (relay_handle, relay_stop) =
            crate::mcp_progress_relay::start_relay(&run_id, &data_dir);
        self.relay_stop_flags.push(relay_stop);

        let batch_tickers: Vec<String> = tickers.iter().map(|(t, _, _)| t.clone()).collect();
        let is_native = crate::llm_backend::current_backend_name() != "codex";

        let progress_run_id = run_id.clone();
        let progress_data_dir = data_dir.clone();
        let progress_tickers = batch_tickers.clone();

        thread::spawn(move || {
            if is_native {
                // Native backend: parallel per-line — LLM returns JSON, we validate+persist
                let dd = std::path::PathBuf::from(&progress_data_dir);

                // Pre-fetch all line data (fast, from cache/disk)
                let line_data_vec: Vec<(String, String, String, Value)> = tickers.iter().map(|(ticker, nom, line_type)| {
                    let line_data = crate::mcp_server::dispatch_tool_direct(
                        &dd,
                        "get_line_data",
                        &serde_json::json!({"run_id": progress_run_id, "line_id": format!("{line_type}:{ticker}")}),
                    );
                    (ticker.clone(), nom.clone(), line_type.clone(), line_data)
                }).collect();

                // Spawn one thread per line
                let handles: Vec<_> = line_data_vec.into_iter().map(|(ticker, nom, line_type, line_data)| {
                    let rid = progress_run_id.clone();
                    let pdd = dd.clone();
                    let tk = ticker.clone();

                    thread::spawn(move || {
                        let prid = rid.clone();
                        let pdd2 = pdd.clone();
                        let tk2 = tk.clone();

                        let progress_cb: Option<crate::llm_backend::ProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
                            if label.starts_with("tokens:") || label.starts_with("rate_limit:") {
                                let path = pdd2.join("runtime-state").join(format!("{prid}_mcp_progress.jsonl"));
                                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                                    use std::io::Write;
                                    let event = if label.starts_with("tokens:") {
                                        let parts: Vec<&str> = label.split(':').collect();
                                        serde_json::json!({
                                            "type": "token_usage",
                                            "total": parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                            "input": parts.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                            "output": parts.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                        })
                                    } else {
                                        serde_json::json!({
                                            "type": "rate_limit",
                                            "used_pct": label.trim_start_matches("rate_limit:").trim_end_matches('%'),
                                        })
                                    };
                                    let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
                                }
                                return;
                            }
                            if !tk2.is_empty() {
                                let dominated = label.starts_with("writing (") || label.starts_with("round ");
                                if !dominated {
                                    crate::mcp_progress_relay::write_progress_event(
                                        &pdd2, &prid, &tk2, "analyzing", &label.replace('\u{2026}', "..."),
                                    );
                                }
                            }
                        }));

                        match run_native_line_analysis(&rid, &tk, &nom, &line_type, &line_data, &pdd, progress_cb) {
                            Ok(()) => Ok(tk.clone()),
                            Err(e) => {
                                eprintln!("[mcp-native] line {tk} failed: {e}");
                                let err_msg = format!("{e}");
                                let _ = crate::run_state::update_line_status_with_error(&rid, &tk, "failed", Some(&err_msg));
                                Err(e)
                            }
                        }
                    })
                }).collect();

                // Join all line threads
                let mut ok_tickers: Vec<String> = Vec::new();
                let mut has_error = false;
                for handle in handles {
                    match handle.join() {
                        Ok(Ok(ticker)) => ok_tickers.push(ticker),
                        Ok(Err(_)) => has_error = true,
                        Err(_) => has_error = true,
                    }
                }

                let _ = tx.send(if ok_tickers.is_empty() && has_error {
                    Err(anyhow::anyhow!("all_lines_failed"))
                } else {
                    Ok(ok_tickers)
                });
                drop(relay_handle);
            } else {
                // Codex backend: batch prompt (model calls get_line_data via MCP tools)
                let prompt = build_batch_prompt(&progress_run_id, &tickers);
                let timeout_ms = 180_000 + (progress_tickers.len() as u64 * 30_000);

                let current_ticker = std::sync::Arc::new(std::sync::Mutex::new(
                    progress_tickers.first().cloned().unwrap_or_default()
                ));
                let ct = std::sync::Arc::clone(&current_ticker);
                let prid = progress_run_id.clone();
                let pdd = std::path::PathBuf::from(&progress_data_dir);

                let progress_cb: Option<crate::llm_backend::ProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
                    let ticker = ct.lock().map(|g| g.clone()).unwrap_or_default();

                    if label.starts_with("tokens:") || label.starts_with("rate_limit:") {
                        let path = pdd.join("runtime-state").join(format!("{prid}_mcp_progress.jsonl"));
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                            use std::io::Write;
                            let event = if label.starts_with("tokens:") {
                                let parts: Vec<&str> = label.split(':').collect();
                                serde_json::json!({
                                    "type": "token_usage",
                                    "total": parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                    "input": parts.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                    "output": parts.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                })
                            } else {
                                serde_json::json!({
                                    "type": "rate_limit",
                                    "used_pct": label.trim_start_matches("rate_limit:").trim_end_matches('%'),
                                })
                            };
                            let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
                        }
                        return;
                    }

                    if ticker.is_empty() { return; }
                    crate::mcp_progress_relay::write_progress_event(
                        &pdd, &prid, &ticker, "analyzing", &label.replace('\u{2026}', "..."),
                    );
                }));

                // Retry transient errors (capacity, stream disconnect, network) with backoff
                const MAX_BATCH_RETRIES: u32 = 2;
                let mut progress_cb = progress_cb; // make mutable for .take()
                let mut last_err = None;
                for attempt in 0..=MAX_BATCH_RETRIES {
                    if attempt > 0 {
                        let backoff_secs = 10 * attempt as u64;
                        eprintln!("[mcp-batch] retry {attempt}/{MAX_BATCH_RETRIES} in {backoff_secs}s");
                        for t in &batch_tickers {
                            let _ = crate::run_state::update_line_status_with_progress(
                                &progress_run_id, t, "analyzing",
                                &format!("retry {attempt}/{MAX_BATCH_RETRIES}\u{2026}"),
                            );
                        }
                        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
                    }

                    // Progress callback is consumed on first attempt only
                    let cb = if attempt == 0 { progress_cb.take() } else { None };
                    let result = crate::llm_backend::run_prompt(&prompt, timeout_ms, cb);

                    match result {
                        Ok(_) => { last_err = None; break; }
                        Err(e) => {
                            let msg = format!("{e}");
                            let is_transient = msg.contains("at capacity")
                                || msg.contains("stream disconnected")
                                || msg.contains("systemError")
                                || msg.contains("os error 10060")
                                || msg.contains("connection")
                                || msg.contains("Network Error");
                            if is_transient && attempt < MAX_BATCH_RETRIES {
                                eprintln!("[mcp-batch] transient error (attempt {attempt}): {msg}");
                                last_err = Some(e);
                                continue;
                            }
                            last_err = Some(e);
                            break;
                        }
                    }
                }

                let _ = tx.send(match last_err {
                    None => Ok(batch_tickers),
                    Some(e) => {
                        eprintln!("[mcp-batch] batch failed after retries: {e}");
                        let err_msg = format!("{e}");
                        for t in &batch_tickers {
                            let _ = crate::run_state::update_line_status_with_error(&progress_run_id, t, "failed", Some(&err_msg));
                            crate::emit_event("alfred://line-progress", serde_json::json!({
                                "run_id": progress_run_id,
                                "ticker": t,
                                "line_status": { "status": "failed", "error": err_msg },
                            }));
                        }
                        Err(e)
                    }
                });
                drop(relay_handle);
            }
        });

        self.active_batches += 1;
        Ok(())
    }
}

// ── Synthesis turn ───────────────────────────────────────────────

pub fn run_synthesis_turn(run_id: &str, data_dir: &str) -> Result<Value> {
    crate::run_state::set_native_run_stage(run_id, "llm_generating", None, None)?;

    let is_native = crate::llm_backend::current_backend_name() != "codex";

    if is_native {
        return run_native_synthesis(run_id, data_dir);
    }

    // Codex backend: MCP tool-based synthesis (model calls validate_synthesis + finalize_report)
    let (relay_handle, relay_stop) = crate::mcp_progress_relay::start_relay(run_id, data_dir);

    let prompt = build_synthesis_prompt(run_id);
    let pdd = std::path::PathBuf::from(data_dir);
    let prid = run_id.to_string();
    // Use dedicated app-server with only synthesis tools — prevents model from
    // re-analyzing individual lines via get_line_data/validate_recommendation.
    let synthesis_progress: Option<crate::codex::CodexProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
        let progress_text = label.replace('\u{2026}', "...");
        crate::mcp_progress_relay::write_progress_event(
            &pdd, &prid, "__synthesis__", "generating", &progress_text,
        );
    }));
    // Retry transient errors for synthesis too
    const MAX_SYNTH_RETRIES: u32 = 2;
    let mut synthesis_progress = synthesis_progress;
    let mut last_result = None;
    for attempt in 0..=MAX_SYNTH_RETRIES {
        if attempt > 0 {
            let backoff_secs = 15 * attempt as u64;
            eprintln!("[mcp-synthesis] retry {attempt}/{MAX_SYNTH_RETRIES} in {backoff_secs}s");
            crate::mcp_progress_relay::write_progress_event(
                &std::path::PathBuf::from(data_dir), run_id,
                "__synthesis__", "generating",
                &format!("retry {attempt}/{MAX_SYNTH_RETRIES}…"),
            );
            std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
        }

        let cb = if attempt == 0 { synthesis_progress.take() } else { None };
        match crate::codex::run_synthesis_prompt(&prompt, cb) {
            Ok(v) => { last_result = Some(Ok(v)); break; }
            Err(e) => {
                let msg = format!("{e}");
                let is_transient = msg.contains("at capacity")
                    || msg.contains("stream disconnected")
                    || msg.contains("systemError")
                    || msg.contains("os error")
                    || msg.contains("Network Error");
                if is_transient && attempt < MAX_SYNTH_RETRIES {
                    eprintln!("[mcp-synthesis] transient error (attempt {attempt}): {msg}");
                    last_result = Some(Err(e));
                    continue;
                }
                last_result = Some(Err(e));
                break;
            }
        }
    }

    relay_stop.store(true, Ordering::Relaxed);
    let _ = relay_handle.join();

    // Merge synthesis results from MCP sidecar
    merge_mcp_results(run_id, data_dir);

    match last_result.unwrap_or_else(|| Err(anyhow::anyhow!("synthesis_no_result"))) {
        Ok(turn_result) => {
            // Evict (not just flush) — the MCP server process may have written
            // a completed state directly to disk via finalize_report. If we only
            // flush, the cache overwrites that completed state with the stale
            // "running" version. Evict discards the cache so load_run_by_id_direct
            // reads the authoritative on-disk state.
            crate::run_state_cache::evict(run_id);
            line_memory_flush_now();
            let run_state = crate::load_run_by_id_direct(run_id)?;
            let status = run_state
                .get("orchestration")
                .and_then(|o| o.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("running");

            if status == "completed" || status == "completed_degraded" {
                Ok(json!({
                    "ok": true,
                    "orchestration_status": status,
                    "run_id": run_id,
                }))
            } else {
                // Model didn't call finalize_report — extract from turn output
                codex_synthesis_fallback(run_id, &turn_result)
            }
        }
        Err(e) => Err(e),
    }
}

/// Native backend synthesis: LLM returns JSON, we validate (with retry) + finalize.
fn run_native_synthesis(run_id: &str, data_dir: &str) -> Result<Value> {
    crate::run_state_cache::flush_now(run_id);
    let run_state = crate::load_run_by_id_direct(run_id)?;
    let prompt = crate::llm_prompts::build_report_prompt(&run_state);
    let dd = std::path::Path::new(data_dir);

    crate::debug_log("[native-synthesis] generating synthesis via direct JSON...");

    const MAX_RETRIES: u32 = 2;
    let mut last_issues: Vec<String> = Vec::new();

    for attempt in 0..=MAX_RETRIES {
        let retry_prompt = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{}\n\n=== CORRECTION (tentative {}/{}) ===\nTa synthese precedente avait ces problemes: {}.\nCorrige-les et renvoie le JSON complet.",
                prompt,
                attempt + 1,
                MAX_RETRIES + 1,
                last_issues.join(", ")
            )
        };

        let prid = run_id.to_string();
        let pdd = std::path::PathBuf::from(data_dir);
        let cb: Option<crate::llm_backend::ProgressFn> = if attempt == 0 {
            Some(Box::new(move |_bytes, _lines, label| {
                let dominated = label.starts_with("writing (") || label.starts_with("round ");
                if !dominated {
                    crate::mcp_progress_relay::write_progress_event(
                        &pdd, &prid, "__synthesis__", "generating", &label.replace('\u{2026}', "..."),
                    );
                }
            }))
        } else {
            None
        };

        let result = crate::llm_backend::run_prompt(&retry_prompt, 300_000, cb)?;

        // Extract synthesis JSON
        let draft = if result.get("synthese_marche").is_some() {
            result.clone()
        } else if result.get("draft").is_some() {
            result.get("draft").cloned().unwrap_or(result.clone())
        } else {
            result.clone()
        };

        let synthese = draft.get("synthese_marche")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if synthese.len() < 20 {
            crate::debug_log(&format!("[native-synthesis] attempt {}: no synthese_marche in response", attempt + 1));
            last_issues = vec!["synthese_marche_missing_or_empty".to_string()];
            continue;
        }

        // Validate
        let actions_str = serde_json::to_string(
            draft.get("actions_immediates").unwrap_or(&json!([]))
        ).unwrap_or_else(|_| "[]".to_string());

        let validation = crate::mcp_server::dispatch_tool_direct(dd, "validate_synthesis", &json!({
            "run_id": run_id,
            "synthese_marche": synthese,
            "actions_immediates": actions_str,
            "prochaine_analyse": draft.get("prochaine_analyse").and_then(|v| v.as_str()).unwrap_or(""),
            "opportunites_watchlist": draft.get("opportunites_watchlist").and_then(|v| v.as_str()).unwrap_or(""),
        }));

        let valid = validation.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if valid || attempt == MAX_RETRIES {
            if !valid {
                crate::debug_log(&format!("[native-synthesis] accepting after {MAX_RETRIES} retries despite issues"));
            }
            crate::debug_log(&format!("[native-synthesis] synthesis ({} chars), finalizing", synthese.len()));
            crate::mcp_server::dispatch_tool_direct(dd, "finalize_report", &json!({"run_id": run_id}));
            line_memory_flush_now();
            return Ok(json!({
                "ok": true,
                "orchestration_status": "completed",
                "run_id": run_id,
            }));
        }

        // Validation failed — collect issues for retry prompt
        last_issues = validation
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["validation_failed".to_string()]);

        crate::debug_log(&format!(
            "[native-synthesis] attempt {}: issues: {:?}",
            attempt + 1, last_issues
        ));
    }

    unreachable!("last attempt always accepts")
}

/// Codex fallback: model didn't call finalize_report — extract synthesis from
/// the turn output (agent text or composed_payload) and persist directly.
fn codex_synthesis_fallback(run_id: &str, turn_result: &Value) -> Result<Value> {
    crate::run_state_cache::flush_now(run_id);
    line_memory_flush_now();
    let run_state = crate::load_run_by_id_direct(run_id)?;
    let reco_count = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if reco_count > 0 {
        crate::debug_log(&format!(
            "[synthesis-fallback] finalize_report not called, attempting with {reco_count} recommendations"
        ));

        // Try to extract synthesis from the model's text output first
        let agent_text = turn_result.get("agent_text")
            .or_else(|| turn_result.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let extracted = if !agent_text.is_empty() {
            crate::llm_parsing::extract_json_object(agent_text)
        } else {
            None
        };

        // Priority: extracted from agent text > composed_payload from MCP tool > empty
        let composed = extracted.as_ref()
            .or_else(|| run_state.get("composed_payload"))
            .cloned()
            .unwrap_or(json!({}));

        let synthese = composed.get("synthese_marche")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() > 20)
            .unwrap_or("Synthese partielle — le rapport a ete compose a partir des recommandations disponibles.");
        let mut actions = composed.get("actions_immediates").cloned().unwrap_or(json!([]));

        // If actions_immediates is empty, derive from per-line recommendations
        if actions.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            let recs = run_state.get("pending_recommandations")
                .and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut derived: Vec<Value> = recs.iter()
                .filter(|r| {
                    let sig = r.get("signal").and_then(|v| v.as_str()).unwrap_or("");
                    matches!(sig, "ACHAT_FORT" | "ACHAT" | "VENTE" | "ALLEGEMENT" | "RENFORCEMENT")
                })
                .map(|r| {
                    let ticker = r.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
                    let nom = r.get("nom").and_then(|v| v.as_str()).unwrap_or("");
                    let signal = r.get("signal").and_then(|v| v.as_str()).unwrap_or("");
                    let action_text = r.get("action_recommandee").and_then(|v| v.as_str()).unwrap_or("");
                    let rationale = if !action_text.is_empty() {
                        action_text.chars().take(200).collect::<String>()
                    } else {
                        r.get("synthese").and_then(|v| v.as_str())
                            .map(|s| s.chars().take(120).collect::<String>())
                            .unwrap_or_default()
                    };
                    json!({
                        "ticker": ticker,
                        "nom": nom,
                        "action": signal,
                        "rationale": rationale,
                        "priority": signal,
                    })
                })
                .collect();
            derived.truncate(5);
            if !derived.is_empty() {
                // Assign unique priorities 1..N
                for (i, a) in derived.iter_mut().enumerate() {
                    if let Some(obj) = a.as_object_mut() {
                        obj.insert("priority".to_string(), json!(i + 1));
                    }
                }
                actions = json!(derived);
                crate::debug_log(&format!(
                    "[synthesis-fallback] derived {} actions from per-line recommendations", derived.len()
                ));
            }
        }
        let draft = json!({
            "synthese_marche": synthese,
            "actions_immediates": actions,
            "prochaine_analyse": composed.get("prochaine_analyse").cloned().unwrap_or(json!("")),
            "opportunites_watchlist": composed.get("opportunites_watchlist").cloned().unwrap_or(json!("")),
            "llm_utilise": "codex-mcp-partial",
        });
        // persist_retry_global_synthesis builds a full composed_payload (with
        // portfolio KPIs: valeur_portefeuille, plus_value_totale, liquidites)
        // and writes it to run_state + report artifacts.  If it succeeds we
        // must NOT overwrite composed_payload — the persisted version is richer.
        match crate::report::persist_retry_global_synthesis(run_id, &draft) {
            Ok(result) => {
                crate::debug_log(&format!(
                    "[synthesis-fallback] persist_retry_global_synthesis succeeded for {run_id}"
                ));
                return Ok(result);
            }
            Err(e) => {
                // persist failed (e.g. no recommendations yet) — fall back to
                // manual composed_payload, but include portfolio KPIs so UI
                // doesn't show "—" for portfolio value.
                crate::debug_log(&format!(
                    "[synthesis-fallback] persist_retry_global_synthesis failed ({e}), building manual payload"
                ));
                let portfolio = run_state.get("portfolio").cloned().unwrap_or(json!({}));
                let full_payload = json!({
                    "date": chrono::Utc::now().to_rfc3339(),
                    "valeur_portefeuille": portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    "plus_value_totale": portfolio.get("plus_value_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    "liquidites": portfolio.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    "synthese_marche": synthese,
                    "actions_immediates": actions,
                    "prochaine_analyse": composed.get("prochaine_analyse").cloned().unwrap_or(json!("")),
                    "opportunites_watchlist": composed.get("opportunites_watchlist").cloned().unwrap_or(json!("")),
                    "llm_utilise": "codex-mcp-partial",
                });
                let payload_clone = full_payload.clone();
                let _ = crate::run_state::patch_run_state_with(run_id, |rs| {
                    if let Some(obj) = rs.as_object_mut() {
                        obj.insert("composed_payload".to_string(), payload_clone);
                    }
                });
            }
        }
        // Mark orchestration as completed so sidebar/UI stops showing "running"
        let _ = crate::run_state::set_native_run_stage(run_id, "completed", None, None);
        Ok(json!({
            "ok": true,
            "orchestration_status": "completed",
            "run_id": run_id,
        }))
    } else {
        Err(anyhow::anyhow!("synthesis_incomplete:no_recommendations_and_finalize_not_called"))
    }
}
