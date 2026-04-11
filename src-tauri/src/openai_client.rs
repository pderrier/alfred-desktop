//! Native OpenAI Responses API client with agentic tool-use loop.
//!
//! Uses the Responses API (`/v1/responses`) which supports both function
//! tools and native web search. The model decides when to search.
//! Tool calls are executed locally via `mcp_server::dispatch_tool_direct()`.

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
    if let Ok(key) = env::var("OPENAI_API_KEY") {
        let k = key.trim().to_string();
        if !k.is_empty() {
            return Ok(k);
        }
    }
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

/// Resolve model: explicit override > user setting > auto-detect best available.
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
    // Auto-detect: query /v1/models and pick the best one
    if let Ok(best) = resolve_best_model() {
        return best;
    }
    DEFAULT_MODEL.to_string()
}

/// Query /v1/models and pick the best available model.
/// Same ranking logic as the Codex backend: gpt-5.x > o4 > o3 > gpt-4.x
fn resolve_best_model() -> Result<String> {
    let key = api_key()?;
    let base = api_base();

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();

    let resp = agent
        .get(&format!("{base}/models"))
        .set("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| anyhow!("model_list_failed:{e}"))?;

    let body: Value = resp.into_json().unwrap_or(json!({}));
    let models: Vec<String> = body
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    if models.is_empty() {
        return Err(anyhow!("no_models_available"));
    }

    let mut best: Option<&str> = None;
    let mut best_score: i32 = -1;

    for model in &models {
        let score = model_score(model);
        if score > best_score {
            best_score = score;
            best = Some(model);
        }
    }

    let selected = best.unwrap_or(&models[0]).to_string();
    crate::debug_log(&format!("openai_client: auto-selected model {selected} (score={best_score}, {} available)", models.len()));
    Ok(selected)
}

/// Score a model name for ranking. Higher is better.
fn model_score(name: &str) -> i32 {
    if name.starts_with("gpt-5") {
        let version: f32 = name
            .strip_prefix("gpt-")
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(5.0);
        (version * 10.0) as i32
    } else if name.starts_with("gpt-4.1") {
        41
    } else if name.starts_with("o4") {
        40
    } else if name.starts_with("o3") {
        30
    } else if name.starts_with("gpt-4") {
        20
    } else {
        0
    }
}

/// Whether a model is a reasoning model (o-series) that doesn't support temperature.
fn is_reasoning_model(name: &str) -> bool {
    name.starts_with("o1") || name.starts_with("o3") || name.starts_with("o4")
}

fn data_dir() -> PathBuf {
    crate::paths::resolve_runtime_state_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| crate::paths::default_data_dir())
}

// ── Tool definitions (Responses API format) ───────────────────────

fn build_tools() -> Vec<Value> {
    let mut tools: Vec<Value> = crate::mcp_server::tool_definitions_openai()
        .into_iter()
        .map(|t| json!({
            "type": "function",
            "name": t.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
            "description": t.get("description").and_then(|v| v.as_str()).unwrap_or_default(),
            "parameters": t.get("parameters").cloned().unwrap_or(json!({"type": "object", "properties": {}})),
        }))
        .collect();
    // Native web search — model decides when to search
    tools.push(json!({"type": "web_search_preview"}));
    tools
}

// ── Public API ────────────────────────────────────────────────────

/// Execute a prompt via the Responses API with tool-use loop.
/// Sends prompt → model returns output items → execute function_call items
/// → send tool results as new input → repeat until model returns message.
pub fn run_prompt(
    prompt: &str,
    timeout_ms: u64,
    on_progress: Option<ProgressFn>,
) -> Result<Value> {
    let key = api_key()?;
    let base = api_base();
    let model = model_name();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let tools = build_tools();
    let dd = data_dir();

    crate::debug_log(&format!(
        "openai_client: responses api, model={model} tools={} timeout={timeout_ms}ms",
        tools.len()
    ));

    // First request: send the user prompt
    let mut input: Vec<Value> = vec![json!({
        "role": "user",
        "content": prompt,
    })];
    let mut previous_response_id: Option<String> = None;

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

        // Build request body — adapt parameters per model family
        let mut body = json!({
            "model": model,
            "input": input,
            "tools": tools,
            "stream": true,
        });
        if is_reasoning_model(&model) {
            // Reasoning models (o-series): no temperature, use reasoning effort instead
            body["reasoning"] = json!({"effort": "medium"});
        } else {
            body["temperature"] = json!(0.3);
        }
        if let Some(ref prev_id) = previous_response_id {
            body["previous_response_id"] = json!(prev_id);
        }

        // Call Responses API with streaming
        let (response_id, output_items) = call_responses_streamed(
            &base, &key, &body, &on_progress, deadline,
        )?;

        previous_response_id = Some(response_id);

        // Process output items — collect function calls and final text
        let mut function_calls: Vec<(String, String, String)> = Vec::new(); // (call_id, name, arguments)
        let mut final_text = String::new();

        for item in &output_items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or_default();
            match item_type {
                "function_call" => {
                    let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}").to_string();
                    function_calls.push((call_id, name, arguments));
                }
                "message" => {
                    // Extract text from content array
                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                        for part in content {
                            if part.get("type").and_then(|v| v.as_str()) == Some("output_text") {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    final_text.push_str(text);
                                }
                            }
                        }
                    }
                }
                "web_search_call" => {
                    // Web search executed by OpenAI — logged but no action needed
                    let status = item.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
                    crate::debug_log(&format!("openai_client: web_search_call status={status}"));
                    if let Some(ref cb) = on_progress {
                        cb(0, round, "searching the web\u{2026}");
                    }
                }
                _ => {
                    crate::debug_log(&format!("openai_client: unknown output item type={item_type}"));
                }
            }
        }

        // If no function calls, we have the final response
        if function_calls.is_empty() {
            crate::debug_log(&format!(
                "openai_client: completed in {round} rounds, {} chars",
                final_text.len()
            ));
            // Same behavior as Codex: try JSON extraction, fall back to success marker
            match extract_json_result(&final_text) {
                Ok(v) => return Ok(v),
                Err(_) => return Ok(json!({"ok": true, "mcp_turn": true, "agent_text_chars": final_text.len()})),
            }
        }

        // Execute function calls and build input for next round
        input = Vec::new();
        for (call_id, name, arguments) in &function_calls {
            if let Some(ref cb) = on_progress {
                cb(0, round, &format!("tool:{name}"));
            }
            crate::debug_log(&format!("openai_client: calling tool {name}"));

            let args: Value = serde_json::from_str(arguments).unwrap_or(json!({}));
            let tool_result = crate::mcp_server::dispatch_tool_direct(&dd, &name, &args);
            let result_str = if let Some(s) = tool_result.as_str() {
                s.to_string()
            } else {
                serde_json::to_string(&tool_result).unwrap_or_else(|_| "{}".to_string())
            };

            input.push(json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": result_str,
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

// ── Streaming Responses API call ──────────────────────────────────

/// Call POST /v1/responses with SSE streaming.
/// Returns (response_id, output_items).
fn call_responses_streamed(
    base: &str,
    key: &str,
    body: &Value,
    on_progress: &Option<ProgressFn>,
    deadline: Instant,
) -> Result<(String, Vec<Value>)> {
    let url = format!("{base}/responses");
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
                let err_body = resp.into_string().unwrap_or_default();
                if status == 429 || status >= 500 {
                    last_err = Some(anyhow!("openai_api_error:{status}:{err_body}"));
                    continue;
                }
                return Err(anyhow!("openai_api_error:{status}:{err_body}"));
            }
            Err(e) => {
                last_err = Some(anyhow!("openai_api_request_failed:{e}"));
                continue;
            }
        };

        // Parse SSE stream
        let reader = BufReader::new(resp.into_reader());
        let mut response_id = String::new();
        let mut output_items: Vec<Value> = Vec::new();

        // Accumulate streamed text deltas per output item index
        let mut text_bufs: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        let mut reasoning_buf = String::new();
        // Accumulate function_call argument deltas per output item index
        let mut fn_arg_bufs: std::collections::HashMap<u64, (String, String, String)> =
            std::collections::HashMap::new(); // index → (call_id, name, args_buf)

        for line in reader.lines() {
            let line = line.map_err(|e| anyhow!("openai_sse_read_error:{e}"))?;

            if !line.starts_with("data: ") {
                continue;
            }
            let data = &line[6..];
            if data == "[DONE]" {
                break;
            }

            let event: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or_default();

            match event_type {
                // Response created — capture ID
                "response.created" | "response.completed" => {
                    if let Some(resp_obj) = event.get("response") {
                        if let Some(id) = resp_obj.get("id").and_then(|v| v.as_str()) {
                            response_id = id.to_string();
                        }
                        if event_type == "response.completed" {
                            if let Some(output) = resp_obj.get("output").and_then(|v| v.as_array()) {
                                output_items = output.clone();
                            }
                            // Extract token usage and emit via progress callback
                            if let Some(usage) = resp_obj.get("usage") {
                                let total = usage.get("total_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                let output_t = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                if total > 0 {
                                    if let Some(ref cb) = on_progress {
                                        cb(0, 0, &format!("tokens:{total}:{input}:{output_t}"));
                                    }
                                }
                            }
                        }
                    }
                }

                // Output item completed — add to our list
                "response.output_item.done" => {
                    if let Some(item) = event.get("item") {
                        // For function_call items, merge accumulated arguments
                        let idx = event.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                        if let Some((call_id, name, args)) = fn_arg_bufs.remove(&idx) {
                            let mut item = item.clone();
                            if item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().is_empty() {
                                item["call_id"] = json!(call_id);
                            }
                            if item.get("name").and_then(|v| v.as_str()).unwrap_or_default().is_empty() {
                                item["name"] = json!(name);
                            }
                            if item.get("arguments").and_then(|v| v.as_str()).unwrap_or_default().is_empty() {
                                item["arguments"] = json!(args);
                            }
                            output_items.push(item);
                        } else {
                            output_items.push(item.clone());
                        }
                    }
                }

                // Text deltas — accumulate silently (final JSON output, not useful for progress)
                "response.output_text.delta" => {
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        let idx = event.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let buf = text_bufs.entry(idx).or_default();
                        buf.push_str(delta);
                        // Show a periodic "writing..." indicator (every ~500 chars)
                        if buf.len() % 500 < delta.len() {
                            if let Some(ref cb) = on_progress {
                                cb(0, 0, &format!("writing ({:.1}kB)\u{2026}", buf.len() as f64 / 1024.0));
                            }
                        }
                    }
                }

                // Reasoning tokens (o-series models) — periodic summary, not every token
                "response.reasoning.delta" => {
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        reasoning_buf.push_str(delta);
                        // Show thinking indicator every ~200 chars
                        if reasoning_buf.len() % 200 < delta.len() {
                            if let Some(ref cb) = on_progress {
                                // Show last ~60 chars of reasoning as preview
                                let preview: String = reasoning_buf.chars().rev().take(60).collect::<String>().chars().rev().collect();
                                cb(0, 0, &format!("thinking: {preview}\u{2026}"));
                            }
                        }
                    }
                }

                // Function call argument deltas
                "response.function_call_arguments.delta" => {
                    let idx = event.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        fn_arg_bufs.entry(idx).or_insert_with(|| (String::new(), String::new(), String::new())).2.push_str(delta);
                    }
                }
                "response.function_call_arguments.done" => {
                    let idx = event.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                    // call_id and name come from output_item.added
                    if let Some(args) = event.get("arguments").and_then(|v| v.as_str()) {
                        let entry = fn_arg_bufs.entry(idx).or_insert_with(|| (String::new(), String::new(), String::new()));
                        entry.2 = args.to_string(); // replace with final complete args
                    }
                }

                // Output item added — capture call_id and name for function calls
                "response.output_item.added" => {
                    if let Some(item) = event.get("item") {
                        let idx = event.get("output_index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or_default();
                        if item_type == "function_call" {
                            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                            fn_arg_bufs.entry(idx).or_insert_with(|| (call_id, name, String::new()));
                        }
                    }
                }

                // Web search status
                "response.web_search_call.in_progress" | "response.web_search_call.searching" => {
                    if let Some(ref cb) = on_progress {
                        cb(0, 0, "searching the web\u{2026}");
                    }
                }

                _ => {} // Ignore other event types
            }
        }

        // If we didn't get output_items from response.completed, build from accumulated items
        // (output_items is already populated from output_item.done events)

        return Ok((response_id, output_items));
    }

    Err(last_err.unwrap_or_else(|| anyhow!("openai_api_retries_exhausted")))
}

// ── Native-OAuth mode ────────────────────────────────────────────
//
// Uses `codex::run_simple_prompt()` as the LLM transport (OAuth auth,
// no API key) while keeping the native Rust tool-use loop.
//
// The app-server returns plain text, so we embed tool definitions in
// the prompt and ask the model to respond with a structured protocol:
//   - Final answer: return the analysis JSON directly
//   - Tool call: return `<tool_call>{"name":"...","arguments":{...}}</tool_call>`
//
// The Rust loop parses tool calls, executes them via dispatch_tool_direct(),
// feeds results back, and repeats until the model returns a final answer.

/// Execute a prompt via the Codex app-server (OAuth) with a native tool-use loop.
/// Same orchestration as `run_prompt()` but uses `codex::run_simple_prompt()`
/// as the LLM transport instead of direct HTTP to api.openai.com.
pub fn run_prompt_oauth(
    prompt: &str,
    timeout_ms: u64,
    on_progress: Option<ProgressFn>,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let dd = data_dir();
    let tool_instructions = build_tool_instructions();

    crate::debug_log(&format!(
        "openai_client: native-oauth mode, timeout={timeout_ms}ms"
    ));

    // Build the initial prompt with tool instructions prepended
    let mut conversation = format!(
        "{tool_instructions}\n\n---\n\n{prompt}"
    );

    let mut round = 0;
    loop {
        round += 1;
        if round > MAX_TOOL_ROUNDS {
            return Err(anyhow!(
                "openai_client:native_oauth:max_tool_rounds_exceeded:{MAX_TOOL_ROUNDS}"
            ));
        }
        if Instant::now() > deadline {
            return Err(anyhow!(
                "openai_client:native_oauth:timeout_exceeded:{timeout_ms}ms"
            ));
        }

        if let Some(ref cb) = on_progress {
            cb(0, round, &format!("round {round}\u{2026}"));
        }

        // Call the app-server (OAuth, text-in/text-out).
        // Progress forwarding: we pass None since the inner callback
        // cannot share the outer Box<dyn Fn + Send>.
        let response_text =
            crate::codex::run_simple_prompt(&conversation, None)?;

        crate::debug_log(&format!(
            "openai_client: native-oauth round {round}, {} chars",
            response_text.len()
        ));

        // Check for tool calls in the response
        let tool_calls = parse_tool_calls(&response_text);

        if tool_calls.is_empty() {
            // No tool calls — this is the final answer
            crate::debug_log(&format!(
                "openai_client: native-oauth completed in {round} rounds, {} chars",
                response_text.len()
            ));
            match extract_json_result(&response_text) {
                Ok(v) => return Ok(v),
                Err(_) => {
                    return Ok(json!({
                        "ok": true,
                        "mcp_turn": true,
                        "agent_text_chars": response_text.len()
                    }))
                }
            }
        }

        // Execute tool calls and build context for next round
        let mut tool_results = String::new();
        for (name, arguments) in &tool_calls {
            if let Some(ref cb) = on_progress {
                cb(0, round, &format!("tool:{name}"));
            }
            crate::debug_log(&format!("openai_client: native-oauth calling tool {name}"));

            let args: Value = serde_json::from_str(arguments).unwrap_or(json!({}));
            let result = crate::mcp_server::dispatch_tool_direct(&dd, name, &args);
            let result_str = if let Some(s) = result.as_str() {
                s.to_string()
            } else {
                serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string())
            };

            tool_results.push_str(&format!(
                "\n<tool_result name=\"{name}\">\n{result_str}\n</tool_result>\n"
            ));
        }

        // Append tool results to the conversation for the next round
        conversation = format!(
            "{conversation}\n\n[Assistant called tools]\n{tool_results}\n\n\
             Continue with the analysis. If you need more data, call another tool. \
             Otherwise, provide your final JSON response (no <tool_call> tags)."
        );
    }
}

/// Build a system instruction describing available tools for text-based
/// tool calling. The model must use `<tool_call>...</tool_call>` tags.
fn build_tool_instructions() -> String {
    let tools = crate::mcp_server::tool_definitions_openai();
    let mut desc = String::from(
        "You have access to the following tools. To call a tool, respond ONLY with a \
         <tool_call> XML tag containing a JSON object with \"name\" and \"arguments\" fields. \
         Do NOT include any other text when making a tool call.\n\n\
         Example: <tool_call>{\"name\":\"get_line_data\",\"arguments\":{\"ticker\":\"AAPL\"}}</tool_call>\n\n\
         You may call multiple tools in one response by including multiple <tool_call> tags.\n\n\
         Available tools:\n",
    );
    for tool in &tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let description = tool.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let params = tool
            .get("parameters")
            .map(|v| serde_json::to_string(v).unwrap_or_default())
            .unwrap_or_default();
        desc.push_str(&format!("\n- **{name}**: {description}\n  Parameters: {params}\n"));
    }
    desc.push_str(
        "\nWhen you have the final answer, respond with the JSON result directly \
         (no <tool_call> tags). Do NOT wrap your final answer in any XML tags.",
    );
    desc
}

/// Parse `<tool_call>...</tool_call>` tags from model text.
/// Returns a vec of (name, arguments_json_string) pairs.
fn parse_tool_calls(text: &str) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("<tool_call>") {
        let abs_start = search_from + start + "<tool_call>".len();
        if let Some(end) = text[abs_start..].find("</tool_call>") {
            let inner = text[abs_start..abs_start + end].trim();
            if let Ok(obj) = serde_json::from_str::<Value>(inner) {
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = obj
                    .get("arguments")
                    .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string());
                if !name.is_empty() {
                    calls.push((name, args));
                }
            }
            search_from = abs_start + end + "</tool_call>".len();
        } else {
            break;
        }
    }
    calls
}

// ── JSON extraction ───────────────────────────────────────────────

/// Extract JSON from the model's final text output.
fn extract_json_result(text: &str) -> Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
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
    if let Some(v) = crate::llm_parsing::extract_json_object(text) {
        return Ok(v);
    }
    Err(anyhow!("openai_client:no_json_in_response"))
}
