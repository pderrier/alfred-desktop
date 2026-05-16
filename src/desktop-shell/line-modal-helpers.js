/**
 * Pure helpers for the line inspect modal (`app-line-modal.js`).
 *
 * Extracted so unit tests can exercise rendering logic without spinning up
 * the full DOM / Tauri bridge. Each helper takes its DOM nodes and the
 * dependencies it needs via parameters — no module-scope DOM lookups.
 *
 * This file deliberately has NO imports — it must be loadable from node
 * test harnesses via plain ESM (`import { ... } from "..."`) without the
 * Tauri / browser absolute-path resolver. Inline `escapeHtml` mirrors
 * `ui-display-utils.js::escapeHtml` (same character set).
 */

/**
 * Render the P2-6 "données partielles" badge next to the recommendation
 * signal in the line modal KPI strip.
 *
 * Contract pinned by `apps/desktop-ui/test/data-quality-badge.test.js`:
 * - `rec.data_quality === "fundamentals_missing"` → badge visible with
 *   the warning glyph + "données partielles" label.
 * - Any other value (null, undefined, missing key, empty string) → badge
 *   hidden (additive UI only — never replaces the signal text).
 *
 * The badge is placed inside the existing `lm-kpi-signal` strong tag's
 * containing `<span class="lm-kpi">` so layout stays unchanged. A
 * pre-existing badge from a prior open is always cleared first
 * (idempotent).
 *
 * @param {object|null} rec       Recommendation payload (may carry data_quality).
 * @param {HTMLElement|null} signalKpiNode  The `<span class="lm-kpi">`
 *        wrapping `#lm-kpi-signal`. Pass `null` to no-op.
 * @returns {boolean} `true` when the badge is rendered, `false` otherwise.
 */
export function updateDataQualityBadge(rec, signalKpiNode) {
  if (!signalKpiNode) return false;

  // Always strip the previous badge first — idempotent open/close.
  const existing = signalKpiNode.querySelector(".lm-data-quality-badge");
  if (existing && existing.parentNode === signalKpiNode) {
    signalKpiNode.removeChild(existing);
  }

  const flag = rec && typeof rec === "object" ? rec.data_quality : null;
  if (flag !== "fundamentals_missing") {
    return false;
  }

  const badge = document.createElement("span");
  badge.className = "lm-data-quality-badge";
  badge.setAttribute(
    "title",
    "Donnees fondamentales partielles — le signal s'appuie sur la memoire et la technique."
  );
  badge.setAttribute("aria-label", "Donnees partielles");
  // Warning glyph + label — additive next to the existing signal value.
  badge.innerHTML = `<span class="lm-data-quality-icon" aria-hidden="true">⚠</span> donnees partielles`;
  signalKpiNode.appendChild(badge);
  return true;
}

function escapeHtml(text) {
  return String(text || "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * Render the "Previous Discussions" sub-section inside the Notes & Discussions
 * panel of the line modal.
 *
 * Behaviour pinned by tests in
 * `apps/desktop-ui/test/line-modal-notes-section.test.js`:
 * - Empty thread list → block hidden, no list items rendered.
 * - 3 threads → 3 `<li>` rendered, ticker scope (`line:{ticker}`) is the
 *   filter, top-5 ordering by `updated_at desc` is delegated to
 *   `getDiscussionThreads` which already sorts.
 * - Click a thread → `openDiscussionHistoryModal({ scopePrefix })` is called
 *   so the user lands inside the same modal the "Previous Discussion" button
 *   opens (single source of truth for thread management).
 * - >5 threads → only the first 5 are rendered (top-5 by recency).
 *
 * Returns the number of threads rendered (useful for the parent caller to
 * decide whether to surface the outer `<details>` section).
 *
 * @param {string} ticker        Ticker symbol; used to compose `scopePrefix`.
 * @param {HTMLElement|null} blockNode  Outer block element (toggled hidden/shown).
 * @param {HTMLElement|null} listNode   `<ul>` to receive `<li>` children.
 * @param {object} deps
 * @param {(scopePrefix: string) => Array<object>} deps.getThreads
 *        Resolves the threads for this ticker (already sorted desc by
 *        `updated_at`).
 * @param {(opts: object) => void} deps.openHistoryModal
 *        Called when a `<li>` is clicked.
 * @param {number} [deps.limit=5]   Max threads to render.
 * @returns {number} count of threads actually rendered.
 */
export function renderDiscussionThreadsList(ticker, blockNode, listNode, deps) {
  const { getThreads, openHistoryModal, limit = 5 } = deps || {};
  if (!listNode) return 0;
  listNode.innerHTML = "";

  const safeTicker = String(ticker || "").trim();
  if (!safeTicker || typeof getThreads !== "function") {
    if (blockNode) blockNode.classList.add("hidden");
    return 0;
  }

  const scopePrefix = `line:${safeTicker}`;
  const allThreads = getThreads(scopePrefix) || [];
  const threads = allThreads.slice(0, Math.max(0, Number(limit) || 0));

  if (threads.length === 0) {
    if (blockNode) blockNode.classList.add("hidden");
    return 0;
  }

  for (const thread of threads) {
    const li = document.createElement("li");
    li.className = "lm-discussions-item";
    li.setAttribute("role", "button");
    li.tabIndex = 0;

    const titleText = String(thread.title || thread.scope || "Discussion").trim();
    const updatedAtRaw = thread.updated_at || thread.created_at || "";
    let updatedLabel = "";
    if (updatedAtRaw) {
      const d = new Date(updatedAtRaw);
      if (!Number.isNaN(d.getTime())) {
        updatedLabel = d.toLocaleString();
      }
    }

    li.innerHTML = `
      <span class="lm-discussions-title">${escapeHtml(titleText || "Discussion")}</span>
      ${updatedLabel ? `<span class="lm-discussions-meta">${escapeHtml(updatedLabel)}</span>` : ""}
    `;

    const open = () => {
      if (typeof openHistoryModal === "function") {
        openHistoryModal({ scopePrefix, title: `Discussions — ${safeTicker}` });
      }
    };
    li.addEventListener("click", open);
    li.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        open();
      }
    });

    listNode.appendChild(li);
  }

  if (blockNode) blockNode.classList.remove("hidden");
  return threads.length;
}
