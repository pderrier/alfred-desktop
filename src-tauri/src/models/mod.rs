use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── Portfolio position ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub ticker: String,
    pub nom: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isin: Option<String>,
    pub quantite: f64,
    pub prix_actuel: f64,
    pub prix_revient: f64,
    pub valeur_actuelle: f64,
    pub plus_moins_value: f64,
    pub plus_moins_value_pct: f64,
    pub compte: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub position_type: Option<String>,
}

// ── Enrichment issue ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentIssue {
    pub ticker: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
}

impl EnrichmentIssue {
    /// Build from a raw issue Value (as returned by enrichment pipeline).
    pub fn from_raw(ticker: &str, name: &str, issue: &Value, with_timestamp: bool) -> Self {
        Self {
            ticker: ticker.to_string(),
            nom: if name.is_empty() { None } else { Some(name.to_string()) },
            scope: issue.get("scope").and_then(|v| v.as_str()).map(String::from),
            error_code: issue.get("error_code").and_then(|v| v.as_str()).map(String::from),
            message: issue.get("message").and_then(|v| v.as_str()).map(String::from),
            provider: issue.get("provider").and_then(|v| v.as_str()).map(String::from),
            upstream_status: issue.get("upstream_status").cloned(),
            at: if with_timestamp { Some(crate::now_iso_string()) } else { None },
            issue_type: Some("enrichment_fetch_failed".to_string()),
        }
    }

    /// Build a failure record (no timestamp, no type, no nom).
    pub fn as_failure(&self) -> Self {
        Self {
            ticker: self.ticker.clone(),
            nom: None,
            scope: self.scope.clone(),
            error_code: self.error_code.clone(),
            message: self.message.clone(),
            provider: self.provider.clone(),
            upstream_status: self.upstream_status.clone(),
            at: None,
            issue_type: None,
        }
    }
}

// ── Line recommendation (pushed to frontend via alfred://line-done) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineRecommendationSummary {
    pub ticker: String,
    pub signal: String,
    pub conviction: String,
    pub synthese: String,
    pub action_recommandee: String,
    #[serde(rename = "type")]
    pub line_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reanalyse_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reanalyse_reason: Option<String>,
}

impl LineRecommendationSummary {
    pub fn from_json(v: &serde_json::Value) -> Self {
        Self {
            ticker: v.get("ticker").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            signal: v.get("signal").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            conviction: v.get("conviction").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            synthese: v.get("synthese").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            action_recommandee: v.get("action_recommandee").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            line_type: v.get("type").and_then(|v| v.as_str()).unwrap_or("position").to_string(),
            reanalyse_after: v.get("reanalyse_after").and_then(|v| v.as_str()).map(String::from),
            reanalyse_reason: v.get("reanalyse_reason").and_then(|v| v.as_str()).map(String::from),
        }
    }
}

// ── Line done event (Tauri push) ────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct LineDoneEvent {
    pub run_id: String,
    pub ticker: String,
    pub recommendation: LineRecommendationSummary,
    pub line_progress: LineProgress,
}

// ── Progress counters ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineProgress {
    pub completed: usize,
    pub total: usize,
}

impl LineProgress {
    pub fn new(completed: usize, total: usize) -> Self {
        Self { completed, total }
    }
}

// ── Run orchestration state ─────────────────────────────────────

/// Typed orchestration state — replaces ~15 inline json!({}) read-modify-write blocks.
/// Use `from_run_state()` to load, mutate fields, then `apply_to()` to persist.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunOrchestration {
    pub status: String,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_progress: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_progress: Option<Value>,
    #[serde(default)]
    pub degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degradation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degradation_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl RunOrchestration {
    /// Load from an existing run_state JSON (reads the "orchestration" key).
    pub fn from_run_state(run_state: &Value) -> Self {
        run_state
            .get("orchestration")
            .and_then(|v| serde_json::from_value::<Self>(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Write this orchestration back into a mutable run_state object.
    pub fn apply_to(&self, run_state: &mut Value) {
        if let Some(obj) = run_state.as_object_mut() {
            obj.insert(
                "orchestration".to_string(),
                serde_json::to_value(self).unwrap_or_default(),
            );
            obj.insert("updated_at".to_string(), json!(crate::now_iso_string()));
        }
    }

    /// Set running stage with optional progress counters.
    pub fn set_running(&mut self, stage: &str, collection_progress: Option<Value>, line_progress: Option<Value>) {
        self.status = "running".to_string();
        self.stage = stage.to_string();
        self.updated_at = Some(crate::now_iso_string());
        if let Some(cp) = collection_progress {
            self.collection_progress = Some(cp);
        }
        if let Some(lp) = line_progress {
            self.line_progress = Some(lp);
        }
    }

    /// Mark completed successfully.
    pub fn set_completed(&mut self) {
        self.status = "completed".to_string();
        self.stage = "completed".to_string();
        self.degraded = false;
        self.degradation_reason = None;
        self.degradation_message = None;
        self.finished_at = Some(crate::now_iso_string());
        self.updated_at = Some(crate::now_iso_string());
    }

}

