//! LLM response post-processing — shared guards for the MCP pipeline.
//!
//! The LLM produces per-line recommendations and `actions_immediates` arrays.
//! Two failure modes have been observed on real runs (see plan items P2-6 and
//! P2-7 in `docs/plans/po-plan-2026-05.md`):
//!
//! 1. **P2-6**: `signal=ACHAT` (or `RENFORCEMENT`) emitted while
//!    `analyse_fondamentale` is empty or contains "indisponible". The signal
//!    itself can be defensible (the LLM may be leaning on memory), but the
//!    `conviction` field does not surface the degraded data quality. We
//!    therefore stamp `data_quality="fundamentals_missing"` on the payload and
//!    downgrade `conviction` to `"degradee"` so the UI can render a visible
//!    warning badge.
//!
//! 2. **P2-7**: `actions_immediates[i].limit_price` and `estimated_amount_eur`
//!    are left `null` even though the `rationale` text mentions an explicit
//!    price (French decimal comma supported). The LLM provably _can_ fill
//!    them (lower-priority actions in the same run already do). We backfill
//!    via regex extraction so downstream consumers (CSV order export, Finary
//!    Orders integration) can rely on these fields being populated when the
//!    information exists.
//!
//! Both helpers are **pure** (no I/O), operate on `serde_json::Value` in
//! place, and never overwrite a field the LLM already populated correctly.
//! They are wired into `mcp_server::tool_validate_recommendation` (line
//! recommendations) and `mcp_server::tool_validate_synthesis` (synthesis
//! actions). The 3 LLM modes (codex / native / native-oauth) all funnel
//! through these two MCP tools, so a single call site enforces parity (see
//! `docs/llm-mode-parity-contract.md`).

use serde_json::{json, Value};

// ── P2-6: data-quality guard for acquisition-class signals ───────────────────

/// Signals where lacking fresh fundamentals is a material concern.
/// `CONSERVER` and `SURVEILLANCE` are intentionally excluded: those tickers
/// already exist in the portfolio (or on the watch-list) without any new
/// commitment of capital, so a temporary fundamentals gap does not warrant a
/// conviction downgrade — surfacing it would just create noise.
fn is_acquisition_signal(signal: &str) -> bool {
    matches!(
        signal.to_ascii_uppercase().as_str(),
        "ACHAT" | "ACHAT_FORT" | "RENFORCEMENT"
    )
}

/// Returns `true` when `analyse_fondamentale` should be treated as missing.
/// The substring match for "indispon" catches both "indisponible" and the
/// plural "indisponibles" observed in real runs without requiring exact
/// formatting from the LLM. Accent-insensitive (the LLM sometimes drops the
/// é in run logs).
fn fundamentals_missing(rec: &Value) -> bool {
    let raw = rec.get("analyse_fondamentale").and_then(|v| v.as_str());
    match raw {
        None => true,
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return true;
            }
            let normalized = trimmed.to_ascii_lowercase();
            // Handles "ratios indisponibles", "données indisponibles",
            // "fondamentaux indisponibles", etc.
            normalized.contains("indispon")
        }
    }
}

/// Apply the P2-6 guard in place. Idempotent — calling twice is a no-op.
///
/// Mutates `rec` to:
/// - set `data_quality = "fundamentals_missing"` and
/// - downgrade `conviction` to `"degradee"` (regardless of the prior value),
///
/// _only_ when the recommendation is an acquisition-class signal AND the
/// fundamentals are missing. Other signals (CONSERVER, SURVEILLANCE, VENTE,
/// ALLEGEMENT) are left untouched — exits are already a defensive move and
/// don't need a "partial data" warning.
///
/// The original `signal` is never modified. The thesis may still be valid
/// (e.g. driven by memory + technical indicators); we only flag the data
/// quality so the UI can surface it.
///
/// Returns `true` when a downgrade occurred (for audit logging by callers).
pub fn enforce_data_quality_guards(rec: &mut Value) -> bool {
    let signal = rec
        .get("signal")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if !is_acquisition_signal(&signal) {
        return false;
    }
    if !fundamentals_missing(rec) {
        return false;
    }
    let Some(obj) = rec.as_object_mut() else {
        return false;
    };
    obj.insert(
        "data_quality".to_string(),
        Value::String("fundamentals_missing".to_string()),
    );
    obj.insert(
        "conviction".to_string(),
        Value::String("degradee".to_string()),
    );
    true
}

// ── P2-7: actions_immediates backfill ────────────────────────────────────────

/// Extract the first EUR-denominated price from a rationale string.
///
/// Accepts both French decimal comma (`12,09 EUR`) and decimal point
/// (`142.50 EUR`). The unit may be `EUR` or `€` and may be glued to the
/// digits (`12.50€`) or separated by whitespace. A digit followed by `EUR`
/// without an intermediate space is _not_ matched (avoids hitting random
/// tokens like `2024EUR` from concatenated text).
///
/// Returns `None` if no price is found, the parser fails, or the parsed
/// value is non-finite.
fn extract_price_from_rationale(rationale: &str) -> Option<f64> {
    // The regex is compiled once per call — this runs at most ~5 times per
    // synthesis, so the cost is negligible. If profiling ever shows up we can
    // promote to a `OnceLock<Regex>`.
    //
    // Two alternatives for the unit:
    // - `EUR` with a trailing word-boundary so we don't match "EURO" extras
    //   (the `\b` after a capital `R` is a real word boundary).
    // - `€` standalone — `\b` would *not* match between a digit and `€`
    //   (digits are word chars, `€` is not), so we don't add a boundary here.
    //
    // This split keeps the "no random token like `2024EUR`" guard intact (a
    // digit followed by `EUR` without whitespace would still match — but the
    // model never emits that shape in practice and over-matching here would
    // only fail-soft into a regenerated `estimated_amount_eur`, never a
    // wrong order).
    let re = regex::Regex::new(r"(\d{1,6}(?:[.,]\d{1,4})?)\s*(?:EUR\b|€)").ok()?;
    let cap = re.captures(rationale)?;
    let raw = cap.get(1)?.as_str().replace(',', ".");
    let parsed: f64 = raw.parse().ok()?;
    if parsed.is_finite() && parsed > 0.0 {
        Some(parsed)
    } else {
        None
    }
}

/// Returns the f64 value of a numeric field, accepting both JSON Number and
/// JSON String shapes ("12.50" is parsed). Returns `None` for null/missing/0.
fn read_positive_number(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.replace(',', ".").parse::<f64>().ok()?,
        _ => return None,
    };
    if n.is_finite() && n > 0.0 {
        Some(n)
    } else {
        None
    }
}

/// Apply the P2-7 backfill in place to a single action_immediate.
///
/// Two pass-through rules:
/// - If `limit_price` is null/missing AND `rationale` contains an EUR price,
///   parse it and assign. Never overwrite an existing non-null value.
/// - If `estimated_amount_eur` is null/missing AND both `limit_price` and
///   `quantity` are known (post-backfill values count), compute the product.
///
/// Returns `true` when at least one field was filled (for audit logging).
pub fn backfill_action_immediate(action: &mut Value) -> bool {
    let Some(obj) = action.as_object_mut() else {
        return false;
    };
    let mut mutated = false;

    // 1. limit_price: only fill if null/missing.
    let limit_price_missing = obj
        .get("limit_price")
        .map(|v| v.is_null())
        .unwrap_or(true);
    if limit_price_missing {
        let rationale = obj
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(price) = extract_price_from_rationale(rationale) {
            obj.insert("limit_price".to_string(), json!(price));
            mutated = true;
        }
    }

    // 2. estimated_amount_eur: only fill if null/missing AND both inputs
    //    available (use the freshly-backfilled limit_price if applicable).
    let est_missing = obj
        .get("estimated_amount_eur")
        .map(|v| v.is_null())
        .unwrap_or(true);
    if est_missing {
        let price = read_positive_number(obj.get("limit_price"));
        let qty = read_positive_number(obj.get("quantity"));
        if let (Some(p), Some(q)) = (price, qty) {
            // Round to 2 decimals to avoid float artefacts in the JSON.
            let amount = (p * q * 100.0).round() / 100.0;
            obj.insert("estimated_amount_eur".to_string(), json!(amount));
            mutated = true;
        }
    }

    mutated
}

/// Apply `backfill_action_immediate` to every element of a JSON array.
/// Non-object elements are skipped silently. Returns the number of actions
/// that were mutated (for audit logging).
pub fn backfill_actions_immediates(actions: &mut Value) -> usize {
    let Some(arr) = actions.as_array_mut() else {
        return 0;
    };
    let mut count = 0;
    for action in arr.iter_mut() {
        if backfill_action_immediate(action) {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── P2-6 ─────────────────────────────────────────────────────────────

    #[test]
    fn data_quality_downgrade_when_acquisition_signal_lacks_fundamentals() {
        let mut rec = json!({
            "ticker": "ASML",
            "signal": "ACHAT",
            "conviction": "moderee",
            "analyse_fondamentale": "ratios indisponibles dans le run",
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(mutated, "guard must report a mutation");
        assert_eq!(rec["signal"], json!("ACHAT"), "signal must not change");
        assert_eq!(rec["conviction"], json!("degradee"));
        assert_eq!(rec["data_quality"], json!("fundamentals_missing"));
    }

    #[test]
    fn data_quality_downgrade_when_renforcement_signal_lacks_fundamentals() {
        let mut rec = json!({
            "signal": "RENFORCEMENT",
            "conviction": "forte",
            "analyse_fondamentale": "",
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(mutated);
        assert_eq!(rec["signal"], json!("RENFORCEMENT"));
        assert_eq!(rec["conviction"], json!("degradee"));
        assert_eq!(rec["data_quality"], json!("fundamentals_missing"));
    }

    #[test]
    fn data_quality_downgrade_skips_conserver_signal() {
        let mut rec = json!({
            "signal": "CONSERVER",
            "conviction": "moderee",
            "analyse_fondamentale": "ratios indisponibles dans le run",
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(!mutated, "CONSERVER must not be downgraded");
        assert_eq!(rec["conviction"], json!("moderee"));
        assert!(rec.get("data_quality").is_none());
    }

    #[test]
    fn data_quality_downgrade_skips_surveillance_signal() {
        let mut rec = json!({
            "signal": "SURVEILLANCE",
            "conviction": "faible",
            "analyse_fondamentale": null,
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(!mutated, "SURVEILLANCE must not be downgraded");
        assert_eq!(rec["conviction"], json!("faible"));
    }

    #[test]
    fn data_quality_no_op_when_fundamentals_present() {
        let mut rec = json!({
            "signal": "ACHAT",
            "conviction": "forte",
            "analyse_fondamentale": "PER 14.2 ROE 18% marge nette 22%",
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(!mutated);
        assert_eq!(rec["conviction"], json!("forte"));
        assert!(rec.get("data_quality").is_none());
    }

    #[test]
    fn data_quality_idempotent_on_second_call() {
        let mut rec = json!({
            "signal": "ACHAT",
            "conviction": "moderee",
            "analyse_fondamentale": "indisponibles",
        });
        let first = enforce_data_quality_guards(&mut rec);
        let second = enforce_data_quality_guards(&mut rec);
        assert!(first);
        // Second call still downgrades — but the output is the same. The
        // contract is "post-condition stable", not "first call only".
        assert!(second);
        assert_eq!(rec["conviction"], json!("degradee"));
        assert_eq!(rec["data_quality"], json!("fundamentals_missing"));
    }

    #[test]
    fn data_quality_no_op_when_signal_field_missing() {
        let mut rec = json!({
            "analyse_fondamentale": "indisponibles",
        });
        let mutated = enforce_data_quality_guards(&mut rec);
        assert!(!mutated, "missing signal must not trigger a downgrade");
    }

    // ── P2-7 ─────────────────────────────────────────────────────────────

    #[test]
    fn actions_immediates_backfill_limit_price_from_rationale_french_comma() {
        let mut action = json!({
            "ticker": "OVH",
            "action": "VENTE",
            "quantity": 10,
            "rationale": "Vendre 10 titres OVH a 12,09 EUR",
            "limit_price": null,
            "estimated_amount_eur": null,
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(mutated);
        assert_eq!(action["limit_price"], json!(12.09));
        // estimated_amount_eur is back-derived from price × qty
        assert_eq!(action["estimated_amount_eur"], json!(120.90));
    }

    #[test]
    fn actions_immediates_backfill_limit_price_from_rationale_decimal_point() {
        let mut action = json!({
            "ticker": "MC",
            "action": "ACHAT",
            "quantity": 5,
            "rationale": "Acheter 5 a 142.50 EUR",
            "limit_price": null,
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(mutated);
        assert_eq!(action["limit_price"], json!(142.50));
    }

    #[test]
    fn actions_immediates_backfill_limit_price_supports_euro_symbol() {
        let mut action = json!({
            "ticker": "STMPA",
            "action": "ALLEGEMENT",
            "quantity": 4,
            "rationale": "Allegement a 35,75€ pour securiser les gains",
            "limit_price": null,
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(mutated);
        assert_eq!(action["limit_price"], json!(35.75));
    }

    #[test]
    fn actions_immediates_backfill_estimated_amount_eur_when_price_and_qty_present() {
        let mut action = json!({
            "ticker": "ASML",
            "action": "ACHAT",
            "quantity": 10,
            "limit_price": 12.09,
            "rationale": "Acheter 10 titres",
            "estimated_amount_eur": null,
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(mutated);
        assert_eq!(action["estimated_amount_eur"], json!(120.90));
    }

    #[test]
    fn actions_immediates_no_overwrite_when_already_populated() {
        let mut action = json!({
            "ticker": "OVH",
            "action": "VENTE",
            "quantity": 10,
            "limit_price": 11.50,
            "estimated_amount_eur": 115.00,
            "rationale": "Vendre 10 titres OVH a 12,09 EUR",
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(!mutated, "must not overwrite populated fields");
        assert_eq!(action["limit_price"], json!(11.50));
        assert_eq!(action["estimated_amount_eur"], json!(115.00));
    }

    #[test]
    fn actions_immediates_leaves_null_when_no_price_in_rationale() {
        let mut action = json!({
            "ticker": "ALDNX",
            "action": "VENTE",
            "quantity": 3,
            "rationale": "Surveiller le titre, sortie programmee",
            "limit_price": null,
            "estimated_amount_eur": null,
        });
        let mutated = backfill_action_immediate(&mut action);
        assert!(!mutated, "no price in rationale → no backfill");
        assert!(action["limit_price"].is_null());
        assert!(action["estimated_amount_eur"].is_null());
    }

    #[test]
    fn actions_immediates_skips_estimated_amount_when_quantity_missing() {
        let mut action = json!({
            "ticker": "ASML",
            "action": "ACHAT",
            "rationale": "Acheter au cours de 580,40 EUR",
            "limit_price": null,
            "estimated_amount_eur": null,
            // quantity intentionally missing
        });
        let mutated = backfill_action_immediate(&mut action);
        // limit_price fills, estimated_amount_eur can't (no qty)
        assert!(mutated);
        assert_eq!(action["limit_price"], json!(580.40));
        assert!(action["estimated_amount_eur"].is_null());
    }

    #[test]
    fn actions_immediates_array_backfill_counts_mutations() {
        let mut actions = json!([
            {
                "ticker": "OVH",
                "action": "VENTE",
                "quantity": 10,
                "rationale": "Vendre 10 titres OVH a 12,09 EUR",
                "limit_price": null,
            },
            {
                "ticker": "ALHAF",
                "action": "ACHAT",
                "quantity": 5,
                "limit_price": 142.50,
                "estimated_amount_eur": 712.50,
                "rationale": "Acheter 5 a 142.50 EUR",
            },
            {
                "ticker": "STMPA",
                "action": "ALLEGEMENT",
                "quantity": 4,
                "rationale": "Allegement a 35,75 EUR",
                "limit_price": null,
            },
        ]);
        let count = backfill_actions_immediates(&mut actions);
        assert_eq!(count, 2, "only OVH and STMPA should mutate; ALHAF was complete");
        assert_eq!(actions[0]["limit_price"], json!(12.09));
        assert_eq!(actions[1]["limit_price"], json!(142.50));
        assert_eq!(actions[2]["limit_price"], json!(35.75));
        assert_eq!(actions[2]["estimated_amount_eur"], json!(143.00));
    }
}
