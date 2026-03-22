/**
 * Line memory / position inspect modal — extracted from app.js.
 * Handles: modal open/close, position/market/news/analysis rendering, collection detail.
 */
import { escapeHtml } from "/desktop-shell/ui-display-utils.js";

// ── DOM nodes ────────────────────────────────────────────────────

const lineMemoryModalNode = document.getElementById("line-memory-modal");
const lineMemoryModalSubtitleNode = document.getElementById("line-memory-modal-subtitle");
const lineMemorySummaryNode = document.getElementById("line-memory-summary");
const lineMemoryPositionNode = document.getElementById("line-memory-position");
const lineMemoryMarketNode = document.getElementById("line-memory-market");
const lineMemoryNewsNode = document.getElementById("line-memory-news");
const lineMemoryAnalysisNode = document.getElementById("line-memory-analysis");
const lineMemoryMemorySummaryNode = document.getElementById("line-memory-memory-summary");
const lineMemorySignalsNode = document.getElementById("line-memory-signals");
const lineMemoryHistoryNode = document.getElementById("line-memory-history");
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

// ── Public API ───────────────────────────────────────────────────

export function initLineModal() {
  lineMemoryCloseBtn?.addEventListener("click", () => setVisible(false));

  // Detail panel toggle buttons
  document.getElementById("line-memory-market-detail-btn")?.addEventListener("click", () => {
    document.getElementById("line-memory-collection-detail")?.classList.toggle("hidden");
  });
  document.getElementById("line-memory-news-detail-btn")?.addEventListener("click", () => {
    document.getElementById("line-memory-news-detail")?.classList.toggle("hidden");
  });

  function setVisible(visible) {
    lineMemoryModalNode?.classList.toggle("hidden", !visible);
  }

  function open(rec) {
    const memory = rec?.lineMemory || {};
    const details = rec?.details || {};
    const ticker = rec?.ticker || "N/A";
    if (lineMemoryModalSubtitleNode) lineMemoryModalSubtitleNode.textContent = `${ticker}${rec?.name ? ` - ${rec.name}` : ""}`;
    if (lineMemorySummaryNode) lineMemorySummaryNode.textContent = String(rec?.summary || "No recommendation available.");
    // Reanalyse date
    const reanalyseNode = document.getElementById("line-memory-reanalyse");
    if (reanalyseNode) {
      if (rec?.reanalyseAfter) {
        reanalyseNode.textContent = `Next analysis: ${rec.reanalyseAfter}${rec.reanalyseReason ? ` — ${rec.reanalyseReason}` : ""}`;
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
    if (lineMemoryMemorySummaryNode) lineMemoryMemorySummaryNode.textContent = String(memory.llm_memory_summary || "No memory.");
    const signals = memory.llm_strong_signals || [];
    renderSimpleList(lineMemorySignalsNode, signals, "No signals.");
    lineMemorySignalsNode?.closest("article")?.classList?.toggle("hidden", signals.length === 0);
    const history = memory.llm_key_history || [];
    renderSimpleList(lineMemoryHistoryNode, history, "No history.");
    lineMemoryHistoryNode?.closest("article")?.classList?.toggle("hidden", history.length === 0);
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
    setVisible(true);
  }

  function close() {
    setVisible(false);
  }

  return { open, close };
}
