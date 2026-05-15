//! Client for the remote Alfred API server.
//!
//! Calls /api/market, /api/news, /api/search on the remote server.
//! Auth: HMAC-signed requests — the API secret is embedded at compile time
//! (via CI), never exposed in source. The OpenAI JWT never leaves the device.

use std::env;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_API_URL: &str = "https://vps-c5793aab.vps.ovh.net/alfred/api";
const TIMEOUT_SECS: u64 = 10;

/// API secret embedded at compile time by CI (ALFRED_API_SECRET env var).
/// In dev builds without the secret, HMAC auth is skipped (API falls back to permissive mode).
const API_SECRET: Option<&str> = option_env!("ALFRED_API_SECRET");

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

// ── HMAC request signing ────────────────────────────────────────────

/// Deterministic HMAC using FNV-1a (stable across compilations, unlike DefaultHasher).
fn hmac_sign(path: &str, timestamp: u64, secret: &str) -> String {
    let msg = format!("{path}:{timestamp}:{secret}");
    let a = fnv1a_64(msg.as_bytes());
    let msg2 = format!("{a:016x}:{secret}:alfred");
    let b = fnv1a_64(msg2.as_bytes());
    format!("{a:016x}{b:016x}")
}

/// FNV-1a 64-bit hash — deterministic, no random seed.
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Get a client hash for rate-limiting (from local JWT, never sent raw).
fn get_client_hash() -> Option<String> {
    let jwt = get_local_jwt()?;
    Some(format!("{:016x}", fnv1a_64(jwt.as_bytes())))
}

/// Apply auth headers to a ureq request: HMAC signature + client hash + timestamp.
fn apply_auth(req: ureq::Request, path: &str) -> ureq::Request {
    let ts = now_epoch_secs();
    let client_hash = get_client_hash().unwrap_or_default();
    // Sign only the path component (no query string) — must match server-side req.uri().path()
    let sign_path = path.split('?').next().unwrap_or(path);
    let req = req
        .set("X-Client-Hash", &client_hash)
        .set("X-Timestamp", &ts.to_string());
    let runtime_secret = env::var("ALFRED_API_SECRET").ok();
    let secret = API_SECRET.or(runtime_secret.as_deref());
    if let Some(s) = secret {
        let sig = hmac_sign(sign_path, ts, s);
        req.set("X-Signature", &sig)
    } else {
        req
    }
}

/// Read the OpenAI JWT from local Codex session (never sent to the API).
fn get_local_jwt() -> Option<String> {
    if let Some(token) = env::var("ALFRED_API_TOKEN").ok().filter(|t| !t.is_empty()) {
        return Some(token);
    }
    let home = env::var("HOME").or_else(|_| env::var("USERPROFILE")).ok()?;
    let auth_path = format!("{home}/.codex/auth.json");
    if let Ok(content) = std::fs::read_to_string(&auth_path) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
            if let Some(tokens) = parsed.get("tokens") {
                for key in &["access_token", "id_token"] {
                    if let Some(token) = tokens.get(key).and_then(|v| v.as_str()) {
                        if !token.is_empty() {
                            return Some(token.to_string());
                        }
                    }
                }
            }
            for key in &["access_token", "token", "jwt", "id_token", "OPENAI_API_KEY"] {
                if let Some(token) = parsed.get(key).and_then(|v| v.as_str()) {
                    if !token.is_empty() {
                        return Some(token.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Authenticated GET request to the API.
fn api_get(path: &str, timeout: u64) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::get(&url), path)
        .timeout(Duration::from_secs(timeout));
    let resp = req.call().map_err(|e| map_api_error(e))?;
    resp.into_json().map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))
}

/// Authenticated POST request to the API (fire-and-forget).
fn api_post(path: &str, body: &Value) {
    let base = match api_url() { Some(u) => u, None => return };
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5));
    let _ = req.send_string(&serde_json::to_string(body).unwrap_or_default());
}

/// Fetch market data from the remote API.
pub fn remote_fetch_market(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/market?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch news from the remote API (SearXNG-backed).
pub fn remote_fetch_news(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/news?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch shared insights for a ticker from the API cache.
pub fn remote_fetch_insights(ticker: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/insights?ticker={}&isin={}", urlenc(ticker), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch sector classification for a ticker from the API.
pub fn remote_fetch_sector(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/sector?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch technical snapshot (SMA/RSI/MACD/ATR/52w) computed server-side from
/// ~250 trading days of OHLC. Returns the parsed JSON envelope verbatim — the
/// caller is responsible for unwrapping `technical_snapshot`.
///
/// `isin` is forwarded so the server-side source_router can classify a bare
/// French/EU ticker (e.g. `EXA`, `LBIRD`) as European and derive the correct
/// Yahoo exchange suffix (`.PA`). Without it, the regex `^[A-Z]{1,5}$`
/// classifies these as US and the Yahoo OHLC fetch returns
/// `yahoo:yahoo_no_timestamps` — see `docs/technical-snapshot.md` and the
/// server `source_router::infer_exchange_suffix` helper.
///
/// Server endpoint may not be deployed yet — callers must handle errors
/// gracefully (typically map to `None`).
pub fn remote_fetch_technicals(ticker: &str, isin: Option<&str>) -> Result<Value> {
    api_get(&build_technicals_path(ticker, isin), TIMEOUT_SECS)
}

/// Build the `/api/market/technicals` query string for `(ticker, isin)`.
/// Extracted so the URL contract is unit-testable without an HTTP round trip.
///
/// The `isin` argument is included only when present and non-empty after
/// trimming — empty strings degrade gracefully to a ticker-only query so
/// existing callers without ISIN keep their previous wire shape.
fn build_technicals_path(ticker: &str, isin: Option<&str>) -> String {
    let trimmed_isin = isin.map(str::trim).filter(|s| !s.is_empty());
    match trimmed_isin {
        Some(code) => format!(
            "/api/market/technicals?ticker={}&isin={}",
            urlenc(ticker),
            urlenc(code),
        ),
        None => format!("/api/market/technicals?ticker={}", urlenc(ticker)),
    }
}

/// Fetch COT data for a ticker from the API.
pub fn remote_fetch_cot(ticker: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/cot?ticker={}&isin={}", urlenc(ticker), urlenc(isin)), TIMEOUT_SECS)
}

/// Persist shared insights (generic analysis) back to the API for other users.
/// Optionally includes sector classification and sector analysis memo.
pub fn persist_shared_insights(ticker: &str, isin: &str, insights: &Value, sector: Option<&str>, sector_analysis: Option<&str>) {
    if !insights.is_object() || insights.as_object().map(|o| o.is_empty()).unwrap_or(true) { return; }
    let mut body = serde_json::json!({ "ticker": ticker, "isin": isin, "insights": insights });
    if let Some(s) = sector {
        body["sector"] = serde_json::json!(s);
    }
    if let Some(sa) = sector_analysis {
        body["sector_analysis"] = serde_json::json!(sa);
    }
    api_post("/api/insights", &body);
    crate::debug_log(&format!("alfred-api: persisted shared insights for {ticker}"));
}

/// Persist LLM-extracted fundamental values back to the API cache.
pub fn persist_extracted_fundamentals(ticker: &str, isin: &str, extracted: &Value) {
    if !extracted.is_object() || extracted.as_object().map(|o| o.is_empty()).unwrap_or(true) { return; }
    api_post("/api/market/extracted", &serde_json::json!({ "ticker": ticker, "isin": isin, "extracted_fundamentals": extracted }));
    crate::debug_log(&format!("alfred-api: persisted extracted fundamentals for {ticker}"));
}

/// Persist a deep news summary for a specific article URL to the API cache.
pub fn persist_deep_news_summary(
    ticker: &str, isin: &str,
    article_url: &str, title: &str, summary: &str,
    quality_score: u64, relevance: &str, staleness: &str,
) {
    if summary.is_empty() || article_url.is_empty() { return; }
    api_post("/api/deep-news", &serde_json::json!({
        "ticker": ticker, "isin": isin, "url": article_url, "title": title,
        "summary": summary, "quality_score": quality_score, "relevance": relevance, "staleness": staleness,
    }));
    crate::debug_log(&format!("alfred-api: persisted deep news for {ticker}"));
}

/// Ban a news URL as noise for a ticker.
pub fn ban_deep_news_url(ticker: &str, isin: &str, article_url: &str, reason: &str) {
    if article_url.is_empty() { return; }
    api_post("/api/deep-news/ban", &serde_json::json!({ "ticker": ticker, "isin": isin, "url": article_url, "reason": reason }));
}

fn urlenc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || "-._~".contains(c) => c.to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}

fn map_api_error(e: ureq::Error) -> anyhow::Error {
    match &e {
        ureq::Error::Status(401, _) => anyhow!("alfred_api_unauthorized"),
        ureq::Error::Status(429, _) => anyhow!("alfred_api_rate_limited"),
        ureq::Error::Status(code, _) => anyhow!("alfred_api_http_error:{code}"),
        _ => anyhow!("alfred_api_request_failed:{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn technicals_path_without_isin_keeps_ticker_only_shape() {
        // Existing callers (US tickers, watchlist entries without ISIN) must
        // continue to produce the original wire path so cached server-side
        // responses keyed by URL don't fragment.
        let p = build_technicals_path("AAPL", None);
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_with_isin_appends_isin_param() {
        // The whole point of v0.2.18: French small/mid caps need ISIN so the
        // server can route to the EU OHLC chain (Yahoo with `.PA` suffix).
        let p = build_technicals_path("EXA", Some("FR0010163345"));
        assert_eq!(p, "/api/market/technicals?ticker=EXA&isin=FR0010163345");
    }

    #[test]
    fn technicals_path_empty_isin_treated_as_absent() {
        // Position::isin is Option<String>; an `Some("")` slips through
        // serde for legacy rows. Strip it so we don't ship `isin=` (which
        // some axum query parsers reject) — match the trim-and-empty rule
        // used by the server handlers (handlers.rs:OhlcParams).
        let p = build_technicals_path("AAPL", Some(""));
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
        let p2 = build_technicals_path("AAPL", Some("   "));
        assert_eq!(p2, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_url_encodes_both_params() {
        // Defensive: ISIN is always alphanumeric, but ticker fields from CSVs
        // can carry surprising characters. Confirm urlenc is applied to both.
        let p = build_technicals_path("A B", Some("FR 1234"));
        assert_eq!(p, "/api/market/technicals?ticker=A+B&isin=FR+1234");
    }
}
