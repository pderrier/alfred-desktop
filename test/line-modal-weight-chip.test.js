/**
 * P1-61 — tests for renderWeightDeltaChip() from app-line-modal.js.
 *
 * The module has browser-path imports (Tauri runtime), so we replicate the
 * pure helper here. Same convention as venue-hint.test.js and
 * render-collection-quality.test.js. Source of truth:
 * apps/alfred-desktop/src/desktop-shell/app-line-modal.js — keep both in sync.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Local escapeHtml clone (matches ui-display-utils.js) ─────────

function escapeHtml(input) {
  if (input === null || input === undefined) return "";
  return String(input)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// ── Replicated helper (verbatim copy from app-line-modal.js) ─────

function renderWeightDeltaChip(targetWeightPct, currentWeightPct, weightDeltaPct) {
  const targetNum = Number.isFinite(targetWeightPct) ? targetWeightPct : null;
  const currentNum = Number.isFinite(currentWeightPct) ? currentWeightPct : null;
  if (targetNum === null) return "";
  let delta = Number.isFinite(weightDeltaPct)
    ? weightDeltaPct
    : (currentNum !== null ? Math.round((targetNum - currentNum) * 10) / 10 : null);
  if (delta === null) return "";
  if (Object.is(delta, -0)) delta = 0;
  let tone;
  if (delta > 0) tone = "positive";
  else if (delta < 0) tone = "negative";
  else tone = "neutral";
  const sign = delta > 0 ? "+" : "";
  const label = `${sign}${delta.toFixed(1)} pp`;
  const tooltipParts = [`cible ${targetNum.toFixed(1)} %`];
  if (currentNum !== null) tooltipParts.push(`actuel ${currentNum.toFixed(1)} %`);
  const tooltip = tooltipParts.join(" · ");
  return `<span class="chip chip-weight chip-weight-${tone}" title="${escapeHtml(tooltip)}">${escapeHtml(label)}</span>`;
}

// ── Tests ────────────────────────────────────────────────────────

test("renders positive delta with green tone and + sign", () => {
  const html = renderWeightDeltaChip(12.0, 8.0, 4.0);
  assert.match(html, /chip-weight-positive/);
  assert.match(html, /\+4\.0 pp/);
  assert.match(html, /cible 12\.0 %/);
  assert.match(html, /actuel 8\.0 %/);
});

test("renders negative delta with red tone and minus sign", () => {
  const html = renderWeightDeltaChip(5.0, 6.5, -1.5);
  assert.match(html, /chip-weight-negative/);
  assert.match(html, /-1\.5 pp/);
});

test("renders zero delta with neutral tone and no sign", () => {
  const html = renderWeightDeltaChip(8.0, 8.0, 0.0);
  assert.match(html, /chip-weight-neutral/);
  assert.match(html, /0\.0 pp/);
  // Must NOT carry a + sign.
  assert.ok(!/\+0\.0 pp/.test(html), `expected no leading + on zero, got: ${html}`);
});

test("hides chip when target is null", () => {
  assert.equal(renderWeightDeltaChip(null, 8.0, 4.0), "");
  assert.equal(renderWeightDeltaChip(undefined, 8.0, 4.0), "");
});

test("hides chip when target AND current are both missing", () => {
  // Without a target there's no allocation intent → nothing to show.
  assert.equal(renderWeightDeltaChip(null, null, null), "");
});

test("falls back to recomputing delta when Rust-supplied delta is missing", () => {
  // Legacy payload lost weight_delta_pct in transit but target+current
  // survived — chip should still render.
  const html = renderWeightDeltaChip(10.0, 6.0, null);
  assert.match(html, /chip-weight-positive/);
  assert.match(html, /\+4\.0 pp/);
});

test("hides chip when delta cannot be computed (target only)", () => {
  // target present but current missing AND no Rust delta → nothing to show.
  assert.equal(renderWeightDeltaChip(10.0, null, null), "");
});

test("rejects non-finite numbers (NaN, Infinity)", () => {
  assert.equal(renderWeightDeltaChip(Number.NaN, 5.0, 0.0), "");
  assert.equal(renderWeightDeltaChip(Number.POSITIVE_INFINITY, 5.0, 0.0), "");
  // Bad current does NOT block render if Rust-supplied delta is finite.
  const html = renderWeightDeltaChip(10.0, Number.NaN, 3.0);
  assert.match(html, /\+3\.0 pp/);
  // Tooltip omits the "actuel" line because current is non-finite.
  assert.ok(!/actuel/.test(html), `expected tooltip to skip 'actuel' when current is NaN, got: ${html}`);
});

test("html escapes the tooltip text", () => {
  // The tooltip uses · (middle dot), no HTML-special chars — but ensure the
  // label and tooltip pass through escapeHtml so future additions remain safe.
  const html = renderWeightDeltaChip(10.0, 8.0, 2.0);
  assert.match(html, /title="cible 10\.0 % · actuel 8\.0 %"/);
});

test("coerces negative-zero delta to plain zero (neutral, no minus sign)", () => {
  // JavaScript's -0 would format as "-0.0 pp" without the Object.is(-0) coercion.
  // Pass -0 explicitly via Rust-supplied delta path.
  const html = renderWeightDeltaChip(10.0, 10.0, -0);
  assert.match(html, /chip-weight-neutral/);
  assert.match(html, /0\.0 pp/);
  assert.ok(!/-0\.0 pp/.test(html), `negative-zero must format as 0.0, got: ${html}`);
});
