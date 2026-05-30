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
pub(crate) fn line_memory_patch<F>(mutator: F)
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

/// Test-only: clear the in-memory line-memory cache so the next access
/// reloads from the (possibly freshly seeded) disk path. Keeps unit tests
/// hermetic — without this, the singleton would bleed state across tests.
#[cfg(test)]
pub fn line_memory_reset_for_tests() {
    let mut guard = lm_cache().lock().unwrap_or_else(|p| p.into_inner());
    guard.store = lm_default_store();
    guard.loaded = false;
    guard.dirty = false;
}

/// Test-only wrapper around the private `sync_line_memory` so unit tests can
/// exercise the v0.3 #22 canonical-key dedup contract without going through
/// the full codex/native batch dispatch.
///
/// v0.3.3 P0-8: the price argument is now `Option<(price, stale)>` to pin the
/// contract that `current_price=0` must never be persisted. Tests pass `None`
/// for the "no source available" case.
#[cfg(test)]
pub fn sync_line_memory_for_test(
    run_id: &str,
    ticker: &str,
    resolved_symbol: Option<&str>,
    rec: &Value,
    current_price: Option<(f64, bool)>,
) {
    sync_line_memory(run_id, ticker, resolved_symbol, rec, current_price);
}

/// Test-only wrapper around `resolve_current_price` so unit tests can exercise
/// the v0.3.3 P0-8 fallback chain without faking the full run-state.
#[cfg(test)]
pub fn resolve_current_price_for_test(
    position: Option<&Value>,
    market_entry: Option<&Value>,
    technical_snapshot: Option<&Value>,
    previous_price: Option<f64>,
) -> Option<(f64, bool)> {
    resolve_current_price(position, market_entry, technical_snapshot, previous_price)
}

/// Test-only alias for `line_memory_read` to keep test naming consistent
/// with the other `_for_test` wrappers in this module.
#[cfg(test)]
pub fn line_memory_read_for_test() -> Value {
    line_memory_read()
}

/// Test-only: thin wrapper around the private `compute_trend` so unit tests
/// can exercise the bucket-by-distinct-date contract directly.
#[cfg(test)]
pub fn compute_trend_for_test(signal_history: &[Value]) -> &'static str {
    compute_trend(signal_history)
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
                        // Sync line memory for codex mode (V2 schema).
                        // v0.3 (#22): look up resolved_symbol from the position
                        // row so cross-account dups collapse under canonical_key.
                        // v0.3.3 P0-8: route through resolve_current_price_from_state
                        // so the 4-step fallback chain populates current_price
                        // rather than relying on market-row only.
                        let resolved_owned = lookup_resolved_symbol_from_state(data_path, run_id, ticker);
                        let price = resolve_current_price_from_state(
                            data_path,
                            run_id,
                            ticker,
                            resolved_owned.as_deref(),
                        );
                        sync_line_memory(run_id, ticker, resolved_owned.as_deref(), &rec, price);
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
pub(crate) fn build_native_line_prompt(_run_id: &str, ticker: &str, nom: &str, line_type: &str, line_data: &Value) -> String {
    let position = serde_json::to_string_pretty(&line_data["position"]).unwrap_or_default();
    let market = serde_json::to_string_pretty(&line_data["market_data"]).unwrap_or_default();
    let news = serde_json::to_string_pretty(&line_data["news"]).unwrap_or_default();
    let insights = serde_json::to_string_pretty(&line_data["shared_insights"]).unwrap_or_default();
    // Reuse the shared MEMOIRE LIGNE renderer (codex/MCP path uses the same
    // helper). Two reasons: (1) parity contract — the 3 LLM modes must see
    // the same memory rendering, see `docs/llm-mode-parity-contract.md`;
    // (2) token budget — the bounded human renderer is materially shorter
    // than `serde_json::to_string_pretty(line_memory)` on V2 memory (10
    // signals + 15 themes), see P1-3 in po-plan-2026-05. Pinned by tests
    // `native_line_prompt_renders_memory_via_shared_renderer` and
    // `native_line_prompt_token_budget_drops_substantially_vs_raw_dump`.
    let memory_block = crate::llm_prompts::build_memory_section(
        line_data.get("line_memory"),
    );
    let quality = serde_json::to_string_pretty(&line_data["quality"]).unwrap_or_default();
    // Reuse the shared TECHNIQUE renderer to guarantee parity across the 3
    // LLM modes (codex / native / native-oauth) — see llm_prompts.rs.
    let section_technical = crate::llm_prompts::build_technical_section(
        line_data.get("technical_snapshot"),
    );

    // P1-59: optional cross-portfolio conviction calibration block,
    // computed ONCE per run by the dispatcher and threaded through the
    // line envelope. When absent (legacy callers, tests with empty
    // history, sub-5 cold start) we render an empty string — the
    // prompt format string normalises that into a blank line that the
    // LLM ignores. See `llm_prompts::build_conviction_calibration_section`
    // for the rendering rules + the cold-start cutoff.
    let calibration_block: String = line_data
        .get("calibration_stats")
        .and_then(|v| v.as_object())
        .map(|_obj| {
            // Deserialize the envelope shape back to SignalAccuracyStats
            // for the renderer. We only need the fields the renderer
            // reads (total_signals + by_conviction), which keeps this
            // deserialization cheap and robust to extra keys.
            let stats = parse_calibration_stats(line_data.get("calibration_stats"));
            crate::llm_prompts::build_conviction_calibration_section(&stats)
        })
        .unwrap_or_default();

    // P1-59 — only reference the calibration block in INSTRUCTIONS
    // when it actually rendered, otherwise we'd point the LLM at a
    // section that does not exist in the prompt.
    let calibration_instruction: &str = if calibration_block.is_empty() {
        ""
    } else {
        "\nConsidere la section CALIBRATION CONVICTION ci-dessus quand tu fixes ta conviction (tu ne peux pas etre \"forte\" si ta tier \"forte\" est durablement <50 %)."
    };

    // Watchlist Curation v2 (D1): swap the verdict vocabulary + schema for
    // watchlist proposals. Same `LineSchemaFrame` single source as the codex
    // and repair builders — parity contract.
    let frame = crate::llm_prompts::LineSchemaFrame::for_line(line_type == "watchlist");
    let watchlist_framing = if frame.framing_intro.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", frame.framing_intro)
    };
    // The native schema is a bullet list, not a JSON block, so the
    // verdict_validation field is rendered as its own bullet (or omitted).
    let verdict_validation_bullet = if line_type == "watchlist" {
        format!(
            "\n- verdict_validation: {} (coherent avec signal: ENTRER/ACHAT_SUR_REPLI=>valide, SURVEILLER=>a_surveiller, ECARTER=>ecartee)",
            crate::llm_prompts::WATCHLIST_VERDICT_VALIDATION_ENUM
        )
    } else {
        String::new()
    };

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

MEMOIRE LIGNE = accountability de nos analyses precedentes (signaux et theses Alfred). TECHNIQUE = etat marche actuel calcule sur ~250 jours OHLC (independant des runs). Les deux se completent : la memoire dit ce qu'on a annonce, la technique dit ce que dit le marche.
{watchlist_framing}
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

{memory_block}

{calibration_block}

{section_technical}

Qualite des donnees:
{quality}

=== INSTRUCTIONS ===

Reponds UNIQUEMENT avec un objet JSON (pas de texte avant ou apres) contenant :
- line_id: "{line_type}:{ticker}"
- ticker: "{ticker}", type: "{line_type}", nom: "{nom}"
- signal: {signal_enum}{verdict_validation_bullet}
- conviction: faible | moderee | forte
- {target_weight_rule_line}
- synthese: minimum 150 caracteres (explique comme a un ami)
- memory_narrative: 4-8 phrases, construites comme un FIL HISTORIQUE (ce qui a change depuis les derniers runs), en integrant les signaux precedents, les operations recentes (transactions/orders), et les implications pour la suite
- analyse_technique, analyse_fondamentale, analyse_sentiment
- raisons_principales: 3-5 raisons (array)
- risques, catalyseurs, badges_keywords: arrays
- {action_rule_line}
- deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
- deep_news_memory_summary: MEMOIRE EVOLUTIVE 140-700 chars (OBLIGATOIRE) qui fusionne memoire precedente + nouveaux insights; conserve le fil narratif multi-run, elimine les repetitions, et explicite ce qui change vs ce qui reste valide
- deep_news_quality_score: 0-100
- deep_news_relevance: high|medium|low
- deep_news_staleness: fresh|recent|stale
- extracted_fundamentals: {{ "pe_ratio": ..., "revenue_growth": ..., "profit_margin": ..., "debt_to_equity": ... }} (si trouves via web ou calcules)
- shared_insights: {{ "analyse_technique": "...", "analyse_fondamentale": "...", "analyse_sentiment": "...", "risques": "...", "catalyseurs": "..." }}
- reanalyse_after: date ISO, reanalyse_reason

Si les donnees sont insuffisantes, tu peux faire UNE recherche web (pas plus) pour completer.
Sois concret: chiffres, montants, dates. Pas de generalites.{calibration_instruction}
Les articles "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement."#,
        ticker = ticker,
        nom = nom,
        line_type = line_type,
        position = position,
        market = market,
        news = news,
        insights = insights,
        memory_block = memory_block,
        calibration_block = calibration_block,
        calibration_instruction = calibration_instruction,
        section_technical = section_technical,
        quality = quality,
        target_weight_rule_line = crate::llm_prompts::TARGET_WEIGHT_PCT_SCHEMA_LINE,
        watchlist_framing = watchlist_framing,
        signal_enum = frame.signal_enum,
        verdict_validation_bullet = verdict_validation_bullet,
        action_rule_line = frame.action_rule_line,
    )
}

/// P1-59 — deserialize the JSON envelope shape produced by
/// `signal_accuracy::stats_to_json` back into a `SignalAccuracyStats`
/// for the calibration renderer. We only populate the fields the
/// renderer reads (`total_signals` + `by_conviction`), leaving the rest
/// at default — this keeps the parsing trivial and robust to extra
/// fields the caller may inject for future features.
fn parse_calibration_stats(
    value: Option<&Value>,
) -> crate::signal_accuracy::SignalAccuracyStats {
    use crate::signal_accuracy::{
        ConvictionBreakdown, ConvictionStats, SignalAccuracyStats,
    };
    let mut stats = SignalAccuracyStats::default();
    let Some(obj) = value.and_then(|v| v.as_object()) else { return stats; };
    stats.total_signals = obj.get("total_signals").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let read_tier = |tier: &Value| -> ConvictionStats {
        ConvictionStats {
            correct: tier.get("correct").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            incorrect: tier.get("incorrect").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            n: tier.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        }
    };
    if let Some(bc) = obj.get("by_conviction").and_then(|v| v.as_object()) {
        let mut bd = ConvictionBreakdown::default();
        if let Some(f) = bc.get("forte") { bd.forte = read_tier(f); }
        if let Some(m) = bc.get("moderee") { bd.moderee = read_tier(m); }
        if let Some(w) = bc.get("faible") { bd.faible = read_tier(w); }
        stats.by_conviction = bd;
    }
    stats
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

        // Enforce an evolving deep-news memory authored by the LLM.
        let deep_news_summary = rec.get("deep_news_summary").and_then(|v| v.as_str()).unwrap_or("").trim();
        let deep_news_memory_summary = rec.get("deep_news_memory_summary").and_then(|v| v.as_str()).unwrap_or("").trim();
        if !deep_news_summary.is_empty() && deep_news_memory_summary.is_empty() {
            last_issues = vec!["deep_news_memory_summary_missing".to_string()];
            crate::debug_log(&format!(
                "native_line: {ticker} attempt {}: missing deep_news_memory_summary while deep_news_summary is present",
                attempt + 1
            ));
            continue;
        }

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

/// Compute the line-memory `by_ticker` key for a (ticker, resolved_symbol)
/// pair. Prefers the resolved canonical Yahoo symbol when present so two
/// brokers carrying the same security (e.g. PEA `STMPA` + CTO `STM.MI`)
/// share a single line-memory entry under `STMPA.PA`. Falls back to the
/// uppercased broker ticker when no resolution is available.
///
/// v0.3 (#22) cross-account dedup primitive. Pure function, exposed for unit
/// tests so the dedup contract stays pinned.
pub(crate) fn canonical_line_memory_key(ticker: &str, resolved_symbol: Option<&str>) -> String {
    if let Some(symbol) = resolved_symbol.map(str::trim).filter(|s| !s.is_empty()) {
        return symbol.to_uppercase();
    }
    ticker.trim().to_uppercase()
}

/// Read a `by_ticker` entry honouring v0.3 canonical-key dedup with legacy
/// fallback. Tries `canonical_key` first (where new v0.3 writes land), then
/// falls back to the raw ticker (so pre-v0.3 entries keyed by broker ticker
/// remain readable without an eager migration). Returns `Value::Null` when
/// neither key exists.
pub(crate) fn read_line_memory_entry(store: &Value, ticker: &str, resolved_symbol: Option<&str>) -> Value {
    let canonical_key = canonical_line_memory_key(ticker, resolved_symbol);
    let by_ticker = match store.get("by_ticker") {
        Some(bt) => bt,
        None => return Value::Null,
    };
    if let Some(entry) = by_ticker.get(&canonical_key) { // LINT-ALLOW: canonical-key helper itself
        return entry.clone();
    }
    // Legacy fallback: pre-v0.3 entries written under the raw ticker key.
    let raw_key = ticker.trim().to_uppercase();
    if raw_key != canonical_key {
        if let Some(entry) = by_ticker.get(&raw_key) { // LINT-ALLOW: canonical-key helper itself
            return entry.clone();
        }
    }
    Value::Null
}

/// Canonical-aware reader for line-memory entries when the caller does not
/// have a `resolved_symbol` in hand (Tauri command handlers receive the raw
/// broker ticker only). Implements the v0.3 (#22) canonical-key dedup
/// **read** contract that mirrors the **write** contract pinned by
/// `canonical_line_memory_key` + `sync_line_memory`:
///
/// 1. Try `by_ticker[raw_ticker.to_uppercase()]` — hits pre-v0.3 entries and
///    any post-v0.3 entry whose canonical key equals the broker ticker
///    (e.g. tickers without ISIN resolution).
/// 2. Scan `by_ticker` values for an entry whose `entry["ticker"]` field
///    matches the raw ticker. `sync_line_memory` always writes the broker
///    ticker into the entry body even when the map key is canonical
///    (`STMPA.PA` key, `"ticker": "STMPA"` body), so this catches every
///    canonical-keyed entry without forcing readers to know the resolved
///    Yahoo symbol up-front.
/// 3. Return `None` when neither path matches.
///
/// This is the **single sanctioned entry point** for `by_ticker` reads in
/// command-handler / CLI / scorecard code paths. Any direct
/// `by_ticker.get(&raw_ticker)` lookup is a latent regression in the
/// canonical-key class (see `feedback_contract_tests_regression_audit` —
/// the third occurrence in this class shipped as v0.3.0).
///
/// Returns `Option<&Value>` (borrow) to avoid forcing a clone on the hot
/// path — callers that need ownership can `.cloned()`.
pub(crate) fn resolve_line_memory_key<'a>(store: &'a Value, raw_ticker: &str) -> Option<&'a Value> {
    let by_ticker = store.get("by_ticker").and_then(|v| v.as_object())?;
    let raw_key = raw_ticker.trim().to_uppercase();
    if raw_key.is_empty() {
        return None;
    }
    // Step 1: direct hit (canonical == raw, or pre-v0.3 legacy entry).
    if let Some(entry) = by_ticker.get(&raw_key) { // LINT-ALLOW: canonical-key helper itself
        return Some(entry);
    }
    // Step 2: canonical-keyed entry whose body carries the raw ticker.
    // O(N) over by_ticker size (capped to portfolio size, ~50 in practice).
    by_ticker.values().find(|entry| {
        entry
            .get("ticker")
            .and_then(|v| v.as_str())
            .map(|t| t.trim().eq_ignore_ascii_case(raw_key.as_str()))
            .unwrap_or(false)
    })
}

/// Sync a validated recommendation into the persistent line-memory.json store.
/// V2 schema — clean break from V1. Writes `schema_version: 2`,
/// `signal_history`, `memory_narrative`, `news_themes`, `trend`, `price_tracking`.
/// V1 fields (`llm_memory_summary`, `llm_strong_signals`, `llm_key_history`) are NOT written.
/// NOTE: `memory_narrative` replaces the former `key_reasoning` field name.
///
/// v0.3 (#22): writes under `canonical_line_memory_key(ticker, resolved_symbol)`
/// so cross-account duplicates of the same security share a single entry.
/// `resolved_symbol` may be empty for tickers without ISIN resolution; in that
/// case the key falls back to the uppercased ticker (legacy shape).
/// v0.3.3 P0-8 — resolve a usable `current_price` for a ticker via a four-step
/// fallback chain. NEVER returns `Some(0.0)` — a zero price has no measurable
/// drift and the scorecard treats it as "no data", so persisting 0 would mask
/// the failure. Returns `None` when every source is empty/zero: callers MUST
/// leave the field absent rather than writing 0.
///
/// Fallback order (P1 covers ~99% of cases in practice):
///   P1 — `portfolio.positions[i].prix_actuel` when > 0
///        Finary always populates it; priced CSVs and backfilled CSVs too.
///   P2 — `market[ticker].spot` (or `price`/`last_price`/`cours`/`prix_actuel`)
///        when > 0 AND the source is real (not the `"none"` PRU fallback).
///   P3 — derived from technicals: `high_52w * (1 + current_vs_high_52w_pct / 100)`
///        when both indicators are present and positive.
///   P4 — last-known-good (previous run's `current_price`) with `stale=true`.
///
/// The boolean component of the tuple flags P4 (stale = `true`); P1-P3 are
/// considered fresh.
pub(crate) fn resolve_current_price(
    position: Option<&Value>,
    market_entry: Option<&Value>,
    technical_snapshot: Option<&Value>,
    previous_price: Option<f64>,
) -> Option<(f64, bool)> {
    fn as_f64_loose(v: Option<&Value>) -> Option<f64> {
        match v {
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(s)) => s.trim().replace(',', ".").parse::<f64>().ok(),
            _ => None,
        }
    }
    fn positive(v: Option<f64>) -> Option<f64> {
        v.filter(|x| x.is_finite() && *x > 0.0)
    }

    // P1 — portfolio prix_actuel
    if let Some(p) = position.and_then(|p| as_f64_loose(p.get("prix_actuel"))) {
        if let Some(p) = positive(Some(p)) {
            return Some((p, false));
        }
    }

    // P2 — market spot, guarded by source != "none"
    if let Some(market) = market_entry {
        let source = market
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if crate::native_collection_helpers::is_real_market_source(source) {
            let spot = market
                .get("spot")
                .or_else(|| market.get("price"))
                .or_else(|| market.get("last_price"))
                .or_else(|| market.get("cours"))
                .or_else(|| market.get("prix_actuel"));
            if let Some(p) = positive(as_f64_loose(spot)) {
                return Some((p, false));
            }
        }
    }

    // P3 — derive from technicals
    if let Some(indicators) = technical_snapshot.and_then(|t| t.get("indicators")) {
        let high = positive(as_f64_loose(indicators.get("high_52w")));
        let from_high = as_f64_loose(indicators.get("current_vs_high_52w_pct"));
        if let (Some(h), Some(pct)) = (high, from_high) {
            let derived = h * (1.0 + pct / 100.0);
            if let Some(p) = positive(Some(derived)) {
                return Some((p, false));
            }
        }
    }

    // P4 — last-known-good
    if let Some(p) = positive(previous_price) {
        return Some((p, true));
    }

    None
}

/// Wrap `resolve_current_price` so it can be driven from the run-state cache
/// (codex batch path: only `data_dir + run_id + ticker` are available at merge
/// time). Pulls `portfolio.positions[ticker]`, `market[ticker]`,
/// `technicals[ticker]`, and the previous run's `price_tracking.current_price`
/// from cached line-memory, then defers to `resolve_current_price`.
pub(crate) fn resolve_current_price_from_state(
    data_dir: &std::path::Path,
    run_id: &str,
    ticker: &str,
    resolved_symbol: Option<&str>,
) -> Option<(f64, bool)> {
    let ticker_upper = ticker.trim().to_uppercase();
    if ticker_upper.is_empty() {
        return None;
    }
    let state = crate::run_state_cache::load(data_dir, run_id).ok()?;

    let position = state
        .get("portfolio")
        .and_then(|p| p.get("positions"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .find(|row| {
            row.get("ticker")
                .and_then(|v| v.as_str())
                .map(|t| t.trim().eq_ignore_ascii_case(&ticker_upper))
                .unwrap_or(false)
        })
        .cloned();

    let ticker_lower = ticker_upper.to_lowercase();
    let market_entry = state
        .get("market")
        .and_then(|m| m.get(ticker_upper.as_str()).or_else(|| m.get(ticker_lower.as_str())))
        .cloned();

    let technical_snapshot = state
        .get("technicals")
        .and_then(|t| t.get(ticker_upper.as_str()).or_else(|| t.get(ticker_lower.as_str())))
        .cloned();

    let previous_price = {
        let store = line_memory_read();
        let entry = read_line_memory_entry(&store, &ticker_upper, resolved_symbol);
        entry
            .get("price_tracking")
            .and_then(|pt| pt.get("current_price"))
            .and_then(|v| v.as_f64())
            .filter(|p| p.is_finite() && *p > 0.0)
    };

    resolve_current_price(
        position.as_ref(),
        market_entry.as_ref(),
        technical_snapshot.as_ref(),
        previous_price,
    )
}

/// Sync a validated recommendation into the persistent line-memory store.
///
/// v0.3.3 P0-8: `current_price` is `Option<(price, stale)>`. `None` means "no
/// source produced a usable price" — `current_price` is left absent in the
/// persisted `price_tracking` (NEVER 0). `Some((p, true))` means we kept the
/// last-known-good and the UI surfaces a staleness marker.
fn sync_line_memory(
    run_id: &str,
    ticker: &str,
    resolved_symbol: Option<&str>,
    rec: &Value,
    current_price: Option<(f64, bool)>,
) {
    let ticker = ticker.trim().to_uppercase();
    if ticker.is_empty() { return; }
    let canonical_key = canonical_line_memory_key(&ticker, resolved_symbol);

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let today = now[..10].to_string(); // "YYYY-MM-DD" — safe, ASCII only

    // Read current entry from cache (for merge). Honours canonical key with
    // legacy-ticker fallback so a v0.3 run picks up history written under the
    // pre-v0.3 raw-ticker key without an eager migration.
    let current = {
        let store = line_memory_read();
        let entry = read_line_memory_entry(&store, &ticker, resolved_symbol);
        if entry.is_null() {
            json!({})
        } else {
            // P1-82 Layer-1: never carry a banned run's signal_history forward
            // into a fresh write. If the surgical purge missed this entry, the
            // ban set still keeps the poisoned signal out of the new history.
            let banned = crate::run_deletion::load_banned_run_ids();
            sanitize_entry_for_banned_runs(&entry, &banned)
        }
    };

    // Extract fields from recommendation
    let signal = as_str(rec.get("signal"));
    let conviction = as_str(rec.get("conviction"));
    let synthese = as_str(rec.get("synthese"));

    // For signal_history.price_at_signal we want the freshest non-stale value
    // (so an analysis dated today is anchored to today's market), but never 0.
    // When even the stale fallback is empty, omit the anchor by writing null —
    // downstream scorecard already gates on `price_at_signal > 0`.
    //
    // v0.3.3 P0-8 contract: `resolve_current_price` never returns `Some(0.0)`,
    // but we re-assert here to defend against test fixtures or any future
    // caller that bypasses the resolver. A non-positive price is treated as
    // "no usable price" — same as `None`.
    let (price_value, price_is_stale) = match current_price {
        Some((p, s)) if p.is_finite() && p > 0.0 => (Some(p), s),
        _ => (None, false),
    };
    let signal_anchor_price: Value = price_value
        .map(|p| json!(p))
        .unwrap_or(Value::Null);

    // ── V2 P1-5: signal_history guard ─────────────────────────────────
    // Skip writing the new signal entry when we don't have a usable
    // `price_at_signal`. A signal without a price anchor cannot be scored
    // (the scorecard gates on `price_at_signal > 0`), so persisting it
    // pollutes `signal_history` with non-scorable rows that look real to the
    // LLM and the UI. Prior history is preserved as-is — the next run with a
    // healthy price will be the first to advance the history forward.
    let prior = current.get("signal_history").and_then(|v| v.as_array());
    let signal_history: Vec<Value> = if price_value.is_some() {
        let new_signal_entry = json!({
            "date": today,
            "signal": signal,
            "conviction": conviction,
            "price_at_signal": signal_anchor_price,
            "run_id": run_id,
        });
        build_signal_history(&new_signal_entry, prior)
    } else {
        crate::debug_log(&format!(
            "sync_line_memory: skipping signal_history append for {ticker} run {run_id} — no usable price_at_signal"
        ));
        prior.cloned().unwrap_or_default()
    };

    // ── V2 P1-5: in-line migration of legacy zero-price entries ────────
    // Best-effort backfill: when we have a fresh price for this ticker but the
    // existing `signal_history` carries entries written before P0-8 (or by an
    // older run during a provider outage) with `price_at_signal == 0`,
    // substitute the current price as a proxy and tag the entry with
    // `price_at_signal_source: "migration_proxy"` for an audit trail. Pure
    // best-effort — the proxy is the freshest known price, not the actual
    // historical price; we trade fidelity for scorability.
    //
    // Idempotent: only touches entries with `price_at_signal == 0`. After the
    // first migration write, the entries carry the proxy price and are no
    // longer matched.
    let (signal_history, migrated_count) = if let Some(proxy) = price_value {
        migrate_zero_price_at_signal_entries(signal_history, proxy)
    } else {
        (signal_history, 0)
    };
    if migrated_count > 0 {
        crate::debug_log(&format!(
            "sync_line_memory: migrated {migrated_count} zero price_at_signal entries for {ticker} run {run_id} (proxy={:.4})",
            price_value.unwrap_or(0.0)
        ));
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

    // ── V2 P1-4: key_reasoning (compressed 3-sentence thesis distilled from
    // `synthese`, overwritten each run per the V2 spec). The Rust code's
    // earlier rename to `memory_narrative` left `key_reasoning` as an absent
    // field for all tickers persisted through `sync_line_memory` —
    // re-introducing it satisfies the V2 schema contract and unblocks the
    // 88% of tickers (notably `.PA` suffixed) where it was null in prod.
    //
    // Guard: a run with an empty `synthese` must NOT overwrite a previously
    // captured `key_reasoning`. The non-empty guard mirrors the
    // `memory_narrative` fallback chain and prevents distilled context from
    // being wiped by a degraded run.
    let key_reasoning: Value = non_empty_str(rec.get("key_reasoning"))
        .or_else(|| {
            let distilled = extract_first_sentences(&synthese, 3);
            if distilled.is_empty() {
                None
            } else {
                Some(distilled)
            }
        })
        .or_else(|| non_empty_str(current.get("key_reasoning")))
        .map(Value::String)
        .unwrap_or(Value::Null);

    // ── V2: news_themes (merge from badges_keywords, cap at 15) ────
    let news_themes = merge_string_list(
        json_str_array(rec.get("badges_keywords"))
            .chain(json_str_array(current.get("news_themes"))),
        15,
    );

    // ── V2: trend (computed from last 3 signal_history entries) ─────

    // ── V2: price_tracking (accuracy vs previous signal) ───────────
    let price_tracking = compute_price_tracking(&current, price_value, price_is_stale, &signal);

    // ── Deep news fields (preserved across V1→V2) ──────────────────
    let deep_news_summary = {
        let llm_memory = non_empty_str(rec.get("deep_news_memory_summary"));
        let fresh = non_empty_str(rec.get("deep_news_summary"));
        let prev_memory = non_empty_str(current.get("deep_news_memory_summary"));
        llm_memory
            .or_else(|| compose_deep_news_memory_fallback(prev_memory.as_deref(), fresh.as_deref()))
            .unwrap_or_default()
    };

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

    // Bug B follow-up — clear the zero-price repair flag as soon as a real
    // fresh price comes in. If the new run has no fresh price (None or stale),
    // carry the flag forward so the next prompt build still suppresses the
    // bogus "prix: 0.00€" line.
    let has_fresh_price = matches!(current_price, Some((_, false)));
    let price_data_unavailable = if has_fresh_price {
        false
    } else {
        current.get("price_data_unavailable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };

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
        "key_reasoning": key_reasoning,
        "price_tracking": price_tracking,
        "price_data_unavailable": price_data_unavailable,
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
    let write_key = canonical_key.clone();
    let legacy_key = if ticker != canonical_key { Some(ticker.clone()) } else { None };
    let run_id_log = run_id.to_string();
    line_memory_patch(move |store| {
        // Ensure by_ticker exists
        if !store.get("by_ticker").and_then(|v| v.as_object()).is_some() {
            store["by_ticker"] = json!({});
        }
        if let Some(bt) = store.get_mut("by_ticker").and_then(|v| v.as_object_mut()) {
            // v0.3 (#22): write under canonical key so cross-account dups
            // collapse into a single entry. Remove any pre-v0.3 raw-ticker
            // entry so reads don't bifurcate after the first canonical write
            // — the merged content is already carried forward by the
            // `current` snapshot read above.
            if let Some(raw_key) = legacy_key.as_ref() {
                bt.remove(raw_key);
            }
            bt.insert(write_key.clone(), entry);
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
    crate::debug_log(&format!("sync_line_memory: updated V2 for {ticker} (key={canonical_key}) run {run_id_log} (cached)"));
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

/// v0.3.3 P1-5: walk a `signal_history` slice and backfill any entry with
/// `price_at_signal == 0` (or null/missing) using `proxy_price` as a
/// best-effort historical anchor. The replacement carries an audit trail
/// field `"price_at_signal_source": "migration_proxy"` so consumers can
/// distinguish a real captured price from a backfill.
///
/// Returns the (possibly mutated) history vector and the number of entries
/// that were repaired. The function is pure and idempotent — calling it
/// twice with the same `proxy_price` and an already-migrated history yields
/// the same vector with zero new migrations.
///
/// `proxy_price` must be `> 0`; callers that have no positive price should
/// skip the call rather than passing 0, otherwise the migration would write
/// another zero and the audit trail would lie.
pub(crate) fn migrate_zero_price_at_signal_entries(
    history: Vec<Value>,
    proxy_price: f64,
) -> (Vec<Value>, usize) {
    if !proxy_price.is_finite() || proxy_price <= 0.0 {
        return (history, 0);
    }
    let mut migrated = 0usize;
    let history = history
        .into_iter()
        .map(|entry| {
            let needs_repair = entry
                .get("price_at_signal")
                .map(|v| match v {
                    Value::Number(n) => n.as_f64().map(|x| x <= 0.0).unwrap_or(true),
                    Value::Null => true,
                    _ => true,
                })
                .unwrap_or(true);
            if !needs_repair {
                return entry;
            }
            let mut obj = match entry {
                Value::Object(o) => o,
                // Non-object entries are malformed; leave them alone so we
                // don't silently swallow corrupt data.
                other => return other,
            };
            obj.insert("price_at_signal".to_string(), json!(proxy_price));
            obj.insert(
                "price_at_signal_source".to_string(),
                Value::String("migration_proxy".to_string()),
            );
            migrated += 1;
            Value::Object(obj)
        })
        .collect();
    (history, migrated)
}

/// Build the next-run `signal_history` array. If the prior head entry has
/// the same `(date, signal)` as the new entry, REPLACE it (a same-day re-run
/// with an unchanged signal must not produce a duplicate row in the widget —
/// the new entry's price/run_id is fresher). Otherwise prepend the new
/// entry. Cap at 10.
///
/// Note: same date with a *different* signal (rare — strategy/news shift
/// mid-day) is treated as a legitimate new decision point and still
/// prepends; we only dedup the exact `(date, signal)` pair.
///
/// Price preservation rule: on a same-day same-signal replace, if the new
/// entry has `price_at_signal == 0.0` but the existing head carries a
/// non-zero price, KEEP the existing head's `price_at_signal`. This guards
/// against a fresh run where the PRU-fallback chain (see
/// `apply_pru_fallback_to_market_row` / `resolve_current_price`) produced no
/// usable current price;
/// the previously-recorded real price is more truthful than the regression
/// to zero. `run_id`/`conviction` from the new entry are still adopted.
pub(crate) fn build_signal_history(new_entry: &Value, prior: Option<&Vec<Value>>) -> Vec<Value> {
    const CAP: usize = 10;
    let new_date = new_entry.get("date").and_then(|v| v.as_str()).unwrap_or("");
    let new_signal = new_entry.get("signal").and_then(|v| v.as_str()).unwrap_or("");

    let prior_head = prior.and_then(|arr| arr.first());
    let head_matches = prior_head
        .map(|head| {
            head.get("date").and_then(|v| v.as_str()) == Some(new_date)
                && head.get("signal").and_then(|v| v.as_str()) == Some(new_signal)
        })
        .unwrap_or(false);

    // When the new entry would replace the head with a zero price but the
    // existing head has a real price, splice the existing price into the
    // replacement instead of dropping it on the floor.
    let head_entry = if head_matches {
        let new_price = new_entry
            .get("price_at_signal")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let prior_price = prior_head
            .and_then(|h| h.get("price_at_signal"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if new_price == 0.0 && prior_price > 0.0 {
            let mut merged = new_entry.clone();
            if let Some(obj) = merged.as_object_mut() {
                obj.insert(
                    "price_at_signal".to_string(),
                    json!(prior_price),
                );
            }
            merged
        } else {
            new_entry.clone()
        }
    } else {
        new_entry.clone()
    };

    let mut out: Vec<Value> = Vec::with_capacity(CAP);
    out.push(head_entry);
    if let Some(arr) = prior {
        let skip = if head_matches { 1 } else { 0 };
        let keep = CAP - out.len();
        for item in arr.iter().skip(skip).take(keep) {
            out.push(item.clone());
        }
    }
    out
}

/// Collapse a `signal_history` slice into newest-first representatives keyed
/// by `(date, signal)`. Used so legacy contaminated histories (multiple
/// same-day same-signal entries from before Fix 5.1) don't poison trend /
/// accuracy windows that slice `take(3) / skip(3)`.
pub(crate) fn dedupe_signal_history_by_date_signal(signal_history: &[Value]) -> Vec<Value> {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::with_capacity(signal_history.len());
    for entry in signal_history.iter() {
        let date = entry.get("date").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let signal = entry.get("signal").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if seen.insert((date, signal)) {
            out.push(entry.clone());
        }
    }
    out
}

/// Compute trend from last 3 signal_history entries.
/// - `upgrading`: signals move toward stronger buy
/// - `downgrading`: signals move toward sell
/// - `volatile`: alternates direction >= 2 times
/// - `stable`: same signal repeated
fn compute_trend(signal_history: &[Value]) -> &'static str {
    // Bucket by (date, signal) first so contaminated legacy histories with
    // same-day duplicates don't dominate the recent-3 window.
    let bucketed = dedupe_signal_history_by_date_signal(signal_history);
    if bucketed.len() < 2 { return "stable"; }

    let signals: Vec<i8> = bucketed.iter()
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
pub(crate) fn is_bullish_signal(signal: &str) -> bool {
    matches!(signal, "ACHAT_FORT" | "ACHAT" | "RENFORCEMENT")
}

/// Score a signal's outcome: the 1-decimal return percentage from `anchor`
/// to `current` and the accuracy verdict (`correct` when the price moved in
/// the signal's expected direction). Returns `(0.0, "unknown")` when either
/// price is missing or non-positive — the scorecard gates on a real anchor.
/// Single source of truth for the return/accuracy formula shared by
/// `compute_price_tracking` (prev-vs-current model) and
/// `recompute_price_tracking_from_head` (head-as-anchor model).
fn score_return_and_accuracy(
    anchor: Option<f64>,
    current: Option<f64>,
    signal: &str,
) -> (f64, &'static str) {
    match (anchor, current) {
        (Some(a), Some(c)) if a > 0.0 && c > 0.0 => {
            let r = ((c - a) / a * 100.0 * 10.0).round() / 10.0;
            let acc = if is_bullish_signal(signal) == (r > 0.0) {
                "correct"
            } else {
                "incorrect"
            };
            (r, acc)
        }
        _ => (0.0, "unknown"),
    }
}

/// Compute price_tracking from previous signal data and current price.
/// Compute the `price_tracking` block for line-memory.
///
/// v0.3.3 P0-8: `current_price` is `Option<f64>` and the `stale` flag is
/// surfaced as a sibling field. When no fresh OR stale price is available, the
/// block's `current_price` field is `null` rather than `0.0` — the scorecard
/// gates on `> 0`, and writing 0 would silently mark every signal "pending".
fn compute_price_tracking(
    current: &Value,
    current_price: Option<f64>,
    stale: bool,
    current_signal: &str,
) -> Value {
    let price_value: Value = current_price
        .filter(|p| p.is_finite() && *p > 0.0)
        .map(|p| json!(p))
        .unwrap_or(Value::Null);

    // Get the most recent signal_history entry from the PREVIOUS run
    let prev_entry = current.get("signal_history")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());

    if let Some(prev) = prev_entry {
        let prev_signal = prev.get("signal").and_then(|v| v.as_str()).unwrap_or("");
        let prev_date = prev.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let prev_anchor = prev.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let (return_pct, accuracy) =
            score_return_and_accuracy(Some(prev_anchor), current_price, prev_signal);

        json!({
            "last_signal": prev_signal,
            "last_signal_date": prev_date,
            "price_at_signal": prev_anchor,
            "current_price": price_value,
            "stale": stale,
            "return_since_signal_pct": return_pct,
            "signal_accuracy": accuracy,
        })
    } else {
        // First analysis — no previous signal to compare against
        json!({
            "last_signal": current_signal,
            "last_signal_date": &chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "price_at_signal": price_value.clone(),
            "current_price": price_value,
            "stale": stale,
            "return_since_signal_pct": 0.0,
            "signal_accuracy": "first_analysis",
        })
    }
}

// ── P1-82: run deletion / ban — purge + recompute ───────────────
//
// When a run is deleted or banned, every `signal_history[]` entry tagged
// with the offending `run_id` must disappear and the entry's derived
// fields (`signal`, `conviction`, `trend`, `price_tracking`,
// `run_id_last_update`) must be recomputed from the NEW most-recent
// surviving signal — otherwise a deleted ACHAT keeps driving the next
// prompt's "previous call" narrative. These helpers are pure transforms
// on a single `by_ticker` entry so both the surgical-cleanup path
// (`run_deletion::delete_run`) and the read-time ban safety net
// (`build_memory_for_prompt`, codex `get_line_data`) share one funnel —
// satisfying the parity contract (uniform skip across all line-memory
// readers).

/// Remove every `signal_history[]` entry whose `run_id` is in `banned`.
/// Returns the filtered history and the number of entries removed. Pure.
/// Entries with no `run_id` (legacy / migrated) are preserved — a ban
/// targets a specific run, never untagged history.
pub(crate) fn filter_signal_history_by_banned_runs(
    history: &[Value],
    banned: &std::collections::HashSet<String>,
) -> (Vec<Value>, usize) {
    if banned.is_empty() {
        return (history.to_vec(), 0);
    }
    let mut removed = 0usize;
    let kept: Vec<Value> = history
        .iter()
        .filter(|entry| {
            let is_banned = entry
                .get("run_id")
                .and_then(|v| v.as_str())
                .map(|rid| banned.contains(rid))
                .unwrap_or(false);
            if is_banned {
                removed += 1;
            }
            !is_banned
        })
        .cloned()
        .collect();
    (kept, removed)
}

/// Recompute the `price_tracking` block for a post-purge entry directly
/// from its NEW head signal. Unlike `compute_price_tracking` (which models
/// "previous signal vs a fresh market price at run start"), this models
/// "the surviving head signal as the current anchor": the head's
/// `price_at_signal` is both the anchor and the reference, `current_price`
/// is carried over from the entry's prior `price_tracking` (best-effort —
/// no fresh market fetch happens at deletion time), and accuracy is
/// recomputed against the head's direction.
fn recompute_price_tracking_from_head(entry: &Value, head: &Value) -> Value {
    let head_signal = head.get("signal").and_then(|v| v.as_str()).unwrap_or("");
    let head_date = head.get("date").and_then(|v| v.as_str()).unwrap_or("");
    let head_anchor = head
        .get("price_at_signal")
        .and_then(|v| v.as_f64())
        .filter(|p| p.is_finite() && *p > 0.0);

    // Carry over the last known current_price (and its staleness flag) from
    // the entry's prior price_tracking — we have no fresh quote at delete time.
    let prior_pt = entry.get("price_tracking");
    let current_price = prior_pt
        .and_then(|pt| pt.get("current_price"))
        .and_then(|v| v.as_f64())
        .filter(|p| p.is_finite() && *p > 0.0);
    let stale = prior_pt
        .and_then(|pt| pt.get("stale"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let current_price_value: Value = current_price.map(|p| json!(p)).unwrap_or(Value::Null);

    let (return_pct, accuracy) =
        score_return_and_accuracy(head_anchor, current_price, head_signal);

    json!({
        "last_signal": head_signal,
        "last_signal_date": head_date,
        "price_at_signal": head_anchor.map(|p| json!(p)).unwrap_or(Value::Null),
        "current_price": current_price_value,
        "stale": stale,
        "return_since_signal_pct": return_pct,
        "signal_accuracy": accuracy,
    })
}

/// Recompute a `by_ticker` entry's derived fields from a freshly purged
/// `signal_history`. Mutates `entry` in place:
///  - `signal` / `conviction` / `run_id_last_update` ← new head entry,
///  - `trend` ← `compute_trend(history)`,
///  - `price_tracking` ← `recompute_price_tracking_from_head`.
///
/// When `history` is empty (the run was the ticker's only analysis) the
/// caller decides whether to reset-to-first-analysis or drop the entry;
/// this helper only handles the non-empty case and returns `false` for an
/// empty history so the caller can branch.
pub(crate) fn recompute_entry_derived_from_history(entry: &mut Value, history: &[Value]) -> bool {
    let obj = match entry.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    obj.insert("signal_history".to_string(), json!(history));

    let head = match history.first() {
        Some(h) => h.clone(),
        None => return false,
    };

    let head_signal = head.get("signal").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let head_conviction = head.get("conviction").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let head_run_id = head.get("run_id").and_then(|v| v.as_str()).unwrap_or("").to_string();

    obj.insert("signal".to_string(), Value::String(head_signal));
    obj.insert("conviction".to_string(), Value::String(head_conviction));
    if !head_run_id.is_empty() {
        obj.insert("run_id_last_update".to_string(), Value::String(head_run_id));
    }
    obj.insert("trend".to_string(), Value::String(compute_trend(history).to_string()));

    let pt = recompute_price_tracking_from_head(entry, &head);
    if let Some(o) = entry.as_object_mut() {
        o.insert("price_tracking".to_string(), pt);
    }
    true
}

/// Clear an entry's signal-derived state back to a first-analysis baseline:
/// empty `signal_history`, blank `signal`/`conviction`, `stable` trend, null
/// `price_tracking`. Leaves identity (`ticker`) and deep-news caches intact.
/// Shared by the read-time ban sanitizer and the surgical delete reset so
/// the "what does first-analysis look like" contract has one definition.
pub(crate) fn reset_entry_signal_state(entry: &mut Value) {
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("signal_history".to_string(), json!([]));
        obj.insert("signal".to_string(), Value::String(String::new()));
        obj.insert("conviction".to_string(), Value::String(String::new()));
        obj.insert("trend".to_string(), Value::String("stable".to_string()));
        obj.insert("price_tracking".to_string(), Value::Null);
    }
}

/// Read-time ban safety net (Layer 1, P1-82): return a sanitized clone of a
/// `by_ticker` entry with every banned `signal_history` entry removed and
/// derived fields recomputed from the surviving head. When nothing is banned
/// (the common path) the entry is returned unchanged — zero allocation churn
/// on the hot path. This is the SINGLE funnel both line-memory readers use
/// (`build_memory_for_prompt` for native/oauth, `get_line_data` for codex),
/// so the skip is uniform across all 3 LLM modes (parity contract).
pub(crate) fn sanitize_entry_for_banned_runs(
    entry: &Value,
    banned: &std::collections::HashSet<String>,
) -> Value {
    if banned.is_empty() || !entry.is_object() {
        return entry.clone();
    }
    let history: Vec<Value> = entry
        .get("signal_history")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let (filtered, removed) = filter_signal_history_by_banned_runs(&history, banned);
    if removed == 0 {
        return entry.clone();
    }
    let mut sanitized = entry.clone();
    if filtered.is_empty() {
        // The entire visible history belonged to banned runs — present the
        // ticker as a first analysis so the prompt doesn't reference a
        // poisoned signal.
        reset_entry_signal_state(&mut sanitized);
    } else {
        recompute_entry_derived_from_history(&mut sanitized, &filtered);
    }
    sanitized
}

// ── Line memory helpers ─────────────────────────────────────────

/// Look up `resolved_symbol` for `ticker` from the cached run state's
/// `portfolio.positions`. Returns `None` when the run state can't be loaded,
/// the ticker isn't found, or the row has no resolution.
///
/// v0.3 (#22): used by the codex batch dispatch (which has only ticker + rec
/// at merge time) so it can route line-memory writes under the canonical key.
pub(crate) fn lookup_resolved_symbol_from_state(
    data_dir: &std::path::Path,
    run_id: &str,
    ticker: &str,
) -> Option<String> {
    let state = crate::run_state_cache::load(data_dir, run_id).ok()?;
    lookup_resolved_symbol_from_state_value(&state, ticker)
}

/// Pure-function core of `lookup_resolved_symbol_from_state`, exposed for
/// unit tests so we don't round-trip through the run-state cache.
pub(crate) fn lookup_resolved_symbol_from_state_value(state: &Value, ticker: &str) -> Option<String> {
    let upper = ticker.trim().to_uppercase();
    state
        .get("portfolio")
        .and_then(|p| p.get("positions"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .find(|row| {
            row.get("ticker")
                .and_then(|v| v.as_str())
                .map(|t| t.trim().to_uppercase() == upper)
                .unwrap_or(false)
        })
        .and_then(|row| row.get("resolved_symbol").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn as_str(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

fn non_empty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn compose_deep_news_memory_fallback(previous: Option<&str>, fresh: Option<&str>) -> Option<String> {
    let previous = previous.unwrap_or("").trim();
    let fresh = fresh.unwrap_or("").trim();
    match (previous.is_empty(), fresh.is_empty()) {
        (true, true) => None,
        (true, false) => Some(fresh.to_string()),
        (false, true) => Some(previous.to_string()),
        (false, false) => {
            if previous == fresh {
                Some(previous.to_string())
            } else {
                Some(format!(
                    "Memoire precedente: {} | Mise a jour recente: {}",
                    previous, fresh
                ))
            }
        }
    }
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
    // v0.3.3 P0-8: resolve current price via the four-step fallback chain so
    // line-memory never carries a stale 0.0. Previous code read only
    // market_data and wrote 0 when it was missing.
    let resolved_symbol = line_data
        .get("position")
        .and_then(|p| p.get("resolved_symbol"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let previous_price = {
        let store = line_memory_read();
        let entry = read_line_memory_entry(&store, ticker, resolved_symbol);
        entry
            .get("price_tracking")
            .and_then(|pt| pt.get("current_price"))
            .and_then(|v| v.as_f64())
            .filter(|p| p.is_finite() && *p > 0.0)
    };

    let position = line_data.get("position");
    let market_data = line_data.get("market_data");
    let technical_snapshot = line_data.get("technical_snapshot");
    let current_price = resolve_current_price(
        position,
        market_data,
        technical_snapshot,
        previous_price,
    );

    // Sync line memory (cross-run persistent state, V2 schema).
    // v0.3 (#22): resolved_symbol drives canonical key — when present, cross-
    // account dups (PEA STMPA + CTO STM.MI) collapse into a single by_ticker
    // entry keyed by the resolved Yahoo symbol.
    sync_line_memory(run_id, ticker, resolved_symbol, rec, current_price);
    let isin = line_data.get("position")
        .and_then(|p| p.get("isin"))
        .and_then(|v| v.as_str())
        .unwrap_or(ticker);

    // Persist shared insights (with optional sector analysis).
    //
    // ROOT-CAUSE FIX (2026-05-28): build the insights object from the
    // TOP-LEVEL recommendation fields the LLM actually emits — see the
    // schema in `llm_prompts::build_line_analysis_prompt` (lines 350-376).
    // The prior gate `rec.get("shared_insights")` looked for a nested
    // wrapper that the LLM never produces, so the dispatch was silently
    // skipped on every native / native-oauth run and `insights:<ISIN>`
    // stopped accumulating in the API cache once codex stopped being the
    // default backend (around 2026-05-16). Codex mode is unaffected: it
    // bypasses this code path entirely because the LLM emits
    // `tools/call persist_shared_insights` directly via JSON-RPC (see
    // `build_batch_prompt` step 8). The pure helper
    // `extract_shared_insights` is unit-tested in `tests.rs` and shares
    // the field vocabulary with the legacy `llm_parsing` path
    // (handlers.rs:1035 + 1053 on the API side).
    //
    // PARITY FIX (2026-05-17): `run_id` is forwarded so the MCP tool can
    // emit the `"sharing insights"` `line_progress` event picked up by
    // `run_stats::aggregate_from_progress_file`. Without it the
    // aggregator counts `insights_persisted=0` even when the writes
    // succeed (audit: docs/audits/collective-memory-zero-2026-05-17.md).
    if let Some(insights) = extract_shared_insights(rec) {
        let mut params = json!({
            "ticker": ticker,
            "isin": isin,
            "run_id": run_id,
            "insights": serde_json::to_string(&insights).unwrap_or_default(),
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

    // Persist extracted fundamentals
    if let Some(fundamentals) = rec.get("extracted_fundamentals") {
        if !fundamentals.is_null() {
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_extracted_fundamentals",
                &json!({
                    "ticker": ticker,
                    "isin": isin,
                    "run_id": run_id,
                    "fundamentals": serde_json::to_string(fundamentals).unwrap_or_default(),
                }),
            );
        }
    }

    // Persist deep news summary to the per-URL API cache.
    //
    // Route through `dispatch_tool_direct("persist_deep_news", …)` rather
    // than calling `persist_deep_news_if_present` directly, so the MCP tool
    // emits the `caching deep news` progress event (same parity reason as
    // insights/fundamentals above). The URL-selection logic mirrors the
    // legacy `persist_deep_news_if_present` body — prefer the first un-cached
    // article, fall back to the first article with a URL — so the on-disk
    // write shape is unchanged. Codex mode already goes through this MCP
    // tool path so it is byte-equivalent.
    let deep_news_summary = rec.get("deep_news_summary")
        .or_else(|| rec.get("deep_news_memory_summary"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !deep_news_summary.is_empty() {
        let news_value = line_data.get("news").cloned().unwrap_or(Value::Null);
        if let Some((best_url, best_title)) = pick_deep_news_target(&news_value) {
            let quality_score = rec.get("deep_news_quality_score")
                .and_then(|v| v.as_u64()).unwrap_or(50);
            let relevance = rec.get("deep_news_relevance")
                .and_then(|v| v.as_str()).unwrap_or("medium");
            let staleness = rec.get("deep_news_staleness")
                .and_then(|v| v.as_str()).unwrap_or("recent");
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_deep_news",
                &json!({
                    "ticker": ticker,
                    "isin": isin,
                    "run_id": run_id,
                    "url": best_url,
                    "title": best_title,
                    "summary": deep_news_summary,
                    "quality_score": quality_score,
                    "relevance": relevance,
                    "staleness": staleness,
                }),
            );
        }
    }
}

/// Field set the LLM emits at the TOP LEVEL of `recommendation` that
/// makes up a shareable, ticker-generic insight contribution. Mirrors
/// the legacy `llm_parsing::persist_shared_insights_if_present` field
/// list AND the API handler's `generic_fields` + `array_fields` (see
/// `apps/alfred-api/src/handlers.rs::insights_post_handler`). Any new
/// field added to the schema must land here — pinned by
/// `extract_shared_insights_matches_legacy_llm_parsing_helper` in
/// `tests.rs`.
const SHARED_INSIGHT_FIELDS: &[&str] = &[
    "analyse_technique",
    "analyse_fondamentale",
    "analyse_sentiment",
    "deep_news_summary",
    "badges_keywords",
    "risques",
    "catalyseurs",
];

/// Build the `insights` object posted to `/api/insights` from a per-line
/// LLM recommendation.
///
/// The LLM emits these fields at the top level of `recommendation` (see
/// the JSON schema in `llm_prompts::build_line_analysis_prompt` lines
/// 350-376). There is NO nested `shared_insights` wrapper — that was the
/// 2026-03-24 → 2026-05-28 bug, root-caused in commit
/// `bc76bd3` and surfaced when the default backend switched away from
/// codex around 2026-05-16. This helper restores the contribution flow
/// for native / native-oauth modes by reading the same field list the
/// legacy `llm_parsing` path always read.
///
/// Returns `None` when the recommendation carries no populated generic
/// field — the API rejects empty insight objects, so skipping the
/// round-trip is cleaner. Empty strings / empty arrays are treated as
/// "no content" and not forwarded — preserves the API guarantee that a
/// contribution never overwrites richer prior content with noise.
pub(crate) fn extract_shared_insights(rec: &Value) -> Option<Value> {
    let mut insights = serde_json::Map::new();
    for field in SHARED_INSIGHT_FIELDS {
        let Some(val) = rec.get(*field) else { continue };
        let has_content = match val {
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            _ => false,
        };
        if has_content {
            insights.insert((*field).to_string(), val.clone());
        }
    }
    if insights.is_empty() {
        return None;
    }
    Some(Value::Object(insights))
}

/// Pick the article URL to associate a fresh deep_news_summary with.
///
/// Prefers articles where `deep_summary_cached=false` (these are the ones
/// the LLM just read), otherwise falls back to the first article with a
/// URL. Returns `None` when no article has a URL — in that case we drop
/// the summary on the floor rather than caching it against a synthetic
/// key. Pure helper so the URL-selection policy is unit-testable and
/// stays parity-aligned with `llm_parsing::persist_deep_news_if_present`.
pub(crate) fn pick_deep_news_target(news: &Value) -> Option<(String, String)> {
    let articles = news.as_array()
        .or_else(|| news.get("items").and_then(|i| i.as_array()))
        .or_else(|| news.get("articles").and_then(|i| i.as_array()))?;

    // First pass: prefer an un-cached article (the one just read).
    for item in articles {
        let is_cached = item.get("deep_summary_cached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_cached {
            continue;
        }
        if let Some(url) = item.get("url").or_else(|| item.get("link"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let title = item.get("title").or_else(|| item.get("titre"))
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            return Some((url.to_string(), title));
        }
    }

    // Fallback: any article with a URL.
    for item in articles {
        if let Some(url) = item.get("url").or_else(|| item.get("link"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            let title = item.get("title").or_else(|| item.get("titre"))
                .and_then(|v| v.as_str()).unwrap_or("").to_string();
            return Some((url.to_string(), title));
        }
    }

    None
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
   - deep_news_memory_summary: memoire evolutive 140-700 chars (OBLIGATOIRE) qui integre memoire precedente + nouveaux insights, sans duplication mot-a-mot
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
- deep_news_memory_summary doit etre un narratif evolutif (ce qui change / ce qui persiste), pas une copie de deep_news_summary.
- Les articles marques "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement.
- Les articles marques "A APPROFONDIR" n'ont pas de resume — lis-les via recherche web.
- Si `activity` contient des operations recentes, commente les decisions passees (timing, prix d'achat vs cours actuel, renforcements pertinents ou non). Utilise-les pour calibrer ta recommandation."#,
        count = tickers.len(),
        run_id = run_id,
        lines_list = lines_list,
    )
}

fn build_synthesis_prompt(run_id: &str) -> String {
    let run_state = crate::load_run_by_id_direct(run_id).ok().unwrap_or_else(|| json!({}));
    let concentration = compute_theme_concentration(run_id);
    let concentration_section = build_theme_concentration_text(&concentration);
    build_synthesis_prompt_from_state(run_id, &run_state, &concentration_section)
}

/// Pure renderer for the synthesis prompt — extracted so unit tests can
/// drive arbitrary `run_state` fixtures and assert prompt contents
/// (e.g. macro section presence/absence per P1-60) without touching
/// `load_run_by_id_direct` or the theme-concentration disk path.
///
/// `concentration_section` is precomputed by the caller so this helper
/// has zero I/O dependencies. The cross-account section IS computed
/// here (off `run_state` only) since it has no disk side effects.
pub(crate) fn build_synthesis_prompt_from_state(
    run_id: &str,
    run_state: &Value,
    concentration_section: &str,
) -> String {
    let account = run_state.get("account").and_then(|v| v.as_str()).map(String::from).unwrap_or_default();
    let previous_syntheses = crate::llm_prompts::build_previous_syntheses_section_public(&account);

    // Phase 1 cross-account section — same renderer as the native/native-oauth
    // path so 3-mode LLM parity holds. Cross-account themes are computed at
    // prompt build time (not collection time) so they reflect the current
    // line_memory state.
    let cross_account_section = build_cross_account_section_with_themes(run_state);

    // P1-60 — macro briefing (US 10Y / VIX / EUR-USD / Brent). Rendered
    // BEFORE the cross-account section so the LLM reads the wider macro
    // backdrop first, then the portfolio rollup against that backdrop.
    // Returns "" when the briefing is absent (server unavailable or older
    // API), so the format-string injection is a clean no-op in that case.
    let macro_section = crate::macro_briefing::build_macro_briefing_section(run_state);

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
   Si liquidites = 0 (montant connu): uniquement VENTE/ALLEGEMENT (ou 0 action).
   Si liquidites inconnu (portfolio_summary.liquidites_known = false): ne fais
   AUCUNE supposition sur la capacite d'achat — raisonne sur les merites de
   chaque ligne sans contrainte de cash.

   OBLIGATIONS de remplissage (P2-7):
   - TU DOIS peupler `limit_price` pour TOUTE action VENTE / ALLEGEMENT /
     ACHAT / RENFORCEMENT (order_type=LIMIT). Si la rationale mentionne un
     prix (ex: "Vendre 10 titres OVH a 12,09 EUR"), reprends ce prix dans
     `limit_price` — JAMAIS null si le prix existe dans la rationale.
   - TU DOIS peupler `estimated_amount_eur` = quantity x limit_price (ou x
     prix mentionne dans la rationale si MARKET). JAMAIS null si quantity
     et un prix sont connus.
   - Un post-processing detecte un prix oublie via regex sur la rationale,
     mais comptes-y JAMAIS — peuple les champs explicitement.

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
{macro_section}
{cross_account_section}
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
        macro_section = macro_section,
        cross_account_section = cross_account_section,
    )
}

/// Read `cross_account_context` from run_state, fold in current
/// `cross_account_themes` aggregated from line_memory, then render the prompt
/// section. Used by both `build_synthesis_prompt` and `build_report_prompt`.
pub(crate) fn build_cross_account_section_with_themes(run_state: &Value) -> String {
    let mut context = match run_state.get("cross_account_context").cloned() {
        Some(v) if v.is_object() => v,
        _ => return String::new(),
    };
    // Aggregate themes at prompt time so the section sees up-to-date line_memory.
    let themes = aggregate_cross_account_themes(run_state);
    if let Some(obj) = context.as_object_mut() {
        obj.insert("cross_account_themes".to_string(), themes);
    }
    crate::native_collection_helpers::build_cross_account_prompt_section(&context)
}

/// Build cross-account themes by joining `line_memory.by_ticker[].news_themes`
/// with each ticker's account (from `run_state.portfolio.positions[].compte`).
///
/// Output shape: `[{ theme, tickers: [..], accounts: [..] }]`. Only themes
/// spanning ≥ 2 distinct accounts make it through — a theme present on a
/// single account is not "cross-account" by definition.
pub(crate) fn aggregate_cross_account_themes(run_state: &Value) -> Value {
    use std::collections::{BTreeMap, BTreeSet};

    // Ticker → account (uppercased ticker for case-insensitive lookup)
    let mut ticker_to_account: BTreeMap<String, String> = BTreeMap::new();
    if let Some(positions) = run_state.get("portfolio")
        .and_then(|p| p.get("positions"))
        .and_then(|v| v.as_array())
    {
        for pos in positions {
            let ticker = pos.get("ticker").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
            let compte = pos.get("compte").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !ticker.is_empty() && !compte.is_empty() {
                ticker_to_account.insert(ticker, compte);
            }
        }
    }

    // Walk by_ticker themes
    let store = line_memory_read();
    let by_ticker = match store.get("by_ticker").and_then(|v| v.as_object()) {
        Some(bt) => bt,
        None => return json!([]),
    };

    // theme → { tickers: set, accounts: set }
    #[derive(Default)]
    struct ThemeAgg {
        tickers: BTreeSet<String>,
        accounts: BTreeSet<String>,
    }
    let mut by_theme: BTreeMap<String, ThemeAgg> = BTreeMap::new();

    for (ticker, entry) in by_ticker {
        if ticker.starts_with('_') { continue; }
        let upper = ticker.to_uppercase();
        let account = match ticker_to_account.get(&upper) {
            Some(a) => a.clone(),
            None => continue,
        };
        if let Some(themes) = entry.get("news_themes").and_then(|v| v.as_array()) {
            for theme_val in themes {
                if let Some(slug) = theme_val.as_str() {
                    let slug = slug.trim().to_lowercase();
                    if slug.is_empty() { continue; }
                    let agg = by_theme.entry(slug).or_default();
                    agg.tickers.insert(upper.clone());
                    agg.accounts.insert(account.clone());
                }
            }
        }
    }

    // Keep only themes that span ≥ 2 accounts
    let mut result: Vec<Value> = by_theme.into_iter()
        .filter(|(_, agg)| agg.accounts.len() >= 2)
        .map(|(theme, agg)| {
            json!({
                "theme": theme,
                "tickers": agg.tickers.into_iter().collect::<Vec<_>>(),
                "accounts": agg.accounts.into_iter().collect::<Vec<_>>(),
            })
        })
        .collect();

    // Sort: most accounts first, then theme name
    result.sort_by(|a, b| {
        let an = a.get("accounts").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let bn = b.get("accounts").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        bn.cmp(&an).then_with(|| {
            let at = a.get("theme").and_then(|v| v.as_str()).unwrap_or("");
            let bt = b.get("theme").and_then(|v| v.as_str()).unwrap_or("");
            at.cmp(bt)
        })
    });

    Value::Array(result)
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

        // P2-63 (2026-05-24) — invalidate the home Section 8 signal-accuracy
        // TTL cache. All `sync_line_memory` writes for this run originate from
        // per-line threads dispatched by `flush_batch`; by the time `join_all`
        // returns, every successful ticker has rewritten `line-memory.json`
        // (synchronously, before the worker thread send() back on `result_rx`).
        // This is the single deterministic moment when the file has stabilised
        // for the entire portfolio — so home reads issued in the next 5 min
        // (cache TTL) will now see fresh stats instead of the pre-run snapshot.
        //
        // Gated on `!completed_tickers.is_empty()`: if every batch failed,
        // line-memory.json was not mutated and the cached value is still
        // accurate — invalidating would force a pointless re-read.
        if !self.completed_tickers.is_empty() {
            crate::signal_accuracy::invalidate_signal_accuracy_cache();
        }

        Ok(self.completed_tickers.clone())
    }

    /// Test-only constructor that bypasses `Self::new` (which would touch
    /// codex MCP config when the codex backend is active) and lets a unit
    /// test seed `completed_tickers` directly. Used by the P2-63 wiring
    /// test in this file's `#[cfg(test)] mod tests` block.
    #[cfg(test)]
    fn for_test(seed_completed: Vec<String>) -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            run_id: "test-run".to_string(),
            data_dir: std::env::temp_dir()
                .to_string_lossy()
                .to_string(),
            batch_size: 1,
            pending: Vec::new(),
            result_tx,
            result_rx,
            active_batches: 0,
            completed_tickers: seed_completed,
            relay_stop_flags: Vec::new(),
        }
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

                // P1-59: compute the cross-portfolio conviction calibration
                // ONCE per batch. Reading line-memory.json is cheap and only
                // happens here, not per-line. The serialized envelope is
                // then injected into each line_data so the per-line prompt
                // builder can render the CALIBRATION CONVICTION block. A
                // read failure (fresh install, missing file) is treated as
                // "no calibration data yet" — `compute_signal_accuracy`
                // already swallows that as `SignalAccuracyStats::default()`.
                let calibration_envelope: Value = match crate::signal_accuracy::compute_signal_accuracy() {
                    Ok(stats) => crate::signal_accuracy::stats_to_json(&stats),
                    Err(e) => {
                        crate::debug_log(&format!("[mcp-native] calibration compute failed: {e}"));
                        Value::Null
                    }
                };

                // Pre-fetch all line data (fast, from cache/disk).
                // P1-59: inject the calibration envelope into each
                // line_data — the per-line prompt builder reads
                // `line_data["calibration_stats"]` to render the
                // CALIBRATION CONVICTION block.
                let line_data_vec: Vec<(String, String, String, Value)> = tickers.iter().map(|(ticker, nom, line_type)| {
                    let mut line_data = crate::mcp_server::dispatch_tool_direct(
                        &dd,
                        "get_line_data",
                        &serde_json::json!({"run_id": progress_run_id, "line_id": format!("{line_type}:{ticker}")}),
                    );
                    if !calibration_envelope.is_null() {
                        if let Some(obj) = line_data.as_object_mut() {
                            obj.insert("calibration_stats".to_string(), calibration_envelope.clone());
                        }
                    }
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
                                        // Tag with `mode: "native"` so the run_stats
                                        // aggregator sums per-call totals across
                                        // parallel lines (each native call emits one
                                        // event at response.completed — see
                                        // openai_client.rs:594-617). Codex events
                                        // are cumulative-per-thread and must keep
                                        // max-semantics, which run_stats does when
                                        // `mode != "native"`.
                                        serde_json::json!({
                                            "type": "token_usage",
                                            "mode": "native",
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
                                // Codex batch dispatch — emits cumulative
                                // per-thread updates. `mode: "codex"` keeps
                                // max-semantics in the aggregator.
                                serde_json::json!({
                                    "type": "token_usage",
                                    "mode": "codex",
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

// ── Bug B follow-up — line-memory zero-price repair (one-shot) ─────
//
// When Bug B (or any other long-running price-provider outage) leaves a
// ticker with `price_tracking.current_price == 0` AND every
// `signal_history[].price_at_signal == 0`, the prompt renderer happily emits
// `prix: 0.00€` and the LLM concludes "no usable quote", feeding the next
// run's `memory_narrative` with the same conclusion.
//
// `repair_line_memory_zero_prices` walks `line-memory.json` once and tags
// such tickers with `price_data_unavailable: true` so downstream rendering
// (see `build_memory_section` in `llm_prompts.rs`) can skip the "prix au
// signal" line instead of printing a zero. Signal history is preserved as-is
// — only the metadata flag is added.

/// Decide if a single ticker entry is contaminated by an all-zero price
/// history. Caller is responsible for not flagging an entry that is already
/// flagged (re-flagging is a no-op, but skipping is cheaper).
pub(crate) fn ticker_needs_zero_price_repair(entry: &Value) -> bool {
    // Skip entries the LLM hasn't built up history for yet — these are
    // legitimate first-run states (`price_at_signal == current_price`, no
    // contamination).
    let history = match entry.get("signal_history").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return false,
    };
    let current_price = entry
        .get("price_tracking")
        .and_then(|pt| pt.get("current_price"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if current_price != 0.0 {
        return false;
    }
    // ALL entries must have price_at_signal == 0 — a single non-zero entry
    // means the chain was healthy at some point, so we let the existing
    // history stand without flagging.
    history.iter().all(|item| {
        item.get("price_at_signal")
            .and_then(|v| v.as_f64())
            .map(|v| v == 0.0)
            .unwrap_or(true)
    })
}

/// Outcome of a repair pass — separates new flags from cleared flags so the
/// Tauri command result is informative (operator can see "0 flagged, 4
/// cleared" instead of just "0").
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ZeroPriceRepairOutcome {
    pub flagged: usize,
    pub cleared: usize,
}

impl ZeroPriceRepairOutcome {
    pub fn changed(&self) -> bool {
        self.flagged > 0 || self.cleared > 0
    }
}

/// One-shot repair. Returns `(flagged, cleared)`. Pure transform on the store
/// value so it can be exercised directly from tests.
///
/// Two branches now:
/// 1. Entry NOT flagged but `ticker_needs_zero_price_repair` matches → set
///    `price_data_unavailable: true`.
/// 2. Entry currently flagged but `ticker_needs_zero_price_repair` no longer
///    matches (prices recovered, history has at least one real
///    `price_at_signal > 0`, or `current_price > 0`) → clear the flag.
///
/// Without branch 2 an operator-triggered `repair_line_memory_local` could
/// never un-flag a ticker that had healed: the auto-clear in
/// `sync_line_memory` only fires when a fresh run happens.
pub fn apply_zero_price_repair(store: &mut Value) -> ZeroPriceRepairOutcome {
    let by_ticker = match store
        .get_mut("by_ticker")
        .and_then(|v| v.as_object_mut())
    {
        Some(m) => m,
        None => return ZeroPriceRepairOutcome::default(),
    };
    let mut outcome = ZeroPriceRepairOutcome::default();
    for (_, entry) in by_ticker.iter_mut() {
        let currently_flagged = entry
            .get("price_data_unavailable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let needs_flag = ticker_needs_zero_price_repair(entry);
        if currently_flagged {
            if needs_flag {
                // Still contaminated — leave the flag in place (idempotent).
                continue;
            }
            // Recovered — clear the flag so the next prompt build renders
            // prices again.
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("price_data_unavailable".to_string(), Value::Bool(false));
                outcome.cleared += 1;
            }
        } else if needs_flag {
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("price_data_unavailable".to_string(), Value::Bool(true));
                outcome.flagged += 1;
            }
        }
    }
    outcome
}

/// Run the repair against the persisted line-memory store. Always flushes to
/// disk if any tickers were touched, so the change survives a process exit.
pub fn repair_line_memory_zero_prices() -> Result<ZeroPriceRepairOutcome> {
    let mut outcome = ZeroPriceRepairOutcome::default();
    line_memory_patch(|store| {
        outcome = apply_zero_price_repair(store);
    });
    if outcome.changed() {
        line_memory_flush_now();
        crate::debug_log(&format!(
            "repair_line_memory_zero_prices: flagged {} tickers, cleared {} tickers",
            outcome.flagged, outcome.cleared
        ));
    } else {
        crate::debug_log("repair_line_memory_zero_prices: no contaminated tickers found");
    }
    Ok(outcome)
}

/// Startup gate — runs the repair exactly once per install, tracked via the
/// `line_memory_repaired_v1` runtime setting. Errors are swallowed: a repair
/// failure must NOT block the app from booting; the next launch will retry.
pub fn maybe_run_zero_price_repair_at_startup() {
    if crate::runtime_settings::integer_direct("line_memory_repaired_v1", 0) >= 1 {
        return;
    }
    match repair_line_memory_zero_prices() {
        Ok(_) => {
            let _ = crate::runtime_settings::patch(&json!({
                "line_memory_repaired_v1": 1
            }));
        }
        Err(e) => {
            crate::debug_log(&format!(
                "maybe_run_zero_price_repair_at_startup: failed, will retry on next launch: {e}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── P2-63 (2026-05-24) — Pin the wiring between `join_all` returning
    // with at least one completed ticker and the signal-accuracy TTL cache
    // being invalidated.
    //
    // The full path under test:
    //   1. Seed `compute_signal_accuracy()` cache with a known
    //      `SignalAccuracyStats` (1 signal).
    //   2. Write a different `line-memory.json` to disk (5 signals).
    //   3. Call `McpBatchDispatchQueue::join_all` — the unit under test.
    //      With `active_batches=0` and `completed_tickers=["X"]` seeded,
    //      the function short-circuits the recv loop and goes straight to
    //      the tail (final merge + relay-flag flip + invalidation hook).
    //   4. Assert the NEXT `compute_signal_accuracy()` call returns the
    //      NEW 5-signal stats (i.e. the cache was actually dropped).
    //
    // Pairs with `services/signal_accuracy.rs::cache_recomputes_after_invalidate`
    // which pins the same property at the cache layer; this test pins the
    // CALL SITE.

    fn unique_state_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "alfred-p2-63-wiring-{tag}-{}-{}",
            crate::now_epoch_ms(),
            std::process::id()
        ))
    }

    fn write_line_memory_with_n_correct(dir: &std::path::Path, n: usize) {
        std::fs::create_dir_all(dir).expect("create state dir");
        let mut by_ticker = serde_json::Map::new();
        // Shape mirrors `signal_accuracy::tests::entry`: the aggregator
        // counts a ticker as "scored" iff `price_tracking.signal_accuracy`
        // is exactly `"correct"` or `"incorrect"`.
        for i in 0..n {
            by_ticker.insert(
                format!("WIRE_T{i}"),
                json!({
                    "price_tracking": {
                        "last_signal": "ACHAT",
                        "signal_accuracy": "correct",
                        "return_since_signal_pct": 5.0,
                        "current_price": 100.0,
                        "price_at_signal": 100.0,
                    }
                }),
            );
        }
        let store = json!({ "by_ticker": by_ticker });
        std::fs::write(
            dir.join("line-memory.json"),
            serde_json::to_string(&store).unwrap(),
        )
        .expect("write line-memory.json");
    }

    #[test]
    fn join_all_invalidates_signal_accuracy_cache_when_tickers_completed() {
        let _guard = crate::helpers::test_env_lock();
        crate::signal_accuracy::invalidate_signal_accuracy_cache();

        let base = unique_state_dir("invalidate");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        // 1. Seed cache with 1 signal.
        write_line_memory_with_n_correct(&base, 1);
        let before = crate::signal_accuracy::compute_signal_accuracy()
            .expect("populate cache");
        assert_eq!(before.total_signals, 1, "preconditions: cache holds 1");

        // 2. Rewrite the file to 5 signals (no invalidate yet).
        write_line_memory_with_n_correct(&base, 5);
        let cached = crate::signal_accuracy::compute_signal_accuracy()
            .expect("read still-cached value");
        assert_eq!(
            cached.total_signals, 1,
            "preconditions: cache still serves the OLD value before join_all"
        );

        // 3. Run the unit under test — `join_all` with one completed
        //    ticker. `active_batches` is 0 so it falls straight to the
        //    tail block (where the invalidation lives).
        let mut q = McpBatchDispatchQueue::for_test(vec!["WIRE_T0".to_string()]);
        let completed = q.join_all().expect("join_all ok");
        assert_eq!(completed, vec!["WIRE_T0".to_string()]);

        // 4. Cache must now reflect the on-disk 5-signal state.
        let after = crate::signal_accuracy::compute_signal_accuracy()
            .expect("read after join_all");
        assert_eq!(
            after.total_signals, 5,
            "join_all must invalidate the signal-accuracy cache when \
             completed_tickers is non-empty"
        );

        // Cleanup
        crate::signal_accuracy::invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn join_all_preserves_cache_when_no_tickers_completed() {
        // Symmetric guard: a run where every batch failed (no completed
        // tickers) MUST NOT invalidate — the on-disk file wasn't touched
        // and invalidating would just force a pointless re-read.
        let _guard = crate::helpers::test_env_lock();
        crate::signal_accuracy::invalidate_signal_accuracy_cache();

        let base = unique_state_dir("preserve");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        write_line_memory_with_n_correct(&base, 2);
        let before = crate::signal_accuracy::compute_signal_accuracy()
            .expect("populate cache");
        assert_eq!(before.total_signals, 2);

        // Rewrite file to 9 signals — cache still serves OLD (2) because
        // no invalidate happened yet.
        write_line_memory_with_n_correct(&base, 9);

        // join_all with EMPTY completed_tickers — the invalidation branch
        // must NOT fire.
        let mut q = McpBatchDispatchQueue::for_test(Vec::new());
        let completed = q.join_all().expect("join_all ok");
        assert!(completed.is_empty(), "no completed tickers expected");

        // Cache should still hold the OLD value.
        let after = crate::signal_accuracy::compute_signal_accuracy()
            .expect("read after empty join_all");
        assert_eq!(
            after.total_signals, 2,
            "join_all with zero completed tickers must NOT invalidate the cache"
        );

        // Cleanup
        crate::signal_accuracy::invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }
}
