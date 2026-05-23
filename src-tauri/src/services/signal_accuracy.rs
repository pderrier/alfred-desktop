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

#[derive(Debug, Default, Clone)]
pub struct SignalAccuracyStats {
    pub total_signals: usize,
    pub correct: usize,
    pub incorrect: usize,
    pub best_pick: Option<(String, String, f64)>,  // (ticker, signal, return_pct)
    pub worst_pick: Option<(String, String, f64)>,
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
        if accuracy == "correct" {
            stats.correct += 1;
            correct_picks.push(pick);
        } else {
            stats.incorrect += 1;
            incorrect_picks.push(pick);
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
}
