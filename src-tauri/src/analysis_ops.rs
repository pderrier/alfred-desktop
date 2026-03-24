use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
};

use anyhow::{anyhow, Result};
use serde_json::json;

use crate::health;
use crate::helpers::{infer_error_code, new_analysis_operation_id, now_epoch_ms};
use crate::native_collection::execute_native_local_analysis_workflow;
use crate::native_line_analysis::persist_native_collection_state;
use crate::run_state;

// ── Cancellation registry ────────────────────────────────────────────────

// Cancel flags keyed by operation_id
static CANCEL_FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn cancel_registry() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    CANCEL_FLAGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_cancel_flag(operation_id: &str) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    if let Ok(mut store) = cancel_registry().lock() {
        store.insert(operation_id.to_string(), Arc::clone(&flag));
    }
    flag
}

fn unregister_cancel_flag(operation_id: &str) {
    if let Ok(mut store) = cancel_registry().lock() {
        store.remove(operation_id);
    }
}

/// Request cancellation of a running analysis operation.
pub fn request_cancellation(operation_id: &str) -> Result<serde_json::Value> {
    let safe_id = operation_id.trim();
    if safe_id.is_empty() {
        return Err(anyhow!("analysis_operation_id_required"));
    }
    // Set the cancel flag
    let found = if let Ok(store) = cancel_registry().lock() {
        if let Some(flag) = store.get(safe_id) {
            flag.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    } else {
        false
    };
    if !found {
        return Err(anyhow!("analysis_operation_not_found:{safe_id}"));
    }
    crate::debug_log(&format!("analysis: cancellation requested for {safe_id}"));
    // Kill any active codex child processes immediately
    crate::codex::kill_all_active();
    // Mark the run state as aborted + all "analyzing" lines as aborted
    if let Ok(ops) = ops_store().lock() {
        if let Some(record) = ops.get(safe_id) {
            if let Some(run_id) = record.run_id.as_ref() {
                // Flush cache before patching (abort may race with tool writes)
                crate::run_state_cache::flush_now(run_id);
                let _ = run_state::patch_run_state_with(run_id, |state| {
                    if let Some(obj) = state.as_object_mut() {
                        obj.insert("orchestration".to_string(), json!({
                            "status": "aborted",
                            "stage": "aborted",
                            "error_code": "run_aborted",
                            "error_message": "Run was stopped by user.",
                            "updated_at": crate::helpers::now_iso_string()
                        }));
                        // Mark all in-progress lines as aborted
                        if let Some(line_status) = obj.get_mut("line_status").and_then(|v| v.as_object_mut()) {
                            let active = ["analyzing", "repairing", "pending", "collecting"];
                            for (_ticker, status) in line_status.iter_mut() {
                                // Status can be a string or {"status": "...", "progress": "..."}
                                let s = status.as_str()
                                    .or_else(|| status.get("status").and_then(|v| v.as_str()))
                                    .unwrap_or_default();
                                if active.contains(&s) {
                                    *status = json!("aborted");
                                }
                            }
                        }
                    }
                });
                crate::run_state_cache::evict(run_id);
            }
        }
    }
    Ok(json!({ "ok": true, "operation_id": safe_id, "cancelled": true }))
}

fn is_cancelled(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

// ── Global cancel check (callable from worker threads by operation_id) ───

/// Check if any running operation has been cancelled. Used by dispatch workers.
pub fn is_any_operation_cancelled_for_run(run_id: &str) -> bool {
    if let Ok(ops) = ops_store().lock() {
        for record in ops.values() {
            if record.run_id.as_deref() == Some(run_id) {
                if let Ok(flags) = cancel_registry().lock() {
                    if let Some(flag) = flags.get(&record.operation_id) {
                        if flag.load(Ordering::Relaxed) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

#[derive(Clone)]
pub struct AnalysisOperationRecord {
    pub operation_id: String,
    pub status: String,
    pub stage: String,
    pub run_id: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub result: Option<serde_json::Value>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub collection_progress: Option<serde_json::Value>,
    pub line_progress: Option<serde_json::Value>,
    pub line_status: Option<serde_json::Value>,
}

static ANALYSIS_OPS: OnceLock<Mutex<HashMap<String, AnalysisOperationRecord>>> = OnceLock::new();

pub fn ops_store() -> &'static Mutex<HashMap<String, AnalysisOperationRecord>> {
    ANALYSIS_OPS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn start_analysis(options: Option<serde_json::Value>) -> Result<serde_json::Value> {
    crate::debug_log("analysis: start_analysis called");
    health::run_preflight(options.as_ref())?;
    let initialized = run_state::initialize_with_control_plane(options.as_ref())?;
    let run_id = initialized
        .get("run_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing_run_id"))?
        .to_string();
    let mut worker_options = options.unwrap_or_else(|| json!({}));
    if let Some(object) = worker_options.as_object_mut() {
        object.insert("run_id".to_string(), serde_json::Value::String(run_id.clone()));
        object.insert(
            "__native_preflight_completed".to_string(),
            serde_json::Value::Bool(health::is_preflight_enabled()),
        );
    } else {
        worker_options = json!({
            "run_id": run_id.clone(),
            "__native_preflight_completed": health::is_preflight_enabled()
        });
    }
    let operation_id = new_analysis_operation_id();
    let started_at_ms = now_epoch_ms();
    {
        let mut store = ops_store()
            .lock()
            .map_err(|_| anyhow!("analysis_run_start_local_failed:state_lock_poisoned"))?;
        store.insert(
            operation_id.clone(),
            AnalysisOperationRecord {
                operation_id: operation_id.clone(),
                status: "running".to_string(),
                stage: "starting".to_string(),
                run_id: Some(run_id.clone()),
                started_at_ms,
                finished_at_ms: None,
                result: None,
                error_code: None,
                error_message: None,
                collection_progress: None,
                line_progress: None,
                line_status: None,
            },
        );
    }

    let cancel_flag = register_cancel_flag(&operation_id);
    let operation_id_for_thread = operation_id.clone();
    let worker_options_for_thread = worker_options.clone();
    let cancel_flag_for_thread = Arc::clone(&cancel_flag);
    thread::spawn(move || {
        let llm_token = worker_options_for_thread
            .get("llm_token")
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        crate::debug_log(&format!("analysis: worker thread started for {}", operation_id_for_thread));
        let outcome =
            execute_native_local_analysis_workflow(Some(worker_options_for_thread), llm_token.as_deref());
        let finished_at_ms = now_epoch_ms();
        match &outcome {
            Ok(_) => crate::debug_log(&format!("analysis: worker completed for {}", operation_id_for_thread)),
            Err(e) => crate::debug_log(&format!("analysis: worker FAILED for {}: {}", operation_id_for_thread, e)),
        }
        // Cleanup: flush cache, unregister cancel flag
        crate::run_state_cache::flush_to_disk();
        unregister_cancel_flag(&operation_id_for_thread);
        if let Ok(ref payload) = outcome {
            if let Some(rid) = payload.get("result").and_then(|v| v.get("run_id")).and_then(|v| v.as_str()) {
                crate::run_state_cache::clear_run(rid);
            }
        }
        // If cancelled, override outcome to aborted
        let outcome = if is_cancelled(&cancel_flag_for_thread) {
            crate::debug_log(&format!("analysis: worker cancelled for {}", operation_id_for_thread));
            Err(anyhow!("run_aborted:analysis stopped by user"))
        } else {
            outcome
        };
        if let Ok(mut store) = ops_store().lock() {
            if let Some(record) = store.get_mut(&operation_id_for_thread) {
                match outcome {
                    Ok(payload) => {
                        let worker_result = payload
                            .get("result")
                            .cloned()
                            .unwrap_or_else(|| json!({}));
                        let run_id = worker_result
                            .get("run_id")
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string());
                        let final_payload_result = (|| -> Result<serde_json::Value> {
                            if let Some(collection_state) = worker_result
                                .get("collection")
                                .and_then(|value| value.get("collection_state"))
                            {
                                if let Some(delegated_run_id) = run_id.clone() {
                                    persist_native_collection_state(&delegated_run_id, collection_state)?;
                                }
                            }
                            if worker_result
                                .get("finalization_delegated")
                                .and_then(|v| v.as_bool())
                                == Some(true)
                            {
                                match run_id.clone() {
                                    Some(delegated_run_id) => {
                                        // catch_unwind: a panic in the synthesis (e.g. UTF-8 boundary)
                                        // must not leave the run stuck in "running" forever
                                        let synthesis_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                            let data_dir = crate::resolve_runtime_state_dir()
                                                .parent()
                                                .map(|p| p.to_string_lossy().to_string())
                                                .unwrap_or_else(|| ".".to_string());
                                            crate::native_mcp_analysis::run_synthesis_turn(&delegated_run_id, &data_dir)
                                        }));
                                        match synthesis_result {
                                            Ok(inner) => inner,
                                            Err(panic_info) => {
                                                let msg = panic_info
                                                    .downcast_ref::<String>()
                                                    .map(|s| s.as_str())
                                                    .or_else(|| panic_info.downcast_ref::<&str>().copied())
                                                    .unwrap_or("unknown panic");
                                                crate::debug_log(&format!("analysis: synthesis panicked: {msg}"));
                                                Err(anyhow!("synthesis_panicked:{msg}"))
                                            }
                                        }
                                    }.map(|finalized| {
                                            let mut merged_result = worker_result.clone();
                                            if let (Some(source), Some(target)) =
                                                (finalized.as_object(), merged_result.as_object_mut())
                                            {
                                                for (key, value) in source {
                                                    target.insert(key.clone(), value.clone());
                                                }
                                            }
                                            json!({
                                                "ok": true,
                                                "action": "analysis:run-start-local",
                                                "result": merged_result
                                            })
                                        }),
                                    None => Err(anyhow!("analysis_run_start_local_failed:missing_run_id")),
                                }
                            } else {
                                Ok(payload)
                            }
                        })();
                        let final_payload = match final_payload_result {
                            Ok(payload) => payload,
                            Err(error) => {
                                let message = error.to_string();
                                let code = infer_error_code(&message);
                                // Mark run_state on disk as failed (not just ops_store)
                                if let Some(ref rid) = run_id {
                                    let _ = crate::patch_run_state_direct_with(rid, |rs| {
                                        if let Some(obj) = rs.as_object_mut() {
                                            let orch = obj.entry("orchestration".to_string())
                                                .or_insert_with(|| serde_json::json!({}));
                                            if let Some(o) = orch.as_object_mut() {
                                                o.insert("status".to_string(), serde_json::json!("failed"));
                                                o.insert("stage".to_string(), serde_json::json!("failed"));
                                                o.insert("finished_at".to_string(), serde_json::json!(crate::now_iso_string()));
                                                o.insert("error_code".to_string(), serde_json::json!(&code));
                                                o.insert("error_message".to_string(), serde_json::json!(&message));
                                            }
                                        }
                                    });
                                }
                                // Collect stats even on failure (partial data is valuable)
                                if let Some(ref rid) = run_id {
                                    let data_dir = crate::resolve_runtime_state_dir()
                                        .parent().map(|p| p.to_string_lossy().to_string())
                                        .unwrap_or_else(|| ".".to_string());
                                    crate::run_stats::collect_and_persist(rid, &data_dir);
                                }
                                record.status = "failed".to_string();
                                record.stage = "failed".to_string();
                                record.run_id = run_id;
                                record.result = None;
                                record.finished_at_ms = Some(finished_at_ms);
                                record.error_code = Some(code);
                                record.error_message = Some(message);
                                return;
                            }
                        };
                        let final_stage = final_payload
                            .get("result")
                            .and_then(|v| v.get("orchestration_status"))
                            .and_then(|v| v.as_str())
                            .map(|status| {
                                if status == "completed_degraded" {
                                    "completed_degraded".to_string()
                                } else {
                                    "completed".to_string()
                                }
                            })
                            .unwrap_or_else(|| "completed".to_string());
                        // Collect and persist run statistics
                        if let Some(ref rid) = run_id {
                            let data_dir = crate::resolve_runtime_state_dir()
                                .parent().map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| ".".to_string());
                            crate::run_stats::collect_and_persist(rid, &data_dir);
                        }
                        record.status = "completed".to_string();
                        record.stage = final_stage;
                        record.run_id = run_id;
                        record.result = Some(final_payload);
                        record.finished_at_ms = Some(finished_at_ms);
                        record.error_code = None;
                        record.error_message = None;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let error_code = infer_error_code(&message);
                        record.status = "failed".to_string();
                        record.stage = "failed".to_string();
                        record.result = None;
                        record.finished_at_ms = Some(finished_at_ms);
                        record.error_code = Some(error_code.clone());
                        record.error_message = Some(message.clone());
                        // Persist error to run state file for debugging
                        if let Some(rid) = record.run_id.as_ref() {
                            let err_code = error_code.clone();
                            let err_msg = message.clone();
                            let _ = run_state::patch_run_state_with(rid, move |state| {
                                if let Some(obj) = state.as_object_mut() {
                                    obj.insert("orchestration".to_string(), json!({
                                        "status": "failed",
                                        "stage": "failed",
                                        "error_code": err_code,
                                        "error_message": err_msg,
                                        "updated_at": crate::helpers::now_iso_string()
                                    }));
                                }
                            });
                        }
                    }
                }
            }
        }
    });

    Ok(json!({
        "ok": true,
        "action": "analysis:run-start-local",
        "result": {
            "ok": true,
            "operation_id": operation_id,
            "status": "running",
            "stage": "starting",
            "started_at_ms": started_at_ms
        }
    }))
}

pub fn poll_analysis_status(operation_id: String) -> Result<serde_json::Value> {
    let safe_operation_id = operation_id.trim().to_string();
    if safe_operation_id.is_empty() {
        return Err(anyhow!("analysis_operation_id_required"));
    }
    let store = ops_store()
        .lock()
        .map_err(|_| anyhow!("analysis_run_status_local_failed:state_lock_poisoned"))?;
    let mut record = store
        .get(&safe_operation_id)
        .cloned()
        .ok_or_else(|| anyhow!("analysis_operation_not_found:{safe_operation_id}"))?;
    drop(store);

    if record.status == "running" {
        if let Some((
            discovered_run_id,
            discovered_status,
            discovered_stage,
            discovered_collection_progress,
            discovered_line_progress,
            discovered_error_code,
            discovered_error_message,
            discovered_line_status,
        )) = run_state::discover_running_stage(record.run_id.as_ref())
        {
            record.run_id = Some(discovered_run_id);
            record.stage = discovered_stage;
            record.collection_progress = discovered_collection_progress;
            record.line_progress = discovered_line_progress;
            record.line_status = discovered_line_status;
            if discovered_status == "failed" {
                record.status = "failed".to_string();
                record.error_code =
                    Some(discovered_error_code.unwrap_or_else(|| "analysis_run_failed".to_string()));
                record.error_message = Some(
                    discovered_error_message
                        .unwrap_or_else(|| "analysis_run_failed".to_string()),
                );
                record.finished_at_ms = Some(now_epoch_ms());
            }
            if let Ok(mut store_mut) = ops_store().lock() {
                if let Some(current) = store_mut.get_mut(&safe_operation_id) {
                    current.run_id = record.run_id.clone();
                    current.stage = record.stage.clone();
                    current.collection_progress = record.collection_progress.clone();
                    current.line_progress = record.line_progress.clone();
                    current.line_status = record.line_status.clone();
                    if record.status == "failed" {
                        current.status = record.status.clone();
                        current.error_code = record.error_code.clone();
                        current.error_message = record.error_message.clone();
                        current.finished_at_ms = record.finished_at_ms;
                    }
                }
            }
        }
    }
    Ok(json!({
        "ok": true,
        "action": "analysis:run-status-local",
        "result": {
            "ok": true,
            "operation_id": record.operation_id,
            "status": record.status,
            "stage": record.stage,
            "run_id": record.run_id,
            "collection_progress": record.collection_progress,
            "line_progress": record.line_progress,
            "line_status": record.line_status,
            "started_at_ms": record.started_at_ms,
            "finished_at_ms": record.finished_at_ms,
            "result": record.result,
            "error": if record.error_code.is_some() || record.error_message.is_some() {
                json!({
                    "code": record.error_code.clone().unwrap_or_else(|| "analysis_run_failed".to_string()),
                    "message": record.error_message.clone().unwrap_or_else(|| "analysis_run_failed".to_string())
                })
            } else {
                serde_json::Value::Null
            }
        }
    }))
}
