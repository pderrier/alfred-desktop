/**
 * Home Section 2 — "Alfred t'a dit quoi"
 *
 * P1-21 (2026-05-23) — pure helpers consumed by `app.js` welcome view
 * to format the last LLM synthesis paragraph in a compact, home-sized
 * snippet plus a chip counting non-hold pending recommandations.
 *
 * Kept as a separate module so the snippet/count logic is unit-testable
 * without driving the welcome view DOM.
 */

const MAX_LEN = 280;
const MIN_LEN_BEFORE_BOUNDARY = 200;

/**
 * Extract a home-sized first sentence from a multi-paragraph LLM
 * synthesis. Cascade :
 *   1. Take everything up to the first blank-line break (paragraph 1).
 *   2. Take the first sentence (split on `.!?` followed by whitespace).
 *   3. Cap at MAX_LEN chars, truncate on a word boundary (last space
 *      between MIN_LEN_BEFORE_BOUNDARY and MAX_LEN).
 *
 * Returns the empty string on null / non-string input.
 */
export function extractFirstSentence(text) {
  if (!text || typeof text !== "string") return "";
  const firstParagraph = text.split(/\n\s*\n/)[0] || text;
  // Lookbehind on punctuation works in all Node versions Alfred targets
  // (>= 14). Test runner is `node:test`, fine with /(?<=…)/.
  const firstSentence = firstParagraph.split(/(?<=[.!?])\s+/)[0] || firstParagraph;
  let snippet = firstSentence.trim();
  if (snippet.length <= MAX_LEN) return snippet;
  // Word-boundary truncation : try to break on a space between
  // MIN_LEN_BEFORE_BOUNDARY and (MAX_LEN - 3).
  const targetCut = snippet.lastIndexOf(" ", MAX_LEN - 3);
  const cut = targetCut > MIN_LEN_BEFORE_BOUNDARY ? targetCut : (MAX_LEN - 3);
  return `${snippet.slice(0, cut).trimEnd()}…`;
}

/**
 * Count recommandations whose `signal` is non-hold (i.e. require user
 * attention). Treats French and English "hold" synonyms as the same
 * (LLM output drifts between languages run to run) :
 *   - English  : "hold"
 *   - French   : "conserver" / "neutre"
 *
 * Empty/null arrays return 0. Items without a `signal` field are
 * skipped (don't count as "pending" since we can't classify them).
 */
export function countPendingRecos(recos) {
  if (!Array.isArray(recos)) return 0;
  const HOLD_SIGNALS = new Set(["hold", "conserver", "neutre"]);
  return recos.filter((r) => {
    const sig = String(r?.signal || "").trim().toLowerCase();
    if (!sig) return false;
    return !HOLD_SIGNALS.has(sig);
  }).length;
}
