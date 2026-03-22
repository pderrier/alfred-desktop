use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::{now_iso_string, patch_run_state_direct_with};

pub(crate) fn persist_native_collection_state(run_id: &str, collection_state: &Value) -> Result<()> {
    let collection_object = collection_state
        .as_object()
        .ok_or_else(|| anyhow!("native_collection_state_invalid"))?;
    patch_run_state_direct_with(run_id, |run_state| {
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
