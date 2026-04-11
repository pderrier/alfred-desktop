/**
 * Chat wizard modal — reusable LLM-driven conversation UI.
 *
 * Opens a modal with a chat interface. The LLM guides the user through a
 * decision (e.g., matching cash accounts to investment accounts). Returns
 * a Promise that resolves with the extracted result on confirm, or null on
 * cancel.
 *
 * Usage:
 *   const result = await openChatWizard({
 *     title: "Link Cash Accounts",
 *     systemContext: "You are helping the user match cash accounts...",
 *     initialMessage: "I found 2 investment accounts and 2 cash accounts...",
 *     extractResult: (history) => parseMappingFromHistory(history),
 *   });
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

/**
 * @param {Object} config
 * @param {string} config.title — Modal title
 * @param {string} config.systemContext — System prompt for the LLM
 * @param {string} config.initialMessage — First assistant message shown to user
 * @param {Function} [config.extractResult] — (history) => any — parse the final answer from conversation
 * @param {boolean} [config.returnHistoryOnClose] — if true, return history array even on cancel/close (for post-chat LLM synthesis)
 * @returns {Promise<any|null>} — resolves with result on confirm, null on cancel (or history if returnHistoryOnClose)
 */
export function openChatWizard(config) {
  const { title, systemContext, initialMessage, extractResult, returnHistoryOnClose } = config;

  return new Promise((resolve) => {
    // ── State ───────────────────────────────────────────────
    const history = [];
    let sending = false;
    let resolved = false;

    function finish(value) {
      console.log("[chat-wizard] finish called, resolved=", resolved, "value type=", Array.isArray(value) ? `array(${value.length})` : typeof value);
      if (resolved) return;
      resolved = true;
      cleanup();
      resolve(value);
    }

    // ── Build DOM ───────────────────────────────────────────
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10000";

    overlay.innerHTML = `
      <div class="modal-card" style="width:min(40rem,calc(100vw - 2rem));max-height:min(80vh,48rem);display:flex;flex-direction:column;padding:0">
        <div style="display:flex;align-items:center;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
          <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">${escapeHtml(title)}</h3>
          <button class="cw-close-btn" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem" title="Close">&times;</button>
        </div>
        <div class="cw-messages" style="flex:1;overflow-y:auto;padding:1rem 1.2rem;display:flex;flex-direction:column;gap:0.6rem;min-height:12rem"></div>
        <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);align-items:flex-end">
          <textarea class="cw-input" rows="2" placeholder="Type your reply..." style="flex:1;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:8px;color:var(--sea-text,#e0e8f0);padding:0.5rem 0.7rem;font-size:0.85rem;resize:none;font-family:inherit"></textarea>
          <button class="cw-send-btn cmd-btn" style="white-space:nowrap;padding:0.5rem 1rem">Send</button>
        </div>
        <div class="cw-actions" style="display:none;padding:0 1.2rem 1rem;gap:0.5rem;justify-content:flex-end">
          <button class="cw-confirm-btn cmd-btn">Confirm</button>
          <button class="cw-reject-btn cmd-btn ghost-btn">Reject</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    const messagesDiv = overlay.querySelector(".cw-messages");
    const inputEl = overlay.querySelector(".cw-input");
    const sendBtn = overlay.querySelector(".cw-send-btn");
    const closeBtn = overlay.querySelector(".cw-close-btn");
    const confirmBtn = overlay.querySelector(".cw-confirm-btn");
    const rejectBtn = overlay.querySelector(".cw-reject-btn");

    // ── Helpers ─────────────────────────────────────────────

    function addBubble(role, text) {
      const bubble = document.createElement("div");
      const isAssistant = role === "assistant";
      bubble.style.cssText = `
        max-width:85%;
        padding:0.6rem 0.9rem;
        border-radius:12px;
        font-size:0.85rem;
        line-height:1.45;
        white-space:pre-wrap;
        word-break:break-word;
        ${isAssistant
          ? "align-self:flex-start;background:rgba(73,100,126,0.25);color:var(--sea-text,#e0e8f0)"
          : "align-self:flex-end;background:rgba(56,132,244,0.2);color:var(--sea-text,#e0e8f0)"
        }
      `;
      bubble.textContent = text;
      messagesDiv.appendChild(bubble);
      messagesDiv.scrollTop = messagesDiv.scrollHeight;
      return bubble;
    }

    function showConfirmReject() {
      const actionsDiv = overlay.querySelector(".cw-actions");
      if (actionsDiv) actionsDiv.style.display = "flex";
      confirmBtn.style.display = "";
      rejectBtn.style.display = "";
    }

    function showDoneButton() {
      const actionsDiv = overlay.querySelector(".cw-actions");
      if (!actionsDiv) return;
      // Hide confirm/reject, show a single "Done" button
      confirmBtn.style.display = "none";
      rejectBtn.style.display = "none";
      if (!actionsDiv.querySelector(".cw-done-btn")) {
        const doneBtn = document.createElement("button");
        doneBtn.className = "cw-done-btn cmd-btn";
        doneBtn.textContent = "Done \u2014 save insights";
        doneBtn.addEventListener("click", (e) => {
          e.stopPropagation();
          console.log("[chat-wizard] Done clicked, resolved=", resolved, "history.length=", history.length);
          finish(history);
        });
        actionsDiv.appendChild(doneBtn);
      }
      actionsDiv.style.display = "flex";
    }

    function setInputEnabled(enabled) {
      inputEl.disabled = !enabled;
      sendBtn.disabled = !enabled;
      sendBtn.textContent = enabled ? "Send" : "...";
    }

    async function sendMessage() {
      const text = inputEl.value.trim();
      if (!text || sending) return;

      sending = true;
      setInputEnabled(false);
      inputEl.value = "";

      // User bubble
      addBubble("user", text);
      history.push({ role: "user", content: text });

      // Call backend
      try {
        const tauriInvoke = window?.__TAURI__?.core?.invoke;
        if (!tauriInvoke) throw new Error("Tauri not available");

        const result = await tauriInvoke("chat_wizard_send_local", {
          context: systemContext,
          history: history.slice(0, -1), // all prior messages (the latest user msg is passed separately)
          userMessage: text,
        });

        const response = result?.response || "(no response)";
        addBubble("assistant", response);
        history.push({ role: "assistant", content: response });

        // Decision flows (extractResult): show Confirm/Reject
        // Q&A flows: show "Done" button to proceed to save
        if (history.filter((m) => m.role === "assistant").length >= 2) {
          if (extractResult) {
            showConfirmReject();
          } else {
            showDoneButton();
          }
        }
      } catch (err) {
        addBubble("assistant", `Error: ${err?.message || err}`);
      }

      sending = false;
      setInputEnabled(true);
      inputEl.focus();
    }

    function cleanup() {
      overlay.remove();
    }

    // ── Events ──────────────────────────────────────────────

    sendBtn.addEventListener("click", sendMessage);
    inputEl.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        sendMessage();
      }
    });

    const cancelValue = () => returnHistoryOnClose ? history : null;

    closeBtn.addEventListener("click", () => finish(cancelValue()));
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) finish(cancelValue());
    });

    confirmBtn.addEventListener("click", () => {
      const result = extractResult ? extractResult(history) : history;
      finish(result);
    });

    rejectBtn.addEventListener("click", () => finish(cancelValue()));

    // ── Show initial message ────────────────────────────────

    addBubble("assistant", initialMessage);
    history.push({ role: "assistant", content: initialMessage });
    inputEl.focus();
  });
}

// ── Cash account matching helper ──────────────────────────────────

/**
 * Open a chat wizard pre-configured for ambiguous cash account matching.
 *
 * @param {Object} opts
 * @param {Array} opts.investmentAccounts — [{ name, connection_id, securities_count, ... }]
 * @param {Array} opts.cashAccounts — [{ name, connection_id, fiats_sum, ... }]
 * @param {Object} opts.currentMapping — { investmentName: cashAmount } — heuristic mapping to confirm
 * @returns {Promise<Object|null>} — confirmed mapping { investmentAccountName: cashAccountName } or null
 */
export function openCashMatchingWizard(opts) {
  const { investmentAccounts, cashAccounts, currentMapping } = opts;

  const investmentList = investmentAccounts
    .map((a) => `  - "${a.name}" (${a.securities_count || 0} securities, connection ${a.connection_id || "?"})`)
    .join("\n");

  const cashList = cashAccounts
    .map((a) => `  - "${a.name}" (${formatEuro(a.fiats_sum || 0)}, connection ${a.connection_id || "?"})`)
    .join("\n");

  const currentMappingText = currentMapping
    ? Object.entries(currentMapping)
        .map(([inv, cash]) => `  - "${inv}" <-> ${formatEuro(cash)}`)
        .join("\n")
    : "  (none)";

  const systemContext = `You are helping the user match cash accounts to their investment accounts in a portfolio tracker.

The user has multiple investment accounts and multiple cash (espece) accounts from the same bank connection, and the system could not auto-match them with certainty.

Your job:
1. Look at the account names, connection IDs, and amounts
2. Propose a 1-to-1 mapping of each investment account to its cash account
3. Explain your reasoning briefly
4. Ask the user to confirm or correct

Be concise. Use the exact account names in your mapping. Format the mapping clearly, one pair per line.
When proposing a mapping, use this format:
MAPPING:
"Investment Account Name" -> "Cash Account Name"
"Investment Account Name 2" -> "Cash Account Name 2"

Keep responses short — this is a quick confirmation dialog.`;

  const initialMessage = `I found an ambiguous cash account situation that needs your help:

**Investment accounts:**
${investmentList}

**Cash accounts:**
${cashList}

**Current heuristic mapping:**
${currentMappingText}

Does this mapping look correct? You can say "yes" to confirm, or tell me which accounts should be paired differently.`;

  return openChatWizard({
    title: "Link Cash Accounts",
    systemContext,
    initialMessage,
    extractResult: (history) => extractCashMapping(history, investmentAccounts, cashAccounts),
  });
}

/**
 * Parse the conversation to extract the confirmed cash account mapping.
 * Looks for the last assistant message with a MAPPING: block, or falls back
 * to user confirmation of the initial mapping.
 */
function extractCashMapping(history, investmentAccounts, cashAccounts) {
  // Look backwards through assistant messages for MAPPING: blocks
  for (let i = history.length - 1; i >= 0; i--) {
    const msg = history[i];
    if (msg.role !== "assistant") continue;
    const mappingMatch = msg.content.match(/MAPPING:\s*\n([\s\S]*?)(?:\n\n|$)/i);
    if (mappingMatch) {
      const lines = mappingMatch[1].split("\n").filter((l) => l.includes("->"));
      const mapping = {};
      for (const line of lines) {
        const parts = line.split("->").map((s) => s.replace(/"/g, "").trim());
        if (parts.length === 2 && parts[0] && parts[1]) {
          mapping[parts[0]] = parts[1];
        }
      }
      if (Object.keys(mapping).length > 0) return mapping;
    }
  }

  // If the user confirmed the initial mapping, look for confirmation words
  const lastUserMsg = [...history].reverse().find((m) => m.role === "user");
  if (lastUserMsg) {
    const text = lastUserMsg.content.toLowerCase();
    if (text.match(/^(yes|oui|ok|correct|confirm|looks good|that'?s right|d'accord)/)) {
      // Return original heuristic mapping as confirmed — caller should interpret null keys
      return { confirmed: true };
    }
  }

  return null;
}

function formatEuro(n) {
  return new Intl.NumberFormat("fr-FR", { style: "currency", currency: "EUR" }).format(n);
}
