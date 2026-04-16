/**
 * Tests for the __none__ sentinel value used in cash_account_links.
 *
 * When a user explicitly confirms an investment account has NO associated
 * cash account, the wizard stores "__none__" as the link value. The guards
 * in app-wizard.js and app-alfred-triggers.js use `!savedLinks[a.name]`
 * to check if a group still needs resolution. The sentinel must be truthy
 * so these guards treat it as "covered".
 *
 * Source of truth:
 *   - apps/alfred-desktop/src/desktop-shell/app-wizard.js line 548
 *   - apps/alfred-desktop/src/desktop-shell/app-alfred-triggers.js line 373
 */
import test from "node:test";
import assert from "node:assert/strict";

const SENTINEL = "__none__";

test("__none__ sentinel is truthy in JS", () => {
  assert.ok(SENTINEL, "__none__ must be truthy");
  assert.ok(!!SENTINEL, "double-bang __none__ must be true");
});

test("__none__ sentinel passes the !savedLinks[name] guard", () => {
  const savedLinks = { "PEA Bourso": "__none__" };
  // The guard: !savedLinks[a.name] — should be false when sentinel is set
  const isCovered = !!savedLinks["PEA Bourso"];
  assert.ok(isCovered, "sentinel-linked account must be treated as covered");
  // Uncovered account
  const isUncovered = !savedLinks["Unknown Account"];
  assert.ok(isUncovered, "missing account must be treated as uncovered");
});

test("__none__ sentinel: every() guard with mixed covered/uncovered", () => {
  const savedLinks = {
    "PEA Bourso": "Compte espece PEA",
    "CTO": "__none__"
  };
  const allInvestmentNames = ["PEA Bourso", "CTO"];
  const allCovered = allInvestmentNames.every((name) => savedLinks[name]);
  assert.ok(allCovered, "all names must be covered when mix of real links and sentinels");
});

test("__none__ sentinel: every() fails when one name has no entry", () => {
  const savedLinks = {
    "PEA Bourso": "Compte espece PEA"
  };
  const allInvestmentNames = ["PEA Bourso", "CTO"];
  const allCovered = allInvestmentNames.every((name) => savedLinks[name]);
  assert.ok(!allCovered, "every() must fail when CTO has no entry");
});

test("__none__ sentinel is a non-empty string", () => {
  assert.notEqual(SENTINEL, "");
  assert.notEqual(SENTINEL, null);
  assert.notEqual(SENTINEL, undefined);
  assert.equal(typeof SENTINEL, "string");
});
