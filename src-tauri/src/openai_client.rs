//! Native OpenAI Responses API client with agentic tool-use loop.
//!
//! Uses the Responses API (`/v1/responses`) which supports both function
//! tools and native web search. The model decides when to search.
//! Tool calls are executed locally via `mcp_server::dispatch_tool_direct()`.

use std::collections::hash_map::DefaultHasher;
use std::env;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::llm_backend::ProgressFn;

/// Last-resort default if /v1/models is unreachable AND no override is set.
/// gpt-5 is widely available on accounts with API access; gpt-4.1 was a
/// poor fallback because it lags newer models on schema compliance.
const DEFAULT_MODEL: &str = "gpt-5";
const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const MAX_TOOL_ROUNDS: usize = 30;
const MAX_RETRIES: usize = 4;
const RETRY_BASE_MS: u64 = 500;
const MODEL_DISCOVERY_TIMEOUT_SECS: u64 = 30;

/// Process-lifetime cache for the auto-selected model. `/v1/models` is
/// hit at most once per process, not once per line analysis.
static AUTO_SELECTED_MODEL: OnceLock<String> = OnceLock::new();

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
    Err(anyhow!(
        "openai_api_key_missing:set OPENAI_API_KEY or configure in settings"
    ))
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
///
/// Auto-detection is cached for the process lifetime via `AUTO_SELECTED_MODEL`,
/// so repeated calls (one per line analysis) don't re-hit `/v1/models`.
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
    // Auto-detect, cached for process lifetime. OnceLock::get_or_init blocks
    // concurrent callers until the first resolution completes.
    AUTO_SELECTED_MODEL
        .get_or_init(|| match resolve_best_model() {
            Ok(model) => model,
            Err(err) => {
                crate::debug_log(&format!(
                    "openai_client: auto-select failed ({err}), falling back to {DEFAULT_MODEL}"
                ));
                DEFAULT_MODEL.to_string()
            }
        })
        .clone()
}

/// Query /v1/models and pick the best available model.
/// Same ranking logic as the Codex backend: gpt-5.x > o4 > o3 > gpt-4.x
///
/// Every failure path logs explicitly so silent fallback to the default
/// model is observable in the debug log.
fn resolve_best_model() -> Result<String> {
    let key = api_key().map_err(|e| {
        crate::debug_log(&format!("openai_client: resolve_best_model api_key error: {e}"));
        e
    })?;
    let base = api_base();

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(MODEL_DISCOVERY_TIMEOUT_SECS))
        .build();

    let resp = agent
        .get(&format!("{base}/models"))
        .set("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| {
            let err = anyhow!("model_list_failed:{e}");
            crate::debug_log(&format!("openai_client: /v1/models request failed: {e}"));
            err
        })?;

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
        crate::debug_log("openai_client: /v1/models returned empty data array");
        return Err(anyhow!("no_models_available"));
    }

    let (selected, best_score) = pick_best_model(&models);
    crate::debug_log(&format!(
        "openai_client: auto-selected model {selected} (score={best_score}, {} available)",
        models.len()
    ));
    Ok(selected)
}

/// Pick the highest-scoring model from a list. Pure, testable.
/// Returns (selected_model, best_score). Falls back to the first model
/// in the list if none score above 0.
fn pick_best_model(models: &[String]) -> (String, i32) {
    let mut best: Option<&str> = None;
    let mut best_score: i32 = -1;
    for model in models {
        let score = model_score(model);
        if score > best_score {
            best_score = score;
            best = Some(model);
        }
    }
    (
        best.unwrap_or_else(|| models[0].as_str()).to_string(),
        best_score,
    )
}

/// Score a model name for ranking. Higher is better.
/// gpt-5.x ranks above gpt-4.x; minor versions (5.1, 5.2) outrank 5.0.
fn model_score(name: &str) -> i32 {
    if name.starts_with("gpt-5") {
        // Parse version: "gpt-5" -> 5.0, "gpt-5.1" -> 5.1, "gpt-5.2-mini" -> 5.2
        // Use the FIRST hyphen-separated token after "gpt-" as the version string,
        // accepting decimals (e.g. "5.2"). Multiply by 10 to keep an int score.
        let version_str = name
            .strip_prefix("gpt-")
            .and_then(|s| s.split('-').next())
            .unwrap_or("5");
        let version: f32 = version_str.parse().unwrap_or(5.0);
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
    artifacts: Option<&crate::agentos_artifacts::ArtifactContext>,
) -> Result<Value> {
    let key = api_key()?;
    let base = api_base();
    let model = model_name();
    if let Some(ctx) = artifacts {
        crate::agentos_artifacts::merge_runtime_data(ctx, json!({ "model": model }));
    }
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
            return Err(anyhow!(
                "openai_client:max_tool_rounds_exceeded:{MAX_TOOL_ROUNDS}"
            ));
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
        let (response_id, output_items, usage) =
            call_responses_streamed(&base, &key, &body, &on_progress, deadline, artifacts)?;
        if let Some(ctx) = artifacts {
            if !usage.is_null() {
                crate::agentos_artifacts::merge_runtime_data(ctx, json!({ "token_usage": usage }));
            }
        }

        previous_response_id = Some(response_id);

        // Process output items — collect function calls and final text
        let mut function_calls: Vec<(String, String, String)> = Vec::new(); // (call_id, name, arguments)
        let mut final_text = String::new();

        for item in &output_items {
            let item_type = item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match item_type {
                "function_call" => {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}")
                        .to_string();
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
                    let status = item
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    crate::debug_log(&format!("openai_client: web_search_call status={status}"));
                    if let Some(ref cb) = on_progress {
                        cb(0, round, "searching the web\u{2026}");
                    }
                }
                _ => {
                    crate::debug_log(&format!(
                        "openai_client: unknown output item type={item_type}"
                    ));
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
                Ok(v) => {
                    if let Some(ctx) = artifacts {
                        crate::agentos_artifacts::record_decision(
                            ctx,
                            "llm.output.parse",
                            "llm.output.parse",
                            json!({ "json_extracted": true }),
                            None,
                            None,
                        );
                    }
                    return Ok(v);
                }
                Err(_) => {
                    if let Some(ctx) = artifacts {
                        crate::agentos_artifacts::record_decision(
                            ctx,
                            "llm.output.parse",
                            "llm.output.parse",
                            json!({ "json_extracted": false }),
                            None,
                            None,
                        );
                    }
                    return Ok(
                        json!({"ok": true, "mcp_turn": true, "agent_text_chars": final_text.len()}),
                    );
                }
            }
        }

        // Execute function calls and build input for next round
        input = Vec::new();
        for (call_id, name, arguments) in &function_calls {
            if let Some(ref cb) = on_progress {
                cb(0, round, &format!("tool:{name}"));
            }
            crate::debug_log(&format!("openai_client: calling tool {name}"));
            if let Some(ctx) = artifacts {
                let mut hasher = DefaultHasher::new();
                arguments.hash(&mut hasher);
                let args_fingerprint = format!("{:016x}", hasher.finish());
                crate::agentos_artifacts::record_decision(
                    ctx,
                    "llm.tool.dispatch",
                    "llm.tool.dispatch",
                    json!({
                        "tool": name,
                        "args_fingerprint": args_fingerprint,
                    }),
                    None,
                    None,
                );
            }

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
    artifacts: Option<&crate::agentos_artifacts::ArtifactContext>,
) -> Result<(String, Vec<Value>, Value)> {
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
                    let reason = format!("status_{status}");
                    if let Some(ctx) = artifacts {
                        crate::agentos_artifacts::record_decision(
                            ctx,
                            "llm.retry.policy",
                            "llm.retry.policy",
                            json!({ "attempt": attempt + 1, "reason": reason }),
                            None,
                            None,
                        );
                    }
                    last_err = Some(anyhow!("openai_api_error:{status}:{err_body}"));
                    continue;
                }
                return Err(anyhow!("openai_api_error:{status}:{err_body}"));
            }
            Err(e) => {
                if let Some(ctx) = artifacts {
                    crate::agentos_artifacts::record_decision(
                        ctx,
                        "llm.retry.policy",
                        "llm.retry.policy",
                        json!({ "attempt": attempt + 1, "reason": e.to_string() }),
                        None,
                        None,
                    );
                }
                last_err = Some(anyhow!("openai_api_request_failed:{e}"));
                continue;
            }
        };

        // Parse SSE stream
        let reader = BufReader::new(resp.into_reader());
        let mut response_id = String::new();
        let mut output_items: Vec<Value> = Vec::new();
        let mut usage_payload = Value::Null;

        // Accumulate streamed text deltas per output item index
        let mut text_bufs: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
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

            let event_type = event
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            match event_type {
                // Response created — capture ID
                "response.created" | "response.completed" => {
                    if let Some(resp_obj) = event.get("response") {
                        if let Some(id) = resp_obj.get("id").and_then(|v| v.as_str()) {
                            response_id = id.to_string();
                        }
                        if event_type == "response.completed" {
                            if let Some(output) = resp_obj.get("output").and_then(|v| v.as_array())
                            {
                                output_items = output.clone();
                            }
                            // Extract token usage and emit via progress callback
                            if let Some(usage) = resp_obj.get("usage") {
                                let total = usage
                                    .get("total_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let input = usage
                                    .get("input_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let output_t = usage
                                    .get("output_tokens")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if total > 0 {
                                    if let Some(ref cb) = on_progress {
                                        cb(0, 0, &format!("tokens:{total}:{input}:{output_t}"));
                                    }
                                    usage_payload = json!({
                                        "total_tokens": total,
                                        "input_tokens": input,
                                        "output_tokens": output_t,
                                    });
                                }
                            }
                        }
                    }
                }

                // Output item completed — add to our list
                "response.output_item.done" => {
                    if let Some(item) = event.get("item") {
                        // For function_call items, merge accumulated arguments
                        let idx = event
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        if let Some((call_id, name, args)) = fn_arg_bufs.remove(&idx) {
                            let mut item = item.clone();
                            if item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .is_empty()
                            {
                                item["call_id"] = json!(call_id);
                            }
                            if item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .is_empty()
                            {
                                item["name"] = json!(name);
                            }
                            if item
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .is_empty()
                            {
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
                        let idx = event
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let buf = text_bufs.entry(idx).or_default();
                        buf.push_str(delta);
                        // Show a periodic "writing..." indicator (every ~500 chars)
                        if buf.len() % 500 < delta.len() {
                            if let Some(ref cb) = on_progress {
                                cb(
                                    0,
                                    0,
                                    &format!(
                                        "writing ({:.1}kB)\u{2026}",
                                        buf.len() as f64 / 1024.0
                                    ),
                                );
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
                                let preview: String = reasoning_buf
                                    .chars()
                                    .rev()
                                    .take(60)
                                    .collect::<String>()
                                    .chars()
                                    .rev()
                                    .collect();
                                cb(0, 0, &format!("thinking: {preview}\u{2026}"));
                            }
                        }
                    }
                }

                // Function call argument deltas
                "response.function_call_arguments.delta" => {
                    let idx = event
                        .get("output_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                        fn_arg_bufs
                            .entry(idx)
                            .or_insert_with(|| (String::new(), String::new(), String::new()))
                            .2
                            .push_str(delta);
                    }
                }
                "response.function_call_arguments.done" => {
                    let idx = event
                        .get("output_index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    // call_id and name come from output_item.added
                    if let Some(args) = event.get("arguments").and_then(|v| v.as_str()) {
                        let entry = fn_arg_bufs
                            .entry(idx)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                        entry.2 = args.to_string(); // replace with final complete args
                    }
                }

                // Output item added — capture call_id and name for function calls
                "response.output_item.added" => {
                    if let Some(item) = event.get("item") {
                        let idx = event
                            .get("output_index")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let item_type = item
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        if item_type == "function_call" {
                            let call_id = item
                                .get("call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string();
                            fn_arg_bufs
                                .entry(idx)
                                .or_insert_with(|| (call_id, name, String::new()));
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

        return Ok((response_id, output_items, usage_payload));
    }

    Err(last_err.unwrap_or_else(|| anyhow!("openai_api_retries_exhausted")))
}

// ── Native-OAuth mode ────────────────────────────────────────────
//
// Delegates to `codex::run_codex_prompt_with_progress()` — the same
// structured MCP tool-dispatch path used by the regular codex backend.
// The app-server handles OAuth auth transparently; tool calls are
// dispatched natively via `mcpToolCall`/`functionCall` item types.
// No text parsing needed.

/// Execute a prompt via the Codex app-server (OAuth) with structured
/// MCP tool dispatch. Uses the same path as the codex backend —
/// `spawn_with_tools()` configures the app-server with MCP tools and
/// the notification loop handles `mcpToolCall`/`functionCall` items.
pub fn run_prompt_oauth(
    prompt: &str,
    timeout_ms: u64,
    on_progress: Option<ProgressFn>,
    artifacts: Option<&crate::agentos_artifacts::ArtifactContext>,
) -> Result<Value> {
    crate::debug_log(&format!(
        "openai_client: native-oauth mode (structured MCP dispatch), timeout={timeout_ms}ms"
    ));

    // Ensure legacy global MCP config is cleaned so -c flags take precedence
    crate::codex::ensure_mcp_config();
    if let Some(ctx) = artifacts {
        crate::agentos_artifacts::merge_runtime_data(
            ctx,
            json!({"model": "oauth-managed", "backend_path": "native-oauth"}),
        );
    }

    // Delegate to the codex app-server path which already handles:
    // - OAuth authentication (app-server session)
    // - MCP tool configuration via spawn_with_tools()
    // - Structured tool dispatch (mcpToolCall/functionCall notifications)
    // - Streaming progress, token usage, rate limits
    // - JSON extraction from agent output
    crate::codex::run_codex_prompt_with_progress(prompt, timeout_ms, on_progress)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_score_orders_gpt5_minor_versions() {
        assert!(model_score("gpt-5.2") > model_score("gpt-5.1"));
        assert!(model_score("gpt-5.1") > model_score("gpt-5"));
        assert!(model_score("gpt-5") > model_score("gpt-4.1"));
        assert!(model_score("gpt-4.1") > model_score("o4-mini"));
        assert!(model_score("o4-mini") > model_score("o3-mini"));
        assert!(model_score("o3-mini") > model_score("gpt-4o"));
        assert!(model_score("gpt-4o") > model_score("text-embedding-ada-002"));
    }

    #[test]
    fn model_score_handles_suffixed_variants() {
        // Suffixes like "-mini", "-turbo", "-preview" should not lower the score
        // below the base family — the family prefix decides ranking.
        assert_eq!(model_score("gpt-5.2-mini"), model_score("gpt-5.2"));
        assert_eq!(model_score("gpt-5-turbo"), model_score("gpt-5"));
    }

    #[test]
    fn pick_best_model_selects_highest_version() {
        let models = vec![
            "gpt-3.5-turbo".to_string(),
            "gpt-4.1".to_string(),
            "gpt-5".to_string(),
            "gpt-5.1".to_string(),
            "gpt-5.2".to_string(),
            "o3-mini".to_string(),
            "text-embedding-3-small".to_string(),
        ];
        let (selected, score) = pick_best_model(&models);
        assert_eq!(selected, "gpt-5.2");
        assert_eq!(score, model_score("gpt-5.2"));
    }

    #[test]
    fn pick_best_model_handles_only_legacy_models() {
        // No gpt-5 family available — should pick the highest of what's there.
        let models = vec!["gpt-4o".to_string(), "gpt-4.1".to_string()];
        let (selected, _score) = pick_best_model(&models);
        assert_eq!(selected, "gpt-4.1");
    }

    #[test]
    fn pick_best_model_falls_back_to_first_when_all_zero() {
        // No recognized model families — should pick first to avoid panic.
        let models = vec!["unknown-1".to_string(), "unknown-2".to_string()];
        let (selected, _score) = pick_best_model(&models);
        assert_eq!(selected, "unknown-1");
    }

    #[test]
    fn default_model_is_gpt5() {
        // Regression guard: the last-resort fallback must NOT be gpt-4.1 again.
        assert_eq!(DEFAULT_MODEL, "gpt-5");
    }
}
