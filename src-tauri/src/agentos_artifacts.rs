use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};

#[derive(Clone)]
pub struct ArtifactContext {
    enabled: bool,
    run_id: String,
    run_dir: PathBuf,
    file_lock: Arc<Mutex<()>>,
    runtime_data: Arc<Mutex<serde_json::Map<String, Value>>>,
}

impl ArtifactContext {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            run_id: String::new(),
            run_dir: PathBuf::new(),
            file_lock: Arc::new(Mutex::new(())),
            runtime_data: Arc::new(Mutex::new(serde_json::Map::new())),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }
}

pub fn enabled() -> bool {
    if let Ok(raw) = std::env::var("ALFRED_AGENTOS_ARTIFACTS_ENABLED") {
        let value = raw.trim().to_ascii_lowercase();
        if value == "1" || value == "true" || value == "yes" || value == "on" {
            return true;
        }
        if value == "0" || value == "false" || value == "no" || value == "off" {
            return false;
        }
    }
    crate::runtime_setting_integer_direct("agentos_artifacts_enabled", 0) == 1
}

pub fn start_run(run_id: &str, intent: &str, metadata: Value) -> ArtifactContext {
    if !enabled() {
        return ArtifactContext::disabled();
    }
    let run_dir = crate::paths::default_data_dir()
        .join("agentos-artifacts")
        .join(run_id);
    if std::fs::create_dir_all(&run_dir).is_err() {
        return ArtifactContext::disabled();
    }
    let ctx = ArtifactContext {
        enabled: true,
        run_id: run_id.to_string(),
        run_dir,
        file_lock: Arc::new(Mutex::new(())),
        runtime_data: Arc::new(Mutex::new(serde_json::Map::new())),
    };
    let meta = json!({
        "run_id": run_id,
        "intent": intent,
        "metadata": metadata,
        "started_at": crate::now_iso_string(),
    });
    let _ = write_value(&ctx, "meta.json", &meta);
    ctx
}

pub fn record_decision(
    ctx: &ArtifactContext,
    step_id: &str,
    decision_key: &str,
    output_json: Value,
    evidence: Option<Value>,
    candidate: Option<Value>,
) {
    if !ctx.enabled {
        return;
    }
    let _guard = ctx.file_lock.lock().unwrap_or_else(|p| p.into_inner());
    let path = ctx.run_dir.join("decisions.json");
    let mut root = match crate::storage::read_json_file(&path) {
        Ok(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let decisions = root
        .entry("decisions".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let decision = json!({
        "ts": crate::now_iso_string(),
        "step_id": step_id,
        "decision_key": decision_key,
        "output": output_json,
        "evidence": evidence.unwrap_or(Value::Null),
        "candidate": candidate.unwrap_or(Value::Null),
    });
    if let Some(list) = decisions.as_array_mut() {
        list.push(decision);
    } else {
        root.insert("decisions".to_string(), Value::Array(vec![decision]));
    }
    let _ = crate::storage::write_json_file(&path, &Value::Object(root));
}

pub fn record_outcome(ctx: &ArtifactContext, status: &str, data_json: Value) {
    if !ctx.enabled {
        return;
    }
    let payload = json!({
        "run_id": ctx.run_id,
        "status": status,
        "finished_at": crate::now_iso_string(),
        "data": data_json,
    });
    let _ = write_value(ctx, "outcome.json", &payload);
}

pub fn merge_runtime_data(ctx: &ArtifactContext, data: Value) {
    if !ctx.enabled {
        return;
    }
    let mut guard = ctx.runtime_data.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(map) = data.as_object() {
        for (key, value) in map {
            guard.insert(key.to_string(), value.clone());
        }
    }
}

pub fn runtime_data(ctx: &ArtifactContext) -> Value {
    let guard = ctx.runtime_data.lock().unwrap_or_else(|p| p.into_inner());
    Value::Object(guard.clone())
}

fn write_value(ctx: &ArtifactContext, file_name: &str, payload: &Value) -> anyhow::Result<()> {
    if !ctx.enabled {
        return Ok(());
    }
    let path = ctx.run_dir.join(file_name);
    crate::storage::write_json_file(&path, payload)
}
