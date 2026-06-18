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
    case "alfred_redeem_expired":
      return {
        tone: "error",
        text: "Ce code a expiré. Demande un nouveau code pour réactiver l'accès illimité.",
      };
    case "alfred_redeem_device_limit":
      return {
        tone: "error",
        text: "Ce code est déjà utilisé sur 2 appareils. Écris à l'auteur pour obtenir un nouveau code.",
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

/**
 * Pure helper: normalise a candidate redeemed-code value to a trimmed string,
 * or `""` when absent/blank. Centralised so the "is there a code to show"
 * decision is identical whether the code comes from a fresh redeem response
 * or from the persisted `redeemed_code` preference.
 */
export function normalizeRedeemedCode(code) {
  return typeof code === "string" ? code.trim() : "";
}

/** Stable id of the read-only redeemed-code row (so it can be re-rendered). */
const REDEEMED_CODE_ROW_ID = "redeem-active-code-row";

/**
 * Pure helper: HTML for the "your active code" row.
 *
 * MON-E UX fix (2026-06-18): the app already HOLDS the code, so it must not
 * make the user copy-paste it back into the input. The primary affordance is
 * now a one-click "Réactiver mon code" button that submits the stored code
 * directly. A discreet "Copier" remains for the user who wants the raw value
 * (e.g. to use it on another machine). The input below stays available +
 * pre-filled (handled at the call site) so entering a DIFFERENT code still
 * works — fully additive.
 *
 * Returns "" when there is no code to show (row simply absent).
 */
export function buildRedeemedCodeRowHtml(code) {
  const value = normalizeRedeemedCode(code);
  if (!value) return "";
  return `
      <div class="redeem-active-code-row" id="${REDEEMED_CODE_ROW_ID}">
        <span class="redeem-active-code-label">Ton code d'activation :</span>
        <code class="redeem-active-code-value">${escapeHtml(value)}</code>
        <button type="button" class="redeem-reactivate-code-btn">Réactiver mon code</button>
        <button type="button" class="redeem-copy-code-btn">Copier</button>
      </div>`;
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
 * @param {string} [deps.redeemedCode] — MON-E: a previously-redeemed code
 *   (resolved from the `redeemed_code` preference at the call site). When
 *   present, the modal shows it read-only with a copy affordance. ADDITIVE —
 *   absent/blank means the row simply isn't rendered.
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
      ${buildRedeemedCodeRowHtml(deps.redeemedCode)}
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

  const renderResult = (msg) => {
    resultEl.textContent = msg.text;
    resultEl.dataset.tone = msg.tone;
  };

  const close = () => {
    overlay.classList.add("hidden");
  };

  // Shared copy-button wiring (DRY) — the email row (always present) and the
  // read-only redeemed-code row (present only when a code exists) both copy a
  // value to the clipboard and toast the same success/failure pattern.
  const wireCopyButton = (btn, value, okMsg, failMsg) => {
    if (!btn) return;
    btn.addEventListener("click", async () => {
      const ok = await copyToClipboard(win, value);
      showToast(ok ? okMsg : failMsg, ok ? "success" : "info");
    });
  };

  wireCopyButton(
    overlay.querySelector(".redeem-copy-email-btn"),
    CONTACT_EMAIL,
    "Email copié dans le presse-papiers.",
    `Copie indisponible — écris à ${CONTACT_EMAIL}`
  );

  let inFlight = false;

  /**
   * Submit a code for redemption. `overrideCode` is supplied by the one-click
   * "Réactiver mon code" button (the stored code); otherwise we read whatever
   * is in the input (a fresh / different code the user typed).
   */
  const submit = async (overrideCode) => {
    if (inFlight) return;
    const code = (
      typeof overrideCode === "string" && overrideCode.trim()
        ? overrideCode
        : input.value || ""
    ).trim();
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
      // MON-E: surface the just-redeemed code read-only with the one-click
      // reactivate + copy affordances (Rust persisted it to `redeemed_code`
      // for later opens).
      renderRedeemedCode(code);
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

  // MON-E: the "your active code" row. Present in the initial HTML when
  // `deps.redeemedCode` was passed (a prior code); re-rendered on a fresh
  // successful redeem to reflect the just-entered code. Idempotent —
  // replaces any existing row in place rather than duplicating it, and wires
  // both the one-click reactivate button and the copy button each time.
  const renderRedeemedCode = (code) => {
    const value = normalizeRedeemedCode(code);
    if (!value) return;
    const html = buildRedeemedCodeRowHtml(value);
    const existing = overlay.querySelector(`#${REDEEMED_CODE_ROW_ID}`);
    if (existing) {
      existing.outerHTML = html; // reflect a freshly-redeemed code
    } else {
      // Inject just before the code-input label so it sits with the other
      // identity rows (additive — never disturbs the input/actions).
      const label = overlay.querySelector(".redeem-code-label");
      if (label) label.insertAdjacentHTML("beforebegin", html);
    }
    // One-click reactivate: submit the stored code directly, no copy-paste.
    const reactivateBtn = overlay.querySelector(".redeem-reactivate-code-btn");
    if (reactivateBtn) {
      reactivateBtn.addEventListener("click", () => submit(value));
    }
    wireCopyButton(
      overlay.querySelector(".redeem-copy-code-btn"),
      value,
      "Code copié dans le presse-papiers.",
      "Copie indisponible — sélectionne le code à la main."
    );
  };
  // Render + wire the code row for a code passed in the initial open call.
  renderRedeemedCode(deps.redeemedCode);

  // Pre-fill the input with the stored code so the primary "Activer" button
  // (and Enter) also work without a manual paste — while still letting the
  // user clear/replace it to enter a DIFFERENT code (additive).
  const storedCode = normalizeRedeemedCode(deps.redeemedCode);
  if (storedCode) input.value = storedCode;

  cancelBtn.addEventListener("click", close);

  activateBtn.addEventListener("click", () => submit());
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
  });

  // Focus the input so the user can paste / edit immediately.
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
  normalizeRedeemedCode,
  buildRedeemedCodeRowHtml,
  bridgeErrorCode,
  MODAL_ID,
  REDEEMED_CODE_ROW_ID,
};
