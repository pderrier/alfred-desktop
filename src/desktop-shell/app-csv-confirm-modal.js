/**
 * CSV import confirm modal — P0-77b (v0.4.4).
 *
 * Replaces the broken chat-wizard CSV-confirmation flow with a structured
 * Cancel / Confirm modal. For each position the parser could not resolve
 * (generic ticker, malformed ISIN, parts-sociales pattern), the user
 * picks one of:
 *   - "Accept historical suggestion" (when Alfred has analysed the same
 *     position name on the same account in a past run).
 *   - "Marker comme cash-equivalent" (parts-sociales pattern).
 *   - "Saisir un autre ISIN" (manual input, live-validated via the Rust
 *     ISO-6166 + Luhn helper).
 *   - "Skip — analyser sans enrichissement".
 *
 * Returns:
 *   { confirmed: true, corrections: [{ position_index, action, isin? }, ...] }
 *   when the user clicks "Confirmer l'analyse", or `null` when they cancel
 *   / close the modal. The corrections array is sparse — only entries
 *   that need backend action are emitted.
 *
 * Wired into app-wizard.js: when the preview returns at least one row in
 * `positions_needing_review`, the modal opens BEFORE runAnalysis. The
 * confirmed corrections are funnelled through
 * `csv_import_apply_corrections_local` to build the snapshot that's
 * passed as `uploaded_snapshot` to the analysis pipeline.
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

const tauriInvoke = () => window?.__TAURI__?.core?.invoke;

/**
 * @param {Object} opts
 * @param {Object} opts.previewPayload — return value of preview_csv_import_local
 * @param {string} opts.account        — display label for the import target
 * @returns {Promise<{confirmed: true, corrections: Array<Object>} | null>}
 */
export function openCsvConfirmModal({ previewPayload, account } = {}) {
  const preview = previewPayload || {};
  const reviewItems = Array.isArray(preview.positions_needing_review)
    ? preview.positions_needing_review
    : [];
  const positions = Array.isArray(preview.positions) ? preview.positions : [];
  const totalCount = positions.length;
  const reviewCount = reviewItems.length;
  const cleanCount = Math.max(0, totalCount - reviewCount);
  const detectedFormat = preview.detected_format || "unknown";
  const detectedBroker = preview.detected_broker || "AI-detected";

  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10000";
    overlay.dataset.testid = "csv-confirm-modal";

    overlay.innerHTML = buildModalHtml({
      account,
      detectedFormat,
      detectedBroker,
      totalCount,
      reviewItems,
      cleanCount,
    });
    document.body.appendChild(overlay);

    let resolved = false;
    function finish(value) {
      if (resolved) return;
      resolved = true;
      overlay.remove();
      resolve(value);
    }

    // ── Wire per-row radio + manual-ISIN inputs ─────────────────────
    for (const row of overlay.querySelectorAll(".csv-row")) {
      const idx = Number(row.dataset.positionIndex);
      const manualInput = row.querySelector(".csv-manual-isin");
      const radios = row.querySelectorAll('input[type="radio"]');

      // When the manual-ISIN radio is chosen, focus the input.
      for (const radio of radios) {
        radio.addEventListener("change", () => {
          const isManual = radio.value === "manual_isin" && radio.checked;
          if (manualInput) {
            manualInput.disabled = !(radio.value === "manual_isin" && radio.checked);
            if (isManual) manualInput.focus();
          }
          updateConfirmEnabled(overlay);
        });
      }

      // Live-validate the manual ISIN as the user types.
      manualInput?.addEventListener("input", async () => {
        const raw = manualInput.value.trim().toUpperCase();
        manualInput.value = raw;
        const status = row.querySelector(".csv-isin-status");
        if (!raw) {
          if (status) {
            status.textContent = "";
            status.dataset.valid = "";
          }
          updateConfirmEnabled(overlay);
          return;
        }
        const ok = await callValidateIsin(raw);
        if (status) {
          status.textContent = ok ? "ISIN valide" : "ISIN non valide";
          status.dataset.valid = ok ? "true" : "false";
          status.style.color = ok ? "var(--sea-success, #5cb85c)" : "var(--sea-danger, #d9534f)";
        }
        updateConfirmEnabled(overlay);
      });

      // Make the whole position_index/idx visible via dataset for tests.
      row.dataset.positionIndex = String(idx);
    }

    overlay.querySelector(".csv-confirm-btn")?.addEventListener("click", () => {
      const corrections = collectCorrections(overlay);
      finish(buildModalResult(true, corrections));
    });

    overlay.querySelector(".csv-cancel-btn")?.addEventListener("click", () => finish(buildModalResult(false)));
    overlay.querySelector(".csv-close-btn")?.addEventListener("click", () => finish(buildModalResult(false)));
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) finish(buildModalResult(false));
    });

    // Initial confirm-enabled state.
    updateConfirmEnabled(overlay);
  });
}

// ── Internal helpers ──────────────────────────────────────────────

function buildModalHtml({ account, detectedFormat, detectedBroker, totalCount, reviewItems, cleanCount }) {
  const summary = reviewItems.length === 0
    ? `<p style="margin:0;color:var(--sea-muted)">Les ${totalCount} position(s) sont propres — clique sur <em>Confirmer l'analyse</em> pour lancer l'analyse.</p>`
    : `<p style="margin:0;color:var(--sea-muted)">${escapeHtml(String(reviewItems.length))} ligne(s) sur ${escapeHtml(String(totalCount))} ont besoin de ton arbitrage. Les ${escapeHtml(String(cleanCount))} autres lignes sont propres et seront analysées telles quelles.</p>`;

  const rows = reviewItems.map(renderReviewRow).join("");

  return `
    <div class="modal-card" style="width:min(40rem,calc(100vw - 2rem));max-height:min(80vh,46rem);display:flex;flex-direction:column;padding:0">
      <div style="display:flex;align-items:center;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
        <div>
          <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">Confirme l'import CSV</h3>
          <p style="margin:0.2rem 0 0;font-size:0.78rem;color:var(--sea-muted,#8a9bb0)">
            Format : <strong>${escapeHtml(detectedBroker)}</strong> (${escapeHtml(detectedFormat)}) ·
            Compte : <strong>${escapeHtml(account || "")}</strong>
          </p>
        </div>
        <button class="csv-close-btn" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem" title="Fermer">&times;</button>
      </div>
      <div style="flex:1;overflow-y:auto;padding:1rem 1.2rem;display:flex;flex-direction:column;gap:0.8rem">
        ${summary}
        ${rows}
      </div>
      <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);align-items:center;justify-content:flex-end">
        <button class="csv-cancel-btn cmd-btn ghost-btn">Annuler</button>
        <button class="csv-confirm-btn cmd-btn">Confirmer l'analyse</button>
      </div>
    </div>
  `;
}

function renderReviewRow(item) {
  const idx = Number(item.position_index ?? 0);
  const ticker = String(item.ticker || "?");
  const nom = String(item.nom || "?");
  const isin = String(item.isin || "");
  const suggestion = item.historical_suggestion || null;
  const isParts = item.is_parts_pattern === true;
  const issue = String(item.issue || "");

  const issueLabel = issue === "malformed_isin"
    ? "ISIN mal formé"
    : issue === "generic"
      ? "ticker générique"
      : issue === "missing_ticker"
        ? "ticker absent"
        : "à arbitrer";

  // Default radio:
  //   1. accept_suggestion if a historical match is available,
  //   2. cash_equivalent if it's a parts-sociales pattern,
  //   3. skip otherwise.
  const defaultAction = suggestion
    ? "accept_suggestion"
    : isParts
      ? "cash_equivalent"
      : "skip";

  const optionAccept = suggestion ? `
    <label class="csv-option">
      <input type="radio" name="csv-action-${idx}" value="accept_suggestion"${defaultAction === "accept_suggestion" ? " checked" : ""}>
      Accepter <strong>${escapeHtml(suggestion.ticker || "")}</strong> / <code>${escapeHtml(suggestion.isin || "")}</code>
      <span style="color:var(--sea-muted);font-size:0.75rem">(depuis ${escapeHtml(suggestion.run_date || "un run précédent")})</span>
    </label>
  ` : "";

  const optionCash = isParts ? `
    <label class="csv-option">
      <input type="radio" name="csv-action-${idx}" value="cash_equivalent"${defaultAction === "cash_equivalent" ? " checked" : ""}>
      Marquer comme parts sociales (cash-equivalent — pas d'enrichissement marché)
    </label>
  ` : "";

  const optionManual = `
    <label class="csv-option">
      <input type="radio" name="csv-action-${idx}" value="manual_isin">
      Saisir un autre ISIN :
      <input type="text" class="csv-manual-isin" maxlength="12" placeholder="FR0000000000" disabled
        style="margin-left:0.4rem;padding:0.2rem 0.4rem;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:4px;color:var(--sea-text);font-family:monospace;font-size:0.85rem;width:8rem">
      <span class="csv-isin-status" data-valid="" style="margin-left:0.5rem;font-size:0.75rem"></span>
    </label>
  `;

  const optionSkip = `
    <label class="csv-option">
      <input type="radio" name="csv-action-${idx}" value="skip"${defaultAction === "skip" ? " checked" : ""}>
      Skip — analyser sans enrichissement marché
    </label>
  `;

  return `
    <div class="csv-row" data-position-index="${idx}"
      style="border:1px solid rgba(73,100,126,0.3);border-radius:8px;padding:0.7rem 0.9rem;display:flex;flex-direction:column;gap:0.4rem">
      <div style="display:flex;justify-content:space-between;align-items:baseline;gap:0.5rem">
        <strong style="font-size:0.92rem">${escapeHtml(nom)}</strong>
        <span style="font-size:0.7rem;color:var(--sea-warn,#d9a833);text-transform:uppercase;letter-spacing:0.04em">${escapeHtml(issueLabel)}</span>
      </div>
      <p style="margin:0;font-size:0.78rem;color:var(--sea-muted,#8a9bb0)">
        ticker CSV : <code>${escapeHtml(ticker)}</code> · ISIN CSV : <code>${escapeHtml(isin || "—")}</code>
      </p>
      <div style="display:flex;flex-direction:column;gap:0.25rem;font-size:0.85rem;color:var(--sea-text,#e0e8f0)">
        ${optionAccept}
        ${optionCash}
        ${optionManual}
        ${optionSkip}
      </div>
    </div>
  `;
}

/**
 * Pure shape builder for the modal's resolved value.
 *   - on confirm: `{ confirmed: true, corrections: [...] }`
 *   - on cancel / close / overlay click: `null`
 *
 * Extracted so the contract is testable without driving the live DOM —
 * the click handlers above call this directly. Any future change to the
 * resolved shape (e.g. adding metadata) must go through this helper so
 * the test seam catches it.
 */
function buildModalResult(confirmed, corrections) {
  if (!confirmed) return null;
  return { confirmed: true, corrections: Array.isArray(corrections) ? corrections : [] };
}

/**
 * Walk the modal and produce the corrections array. Only rows whose
 * selected action is non-default ("skip" with no other option present)
 * still emit a correction so the backend always knows the user's choice
 * for each reviewed row.
 */
function collectCorrections(overlay) {
  const out = [];
  for (const row of overlay.querySelectorAll(".csv-row")) {
    const idx = Number(row.dataset.positionIndex);
    const radio = row.querySelector('input[type="radio"]:checked');
    if (!radio) continue;
    const action = radio.value;
    if (action === "manual_isin") {
      const isin = row.querySelector(".csv-manual-isin")?.value?.trim()?.toUpperCase() || "";
      out.push({ position_index: idx, action, isin });
    } else {
      out.push({ position_index: idx, action });
    }
  }
  return out;
}

/**
 * Confirm button is enabled unless one of the rows has a "manual_isin"
 * radio selected with an empty or invalid ISIN — in that case the user
 * picked an option but didn't supply the data, which is a UX dead-end.
 */
function updateConfirmEnabled(overlay) {
  const confirmBtn = overlay.querySelector(".csv-confirm-btn");
  if (!confirmBtn) return;
  let blocked = false;
  for (const row of overlay.querySelectorAll(".csv-row")) {
    const radio = row.querySelector('input[type="radio"]:checked');
    if (!radio) continue;
    if (radio.value !== "manual_isin") continue;
    const input = row.querySelector(".csv-manual-isin");
    const status = row.querySelector(".csv-isin-status");
    const value = (input?.value || "").trim();
    if (!value) { blocked = true; break; }
    if (status?.dataset?.valid === "false") { blocked = true; break; }
    if (status?.dataset?.valid !== "true") { blocked = true; break; }
  }
  confirmBtn.disabled = blocked;
  confirmBtn.style.opacity = blocked ? "0.6" : "1";
  confirmBtn.style.cursor = blocked ? "not-allowed" : "pointer";
}

async function callValidateIsin(isin) {
  const inv = tauriInvoke();
  if (!inv) return false;
  try {
    return await inv("validate_isin_local", { isin });
  } catch {
    return false;
  }
}

// ── Test seams (exported for unit tests) ──────────────────────────
//
// The modal renders into the live DOM and runs Tauri RPC for ISIN
// validation; both are unavailable in the Node test runner. Exporting
// these pure helpers gives the test suite full control over the
// branching logic without mocking the modal lifecycle.

export const __test = {
  renderReviewRow,
  collectCorrections,
  buildModalResult,
  defaultActionFor(item) {
    if (!item) return "skip";
    if (item.historical_suggestion) return "accept_suggestion";
    if (item.is_parts_pattern === true) return "cash_equivalent";
    return "skip";
  },
};
