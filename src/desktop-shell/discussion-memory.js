import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

const STORAGE_KEY = "alfred_discussion_memory_v1";

function nowIso() {
  return new Date().toISOString();
}

function loadStore() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { threads: [] };
    const parsed = JSON.parse(raw);
    if (!parsed || !Array.isArray(parsed.threads)) return { threads: [] };
    return parsed;
  } catch {
    return { threads: [] };
  }
}

function saveStore(store) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
}

function cleanText(value) {
  return String(value || "").trim();
}

export function saveDiscussionThread({ scope, title, summary, note, metadata }) {
  if (!scope) return null;
  const cleanSummary = cleanText(summary);
  const cleanNote = cleanText(note);
  if (!cleanSummary && !cleanNote) return null;

  const store = loadStore();
  const existing = store.threads.find((t) => t.scope === scope && (t.title || scope) === (title || scope));
  if (existing) {
    if (cleanSummary) existing.summary = cleanSummary;
    if (cleanNote) existing.note = cleanNote;
    existing.updated_at = nowIso();
    existing.metadata = { ...(existing.metadata || {}), ...(metadata || {}) };
    saveStore(store);
    return existing;
  }

  const thread = {
    id: `d_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
    scope,
    title: title || scope,
    created_at: nowIso(),
    updated_at: nowIso(),
    summary: cleanSummary,
    note: cleanNote,
    metadata: metadata || {},
  };
  store.threads.unshift(thread);
  store.threads = store.threads.slice(0, 200);
  saveStore(store);
  return thread;
}

export function getDiscussionThreads(scopePrefix = "") {
  const store = loadStore();
  const prefix = String(scopePrefix || "").trim();
  const threads = prefix
    ? store.threads.filter((t) => String(t.scope || "").startsWith(prefix))
    : store.threads;
  return threads.sort((a, b) => String(b.updated_at || "").localeCompare(String(a.updated_at || "")));
}

export function deleteDiscussionThread(threadId) {
  const store = loadStore();
  store.threads = store.threads.filter((t) => t.id !== threadId);
  saveStore(store);
}

export function updateDiscussionInsight(threadId, field, nextText) {
  const store = loadStore();
  const thread = store.threads.find((t) => t.id === threadId);
  if (!thread) return false;
  const key = field === "note" ? "note" : "summary";
  thread[key] = cleanText(nextText);
  thread.updated_at = nowIso();
  saveStore(store);
  return true;
}

export function removeDiscussionInsight(threadId, field) {
  const store = loadStore();
  const thread = store.threads.find((t) => t.id === threadId);
  if (!thread) return false;
  const key = field === "note" ? "note" : "summary";
  thread[key] = "";
  thread.updated_at = nowIso();
  if (!thread.summary && !thread.note) {
    store.threads = store.threads.filter((t) => t.id !== threadId);
  }
  saveStore(store);
  return true;
}

export function buildDiscussionGuidance(scopePrefix, maxInsights = 8) {
  const threads = getDiscussionThreads(scopePrefix);
  const lines = [];
  for (const thread of threads) {
    if (thread.summary) lines.push(`- Summary: ${thread.summary}`);
    if (thread.note) lines.push(`- User note: ${thread.note}`);
    if (lines.length >= maxInsights) break;
  }
  if (lines.length === 0) return "";
  return `User saved summaries/notes to consider in this analysis:\n${lines.slice(0, maxInsights).join("\n")}`;
}

export function openDiscussionHistoryModal({ scopePrefix = "", title = "Previous discussions", onApplyInsight, onAfterEdit } = {}) {
  const threads = getDiscussionThreads(scopePrefix);
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  overlay.style.zIndex = "10002";

  const rows = threads.length > 0
    ? threads.map((thread) => {
      const summary = cleanText(thread.summary);
      const note = cleanText(thread.note);
      return `
        <article class="discussion-thread" data-thread-id="${escapeHtml(thread.id)}">
          <header>
            <strong>${escapeHtml(thread.title || thread.scope || "Discussion")}</strong>
            <span class="welcome-meta">${escapeHtml(new Date(thread.updated_at || thread.created_at).toLocaleString())}</span>
          </header>
          <ul class="discussion-insight-list">
            <li class="discussion-insight-row" data-thread-id="${escapeHtml(thread.id)}" data-field="summary">
              <span class="discussion-insight-text"><strong>Summary:</strong> ${escapeHtml(summary || "(empty)")}</span>
              <button class="ghost-btn discussion-insight-edit" type="button">Edit</button>
              <button class="ghost-btn discussion-insight-delete" type="button">Delete</button>
              ${onApplyInsight ? '<button class="ghost-btn discussion-insight-apply" type="button">Use</button>' : ''}
            </li>
            <li class="discussion-insight-row" data-thread-id="${escapeHtml(thread.id)}" data-field="note">
              <span class="discussion-insight-text"><strong>Note:</strong> ${escapeHtml(note || "(empty)")}</span>
              <button class="ghost-btn discussion-insight-edit" type="button">Edit</button>
              <button class="ghost-btn discussion-insight-delete" type="button">Delete</button>
              ${onApplyInsight ? '<button class="ghost-btn discussion-insight-apply" type="button">Use</button>' : ''}
            </li>
          </ul>
          <div style="display:flex;justify-content:flex-end"><button class="ghost-btn discussion-thread-delete" type="button">Delete discussion</button></div>
        </article>
      `;
    }).join("")
    : `<p class="welcome-global-loading">No saved summaries yet.</p>`;

  overlay.innerHTML = `
    <div class="modal-card" style="width:min(52rem,calc(100vw - 2rem));max-height:min(82vh,52rem);display:flex;flex-direction:column;">
      <div class="run-wizard-head" style="margin:0 0 0.8rem">
        <h2 style="font-size:1rem">${escapeHtml(title)}</h2>
        <button class="modal-close-btn discussion-close" type="button" aria-label="Close">&times;</button>
      </div>
      <div style="overflow:auto;display:grid;gap:0.6rem">${rows}</div>
    </div>
  `;

  function close() { overlay.remove(); }
  overlay.querySelector(".discussion-close")?.addEventListener("click", close);
  overlay.addEventListener("click", (e) => { if (e.target === overlay) close(); });

  overlay.addEventListener("click", (e) => {
    const target = e.target;
    if (!(target instanceof HTMLElement)) return;
    const row = target.closest(".discussion-insight-row");
    const threadCard = target.closest(".discussion-thread");

    if (target.classList.contains("discussion-thread-delete") && threadCard) {
      deleteDiscussionThread(threadCard.getAttribute("data-thread-id"));
      close();
      onAfterEdit?.();
      openDiscussionHistoryModal({ scopePrefix, title, onApplyInsight, onAfterEdit });
      return;
    }

    if (!row) return;
    const threadId = row.getAttribute("data-thread-id");
    const field = row.getAttribute("data-field") || "summary";
    const textNode = row.querySelector(".discussion-insight-text");
    const currentText = (textNode?.textContent || "").replace(/^Summary:\s*|^Note:\s*/, "");

    if (target.classList.contains("discussion-insight-edit")) {
      const next = window.prompt(`Edit ${field}`, currentText);
      if (next !== null && updateDiscussionInsight(threadId, field, next)) {
        close();
        onAfterEdit?.();
        openDiscussionHistoryModal({ scopePrefix, title, onApplyInsight, onAfterEdit });
      }
    } else if (target.classList.contains("discussion-insight-delete")) {
      if (removeDiscussionInsight(threadId, field)) {
        close();
        onAfterEdit?.();
        openDiscussionHistoryModal({ scopePrefix, title, onApplyInsight, onAfterEdit });
      }
    } else if (target.classList.contains("discussion-insight-apply")) {
      onApplyInsight?.(currentText, { threadId, field });
      close();
    }
  });

  document.body.appendChild(overlay);
}
