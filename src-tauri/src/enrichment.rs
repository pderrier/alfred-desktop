//! Enrichment API — thin client to the remote Alfred API.
//!
//! All scraping and news fetching happens server-side.
//! Errors are typed so the frontend can show appropriate modals
//! (reconnect for 401, retry for 429, API down for network errors).

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

// ── Public API ─────────────────────────────────────────────────────

pub fn fetch_market_spot(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_market(ticker, name, isin) {
        Ok(resp) => {
            if let Some(market) = resp.get("market") {
                Ok(json!({ "ok": true, "market": market, "cache_hit": false }))
            } else {
                Err(anyhow!("enrichment_market_empty_response:{ticker}"))
            }
        }
        Err(e) => Err(classify_api_error("market", ticker, e)),
    }
}

pub fn fetch_shared_insights(ticker: &str, isin: &str) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_insights(ticker, isin) {
        Ok(resp) => {
            let insights = resp.get("insights").cloned().unwrap_or(Value::Null);
            Ok(json!({ "ok": true, "insights": insights }))
        }
        Err(e) => {
            crate::debug_log(&format!("enrichment insights unavailable for {ticker}: {e}"));
            Ok(json!({ "ok": true, "insights": null }))
        }
    }
}

pub fn fetch_sector(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_sector(ticker, name, isin, canonical) {
        Ok(resp) => Ok(resp),
        Err(e) => {
            crate::debug_log(&format!("enrichment sector unavailable for {ticker}: {e}"));
            Ok(json!({ "ok": true, "sector": null }))
        }
    }
}

pub fn fetch_cot(ticker: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_cot(ticker, isin, canonical) {
        Ok(resp) => Ok(resp),
        Err(e) => {
            crate::debug_log(&format!("enrichment COT unavailable for {ticker}: {e}"));
            Ok(json!({ "ok": true, "cot": null }))
        }
    }
}

/// Resolve the canonical Yahoo symbol for an ISIN via `GET /api/resolve`.
///
/// Silent on errors — mirrors `fetch_sector`/`fetch_cot` style. Returns `None`
/// when:
///   - `isin` is empty/whitespace (no point hitting the resolver)
///   - the server returns `source = "none"` or a null symbol
///   - `/api/resolve` is unreachable (older server, transport error, 5xx)
///
/// Callers must treat `None` as "no canonical resolution available" — they
/// fall back to using the raw ticker as before. Additive contract per
/// `feedback_snapshot_ui_contract`.
pub fn fetch_resolved_symbol(isin: &str) -> Option<String> {
    let code = isin.trim();
    if code.is_empty() {
        return None;
    }
    match crate::alfred_api_client::remote_fetch_resolve(code) {
        Ok(symbol) => symbol,
        Err(e) => {
            crate::debug_log(&format!("enrichment resolve unavailable for {code}: {e}"));
            None
        }
    }
}

/// Fetch the 250-day technical snapshot for a ticker.
///
/// `isin` is forwarded to the server so the source_router can classify EU
/// equities whose ticker is a bare alpha string (e.g. `EXA`, `LBIRD`,
/// `VETO`) — these match the `^[A-Z]{1,5}$` US regex and would otherwise
/// route to Yahoo without an exchange suffix and fail. With the ISIN, the
/// server infers `.PA` (or `.AS`, `.DE`, …) and the Yahoo OHLC fetch
/// succeeds. Callers should pass the row ISIN whenever available.
///
/// Returns `None` silently on any non-200 (404 = ticker unsupported / endpoint
/// not yet deployed, 429 = rate-limited, transport errors, parse failures).
/// The caller stores the result alongside other enrichments; downstream
/// prompt builders render a "non disponible" fallback when `None`.
///
/// The returned `Value` is the inner `technical_snapshot` object — same
/// shape as the `TechnicalSnapshot` struct in `models::`. We pass it as
/// `Value` here to avoid eagerly typing it in the hot path (deserialization
/// happens only when the prompt builder needs it).
///
/// v0.3.2 (P0-1): instrumentation expanded — every failure path now logs
/// the *reason* with the ticker so partial-coverage runs (10/28 in
/// `019e2c9de5ee`) can be diagnosed from `data/debug.log` instead of
/// guessing between 4 hypotheses (timeout / 429 / parse / null indicators).
/// The response parsing is also factored into the pure
/// `parse_technical_snapshot_response` helper so the null-indicators and
/// envelope-shape contracts can be unit-tested without HTTP.
pub fn fetch_technical_snapshot(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> Option<Value> {
    match crate::alfred_api_client::remote_fetch_technicals(ticker, isin, canonical) {
        Ok(resp) => match parse_technical_snapshot_response(&resp) {
            Some(snapshot) => {
                crate::debug_log(&format!(
                    "enrichment technical_snapshot ok for {ticker} (samples={})",
                    snapshot
                        .get("samples")
                        .and_then(|v| v.as_i64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                ));
                Some(snapshot)
            }
            None => {
                crate::debug_log(&format!(
                    "enrichment technical_snapshot dropped for {ticker} — server response had no usable `indicators` object (body keys: {:?})",
                    resp.as_object()
                        .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ));
                None
            }
        },
        Err(e) => {
            crate::debug_log(&format!(
                "enrichment technical_snapshot transport error for {ticker} (isin={:?}, canonical={:?}): {e}",
                isin, canonical
            ));
            None
        }
    }
}

/// Pure response parser for the `/api/market/technicals` envelope. Extracted
/// from `fetch_technical_snapshot` so the contract (null indicators dropped,
/// envelope-or-flat shape accepted, empty object dropped) can be unit-tested
/// without an HTTP round trip.
///
/// Accepts either:
///   - `{ "technical_snapshot": { "indicators": {...}, ... } }` (envelope)
///   - `{ "indicators": {...}, ... }` (flat)
///
/// Rejects (returns `None`) when:
///   - `indicators` field is absent
///   - `indicators` is `null` (server populated cache with no-data marker)
///   - `indicators` is an empty object `{}` (server returned shell)
///   - `indicators` is not an object (defensive — server contract drift)
///
/// v0.3.2 (P0-1): the previous check used `snapshot.get("indicators").is_some()`
/// which **accepts** `indicators: null` (Some(&Value::Null)) and stores a
/// useless snapshot. Tightened so the persisted `technicals` map never
/// contains snapshots that the prompt renderer would treat as "non disponible"
/// anyway — partial coverage is now visible at the persist layer, not buried
/// in the prompt.
pub(crate) fn parse_technical_snapshot_response(resp: &Value) -> Option<Value> {
    // Envelope may wrap as { "technical_snapshot": {...} } or be the snapshot
    // directly — accept both. Cloning is cheap: ~1KB JSON per ticker.
    let snapshot = resp
        .get("technical_snapshot")
        .cloned()
        .unwrap_or_else(|| resp.clone());
    let indicators = snapshot.get("indicators")?;
    let indicators_obj = indicators.as_object()?;
    if indicators_obj.is_empty() {
        return None;
    }
    Some(snapshot)
}

pub fn fetch_news(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_news(ticker, name, isin, canonical) {
        Ok(resp) => {
            if let Some(news) = resp.get("news") {
                Ok(json!({ "ok": true, "news": news, "cache_hit": false }))
            } else {
                Ok(json!({ "ok": true, "news": { "items": [] }, "cache_hit": false }))
            }
        }
        Err(e) => Err(classify_api_error("news", ticker, e)),
    }
}

/// Classify API errors into typed codes the frontend can act on.
fn classify_api_error(scope: &str, ticker: &str, err: anyhow::Error) -> anyhow::Error {
    let msg = err.to_string();
    if msg.contains("alfred_api_unauthorized") {
        anyhow!("alfred_api_auth_required:{}:{}", scope, ticker)
    } else if msg.contains("alfred_api_rate_limited") {
        anyhow!("alfred_api_rate_limited:{}:{}", scope, ticker)
    } else if msg.contains("alfred_api_not_configured") || msg.contains("alfred_api_no_jwt") {
        anyhow!("alfred_api_not_configured:{}:{}", scope, ticker)
    } else if msg.contains("alfred_api_http_error") {
        anyhow!("alfred_api_server_error:{}:{}:{}", scope, ticker, msg)
    } else {
        anyhow!("alfred_api_unreachable:{}:{}:{}", scope, ticker, msg)
    }
}
