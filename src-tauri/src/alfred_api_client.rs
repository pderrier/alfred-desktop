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
///
/// v0.4.0 (P0-14 + P0-15): account-level endpoints are explicitly exempted
/// from X-Run-Session injection even when the slot is filled. Rationale: if
/// an admin clicks the Admin tab while a run is in flight, or if the user
/// clicks Upgrade and the LS overlay activation handler hits `/license/*`,
/// the active session slot is non-empty, but those endpoints are NOT
/// mounted under `require_run_session` server-side (see
/// `apps/alfred-api/src/lib.rs` — admin routes layered with `require_admin`,
/// license routes layered with `require_auth` only, neither under
/// `require_run_session`). Sending the header would not break anything
/// (the server middleware simply doesn't consume it), but the contract is
/// "endpoints get exactly the auth headers they require, no more".
/// Defense-in-depth — and it keeps the request payload smaller for
/// account-scope polls. See `is_session_exempt_path` for the path-prefix
/// rule.
fn apply_auth(req: ureq::Request, path: &str) -> ureq::Request {
    let ts = now_epoch_secs();
    let client_hash = get_client_hash().unwrap_or_default();
    // Sign only the path component (no query string) — must match server-side req.uri().path()
    let sign_path = path.split('?').next().unwrap_or(path);
    let mut req = req
        .set("X-Client-Hash", &client_hash)
        .set("X-Timestamp", &ts.to_string());
    if !is_session_exempt_path(sign_path) {
        if let Some(session_id) = active_run_session() {
            req = req.set("X-Run-Session", &session_id);
        }
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

/// Pure helper: is this request path an account-level surface that does
/// NOT live under the `require_run_session` middleware? Used by
/// `apply_auth` to skip `X-Run-Session` injection — sending the header
/// would be dead weight (the server middleware doesn't consume it on
/// these routes) and silently leaks a per-run identifier into an
/// account-scope call.
///
/// Match rules:
///   - `/admin` or anything under `/admin/` (v0.4.0 P0-14 — admin
///     observability endpoints, gated by `require_admin` server-side)
///   - `/license` or anything under `/license/` (v0.4.0 P0-15 — LS
///     activation/validation/status, gated by `require_auth` only)
///   - `/quota` or anything under `/quota/` (v0.4.7 P3-31 — read-only
///     home-strip quota probe, mounted at root alongside `/run/start`,
///     gated by `require_auth` only — see `handlers::quota_routes`)
///
/// Anything else returns false — there is no fuzzy / partial /
/// case-insensitive match because the server-side routes are
/// case-sensitive too.
///
/// Renamed from `is_admin_path` in v0.4.0 P0-15 (P3-40 follow-up) when
/// `/license/*` joined the exemption set. `/quota/*` joined in v0.4.7
/// (P3-31). The behavioural contract is pinned by
/// `session_exempt_endpoints_skip_run_session_header`.
fn is_session_exempt_path(path: &str) -> bool {
    path == "/admin"
        || path.starts_with("/admin/")
        || path == "/license"
        || path.starts_with("/license/")
        || path == "/quota"
        || path.starts_with("/quota/")
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
///
/// v0.4.0 (P0-13): the 429 path now reads the response body so the
/// `error` field can distinguish:
/// - `rate_limited` → retryable transient → `RateLimited { retry_after_secs }`
/// - `free_tier_exhausted` → permanent until quota slides → `Err(...)`
///   surfaced immediately so the retry loop never wastes sleep budget
///   on a condition the user must address (upgrade) rather than wait
///   out a few seconds. See `docs/monetization-architecture.md` for the
///   server-side semantics.
///
/// Same body-aware split for 401: a `run_session_invalid` is distinct
/// from a hard auth failure (HMAC mismatch / missing client hash) — the
/// caller can surface a "session expired, click Run again" hint instead
/// of the generic "API unauthorized" message.
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
            // Read header BEFORE consuming the body — `into_string()` takes
            // self so the response is gone afterwards.
            let retry_after_secs = resp
                .header("Retry-After")
                .and_then(|v| v.trim().parse::<u64>().ok());
            let body = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if let Some(structured) = classify_429_body(&parsed, retry_after_secs) {
                // Quota exhaustion is NOT a transient error — surface it
                // immediately so `api_get_with_retry` doesn't burn its
                // budget waiting for a condition that won't resolve.
                ApiGetOutcome::Err(anyhow!("{structured}"))
            } else {
                ApiGetOutcome::RateLimited { retry_after_secs }
            }
        }
        Err(ureq::Error::Status(401, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            ApiGetOutcome::Err(anyhow!("{}", classify_401_body(&parsed)))
        }
        Err(e) => ApiGetOutcome::Err(map_api_error(e)),
    }
}

/// Classify a 429 response body. Returns:
/// - `Some(message)` when the body identifies a non-retryable quota
///   exhaustion (`error == "free_tier_exhausted"`). The message follows
///   the colon-delimited contract `alfred_free_tier_exhausted:{retry_after}:{limit}:{period}`
///   so the JS layer's `inferCodedErrorFromText` regex (extended for
///   structured codes in v0.4.0) can parse out the params for the
///   upgrade modal.
/// - `None` when the body says `rate_limited` or is empty/unparseable —
///   the caller falls through to the existing retry path. We don't
///   raise an error on missing fields because the server may be older
///   than the desktop client; missing fields just mean "no extra hint".
///
/// `retry_after_header` is the parsed `Retry-After` header (caller
/// already extracted it). When the body field `retry_after` is missing
/// or invalid we fall back to the header, then to `0`. Either way the
/// modal renders something — never a `NaN`/`null` placeholder.
pub(crate) fn classify_429_body(body: &Value, retry_after_header: Option<u64>) -> Option<String> {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code != "free_tier_exhausted" {
        return None;
    }
    let retry_after = body
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .or(retry_after_header)
        .unwrap_or(0);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let period = body
        .get("period")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("rolling_7d");
    Some(format!(
        "alfred_free_tier_exhausted:{retry_after}:{limit}:{period}"
    ))
}

/// Classify a 401 response body. Returns:
/// - `"alfred_run_session_invalid"` when the body identifies an expired
///   or absent run-session token (v0.4.0 P0-11). This is recoverable —
///   the desktop just needs to call `/run/start` again to get a fresh
///   session. The JS layer can show a soft hint rather than the global
///   "API unauthorized" modal.
/// - `"alfred_api_unauthorized"` otherwise (HMAC mismatch, missing
///   `X-Client-Hash`, or an older server that doesn't set the body
///   field). Same wire shape as v0.3.x so existing callers keep working.
pub(crate) fn classify_401_body(body: &Value) -> &'static str {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code == "run_session_invalid" {
        "alfred_run_session_invalid"
    } else {
        "alfred_api_unauthorized"
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

/// Fetch the global macro briefing (US 10Y, VIX, EUR/USD, Brent) from
/// `GET /api/macro` (P1-60).
///
/// Portfolio-agnostic, no parameters. The server caches the briefing
/// globally for 30 min, so the desktop never needs its own caching layer.
/// Server endpoint may not be deployed on older API instances (returns
/// 404 in that case) — callers must degrade gracefully (treat as "no
/// macro briefing available", same pattern as `remote_fetch_sector`).
pub fn remote_fetch_macro_briefing() -> Result<Value> {
    api_get("/api/macro", TIMEOUT_SECS)
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

// ── Admin observability (v0.4.0 P0-14) ───────────────────────────────

/// Typed mirror of the `/admin/usage` response envelope.
///
/// Pinning the shape via serde — instead of leaving it as `serde_json::Value`
/// — gives us a parse-time contract test: if the server-side
/// `apps/alfred-api/src/admin.rs::admin_usage_handler` renames or drops a
/// field, deserialization fails loudly here rather than silently rendering
/// an empty table client-side. All fields default to safe zeros / empty
/// collections so an older server that doesn't yet emit a particular field
/// degrades gracefully.
///
/// The server hash is already truncated to 8 chars before wire (see
/// `admin.rs::anonymize_user_hash`); this struct simply forwards the
/// anonymised value — no further truncation needed.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminUsage {
    /// Top-N users by `runs_7d`, anonymised hashes only.
    #[serde(default)]
    pub top_users: Vec<AdminTopUser>,
    /// Most-recently-seen tickers (ISIN + epoch timestamp).
    #[serde(default)]
    pub top_tickers: Vec<AdminTopTicker>,
    /// Sum of `runs_7d` across all users.
    #[serde(default)]
    pub runs_7d: u64,
    /// Sum of `runs_24h` across all users.
    #[serde(default)]
    pub runs_24h: u64,
    /// Today's `metrics:429:<date>` counter.
    #[serde(default)]
    pub errors_429_today: u64,
    /// v1 placeholder — per-endpoint counters deferred. Forwarded verbatim
    /// so the UI can surface the server's "tracked: false" hint.
    #[serde(default)]
    pub by_endpoint: Value,
    /// Server timestamp at response emission (epoch seconds).
    #[serde(default)]
    pub generated_at: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminTopUser {
    pub user_hash: String,
    pub runs_7d: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminTopTicker {
    pub isin: String,
    pub last_seen: u64,
}

/// Typed mirror of the `/admin/vps-stats` response envelope. Same contract
/// rationale as [`AdminUsage`]: parse-time validation against the server's
/// `admin_vps_stats_handler` shape.
///
/// All numeric fields default to 0 so an older server that doesn't yet
/// emit a particular sub-field degrades gracefully.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminVpsStats {
    #[serde(default)]
    pub redis: AdminRedisStats,
    #[serde(default)]
    pub process: AdminProcessStats,
    #[serde(default)]
    pub generated_at: u64,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AdminRedisStats {
    #[serde(default)]
    pub memory_used: Option<u64>,
    #[serde(default)]
    pub memory_peak: Option<u64>,
    #[serde(default)]
    pub connected_clients: Option<u64>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AdminProcessStats {
    #[serde(default)]
    pub rss_bytes: u64,
    #[serde(default)]
    pub uptime_secs: u64,
}

/// P0-16 (2026-05-23) — server-driven admin tab visibility probe.
/// Calls `GET /admin/check`, returns `Ok(true)` on 204 (admin),
/// `Ok(false)` on 403 (not admin). Other failures (5xx, network) are
/// surfaced as `Err` so the caller can log the diagnostic instead of
/// silently flipping the tab off.
///
/// Replaces the desktop's previous `admin_config::is_whitelisted_admin`
/// path — the allowlist now lives only in the server's
/// `ALFRED_ADMIN_HASHES` env var, so adding an admin no longer requires
/// a desktop rebuild.
pub fn admin_check() -> Result<bool> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}/admin/check");
    let req = apply_auth(ureq::get(&url), "/admin/check")
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    match req.call() {
        // 204 NoContent is the exact contract from the server handler.
        Ok(resp) if resp.status() == 204 => Ok(true),
        Ok(resp) => {
            // 2xx other than 204 isn't expected — surface as error so
            // a future server-side regression doesn't silently degrade
            // to "everyone is admin".
            Err(anyhow!(
                "alfred_api_admin_check_unexpected_status:{}",
                resp.status()
            ))
        }
        Err(ureq::Error::Status(403, _)) => Ok(false),
        Err(e) => Err(map_api_error(e)),
    }
}

/// Fetch `/admin/usage`. Returns the raw envelope :
/// `{ok, top_users, top_tickers, runs_7d, runs_24h, errors_429_today,
///   by_endpoint, generated_at}`.
///
/// Requires the user's client_hash to be on the server-side
/// `ALFRED_ADMIN_HASHES` allowlist; otherwise the server responds 403 and
/// this function surfaces `alfred_api_http_error:403`. The desktop UI
/// hides the Admin tab unless the cold-start `admin_check` probe
/// returned 204 (P0-16, 2026-05-23) so the 403 path is normally
/// unreachable for non-admins.
///
/// Admin endpoints are NOT under the run-session middleware (verified in
/// `apps/alfred-api/src/lib.rs::build_router` — admin_routes are mounted
/// at the root, not nested under `/api/`). `apply_auth` exempts
/// `/admin/*` paths from `X-Run-Session` injection so the header is
/// guaranteed absent here — see `is_admin_path` and the contract test
/// `admin_endpoint_calls_do_not_inject_run_session_header`.
///
/// The response is parsed through [`AdminUsage`] for contract validation
/// (a server-side field rename will surface as a parse error rather than
/// a silently-empty UI), then re-serialised back to `Value` for forwarding
/// through Tauri to the JS layer. JS reads the same key shape as before.
pub fn get_admin_usage() -> Result<Value> {
    let raw = api_get("/admin/usage", TIMEOUT_SECS)?;
    let parsed: AdminUsage = serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:admin_usage:{e}"))?;
    serde_json::to_value(parsed)
        .map_err(|e| anyhow!("alfred_api_serialize_failed:admin_usage:{e}"))
}

/// Fetch `/admin/vps-stats`. Returns the raw envelope :
/// `{ok, redis: {memory_used, memory_peak, connected_clients},
///   process: {rss_bytes, uptime_secs}, generated_at}`.
///
/// Same auth contract as [`get_admin_usage`]. Parsed through
/// [`AdminVpsStats`] for contract validation then re-serialised back to
/// `Value` for Tauri forwarding.
pub fn get_admin_vps_stats() -> Result<Value> {
    let raw = api_get("/admin/vps-stats", TIMEOUT_SECS)?;
    let parsed: AdminVpsStats = serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:admin_vps_stats:{e}"))?;
    serde_json::to_value(parsed)
        .map_err(|e| anyhow!("alfred_api_serialize_failed:admin_vps_stats:{e}"))
}

/// Compute the current user's client hash. Exposes the previously-private
/// `get_client_hash` so the admin tab visibility check can run on the
/// same value the server sees in `X-Client-Hash`. Returns `None` when no
/// OpenAI JWT is available locally (e.g. user not signed in to Codex).
///
/// Kept under a dedicated public function rather than re-exporting the
/// internal helper, so the contract ("the hash sent on every request") is
/// stable and the doc-comment can pin the format.
pub fn current_user_hash() -> Option<String> {
    get_client_hash()
}

// ── License client (v0.4.0 P0-15) ─────────────────────────────────────
//
// Proxies the Lemon Squeezy License API via the alfred-api server (the
// desktop never talks to LS directly — see `docs/monetization-architecture.md`
// § "Lemon Squeezy authoritative + Redis cache"). All requests follow the
// HMAC auth contract (`X-Client-Hash` + `X-Timestamp` + `X-Signature`)
// but are exempt from `X-Run-Session` injection — license endpoints are
// account-level, not per-run (see `is_session_exempt_path`).
//
// Server response shapes pinned in `apps/alfred-api/src/license.rs`. Each
// struct here uses `#[serde(default)]` on every field so an older server
// (one wire-compatible release behind) degrades gracefully — missing
// fields default to None / empty rather than parse-failing.

/// Typed mirror of the `/license/activate` success response envelope.
///
/// Wire shape (from `license.rs::activate_handler` Activated arm):
/// ```json
/// { "ok": true, "tier": "paid", "instance_id": "...", "expires_at": "ISO",
///   "validated_at": 1700000000 }
/// ```
///
/// The error path (LS rejected, key revoked, etc.) is HTTP 400 with a
/// `{error: "license_invalid", reason: "..."}` body that surfaces through
/// the standard `map_api_error` chain — not this struct.
///
/// The 503 `license_provider_not_configured` path (LEMON_SQUEEZY_API_KEY
/// empty) is mapped to the structured error code
/// `alfred_license_provider_not_configured` in `map_license_error`
/// below so the desktop UI can render a clean banner instead of a
/// vague HTTP error.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LicenseActivationResponse {
    /// Always `true` on success; defaults to `false` if the server omits.
    #[serde(default)]
    pub ok: bool,
    /// `"paid"` on success — pinned for an extra sanity check at the call
    /// site (we can assert this in tests + when persisting to user-prefs).
    #[serde(default)]
    pub tier: String,
    /// LS-assigned instance identifier. Used by P2-8 multi-device
    /// deactivation. May be `None` on older server versions.
    #[serde(default)]
    pub instance_id: Option<String>,
    /// ISO-8601 expiry timestamp (LS-native format). `None` when the
    /// subscription has no fixed end date.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Epoch seconds when the activation was validated (server clock).
    /// Used as the source for the desktop's `license_validated_at`
    /// user-preference.
    #[serde(default)]
    pub validated_at: u64,
}

/// Typed mirror of the `/license/status` response envelope.
///
/// Wire shape (from `license.rs::status_handler`):
/// ```json
/// { "ok": true, "tier": "free"|"paid", "expires_at": <epoch>|null,
///   "validated_at": <epoch>|null, "pending_notice": "refunded"|"expired"|null }
/// ```
///
/// Note `expires_at` is an epoch second here (different from the
/// activation response's ISO string) — the status endpoint reads back
/// from the Redis `TierRecord` which stores epoch seconds.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LicenseStatusResponse {
    #[serde(default)]
    pub ok: bool,
    /// `"free"` or `"paid"`. Defaults to empty string if the server omits.
    #[serde(default)]
    pub tier: String,
    /// Epoch seconds when the subscription expires. `None` when the user
    /// is on the free tier or when the paid tier has no fixed end.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Epoch seconds of the last successful activation/revalidation.
    #[serde(default)]
    pub validated_at: Option<u64>,
    /// Sticky banner set by webhook events: `"refunded"`, `"expired"`,
    /// or `None`. The desktop reads this on cold start to surface a
    /// one-shot notice to the user (P1-8 — separate item).
    #[serde(default)]
    pub pending_notice: Option<String>,
}

/// Activate a Lemon Squeezy license key against the server.
///
/// `POST /license/activate` body `{license_key, instance_name}`. The
/// server proxies to LS, sets `tier:<hash> = paid` on success, and
/// returns the typed envelope above.
///
/// Error semantics:
/// - HTTP 400 `license_invalid` → bubbled up as
///   `alfred_api_http_error:400` through `map_api_error` (caller can
///   parse the body via `inferCodedErrorFromText` JS-side).
/// - HTTP 503 `license_provider_not_configured` → mapped to the clean
///   structured code `alfred_license_provider_not_configured` so the
///   desktop renders a "service temporarily unavailable" banner
///   instead of a confusing "auth failure" message.
/// - Any other status / transport error → standard `map_api_error`.
///
/// `instance_name` is forwarded to LS for display in the user's LS
/// dashboard ("Activated from Alfred on Pierre's MacBook Pro"). Pass
/// the OS hostname or a fixed `"alfred-desktop"` when unknown — the
/// server defaults to the latter when this is empty.
pub fn activate_license(
    license_key: &str,
    instance_name: &str,
) -> Result<LicenseActivationResponse> {
    let body = serde_json::json!({
        "license_key": license_key,
        "instance_name": instance_name,
    });
    let raw = api_post_json("/license/activate", &body)?;
    serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:license_activate:{e}"))
}

/// Validate a Lemon Squeezy license key against the server.
///
/// `POST /license/validate` body `{license_key}`. The server proxies to
/// the LS validate endpoint and returns the raw LS response — full
/// integration deferred to P1-6 (cold-start revalidation orchestration).
/// Today wired so the desktop API client has a stable surface to call
/// when P1-6 lands; this method just returns the parsed JSON envelope.
///
/// Same error semantics as [`activate_license`].
pub fn validate_license(license_key: &str) -> Result<Value> {
    let body = serde_json::json!({
        "license_key": license_key,
    });
    api_post_json("/license/validate", &body)
}

/// Fetch the cached license status for the current user.
///
/// `GET /license/status`. Reads the Redis `tier:<hash>` record (no LS
/// round-trip). Returns the typed envelope above.
///
/// The desktop reads this on cold start (P1-6 separate item) and after
/// every successful activation to refresh the user-pref cache. A
/// non-paid `tier` here means the cache is stale or the user has never
/// upgraded — the `pending_notice` field surfaces refund/expired
/// banners set by webhooks.
pub fn get_license_status() -> Result<LicenseStatusResponse> {
    let raw = api_get("/license/status", TIMEOUT_SECS)?;
    serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:license_status:{e}"))
}

/// Fetch the authoritative rolling-7d quota for the current user.
///
/// `GET /quota/status` (v0.4.7 P3-31). READ-ONLY — the server reuses
/// `quota::get_quota_info`, whose only Redis write is the expired-entry
/// prune; it NEVER consumes a run. This is the primary source for the
/// home strip count, replacing the local `run-index.json` count that
/// drifts ±1 vs the server ZSET (`docs/monetization-architecture.md`
/// § "Quota exposure to UI").
///
/// Returns the raw server envelope `{count, limit, period, reset_at}`
/// unparsed: `limit` is a NUMBER for free tier but the STRING
/// `"unlimited"` for paid tier, so a typed `u32` mirror would fail on
/// paid users. The JS layer (`quota-local-counter.js`) does the
/// numeric coercion and treats a non-numeric `limit` as the desktop
/// default of 3 — paid tier is rendered from `licenseStatus().tier`,
/// not from this `limit`.
///
/// `/quota/status` is auth-gated (HMAC + `X-Client-Hash`) but NOT under
/// `require_run_session` — `is_session_exempt_path` skips injecting the
/// `X-Run-Session` header so a cold-start home render works before any
/// run session exists.
pub fn get_quota_status() -> Result<Value> {
    api_get("/quota/status", TIMEOUT_SECS)
}

/// Authenticated POST request with a JSON body that parses and returns
/// the response envelope. Used by `/license/*` calls where the server
/// returns a structured response we want to consume (unlike `api_post`
/// which is fire-and-forget).
///
/// Centralises the body-shape handling for the license codes:
/// - HTTP 503 with body `error=license_provider_not_configured` →
///   structured code `alfred_license_provider_not_configured`.
/// - Other 4xx/5xx → `alfred_api_http_error:<code>` (existing contract).
/// - Transport failures → `alfred_api_request_failed:<e>` (existing).
fn api_post_json(path: &str, body: &Value) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    let result = req.send_string(&serde_json::to_string(body).unwrap_or_default());
    match result {
        Ok(resp) => resp
            .into_json::<Value>()
            .map_err(|e| anyhow!("alfred_api_parse_failed:{e}")),
        Err(ureq::Error::Status(503, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
            Err(anyhow!("{}", classify_503_body(&parsed)))
        }
        Err(e) => Err(map_api_error(e)),
    }
}

/// Classify a 503 response body. Returns:
/// - `"alfred_license_provider_not_configured"` when the body identifies
///   an unconfigured LS provider (server-side `LEMON_SQUEEZY_API_KEY`
///   env var empty). The desktop renders a clean "temporarily unavailable"
///   banner rather than a vague HTTP error — see `monetization-architecture.md`.
/// - `"alfred_api_http_error:503"` for any other 503 (server overloaded,
///   upstream failure with no body, etc.) so existing transport handling
///   continues to work.
pub(crate) fn classify_503_body(body: &Value) -> String {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code == "license_provider_not_configured" {
        "alfred_license_provider_not_configured".to_string()
    } else {
        "alfred_api_http_error:503".to_string()
    }
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

/// Classify residual ureq errors after `api_get_once` has already peeled
/// off 401 and 429 (which need body-aware handling for v0.4.0 quota /
/// session codes). What lands here is the non-status transport tail
/// (timeouts, DNS, TLS) plus any non-401/429 HTTP status (4xx other than
/// auth, 5xx). Keep the colon-delimited shape (`alfred_api_http_error:<code>`)
/// because downstream classifiers in `enrichment::classify_api_error`
/// regex on that prefix.
fn map_api_error(e: ureq::Error) -> anyhow::Error {
    match &e {
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

    // ── Session-exempt path exemption from X-Run-Session ────────────
    //
    // v0.4.0 P0-14 added /admin/*; v0.4.0 P0-15 (P3-40) extended to
    // /license/* and renamed `is_admin_path` → `is_session_exempt_path`.
    //
    // CRITICAL contract — if a future refactor of apply_auth ever
    // injects X-Run-Session unconditionally, account-level calls fired
    // during an active run would leak a session header into endpoints
    // that are NOT under `require_run_session`. The server would ignore
    // the header (no harm), but the contract is "endpoints get exactly
    // the auth headers they need". Pinned here so a regression fails CI
    // instead of degrading silently to a payload bloat.

    #[test]
    fn is_session_exempt_path_matches_admin_root_and_subroutes() {
        // /admin equals the collection root (defensive — not actually
        // a server-side route today but the rule must cover it).
        assert!(is_session_exempt_path("/admin"));
        // Documented sub-routes from `apps/alfred-api/src/admin.rs`.
        assert!(is_session_exempt_path("/admin/usage"));
        assert!(is_session_exempt_path("/admin/vps-stats"));
        // Future admin routes must inherit the rule.
        assert!(is_session_exempt_path("/admin/anything/nested/here"));
    }

    #[test]
    fn is_session_exempt_path_matches_license_root_and_subroutes() {
        // v0.4.0 P0-15: /license/* added to the exempt set. License
        // endpoints are account-level (activation / validation /
        // status) and not under `require_run_session` server-side.
        assert!(is_session_exempt_path("/license"));
        // Documented sub-routes from `apps/alfred-api/src/license.rs`.
        assert!(is_session_exempt_path("/license/activate"));
        assert!(is_session_exempt_path("/license/validate"));
        assert!(is_session_exempt_path("/license/status"));
        // Future license routes must inherit the rule (e.g. P2-8
        // device deactivation).
        assert!(is_session_exempt_path("/license/devices/abc/deactivate"));
    }

    #[test]
    fn is_session_exempt_path_matches_quota_root_and_subroutes() {
        // v0.4.7 P3-31: /quota/* added to the exempt set. The home strip
        // probes `GET /quota/status` at cold start AND during an active
        // run (when the X-Run-Session slot is populated). The endpoint is
        // NOT under `require_run_session` server-side (mounted at root via
        // `handlers::quota_routes`), so the header must be suppressed —
        // otherwise an active-run home re-render would leak a session
        // header into a non-session endpoint.
        assert!(is_session_exempt_path("/quota"));
        assert!(is_session_exempt_path("/quota/status"));
        // Future quota routes must inherit the rule.
        assert!(is_session_exempt_path("/quota/anything/nested"));
    }

    #[test]
    fn is_session_exempt_path_rejects_non_exempt_paths() {
        // Make sure we don't accidentally over-match. Every gated
        // analysis endpoint must continue to receive X-Run-Session.
        assert!(!is_session_exempt_path("/api/market"));
        assert!(!is_session_exempt_path("/api/news"));
        assert!(!is_session_exempt_path("/api/admin"));        // suffix, not prefix
        assert!(!is_session_exempt_path("/api/license"));      // suffix, not prefix
        assert!(!is_session_exempt_path("/api/quota"));        // suffix, not prefix
        assert!(!is_session_exempt_path("/run/start"));
        assert!(!is_session_exempt_path("/healthz"));
        assert!(!is_session_exempt_path(""));
        assert!(!is_session_exempt_path("/"));
        // Case-sensitive — server routes are case-sensitive too.
        assert!(!is_session_exempt_path("/Admin/usage"));
        assert!(!is_session_exempt_path("/ADMIN/usage"));
        assert!(!is_session_exempt_path("/License/activate"));
        assert!(!is_session_exempt_path("/LICENSE/activate"));
    }

    #[test]
    fn session_exempt_endpoints_skip_run_session_header() {
        // CRITICAL: when an admin clicks the Admin tab while a run is
        // active, or the user clicks Upgrade and the LS activation
        // handler fires `/license/activate`, the session slot is
        // populated. apply_auth must NOT leak X-Run-Session into these
        // account-level requests — those endpoints are not gated by
        // `require_run_session` server-side.
        //
        // Renamed from `admin_endpoint_calls_do_not_inject_run_session_header`
        // in v0.4.0 P0-15 (P3-40) when /license/* joined the exempt
        // set. Same assertions, expanded coverage.
        let _guard = run_session_test_lock();
        clear_active_run_session();
        set_active_run_session("active-run-while-account-call-fires");

        for path in [
            "/admin/usage",
            "/admin/vps-stats",
            "/license/activate",
            "/license/validate",
            "/license/status",
        ] {
            let raw = ureq::get(&format!("https://example.test{path}"));
            let signed = apply_auth(raw, path);
            let dbg = format!("{signed:?}");
            assert!(
                !dbg.to_lowercase().contains("x-run-session"),
                "X-Run-Session must be exempt for {path} — leaked into account-level call. Debug: {dbg}",
            );
        }

        // Sanity: a non-exempt path during the same run still gets the
        // header. Pins the exemption to the path, not a global toggle.
        let raw = ureq::get("https://example.test/api/news");
        let signed = apply_auth(raw, "/api/news");
        let dbg = format!("{signed:?}");
        assert!(
            dbg.to_lowercase().contains("x-run-session"),
            "Non-exempt paths must still receive X-Run-Session during an active run",
        );

        clear_active_run_session();
    }

    #[test]
    fn session_exempt_endpoints_still_inject_hmac_auth() {
        // The X-Run-Session exemption must NOT cascade into the HMAC
        // signing path — admin AND license endpoints are still wrapped
        // by `require_auth` server-side, so X-Client-Hash + X-Timestamp
        // + X-Signature are mandatory. Verify all three headers survive
        // the session-exempt branch.
        //
        // Renamed from `admin_endpoint_calls_still_inject_hmac_auth` in
        // v0.4.0 P0-15 when license was added to the exempt set.
        let _guard = run_session_test_lock();
        clear_active_run_session();
        // Set a compile-time-style secret via the runtime env fallback
        // so apply_auth signs the request even in test builds (the
        // option_env! const is None when ALFRED_API_SECRET wasn't
        // injected at compile time).
        std::env::set_var("ALFRED_API_SECRET", "test-secret-for-hmac");

        for path in ["/admin/usage", "/license/activate"] {
            let raw = ureq::get(&format!("https://example.test{path}"));
            let signed = apply_auth(raw, path);
            let dbg = format!("{signed:?}").to_lowercase();
            assert!(
                dbg.contains("x-client-hash"),
                "X-Client-Hash header missing from {path} request. Debug: {dbg}",
            );
            assert!(
                dbg.contains("x-timestamp"),
                "X-Timestamp header missing from {path} request. Debug: {dbg}",
            );
            assert!(
                dbg.contains("x-signature"),
                "X-Signature header missing from {path} request. Debug: {dbg}",
            );
        }

        std::env::remove_var("ALFRED_API_SECRET");
    }

    // ── Admin response shape contract (v0.4.0 P0-14) ────────────────

    #[test]
    fn admin_usage_deserializes_full_server_envelope() {
        // Mirror the exact shape emitted by `admin_usage_handler` in
        // apps/alfred-api/src/admin.rs. If the server ever renames or
        // drops a field, this parse fails loudly — a much better
        // failure mode than silently rendering an empty table.
        let body = serde_json::json!({
            "ok": true,
            "top_users": [
                { "user_hash": "abc12345", "runs_7d": 12 },
                { "user_hash": "def67890", "runs_7d": 3 }
            ],
            "top_tickers": [
                { "isin": "FR0010163345", "last_seen": 1_700_000_000u64 }
            ],
            "runs_7d": 15u64,
            "runs_24h": 4u64,
            "errors_429_today": 2u64,
            "by_endpoint": { "tracked": false, "note": "deferred" },
            "generated_at": 1_700_000_100u64,
        });
        let parsed: AdminUsage = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.top_users.len(), 2);
        assert_eq!(parsed.top_users[0].user_hash, "abc12345");
        assert_eq!(parsed.top_users[0].runs_7d, 12);
        assert_eq!(parsed.top_tickers.len(), 1);
        assert_eq!(parsed.top_tickers[0].isin, "FR0010163345");
        assert_eq!(parsed.runs_7d, 15);
        assert_eq!(parsed.runs_24h, 4);
        assert_eq!(parsed.errors_429_today, 2);
        assert_eq!(parsed.generated_at, 1_700_000_100);
        assert!(parsed.by_endpoint.is_object());
    }

    #[test]
    fn admin_usage_degrades_gracefully_on_missing_fields() {
        // An older server (deployed before today's full payload) might
        // omit a sub-field. We default to zero/empty so the UI renders
        // a benign "nothing yet" state instead of erroring out.
        let body = serde_json::json!({ "ok": true });
        let parsed: AdminUsage = serde_json::from_value(body).unwrap();
        assert!(parsed.top_users.is_empty());
        assert!(parsed.top_tickers.is_empty());
        assert_eq!(parsed.runs_7d, 0);
        assert_eq!(parsed.runs_24h, 0);
        assert_eq!(parsed.errors_429_today, 0);
        assert_eq!(parsed.generated_at, 0);
    }

    #[test]
    fn admin_vps_stats_deserializes_full_server_envelope() {
        let body = serde_json::json!({
            "ok": true,
            "redis": {
                "memory_used": 1_048_576u64,
                "memory_peak": 2_097_152u64,
                "connected_clients": 3u64,
            },
            "process": {
                "rss_bytes": 52_428_800u64,
                "uptime_secs": 3600u64,
            },
            "generated_at": 1_700_000_100u64,
        });
        let parsed: AdminVpsStats = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.redis.memory_used, Some(1_048_576));
        assert_eq!(parsed.redis.memory_peak, Some(2_097_152));
        assert_eq!(parsed.redis.connected_clients, Some(3));
        assert_eq!(parsed.process.rss_bytes, 52_428_800);
        assert_eq!(parsed.process.uptime_secs, 3600);
        assert_eq!(parsed.generated_at, 1_700_000_100);
    }

    #[test]
    fn admin_vps_stats_handles_optional_redis_fields() {
        // Redis INFO parse can return None for any field (older Redis,
        // non-numeric value). The wire shape sends explicit null in
        // that case; deserialization must accept it as Option::None
        // rather than erroring.
        let body = serde_json::json!({
            "ok": true,
            "redis": {
                "memory_used": null,
                "memory_peak": null,
                "connected_clients": null,
            },
            "process": {
                "rss_bytes": 0,
                "uptime_secs": 0,
            },
            "generated_at": 0,
        });
        let parsed: AdminVpsStats = serde_json::from_value(body).unwrap();
        assert!(parsed.redis.memory_used.is_none());
        assert!(parsed.redis.memory_peak.is_none());
        assert!(parsed.redis.connected_clients.is_none());
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

    // ── 429 / 401 body classification (v0.4.0 P0-13) ────────────────

    #[test]
    fn classify_429_body_detects_free_tier_exhausted_with_full_payload() {
        // Server-side P0-12 emits this exact envelope on quota exhaustion
        // (see docs/monetization-architecture.md). The desktop must surface
        // ALL three params (retry_after, limit, period) so the modal can
        // render "limite 3 / 7 jours, reset dans Xj" without re-querying.
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 86400,
            "runs_this_week": 3,
            "limit": 3,
            "period": "rolling_7d"
        });
        let classified = classify_429_body(&body, None);
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:86400:3:rolling_7d"),
            "structured code must encode retry_after:limit:period in order"
        );
    }

    #[test]
    fn classify_429_body_falls_back_to_header_when_body_retry_after_missing() {
        // Defensive: if the server forgets `retry_after` in the body but
        // sets the standard HTTP header, use the header — the modal still
        // needs *something* to compute "reset in Xj".
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "limit": 3,
            "period": "rolling_7d"
        });
        let classified = classify_429_body(&body, Some(43200));
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:43200:3:rolling_7d"),
        );
    }

    #[test]
    fn classify_429_body_uses_default_period_when_missing() {
        // Older servers might omit `period` — default to `rolling_7d`
        // because that's the only quota policy v0.4.0 ships.
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 100,
            "limit": 3
        });
        let classified = classify_429_body(&body, None);
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:100:3:rolling_7d"),
        );
    }

    #[test]
    fn classify_429_body_returns_none_for_rate_limited() {
        // The existing P0-9 global RPM rate-limit code (`rate_limited`) must
        // NOT match — that one is retryable and stays on the transient path.
        let body = serde_json::json!({"error": "rate_limited", "retry_after": 12});
        assert_eq!(classify_429_body(&body, None), None);
    }

    #[test]
    fn classify_429_body_returns_none_for_empty_or_unparseable() {
        // An older server (or a proxy that strips bodies) might give us an
        // empty 429. We must fall through to the retry path — assuming
        // "rate_limited" — rather than ship a bogus modal.
        assert_eq!(classify_429_body(&Value::Null, None), None);
        assert_eq!(classify_429_body(&serde_json::json!({}), None), None);
    }

    #[test]
    fn classify_401_body_detects_run_session_invalid() {
        // P0-11 session token expired or revoked — the desktop should call
        // /run/start to get a fresh one. Distinct from HMAC failure.
        let body = serde_json::json!({"error": "run_session_invalid", "hint": "session expired"});
        assert_eq!(classify_401_body(&body), "alfred_run_session_invalid");
    }

    #[test]
    fn classify_401_body_defaults_to_unauthorized() {
        // HMAC failure, missing X-Client-Hash, dev-mode rejection, or older
        // server that doesn't set the body field — all map to the generic
        // `alfred_api_unauthorized` (the v0.3.x wire shape).
        assert_eq!(classify_401_body(&Value::Null), "alfred_api_unauthorized");
        assert_eq!(
            classify_401_body(&serde_json::json!({})),
            "alfred_api_unauthorized",
        );
        assert_eq!(
            classify_401_body(&serde_json::json!({"error": "unauthorized"})),
            "alfred_api_unauthorized",
        );
    }

    // ── License response contract (v0.4.0 P0-15) ────────────────────

    #[test]
    fn license_activation_response_parses_full_envelope() {
        // Mirror the exact shape emitted by `activate_handler` in
        // apps/alfred-api/src/license.rs (Activated arm). A server-side
        // rename or drop of any of these fields would surface here as a
        // parse error rather than a silently-empty user-pref write.
        let body = serde_json::json!({
            "ok": true,
            "tier": "paid",
            "instance_id": "inst-abc123",
            "expires_at": "2027-05-17T00:00:00Z",
            "validated_at": 1_700_000_100u64,
        });
        let parsed: LicenseActivationResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert_eq!(parsed.instance_id.as_deref(), Some("inst-abc123"));
        assert_eq!(parsed.expires_at.as_deref(), Some("2027-05-17T00:00:00Z"));
        assert_eq!(parsed.validated_at, 1_700_000_100);
    }

    #[test]
    fn license_activation_response_degrades_on_missing_fields() {
        // A wire-compatible older server (one ship behind) may omit a
        // sub-field — defaults must keep parse green so the desktop
        // doesn't dead-end on an upgrade flow.
        let body = serde_json::json!({ "ok": true, "tier": "paid" });
        let parsed: LicenseActivationResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert!(parsed.instance_id.is_none());
        assert!(parsed.expires_at.is_none());
        assert_eq!(parsed.validated_at, 0);
    }

    #[test]
    fn license_status_response_parses_full_envelope() {
        // Mirror the exact shape emitted by `status_handler` in
        // apps/alfred-api/src/license.rs. Note: `expires_at` is an
        // epoch second here (distinct from activate's ISO string) —
        // pin the type so they don't drift.
        let body = serde_json::json!({
            "ok": true,
            "tier": "paid",
            "expires_at": 1_810_857_600u64,
            "validated_at": 1_700_000_100u64,
            "pending_notice": "refunded",
        });
        let parsed: LicenseStatusResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert_eq!(parsed.expires_at, Some(1_810_857_600));
        assert_eq!(parsed.validated_at, Some(1_700_000_100));
        assert_eq!(parsed.pending_notice.as_deref(), Some("refunded"));
    }

    #[test]
    fn license_status_response_handles_free_tier_null_fields() {
        // Free-tier users have `tier: "free"` and the timestamp +
        // notice fields are explicit nulls (default TierRecord).
        let body = serde_json::json!({
            "ok": true,
            "tier": "free",
            "expires_at": null,
            "validated_at": null,
            "pending_notice": null,
        });
        let parsed: LicenseStatusResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "free");
        assert!(parsed.expires_at.is_none());
        assert!(parsed.validated_at.is_none());
        assert!(parsed.pending_notice.is_none());
    }

    #[test]
    fn classify_503_body_detects_license_provider_not_configured() {
        // The server returns this when LEMON_SQUEEZY_API_KEY is empty —
        // common on first deploy before Pierre wires the LS account.
        // The desktop must show a clean "temporarily unavailable" banner.
        let body = serde_json::json!({
            "error": "license_provider_not_configured",
            "hint": "Server admin must populate LEMON_SQUEEZY_API_KEY",
        });
        assert_eq!(
            classify_503_body(&body),
            "alfred_license_provider_not_configured",
        );
    }

    #[test]
    fn classify_503_body_falls_back_to_generic_http_error() {
        // Any other 503 (overload, upstream failure with no body) must
        // surface as the standard `alfred_api_http_error:503` so existing
        // transport handling continues to work.
        assert_eq!(
            classify_503_body(&Value::Null),
            "alfred_api_http_error:503",
        );
        assert_eq!(
            classify_503_body(&serde_json::json!({})),
            "alfred_api_http_error:503",
        );
        assert_eq!(
            classify_503_body(&serde_json::json!({"error": "service_unavailable"})),
            "alfred_api_http_error:503",
        );
    }
}
