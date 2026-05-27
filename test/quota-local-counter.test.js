import test from "node:test";
import assert from "node:assert/strict";
import {
  getLocalQuotaState,
  resetLocalQuotaCache,
} from "../src/desktop-shell/quota-local-counter.js";

// ── P0-20 / P3-31: home tier+quota header strip ─────────────────────
//
// v0.4.7 P3-31 flipped the PRIMARY source from the local run-index
// count (`runsCountLast7d`) to the authoritative server probe
// (`quotaStatus` → GET /quota/status). The local count is now the
// FALLBACK used only when the server probe fails.

test("getLocalQuotaState: primary source is the server quotaStatus probe", async () => {
  resetLocalQuotaCache();
  let usedFallback = false;
  const bridge = {
    quotaStatus: async () => ({ count: 2, limit: 3, period: "rolling_7d", reset_at: 1_700_000_000 }),
    runsCountLast7d: async () => { usedFallback = true; return { count: 99, limit: 3, period: "rolling_7d" }; },
  };
  const state = await getLocalQuotaState(bridge);
  assert.deepEqual(state, { count: 2, limit: 3, period: "rolling_7d", reset_at: 1_700_000_000 });
  assert.equal(usedFallback, false, "fallback must NOT be hit when the server probe succeeds");
});

test("getLocalQuotaState: falls back to local count when server probe fails", async () => {
  resetLocalQuotaCache();
  const bridge = {
    quotaStatus: async () => { throw new Error("alfred_api_request_failed:timeout"); },
    runsCountLast7d: async () => ({ count: 1, limit: 3, period: "rolling_7d" }),
  };
  const state = await getLocalQuotaState(bridge);
  // Fallback path has no reset_at — must surface null, not undefined/NaN.
  assert.deepEqual(state, { count: 1, limit: 3, period: "rolling_7d", reset_at: null });
});

test("getLocalQuotaState: memoises the result for 60s (server probe only once)", async () => {
  resetLocalQuotaCache();
  let calls = 0;
  const bridge = {
    quotaStatus: async () => { calls += 1; return { count: 1, limit: 3, period: "rolling_7d", reset_at: null }; },
    runsCountLast7d: async () => ({ count: 0, limit: 3, period: "rolling_7d" }),
  };
  await getLocalQuotaState(bridge);
  await getLocalQuotaState(bridge);
  await getLocalQuotaState(bridge);
  assert.equal(calls, 1, "memoisation must collapse repeated calls within TTL");
});

test("getLocalQuotaState: surfaces error and does NOT cache when both sources fail", async () => {
  resetLocalQuotaCache();
  let primaryCalls = 0;
  const bridge = {
    quotaStatus: async () => { primaryCalls += 1; throw new Error("server_down"); },
    runsCountLast7d: async () => { throw new Error("tauri_command_missing"); },
  };
  const state = await getLocalQuotaState(bridge);
  assert.equal(state.count, 0);
  assert.equal(state.limit, 3);
  assert.equal(state.period, "rolling_7d");
  assert.equal(state.reset_at, null);
  // The fallback error message wins (it's the last attempted source).
  assert.match(state.error, /tauri_command_missing/);
  // The inert value must NOT be cached — next render retries the server.
  await getLocalQuotaState(bridge);
  assert.equal(primaryCalls, 2, "total failure must not be cached; next call retries the server");
});

test("getLocalQuotaState: coerces non-numeric server payload safely", async () => {
  resetLocalQuotaCache();
  // Contract drift: a future server might return strings. The helper must
  // degrade gracefully rather than render NaN.
  const bridge = {
    quotaStatus: async () => ({ count: "not-a-number", limit: null, period: null, reset_at: "soon" }),
    runsCountLast7d: async () => ({ count: 0, limit: 3, period: "rolling_7d" }),
  };
  const state = await getLocalQuotaState(bridge);
  assert.equal(state.count, 0);
  assert.equal(state.limit, 3);
  assert.equal(state.period, "rolling_7d");
  assert.equal(state.reset_at, null, "non-numeric reset_at must coerce to null");
});

test("getLocalQuotaState: paid 'unlimited' limit string coerces to numeric default", async () => {
  resetLocalQuotaCache();
  // Paid tier reports the "unlimited" sentinel. The strip never shows the
  // limit for paid (it branches on licenseStatus().tier), but the value
  // must still coerce to a sane number, not NaN.
  const bridge = {
    quotaStatus: async () => ({ count: 0, limit: "unlimited", period: "rolling_7d", reset_at: null }),
    runsCountLast7d: async () => ({ count: 0, limit: 3, period: "rolling_7d" }),
  };
  const state = await getLocalQuotaState(bridge);
  assert.equal(state.limit, 3);
  assert.equal(state.count, 0);
});

test("resetLocalQuotaCache: forces a re-fetch on next call", async () => {
  resetLocalQuotaCache();
  let calls = 0;
  const bridge = {
    quotaStatus: async () => { calls += 1; return { count: calls, limit: 3, period: "rolling_7d", reset_at: null }; },
    runsCountLast7d: async () => ({ count: 0, limit: 3, period: "rolling_7d" }),
  };
  await getLocalQuotaState(bridge);
  resetLocalQuotaCache();
  await getLocalQuotaState(bridge);
  assert.equal(calls, 2, "manual reset bypasses the TTL");
});
