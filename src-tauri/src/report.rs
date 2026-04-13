use std::fs;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::helpers::{
    now_iso_string, run_state_update_lock, safe_text, safe_upper,
};
use crate::paths::{
    resolve_report_history_dir, resolve_reports_dir,
    resolve_runtime_state_dir,
};
use crate::run_state::load_run_by_id;
use crate::storage::write_json_file;

// ── Line-id helpers ──

fn as_line_id(value: &serde_json::Value) -> String {
    let direct = safe_text(value.get("line_id"));
    if !direct.is_empty() {
        return direct;
    }
    let ticker = safe_upper(value.get("ticker"));
    if ticker.is_empty() {
        return String::new();
    }
    let line_type = {
        let raw = crate::helpers::safe_lower(value.get("type"));
        if raw.is_empty() {
            "position".to_string()
        } else {
            raw
        }
    };
    format!("{line_type}:{ticker}")
}

fn line_ticker(line_id: &str) -> String {
    let safe = line_id.trim();
    if safe.is_empty() {
        return String::new();
    }
    safe.split(':').last().unwrap_or_default().trim().to_ascii_uppercase()
}

fn push_ticker_issue(
    by_ticker: &mut serde_json::Map<String, serde_json::Value>,
    ticker: &str,
    issue: &str,
) {
    let safe_ticker = ticker.trim().to_ascii_uppercase();
    if safe_ticker.is_empty() || issue.trim().is_empty() {
        return;
    }
    let entry = by_ticker
        .entry(safe_ticker)
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let Some(items) = entry.as_array_mut() {
        items.push(serde_json::Value::String(issue.trim().to_string()));
    }
}

pub fn derive_expected_line_ids(run_state: &serde_json::Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (line_type, rows) in [
        ("position", run_state.get("portfolio").and_then(|v| v.get("positions")).and_then(|v| v.as_array())),
        ("watchlist", run_state.get("watchlist").and_then(|v| v.get("items")).and_then(|v| v.as_array())),
    ] {
        for row in rows.unwrap_or(&Vec::new()) {
            let ticker = safe_upper(row.get("ticker"));
            if ticker.is_empty() {
                continue;
            }
            let line_id = format!("{line_type}:{ticker}");
            if seen.insert(line_id.clone()) {
                ids.push(line_id);
            }
        }
    }
    ids.sort();
    ids
}

// ── Action enrichment from line recommendations ──

const ACTIONABLE_SIGNALS: &[&str] = &[
    "ACHAT_FORT", "ACHAT", "RENFORCEMENT", "ALLEGEMENT", "VENTE",
];

fn conviction_priority(conviction: &str) -> u8 {
    match conviction.to_lowercase().as_str() {
        "forte" => 1,
        "moderee" | "modérée" => 2,
        "faible" => 3,
        _ => 4,
    }
}

fn signal_priority(signal: &str) -> u8 {
    let upper = signal.to_uppercase();
    if upper.contains("VENTE") { return 1; }
    if upper.contains("ALLEG") { return 2; }
    if upper.contains("ACHAT_FORT") { return 3; }
    if upper.contains("ACHAT") { return 4; }
    if upper.contains("RENFORC") { return 5; }
    10
}

/// Enrich LLM-generated actions_immediates with any actionable line recommendations
/// that the LLM missed. Sorts by conviction (forte first) then signal priority.
fn enrich_actions_from_recommendations(
    llm_actions: &[serde_json::Value],
    recommendations: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut actions = llm_actions.to_vec();
    let existing_tickers: std::collections::HashSet<String> = actions
        .iter()
        .filter_map(|a| a.get("ticker").and_then(|v| v.as_str()))
        .map(|t| t.to_uppercase())
        .collect();

    for rec in recommendations {
        let signal = rec.get("signal").and_then(|v| v.as_str()).unwrap_or_default().to_uppercase();
        let ticker = rec.get("ticker").and_then(|v| v.as_str()).unwrap_or_default().to_uppercase();
        let rec_type = rec.get("type").and_then(|v| v.as_str()).unwrap_or("position");
        if ticker.is_empty() || rec_type == "watchlist" { continue; }
        let is_actionable = ACTIONABLE_SIGNALS.iter().any(|s| signal.contains(s));
        if !is_actionable || existing_tickers.contains(&ticker) { continue; }

        let conviction = rec.get("conviction").and_then(|v| v.as_str()).unwrap_or_default();
        let action_rec = rec.get("action_recommandee").and_then(|v| v.as_str()).unwrap_or_default();

        actions.push(json!({
            "ticker": ticker,
            "action": signal,
            "order_type": "MARKET",
            "limit_price": serde_json::Value::Null,
            "quantity": serde_json::Value::Null,
            "estimated_amount_eur": serde_json::Value::Null,
            "priority": actions.len() + 1,
            "rationale": if action_rec.is_empty() {
                format!("Signal {} (conviction {})", signal, conviction)
            } else {
                action_rec.to_string()
            },
            "_source": "auto_from_recommendation",
            "_conviction": conviction
        }));
    }

    // Sort: conviction forte first, then by signal priority
    actions.sort_by(|a, b| {
        let conv_a = conviction_priority(a.get("_conviction").and_then(|v| v.as_str()).unwrap_or(
            a.get("conviction").and_then(|v| v.as_str()).unwrap_or("faible")
        ));
        let conv_b = conviction_priority(b.get("_conviction").and_then(|v| v.as_str()).unwrap_or(
            b.get("conviction").and_then(|v| v.as_str()).unwrap_or("faible")
        ));
        let sig_a = signal_priority(a.get("action").and_then(|v| v.as_str()).unwrap_or(""));
        let sig_b = signal_priority(b.get("action").and_then(|v| v.as_str()).unwrap_or(""));
        conv_a.cmp(&conv_b).then(sig_a.cmp(&sig_b))
    });

    // Re-assign priorities after sorting
    for (i, action) in actions.iter_mut().enumerate() {
        if let Some(obj) = action.as_object_mut() {
            obj.insert("priority".to_string(), json!(i + 1));
        }
    }

    if actions.len() > llm_actions.len() {
        eprintln!(
            "[report] enriched actions_immediates: {} from LLM + {} auto-injected from recommendations",
            llm_actions.len(),
            actions.len() - llm_actions.len()
        );
    }

    actions
}

// ── Validation ──

fn validate_synthesis_quality(synthese: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if synthese.trim().chars().count() < 100 {
        errors.push("synthese_marche_too_short".to_string());
    }
    errors
}

fn validate_immediate_actions(actions: &[serde_json::Value]) -> (Vec<String>, serde_json::Map<String, serde_json::Value>) {
    let mut errors = Vec::new();
    let mut by_ticker = serde_json::Map::new();
    if actions.len() > 5 {
        errors.push("actions_immediates_too_many".to_string());
    }
    let mut seen_priorities = std::collections::HashSet::new();
    for (index, action) in actions.iter().enumerate() {
        let prefix = format!("actions_immediates_invalid:{index}");
        let ticker = safe_upper(action.get("ticker"));
        let label = safe_upper(action.get("action"));
        let order_type = safe_upper(action.get("order_type").or_else(|| action.get("orderType")));
        let rationale = safe_text(action.get("rationale"));
        let priority = action.get("priority").and_then(|v| v.as_i64());
        let quantity = action.get("quantity").and_then(|v| v.as_f64());
        let price_limit = action
            .get("price_limit")
            .or_else(|| action.get("priceLimit"))
            .and_then(|v| v.as_f64());
        let estimated_amount = action
            .get("estimated_amount_eur")
            .or_else(|| action.get("estimatedAmountEur"))
            .and_then(|v| v.as_f64());
        let mut action_errors = Vec::new();
        if ticker.is_empty() {
            action_errors.push(format!("{prefix}:ticker_required"));
        }
        if label.is_empty() {
            action_errors.push(format!("{prefix}:action_required"));
        }
        if !matches!(order_type.as_str(), "MARKET" | "LIMIT") {
            action_errors.push(format!("{prefix}:order_type_invalid"));
        }
        if rationale.is_empty() {
            action_errors.push(format!("{prefix}:rationale_required"));
        }
        if !matches!(priority, Some(value) if (1..=5).contains(&value)) {
            action_errors.push(format!("{prefix}:priority_invalid"));
        } else if !seen_priorities.insert(priority.unwrap()) {
            action_errors.push(format!("actions_immediates_duplicate_priority:{}", priority.unwrap()));
        }
        let is_transactional = matches!(label.as_str(), "ACHETER" | "RENFORCER" | "ALLEGER" | "VENDRE");
        if is_transactional && !matches!(quantity, Some(value) if value > 0.0) {
            action_errors.push(format!("{prefix}:quantity_invalid"));
        }
        if is_transactional && !matches!(estimated_amount, Some(value) if value > 0.0) {
            action_errors.push(format!("{prefix}:estimated_amount_invalid"));
        }
        if !is_transactional && matches!(estimated_amount, Some(value) if value < 0.0) {
            action_errors.push(format!("{prefix}:estimated_amount_invalid"));
        }
        if order_type == "LIMIT" && !matches!(price_limit, Some(value) if value > 0.0) {
            action_errors.push(format!("{prefix}:price_limit_required_for_limit"));
        }
        if order_type == "MARKET" && matches!(price_limit, Some(value) if value > 0.0) {
            action_errors.push(format!("{prefix}:price_limit_forbidden_for_market"));
        }
        for issue in action_errors {
            if !ticker.is_empty() {
                push_ticker_issue(&mut by_ticker, &ticker, &issue);
            }
            errors.push(issue);
        }
    }
    (errors, by_ticker)
}

fn validate_recommendation_coverage(
    recommendations: &[serde_json::Value],
    expected_line_ids: &[String],
) -> (Vec<String>, serde_json::Map<String, serde_json::Value>) {
    let mut errors = Vec::new();
    let mut by_ticker = serde_json::Map::new();
    let mut by_line = std::collections::HashMap::new();
    for rec in recommendations {
        let line_id = as_line_id(rec);
        let ticker = {
            let direct = safe_upper(rec.get("ticker"));
            if direct.is_empty() {
                line_ticker(&line_id)
            } else {
                direct
            }
        };
        if line_id.is_empty() {
            let issue = "recommendation_line_id_missing".to_string();
            push_ticker_issue(&mut by_ticker, &ticker, &issue);
            errors.push(issue);
            continue;
        }
        if by_line.insert(line_id.clone(), true).is_some() {
            let issue = format!("duplicate_line_result:{line_id}");
            push_ticker_issue(&mut by_ticker, &ticker, &issue);
            errors.push(issue);
            continue;
        }
        let synthese = safe_text(rec.get("synthese"));
        if synthese.chars().count() < 80 {
            let issue = format!("line_synthese_too_short:{line_id}");
            push_ticker_issue(&mut by_ticker, &ticker, &issue);
            errors.push(issue);
        }
        if rec
            .get("line_validation")
            .and_then(|v| v.get("ok"))
            .and_then(|v| v.as_bool())
            == Some(false)
        {
            let issue = format!("line_incomplete:{line_id}");
            push_ticker_issue(&mut by_ticker, &ticker, &issue);
            errors.push(issue);
        }
    }
    let expected: std::collections::HashSet<String> = expected_line_ids.iter().cloned().collect();
    for line_id in expected.iter() {
        if !by_line.contains_key(line_id) {
            let issue = format!("missing_line_recommendation:{line_id}");
            push_ticker_issue(&mut by_ticker, &line_ticker(line_id), &issue);
            errors.push(issue);
        }
    }
    for line_id in by_line.keys() {
        if !expected.contains(line_id) {
            let issue = format!("unexpected_line_recommendation:{line_id}");
            push_ticker_issue(&mut by_ticker, &line_ticker(line_id), &issue);
            errors.push(issue);
        }
    }
    (errors, by_ticker)
}

fn merge_ticker_issue_maps(
    left: serde_json::Map<String, serde_json::Value>,
    right: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = left;
    for (ticker, issues) in right {
        let entry = merged
            .entry(ticker)
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if let (Some(target), Some(source)) = (entry.as_array_mut(), issues.as_array()) {
            target.extend(source.iter().cloned());
        }
    }
    merged
}

// ── Draft generation via LLM ──

pub fn generate_draft_via_litellm(run_state: &serde_json::Value, run_id: &str) -> Result<serde_json::Value> {
    let result = crate::llm::generate_report_draft(run_state, run_id)?;
    let draft = result.get("draft").cloned().unwrap_or_else(|| result.clone());
    Ok(json!({
        "ok": true,
        "draft": draft,
        "llm_utilise": result.get("llm_utilise").cloned().unwrap_or_else(|| json!("native"))
    }))
}

// ── Report persistence (shared between retry and finalize) ──

fn save_report_artifact(
    run_id: &str,
    account: &str,
    payload: &serde_json::Value,
) -> Result<(String, String, String, String)> {
    let saved_at = now_iso_string();
    let timestamp_file_part = saved_at
        .replace('-', "")
        .replace(':', "")
        .split('.')
        .next()
        .unwrap_or_default()
        .replace('T', "_");
    let reports_dir = resolve_reports_dir();
    let history_dir = resolve_report_history_dir();
    fs::create_dir_all(&reports_dir)?;
    fs::create_dir_all(&history_dir)?;
    let latest_path = reports_dir.join("latest.json");
    let history_filename = format!("{timestamp_file_part}_{run_id}.json");
    let history_path = history_dir.join(&history_filename);
    let artifact = json!({
        "run_id": run_id,
        "account": account,
        "saved_at": saved_at,
        "payload": payload
    });
    write_json_file(&latest_path, &artifact)?;
    write_json_file(&history_path, &artifact)?;
    Ok((
        saved_at,
        history_filename,
        latest_path.to_string_lossy().to_string(),
        history_path.to_string_lossy().to_string(),
    ))
}

pub fn persist_retry_global_synthesis(run_id: &str, generated_draft: &serde_json::Value) -> Result<serde_json::Value> {
    let run_path = resolve_runtime_state_dir().join(format!("{run_id}.json"));
    let mut run_state = load_run_by_id(run_id)?;
    let pending_recommendations = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if pending_recommendations.is_empty() {
        return Err(anyhow!("global_synthesis_retry_not_available:{run_id}"));
    }
    // generated_draft may wrap fields inside "draft" (from generate_draft_via_litellm)
    let inner = generated_draft.get("draft").unwrap_or(generated_draft);
    let synthese = safe_text(inner.get("synthese_marche"));
    let llm_actions = inner
        .get("actions_immediates")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let actions = enrich_actions_from_recommendations(&llm_actions, &pending_recommendations);
    let expected_line_ids = derive_expected_line_ids(&run_state);
    let synthesis_errors = validate_synthesis_quality(&synthese);
    let (action_errors, action_by_ticker) = validate_immediate_actions(&actions);
    let recommendation_values = pending_recommendations.clone();
    let (recommendation_errors, recommendation_by_ticker) =
        validate_recommendation_coverage(&recommendation_values, &expected_line_ids);
    let mut global_errors = Vec::new();
    global_errors.extend(synthesis_errors);
    global_errors.extend(action_errors);
    global_errors.extend(recommendation_errors);
    let corrections = json!({
        "global": global_errors,
        "by_ticker": serde_json::Value::Object(merge_ticker_issue_maps(action_by_ticker, recommendation_by_ticker))
    });
    // Log validation issues but don't reject the draft — a slightly imperfect
    // synthesis is much better than the generic degraded fallback text.
    let has_issues = !corrections
        .get("global")
        .and_then(|v| v.as_array())
        .map(|rows| rows.is_empty())
        .unwrap_or(true);
    if has_issues {
        let issue_summary = corrections["global"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        crate::debug_log(&format!("report: synthesis validation warnings (kept): {issue_summary}"));
    }
    // Phase 2b: theme concentration data
    let theme_concentration = crate::native_mcp_analysis::compute_theme_concentration(run_id);

    let payload = json!({
        "date": now_iso_string(),
        "valeur_portefeuille": run_state.get("portfolio").and_then(|v| v.get("valeur_totale")).and_then(|v| v.as_f64()).unwrap_or(0.0),
        "plus_value_totale": run_state.get("portfolio").and_then(|v| v.get("plus_value_totale")).and_then(|v| v.as_f64()).unwrap_or(0.0),
        "liquidites": run_state.get("portfolio").and_then(|v| v.get("liquidites")).and_then(|v| v.as_f64()).unwrap_or(0.0),
        "synthese_marche": synthese,
        "actions_immediates": actions,
        "llm_utilise": generated_draft.get("llm_utilise").cloned().unwrap_or_else(|| json!("litellm")),
        "recommandations": pending_recommendations,
        "theme_concentration": theme_concentration
    });
    let account = run_state.get("account").and_then(|v| v.as_str()).unwrap_or("");
    let (saved_at, history_filename, latest_path_str, history_path_str) =
        save_report_artifact(run_id, account, &payload)?;
    if let Some(object) = run_state.as_object_mut() {
        object.insert("validation_corrections".to_string(), corrections.clone());
        object.insert("composed_payload".to_string(), payload.clone());
        object.insert("composed_payload_updated_at".to_string(), serde_json::Value::String(now_iso_string()));
        object.insert("report_artifacts".to_string(), json!({
            "run_id": run_id,
            "saved_at": saved_at,
            "history_filename": history_filename,
            "latest_path": latest_path_str,
            "history_path": history_path_str
        }));
    }
    let mut orch = crate::models::RunOrchestration::from_run_state(&run_state);
    orch.set_completed();
    orch.apply_to(&mut run_state);
    {
        let _lock = run_state_update_lock();
        write_json_file(&run_path, &run_state)?;
    }
    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "generated_draft": generated_draft,
        "report": {
            "ok": true,
            "run_id": run_id,
            "num_recommandations": recommendation_values.len(),
            "composed_payload_updated_at": run_state.get("composed_payload_updated_at").cloned().unwrap_or(serde_json::Value::Null),
            "latest_report_path": latest_path_str,
            "history_report_path": history_path_str,
            "errors": corrections.get("global").cloned().unwrap_or_else(|| json!([])),
            "corrections": corrections
        }
    }))
}

