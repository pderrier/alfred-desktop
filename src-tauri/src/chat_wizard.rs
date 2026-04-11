//! Chat wizard — lightweight LLM chat for user-facing decision flows.
//!
//! Provides a simple multi-turn conversation interface. The frontend sends
//! a system context (describing the decision to make) plus the user's message,
//! and receives the LLM's text response. No tool use, no streaming — just a
//! quick text exchange suitable for short wizard interactions.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Tauri command: send a message to the LLM in a chat wizard context.
///
/// `context` — system prompt describing the decision (e.g., cash account matching)
/// `history` — array of `{ role, content }` objects (prior messages in this conversation)
/// `user_message` — the latest user input
///
/// Returns `{ "response": "..." }` with the assistant's reply.
pub fn chat_wizard_send_impl(
    context: String,
    history: Vec<Value>,
    user_message: String,
) -> Result<Value> {
    // Build messages array: system + history + user
    let mut messages: Vec<Value> = Vec::new();
    messages.push(json!({ "role": "system", "content": context }));
    for msg in &history {
        messages.push(msg.clone());
    }
    messages.push(json!({ "role": "user", "content": user_message }));

    // Try native OpenAI first, then codex backend
    let backend = crate::llm_backend::current_backend_name();
    crate::debug_log(&format!("chat_wizard: sending via {backend}, {} history messages", history.len()));

    let response_text = match backend.as_str() {
        "native" | "openai" => call_openai_chat(&messages)?,
        "native-oauth" => call_via_app_server_chat(&context, &history, &user_message)?,
        _ => call_via_backend(&context, &history, &user_message)?,
    };

    Ok(json!({ "response": response_text }))
}

/// Direct OpenAI Chat Completions call (simple, no tools, no streaming).
fn call_openai_chat(messages: &[Value]) -> Result<String> {
    let key = std::env::var("OPENAI_API_KEY")
        .or_else(|_| crate::runtime_settings::string_direct("openai_api_key"))
        .map_err(|_| anyhow!("openai_api_key_missing"))?;
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(anyhow!("openai_api_key_missing"));
    }

    let base = std::env::var("OPENAI_API_BASE")
        .or_else(|_| crate::runtime_settings::string_direct("openai_api_base"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let base = base.trim().trim_end_matches('/').to_string();

    let model = std::env::var("ALFRED_MODEL")
        .or_else(|_| crate::runtime_settings::string_direct("openai_model"))
        .unwrap_or_else(|_| "gpt-4.1-mini".to_string());
    let model = model.trim().to_string();

    let body = json!({
        "model": model,
        "temperature": 0.3,
        "max_tokens": 2048,
        "messages": messages
    });

    let url = format!("{base}/chat/completions");
    let response = ureq::post(&url)
        .timeout(Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .send_json(&body)
        .map_err(|e| anyhow!("chat_wizard_openai_failed:{e}"))?;

    let parsed: Value = response.into_json()
        .map_err(|e| anyhow!("chat_wizard_parse_failed:{e}"))?;

    let content = parsed
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();

    if content.is_empty() {
        return Err(anyhow!("chat_wizard_empty_response"));
    }

    Ok(content)
}

/// Native-oauth chat: flatten conversation and send via app-server (no API key needed).
fn call_via_app_server_chat(context: &str, history: &[Value], user_message: &str) -> Result<String> {
    let mut prompt = format!("System: {context}\n\n");
    for msg in history {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        prompt.push_str(&format!("{role}: {content}\n\n"));
    }
    prompt.push_str(&format!("user: {user_message}\n\nassistant:"));

    let text = crate::codex::run_simple_prompt(&prompt, None)?;
    if text.trim().is_empty() {
        return Err(anyhow!("chat_wizard_empty_response"));
    }
    Ok(text)
}

/// Fallback: flatten conversation into a single prompt for the codex backend.
fn call_via_backend(context: &str, history: &[Value], user_message: &str) -> Result<String> {
    let mut prompt = format!("System: {context}\n\n");
    for msg in history {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        prompt.push_str(&format!("{role}: {content}\n\n"));
    }
    prompt.push_str(&format!("user: {user_message}\n\nassistant:"));

    let timeout_ms: u64 = std::env::var("CODEX_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);

    let result = crate::llm_backend::run_prompt(&prompt, timeout_ms, None)?;

    // Extract text from the response — try multiple shapes:
    // 1. OpenAI format: choices[0].message.content
    if let Some(text) = result.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|msg| msg.get("content"))
        .and_then(|c| c.as_str())
    {
        return Ok(text.to_string());
    }
    // 2. Codex MCP turn wraps natural language in agent_text
    if let Some(text) = result.get("agent_text").and_then(|v| v.as_str()) {
        return Ok(text.to_string());
    }
    // 3. Plain string result
    if let Some(text) = result.as_str() {
        return Ok(text.to_string());
    }
    // 4. Fallback — should not happen, but better than raw JSON
    Err(anyhow!("chat_wizard_unexpected_response_shape"))
}
