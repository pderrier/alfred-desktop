/**
 * Line memory / position inspect modal — extracted from app.js.
 * Handles: modal open/close, position/market/news/analysis rendering, collection detail.
 */
import { escapeHtml } from "/desktop-shell/ui-display-utils.js";
import { openChatWizard } from "/desktop-shell/app-chat-wizard.js";
import { openDiscussionHistoryModal, saveDiscussionThread } from "/desktop-shell/discussion-memory.js";

// ── DOM nodes ────────────────────────────────────────────────────

const lineMemoryModalNode = document.getElementById("line-memory-modal");
const lineMemoryModalSubtitleNode = document.getElementById("line-memory-modal-subtitle");
const lineMemorySummaryNode = document.getElementById("line-memory-summary");
const lineMemoryPositionNode = document.getElementById("line-memory-position");
const lineMemoryMarketNode = document.getElementById("line-memory-market");
const lineMemoryNewsNode = document.getElementById("line-memory-news");
const lineMemoryAnalysisNode = document.getElementById("line-memory-analysis");
const lineMemoryNarrativeNode = document.getElementById("line-memory-narrative");
const lineMemoryDeepNewsSummaryNode = document.getElementById("line-memory-deep-news-summary");
const lineMemoryDeepNewsMemorySummaryNode = document.getElementById("line-memory-deep-news-memory-summary");
const lineMemoryDeepNewsSelectedUrlNode = document.getElementById("line-memory-deep-news-selected-url");
const lineMemorySeenUrlsNode = document.getElementById("line-memory-seen-urls");
const lineMemoryBanlistNode = document.getElementById("line-memory-banlist");
const lineMemoryCloseBtn = document.getElementById("line-memory-close-btn");

// ── Helpers ──────────────────────────────────────────────────────

// escapeHtml imported from ui-display-utils.js

function renderSimpleList(listNode, items, emptyLabel) {
  if (!listNode) return;
  listNode.innerHTML = "";
  if (!Array.isArray(items) || items.length === 0) {
    const li = document.createElement("li");
    li.className = "empty-hint";
    li.textContent = emptyLabel;
    listNode.appendChild(li);
    return;
  }
  for (const value of items.slice(0, 24)) {
    const li = document.createElement("li");
    li.textContent = String(value);
    listNode.appendChild(li);
  }
}

function toMetricRows(record = {}, labels = {}) {
  return Object.entries(record || {})
    .filter(([, value]) => value !== null && value !== undefined && String(value).trim() !== "")
    .map(([key, value]) => ({ key: labels[key] || key, value: String(value) }));
}

function renderAnalysisList(listNode, items, emptyLabel) {
  if (!listNode) return;
  listNode.innerHTML = "";
  if (!Array.isArray(items) || items.length === 0) {
    const li = document.createElement("li");
    li.className = "empty-hint";
    li.textContent = emptyLabel;
    listNode.appendChild(li);
    return;
  }
  for (const item of items) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="lm-analysis-key">${escapeHtml(item.key)}:</span> ${escapeHtml(item.value)}`;
    listNode.appendChild(li);
  }
}

function renderMetricList(listNode, items, emptyLabel) {
  if (!listNode) return;
  listNode.innerHTML = "";
  if (!Array.isArray(items) || items.length === 0) {
    const li = document.createElement("li");
    li.className = "empty-hint";
    li.textContent = emptyLabel;
    listNode.appendChild(li);
    return;
  }
  for (const item of items) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="lm-metric-key">${escapeHtml(item.key)}</span><span class="lm-metric-val">${escapeHtml(item.value)}</span>`;
    listNode.appendChild(li);
  }
}

// ── News rendering ───────────────────────────────────────────────

function newsAgeDays(dateStr) {
  if (!dateStr) return null;
  const ms = Date.now() - new Date(dateStr).getTime();
  return Number.isFinite(ms) ? ms / 86400000 : null;
}

function newsStalenessOpacity(dateStr) {
  const days = newsAgeDays(dateStr);
  if (days === null) return 0.5;
  if (days <= 1) return 1;
  if (days <= 3) return 0.85;
  if (days <= 7) return 0.65;
  if (days <= 14) return 0.45;
  return 0.3;
}

function newsStalenessLabel(dateStr) {
  const days = newsAgeDays(dateStr);
  if (days === null) return "";
  if (days <= 1) return "today";
  if (days <= 7) return `${Math.round(days)}d ago`;
  if (days <= 30) return `${Math.round(days / 7)}w ago`;
  return `${Math.round(days / 30)}mo ago`;
}

function renderNewsShortList(listNode, newsItems) {
  if (!listNode) return;
  listNode.innerHTML = "";
  if (!Array.isArray(newsItems) || newsItems.length === 0) {
    const li = document.createElement("li");
    li.className = "empty-hint";
    li.textContent = "No news.";
    listNode.appendChild(li);
    return;
  }
  for (const article of newsItems.slice(0, 10)) {
    const li = document.createElement("li");
    const url = article.url || article.link || "";
    const title = article.title || "Untitled";
    const source = article.source || "";
    const date = article.date || article.published_at || "";
    const opacity = newsStalenessOpacity(date);
    li.style.opacity = String(opacity);
    if (url) {
      const a = document.createElement("a");
      a.href = "#";
      a.className = "news-article-link";
      a.dataset.url = url;
      a.textContent = title;
      a.style.color = "#8ecae6";
      a.style.textDecoration = "none";
      li.appendChild(a);
    } else {
      li.appendChild(document.createTextNode(title));
    }
    const age = newsStalenessLabel(date);
    const meta = [source, age].filter(Boolean).join(" \u00b7 ");
    if (meta) {
      const span = document.createElement("span");
      span.style.cssText = "color:var(--sea-muted);font-size:0.7rem;margin-left:0.3rem";
      span.textContent = meta;
      li.appendChild(span);
    }
    listNode.appendChild(li);
  }
}

function renderNewsDetail(newsItems) {
  const panel = document.getElementById("line-memory-news-detail");
  const articlesNode = document.getElementById("line-memory-news-articles");
  if (!panel || !articlesNode) return;

  if (!newsItems || newsItems.length === 0) {
    articlesNode.innerHTML = `<p class="empty-hint">No news articles collected.</p>`;
    panel.classList.add("hidden");
    return;
  }

  articlesNode.innerHTML = newsItems.map((article) => {
    const title = article.title || "Untitled";
    const url = article.url || article.link || "";
    const source = article.source || "";
    const date = article.date || article.published_at || "";
    const summary = article.summary || "";
    const dateStr = date ? new Date(date).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" }) : "";
    const opacity = newsStalenessOpacity(date);
    const age = newsStalenessLabel(date);
    const titleHtml = url ? `<a href="#" class="news-article-link" data-url="${escapeHtml(url)}">${escapeHtml(title)}</a>` : escapeHtml(title);
    return `<div class="news-article-card" style="opacity:${opacity}">
      <p class="news-article-title">${titleHtml}</p>
      <p class="news-article-meta">${[source, dateStr, age].filter(Boolean).join(" \u00b7 ")}</p>
      ${summary ? `<p class="news-article-summary">${escapeHtml(summary)}</p>` : ""}
    </div>`;
  }).join("");

  panel.classList.add("hidden");
}

// ── Collection detail ────────────────────────────────────────────

function renderCollectionDetail(details) {
  const panel = document.getElementById("line-memory-collection-detail");
  const grid = document.getElementById("line-memory-indicators-grid");
  const issuesNode = document.getElementById("line-memory-collection-issues");
  if (!panel || !grid) return;

  const market = details?.market || {};
  const quality = details?.quality || {};
  const issues = details?.enrichmentIssues || [];
  const source = details?.marketSource || "unknown";
  const sourceParts = source.split(":");
  const sourceProvider = sourceParts[0] || "unknown";
  const sourceType = sourceParts[1] || "";

  const indicators = [
    { key: "prix_actuel", label: "Spot Price", value: market.prix_actuel, unit: "\u20ac" },
    { key: "pe_ratio", label: "P/E Ratio", value: market.pe_ratio, unit: "x" },
    { key: "revenue_growth", label: "Revenue Growth", value: market.revenue_growth, unit: "%" },
    { key: "profit_margin", label: "Profit Margin", value: market.profit_margin, unit: "%" },
    { key: "debt_to_equity", label: "Debt / Equity", value: market.debt_to_equity, unit: "x" }
  ];

  grid.innerHTML = `<div class="collection-source-badge">${escapeHtml(sourceProvider)}${sourceType ? ` <span class="source-type">${escapeHtml(sourceType)}</span>` : ""}</div>` +
    indicators.map((ind) => {
      const present = ind.value !== null && ind.value !== undefined;
      const dotClass = present ? "present" : "missing";
      const valueText = present
        ? `${typeof ind.value === "number" ? ind.value.toFixed(2) : ind.value}${ind.unit || ""}`
        : "n/a";
      return `<div class="indicator-row">
        <span class="indicator-dot ${dotClass}"></span>
        <span class="indicator-label">${ind.label}</span>
        <span class="indicator-value">${valueText}</span>
      </div>`;
    }).join("") +
    (quality.news_quality_score != null
      ? `<div class="indicator-row"><span class="indicator-dot ${quality.news_quality_score >= 60 ? "present" : "missing"}"></span><span class="indicator-label">News Quality</span><span class="indicator-value">${quality.news_quality_score}/100</span></div>`
      : "");

  if (issuesNode) {
    issuesNode.innerHTML = issues.length > 0
      ? issues.map((issue) =>
          `<div class="collection-issue-row">\u26a0 ${escapeHtml(issue.scope || "")} \u2014 ${escapeHtml(issue.message || issue.error_code || "unknown")}${issue.provider ? ` <span class="source-type">${escapeHtml(issue.provider)}</span>` : ""}</div>`
        ).join("")
      : "";
  }

  panel.classList.add("hidden");
}

// ── Position chat context builder ────────────────────────────────

export function buildPositionContext(rec) {
  const ticker = rec?.ticker || "N/A";
  const name = rec?.name || "";
  const signal = rec?.signal || "N/A";
  const conviction = rec?.conviction || "N/A";
  const summary = rec?.summary || "";
  const details = rec?.details || {};
  const memory = rec?.lineMemory || {};
  const analysis = details.analysis || {};
  const position = details.position || {};
  const market = details.market || {};
  const news = details.news || [];

  const sections = [];
  sections.push(`Position: ${ticker}${name ? ` (${name})` : ""}`);
  sections.push(`Signal: ${signal} | Conviction: ${conviction}`);
  if (summary) sections.push(`Recommendation: ${summary}`);

  // Position metrics
  const posMetrics = [];
  if (position.quantite != null) posMetrics.push(`Qty: ${position.quantite}`);
  if (position.poids_pct != null) posMetrics.push(`Weight: ${position.poids_pct}%`);
  if (position.prix_actuel != null) posMetrics.push(`Price: ${position.prix_actuel}`);
  if (position.plus_moins_value_pct != null) posMetrics.push(`P/L: ${position.plus_moins_value_pct}%`);
  if (posMetrics.length > 0) sections.push(`Position: ${posMetrics.join(", ")}`);

  // Market data
  const mktMetrics = [];
  if (market.prix_actuel != null) mktMetrics.push(`Price: ${market.prix_actuel}`);
  if (market.pe_ratio != null) mktMetrics.push(`P/E: ${market.pe_ratio}`);
  if (market.revenue_growth != null) mktMetrics.push(`Rev Growth: ${market.revenue_growth}%`);
  if (market.profit_margin != null) mktMetrics.push(`Margin: ${market.profit_margin}%`);
  if (market.debt_to_equity != null) mktMetrics.push(`D/E: ${market.debt_to_equity}`);
  if (mktMetrics.length > 0) sections.push(`Market: ${mktMetrics.join(", ")}`);

  // Analysis
  if (analysis.analyse_technique) sections.push(`Technical: ${analysis.analyse_technique}`);
  if (analysis.analyse_fondamentale) sections.push(`Fundamental: ${analysis.analyse_fondamentale}`);
  if (analysis.analyse_sentiment) sections.push(`Sentiment: ${analysis.analyse_sentiment}`);
  if (analysis.deep_news_summary) sections.push(`News analysis: ${analysis.deep_news_summary}`);

  // Key reasons, risks, catalysts
  const reasons = analysis.raisons_principales || [];
  if (reasons.length > 0) sections.push(`Key reasons: ${reasons.slice(0, 5).join("; ")}`);
  const risks = analysis.risques || [];
  if (risks.length > 0) sections.push(`Risks: ${risks.slice(0, 5).join("; ")}`);
  const catalysts = analysis.catalyseurs || [];
  if (catalysts.length > 0) sections.push(`Catalysts: ${catalysts.slice(0, 5).join("; ")}`);

  // News headlines (concise)
  if (news.length > 0) {
    const headlines = news.slice(0, 5).map((a) => a.title || "Untitled").join("; ");
    sections.push(`Recent news: ${headlines}`);
  }

  // Line memory — V2 fields
  if (memory.memory_narrative) sections.push(`Memory narrative: ${memory.memory_narrative}`);
  const sigHistory = memory.signal_history || [];
  if (sigHistory.length > 0) {
    const recent = sigHistory.slice(-3).map((entry) => {
      const date = entry.date || "?";
      const sig = entry.signal || entry.recommendation || "?";
      const conv = entry.conviction || "?";
      const price = entry.price_at_signal != null ? ` @ ${entry.price_at_signal}` : "";
      return `[${date}] ${sig} (${conv})${price}`;
    });
    sections.push(`Signal history: ${recent.join(", ")}`);
  }
  const priceTracking = memory.price_tracking;
  if (priceTracking?.return_since_signal_pct != null) {
    sections.push(`Return since signal: ${priceTracking.return_since_signal_pct}%`);
  }
  const newsThemes = memory.news_themes || [];
  if (newsThemes.length > 0) sections.push(`Active themes: ${newsThemes.slice(0, 5).join(", ")}`);
  if (memory.trend) sections.push(`Trend: ${memory.trend}`);

  return `The user is inspecting their ${ticker}${name ? ` (${name})` : ""} position. Here is the full analysis context:\n\n${sections.join("\n")}\n\nYou are a portfolio analysis assistant. Answer questions about this position based on the context above. This is a read-only discussion — you cannot change recommendations or portfolio state. Be concise and specific. Only answer questions related to the portfolio, positions, and financial analysis. Politely decline any off-topic requests.

Important: this is a chat flow. If useful context is still missing (latest report snapshot, line-memory history, deep-news memory, or related portfolio context), fetch it with available MCP/CLI tools before answering instead of asking the user to paste it. Mention briefly when you used a tool.`;
}

// ── LLM chat synthesis for save pre-fill ────────────────────────

/**
 * Synthesize with a visible loading overlay so the user knows something is happening.
 */
export async function synthesizeChatForMemoryWithUI(ticker, name, chatHistory) {
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  overlay.style.zIndex = "10001";
  overlay.innerHTML = `
    <div class="modal-card" style="width:min(24rem,calc(100vw - 2rem));padding:2rem;text-align:center">
      <div class="pipeline-spinner" style="margin:0 auto 1rem;width:1.5rem;height:1.5rem"></div>
      <p style="color:var(--sea-text,#e0e8f0);font-size:0.9rem;margin:0">Summarizing conversation\u2026</p>
    </div>
  `;
  document.body.appendChild(overlay);
  try {
    const result = await synthesizeChatForMemory(ticker, name, chatHistory);
    return result;
  } finally {
    overlay.remove();
  }
}

/**
 * Synthesize a chat conversation into memory_narrative + user_note via LLM.
 * Returns { keyReasoning, userNote } or null on failure.
 */
export async function synthesizeChatForMemory(ticker, name, chatHistory) {
  if (!Array.isArray(chatHistory)) return null;
  const userMessages = chatHistory.filter((m) => m.role === "user");
  if (userMessages.length === 0) return null;

  const formatted = chatHistory
    .map((m) => `${m.role === "user" ? "User" : "Assistant"}: ${m.content}`)
    .join("\n\n");

  const prompt = `Given this conversation about ${ticker}${name ? ` (${name})` : ""}, extract:
1. KEY_REASONING: The main analytical insight or thesis update (1-3 sentences)
2. USER_NOTE: Any personal observation or decision the user expressed (1-2 sentences, or empty if none)

Conversation:
${formatted}

Respond exactly as:
KEY_REASONING: ...
USER_NOTE: ...`;

  try {
    const invoke = window?.__TAURI__?.core?.invoke;
    if (!invoke) return null;
    const result = await invoke("chat_wizard_send_local", {
      context: "You extract structured insights from portfolio analysis conversations. Be concise and faithful to what was discussed.",
      history: [],
      userMessage: prompt,
    });
    const response = result?.response || "";
    const krMatch = response.match(/KEY_REASONING:\s*([\s\S]*?)(?=\nUSER_NOTE:|$)/);
    const unMatch = response.match(/USER_NOTE:\s*([\s\S]*?)$/);
    const keyReasoning = krMatch?.[1]?.trim() || "";
    const userNote = unMatch?.[1]?.trim() || "";
    return {
      keyReasoning: keyReasoning && keyReasoning.toLowerCase() !== "empty" ? keyReasoning : "",
      userNote: userNote && userNote.toLowerCase() !== "empty" ? userNote : "",
    };
  } catch (err) {
    console.error("chat_synthesis_failed", err);
    return null;
  }
}

// ── Save to Memory panel ────────────────────────────────────────

/**
 * Show an inline overlay panel where the user can persist insights
 * from a chat drill-down back into line-memory.json.
 * Fields: memory_narrative (textarea), user_note (textarea), news_themes (checkboxes).
 *
 * @param {Object} rec — the recommendation / position record
 * @param {Object} [prefill] — optional LLM-generated pre-fill values
 * @param {string} [prefill.keyReasoning] — suggested memory narrative text
 * @param {string} [prefill.userNote] — suggested personal note text
 */
export function showSaveToMemoryPanel(rec, prefill) {
  if (!rec) return;
  const ticker = rec.ticker || "";
  const memory = rec.lineMemory || {};
  const existingReasoning = memory.memory_narrative || "";
  const existingThemes = Array.isArray(memory.news_themes) ? memory.news_themes : [];
  // Merge existing reasoning with prefill if both present
  const prefillReasoning = prefill?.keyReasoning || "";
  const mergedReasoning = existingReasoning && prefillReasoning
    ? `${existingReasoning}\n---\n${prefillReasoning}`
    : prefillReasoning || existingReasoning;
  const mergedNote = prefill?.userNote || "";
  // Merge run badges with existing themes for checkbox list
  const analysis = rec.details?.analysis || {};
  const runBadges = Array.isArray(analysis.badges_keywords) ? analysis.badges_keywords : [];
  const allThemes = [...new Set([...existingThemes, ...runBadges])].filter(Boolean);

  // Build overlay
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  overlay.style.zIndex = "10001"; // above chat wizard

  const themesHtml = allThemes.length > 0
    ? allThemes.map((t) => {
        const checked = existingThemes.includes(t) ? "checked" : "";
        const id = `stm-theme-${t.replace(/\W/g, "_")}`;
        return `<label class="stm-theme-label" for="${id}">
          <input type="checkbox" id="${id}" value="${escapeHtml(t)}" ${checked} class="stm-theme-cb" />
          ${escapeHtml(t)}
        </label>`;
      }).join("")
    : `<span class="stm-empty-hint">No themes available.</span>`;

  overlay.innerHTML = `
    <div class="modal-card" style="width:min(36rem,calc(100vw - 2rem));max-height:min(70vh,42rem);display:flex;flex-direction:column;padding:0">
      <div style="display:flex;align-items:center;justify-content:space-between;padding:1rem 1.2rem 0.6rem;border-bottom:1px solid rgba(73,100,126,0.3)">
        <h3 style="margin:0;font-size:1rem;color:var(--sea-text,#e0e8f0)">Save to Memory \u2014 ${escapeHtml(ticker)}</h3>
        <button class="stm-cancel-btn" style="background:none;border:none;color:var(--sea-muted,#8a9bb0);font-size:1.2rem;cursor:pointer;padding:0.2rem 0.4rem" title="Cancel">&times;</button>
      </div>
      <div style="flex:1;overflow-y:auto;padding:1rem 1.2rem;display:flex;flex-direction:column;gap:1rem">
        <div>
          <label style="display:block;font-size:0.8rem;color:var(--sea-muted,#8a9bb0);margin-bottom:0.3rem">Memory</label>
          <textarea class="stm-key-reasoning" rows="4" style="width:100%;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:8px;color:var(--sea-text,#e0e8f0);padding:0.5rem 0.7rem;font-size:0.85rem;resize:vertical;font-family:inherit">${escapeHtml(mergedReasoning)}</textarea>
        </div>
        <div>
          <label style="display:block;font-size:0.8rem;color:var(--sea-muted,#8a9bb0);margin-bottom:0.3rem">Personal Note</label>
          <textarea class="stm-user-note" rows="3" placeholder="Any personal notes about this position\u2026" style="width:100%;background:rgba(10,17,24,0.6);border:1px solid rgba(73,100,126,0.4);border-radius:8px;color:var(--sea-text,#e0e8f0);padding:0.5rem 0.7rem;font-size:0.85rem;resize:vertical;font-family:inherit">${escapeHtml(mergedNote)}</textarea>
        </div>
        <div>
          <label style="display:block;font-size:0.8rem;color:var(--sea-muted,#8a9bb0);margin-bottom:0.3rem">News Themes</label>
          <div class="stm-themes-grid" style="display:flex;flex-wrap:wrap;gap:0.4rem 0.8rem">${themesHtml}</div>
        </div>
      </div>
      <div style="display:flex;gap:0.5rem;padding:0.8rem 1.2rem;border-top:1px solid rgba(73,100,126,0.3);justify-content:flex-end">
        <button class="stm-cancel-btn ghost-btn" type="button">Cancel</button>
        <button class="stm-confirm-btn cmd-btn" type="button">Save</button>
      </div>
    </div>
  `;

  document.body.appendChild(overlay);

  const confirmBtn = overlay.querySelector(".stm-confirm-btn");
  const cancelBtns = overlay.querySelectorAll(".stm-cancel-btn");
  const reasoningEl = overlay.querySelector(".stm-key-reasoning");
  const noteEl = overlay.querySelector(".stm-user-note");

  function close() { overlay.remove(); }

  // Cancel: close without saving
  for (const btn of cancelBtns) btn.addEventListener("click", close);
  // Only allow backdrop dismiss after a mousedown+mouseup cycle on the overlay itself
  // This prevents stale click events from prior overlays from closing this one
  let backdropMouseDown = false;
  overlay.addEventListener("mousedown", (e) => { if (e.target === overlay) backdropMouseDown = true; });
  overlay.addEventListener("mouseup", (e) => {
    if (e.target === overlay && backdropMouseDown) close();
    backdropMouseDown = false;
  });

  // Confirm: collect fields and call Tauri command
  confirmBtn.addEventListener("click", async () => {
    const memoryNarrative = reasoningEl.value.trim() || null;
    const userNote = noteEl.value.trim() || null;
    const checkedThemes = [...overlay.querySelectorAll(".stm-theme-cb:checked")]
      .map((cb) => cb.value);
    const newsThemes = checkedThemes.length > 0 ? checkedThemes : null;

    // Nothing to save
    if (!memoryNarrative && !userNote && !newsThemes) {
      close();
      return;
    }

    confirmBtn.disabled = true;
    confirmBtn.textContent = "Saving\u2026";

    try {
      const invoke = window?.__TAURI__?.core?.invoke;
      if (!invoke) throw new Error("Tauri not available");
      await invoke("update_line_memory_local", {
        ticker,
        memoryNarrative,
        userNote,
        newsThemes,
      });
      close();
    } catch (err) {
      confirmBtn.textContent = "Error \u2014 retry";
      confirmBtn.disabled = false;
      console.error("save_to_memory_failed", err);
    }
  });

  // Focus the reasoning textarea
  reasoningEl?.focus();
}

// ── Public API ───────────────────────────────────────────────────

export function initLineModal() {
  let currentRec = null;

  lineMemoryCloseBtn?.addEventListener("click", () => setVisible(false));

  // Detail panel toggle buttons
  document.getElementById("line-memory-market-detail-btn")?.addEventListener("click", () => {
    document.getElementById("line-memory-collection-detail")?.classList.toggle("hidden");
  });
  document.getElementById("line-memory-news-detail-btn")?.addEventListener("click", () => {
    document.getElementById("line-memory-news-detail")?.classList.toggle("hidden");
  });

  // "Ask about this" chat button — opens chat wizard, then offers "Save to Memory"
  const askBtn = document.getElementById("line-memory-ask-btn");
  askBtn?.addEventListener("click", async () => {
    if (!currentRec) return;
    const rec = currentRec; // capture at click time — may change during awaits
    const ticker = rec.ticker || "N/A";
    const name = rec.name || "";
    const signal = rec.signal || "N/A";
    const conviction = rec.conviction || "N/A";
    const label = name ? `${name} (${ticker})` : ticker;
    let doneHandled = false;
    await openChatWizard({
      title: `Ask about ${ticker}`,
      systemContext: buildPositionContext(rec),
      initialMessage: `I can answer questions about your ${label} position. The current recommendation is ${signal} (${conviction}). What would you like to know?`,
      discussionScope: `line:${ticker}`,
      discussionMetadata: { ticker, name },
      returnHistoryOnClose: true,
      onDone: async (history) => {
        // Called directly from Done button — bypasses promise chain
        doneHandled = true;
        const hadConversation = history.some((m) => m.role === "user");
        const prefill = hadConversation
          ? await synthesizeChatForMemoryWithUI(ticker, name, history)
          : null;
        if (prefill?.keyReasoning || prefill?.userNote) {
          saveDiscussionThread({
            scope: `line:${ticker}`,
            title: `Line ${ticker}`,
            summary: prefill?.keyReasoning || "",
            note: prefill?.userNote || "",
            metadata: { ticker, name },
          });
        }
        showSaveToMemoryPanel(rec, prefill);
      },
    });
    // If closed via X/backdrop (onDone not called), still show save panel
    if (!doneHandled) {
      showSaveToMemoryPanel(rec, null);
    }
  });

  function setVisible(visible) {
    lineMemoryModalNode?.classList.toggle("hidden", !visible);
  }

  function open(rec) {
    currentRec = rec;
    const memory = rec?.lineMemory || {};
    const details = rec?.details || {};
    const ticker = rec?.ticker || "N/A";
    if (lineMemoryModalSubtitleNode) lineMemoryModalSubtitleNode.textContent = `${ticker}${rec?.name ? ` - ${rec.name}` : ""}`;
    // KPI strip — position metrics at a glance
    const kpiStrip = document.getElementById("line-memory-kpi-strip");
    if (kpiStrip) {
      const pos = details.position || {};
      const qty = pos.quantite;
      const pru = pos.prix_revient;
      const price = pos.prix_actuel ?? details.market?.prix_actuel;
      const pvPct = pos.plus_moins_value_pct;
      const total = pos.valorisation ?? ((typeof qty === "number" && typeof price === "number") ? qty * price : null);
      const signal = rec?.signal || "";
      const signalNode = document.getElementById("lm-kpi-signal");
      if (signalNode) {
        signalNode.textContent = signal;
        signalNode.className = "lm-kpi-signal";
        const su = signal.toUpperCase();
        if (su.includes("ACHAT") || su === "BUY") signalNode.classList.add("signal-buy");
        else if (su.includes("VENT") || su === "SELL") signalNode.classList.add("signal-sell");
        else if (su.includes("RENFOR") || su === "REINFORCE") signalNode.classList.add("signal-reinforce");
        else signalNode.classList.add("signal-hold");
      }
      const fmt = (v) => v != null && Number.isFinite(v) ? v.toLocaleString("fr-FR", { maximumFractionDigits: 2 }) : "—";
      const fmtPct = (v) => v != null && Number.isFinite(v) ? `${v >= 0 ? "+" : ""}${v.toFixed(1)}%` : "—";
      const el = (id) => document.getElementById(id);
      if (el("lm-kpi-qty")) el("lm-kpi-qty").textContent = fmt(qty);
      if (el("lm-kpi-pru")) el("lm-kpi-pru").textContent = fmt(pru);
      if (el("lm-kpi-price")) el("lm-kpi-price").textContent = fmt(price);
      const pvNode = el("lm-kpi-pv");
      if (pvNode) {
        pvNode.textContent = fmtPct(pvPct);
        pvNode.style.color = pvPct >= 0 ? "#4ade80" : "#f87171";
      }
      if (el("lm-kpi-total")) el("lm-kpi-total").textContent = total != null ? fmt(total) + " \u20ac" : "—";
      kpiStrip.classList.remove("hidden");
    }
    if (lineMemorySummaryNode) lineMemorySummaryNode.textContent = String(rec?.summary || "No recommendation available.");
    let previousBtn = document.getElementById("line-memory-previous-discussions-btn");
    const askBtnNode = document.getElementById("line-memory-ask-btn");
    if (!previousBtn && askBtnNode?.parentElement) {
      previousBtn = document.createElement("button");
      previousBtn.id = "line-memory-previous-discussions-btn";
      previousBtn.type = "button";
      previousBtn.className = "ghost-btn";
      previousBtn.style.cssText = "margin-top:0.4rem";
      previousBtn.textContent = "Previous discussions";
      askBtnNode.parentElement.appendChild(previousBtn);
    }
    previousBtn?.replaceWith(previousBtn?.cloneNode(true));
    previousBtn = document.getElementById("line-memory-previous-discussions-btn");
    previousBtn?.addEventListener("click", () => {
      openDiscussionHistoryModal({
        scopePrefix: `line:${ticker}`,
        title: `Previous discussions — ${ticker}`,
        onApplyInsight: (insight) => {
          const noteNode = document.getElementById("line-memory-user-note");
          if (noteNode) noteNode.textContent = insight;
        }
      });
    });
    // Reanalyse date
    const reanalyseNode = document.getElementById("line-memory-reanalyse");
    if (reanalyseNode) {
      if (rec?.reanalyseAfter) {
        reanalyseNode.innerHTML = `
          <span class="lm-reanalyse-icon">\u{1F4C5}</span>
          <div class="lm-reanalyse-content">
            <strong>Next analysis: ${rec.reanalyseAfter}</strong>
            ${rec.reanalyseReason ? `<p>${rec.reanalyseReason}</p>` : ""}
          </div>
        `;
        reanalyseNode.classList.remove("hidden");
      } else {
        reanalyseNode.classList.add("hidden");
      }
    }
    renderMetricList(lineMemoryPositionNode, toMetricRows(details.position, { nom: "Name", quantite: "Quantity", poids_pct: "Weight %", prix_actuel: "Current Price", plus_moins_value_pct: "P/L %" }), "No position data.");
    renderMetricList(lineMemoryMarketNode, toMetricRows(details.market, { prix_actuel: "Spot Price", pe_ratio: "P/E Ratio", revenue_growth: "Revenue Growth", profit_margin: "Profit Margin", debt_to_equity: "Debt/Equity" }), "No market data.");
    renderCollectionDetail(details);
    renderNewsShortList(lineMemoryNewsNode, details.news || []);
    renderNewsDetail(details.news || []);
    renderAnalysisList(lineMemoryAnalysisNode, toMetricRows(details.analysis, { analyse_technique: "Technical", analyse_fondamentale: "Fundamental", analyse_sentiment: "Sentiment" }), "No analysis.");
    // Memory narrative (replaces former llm_memory_summary + key_reasoning)
    const narrative = memory.memory_narrative || "";
    if (lineMemoryNarrativeNode) lineMemoryNarrativeNode.textContent = narrative || "No memory.";
    // Notes & Discussions — user-saved insights from chat drill-downs
    const notesSection = document.getElementById("line-memory-notes-section");
    const userNoteNode = document.getElementById("line-memory-user-note");
    const userNote = memory.user_note || "";
    const hasNotes = narrative || userNote;
    if (notesSection) notesSection.classList.toggle("hidden", !hasNotes);
    if (userNoteNode) {
      userNoteNode.textContent = userNote || "No personal note.";
      document.getElementById("line-memory-user-note-block")?.classList.toggle("hidden", !userNote);
    }
    // "News Analysis" = LLM's deep_news_summary from this run
    const freshSummary = String(details.analysis?.deep_news_summary || "");
    const memorySummary = String(memory.deep_news_memory_summary || "");
    if (lineMemoryDeepNewsSummaryNode) {
      lineMemoryDeepNewsSummaryNode.textContent = freshSummary || memorySummary || "No news analysis.";
    }
    // "News Context" = cross-run memory — only show if different from the analysis
    if (lineMemoryDeepNewsMemorySummaryNode) {
      if (memorySummary && memorySummary !== freshSummary) {
        lineMemoryDeepNewsMemorySummaryNode.textContent = memorySummary;
        lineMemoryDeepNewsMemorySummaryNode.parentElement?.classList?.remove("hidden");
      } else {
        lineMemoryDeepNewsMemorySummaryNode.parentElement?.classList?.add("hidden");
      }
    }
    // Source URL / Seen / Banned — hide if empty (tracked server-side in deep_news cache now)
    if (lineMemoryDeepNewsSelectedUrlNode) {
      const url = memory.deep_news_selected_url || "";
      lineMemoryDeepNewsSelectedUrlNode.textContent = url || "No source URL.";
      lineMemoryDeepNewsSelectedUrlNode.closest("article")?.classList?.toggle("hidden", !url);
    }
    const seenUrls = memory.deep_news_seen_urls || [];
    renderSimpleList(lineMemorySeenUrlsNode, seenUrls, "No sources reviewed.");
    lineMemorySeenUrlsNode?.closest("article")?.classList?.toggle("hidden", seenUrls.length === 0);
    const bannedUrls = memory.deep_news_banned_urls || [];
    renderSimpleList(lineMemoryBanlistNode, bannedUrls, "No excluded sources.");
    lineMemoryBanlistNode?.closest("article")?.classList?.toggle("hidden", bannedUrls.length === 0);
    // Phase 3b: signal accuracy scorecard
    renderSignalScorecard(ticker);
    setVisible(true);
  }

  async function renderSignalScorecard(ticker) {
    const container = document.getElementById("line-memory-scorecard");
    if (!container) return;
    try {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) { container.classList.add("hidden"); return; }
      const data = await invoke("get_signal_scorecard_local", { ticker });
      if (!data?.signals?.length) { container.classList.add("hidden"); return; }
      const trendIcon = data.trend === "improving" ? "\u2197\uFE0F" : data.trend === "declining" ? "\u2198\uFE0F" : "\u27A1\uFE0F";
      const barColor = data.overall_accuracy_pct >= 70 ? "#2a9d8f" : data.overall_accuracy_pct >= 40 ? "#e9c46a" : "#e76f51";
      let html = `<h3 class="scorecard-title">Signal Accuracy</h3>`;
      html += `<div class="scorecard-accuracy-bar"><div style="width:${data.overall_accuracy_pct}%;background:${barColor}"></div></div>`;
      html += `<div class="scorecard-summary">${data.overall_accuracy_pct}% accurate (${data.correct_count}/${data.scored_count} scored) ${trendIcon} ${data.trend}</div>`;
      html += `<table class="scorecard-table"><thead><tr><th>Date</th><th>Signal</th><th>Price</th><th>Return</th><th></th></tr></thead><tbody>`;
      for (const s of data.signals.slice(0, 8)) {
        const icon = s.accuracy === "correct" ? "\u2705" : s.accuracy === "incorrect" ? "\u274C" : "\u2796";
        const cls = `scorecard-${s.accuracy}`;
        html += `<tr class="${cls}"><td>${s.date}</td><td>${s.signal}</td><td>${s.price_at_signal?.toFixed(1) || "?"}</td><td>${s.return_pct >= 0 ? "+" : ""}${s.return_pct?.toFixed(1) || 0}%</td><td>${icon}</td></tr>`;
      }
      html += `</tbody></table>`;
      container.innerHTML = html;
      container.classList.remove("hidden");
    } catch {
      container.classList.add("hidden");
    }
  }

  function close() {
    setVisible(false);
  }

  return { open, close };
}
