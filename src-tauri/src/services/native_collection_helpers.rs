use std::{collections::HashSet, env};

use anyhow::Result;
use regex::Regex;
use serde_json::{json, Map, Value};

pub(crate) type HttpRequestFn =
    fn(&str, &str, u16, &str, Option<&str>, Option<u64>) -> Result<Value>;

pub(crate) fn as_text(value: Option<&Value>) -> String {
    value.and_then(|v| v.as_str()).unwrap_or_default().trim().to_string()
}

pub(crate) fn as_array<'a>(value: Option<&'a Value>) -> &'a [Value] {
    value.and_then(|v| v.as_array()).map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn normalize_ticker(raw: Option<&Value>) -> String {
    as_text(raw).to_uppercase()
}

pub(crate) fn to_number(raw: Option<&Value>) -> f64 {
    raw.and_then(|v| v.as_f64())
        .or_else(|| raw.and_then(|v| v.as_i64()).map(|v| v as f64))
        .or_else(|| raw.and_then(|v| v.as_u64()).map(|v| v as f64))
        .unwrap_or(0.0)
}

pub(crate) fn percent_encode_component(raw: &str) -> String {
    let mut out = String::new();
    for byte in raw.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

pub(crate) fn resolve_enrichment_base_url() -> String {
    env::var("ALFRED_ENRICHMENT_API_URL")
        .or_else(|_| env::var("ALFRED_ENRICHMENT_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:4402".to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}


pub(crate) fn request_json_from_url(
    method: &str,
    url: &str,
    body: Option<&str>,
    timeout_ms: Option<u64>,
    request_fn: HttpRequestFn,
) -> Result<Value> {
    let (host, port, path) = crate::local_http::parse_http_url(url)?;
    request_fn(method, &host, port, &path, body, timeout_ms)
}

pub(crate) fn resolve_source_current_price(row: &Value) -> Option<f64> {
    let direct = row.get("prix_actuel").and_then(|v| v.as_f64());
    if matches!(direct, Some(value) if value > 0.0) {
        return direct;
    }
    let qty = row.get("quantite").and_then(|v| v.as_f64());
    let current_value = row.get("valeur_actuelle").and_then(|v| v.as_f64());
    if let (Some(quantity), Some(value)) = (qty, current_value) {
        if quantity > 0.0 && value > 0.0 {
            return Some(value / quantity);
        }
    }
    row.get("prix_revient").and_then(|v| v.as_f64()).filter(|v| *v > 0.0)
}

fn score_news(articles: &[Value]) -> i64 {
    std::cmp::min(100, (articles.len() as i64) * 40)
}

pub(crate) fn assess_ticker_quality(
    ticker: &str,
    name: &str,
    market_row: &Value,
    news_row: &Value,
    news_quality_threshold: i64,
    max_missing_market_fields: usize,
) -> Value {
    let mut missing = Vec::new();
    for key in ["pe_ratio", "revenue_growth", "profit_margin", "debt_to_equity"] {
        if market_row.get(key).map(|v| v.is_null()).unwrap_or(true) {
            missing.push(Value::String(key.to_string()));
        }
    }
    let articles_len = as_array(news_row.get("articles"));
    let news_quality_score = score_news(articles_len);
    let enrich_market = missing.len() > max_missing_market_fields;
    let enrich_news = news_quality_score < news_quality_threshold;
    let mut reasons = Vec::new();
    if enrich_market {
        reasons.push(Value::String("market_fundamentals_incomplete".to_string()));
    }
    if enrich_news {
        reasons.push(Value::String("news_quality_low".to_string()));
    }
    json!({
        "ticker": ticker,
        "nom": name,
        "missing_market_fundamentals": missing,
        "news_quality_score": news_quality_score,
        "needs_enrichment": enrich_market || enrich_news,
        "enrich_market": enrich_market,
        "enrich_news": enrich_news,
        "reasons": reasons
    })
}

pub(crate) fn diagnose_run_quality(
    market: &Map<String, Value>,
    news: &Map<String, Value>,
    positions: &[Value],
    news_quality_threshold: i64,
    max_missing_market_fields: usize,
) -> Value {
    let mut by_ticker = Map::new();
    let mut weak_tickers = Vec::new();
    for row in positions {
        let ticker = normalize_ticker(row.get("ticker"));
        if ticker.is_empty() {
            continue;
        }
        let quality = assess_ticker_quality(
            &ticker,
            &as_text(row.get("nom")),
            market.get(&ticker).unwrap_or(&Value::Null),
            news.get(&ticker).unwrap_or(&Value::Null),
            news_quality_threshold,
            max_missing_market_fields,
        );
        if quality
            .get("needs_enrichment")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            weak_tickers.push(Value::String(ticker.clone()));
        }
        by_ticker.insert(ticker, quality);
    }
    json!({
        "ok": true,
        "weak_tickers": weak_tickers,
        "by_ticker": by_ticker
    })
}

pub(crate) fn normalize_finary_snapshot(snapshot: &Value) -> Value {
    let positions = as_array(snapshot.get("positions"))
        .iter()
        .map(|row| {
            json!({
                "ticker": normalize_ticker(row.get("symbol").or_else(|| row.get("ticker"))),
                "nom": row.get("name").cloned().or_else(|| row.get("nom").cloned()).unwrap_or(Value::Null),
                "isin": row.get("isin").cloned().unwrap_or(Value::Null),
                "quantite": to_number(row.get("quantity").or_else(|| row.get("quantite"))),
                "prix_actuel": to_number(row.get("price").or_else(|| row.get("prix_actuel"))),
                "valeur_actuelle": to_number(row.get("market_value").or_else(|| row.get("valeur_actuelle"))),
                "prix_revient": to_number(row.get("cost_basis").or_else(|| row.get("prix_revient"))),
                "plus_moins_value": to_number(row.get("gain_loss").or_else(|| row.get("plus_moins_value"))),
                "plus_moins_value_pct": to_number(row.get("gain_loss_pct").or_else(|| row.get("plus_moins_value_pct"))),
                "compte": row.get("account").cloned().or_else(|| row.get("compte").cloned()).unwrap_or_else(|| json!("FINARY"))
            })
        })
        .collect::<Vec<_>>();
    let mut result = json!({
        "portfolio_source": "finary",
        "positions": positions,
        "accounts": snapshot.get("accounts").cloned().unwrap_or_else(|| json!([])),
        "transactions": snapshot.get("transactions").cloned().unwrap_or_else(|| json!([])),
        "orders": snapshot.get("orders").cloned().unwrap_or_else(|| json!([])),
        "valeur_totale": to_number(snapshot.get("total_value").or_else(|| snapshot.get("valeur_totale"))),
        "plus_value_totale": to_number(snapshot.get("total_gain").or_else(|| snapshot.get("plus_value_totale"))),
        "liquidites": to_number(snapshot.get("cash").or_else(|| snapshot.get("liquidites")))
    });
    // Preserve ambiguous cash groups through normalization so the wizard can trigger
    if let Some(groups) = snapshot.get("ambiguous_cash_groups") {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("ambiguous_cash_groups".to_string(), groups.clone());
        }
    }
    result
}

pub(crate) fn normalize_csv_snapshot(snapshot: &Value) -> Value {
    let mut result = json!({
        "portfolio_source": "csv",
        "positions": snapshot.get("positions").cloned().unwrap_or_else(|| json!([])),
        "transactions": snapshot.get("transactions").cloned().unwrap_or_else(|| json!([])),
        "orders": snapshot.get("orders").cloned().unwrap_or_else(|| json!([])),
        "valeur_totale": to_number(snapshot.get("valeur_totale")),
        "plus_value_totale": to_number(snapshot.get("plus_value_totale")),
        "liquidites": to_number(snapshot.get("liquidites"))
    });
    // Preserve transaction history reconciliation metadata through normalization
    if let Some(csv_source) = snapshot.get("csv_source") {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("csv_source".to_string(), csv_source.clone());
        }
    }
    if let Some(reconciliation) = snapshot.get("reconciliation") {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("reconciliation".to_string(), reconciliation.clone());
        }
    }
    result
}

pub(crate) fn parse_fr_number(raw: &str) -> f64 {
    let sanitized = raw
        .replace('\u{202f}', "")
        .replace('\u{00a0}', "")
        .replace('€', "")
        .replace('%', "")
        .trim()
        .replace(' ', "")
        .replace(',', ".");
    // Strip leading currency codes (e.g. "USD235.56" from Revolut "USD 235.56")
    let stripped = if sanitized.len() > 3
        && sanitized.as_bytes()[..3].iter().all(|b| b.is_ascii_uppercase())
        && sanitized.as_bytes().get(3).map_or(false, |b| *b == b'-' || *b == b'.' || b.is_ascii_digit())
    {
        &sanitized[3..]
    } else {
        &sanitized
    };
    stripped.parse::<f64>().unwrap_or(0.0)
}

/// Extract a value using a regex capture group 1. If no regex or no match, returns raw as-is.
pub(crate) fn extract_with_pattern(raw: &str, regex: Option<&Regex>) -> String {
    let Some(re) = regex else { return raw.to_string() };
    re.captures(raw)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// Parse a numeric string according to the specified number format.
/// - "french": comma is decimal separator, space/dot are thousands separators
/// - "english": dot is decimal separator, comma is thousands separator
pub(crate) fn parse_number_with_format(raw: &str, number_format: &str) -> f64 {
    let trimmed = raw
        .replace('\u{202f}', "")
        .replace('\u{00a0}', "")
        .trim()
        .to_string();
    if trimmed.is_empty() {
        return 0.0;
    }
    match number_format {
        "english" => {
            // Strip thousands commas, parse with dot as decimal
            let cleaned = trimmed.replace(',', "").replace(' ', "");
            cleaned.parse::<f64>().unwrap_or(0.0)
        }
        _ => {
            // French: space/dot are thousands, comma is decimal
            parse_fr_number(&trimmed)
        }
    }
}

pub(crate) fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .split('#')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string()
}

pub(crate) fn as_string_list(value: Option<&Value>, max_items: usize, max_len: usize) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for row in as_array(value) {
        let text = as_text(Some(row));
        if text.is_empty() {
            continue;
        }
        let compact = text.chars().take(max_len).collect::<String>();
        let key = compact.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        out.push(Value::String(compact));
        if out.len() >= max_items {
            break;
        }
    }
    out
}

pub(crate) fn normalize_deep_news_score(value: Option<&Value>) -> Value {
    let parsed = value.and_then(|v| v.as_f64());
    match parsed {
        Some(raw) if raw.is_finite() => json!(raw.round().clamp(0.0, 100.0) as i64),
        _ => Value::Null,
    }
}

pub(crate) fn normalize_deep_news_enum(value: Option<&Value>, accepted: &[&str]) -> String {
    let normalized = as_text(value).to_lowercase();
    if accepted.iter().any(|candidate| *candidate == normalized) {
        normalized
    } else {
        String::new()
    }
}

pub(crate) fn build_memory_for_prompt(entry: Option<&Value>, global_banned_urls: Option<&Value>) -> Option<Value> {
    let entry = entry?;
    let deep_news_banned_urls = as_string_list(entry.get("deep_news_banned_urls"), 200, 700)
        .into_iter()
        .filter_map(|value| value.as_str().map(normalize_url))
        .filter(|value| !value.is_empty())
        .map(Value::String)
        .collect::<Vec<_>>();
    let banned_set = deep_news_banned_urls
        .iter()
        .filter_map(|value| value.as_str().map(|text| text.to_string()))
        .chain(
            as_string_list(global_banned_urls, 2000, 700)
                .into_iter()
                .filter_map(|value| value.as_str().map(normalize_url)),
        )
        .collect::<HashSet<_>>();
    let deep_news_seen_urls = as_string_list(entry.get("deep_news_seen_urls"), 400, 700)
        .into_iter()
        .filter_map(|value| value.as_str().map(normalize_url))
        .filter(|url| !url.is_empty() && !banned_set.contains(url))
        .map(Value::String)
        .collect::<Vec<_>>();
    let selected_url = normalize_url(&as_text(entry.get("deep_news_selected_url")));
    Some(json!({
        // V2 fields
        "schema_version": entry.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0),
        "signal": as_text(entry.get("signal")),
        "conviction": as_text(entry.get("conviction")),
        "signal_history": entry.get("signal_history").cloned().unwrap_or(json!([])),
        "key_reasoning": as_text(entry.get("key_reasoning")),
        "price_tracking": entry.get("price_tracking").cloned().unwrap_or(Value::Null),
        "news_themes": entry.get("news_themes").cloned().unwrap_or(json!([])),
        "trend": as_text(entry.get("trend")),
        "user_action": entry.get("user_action").cloned().unwrap_or(Value::Null),
        // Deep news fields (preserved)
        "deep_news_memory_summary": as_text(entry.get("deep_news_memory_summary")),
        "deep_news_selected_url": if !selected_url.is_empty() && !banned_set.contains(&selected_url) { Value::String(selected_url) } else { Value::String(String::new()) },
        "deep_news_seen_urls": deep_news_seen_urls,
        "deep_news_banned_urls": deep_news_banned_urls,
        "deep_news_ban_reasons": entry.get("deep_news_ban_reasons").cloned().unwrap_or_else(|| json!([])),
        "deep_news_quality_score": normalize_deep_news_score(entry.get("deep_news_quality_score")),
        "deep_news_relevance": normalize_deep_news_enum(entry.get("deep_news_relevance"), &["high", "medium", "low"]),
        "deep_news_staleness": normalize_deep_news_enum(entry.get("deep_news_staleness"), &["fresh", "recent", "stale"]),
        "structural_news_insights": entry.get("structural_news_insights").cloned().unwrap_or_else(|| json!([])),
        "last_recommendation": entry.get("last_recommendation").cloned().unwrap_or(Value::Null),
        "run_history": entry.get("run_history").cloned().unwrap_or_else(|| json!([]))
    }))
}

pub(crate) fn read_rotation_seen_urls(store: &Value, ticker: &str) -> HashSet<String> {
    as_string_list(
        store
            .get("deep_news_rotation_cache")
            .and_then(|value| value.get("by_ticker"))
            .and_then(|value| value.get(ticker))
            .and_then(|value| value.get("seen_urls")),
        500,
        700,
    )
    .into_iter()
    .filter_map(|value| value.as_str().map(normalize_url))
    .collect()
}

pub(crate) fn hydrate_row_with_line_memory(store: &Value, row: &Value, news_row: &Value) -> (Value, Value, Value) {
    let ticker = normalize_ticker(row.get("ticker"));
    if ticker.is_empty() {
        return (
            row.clone(),
            news_row.clone(),
            json!({
                "tickers_hydrated": 0,
                "banned_articles_filtered": 0,
                "seen_articles_filtered": 0,
                "global_banned_articles_filtered": 0,
                "total_articles_filtered": 0
            }),
        );
    }
    let entry = store.get("by_ticker").and_then(|value| value.get(&ticker));
    let memory = build_memory_for_prompt(entry, store.get("global_deep_news_banned_urls"));
    let global_banned = as_string_list(store.get("global_deep_news_banned_urls"), 2000, 700)
        .into_iter()
        .filter_map(|value| value.as_str().map(normalize_url))
        .collect::<HashSet<_>>();
    let mut banned = HashSet::new();
    for value in as_string_list(memory.as_ref().and_then(|value| value.get("deep_news_banned_urls")), 200, 700) {
        if let Some(url) = value.as_str() {
            banned.insert(normalize_url(url));
        }
    }
    for url in &global_banned {
        banned.insert(url.clone());
    }
    let mut seen = HashSet::new();
    for value in as_string_list(memory.as_ref().and_then(|value| value.get("deep_news_seen_urls")), 400, 700) {
        if let Some(url) = value.as_str() {
            seen.insert(url.to_string());
        }
    }
    for url in read_rotation_seen_urls(store, &ticker) {
        seen.insert(url);
    }
    let mut banned_filtered = 0;
    let mut seen_filtered = 0;
    let mut global_banned_filtered = 0;
    let articles = as_array(news_row.get("articles"));
    let kept = articles
        .iter()
        .filter_map(|article| {
            let url = normalize_url(&as_text(article.get("url").or_else(|| article.get("link"))));
            if !url.is_empty() && banned.contains(&url) {
                banned_filtered += 1;
                if global_banned.contains(&url) {
                    global_banned_filtered += 1;
                }
                return None;
            }
            if !url.is_empty() && seen.contains(&url) {
                seen_filtered += 1;
                return None;
            }
            Some(article.clone())
        })
        .collect::<Vec<_>>();
    let hydrated_row = if let Some(memory) = memory {
        let mut object = row.as_object().cloned().unwrap_or_default();
        object.insert("memoire_ligne".to_string(), memory);
        Value::Object(object)
    } else {
        row.clone()
    };
    (
        hydrated_row,
        json!({
            "articles": kept,
            "sources": news_row.get("sources").cloned().unwrap_or_else(|| json!([]))
        }),
        json!({
            "tickers_hydrated": if entry.is_some() { 1 } else { 0 },
            "banned_articles_filtered": banned_filtered,
            "seen_articles_filtered": seen_filtered,
            "global_banned_articles_filtered": global_banned_filtered,
            "total_articles_filtered": banned_filtered + seen_filtered
        }),
    )
}

pub(crate) fn fetch_ticker_enrichment(
    ticker: &str,
    name: Option<&str>,
    isin: Option<&str>,
    request_fn: HttpRequestFn,
) -> (Value, Value, Vec<Value>) {
    let base_url = resolve_enrichment_base_url();
    let mut query = format!("ticker={}", percent_encode_component(ticker));
    if let Some(value) = name.filter(|value| !value.trim().is_empty()) {
        query.push_str("&name=");
        query.push_str(&percent_encode_component(value.trim()));
    }
    if let Some(value) = isin.filter(|value| !value.trim().is_empty()) {
        query.push_str("&isin=");
        query.push_str(&percent_encode_component(value.trim().to_uppercase().as_str()));
    }
    let market_result =
        request_json_from_url("GET", &format!("{base_url}/market/spot?{query}"), None, Some(8000), request_fn);
    let news_result =
        request_json_from_url("GET", &format!("{base_url}/news?{query}"), None, Some(8000), request_fn);
    let mut issues = Vec::new();
    if let Err(error) = &market_result {
        issues.push(json!({
            "scope": "market",
            "error_code": infer_issue_code(error),
            "message": error.to_string(),
            "provider": null,
            "upstream_status": null
        }));
    }
    if let Err(error) = &news_result {
        issues.push(json!({
            "scope": "news",
            "error_code": infer_issue_code(error),
            "message": error.to_string(),
            "provider": null,
            "upstream_status": null
        }));
    }
    let market = market_result
        .ok()
        .and_then(|payload| payload.get("market").cloned())
        .unwrap_or_else(|| json!({}));
    let news_items = news_result
        .ok()
        .and_then(|payload| payload.get("news").and_then(|news| news.get("items")).cloned())
        .unwrap_or_else(|| json!([]));
    let sources = as_array(Some(&news_items))
        .iter()
        .filter_map(|article| article.get("source").and_then(|value| value.as_str()))
        .fold(Vec::<Value>::new(), |mut acc, source| {
            if !acc.iter().any(|value| value.as_str() == Some(source)) {
                acc.push(Value::String(source.to_string()));
            }
            acc
        });
    (
        json!({
            "prix_actuel": market.get("price").cloned().unwrap_or(Value::Null),
            "pe_ratio": market.get("pe_ratio").cloned().unwrap_or(Value::Null),
            "revenue_growth": market.get("revenue_growth").cloned().unwrap_or(Value::Null),
            "profit_margin": market.get("profit_margin").cloned().unwrap_or(Value::Null),
            "debt_to_equity": market.get("debt_to_equity").cloned().unwrap_or(Value::Null),
            "source": market.get("source").cloned().unwrap_or(Value::Null)
        }),
        json!({
            "articles": news_items,
            "sources": sources
        }),
        issues,
    )
}

pub(crate) fn infer_issue_code(error: &anyhow::Error) -> String {
    error
        .to_string()
        .split(':')
        .next()
        .unwrap_or("enrichment_fetch_failed")
        .to_string()
}

pub(crate) fn build_collection_state(
    snapshot: &Value,
    positions: &[Value],
    market: &Map<String, Value>,
    news: &Map<String, Value>,
    quality: &Value,
    collection_issues: &[Value],
    failures: &[Value],
    source_mode: &str,
    source_status: &str,
    source_details: &Value,
    hydration: &Value,
) -> Value {
    json!({
        "portfolio": {
            "positions": positions,
            "accounts": snapshot.get("accounts").cloned().unwrap_or_else(|| json!([])),
            "valeur_totale": snapshot.get("valeur_totale").cloned().unwrap_or_else(|| json!(0.0)),
            "plus_value_totale": snapshot.get("plus_value_totale").cloned().unwrap_or_else(|| json!(0.0)),
            "liquidites": snapshot.get("liquidites").cloned().unwrap_or_else(|| json!(0.0))
        },
        "transactions": snapshot.get("transactions").cloned().unwrap_or_else(|| json!([])),
        "orders": snapshot.get("orders").cloned().unwrap_or_else(|| json!([])),
        "market": Value::Object(market.clone()),
        "news": Value::Object(news.clone()),
        "quality": quality.clone(),
        "collection_issues": {
            "count": collection_issues.len(),
            "items": collection_issues
        },
        "enrichment": {
            "status": if failures.is_empty() { "success" } else { "degraded" },
            "failures": failures
        },
        "source_ingestion": {
            "mode": source_mode,
            "status": source_status,
            "connector": if source_mode == "finary" { "finary-connector" } else { "csv" },
            "updated_at": crate::now_iso_string(),
            "used_latest_snapshot": source_details.get("used_latest_snapshot").cloned().unwrap_or_else(|| json!(false)),
            "latest_snapshot_saved_at": source_details.get("latest_snapshot_saved_at").cloned().unwrap_or(Value::Null),
            "degradation_reason": source_details.get("degradation_reason").cloned().unwrap_or(Value::Null)
        },
        "normalization": Value::Null,
        "line_memory_hydration": hydration.clone()
    })
}
