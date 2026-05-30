/**
 * First-launch legal / liability consent gate.
 *
 * Alfred is a free, personal tool that merely connects a user's accounts to a
 * third-party AI model. To protect the author legally, the user must explicitly
 * acknowledge — with three checkboxes, before the app becomes usable — that:
 *   1. Alfred is only a technical connector; advice comes from the AI, not Alfred.
 *   2. Entrusting financial decisions to an AI carries specific risks.
 *   3. Connecting an AI provider transmits their financial data to that
 *      provider, and Alfred's own enrichment API receives only public market
 *      identifiers/data (never positions, amounts, balances, or identifying info).
 *
 * The acknowledgement is recorded persistently and auditably in
 * `user-preferences.json` under the `legal_consent` key (timestamp + app version
 * + per-checkbox flags), reusing the existing `save_user_preferences_local`
 * merge-only command — no new Rust persistence path.
 *
 * The gate is a deliberate blocking pre-UI screen (z-index above the splash). It
 * is additive: it gates entry without removing or replacing any existing UI. The
 * modal CANNOT be dismissed by clicking the overlay or an X — the only exits are
 * "J'accepte et je continue" (enabled only when all three boxes are checked) and
 * "Refuser et quitter" (closes the app).
 *
 * Bumping LEGAL_CONSENT_VERSION re-prompts every user (terms changed).
 */

/** Single source of truth for the consent schema version. */
export const LEGAL_CONSENT_VERSION = 1;

/** Stable identifiers for the three acknowledgements (persisted + test seam). */
const ACK_KEYS = ["tool_not_advisor", "ai_risk_no_professional", "data_sent_to_provider"];

const LEGAL_COPY = {
  title: "Avant de commencer — à lire attentivement",
  intro:
    "Alfred est une application personnelle et gratuite. Pour l'utiliser, vous devez lire et accepter les points suivants.",
  scrollHint:
    "Faites défiler pour lire l'intégralité de chaque point. Les trois cases doivent être cochées pour continuer.",
  acknowledgements: [
    {
      key: "tool_not_advisor",
      text:
        "Je comprends qu'Alfred est uniquement un outil technique de connexion entre mes comptes personnels et le modèle d'intelligence artificielle que je choisis de connecter. Les analyses, recommandations et conseils d'investissement proviennent exclusivement de ce modèle d'IA tiers, et non d'Alfred ni de son auteur. Je reste seul juge de leur pertinence et de leur validité, et je renonce à tout recours contre cette application — gratuite, personnelle et fournie « en l'état » — en cas de perte de capital ou de tout autre préjudice.",
    },
    {
      key: "ai_risk_no_professional",
      text:
        "Je comprends que confier des décisions financières (ou toute autre décision) à une intelligence artificielle comporte des risques spécifiques : l'IA peut produire des informations inexactes, incomplètes ou trompeuses. Le conseil en investissement relève normalement de professionnels agréés. J'utilise l'IA comme aide à la décision en pleine conscience de ces risques et j'assume l'entière responsabilité de mes décisions. Je confirme être majeur et disposer de la capacité juridique pour accepter ces conditions en mon nom propre.",
    },
    {
      key: "data_sent_to_provider",
      text:
        "Je comprends qu'en connectant un fournisseur d'IA (par exemple OpenAI / ChatGPT), mes données financières personnelles (positions, montants, composition de mon portefeuille) sont transmises à ce fournisseur tiers afin d'être analysées, et qu'elles sont alors soumises aux conditions d'utilisation et à la politique de confidentialité de ce fournisseur, sur lesquels Alfred n'exerce aucun contrôle. Je comprends également qu'Alfred utilise un service d'enrichissement (API) qui ne reçoit que des identifiants et données de marché publics (symboles boursiers, codes ISIN, noms d'émetteurs, indicateurs financiers publics, résumés d'actualité) afin de récupérer cours et informations — jamais mes positions, mes montants, mes soldes, ni aucune information permettant de m'identifier. J'accepte ces transmissions de données.",
    },
  ],
  accept: "J'accepte et je continue",
  refuse: "Refuser et quitter",
};

/**
 * Gate predicate. Returns true when consent must be (re-)collected:
 *   - prefs missing / malformed,
 *   - no `legal_consent.version`,
 *   - recorded version strictly older than the current schema version.
 * A recorded version >= current means the user already accepted current terms.
 *
 * @param {*} prefs            value returned by `get_user_preferences_local`
 * @param {number} currentVersion  the schema version to gate against
 * @returns {boolean}
 */
export function needsLegalConsent(prefs, currentVersion) {
  const recorded = prefs && prefs.legal_consent ? prefs.legal_consent.version : undefined;
  if (typeof recorded !== "number" || Number.isNaN(recorded)) return true;
  return recorded < currentVersion;
}

/**
 * Pure accept-enabled predicate: true only when every required acknowledgement
 * is checked. Drives the "enable accept button" logic in the modal.
 *
 * @param {Record<string, boolean>} checkboxStates  key -> checked
 * @returns {boolean}
 */
export function allAcknowledged(checkboxStates) {
  if (!checkboxStates || typeof checkboxStates !== "object") return false;
  return ACK_KEYS.every((key) => checkboxStates[key] === true);
}

/**
 * Pure builder for the persisted consent record. Extracted so the persistence
 * contract (the exact shape written to user-preferences.json) is pinned by a
 * unit test independent of the live DOM / Tauri layer.
 *
 * @param {number} version   schema version accepted
 * @param {string} appVersion  best-effort app version string
 * @param {string} nowIso    ISO-8601 acceptance timestamp
 * @returns {{legal_consent: object}}
 */
export function buildConsentRecord(version, appVersion, nowIso) {
  return {
    legal_consent: {
      version,
      accepted_at: nowIso,
      app_version: appVersion,
      acknowledgements: {
        tool_not_advisor: true,
        ai_risk_no_professional: true,
        data_sent_to_provider: true,
      },
    },
  };
}

// ── Modal (live DOM) ──────────────────────────────────────────────

function buildModalHtml() {
  const items = LEGAL_COPY.acknowledgements
    .map(
      (ack) => `
        <label class="legal-consent-ack" style="display:flex;gap:0.6rem;align-items:flex-start;padding:0.7rem 0.9rem;border:1px solid rgba(73,100,126,0.3);border-radius:8px;cursor:pointer;line-height:1.45">
          <input type="checkbox" class="legal-consent-checkbox" data-ack="${ack.key}" style="margin-top:0.2rem;flex:0 0 auto;width:1rem;height:1rem;cursor:pointer">
          <span style="font-size:0.85rem;color:var(--sea-text,#e0e8f0)">${ack.text}</span>
        </label>`
    )
    .join("");

  return `
    <div class="modal-card" style="width:min(44rem,calc(100vw - 2rem));max-height:min(86vh,52rem);display:flex;flex-direction:column;padding:0">
      <div style="padding:1.1rem 1.3rem 0.7rem;border-bottom:1px solid rgba(73,100,126,0.3)">
        <h3 style="margin:0;font-size:1.05rem;color:var(--sea-text,#e0e8f0)">${LEGAL_COPY.title}</h3>
        <p style="margin:0.35rem 0 0;font-size:0.8rem;color:var(--sea-muted,#8a9bb0)">${LEGAL_COPY.intro}</p>
        <p style="margin:0.3rem 0 0;font-size:0.75rem;color:var(--sea-muted,#8a9bb0)">${LEGAL_COPY.scrollHint}</p>
      </div>
      <div style="flex:1;overflow-y:auto;padding:1rem 1.3rem;display:flex;flex-direction:column;gap:0.7rem">
        ${items}
      </div>
      <div style="display:flex;gap:0.5rem;padding:0.85rem 1.3rem;border-top:1px solid rgba(73,100,126,0.3);align-items:center;justify-content:flex-end">
        <button class="legal-consent-refuse cmd-btn ghost-btn">${LEGAL_COPY.refuse}</button>
        <button class="legal-consent-accept cmd-btn" disabled>${LEGAL_COPY.accept}</button>
      </div>
    </div>
  `;
}

/**
 * Read the checkbox DOM into the { key: boolean } state allAcknowledged expects.
 */
function readCheckboxStates(overlay) {
  const states = {};
  for (const box of overlay.querySelectorAll(".legal-consent-checkbox")) {
    states[box.dataset.ack] = box.checked === true;
  }
  return states;
}

/**
 * Best-effort: close the application window. Tauri v2 first, then a custom
 * quit command if one exists, otherwise no-op (the gate stays up — there is no
 * silent fallthrough into the app).
 */
async function quitApp() {
  const tauriWindow = globalThis.window?.__TAURI__?.window;
  const current = tauriWindow?.getCurrentWindow?.();
  if (current?.close) {
    await current.close();
    return;
  }
  const invoke = globalThis.window?.__TAURI__?.core?.invoke;
  if (invoke) {
    try {
      await invoke("quit_app_local");
    } catch {
      /* no quit command available — leave the gate up rather than admit entry */
    }
  }
}

/**
 * Show the blocking consent modal.
 * @returns {Promise<boolean>} resolves true only on accept-with-all-checked;
 *   never resolves on refuse (the app is closing instead).
 */
export function openLegalConsentModal() {
  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10001"; // above the splash (#splash-screen)
    overlay.dataset.testid = "legal-consent-modal";
    overlay.innerHTML = buildModalHtml();
    document.body.appendChild(overlay);

    const acceptBtn = overlay.querySelector(".legal-consent-accept");

    function syncAcceptEnabled() {
      const enabled = allAcknowledged(readCheckboxStates(overlay));
      acceptBtn.disabled = !enabled;
      acceptBtn.style.opacity = enabled ? "1" : "0.6";
      acceptBtn.style.cursor = enabled ? "pointer" : "not-allowed";
    }

    for (const box of overlay.querySelectorAll(".legal-consent-checkbox")) {
      box.addEventListener("change", syncAcceptEnabled);
    }

    acceptBtn.addEventListener("click", () => {
      if (acceptBtn.disabled) return;
      overlay.remove();
      resolve(true);
    });

    // Refuse closes the app. It deliberately does NOT resolve — there is no
    // path past the gate without acceptance.
    overlay.querySelector(".legal-consent-refuse").addEventListener("click", () => {
      void quitApp();
    });

    syncAcceptEnabled();
  });
}

// ── Orchestrator ──────────────────────────────────────────────────

/**
 * Best-effort app version for the audit record. Tauri v2 exposes
 * `window.__TAURI__.app.getVersion()`; anything else yields "unknown".
 */
async function resolveAppVersion() {
  try {
    const getVersion = globalThis.window?.__TAURI__?.app?.getVersion;
    if (typeof getVersion === "function") {
      const v = await getVersion();
      if (typeof v === "string" && v) return v;
    }
  } catch {
    /* fall through to unknown */
  }
  return "unknown";
}

/**
 * Enforce the consent gate. Reads prefs, and if consent is needed, blocks on the
 * modal and persists the record on acceptance. Resolves once the gate is
 * satisfied (already consented, or freshly accepted). On refuse the app closes,
 * so this promise never resolves down that branch.
 *
 * @param {{invoke?: Function}} [deps]  injectable Tauri invoke (testability /
 *   web context). Defaults to `window.__TAURI__.core.invoke`.
 */
export async function enforceLegalConsentGate(deps = {}) {
  const invoke = deps.invoke || globalThis.window?.__TAURI__?.core?.invoke;
  if (typeof invoke !== "function") {
    // No Tauri bridge (web / test context). Cannot read or persist prefs and
    // cannot quit the window — do not fabricate a gate that can't be honoured.
    return;
  }

  let prefs;
  try {
    prefs = await invoke("get_user_preferences_local");
  } catch {
    // Reading prefs failed — treat as "needs consent" so we never silently
    // skip the gate, but persist below only if the user actually accepts.
    prefs = null;
  }

  if (!needsLegalConsent(prefs, LEGAL_CONSENT_VERSION)) return;

  const accepted = await openLegalConsentModal();
  if (!accepted) return; // refuse branch closed the window; defensive guard.

  const appVersion = await resolveAppVersion();
  const record = buildConsentRecord(
    LEGAL_CONSENT_VERSION,
    appVersion,
    new Date().toISOString()
  );
  await invoke("save_user_preferences_local", { prefs: record });
}

// ── Test seam ─────────────────────────────────────────────────────
//
// The pure helpers are already top-level exports (importable under
// `node --test` because this module touches no DOM/Tauri at import time).
// `__test` additionally exposes internals that the modal wires together so a
// future change to the checkbox-state contract is caught.
export const __test = {
  ACK_KEYS,
  LEGAL_COPY,
  allAcknowledged,
  buildConsentRecord,
  needsLegalConsent,
  buildModalHtml,
};
