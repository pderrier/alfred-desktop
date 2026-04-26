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
  const { title, systemContext, initialMessage, extractResult, returnHistoryOnClose, onDone, discussionScope, discussionMetadata } = config;

  return new Promise((resolve) => {
    // ── State ───────────────────────────────────────────────
    const history = [];
    let sending = false;
    let resolved = false;

    const jsLog = (msg) => window?.__TAURI__?.core?.invoke?.("js_log_local", { message: msg }).catch(() => {});

    function finish(value) {
      const vtype = Array.isArray(value) ? `array(${value.length})` : value === null ? "null" : typeof value;
      jsLog(`chat-wizard finish: resolved=${resolved} value=${vtype}`);
      if (resolved) {
        jsLog(`chat-wizard finish: SKIPPED — already resolved`);
        return;
      }
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
          jsLog(`chat-wizard: Done button clicked, history.length=${history.length} onDone=${!!onDone}`);
          if (onDone) {
            // Call onDone directly — don't go through finish/resolve/await chain
            cleanup();
            resolved = true;
            onDone(history);
            resolve(null); // resolve promise so await doesn't hang
          } else {
            finish(history);
          }
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

    closeBtn.addEventListener("click", () => { jsLog("chat-wizard: X button clicked"); finish(cancelValue()); });
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) { jsLog("chat-wizard: backdrop clicked"); finish(cancelValue()); }
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

// ── Cash account matching — structured dropdown UI (Item 16) ─────

/**
 * Open a dropdown-based modal for cash account matching.
 * Replaces the verbose LLM text-based chat with a structured UI.
 *
 * @param {Object} opts
 * @param {Array} opts.investmentAccounts — [{ name, connection_id, securities_count, ... }]
 * @param {Array} opts.cashAccounts — [{ name, connection_id, fiats_sum, ... }]
 * @param {Object} [opts.currentMapping] — { investmentName: cashAmount } — heuristic mapping
 * @returns {Promise<Object|null>} — mapping { investmentAccountName: cashAccountName } or null
 */
export function openCashMatchingWizard(opts) {
  const { investmentAccounts, cashAccounts, currentMapping } = opts;

  return new Promise((resolve) => {
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    overlay.style.zIndex = "10000";

    // Build best-guess pre-selections from heuristic mapping
    const heuristicMap = buildHeuristicPreselection(investmentAccounts, cashAccounts, currentMapping);

    let html = `
      <div class="modal-card" style="width:min(36rem,calc(100vw - 2rem));max-height:min(70vh,40rem);display:flex;flex-direction:column;padding:0">
        <div style="display:flex;align-items:center;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
          <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">Link Cash Accounts</h3>
          <button class="cm-close-btn" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem" title="Close">&times;</button>
        </div>
        <div style="flex:1;overflow-y:auto;padding:1rem 1.2rem;display:flex;flex-direction:column;gap:1rem">
    `;

    for (const inv of investmentAccounts) {
      const invName = escapeHtml(inv.name || "");
      const selectId = `cm-select-${invName.replace(/[^a-zA-Z0-9]/g, "_")}`;
      html += `
        <div class="cm-row" style="display:flex;flex-direction:column;gap:0.3rem">
          <label style="font-size:0.85rem;font-weight:600;color:var(--sea-text,#e0e8f0)">For ${invName}, select cash account:</label>
          <select class="cm-select" data-inv-name="${invName}" id="${selectId}" style="padding:0.5rem 0.7rem;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:8px;color:var(--sea-text,#e0e8f0);font-size:0.85rem;font-family:inherit">
            ${cashAccounts.map((c) => {
              const cashLabel = `${escapeHtml(c.name)} (${formatEuro(c.fiats_sum || 0)})`;
              const selected = heuristicMap[inv.name] === c.name ? "selected" : "";
              return `<option value="${escapeHtml(c.name)}" ${selected}>${cashLabel}</option>`;
            }).join("")}
            <option value="__none__"${!heuristicMap[inv.name] ? " selected" : ""}>No cash account</option>
          </select>
        </div>
      `;
    }

    html += `
        </div>
        <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);align-items:center;justify-content:space-between">
          <a href="#" class="cm-help-link" style="font-size:0.78rem;color:var(--sea-muted,#8a9bb0)">Need help? Ask Alfred</a>
          <div style="display:flex;gap:0.5rem">
            <button class="cm-cancel-btn cmd-btn ghost-btn">Cancel</button>
            <button class="cm-confirm-btn cmd-btn">Confirm</button>
          </div>
        </div>
      </div>
    `;

    overlay.innerHTML = html;
    document.body.appendChild(overlay);

    let resolved = false;
    function finish(value) {
      if (resolved) return;
      resolved = true;
      overlay.remove();
      resolve(value);
    }

    // Confirm — build mapping from selects
    overlay.querySelector(".cm-confirm-btn")?.addEventListener("click", () => {
      const mapping = {};
      const selects = overlay.querySelectorAll(".cm-select");
      for (const sel of selects) {
        const invName = sel.dataset.invName;
        const cashName = sel.value;
        if (invName && cashName) {
          mapping[invName] = cashName;
        }
      }
      finish(mapping);
    });

    // Cancel
    overlay.querySelector(".cm-cancel-btn")?.addEventListener("click", () => finish(null));
    overlay.querySelector(".cm-close-btn")?.addEventListener("click", () => finish(null));
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) finish(null);
    });

    // Help link — fall back to LLM chat wizard
    overlay.querySelector(".cm-help-link")?.addEventListener("click", (e) => {
      e.preventDefault();
      finish(null);
      openCashMatchingWizardLlm(opts).then(resolve);
    });
  });
}

/**
 * Build a heuristic pre-selection map: { investmentName: cashName }.
 * Uses index-based mapping from currentMapping amounts to find best match.
 */
function buildHeuristicPreselection(investmentAccounts, cashAccounts, currentMapping) {
  const preselection = {};
  if (!currentMapping) return preselection;
  // currentMapping is { investmentName: cashAmount }
  // Match by amount to find which cash account the heuristic chose
  for (const [invName, cashAmount] of Object.entries(currentMapping)) {
    const bestCash = cashAccounts.find((c) => Math.abs((c.fiats_sum || 0) - cashAmount) < 0.01);
    if (bestCash) {
      preselection[invName] = bestCash.name;
    }
  }
  return preselection;
}

/**
 * LLM-based cash matching wizard — fallback for "Need help? Ask Alfred".
 * Preserves the original chat-based approach as optional.
 */
function openCashMatchingWizardLlm(opts) {
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
    discussionScope: "wizard:cash_matching",
    discussionMetadata: { investmentCount: investmentAccounts.length, cashCount: cashAccounts.length },
    extractResult: (history) => extractCashMapping(history, investmentAccounts, cashAccounts),
  });
}

/**
 * Strip trailing parenthesized content and trim whitespace from an account name.
 * E.g. `"Compte espèce PEA (228.45€)"` → `"Compte espèce PEA"`
 */
function normalizeAccountName(name) {
  return (name || "").replace(/\s*\([^)]*\)\s*$/, "").trim();
}

/**
 * Given a raw key extracted from the LLM, find the best matching canonical account name.
 * Checks exact match first, then normalized match, then substring containment.
 */
function resolveToCanonicalName(rawName, canonicalNames) {
  const normalized = normalizeAccountName(rawName);
  // Exact match
  if (canonicalNames.includes(normalized)) return normalized;
  // Case-insensitive match
  const lower = normalized.toLowerCase();
  const ci = canonicalNames.find((n) => n.toLowerCase() === lower);
  if (ci) return ci;
  // Substring containment — the canonical name is contained in the raw name or vice versa
  const sub = canonicalNames.find(
    (n) => lower.includes(n.toLowerCase()) || n.toLowerCase().includes(lower)
  );
  if (sub) return sub;
  // Fallback: return normalized form (best effort)
  return normalized;
}

/**
 * Parse arrow-separated mapping lines from text.
 * Handles: `->`, `→`, `←`, `<->`, with or without quotes, bullet points, numbered lists.
 * Returns array of [left, right] pairs.
 */
function parseArrowLines(text) {
  const pairs = [];
  const lines = text.split("\n");
  // Match lines with -> or → (with optional leading bullets, numbers, dashes)
  const arrowRe = /^[\s\-*\d.)]*["']?(.+?)["']?\s*(?:->|→|<->|<→)\s*["']?(.+?)["']?\s*$/;
  for (const line of lines) {
    const m = line.match(arrowRe);
    if (m && m[1] && m[2]) {
      pairs.push([m[1].trim(), m[2].trim()]);
    }
  }
  return pairs;
}

/**
 * Parse the conversation to extract the confirmed cash account mapping.
 * Looks for the last assistant message with a MAPPING: block, or falls back
 * to arrow-line scanning, then to user confirmation of the initial mapping.
 */
function extractCashMapping(history, investmentAccounts, cashAccounts) {
  const invNames = (investmentAccounts || []).map((a) => a.name);
  const cashNames = (cashAccounts || []).map((a) => a.name);

  /**
   * Build a normalized mapping from raw pairs, resolving keys/values against
   * canonical investment/cash account names and stripping decorated text.
   */
  function buildNormalizedMapping(pairs) {
    const mapping = {};
    for (const [rawKey, rawVal] of pairs) {
      const key = resolveToCanonicalName(rawKey, invNames);
      const val = resolveToCanonicalName(rawVal, cashNames);
      if (key && val) mapping[key] = val;
    }
    return Object.keys(mapping).length > 0 ? mapping : null;
  }

  // Strategy 1: Look backwards through assistant messages for MAPPING: blocks
  for (let i = history.length - 1; i >= 0; i--) {
    const msg = history[i];
    if (msg.role !== "assistant") continue;
    const mappingMatch = msg.content.match(/MAPPING:\s*\n([\s\S]*?)(?:\n\n|$)/i);
    if (mappingMatch) {
      const pairs = parseArrowLines(mappingMatch[1]);
      const mapping = buildNormalizedMapping(pairs);
      if (mapping) return mapping;
    }
  }

  // Strategy 2: Fallback — scan all assistant messages for any arrow lines
  for (let i = history.length - 1; i >= 0; i--) {
    const msg = history[i];
    if (msg.role !== "assistant") continue;
    const pairs = parseArrowLines(msg.content);
    if (pairs.length > 0) {
      const mapping = buildNormalizedMapping(pairs);
      if (mapping) return mapping;
    }
  }

  // Strategy 3: If the user confirmed the initial mapping, look for confirmation words
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
