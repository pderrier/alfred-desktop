//! Predicate that decides whether a previously-analysed position must be
//! re-evaluated in `refresh_synthesis` mode (P0-58 — 2026-05-23).
//!
//! Historically the only signal forcing re-analysis was the LLM-set
//! `reanalyse_after` date on a prior recommendation. That signal misses three
//! situations that should also invalidate the cached recommendation:
//!
//! 1. **Cold ticker** — the position has never produced a signal history
//!    (e.g. import just happened, or every prior run failed for this line).
//! 2. **Price drift** — the price has moved ≥ 5 % (absolute) since the last
//!    signal was recorded, which usually obsoletes the prior reasoning.
//! 3. **Material fresh news** — news flagged `severity = "material"` was
//!    published after the line memory was last touched.
//!
//! Priority order (first match wins): `reanalyse_after` → cold → drift → news.
//!
//! The function is **pure** (no I/O, no globals, no time source other than
//! the supplied `today`). All callers thread their inputs through; this keeps
//! the predicate trivially testable.

use serde_json::Value;

/// Absolute threshold (percent points) above which a price move since the
/// last signal forces a fresh analysis.
pub(crate) const PRICE_DRIFT_PCT_THRESHOLD: f64 = 5.0;

/// Why a position must be re-analysed (or `None` to keep the cached reco).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForceReanalyseReason {
    /// Cached reco is still valid — skip re-analysis.
    None,
    /// The LLM-set `reanalyse_after` date is today or in the past.
    ReanalyseAfterExpired,
    /// No prior signal recorded — first analysis or every prior run failed.
    SignalHistoryEmpty,
    /// `|return_since_signal_pct| >= PRICE_DRIFT_PCT_THRESHOLD`.
    PriceDriftSinceSignal,
    /// News article tagged `severity == "material"` dated after the
    /// line-memory `updated_at` timestamp.
    MaterialNewsFresh,
}

impl ForceReanalyseReason {
    /// True for every variant except `None`.
    pub(crate) fn is_force(&self) -> bool {
        !matches!(self, ForceReanalyseReason::None)
    }

    /// Short tag for diagnostic logs / run-state metadata.
    pub(crate) fn as_tag(&self) -> &'static str {
        match self {
            ForceReanalyseReason::None => "none",
            ForceReanalyseReason::ReanalyseAfterExpired => "reanalyse_after_expired",
            ForceReanalyseReason::SignalHistoryEmpty => "signal_history_empty",
            ForceReanalyseReason::PriceDriftSinceSignal => "price_drift_since_signal",
            ForceReanalyseReason::MaterialNewsFresh => "material_news_fresh",
        }
    }
}

/// Decide whether the cached recommendation `rec` must be re-evaluated today.
///
/// All inputs are optional / defensive — a missing field never panics, it
/// simply causes that branch to be skipped. `today` is expected as an
/// ISO-8601 prefix (`YYYY-MM-DD`); callers should pass `now_iso_string()`
/// truncated to 10 chars.
///
/// Priority (first match returned):
///   1. `rec.reanalyse_after` non-empty AND ≤ `today`     → `ReanalyseAfterExpired`
///   2. `line_memory.signal_history` missing or empty     → `SignalHistoryEmpty`
///   3. `|line_memory.price_tracking.return_since_signal_pct| >= 5.0`
///                                                        → `PriceDriftSinceSignal`
///   4. Any `news_row.articles[]` with `severity == "material"` and
///      `date > line_memory.updated_at`                   → `MaterialNewsFresh`
///   5. otherwise                                         → `None`
pub(crate) fn should_force_reanalyse(
    rec: &Value,
    _market_row: Option<&Value>,
    line_memory: Option<&Value>,
    news_row: Option<&Value>,
    today: &str,
) -> ForceReanalyseReason {
    // 1. reanalyse_after expired (existing v0.3.x behaviour).
    if let Some(date) = rec.get("reanalyse_after").and_then(|v| v.as_str()) {
        let date = date.trim();
        if !date.is_empty() && date <= today {
            return ForceReanalyseReason::ReanalyseAfterExpired;
        }
    }

    // 2. Cold ticker — no usable signal history.
    let history_len = line_memory
        .and_then(|m| m.get("signal_history"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if history_len == 0 {
        return ForceReanalyseReason::SignalHistoryEmpty;
    }

    // 3. Price drift above threshold (absolute value).
    if let Some(ret) = line_memory
        .and_then(|m| m.get("price_tracking"))
        .and_then(|pt| pt.get("return_since_signal_pct"))
        .and_then(|v| v.as_f64())
    {
        if ret.abs() >= PRICE_DRIFT_PCT_THRESHOLD {
            return ForceReanalyseReason::PriceDriftSinceSignal;
        }
    }

    // 4. Material news fresher than the line memory.
    let memory_updated_at = line_memory
        .and_then(|m| m.get("updated_at"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if let Some(articles) = news_row
        .and_then(|n| n.get("articles"))
        .and_then(|v| v.as_array())
    {
        let has_material = articles.iter().any(|article| {
            let severity = article
                .get("severity")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            if !severity.eq_ignore_ascii_case("material") {
                return false;
            }
            let date = article
                .get("date")
                .or_else(|| article.get("published_at"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            // Defensive: with no memory timestamp, treat any material article
            // as fresh. With no article date, do NOT trigger — we can't prove
            // it is newer than what the LLM already saw.
            if date.is_empty() {
                false
            } else if memory_updated_at.is_empty() {
                true
            } else {
                date > memory_updated_at
            }
        });
        if has_material {
            return ForceReanalyseReason::MaterialNewsFresh;
        }
    }

    ForceReanalyseReason::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Build a line-memory entry shaped like the real V2 store.
    fn line_memory(
        signal_history: Value,
        return_pct: Option<f64>,
        updated_at: &str,
    ) -> Value {
        let mut pt = serde_json::Map::new();
        if let Some(p) = return_pct {
            pt.insert("return_since_signal_pct".into(), json!(p));
        }
        json!({
            "schema_version": 2,
            "signal_history": signal_history,
            "price_tracking": Value::Object(pt),
            "updated_at": updated_at,
        })
    }

    /// Build a non-cold history (one prior signal).
    fn warm_history() -> Value {
        json!([{ "date": "2026-05-10", "signal": "CONSERVER" }])
    }

    #[test]
    fn force_reanalyse_returns_none_when_all_signals_quiet() {
        // reanalyse_after in the future, warm history, 1% drift, no news.
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-06-10" });
        let mem = line_memory(warm_history(), Some(1.0), "2026-05-22T10:00:00Z");
        let news = json!({ "articles": [] });
        let reason =
            should_force_reanalyse(&rec, None, Some(&mem), Some(&news), "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::None);
        assert!(!reason.is_force());
    }

    #[test]
    fn force_reanalyse_returns_expired_when_date_passed() {
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-05-22" });
        let mem = line_memory(warm_history(), Some(1.0), "2026-05-22T10:00:00Z");
        let reason = should_force_reanalyse(&rec, None, Some(&mem), None, "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::ReanalyseAfterExpired);
        assert!(reason.is_force());
    }

    #[test]
    fn force_reanalyse_returns_signal_history_empty_when_cold() {
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-06-10" });
        // Empty array — explicitly cold.
        let mem = line_memory(json!([]), Some(1.0), "2026-05-22T10:00:00Z");
        let reason = should_force_reanalyse(&rec, None, Some(&mem), None, "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::SignalHistoryEmpty);
    }

    #[test]
    fn force_reanalyse_returns_price_drift_when_above_threshold() {
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-06-10" });
        let mem = line_memory(warm_history(), Some(6.0), "2026-05-22T10:00:00Z");
        let reason = should_force_reanalyse(&rec, None, Some(&mem), None, "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::PriceDriftSinceSignal);

        // Symmetric: negative drift also triggers.
        let mem_neg = line_memory(warm_history(), Some(-5.0), "2026-05-22T10:00:00Z");
        let reason_neg =
            should_force_reanalyse(&rec, None, Some(&mem_neg), None, "2026-05-23");
        assert_eq!(reason_neg, ForceReanalyseReason::PriceDriftSinceSignal);

        // Just below threshold stays None.
        let mem_under = line_memory(warm_history(), Some(4.9), "2026-05-22T10:00:00Z");
        let reason_under =
            should_force_reanalyse(&rec, None, Some(&mem_under), None, "2026-05-23");
        assert_eq!(reason_under, ForceReanalyseReason::None);
    }

    #[test]
    fn force_reanalyse_returns_material_news_when_fresh() {
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-06-10" });
        let mem = line_memory(warm_history(), Some(1.0), "2026-05-20T10:00:00Z");
        let news = json!({
            "articles": [
                { "title": "minor",    "severity": "low",      "date": "2026-05-22" },
                { "title": "headline", "severity": "material", "date": "2026-05-22" },
            ]
        });
        let reason =
            should_force_reanalyse(&rec, None, Some(&mem), Some(&news), "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::MaterialNewsFresh);

        // Older material news must NOT trigger.
        let stale_news = json!({
            "articles": [
                { "title": "old", "severity": "material", "date": "2026-05-19" },
            ]
        });
        let reason_stale =
            should_force_reanalyse(&rec, None, Some(&mem), Some(&stale_news), "2026-05-23");
        assert_eq!(reason_stale, ForceReanalyseReason::None);

        // Article missing a date does NOT trigger — defensive.
        let undated = json!({
            "articles": [
                { "title": "no date", "severity": "material" },
            ]
        });
        let reason_undated =
            should_force_reanalyse(&rec, None, Some(&mem), Some(&undated), "2026-05-23");
        assert_eq!(reason_undated, ForceReanalyseReason::None);
    }

    #[test]
    fn force_reanalyse_priority_order_date_wins() {
        // All four triggers active. Date must win (priority 1).
        let rec = json!({ "ticker": "AAPL", "reanalyse_after": "2026-05-22" });
        let mem = line_memory(json!([]), Some(10.0), "2026-05-20T10:00:00Z");
        let news = json!({
            "articles": [
                { "severity": "material", "date": "2026-05-22" },
            ]
        });
        let reason =
            should_force_reanalyse(&rec, None, Some(&mem), Some(&news), "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::ReanalyseAfterExpired);

        // Drop the date — cold (priority 2) wins over drift + news.
        let rec_no_date = json!({ "ticker": "AAPL" });
        let reason_cold = should_force_reanalyse(
            &rec_no_date,
            None,
            Some(&mem),
            Some(&news),
            "2026-05-23",
        );
        assert_eq!(reason_cold, ForceReanalyseReason::SignalHistoryEmpty);

        // Drop cold — drift (priority 3) wins over news.
        let mem_warm = line_memory(warm_history(), Some(10.0), "2026-05-20T10:00:00Z");
        let reason_drift = should_force_reanalyse(
            &rec_no_date,
            None,
            Some(&mem_warm),
            Some(&news),
            "2026-05-23",
        );
        assert_eq!(reason_drift, ForceReanalyseReason::PriceDriftSinceSignal);
    }

    #[test]
    fn force_reanalyse_handles_missing_inputs_gracefully() {
        // Missing market_row, missing line_memory, missing news_row.
        // Empty/missing signal_history collapses to cold (priority 2).
        let rec = json!({ "ticker": "AAPL" });
        let reason = should_force_reanalyse(&rec, None, None, None, "2026-05-23");
        assert_eq!(reason, ForceReanalyseReason::SignalHistoryEmpty);

        // Same rec but with empty `reanalyse_after` — still falls through to cold.
        let rec_empty_date = json!({ "ticker": "AAPL", "reanalyse_after": "" });
        let reason_empty =
            should_force_reanalyse(&rec_empty_date, None, None, None, "2026-05-23");
        assert_eq!(reason_empty, ForceReanalyseReason::SignalHistoryEmpty);

        // Future date wins over the missing memory.
        let rec_future = json!({ "ticker": "AAPL", "reanalyse_after": "2026-06-10" });
        let reason_future =
            should_force_reanalyse(&rec_future, None, None, None, "2026-05-23");
        // No memory -> cold (priority 2) beats absent priority-1 trigger.
        assert_eq!(reason_future, ForceReanalyseReason::SignalHistoryEmpty);
    }

    #[test]
    fn as_tag_covers_every_variant() {
        // Pin the tag set: any future variant must update the match arm.
        assert_eq!(ForceReanalyseReason::None.as_tag(), "none");
        assert_eq!(
            ForceReanalyseReason::ReanalyseAfterExpired.as_tag(),
            "reanalyse_after_expired"
        );
        assert_eq!(
            ForceReanalyseReason::SignalHistoryEmpty.as_tag(),
            "signal_history_empty"
        );
        assert_eq!(
            ForceReanalyseReason::PriceDriftSinceSignal.as_tag(),
            "price_drift_since_signal"
        );
        assert_eq!(
            ForceReanalyseReason::MaterialNewsFresh.as_tag(),
            "material_news_fresh"
        );
    }
}
