import test from "node:test";
import assert from "node:assert/strict";
import {
  findConcentrationThemeToTrigger,
  shouldNotifyConcentration,
} from "../src/desktop-shell/report-view-model.js";

// ── P3-52: findConcentrationThemeToTrigger ─────────────────────────

test("findConcentrationThemeToTrigger: returns the first row meeting both thresholds", () => {
  const themes = [
    // accountCount < 2 → excluded
    { theme: "tariffs", accountCount: 1, totalCount: 8, accounts: ["PEA"] },
    // totalCount < 4 → excluded
    { theme: "luxury", accountCount: 3, totalCount: 3, accounts: ["PEA", "CTO", "PEA-PME"] },
    // matches → returned
    { theme: "tech", accountCount: 2, totalCount: 6, accounts: ["PEA", "CTO"] },
    // also matches but loses on iteration order (first wins)
    { theme: "macro", accountCount: 3, totalCount: 10, accounts: ["PEA", "CTO", "AV"] },
  ];
  const result = findConcentrationThemeToTrigger(themes);
  assert.equal(result.theme, "tech");
  assert.equal(result.totalCount, 6);
  assert.equal(result.accountCount, 2);
  assert.deepEqual(result.accounts, ["PEA", "CTO"]);
});

test("findConcentrationThemeToTrigger: returns null when no row meets the thresholds", () => {
  const themes = [
    { theme: "tariffs", accountCount: 1, totalCount: 100 }, // accountCount short
    { theme: "luxury", accountCount: 5, totalCount: 3 },    // totalCount short
  ];
  assert.equal(findConcentrationThemeToTrigger(themes), null);
});

test("findConcentrationThemeToTrigger: null on null/non-array input", () => {
  assert.equal(findConcentrationThemeToTrigger(null), null);
  assert.equal(findConcentrationThemeToTrigger(undefined), null);
  assert.equal(findConcentrationThemeToTrigger({}), null);
  assert.equal(findConcentrationThemeToTrigger("not an array"), null);
});

test("findConcentrationThemeToTrigger: null on empty array", () => {
  assert.equal(findConcentrationThemeToTrigger([]), null);
});

test("findConcentrationThemeToTrigger: skips non-object entries gracefully", () => {
  const themes = [
    null,
    "string entry",
    42,
    { theme: "real_one", accountCount: 2, totalCount: 5, accounts: ["A", "B"] },
  ];
  const result = findConcentrationThemeToTrigger(themes);
  assert.equal(result.theme, "real_one");
});

test("findConcentrationThemeToTrigger: coerces numeric strings safely", () => {
  // accountCount/totalCount might arrive as strings from a stale
  // payload. Number() coerces, so the helper still works.
  const themes = [
    { theme: "stringed", accountCount: "2", totalCount: "4", accounts: ["A", "B"] },
  ];
  const result = findConcentrationThemeToTrigger(themes);
  assert.equal(result.theme, "stringed");
});

test("findConcentrationThemeToTrigger: thresholds at the exact boundary", () => {
  // accountCount = 2 AND totalCount = 4 — both at the boundary, must pass.
  const themes = [{ theme: "edge", accountCount: 2, totalCount: 4, accounts: ["A", "B"] }];
  assert.ok(findConcentrationThemeToTrigger(themes));
});

test("findConcentrationThemeToTrigger: accounts defaults to [] when missing", () => {
  const themes = [{ theme: "no_accounts", accountCount: 2, totalCount: 5 }];
  const result = findConcentrationThemeToTrigger(themes);
  assert.deepEqual(result.accounts, []);
});

// ── P3-64: shouldNotifyConcentration ───────────────────────────────
// Pure dedup decision helper. The orchestrator (`app.js`) holds the
// `lastNotifiedSnapshotKey` slot and rotates it after firing ; this
// helper just answers "should I fire now ?".
//
// Pin semantics : a new run rotates `computeHomeSnapshotKey` (which
// includes `runs.length`), so the dedup key changes and the helper
// returns `true` — that's the intended "new run = new context"
// behavior. Same-snapshot welcome re-renders MUST be suppressed.

test("shouldNotifyConcentration: returns false when no concentrated theme", () => {
  // Nothing to notify about — must always suppress regardless of keys.
  assert.equal(shouldNotifyConcentration("key-a", null, null), false);
  assert.equal(shouldNotifyConcentration("key-a", "key-a", null), false);
  assert.equal(shouldNotifyConcentration("key-a", "key-b", null), false);
  assert.equal(shouldNotifyConcentration("key-a", null, undefined), false);
});

test("shouldNotifyConcentration: returns true on first call with a concentrated theme", () => {
  // First call : lastNotifiedSnapshotKey is null (initial state).
  const theme = { theme: "tech", totalCount: 6, accountCount: 2, accounts: ["PEA", "CTO"] };
  assert.equal(shouldNotifyConcentration("snap-1", null, theme), true);
});

test("shouldNotifyConcentration: returns false on second call with same snapshot key (re-render suppression)", () => {
  // Welcome view re-renders without a new run → keys are identical →
  // must suppress to avoid spamming the overlay.
  const theme = { theme: "tech", totalCount: 6, accountCount: 2, accounts: ["PEA", "CTO"] };
  assert.equal(shouldNotifyConcentration("snap-1", "snap-1", theme), false);
});

test("shouldNotifyConcentration: returns true when snapshot key changes (new run completes)", () => {
  // A new completed run rotates `computeHomeSnapshotKey` via
  // `runs.length` and updated_at — the helper MUST fire again.
  // This is the explicit pin that closes the QA caveat for P3-52.
  const theme = { theme: "tech", totalCount: 6, accountCount: 2, accounts: ["PEA", "CTO"] };
  assert.equal(shouldNotifyConcentration("snap-2", "snap-1", theme), true);
});

test("shouldNotifyConcentration: returns true when last key was null and current is non-null (first notify)", () => {
  // Same as the first-call case but explicit about the null→value
  // transition — defends against a future refactor that initializes
  // the slot to `""` instead of `null`.
  const theme = { theme: "tech", totalCount: 6, accountCount: 2, accounts: ["PEA", "CTO"] };
  assert.equal(shouldNotifyConcentration("snap-1", null, theme), true);
});

test("shouldNotifyConcentration: returns true when current key is empty/null but theme exists (degenerate but firable)", () => {
  // Degenerate case : snapshot key is empty (shouldn't happen in prod
  // because `computeHomeSnapshotKey` always returns a non-empty
  // pipe-joined string, but defensive). Helper fires once and the
  // caller still writes back the empty key — subsequent calls hit the
  // suppression branch via the `currentSnapshotKey && ...` guard
  // (falsy current short-circuits to "fire").
  const theme = { theme: "tech", totalCount: 6, accountCount: 2, accounts: ["PEA", "CTO"] };
  assert.equal(shouldNotifyConcentration("", null, theme), true);
  assert.equal(shouldNotifyConcentration(null, null, theme), true);
});
