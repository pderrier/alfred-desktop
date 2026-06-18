# Native-mode parity bugs (post 2026-06-09 native-oauth switch)

Branch: worktree-agent-abb7678680188a1e0

## BUG #1 — synthese globale absente ("Partial latest-run artifact") [CONFIRMED, ROOT CAUSE FOUND]

Empirical: runs 019edc1c0aeb + 019edc06b126 stuck on disk at
orchestration.status=running / stage=llm_generating / composed_payload empty,
while latest report HAS the synthesis. 2/4 recent runs stuck (timing race).

Root cause (NOT exactly the brief's hypothesis):
`tool_finalize_report` (mcp_server.rs:1530) order is:
  1553 flush_now(run_id)         -> writes STALE cached `llm_generating` to disk
  1557 persist_retry_global_synthesis -> reads disk, writes `completed` to disk
  1560 evict(run_id)             -> evict()=flush_now()+remove: flush_now writes
                                   the STALL cached `llm_generating` OVER the
                                   freshly written `completed`, THEN drops entry.
The cache still holds the pre-synthesis snapshot (set at run_synthesis_turn:3307
set_native_run_stage("llm_generating")), nothing re-synced it from the completed
disk write. So evict's flush clobbers disk back to `running`.

Why codex works: codex finalize_report runs in a SEPARATE MCP process (own cache);
the main-process cache is re-synced from the completed disk by merge_mcp_results
(native_mcp_analysis.rs:3422 patch->load-from-disk) BEFORE the main-process
evict at 3431. Native path skips merge_mcp_results and finalizes in-process, so
its cache is never re-synced -> stale clobber.

Fix (shared chokepoint, parity-safe): in tool_finalize_report, EVICT BEFORE
persist (mirrors the test mock at tests.rs:191-198 and codex separate-process
semantics). evict flushes pending recs to disk + DROPS the stale entry, persist
then reads a synced disk and writes `completed` authoritatively with nothing left
to clobber. Remove the redundant pre-persist flush_now and post-persist evict.

Test: parity test — drive a native finalize and assert the on-disk run-state
status==completed AND composed_payload.synthese_marche non-empty (run-state on
disk == report). This is the class the parity contract must pin.

## BUG #2 — narrator progress stuck at 1/35 in native [CONFIRMED + FIXED]

Empirical (real runs 019edc1c0aeb, 019edc06b126 progress sidecars): `completed`
climbs 1..6 within the first batch then RESETS to 1 and stays at 1 for every
later batch. NO ticker ever has >1 line_done (no real re-analysis).

Root cause: `tool_validate_recommendation` (mcp_server.rs) derived `completed`
by counting `"recommendation"` lines in `{run_id}_mcp_results.jsonl`. But
`merge_mcp_results` RENAMES that sidecar away after every batch, so the count
resets to ~1 each batch in native/native-oauth (multi-batch). The narrator's
ProgressSnapshot (run_narrator.rs:128-150) only updates from line_done
completed/total -> it saw 1/35 forever.

Fix (DRY, source of truth): derive `completed` from `pending_recommandations`
(disk + sidecar overlay in load_run_state, deduped by line_id) via a new pure
helper `count_covered_lines` — the same set the coverage gate counts. Cumulative
across batches, dedups reprise re-analysis.

Tests: count_covered_lines_is_cumulative_and_dedups_by_line_id (pure) +
native_line_done_completed_does_not_reset_across_batch_merge (e2e, verified
FAILS at completed=1 vs 3 against the old sidecar-count).

## BUG #3 — re-analysis loop + negative narrator tone [CONFIRMED: loop is an
artifact of #2; prompt hardened]

1. Re-analysis loop: NOT REAL. The real progress sidecars show each ticker
   analyzed exactly once (0 tickers with >1 line_done). The "en boucle sur
   UBI/MC" was a narrator hallucination: with `completed` stuck at 1 (BUG #2)
   and fresh line_done events for UBI/MC/TTE all reporting "1/35", the LLM
   inferred Alfred was re-analyzing position #1 in a loop. Fixing #2 removes
   the cause. No real retry loop to fix. (Per-line MAX_RETRIES=2 and the
   coverage reprise cap MAX_COVERAGE_RETRIES=2 are bounded and correct.)
2. Narrator tone: hardened build_narration_prompt (run_narrator.rs) with an
   explicit FACTUAL guard — never infer "bloqué"/"en boucle"/"panne" from
   latency, a stable counter, or repeated events; only mention a slowdown on an
   explicit `repairing`/`retry`/error event. Test:
   narration_prompt_forbids_inventing_blockage_from_latency.
   The "1/35" template text itself lives in JS (app-alfred-triggers.js:460,
   "Analyzed X/Y positions") and is benign — it now shows the correct count.
