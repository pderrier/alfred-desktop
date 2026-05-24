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

/// Fetch the macro briefing (US 10Y / VIX / EUR-USD / Brent) once at run
/// start. P1-60.
///
/// Silent degradation on any transport / auth / 5xx failure — mirrors the
/// `fetch_sector` / `fetch_cot` pattern. The caller persists the result
/// (whether populated or null) into `run_state.macro_briefing`; the
/// synthesis-prompt renderer treats a null briefing as "no macro section",
/// which is the safe display behaviour the LLM understands.
pub fn fetch_macro_briefing() -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_macro_briefing() {
        Ok(resp) => Ok(resp),
        Err(e) => {
            crate::debug_log(&format!("enrichment macro briefing unavailable: {e}"));
            Ok(json!({ "ok": true, "macro": null }))
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
///
/// In tests, the network round trip can be overridden via
/// `set_resolve_mock` — the workflow under test then exercises every
/// downstream code path without hitting `/api/resolve`. This mirrors the
/// `set_codex_mock` seam in `codex.rs`.
pub fn fetch_resolved_symbol(isin: &str) -> Option<String> {
    let code = isin.trim();
    if code.is_empty() {
        return None;
    }
    if let Some(slot) = RESOLVE_MOCK.get() {
        if let Ok(guard) = slot.lock() {
            if let Some(mock_fn) = *guard {
                return mock_fn(code);
            }
        }
    }
    match crate::alfred_api_client::remote_fetch_resolve(code) {
        Ok(symbol) => symbol,
        Err(e) => {
            crate::debug_log(&format!("enrichment resolve unavailable for {code}: {e}"));
            None
        }
    }
}

/// Test-only mock: when set, `fetch_resolved_symbol` calls this instead of
/// `/api/resolve`. Thread-safe static function pointer, mirroring
/// `codex::set_codex_mock`.
pub type ResolveMockFn = fn(&str) -> Option<String>;
static RESOLVE_MOCK: std::sync::OnceLock<std::sync::Mutex<Option<ResolveMockFn>>> =
    std::sync::OnceLock::new();

/// Install (or clear with `None`) the resolver mock. Tests should call this
/// inside an `env_lock()` guard to keep parallel test threads from racing.
#[allow(dead_code)] // called from #[cfg(test)] code paths only
pub fn set_resolve_mock(mock: Option<ResolveMockFn>) {
    let slot = RESOLVE_MOCK.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = mock;
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
///
/// v0.3.2 (P0-1 + P0-7): retry-with-backoff added on top of instrumentation.
/// Pre-fix diagnostic: 18/18 serial probes against the missing tickers of
/// run `019e2c9de5ee` returned HTTP 200 + valid indicators (`quality=fresh`).
/// Conclusion: the 10/28 partial coverage is transient failure under
/// parallel load (Yahoo rate-limit, network blip, 5xx), NOT a contract
/// issue. Retry budget: 3 attempts (1 initial + 2 retries), backoff 500ms
/// then 1500ms — total worst-case latency 2s per ticker on full failure.
/// Retry triggers ONLY on transport `Err` — a 200 response with invalid
/// indicators is a server verdict ("unavailable") and must NOT trigger
/// retry (would waste rate budget on a no-data marker that won't change).
///
/// Per-ticker visibility (P0-7) is wired in the same path: each retry and
/// each final failure emits `alfred://ticker-collection-event` so the
/// frontend aggregator (worktree v032-D) can render a live counter without
/// having to parse the debug log. Payload schema:
///   - `{ ticker, kind: "retry",   attempt: 1|2,   reason: "<err>" }`
///   - `{ ticker, kind: "failure", attempts: 3,    reason: "<err>" }`
///   - `{ ticker, kind: "success", attempts: 2|3 }`  (only after a retry —
///     first-try success emits NO event to keep the happy path silent;
///     worktree D uses this to decrement the per-ticker "retrying" badge.)
pub fn fetch_technical_snapshot(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> Option<Value> {
    fetch_technical_snapshot_with_retry(
        ticker,
        isin,
        canonical,
        &|t, i, c| crate::alfred_api_client::remote_fetch_technicals(t, i, c),
        &default_sleep,
    )
}

/// Retry-with-backoff policy constants. See `fetch_technical_snapshot`
/// docstring for the rationale (parallel-load transients, not contract drift).
pub(crate) const TECHNICAL_SNAPSHOT_MAX_ATTEMPTS: u32 = 3;
const TECHNICAL_SNAPSHOT_BACKOFFS: [std::time::Duration; 2] = [
    std::time::Duration::from_millis(500),
    std::time::Duration::from_millis(1500),
];

fn default_sleep(d: std::time::Duration) {
    std::thread::sleep(d);
}

/// Pure helper extracted from `fetch_technical_snapshot` so the retry loop
/// (decision tree: Err → retry, Ok-but-unusable → terminate, Ok-good → done)
/// can be unit-tested without an HTTP runtime and without a sleep budget.
///
/// `fetcher` is the underlying transport (real path: `remote_fetch_technicals`).
/// `sleeper` is the backoff implementation (real path: `std::thread::sleep`;
/// tests pass a no-op). Both are passed as references so callers don't pay
/// generic monomorphisation cost in release builds.
///
/// Follows the `feedback_pure_helper_for_async_testability` pattern: the
/// production wrapper does no logic beyond binding the two injection points.
pub(crate) fn fetch_technical_snapshot_with_retry<F, S>(
    ticker: &str,
    isin: Option<&str>,
    canonical: Option<&str>,
    fetcher: &F,
    sleeper: &S,
) -> Option<Value>
where
    F: Fn(&str, Option<&str>, Option<&str>) -> Result<Value>,
    S: Fn(std::time::Duration),
{
    let mut last_reason: Option<String> = None;
    for attempt in 1..=TECHNICAL_SNAPSHOT_MAX_ATTEMPTS {
        match fetcher(ticker, isin, canonical) {
            Ok(resp) => {
                if let Some(snapshot) = parse_technical_snapshot_response(&resp) {
                    let samples = snapshot
                        .get("samples")
                        .and_then(|v| v.as_i64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    crate::debug_log(&format!(
                        "enrichment technical_snapshot ok for {ticker} (samples={samples}, attempt={attempt})"
                    ));
                    // Emit a `success` event ONLY when a retry was required
                    // — happy path (attempt == 1) stays silent so worktree D
                    // doesn't get 28 noise events per run.
                    if attempt > 1 {
                        emit_ticker_collection_event(json!({
                            "ticker": ticker,
                            "kind": "success",
                            "attempts": attempt as i64,
                        }));
                    }
                    return Some(snapshot);
                }
                // 200 with unusable body = server verdict "unavailable".
                // NOT a transient — do not retry; do not emit a retry event.
                // (A failure event is also not emitted because this is the
                // expected steady-state for tickers the server can't cover;
                // it's already logged below for diagnosis.)
                crate::debug_log(&format!(
                    "enrichment technical_snapshot dropped for {ticker} — server response had no usable `indicators` object (body keys: {:?}, attempt={attempt})",
                    resp.as_object()
                        .map(|m| m.keys().cloned().collect::<Vec<_>>())
                        .unwrap_or_default(),
                ));
                return None;
            }
            Err(e) => {
                let reason = e.to_string();
                last_reason = Some(reason.clone());
                let is_last_attempt = attempt >= TECHNICAL_SNAPSHOT_MAX_ATTEMPTS;
                if is_last_attempt {
                    crate::debug_log(&format!(
                        "enrichment technical_snapshot transport error for {ticker} (isin={isin:?}, canonical={canonical:?}, attempt={attempt}/final): {reason}"
                    ));
                } else {
                    crate::debug_log(&format!(
                        "enrichment technical_snapshot transport error for {ticker} (isin={isin:?}, canonical={canonical:?}, attempt={attempt}, will retry): {reason}"
                    ));
                    emit_ticker_collection_event(json!({
                        "ticker": ticker,
                        "kind": "retry",
                        "attempt": attempt as i64,
                        "reason": reason,
                    }));
                    // Backoff index is attempt-1: attempt 1 → 500ms, attempt 2 → 1500ms.
                    if let Some(delay) = TECHNICAL_SNAPSHOT_BACKOFFS.get((attempt - 1) as usize) {
                        sleeper(*delay);
                    }
                }
            }
        }
    }
    // All attempts exhausted with Err — emit the failure event.
    emit_ticker_collection_event(json!({
        "ticker": ticker,
        "kind": "failure",
        "attempts": TECHNICAL_SNAPSHOT_MAX_ATTEMPTS as i64,
        "reason": last_reason.unwrap_or_else(|| "unknown".to_string()),
    }));
    None
}

fn emit_ticker_collection_event(payload: Value) {
    crate::emit_event("alfred://ticker-collection-event", payload);
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
