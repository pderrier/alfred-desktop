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

pub fn fetch_sector(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_sector(ticker, name, isin) {
        Ok(resp) => Ok(resp),
        Err(e) => {
            crate::debug_log(&format!("enrichment sector unavailable for {ticker}: {e}"));
            Ok(json!({ "ok": true, "sector": null }))
        }
    }
}

pub fn fetch_cot(ticker: &str, isin: &str) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_cot(ticker, isin) {
        Ok(resp) => Ok(resp),
        Err(e) => {
            crate::debug_log(&format!("enrichment COT unavailable for {ticker}: {e}"));
            Ok(json!({ "ok": true, "cot": null }))
        }
    }
}

pub fn fetch_news(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    match crate::alfred_api_client::remote_fetch_news(ticker, name, isin) {
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
