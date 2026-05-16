use std::{
    env,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::local_http::request_http_json;
use crate::paths::resolve_runtime_settings_path;
use crate::storage::read_json_file;

fn read_http_json(host: &str, port: u16, path: &str, timeout_ms: u64) -> Result<serde_json::Value> {
    request_http_json("GET", host, port, path, None, Some(timeout_ms))
}

pub fn resolve_stack_health_timeout_ms() -> u64 {
    if let Ok(raw) = env::var("ALFRED_STACK_HEALTH_TIMEOUT_MS") {
        if let Ok(parsed) = raw.trim().parse::<u64>() {
            if parsed >= 500 {
                return parsed;
            }
        }
    }
    let settings_path = resolve_runtime_settings_path();
    if settings_path.exists() {
        if let Ok(settings) = read_json_file(&settings_path) {
            if let Some(parsed) = settings
                .get("values")
                .and_then(|v| v.get("stack_health_timeout_ms"))
                .and_then(|v| v.as_u64())
            {
                if parsed >= 500 {
                    return parsed;
                }
            }
        }
    }
    1500
}

fn normalize_service(
    name: &str,
    port: u16,
    allow_degraded: bool,
    payload: &serde_json::Value,
) -> serde_json::Value {
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(if payload.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            "healthy"
        } else {
            "unhealthy"
        })
        .to_ascii_lowercase();
    let live = payload.get("live").and_then(|v| v.as_bool()).unwrap_or(true);
    let ready = payload.get("ready").and_then(|v| v.as_bool()).unwrap_or(payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    if payload.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return json!({
            "name": name,
            "port": port,
            "ok": true,
            "accepted": false,
            "live": true,
            "ready": true,
            "status": if status.is_empty() { "healthy" } else { &status },
            "diagnostics": payload.get("diagnostics").cloned().unwrap_or(serde_json::Value::Null)
        });
    }
    if allow_degraded && status == "degraded" {
        return json!({
            "name": name,
            "port": port,
            "ok": false,
            "accepted": true,
            "live": live,
            "ready": ready,
            "status": "degraded",
            "diagnostics": payload.get("diagnostics").cloned().unwrap_or(serde_json::Value::Null)
        });
    }
    json!({
        "name": name,
        "port": port,
        "ok": false,
        "accepted": false,
        "live": live,
        "ready": ready,
        "status": if live && !ready {
            if status.is_empty() { "not_ready" } else { &status }
        } else if status.is_empty() {
            "unhealthy"
        } else {
            &status
        },
        "diagnostics": payload.get("diagnostics").cloned().unwrap_or(serde_json::Value::Null)
    })
}

/// Classify a 429 health probe based on the quota-tagged response
/// headers the server-side P0-12 sets on free-tier exhaustion
/// (`X-Quota-Limit`, `X-Quota-Reset-After`). Returns
/// `(status_label, diagnostics_payload)` where the diagnostics carries
/// the structured envelope the splash UI hands to `showErrorModal`.
///
/// Pure helper — no `ureq::Response` dependency so tests can drive it
/// with synthetic header pairs without an HTTP runtime. See
/// `feedback_pure_helper_for_async_testability`.
///
/// When neither header is present we fall back to the legacy P0-9
/// global RPM rate-limit classification (`rate_limited`).
pub(crate) fn classify_health_429(
    retry_after_secs: Option<u64>,
    limit: Option<u64>,
) -> (&'static str, Option<serde_json::Value>) {
    if limit.is_none() && retry_after_secs.is_none() {
        return ("rate_limited", None);
    }
    let mut diag = serde_json::Map::new();
    if let Some(v) = retry_after_secs {
        diag.insert("retry_after".to_string(), json!(v));
    }
    if let Some(v) = limit {
        diag.insert("limit".to_string(), json!(v));
    }
    diag.insert("period".to_string(), json!("rolling_7d"));
    ("free_tier_exhausted", Some(serde_json::Value::Object(diag)))
}

/// Classify a transport-level health-probe error into the status code
/// the splash UI consumes. Thin wrapper around `classify_health_429`
/// that extracts the relevant headers from a live `ureq::Response`.
fn classify_health_error(
    e: &ureq::Error,
) -> (&'static str, Option<serde_json::Value>) {
    match e {
        ureq::Error::Status(401, _) => ("unauthorized", None),
        ureq::Error::Status(429, resp) => {
            // Server-side P0-12 sets X-Quota-Limit / X-Quota-Reset-After
            // on free-tier exhaustion; their absence means this is the
            // global RPM rate-limit. ureq::Response is not Clone and
            // into_string() consumes it, but header() takes &self so
            // we can inspect both before dropping the borrow.
            let limit = resp
                .header("X-Quota-Limit")
                .and_then(|v| v.trim().parse::<u64>().ok());
            let retry_after = resp
                .header("X-Quota-Reset-After")
                .or_else(|| resp.header("Retry-After"))
                .and_then(|v| v.trim().parse::<u64>().ok());
            classify_health_429(retry_after, limit)
        }
        ureq::Error::Status(_code, _) => ("http_error", None),
        _ => ("unreachable", None),
    }
}

/// Check Alfred API remote health — calls /healthz endpoint.
///
/// v0.4.0 (P0-13): the 429 path now inspects the response body to
/// distinguish global RPM rate-limit (`rate_limited`) from per-user
/// quota exhaustion (`free_tier_exhausted`). The splash-screen consumer
/// uses this distinction to decide whether to show a "back in a few
/// seconds" toast or the upgrade modal. Body shape is documented in
/// `docs/monetization-architecture.md`.
fn check_alfred_api_health(timeout_ms: u64) -> serde_json::Value {
    let api_url = env::var("ALFRED_API_URL")
        .unwrap_or_else(|_| "https://vps-c5793aab.vps.ovh.net/alfred/api".to_string());
    let base = api_url.trim_end_matches('/');
    let url = format!("{base}/healthz");

    // 1. Public health check (no auth)
    let healthz = match ureq::get(&url)
        .timeout(Duration::from_millis(timeout_ms.max(3000)))
        .call()
    {
        Ok(resp) => resp.into_json::<serde_json::Value>().unwrap_or(json!({})),
        Err(e) => {
            let (status, diagnostics) = classify_health_error(&e);
            crate::debug_log(&format!("alfred-api health: {status}"));
            let mut payload = json!({
                "name": "alfred-api",
                "ok": false, "ready": false,
                "live": status != "unreachable",
                "accepted": false,
                "status": status,
                "error": e.to_string()
            });
            // Attach the parsed quota envelope (retry_after / limit / period)
            // when the server sent one — the splash UI uses this to
            // render the upgrade modal without a second round-trip.
            if let Some(extras) = diagnostics {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("diagnostics".to_string(), extras);
                }
            }
            return payload;
        }
    };

    let redis_ok = healthz.get("redis").and_then(|v| v.as_str()) == Some("up");
    let searxng_ok = healthz.get("searxng").and_then(|v| v.as_str()) == Some("up");
    let services_ok = redis_ok && searxng_ok;

    json!({
        "name": "alfred-api",
        "ok": services_ok,
        "ready": services_ok,
        "live": true,
        "accepted": true,
        "status": if services_ok { "healthy" } else { "degraded" },
        "auth_checked": false,
        "diagnostics": healthz
    })
}

/// Verify authenticated API access works (HMAC signature).
/// Call this after OpenAI is connected — if auth fails, analysis will
/// get no market data. Returns the full health payload with auth status.
pub fn check_api_auth() -> serde_json::Value {
    let timeout_ms = resolve_stack_health_timeout_ms();
    let base_health = check_alfred_api_health(timeout_ms);
    let live = base_health.get("live").and_then(|v| v.as_bool()).unwrap_or(false);
    if !live {
        return base_health; // API unreachable, no point checking auth
    }

    // Try an authenticated call (lightweight insights fetch for a dummy ticker)
    let auth_ok = match crate::enrichment::fetch_shared_insights("__healthcheck__", "__healthcheck__") {
        Ok(_) => true,
        Err(e) => {
            let err = e.to_string();
            if err.contains("unauthorized") || err.contains("401") {
                crate::debug_log("alfred-api auth check: HMAC failed — check ALFRED_API_SECRET");
                false
            } else {
                // Other errors (empty result, etc.) mean auth worked
                true
            }
        }
    };

    let services_ok = base_health.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let ok = services_ok && auth_ok;
    let status = if ok {
        "healthy"
    } else if !auth_ok {
        "auth_failed"
    } else {
        "degraded"
    };

    json!({
        "ok": ok,
        "live": true,
        "ready": ok,
        "status": status,
        "auth": if auth_ok { "ok" } else { "hmac_failed" },
        "services": base_health.get("diagnostics").cloned().unwrap_or(json!({}))
    })
}

/// Check Codex app-server health — tries session status.
fn check_codex_health() -> serde_json::Value {
    match crate::codex::session_status() {
        Ok(status) => {
            let logged_in = status.get("logged_in").and_then(|v| v.as_bool()).unwrap_or(false);
            let s = status.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
            json!({
                "name": "codex",
                "ok": logged_in,
                "ready": logged_in,
                "live": s != "no_binary",
                "accepted": true,
                "status": s,
                "diagnostics": status
            })
        }
        Err(e) => json!({
            "name": "codex",
            "ok": false,
            "ready": false,
            "live": false,
            "accepted": false,
            "status": "error",
            "error": e.to_string()
        }),
    }
}

pub fn collect_stack_health() -> serde_json::Value {
    let timeout_ms = resolve_stack_health_timeout_ms();
    let mut native_services: Vec<serde_json::Value> = vec![
        json!({ "name": "control-plane", "ok": true, "ready": true, "live": true, "accepted": true, "status": "native" }),
        json!({ "name": "llm-generation", "ok": true, "ready": true, "live": true, "accepted": true, "status": "native" }),
        json!({ "name": "portfolio-mcp", "ok": true, "ready": true, "live": true, "accepted": true, "status": "native" }),
        json!({ "name": "finary", "ok": true, "ready": true, "live": true, "accepted": true, "status": "native" }),
        check_alfred_api_health(timeout_ms),
        check_codex_health(),
    ];
    let services: Vec<(&str, u16, bool)> = vec![];
    let normalized: Vec<serde_json::Value> = services
        .into_iter()
        .map(|(name, port, allow_degraded)| match read_http_json("127.0.0.1", port, "/health", timeout_ms) {
            Ok(payload) => normalize_service(name, port, allow_degraded, &payload),
            Err(_) => json!({
                "name": name,
                "port": port,
                "ok": false,
                "accepted": false,
                "live": false,
                "ready": false,
                "status": "unreachable",
                "diagnostics": serde_json::Value::Null
            }),
        })
        .collect();
    native_services.extend(normalized);
    let normalized = native_services;
    let all_live = normalized
        .iter()
        .all(|service| service.get("live").and_then(|v| v.as_bool()) != Some(false));
    let all_ready = normalized.iter().all(|service| {
        service.get("ok").and_then(|v| v.as_bool()) == Some(true)
            || service.get("accepted").and_then(|v| v.as_bool()) == Some(true)
    });
    json!({
        "ok": all_ready,
        "live": all_live,
        "ready": all_ready,
        "status": if all_ready { "healthy" } else if all_live { "degraded" } else { "unreachable" },
        "services": normalized
    })
}

pub(crate) fn health_payload_ready(payload: &serde_json::Value, allow_degraded: bool) -> Result<bool> {
    let ok = payload.get("ok").and_then(|v| v.as_bool());
    let ready = payload.get("ready").and_then(|v| v.as_bool());
    let live = payload.get("live").and_then(|v| v.as_bool()).unwrap_or(true);
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    if ok == Some(true) || ready == Some(true) {
        return Ok(true);
    }
    if allow_degraded && status == "degraded" {
        return Ok(true);
    }
    if ok == Some(false) {
        let has_shape = ready == Some(false) || payload.get("live").is_some() || !status.is_empty();
        if !has_shape {
            return Err(anyhow!("service_health_payload_invalid"));
        }
        if live {
            return Err(anyhow!("service_unhealthy"));
        }
    }
    Ok(false)
}

fn wait_for_service(
    name: &str,
    port: u16,
    allow_degraded: bool,
    timeout_ms: u64,
) -> Result<()> {
    let started_at = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let mut observed_live = false;
    while started_at.elapsed() < timeout {
        match read_http_json("127.0.0.1", port, "/health", timeout_ms.max(500)) {
            Ok(payload) => match health_payload_ready(&payload, allow_degraded) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) if error.to_string().contains("service_health_payload_invalid") => {
                    return Err(anyhow!("service_health_payload_invalid:{name}"))
                }
                Err(error) if error.to_string().contains("service_unhealthy") => {
                    observed_live = true;
                }
                Err(error) => return Err(error),
            },
            Err(_) => {}
        }
        thread::sleep(Duration::from_millis(250));
    }
    if observed_live {
        return Err(anyhow!("service_unhealthy:{name}"));
    }
    Err(anyhow!("service_unreachable:{name}"))
}

fn resolve_preflight_enabled() -> bool {
    match env::var("ALFRED_PREFLIGHT_ENABLED") {
        Ok(raw) => {
            let normalized = raw.trim().to_ascii_lowercase();
            !(normalized == "0" || normalized == "false" || normalized == "no")
        }
        Err(_) => true,
    }
}

pub fn is_preflight_enabled() -> bool {
    resolve_preflight_enabled()
}

fn resolve_portfolio_source(options: Option<&serde_json::Value>) -> String {
    options
        .and_then(|payload| payload.get("portfolio_source"))
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "finary".to_string())
}

pub fn run_preflight(options: Option<&serde_json::Value>) -> Result<()> {
    if !resolve_preflight_enabled() {
        return Ok(());
    }
    let _portfolio_source = resolve_portfolio_source(options);
    // All sidecars native — no external services to preflight
    let services: Vec<(&str, u16, bool)> = vec![];
    let timeout_ms = resolve_stack_health_timeout_ms();
    for (name, port, allow_degraded) in services {
        wait_for_service(name, port, allow_degraded, timeout_ms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_health_429_with_quota_headers_surfaces_free_tier_exhausted() {
        // Server P0-12 sets X-Quota-Limit and X-Quota-Reset-After on
        // quota exhaustion. The splash probe uses both to render the
        // upgrade modal: limit shows "3 / 7 days", reset_after shows
        // "next slot in Xj".
        let (status, diag) = classify_health_429(Some(86400), Some(3));
        assert_eq!(status, "free_tier_exhausted");
        let diag = diag.expect("quota diagnostics must be attached");
        assert_eq!(diag.get("retry_after").and_then(|v| v.as_u64()), Some(86400));
        assert_eq!(diag.get("limit").and_then(|v| v.as_u64()), Some(3));
        assert_eq!(
            diag.get("period").and_then(|v| v.as_str()),
            Some("rolling_7d"),
            "period defaults to rolling_7d (only policy v0.4.0 ships)",
        );
    }

    #[test]
    fn classify_health_429_with_only_retry_after_still_quota() {
        // Defensive: if the server forgets X-Quota-Limit but sets
        // Retry-After, we still classify as quota exhaustion because
        // the global RPM limit (P0-9) never sets X-Quota-Reset-After.
        let (status, diag) = classify_health_429(Some(60), None);
        assert_eq!(status, "free_tier_exhausted");
        let diag = diag.expect("partial quota diagnostics still attached");
        assert_eq!(diag.get("retry_after").and_then(|v| v.as_u64()), Some(60));
        assert!(
            diag.get("limit").is_none(),
            "absent header must not synthesize a phantom limit",
        );
    }

    #[test]
    fn classify_health_429_without_any_quota_header_falls_back_to_rate_limited() {
        // No quota tagging → this is the existing P0-9 global RPM
        // rate-limit. Splash UI keeps its v0.3.x behaviour (status pill,
        // no modal). Backward compatible with servers that haven't
        // shipped P0-12 yet.
        let (status, diag) = classify_health_429(None, None);
        assert_eq!(status, "rate_limited");
        assert!(diag.is_none());
    }
}
