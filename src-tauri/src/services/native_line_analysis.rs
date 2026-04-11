use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::now_iso_string;

pub(crate) fn persist_native_collection_state(run_id: &str, collection_state: &Value) -> Result<()> {
    let collection_object = collection_state
        .as_object()
        .ok_or_else(|| anyhow!("native_collection_state_invalid"))?;
    let data_dir = crate::paths::resolve_runtime_state_dir()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(crate::paths::default_data_dir);
    crate::run_state_cache::patch(&data_dir, run_id, |run_state| {
        if let Some(object) = run_state.as_object_mut() {
            for key in [
                "portfolio",
                "transactions",
                "orders",
                "market",
                "news",
                "quality",
                "collection_issues",
                "enrichment",
                "source_ingestion",
                "normalization",
                "line_memory_hydration",
            ] {
                if let Some(value) = collection_object.get(key) {
                    object.insert(key.to_string(), value.clone());
                }
            }
            object.insert("updated_at".to_string(), Value::String(now_iso_string()));
        }
    })?;
    Ok(())
}
