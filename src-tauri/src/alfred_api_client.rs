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
///
/// When `canonical` is `Some(symbol)`, the server uses it for upstream news
/// queries (Yahoo finance, news aggregators) instead of the broker `ticker` —
/// this is how a French PEA `STMPA` gets routed to the same news bucket as a
/// CTO `STM.MI`. Server falls back to `ticker` when absent (backward compat).
pub fn remote_fetch_news(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/news?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
}

/// Fetch shared insights for a ticker from the API cache.
pub fn remote_fetch_insights(ticker: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/insights?ticker={}&isin={}", urlenc(ticker), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch sector classification for a ticker from the API.
///
/// `canonical` lets the server route on the resolved Yahoo symbol so PEA
/// vs CTO duplicates of the same security share a single sector lookup. See
/// `remote_fetch_news` for the full rationale.
pub fn remote_fetch_sector(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/sector?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
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
pub fn remote_fetch_technicals(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> Result<Value> {
    api_get(&build_technicals_path(ticker, isin, canonical), TIMEOUT_SECS)
}

/// Build the `/api/market/technicals` query string for `(ticker, isin, canonical)`.
/// Extracted so the URL contract is unit-testable without an HTTP round trip.
///
/// The `isin` argument is included only when present and non-empty after
/// trimming — empty strings degrade gracefully to a ticker-only query so
/// existing callers without ISIN keep their previous wire shape. `canonical`
/// follows the same trim-and-empty rule and is appended last.
fn build_technicals_path(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> String {
    let trimmed_isin = isin.map(str::trim).filter(|s| !s.is_empty());
    let base = match trimmed_isin {
        Some(code) => format!(
            "/api/market/technicals?ticker={}&isin={}",
            urlenc(ticker),
            urlenc(code),
        ),
        None => format!("/api/market/technicals?ticker={}", urlenc(ticker)),
    };
    append_canonical(base, canonical)
}

/// Fetch COT data for a ticker from the API.
///
/// `canonical` lets the server use the resolved Yahoo symbol for upstream
/// COT lookups — see `remote_fetch_news` for the full rationale.
pub fn remote_fetch_cot(ticker: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/cot?ticker={}&isin={}", urlenc(ticker), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
}

/// Append `&canonical=<symbol>` to a query path when a non-empty resolved
/// Yahoo symbol is present. Centralised so the trim/empty-rejection rule is
/// applied identically across every endpoint that accepts canonical routing
/// — keeps the v0.3 parity contract honest: all 3 LLM modes see the same
/// canonical enrichment data (`product_llm_mode_parity_2026_04`).
fn append_canonical(path: String, canonical: Option<&str>) -> String {
    match canonical.map(str::trim).filter(|s| !s.is_empty()) {
        Some(symbol) => format!("{path}&canonical={}", urlenc(symbol)),
        None => path,
    }
}

/// Resolve a canonical Yahoo symbol for the given ISIN via
/// `GET /api/resolve?isin=X` (deployed in server-side v0.3 — feat/v0.3-server-bundle).
///
/// Returns `Ok(Some(symbol))` when the server returns a non-null symbol
/// (`source = "yahoo_search"`), `Ok(None)` when the resolver returned no match
/// (`source = "none"` or `symbol = null`), and `Err` on transport/auth errors
/// so the caller can decide whether to log or swallow.
///
/// Server-side cache: 180 days positive, 15s negative — desktop callers do not
/// need their own caching. Endpoint may not be deployed on older servers, so
/// callers must degrade gracefully (treat error as "no resolution available").
pub fn remote_fetch_resolve(isin: &str) -> Result<Option<String>> {
    let resp = api_get(&build_resolve_path(isin), TIMEOUT_SECS)?;
    // Source = "none" means the resolver searched but found nothing — treat
    // identically to a null symbol so callers don't store the sentinel.
    let source = resp.get("source").and_then(|v| v.as_str()).unwrap_or("");
    if source == "none" {
        return Ok(None);
    }
    let symbol = resp
        .get("symbol")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(symbol)
}

/// Build the `/api/resolve` query string for a given ISIN. Extracted so the
/// URL contract is unit-testable without a live HTTP round trip and so the
/// trim/empty-rejection rule is colocated with the other path builders.
fn build_resolve_path(isin: &str) -> String {
    let code = isin.trim();
    format!("/api/resolve?isin={}", urlenc(code))
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
        let p = build_technicals_path("AAPL", None, None);
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_with_isin_appends_isin_param() {
        // The whole point of v0.2.18: French small/mid caps need ISIN so the
        // server can route to the EU OHLC chain (Yahoo with `.PA` suffix).
        let p = build_technicals_path("EXA", Some("FR0010163345"), None);
        assert_eq!(p, "/api/market/technicals?ticker=EXA&isin=FR0010163345");
    }

    #[test]
    fn technicals_path_empty_isin_treated_as_absent() {
        // Position::isin is Option<String>; an `Some("")` slips through
        // serde for legacy rows. Strip it so we don't ship `isin=` (which
        // some axum query parsers reject) — match the trim-and-empty rule
        // used by the server handlers (handlers.rs:OhlcParams).
        let p = build_technicals_path("AAPL", Some(""), None);
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
        let p2 = build_technicals_path("AAPL", Some("   "), None);
        assert_eq!(p2, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_url_encodes_both_params() {
        // Defensive: ISIN is always alphanumeric, but ticker fields from CSVs
        // can carry surprising characters. Confirm urlenc is applied to both.
        let p = build_technicals_path("A B", Some("FR 1234"), None);
        assert_eq!(p, "/api/market/technicals?ticker=A+B&isin=FR+1234");
    }

    // ── &canonical= passthrough (v0.3) ──────────────────────────────

    #[test]
    fn technicals_path_with_canonical_appends_after_isin() {
        // v0.3 parity: when the desktop has resolved the canonical Yahoo
        // symbol, the server must route on `canonical=` instead of the raw
        // ticker. The query order is deterministic (ticker, isin, canonical)
        // so URL-keyed caches stay stable across runs.
        let p = build_technicals_path("STMPA", Some("NL0000226223"), Some("STMPA.PA"));
        assert_eq!(
            p,
            "/api/market/technicals?ticker=STMPA&isin=NL0000226223&canonical=STMPA.PA"
        );
    }

    #[test]
    fn technicals_path_canonical_without_isin_still_appended() {
        // Watchlist entries can have a resolved symbol from earlier resolve
        // calls but no ISIN at all. The canonical param must still pass
        // through so the server routes on it.
        let p = build_technicals_path("STMPA", None, Some("STMPA.PA"));
        assert_eq!(p, "/api/market/technicals?ticker=STMPA&canonical=STMPA.PA");
    }

    #[test]
    fn technicals_path_canonical_empty_treated_as_absent() {
        // `Position::resolved_symbol` is Option<String>; an empty/whitespace
        // string slipped in by serde must NOT emit `canonical=` (server
        // would treat it as a query for the empty symbol).
        let p = build_technicals_path("AAPL", None, Some(""));
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
        let p2 = build_technicals_path("AAPL", None, Some("   "));
        assert_eq!(p2, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn append_canonical_idempotent_when_absent() {
        // append_canonical is the single helper applied by every endpoint
        // (news/sector/cot/technicals/market). Verify the no-op branch never
        // mutates the path so legacy server caches keyed by URL don't
        // fragment when resolver is disabled.
        let base = "/api/news?ticker=AAPL&name=Apple&isin=US0378331005".to_string();
        let out = append_canonical(base.clone(), None);
        assert_eq!(out, base);
    }

    #[test]
    fn append_canonical_url_encodes_symbol() {
        // Yahoo suffixes are dotted (`.PA`, `.HK`) and dots are urlenc-safe,
        // but exotic suffixes or whitespace must still round-trip cleanly.
        let p = append_canonical("/api/sector?ticker=X".to_string(), Some("X Y"));
        assert_eq!(p, "/api/sector?ticker=X&canonical=X+Y");
    }

    // ── /api/resolve path builder (v0.3) ────────────────────────────

    #[test]
    fn resolve_path_emits_canonical_isin_query() {
        // Canonical shape the server-side resolver expects — must mirror
        // `feat/v0.3-server-bundle` commit 877f18f.
        let p = build_resolve_path("NL0000226223");
        assert_eq!(p, "/api/resolve?isin=NL0000226223");
    }

    #[test]
    fn resolve_path_trims_whitespace_before_encoding() {
        // ISIN slips through with leading/trailing whitespace from CSV imports.
        // Trim BEFORE urlenc so we emit the canonical shape — otherwise we'd
        // ship `%20FR0010163345%20` and split the server-side 180d positive
        // cache key across whitespace variants of the same instrument.
        let p = build_resolve_path("  FR0010163345  ");
        assert_eq!(p, "/api/resolve?isin=FR0010163345");
    }

    #[test]
    fn resolve_path_url_encodes_unexpected_chars() {
        // Defensive: malformed ISINs from broker CSVs must round-trip safely
        // — never crash, never emit a raw `+` or space into the query string.
        let p = build_resolve_path("FR 12+34");
        assert_eq!(p, "/api/resolve?isin=FR+12%2B34");
    }
}
