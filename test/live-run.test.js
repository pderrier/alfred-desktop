/**
 * Tests for shell-live-run.js pure logic — status parsing, chip rendering, row classification.
 *
 * The module relies on DOM nodes at module scope, so we replicate the pure
 * helper functions here. No DOM environment needed.
 *
 * Source of truth: apps/alfred-desktop/src/desktop-shell/shell-live-run.js
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated helpers (exact copies from shell-live-run.js) ────

function escapeHtml(str) {
  const s = String(str ?? "");
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function parseLineStatus(raw) {
  return typeof raw === "object" ? (raw?.status || "unknown") : String(raw || "");
}

function lineRowClass(status) {
  if (status === "done" || status === "failed" || status === "aborted") return "line-done";
  if (status === "collecting" || status === "analyzing" || status === "repairing") return "line-active";
  return "line-waiting";
}

function renderLiveStatusChip(raw) {
  const status = parseLineStatus(raw);
  const errorMsg = typeof raw === "object" ? (raw?.error || "") : "";
  const spinner = (status === "collecting" || status === "analyzing" || status === "repairing") ? `<span class="pipeline-spinner"></span>` : "";
  const chipClass =
    status === "done" ? "s-completed" :
    status === "analyzing" ? "s-analyzing" :
    status === "repairing" ? "s-repairing" :
    status === "collecting" ? "s-collecting" :
    (status === "failed" || status === "aborted") ? "s-failed" :
    "s-queued";
  const label =
    status === "done" ? "\u2713 Done" :
    status === "collecting" ? "Collecting\u2026" :
    status === "analyzing" ? "Analyzing\u2026" :
    status === "repairing" ? "Repairing\u2026" :
    status === "failed" ? "\u2717 Failed" :
    status === "aborted" ? "Aborted" :
    status === "waiting" ? "Queued" :
    status || "\u2014";
  const chip = `<span class="pipeline-chip ${chipClass}">${spinner}${escapeHtml(label)}</span>`;
  if (errorMsg) {
    return `${chip}<span class="pipeline-error">${escapeHtml(errorMsg)}</span>`;
  }
  return chip;
}

/**
 * Replicated preRenderQueuedPositions guard logic (no DOM, just the validation).
 * Returns true if pre-render should proceed, false otherwise.
 */
function shouldPreRender(positions, existingRowCount) {
  if (!Array.isArray(positions) || positions.length === 0) return false;
  if (existingRowCount > 0) return false;
  return true;
}

/**
 * Replicated progress counter logic from updateProgressCounter.
 */
function buildProgressText(lineStatus) {
  const tickers = Object.keys(lineStatus).filter((t) => t !== "__synthesis__");
  const total = tickers.length;
  const counts = { done: 0, failed: 0, collecting: 0, analyzing: 0, repairing: 0, waiting: 0 };
  for (const t of tickers) {
    const s = parseLineStatus(lineStatus[t]);
    if (s === "done") counts.done++;
    else if (s === "failed" || s === "aborted") counts.failed++;
    else if (s === "collecting") counts.collecting++;
    else if (s === "repairing") counts.repairing++;
    else if (s === "analyzing") counts.analyzing++;
    else counts.waiting++;
  }
  const parts = [`${counts.done}/${total} done`];
  if (counts.failed > 0) parts.push(`${counts.failed} failed`);
  if (counts.collecting > 0) parts.push(`${counts.collecting} collecting`);
  if (counts.analyzing > 0) parts.push(`${counts.analyzing} analyzing`);
  if (counts.repairing > 0) parts.push(`${counts.repairing} repairing`);
  if (counts.waiting > 0) parts.push(`${counts.waiting} waiting`);
  return parts.join(" \u00b7 ");
}

// ── Tests: parseLineStatus ──────────────────────────────────────

test("parseLineStatus: string input passed through", () => {
  assert.equal(parseLineStatus("waiting"), "waiting");
  assert.equal(parseLineStatus("done"), "done");
  assert.equal(parseLineStatus("analyzing"), "analyzing");
});

test("parseLineStatus: object with status field extracts status", () => {
  assert.equal(parseLineStatus({ status: "collecting" }), "collecting");
  assert.equal(parseLineStatus({ status: "done", progress: "100%" }), "done");
});

test("parseLineStatus: object without status returns 'unknown'", () => {
  assert.equal(parseLineStatus({}), "unknown");
  assert.equal(parseLineStatus({ progress: "50%" }), "unknown");
});

test("parseLineStatus: null returns 'unknown' (typeof null === 'object' in JS)", () => {
  // typeof null === "object", so it enters the object branch: null?.status → undefined → "unknown"
  assert.equal(parseLineStatus(null), "unknown");
});

test("parseLineStatus: undefined returns empty string", () => {
  assert.equal(parseLineStatus(undefined), "");
});

// ── Tests: lineRowClass ─────────────────────────────────────────

test("lineRowClass: done/failed/aborted → line-done", () => {
  assert.equal(lineRowClass("done"), "line-done");
  assert.equal(lineRowClass("failed"), "line-done");
  assert.equal(lineRowClass("aborted"), "line-done");
});

test("lineRowClass: collecting/analyzing/repairing → line-active", () => {
  assert.equal(lineRowClass("collecting"), "line-active");
  assert.equal(lineRowClass("analyzing"), "line-active");
  assert.equal(lineRowClass("repairing"), "line-active");
});

test("lineRowClass: waiting/unknown → line-waiting", () => {
  assert.equal(lineRowClass("waiting"), "line-waiting");
  assert.equal(lineRowClass("unknown"), "line-waiting");
  assert.equal(lineRowClass(""), "line-waiting");
});

// ── Tests: renderLiveStatusChip ─────────────────────────────────

test("renderLiveStatusChip: waiting renders as 'Queued' with s-queued class", () => {
  const chip = renderLiveStatusChip("waiting");
  assert.ok(chip.includes("s-queued"));
  assert.ok(chip.includes("Queued"));
  assert.ok(!chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: object with status=waiting renders as 'Queued'", () => {
  const chip = renderLiveStatusChip({ status: "waiting" });
  assert.ok(chip.includes("s-queued"));
  assert.ok(chip.includes("Queued"));
});

test("renderLiveStatusChip: collecting renders with spinner and s-collecting", () => {
  const chip = renderLiveStatusChip("collecting");
  assert.ok(chip.includes("s-collecting"));
  assert.ok(chip.includes("Collecting"));
  assert.ok(chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: analyzing renders with spinner and s-analyzing", () => {
  const chip = renderLiveStatusChip("analyzing");
  assert.ok(chip.includes("s-analyzing"));
  assert.ok(chip.includes("Analyzing"));
  assert.ok(chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: repairing renders with spinner and s-repairing", () => {
  const chip = renderLiveStatusChip("repairing");
  assert.ok(chip.includes("s-repairing"));
  assert.ok(chip.includes("Repairing"));
  assert.ok(chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: done renders checkmark without spinner", () => {
  const chip = renderLiveStatusChip("done");
  assert.ok(chip.includes("s-completed"));
  assert.ok(chip.includes("\u2713 Done"));
  assert.ok(!chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: failed renders X mark without spinner", () => {
  const chip = renderLiveStatusChip("failed");
  assert.ok(chip.includes("s-failed"));
  assert.ok(chip.includes("\u2717 Failed"));
  assert.ok(!chip.includes("pipeline-spinner"));
});

test("renderLiveStatusChip: aborted uses s-failed class", () => {
  const chip = renderLiveStatusChip("aborted");
  assert.ok(chip.includes("s-failed"));
  assert.ok(chip.includes("Aborted"));
});

test("renderLiveStatusChip: object with error shows error span", () => {
  const chip = renderLiveStatusChip({ status: "failed", error: "Timeout" });
  assert.ok(chip.includes("s-failed"));
  assert.ok(chip.includes("pipeline-error"));
  assert.ok(chip.includes("Timeout"));
});

test("renderLiveStatusChip: error message is HTML-escaped", () => {
  const chip = renderLiveStatusChip({ status: "failed", error: "<b>bad</b>" });
  assert.ok(chip.includes("&lt;b&gt;bad&lt;/b&gt;"));
  assert.ok(!chip.includes("<b>bad</b>"));
});

test("renderLiveStatusChip: unknown status falls through to raw label with s-queued class", () => {
  const chip = renderLiveStatusChip("something_else");
  assert.ok(chip.includes("s-queued"));
  assert.ok(chip.includes("something_else"));
});

test("renderLiveStatusChip: null renders 'unknown' label with s-queued (typeof null === 'object')", () => {
  // null hits object branch → status "unknown" → fallthrough label is "unknown"
  const chip = renderLiveStatusChip(null);
  assert.ok(chip.includes("s-queued"));
  assert.ok(chip.includes("unknown"));
});

test("renderLiveStatusChip: undefined renders em dash with s-queued", () => {
  const chip = renderLiveStatusChip(undefined);
  assert.ok(chip.includes("s-queued"));
  assert.ok(chip.includes("\u2014"));
});

// ── Tests: preRender guard logic ────────────────────────────────

test("shouldPreRender: valid positions array with no existing rows → true", () => {
  assert.ok(shouldPreRender([{ ticker: "AAPL" }], 0));
});

test("shouldPreRender: empty array → false", () => {
  assert.ok(!shouldPreRender([], 0));
});

test("shouldPreRender: null → false", () => {
  assert.ok(!shouldPreRender(null, 0));
});

test("shouldPreRender: undefined → false", () => {
  assert.ok(!shouldPreRender(undefined, 0));
});

test("shouldPreRender: existing rows > 0 → false (already have content)", () => {
  assert.ok(!shouldPreRender([{ ticker: "AAPL" }], 3));
});

test("shouldPreRender: non-array → false", () => {
  assert.ok(!shouldPreRender("not an array", 0));
  assert.ok(!shouldPreRender({}, 0));
});

// ── Tests: progress counter ─────────────────────────────────────

test("buildProgressText: all waiting", () => {
  const text = buildProgressText({ AAPL: "waiting", MSFT: "waiting" });
  assert.equal(text, "0/2 done \u00b7 2 waiting");
});

test("buildProgressText: mixed statuses", () => {
  const text = buildProgressText({
    AAPL: "done",
    MSFT: { status: "analyzing" },
    GOOG: "collecting",
    META: "waiting",
    AMZN: { status: "failed" },
  });
  assert.equal(text, "1/5 done \u00b7 1 failed \u00b7 1 collecting \u00b7 1 analyzing \u00b7 1 waiting");
});

test("buildProgressText: all done", () => {
  const text = buildProgressText({ AAPL: "done", MSFT: "done" });
  assert.equal(text, "2/2 done");
});

test("buildProgressText: __synthesis__ key excluded from count", () => {
  const text = buildProgressText({
    AAPL: "done",
    __synthesis__: { status: "generating", progress: "50%" }
  });
  assert.equal(text, "1/1 done");
});

test("buildProgressText: repairing status counted", () => {
  const text = buildProgressText({ AAPL: { status: "repairing" } });
  assert.equal(text, "0/1 done \u00b7 1 repairing");
});

test("buildProgressText: aborted counted as failed", () => {
  const text = buildProgressText({ AAPL: "aborted" });
  assert.equal(text, "0/1 done \u00b7 1 failed");
});
