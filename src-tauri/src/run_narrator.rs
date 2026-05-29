//! Run-narration LLM toast — periodic, short LLM-generated summaries of in-flight
//! SSE events. Spawned at run start, stopped at run end. Each tick (10 s):
//!   1. Drain new SSE events since previous tick.
//!   2. If non-empty, build a compact prompt (~120 tokens target).
//!   3. Call `llm_backend::run_prompt` (8 s timeout, parity contract honored).
//!   4. On success: `emit_event("alfred://run-narration", { run_id, message })`.
//!   5. On failure: log + increment `consecutive_failures`. Three failures
//!      degrade the narrator for the rest of the run (no further LLM calls).
//!
//! Kill-switch: setting `run_narration_enabled` (default 1). When 0, every
//! tick short-circuits without an LLM call.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use serde_json::{json, Value};

const RING_CAPACITY: usize = 200;
const TICK_INTERVAL: Duration = Duration::from_secs(10);
/// First-tick LLM timeout — initial narration calls are heavier (model warmup
/// + no continuity context to anchor on), so we give them 15s. P0-2 fix:
/// the previous flat 8s timeout caused systematic timeouts on the first 1-2
/// ticks of every run, which then accumulated `consecutive_failures` and
/// silently degraded the narrator for the rest of the run.
const LLM_TIMEOUT_FIRST_MS: u64 = 15_000;
/// Subsequent-tick LLM timeout — after at least one successful narration the
/// model has warmed up and the prompt carries continuity context, so 10s is
/// enough. Capped strictly below 15s so subsequent and first-tick budgets
/// stay distinguishable (and below the 10s tick interval would oversubscribe).
const LLM_TIMEOUT_SUBSEQUENT_MS: u64 = 10_000;
/// Number of consecutive LLM failures before the narrator gives up for the
/// rest of the run. P0-2 fix: raised from 3 to 10 so a brief outage at run
/// start (3 timeouts in a row was trivially easy with the old 8s budget)
/// can't permanently silence the narrator.
const FAILURE_DEGRADE_THRESHOLD: u32 = 10;
const MAX_EVENTS_IN_PROMPT: usize = 20;

/// One recorded SSE event with the moment it landed.
#[derive(Clone, Debug)]
pub struct RecordedEvent {
    /// Captured at insertion time. Kept for future windowing/diagnostics and
    /// to anchor recorded events to wall-clock when we eventually surface
    /// per-event timing in the prompt.
    #[allow(dead_code)]
    pub ts: SystemTime,
    pub kind: String,
    pub payload: Value,
}

/// Aggregate progress carried across ticks so the prompt can surface
/// "X/Y positions analysées, dernière: TTE" — the same data points the
/// alfred-analysis-progress template trigger uses. Updated on every
/// `line_done` event.
#[derive(Clone, Debug)]
pub(crate) struct ProgressSnapshot {
    pub completed: usize,
    pub total: usize,
    pub latest_ticker: String,
}

/// Per-run narrator state.
struct NarratorState {
    buffer: VecDeque<RecordedEvent>,
    last_drain_at: Instant,
    consecutive_failures: u32,
    degraded: bool,
    /// Most recent narration produced for this run. Injected into the next
    /// prompt so the LLM can build on it rather than starting from zero each
    /// tick (narrative continuity).
    last_narration: Option<String>,
    /// Latest aggregated progress (completed/total + last ticker done).
    /// Updated from each `line_done` event and surfaced in the prompt even
    /// when the current tick's drained events don't include a fresh
    /// line_done. Mirrors the data the JS template trigger uses.
    latest_progress: Option<ProgressSnapshot>,
    stop_flag: Arc<AtomicBool>,
}

impl NarratorState {
    fn new(stop_flag: Arc<AtomicBool>) -> Self {
        Self {
            buffer: VecDeque::with_capacity(RING_CAPACITY),
            last_drain_at: Instant::now(),
            consecutive_failures: 0,
            degraded: false,
            last_narration: None,
            latest_progress: None,
            stop_flag,
        }
    }
}

static RUN_NARRATORS: OnceLock<Mutex<HashMap<String, NarratorState>>> = OnceLock::new();

fn narrators() -> &'static Mutex<HashMap<String, NarratorState>> {
    RUN_NARRATORS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Push an SSE event into this run's ring buffer. No-op if the narrator is
/// not running for this run_id (e.g. kill-switch disabled).
pub fn record_event(run_id: &str, event: &Value) {
    let kind = event
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if kind.is_empty() {
        return;
    }
    // Only the 4 event kinds the narrator can actually describe.
    if !matches!(
        kind.as_str(),
        "line_progress" | "line_done" | "synthesis_progress" | "stage"
    ) {
        return;
    }

    let Ok(mut guard) = narrators().lock() else { return };
    let Some(state) = guard.get_mut(run_id) else { return };

    // On line_done, refresh the aggregate progress snapshot so the prompt
    // can mention "X/Y analysées, dernière: TTE" even when the current
    // tick drains only line_progress events.
    if kind == "line_done" {
        let ticker = event
            .get("ticker")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let completed = event
            .get("completed")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let total = event
            .get("total")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        if let (Some(completed), Some(total)) = (completed, total) {
            if total > 0 && !ticker.is_empty() {
                state.latest_progress = Some(ProgressSnapshot {
                    completed,
                    total,
                    latest_ticker: ticker,
                });
            }
        }
    }

    let recorded = RecordedEvent {
        ts: SystemTime::now(),
        kind,
        payload: event.clone(),
    };
    if state.buffer.len() >= RING_CAPACITY {
        state.buffer.pop_front();
    }
    state.buffer.push_back(recorded);
}

/// Drain events that landed since the previous drain, updating `last_drain_at`.
fn drain_since_last_tick(run_id: &str) -> Vec<RecordedEvent> {
    let Ok(mut guard) = narrators().lock() else {
        return Vec::new();
    };
    let Some(state) = guard.get_mut(run_id) else {
        return Vec::new();
    };
    let now = Instant::now();
    // The buffer is a sliding window of the latest events; the simplest
    // "since last tick" is "everything currently in the buffer", since we
    // clear it after draining.
    let drained: Vec<RecordedEvent> = state.buffer.drain(..).collect();
    state.last_drain_at = now;
    drained
}

/// Start the narrator for `run_id`. Idempotent — calling twice is a no-op.
/// Honors the `run_narration_enabled` kill-switch (no thread spawned when 0).
pub fn start(run_id: &str) {
    if !is_enabled() {
        crate::debug_log("run_narrator: disabled via setting, not starting");
        return;
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut guard) = narrators().lock() else { return };
        if guard.contains_key(run_id) {
            return;
        }
        guard.insert(run_id.to_string(), NarratorState::new(Arc::clone(&stop_flag)));
    }

    let run_id_thread = run_id.to_string();
    thread::spawn(move || {
        run_loop(&run_id_thread, stop_flag, llm_call_real);
    });
}

/// Stop the narrator for `run_id`. Idempotent — calling twice or on a run
/// that was never started is a no-op.
pub fn stop(run_id: &str) {
    let Ok(mut guard) = narrators().lock() else { return };
    if let Some(state) = guard.remove(run_id) {
        state.stop_flag.store(true, Ordering::Relaxed);
    }
}

/// LLM call function — extracted so tests can inject a fake. Takes the prompt
/// and the timeout-in-milliseconds the narrator picked for this tick (varies
/// between first and subsequent ticks per P0-2 fix). Returns the narration
/// text (non-empty) on success, or an error.
pub type LlmCall = fn(&str, u64) -> Result<String>;

fn llm_call_real(prompt: &str, timeout_ms: u64) -> Result<String> {
    // Route through the narration-specific entry point so codex / native-oauth
    // modes use the dedicated narration app-server slot and never block behind
    // an in-flight line-analysis LLM call (root-cause fix for narration lag).
    let value = crate::llm_backend::run_prompt_narration(prompt, timeout_ms, None)?;
    Ok(extract_text(&value))
}

/// Pull the narration text out of whatever `run_prompt` returned.
/// Matches the existing extraction pattern used elsewhere (cf.
/// `services/native_mcp_analysis.rs`).
fn extract_text(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.trim().to_string();
    }
    let candidate = value
        .get("agent_text")
        .or_else(|| value.get("text"))
        .or_else(|| value.get("message"))
        .and_then(|v| v.as_str());
    if let Some(s) = candidate {
        return s.trim().to_string();
    }
    String::new()
}

fn is_enabled() -> bool {
    // 1 = on (default), 0 = off. Read via runtime_settings.
    crate::runtime_setting_integer_direct("run_narration_enabled", 1) != 0
}

/// Main tick loop. Exits when `stop_flag` is set or the run was removed.
fn run_loop(run_id: &str, stop_flag: Arc<AtomicBool>, llm_call: LlmCall) {
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        // Sleep first so we don't fire immediately at start (there are no
        // events yet) and so a quick stop() does not race a tick.
        thread::sleep(TICK_INTERVAL);
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }
        tick_once(run_id, &llm_call);
    }
}

/// Run a single tick. `pub` for tests so we can drive it without a real
/// thread. Returns true if the LLM was actually called this tick.
pub fn tick_once(run_id: &str, llm_call: &LlmCall) -> bool {
    // Bail early if the narrator was removed (stop) or marked degraded.
    {
        let Ok(guard) = narrators().lock() else { return false };
        match guard.get(run_id) {
            Some(state) if state.degraded => return false,
            None => return false,
            _ => {}
        }
    }

    if !is_enabled() {
        // Drain anyway so the buffer doesn't grow stale; just don't call LLM.
        let _ = drain_since_last_tick(run_id);
        return false;
    }

    let events = drain_since_last_tick(run_id);
    if events.is_empty() {
        return false;
    }

    let (last_narration, progress): (Option<String>, Option<ProgressSnapshot>) = narrators()
        .lock()
        .ok()
        .and_then(|g| {
            g.get(run_id).map(|s| (s.last_narration.clone(), s.latest_progress.clone()))
        })
        .unwrap_or((None, None));

    // P0-2: first tick (no prior narration to anchor on) gets a longer
    // budget. Any tick after the first successful narration uses the
    // shorter budget.
    let timeout_ms = if last_narration.is_none() {
        LLM_TIMEOUT_FIRST_MS
    } else {
        LLM_TIMEOUT_SUBSEQUENT_MS
    };

    let prompt = build_narration_prompt(&events, last_narration.as_deref(), progress.as_ref());
    match llm_call(&prompt, timeout_ms) {
        Ok(text) if !text.trim().is_empty() => {
            let cleaned = text.trim().to_string();
            on_success(run_id, &cleaned);
            crate::emit_event(
                "alfred://run-narration",
                json!({ "run_id": run_id, "message": cleaned }),
            );
            true
        }
        Ok(_) => {
            // Empty text counts as a failure — the model returned nothing usable.
            on_failure(run_id, "empty_response");
            true
        }
        Err(err) => {
            on_failure(run_id, &err.to_string());
            true
        }
    }
}

fn on_success(run_id: &str, narration: &str) {
    if let Ok(mut guard) = narrators().lock() {
        if let Some(state) = guard.get_mut(run_id) {
            state.consecutive_failures = 0;
            state.last_narration = Some(narration.to_string());
        }
    }
}

fn on_failure(run_id: &str, reason: &str) {
    // Track whether THIS failure was the one that flipped the run into
    // degraded mode, so we can emit the status event exactly once outside
    // the narrators() lock (event listeners shouldn't be blocked on it).
    let mut just_degraded = false;
    let mut failure_count: u32 = 0;
    if let Ok(mut guard) = narrators().lock() {
        if let Some(state) = guard.get_mut(run_id) {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            failure_count = state.consecutive_failures;
            if state.consecutive_failures >= FAILURE_DEGRADE_THRESHOLD && !state.degraded {
                state.degraded = true;
                just_degraded = true;
            }
        }
    }
    if just_degraded {
        crate::debug_log(&format!(
            "run_narrator: run {run_id} entering degraded mode after {failure_count} consecutive failures (last reason: {reason})"
        ));
        // P0-2: surface degraded state to the UI so it can show a
        // "narration: mode dégradé" badge instead of silently going dark.
        crate::emit_event(
            "alfred://run-narration-status",
            json!({
                "run_id": run_id,
                "mode": "degraded",
                "consecutive_failures": failure_count,
                "reason": reason,
            }),
        );
    } else {
        crate::debug_log(&format!(
            "run_narrator: run {run_id} LLM failure #{failure_count} ({reason})"
        ));
    }
}

/// Build the compact narration prompt from a slice of recorded events plus
/// the previous narration (for continuity) plus the latest aggregate
/// progress snapshot (so the LLM has the same X/Y counter the template
/// trigger uses). Keeps the most recent `MAX_EVENTS_IN_PROMPT` so the
/// prompt stays bounded even on very busy runs.
pub(crate) fn build_narration_prompt(
    events: &[RecordedEvent],
    last_narration: Option<&str>,
    progress: Option<&ProgressSnapshot>,
) -> String {
    let total = events.len();
    let slice: &[RecordedEvent] = if total > MAX_EVENTS_IN_PROMPT {
        &events[total - MAX_EVENTS_IN_PROMPT..]
    } else {
        events
    };

    let mut body = String::with_capacity(256);
    for ev in slice {
        let line = format_event_line(ev);
        if !line.is_empty() {
            body.push_str("- ");
            body.push_str(&line);
            body.push('\n');
        }
    }

    let previous_block = match last_narration {
        Some(s) if !s.trim().is_empty() => format!(
            "Pr\u{00e9}c\u{00e9}dent r\u{00e9}sum\u{00e9} (continuer le fil, NE PAS r\u{00e9}p\u{00e9}ter) :\n\u{00ab} {} \u{00bb}\n\n",
            s.trim()
        ),
        _ => String::new(),
    };

    let progress_block = match progress {
        Some(p) if p.total > 0 => format!(
            "Progression globale : {}/{} positions analys\u{00e9}es \
             (derni\u{00e8}re finalis\u{00e9}e : {}).\n\n",
            p.completed, p.total, p.latest_ticker
        ),
        _ => String::new(),
    };

    format!(
        "Tu narres en direct l'analyse de portefeuille men\u{00e9}e par Alfred. \
         Alfred est l'agent IA qui analyse les positions. \
         Les codes courts en MAJUSCULES (ex : AAPL, MSFT, NVDA, EXA, TSLA) sont des TICKERS \
         d'ENTREPRISES analys\u{00e9}es par Alfred — jamais des agents qui font une action.\n\n\
         {previous}\
         {progress}\
         \u{00c9}v\u{00e8}nements r\u{00e9}cents de l'analyse :\n{body}\n\
         Produis 1 \u{00e0} 2 phrases courtes (25-45 mots au total, fran\u{00e7}ais, ton calme et pr\u{00e9}cis), \
         qui m\u{00ea}lent :\n\
         (1) la progression c\u{00f4}t\u{00e9} Alfred (ce qui vient de se passer, et si pertinent le compteur X/Y),\n\
         (2) quand c'est pertinent, une br\u{00e8}ve sur une de ces entreprises \
         (secteur, contexte connu, fait notable) en t'appuyant sur ta connaissance g\u{00e9}n\u{00e9}rale.\n\
         Ne fais pas r\u{00e9}p\u{00e9}ter une entreprise comme « agent ». Pas de pr\u{00e9}ambule. \
         R\u{00e9}ponds uniquement avec la ou les phrases.",
        previous = previous_block,
        progress = progress_block,
        body = body
    )
}

fn format_event_line(ev: &RecordedEvent) -> String {
    let p = &ev.payload;
    match ev.kind.as_str() {
        "line_progress" => {
            let ticker = p.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
            let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("analyzing");
            let progress = p.get("progress").and_then(|v| v.as_str()).unwrap_or("");
            if progress.is_empty() {
                format!("{ticker}: {status}")
            } else {
                format!("{ticker}: {status} ({progress})")
            }
        }
        "line_done" => {
            let ticker = p.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
            let signal = p
                .get("recommendation")
                .and_then(|r| r.get("signal").or_else(|| r.get("action")))
                .and_then(|v| v.as_str())
                .unwrap_or("done");
            let conviction = p
                .get("recommendation")
                .and_then(|r| r.get("conviction"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if conviction.is_empty() {
                format!("{ticker}: done \u{2014} {signal}")
            } else {
                format!("{ticker}: done \u{2014} {signal} conviction {conviction}")
            }
        }
        "synthesis_progress" => {
            let progress = p.get("progress").and_then(|v| v.as_str()).unwrap_or("");
            if progress.is_empty() {
                "synthesis: in progress".to_string()
            } else {
                format!("synthesis: {progress}")
            }
        }
        "stage" => {
            let stage = p.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            if stage.is_empty() {
                String::new()
            } else {
                format!("run-stage: {stage}")
            }
        }
        _ => String::new(),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Helper: insert a fresh state for `run_id` directly (bypasses start()
    /// so tests don't spawn real threads or depend on settings).
    fn install_state(run_id: &str) -> Arc<AtomicBool> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut guard = narrators().lock().unwrap();
        guard.insert(run_id.to_string(), NarratorState::new(Arc::clone(&stop_flag)));
        stop_flag
    }

    fn remove_state(run_id: &str) {
        let mut guard = narrators().lock().unwrap();
        guard.remove(run_id);
    }

    fn make_event(kind: &str, payload: Value) -> Value {
        let mut v = payload;
        if let Some(obj) = v.as_object_mut() {
            obj.insert("type".to_string(), Value::String(kind.to_string()));
        }
        v
    }

    #[test]
    fn test_buffer_capacity_eviction() {
        let run_id = "test_capacity";
        install_state(run_id);

        for i in 0..250 {
            let ev = make_event(
                "line_progress",
                json!({"ticker": format!("T{i}"), "status": "analyzing", "progress": ""}),
            );
            record_event(run_id, &ev);
        }

        let guard = narrators().lock().unwrap();
        let state = guard.get(run_id).unwrap();
        assert_eq!(state.buffer.len(), RING_CAPACITY);
        // Oldest tickers should have been evicted.
        let first_ticker = state.buffer.front().and_then(|e| e.payload.get("ticker"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(first_ticker, "T50", "first 50 events should have been dropped");
        drop(guard);
        remove_state(run_id);
    }

    #[test]
    fn test_drain_since_last_tick_empty() {
        let run_id = "test_drain_empty";
        install_state(run_id);

        // No events yet → empty.
        let drained = drain_since_last_tick(run_id);
        assert!(drained.is_empty());

        // Push one → drain returns it.
        record_event(
            run_id,
            &make_event("line_done", json!({"ticker": "AAPL", "recommendation": {"signal": "BUY"}})),
        );
        let drained = drain_since_last_tick(run_id);
        assert_eq!(drained.len(), 1);

        // Second drain → empty again.
        let drained = drain_since_last_tick(run_id);
        assert!(drained.is_empty());

        remove_state(run_id);
    }

    #[test]
    fn test_build_narration_prompt_compact() {
        let events = vec![
            RecordedEvent {
                ts: SystemTime::now(),
                kind: "line_progress".to_string(),
                payload: json!({"ticker": "AAPL", "status": "analyzing", "progress": "validating fundamentals"}),
            },
            RecordedEvent {
                ts: SystemTime::now(),
                kind: "line_done".to_string(),
                payload: json!({"ticker": "MSFT", "recommendation": {"signal": "BUY", "conviction": "high"}}),
            },
            RecordedEvent {
                ts: SystemTime::now(),
                kind: "line_progress".to_string(),
                payload: json!({"ticker": "NVDA", "status": "analyzing", "progress": "fetching news"}),
            },
            RecordedEvent {
                ts: SystemTime::now(),
                kind: "synthesis_progress".to_string(),
                payload: json!({"progress": "drafting outlook"}),
            },
            RecordedEvent {
                ts: SystemTime::now(),
                kind: "stage".to_string(),
                payload: json!({"stage": "line_analysis"}),
            },
        ];
        let prompt = build_narration_prompt(&events, None, None);
        // Prompt is richer than v1 (system framing + insight ask) but still bounded.
        assert!(prompt.len() < 1500, "prompt should be < 1500 chars, got {}", prompt.len());
        for ticker in ["AAPL", "MSFT", "NVDA"] {
            assert!(prompt.contains(ticker), "prompt missing ticker {ticker}");
        }
        assert!(prompt.contains("synthesis"));
        assert!(prompt.contains("line_analysis"));
        // Sanity: the new framing must explicitly disambiguate tickers vs agents
        // (regression guard for the "EXA est en train d'analyser" bug).
        assert!(
            prompt.contains("TICKERS") && prompt.contains("ENTREPRISES"),
            "prompt must clarify that tickers are companies, not agents"
        );
        assert!(prompt.contains("Alfred"));
        // First tick has no previous narration block.
        assert!(!prompt.contains("Pr\u{00e9}c\u{00e9}dent r\u{00e9}sum\u{00e9}"));
    }

    #[test]
    fn test_build_narration_prompt_with_previous_narration() {
        let events = vec![RecordedEvent {
            ts: SystemTime::now(),
            kind: "line_done".to_string(),
            payload: json!({"ticker": "NVDA", "recommendation": {"signal": "BUY", "conviction": "high"}}),
        }];
        let previous = "Alfred valide AAPL en BUY, attaque NVDA dans la foul\u{00e9}e.";
        let prompt = build_narration_prompt(&events, Some(previous), None);
        assert!(prompt.contains("Pr\u{00e9}c\u{00e9}dent r\u{00e9}sum\u{00e9}"));
        assert!(prompt.contains(previous));
        assert!(prompt.contains("NVDA"));
        // Whitespace-only previous narration must be skipped — exercise that path too.
        let prompt_blank = build_narration_prompt(&events, Some("   "), None);
        assert!(!prompt_blank.contains("Pr\u{00e9}c\u{00e9}dent r\u{00e9}sum\u{00e9}"));
    }

    #[test]
    fn test_build_narration_prompt_includes_progress_snapshot() {
        let events = vec![RecordedEvent {
            ts: SystemTime::now(),
            kind: "line_progress".to_string(),
            payload: json!({"ticker": "MSFT", "status": "analyzing"}),
        }];
        let progress = ProgressSnapshot {
            completed: 21,
            total: 33,
            latest_ticker: "TTE".to_string(),
        };
        let prompt = build_narration_prompt(&events, None, Some(&progress));
        assert!(prompt.contains("21/33"));
        assert!(prompt.contains("TTE"));
        assert!(prompt.contains("Progression globale"));

        // Zero-total is treated as no-progress (avoid bogus "0/0 positions analysées").
        let progress_zero = ProgressSnapshot {
            completed: 0,
            total: 0,
            latest_ticker: "X".to_string(),
        };
        let prompt_zero = build_narration_prompt(&events, None, Some(&progress_zero));
        assert!(!prompt_zero.contains("Progression globale"));
    }

    #[test]
    fn test_record_event_updates_progress_snapshot() {
        let run_id = "test_progress_snapshot";
        install_state(run_id);

        // line_progress alone must NOT touch the progress snapshot.
        record_event(
            run_id,
            &make_event("line_progress", json!({"ticker": "AAPL", "status": "analyzing"})),
        );
        {
            let guard = narrators().lock().unwrap();
            assert!(guard.get(run_id).unwrap().latest_progress.is_none());
        }

        // line_done with completed/total/ticker updates the snapshot.
        record_event(
            run_id,
            &make_event(
                "line_done",
                json!({"ticker": "TTE", "completed": 21, "total": 33, "recommendation": {"signal": "HOLD"}}),
            ),
        );
        {
            let guard = narrators().lock().unwrap();
            let p = guard.get(run_id).unwrap().latest_progress.as_ref().unwrap();
            assert_eq!(p.completed, 21);
            assert_eq!(p.total, 33);
            assert_eq!(p.latest_ticker, "TTE");
        }

        // A later line_done overwrites the snapshot.
        record_event(
            run_id,
            &make_event(
                "line_done",
                json!({"ticker": "MSFT", "completed": 22, "total": 33, "recommendation": {"signal": "BUY"}}),
            ),
        );
        {
            let guard = narrators().lock().unwrap();
            let p = guard.get(run_id).unwrap().latest_progress.as_ref().unwrap();
            assert_eq!(p.completed, 22);
            assert_eq!(p.latest_ticker, "MSFT");
        }

        // line_done missing completed/total leaves the existing snapshot intact.
        record_event(
            run_id,
            &make_event(
                "line_done",
                json!({"ticker": "NVDA", "recommendation": {"signal": "HOLD"}}),
            ),
        );
        {
            let guard = narrators().lock().unwrap();
            let p = guard.get(run_id).unwrap().latest_progress.as_ref().unwrap();
            assert_eq!(p.completed, 22);
            assert_eq!(p.latest_ticker, "MSFT");
        }

        remove_state(run_id);
    }

    #[test]
    fn test_start_stop_lifecycle() {
        // Use install_state directly (start() reads settings + spawns a thread
        // we don't want in unit tests). Verify stop() removes the entry and
        // sets stop_flag.
        let run_id = "test_lifecycle";
        let stop_flag = install_state(run_id);
        assert!(narrators().lock().unwrap().contains_key(run_id));
        assert!(!stop_flag.load(Ordering::Relaxed));

        stop(run_id);
        assert!(!narrators().lock().unwrap().contains_key(run_id));
        assert!(stop_flag.load(Ordering::Relaxed));

        // Stop a second time → no-op.
        stop(run_id);
        assert!(!narrators().lock().unwrap().contains_key(run_id));
    }

    #[test]
    fn test_record_event_noop_without_state() {
        // No state installed → record_event is a no-op, no panic.
        let run_id = "test_noop";
        record_event(
            run_id,
            &make_event("line_progress", json!({"ticker": "AAPL", "status": "analyzing"})),
        );
        let guard = narrators().lock().unwrap();
        assert!(!guard.contains_key(run_id));
    }

    #[test]
    fn test_record_event_ignores_unknown_kinds() {
        let run_id = "test_unknown_kind";
        install_state(run_id);
        record_event(run_id, &make_event("garbage", json!({"foo": "bar"})));
        record_event(run_id, &make_event("done", json!({})));
        let guard = narrators().lock().unwrap();
        assert_eq!(guard.get(run_id).unwrap().buffer.len(), 0);
        drop(guard);
        remove_state(run_id);
    }

    #[test]
    fn test_extract_text_variants() {
        assert_eq!(extract_text(&json!("hello")), "hello");
        assert_eq!(extract_text(&json!({"agent_text": "from agent"})), "from agent");
        assert_eq!(extract_text(&json!({"text": "from text"})), "from text");
        assert_eq!(extract_text(&json!({"message": "from message"})), "from message");
        assert_eq!(extract_text(&json!({"unrelated": "x"})), "");
    }

    #[test]
    fn test_tick_skips_when_buffer_empty() {
        let _env = SETTINGS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = "test_tick_empty";
        install_state(run_id);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        CALLS.store(0, Ordering::SeqCst);
        fn fake(_p: &str, _t: u64) -> Result<String> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok("nope".to_string())
        }
        let fired = tick_once(run_id, &(fake as LlmCall));
        assert!(!fired);
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
        remove_state(run_id);
    }

    /// Serializes every test whose outcome depends on `is_enabled()` — i.e.
    /// any test that drives `tick_once`. One test mutates the process-global
    /// `ALFRED_RUN_NARRATION_ENABLED` env var that `is_enabled()` reads; if it
    /// ran concurrently with another `tick_once` test that expects the default
    /// (enabled), the kill-switch would leak and the other test would see the
    /// narrator disabled. Holding this lock across each such test makes the
    /// env-var window exclusive.
    static SETTINGS_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Kill-switch + lazy-start proof: when `run_narration_enabled` is 0,
    /// `tick_once` must short-circuit BEFORE the LLM call — which is the only
    /// path that would lazily spawn the dedicated narration app-server slot.
    /// So a disabled session never pays the extra-process cost. We flip the
    /// switch via its env override (`ALFRED_RUN_NARRATION_ENABLED`).
    #[test]
    fn test_tick_skips_llm_when_kill_switch_disabled() {
        let _env = SETTINGS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let prev = std::env::var("ALFRED_RUN_NARRATION_ENABLED").ok();
        std::env::set_var("ALFRED_RUN_NARRATION_ENABLED", "0");

        // Sanity: the switch is now read as disabled (proves the kill-switch
        // is actually wired — before this fix the setting key was undefined
        // and `is_enabled()` was hardwired to true).
        assert!(!is_enabled(), "kill-switch override should disable narration");

        let run_id = "test_kill_switch";
        install_state(run_id);
        // Buffer has events — the ONLY reason a tick wouldn't call the LLM is
        // the kill-switch (rules out the empty-buffer short-circuit).
        record_event(
            run_id,
            &make_event("line_done", json!({"ticker": "AAPL", "recommendation": {"signal": "BUY"}})),
        );

        static CALLS: AtomicUsize = AtomicUsize::new(0);
        CALLS.store(0, Ordering::SeqCst);
        fn fake(_p: &str, _t: u64) -> Result<String> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok("should not be called".to_string())
        }
        let fired = tick_once(run_id, &(fake as LlmCall));
        assert!(!fired, "disabled narrator must not fire the LLM");
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            0,
            "kill-switch must prevent the LLM call (and thus the narration-slot spawn)"
        );

        // Restore env + cleanup.
        match prev {
            Some(v) => std::env::set_var("ALFRED_RUN_NARRATION_ENABLED", v),
            None => std::env::remove_var("ALFRED_RUN_NARRATION_ENABLED"),
        }
        remove_state(run_id);
    }

    #[test]
    fn test_tick_fires_when_buffer_has_events() {
        let _env = SETTINGS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = "test_tick_fires";
        install_state(run_id);
        record_event(
            run_id,
            &make_event("line_done", json!({"ticker": "AAPL", "recommendation": {"signal": "BUY"}})),
        );
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        CALLS.store(0, Ordering::SeqCst);
        fn fake(_p: &str, _t: u64) -> Result<String> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok("  Alfred vient de valider AAPL en BUY haute conviction.  ".to_string())
        }
        let fired = tick_once(run_id, &(fake as LlmCall));
        assert!(fired);
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        let guard = narrators().lock().unwrap();
        let state = guard.get(run_id).unwrap();
        assert_eq!(state.consecutive_failures, 0);
        // The successful narration is stored (trimmed) for the next tick's continuity.
        assert_eq!(
            state.last_narration.as_deref(),
            Some("Alfred vient de valider AAPL en BUY haute conviction.")
        );
        drop(guard);
        remove_state(run_id);
    }

    #[test]
    fn test_previous_narration_is_passed_to_next_tick() {
        let _env = SETTINGS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = "test_continuity";
        install_state(run_id);

        // First tick — record an event, fake an LLM that returns a narration.
        record_event(
            run_id,
            &make_event("line_done", json!({"ticker": "AAPL", "recommendation": {"signal": "BUY"}})),
        );
        fn fake_first(_p: &str, _t: u64) -> Result<String> {
            Ok("Premi\u{00e8}re narration sur AAPL.".to_string())
        }
        assert!(tick_once(run_id, &(fake_first as LlmCall)));

        // Second tick — record a fresh event, the next LLM call must receive
        // the first narration in its prompt.
        record_event(
            run_id,
            &make_event("line_done", json!({"ticker": "NVDA", "recommendation": {"signal": "HOLD"}})),
        );
        static SEEN_PROMPT: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
        fn fake_second(p: &str, _t: u64) -> Result<String> {
            *SEEN_PROMPT.lock().unwrap() = p.to_string();
            Ok("Deuxi\u{00e8}me narration.".to_string())
        }
        assert!(tick_once(run_id, &(fake_second as LlmCall)));

        let captured = SEEN_PROMPT.lock().unwrap().clone();
        assert!(
            captured.contains("Premi\u{00e8}re narration sur AAPL."),
            "second prompt should carry the first narration; got: {captured}"
        );
        assert!(captured.contains("Pr\u{00e9}c\u{00e9}dent r\u{00e9}sum\u{00e9}"));
        // The second narration should now be the stored state.
        let guard = narrators().lock().unwrap();
        assert_eq!(
            guard.get(run_id).unwrap().last_narration.as_deref(),
            Some("Deuxi\u{00e8}me narration.")
        );
        drop(guard);
        remove_state(run_id);
    }

    #[test]
    fn test_failure_threshold_enters_degraded_at_configured_count() {
        // Renamed from `test_three_failures_enter_degraded` after P0-2 raised
        // the threshold from 3 to 10. The test now asserts the contract in
        // terms of the threshold constant itself, so future bumps don't
        // require rewriting the test.
        let _env = SETTINGS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let run_id = "test_failure_threshold";
        install_state(run_id);
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        CALLS.store(0, Ordering::SeqCst);
        fn always_fail(_p: &str, _t: u64) -> Result<String> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("simulated_llm_timeout"))
        }

        let threshold = FAILURE_DEGRADE_THRESHOLD;
        // Drive past the threshold by one to confirm the post-degrade tick
        // is a no-op (no LLM call).
        for _ in 0..(threshold + 1) {
            record_event(
                run_id,
                &make_event("line_progress", json!({"ticker": "X", "status": "analyzing"})),
            );
            tick_once(run_id, &(always_fail as LlmCall));
        }

        // The narrator should have stopped calling the LLM as soon as it
        // hit the threshold — so total CALLS == threshold (the +1 iteration
        // was a no-op).
        assert_eq!(
            CALLS.load(Ordering::SeqCst) as u32,
            threshold,
            "after {threshold} failures, the narrator must stop calling the LLM"
        );

        let guard = narrators().lock().unwrap();
        let state = guard.get(run_id).unwrap();
        assert!(state.degraded);
        assert_eq!(state.consecutive_failures, threshold);
        drop(guard);
        remove_state(run_id);
    }
}

// ── Test-only helpers (cfg(test)) ────────────────────────────────────────
//
// Exposed at module scope so integration-style tests in tests.rs (outside
// this module's private `tests` submodule) can drive the narrator without
// spawning a real thread or relying on the runtime_settings store.

/// Snapshot of narrator state for assertions in tests outside this module.
#[cfg(test)]
pub struct NarratorSnapshot {
    pub consecutive_failures: u32,
    pub degraded: bool,
}

#[cfg(test)]
pub fn test_install_state(run_id: &str) {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let mut guard = narrators().lock().unwrap();
    guard.insert(run_id.to_string(), NarratorState::new(stop_flag));
}

#[cfg(test)]
pub fn test_remove_state(run_id: &str) {
    let mut guard = narrators().lock().unwrap();
    guard.remove(run_id);
}

#[cfg(test)]
pub fn test_state_snapshot(run_id: &str) -> Option<NarratorSnapshot> {
    let guard = narrators().lock().ok()?;
    let state = guard.get(run_id)?;
    Some(NarratorSnapshot {
        consecutive_failures: state.consecutive_failures,
        degraded: state.degraded,
    })
}
