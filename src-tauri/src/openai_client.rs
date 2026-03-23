//! Native OpenAI API client with agentic tool-use loop.
//!
//! Replaces Codex app-server for users who prefer direct API access.
//! Calls chat completions with tool definitions, executes tool calls
//! via `mcp_server::dispatch_tool_direct()`, and loops until the model
//! returns a final text response.

use std::env;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::llm_backend::ProgressFn;

const DEFAULT_MODEL: &str = "gpt-4.1";
const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const MAX_TOOL_ROUNDS: usize = 30;
const MAX_RETRIES: usize = 4;
const RETRY_BASE_MS: u64 = 500;

// ── Configuration ─────────────────────────────────────────────────

fn api_key() -> Result<String> {
    // 1. Env var
    if let Ok(key) = env::var("OPENAI_API_KEY") {
        let k = key.trim().to_string();
        if !k.is_empty() {
            return Ok(k);
        }
    }
    // 2. User preferences
    if let Ok(prefs) = crate::runtime_settings::string_direct("openai_api_key") {
        let k = prefs.trim().to_string();
        if !k.is_empty() {
            return Ok(k);
        }
    }
    Err(anyhow!("openai_api_key_missing:set OPENAI_API_KEY or configure in settings"))
}

fn api_base() -> String {
    if let Ok(v) = env::var("OPENAI_API_BASE") {
        let t = v.trim().to_string();
        if !t.is_empty() {
            return t.trim_end_matches('/').to_string();
        }
    }
    if let Ok(v) = crate::runtime_settings::string_direct("openai_api_base") {
        let t = v.trim().to_string();
        if !t.is_empty() {
            return t.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_API_BASE.to_string()
}

fn model_name() -> String {
    if let Ok(m) = env::var("ALFRED_MODEL") {
        let t = m.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    if let Ok(m) = crate::runtime_settings::string_direct("openai_model") {
        let t = m.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    DEFAULT_MODEL.to_string()
}

fn data_dir() -> PathBuf {
    crate::paths::resolve_runtime_state_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| crate::paths::default_data_dir())
}

// ── Tool Definitions (OpenAI function calling format) ─────────────

fn tool_definitions() -> Vec<Value> {
    crate::mcp_server::tool_definitions_openai()
}

// ── Public API ────────────────────────────────────────────────────

/// Execute a prompt with the native OpenAI client.
/// Implements the agentic tool-use loop: prompt → tool_calls → execute → repeat.
pub fn run_prompt(
    prompt: &str,
    timeout_ms: u64,
    on_progress: Option<ProgressFn>,
) -> Result<Value> {
    let key = api_key()?;
    let base = api_base();
    let model = model_name();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let tools = tool_definitions();
    let dd = data_dir();

    crate::debug_log(&format!(
        "openai_client: model={model} tools={} timeout={timeout_ms}ms",
        tools.len()
    ));

    // Build initial messages
    let mut messages: Vec<Value> = vec![
        json!({
            "role": "user",
            "content": prompt
        }),
    ];

    let mut round = 0;
    loop {
        round += 1;
        if round > MAX_TOOL_ROUNDS {
            return Err(anyhow!("openai_client:max_tool_rounds_exceeded:{MAX_TOOL_ROUNDS}"));
        }
        if Instant::now() > deadline {
            return Err(anyhow!("openai_client:timeout_exceeded:{timeout_ms}ms"));
        }

        if let Some(ref cb) = on_progress {
            cb(0, round, &format!("round {round}\u{2026}"));
        }

        // Call chat completions
        // Include both function tools and native web search
        let mut all_tools: Vec<Value> = tools
            .iter()
            .map(|t| json!({"type": "function", "function": t}))
            .collect();
        // Native web search — lets the model search and read pages directly.
        // Only works with OpenAI models; silently ignored by other providers.
        all_tools.push(json!({"type": "web_search_preview"}));

        let body = json!({
            "model": model,
            "messages": messages,
            "tools": all_tools,
            "temperature": 0.3,
            "stream": true,
        });

        let response_text = call_chat_completions_streamed(
            &base, &key, &body, &on_progress, deadline,
        )?;

        // Parse the accumulated response
        let assistant_msg = parse_streamed_response(&response_text)?;

        // Add assistant message to history
        messages.push(assistant_msg.clone());

        // Check for tool calls
        let tool_calls = assistant_msg
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // No tool calls — model returned final text
            let content = assistant_msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            crate::debug_log(&format!(
                "openai_client: completed in {round} rounds, {} chars",
                content.len()
            ));

            // Parse JSON from the content
            return extract_json_result(content);
        }

        // Execute tool calls and add results to messages
        for tc in &tool_calls {
            let call_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let empty = json!({});
            let func = tc.get("function").unwrap_or(&empty);
            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            let args_str = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

            if let Some(ref cb) = on_progress {
                cb(0, round, &format!("tool:{name}"));
            }

            crate::debug_log(&format!("openai_client: calling tool {name}"));

            // Execute tool directly — no IPC, no subprocess
            let tool_result = crate::mcp_server::dispatch_tool_direct(&dd, name, &args);
            let result_text = tool_result_to_string(&tool_result);

            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_text,
            }));
        }
    }
}

/// Validate the API key by calling the models endpoint.
pub fn validate_api_key() -> Result<Value> {
    let key = api_key()?;
    let base = api_base();

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();

    let resp = agent
        .get(&format!("{base}/models"))
        .set("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| anyhow!("openai_api_key_invalid:{e}"))?;

    let body: Value = resp.into_json().unwrap_or(json!({}));
    let model_count = body
        .get("data")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    Ok(json!({
        "ok": true,
        "backend": "native",
        "models_available": model_count,
        "model": model_name(),
    }))
}

// ── Streaming chat completions ────────────────────────────────────

fn call_chat_completions_streamed(
    base: &str,
    key: &str,
    body: &Value,
    on_progress: &Option<ProgressFn>,
    deadline: Instant,
) -> Result<String> {
    let url = format!("{base}/chat/completions");
    let body_str = serde_json::to_string(body)?;

    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        if Instant::now() > deadline {
            break;
        }
        if attempt > 0 {
            let delay = RETRY_BASE_MS * (1u64 << attempt.min(4));
            std::thread::sleep(Duration::from_millis(delay));
            crate::debug_log(&format!("openai_client: retry {attempt}/{MAX_RETRIES}"));
        }

        let remaining = deadline.duration_since(Instant::now());
        let agent = ureq::AgentBuilder::new()
            .timeout(remaining.min(Duration::from_secs(300)))
            .build();

        let resp = match agent
            .post(&url)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .send_string(&body_str)
        {
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                if status == 429 || status >= 500 {
                    last_err = Some(anyhow!("openai_api_error:{status}:{body}"));
                    continue; // retryable
                }
                return Err(anyhow!("openai_api_error:{status}:{body}"));
            }
            Err(e) => {
                last_err = Some(anyhow!("openai_api_request_failed:{e}"));
                continue; // retryable
            }
        };

        // Read SSE stream
        let reader = BufReader::new(resp.into_reader());
        let mut accumulated = String::new();
        let mut bytes_received: usize = 0;

        for line in reader.lines() {
            let line = line.map_err(|e| anyhow!("openai_sse_read_error:{e}"))?;

            if !line.starts_with("data: ") {
                continue;
            }
            let data = &line[6..];
            if data == "[DONE]" {
                break;
            }

            bytes_received += line.len();
            accumulated.push_str(data);
            accumulated.push('\n');

            // Extract delta text for progress
            if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                if let Some(delta_text) = chunk
                    .pointer("/choices/0/delta/content")
                    .and_then(|v| v.as_str())
                {
                    if let Some(ref cb) = on_progress {
                        cb(bytes_received, 0, delta_text);
                    }
                }
            }
        }

        return Ok(accumulated);
    }

    Err(last_err.unwrap_or_else(|| anyhow!("openai_api_retries_exhausted")))
}

// ── Response parsing ──────────────────────────────────────────────

/// Reassemble a streamed SSE response into a single assistant message.
fn parse_streamed_response(accumulated: &str) -> Result<Value> {
    let mut content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    // Track tool call assembly: index → {id, name, arguments_buf}
    let mut tc_map: std::collections::HashMap<u64, (String, String, String)> =
        std::collections::HashMap::new();

    for line in accumulated.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let chunk: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let delta = match chunk.pointer("/choices/0/delta") {
            Some(d) => d,
            None => continue,
        };

        // Content delta
        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
            content.push_str(text);
        }

        // Tool call deltas
        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let entry = tc_map.entry(idx).or_insert_with(|| {
                    let id = tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = tc
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    (id, name, String::new())
                });
                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                }
                if let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) {
                    if !name.is_empty() {
                        entry.1 = name.to_string();
                    }
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(|v| v.as_str()) {
                    entry.2.push_str(args);
                }
            }
        }
    }

    // Assemble tool calls
    let mut indices: Vec<u64> = tc_map.keys().copied().collect();
    indices.sort();
    for idx in indices {
        let (id, name, args) = tc_map.remove(&idx).unwrap();
        tool_calls.push(json!({
            "id": id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": args,
            }
        }));
    }

    let mut msg = json!({
        "role": "assistant",
        "content": if content.is_empty() { Value::Null } else { Value::String(content) },
    });

    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }

    Ok(msg)
}

/// Extract JSON from the model's final text output.
fn extract_json_result(text: &str) -> Result<Value> {
    // Try direct parse
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    // Try extracting from markdown fences
    if let Some(start) = text.find("```json") {
        let after = &text[start + 7..];
        if let Some(end) = after.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(after[..end].trim()) {
                return Ok(v);
            }
        }
    }
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(end) = after.find("```") {
            if let Ok(v) = serde_json::from_str::<Value>(after[..end].trim()) {
                return Ok(v);
            }
        }
    }
    // Try finding first { ... } or [ ... ]
    if let Some(v) = crate::llm_parsing::extract_json_object(text) {
        return Ok(v);
    }
    Err(anyhow!("openai_client:no_json_in_response"))
}

/// Convert a tool dispatch result to a string for the chat message.
fn tool_result_to_string(result: &Value) -> String {
    // dispatch_tool_direct returns the raw result Value
    if let Some(text) = result.as_str() {
        text.to_string()
    } else {
        serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string())
    }
}
