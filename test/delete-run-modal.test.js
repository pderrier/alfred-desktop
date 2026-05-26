/**
 * Tests for the delete-run confirm modal + delete-button control — P1-82
 * (v0.4.5).
 *
 * The module `apps/alfred-desktop/src/desktop-shell/app-delete-run-modal.js`
 * imports browser-only paths (escapeHtml, document) at the top, so it cannot
 * be imported directly under the Node test runner. Following the same
 * pattern as `csv-confirm-modal.test.js` / `normalize-line-memory.test.js`,
 * we replicate the two pure/structural seams here and exercise:
 *   - `buildModalResult`     — the resolved-shape contract (confirm vs cancel),
 *   - `buildDeleteRunButton` — class / data-run-id / accessible title +
 *     the stopPropagation guard (a delete click must NOT select the run).
 *
 * Source of truth: app-delete-run-modal.js. The replicated functions MUST
 * stay behaviour-identical to the originals; any divergence is a test bug.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated pure logic from app-delete-run-modal.js ──────────────

function buildModalResult(confirmed) {
  if (!confirmed) return null;
  return { confirmed: true };
}

// Replicated post-delete toast builder (CR-1, P1-82). MUST stay
// behaviour-identical to buildDeleteRunToast in app-delete-run-modal.js.
function buildDeleteRunToast(summary = {}) {
  const base = "Run supprimé. Les signaux erronés sont purgés.";
  if (summary && summary.residual_narrative_warning === true) {
    return `${base} La synthèse narrative se rafraîchira au prochain run.`;
  }
  return base;
}

// Minimal element stub mirroring the DOM surface buildDeleteRunButton uses.
function makeElementStub() {
  return {
    type: "",
    className: "",
    dataset: {},
    title: "",
    textContent: "",
    _attrs: {},
    _listeners: {},
    setAttribute(name, value) { this._attrs[name] = value; },
    getAttribute(name) { return this._attrs[name]; },
    addEventListener(name, fn) { this._listeners[name] = fn; },
    dispatch(name, ev) { this._listeners[name]?.(ev); },
  };
}

// Replicated structural builder. The original calls
// document.createElement("button"); here we inject a stub so we can assert
// the structure + the stopPropagation guard without a live DOM.
function buildDeleteRunButton(runId, onClick, createElement) {
  const btn = createElement("button");
  btn.type = "button";
  btn.className = "run-delete-btn";
  btn.dataset.runId = String(runId || "");
  btn.title = "Supprimer ce run";
  btn.setAttribute("aria-label", "Supprimer ce run");
  btn.textContent = "\u{1F5D1}";
  if (typeof onClick === "function") {
    btn.addEventListener("click", (ev) => {
      ev.stopPropagation();
      onClick(String(runId || ""), ev);
    });
  }
  return btn;
}

// ── Tests ──────────────────────────────────────────────────────────

test("run_entry_delete_button_renders", () => {
  const btn = buildDeleteRunButton("run_abc", null, makeElementStub);
  assert.equal(btn.type, "button");
  assert.equal(btn.className, "run-delete-btn");
  assert.equal(btn.dataset.runId, "run_abc");
  assert.equal(btn.title, "Supprimer ce run");
  assert.equal(btn.getAttribute("aria-label"), "Supprimer ce run");
  assert.equal(btn.textContent, "\u{1F5D1}"); // 🗑
});

test("delete button coerces a missing run id to empty string (no 'undefined')", () => {
  const btn = buildDeleteRunButton(undefined, null, makeElementStub);
  assert.equal(btn.dataset.runId, "");
});

test("delete button click stops propagation so the run is not selected", () => {
  let selectedRun = null;     // simulates selectRun side effect
  let deletedRunId = null;
  let propagationStopped = false;

  const btn = buildDeleteRunButton(
    "run_xyz",
    (runId) => { deletedRunId = runId; },
    makeElementStub
  );

  // Simulate the run-entry's own click handler that would fire if the event
  // bubbled — stopPropagation must prevent it.
  const ev = {
    stopPropagation() { propagationStopped = true; },
  };
  // The wrapper would normally call selectRun on bubble; we model "did the
  // event reach the row?" via propagationStopped.
  btn.dispatch("click", ev);
  if (!propagationStopped) selectedRun = "run_xyz";

  assert.equal(propagationStopped, true, "click must stop propagation");
  assert.equal(deletedRunId, "run_xyz", "delete handler still fires with the run id");
  assert.equal(selectedRun, null, "the run must NOT be selected by a delete click");
});

test("post-delete toast names the signal purge and the narrative drain when residual", () => {
  // residual_narrative_warning === true → append the next-run drain sentence.
  const withResidual = buildDeleteRunToast({ residual_narrative_warning: true });
  assert.equal(
    withResidual,
    "Run supprimé. Les signaux erronés sont purgés. La synthèse narrative se rafraîchira au prochain run.",
  );
  assert.match(withResidual, /se rafraîchira au prochain run/);
});

test("post-delete toast omits the narrative-drain sentence when not residual", () => {
  const base = "Run supprimé. Les signaux erronés sont purgés.";
  // No flag, explicit false, and a missing summary all collapse to the base.
  assert.equal(buildDeleteRunToast({}), base);
  assert.equal(buildDeleteRunToast({ residual_narrative_warning: false }), base);
  assert.equal(buildDeleteRunToast(), base);
  // Only the strict boolean true triggers the extra sentence (no truthy coercion).
  assert.equal(buildDeleteRunToast({ residual_narrative_warning: "true" }), base);
  assert.doesNotMatch(buildDeleteRunToast({}), /prochain run/);
});

test("confirm_modal_returns_decision", () => {
  // Confirm path → { confirmed: true }; the app.js callback checks
  // `result?.confirmed` before invoking bridge.deleteRun.
  assert.deepEqual(buildModalResult(true), { confirmed: true });
  // Cancel / close / overlay-click all funnel through buildModalResult(false)
  // → exactly null, which short-circuits the delete.
  assert.equal(buildModalResult(false), null);
  assert.equal(buildModalResult(undefined), null);
});
