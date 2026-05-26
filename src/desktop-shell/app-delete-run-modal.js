/**
 * Delete-run confirm modal — P1-82 (v0.4.5).
 *
 * A run with poisoned data (bad CSV mapping, garbage prices) contaminates
 * future analyses through accumulated signals and the forwarded LLM
 * synthesis. This modal confirms the user really wants to delete/ban a run
 * before the backend purges its signal_history, recomputes derived line
 * memory, and removes the run's files.
 *
 * Mirrors the structured Cancel / Confirm pattern of app-csv-confirm-modal.js
 * (modal-overlay + modal-card, escapeHtml, a pure `buildModalResult` seam so
 * the resolved shape is unit-testable without driving the live DOM).
 *
 * Returns a Promise that resolves to:
 *   { confirmed: true } when the user clicks "Supprimer",
 *   null               when they cancel / close / click the overlay.
 *
 * FR-tutoiement throughout, per the home/voice convention.
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

/**
 * Open the confirm modal for a run.
 *
 * @param {Object} opts
 * @param {string} opts.runId   — run_id being deleted (shown to the user).
 * @param {string} [opts.label] — optional human label (date / account) for the run.
 * @returns {Promise<{confirmed: true} | null>}
 */
export function openDeleteRunModal({ runId, label } = {}) {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10000";
    overlay.dataset.testid = "delete-run-modal";
    overlay.innerHTML = buildModalHtml({ runId, label });
    document.body.appendChild(overlay);

    let resolved = false;
    function finish(value) {
      if (resolved) return;
      resolved = true;
      overlay.remove();
      resolve(value);
    }

    overlay.querySelector(".delete-run-confirm-btn")?.addEventListener("click", () => finish(buildModalResult(true)));
    overlay.querySelector(".delete-run-cancel-btn")?.addEventListener("click", () => finish(buildModalResult(false)));
    overlay.querySelector(".delete-run-close-btn")?.addEventListener("click", () => finish(buildModalResult(false)));
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) finish(buildModalResult(false));
    });
  });
}

// ── Internal helpers ──────────────────────────────────────────────

function buildModalHtml({ runId, label }) {
  const safeLabel = label ? escapeHtml(String(label)) : "";
  const labelLine = safeLabel
    ? `<p style="margin:0.2rem 0 0;font-size:0.78rem;color:var(--sea-muted,#8a9bb0)">${safeLabel}</p>`
    : "";
  return `
    <div class="modal-card" style="width:min(28rem,calc(100vw - 2rem));display:flex;flex-direction:column;padding:0">
      <div style="display:flex;align-items:flex-start;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
        <div>
          <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">Supprimer ce run ?</h3>
          ${labelLine}
        </div>
        <button class="delete-run-close-btn" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem" title="Fermer">&times;</button>
      </div>
      <div style="padding:1rem 1.2rem;font-size:0.85rem;color:var(--sea-text,#e0e8f0);line-height:1.5">
        <p style="margin:0">Cette analyse sera retirée et ne polluera plus tes futures analyses : ses signaux accumulés sont purgés et sa synthèse ne sera plus reprise.</p>
      </div>
      <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);align-items:center;justify-content:flex-end">
        <button class="delete-run-cancel-btn cmd-btn ghost-btn">Annuler</button>
        <button class="delete-run-confirm-btn cmd-btn danger-btn">Supprimer</button>
      </div>
    </div>
  `;
}

/**
 * Pure shape builder for the modal's resolved value.
 *   - on confirm: `{ confirmed: true }`
 *   - on cancel / close / overlay click: `null`
 *
 * Extracted so the contract is testable without driving the live DOM. The
 * caller checks `if (result?.confirmed)` before invoking the backend.
 */
export function buildModalResult(confirmed) {
  if (!confirmed) return null;
  return { confirmed: true };
}

// ── Test seams (exported for unit tests) ──────────────────────────
//
// The modal renders into the live DOM (unavailable in the Node test
// runner). `buildDeleteRunButton` builds the per-run delete control as a
// detached node so the render test can assert on it without a full sidebar.

/**
 * Build the per-run delete control (the 🗑 icon). Returned detached so
 * shell-layout can append it to a run-entry row and the unit test can
 * assert structure (class, data-run-id, accessible title) in isolation.
 *
 * @param {string} runId
 * @param {(runId: string, ev: Event) => void} [onClick]
 * @returns {HTMLButtonElement}
 */
export function buildDeleteRunButton(runId, onClick) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "run-delete-btn";
  btn.dataset.runId = String(runId || "");
  btn.title = "Supprimer ce run";
  btn.setAttribute("aria-label", "Supprimer ce run");
  btn.textContent = "\u{1F5D1}"; // 🗑
  if (typeof onClick === "function") {
    btn.addEventListener("click", (ev) => {
      // The delete control lives inside the run-entry row — never let the
      // click bubble up and select the run we're about to delete.
      ev.stopPropagation();
      onClick(String(runId || ""), ev);
    });
  }
  return btn;
}

/**
 * Build the post-delete toast message (CR-1, P1-82).
 *
 * Signals are purged synchronously by `delete_run_local`, so we always state
 * that. The narrative synthesis, however, can only be regenerated on the NEXT
 * run — when the backend reports `residual_narrative_warning === true`, the
 * on-disk synthesis still carries the deleted run's reasoning, so we tell the
 * user it will refresh then. FR-tutoiement, one-to-two sentences.
 *
 * Pure (no DOM) so the wording contract is unit-testable.
 *
 * @param {{ residual_narrative_warning?: boolean }} [summary]
 * @returns {string}
 */
export function buildDeleteRunToast(summary = {}) {
  const base = "Run supprimé. Les signaux erronés sont purgés.";
  if (summary && summary.residual_narrative_warning === true) {
    return `${base} La synthèse narrative se rafraîchira au prochain run.`;
  }
  return base;
}

export const __test = {
  buildModalResult,
  buildDeleteRunButton,
  buildDeleteRunToast,
};
