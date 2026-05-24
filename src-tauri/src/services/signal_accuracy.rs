//! P2-24 (2026-05-23) — cross-portfolio aggregation of Alfred's own
//! signal track record.
//!
//! Reads `line-memory.json` and accumulates the
//! `price_tracking.signal_accuracy` field that is already pre-computed
//! per ticker (see `services/native_mcp_analysis.rs` for the writer).
//! Each ticker carries :
//!   - `last_signal`   : "ACHAT" / "VENTE" / "CONSERVER" / etc.
//!   - `price_at_signal` : numeric anchor
//!   - `current_price`   : numeric live quote
//!   - `return_since_signal_pct` : pre-computed delta
//!   - `signal_accuracy` : "correct" / "incorrect" / "first_analysis" / "unknown"
//!
//! Returns `{total_signals, accuracy_count, accuracy_pct, best_pick,
//! worst_pick}` shaped for direct rendering on the home Section 8.
//! Surface gated on `total_signals >= 5` by the caller — sub-5 tickers
//! is statistically meaningless.

use anyhow::Result;
use serde_json::{json, Value};
use std::fs;

use crate::paths::resolve_runtime_state_dir;

#[derive(Debug, Clone)]
struct Pick {
    ticker: String,
    signal: String,
    return_pct: f64,
}

/// P1-59 (2026-05-24) — per-conviction-tier accuracy bucket.
///
/// `n` is `correct + incorrect`. We keep all three counters even when
/// `n == 0` so the JSON shape is stable across runs (the home/UI and
/// the LLM calibration block both expect every tier to exist).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConvictionStats {
    pub correct: usize,
    pub incorrect: usize,
    pub n: usize,
}

/// P1-59 — three-tier breakdown of `SignalAccuracyStats`. Entries with
/// missing/invalid `conviction` still contribute to `total_signals` /
/// `correct` / `incorrect` at the parent level but are NOT bucketed into
/// any tier — `forte + moderee + faible` may be `<= total_signals`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ConvictionBreakdown {
    pub forte: ConvictionStats,
    pub moderee: ConvictionStats,
    pub faible: ConvictionStats,
}

#[derive(Debug, Default, Clone)]
pub struct SignalAccuracyStats {
    pub total_signals: usize,
    pub correct: usize,
    pub incorrect: usize,
    pub best_pick: Option<(String, String, f64)>,  // (ticker, signal, return_pct)
    pub worst_pick: Option<(String, String, f64)>,
    /// P1-59 — per-conviction-tier accuracy. Always present (tiers with
    /// `n == 0` are still serialized) so downstream consumers (LLM
    /// calibration block, future UI) can rely on the shape.
    pub by_conviction: ConvictionBreakdown,
}

/// Normalize a raw `conviction` string from line-memory to one of the
/// three canonical tiers — `forte` / `moderee` / `faible`. Returns
/// `None` for anything else (empty, "high", typo, etc.). Mirrors the
/// validator at `mcp_server.rs:1065-1069` so we tier exactly the values
/// the validator accepts as valid.
fn normalize_conviction(raw: &str) -> Option<&'static str> {
    let norm = raw
        .trim()
        .to_lowercase()
        .replace('é', "e")
        .replace('è', "e");
    match norm.as_str() {
        "forte" => Some("forte"),
        "moderee" => Some("moderee"),
        "faible" => Some("faible"),
        _ => None,
    }
}

/// Read the line-memory file and aggregate accuracy across all tickers.
pub fn compute_signal_accuracy() -> Result<SignalAccuracyStats> {
    let path = resolve_runtime_state_dir().join("line-memory.json");
    if !path.exists() {
        return Ok(SignalAccuracyStats::default());
    }
    let raw = fs::read_to_string(&path)?;
    let store: Value = serde_json::from_str(&raw)?;
    let by_ticker = store.get("by_ticker").and_then(|v| v.as_object());
    let Some(map) = by_ticker else { return Ok(SignalAccuracyStats::default()); };
    Ok(aggregate_from_by_ticker(map))
}

/// Pure aggregator — exposed for unit testing without disk I/O. Walks
/// the `by_ticker` map, counts correct/incorrect, picks the best/worst
/// by absolute return among scored entries.
pub(crate) fn aggregate_from_by_ticker(
    by_ticker: &serde_json::Map<String, Value>,
) -> SignalAccuracyStats {
    let mut stats = SignalAccuracyStats::default();
    let mut correct_picks: Vec<Pick> = Vec::new();
    let mut incorrect_picks: Vec<Pick> = Vec::new();

    for (ticker, entry) in by_ticker {
        let pt = match entry.get("price_tracking").and_then(|v| v.as_object()) {
            Some(p) => p,
            None => continue,
        };
        let accuracy = pt.get("signal_accuracy").and_then(|v| v.as_str()).unwrap_or("");
        match accuracy {
            "correct" | "incorrect" => {}
            _ => continue, // skip unknown / first_analysis / absent
        }
        stats.total_signals += 1;
        let signal = pt.get("last_signal").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        let return_pct = pt
            .get("return_since_signal_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let pick = Pick { ticker: ticker.clone(), signal, return_pct };

        // P1-59: tier the entry by its top-level `conviction` field
        // (written by `sync_line_memory`, see native_mcp_analysis.rs:997).
        // Invalid/missing convictions still count in totals but skip
        // tiering — i.e. forte+moderee+faible can be < total_signals.
        let conviction_raw = entry.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let tier_target: Option<&mut ConvictionStats> = match normalize_conviction(conviction_raw) {
            Some("forte") => Some(&mut stats.by_conviction.forte),
            Some("moderee") => Some(&mut stats.by_conviction.moderee),
            Some("faible") => Some(&mut stats.by_conviction.faible),
            _ => None,
        };

        if accuracy == "correct" {
            stats.correct += 1;
            correct_picks.push(pick);
            if let Some(tier) = tier_target {
                tier.correct += 1;
                tier.n += 1;
            }
        } else {
            stats.incorrect += 1;
            incorrect_picks.push(pick);
            if let Some(tier) = tier_target {
                tier.incorrect += 1;
                tier.n += 1;
            }
        }
    }

    // Best pick = correct call with the largest absolute return (most
    // impactful win). Worst pick = incorrect call with the largest
    // absolute return (most painful miss).
    correct_picks.sort_by(|a, b| {
        b.return_pct.abs().partial_cmp(&a.return_pct.abs()).unwrap_or(std::cmp::Ordering::Equal)
    });
    incorrect_picks.sort_by(|a, b| {
        b.return_pct.abs().partial_cmp(&a.return_pct.abs()).unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(p) = correct_picks.first() {
        stats.best_pick = Some((p.ticker.clone(), p.signal.clone(), p.return_pct));
    }
    if let Some(p) = incorrect_picks.first() {
        stats.worst_pick = Some((p.ticker.clone(), p.signal.clone(), p.return_pct));
    }
    stats
}

/// Serialize stats into the Tauri-bridge envelope shape expected by
/// the JS layer (`apps/alfred-desktop/src/desktop-shell/app.js`
/// section 8). Numbers stay raw (the JS renderer formats them).
pub fn stats_to_json(stats: &SignalAccuracyStats) -> Value {
    let accuracy_pct = if stats.total_signals > 0 {
        (stats.correct as f64) * 100.0 / (stats.total_signals as f64)
    } else {
        0.0
    };
    // P1-59 — additive `by_conviction` block. The three tiers are
    // ALWAYS present in the JSON shape (even with n=0) so downstream
    // consumers can read `by_conviction.forte.n` without a defensive
    // existence check. UI contract: existing top-level keys
    // (`total_signals`, `correct`, `incorrect`, `accuracy_pct`,
    // `best_pick`, `worst_pick`) are NEVER reshaped — only this new
    // key is added.
    let tier_json = |t: &ConvictionStats| {
        json!({"correct": t.correct, "incorrect": t.incorrect, "n": t.n})
    };
    json!({
        "total_signals": stats.total_signals,
        "correct": stats.correct,
        "incorrect": stats.incorrect,
        "accuracy_pct": accuracy_pct,
        "best_pick": stats.best_pick.as_ref().map(|(t, s, r)| json!({
            "ticker": t, "signal": s, "return_pct": r,
        })),
        "worst_pick": stats.worst_pick.as_ref().map(|(t, s, r)| json!({
            "ticker": t, "signal": s, "return_pct": r,
        })),
        "by_conviction": {
            "forte": tier_json(&stats.by_conviction.forte),
            "moderee": tier_json(&stats.by_conviction.moderee),
            "faible": tier_json(&stats.by_conviction.faible),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(signal: &str, accuracy: &str, return_pct: f64) -> Value {
        json!({
            "price_tracking": {
                "last_signal": signal,
                "signal_accuracy": accuracy,
                "return_since_signal_pct": return_pct,
                "current_price": 100.0,
                "price_at_signal": 100.0,
            }
        })
    }

    #[test]
    fn aggregates_correct_vs_incorrect_counts() {
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry("ACHAT", "correct", 5.0));
        map.insert("B".into(), entry("VENTE", "correct", -8.0));
        map.insert("C".into(), entry("ACHAT", "incorrect", -3.0));
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.total_signals, 3);
        assert_eq!(stats.correct, 2);
        assert_eq!(stats.incorrect, 1);
    }

    #[test]
    fn skips_first_analysis_and_unknown_entries() {
        // first_analysis and unknown rows must NOT count toward the
        // total — they're tickers Alfred analysed once but with no
        // prior signal to score against.
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry("CONSERVER", "first_analysis", 0.0));
        map.insert("B".into(), entry("ACHAT", "unknown", 0.0));
        map.insert("C".into(), entry("ACHAT", "correct", 4.0));
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.total_signals, 1, "only scored rows count");
        assert_eq!(stats.correct, 1);
    }

    #[test]
    fn best_pick_is_largest_correct_return_by_abs() {
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry("VENTE", "correct", -8.0));
        map.insert("B".into(), entry("ACHAT", "correct", 12.0));
        map.insert("C".into(), entry("VENTE", "correct", -3.0));
        let stats = aggregate_from_by_ticker(&map);
        let (ticker, signal, ret) = stats.best_pick.expect("must have a best pick");
        assert_eq!(ticker, "B");
        assert_eq!(signal, "ACHAT");
        assert!((ret - 12.0).abs() < 1e-6);
    }

    #[test]
    fn worst_pick_is_largest_incorrect_return_by_abs() {
        let mut map = serde_json::Map::new();
        map.insert("X".into(), entry("ACHAT", "incorrect", -3.0));
        map.insert("Y".into(), entry("CONSERVER", "incorrect", -14.6));
        map.insert("Z".into(), entry("VENTE", "incorrect", 7.0));
        let stats = aggregate_from_by_ticker(&map);
        let (ticker, _, ret) = stats.worst_pick.expect("must have a worst pick");
        assert_eq!(ticker, "Y", "biggest pain wins");
        assert!((ret - (-14.6)).abs() < 1e-6);
    }

    #[test]
    fn empty_input_returns_zero_stats() {
        let map = serde_json::Map::new();
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.total_signals, 0);
        assert!(stats.best_pick.is_none());
        assert!(stats.worst_pick.is_none());
    }

    #[test]
    fn stats_to_json_computes_accuracy_pct() {
        let stats = SignalAccuracyStats {
            total_signals: 4, correct: 3, incorrect: 1,
            best_pick: Some(("A".into(), "ACHAT".into(), 10.0)),
            worst_pick: None,
            by_conviction: ConvictionBreakdown::default(),
        };
        let value = stats_to_json(&stats);
        assert_eq!(value["total_signals"], 4);
        assert_eq!(value["correct"], 3);
        assert_eq!(value["incorrect"], 1);
        assert!((value["accuracy_pct"].as_f64().unwrap() - 75.0).abs() < 1e-6);
        assert!(value["best_pick"].is_object());
        assert!(value["worst_pick"].is_null());
    }

    #[test]
    fn stats_to_json_zero_signals_yields_zero_pct() {
        let stats = SignalAccuracyStats::default();
        let value = stats_to_json(&stats);
        assert_eq!(value["total_signals"], 0);
        assert!((value["accuracy_pct"].as_f64().unwrap() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn skips_entries_without_price_tracking() {
        let mut map = serde_json::Map::new();
        map.insert("NoPT".into(), json!({"ticker": "X"}));
        map.insert("OK".into(), entry("ACHAT", "correct", 5.0));
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.total_signals, 1);
    }

    // ── P1-59: conviction calibration aggregation ────────────────

    /// Helper to build a line-memory entry with a TOP-LEVEL `conviction`
    /// (mirrors what `sync_line_memory` writes at
    /// `native_mcp_analysis.rs:997`).
    fn entry_with_conviction(
        signal: &str,
        accuracy: &str,
        return_pct: f64,
        conviction: &str,
    ) -> Value {
        json!({
            "conviction": conviction,
            "price_tracking": {
                "last_signal": signal,
                "signal_accuracy": accuracy,
                "return_since_signal_pct": return_pct,
                "current_price": 100.0,
                "price_at_signal": 100.0,
            }
        })
    }

    #[test]
    fn aggregates_by_conviction_basic() {
        let mut map = serde_json::Map::new();
        // forte: 2 correct, 1 incorrect → n=3
        map.insert("F1".into(), entry_with_conviction("ACHAT", "correct", 5.0, "forte"));
        map.insert("F2".into(), entry_with_conviction("ACHAT", "correct", 8.0, "forte"));
        map.insert("F3".into(), entry_with_conviction("VENTE", "incorrect", 2.0, "forte"));
        // moderee: 1 correct, 1 incorrect → n=2
        map.insert("M1".into(), entry_with_conviction("CONSERVER", "correct", 1.0, "moderee"));
        map.insert("M2".into(), entry_with_conviction("ACHAT", "incorrect", -4.0, "moderee"));
        // faible: 0 correct, 1 incorrect → n=1
        map.insert("W1".into(), entry_with_conviction("VENTE", "incorrect", -10.0, "faible"));

        let stats = aggregate_from_by_ticker(&map);
        // Parent totals
        assert_eq!(stats.total_signals, 6);
        assert_eq!(stats.correct, 3);
        assert_eq!(stats.incorrect, 3);
        // Per-tier
        assert_eq!(stats.by_conviction.forte, ConvictionStats { correct: 2, incorrect: 1, n: 3 });
        assert_eq!(stats.by_conviction.moderee, ConvictionStats { correct: 1, incorrect: 1, n: 2 });
        assert_eq!(stats.by_conviction.faible, ConvictionStats { correct: 0, incorrect: 1, n: 1 });
    }

    #[test]
    fn aggregates_by_conviction_handles_accent_normalization() {
        // The validator at mcp_server.rs:1067-1068 normalizes é/è → e
        // before checking the tier. The aggregator must mirror that —
        // an entry persisted with accented "modérée" still bumps the
        // `moderee` tier.
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry_with_conviction("ACHAT", "correct", 5.0, "Modérée"));
        map.insert("B".into(), entry_with_conviction("ACHAT", "correct", 7.0, "FAIBLE")); // case
        map.insert("C".into(), entry_with_conviction("ACHAT", "incorrect", -1.0, "  forte  ")); // whitespace
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.by_conviction.moderee.n, 1, "moderée maps to moderee");
        assert_eq!(stats.by_conviction.moderee.correct, 1);
        assert_eq!(stats.by_conviction.faible.n, 1, "FAIBLE maps to faible (case)");
        assert_eq!(stats.by_conviction.faible.correct, 1);
        assert_eq!(stats.by_conviction.forte.n, 1, "trimmed whitespace tier");
        assert_eq!(stats.by_conviction.forte.incorrect, 1);
    }

    #[test]
    fn aggregates_by_conviction_skips_invalid_conviction() {
        // Invalid convictions (typo / English / empty) must still count
        // in the parent `total_signals` / `correct` / `incorrect` but
        // NOT bump any tier — i.e. tier sum < total_signals is OK.
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry_with_conviction("ACHAT", "correct", 5.0, "high"));
        map.insert("B".into(), entry_with_conviction("VENTE", "correct", 3.0, ""));
        map.insert("C".into(), entry_with_conviction("ACHAT", "incorrect", -2.0, "strong"));
        let stats = aggregate_from_by_ticker(&map);
        assert_eq!(stats.total_signals, 3, "all 3 scored rows in parent total");
        assert_eq!(stats.correct, 2);
        assert_eq!(stats.incorrect, 1);
        // None of the 3 tiers should have been touched
        assert_eq!(stats.by_conviction.forte.n, 0);
        assert_eq!(stats.by_conviction.moderee.n, 0);
        assert_eq!(stats.by_conviction.faible.n, 0);
    }

    #[test]
    fn aggregates_by_conviction_zero_tier_stable_shape() {
        // A tier with no signals must STILL be present in the JSON output
        // (UI/LLM consumers should not have to existence-check tiers).
        let mut map = serde_json::Map::new();
        map.insert("A".into(), entry_with_conviction("ACHAT", "correct", 5.0, "forte"));
        // No moderee or faible entries → those tiers stay at n=0 but
        // must still serialize.
        let stats = aggregate_from_by_ticker(&map);
        let value = stats_to_json(&stats);
        let by_conv = value.get("by_conviction").expect("by_conviction must be present");
        assert!(by_conv.get("forte").is_some());
        assert!(by_conv.get("moderee").is_some(), "zero-tier moderee must still serialize");
        assert!(by_conv.get("faible").is_some(), "zero-tier faible must still serialize");
        assert_eq!(by_conv["forte"]["n"], 1);
        assert_eq!(by_conv["forte"]["correct"], 1);
        assert_eq!(by_conv["forte"]["incorrect"], 0);
        // zero tiers serialize as {correct:0, incorrect:0, n:0}
        assert_eq!(by_conv["moderee"]["n"], 0);
        assert_eq!(by_conv["moderee"]["correct"], 0);
        assert_eq!(by_conv["moderee"]["incorrect"], 0);
        assert_eq!(by_conv["faible"]["n"], 0);
    }

    #[test]
    fn stats_to_json_preserves_existing_top_level_keys() {
        // Snapshot UI contract: adding `by_conviction` must NOT alter the
        // shape of the existing top-level keys consumed by app.js section 8.
        let stats = SignalAccuracyStats {
            total_signals: 4, correct: 3, incorrect: 1,
            best_pick: Some(("A".into(), "ACHAT".into(), 10.0)),
            worst_pick: Some(("B".into(), "VENTE".into(), -7.5)),
            by_conviction: ConvictionBreakdown::default(),
        };
        let value = stats_to_json(&stats);
        // Existing keys, byte-identical shape
        assert_eq!(value["total_signals"], 4);
        assert_eq!(value["correct"], 3);
        assert_eq!(value["incorrect"], 1);
        assert!((value["accuracy_pct"].as_f64().unwrap() - 75.0).abs() < 1e-6);
        assert_eq!(value["best_pick"]["ticker"], "A");
        assert_eq!(value["best_pick"]["signal"], "ACHAT");
        assert!((value["best_pick"]["return_pct"].as_f64().unwrap() - 10.0).abs() < 1e-6);
        assert_eq!(value["worst_pick"]["ticker"], "B");
        // And the new key is also present
        assert!(value.get("by_conviction").is_some());
    }
}
