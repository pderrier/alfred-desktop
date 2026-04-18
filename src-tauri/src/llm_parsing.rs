//! LLM response parsing — JSON extraction, insights persistence, validation helpers.
//!
//! Extracted from `llm.rs` for single-responsibility.

use anyhow::{anyhow, Result};
use serde_json::Value;

// ── Response extraction ──────────────────────────────────────────

/// Extract a JSON draft object from any LLM backend response format.
///
/// Supported formats (tried in order):
/// 1. **OpenAI Chat Completions**: `{"choices": [{"message": {"content": "..."}}]}`
///    → extract content string → parse JSON from it
/// 2. **Codex proxy `agent_text`**: `{"ok": true, "mcp_turn": true, "agent_text": "..."}`
///    → extract JSON from agent_text string
/// 3. **Raw domain JSON**: response IS the JSON object directly (has domain-specific
///    fields like `format_type`, `recommendation`, `synthese_marche`, etc.)
///    → return as-is (excludes bare `{"ok": true, "mcp_turn": true}` markers)
/// 4. **Content envelope**: `{"content": "..."}` → extract JSON from content string
pub(crate) fn extract_draft_from_response(response: &Value) -> Result<Value> {
    // Format 1: OpenAI Chat Completions — choices[0].message.content
    if let Some(content) = response
        .get("choices")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|v| v.as_str())
    {
        return extract_json_object(content)
            .ok_or_else(|| anyhow!("llm_invalid_json_response"));
    }

    // Format 2: Codex proxy with agent_text wrapper
    if let Some(text) = response.get("agent_text").and_then(|v| v.as_str()) {
        return extract_json_object(text)
            .ok_or_else(|| anyhow!("llm_no_json_in_agent_text"));
    }

    // Format 3: Raw domain JSON — the response IS the parsed object.
    // Exclude bare success markers that carry no domain data.
    if response.is_object() {
        let is_bare_marker = response.get("ok").is_some()
            && response.as_object().map_or(true, |m| {
                m.keys().all(|k| matches!(k.as_str(), "ok" | "mcp_turn" | "agent_text_chars"))
            });
        if !is_bare_marker {
            return Ok(response.clone());
        }
    }

    // Format 4: Content envelope — {"content": "..."}
    if let Some(content) = response.get("content").and_then(|v| v.as_str()) {
        return extract_json_object(content)
            .ok_or_else(|| anyhow!("llm_invalid_json_in_content"));
    }

    Err(anyhow!("llm_no_content_in_response"))
}

pub(crate) fn extract_recommendation_from_response(response: &Value) -> Result<Value> {
    let draft = extract_draft_from_response(response)?;
    if let Some(rec) = draft.get("recommendation") {
        Ok(rec.clone())
    } else {
        Ok(draft)
    }
}

pub(crate) fn extract_watchlist_from_response(response: &Value) -> Result<Vec<Value>> {
    // Try various locations where the watchlist JSON might be
    if let Some(arr) = response.get("watchlist").and_then(|v| v.as_array()) {
        return Ok(arr.clone());
    }
    if let Some(arr) = response.get("draft").and_then(|d| d.get("watchlist")).and_then(|v| v.as_array()) {
        return Ok(arr.clone());
    }
    // Try parsing content string as JSON
    if let Some(content) = response.get("content").and_then(|v| v.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(content) {
            if let Some(arr) = parsed.get("watchlist").and_then(|v| v.as_array()) {
                return Ok(arr.clone());
            }
        }
    }
    // Try response itself if it contains a watchlist array
    if let Some(arr) = response.as_array() {
        return Ok(arr.clone());
    }
    eprintln!("[watchlist] Could not extract watchlist from response: {:?}",
        serde_json::to_string(response).unwrap_or_default().chars().take(500).collect::<String>());
    Ok(Vec::new())
}

// ── JSON extraction ──────────────────────────────────────────────

pub(crate) fn extract_json_object(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Some(value);
        }
    }
    // Try markdown fence extraction
    for fence in ["```json", "```"] {
        if let Some(start) = trimmed.find(fence) {
            let content_start = start + fence.len();
            if let Some(end) = trimmed[content_start..].find("```") {
                let candidate = trimmed[content_start..content_start + end].trim();
                if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                    return Some(value);
                }
            }
        }
    }
    // Brace matching
    let first_brace = trimmed.find('{')?;
    let last_brace = trimmed.rfind('}')?;
    if first_brace < last_brace {
        if let Ok(value) = serde_json::from_str::<Value>(&trimmed[first_brace..=last_brace]) {
            return Some(value);
        }
    }
    None
}

// ── Repair pass detection ────────────────────────────────────────

pub(crate) fn is_repair_pass(validation_context: Option<&Value>) -> bool {
    validation_context
        .and_then(|vc| vc.get("analysis_mode"))
        .and_then(|v| v.as_str())
        .map(|m| m == "validation_repair" || m == "repair_websearch")
        .unwrap_or(false)
}

// ── Insights persistence ─────────────────────────────────────────

/// Extract generic insights from a recommendation and persist them to the shared cache.
pub(crate) fn persist_shared_insights_if_present(recommendation: &Value, line_context: &Value) {
    let generic_fields = [
        "analyse_technique", "analyse_fondamentale", "analyse_sentiment",
        "deep_news_summary", "badges_keywords", "risques", "catalyseurs",
    ];
    let mut insights = serde_json::Map::new();
    for field in &generic_fields {
        if let Some(val) = recommendation.get(*field) {
            let is_nonempty = match val {
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                _ => false,
            };
            if is_nonempty {
                insights.insert(field.to_string(), val.clone());
            }
        }
    }
    if insights.is_empty() {
        return;
    }
    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
    let isin = line_context
        .get("row")
        .and_then(|r| r.get("isin"))
        .and_then(|v| v.as_str())
        .unwrap_or(ticker);
    // Extract sector from line_context if available (set by get_line_data)
    let sector = line_context.get("sector").and_then(|v| v.as_str());
    let sector_analysis = recommendation.get("sector_analysis").and_then(|v| v.as_str());
    crate::alfred_api_client::persist_shared_insights(ticker, isin, &Value::Object(insights), sector, sector_analysis);
}

/// Persist the deep news summary to the per-URL API cache.
/// Picks the first un-cached news article URL from context as the key.
pub(crate) fn persist_deep_news_if_present(recommendation: &Value, line_context: &Value) {
    let summary = recommendation.get("deep_news_summary")
        .and_then(|v| v.as_str()).unwrap_or("");
    if summary.is_empty() { return; }

    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
    let isin = line_context.get("row")
        .and_then(|r| r.get("isin")).and_then(|v| v.as_str()).unwrap_or(ticker);
    if ticker.is_empty() { return; }

    // Find the best article URL to associate this summary with.
    // Prefer un-cached articles (deep_summary_cached=false) since those are the ones the LLM just read.
    let news = line_context.get("news").or_else(|| line_context.get("market").and_then(|m| m.get("news")));
    let articles = news.and_then(|n| {
        n.as_array()
            .or_else(|| n.get("items").and_then(|i| i.as_array()))
            .or_else(|| n.get("articles").and_then(|i| i.as_array()))
    });

    let mut best_url = String::new();
    let mut best_title = String::new();

    if let Some(arr) = articles {
        // First pass: find an un-cached article
        for item in arr {
            let is_cached = item.get("deep_summary_cached").and_then(|v| v.as_bool()).unwrap_or(false);
            if !is_cached {
                if let Some(url) = item.get("url").or_else(|| item.get("link")).and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        best_url = url.to_string();
                        best_title = item.get("title").or_else(|| item.get("titre"))
                            .and_then(|v| v.as_str()).unwrap_or("").to_string();
                        break;
                    }
                }
            }
        }
        // Fallback: use first article with a URL
        if best_url.is_empty() {
            for item in arr {
                if let Some(url) = item.get("url").or_else(|| item.get("link")).and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        best_url = url.to_string();
                        best_title = item.get("title").or_else(|| item.get("titre"))
                            .and_then(|v| v.as_str()).unwrap_or("").to_string();
                        break;
                    }
                }
            }
        }
    }

    if best_url.is_empty() { return; }

    let quality_score = recommendation.get("deep_news_quality_score")
        .and_then(|v| v.as_u64()).unwrap_or(50);
    let relevance = recommendation.get("deep_news_relevance")
        .and_then(|v| v.as_str()).unwrap_or("medium");
    let staleness = recommendation.get("deep_news_staleness")
        .and_then(|v| v.as_str()).unwrap_or("recent");

    crate::alfred_api_client::persist_deep_news_summary(
        ticker, isin, &best_url, &best_title, summary,
        quality_score, relevance, staleness,
    );
}

/// If the LLM returned extracted_fundamentals, persist them to the API cache.
pub(crate) fn persist_extracted_fundamentals_if_present(recommendation: &Value, line_context: &Value) {
    if let Some(extracted) = recommendation.get("extracted_fundamentals") {
        if extracted.is_object() && !extracted.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
            let isin = line_context
                .get("row")
                .and_then(|r| r.get("isin"))
                .and_then(|v| v.as_str())
                .unwrap_or(ticker);
            crate::alfred_api_client::persist_extracted_fundamentals(ticker, isin, extracted);
        }
    }
}
