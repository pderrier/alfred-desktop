/**
 * Tests for the watchlist confirm modal — Watchlist Curation v2 (D2/D3/D4).
 *
 * The modal lives in
 * `apps/alfred-desktop/src/desktop-shell/app-watchlist-confirm-modal.js` but
 * imports browser-only APIs via an absolute `/desktop-shell/...` path that does
 * not resolve under the Node test runner (same constraint as
 * `csv-confirm-modal.test.js`). We therefore replicate its PURE logic here and
 * exercise:
 *   - parseAddedTickers: free-text → deduped upper-cased {ticker} objects,
 *   - buildConfirmPayload: the exact RPC argument shape for
 *     `watchlist_confirm_local`,
 *   - collectChecklist (pure decomposition): only checked candidates survive,
 *     full item metadata is preserved.
 *
 * Source of truth: app-watchlist-confirm-modal.js — parseAddedTickers /
 * buildConfirmPayload / collectChecklist. The replicated functions MUST stay
 * byte-identical to the originals; any divergence is a test bug.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated pure logic from app-watchlist-confirm-modal.js ───────

function parseAddedTickers(raw) {
  const seen = new Set();
  const out = [];
  for (const tok of String(raw || "").split(/[\s,;]+/)) {
    const t = tok.trim().toUpperCase();
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push({ ticker: t });
  }
  return out;
}

function buildConfirmPayload({ runId, account, checklist, added, feedback }) {
  return {
    run_id: String(runId || ""),
    account: String(account || ""),
    confirmed_items: Array.isArray(checklist) ? checklist : [],
    added_tickers: Array.isArray(added) ? added : [],
    feedback: typeof feedback === "string" ? feedback : "",
  };
}

/**
 * Pure decomposition of collectChecklist: given the original candidate list and
 * the set of tickers the user left checked, return the full item objects for
 * the checked rows (falling back to `{ticker}` when metadata is missing). The
 * DOM-side version reads checkbox state + the stashed candidate index; this
 * pure form skips the DOM layer.
 */
function collectCheckedItems(candidates, checkedTickers) {
  const index = new Map();
  for (const item of candidates) {
    const t = String(item?.ticker || "").toUpperCase();
    if (t) index.set(t, item);
  }
  const out = [];
  for (const ticker of checkedTickers) {
    const t = String(ticker || "").toUpperCase();
    if (!t) continue;
    out.push(index.get(t) || { ticker: t });
  }
  return out;
}

// ── parseAddedTickers ───────────────────────────────────────────────

test("parseAddedTickers splits on comma, space, semicolon and upper-cases", () => {
  assert.deepEqual(parseAddedTickers("aapl, mc.pa  air;ttef"), [
    { ticker: "AAPL" },
    { ticker: "MC.PA" },
    { ticker: "AIR" },
    { ticker: "TTEF" },
  ]);
});

test("parseAddedTickers dedupes case-insensitively and drops empties", () => {
  assert.deepEqual(parseAddedTickers("aapl, AAPL ,, aapl"), [{ ticker: "AAPL" }]);
});

test("parseAddedTickers returns [] for empty/blank/nullish input", () => {
  assert.deepEqual(parseAddedTickers(""), []);
  assert.deepEqual(parseAddedTickers("   "), []);
  assert.deepEqual(parseAddedTickers(null), []);
  assert.deepEqual(parseAddedTickers(undefined), []);
});

// ── buildConfirmPayload ─────────────────────────────────────────────

test("buildConfirmPayload assembles the exact RPC argument shape", () => {
  const payload = buildConfirmPayload({
    runId: "run-123",
    account: "PEA",
    checklist: [{ ticker: "MC", nom: "LVMH", isin: "FR0000121014" }],
    added: [{ ticker: "AAPL" }],
    feedback: "ETF only",
  });
  assert.deepEqual(payload, {
    run_id: "run-123",
    account: "PEA",
    confirmed_items: [{ ticker: "MC", nom: "LVMH", isin: "FR0000121014" }],
    added_tickers: [{ ticker: "AAPL" }],
    feedback: "ETF only",
  });
});

test("buildConfirmPayload coerces missing fields to safe defaults", () => {
  const payload = buildConfirmPayload({});
  assert.deepEqual(payload, {
    run_id: "",
    account: "",
    confirmed_items: [],
    added_tickers: [],
    feedback: "",
  });
});

test("buildConfirmPayload normalises non-array checklist/added to []", () => {
  const payload = buildConfirmPayload({
    runId: "r",
    account: "a",
    checklist: null,
    added: "nope",
    feedback: 42,
  });
  assert.deepEqual(payload.confirmed_items, []);
  assert.deepEqual(payload.added_tickers, []);
  // feedback only kept when it's actually a string.
  assert.equal(payload.feedback, "");
});

// ── collectChecklist (pure decomposition) ───────────────────────────

test("collectCheckedItems keeps only checked candidates with full metadata", () => {
  const candidates = [
    { ticker: "MC", nom: "LVMH", isin: "FR0000121014", secteur: "luxe" },
    { ticker: "AIR", nom: "Airbus", secteur: "aero" },
    { ticker: "TTE", nom: "TotalEnergies", secteur: "energie" },
  ];
  // User unchecked AIR.
  const checked = ["MC", "TTE"];
  const out = collectCheckedItems(candidates, checked);
  assert.equal(out.length, 2);
  assert.deepEqual(out[0], candidates[0]);
  assert.deepEqual(out[1], candidates[2]);
});

test("collectCheckedItems falls back to {ticker} for an added-only ticker", () => {
  const candidates = [{ ticker: "MC", nom: "LVMH" }];
  // BNP was added by the user (not in the original candidate index).
  const out = collectCheckedItems(candidates, ["MC", "BNP"]);
  assert.deepEqual(out, [{ ticker: "MC", nom: "LVMH" }, { ticker: "BNP" }]);
});

test("collectCheckedItems returns [] when nothing is checked", () => {
  const candidates = [{ ticker: "MC" }, { ticker: "AIR" }];
  assert.deepEqual(collectCheckedItems(candidates, []), []);
});
