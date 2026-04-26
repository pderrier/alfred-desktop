use std::env;

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::paths::resolve_runtime_settings_path;
use crate::storage::{read_json_file, write_json_file};

pub fn definitions_json() -> serde_json::Value {
    json!({
        "default_run_mode": {
            "type": "enum",
            "values": ["finary_resync", "finary_cached", "csv"],
            "defaultValue": "finary_resync",
            "section": "product",
            "restartRequired": false,
            "envName": "ALFRED_PORTFOLIO_SOURCE",
            "label": "Default analysis source",
            "description": "Choose which source Alfred should preselect when opening Run Analysis.",
            "whyItMatters": "This changes startup workflow speed versus data freshness.",
            "resetLabel": "Use recommended source"
        },
        "shell_theme": {
            "type": "enum",
            "values": ["dark", "midnight", "system"],
            "defaultValue": "dark",
            "section": "product",
            "restartRequired": false,
            "envName": "ALFRED_SHELL_THEME",
            "label": "Theme behavior",
            "description": "Control how the desktop shell resolves its color theme.",
            "whyItMatters": "Theme choice affects readability and contrast during long review sessions.",
            "resetLabel": "Use dark theme"
        },
        "llm_backend": {
            "type": "enum",
            "values": ["codex", "native-oauth", "native"],
            "defaultValue": "codex",
            "section": "product",
            "restartRequired": false,
            "envName": "ALFRED_LLM_BACKEND",
            "label": "LLM backend",
            "description": "Codex uses OpenAI OAuth with AI orchestration. Native+OAuth uses OAuth with optimized Rust orchestration (free, recommended). Native uses your own API key (pay-per-use).",
            "whyItMatters": "Native+OAuth combines free OAuth auth with optimized local tool calls. Codex lets the AI orchestrate. Native requires a paid API key.",
            "resetLabel": "Use Codex (free tier)"
        },
        "agentos_artifacts_enabled": {
            "type": "integer",
            "min": 0,
            "max": 1,
            "defaultValue": 0,
            "section": "product",
            "restartRequired": false,
            "envName": "ALFRED_AGENTOS_ARTIFACTS_ENABLED",
            "label": "AgentOS artifact emission",
            "description": "Emit per-turn AgentOS decision and outcome artifacts to disk.",
            "whyItMatters": "Keeps rollout safe by leaving instrumentation off until explicitly enabled.",
            "resetLabel": "Disable artifacts"
        },
        "openai_api_key": {
            "type": "text",
            "maxLength": 200,
            "defaultValue": "",
            "section": "product",
            "restartRequired": false,
            "envName": "OPENAI_API_KEY",
            "label": "OpenAI API key",
            "description": "Required when using the native backend. Get your key at platform.openai.com/api-keys.",
            "whyItMatters": "The native backend calls the OpenAI API directly with your key. Your key stays on-device.",
            "resetLabel": "Clear API key"
        },
        "openai_model": {
            "type": "text",
            "maxLength": 50,
            "defaultValue": "gpt-4.1",
            "section": "product",
            "restartRequired": false,
            "envName": "ALFRED_MODEL",
            "label": "OpenAI model (native backend)",
            "description": "Model to use with the native backend. Leave empty for default.",
            "whyItMatters": "More capable models produce better analysis but cost more per call.",
            "resetLabel": "Use default model"
        },
        "openai_api_base": {
            "type": "text",
            "maxLength": 200,
            "defaultValue": "",
            "section": "product",
            "restartRequired": false,
            "envName": "OPENAI_API_BASE",
            "label": "OpenAI API base URL",
            "description": "Custom API endpoint. Leave empty for default (api.openai.com). Useful for proxies or compatible APIs.",
            "whyItMatters": "Allows routing API calls through a custom endpoint or compatible provider.",
            "resetLabel": "Use default endpoint"
        },
        "agent_guidelines": {
            "type": "text",
            "maxLength": 4000,
            "defaultValue": "",
            "section": "product",
            "restartRequired": false,
            "envName": serde_json::Value::Null,
            "label": "Global portfolio guidance",
            "description": "Default portfolio-level guidance injected into new analysis runs unless you edit it before launch.",
            "whyItMatters": "This keeps recurring portfolio constraints and execution discipline persistent across UI restarts.",
            "resetLabel": "Clear guidance"
        },
        "line_analysis_concurrency": {
            "type": "integer",
            "min": 1,
            "max": 12,
            "defaultValue": 6,
            "section": "analysis",
            "restartRequired": false,
            "envName": "ALFRED_LINE_ANALYSIS_CONCURRENCY",
            "label": "Line analysis concurrency",
            "description": "How many portfolio lines Alfred analyzes in parallel.",
            "whyItMatters": "Higher values improve throughput but can increase provider load and instability.",
            "resetLabel": "Use balanced concurrency"
        },
        "line_analysis_throttle_ms": {
            "type": "integer",
            "min": 0,
            "max": 5000,
            "defaultValue": 80,
            "section": "analysis",
            "restartRequired": false,
            "envName": "ALFRED_LINE_ANALYSIS_THROTTLE_MS",
            "label": "Line analysis throttle",
            "description": "Pause inserted between line-analysis dispatches.",
            "whyItMatters": "A small delay reduces burst load on sidecars and upstream providers.",
            "resetLabel": "Use default throttle"
        },
        "collection_concurrency": {
            "type": "integer",
            "min": 1,
            "max": 12,
            "defaultValue": 4,
            "section": "analysis",
            "restartRequired": false,
            "envName": "ALFRED_COLLECTION_CONCURRENCY",
            "label": "Collection concurrency",
            "description": "How many tickers Alfred enriches in parallel during collection.",
            "whyItMatters": "Higher values reduce collection time but increase concurrent calls to enrichment providers.",
            "resetLabel": "Use balanced concurrency"
        },
        "collection_throttle_ms": {
            "type": "integer",
            "min": 0,
            "max": 5000,
            "defaultValue": 40,
            "section": "analysis",
            "restartRequired": false,
            "envName": "ALFRED_COLLECTION_THROTTLE_MS",
            "label": "Collection throttle",
            "description": "Pause inserted between enrichment dispatches.",
            "whyItMatters": "A small delay reduces burst load on market/news providers.",
            "resetLabel": "Use default throttle"
        },
        "litellm_generation_timeout_ms": {
            "type": "integer",
            "min": 0,
            "max": 900000,
            "defaultValue": 0,
            "section": "analysis",
            "restartRequired": false,
            "envName": "ALFRED_LITELLM_GENERATION_TIMEOUT_MS",
            "label": "LLM generation kill timeout",
            "description": "Optional desktop-side kill timeout for line and report generation calls. Set to 0 to disable.",
            "whyItMatters": "Disabling this avoids burning valid long-running generations; use only if you explicitly want desktop-side aborts.",
            "resetLabel": "Disable timeout"
        },
        "stack_health_timeout_ms": {
            "type": "integer",
            "min": 500,
            "max": 15000,
            "defaultValue": 1500,
            "section": "health",
            "restartRequired": false,
            "envName": "ALFRED_STACK_HEALTH_TIMEOUT_MS",
            "label": "Stack health timeout",
            "description": "How long Alfred waits for service health checks before degrading.",
            "whyItMatters": "This controls how quickly startup shifts from waiting to explicit diagnostics.",
            "resetLabel": "Use fast health check"
        },
        "finary_connector_timeout_ms": {
            "type": "integer",
            "min": 0,
            "max": 120000,
            "defaultValue": 0,
            "section": "connector",
            "restartRequired": false,
            "envName": "ALFRED_FINARY_CONNECTOR_TIMEOUT_MS",
            "label": "Finary connector kill timeout",
            "description": "Optional desktop-side kill timeout for regular Finary connector requests. Set to 0 to disable.",
            "whyItMatters": "Disabling this avoids aborting valid but slow Finary requests; use a nonzero value only if you explicitly want desktop-side aborts.",
            "resetLabel": "Disable timeout"
        },
        "finary_connector_snapshot_timeout_ms": {
            "type": "integer",
            "min": 0,
            "max": 300000,
            "defaultValue": 0,
            "section": "connector",
            "restartRequired": false,
            "envName": "ALFRED_FINARY_CONNECTOR_SNAPSHOT_TIMEOUT_MS",
            "label": "Finary snapshot kill timeout",
            "description": "Optional desktop-side kill timeout for snapshot materialization and portfolio fetches. Set to 0 to disable.",
            "whyItMatters": "Disabling this avoids aborting valid but slow snapshot work; use a nonzero value only if you explicitly want desktop-side aborts.",
            "resetLabel": "Disable timeout"
        },
        "finary_connector_snapshot_retries": {
            "type": "integer",
            "min": 0,
            "max": 5,
            "defaultValue": 1,
            "section": "connector",
            "restartRequired": false,
            "envName": "ALFRED_FINARY_CONNECTOR_SNAPSHOT_RETRIES",
            "label": "Finary snapshot retries",
            "description": "How many times Alfred retries a snapshot request before degrading.",
            "whyItMatters": "More retries can recover transient failures but increase total wait time.",
            "resetLabel": "Retry once"
        }
    })
}

fn read_overrides() -> serde_json::Map<String, serde_json::Value> {
    let path = resolve_runtime_settings_path();
    if !path.exists() {
        return serde_json::Map::new();
    }
    match read_json_file(&path) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

fn normalize_value(key: &str, value: &serde_json::Value) -> Result<serde_json::Value> {
    match key {
        "default_run_mode" => {
            let normalized = value.as_str().unwrap_or_default().trim();
            if ["finary_resync", "finary_cached", "csv"].contains(&normalized) {
                Ok(json!(normalized))
            } else {
                Err(anyhow!("runtime_setting_invalid:{key}"))
            }
        }
        "shell_theme" => {
            let normalized = value.as_str().unwrap_or_default().trim();
            if ["dark", "midnight", "system"].contains(&normalized) {
                Ok(json!(normalized))
            } else {
                Err(anyhow!("runtime_setting_invalid:{key}"))
            }
        }
        "llm_backend" => {
            let normalized = value.as_str().unwrap_or_default().trim();
            if ["codex", "native", "native-oauth"].contains(&normalized) {
                Ok(json!(normalized))
            } else {
                Err(anyhow!("runtime_setting_invalid:{key}"))
            }
        }
        "openai_api_key" | "openai_model" | "openai_api_base" => {
            let normalized = value.as_str().unwrap_or_default().trim();
            if normalized.len() > 200 {
                Err(anyhow!("runtime_setting_out_of_range:{key}"))
            } else {
                Ok(json!(normalized))
            }
        }
        "agent_guidelines" => {
            let normalized = value.as_str().unwrap_or_default().trim();
            if normalized.len() > 4000 {
                Err(anyhow!("runtime_setting_out_of_range:{key}"))
            } else {
                Ok(json!(normalized))
            }
        }
        "line_analysis_concurrency" => normalize_integer(key, value, 1, 12),
        "agentos_artifacts_enabled" => normalize_integer(key, value, 0, 1),
        "line_analysis_throttle_ms" => normalize_integer(key, value, 0, 5000),
        "collection_concurrency" => normalize_integer(key, value, 1, 12),
        "collection_throttle_ms" => normalize_integer(key, value, 0, 5000),
        "litellm_generation_timeout_ms" => normalize_integer(key, value, 0, 900000),
        "stack_health_timeout_ms" => normalize_integer(key, value, 500, 15000),
        "finary_connector_timeout_ms" => normalize_integer(key, value, 0, 120000),
        "finary_connector_snapshot_timeout_ms" => normalize_integer(key, value, 0, 300000),
        "finary_connector_snapshot_retries" => normalize_integer(key, value, 0, 5),
        _ => Err(anyhow!("runtime_setting_unknown:{key}")),
    }
}

fn normalize_integer(
    key: &str,
    value: &serde_json::Value,
    min: i64,
    max: i64,
) -> Result<serde_json::Value> {
    let parsed = match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("runtime_setting_invalid:{key}"))?;
    if parsed < min || parsed > max {
        return Err(anyhow!("runtime_setting_out_of_range:{key}"));
    }
    Ok(json!(parsed))
}

fn value_from_env(key: &str) -> Option<serde_json::Value> {
    match key {
        "shell_theme" => env::var("ALFRED_SHELL_THEME")
            .ok()
            .map(|value| json!(value)),
        "line_analysis_concurrency" => env::var("ALFRED_LINE_ANALYSIS_CONCURRENCY")
            .ok()
            .map(|value| json!(value)),
        "agentos_artifacts_enabled" => env::var("ALFRED_AGENTOS_ARTIFACTS_ENABLED")
            .ok()
            .map(|value| json!(value)),
        "line_analysis_throttle_ms" => env::var("ALFRED_LINE_ANALYSIS_THROTTLE_MS")
            .ok()
            .map(|value| json!(value)),
        "collection_concurrency" => env::var("ALFRED_COLLECTION_CONCURRENCY")
            .ok()
            .map(|value| json!(value)),
        "collection_throttle_ms" => env::var("ALFRED_COLLECTION_THROTTLE_MS")
            .ok()
            .map(|value| json!(value)),
        "litellm_generation_timeout_ms" => env::var("ALFRED_LITELLM_GENERATION_TIMEOUT_MS")
            .ok()
            .map(|value| json!(value)),
        "stack_health_timeout_ms" => env::var("ALFRED_STACK_HEALTH_TIMEOUT_MS")
            .ok()
            .map(|value| json!(value)),
        "finary_connector_timeout_ms" => env::var("ALFRED_FINARY_CONNECTOR_TIMEOUT_MS")
            .ok()
            .map(|value| json!(value)),
        "finary_connector_snapshot_timeout_ms" => {
            env::var("ALFRED_FINARY_CONNECTOR_SNAPSHOT_TIMEOUT_MS")
                .ok()
                .map(|value| json!(value))
        }
        "finary_connector_snapshot_retries" => env::var("ALFRED_FINARY_CONNECTOR_SNAPSHOT_RETRIES")
            .ok()
            .map(|value| json!(value)),
        "default_run_mode" => match env::var("ALFRED_PORTFOLIO_SOURCE").ok() {
            Some(raw) if raw.trim() == "csv" => Some(json!("csv")),
            _ => None,
        },
        _ => None,
    }
}

pub fn integer_direct(key: &str, default_value: i64) -> i64 {
    match get_payload() {
        Ok(payload) => payload
            .get("values")
            .and_then(|value| value.get(key))
            .and_then(|value| value.as_i64())
            .unwrap_or(default_value),
        Err(_) => default_value,
    }
}

pub fn string_direct(key: &str) -> Result<String, ()> {
    match get_payload() {
        Ok(payload) => payload
            .get("values")
            .and_then(|value| value.get(key))
            .and_then(|value| value.as_str())
            .map(|s| s.to_string())
            .ok_or(()),
        Err(_) => Err(()),
    }
}

fn build_advanced_payload() -> serde_json::Value {
    let endpoints = vec![
        ("Control plane", "native (built-in)".to_string()),
        (
            "LLM generation",
            env::var("ALFRED_LITELLM_BASE_URL").unwrap_or_else(|_| "native (built-in)".to_string()),
        ),
        (
            "Codex CLI",
            env::var("CODEX_PROXY_CLI_CMD").unwrap_or_else(|_| "auto-detected".to_string()),
        ),
        ("Enrichment", "native (built-in)".to_string()),
        ("Portfolio MCP", "native (built-in)".to_string()),
        ("Finary", "native (built-in, browser CDP)".to_string()),
    ]
    .into_iter()
    .map(|(label, value)| {
        json!({
            "label": label,
            "value": value.trim(),
            "managed_externally": true,
            "status": if value.trim().is_empty() { "missing" } else { "configured" }
        })
    })
    .collect::<Vec<_>>();

    let credentials = vec![
        (
            "llm_token",
            "LLM API token",
            vec!["ALFRED_LLM_TOKEN"],
            "Managed externally",
        ),
        (
            "alphavantage_api_key",
            "AlphaVantage API key",
            vec!["ALPHAVANTAGE_API_KEY"],
            "Managed externally",
        ),
        (
            "finary_login",
            "Finary login",
            vec!["FINARY_EMAIL", "FINARY_PASSWORD"],
            "Managed externally",
        ),
        (
            "finary_mfa_code",
            "Finary MFA code",
            vec!["FINARY_MFA_CODE"],
            "Provided when needed",
        ),
    ]
    .into_iter()
    .map(|(id, label, env_names, managed_label)| {
        let configured = env_names.iter().all(|name| {
            env::var(name)
                .ok()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        });
        json!({
            "id": id,
            "label": label,
            "value": if configured { "configured" } else { "missing" },
            "managed_externally": true,
            "detail": if configured { managed_label } else { "Missing external configuration" }
        })
    })
    .collect::<Vec<_>>();

    json!({
        "endpoints": endpoints,
        "credentials": credentials
    })
}

// ── User preferences (first-run + persisted) ─────────────────────

fn resolve_preferences_path() -> std::path::PathBuf {
    resolve_runtime_settings_path()
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("user-preferences.json")
}

pub fn get_user_preferences() -> serde_json::Value {
    let path = resolve_preferences_path();
    if path.exists() {
        read_json_file(&path).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    }
}

pub fn save_user_preferences(prefs: &serde_json::Value) -> Result<()> {
    let path = resolve_preferences_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Merge with existing preferences to prevent accidental data loss.
    // Deep-merge guidelines_by_account so saving one account doesn't wipe others.
    let mut existing = if path.exists() {
        read_json_file(&path).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), prefs.as_object()) {
        for (key, new_val) in new_obj {
            if key == "guidelines_by_account" {
                let existing_guidelines =
                    existing_obj.entry(key.clone()).or_insert_with(|| json!({}));
                if let (Some(eg), Some(ng)) =
                    (existing_guidelines.as_object_mut(), new_val.as_object())
                {
                    for (acct, guideline) in ng {
                        eg.insert(acct.clone(), guideline.clone());
                    }
                }
            } else {
                existing_obj.insert(key.clone(), new_val.clone());
            }
        }
    }
    crate::storage::write_json_file(&path, &existing)
}

pub fn get_payload() -> Result<serde_json::Value> {
    let definitions = definitions_json();
    let overrides = read_overrides();
    let definitions_map = definitions
        .as_object()
        .ok_or_else(|| anyhow!("runtime_settings_definitions_invalid"))?;
    let mut values = serde_json::Map::new();
    for (key, definition) in definitions_map {
        let value = if let Some(override_value) = overrides.get(key) {
            normalize_value(key, override_value)?
        } else if let Some(env_value) = value_from_env(key) {
            normalize_value(key, &env_value)?
        } else {
            definition
                .get("defaultValue")
                .cloned()
                .ok_or_else(|| anyhow!("runtime_settings_definition_missing_default:{key}"))?
        };
        values.insert(key.clone(), value);
    }
    Ok(json!({
        "values": values,
        "overrides": overrides,
        "definitions": definitions,
        "advanced": build_advanced_payload()
    }))
}

pub fn patch(patch: &serde_json::Value) -> Result<serde_json::Value> {
    let patch_map = patch
        .as_object()
        .ok_or_else(|| anyhow!("runtime_settings_payload_invalid"))?;
    let mut next_overrides = read_overrides();
    for (key, value) in patch_map {
        let should_remove = match value {
            serde_json::Value::Null => true,
            serde_json::Value::String(text) => text.trim().is_empty(),
            _ => false,
        };
        if should_remove {
            next_overrides.remove(key);
            continue;
        }
        next_overrides.insert(key.clone(), normalize_value(key, value)?);
    }
    let path = resolve_runtime_settings_path();
    write_json_file(&path, &serde_json::Value::Object(next_overrides))?;
    get_payload()
}

pub fn reset() -> Result<serde_json::Value> {
    let path = resolve_runtime_settings_path();
    write_json_file(&path, &json!({}))?;
    get_payload()
}
