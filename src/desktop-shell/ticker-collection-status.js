/**
 * Per-ticker retry/failure status aggregator (P0-7).
 *
 * Consumes `alfred://ticker-collection-event` payloads emitted by the Rust
 * enrichment retry loop (`fetch_technical_snapshot`, worktree v032-A) and
 * surfaces aggregated counters via a small badge in the run-in-progress card.
 *
 * Pure module — no DOM imports at module scope. The caller passes a
 * `renderHtml(html, hidden)` sink that writes into the actual badge slot
 * (`#run-ticker-collection-status`). Tests pass an in-memory sink.
 *
 * Reset trigger: any event carrying a `run_id` different from the previously
 * observed `run_id` clears all state — this handles the "new run starts"
 * boundary regardless of which Rust stage event arrives first. The
 * `resetTickerCollectionStatus()` helper can also be called explicitly from
 * the run lifecycle (e.g. when `alfred://run-stage` reports an early
 * collection stage), guaranteeing the badge clears even if no
 * ticker-collection-event ever fires for the new run.
 *
 * Event payload schema (from Rust):
 *   { ticker: string,
 *     kind: "retry" | "failure" | "success",
 *     attempt?: number,          // for kind=retry
 *     reason?: string,           // human-readable Err string
 *     attempts?: number,         // for kind=failure (total attempts tried)
 *     run_id?: string }
 *
 * Per `feedback_alfred_overlay_additive`: this badge is ADDITIVE — it sits
 * next to the existing pipeline bar and other run-in-progress indicators,
 * never replacing them.
 */

// ── Module-level state ──────────────────────────────────────────────

const state = {
  runId: null,
  // ticker -> retry count
  tickersRetried: new Map(),
  // set of tickers that hit final failure
  tickersFailed: new Set(),
  totalRetries: 0,
  totalFailures: 0,
};

// ── Public API ──────────────────────────────────────────────────────

/**
 * Apply a ticker-collection-event payload to the aggregator and render the
 * badge HTML via the provided sink.
 *
 * @param {object|null} payload - the event payload, see schema above
 * @param {{ renderHtml?: (html: string, hidden: boolean) => void }} [opts]
 */
export function updateTickerCollectionStatus(payload, opts = {}) {
  const render = typeof opts.renderHtml === "function" ? opts.renderHtml : defaultRenderHtml;
  if (!payload || typeof payload !== "object") {
    renderBadge(render);
    return;
  }
  const ticker = typeof payload.ticker === "string" ? payload.ticker.trim() : "";
  const kind = typeof payload.kind === "string" ? payload.kind : "";
  if (!ticker || !kind) {
    renderBadge(render);
    return;
  }

  // New run_id observed -> reset accumulator before recording the event.
  const runId = typeof payload.run_id === "string" && payload.run_id.length > 0
    ? payload.run_id
    : null;
  if (runId && state.runId && runId !== state.runId) {
    clearState();
  }
  if (runId && !state.runId) {
    state.runId = runId;
  }

  if (kind === "retry") {
    const prev = state.tickersRetried.get(ticker) || 0;
    state.tickersRetried.set(ticker, prev + 1);
    state.totalRetries += 1;
  } else if (kind === "failure") {
    if (!state.tickersFailed.has(ticker)) {
      state.tickersFailed.add(ticker);
      state.totalFailures += 1;
    }
  }
  // kind === "success" → recorded as no-op (intentional; success events
  // don't change retry/failure counters, they just signal the loop ended ok).

  renderBadge(render);
}

/**
 * Explicitly clear the aggregator and hide the badge. Called by
 * `app-events.js` when an `alfred://run-stage` event indicates a new run is
 * starting, so the badge clears even if no ticker-collection-event fires
 * yet for this run.
 *
 * @param {{ renderHtml?: (html: string, hidden: boolean) => void }} [opts]
 */
export function resetTickerCollectionStatus(opts = {}) {
  const render = typeof opts.renderHtml === "function" ? opts.renderHtml : defaultRenderHtml;
  clearState();
  renderBadge(render);
}

/**
 * Test seam — exposes a snapshot of the internal state for assertion.
 */
export function getTickerCollectionStatusState() {
  return {
    runId: state.runId,
    totalRetries: state.totalRetries,
    totalFailures: state.totalFailures,
    tickersRetried: new Map(state.tickersRetried),
    tickersFailed: new Set(state.tickersFailed),
  };
}

/**
 * Test-only — force the module back to its initial state. Tests call this in
 * a beforeEach to keep ordering independence (Node test runner runs tests in
 * the same process and module state persists).
 */
export function __resetTickerCollectionStatusForTests() {
  clearState();
}

// ── Internal helpers ────────────────────────────────────────────────

function clearState() {
  state.runId = null;
  state.tickersRetried.clear();
  state.tickersFailed.clear();
  state.totalRetries = 0;
  state.totalFailures = 0;
}

function buildBadgeHtml() {
  const parts = [];
  if (state.totalRetries > 0) {
    parts.push(
      `<span class="ticker-collection-retries">⚠ ${state.totalRetries} retries</span>`
    );
  }
  if (state.totalFailures > 0) {
    parts.push(
      `<span class="ticker-collection-failures">✗ ${state.totalFailures} failed</span>`
    );
  }
  if (parts.length === 0) return "";
  return parts.join(`<span class="ticker-collection-sep">·</span>`);
}

function renderBadge(render) {
  const html = buildBadgeHtml();
  const hidden = html.length === 0;
  render(html, hidden);
}

/**
 * Default sink — writes into `#run-ticker-collection-status`. Safe to call
 * before the DOM is ready (no-op if the slot isn't found).
 */
function defaultRenderHtml(html, hidden) {
  if (typeof document === "undefined") return;
  const node = document.getElementById("run-ticker-collection-status");
  if (!node) return;
  node.innerHTML = html;
  if (hidden) {
    node.classList.add("hidden");
  } else {
    node.classList.remove("hidden");
  }
}
