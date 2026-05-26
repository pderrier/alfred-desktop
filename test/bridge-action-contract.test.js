import test from "node:test";
import assert from "node:assert/strict";
import { createDesktopBridgeClient } from "../src/shared/bridge-client.js";

// ── P2-70 (2026-05-24) — bridge action-name contract test ──────────────
//
// Background: every Rust handler in `src-tauri/src/command_handlers.rs`
// wraps its result in `bridge_envelope(action, result)` (see
// `command_handlers.rs:23`), producing `{ok: true, action, result}`. The
// JS bridge in `src/shared/bridge-client.js` invokes the Tauri command
// and validates the returned envelope against an `acceptedActions`
// whitelist passed to `normalizeTauriPayload`. If the strings don't
// match byte-for-byte, the validator throws `bridge_payload_invalid` and
// the feature silently breaks (no error toast — the home tile or
// quota counter just doesn't render).
//
// This file pins the contract on the methods that shipped in v0.4.1
// with mismatched strings (P0-20 quota counter + P2-24 retrospective
// accuracy) and the brand-new P2-70 macro tile. A regression on any of
// these would re-introduce the silent breakage.
//
// We exercise the public `createDesktopBridgeClient` factory with a
// stub `invoke` so the contract is anchored to the exported API, not
// the internal `normalizeTauriPayload` helper (which isn't exported).

// Helper — build a bridge wired to a stub invoke that returns the given
// envelope verbatim for any command name.
function bridgeReturning(envelope) {
  const invoke = async () => envelope;
  return createDesktopBridgeClient({ invoke, globalObject: { fetch: null } });
}

// Helper — assert that a bridge call throws a `bridge_payload_invalid`
// structured error. Used both for the negative case and as the failure
// mode the production bug surfaced as.
async function assertRejectsPayloadInvalid(promise) {
  await assert.rejects(promise, (err) => {
    assert.equal(err.code, "bridge_payload_invalid");
    return true;
  });
}

// ── runsCountLast7d (P0-20) — broken in v0.4.1 ─────────────────────────

test("bridge.runsCountLast7d accepts the canonical Rust action 'home:runs-count-last-7d-local'", async () => {
  // Matches the action string emitted by `run_runs_count_last_7d` at
  // `command_handlers.rs:1390`. A regression here re-breaks the home
  // tier+quota header strip silently.
  const bridge = bridgeReturning({
    ok: true,
    action: "home:runs-count-last-7d-local",
    result: { count: 2, limit: 3, period: "rolling_7d" },
  });
  const result = await bridge.runsCountLast7d();
  assert.deepEqual(result, { count: 2, limit: 3, period: "rolling_7d" });
});

test("bridge.runsCountLast7d rejects the legacy underscore action (regression guard)", async () => {
  // The pre-fix JS whitelisted `runs_count_last_7d_local`, which Rust
  // never emits. This negative case pins the rejection so the bug can't
  // re-land by accident.
  const bridge = bridgeReturning({
    ok: true,
    action: "runs_count_last_7d_local",
    result: { count: 2, limit: 3, period: "rolling_7d" },
  });
  await assertRejectsPayloadInvalid(bridge.runsCountLast7d());
});

// ── computeSignalAccuracy (P2-24) — broken in v0.4.1 ───────────────────

test("bridge.computeSignalAccuracy accepts the canonical Rust action 'home:signal-accuracy-local'", async () => {
  // Matches the action string emitted by `run_compute_signal_accuracy`
  // at `command_handlers.rs:1409`. A regression here re-breaks the
  // home Section 8 retrospective accuracy tile silently.
  const bridge = bridgeReturning({
    ok: true,
    action: "home:signal-accuracy-local",
    result: {
      total_signals: 12,
      correct: 7,
      incorrect: 5,
      accuracy_pct: 58.3,
      best_pick: null,
      worst_pick: null,
    },
  });
  const result = await bridge.computeSignalAccuracy();
  assert.equal(result.total_signals, 12);
  assert.equal(result.accuracy_pct, 58.3);
});

test("bridge.computeSignalAccuracy rejects the legacy underscore action (regression guard)", async () => {
  const bridge = bridgeReturning({
    ok: true,
    action: "compute_signal_accuracy_local",
    result: { total_signals: 0, correct: 0, incorrect: 0, accuracy_pct: 0 },
  });
  await assertRejectsPayloadInvalid(bridge.computeSignalAccuracy());
});

// ── macroBriefing (P2-70) — new tile, must stay correct ────────────────

test("bridge.macroBriefing accepts the canonical Rust action 'home:macro-briefing-local'", async () => {
  // Matches the action string emitted by `run_macro_briefing` at
  // `command_handlers.rs:1426`. This was already correct when shipped;
  // the assertion locks the contract so the new tile can't drift.
  const bridge = bridgeReturning({
    ok: true,
    action: "home:macro-briefing-local",
    result: {
      ok: true,
      macro: {
        us_10y_yield: { value: 4.55, as_of: "2026-05-24T15:30:00Z" },
        vix: { value: 16.7, as_of: "2026-05-24T15:30:00Z" },
        eur_usd: { value: 1.083, as_of: "2026-05-24T15:30:00Z" },
        brent_usd: { value: 82.15, as_of: "2026-05-24T15:30:00Z" },
      },
      cache_hit: true,
      source: "yahoo+cache",
      as_of: "2026-05-24T15:30:00Z",
    },
  });
  const result = await bridge.macroBriefing();
  assert.equal(result.ok, true);
  assert.equal(result.macro.us_10y_yield.value, 4.55);
});

// ── isAdminUser (P2-70 iter 3) — broken in v0.4.1 ──────────────────────

test("bridge.isAdminUser accepts the canonical Rust action 'admin:check-local'", async () => {
  // Matches the action string emitted by `run_admin_check_local` at
  // `command_handlers.rs:1446`. A regression here re-breaks the admin
  // tab visibility probe silently — Pierre uses this daily.
  const bridge = bridgeReturning({
    ok: true,
    action: "admin:check-local",
    result: { is_admin: true },
  });
  const result = await bridge.isAdminUser();
  assert.deepEqual(result, { is_admin: true });
});

test("bridge.isAdminUser rejects the legacy underscore action (regression guard)", async () => {
  // The pre-fix JS whitelisted `admin_check_local`, which Rust never
  // emits. This negative case pins the rejection so the bug can't
  // re-land by accident.
  const bridge = bridgeReturning({
    ok: true,
    action: "admin_check_local",
    result: { is_admin: true },
  });
  await assertRejectsPayloadInvalid(bridge.isAdminUser());
});

// ── getAdminUsage (P2-70 iter 3) — broken in v0.4.1 ────────────────────

test("bridge.getAdminUsage accepts the canonical Rust action 'admin:usage-local'", async () => {
  // Matches the action string emitted by `run_get_admin_usage` at
  // `command_handlers.rs:1353`. A regression here re-breaks the admin
  // usage envelope rendering silently.
  const bridge = bridgeReturning({
    ok: true,
    action: "admin:usage-local",
    result: { total_runs: 42, distinct_users: 7 },
  });
  const result = await bridge.getAdminUsage();
  assert.deepEqual(result, { total_runs: 42, distinct_users: 7 });
});

test("bridge.getAdminUsage rejects the legacy underscore action (regression guard)", async () => {
  const bridge = bridgeReturning({
    ok: true,
    action: "get_admin_usage_local",
    result: { total_runs: 42, distinct_users: 7 },
  });
  await assertRejectsPayloadInvalid(bridge.getAdminUsage());
});

// ── getAdminVpsStats (P2-70 iter 3) — broken in v0.4.1 ─────────────────

test("bridge.getAdminVpsStats accepts the canonical Rust action 'admin:vps-stats-local'", async () => {
  // Matches the action string emitted by `run_get_admin_vps_stats` at
  // `command_handlers.rs:1359`. A regression here re-breaks the admin
  // VPS stats rendering silently.
  const bridge = bridgeReturning({
    ok: true,
    action: "admin:vps-stats-local",
    result: { cpu_pct: 12.4, mem_pct: 58.1, disk_pct: 33.0 },
  });
  const result = await bridge.getAdminVpsStats();
  assert.equal(result.cpu_pct, 12.4);
  assert.equal(result.mem_pct, 58.1);
});

test("bridge.getAdminVpsStats rejects the legacy underscore action (regression guard)", async () => {
  const bridge = bridgeReturning({
    ok: true,
    action: "get_admin_vps_stats_local",
    result: { cpu_pct: 12.4, mem_pct: 58.1, disk_pct: 33.0 },
  });
  await assertRejectsPayloadInvalid(bridge.getAdminVpsStats());
});

// ── deleteRun (P1-82) ──────────────────────────────────────────────────

test("bridge.deleteRun accepts the canonical Rust action 'run:delete-local' and unwraps the summary", async () => {
  // Matches the action string emitted by `run_delete_run` in
  // command_handlers.rs. app.js reads the unwrapped summary directly
  // (signal_entries_purged / tickers_affected). A regression on the
  // action string silently breaks the delete toast + counts.
  const bridge = bridgeReturning({
    ok: true,
    action: "run:delete-local",
    result: { deleted: true, banned: true, signal_entries_purged: 3, tickers_affected: 2 },
  });
  const result = await bridge.deleteRun("run_bad");
  assert.deepEqual(result, {
    deleted: true,
    banned: true,
    signal_entries_purged: 3,
    tickers_affected: 2,
  });
});

test("bridge.deleteRun rejects an empty run id without invoking", async () => {
  let invoked = false;
  const bridge = createDesktopBridgeClient({
    invoke: async () => { invoked = true; return {}; },
    globalObject: { fetch: null },
  });
  await assert.rejects(bridge.deleteRun("   "), (err) => {
    assert.equal(err.code, "run_id_required");
    return true;
  });
  assert.equal(invoked, false, "must not hit the backend with a blank run id");
});

// ── Validator-contract negative case ───────────────────────────────────

test("normalizeTauriPayload rejects a deliberate-mismatch action (validator stays strict)", async () => {
  // Confirms the validator's contract isn't permissive: a typo'd or
  // wrong action string must throw, never silently pass through. If
  // this test ever passes by accepting the wrong action, the
  // whitelist mechanism is broken and every bridge method becomes
  // vulnerable to silent breakage.
  const bridge = bridgeReturning({
    ok: true,
    action: "home:wrong-name",
    result: { count: 0, limit: 3, period: "rolling_7d" },
  });
  await assertRejectsPayloadInvalid(bridge.runsCountLast7d());
});
