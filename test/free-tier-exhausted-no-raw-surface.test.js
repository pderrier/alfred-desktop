import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

// ── v0.4.8 P0 anti-regression — Pierre saw the raw code on v0.4.7 ──
//
// v0.4.7 P3-31 patched the `alfred_free_tier_exhausted:…` routing in
// `app-wizard.js::displayError`, but the run-progress handler in
// `app.js` (run.failed → showErrorModal) bypassed it. The user saw the
// literal `alfred_free_tier_exhausted:179364:3:rolling_7d` rendered as
// the modal body.
//
// This test is the CONTRACT that pins the fix: every `showErrorModal`
// call site in the shell that could possibly receive a quota code MUST
// go through `displayQuotaAwareError` (or `buildFreeTierExhaustedModalCopy`
// for the splash path). Static analysis — runs against source text so
// it cannot drift behind a green unit suite.

const __dirname = dirname(fileURLToPath(import.meta.url));
const SHELL_DIR = resolve(__dirname, "../src/desktop-shell");

/** Files that drive run/error display surfaces. */
const SURFACES = [
  "app.js",
  "app-wizard.js",
];

/**
 * Strip block + line comments so commented-out references don't trip
 * the assertions. Conservative: we do NOT try to parse strings; the raw
 * code literal in a string is exactly what we're hunting.
 */
function stripComments(src) {
  // Remove /* … */ blocks (non-greedy, multi-line).
  let out = src.replace(/\/\*[\s\S]*?\*\//g, "");
  // Remove `// …` lines (keep the newline so line counts roughly hold).
  out = out.replace(/^\s*\/\/.*$/gm, "");
  return out;
}

test("anti-regression: no source file contains the literal raw quota code", () => {
  for (const name of SURFACES) {
    const src = readFileSync(resolve(SHELL_DIR, name), "utf8");
    const code = stripComments(src);
    assert.equal(
      /["'`]alfred_free_tier_exhausted:/.test(code),
      false,
      `${name} must not embed the raw quota code as a string literal — it should ALWAYS flow through parseStructuredErrorCode + buildFreeTierExhaustedModalCopy`
    );
  }
});

test("anti-regression: every showErrorModal call in run-progress / wizard paths routes via displayQuotaAwareError or buildFreeTierExhaustedModalCopy", () => {
  // Allowed (sanctioned) call patterns for `showErrorModal` in
  // `app.js` / `app-wizard.js` — they fall into ONE of:
  //   (a) inside a `displayQuotaAwareError` fallback callback (the fallback IS the modal);
  //   (b) inside a `buildFreeTierExhaustedModalCopy(...)` block (splash path);
  //   (c) the `showErrorModal` import re-export wiring (not a call).
  //
  // Any other bare `showErrorModal("Analysis Failed", ...)` call in
  // these two files is a smell — it means a new error path was added
  // without going through the quota-aware router. Fail loudly.

  for (const name of SURFACES) {
    const src = readFileSync(resolve(SHELL_DIR, name), "utf8");
    const code = stripComments(src);

    // Bag of lines that look like `showErrorModal(` invocations.
    const lines = code.split("\n");
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      // Look only for invocations of `showErrorModal(` — i.e. a `(`
      // immediately after the identifier. Skips re-export lines like
      // `showErrorModal,` in import lists or the function definition.
      if (!/\bshowErrorModal\s*\(/.test(line)) continue;
      // Skip dependency-passing object literals like `{ showErrorModal,`
      // or `showErrorModal: foo` — only the call form `showErrorModal(`
      // matters.
      if (/[{,]\s*showErrorModal\s*[,}]/.test(line)) continue;

      // Inspect a small window above the call for the sanctioning context.
      const windowStart = Math.max(0, i - 8);
      const windowText = lines.slice(windowStart, i + 1).join("\n");
      const sanctioned =
        /displayQuotaAwareError\s*\(/.test(windowText) ||
        /buildFreeTierExhaustedModalCopy\s*\(/.test(windowText);
      assert.equal(
        sanctioned,
        true,
        `${name}:${i + 1} — showErrorModal call is not preceded by displayQuotaAwareError or buildFreeTierExhaustedModalCopy.\n` +
          `If this error path can NEVER receive a quota code, document with an inline\n` +
          `// LINT-ALLOW: no-quota-code marker above the call. Otherwise route it.\n` +
          `Context:\n${windowText}`
      );
    }
  }
});

test("anti-regression: app.js run-progress handler delegates to displayQuotaAwareError", () => {
  // Pin the EXACT call shape of the bug-site fix so a future refactor
  // can't accidentally drop the routing.
  const src = readFileSync(resolve(SHELL_DIR, "app.js"), "utf8");
  // Locate the run.failed branch and assert it contains the helper.
  assert.equal(
    /Analysis Failed[\s\S]{0,400}displayQuotaAwareError|displayQuotaAwareError[\s\S]{0,400}Analysis Failed/.test(src),
    true,
    "app.js's 'Analysis Failed' modal must be guarded by displayQuotaAwareError — see v0.4.7 → v0.4.8 P0"
  );
});

test("anti-regression: app-wizard.js displayError delegates to displayQuotaAwareError", () => {
  const src = readFileSync(resolve(SHELL_DIR, "app-wizard.js"), "utf8");
  // The wizard displayError body must call the shared helper, not the
  // inline parseStructuredErrorCode + buildFreeTierExhaustedModalCopy
  // duplication that v0.4.7 shipped.
  const match = src.match(/function displayError\s*\([^)]*\)\s*{([\s\S]*?)\n  }/);
  assert.ok(match, "displayError function must exist");
  const body = match[1];
  assert.equal(
    /displayQuotaAwareError\s*\(/.test(body),
    true,
    "displayError must route via displayQuotaAwareError (DRY contract)"
  );
});
