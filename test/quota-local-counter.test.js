import test from "node:test";
import assert from "node:assert/strict";
import {
  getLocalQuotaState,
  resetLocalQuotaCache,
} from "../src/desktop-shell/quota-local-counter.js";

// ── P0-20: home tier+quota header strip ─────────────────────────────

test("getLocalQuotaState: returns the bridge payload normalised", async () => {
  resetLocalQuotaCache();
  const bridge = {
    runsCountLast7d: async () => ({ count: 2, limit: 3, period: "rolling_7d" }),
  };
  const state = await getLocalQuotaState(bridge);
  assert.deepEqual(state, { count: 2, limit: 3, period: "rolling_7d" });
});

test("getLocalQuotaState: memoises the result for 60s", async () => {
  resetLocalQuotaCache();
  let calls = 0;
  const bridge = {
    runsCountLast7d: async () => {
      calls += 1;
      return { count: 1, limit: 3, period: "rolling_7d" };
    },
  };
  await getLocalQuotaState(bridge);
  await getLocalQuotaState(bridge);
  await getLocalQuotaState(bridge);
  assert.equal(calls, 1, "memoisation must collapse repeated calls within TTL");
});

test("getLocalQuotaState: falls back to count=0 on bridge error", async () => {
  resetLocalQuotaCache();
  const bridge = {
    runsCountLast7d: async () => { throw new Error("tauri_command_missing"); },
  };
  const state = await getLocalQuotaState(bridge);
  assert.equal(state.count, 0);
  assert.equal(state.limit, 3);
  assert.equal(state.period, "rolling_7d");
  assert.match(state.error, /tauri_command_missing/);
});

test("getLocalQuotaState: coerces non-numeric payload safely", async () => {
  resetLocalQuotaCache();
  // Server / bridge contract drift: a future endpoint might return
  // strings instead of numbers — `getLocalQuotaState` must degrade
  // gracefully rather than render NaN.
  const bridge = {
    runsCountLast7d: async () => ({ count: "not-a-number", limit: null, period: null }),
  };
  const state = await getLocalQuotaState(bridge);
  assert.equal(state.count, 0);
  assert.equal(state.limit, 3);
  assert.equal(state.period, "rolling_7d");
});

test("resetLocalQuotaCache: forces a re-fetch on next call", async () => {
  resetLocalQuotaCache();
  let calls = 0;
  const bridge = {
    runsCountLast7d: async () => {
      calls += 1;
      return { count: calls, limit: 3, period: "rolling_7d" };
    },
  };
  await getLocalQuotaState(bridge);
  resetLocalQuotaCache();
  await getLocalQuotaState(bridge);
  assert.equal(calls, 2, "manual reset bypasses the TTL");
});
