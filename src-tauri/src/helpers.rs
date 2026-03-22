use std::{
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};

static ANALYSIS_OP_SEQ: AtomicU64 = AtomicU64::new(1);
static RUN_STATE_UPDATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_iso_string() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_run_id() -> String {
    let seq = ANALYSIS_OP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:012x}", now_epoch_ms() ^ seq)
}

pub fn new_analysis_operation_id() -> String {
    let seq = ANALYSIS_OP_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("analysis_op_{}_{}", now_epoch_ms(), seq)
}

pub fn run_state_update_lock() -> std::sync::MutexGuard<'static, ()> {
    RUN_STATE_UPDATE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn infer_error_code(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "analysis_run_failed".to_string();
    }
    let code = trimmed.split(':').next().unwrap_or("analysis_run_failed").trim();
    if code.is_empty() {
        "analysis_run_failed".to_string()
    } else {
        code.to_string()
    }
}

pub fn parse_timestamp_millis(value: Option<&str>) -> i64 {
    value
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

pub fn read_json_env_override(env_key: &str) -> Result<Option<serde_json::Value>> {
    match env::var(env_key) {
        Ok(raw) if !raw.trim().is_empty() => {
            let parsed = serde_json::from_str::<serde_json::Value>(raw.trim())
                .map_err(|error| anyhow!("invalid_json_env:{env_key}:{error}"))?;
            Ok(Some(parsed))
        }
        _ => Ok(None),
    }
}

pub fn safe_trimmed_option(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = value.and_then(|v| v.as_str()).unwrap_or_default().trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

pub fn safe_portfolio_source(value: Option<&serde_json::Value>) -> Result<String> {
    let source = safe_trimmed_option(value).unwrap_or_else(|| "finary".to_string());
    if source == "finary" || source == "csv" {
        Ok(source)
    } else {
        Err(anyhow!("invalid_portfolio_source:{source}"))
    }
}

pub fn safe_text(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

pub fn safe_upper(value: Option<&serde_json::Value>) -> String {
    safe_text(value).to_ascii_uppercase()
}

pub fn safe_lower(value: Option<&serde_json::Value>) -> String {
    safe_text(value).to_ascii_lowercase()
}

/// Extract a subset of keys from a JSON Value into a new object.
/// Missing keys become `null`. Avoids repetitive `.get("k").cloned().unwrap_or(Value::Null)` chains.
pub fn pick_json_fields(source: &serde_json::Value, keys: &[&str]) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for key in keys {
        obj.insert(
            key.to_string(),
            source.get(*key).cloned().unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(obj)
}

/// Append a timestamped line to the debug log file.
pub fn debug_log(message: &str) {
    use std::io::Write;
    let path = crate::paths::resolve_debug_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "[{}] {}", now_iso_string(), message);
    }
}

#[cfg(test)]
static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
