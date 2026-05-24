// P2-62 (2026-05-24) — debounce helper for per-function refresh slots.
//
// During a live run, SSE events drive frequent dashboard refreshes
// (`refreshDashboardInner()`). Each refresh fans out into smaller
// per-feature refresh functions (header strip, signal accuracy, …)
// that hit Tauri commands. Without coalescing, those Tauri commands
// thrash. The outer `refreshInFlight` guard only serialises the parent
// refresh — it does not throttle the inner fan-out.
//
// This module provides a minimal slot-based "ran within last N ms?"
// guard. It is intentionally a module singleton so callers in any
// file can share slot identity by string name. A separate module also
// keeps `app.js` free of mutable Map state and makes the guard unit
// testable without dragging in DOM/Tauri stubs.
//
// Contract:
//   - `shouldRunRefresh(name, intervalMs, nowFn?)` returns `true` and
//     updates the slot if the previous run was ≥ `intervalMs` ago (or
//     never). Returns `false` otherwise — the caller should bail out.
//   - `forceRefreshDebounce(name)` resets a single slot. Use for
//     user-initiated events (e.g. license activation) that must bypass
//     the next debounce window.
//   - `resetRefreshDebounce()` clears all slots. Test-only escape hatch.
//
// The `nowFn` parameter exists for tests; production callers should
// rely on the `Date.now` default.

const refreshDebounceMs = new Map(); // name → lastRefreshedAt (ms epoch)

/**
 * Return `true` and stamp the slot if the named refresh should run.
 *
 * The first call for a given `name` always returns `true` because the
 * Map starts empty (so the recorded last-run is treated as 0).
 *
 * @param {string} name - Slot identifier. Distinct names are independent.
 * @param {number} [intervalMs=5000] - Minimum gap between successful
 *   calls for the same slot. Must be ≥ 0.
 * @param {() => number} [nowFn=Date.now] - Clock source. Injectable for
 *   tests; production must use the default.
 * @returns {boolean} Whether the caller should proceed with the refresh.
 */
export function shouldRunRefresh(name, intervalMs = 5000, nowFn = Date.now) {
  if (typeof name !== "string" || name.length === 0) {
    throw new TypeError("shouldRunRefresh: name must be a non-empty string");
  }
  const now = nowFn();
  const last = refreshDebounceMs.get(name) || 0;
  if (now - last < intervalMs) return false;
  refreshDebounceMs.set(name, now);
  return true;
}

/**
 * Reset a single slot so the next `shouldRunRefresh(name, …)` call
 * fires regardless of the elapsed interval. Used by user-initiated
 * events (license activation, manual refresh) that must surface fresh
 * data immediately.
 *
 * @param {string} name - Slot identifier.
 */
export function forceRefreshDebounce(name) {
  if (typeof name !== "string" || name.length === 0) {
    throw new TypeError("forceRefreshDebounce: name must be a non-empty string");
  }
  refreshDebounceMs.delete(name);
}

/**
 * Clear every debounce slot. Tests only.
 */
export function resetRefreshDebounce() {
  refreshDebounceMs.clear();
}
