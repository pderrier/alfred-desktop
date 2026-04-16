/**
 * Tests for accuracy nudge trigger logic from app-alfred-triggers.js
 *
 * The accuracy nudge fires when a recommendation signal is misaligned with
 * actual price movement (e.g., RENFORCER but price dropped 12%). The core
 * logic is extracted here as pure functions for testability.
 *
 * Source of truth:
 *   apps/alfred-desktop/src/desktop-shell/app-alfred-triggers.js lines 501-585
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated pure logic from the accuracy nudge contextBuilder ──

// Signals where positive return = correct direction
const BULLISH_SIGNALS = new Set(["RENFORCER", "CONSERVER", "ACHETER", "BUY", "HOLD", "REINFORCE"]);
// Signals where negative return = correct direction
const BEARISH_SIGNALS = new Set(["ALLEGER", "VENDRE", "SELL", "REDUCE"]);
const THRESHOLD_PCT = 10;
const COOLDOWN_MS = 86400000; // 24 hours

/**
 * Classify a signal as bullish, bearish, or neutral.
 * @returns {"bullish"|"bearish"|"neutral"}
 */
function classifySignal(signal) {
  const upper = String(signal || "").toUpperCase();
  if (BULLISH_SIGNALS.has(upper)) return "bullish";
  if (BEARISH_SIGNALS.has(upper)) return "bearish";
  return "neutral";
}

/**
 * Calculate misalignment amount for a signal+return pair.
 * Positive = misaligned (bad), 0 = aligned or neutral.
 */
function calcMisalignment(signal, returnPct) {
  const r = Number(returnPct);
  if (!Number.isFinite(r)) return 0;
  const classification = classifySignal(signal);
  if (classification === "bullish") return -r;   // bullish + negative return = positive misalignment
  if (classification === "bearish") return r;     // bearish + positive return = positive misalignment
  return 0; // neutral signals have no misalignment
}

/**
 * Filter recommendations to find misaligned positions, applying threshold and cooldown.
 * Returns sorted array of { ticker, signal, returnPct, misalignedAmt } capped at top 2.
 */
function findMisaligned(recommendations, tickerCooldowns, now) {
  if (!Array.isArray(recommendations) || recommendations.length === 0) return [];

  const misaligned = [];
  for (const rec of recommendations) {
    const lm = rec.lineMemory || rec.memoire_ligne || {};
    const pt = lm.price_tracking;
    if (!pt || pt.return_since_signal_pct == null) continue;
    const returnPct = Number(pt.return_since_signal_pct);
    if (!Number.isFinite(returnPct)) continue;
    const signal = String(rec.signal || "").toUpperCase();
    const isBullish = BULLISH_SIGNALS.has(signal);
    const isBearish = BEARISH_SIGNALS.has(signal);
    const misalignedAmt = isBullish ? -returnPct : isBearish ? returnPct : 0;
    const ticker = rec.ticker || rec.nom || "?";
    if (misalignedAmt >= THRESHOLD_PCT) {
      const lastNudged = (tickerCooldowns || {})[ticker] || 0;
      if (now - lastNudged < COOLDOWN_MS) continue;
      misaligned.push({ ticker, signal, returnPct, misalignedAmt });
    }
  }
  if (misaligned.length === 0) return [];

  // Sort by worst misalignment first, cap at 2
  misaligned.sort((a, b) => b.misalignedAmt - a.misalignedAmt);
  return misaligned.slice(0, 2);
}

// ── Tests: Signal classification ────────────────────────────────

test("classifySignal: RENFORCER is bullish", () => {
  assert.equal(classifySignal("RENFORCER"), "bullish");
});

test("classifySignal: CONSERVER is bullish", () => {
  assert.equal(classifySignal("CONSERVER"), "bullish");
});

test("classifySignal: ACHETER is bullish", () => {
  assert.equal(classifySignal("ACHETER"), "bullish");
});

test("classifySignal: BUY is bullish", () => {
  assert.equal(classifySignal("BUY"), "bullish");
});

test("classifySignal: HOLD is bullish", () => {
  assert.equal(classifySignal("HOLD"), "bullish");
});

test("classifySignal: REINFORCE is bullish", () => {
  assert.equal(classifySignal("REINFORCE"), "bullish");
});

test("classifySignal: VENDRE is bearish", () => {
  assert.equal(classifySignal("VENDRE"), "bearish");
});

test("classifySignal: ALLEGER is bearish", () => {
  assert.equal(classifySignal("ALLEGER"), "bearish");
});

test("classifySignal: SELL is bearish", () => {
  assert.equal(classifySignal("SELL"), "bearish");
});

test("classifySignal: REDUCE is bearish", () => {
  assert.equal(classifySignal("REDUCE"), "bearish");
});

test("classifySignal: unknown signal is neutral", () => {
  assert.equal(classifySignal("UNKNOWN"), "neutral");
});

test("classifySignal: empty string is neutral", () => {
  assert.equal(classifySignal(""), "neutral");
});

test("classifySignal: null is neutral", () => {
  assert.equal(classifySignal(null), "neutral");
});

test("classifySignal: case-insensitive (lowercase input)", () => {
  assert.equal(classifySignal("renforcer"), "bullish");
  assert.equal(classifySignal("vendre"), "bearish");
});

// ── Tests: Misalignment calculation ─────────────────────────────

test("misalignment: RENFORCER with -12% return → misaligned (12)", () => {
  const amt = calcMisalignment("RENFORCER", -12);
  assert.equal(amt, 12);
});

test("misalignment: RENFORCER with +5% return → not misaligned (-5)", () => {
  const amt = calcMisalignment("RENFORCER", 5);
  assert.equal(amt, -5);
});

test("misalignment: VENDRE with +15% return → misaligned (15)", () => {
  const amt = calcMisalignment("VENDRE", 15);
  assert.equal(amt, 15);
});

test("misalignment: VENDRE with -8% return → not misaligned (-8)", () => {
  const amt = calcMisalignment("VENDRE", -8);
  assert.equal(amt, -8);
});

test("misalignment: CONSERVER with -10% return → exactly at threshold (10)", () => {
  const amt = calcMisalignment("CONSERVER", -10);
  assert.equal(amt, 10);
});

test("misalignment: unknown signal → always 0", () => {
  assert.equal(calcMisalignment("UNKNOWN", -50), 0);
  assert.equal(calcMisalignment("UNKNOWN", 50), 0);
});

test("misalignment: NaN return → 0", () => {
  assert.equal(calcMisalignment("RENFORCER", NaN), 0);
});

test("misalignment: null return → 0 (not finite, returns 0)", () => {
  // Number(null) = 0, but calcMisalignment checks isFinite first — 0 is finite,
  // so for bullish: -0 ≈ 0. Use Object.is to verify it's negative zero.
  const amt = calcMisalignment("RENFORCER", null);
  assert.ok(amt === 0 || Object.is(amt, -0), "should be 0 or -0");
  assert.ok(amt < THRESHOLD_PCT, "should be below threshold (no nudge fires)");
});

test("misalignment: ACHETER with 0% return → 0 (no misalignment)", () => {
  const amt = calcMisalignment("ACHETER", 0);
  // bullish + 0% return → -0 in JS, which is semantically 0
  assert.ok(amt === 0 || Object.is(amt, -0), "should be 0 or -0");
  assert.ok(amt < THRESHOLD_PCT, "should be below threshold");
});

// ── Tests: findMisaligned — full pipeline ───────────────────────

function makeRec(ticker, signal, returnPct) {
  return {
    ticker,
    signal,
    lineMemory: {
      price_tracking: { return_since_signal_pct: returnPct }
    }
  };
}

const NOW = 1700000000000; // fixed timestamp for tests

test("findMisaligned: RENFORCER -12% fires (above threshold)", () => {
  const recs = [makeRec("AAPL", "RENFORCER", -12)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "AAPL");
  assert.equal(result[0].misalignedAmt, 12);
});

test("findMisaligned: RENFORCER +5% does not fire (aligned)", () => {
  const recs = [makeRec("AAPL", "RENFORCER", 5)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: VENDRE +15% fires (price rose against sell)", () => {
  const recs = [makeRec("MSFT", "VENDRE", 15)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "MSFT");
  assert.equal(result[0].misalignedAmt, 15);
});

test("findMisaligned: VENDRE -8% does not fire (aligned)", () => {
  const recs = [makeRec("MSFT", "VENDRE", -8)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: CONSERVER -10% fires (exactly at threshold)", () => {
  const recs = [makeRec("GOOG", "CONSERVER", -10)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "GOOG");
  assert.equal(result[0].misalignedAmt, 10);
});

test("findMisaligned: unknown signal never fires", () => {
  const recs = [makeRec("X", "OBSERVER", -50)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: -9.9% below threshold does not fire", () => {
  const recs = [makeRec("AAPL", "ACHETER", -9.9)];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

// ── Tests: Top 2 cap ────────────────────────────────────────────

test("findMisaligned: more than 2 misaligned → only top 2 returned", () => {
  const recs = [
    makeRec("A", "RENFORCER", -15),  // misaligned 15
    makeRec("B", "RENFORCER", -25),  // misaligned 25
    makeRec("C", "RENFORCER", -12),  // misaligned 12
    makeRec("D", "RENFORCER", -30),  // misaligned 30
  ];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 2);
  // Sorted by worst first
  assert.equal(result[0].ticker, "D");
  assert.equal(result[0].misalignedAmt, 30);
  assert.equal(result[1].ticker, "B");
  assert.equal(result[1].misalignedAmt, 25);
});

test("findMisaligned: exactly 2 misaligned → both returned", () => {
  const recs = [
    makeRec("X", "VENDRE", 20),
    makeRec("Y", "ACHETER", -15),
  ];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 2);
  assert.equal(result[0].ticker, "X");
  assert.equal(result[1].ticker, "Y");
});

test("findMisaligned: 1 misaligned → returns 1", () => {
  const recs = [
    makeRec("ONLY", "RENFORCER", -11),
    makeRec("FINE", "RENFORCER", 5),
  ];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "ONLY");
});

// ── Tests: Cooldown ─────────────────────────────────────────────

test("findMisaligned: ticker nudged < 24h ago → filtered out", () => {
  const recs = [makeRec("AAPL", "RENFORCER", -20)];
  // Last nudged 1 hour ago
  const cooldowns = { AAPL: NOW - 3600000 };
  const result = findMisaligned(recs, cooldowns, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: ticker nudged exactly 24h ago → included (cooldown expired)", () => {
  const recs = [makeRec("AAPL", "RENFORCER", -20)];
  const cooldowns = { AAPL: NOW - COOLDOWN_MS };
  const result = findMisaligned(recs, cooldowns, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "AAPL");
});

test("findMisaligned: ticker nudged > 24h ago → included", () => {
  const recs = [makeRec("AAPL", "RENFORCER", -20)];
  const cooldowns = { AAPL: NOW - COOLDOWN_MS - 1 };
  const result = findMisaligned(recs, cooldowns, NOW);
  assert.equal(result.length, 1);
});

test("findMisaligned: one ticker cooled down, another not → only fresh one returned", () => {
  const recs = [
    makeRec("COOL", "RENFORCER", -30),
    makeRec("FRESH", "RENFORCER", -20),
  ];
  const cooldowns = { COOL: NOW - 1000 }; // just nudged
  const result = findMisaligned(recs, cooldowns, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "FRESH");
});

test("findMisaligned: null cooldowns treated as empty", () => {
  const recs = [makeRec("AAPL", "RENFORCER", -20)];
  const result = findMisaligned(recs, null, NOW);
  assert.equal(result.length, 1);
});

// ── Tests: Edge cases ───────────────────────────────────────────

test("findMisaligned: empty recommendations → empty result", () => {
  assert.deepEqual(findMisaligned([], {}, NOW), []);
});

test("findMisaligned: null recommendations → empty result", () => {
  assert.deepEqual(findMisaligned(null, {}, NOW), []);
});

test("findMisaligned: rec without price_tracking → skipped", () => {
  const recs = [{ ticker: "AAPL", signal: "RENFORCER", lineMemory: {} }];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: rec with null return_since_signal_pct → skipped", () => {
  const recs = [{
    ticker: "AAPL",
    signal: "RENFORCER",
    lineMemory: { price_tracking: { return_since_signal_pct: null } }
  }];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 0);
});

test("findMisaligned: rec uses memoire_ligne fallback for line memory", () => {
  const recs = [{
    ticker: "MC",
    signal: "ACHETER",
    memoire_ligne: { price_tracking: { return_since_signal_pct: -15 } }
  }];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "MC");
});

test("findMisaligned: ticker falls back to nom when ticker is missing", () => {
  const recs = [{
    nom: "LVMH",
    signal: "RENFORCER",
    lineMemory: { price_tracking: { return_since_signal_pct: -20 } }
  }];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 1);
  assert.equal(result[0].ticker, "LVMH");
});

test("findMisaligned: mixed bullish and bearish misaligned, sorted by worst", () => {
  const recs = [
    makeRec("BULL1", "ACHETER", -18),    // misaligned 18
    makeRec("BEAR1", "VENDRE", 25),       // misaligned 25
    makeRec("BULL2", "CONSERVER", -11),   // misaligned 11
  ];
  const result = findMisaligned(recs, {}, NOW);
  assert.equal(result.length, 2);
  assert.equal(result[0].ticker, "BEAR1");
  assert.equal(result[0].misalignedAmt, 25);
  assert.equal(result[1].ticker, "BULL1");
  assert.equal(result[1].misalignedAmt, 18);
});
