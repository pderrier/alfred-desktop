# QA Review -- v0.2.1

**Reviewer:** Automated QA Agent  
**Date:** 2026-04-13  
**Scope:** All changes in apps/alfred-desktop since v0.2.0  
**Focus areas:** Stale alerts (Phase 1b), Theme concentration (Phase 2b), Signal scorecard (Phase 3b), Run diff (Phase 4a), Alfred overlay (Phase A/B), Chat wizard + save-to-memory, Line memory V2 schema

---

## 1. Critical Issues (Must fix before shipping)

### C-1. Race condition: concurrent writes to line-memory.json (Rust)

**Files:** `native_mcp_analysis.rs` (lines 356, 517, 642, 708), `command_handlers.rs` (lines 280, 328, 417), `mcp_server.rs` (line 145)

`line-memory.json` is read-modify-written by multiple functions with no shared lock:
- `sync_line_memory()` -- called during parallel line analysis (one per ticker, across threads)
- `update_line_memory_fields()` -- called from the UI "Save to Memory" panel
- `compute_theme_concentration()` -- read-only but during an active write window
- `run_get_stale_positions()`, `run_get_signal_scorecard()`, `run_get_run_diff()` -- all read the same file

Although `storage::write_json_file` uses a per-file `.lock` file for the final rename step, the read-modify-write cycle is not atomic. Two concurrent `sync_line_memory` calls (thread A reads file, thread B reads same file, A writes, B writes) will cause B to overwrite A's changes. During a parallel analysis run with 20+ tickers, this **will** happen.

**Impact:** Silent data loss -- some ticker entries in line-memory.json will be overwritten, losing signal history, key reasoning, and price tracking data.

**Fix:** Wrap the entire read-modify-write of `line-memory.json` in a dedicated Mutex (similar to `run_state_update_lock`) or route all line-memory writes through the `run_state_cache` pattern.

---

### C-2. Potential panic: `unwrap()` on mutable cache entry (Rust)

**File:** `native_mcp_analysis.rs`, line 53

```rust
let pending = state.as_object_mut().unwrap()
    .entry("pending_recommandations")
    .or_insert_with(|| json!([]));
```

If the cached `state` value is not a JSON object (e.g. corrupted data, or a JSON array), this `unwrap()` will panic inside the `run_state_cache` Mutex lock, poisoning the Mutex and causing all subsequent cache operations to fail with `PoisonError`.

A second instance exists at line 384 in the same file:
```rust
store["by_ticker"].as_object_mut().unwrap()
```
This is reachable if the `by_ticker` field exists but is not an object.

**Impact:** Application crash or permanent Mutex poisoning requiring an app restart.

**Fix:** Replace `unwrap()` with `unwrap_or_else` or match, and return early on malformed data.

---

### C-3. Potential panic: `unwrap()` in run_state_cache.rs patch function

**File:** `run_state_cache.rs`, line 112

```rust
let entry = guard.entries.get_mut(run_id).unwrap();
```

After the `if !guard.entries.contains_key(run_id)` check loads from disk, there is a theoretical interleave where the entry could still be missing (e.g. if the `insert` on line 110 fails silently, or if a `HashMap` internal error occurs). While unlikely, this is inside a Mutex-protected path where panics poison the lock.

**Impact:** Mutex poisoning, rendering the entire in-memory cache unusable.

**Fix:** Use `guard.entries.get_mut(run_id).ok_or_else(|| anyhow!("cache_entry_missing"))?` instead.

---

## 2. Warnings (Should fix soon, not blocking)

### W-1. String slicing without UTF-8 boundary check

**File:** `command_handlers.rs`, line 302

```rust
let date_part = &reanalyse_after[..10];
```

The code checks `reanalyse_after.len() < 10` (byte length) before slicing, but if the string contains multi-byte UTF-8 characters in the first 10 bytes, `[..10]` will index by bytes and may slice in the middle of a character, causing a panic.

ISO date strings ("2026-04-13") are ASCII-only, so this is safe for well-formed data. However, if an LLM produces a date like "2026\u200B04-13" (with a zero-width space), the slice will panic.

**Fix:** Use `reanalyse_after.chars().take(10).collect::<String>()` or validate that the string is ASCII before slicing.

### W-2. Signal scorecard uses first history entry as "most recent" but history ordering is not validated

**File:** `command_handlers.rs`, lines 350-410

`run_get_signal_scorecard()` assumes `history[0]` is the most recent signal and `history[1]` is the previous one. The signal history is prepended in `sync_line_memory()` so newest-first is the expected order. However, if a user manually edits `line-memory.json` or a migration reorders entries, the scorecard will produce incorrect accuracy calculations with no error or warning.

**Fix:** Sort the history by date descending before calculating, or validate that `history[0].date >= history[1].date`.

### W-3. Run diff counts conviction changes as signal upgrades/downgrades

**File:** `command_handlers.rs`, lines 462-466

When `sig_changed` is false but `conv_changed` is true, the diff entry is created but `signal_changes` is not incremented. This is correct. However, when `sig_changed` is true and `buy_strength(curr) == buy_strength(prev)`, the code still increments `downgrades` (the `else` branch on line 465). This means a signal change between two signals of equal strength (which shouldn't happen given the current signal set, but could with future signals) would be counted as a downgrade.

**Fix:** Add an explicit equality check: `if buy_strength(curr) > buy_strength(prev) { upgrades += 1; } else if buy_strength(curr) < buy_strength(prev) { downgrades += 1; }`

### W-4. Alfred overlay priority comparison may allow lower-priority triggers to preempt

**File:** `app-alfred-overlay.js`, line 358

```javascript
if (panelVisible && currentPriority >= def.priority && currentTriggerId !== triggerId) return;
```

The `>=` comparison means a trigger of **equal** priority to the currently showing trigger will be blocked. However, the async path (line 376) rechecks with the same `>=` condition after the await, but between markFired and the recheck, the panel state may have changed (dismissed, new trigger shown). The suppression model is sound but the edge case of two equal-priority triggers firing within milliseconds is not deterministic.

**Impact:** Low -- priorities in the current trigger catalog are distinct (1, 3, 5, 8).

### W-5. Chat wizard `onDone` callback and Promise resolution interaction

**File:** `app-chat-wizard.js`, lines 126-136

The "Done" button handler calls `cleanup()`, sets `resolved = true`, calls `onDone(history)`, then `resolve(null)`. The `onDone` callback is async (it calls `synthesizeChatForMemoryWithUI`), but the code does not await it. The Promise resolves with `null` immediately while `onDone` is still running.

In `app.js` (lines 429-447), the caller has:
```javascript
await openChatWizard({ ... onDone: async (history) => { ... } });
if (!doneHandled) { showSaveToMemoryPanel(rec, null); }
```

Since `onDone` sets `doneHandled = true` inside the callback closure, but the `await openChatWizard` resolves before `onDone` completes, `doneHandled` will still be `false` when the `if` check runs. This means `showSaveToMemoryPanel` is called twice -- once from `onDone` and once from the fallback check.

**Impact:** The user sees two overlapping "Save to Memory" panels.

**Fix:** Either (a) await `onDone` before resolving the Promise, or (b) set `doneHandled` synchronously before the async work in `onDone`, or (c) remove the fallback `if (!doneHandled)` check when `onDone` is provided.

### W-6. `line-memory.json` not protected by `storage::write_json_file` in all writers

**File:** `native_mcp_analysis.rs`, line 517

`sync_line_memory` uses `crate::storage::write_json_file(&pb, &store)` which acquires a file lock. But the file lock only serializes the final write; it does not protect the entire read-modify-write cycle. The `update_line_memory_fields` function on line 708 also uses `write_json_file`, so both share the same lock file. However, the time window between read (line 361) and write (line 517) can be significant (the function does string processing, array manipulation, etc.).

This overlaps with C-1 but is noted separately because the fix requires architectural change, not just adding a lock.

### W-7. z-index stacking order inconsistency

**Files:** `styles.css`, `app-chat-wizard.js`, `app-line-modal.js`

Current z-index map:
- Splash screen: 100-101
- Sidebar: 50
- Gear panel: 60
- Pipeline bar: 70
- Status popover: 80
- Toast container: 9999
- Modal overlay (CSS): 10000
- Chat wizard (JS inline): 10000
- Alfred overlay panel: 9000
- Save to Memory panel (JS inline): 10001
- Synthesis loading overlay (JS inline): 10001

The Save to Memory panel and the synthesis loading overlay both use `z-index: 10001` (set in JS). If both appear simultaneously (unlikely but possible with fast clicking), they will overlap non-deterministically. More importantly, the toast container (9999) is below the modal overlay (10000), which means toasts are invisible behind modal dialogs.

**Fix:** Establish a documented z-index scale: content < overlay panel < modals < toasts.

### W-8. No input sanitization for external URL open command

**File:** `command_handlers.rs`, lines 189-234

The `validate_external_url` function checks for `http://` or `https://` prefixes, which prevents `file://` and other protocol handlers. However, URLs like `https://evil.com/foo%0d%0abar` with encoded newlines are passed directly to `xdg-open` / `open` / `cmd.exe`. On some systems, this could lead to command injection via URL parameters.

**Impact:** Low on macOS/Linux (`open`/`xdg-open` handle URLs safely), but higher risk on Windows where `cmd /C start` may interpret special characters.

**Fix:** URL-encode or reject URLs with control characters before passing to the shell.

---

## 3. Observations (Code quality notes)

### O-1. Clean architecture and SOLID separation

The extraction of `app-wizard.js`, `app-chat-wizard.js`, `app-line-modal.js`, `app-alfred-overlay.js`, `app-alfred-triggers.js`, `app-alfred-idle.js`, `shell-positions.js`, and `report-view-model.js` from the monolithic `app.js` is well-done. Each module has a clear single responsibility with explicit dependency injection. The overlay bus pattern (trigger registration, suppression engine, priority preemption) is a good architectural choice.

### O-2. Defensive JS coding is consistently applied

The `report-view-model.js` uses `asText()`, `asNumber()`, and `?.` consistently. The `resolveDataSource` function properly handles all four states (composed, pending, artifact, empty). The `normalizeLineMemory` function handles both V1 and V2 schemas gracefully.

### O-3. Rust Tauri command wrappers follow a consistent pattern

All Tauri commands use `spawn_blocking + .map_err(e.to_string())` consistently. The error contract is uniform. The `command_handlers.rs` layer properly validates inputs (e.g., `run_id.trim().to_string()`, empty checks).

### O-4. The `storage.rs` file-locking mechanism is well-designed

The write-to-temp-then-rename pattern with retry logic and cross-platform lock file handling is robust for single-writer scenarios. The `with_file_lock` + `replace_file_with_retry` combination handles Windows file-locking quirks correctly.

### O-5. `run_state_cache.rs` background flush design is effective

The 2-second flush interval with dirty-marking is a good balance between durability and I/O performance. The `flush_now` for run completion and `evict` for cleanup are correct. The `unwrap_or_else(|p| p.into_inner())` pattern for Mutex poisoning recovery is intentional and documented.

### O-6. `escapeHtml` usage in innerHTML is consistent

All user-controlled strings rendered via `innerHTML` are wrapped in `escapeHtml()` across `shell-positions.js`, `app-line-modal.js`, and `app.js`. The exception is the `account-mismatch-modal` in `app-wizard.js` (line 253-258) which uses `${selectedAccount}` and `${csvAccounts[0]}` inside innerHTML without escaping. These values come from Tauri backend responses (not raw user input), but should still be escaped for defense-in-depth.

### O-7. Idle timer implementation is clean but re-fires continuously

`app-alfred-idle.js` re-schedules the timer after every fire (line 32: `scheduleTimeout()`), which means the idle callback fires repeatedly every `timeoutMs` during extended inactivity. Since it calls `alfredOverlay.notify("idle", {})` and no trigger is registered with `autoFireOn: "idle"` yet, this is harmless but will become an issue when idle triggers are enabled (Phase C).

### O-8. The `run_get_run_diff` function only examines tickers with 2+ history entries

Tickers with exactly one signal history entry are silently skipped (`h.len() >= 2`). New positions added in the current run will not appear in the diff even though they are genuinely new. This is arguably correct behavior (no "previous" to diff against), but users may expect to see new additions highlighted.

### O-9. The `NON_ACTIONABLE` set is duplicated between Rust and JS

`report.rs` defines `ACTIONABLE_SIGNALS` for Rust-side filtering, while `app.js` and `report-view-model.js` define `NON_ACTIONABLE` for JS-side filtering. The two sets are complementary but not formally verified to be consistent. A signal like "SURVEILLER" would be non-actionable in JS but not explicitly in the Rust set.

---

## 4. Recommended Test Cases (Prioritized)

### Priority 1: Data integrity

1. **Concurrent line-memory writes** -- Start a 20-position analysis and simultaneously trigger "Save to Memory" from the UI for one of the tickers. Verify no signal_history entries are lost.

2. **line-memory.json with malformed data** -- Place a `line-memory.json` with `"by_ticker": "not_an_object"` and call `get_stale_positions_local`, `get_signal_scorecard_local`, `get_run_diff_local`. Verify no panic, graceful empty response.

3. **Signal scorecard with 1 signal** -- Create a ticker with exactly one signal_history entry. Verify scorecard returns `scored_count: 0` (since 0 return_pct for self-reference) or `scored_count: 1` with correct accuracy.

4. **Run diff with no previous run** -- Complete a first-ever analysis and verify `run_diff` returns `has_previous: false`.

5. **Round-trip: save-to-memory and read-back** -- Save key_reasoning + user_note + news_themes via `update_line_memory_local`, then verify the values appear in the next analysis's line memory context and in the scorecard UI.

### Priority 2: UI interaction

6. **Chat wizard "Done" button double-panel** -- Open a position chat, have a 2-turn conversation, click "Done -- save insights". Verify exactly one Save to Memory panel appears (not two).

7. **Alfred overlay priority preemption** -- Fire `alfred-welcome` (priority 1), then immediately fire `alfred-error-analysis-failed` (priority 8). Verify the error trigger preempts the welcome.

8. **Alfred overlay async contextBuilder** -- Trigger `alfred-run-completed` with both `get_stale_positions_local` and `get_run_diff_local` failing. Verify the panel shows with a fallback message (no unhandled rejection).

9. **Theme concentration card insertion** -- Complete a run where 3+ tickers share a badge keyword. Verify the theme concentration card appears between the synthesis card and the actions section.

10. **Run diff "What Changed" rendering** -- Complete two consecutive runs where at least one ticker changes signal from CONSERVER to ACHAT. Verify the diff shows the upgrade arrow and correct direction.

### Priority 3: Edge cases

11. **Stale positions with future reanalyse_after dates** -- Set all `reanalyse_after` dates to tomorrow. Verify `stale_count: 0`.

12. **Signal scorecard accuracy with zero price_at_signal** -- Create a signal_history entry with `price_at_signal: 0`. Verify no division-by-zero, return_pct is 0.

13. **Cash mapping wizard cancellation** -- Cancel the cash matching wizard during pre-run check. Verify the analysis still proceeds with a warning toast.

14. **External URL with encoded characters** -- Call `desktop_open_external_url` with a URL containing `%0d%0a` or non-ASCII characters. Verify no command injection or panic.

15. **Save to Memory with empty fields** -- Open the Save to Memory panel, leave all fields empty, click Save. Verify the panel closes without making a Tauri call (current behavior: returns early).

### Priority 4: Regression

16. **Dashboard rendering after run completion** -- Complete a run and verify all three rendering paths produce consistent output: (a) `buildReportViewModel` with composed_payload, (b) `buildReportViewModel` with pending_recommandations, (c) `buildReportViewModel` with artifact report.

17. **Position table not wiped during live run** -- Start a run, verify bootstrapped positions appear, verify they are not wiped by a concurrent dashboard refresh.

18. **Settings save/reset cycle** -- Change LLM backend to "native", save, verify OpenAI fields appear. Reset settings, verify backend reverts to default.

19. **Idle timer cleanup** -- Navigate away from the app, wait 5+ minutes, return. Verify no accumulated timeouts or excessive `notify("idle")` calls.

20. **Multi-account run isolation** -- Run analysis for Account A, then switch to Account B and run analysis. Verify Account A's positions/actions are not shown during Account B's run.

---

## Summary

| Category | Count |
|----------|-------|
| Critical | 3 |
| Warnings | 8 |
| Observations | 9 |
| Test cases | 20 |

The most significant risk is **C-1 (concurrent line-memory writes)**, which will cause silent data loss during multi-ticker parallel analysis. This should be addressed before shipping to users who run analyses on portfolios with 10+ positions.

The **W-5 (double Save to Memory panel)** is the most user-visible bug and should be a quick fix (set `doneHandled = true` synchronously at the top of the `onDone` callback).

The codebase quality is high overall. The architecture is clean, the error handling is consistent, and the defensive coding patterns are well-applied.
