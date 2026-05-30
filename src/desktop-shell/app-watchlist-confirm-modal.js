/**
 * Watchlist confirmation modal — Watchlist Curation v2 (D2/D3/D4, v0.4.x).
 *
 * Unlike the CSV confirm modal (which opens BEFORE the run from the wizard),
 * this modal is event-triggered MID-RUN: the Rust worker pauses after the held
 * positions are analysed, emits `watchlist_confirmation_needed` with the
 * candidate tickers + the current per-account feedback, and blocks on a backend
 * gate for ~45 s. This modal lets the user:
 *   - check/uncheck which candidate tickers to analyse this run (D3 — the
 *     confirmed checklist becomes the persisted curated list),
 *   - add their own ticker(s) via a small inline form,
 *   - edit a free-text per-account directive (D4 — e.g. "ce compte est
 *     ETF/fonds, pas de stock-picking"),
 * then Confirm → `watchlist_confirm_local`, which persists the curated list +
 * feedback and signals the gate so the run proceeds with exactly that set.
 *
 * A countdown reflects the 45 s gate. If the user does nothing the gate times
 * out backend-side and the run proceeds with the candidates as-is; this modal
 * simply closes when the countdown reaches zero (no RPC — the backend already
 * has the candidate list).
 *
 * Mirrors the structured Cancel / Confirm pattern of app-csv-confirm-modal.js.
 * The pure shape builders (`buildConfirmPayload`, `collectChecklist`,
 * `parseAddedTickers`) are exported under `__test` for jsdom unit tests.
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

const tauriInvoke = () => window?.__TAURI__?.core?.invoke;

/**
 * Register the mid-run watchlist confirmation listener. Called once from
 * app-events.js with the active-run accessor so a stale event from a finished
 * run is ignored.
 *
 * @param {Object} deps
 * @param {() => string|null} deps.getActiveRunId
 */
export function initWatchlistConfirmModal({ getActiveRunId } = {}) {
  const listen = window?.__TAURI__?.event?.listen;
  if (typeof listen !== "function") return;
  listen("watchlist_confirmation_needed", (event) => {
    const payload = event?.payload || {};
    // Ignore an event that does not belong to the run the UI is showing.
    if (!getActiveRunId || !getActiveRunId()) return;
    openWatchlistConfirmModal(payload);
  });
}

/**
 * Open the modal for a `watchlist_confirmation_needed` payload.
 * @param {Object} payload — { run_id, account, candidates, feedback, timeout_ms }
 * @returns {Promise<{confirmed: boolean}>}
 */
export function openWatchlistConfirmModal(payload = {}) {
  const runId = String(payload.run_id || "");
  const account = String(payload.account || "");
  const candidates = Array.isArray(payload.candidates) ? payload.candidates : [];
  const feedback = String(payload.feedback || "");
  const timeoutMs = Number.isFinite(payload.timeout_ms) ? payload.timeout_ms : 45000;

  // Guard against a duplicate event opening two modals for the same run.
  if (document.querySelector(`.modal-overlay[data-testid="watchlist-confirm-modal"]`)) {
    return Promise.resolve({ confirmed: false });
  }

  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10001";
    overlay.dataset.testid = "watchlist-confirm-modal";
    overlay.dataset.runId = runId;
    // Stash the original candidate payload so collectChecklist can return full
    // item objects (with isin/secteur/raison) for the checked rows.
    try {
      overlay.dataset.candidates = JSON.stringify(candidates);
    } catch {
      overlay.dataset.candidates = "[]";
    }
    overlay.innerHTML = buildModalHtml({ account, candidates, feedback });
    document.body.appendChild(overlay);

    let resolved = false;
    let countdownTimer = null;
    function finish(value) {
      if (resolved) return;
      resolved = true;
      if (countdownTimer) clearInterval(countdownTimer);
      overlay.remove();
      resolve(value);
    }

    // ── Countdown: reflect the backend gate. On expiry, close silently —
    // the backend times out on its own and proceeds with the candidates. ──
    const deadline = Date.now() + timeoutMs;
    const countdownNode = overlay.querySelector(".wl-countdown");
    function tick() {
      const remaining = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      if (countdownNode) {
        countdownNode.textContent = `${remaining}s`;
      }
      if (remaining <= 0) {
        finish({ confirmed: false, reason: "timeout" });
      }
    }
    tick();
    countdownTimer = setInterval(tick, 1000);

    // ── Add-ticker row toggle ──
    const addBtn = overlay.querySelector(".wl-add-btn");
    const addRow = overlay.querySelector(".wl-add-row");
    addBtn?.addEventListener("click", () => {
      if (addRow) addRow.classList.toggle("hidden");
      addRow?.querySelector(".wl-add-input")?.focus();
    });

    // ── Confirm / Cancel / close ──
    overlay.querySelector(".wl-confirm-btn")?.addEventListener("click", async () => {
      const checklist = collectChecklist(overlay);
      const added = parseAddedTickers(overlay.querySelector(".wl-add-input")?.value || "");
      const fb = String(overlay.querySelector(".wl-feedback-input")?.value || "").trim();
      const submitted = buildConfirmPayload({ runId, account, checklist, added, feedback: fb });
      await callWatchlistConfirm(submitted);
      finish({ confirmed: true });
    });
    overlay.querySelector(".wl-cancel-btn")?.addEventListener("click", () => finish({ confirmed: false, reason: "cancel" }));
    overlay.querySelector(".wl-close-btn")?.addEventListener("click", () => finish({ confirmed: false, reason: "cancel" }));
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) finish({ confirmed: false, reason: "cancel" });
    });
  });
}

// ── HTML builders ──────────────────────────────────────────────────

function buildModalHtml({ account, candidates, feedback }) {
  const rows = candidates.length === 0
    ? `<p style="margin:0;color:var(--sea-muted)">Aucune opportunité proposée pour ce compte. Tu peux en ajouter une ci-dessous.</p>`
    : candidates.map(renderCandidateRow).join("");

  return `
    <div class="modal-card" style="width:min(38rem,calc(100vw - 2rem));max-height:min(80vh,44rem);display:flex;flex-direction:column;padding:0">
      <div style="display:flex;align-items:center;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
        <div>
          <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">Valide ta watchlist</h3>
          <p style="margin:0.2rem 0 0;font-size:0.78rem;color:var(--sea-muted,#8a9bb0)">
            Compte : <strong>${escapeHtml(account || "")}</strong> · ces opportunités seront analysées (Alfred peut en écarter).
          </p>
        </div>
        <button class="wl-close-btn" title="Fermer" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem">&times;</button>
      </div>
      <div style="flex:1;overflow-y:auto;padding:1rem 1.2rem;display:flex;flex-direction:column;gap:0.8rem">
        <div class="wl-candidates" style="display:flex;flex-direction:column;gap:0.45rem">
          ${rows}
        </div>
        <div>
          <button class="wl-add-btn cmd-btn ghost-btn" type="button">+ Ajouter une valeur</button>
          <div class="wl-add-row hidden" style="margin-top:0.5rem">
            <input type="text" class="wl-add-input" placeholder="Ticker(s) — ex: AAPL, MC.PA"
              style="width:100%;padding:0.35rem 0.5rem;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:4px;color:var(--sea-text);font-family:monospace;font-size:0.85rem">
          </div>
        </div>
        <label style="display:flex;flex-direction:column;gap:0.3rem;font-size:0.82rem;color:var(--sea-text,#e0e8f0)">
          Directives pour ce compte (optionnel) :
          <textarea class="wl-feedback-input" rows="2" placeholder="ex: ce compte est ETF/fonds, pas de stock-picking"
            style="width:100%;padding:0.4rem 0.5rem;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:4px;color:var(--sea-text);font-size:0.82rem;resize:vertical">${escapeHtml(feedback)}</textarea>
        </label>
      </div>
      <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);align-items:center;justify-content:space-between">
        <span style="font-size:0.75rem;color:var(--sea-muted,#8a9bb0)">Sans action : analyse auto dans <strong class="wl-countdown">45s</strong></span>
        <span style="display:flex;gap:0.5rem">
          <button class="wl-cancel-btn cmd-btn ghost-btn">Ignorer</button>
          <button class="wl-confirm-btn cmd-btn">Confirmer</button>
        </span>
      </div>
    </div>
  `;
}

function renderCandidateRow(item) {
  const ticker = String(item?.ticker || "?").toUpperCase();
  const nom = String(item?.nom || "");
  const secteur = String(item?.secteur || "");
  const raison = String(item?.raison || "");
  const meta = [secteur, raison].filter(Boolean).join(" · ");
  return `
    <label class="wl-candidate" data-ticker="${escapeHtml(ticker)}"
      style="display:flex;gap:0.55rem;align-items:flex-start;border:1px solid rgba(73,100,126,0.3);border-radius:8px;padding:0.5rem 0.7rem;cursor:pointer">
      <input type="checkbox" class="wl-candidate-check" checked style="margin-top:0.2rem">
      <span style="display:flex;flex-direction:column;gap:0.15rem">
        <span style="font-size:0.9rem"><strong>${escapeHtml(ticker)}</strong>${nom ? ` · ${escapeHtml(nom)}` : ""}</span>
        ${meta ? `<span style="font-size:0.74rem;color:var(--sea-muted,#8a9bb0)">${escapeHtml(meta)}</span>` : ""}
      </span>
    </label>
  `;
}

// ── Pure helpers (exported for tests) ───────────────────────────────

/**
 * Collect the candidate rows the user left CHECKED, returning the full item
 * objects (the candidate payload carries ticker/nom/isin/secteur/raison so the
 * persisted curated list keeps that metadata).
 */
function collectChecklist(overlay) {
  const candidatesByTicker = readCandidateIndex(overlay);
  const out = [];
  for (const row of overlay.querySelectorAll(".wl-candidate")) {
    const check = row.querySelector(".wl-candidate-check");
    if (!check || !check.checked) continue;
    const ticker = String(row.dataset.ticker || "").toUpperCase();
    if (!ticker) continue;
    out.push(candidatesByTicker.get(ticker) || { ticker });
  }
  return out;
}

/**
 * Build a ticker→item index from the original candidate payload stashed on the
 * overlay. Falls back to a minimal `{ticker}` when metadata is unavailable.
 */
function readCandidateIndex(overlay) {
  const map = new Map();
  let parsed = [];
  try {
    parsed = JSON.parse(overlay?.dataset?.candidates || "[]");
  } catch {
    parsed = [];
  }
  for (const item of Array.isArray(parsed) ? parsed : []) {
    const ticker = String(item?.ticker || "").toUpperCase();
    if (ticker) map.set(ticker, item);
  }
  return map;
}

/**
 * Parse a free-text "ticker(s)" input into a list of `{ticker}` objects.
 * Accepts comma/space/semicolon separators, upper-cases, dedupes, drops empties.
 */
function parseAddedTickers(raw) {
  const seen = new Set();
  const out = [];
  for (const tok of String(raw || "").split(/[\s,;]+/)) {
    const t = tok.trim().toUpperCase();
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push({ ticker: t });
  }
  return out;
}

/**
 * Pure shape builder for the `watchlist_confirm_local` RPC arguments. Extracted
 * so the payload contract is testable without driving the live DOM.
 */
function buildConfirmPayload({ runId, account, checklist, added, feedback }) {
  return {
    run_id: String(runId || ""),
    account: String(account || ""),
    confirmed_items: Array.isArray(checklist) ? checklist : [],
    added_tickers: Array.isArray(added) ? added : [],
    feedback: typeof feedback === "string" ? feedback : "",
  };
}

async function callWatchlistConfirm(args) {
  const inv = tauriInvoke();
  if (!inv) return false;
  try {
    await inv("watchlist_confirm_local", args);
    return true;
  } catch {
    return false;
  }
}

export const __test = {
  buildModalHtml,
  renderCandidateRow,
  collectChecklist,
  parseAddedTickers,
  buildConfirmPayload,
};
