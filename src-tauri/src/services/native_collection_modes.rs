//! Run modes — refresh+synthesis and retry-failed logic.
//! Watchlist is now integrated into the main pipeline (native_collection.rs).

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::{
    native_collection_dispatch::NativeCollectionDispatchQueue,
    now_iso_string, patch_run_state_direct_with,
    set_native_run_stage,
};

use crate::native_collection_helpers::{
    as_text, build_collection_state, diagnose_run_quality, HttpRequestFn,
};

use crate::native_collection::read_line_memory_store;

/// Refresh + synthesis mode: re-collect market/news data for all positions,
/// reuse existing recommendations (except expired reanalyse_after), skip line analysis,
/// then delegate to finalization for global synthesis.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_refresh_synthesis_mode(
    run_id: &str,
    snapshot: &Value,
    positions: &[Value],
    portfolio_source: &str,
    source_ingestion_status: &str,
    source_details: &Value,
    payload: &Value,
    _llm_token: Option<&str>,
    request_fn: HttpRequestFn,
) -> Result<Value> {
    let total_lines = positions.len() as i64;
    set_native_run_stage(
        run_id,
        "collecting_data",
        Some(json!({ "completed": 0, "total": total_lines })),
        None,
    )?;

    let line_memory_store = Arc::new(read_line_memory_store()?);
    let news_quality_threshold = payload
        .get("news_quality_threshold")
        .and_then(|v| v.as_i64())
        .unwrap_or(60);
    let max_missing = payload
        .get("max_missing_market_fields")
        .and_then(|v| v.as_u64())
        .unwrap_or(2) as usize;
    let concurrency = std::cmp::max(
        1,
        crate::runtime_setting_integer_direct("collection_concurrency", 2),
    )
    .clamp(1, 12) as usize;
    let throttle_ms =
        std::cmp::max(0, crate::runtime_setting_integer_direct("collection_throttle_ms", 40))
            as u64;

    let mut market_by_ticker = Map::new();
    let mut news_by_ticker = Map::new();
    let mut technicals_by_ticker: Map<String, Value> = Map::new();
    let collection_issues = Vec::new();
    let failures = Vec::new();
    let hydration_totals = json!({
        "tickers_hydrated": 0,
        "banned_articles_filtered": 0,
        "seen_articles_filtered": 0,
        "global_banned_articles_filtered": 0,
        "total_articles_filtered": 0
    });

    // Collect market + news only (no line analysis dispatch)
    let mut dispatch = NativeCollectionDispatchQueue::new(
        concurrency,
        throttle_ms,
        Arc::clone(&line_memory_store),
        news_quality_threshold,
        max_missing,
        request_fn,
    );
    let mut completed = 0usize;
    for (index, row) in positions.iter().enumerate() {
        let ticker = as_text(row.get("ticker"));
        if !ticker.is_empty() {
            let _ = crate::update_line_status(run_id, &ticker, "collecting");
        }
        dispatch.push(index, row.clone())?;
        for result in dispatch.drain_ready() {
            completed += 1;
            market_by_ticker.insert(result.ticker.clone(), result.market_row.clone());
            news_by_ticker.insert(result.ticker.clone(), result.news_row.clone());
            if let Some(snap) = result.technical_snapshot.clone() {
                technicals_by_ticker.insert(result.ticker.clone(), snap);
            }
            let _ = crate::update_line_status(run_id, &result.ticker, "done");
            set_native_run_stage(
                run_id,
                "collecting_data",
                Some(json!({ "completed": completed, "total": total_lines })),
                None,
            )?;
        }
    }
    while completed < positions.len() {
        let result = dispatch.recv_blocking()?;
        completed += 1;
        market_by_ticker.insert(result.ticker.clone(), result.market_row.clone());
        news_by_ticker.insert(result.ticker.clone(), result.news_row.clone());
        if let Some(snap) = result.technical_snapshot.clone() {
            technicals_by_ticker.insert(result.ticker.clone(), snap);
        }
        let _ = crate::update_line_status(run_id, &result.ticker, "done");
    }
    crate::run_state_cache::flush_to_disk();

    // Load previous recommendations from the last completed run (not the current empty one)
    let today = now_iso_string().chars().take(10).collect::<String>();
    let (prev_recs_vec, _prev_line_status) = crate::run_state::load_previous_run_data(run_id);
    let prev_recs = prev_recs_vec;
    let mut kept_recs = Vec::new();
    let mut expired_tickers = Vec::new();
    let mut force_reasons: Vec<(String, &'static str)> = Vec::new();
    for rec in &prev_recs {
        let ticker = as_text(rec.get("ticker"));
        // `market_by_ticker` / `news_by_ticker` are per-run maps keyed by raw
        // position ticker — unrelated to the line-memory canonical-key contract
        // enforced by `resolve_line_memory_key`. Sanctioned sites tagged below.
        let market_row = market_by_ticker.get(&ticker); // LINT-ALLOW: per-run raw-ticker map
        let news_row = news_by_ticker.get(&ticker); // LINT-ALLOW: per-run raw-ticker map
        let line_memory =
            crate::native_mcp_analysis::resolve_line_memory_key(&*line_memory_store, &ticker);
        let reason = crate::force_reanalyse::should_force_reanalyse(
            rec,
            market_row,
            line_memory,
            news_row,
            today.as_str(),
        );
        if reason.is_force() {
            force_reasons.push((ticker.clone(), reason.as_tag()));
            expired_tickers.push(ticker);
        } else {
            kept_recs.push(rec.clone());
        }
    }
    // Also re-analyze positions that have NO recommendation at all (previous run failed for them)
    let rec_tickers: std::collections::HashSet<String> = prev_recs
        .iter()
        .map(|r| as_text(r.get("ticker")).to_uppercase())
        .collect();
    let pos_tickers: Vec<String> = positions
        .iter()
        .map(|p| as_text(p.get("ticker")).to_uppercase())
        .filter(|t| !t.is_empty())
        .collect();
    for t in &pos_tickers {
        if !rec_tickers.contains(t) && !expired_tickers.iter().any(|e| e.to_uppercase() == *t) {
            expired_tickers.push(t.clone());
        }
    }

    if !expired_tickers.is_empty() {
        eprintln!(
            "[refresh_synthesis] {} tickers to re-analyze (stale or missing recommendation): {:?}",
            expired_tickers.len(),
            expired_tickers
        );
        if !force_reasons.is_empty() {
            eprintln!(
                "[refresh_synthesis] force_reanalyse reasons: {:?}",
                force_reasons
            );
        }
    }
    let _ = patch_run_state_direct_with(run_id, |rs| {
        if let Some(obj) = rs.as_object_mut() {
            obj.insert(
                "pending_recommandations".to_string(),
                json!(kept_recs),
            );
            obj.insert("market".to_string(), Value::Object(market_by_ticker.clone()));
            obj.insert("news".to_string(), Value::Object(news_by_ticker.clone()));
        }
    });

    // Re-analyze stale (expired reanalyse_after) positions
    if !expired_tickers.is_empty() {
        let expired_set: std::collections::HashSet<String> =
            expired_tickers.iter().map(|t| t.to_uppercase()).collect();
        let stale_positions: Vec<Value> = positions
            .iter()
            .filter(|p| expired_set.contains(&as_text(p.get("ticker")).to_uppercase()))
            .cloned()
            .collect();
        if !stale_positions.is_empty() {
            set_native_run_stage(run_id, "analyzing_stale", None, None)?;
            let data_dir = crate::resolve_runtime_state_dir()
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());
            let batch_size = crate::runtime_setting_integer_direct("mcp_batch_size", 5).max(1) as usize;
            let mut mcp_dispatch = crate::native_mcp_analysis::McpBatchDispatchQueue::new(
                run_id, &data_dir, batch_size,
            );
            for row in &stale_positions {
                let ticker = as_text(row.get("ticker"));
                if ticker.is_empty() { continue; }
                let name = as_text(row.get("nom"));
                let line_type = as_text(row.get("type"));
                mcp_dispatch.push(crate::native_mcp_analysis::McpLinePacket {
                    ticker,
                    nom: name,
                    line_type: if line_type.is_empty() { "position".to_string() } else { line_type },
                })?;
            }
            mcp_dispatch.flush_pending()?;
            mcp_dispatch.join_all()?;
            crate::run_state_cache::flush_to_disk();
        }
    }

    let incremental_positions: Vec<Value> = positions.to_vec();
    let quality = diagnose_run_quality(
        &market_by_ticker,
        &news_by_ticker,
        &incremental_positions,
        news_quality_threshold,
        max_missing,
    );
    // refresh_synthesis mode runs after an initial full run that already
    // wrote cross_account_context. We preserve the existing value if present
    // on disk instead of recomputing it here.
    let existing_ctx = crate::load_run_by_id_direct(run_id)
        .ok()
        .and_then(|rs| rs.get("cross_account_context").cloned());
    let final_collection_state = build_collection_state(
        snapshot,
        &incremental_positions,
        &market_by_ticker,
        &news_by_ticker,
        &technicals_by_ticker,
        &quality,
        &collection_issues,
        &failures,
        portfolio_source,
        source_ingestion_status,
        source_details,
        &hydration_totals,
        existing_ctx.as_ref(),
    );
    crate::native_line_analysis::persist_native_collection_state(run_id, &final_collection_state)?;
    crate::run_state_cache::clear_run(run_id);

    Ok(json!({
        "ok": true,
        "action": "analysis:run-start-local",
        "result": {
            "ok": true,
            "run_id": run_id,
            "run_mode": "refresh_synthesis",
            "collection": {
                "ok": true,
                "run_id": run_id,
                "positions_count": positions.len(),
                "source_mode": portfolio_source,
                "ingestion_status": source_ingestion_status,
                "collection_state": final_collection_state
            },
            "generated_draft": null,
            "report": null,
            "line_memory_sync": null,
            "degraded": false,
            "degradation_reason": null,
            "finalization_delegated": true
        }
    }))
}
