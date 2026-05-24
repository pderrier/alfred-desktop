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
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::paths::resolve_runtime_state_dir;

// ── P2-63 (2026-05-24) — time-based cache for `compute_signal_accuracy` ─
//
// The aggregator reads + parses `line-memory.json` (~1MB in production)
// every time it's called. Three call sites hit it (`command_handlers.rs`
// `run_compute_signal_accuracy` on home render, `llm.rs` per-run
// calibration, `native_mcp_analysis.rs` `flush_batch` per LLM batch) and
// the underlying file is only mutated at the end of a run — so a short
// TTL memo trades zero correctness for a guaranteed disk-read cap.
//
// Pattern mirrors `llm.rs:38-65` (OnceLock<Mutex<…>>) but the cache key
// is TIME, not run_id — the stat is portfolio-level and rolls over
// implicitly when the next run rewrites the file (combined with the
// explicit `invalidate_signal_accuracy_cache()` hook for callers that
// know the file just changed).
const SIGNAL_ACCURACY_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

static SIGNAL_ACCURACY_CACHE: OnceLock<Mutex<Option<(Instant, SignalAccuracyStats)>>> =
    OnceLock::new();

fn signal_accuracy_cache() -> &'static Mutex<Option<(Instant, SignalAccuracyStats)>> {
    SIGNAL_ACCURACY_CACHE.get_or_init(|| Mutex::new(None))
}

/// Invalidate the in-process cache. Safe to call from any thread.
/// Called at the end of `McpBatchDispatchQueue::join_all` (the single
/// deterministic moment when a run has finished writing
/// `line-memory.json` for the whole portfolio — see
/// `services/native_mcp_analysis.rs::McpBatchDispatchQueue::join_all`)
/// and exercised by tests.
pub fn invalidate_signal_accuracy_cache() {
    if let Ok(mut guard) = signal_accuracy_cache().lock() {
        *guard = None;
    }
}

/// Test-only helper to poke the cache slot with an arbitrary `Instant`.
/// Used to simulate TTL expiry deterministically without `thread::sleep`.
#[cfg(test)]
pub(crate) fn set_cache_slot_for_test(at: Instant, stats: SignalAccuracyStats) {
    if let Ok(mut guard) = signal_accuracy_cache().lock() {
        *guard = Some((at, stats));
    }
}

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
///
/// Cached for `SIGNAL_ACCURACY_CACHE_TTL` (5 min). The cache is purely
/// in-process; the returned `SignalAccuracyStats` is cloned per call so
/// callers may freely mutate it. Use `invalidate_signal_accuracy_cache()`
/// to force a re-read (e.g. after a run completes and rewrites the file).
pub fn compute_signal_accuracy() -> Result<SignalAccuracyStats> {
    // Fast path — return cached value if fresh.
    if let Ok(guard) = signal_accuracy_cache().lock() {
        if let Some((computed_at, ref stats)) = *guard {
            if computed_at.elapsed() < SIGNAL_ACCURACY_CACHE_TTL {
                return Ok(stats.clone());
            }
        }
    }

    // Cold path — recompute, cache, return.
    let stats = compute_signal_accuracy_uncached()?;
    if let Ok(mut guard) = signal_accuracy_cache().lock() {
        *guard = Some((Instant::now(), stats.clone()));
    }
    Ok(stats)
}

/// Uncached aggregator — exposed for tests and any caller that
/// explicitly does not want to populate or read the TTL cache. Performs
/// the disk read + JSON parse + aggregation unconditionally.
pub(crate) fn compute_signal_accuracy_uncached() -> Result<SignalAccuracyStats> {
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

    // ── P2-63: TTL cache layer ─────────────────────────────────────
    //
    // All tests below mutate `ALFRED_STATE_DIR` AND the process-global
    // cache slot. Env vars are process-wide and the cache is a static
    // `OnceLock`, so we must serialize against ALL other env-mutating
    // tests in the crate. We reuse the canonical `helpers::test_env_lock`
    // for that (same lock other modules use — see `tests.rs::env_lock`,
    // `llm_prompts.rs` test calls).

    fn unique_state_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "alfred-signal-accuracy-cache-{tag}-{}-{}",
            crate::now_epoch_ms(),
            std::process::id()
        ))
    }

    /// Write a minimal `line-memory.json` with N correct entries.
    fn write_line_memory(dir: &std::path::Path, correct_n: usize) {
        std::fs::create_dir_all(dir).expect("create state dir");
        let mut by_ticker = serde_json::Map::new();
        for i in 0..correct_n {
            by_ticker.insert(format!("T{i}"), entry("ACHAT", "correct", 5.0));
        }
        let store = json!({ "by_ticker": by_ticker });
        std::fs::write(
            dir.join("line-memory.json"),
            serde_json::to_string(&store).unwrap(),
        )
        .expect("write line-memory.json");
    }

    #[test]
    fn cache_returns_first_call_fresh_within_ttl() {
        let _guard = crate::helpers::test_env_lock();
        invalidate_signal_accuracy_cache();
        let base = unique_state_dir("fresh");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        write_line_memory(&base, 3);

        // First call: cold path — populates cache from the file.
        let first = compute_signal_accuracy().expect("first call ok");
        assert_eq!(first.total_signals, 3);
        assert_eq!(first.correct, 3);

        // Delete the file. If the cache is honored, the second call
        // must STILL return the cached value (no disk read).
        std::fs::remove_file(base.join("line-memory.json")).expect("delete file");
        let second = compute_signal_accuracy().expect("second call ok");
        assert_eq!(second.total_signals, 3, "cached value must be returned");
        assert_eq!(second.correct, 3);

        // Cleanup
        invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_recomputes_after_invalidate() {
        let _guard = crate::helpers::test_env_lock();
        invalidate_signal_accuracy_cache();
        let base = unique_state_dir("invalidate");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        write_line_memory(&base, 2);
        let first = compute_signal_accuracy().expect("first call ok");
        assert_eq!(first.total_signals, 2);

        // Mutate the underlying file and invalidate — next call must
        // observe the new value (uncached read).
        write_line_memory(&base, 5);
        invalidate_signal_accuracy_cache();
        let after = compute_signal_accuracy().expect("after invalidate ok");
        assert_eq!(after.total_signals, 5, "invalidate must force re-read");

        // Cleanup
        invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_recomputes_after_ttl_expiry() {
        let _guard = crate::helpers::test_env_lock();
        invalidate_signal_accuracy_cache();
        let base = unique_state_dir("ttl");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        // File holds 7 signals, but we poke the cache with a STALE entry
        // (10 minutes old, total_signals=1). The TTL is 5min so the
        // cached value must be treated as expired and the file re-read.
        write_line_memory(&base, 7);
        let stale_stats = SignalAccuracyStats {
            total_signals: 1,
            correct: 1,
            ..Default::default()
        };
        let stale_at = Instant::now()
            .checked_sub(Duration::from_secs(10 * 60))
            .expect("instant minus 10min");
        set_cache_slot_for_test(stale_at, stale_stats);

        let stats = compute_signal_accuracy().expect("ok");
        assert_eq!(
            stats.total_signals, 7,
            "stale cache (>TTL) must be discarded and recomputed from disk"
        );

        // Cleanup
        invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cache_within_ttl_short_age_is_honored() {
        // Complement to `recomputes_after_ttl_expiry`: a cache slot with
        // an Instant that's WITHIN the TTL window must be honored even if
        // the file would yield a different value. Anchors the boundary.
        let _guard = crate::helpers::test_env_lock();
        invalidate_signal_accuracy_cache();
        let base = unique_state_dir("ttl-fresh");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());

        write_line_memory(&base, 99);
        let cached_stats = SignalAccuracyStats {
            total_signals: 42,
            correct: 42,
            ..Default::default()
        };
        // 1 second old — well within the 5-minute TTL.
        set_cache_slot_for_test(Instant::now(), cached_stats);

        let stats = compute_signal_accuracy().expect("ok");
        assert_eq!(
            stats.total_signals, 42,
            "fresh cache (< TTL) must be returned, NOT re-read from disk"
        );

        // Cleanup
        invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn concurrent_callers_serialize_no_deadlock() {
        let _guard = crate::helpers::test_env_lock();
        invalidate_signal_accuracy_cache();
        let base = unique_state_dir("concurrent");
        std::env::set_var("ALFRED_STATE_DIR", base.as_os_str());
        write_line_memory(&base, 4);

        let start = Instant::now();
        let h1 = std::thread::spawn(|| compute_signal_accuracy());
        let h2 = std::thread::spawn(|| compute_signal_accuracy());
        let r1 = h1.join().expect("thread 1 panic").expect("thread 1 ok");
        let r2 = h2.join().expect("thread 2 panic").expect("thread 2 ok");
        let elapsed = start.elapsed();

        assert_eq!(r1.total_signals, 4);
        assert_eq!(r2.total_signals, 4);
        assert!(
            elapsed < Duration::from_millis(2_000),
            "two concurrent callers must not deadlock; took {:?}",
            elapsed
        );

        // Cleanup
        invalidate_signal_accuracy_cache();
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn default_ttl_is_5_minutes() {
        assert_eq!(SIGNAL_ACCURACY_CACHE_TTL, Duration::from_secs(300));
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
