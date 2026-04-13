/**
 * Alfred Overlay — Proactive overlay bus, trigger registry, panel renderer.
 *
 * Creates a non-modal panel in the bottom-right corner that surfaces proactive
 * suggestions, error guidance, and contextual prompts without blocking user
 * interaction. All existing UI (modals, toasts, wizards) remain untouched.
 *
 * Usage:
 *   const overlay = initAlfredOverlay();
 *   registerTrigger({ id, priority, cooldown, contextBuilder, label });
 *   fireTrigger("alfred-welcome");
 */

import { openChatWizard } from "/desktop-shell/app-chat-wizard.js";

// ── Internal state ──────────────────────────────────────────────

/** @type {Map<string, TriggerDef>} */
const triggers = new Map();

/** @type {Map<string, SuppressionEntry>} */
const suppressions = new Map();

/** @type {Map<string, any>} */
const eventStore = new Map();

let panelEl = null;
let headerLabelEl = null;
let bodyEl = null;
let actionsEl = null;
let closeBtn = null;
let autoDismissTimer = null;
let countdownBarEl = null;
let currentTriggerId = null;
let currentPriority = 0;
let panelVisible = false;

// ── DOM construction ────────────────────────────────────────────

function buildPanel() {
  const panel = document.createElement("div");
  panel.id = "alfred-overlay";
  panel.className = "alfred-panel hidden";
  panel.setAttribute("role", "complementary");
  panel.setAttribute("aria-label", "Alfred assistant");

  panel.innerHTML = `
    <div class="alfred-header">
      <img src="/desktop-shell/alfred-icon.png" class="alfred-avatar" alt="" />
      <span class="alfred-persona">Alfred</span>
      <span class="alfred-trigger-label"></span>
      <button class="alfred-close" type="button" aria-label="Dismiss">&times;</button>
    </div>
    <div class="alfred-body"></div>
    <div class="alfred-actions"></div>
    <div class="alfred-countdown-bar"></div>
  `;

  document.body.appendChild(panel);

  panelEl = panel;
  headerLabelEl = panel.querySelector(".alfred-trigger-label");
  bodyEl = panel.querySelector(".alfred-body");
  actionsEl = panel.querySelector(".alfred-actions");
  closeBtn = panel.querySelector(".alfred-close");
  countdownBarEl = panel.querySelector(".alfred-countdown-bar");

  closeBtn.addEventListener("click", () => {
    // Dismiss via X => suppress trigger for session
    if (currentTriggerId) {
      suppressForSession(currentTriggerId);
    }
    dismissPanel();
  });

  return panel;
}

// ── Suppression engine ──────────────────────────────────────────

/**
 * @typedef {Object} SuppressionEntry
 * @property {number} lastFired
 * @property {number} snoozedUntil
 * @property {number} cooldownMs
 * @property {boolean} sessionDismissed
 * @property {boolean} handled
 */

function getOrCreateSuppression(triggerId, cooldownMs) {
  if (!suppressions.has(triggerId)) {
    suppressions.set(triggerId, {
      lastFired: 0,
      snoozedUntil: 0,
      cooldownMs: cooldownMs || 0,
      sessionDismissed: false,
      handled: false
    });
  }
  return suppressions.get(triggerId);
}

function isSuppressed(triggerId, cooldownMs) {
  const entry = getOrCreateSuppression(triggerId, cooldownMs);
  const now = Date.now();

  if (entry.sessionDismissed) return true;
  if (entry.handled) return true;
  if (entry.snoozedUntil > now) return true;
  if (entry.cooldownMs > 0 && entry.lastFired > 0 && (now - entry.lastFired) < entry.cooldownMs) return true;

  return false;
}

function markFired(triggerId, cooldownMs) {
  const entry = getOrCreateSuppression(triggerId, cooldownMs);
  entry.lastFired = Date.now();
}

function suppressForSession(triggerId) {
  const entry = getOrCreateSuppression(triggerId, 0);
  entry.sessionDismissed = true;
}

function markHandled(triggerId) {
  const entry = getOrCreateSuppression(triggerId, 0);
  entry.handled = true;
}

// ── Panel display ───────────────────────────────────────────────

function showPanel(triggerId, context, triggerDef) {
  if (!panelEl) return;

  currentTriggerId = triggerId;
  currentPriority = triggerDef.priority || 0;
  const { initialMessage, actions } = context;

  // Header label
  if (headerLabelEl) {
    headerLabelEl.textContent = triggerDef.label || "";
  }

  // Body
  if (bodyEl) {
    bodyEl.textContent = initialMessage || "";
  }

  // Actions
  if (actionsEl) {
    actionsEl.innerHTML = "";
    if (Array.isArray(actions)) {
      actions.forEach((action) => {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.textContent = action.label || "OK";

        if (action.dismiss) {
          btn.className = "alfred-btn alfred-btn-dismiss";
          btn.addEventListener("click", () => {
            snooze(triggerId, triggerDef.snoozeDuration || 3600000);
            dismissPanel();
          });
        } else if (action.chat) {
          btn.className = "alfred-btn alfred-btn-primary";
          btn.addEventListener("click", () => {
            markHandled(triggerId);
            dismissPanel();
            openChatWizard({
              title: triggerDef.label || "Alfred",
              systemContext: context.systemContext || "",
              initialMessage: context.chatMessage || initialMessage || ""
            });
          });
        } else if (action.callback && typeof action.callback === "function") {
          btn.className = "alfred-btn alfred-btn-secondary";
          btn.addEventListener("click", () => {
            action.callback();
            dismissPanel();
          });
        } else {
          // Default: dismiss
          btn.className = "alfred-btn alfred-btn-dismiss";
          btn.addEventListener("click", () => dismissPanel());
        }

        actionsEl.appendChild(btn);
      });
    }
  }

  // Show panel with animation
  panelEl.classList.remove("hidden");
  // Force reflow so transition fires
  void panelEl.offsetHeight;
  panelEl.classList.add("visible");
  panelVisible = true;

  // Auto-dismiss for non-error triggers (priority < 8) after 90 seconds
  clearAutoDismiss();
  if (currentPriority < 8) {
    startAutoDismiss(90000);
  }
}

function startAutoDismiss(durationMs) {
  if (countdownBarEl) {
    countdownBarEl.style.transition = "none";
    countdownBarEl.style.width = "100%";
    // Force reflow
    void countdownBarEl.offsetHeight;
    countdownBarEl.style.transition = `width ${durationMs}ms linear`;
    countdownBarEl.style.width = "0%";
  }
  autoDismissTimer = setTimeout(() => {
    dismissPanel();
  }, durationMs);
}

function clearAutoDismiss() {
  if (autoDismissTimer) {
    clearTimeout(autoDismissTimer);
    autoDismissTimer = null;
  }
  if (countdownBarEl) {
    countdownBarEl.style.transition = "none";
    countdownBarEl.style.width = "0%";
  }
}

// ── Public API ──────────────────────────────────────────────────

/**
 * Create the Alfred overlay panel DOM and return the overlay API.
 * Call once at app init, after initShellLayout.
 */
export function initAlfredOverlay() {
  buildPanel();

  // Expose a debug hook for console testing (Phase A acceptance criterion)
  window.__alfred = {
    show: (ctx) => {
      if (panelEl) {
        showPanel("__debug__", ctx || { initialMessage: "Alfred debug panel", actions: [{ label: "OK", dismiss: true }] }, { priority: 1, label: "Debug" });
      }
    },
    hide: () => dismissPanel(),
    triggers: () => [...triggers.keys()],
    fire: (id, extra) => fireTrigger(id, extra)
  };

  return {
    registerTrigger,
    fireTrigger,
    dismissPanel,
    snooze,
    isOpen,
    notify
  };
}

/**
 * Register a named trigger.
 * @param {Object} def
 * @param {string} def.id
 * @param {number} def.priority — 1-10
 * @param {number} def.cooldown — ms
 * @param {Function} def.contextBuilder — () => { initialMessage, actions[], systemContext?, chatMessage? }
 * @param {string} def.label — human-readable label for the panel header
 * @param {number} [def.snoozeDuration] — ms to snooze on "Not now" (default 1h)
 * @param {boolean} [def.enabled] — if false, trigger cannot fire (default true)
 */
export function registerTrigger(def) {
  if (!def || !def.id) return;
  triggers.set(def.id, {
    ...def,
    enabled: def.enabled !== false,
    snoozeDuration: def.snoozeDuration || 3600000
  });
}

/**
 * Attempt to fire a trigger. Checks suppression rules before showing.
 * If a higher-priority trigger is already showing, the new one preempts only if higher priority.
 * @param {string} triggerId
 * @param {Object} [extraContext] — merged into contextBuilder output
 */
export function fireTrigger(triggerId, extraContext) {
  const def = triggers.get(triggerId);
  if (!def) return;
  if (!def.enabled) return;

  // Suppression check
  if (isSuppressed(triggerId, def.cooldown)) return;

  // If panel is already open with a higher-priority trigger, don't preempt
  if (panelVisible && currentPriority >= def.priority && currentTriggerId !== triggerId) return;

  // Build context
  let context = {};
  if (typeof def.contextBuilder === "function") {
    context = def.contextBuilder(extraContext) || {};
  }
  if (extraContext) {
    context = { ...context, ...extraContext };
  }

  // Mark as fired
  markFired(triggerId, def.cooldown);

  // If panel is open with lower priority, dismiss it first
  if (panelVisible) {
    clearAutoDismiss();
    panelEl.classList.remove("visible");
  }

  showPanel(triggerId, context, def);
}

/**
 * Hide the Alfred panel.
 */
export function dismissPanel() {
  if (!panelEl) return;
  clearAutoDismiss();
  panelEl.classList.remove("visible");
  panelVisible = false;
  currentTriggerId = null;
  currentPriority = 0;

  // After transition ends, add hidden class
  setTimeout(() => {
    if (!panelVisible && panelEl) {
      panelEl.classList.add("hidden");
    }
  }, 320); // matches CSS transition duration
}

/**
 * Snooze a trigger for durationMs.
 * @param {string} triggerId
 * @param {number} durationMs
 */
export function snooze(triggerId, durationMs) {
  const entry = getOrCreateSuppression(triggerId, 0);
  entry.snoozedUntil = Date.now() + (durationMs || 3600000);
}

/**
 * Check whether the Alfred panel is currently visible.
 * @returns {boolean}
 */
export function isOpen() {
  return panelVisible;
}

/**
 * Store an event for triggers to inspect later.
 * Triggers can call eventStore.get(eventName) to check data.
 * @param {string} eventName
 * @param {*} data
 */
export function notify(eventName, data) {
  eventStore.set(eventName, { data, timestamp: Date.now() });
}
