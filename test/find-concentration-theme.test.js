import test from "node:test";
import assert from "node:assert/strict";
import { findConcentrationThemeToTrigger } from "../src/desktop-shell/report-view-model.js";

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
