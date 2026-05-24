import test from "node:test";
import assert from "node:assert/strict";
import {
  formatMacroTile,
  vixBucketLabel,
} from "../src/desktop-shell/macro-home-tile.js";

// Helper — build a server envelope with the four indicators. Pass `null`
// to omit an indicator (the server emits `null` for a failed symbol).
function envelope({ us10y = null, vix = null, eurusd = null, brent = null } = {}) {
  const macro = {};
  macro.us_10y_yield = us10y === null ? null : { value: us10y, as_of: "2026-05-24T15:30:00Z" };
  macro.vix = vix === null ? null : { value: vix, as_of: "2026-05-24T15:30:00Z" };
  macro.eur_usd = eurusd === null ? null : { value: eurusd, as_of: "2026-05-24T15:30:00Z" };
  macro.brent_usd = brent === null ? null : { value: brent, as_of: "2026-05-24T15:30:00Z" };
  return {
    ok: true,
    macro,
    cache_hit: true,
    source: "yahoo+cache",
    as_of: "2026-05-24T15:30:00Z",
  };
}

// ── vixBucketLabel ──────────────────────────────────────────────────

test("vixBucketLabel: faible below 15", () => {
  assert.equal(vixBucketLabel(0), "faible");
  assert.equal(vixBucketLabel(10), "faible");
  assert.equal(vixBucketLabel(14.99), "faible");
});

test("vixBucketLabel: boundary 15 is modérée (inclusive on low side)", () => {
  // Matches the Rust contract in services/macro_briefing.rs::vix_bucket_label —
  // `vix < 15` is faible, so 15.0 itself enters the next bucket.
  assert.equal(vixBucketLabel(15), "modérée");
  assert.equal(vixBucketLabel(20), "modérée");
  assert.equal(vixBucketLabel(24.99), "modérée");
});

test("vixBucketLabel: boundary 25 is élevée", () => {
  assert.equal(vixBucketLabel(25), "élevée");
  assert.equal(vixBucketLabel(30), "élevée");
  assert.equal(vixBucketLabel(34.99), "élevée");
});

test("vixBucketLabel: boundary 35 is stress", () => {
  assert.equal(vixBucketLabel(35), "stress");
  assert.equal(vixBucketLabel(50), "stress");
  assert.equal(vixBucketLabel(100), "stress");
});

test("vixBucketLabel: non-finite returns empty", () => {
  assert.equal(vixBucketLabel(null), "");
  assert.equal(vixBucketLabel(undefined), "");
  assert.equal(vixBucketLabel(NaN), "");
  assert.equal(vixBucketLabel("not a number"), "");
});

// ── formatMacroTile ─────────────────────────────────────────────────

test("formatMacroTile: renders full payload with all 4 indicators", () => {
  const env = envelope({ us10y: 4.55, vix: 16.7, eurusd: 1.083, brent: 82.15 });
  // Pinned format — drift would break the home tile contract. Spaces
  // around `·`, `%` after 10Y, parens around bucket, 3 decimals for
  // EUR/USD, `$` (not `USD`) after Brent to keep the line short.
  const expected = "🌍 Contexte macro : 10Y 4.55 % · VIX 16.7 (modérée) · EUR/USD 1.083 · Brent 82.15 $";
  assert.equal(formatMacroTile(env), expected);
});

test("formatMacroTile: total length stays ≤ 90 chars in the typical full case", () => {
  // Sanity guard — keeps the line glanceable on one row at the default
  // home container width (max-width 480px). The full payload renders at
  // ~83 chars including the emoji prefix and accented bucket label ; 90
  // gives a small headroom for a future indicator format tweak without
  // re-flowing onto two lines.
  const env = envelope({ us10y: 4.55, vix: 16.7, eurusd: 1.083, brent: 82.15 });
  const out = formatMacroTile(env);
  assert.ok(out !== null);
  // emoji is a 4-byte UTF-16 surrogate pair, count by characters not bytes
  assert.ok(out.length <= 90, `expected ≤90 chars, got ${out.length}: ${out}`);
});

test("formatMacroTile: handles partial data — VIX null", () => {
  const env = envelope({ us10y: 4.55, vix: null, eurusd: 1.083, brent: 82.15 });
  const out = formatMacroTile(env);
  assert.equal(out, "🌍 Contexte macro : 10Y 4.55 % · EUR/USD 1.083 · Brent 82.15 $");
  assert.ok(!out.includes("VIX"));
});

test("formatMacroTile: handles partial data — only one indicator present", () => {
  const env = envelope({ vix: 22.4 });
  assert.equal(formatMacroTile(env), "🌍 Contexte macro : VIX 22.4 (modérée)");
});

test("formatMacroTile: VIX stress regime renders the stress label", () => {
  const env = envelope({ vix: 45.0 });
  assert.equal(formatMacroTile(env), "🌍 Contexte macro : VIX 45.0 (stress)");
});

test("formatMacroTile: returns null when no usable data (all indicators null)", () => {
  const env = envelope({});
  assert.equal(formatMacroTile(env), null);
});

test("formatMacroTile: returns null when envelope.macro is null", () => {
  assert.equal(
    formatMacroTile({ ok: true, macro: null, cache_hit: false, source: "", as_of: "" }),
    null
  );
});

test("formatMacroTile: returns null when envelope itself is null/undefined/non-object", () => {
  assert.equal(formatMacroTile(null), null);
  assert.equal(formatMacroTile(undefined), null);
  assert.equal(formatMacroTile("string"), null);
  assert.equal(formatMacroTile(42), null);
});

test("formatMacroTile: returns null when macro is missing entirely", () => {
  // Should match the silent-degrade envelope `{ok: true}` with no `macro`.
  assert.equal(formatMacroTile({ ok: true }), null);
});

test("formatMacroTile: rejects indicators whose value is not a finite number", () => {
  // Server emits null on a failed symbol, but defence in depth: a malformed
  // value (NaN, Infinity, string, missing) must drop the segment, not crash.
  const env = {
    ok: true,
    macro: {
      us_10y_yield: { value: NaN, as_of: "x" },
      vix: { value: "not a number", as_of: "x" },
      eur_usd: { value: Infinity, as_of: "x" },
      brent_usd: { value: 82.15, as_of: "x" },
    },
  };
  assert.equal(formatMacroTile(env), "🌍 Contexte macro : Brent 82.15 $");
});

test("formatMacroTile: glanceable emoji prefix is the first character segment", () => {
  // Pinned: the visual hook must come first. If the prefix ever changes
  // we want the test to flag it for review.
  const env = envelope({ us10y: 4.55 });
  const out = formatMacroTile(env);
  assert.ok(out.startsWith("🌍 Contexte macro : "), `prefix lost: ${out}`);
});

test("formatMacroTile: segment separator is the middle-dot, not pipe or comma", () => {
  // Pinned: consistency with `globalSynthesisCard` and `sectorAllocationCard`
  // (both use ` · ` as the segment separator).
  const env = envelope({ us10y: 4.55, vix: 16.7 });
  const out = formatMacroTile(env);
  assert.ok(out.includes(" · "), `separator lost: ${out}`);
  assert.ok(!out.includes(" | "));
  assert.ok(!out.includes(", "));
});
