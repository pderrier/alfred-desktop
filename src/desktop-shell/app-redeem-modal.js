/**
 * Activation-code (comp-code) modal — MON-C / MON-D (2026-05-31).
 *
 * Spec: `docs/plans/po-plan-2026-05.md` § "Identity & entitlement
 * durability" → RÉVISION 2026-05-31 (MON-C / MON-D).
 *
 * Replaces the old dead-end mailto CTA. The user obtains a code from Pierre
 * "en échange d'un feedback" and enters it here:
 *   - a code input + "Activer" button → `bridge.redeemCode(code)` →
 *     `POST /redeem` → on success refresh the tier (reuse `installUpgradeFlow`
 *     plumbing via the `alfred://upgrade-activated` event).
 *   - states: success (shows the expiry date), invalid code,
 *     already-used / expired code.
 *   - a FIRST-CLASS, always-visible, COPYABLE email element ("Copier
 *     l'email") so the contact is never hidden behind a mailto that may
 *     fail silently (the MON-D email-UX bug fix).
 *   - on expiry (`mode: "renew"`) the heading + intro pivot to
 *     "Demander un renouvellement de l'accès illimité" with the same
 *     reliable + copyable email.
 *
 * Overlay is ADDITIVE (BINDING `feedback_alfred_overlay_additive`) — it
 * never removes existing UI. Singleton: re-opening reuses the same DOM node.
 * FR-tutoiement throughout.
 *
 * Pure copy/state builders are exported under `__test` for jsdom unit tests
 * without a real bridge.
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

/** Contact email surfaced as a first-class copyable element. */
export const CONTACT_EMAIL = "pierre.derrier@gmail.com";

/**
 * Pure helper: the modal's heading + intro copy for a given mode.
 *   - "activate" (default) — entering a code to unlock unlimited.
 *   - "renew" — access expired (MON-B), asking for a fresh code.
 */
export function buildRedeemIntro(mode = "activate") {
  if (mode === "renew") {
    return {
      title: "Demander un renouvellement de l'accès illimité",
      intro:
        "Ton accès illimité a expiré. Écris à l'auteur pour obtenir un nouveau code, puis saisis-le ci-dessous.",
    };
  }
  return {
    title: "Activer l'accès illimité",
    intro:
      "Obtiens un code d'activation en échange d'un feedback : écris à l'auteur, puis saisis ton code ci-dessous.",
  };
}

/**
 * Pure helper: map a redeem outcome (success payload OR thrown bridge error
 * code) to the success/error message rendered under the input. Centralises
 * the state copy so it is unit-testable without the DOM.
 *
 * @param {Object} result — `{ ok, kind, tier, expiresAt, code }`
 *   - kind "success": `expiresAt` is epoch seconds (or null).
 *   - kind "error":   `code` is the structured bridge error code.
 */
export function buildRedeemResultMessage(result = {}) {
  if (result.kind === "success") {
    const when = formatExpiryFr(result.expiresAt);
    const tail = when ? ` Ton accès est valide jusqu'au ${when}.` : "";
    return {
      tone: "success",
      text: `Code activé — accès illimité débloqué !${tail}`,
    };
  }
  switch (result.code) {
    case "alfred_redeem_invalid":
      return {
        tone: "error",
        text: "Code invalide. Vérifie la saisie (sans espaces) et réessaie.",
      };
    case "alfred_redeem_already_used":
      return {
        tone: "error",
        text: "Ce code a déjà été utilisé ou a expiré. Demande-en un nouveau à l'auteur.",
      };
    case "alfred_api_not_configured":
      return {
        tone: "error",
        text: "Connexion au serveur indisponible — réessaie dans un instant.",
      };
    default:
      return {
        tone: "error",
        text: "Impossible d'activer le code pour l'instant. Réessaie plus tard.",
      };
  }
}

/** Format an epoch-seconds expiry as a French date (JJ/MM/AAAA), or "". */
export function formatExpiryFr(epochSecs) {
  if (!Number.isFinite(epochSecs) || epochSecs <= 0) return "";
  const d = new Date(epochSecs * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const dd = String(d.getDate()).padStart(2, "0");
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  return `${dd}/${mm}/${d.getFullYear()}`;
}

const MODAL_ID = "redeem-code-modal-overlay";

/**
 * Open (or reuse) the activation-code modal.
 *
 * @param {Object} deps
 * @param {Object} deps.bridge — must expose `redeemCode(code)`.
 * @param {Function} [deps.showToast] — `(msg, tone)` for the copy confirmation.
 * @param {Document} [deps.doc] — injectable for tests.
 * @param {Window} [deps.win] — injectable for tests (clipboard + events).
 * @param {string} [deps.mode] — "activate" (default) | "renew".
 */
export function openRedeemModal(deps = {}) {
  const doc = deps.doc || document;
  const win = deps.win || window;
  const bridge = deps.bridge;
  const showToast = deps.showToast || (() => {});
  const mode = deps.mode === "renew" ? "renew" : "activate";

  const { title, intro } = buildRedeemIntro(mode);

  let overlay = doc.getElementById(MODAL_ID);
  if (!overlay) {
    overlay = doc.createElement("div");
    overlay.id = MODAL_ID;
    overlay.className = "error-modal-overlay";
    doc.body.appendChild(overlay);
  }
  overlay.innerHTML = `
    <div class="error-modal-content redeem-modal-content" role="dialog" aria-modal="true" aria-labelledby="redeem-modal-title">
      <h2 id="redeem-modal-title">${escapeHtml(title)}</h2>
      <p class="redeem-modal-intro">${escapeHtml(intro)}</p>
      <div class="redeem-email-row">
        <span class="redeem-email-label">Email de l'auteur :</span>
        <code class="redeem-email-value">${escapeHtml(CONTACT_EMAIL)}</code>
        <button type="button" class="redeem-copy-email-btn">Copier l'email</button>
      </div>
      <label class="redeem-code-label" for="redeem-code-input">Code d'activation</label>
      <input id="redeem-code-input" class="redeem-code-input" type="text"
             autocomplete="off" spellcheck="false" placeholder="XXXX-XXXX-XXXX-XXXX" />
      <p class="redeem-result-message" aria-live="polite"></p>
      <div class="error-modal-actions">
        <button type="button" class="redeem-cancel-btn">Fermer</button>
        <button type="button" class="redeem-activate-btn">Activer</button>
      </div>
    </div>
  `;
  overlay.classList.remove("hidden");

  const input = overlay.querySelector(".redeem-code-input");
  const resultEl = overlay.querySelector(".redeem-result-message");
  const activateBtn = overlay.querySelector(".redeem-activate-btn");
  const cancelBtn = overlay.querySelector(".redeem-cancel-btn");
  const copyBtn = overlay.querySelector(".redeem-copy-email-btn");

  const renderResult = (msg) => {
    resultEl.textContent = msg.text;
    resultEl.dataset.tone = msg.tone;
  };

  const close = () => {
    overlay.classList.add("hidden");
  };

  copyBtn.addEventListener("click", async () => {
    const ok = await copyToClipboard(win, CONTACT_EMAIL);
    showToast(
      ok ? "Email copié dans le presse-papiers." : `Copie indisponible — écris à ${CONTACT_EMAIL}`,
      ok ? "success" : "info"
    );
  });

  cancelBtn.addEventListener("click", close);

  let inFlight = false;
  const submit = async () => {
    if (inFlight) return;
    const code = (input.value || "").trim();
    if (!code) {
      renderResult(buildRedeemResultMessage({ kind: "error", code: "alfred_redeem_invalid" }));
      return;
    }
    inFlight = true;
    activateBtn.disabled = true;
    renderResult({ tone: "pending", text: "Activation en cours…" });
    try {
      const res = await bridge.redeemCode(code);
      const expiresAt = Number(res?.expires_at);
      renderResult(
        buildRedeemResultMessage({ kind: "success", expiresAt, tier: res?.tier })
      );
      input.disabled = true;
      activateBtn.style.display = "none";
      cancelBtn.textContent = "Terminé";
      // Reuse the existing tier-refresh plumbing: the upgrade-activated
      // listener (app.js) refreshes the health pill + busts the home-header
      // debounce so the new tier surfaces immediately.
      win.dispatchEvent(
        new win.CustomEvent("alfred://upgrade-activated", { detail: { via: "redeem", ...res } })
      );
    } catch (err) {
      const code = bridgeErrorCode(err);
      renderResult(buildRedeemResultMessage({ kind: "error", code }));
      activateBtn.disabled = false;
    } finally {
      inFlight = false;
    }
  };

  activateBtn.addEventListener("click", submit);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
  });

  // Focus the input so the user can paste immediately.
  try {
    input.focus();
  } catch {
    /* focus is best-effort in headless tests */
  }

  return { overlay, close, submit };
}

/** Extract a structured code from a thrown bridge error (string or object). */
function bridgeErrorCode(err) {
  if (!err) return "";
  if (typeof err === "string") return err;
  return err.code || err.message || "";
}

/**
 * Copy text to the clipboard, returning a boolean success. Uses the async
 * Clipboard API when available; falls back to a hidden textarea +
 * `execCommand("copy")` for older WebViews. Never throws.
 */
async function copyToClipboard(win, text) {
  try {
    if (win.navigator?.clipboard?.writeText) {
      await win.navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* fall through to legacy path */
  }
  try {
    const doc = win.document;
    const ta = doc.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "absolute";
    ta.style.left = "-9999px";
    doc.body.appendChild(ta);
    ta.select();
    const ok = doc.execCommand && doc.execCommand("copy");
    doc.body.removeChild(ta);
    return !!ok;
  } catch {
    return false;
  }
}

export const __test = {
  buildRedeemIntro,
  buildRedeemResultMessage,
  formatExpiryFr,
  bridgeErrorCode,
  MODAL_ID,
};
