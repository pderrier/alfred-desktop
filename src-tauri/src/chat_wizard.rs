//! Chat wizard — lightweight LLM chat for user-facing decision flows.
//!
//! Provides a simple multi-turn conversation interface. The frontend sends
//! a system context (describing the decision to make) plus the user's message,
//! and receives the LLM's text response. No streaming — just a quick text
//! exchange suitable for short wizard interactions, while keeping the same
//! backend/tool behavior as the main analysis runtime.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

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
    // Route through the regular backend pipeline for all backends.
    let backend = crate::llm_backend::current_backend_name();
    crate::debug_log(&format!(
        "chat_wizard: sending via {backend}, {} history messages",
        history.len()
    ));

    let response_text = call_via_backend(&context, &history, &user_message)?;

    Ok(json!({ "response": response_text }))
}

fn build_backend_prompt(context: &str, history: &[Value], user_message: &str) -> String {
    let mut prompt = format!("System: {context}\n\n");
    for msg in history {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        prompt.push_str(&format!("{role}: {content}\n\n"));
    }
    prompt.push_str(&format!("user: {user_message}\n\nassistant:"));
    prompt
}

fn extract_chat_text(result: &Value) -> Result<String> {
    // 1. OpenAI format: choices[0].message.content
    if let Some(text) = result
        .get("choices")
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

fn call_via_backend_with_runner<F>(
    context: &str,
    history: &[Value],
    user_message: &str,
    run_prompt: F,
) -> Result<String>
where
    F: Fn(&str, u64) -> Result<Value>,
{
    let prompt = build_backend_prompt(context, history, user_message);

    let timeout_ms: u64 = std::env::var("CODEX_PROXY_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30_000);

    let result = run_prompt(&prompt, timeout_ms)?;
    extract_chat_text(&result)
}

/// Route chat wizard turns through the same backend abstraction as analysis.
fn call_via_backend(context: &str, history: &[Value], user_message: &str) -> Result<String> {
    call_via_backend_with_runner(context, history, user_message, |prompt, timeout_ms| {
        crate::llm_backend::run_prompt(prompt, timeout_ms, None)
    })
}

#[cfg(test)]
mod tests {
    use super::{build_backend_prompt, call_via_backend_with_runner, extract_chat_text};
    use serde_json::json;

    #[test]
    fn build_backend_prompt_preserves_roles_and_order() {
        let prompt = build_backend_prompt(
            "You are a helper.",
            &[
                json!({"role":"assistant","content":"Hello"}),
                json!({"role":"user","content":"Need context"}),
            ],
            "What changed?",
        );

        assert!(prompt.starts_with("System: You are a helper."));
        assert!(prompt.contains("assistant: Hello"));
        assert!(prompt.contains("user: Need context"));
        assert!(prompt.ends_with("user: What changed?\n\nassistant:"));
    }

    #[test]
    fn extract_chat_text_handles_openai_shape() {
        let v = json!({"choices":[{"message":{"content":"ok"}}]});
        assert_eq!(extract_chat_text(&v).expect("text"), "ok");
    }

    #[test]
    fn extract_chat_text_handles_agent_text_shape() {
        let v = json!({"ok":true,"mcp_turn":true,"agent_text":"tool answer"});
        assert_eq!(extract_chat_text(&v).expect("text"), "tool answer");
    }

    #[test]
    fn extract_chat_text_handles_plain_string_shape() {
        let v = json!("plain");
        assert_eq!(extract_chat_text(&v).expect("text"), "plain");
    }

    #[test]
    fn call_via_backend_with_runner_handles_openai_protocol_shape() {
        let history = vec![json!({"role":"assistant","content":"Hi"})];
        let result =
            call_via_backend_with_runner("ctx", &history, "question", |prompt, _timeout| {
                assert!(prompt.contains("System: ctx"));
                assert!(prompt.contains("assistant: Hi"));
                Ok(json!({"choices":[{"message":{"content":"openai-like"}}]}))
            })
            .expect("response");

        assert_eq!(result, "openai-like");
    }

    #[test]
    fn call_via_backend_with_runner_handles_codex_protocol_shape() {
        let result = call_via_backend_with_runner("ctx", &[], "question", |_prompt, _timeout| {
            Ok(json!({"ok":true,"mcp_turn":true,"agent_text":"codex-like"}))
        })
        .expect("response");

        assert_eq!(result, "codex-like");
    }

    #[test]
    fn call_via_backend_with_runner_handles_app_server_plain_text_shape() {
        let result = call_via_backend_with_runner("ctx", &[], "question", |_prompt, _timeout| {
            Ok(json!("appserver-like"))
        })
        .expect("response");

        assert_eq!(result, "appserver-like");
    }
}
