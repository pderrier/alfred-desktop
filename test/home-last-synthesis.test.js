import test from "node:test";
import assert from "node:assert/strict";
import {
  extractFirstSentence,
  countPendingRecos,
} from "../src/desktop-shell/home-last-synthesis.js";

// ── P1-21: extractFirstSentence ─────────────────────────────────────

test("extractFirstSentence: returns the first paragraph for multi-paragraph input", () => {
  const text =
    "Le marché reste hésitant face aux taux. La direction reste exposée aux exports.\n\nDeuxième paragraphe ignoré.";
  // Splits on blank line first, then on the first .!? — so we get
  // only the first sentence of the first paragraph.
  assert.equal(
    extractFirstSentence(text),
    "Le marché reste hésitant face aux taux.",
  );
});

test("extractFirstSentence: single-sentence input returned untouched", () => {
  const text = "Seul paragraphe sans split.";
  assert.equal(extractFirstSentence(text), text);
});

test("extractFirstSentence: truncates very long output on a word boundary", () => {
  const repeated = "Mot ".repeat(100); // 400 chars total
  const result = extractFirstSentence(repeated);
  assert.ok(result.endsWith("…"), `expected ellipsis, got: ${result.slice(-5)}`);
  assert.ok(result.length <= 280, `expected ≤280 chars, got ${result.length}`);
  // Truncated on a word boundary (no half-word at the end).
  assert.ok(!result.slice(0, -1).endsWith(" "), "ellipsis must follow the last full word");
});

test("extractFirstSentence: handles null/empty/non-string input gracefully", () => {
  assert.equal(extractFirstSentence(null), "");
  assert.equal(extractFirstSentence(undefined), "");
  assert.equal(extractFirstSentence(""), "");
  assert.equal(extractFirstSentence(123), "");
  assert.equal(extractFirstSentence({}), "");
});

test("extractFirstSentence: paragraph with no sentence terminator returns the whole paragraph", () => {
  const text = "Une phrase sans ponctuation finale";
  assert.equal(extractFirstSentence(text), text);
});

test("extractFirstSentence: trims leading/trailing whitespace", () => {
  assert.equal(
    extractFirstSentence("   Une phrase propre.   "),
    "Une phrase propre.",
  );
});

// ── P1-21: countPendingRecos ────────────────────────────────────────

test("countPendingRecos: counts non-hold buy/sell signals", () => {
  const recos = [
    { signal: "buy" }, { signal: "sell" }, { signal: "hold" },
    { signal: "buy" },
  ];
  assert.equal(countPendingRecos(recos), 3);
});

test("countPendingRecos: treats French hold synonyms as non-pending", () => {
  const recos = [
    { signal: "conserver" }, { signal: "Neutre" }, { signal: "achat" },
    { signal: "HOLD" }, { signal: "Buy" },
  ];
  // achat + Buy = 2 pending ; conserver / Neutre / HOLD all suppressed.
  assert.equal(countPendingRecos(recos), 2);
});

test("countPendingRecos: skips items missing the signal field", () => {
  const recos = [
    { signal: "" }, { ticker: "MC" }, { signal: null }, { signal: "buy" },
  ];
  assert.equal(countPendingRecos(recos), 1);
});

test("countPendingRecos: handles null/non-array input safely", () => {
  assert.equal(countPendingRecos(null), 0);
  assert.equal(countPendingRecos(undefined), 0);
  assert.equal(countPendingRecos({}), 0);
  assert.equal(countPendingRecos("not an array"), 0);
});

test("countPendingRecos: empty array returns 0", () => {
  assert.equal(countPendingRecos([]), 0);
});
