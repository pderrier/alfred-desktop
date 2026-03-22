//! Client for the remote Alfred API server.
//!
//! Calls /api/market, /api/news, /api/search on the remote server
//! with OpenAI JWT auth. Falls back gracefully on any error.

use std::env;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_API_URL: &str = "https://vps-c5793aab.vps.ovh.net/alfred/api";
const TIMEOUT_SECS: u64 = 10;

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

/// Get the OpenAI JWT from the Codex session (for auth with the API).
fn get_jwt() -> Option<String> {
    // Env var override takes priority
    if let Some(token) = env::var("ALFRED_API_TOKEN").ok().filter(|t| !t.is_empty()) {
        return Some(token);
    }

    // Read from codex auth.json — format: {tokens: {access_token, id_token, ...}}
    let home = env::var("HOME").or_else(|_| env::var("USERPROFILE")).ok()?;
    let auth_path = format!("{home}/.codex/auth.json");
    if let Ok(content) = std::fs::read_to_string(&auth_path) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
            // Codex stores tokens in a nested "tokens" object
            if let Some(tokens) = parsed.get("tokens") {
                for key in &["access_token", "id_token"] {
                    if let Some(token) = tokens.get(key).and_then(|v| v.as_str()) {
                        if !token.is_empty() {
                            return Some(token.to_string());
                        }
                    }
                }
            }
            // Also check top-level keys (other formats)
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

/// Fetch market data from the remote API.
pub fn remote_fetch_market(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let jwt = get_jwt().ok_or_else(|| anyhow!("alfred_api_no_jwt"))?;

    let url = format!(
        "{base}/api/market?ticker={}&name={}&isin={}",
        urlenc(ticker),
        urlenc(name),
        urlenc(isin),
    );

    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .call()
        .map_err(|e| map_api_error(e))?;

    let body: Value = resp.into_json().map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))?;
    Ok(body)
}

/// Fetch news from the remote API (SearXNG-backed).
pub fn remote_fetch_news(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let jwt = get_jwt().ok_or_else(|| anyhow!("alfred_api_no_jwt"))?;

    let url = format!(
        "{base}/api/news?ticker={}&name={}&isin={}",
        urlenc(ticker),
        urlenc(name),
        urlenc(isin),
    );

    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .call()
        .map_err(|e| map_api_error(e))?;

    let body: Value = resp.into_json().map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))?;
    Ok(body)
}

/// Fetch shared insights for a ticker from the API cache.
pub fn remote_fetch_insights(ticker: &str, isin: &str) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let jwt = get_jwt().ok_or_else(|| anyhow!("alfred_api_no_jwt"))?;

    let url = format!(
        "{base}/api/insights?ticker={}&isin={}",
        urlenc(ticker),
        urlenc(isin),
    );

    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .call()
        .map_err(|e| map_api_error(e))?;

    let body: Value = resp.into_json().map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))?;
    Ok(body)
}

/// Persist shared insights (generic analysis) back to the API for other users.
pub fn persist_shared_insights(ticker: &str, isin: &str, insights: &Value) {
    if !insights.is_object() || insights.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return;
    }
    let base = match api_url() {
        Some(u) => u,
        None => return,
    };
    let jwt = match get_jwt() {
        Some(t) => t,
        None => return,
    };

    let url = format!("{base}/api/insights");
    let body = serde_json::json!({
        "ticker": ticker,
        "isin": isin,
        "insights": insights,
    });

    match ureq::post(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&serde_json::to_string(&body).unwrap_or_default())
    {
        Ok(_) => crate::debug_log(&format!("alfred-api: persisted shared insights for {ticker}")),
        Err(e) => crate::debug_log(&format!("alfred-api: failed to persist shared insights for {ticker}: {e}")),
    }
}

/// Persist LLM-extracted fundamental values back to the API cache.
/// Called after the first LLM pass when it finds values from web snippets/search.
pub fn persist_extracted_fundamentals(ticker: &str, isin: &str, extracted: &Value) {
    if !extracted.is_object() || extracted.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return;
    }
    let base = match api_url() {
        Some(u) => u,
        None => return,
    };
    let jwt = match get_jwt() {
        Some(t) => t,
        None => return,
    };

    let url = format!("{base}/api/market/extracted");
    let body = serde_json::json!({
        "ticker": ticker,
        "isin": isin,
        "extracted_fundamentals": extracted,
    });

    // Fire-and-forget — don't block the analysis pipeline on cache persistence
    match ureq::post(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&serde_json::to_string(&body).unwrap_or_default())
    {
        Ok(_) => crate::debug_log(&format!("alfred-api: persisted extracted fundamentals for {ticker}")),
        Err(e) => crate::debug_log(&format!("alfred-api: failed to persist extracted fundamentals for {ticker}: {e}")),
    }
}

/// Persist a deep news summary for a specific article URL to the API cache.
pub fn persist_deep_news_summary(
    ticker: &str, isin: &str,
    article_url: &str, title: &str, summary: &str,
    quality_score: u64, relevance: &str, staleness: &str,
) {
    if summary.is_empty() || article_url.is_empty() { return; }
    let base = match api_url() { Some(u) => u, None => return };
    let jwt = match get_jwt() { Some(t) => t, None => return };

    let url = format!("{base}/api/deep-news");
    let body = serde_json::json!({
        "ticker": ticker,
        "isin": isin,
        "url": article_url,
        "title": title,
        "summary": summary,
        "quality_score": quality_score,
        "relevance": relevance,
        "staleness": staleness,
    });

    match ureq::post(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&serde_json::to_string(&body).unwrap_or_default())
    {
        Ok(_) => crate::debug_log(&format!("alfred-api: persisted deep news for {ticker} url={article_url}")),
        Err(e) => crate::debug_log(&format!("alfred-api: failed to persist deep news for {ticker}: {e}")),
    }
}

/// Ban a news URL as noise for a ticker.
pub fn ban_deep_news_url(ticker: &str, isin: &str, article_url: &str, reason: &str) {
    if article_url.is_empty() { return; }
    let base = match api_url() { Some(u) => u, None => return };
    let jwt = match get_jwt() { Some(t) => t, None => return };

    let url = format!("{base}/api/deep-news/ban");
    let body = serde_json::json!({
        "ticker": ticker,
        "isin": isin,
        "url": article_url,
        "reason": reason,
    });

    let _ = ureq::post(&url)
        .set("Authorization", &format!("Bearer {jwt}"))
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5))
        .send_string(&serde_json::to_string(&body).unwrap_or_default());
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
