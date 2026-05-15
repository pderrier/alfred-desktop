/**
 * Tests for the per-ticker retry/failure status aggregator (P0-7).
 *
 * The aggregator is a pure module driven by the
 * `alfred://ticker-collection-event` Tauri event emitted by the Rust
 * enrichment retry loop (worktree v032-A). The badge surfaces retry +
 * failure counts in real time during a run.
 *
 * Source of truth: apps/alfred-desktop/src/desktop-shell/ticker-collection-status.js
 *   - updateTickerCollectionStatus(payload, { renderHtml })
 *   - resetTickerCollectionStatus({ renderHtml })
 *   - getTickerCollectionStatusState()  (test seam)
 *   - __resetTickerCollectionStatusForTests()
 *
 * The renderHtml sink lets tests assert HTML output with no DOM. The
 * shell-live-run consumer passes a sink that writes to
 * `#run-ticker-collection-status`.
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  updateTickerCollectionStatus,
  resetTickerCollectionStatus,
  getTickerCollectionStatusState,
  __resetTickerCollectionStatusForTests as resetForTests,
} from "../src/desktop-shell/ticker-collection-status.js";

function makeSink() {
  const calls = [];
  return {
    sink: (html, hidden) => { calls.push({ html, hidden }); },
    last: () => calls[calls.length - 1] || null,
    calls,
  };
}

test("aggregate counters increment on retry events", () => {
  resetForTests();
  const { sink } = makeSink();
  updateTickerCollectionStatus(
    { ticker: "STMPA.PA", kind: "retry", attempt: 1, reason: "transport_timeout", run_id: "r1" },
    { renderHtml: sink }
  );
  updateTickerCollectionStatus(
    { ticker: "AAPL", kind: "retry", attempt: 1, reason: "transport_timeout", run_id: "r1" },
    { renderHtml: sink }
  );
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 2);
  assert.equal(state.totalFailures, 0);
  assert.equal(state.tickersRetried.size, 2);
});

test("aggregate counters increment on failure events", () => {
  resetForTests();
  const { sink } = makeSink();
  updateTickerCollectionStatus(
    { ticker: "STMPA.PA", kind: "failure", attempts: 3, reason: "transport_timeout", run_id: "r1" },
    { renderHtml: sink }
  );
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalFailures, 1);
  assert.equal(state.totalRetries, 0);
  assert.ok(state.tickersFailed.has("STMPA.PA"));
});

test("badge text reflects current counters (both retries and failures)", () => {
  resetForTests();
  const { sink, last } = makeSink();
  updateTickerCollectionStatus({ ticker: "A", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "B", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "C", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "D", kind: "failure", attempts: 3, run_id: "r1" }, { renderHtml: sink });
  const rendered = last();
  assert.equal(rendered.hidden, false);
  assert.ok(rendered.html.includes("3 retries"), `expected "3 retries" in: ${rendered.html}`);
  assert.ok(rendered.html.includes("1 failed"), `expected "1 failed" in: ${rendered.html}`);
});

test("badge omits retries half when zero retries", () => {
  resetForTests();
  const { sink, last } = makeSink();
  updateTickerCollectionStatus({ ticker: "X", kind: "failure", attempts: 3, run_id: "r1" }, { renderHtml: sink });
  const rendered = last();
  assert.equal(rendered.hidden, false);
  assert.ok(!rendered.html.includes("retries"), `should not show retries when 0: ${rendered.html}`);
  assert.ok(rendered.html.includes("1 failed"));
});

test("badge omits failures half when zero failures", () => {
  resetForTests();
  const { sink, last } = makeSink();
  updateTickerCollectionStatus({ ticker: "X", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  const rendered = last();
  assert.equal(rendered.hidden, false);
  assert.ok(rendered.html.includes("1 retries"));
  assert.ok(!rendered.html.includes("failed"), `should not show failures when 0: ${rendered.html}`);
});

test("badge hidden when no events", () => {
  resetForTests();
  const { sink, last } = makeSink();
  resetTickerCollectionStatus({ renderHtml: sink });
  const rendered = last();
  assert.equal(rendered.hidden, true);
  assert.equal(rendered.html, "");
});

test("counters reset when a new run_id arrives via update", () => {
  resetForTests();
  const { sink } = makeSink();
  updateTickerCollectionStatus({ ticker: "A", kind: "retry", attempt: 1, run_id: "run-1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "B", kind: "retry", attempt: 1, run_id: "run-1" }, { renderHtml: sink });
  assert.equal(getTickerCollectionStatusState().totalRetries, 2);

  // New run begins — first event carries the new run_id.
  updateTickerCollectionStatus({ ticker: "C", kind: "retry", attempt: 1, run_id: "run-2" }, { renderHtml: sink });
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 1, "previous run counters must be cleared");
  assert.equal(state.totalFailures, 0);
  assert.equal(state.runId, "run-2");
});

test("explicit resetTickerCollectionStatus clears state and hides badge", () => {
  resetForTests();
  const { sink, last } = makeSink();
  updateTickerCollectionStatus({ ticker: "A", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "B", kind: "failure", attempts: 3, run_id: "r1" }, { renderHtml: sink });
  resetTickerCollectionStatus({ renderHtml: sink });
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 0);
  assert.equal(state.totalFailures, 0);
  assert.equal(state.runId, null);
  const rendered = last();
  assert.equal(rendered.hidden, true);
});

test("optional success event does not inflate counters", () => {
  resetForTests();
  const { sink } = makeSink();
  updateTickerCollectionStatus({ ticker: "A", kind: "success", run_id: "r1" }, { renderHtml: sink });
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 0);
  assert.equal(state.totalFailures, 0);
});

test("malformed payload (no ticker / no kind) is ignored without throwing", () => {
  resetForTests();
  const { sink } = makeSink();
  assert.doesNotThrow(() => {
    updateTickerCollectionStatus({}, { renderHtml: sink });
    updateTickerCollectionStatus({ ticker: "" }, { renderHtml: sink });
    updateTickerCollectionStatus({ kind: "retry" }, { renderHtml: sink });
    updateTickerCollectionStatus(null, { renderHtml: sink });
  });
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 0);
  assert.equal(state.totalFailures, 0);
});

test("multiple retries on the same ticker count cumulatively", () => {
  resetForTests();
  const { sink, last } = makeSink();
  updateTickerCollectionStatus({ ticker: "STMPA.PA", kind: "retry", attempt: 1, run_id: "r1" }, { renderHtml: sink });
  updateTickerCollectionStatus({ ticker: "STMPA.PA", kind: "retry", attempt: 2, run_id: "r1" }, { renderHtml: sink });
  const state = getTickerCollectionStatusState();
  assert.equal(state.totalRetries, 2, "each retry event increments the counter");
  assert.equal(state.tickersRetried.size, 1);
  assert.equal(state.tickersRetried.get("STMPA.PA"), 2);
  const rendered = last();
  assert.ok(rendered.html.includes("2 retries"));
});
