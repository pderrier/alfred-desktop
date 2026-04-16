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

// ── Chat mode state ────────────────────────────────────────────
let chatMode = false;
let chatHistory = [];
let chatSystemContext = "";
let chatMessagesEl = null;
let chatInputEl = null;
let chatSendBtn = null;
let chatExpandLink = null;
let chatSending = false;
/** @type {Function|null} onChatComplete callback from current trigger context */
let currentOnChatComplete = null;

// ── Alfred suggestions preference ─────────────────────────────
/** When true, all triggers with priority < 8 are suppressed. */
let alfredSuggestionsDisabled = false;

// ── Session memory ─────────────────────────────────────────────
/** @type {Array<{triggerId: string, timestamp: number, action: string}>} */
const sessionEvents = [];

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
    <div class="alfred-messages"></div>
    <div class="alfred-input-row">
      <input type="text" class="alfred-chat-input" placeholder="Ask Alfred..." />
      <button class="alfred-send-btn" type="button">Send</button>
    </div>
    <a class="alfred-expand-link">Open full chat</a>
    <div class="alfred-countdown-bar"></div>
  `;

  document.body.appendChild(panel);

  panelEl = panel;
  headerLabelEl = panel.querySelector(".alfred-trigger-label");
  bodyEl = panel.querySelector(".alfred-body");
  actionsEl = panel.querySelector(".alfred-actions");
  closeBtn = panel.querySelector(".alfred-close");
  countdownBarEl = panel.querySelector(".alfred-countdown-bar");
  chatMessagesEl = panel.querySelector(".alfred-messages");
  chatInputEl = panel.querySelector(".alfred-chat-input");
  chatSendBtn = panel.querySelector(".alfred-send-btn");
  chatExpandLink = panel.querySelector(".alfred-expand-link");

  closeBtn.addEventListener("click", () => {
    if (chatMode) {
      recordSessionEvent(currentTriggerId, "chatted");
    }
    // Dismiss via X => suppress trigger for session
    if (currentTriggerId) {
      suppressForSession(currentTriggerId);
    }
    exitChatMode();
    dismissPanel();
  });

  // Chat input: send on Enter
  chatInputEl.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendChatMessage();
    }
  });

  // Send button
  chatSendBtn.addEventListener("click", () => {
    sendChatMessage();
  });

  // Expand link: open full chat wizard
  chatExpandLink.addEventListener("click", (e) => {
    e.preventDefault();
    const triggerDef = currentTriggerId ? triggers.get(currentTriggerId) : null;
    const title = triggerDef?.label || "Alfred";
    const context = chatSystemContext;
    const history = [...chatHistory];
    markHandled(currentTriggerId);
    exitChatMode();
    dismissPanel();
    openChatWizard({ title, systemContext: context, initialMessage: history.length > 0 ? history[0].content : "" });
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

  // Exit any prior chat mode
  exitChatMode();

  currentTriggerId = triggerId;
  currentPriority = triggerDef.priority || 0;
  currentOnChatComplete = typeof context.onChatComplete === "function" ? context.onChatComplete : null;
  const { initialMessage, actions } = context;

  // Record session event: shown
  recordSessionEvent(triggerId, "shown");

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
            recordSessionEvent(triggerId, "dismissed");
            snooze(triggerId, action.snoozeDuration || triggerDef.snoozeDuration || 3600000);
            // Phase C: dismiss actions may also carry a callback (e.g. "Skip" marks onboarding done)
            if (typeof action.callback === "function") {
              try { action.callback(); } catch { /* non-critical */ }
            }
            dismissPanel();
          });
        } else if (action.chat) {
          btn.className = "alfred-btn alfred-btn-primary";
          btn.addEventListener("click", () => {
            // Enter inline chat mode instead of opening full modal
            const sysCtx = buildFullSystemContext(context.systemContext || "", triggerId);
            const firstMsg = context.chatMessage || initialMessage || "";
            enterChatMode(sysCtx, firstMsg, triggerId);
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

// ── Session memory ─────────────────────────────────────────────

function recordSessionEvent(triggerId, action) {
  if (!triggerId) return;
  sessionEvents.push({ triggerId, timestamp: Date.now(), action });
}

/**
 * Build a human-readable summary of what Alfred showed this session.
 * Used as additional context when entering chat mode.
 * @returns {string}
 */
export function getSessionContext() {
  if (sessionEvents.length === 0) return "";
  const lines = [];
  const triggerLabels = new Map();
  for (const [id, def] of triggers) {
    triggerLabels.set(id, def.label || id);
  }
  // Deduplicate — summarize by trigger
  const byTrigger = new Map();
  for (const evt of sessionEvents) {
    if (!byTrigger.has(evt.triggerId)) {
      byTrigger.set(evt.triggerId, []);
    }
    byTrigger.get(evt.triggerId).push(evt);
  }
  for (const [tid, events] of byTrigger) {
    const label = triggerLabels.get(tid) || tid;
    const actions = events.map((e) => e.action);
    const chatted = actions.includes("chatted");
    const dismissed = actions.includes("dismissed");
    if (chatted) {
      lines.push(`Earlier this session, I showed "${label}" and you chatted with me about it.`);
    } else if (dismissed) {
      lines.push(`Earlier this session, I showed "${label}" but you dismissed it.`);
    } else {
      lines.push(`Earlier this session, I showed "${label}".`);
    }
  }
  return lines.join(" ");
}

/**
 * Combine the trigger-specific system context with session memory.
 * @param {string} triggerContext — the trigger's systemContext
 * @param {string} triggerId
 * @returns {string}
 */
function buildFullSystemContext(triggerContext, triggerId) {
  const session = getSessionContext();
  let ctx = triggerContext || "";
  if (session) {
    ctx += "\n\n[Session context] " + session;
  }
  return ctx;
}

// ── Chat mode engine ───────────────────────────────────────────

/**
 * Enter inline chat mode in the Alfred panel.
 * Hides the static body/actions and shows the message area + input.
 * @param {string} systemContext — system prompt for LLM
 * @param {string} firstMessage — initial assistant message
 * @param {string} [triggerId] — the trigger that opened chat mode
 */
export function enterChatMode(systemContext, firstMessage, triggerId) {
  if (!panelEl) return;

  chatMode = true;
  chatHistory = [];
  chatSystemContext = systemContext || "";
  chatSending = false;

  // Stop auto-dismiss — chat mode stays open
  clearAutoDismiss();

  // Switch panel to chat mode
  panelEl.classList.add("alfred-chat-mode");

  // Hide static content
  if (bodyEl) bodyEl.style.display = "none";
  if (actionsEl) actionsEl.style.display = "none";
  if (countdownBarEl) countdownBarEl.style.display = "none";

  // Show chat elements
  if (chatMessagesEl) {
    chatMessagesEl.style.display = "flex";
    chatMessagesEl.innerHTML = "";
  }
  if (chatInputEl) {
    chatInputEl.parentElement.style.display = "flex";
    chatInputEl.value = "";
    chatInputEl.disabled = false;
  }
  if (chatSendBtn) chatSendBtn.disabled = false;
  if (chatExpandLink) chatExpandLink.style.display = "";

  // Add initial assistant message
  if (firstMessage) {
    addChatBubble("assistant", firstMessage);
    chatHistory.push({ role: "assistant", content: firstMessage });
  }

  // Record session event
  recordSessionEvent(triggerId || currentTriggerId, "chatted");

  // Focus the input
  if (chatInputEl) chatInputEl.focus();
}

/**
 * Exit chat mode, restoring the panel to its static notification layout.
 */
function exitChatMode() {
  if (!chatMode) return;

  // Fire onChatComplete callback if there was meaningful chat (at least one user message)
  if (currentOnChatComplete && chatHistory.some((m) => m.role === "user")) {
    try {
      const result = currentOnChatComplete([...chatHistory]);
      // Handle async callbacks (best-effort, non-blocking)
      if (result && typeof result.then === "function") {
        result.catch((err) => console.warn("[Alfred] onChatComplete failed:", err));
      }
    } catch (err) {
      console.warn("[Alfred] onChatComplete failed:", err);
    }
  }
  currentOnChatComplete = null;

  chatMode = false;
  chatHistory = [];
  chatSystemContext = "";
  chatSending = false;

  if (!panelEl) return;
  panelEl.classList.remove("alfred-chat-mode");

  // Restore static content visibility
  if (bodyEl) bodyEl.style.display = "";
  if (actionsEl) actionsEl.style.display = "";
  if (countdownBarEl) countdownBarEl.style.display = "";

  // Hide chat elements
  if (chatMessagesEl) {
    chatMessagesEl.style.display = "none";
    chatMessagesEl.innerHTML = "";
  }
  if (chatInputEl) chatInputEl.parentElement.style.display = "none";
  if (chatExpandLink) chatExpandLink.style.display = "none";
}

/**
 * Add a message bubble to the chat area.
 * @param {"user"|"assistant"} role
 * @param {string} text
 * @returns {HTMLElement}
 */
function addChatBubble(role, text) {
  if (!chatMessagesEl) return null;
  const bubble = document.createElement("div");
  bubble.className = role === "user" ? "alfred-msg-user" : "alfred-msg-assistant";
  bubble.textContent = text;
  chatMessagesEl.appendChild(bubble);
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
  return bubble;
}

/**
 * Show a typing indicator in the chat area.
 * @returns {HTMLElement}
 */
function showTypingIndicator() {
  if (!chatMessagesEl) return null;
  const indicator = document.createElement("div");
  indicator.className = "alfred-msg-assistant alfred-typing";
  indicator.innerHTML = '<span class="dot"></span><span class="dot"></span><span class="dot"></span>';
  chatMessagesEl.appendChild(indicator);
  chatMessagesEl.scrollTop = chatMessagesEl.scrollHeight;
  return indicator;
}

/**
 * Send a message in the mini-chat. Calls chat_wizard_send_local Tauri command.
 */
async function sendChatMessage() {
  if (!chatMode || chatSending) return;
  const text = (chatInputEl?.value || "").trim();
  if (!text) return;

  chatSending = true;
  chatInputEl.value = "";
  if (chatInputEl) chatInputEl.disabled = true;
  if (chatSendBtn) chatSendBtn.disabled = true;

  // User bubble
  addChatBubble("user", text);
  chatHistory.push({ role: "user", content: text });

  // Typing indicator
  const typing = showTypingIndicator();

  try {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) throw new Error("Tauri not available");

    const result = await tauriInvoke("chat_wizard_send_local", {
      context: chatSystemContext,
      history: chatHistory.slice(0, -1), // prior messages (user msg sent separately)
      userMessage: text,
    });

    // Remove typing indicator
    if (typing && typing.parentElement) typing.remove();

    const response = result?.response || "(no response)";
    addChatBubble("assistant", response);
    chatHistory.push({ role: "assistant", content: response });
  } catch (err) {
    // Remove typing indicator
    if (typing && typing.parentElement) typing.remove();
    addChatBubble("assistant", `Error: ${err?.message || err}`);
  }

  chatSending = false;
  if (chatInputEl) chatInputEl.disabled = false;
  if (chatSendBtn) chatSendBtn.disabled = false;
  if (chatInputEl) chatInputEl.focus();
}

// ── Persistent session state (Tauri) ───────────────────────────

/**
 * Save Alfred session state to disk via Tauri command.
 * @param {Object} state
 */
async function saveSessionState(state) {
  try {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) return;
    await tauriInvoke("save_alfred_state_local", { state });
  } catch (err) {
    console.warn("[Alfred] Failed to save session state:", err);
  }
}

/**
 * Load Alfred session state from disk via Tauri command.
 * @returns {Promise<Object>}
 */
async function loadSessionState() {
  try {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) return {};
    const result = await tauriInvoke("load_alfred_state_local");
    return result || {};
  } catch (err) {
    console.warn("[Alfred] Failed to load session state:", err);
    return {};
  }
}

// ── Public API ──────────────────────────────────────────────────

/**
 * Create the Alfred overlay panel DOM and return the overlay API.
 * Call once at app init, after initShellLayout.
 */
export function initAlfredOverlay() {
  buildPanel();

  // Initialize chat elements as hidden (non-chat mode default)
  if (chatMessagesEl) chatMessagesEl.style.display = "none";
  if (chatInputEl) chatInputEl.parentElement.style.display = "none";
  if (chatExpandLink) chatExpandLink.style.display = "none";

  // Load persisted session state (async, non-blocking)
  loadSessionState().then((state) => {
    if (state && Array.isArray(state.sessionEvents)) {
      // Restore prior session events for context continuity
      for (const evt of state.sessionEvents) {
        sessionEvents.push(evt);
      }
    }
  });

  // Expose a debug hook for console testing (Phase A acceptance criterion)
  window.__alfred = {
    show: (ctx) => {
      if (panelEl) {
        showPanel("__debug__", ctx || { initialMessage: "Alfred debug panel", actions: [{ label: "OK", dismiss: true }] }, { priority: 1, label: "Debug" });
      }
    },
    hide: () => dismissPanel(),
    triggers: () => [...triggers.keys()],
    fire: (id, extra) => fireTrigger(id, extra),
    chat: (ctx) => {
      if (panelEl) {
        const sysCtx = ctx?.systemContext || "You are Alfred, a helpful portfolio assistant.";
        const msg = ctx?.initialMessage || "How can I help?";
        showPanel("__debug_chat__", { initialMessage: msg, actions: [{ label: "Talk to Alfred", chat: true }], systemContext: sysCtx, chatMessage: msg }, { priority: 1, label: "Debug Chat" });
      }
    },
    sessionContext: () => getSessionContext(),
    sessionEvents: () => [...sessionEvents]
  };

  return {
    registerTrigger,
    fireTrigger,
    dismissPanel,
    snooze,
    isOpen,
    notify,
    enterChatMode,
    getSessionContext,
    setAlfredSuggestionsEnabled
  };
}

/**
 * Register a named trigger.
 * @param {Object} def
 * @param {string} def.id
 * @param {number} def.priority — 1-10
 * @param {number} def.cooldown — ms
 * @param {Function} def.contextBuilder — (extra) => { initialMessage, actions[], systemContext?, chatMessage? }
 *                                         May be async (return a Promise).
 * @param {string} def.label — human-readable label for the panel header
 * @param {number} [def.snoozeDuration] — ms to snooze on "Not now" (default 1h)
 * @param {boolean} [def.enabled] — if false, trigger cannot fire (default true)
 * @param {string} [def.autoFireOn] — event name that triggers automatic firing via notify()
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
 * contextBuilder may be async (e.g. for Tauri command calls) — handled transparently.
 * When the user has disabled "Alfred suggestions" in settings, triggers with priority < 8
 * are silently suppressed.
 * @param {string} triggerId
 * @param {Object} [extraContext] — merged into contextBuilder output
 */
export function fireTrigger(triggerId, extraContext) {
  const def = triggers.get(triggerId);
  if (!def) return;
  if (!def.enabled) return;

  // Phase C: respect the "Alfred suggestions" user preference
  // Priority >= 8 triggers (errors, critical) bypass the toggle
  if (def.priority < 8 && alfredSuggestionsDisabled) return;

  // Suppression check
  if (isSuppressed(triggerId, def.cooldown)) return;

  // If panel is already open with a higher-priority trigger, don't preempt
  if (panelVisible && currentPriority >= def.priority && currentTriggerId !== triggerId) return;

  // Build context — contextBuilder may return a Promise (async)
  let contextResult = {};
  if (typeof def.contextBuilder === "function") {
    contextResult = def.contextBuilder(extraContext);
  }

  // Handle async contextBuilder transparently
  if (contextResult && typeof contextResult.then === "function") {
    // Mark as fired early to prevent duplicate fires while awaiting
    markFired(triggerId, def.cooldown);
    contextResult.then((resolved) => {
      // Phase C: contextBuilder may return null to signal "nothing to show"
      if (resolved === null) return;
      let context = resolved || {};
      if (extraContext) {
        context = { ...context, ...extraContext };
      }
      // Re-check suppression — state may have changed while awaiting
      if (panelVisible && currentPriority >= def.priority && currentTriggerId !== triggerId) return;
      if (panelVisible) {
        clearAutoDismiss();
        panelEl.classList.remove("visible");
      }
      showPanel(triggerId, context, def);
    }).catch((err) => {
      console.warn(`[Alfred] contextBuilder for "${triggerId}" failed:`, err);
    });
    return;
  }

  // Synchronous path
  if (contextResult === null) return; // contextBuilder signals "nothing to show"
  let context = contextResult || {};
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
  exitChatMode();
  panelEl.classList.remove("visible");
  panelVisible = false;
  currentTriggerId = null;
  currentPriority = 0;
  currentOnChatComplete = null;

  // Persist session state (non-blocking)
  if (sessionEvents.length > 0) {
    saveSessionState({ sessionEvents, savedAt: new Date().toISOString() });
  }

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
 * Enable or disable Alfred proactive suggestions.
 * When disabled, triggers with priority < 8 are suppressed.
 * @param {boolean} enabled
 */
export function setAlfredSuggestionsEnabled(enabled) {
  alfredSuggestionsDisabled = !enabled;
}

/**
 * Store an event and auto-fire any triggers registered with a matching autoFireOn.
 *
 * Phase A stored events passively. Phase B adds reactive dispatch: when a trigger
 * declares `autoFireOn: "run-failed"`, calling `notify("run-failed", data)` will
 * automatically attempt to fire that trigger with the event data as extra context.
 *
 * @param {string} eventName
 * @param {*} data
 */
export function notify(eventName, data) {
  eventStore.set(eventName, { data, timestamp: Date.now() });

  // Phase B: reactive dispatch — fire triggers whose autoFireOn matches this event
  for (const [, def] of triggers) {
    if (def.autoFireOn === eventName && def.enabled) {
      fireTrigger(def.id, data);
    }
  }
}
