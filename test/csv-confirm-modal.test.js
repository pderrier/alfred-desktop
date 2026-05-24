/**
 * Tests for the CSV confirm modal — P0-77b (v0.4.4).
 *
 * The modal lives in `apps/alfred-desktop/src/desktop-shell/app-csv-confirm-modal.js`
 * but consumes browser-only APIs (document, Tauri.invoke) at the top of
 * the module — so we cannot import it directly under the Node test
 * runner. Instead we replicate its pure logic here (same pattern as
 * `normalize-line-memory.test.js`) and exercise:
 *   - default-radio selection rules (historical suggestion / parts pattern / skip),
 *   - corrections collection (manual-ISIN value capture, idle skip rows),
 *   - confirm-enabled guard (blocks when a manual_isin row has empty/invalid input).
 *
 * Source of truth: app-csv-confirm-modal.js — defaultActionFor /
 * collectCorrections / updateConfirmEnabled. The replicated functions
 * MUST stay byte-identical to the originals; any divergence is a test
 * bug. If the modal changes, mirror the change here.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated pure logic from app-csv-confirm-modal.js ─────────────

function defaultActionFor(item) {
  if (!item) return "skip";
  if (item.historical_suggestion) return "accept_suggestion";
  if (item.is_parts_pattern === true) return "cash_equivalent";
  return "skip";
}

/**
 * Pure form of collectCorrections — given an array of row objects
 * `{ position_index, action, isin? }`, return the payload that would be
 * emitted by walking the DOM. The modal-side version reads from radio
 * inputs + the text field; this pure form skips the DOM layer.
 */
function collectCorrections(rows) {
  const out = [];
  for (const r of rows) {
    if (r.action === "manual_isin") {
      out.push({
        position_index: r.position_index,
        action: r.action,
        isin: (r.isin || "").trim().toUpperCase(),
      });
    } else {
      out.push({ position_index: r.position_index, action: r.action });
    }
  }
  return out;
}

/**
 * Pure form of buildModalResult — given the confirm/cancel branch and a
 * corrections array, returns the shape the modal resolves with. Mirrors
 * the helper of the same name in app-csv-confirm-modal.js so the contract
 * the analysis pipeline reads is pinned here.
 */
function buildModalResult(confirmed, corrections) {
  if (!confirmed) return null;
  return { confirmed: true, corrections: Array.isArray(corrections) ? corrections : [] };
}

/**
 * Pure form of updateConfirmEnabled — returns true when the Confirm
 * button should be enabled. Rules:
 *   - manual_isin with empty input → blocked,
 *   - manual_isin with status.valid === "false" → blocked,
 *   - manual_isin with status.valid !== "true" (e.g. unset) → blocked,
 *   - everything else → allowed.
 */
function isConfirmEnabled(rows) {
  for (const r of rows) {
    if (r.action !== "manual_isin") continue;
    const value = (r.isin || "").trim();
    if (!value) return false;
    if (r.statusValid !== "true") return false;
  }
  return true;
}

// ── Tests ──────────────────────────────────────────────────────────

test("defaultActionFor prefers accept_suggestion when history exists", () => {
  const item = {
    position_index: 0,
    historical_suggestion: { ticker: "OVH", isin: "FR0014005HJ9" },
    is_parts_pattern: false,
  };
  assert.equal(defaultActionFor(item), "accept_suggestion");
});

test("defaultActionFor falls back to cash_equivalent for parts pattern", () => {
  const item = {
    position_index: 0,
    historical_suggestion: null,
    is_parts_pattern: true,
  };
  assert.equal(defaultActionFor(item), "cash_equivalent");
});

test("defaultActionFor defaults to skip when no history and not parts pattern", () => {
  const item = {
    position_index: 0,
    historical_suggestion: null,
    is_parts_pattern: false,
  };
  assert.equal(defaultActionFor(item), "skip");
});

test("defaultActionFor handles null/undefined input safely", () => {
  assert.equal(defaultActionFor(null), "skip");
  assert.equal(defaultActionFor(undefined), "skip");
});

test("collectCorrections emits action without isin for non-manual rows", () => {
  const rows = [
    { position_index: 0, action: "accept_suggestion" },
    { position_index: 1, action: "cash_equivalent" },
    { position_index: 2, action: "skip" },
  ];
  const out = collectCorrections(rows);
  assert.equal(out.length, 3);
  assert.deepEqual(out[0], { position_index: 0, action: "accept_suggestion" });
  assert.deepEqual(out[1], { position_index: 1, action: "cash_equivalent" });
  assert.deepEqual(out[2], { position_index: 2, action: "skip" });
});

test("collectCorrections upper-cases and trims manual_isin value", () => {
  const rows = [
    { position_index: 7, action: "manual_isin", isin: "  fr0014005hj9  " },
  ];
  const out = collectCorrections(rows);
  assert.deepEqual(out, [
    { position_index: 7, action: "manual_isin", isin: "FR0014005HJ9" },
  ]);
});

test("isConfirmEnabled returns true when no manual_isin rows", () => {
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "accept_suggestion" },
    { position_index: 1, action: "skip" },
  ]), true);
});

test("isConfirmEnabled blocks when manual_isin has empty value", () => {
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "manual_isin", isin: "", statusValid: "" },
  ]), false);
});

test("isConfirmEnabled blocks when manual_isin status is invalid", () => {
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "manual_isin", isin: "GARBAGE", statusValid: "false" },
  ]), false);
});

test("isConfirmEnabled blocks when manual_isin status is missing/unset", () => {
  // Live-validation hasn't completed — defensive default keeps us blocked.
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "manual_isin", isin: "FR0014005HJ9", statusValid: "" },
  ]), false);
});

test("isConfirmEnabled allows manual_isin once value is valid", () => {
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "manual_isin", isin: "FR0014005HJ9", statusValid: "true" },
  ]), true);
});

test("isConfirmEnabled blocks even if other rows are clean", () => {
  // Mixed rows — one bad manual_isin must veto the whole confirm.
  assert.equal(isConfirmEnabled([
    { position_index: 0, action: "accept_suggestion" },
    { position_index: 1, action: "manual_isin", isin: "", statusValid: "" },
    { position_index: 2, action: "skip" },
  ]), false);
});

// ── P0-77b QA BLOCK-2 ────────────────────────────────────────────
//
// Pin the modal's return shape on the confirm and cancel paths. The
// downstream caller in app-wizard.js destructures `{ confirmed,
// corrections }` and feeds `corrections` into
// `csv_import_apply_corrections_local` — any drift in shape (e.g.
// dropping the `confirmed` flag or wrapping the array) breaks the
// pre-runAnalysis hand-off.
test("modal_assembles_correct_shape_on_confirm", () => {
  const corrections = [
    { position_index: 0, action: "accept_suggestion" },
    { position_index: 1, action: "cash_equivalent" },
    { position_index: 2, action: "manual_isin", isin: "FR0014005HJ9" },
    { position_index: 3, action: "skip" },
  ];
  const result = buildModalResult(true, corrections);
  assert.deepEqual(result, {
    confirmed: true,
    corrections: [
      { position_index: 0, action: "accept_suggestion" },
      { position_index: 1, action: "cash_equivalent" },
      { position_index: 2, action: "manual_isin", isin: "FR0014005HJ9" },
      { position_index: 3, action: "skip" },
    ],
  });
});

test("modal_returns_null_on_cancel", () => {
  // Cancel / close / overlay-click — all three handlers funnel through
  // buildModalResult(false), which must yield exactly null. The caller
  // checks `if (result === null)` to short-circuit the analysis. Even
  // when given a corrections array (defensive — callers should not pass
  // one on cancel, but if they do, the cancel branch wins).
  assert.equal(buildModalResult(false), null);
  assert.equal(buildModalResult(false, [{ position_index: 0, action: "skip" }]), null);
});
