/**
 * Contract test for `signalToneClass` — Watchlist Curation v2 (D1).
 *
 * `signalToneClass` (src/desktop-shell/ui-display-utils.js) is the single
 * source of truth mapping a recommendation signal/verdict string to a CSS tone
 * class. It REPLACED the removed `recommendationActionClass`; this test is the
 * regression guard for that migration — it pins the FULL vocabulary so the tone
 * mapping cannot silently drift across the action cards, the
 * positions/recommendations table badge, and the live-run badge.
 *
 * `ui-display-utils.js` has no browser-only imports, so the real function is
 * imported directly (not replicated) — the assertions verify the SHIPPED code.
 */
import test from "node:test";
import assert from "node:assert/strict";

import { signalToneClass } from "../src/desktop-shell/ui-display-utils.js";

// Full vocabulary → expected tone class. Held-position signals, watchlist
// verdicts (D1), and the substring-matched variants are all pinned here.
const CASES = [
  // Held-position buy-side signals (substring "ACHAT"/"RENFORC").
  ["ACHAT_FORT", "tone-buy"],
  ["ACHAT", "tone-buy"],
  ["RENFORCEMENT", "tone-buy"],
  // Held-position sell-side signals (substring "VENTE"/"ALLEG").
  ["VENTE", "tone-sell"],
  ["ALLEGEMENT", "tone-sell"],
  // Held-position neutral signals.
  ["CONSERVER", "tone-neutral"],
  ["SURVEILLANCE", "tone-neutral"],
  // Watchlist verdicts (D1) — exact-match checks run BEFORE the substring
  // rules so ACHAT_SUR_REPLI is not swallowed by the "ACHAT" → tone-buy rule
  // and ECARTER never falls through to a sell tone.
  ["ENTRER", "tone-entry"],
  ["ACHAT_SUR_REPLI", "tone-entry"],
  ["SURVEILLER", "tone-neutral"],
  ["ECARTER", "tone-discard"],
];

test("signalToneClass maps the full signal/verdict vocabulary", () => {
  for (const [signal, expected] of CASES) {
    assert.equal(
      signalToneClass(signal),
      expected,
      `signal "${signal}" should map to "${expected}"`
    );
  }
});

test("signalToneClass is case-insensitive", () => {
  assert.equal(signalToneClass("achat_sur_repli"), "tone-entry");
  assert.equal(signalToneClass("ecarter"), "tone-discard");
  assert.equal(signalToneClass("vente"), "tone-sell");
});

test("signalToneClass exact-match verdicts win over substring rules", () => {
  // ACHAT_SUR_REPLI contains the "ACHAT" substring that would otherwise yield
  // tone-buy; the exact ENTRER/ACHAT_SUR_REPLI check must take precedence.
  assert.equal(signalToneClass("ACHAT_SUR_REPLI"), "tone-entry");
  assert.notEqual(signalToneClass("ACHAT_SUR_REPLI"), "tone-buy");
});

test("signalToneClass defaults to tone-neutral for unknown/empty input", () => {
  assert.equal(signalToneClass(""), "tone-neutral");
  assert.equal(signalToneClass(null), "tone-neutral");
  assert.equal(signalToneClass(undefined), "tone-neutral");
  assert.equal(signalToneClass("WHATEVER"), "tone-neutral");
});
