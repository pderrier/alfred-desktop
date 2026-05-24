//! Macro pre-run briefing — P1-60.
//!
//! Fetches a 4-indicator macro snapshot (US 10Y yield, VIX, EUR/USD spot,
//! Brent crude) once at run start, persists it into `run_state.macro_briefing`,
//! and exposes a pure renderer the synthesis-prompt builders use to inject
//! a "CONTEXTE MACRO" block.
//!
//! Architecture rationale:
//!   * One-shot at run start (NOT threaded through `build_collection_state`)
//!     — the briefing is portfolio-agnostic and never changes per ticker,
//!     so re-emitting it through every incremental persist would be waste.
//!     `patch_run_state_with` writes it once; the contract test for
//!     `persist_native_collection_state`'s whitelist is therefore not
//!     touched (the field rides outside that propagation path).
//!   * Renderer is a pure function on the persisted `run_state` so
//!     `build_synthesis_prompt` and `build_report_prompt` (both reading
//!     `run_state` to compose) can call it with no side effects, and unit
//!     tests can drive arbitrary fixtures without touching disk.
//!   * Silent degradation: a missing/partial briefing renders as
//!     "no macro section" rather than failing the run. The macro context
//!     is additive — the LLM should always be able to fall back to the
//!     portfolio data alone.

use serde_json::{json, Value};

/// Fetch the macro briefing from `GET /api/macro` and patch it into
/// `run_state.macro_briefing`. Called once per run, right after the
/// `cross_account_context` is built (see
/// `native_collection::execute_native_local_analysis_workflow_with`).
///
/// The fetch is wrapped in `enrichment::fetch_macro_briefing` which
/// already degrades silently on transport/auth failures — when the server
/// returns 503 (`macro_briefing_unavailable`) or the response carries no
/// `macro` field, we store `null` so the renderer treats the run as
/// "no macro section". Never propagates a fetch failure as a run failure.
pub(crate) fn refresh_macro_briefing(run_id: &str) {
    let resp = match crate::enrichment::fetch_macro_briefing() {
        Ok(v) => v,
        Err(e) => {
            // The current `fetch_macro_briefing` is infallible (Ok-wrapped
            // silent-degrade), but keep this arm so a future change that
            // makes the contract fallible won't crash the worker.
            crate::debug_log(&format!(
                "macro briefing fetch errored (will store null): {e}"
            ));
            json!({ "macro": null })
        }
    };

    // Extract the `macro` field (the briefing object) from the envelope.
    // Empty/absent → null. The caller renders null as "no macro section".
    let briefing = resp
        .get("macro")
        .cloned()
        .unwrap_or(Value::Null);

    if let Err(e) = crate::run_state::patch_run_state_with(run_id, |state| {
        if let Some(obj) = state.as_object_mut() {
            obj.insert("macro_briefing".to_string(), briefing.clone());
        }
    }) {
        crate::debug_log(&format!(
            "macro briefing persist failed for run {run_id}: {e}"
        ));
    }
}

/// Render the macro briefing section for a synthesis prompt.
///
/// Returns an empty string when the briefing is absent or carries no
/// usable indicator (so the format-string injection in
/// `build_synthesis_prompt` and `build_report_prompt` adds no whitespace
/// or trailing newlines when there is no data to surface).
///
/// Format (FR, matches the rest of the synthesis prompt tone):
///
/// ```text
/// CONTEXTE MACRO (au moment du run) :
/// - 10Y US : 4.55 %
/// - VIX : 16.7 (volatilité modérée)
/// - EUR/USD : 1.083
/// - Brent : 82.15 USD
/// ```
///
/// Partial data → only the lines we have are emitted. VIX gets a
/// regime-bucket label (faible / modérée / élevée / stress) so the LLM
/// has a human-readable cue without re-thresholding raw numbers. The
/// other 3 indicators are rendered as raw values — they don't have
/// stable categorical regimes the way VIX does.
pub fn build_macro_briefing_section(run_state: &Value) -> String {
    let briefing = match run_state.get("macro_briefing") {
        Some(v) if !v.is_null() => v,
        _ => return String::new(),
    };

    let mut lines: Vec<String> = Vec::new();
    if let Some(v) = read_indicator(briefing, "us_10y_yield") {
        // 10Y US Treasury yield — `^TNX` returns the yield as a percentage
        // (e.g. 4.55 means 4.55%), so render with a `%` suffix.
        lines.push(format!("- 10Y US : {v:.2} %"));
    }
    if let Some(v) = read_indicator(briefing, "vix") {
        let bucket = vix_bucket_label(v);
        lines.push(format!("- VIX : {v:.1} ({bucket})"));
    }
    if let Some(v) = read_indicator(briefing, "eur_usd") {
        // EUR/USD spot — render with 3 decimals (1.083, 1.105) for the
        // typical 0.5–1.5 range.
        lines.push(format!("- EUR/USD : {v:.3}"));
    }
    if let Some(v) = read_indicator(briefing, "brent_usd") {
        lines.push(format!("- Brent : {v:.2} USD"));
    }

    if lines.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nCONTEXTE MACRO (au moment du run) :\n");
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

/// Read a `{value: f64, as_of: String}` indicator's value, returning None
/// when the indicator is absent / null / not a number.
fn read_indicator(briefing: &Value, field: &str) -> Option<f64> {
    briefing
        .get(field)?
        .get("value")?
        .as_f64()
        .filter(|n| n.is_finite())
}

/// VIX regime bucket label. Cuts at <15, 15–25, 25–35, ≥35 — standard
/// thresholds matching the CBOE volatility regime taxonomy.
///
/// Boundaries are inclusive on the low side: `vix_bucket_label(15.0)` is
/// "modérée", not "faible". This matches investor parlance ("VIX above 15
/// is no longer calm") and is the orientation the LLM will read.
pub(crate) fn vix_bucket_label(vix: f64) -> &'static str {
    if vix < 15.0 {
        "volatilité faible"
    } else if vix < 25.0 {
        "volatilité modérée"
    } else if vix < 35.0 {
        "volatilité élevée"
    } else {
        "régime de stress"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn briefing(us_10y: Option<f64>, vix: Option<f64>, eurusd: Option<f64>, brent: Option<f64>) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(v) = us_10y {
            obj.insert("us_10y_yield".into(), json!({"value": v, "as_of": "2026-05-24T15:30:00Z"}));
        } else {
            obj.insert("us_10y_yield".into(), Value::Null);
        }
        if let Some(v) = vix {
            obj.insert("vix".into(), json!({"value": v, "as_of": "2026-05-24T15:30:00Z"}));
        } else {
            obj.insert("vix".into(), Value::Null);
        }
        if let Some(v) = eurusd {
            obj.insert("eur_usd".into(), json!({"value": v, "as_of": "2026-05-24T15:30:00Z"}));
        } else {
            obj.insert("eur_usd".into(), Value::Null);
        }
        if let Some(v) = brent {
            obj.insert("brent_usd".into(), json!({"value": v, "as_of": "2026-05-24T15:30:00Z"}));
        } else {
            obj.insert("brent_usd".into(), Value::Null);
        }
        Value::Object(obj)
    }

    #[test]
    fn renders_fr_block_with_full_data() {
        // Happy path: all 4 indicators present. Pin the exact format so a
        // refactor that drops a line / changes precision / loses the
        // regime label is caught.
        let state = json!({ "macro_briefing": briefing(Some(4.55), Some(16.7), Some(1.083), Some(82.15)) });
        let section = build_macro_briefing_section(&state);
        assert!(section.contains("CONTEXTE MACRO"), "missing header: {section}");
        assert!(section.contains("- 10Y US : 4.55 %"), "10Y format: {section}");
        assert!(section.contains("- VIX : 16.7 (volatilité modérée)"), "VIX line: {section}");
        assert!(section.contains("- EUR/USD : 1.083"), "EUR/USD line: {section}");
        assert!(section.contains("- Brent : 82.15 USD"), "Brent line: {section}");
    }

    #[test]
    fn returns_empty_when_macro_missing() {
        // Field absent entirely → empty string. The prompt format-string
        // injection must produce no extra whitespace / newlines.
        let state = json!({});
        assert_eq!(build_macro_briefing_section(&state), "");

        // Field present but null → also empty.
        let state = json!({ "macro_briefing": Value::Null });
        assert_eq!(build_macro_briefing_section(&state), "");
    }

    #[test]
    fn handles_partial_data() {
        // VIX null but the other 3 succeed: VIX line is omitted, header
        // and the 3 other lines still render. Verifies the "any non-null
        // indicator → render" path and the per-indicator skip.
        let state = json!({ "macro_briefing": briefing(Some(4.55), None, Some(1.083), Some(82.15)) });
        let section = build_macro_briefing_section(&state);
        assert!(section.contains("CONTEXTE MACRO"));
        assert!(section.contains("- 10Y US : 4.55 %"));
        assert!(!section.contains("VIX"), "VIX line should be omitted when null: {section}");
        assert!(section.contains("- EUR/USD : 1.083"));
        assert!(section.contains("- Brent : 82.15 USD"));
    }

    #[test]
    fn handles_all_indicators_null() {
        // Edge case: `macro_briefing` is an object but every indicator is
        // null (rare — happens when the server seeded a stale cache). The
        // renderer must NOT emit a header with no content lines, which
        // would confuse the LLM ("CONTEXTE MACRO :" with nothing after).
        let state = json!({ "macro_briefing": briefing(None, None, None, None) });
        assert_eq!(build_macro_briefing_section(&state), "");
    }

    // ── Prompt-builder integration ──────────────────────────────────
    //
    // Pins that `build_synthesis_prompt_from_state` (native MCP path) and
    // `llm_prompts::build_report_prompt` (native / native-oauth path) BOTH
    // inject the macro section when the run_state carries one, and BOTH
    // emit no macro line when it doesn't. Per
    // `product_llm_mode_parity_2026_04`, the 3 LLM modes must see identical
    // context — this test is the parity guard.

    fn run_state_with_macro() -> Value {
        json!({
            "run_id": "test_run_macro",
            "account": "PEA",
            "portfolio": {
                "positions": [],
                "valeur_totale": 1000.0,
                "plus_value_totale": 0.0,
                "liquidites": 0.0,
            },
            "macro_briefing": briefing(Some(4.55), Some(16.7), Some(1.083), Some(82.15)),
        })
    }

    fn run_state_without_macro() -> Value {
        json!({
            "run_id": "test_run_no_macro",
            "account": "PEA",
            "portfolio": {
                "positions": [],
                "valeur_totale": 1000.0,
                "plus_value_totale": 0.0,
                "liquidites": 0.0,
            },
        })
    }

    #[test]
    fn synthesis_prompt_includes_macro_section_when_data_present() {
        // build_synthesis_prompt_from_state is the testable kernel of the
        // codex-mode synthesis prompt. When the run_state has a populated
        // macro_briefing, the rendered prompt MUST include the FR header
        // and at least the VIX line — the LLM keys off those substrings.
        let state = run_state_with_macro();
        let prompt = crate::native_mcp_analysis::build_synthesis_prompt_from_state(
            "test_run_macro",
            &state,
            "",
        );
        assert!(
            prompt.contains("CONTEXTE MACRO"),
            "synthesis prompt must include macro header: {prompt}"
        );
        assert!(
            prompt.contains("VIX : 16.7"),
            "synthesis prompt must include VIX value: {prompt}"
        );
        assert!(
            prompt.contains("Brent : 82.15"),
            "synthesis prompt must include Brent value: {prompt}"
        );
    }

    #[test]
    fn synthesis_prompt_omits_macro_section_when_data_absent() {
        // Negative path: a run with no macro_briefing field must produce a
        // prompt without the "CONTEXTE MACRO" header — drift here would
        // ship an empty header to the LLM and waste context budget.
        let state = run_state_without_macro();
        let prompt = crate::native_mcp_analysis::build_synthesis_prompt_from_state(
            "test_run_no_macro",
            &state,
            "",
        );
        assert!(
            !prompt.contains("CONTEXTE MACRO"),
            "synthesis prompt must NOT include macro header when data absent: {prompt}"
        );
    }

    #[test]
    fn report_prompt_includes_macro_section_when_data_present() {
        // build_report_prompt is the native / native-oauth equivalent of
        // build_synthesis_prompt. Parity guard: same macro substring must
        // appear in this prompt too when the run_state has the briefing.
        let state = run_state_with_macro();
        let prompt = crate::llm_prompts::build_report_prompt(&state);
        assert!(
            prompt.contains("CONTEXTE MACRO"),
            "report prompt must include macro header: {prompt}"
        );
        assert!(
            prompt.contains("10Y US : 4.55 %"),
            "report prompt must include 10Y line: {prompt}"
        );
        assert!(
            prompt.contains("EUR/USD : 1.083"),
            "report prompt must include EUR/USD line: {prompt}"
        );
    }

    #[test]
    fn report_prompt_omits_macro_section_when_data_absent() {
        let state = run_state_without_macro();
        let prompt = crate::llm_prompts::build_report_prompt(&state);
        assert!(
            !prompt.contains("CONTEXTE MACRO"),
            "report prompt must NOT include macro header when data absent: {prompt}"
        );
    }

    #[test]
    fn vix_bucket_labels_at_boundaries() {
        // Boundary test for the 4 VIX buckets. Pins the inclusive-low
        // orientation: 15.0 is "modérée" (NOT "faible"), 25.0 is "élevée",
        // 35.0 is "stress". Drift here changes the LLM's reading of risk
        // regimes between runs.
        assert_eq!(vix_bucket_label(14.0), "volatilité faible");
        assert_eq!(vix_bucket_label(14.999), "volatilité faible");
        assert_eq!(vix_bucket_label(15.0), "volatilité modérée");
        assert_eq!(vix_bucket_label(20.0), "volatilité modérée");
        assert_eq!(vix_bucket_label(24.999), "volatilité modérée");
        assert_eq!(vix_bucket_label(25.0), "volatilité élevée");
        assert_eq!(vix_bucket_label(30.0), "volatilité élevée");
        assert_eq!(vix_bucket_label(34.999), "volatilité élevée");
        assert_eq!(vix_bucket_label(35.0), "régime de stress");
        assert_eq!(vix_bucket_label(80.0), "régime de stress");
    }
}
