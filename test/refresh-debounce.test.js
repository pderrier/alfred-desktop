import test from "node:test";
import assert from "node:assert/strict";
import {
  shouldRunRefresh,
  forceRefreshDebounce,
  resetRefreshDebounce,
} from "../src/desktop-shell/refresh-debounce.js";

// ── P2-62: per-slot debounce for refresh fan-out ────────────────────
//
// The helper holds module-level state (Map of slot→lastRunAt), so each
// test must reset the state before running and use distinct slot
// names to avoid cross-test interference. `resetRefreshDebounce()` is
// the documented test-only escape hatch.

test("shouldRunRefresh: returns true on the first call for a slot", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
});

test("shouldRunRefresh: blocks a second call within the interval", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // 1ms later — well inside the 5s window.
  now += 1;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  // 4999ms later — still inside the window (now+4999 vs original).
  now += 4998;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
});

test("shouldRunRefresh: allows the next call after the interval", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // Fast-forward exactly 5000ms — should fire (>= interval, since the
  // check is `now - last < intervalMs`, so equal elapsed time passes).
  now += 5000;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
});

test("shouldRunRefresh: slots are independent — home_header and signal_accuracy don't share timestamps", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  // Burn the home_header slot.
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // 1ms later — home_header is blocked, but signal_accuracy is still
  // unburned and must fire on its very first call.
  now += 1;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), true);
  // Both are now stamped — next call to either fails inside the window.
  now += 1;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), false);
});

// P2-62 follow-up (2026-05-24) — pin the asymmetric production intervals.
// `signal_accuracy` only changes when a run completes (portfolio-wide
// line-memory aggregate), so it uses a 30s window; `home_header` stays
// at 5s for time-sensitive tier/quota. These tests pin the contract so
// a future regression flipping signal_accuracy back to 5s fails loudly.
test("shouldRunRefresh: signal_accuracy honors a 30s interval (blocked at 29.999s, fires at 30s)", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  // First call burns the slot.
  assert.equal(shouldRunRefresh("signal_accuracy", 30000, nowFn), true);
  // 1ms later — well inside the window.
  now += 1;
  assert.equal(shouldRunRefresh("signal_accuracy", 30000, nowFn), false);
  // 5s later — would have fired under the old 5000ms interval; must
  // stay blocked under the new 30s contract.
  now += 4999; // total elapsed = 5000ms
  assert.equal(shouldRunRefresh("signal_accuracy", 30000, nowFn), false);
  // 29.999s after the initial call — still blocked.
  now += 24999; // total elapsed = 29999ms
  assert.equal(shouldRunRefresh("signal_accuracy", 30000, nowFn), false);
  // 30s after the initial call — must fire.
  now += 1; // total elapsed = 30000ms
  assert.equal(shouldRunRefresh("signal_accuracy", 30000, nowFn), true);
});

test("shouldRunRefresh: home_header keeps the 5s interval (tier/quota are time-sensitive)", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // 4999ms later — blocked.
  now += 4999;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  // 5000ms after the initial call — fires.
  now += 1;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
});

test("forceRefreshDebounce: resets a slot so the next call within the window fires", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // 1s later — would normally be blocked.
  now += 1000;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  // Force-reset, then immediately retry — should fire even though no
  // real time has elapsed.
  forceRefreshDebounce("home_header");
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // And the post-force call rearms the debounce as usual.
  now += 1;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
});

test("forceRefreshDebounce: only affects the named slot", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  // Burn both slots.
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), true);
  now += 1000;
  // Force-reset home_header only.
  forceRefreshDebounce("home_header");
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  // signal_accuracy is still stamped — still blocked.
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), false);
});

test("shouldRunRefresh: default Date.now clock — first call passes, immediate retry blocked", () => {
  // Smoke test the production default. No clock injection: relies on
  // real Date.now monotonicity (any non-zero gap < 5s fails the retry).
  resetRefreshDebounce();
  assert.equal(shouldRunRefresh("smoke_default_clock"), true);
  assert.equal(shouldRunRefresh("smoke_default_clock"), false);
});

test("shouldRunRefresh: rejects empty/non-string slot names", () => {
  resetRefreshDebounce();
  assert.throws(() => shouldRunRefresh("", 5000), /non-empty string/);
  assert.throws(() => shouldRunRefresh(null, 5000), /non-empty string/);
  assert.throws(() => shouldRunRefresh(undefined, 5000), /non-empty string/);
  assert.throws(() => shouldRunRefresh(42, 5000), /non-empty string/);
});

test("forceRefreshDebounce: rejects empty/non-string slot names", () => {
  resetRefreshDebounce();
  assert.throws(() => forceRefreshDebounce(""), /non-empty string/);
  assert.throws(() => forceRefreshDebounce(null), /non-empty string/);
});

test("shouldRunRefresh: zero interval always fires (effectively disabled)", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  // intervalMs=0 means "no debouncing" — `now - last < 0` is never
  // true, so every call returns true.
  assert.equal(shouldRunRefresh("zero_interval", 0, nowFn), true);
  assert.equal(shouldRunRefresh("zero_interval", 0, nowFn), true);
  assert.equal(shouldRunRefresh("zero_interval", 0, nowFn), true);
});

test("resetRefreshDebounce: clears every slot", () => {
  resetRefreshDebounce();
  let now = 1_000_000;
  const nowFn = () => now;
  shouldRunRefresh("home_header", 5000, nowFn);
  shouldRunRefresh("signal_accuracy", 5000, nowFn);
  shouldRunRefresh("foo", 5000, nowFn);
  // All three blocked within the window.
  now += 100;
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), false);
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), false);
  assert.equal(shouldRunRefresh("foo", 5000, nowFn), false);
  // After a global reset, every slot fires again.
  resetRefreshDebounce();
  assert.equal(shouldRunRefresh("home_header", 5000, nowFn), true);
  assert.equal(shouldRunRefresh("signal_accuracy", 5000, nowFn), true);
  assert.equal(shouldRunRefresh("foo", 5000, nowFn), true);
});
