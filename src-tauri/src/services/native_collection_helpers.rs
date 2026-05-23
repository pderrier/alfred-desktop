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

/// Returns true when the market enrichment provider tag indicates real provider
/// data (boursorama, google_finance, alphavantage, yahoo, …) and not the
/// `"none"` sentinel written by `resolve_source_current_price` when every
/// provider failed and the row's PRU was used as a last-resort price.
///
/// Bug A guard: `apply_collection_result` calls `sync_position_from_market`
/// only when this returns `true`, so the PRU-fallback never overwrites the
/// position's gain/loss fields with `0.0` (`prix_actuel == prix_revient`).
pub(crate) fn is_real_market_source(source: &str) -> bool {
    let s = source.trim();
    !s.is_empty() && s != "none"
}

/// P0-55 integrity check — `true` when the position carries a canonical
/// `resolved_symbol` (set by `resolve_canonical_symbols` after a successful
/// `/api/resolve` call). When `false`, the upstream ISIN resolver either
/// skipped the call (malformed ISIN, P0-54) or got `None` back from the
/// server. Callers must not apply market data on a position that fails
/// this check, since the market row was fetched with the raw `ticker`
/// (often a generic name-derived token like `PARTS` / `THE`) and can be a
/// Google Finance false-match on an unrelated instrument.
///
/// Treats `null` and empty string the same as missing — both shapes are
/// observed depending on the upstream caller (Finary path serializes
/// `None` as null; CSV path may leave the key out entirely until resolve
/// runs).
pub(crate) fn has_canonical_resolution(position: &Value) -> bool {
    position
        .get("resolved_symbol")
        .and_then(|v| v.as_str())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

/// Bug A fix — re-sync `prix_actuel` / `valeur_actuelle` / `plus_moins_value(_pct)`
/// on a position row from the freshly enriched `market` row.
///
/// Returns `true` when the position was updated (real provider price applied),
/// `false` when the market row had no usable price or only the `"none"`
/// fallback source. Caller is responsible for the source guard via
/// `is_real_market_source` — this helper enforces it again so it stays safe
/// when unit-tested in isolation.
///
/// The position contract this writes back is the one consumed by the UI
/// (`shell-layout.js` → `portfolio.positions[]`):
/// - `prix_actuel`           : current market price
/// - `valeur_actuelle`       : quantite * prix_actuel
/// - `plus_moins_value`      : valeur_actuelle - quantite * prix_revient
/// - `plus_moins_value_pct`  : (prix_actuel / prix_revient - 1) * 100, or 0 if PRU is 0
///
/// P0-55 guard — `apply_collection_result` calls this only when
/// `has_canonical_resolution` returns `true`, i.e. when `resolved_symbol`
/// is present on the position. Without that signal the market row may be a
/// Google Finance false-match on a generic name-derived ticker and must
/// not be applied.
pub(crate) fn sync_position_from_market(position: &mut Value, market: &Value) -> bool {
    let source = market
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !is_real_market_source(source) {
        return false;
    }
    let market_price = market
        .get("prix_actuel")
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v > 0.0);
    let Some(price) = market_price else {
        return false;
    };
    let Some(obj) = position.as_object_mut() else {
        return false;
    };
    let qty = obj.get("quantite").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pru = obj.get("prix_revient").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let valeur = qty * price;
    let pnl = valeur - (qty * pru);
    let pnl_pct = if pru > 0.0 { (price / pru - 1.0) * 100.0 } else { 0.0 };
    obj.insert("prix_actuel".to_string(), json!(price));
    obj.insert("valeur_actuelle".to_string(), json!(valeur));
    obj.insert("plus_moins_value".to_string(), json!(pnl));
    obj.insert("plus_moins_value_pct".to_string(), json!(pnl_pct));
    true
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
    // Preserve cross-account context fields (Phase 1) — never consumed by UI,
    // only by the per-account synthesis prompt builders.
    for key in ["holdings_accounts", "portfolio_summary", "cash_by_currency"] {
        if let Some(value) = snapshot.get(key) {
            if let Some(obj) = result.as_object_mut() {
                obj.insert(key.to_string(), value.clone());
            }
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
    // P0-54: pass csv_parsing_issues (price outliers, malformed ISIN) through
    // so the workflow loop can merge them into collection_issues.
    if let Some(issues) = snapshot.get("csv_parsing_issues") {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("csv_parsing_issues".to_string(), issues.clone());
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

// ── CSV ingestion sanity checks (P0-53, P0-54) ───────────────────────
//
// Two helpers introduced for the CSV pipeline:
//
// * `ticker_from_resolved` — derive a short, broker-agnostic ticker from a
//   canonical Yahoo symbol (e.g. `MC.PA` → `MC`, `AAPL` → `AAPL`). Used to
//   reconcile the rough name-derived ticker on a CSV position with the
//   canonical symbol returned by `/api/resolve`. Without this reconciliation
//   the line-memory write-back after analysis forks the `by_ticker` map
//   (one entry under `LVMH`, another under `MC.PA`).
//
// * `validate_isin` — strict ISO 6166 check: 2 uppercase country letters +
//   9 alphanumerics + 1 numeric checksum digit, with the Luhn-mod-10
//   verification step. Lets us skip `/api/resolve` for obvious garbage
//   ISINs and surface a `malformed_isin` issue to the UI.

/// Maximum acceptable unit price (in account currency) for a CSV-imported
/// position. Anything above this triggers a `price_outlier_suspect` issue.
/// `BRK.A` at ~700k$ is the only widely-held single share above 50 000 EUR,
/// so a 10 000 EUR threshold cleanly separates normal equities from parser
/// glitches (Boursorama-style PEA CSVs occasionally fill the price column
/// with a totals figure for non-actively-priced lines).
pub(crate) const CSV_PRICE_OUTLIER_THRESHOLD_EUR: f64 = 10_000.0;

/// Securities whose normal unit price exceeds the outlier threshold. The
/// list is intentionally minimal — adding too many entries weakens the
/// sanity check.
pub(crate) const CSV_HIGH_PRICE_WHITELIST: &[&str] = &[
    // Berkshire Hathaway class A (~700 000 USD per share, ISIN US0846701086)
    "US0846701086", "BRK.A", "BRK-A", "BRKA",
    // NVR Inc (~9 000 USD per share, ISIN US62944T1051) — close to threshold
    // but legitimately a high-priced single share.
    "US62944T1051", "NVR",
];

/// Strip an exchange suffix from a Yahoo-style symbol to produce a short,
/// human-readable ticker. `MC.PA` → `MC`, `STMPA.PA` → `STMPA`, `AAPL` →
/// `AAPL`. Empty input returns `None`.
pub(crate) fn ticker_from_resolved(resolved: &str) -> Option<String> {
    let trimmed = resolved.trim();
    if trimmed.is_empty() {
        return None;
    }
    let base = trimmed.split('.').next().unwrap_or(trimmed);
    if base.is_empty() {
        return None;
    }
    Some(base.to_uppercase())
}

/// Strict ISO 6166 ISIN validator.
///
/// Returns true iff `isin` matches the format `[A-Z]{2}[A-Z0-9]{9}[0-9]`
/// AND the trailing digit is the correct Luhn-mod-10 checksum over the
/// 11-character body (each letter is expanded to its `A=10 .. Z=35`
/// decimal pair before applying the standard Luhn algorithm right-to-left).
///
/// Examples:
/// * `FR0000121014` (LVMH) — valid, checksum 4
/// * `US0378331005` (AAPL) — valid, checksum 5
/// * `000007764440` — invalid, no country code letters at positions 0-1
/// * `FR0000121013` — invalid, checksum mismatch (real LVMH ISIN ends with 4)
///
/// Garbage in, false out — never panics.
pub(crate) fn validate_isin(isin: &str) -> bool {
    let bytes = isin.as_bytes();
    if bytes.len() != 12 {
        return false;
    }
    // Position 0..2 — uppercase letters.
    if !bytes[..2].iter().all(|b| b.is_ascii_uppercase()) {
        return false;
    }
    // Position 2..11 — alphanumeric uppercase or digit.
    if !bytes[2..11]
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return false;
    }
    // Position 11 — single digit.
    if !bytes[11].is_ascii_digit() {
        return false;
    }
    luhn_isin_checksum_matches(bytes)
}

/// Compute and verify the ISIN Luhn-mod-10 checksum.
///
/// Algorithm (ISO 6166): expand letters to their two-digit `A=10 .. Z=35`
/// representation, producing a long decimal string. Walk that string
/// right-to-left, doubling every other digit (starting with the
/// second-from-right). Sum all digits, including those produced by the
/// doubling (split into tens and units). The total must be a multiple
/// of 10.
fn luhn_isin_checksum_matches(bytes: &[u8]) -> bool {
    let mut digits: Vec<u32> = Vec::with_capacity(24);
    for &b in bytes {
        if b.is_ascii_digit() {
            digits.push((b - b'0') as u32);
        } else if b.is_ascii_uppercase() {
            let n = (b - b'A') as u32 + 10;
            digits.push(n / 10);
            digits.push(n % 10);
        } else {
            return false;
        }
    }
    // Right-to-left, double every second digit (positions 1, 3, 5… from the right).
    let mut sum = 0u32;
    for (i, d) in digits.iter().rev().enumerate() {
        let weighted = if i % 2 == 1 { d * 2 } else { *d };
        sum += weighted / 10 + weighted % 10;
    }
    sum % 10 == 0
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
        "memory_narrative": as_text(entry.get("memory_narrative").or(entry.get("key_reasoning"))),
        "price_tracking": entry.get("price_tracking").cloned().unwrap_or(Value::Null),
        "news_themes": entry.get("news_themes").cloned().unwrap_or(json!([])),
        "trend": as_text(entry.get("trend")),
        "user_action": entry.get("user_action").cloned().unwrap_or(Value::Null),
        // Bug B follow-up — propagate the zero-price repair flag so
        // `build_memory_section` can skip the "prix au signal" line entirely
        // when the persisted price history is all zeros.
        "price_data_unavailable": entry
            .get("price_data_unavailable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
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
    // v0.3 (#22): cross-account dedup — prefer the canonical Yahoo symbol
    // stashed on `row.resolved_symbol` so two brokers carrying the same
    // security share line-memory history. Falls back to the raw ticker key
    // for rows without a resolution (pre-v0.3 entries, watchlist without ISIN).
    let resolved = row.get("resolved_symbol").and_then(|v| v.as_str());
    let entry_value = crate::native_mcp_analysis::read_line_memory_entry(store, &ticker, resolved);
    let entry = if entry_value.is_null() { None } else { Some(&entry_value) };
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
    canonical: Option<&str>,
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
    // Canonical Yahoo symbol — additive, propagates the v0.3 parity contract
    // through the in-process HTTP shim to /news (only news consumes it here;
    // /market/spot routes through fetch_market_spot which keys on ticker+isin
    // and benefits transparently from the server-side Yahoo fallback).
    if let Some(value) = canonical.filter(|value| !value.trim().is_empty()) {
        query.push_str("&canonical=");
        query.push_str(&percent_encode_component(value.trim()));
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

// ── Holdings metadata enrichment (Phase 1: cross-account context) ────────────
//
// Extracts an enriched per-account view of `holdings_accounts` from the Finary
// API response. The result feeds `snapshot.holdings_accounts`, which is the
// LLM-only view used to build cross-account context. `snapshot.accounts` (the
// UI contract) is built separately from positions and is NOT affected.
//
// The `kind` field is computed structurally — no name/institution regex — so
// the classification works generically across any Finary deployment:
//   - `liability`   when `total_value < 0`   (loans are negative-value)
//   - `investment`  when `securities_count > 0`
//   - `cash_only`   when `securities_count == 0 && fiats_sum_eur > 0`
//   - `other`       otherwise (real estate manual, non-EUR-only, etc.)
//
// `institution_provider_categories` is the raw list of strings observed in
// `acct.institution_connection.institution_provider.account_types[].name`.
// Real values observed in production: "stocks", "checkings", "savings",
// "cryptos", "real_estate", "loans".
pub(crate) fn build_holdings_metadata(holdings_accounts: &[Value]) -> Vec<Value> {
    holdings_accounts.iter().map(build_holdings_entry).collect()
}

fn build_holdings_entry(acct: &Value) -> Value {
    let name = acct.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let slug = acct.get("slug").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let institution_name = acct.get("institution")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let securities_count = acct.get("securities")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let total_value = acct.get("total_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let total_gain = acct.get("total_gain").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // Cash by currency: each `fiat` in `fiats[]` is in its local currency.
    // Keep them separate — the LLM uses this to recommend cash sourcing.
    let mut cash_by_currency: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    if let Some(fiats) = acct.get("fiats").and_then(|v| v.as_array()) {
        for fiat in fiats {
            let code = fiat.get("currency")
                .and_then(|v| v.get("code"))
                .and_then(|v| v.as_str())
                .unwrap_or("EUR")
                .to_string();
            let amount = fiat.get("current_value")
                .or_else(|| fiat.get("amount"))
                .or_else(|| fiat.get("quantity"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            *cash_by_currency.entry(code).or_insert(0.0) += amount;
        }
    }
    let fiats_sum_eur = cash_by_currency.get("EUR").copied().unwrap_or(0.0);
    let cash_eur = fiats_sum_eur;

    let kind = classify_holding_kind(securities_count, fiats_sum_eur, total_value);

    let institution_provider_categories: Vec<Value> = acct
        .get("institution_connection")
        .and_then(|ic| ic.get("institution_provider"))
        .and_then(|ip| ip.get("account_types"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(String::from))
                .map(Value::String)
                .collect()
        })
        .unwrap_or_default();

    // Serialize `cash_by_currency` deterministically (sorted by key) for stable output.
    let mut currency_keys: Vec<String> = cash_by_currency.keys().cloned().collect();
    currency_keys.sort();
    let mut cash_by_currency_obj = serde_json::Map::new();
    for code in &currency_keys {
        cash_by_currency_obj.insert(code.clone(), json!(cash_by_currency[code]));
    }

    json!({
        "name": name,
        "slug": slug,
        "institution_name": institution_name,
        "kind": kind,
        "institution_provider_categories": institution_provider_categories,
        "total_value": total_value,
        "total_gain": total_gain,
        "securities_count": securities_count,
        "cash": cash_eur,
        "cash_by_currency": Value::Object(cash_by_currency_obj),
    })
}

/// Structural classification of a Finary holdings_account into a coarse kind.
///
/// Rules apply in order — `liability` wins over `investment` (a margin/loan
/// account with negative net value is still a liability). `cash_only` requires
/// strictly positive EUR cash (other-currency-only accounts fall into `other`).
pub(crate) fn classify_holding_kind(
    securities_count: usize,
    fiats_sum_eur: f64,
    total_value: f64,
) -> &'static str {
    if total_value < 0.0 {
        return "liability";
    }
    if securities_count > 0 {
        return "investment";
    }
    if fiats_sum_eur > 0.0 {
        return "cash_only";
    }
    "other"
}

/// Aggregate portfolio-level summary from holdings metadata.
///
/// Tie-break for `value_by_institution_provider_category`: when a holdings
/// account exposes multiple categories (rare in practice), the account's full
/// value is attributed to the FIRST category in the `account_types[]` list.
/// Documented in `docs/finary-snapshot-schema.md`.
pub(crate) fn build_portfolio_summary(holdings_metadata: &[Value]) -> Value {
    let mut total_value = 0.0_f64;
    let mut total_cash_eur = 0.0_f64;
    let mut cash_by_currency: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    let mut value_by_kind: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    let mut value_by_category: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();

    for entry in holdings_metadata {
        let value = entry.get("total_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cash_eur = entry.get("cash").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("other").to_string();

        total_value += value;
        total_cash_eur += cash_eur;
        *value_by_kind.entry(kind).or_insert(0.0) += value;

        if let Some(map) = entry.get("cash_by_currency").and_then(|v| v.as_object()) {
            for (code, amount) in map {
                let amt = amount.as_f64().unwrap_or(0.0);
                if amt != 0.0 {
                    *cash_by_currency.entry(code.clone()).or_insert(0.0) += amt;
                }
            }
        }

        if let Some(categories) = entry.get("institution_provider_categories").and_then(|v| v.as_array()) {
            if let Some(first) = categories.first().and_then(|v| v.as_str()) {
                *value_by_category.entry(first.to_string()).or_insert(0.0) += value;
            }
        }
    }

    // Deterministic serialization (sorted keys) for stable diffs and tests.
    let to_sorted_map = |map: std::collections::HashMap<String, f64>| -> Value {
        let mut keys: Vec<String> = map.keys().cloned().collect();
        keys.sort();
        let mut obj = serde_json::Map::new();
        for k in &keys {
            obj.insert(k.clone(), json!(map[k]));
        }
        Value::Object(obj)
    };

    json!({
        "total_value": total_value,
        "total_cash_eur": total_cash_eur,
        "cash_by_currency": to_sorted_map(cash_by_currency),
        "value_by_kind": to_sorted_map(value_by_kind),
        "value_by_institution_provider_category": to_sorted_map(value_by_category),
        "account_count": holdings_metadata.len(),
    })
}

/// Aggregate `cash_by_currency` across all holdings entries.
/// Used to enrich snapshot top-level with multi-currency cash visibility,
/// while keeping `snapshot.cash` (EUR only) for backward compatibility.
pub(crate) fn aggregate_cash_by_currency(holdings_metadata: &[Value]) -> Value {
    let mut totals: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for entry in holdings_metadata {
        if let Some(map) = entry.get("cash_by_currency").and_then(|v| v.as_object()) {
            for (code, amount) in map {
                let amt = amount.as_f64().unwrap_or(0.0);
                if amt != 0.0 {
                    *totals.entry(code.clone()).or_insert(0.0) += amt;
                }
            }
        }
    }
    let mut keys: Vec<String> = totals.keys().cloned().collect();
    keys.sort();
    let mut obj = serde_json::Map::new();
    for k in &keys {
        obj.insert(k.clone(), json!(totals[k]));
    }
    Value::Object(obj)
}

// ── Cross-account context (Phase 1: synthesis prompt enrichment) ────────────
//
// Builds the `cross_account_context` block stored on `run_state` and
// surfaced to the per-account synthesis prompt. The goal is to let the LLM:
//
//   1. Recommend lifting cash from a livret / compte courant instead of
//      selling a position, when `value_by_kind.cash_only` or a `savings` /
//      `checkings` category holds enough.
//   2. Avoid recommending a reinforcement of a ticker already top-weighted
//      on another account.
//   3. Stay aware of cross-account thematic concentration.
//
// The synthesis itself MUST remain centered on the target account — this is
// reinforced by an explicit instruction block in the prompt.
pub(crate) fn build_cross_account_context(
    snapshot: &Value,
    target_account: &str,
    cross_account_themes: Value,
) -> Value {
    let holdings_metadata = snapshot
        .get("holdings_accounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let portfolio_summary = snapshot
        .get("portfolio_summary")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let all_positions = snapshot
        .get("positions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let other_accounts: Vec<Value> = holdings_metadata
        .iter()
        .filter(|entry| {
            // Exclude the target account. Holdings_accounts identifies by
            // `name`; the target account is identified by the same string
            // value used in `positions[].compte`.
            let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
            name != target_account
        })
        .map(|entry| compact_other_account(entry, &all_positions))
        .collect();

    json!({
        "target_account": target_account,
        "other_accounts": other_accounts,
        "portfolio_totals": portfolio_summary,
        "cross_account_themes": cross_account_themes,
    })
}

fn compact_other_account(entry: &Value, all_positions: &[Value]) -> Value {
    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let slug = entry.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let institution_name = entry.get("institution_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("other").to_string();
    let categories = entry.get("institution_provider_categories").cloned().unwrap_or_else(|| json!([]));
    let total_value = entry.get("total_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cash_by_currency = entry.get("cash_by_currency").cloned().unwrap_or_else(|| json!({}));

    let mut compact = serde_json::Map::new();
    compact.insert("slug".into(), Value::String(slug));
    compact.insert("name".into(), Value::String(name.clone()));
    compact.insert("institution_name".into(), Value::String(institution_name));
    compact.insert("kind".into(), Value::String(kind.clone()));
    compact.insert("institution_provider_categories".into(), categories);
    compact.insert("total_value_eur".into(), json!(total_value));
    compact.insert("cash_by_currency".into(), cash_by_currency);

    // top_positions: only for `investment` accounts (other kinds have none).
    if kind == "investment" {
        compact.insert("top_positions".into(), top_positions_for(&name, all_positions));
    }

    Value::Object(compact)
}

fn top_positions_for(account_name: &str, all_positions: &[Value]) -> Value {
    let mut filtered: Vec<&Value> = all_positions
        .iter()
        .filter(|p| as_text(p.get("compte")) == account_name)
        .collect();
    filtered.sort_by(|a, b| {
        let av = a.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bv = b.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });
    let total_value: f64 = filtered.iter()
        .map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum();
    let top: Vec<Value> = filtered.iter().take(3).map(|p| {
        let value = p.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let weight_pct = if total_value > 0.0 { (value / total_value) * 100.0 } else { 0.0 };
        json!({
            "ticker": as_text(p.get("ticker")),
            "nom": as_text(p.get("nom")),
            "weight_pct": (weight_pct * 10.0).round() / 10.0,
        })
    }).collect();
    Value::Array(top)
}

/// Render the cross-account section that gets injected into both
/// `build_synthesis_prompt` (codex MCP path) and `build_report_prompt`
/// (native / native-oauth path). Same text in both ensures 3-mode parity
/// per `product_llm_mode_parity_2026_04.md`.
///
/// Returns empty string when:
///   - `context` is null or missing the `other_accounts` array
///   - `other_accounts` is empty AND there are no cross-account themes
///     (single-account user — no need to clutter the prompt).
pub(crate) fn build_cross_account_prompt_section(context: &Value) -> String {
    let target = context.get("target_account").and_then(|v| v.as_str()).unwrap_or("");
    let others = match context.get("other_accounts").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return String::new(),
    };
    let themes = context
        .get("cross_account_themes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if others.is_empty() && themes.is_empty() {
        return String::new();
    }

    let totals = context.get("portfolio_totals").cloned().unwrap_or_else(|| json!({}));
    let total_value = totals.get("total_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let total_cash_eur = totals.get("total_cash_eur").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let cash_by_currency = totals.get("cash_by_currency").cloned().unwrap_or_else(|| json!({}));
    let value_by_kind = totals.get("value_by_kind").cloned().unwrap_or_else(|| json!({}));
    let value_by_category = totals
        .get("value_by_institution_provider_category")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let cash_curr_str = compact_map_inline(&cash_by_currency);
    let kind_str = compact_map_inline(&value_by_kind);
    let category_str = compact_map_inline(&value_by_category);

    let mut lines = vec![
        String::new(),
        "## Contexte cross-account (pour arbitrage uniquement)".to_string(),
        format!("- Valeur totale portefeuille (EUR): {total_value:.0}"),
        format!("- Cash total mobilisable (EUR): {total_cash_eur:.0}"),
        format!("- Cash multi-devises: {cash_curr_str}"),
        format!("- Repartition par type structurel: {kind_str}"),
        format!("- Repartition par categorie Finary: {category_str}"),
        format!("- Autres comptes ({}):", others.len()),
    ];
    for entry in others {
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let inst = entry.get("institution_name").and_then(|v| v.as_str()).unwrap_or("?");
        let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
        let cats = entry.get("institution_provider_categories").cloned().unwrap_or_else(|| json!([]));
        let cats_str = cats.as_array()
            .map(|arr| arr.iter().filter_map(|c| c.as_str()).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let value = entry.get("total_value_eur").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let cash_curr = entry.get("cash_by_currency").cloned().unwrap_or_else(|| json!({}));
        let cash_curr_inline = compact_map_inline(&cash_curr);
        let mut line = format!(
            "  - \"{name}\" ({inst}, kind={kind}, categories=[{cats_str}]) valeur={value:.0}EUR, cash={cash_curr_inline}"
        );
        if let Some(tops) = entry.get("top_positions").and_then(|v| v.as_array()) {
            if !tops.is_empty() {
                let tops_str: Vec<String> = tops.iter().map(|p| {
                    let t = p.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
                    let w = p.get("weight_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    format!("{t}({w:.0}%)")
                }).collect();
                line.push_str(&format!(" top: {}", tops_str.join(", ")));
            }
        }
        lines.push(line);
    }

    if !themes.is_empty() {
        lines.push("- Themes communs detectes sur d'autres comptes:".to_string());
        for theme in &themes {
            let name = theme.get("theme").and_then(|v| v.as_str()).unwrap_or("?");
            let tickers = theme.get("tickers").and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let accounts = theme.get("accounts").and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            lines.push(format!("  - \"{name}\": tickers [{tickers}] presents sur comptes [{accounts}]"));
        }
    }

    lines.push(String::new());
    lines.push(format!(
        "INSTRUCTION: la synthese reste centree sur \"{target}\". Utilise ce"
    ));
    lines.push("contexte UNIQUEMENT pour:".to_string());
    lines.push("1. Si tu proposes de lever du cash : signale d'abord si du cash mobilisable".to_string());
    lines.push("   existe ailleurs (kind=cash_only ou categorie savings/checkings) au lieu".to_string());
    lines.push("   de vendre.".to_string());
    lines.push("2. Evite de recommander un renforcement d'une position deja en top".to_string());
    lines.push("   position d'un autre compte (signaler la redondance cross-account).".to_string());
    lines.push("3. Coherence thematique : si un theme est deja dominant sur un autre".to_string());
    lines.push("   compte, en tenir compte.".to_string());

    lines.join("\n")
}

fn compact_map_inline(value: &Value) -> String {
    match value.as_object() {
        Some(obj) if !obj.is_empty() => {
            let mut entries: Vec<(String, f64)> = obj
                .iter()
                .map(|(k, v)| (k.clone(), v.as_f64().unwrap_or(0.0)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{k}={v:.0}"))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        _ => "{}".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_collection_state(
    snapshot: &Value,
    positions: &[Value],
    market: &Map<String, Value>,
    news: &Map<String, Value>,
    technicals: &Map<String, Value>,
    quality: &Value,
    collection_issues: &[Value],
    failures: &[Value],
    source_mode: &str,
    source_status: &str,
    source_details: &Value,
    hydration: &Value,
    cross_account_context: Option<&Value>,
) -> Value {
    let mut state = json!({
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
        // Server-computed 250d technicals (SMA/RSI/MACD/ATR/52w). Parallel to
        // `market` — never replaces it. Empty map when the endpoint is
        // unavailable; consumers fall back to "non disponible".
        "technicals": Value::Object(technicals.clone()),
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
    });
    if let Some(ctx) = cross_account_context {
        if let Some(obj) = state.as_object_mut() {
            obj.insert("cross_account_context".to_string(), ctx.clone());
        }
    }
    state
}
