//! Client for the remote Alfred API server.
//!
//! Calls /api/market, /api/news, /api/search on the remote server.
//! Auth: HMAC-signed requests — the API secret is embedded at compile time
//! (via CI), never exposed in source. The OpenAI JWT never leaves the device.

use std::env;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_API_URL: &str = "https://vps-c5793aab.vps.ovh.net/alfred/api";
const TIMEOUT_SECS: u64 = 10;
/// Timeout for `POST /run/start`. Short on purpose — if the session
/// endpoint isn't responding in 8s, the desktop should fail loudly so
/// the user knows the API is down before they're sunk into a 10-min run.
const RUN_START_TIMEOUT_SECS: u64 = 8;

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
///
/// v0.4.0 (P0-11): also injects `X-Run-Session: <id>` when a run-session
/// has been set via `set_active_run_session` (done by
/// `analysis_ops::start_analysis` right after `/run/start` succeeds). The
/// 14 gated analysis endpoints require this header server-side; without it
/// they 401 with `run_session_invalid`. `/run/start` itself does NOT
/// require the header (it's the source of sessions) — the active-session
/// context is `None` when this function runs for that endpoint.
fn apply_auth(req: ureq::Request, path: &str) -> ureq::Request {
    let ts = now_epoch_secs();
    let client_hash = get_client_hash().unwrap_or_default();
    // Sign only the path component (no query string) — must match server-side req.uri().path()
    let sign_path = path.split('?').next().unwrap_or(path);
    let mut req = req
        .set("X-Client-Hash", &client_hash)
        .set("X-Timestamp", &ts.to_string());
    if let Some(session_id) = active_run_session() {
        req = req.set("X-Run-Session", &session_id);
    }
    let runtime_secret = env::var("ALFRED_API_SECRET").ok();
    let secret = API_SECRET.or(runtime_secret.as_deref());
    if let Some(s) = secret {
        let sig = hmac_sign(sign_path, ts, s);
        req.set("X-Signature", &sig)
    } else {
        req
    }
}

// ── Run-session context (v0.4.0 P0-11) ──────────────────────────────
//
// The desktop holds a single, process-wide "active run-session" slot.
// `analysis_ops::start_analysis` calls `set_active_run_session` after a
// successful `POST /run/start`; the worker thread inherits this context
// (all `api_get` / `api_post` calls on that thread will inject the header
// via `apply_auth`). At run completion, `clear_active_run_session`
// resets the slot.
//
// Why a global instead of explicit threading? The enrichment helpers
// (`enrichment::fetch_market`, `remote_fetch_news`, etc.) are called from
// 20+ call sites and pass `Position` / `Ticker` types — adding an
// optional `session_id` everywhere would be a huge surface change for no
// runtime benefit. The desktop runs ONE analysis at a time (verified by
// the cancellation registry and ops_store invariants — see
// `analysis_ops::ops_store`), so a single global slot is correct.

static ACTIVE_RUN_SESSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<String>> {
    ACTIVE_RUN_SESSION.get_or_init(|| Mutex::new(None))
}

/// Set the active run-session ID. Called by `analysis_ops::start_analysis`
/// after the server has issued one via `POST /run/start`.
///
/// Mutex-protected to be safe under the (rare) case where the cancellation
/// thread races the worker thread on completion. Replaces any prior value
/// — the contract is "one run, one session", and a fresh `/run/start`
/// always supersedes a stale slot.
pub fn set_active_run_session(session_id: impl Into<String>) {
    if let Ok(mut slot) = session_slot().lock() {
        *slot = Some(session_id.into());
    }
}

/// Clear the active run-session ID. Called on run completion / failure /
/// cancellation so a subsequent `api_get` outside of a run does NOT
/// silently use a stale session header (which would 401 with confusing
/// `run_session_invalid` instead of the cleaner "no session" path).
pub fn clear_active_run_session() {
    if let Ok(mut slot) = session_slot().lock() {
        *slot = None;
    }
}

/// Read the active run-session ID (cloned to avoid holding the lock
/// across the HTTP call). Returns `None` when no run is in flight.
pub fn active_run_session() -> Option<String> {
    session_slot().lock().ok().and_then(|s| s.clone())
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

/// Outcome of a single HTTP attempt. Separates 429 from other failures so
/// the retry loop can read `Retry-After` and back off intelligently.
///
/// v0.3.3 (P0-9): introduced to handle alfred-api 429 responses (the P0-1
/// retry-with-backoff only covers Yahoo `/technicals` and treats every
/// transport error identically). Production incident `019e2da21154`
/// (2026-05-15) showed a 28-ticker run firing 34 `alfred_api_rate_limited`
/// errors at run start; the desktop client must respect the server's
/// `Retry-After` instead of surfacing the error to the user.
pub(crate) enum ApiGetOutcome {
    Ok(Value),
    /// HTTP 429 — caller should sleep then retry. `retry_after_secs` is
    /// the parsed `Retry-After` header value (when present + valid).
    RateLimited { retry_after_secs: Option<u64> },
    /// Any other error (4xx, 5xx, parse failure, transport failure) —
    /// not retried, surfaced immediately.
    Err(anyhow::Error),
}

/// Retry-on-429 policy. 1 initial attempt + up to 3 retries = 4 calls max.
///
/// Backoff schedule (used when the server does NOT send `Retry-After`):
/// 500ms → 1500ms → 4500ms (×3 multiplier). Total wait worst case ~6.5s
/// before surfacing the error. The server-side `apply_rate_limit` ticks
/// in 1-minute buckets, so a 60s `Retry-After` is the natural ceiling
/// when the header is honoured.
pub(crate) const ALFRED_API_MAX_RETRIES_ON_429: u32 = 3;
const ALFRED_API_429_BACKOFFS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_millis(4500),
];

fn default_sleep(d: Duration) {
    std::thread::sleep(d);
}

/// One-shot HTTP attempt — surfaces 429 with parsed `Retry-After` header
/// so the retry layer can choose its sleep budget. All other outcomes
/// collapse into the existing `map_api_error` classification.
fn api_get_once(path: &str, timeout: u64) -> ApiGetOutcome {
    let base = match api_url() {
        Some(u) => u,
        None => return ApiGetOutcome::Err(anyhow!("alfred_api_not_configured")),
    };
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::get(&url), path).timeout(Duration::from_secs(timeout));
    match req.call() {
        Ok(resp) => match resp.into_json() {
            Ok(value) => ApiGetOutcome::Ok(value),
            Err(e) => ApiGetOutcome::Err(anyhow!("alfred_api_parse_failed:{e}")),
        },
        Err(ureq::Error::Status(429, resp)) => {
            let retry_after_secs = resp
                .header("Retry-After")
                .and_then(|v| v.trim().parse::<u64>().ok());
            ApiGetOutcome::RateLimited { retry_after_secs }
        }
        Err(e) => ApiGetOutcome::Err(map_api_error(e)),
    }
}

/// Pure retry loop — fetcher + sleeper injected so the policy
/// (max retries, backoff schedule, `Retry-After` honouring) is fully
/// unit-testable without an HTTP runtime. See `tests::alfred_api_client_*`
/// in `tests.rs`.
///
/// Decision tree:
/// - `Ok(value)` → return immediately
/// - `RateLimited { retry_after_secs }` → if budget remains, sleep
///   `retry_after_secs` (clamped to a sane cap) or the next backoff
///   slot, then retry. If exhausted, surface `alfred_api_rate_limited`.
/// - `Err(e)` → surface immediately (don't retry on auth / server /
///   parse errors — those are not transient).
///
/// The `Retry-After` clamp caps at 30s so a hostile server (or a clock
/// skew) can't stall the desktop client indefinitely; beyond that we
/// fall back to the local backoff schedule.
pub(crate) fn api_get_with_retry<F, S>(fetcher: &F, sleeper: &S) -> Result<Value>
where
    F: Fn() -> ApiGetOutcome,
    S: Fn(Duration),
{
    const RETRY_AFTER_CLAMP: Duration = Duration::from_secs(30);
    let mut attempt: u32 = 0;
    loop {
        match fetcher() {
            ApiGetOutcome::Ok(value) => return Ok(value),
            ApiGetOutcome::Err(e) => return Err(e),
            ApiGetOutcome::RateLimited { retry_after_secs } => {
                if attempt >= ALFRED_API_MAX_RETRIES_ON_429 {
                    // Exhausted retry budget — surface the classified error
                    // so existing callers (`enrichment::classify_api_error`
                    // and friends) keep their wire shape.
                    return Err(anyhow!("alfred_api_rate_limited"));
                }
                let backoff = retry_after_secs
                    .map(|s| Duration::from_secs(s).min(RETRY_AFTER_CLAMP))
                    .unwrap_or(ALFRED_API_429_BACKOFFS[attempt as usize]);
                crate::debug_log(&format!(
                    "alfred-api: 429 received (attempt {}/{}), sleeping {}ms before retry (retry_after_header={:?})",
                    attempt + 1,
                    ALFRED_API_MAX_RETRIES_ON_429 + 1,
                    backoff.as_millis(),
                    retry_after_secs,
                ));
                sleeper(backoff);
                attempt += 1;
            }
        }
    }
}

/// Authenticated GET request to the API.
///
/// v0.3.3 (P0-9): now retries on HTTP 429 with `Retry-After` honoured.
/// All other errors (auth, 5xx, transport) surface on the first attempt
/// — only rate-limit errors are transient by design.
fn api_get(path: &str, timeout: u64) -> Result<Value> {
    let fetcher = || api_get_once(path, timeout);
    api_get_with_retry(&fetcher, &default_sleep)
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

// ── /run/start client (v0.4.0 P0-11) ─────────────────────────────────

/// Result of calling `POST /run/start`. Two structured outcomes for the
/// caller to render differently:
/// - `Ok` → caller stores `session_id`, proceeds with the run.
/// - `QuotaExhausted` → caller surfaces the upgrade modal (P0-13 in a
///   separate worktree); the run NEVER starts.
/// - `Err` → unrelated transport/auth failure; caller fails the run with
///   the usual error path.
///
/// The 3-way shape matters because "quota exhausted" is a perfectly
/// understandable user state (you've done 3 analyses this week), not a
/// runtime error — collapsing it into `Err` would lose the structured
/// fields the UI needs to render the upgrade CTA.
#[derive(Debug)]
pub enum RunStartOutcome {
    /// `/run/start` succeeded — session is valid for `expires_at` seconds.
    Ok(RunStartSuccess),
    /// 429 `free_tier_exhausted` — surface upgrade CTA, do NOT start the run.
    QuotaExhausted(QuotaExhaustedInfo),
}

#[derive(Debug, Clone)]
pub struct RunStartSuccess {
    pub run_session_id: String,
    pub expires_at: u64,
    pub runs_this_week: u32,
    /// `None` for paid tier (the server returns the string "unlimited").
    /// Allowed-dead: consumed by P0-13 modal (separate worktree W-B)
    /// when it renders the "X of Y runs used" line. We log it from
    /// `acquire_run_session_or_bail` for diagnostics but don't surface
    /// it via the bridge yet — keeping the field on the struct pins the
    /// wire contract so W-B doesn't have to re-parse the JSON.
    #[allow(dead_code)]
    pub limit: Option<u32>,
    /// `"free"` or `"paid"`. The desktop renders different UI based on this.
    pub tier: String,
}

#[derive(Debug, Clone)]
pub struct QuotaExhaustedInfo {
    pub retry_after: u64,
    /// Surfaced today via the structured `alfred_free_tier_exhausted:…`
    /// error code (see `analysis_ops::acquire_run_session_or_bail`) — the
    /// JS upgrade modal (P0-13, separate worktree W-B) reads it back from
    /// the message body for display. Allowed-dead until W-B lands.
    #[allow(dead_code)]
    pub runs_this_week: u32,
    pub limit: u32,
    /// Always `"rolling_7d"` in v0.4.0; pinned so the UI can format the
    /// reset hint correctly ("rolling 7 days").
    pub period: String,
}

/// Issue a run-session via `POST /run/start`. Decrements the server-side
/// rolling-7d quota. Caller is responsible for storing the returned
/// `run_session_id` via `set_active_run_session` so downstream `api_get`
/// calls inject the header automatically.
///
/// Errors:
/// - `Ok(QuotaExhausted)` for HTTP 429 `free_tier_exhausted` (graceful UX path).
/// - `Err(_)` for transport / auth / parse failures (treated as run-start failure).
///
/// Why not retry on 429? Unlike rate-limit 429s, a free-tier exhaustion
/// won't resolve in seconds — the user has to wait for the rolling window
/// or upgrade. Retrying would just consume the user's quota again the
/// moment it frees up (a hostile pattern). Surface the structured info
/// to the UI instead.
pub fn start_run_session() -> Result<RunStartOutcome> {
    let base = match api_url() {
        Some(u) => u,
        None => return Err(anyhow!("alfred_api_not_configured")),
    };
    let path = "/run/start";
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(RUN_START_TIMEOUT_SECS));

    let response = req.send_string("{}");
    parse_run_start_response(response)
}

/// Pure-ish helper: classify the `/run/start` response into a
/// `RunStartOutcome`. Split from `start_run_session` so the classification
/// logic is unit-testable (mocked `ureq::Response` shapes).
fn parse_run_start_response(
    result: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<RunStartOutcome> {
    match result {
        Ok(resp) => {
            let body: Value = resp
                .into_json()
                .map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))?;
            classify_run_start_ok(body)
        }
        Err(ureq::Error::Status(429, resp)) => {
            let body: Value = resp.into_json().unwrap_or(Value::Null);
            classify_run_start_429(body)
        }
        Err(e) => Err(map_api_error(e)),
    }
}

fn classify_run_start_ok(body: Value) -> Result<RunStartOutcome> {
    let run_session_id = body
        .get("run_session_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("alfred_api_parse_failed:missing run_session_id"))?;
    let expires_at = body
        .get("expires_at")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("alfred_api_parse_failed:missing expires_at"))?;
    let runs_this_week = body
        .get("runs_this_week")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    // `limit` is integer for Free, the string "unlimited" for Paid (the
    // server caps at u32::MAX which we'd never want to display as-is).
    let limit = body.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
    let tier = body
        .get("tier")
        .and_then(|v| v.as_str())
        .unwrap_or("free")
        .to_string();
    Ok(RunStartOutcome::Ok(RunStartSuccess {
        run_session_id,
        expires_at,
        runs_this_week,
        limit,
        tier,
    }))
}

fn classify_run_start_429(body: Value) -> Result<RunStartOutcome> {
    // The server always sends `error: "free_tier_exhausted"` for 429 on
    // /run/start. If it ever returns the generic `rate_limited` here
    // (e.g. global per-IP throttle), treat that as a transport-level
    // 429 and surface to the caller via Err so the existing rate-limit
    // retry path handles it.
    let error_code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if error_code != "free_tier_exhausted" {
        return Err(anyhow!("alfred_api_rate_limited"));
    }
    let retry_after = body
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let runs_this_week = body
        .get("runs_this_week")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    let period = body
        .get("period")
        .and_then(|v| v.as_str())
        .unwrap_or("rolling_7d")
        .to_string();
    Ok(RunStartOutcome::QuotaExhausted(QuotaExhaustedInfo {
        retry_after,
        runs_this_week,
        limit,
        period,
    }))
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

    // ── X-Run-Session propagation (v0.4.0 P0-11) ────────────────────

    /// Sequential lock guard so the set/clear tests don't race each other
    /// (they share a process-global slot — see ACTIVE_RUN_SESSION).
    fn run_session_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn active_run_session_returns_none_when_unset() {
        let _guard = run_session_test_lock();
        clear_active_run_session();
        assert!(active_run_session().is_none());
    }

    #[test]
    fn set_active_run_session_round_trips_through_active_run_session() {
        let _guard = run_session_test_lock();
        clear_active_run_session();
        set_active_run_session("abc123");
        assert_eq!(active_run_session().as_deref(), Some("abc123"));
        clear_active_run_session();
    }

    #[test]
    fn clear_active_run_session_resets_slot() {
        let _guard = run_session_test_lock();
        set_active_run_session("xyz789");
        assert!(active_run_session().is_some());
        clear_active_run_session();
        assert!(active_run_session().is_none());
    }

    #[test]
    fn set_active_run_session_overwrites_previous_value() {
        // Contract: one run, one session — if start_analysis fires a
        // second /run/start (which would be a bug, but defend anyway),
        // the new session must supersede the stale one rather than
        // being silently ignored.
        let _guard = run_session_test_lock();
        clear_active_run_session();
        set_active_run_session("first");
        set_active_run_session("second");
        assert_eq!(active_run_session().as_deref(), Some("second"));
        clear_active_run_session();
    }

    #[test]
    fn apply_auth_injects_run_session_header_when_set() {
        // Smoke-test apply_auth's contract: when a session is set, the
        // resulting ureq::Request carries X-Run-Session. Built using
        // a dummy base URL since ureq::get accepts arbitrary strings
        // and apply_auth doesn't fire the request.
        let _guard = run_session_test_lock();
        clear_active_run_session();
        set_active_run_session("test-session-id");

        let raw = ureq::get("https://example.test/api/news");
        let signed = apply_auth(raw, "/api/news");
        // ureq::Request doesn't expose headers via a public iterator,
        // but the request can be converted back to a fmt::Debug shape
        // that includes them. Cheap proxy: compare the Debug output.
        let dbg = format!("{signed:?}");
        assert!(
            dbg.contains("X-Run-Session") || dbg.contains("x-run-session"),
            "X-Run-Session header missing from signed request — apply_auth contract broken. Debug: {dbg}",
        );

        clear_active_run_session();
    }

    #[test]
    fn apply_auth_omits_run_session_header_when_unset() {
        // /run/start itself runs through apply_auth with no active
        // session — must NOT inject a stale X-Run-Session.
        let _guard = run_session_test_lock();
        clear_active_run_session();

        let raw = ureq::get("https://example.test/run/start");
        let signed = apply_auth(raw, "/run/start");
        let dbg = format!("{signed:?}");
        assert!(
            !dbg.to_lowercase().contains("x-run-session"),
            "X-Run-Session must not be injected when no session is active — request would 401",
        );
    }

    // ── /run/start response classification (v0.4.0 P0-11) ───────────

    #[test]
    fn classify_run_start_ok_parses_success_envelope() {
        let body = serde_json::json!({
            "ok": true,
            "run_session_id": "abcd1234deadbeef00000000aabbccdd",
            "expires_at": 1_700_000_600u64,
            "runs_this_week": 1u64,
            "limit": 3u64,
            "period": "rolling_7d",
            "tier": "free",
        });
        let outcome = classify_run_start_ok(body).unwrap();
        match outcome {
            RunStartOutcome::Ok(s) => {
                assert_eq!(s.run_session_id, "abcd1234deadbeef00000000aabbccdd");
                assert_eq!(s.expires_at, 1_700_000_600);
                assert_eq!(s.runs_this_week, 1);
                assert_eq!(s.limit, Some(3));
                assert_eq!(s.tier, "free");
            }
            _ => panic!("expected Ok outcome"),
        }
    }

    #[test]
    fn classify_run_start_ok_handles_paid_unlimited_limit() {
        // Paid users get `limit: "unlimited"` (string sentinel), not a
        // numeric value. The Rust client maps that to Option::None so
        // the UI doesn't render "u32::MAX runs/week".
        let body = serde_json::json!({
            "ok": true,
            "run_session_id": "deadbeefdeadbeefdeadbeefdeadbeef",
            "expires_at": 1_700_000_600u64,
            "runs_this_week": 5u64,
            "limit": "unlimited",
            "period": "rolling_7d",
            "tier": "paid",
        });
        let outcome = classify_run_start_ok(body).unwrap();
        match outcome {
            RunStartOutcome::Ok(s) => {
                assert_eq!(s.tier, "paid");
                assert!(s.limit.is_none());
            }
            _ => panic!("expected Ok outcome"),
        }
    }

    #[test]
    fn classify_run_start_ok_rejects_missing_session_id() {
        let body = serde_json::json!({"expires_at": 1_700_000_600u64});
        let err = classify_run_start_ok(body).expect_err("missing session_id must error");
        assert!(err.to_string().contains("missing run_session_id"));
    }

    #[test]
    fn classify_run_start_429_parses_free_tier_exhausted_envelope() {
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 86400u64,
            "runs_this_week": 3u64,
            "limit": 3u64,
            "period": "rolling_7d",
        });
        let outcome = classify_run_start_429(body).unwrap();
        match outcome {
            RunStartOutcome::QuotaExhausted(info) => {
                assert_eq!(info.retry_after, 86400);
                assert_eq!(info.runs_this_week, 3);
                assert_eq!(info.limit, 3);
                assert_eq!(info.period, "rolling_7d");
            }
            _ => panic!("expected QuotaExhausted outcome"),
        }
    }

    #[test]
    fn classify_run_start_429_with_rate_limited_falls_back_to_err() {
        // If the server ever returns a generic `rate_limited` on
        // /run/start (e.g. global per-IP throttle), the desktop must
        // surface it as a transport-level 429 — not a free-tier-quota
        // event. The classification helper distinguishes the two so
        // the UI doesn't show the upgrade modal for a transient limit.
        let body = serde_json::json!({"error": "rate_limited", "retry_after": 30u64});
        let err = classify_run_start_429(body).expect_err("rate_limited must Err");
        assert_eq!(err.to_string(), "alfred_api_rate_limited");
    }

    #[test]
    fn classify_run_start_429_empty_body_falls_back_to_rate_limited() {
        // Defensive: a 429 with empty body (server bug or proxy
        // truncation) maps to alfred_api_rate_limited, not a phantom
        // quota event.
        let body = serde_json::Value::Null;
        let err = classify_run_start_429(body).expect_err("empty body must Err");
        assert_eq!(err.to_string(), "alfred_api_rate_limited");
    }
}
