/**
 * Shell Live Run — live positions rendering, pipeline bar, progress counters.
 *
 * Extracted from shell-layout.js for single-responsibility. Manages all
 * real-time UI updates during an active analysis run.
 */

import { formatCurrency, escapeHtml, truncate } from "/desktop-shell/ui-display-utils.js";
import { renderSignalBadge } from "/desktop-shell/shell-positions.js";

// Shared context for enriching done rows — set by app.js
let liveRunContext = { positions: [], dashboardPayload: null };
export function setLiveRunContext(positions, dashboardPayload) {
  liveRunContext = { positions: positions || [], dashboardPayload };
}

// ── DOM refs ──────────────────────────────────────────────────────

const positionsTbodyNode = document.getElementById("positions-tbody");
const positionsEmptyNode = document.getElementById("positions-empty");
const positionsProgressNode = document.getElementById("positions-progress");
const topBarProgressNode = document.getElementById("top-bar-progress");
const topBarProgressFillNode = document.getElementById("top-bar-progress-fill");
const runPipelineBarNode = document.getElementById("run-pipeline-bar");
const reportSynthesisCardNode = document.getElementById("report-synthesis-card");

// ── State ────────────────────────────────────────────────────────

let activeRunInProgress = false;
let liveRunActiveId = null;   // run_id of the currently executing run
let liveRunViewingId = null;  // run_id the user is currently viewing (null = active run)

export function setActiveRunInProgress(active) { activeRunInProgress = active; }
export function isActiveRunInProgress() { return activeRunInProgress; }

export function setLiveRunActiveId(runId) { liveRunActiveId = runId; }
export function getLiveRunActiveId() { return liveRunActiveId; }
export function setLiveRunViewingId(runId) { liveRunViewingId = runId; }

/// Returns true when the user is viewing a completed/different run, not the active one.
function isViewingDifferentRun() {
  if (!activeRunInProgress || !liveRunActiveId) return false;
  // null viewingId means "following the active run"
  if (liveRunViewingId == null) return false;
  return liveRunViewingId !== liveRunActiveId;
}

// ── Live positions ───────────────────────────────────────────────

/// Look up position name from live run context.
function findPositionName(ticker) {
  const upper = ticker.toUpperCase();
  const pos = liveRunContext.positions.find(
    (p) => (p.ticker || "").toUpperCase() === upper
  );
  return pos?.nom || "";
}

/// Check if a ticker is a watchlist item (not in portfolio positions).
function isWatchlistTicker(ticker) {
  const upper = ticker.toUpperCase();
  return !liveRunContext.positions.some(
    (p) => (p.ticker || "").toUpperCase() === upper
  );
}

function buildLiveRow(ticker, lineStatus) {
  const status = parseLineStatus(lineStatus);
  const rowClass = lineRowClass(status);
  const name = findPositionName(ticker);
  const isWl = isWatchlistTicker(ticker);
  const tr = document.createElement("tr");
  tr.className = `pos-main-row ${rowClass}${isWl ? " pos-watchlist-row" : ""}`;
  tr.dataset.ticker = ticker.toUpperCase();
  tr.dataset.watchlist = isWl ? "1" : "";
  tr.innerHTML = `
    <td><strong>${escapeHtml(ticker)}</strong></td>
    <td>${escapeHtml(name)}</td><td></td><td></td><td></td><td></td><td></td>
    <td>${renderLiveStatusChip(lineStatus)}</td>
    <td></td>
  `;
  return tr;
}

/**
 * Pre-render portfolio positions as "Queued" rows before the first line-status-update arrives.
 * Called from app.js on run.started to eliminate the blank-screen window.
 * @param {Array} positions — portfolio positions from latestDashboardPayload
 */
export function preRenderQueuedPositions(positions) {
  if (!positionsTbodyNode || !Array.isArray(positions) || positions.length === 0) return;
  // Only pre-render if the table is currently empty (fresh run start)
  if (positionsTbodyNode.querySelectorAll("tr.pos-main-row[data-ticker]").length > 0) return;
  if (positionsEmptyNode) positionsEmptyNode.classList.add("hidden");
  for (const pos of positions) {
    const ticker = pos.ticker || "";
    if (!ticker) continue;
    positionsTbodyNode.appendChild(buildLiveRow(ticker, "waiting"));
  }
  // Update progress counter
  if (positionsProgressNode) {
    positionsProgressNode.textContent = `0/${positions.length} done · ${positions.length} queued`;
    positionsProgressNode.classList.remove("hidden");
  }
}

/**
 * Show "Connecting to analysis server..." in the synthesis card area.
 * Cleared automatically when the first line-status-update arrives via updateSynthesisCardDuringRun.
 */
export function showConnectingPlaceholder() {
  const synthesis = document.getElementById("report-synthesis");
  if (synthesis) {
    synthesis.innerHTML = `<span class="synthesis-pending-label synthesis-connecting"><span class="pipeline-spinner"></span>Connecting to analysis server\u2026</span>`;
  }
  if (reportSynthesisCardNode) {
    reportSynthesisCardNode.classList.add("synthesis-pending");
  }
}

function ensureWatchlistSeparator() {
  if (!positionsTbodyNode) return;
  if (positionsTbodyNode.querySelector(".watchlist-separator-row")) return;
  const wlRows = positionsTbodyNode.querySelectorAll("tr[data-watchlist='1']");
  if (wlRows.length === 0) return;
  const sep = document.createElement("tr");
  sep.className = "watchlist-separator-row";
  sep.innerHTML = `<td colspan="9" class="watchlist-separator-cell"><span class="watchlist-badge">Watchlist</span></td>`;
  wlRows[0].insertAdjacentElement("beforebegin", sep);
}

export function renderLivePositions(lineStatus, dashboardPayload) {
  if (!positionsTbodyNode || !lineStatus) return;
  // Don't mutate the positions table if the user is browsing a different run/account
  if (isViewingDifferentRun()) return;

  const tickers = Object.keys(lineStatus).filter((t) => t !== "__synthesis__");
  if (tickers.length === 0) return;

  // Split into positions and watchlist
  const posTickers = tickers.filter((t) => !isWatchlistTicker(t));
  const wlTickers = tickers.filter((t) => isWatchlistTicker(t));

  // Bootstrap rows from line_status keys when the table is empty
  const existingRows = positionsTbodyNode.querySelectorAll("tr.pos-main-row[data-ticker]");
  if (existingRows.length === 0) {
    if (positionsEmptyNode) positionsEmptyNode.classList.add("hidden");
    // Positions first, then watchlist
    for (const ticker of posTickers) {
      positionsTbodyNode.appendChild(buildLiveRow(ticker, lineStatus[ticker]));
    }
    if (wlTickers.length > 0) {
      for (const ticker of wlTickers) {
        positionsTbodyNode.appendChild(buildLiveRow(ticker, lineStatus[ticker]));
      }
      ensureWatchlistSeparator();
    }
    updateProgressCounter(lineStatus, tickers);
    return;
  }

  // Update existing rows + add missing ones
  if (positionsEmptyNode) positionsEmptyNode.classList.add("hidden");
  const existingTickerSet = new Set();
  for (const row of existingRows) {
    const ticker = row.dataset.ticker;
    existingTickerSet.add(ticker);
    const raw = lineStatus[ticker];
    if (!raw) continue;
    const status = parseLineStatus(raw);
    row.classList.remove("line-waiting", "line-active", "line-done");
    row.classList.add(lineRowClass(status));
    const signalCell = row.querySelector("td:nth-child(8)");
    if (signalCell) {
      const hasSignalBadge = signalCell.querySelector(".signal-badge");
      // If done with recommendation data, show signal badge instead of "Done" chip
      const rec = typeof raw === "object" ? raw?.recommendation : null;
      if (status === "done" && rec?.signal) {
        if (!hasSignalBadge) signalCell.innerHTML = renderSignalBadge(rec.signal);
      } else if (!(status === "done" && hasSignalBadge)) {
        signalCell.innerHTML = renderLiveStatusChip(raw);
      }
    }
    // Show name in name cell
    const nameCell = row.querySelector("td:nth-child(2)");
    if (nameCell && !nameCell.textContent.trim()) {
      const name = findPositionName(ticker);
      if (name) nameCell.textContent = name;
    }
    // Show progress in sub-row (same row used for recommendation summary when done)
    const progressMsg = typeof raw === "object" ? (raw?.progress || "") : "";
    if ((status === "analyzing" || status === "repairing" || status === "collecting") && progressMsg) {
      let subRow = positionsTbodyNode.querySelector(`tr.pos-sub-row[data-ticker="${ticker.toUpperCase()}"]`);
      if (!subRow) {
        subRow = document.createElement("tr");
        subRow.className = "pos-sub-row";
        subRow.dataset.ticker = ticker.toUpperCase();
        row.insertAdjacentElement("afterend", subRow);
      }
      subRow.innerHTML = `<td colspan="9" class="pos-rec-row"><span class="live-progress-detail">${escapeHtml(progressMsg)}</span></td>`;
    }
  }
  // Add rows for tickers not yet in the table
  for (const ticker of tickers) {
    if (existingTickerSet.has(ticker.toUpperCase()) || existingTickerSet.has(ticker)) continue;
    positionsTbodyNode.appendChild(buildLiveRow(ticker, lineStatus[ticker]));
  }
  ensureWatchlistSeparator();

  updateProgressCounter(lineStatus, tickers);
  updateSynthesisCardDuringRun(lineStatus, tickers);
  updatePipelineFromLineStatus(lineStatus);
}

/// Update a single line's progress in-place (called from Tauri event, no polling).
export function updateSingleLineProgress(ticker, lineStatus) {
  if (!positionsTbodyNode) return;
  if (isViewingDifferentRun()) return;
  const upperTicker = ticker.toUpperCase();
  const row = positionsTbodyNode.querySelector(`tr[data-ticker="${upperTicker}"], tr[data-ticker="${ticker}"]`);
  if (!row) return;
  const status = parseLineStatus(lineStatus);
  row.classList.remove("line-waiting", "line-active", "line-done");
  row.classList.add(lineRowClass(status));
  const signalCell = row.querySelector("td:nth-child(8)");
  const progressMsg = typeof lineStatus === "object" ? (lineStatus.progress || "") : "";
  const rec = typeof lineStatus === "object" ? lineStatus.recommendation : null;

  if (status === "done" && rec) {
    // Fill in position data from portfolio (like final report table)
    const pos = liveRunContext.positions.find(
      (p) => (p.ticker || "").toUpperCase() === upperTicker
    );
    if (pos) {
      const pv = pos.plus_moins_value || 0;
      const pvPct = pos.plus_moins_value_pct || 0;
      const pvClass = pv >= 0 ? "pos-pv-positive" : "pos-pv-negative";
      row.innerHTML = `
        <td><strong>${escapeHtml(upperTicker)}</strong></td>
        <td>${escapeHtml(pos.nom || "")}</td>
        <td class="num">${pos.quantite ?? ""}</td>
        <td class="num">${formatNum(pos.prix_revient)}</td>
        <td class="num">${formatNum(pos.prix_actuel)}</td>
        <td class="num ${pvClass}">${pv >= 0 ? "+" : ""}${formatNum(pv)}</td>
        <td class="num ${pvClass}">${pvPct >= 0 ? "+" : ""}${pvPct.toFixed(1)}%</td>
        <td>${renderSignalBadge(rec.signal || "DONE")}</td>
        <td></td>
      `;
    } else {
      // No position data (watchlist) — just update signal
      if (signalCell) signalCell.innerHTML = renderSignalBadge(rec.signal || "DONE");
    }
    // Make row clickable for inspect modal
    row.style.cursor = "pointer";
    row.onclick = () => {
      if (window.__openLineMemoryModal) {
        window.__openLineMemoryModal({
          ticker: upperTicker,
          signal: rec.signal,
          conviction: rec.conviction,
          summary: rec.synthese || rec.summary || "",
          name: pos?.nom || "",
          type: pos?.type || "position",
          ...rec,
        });
      }
    };
    // Render recommendation summary in sub-row
    let subRow = positionsTbodyNode.querySelector(`tr.pos-sub-row[data-ticker="${upperTicker}"]`);
    if (!subRow) {
      subRow = document.createElement("tr");
      subRow.className = "pos-sub-row";
      subRow.dataset.ticker = upperTicker;
      row.insertAdjacentElement("afterend", subRow);
    }
    const signal = escapeHtml(rec.signal || "");
    const conviction = escapeHtml(rec.conviction || "");
    const summary = escapeHtml(truncate(rec.synthese || rec.summary || "", 120));
    subRow.innerHTML = `<td colspan="9" class="pos-rec-row">${signal} \u00b7 ${conviction} \u00b7 ${summary}</td>`;
    // Restore name in name cell
    const nameCell2 = row.querySelector("td:nth-child(2)");
    if (nameCell2) nameCell2.textContent = pos?.nom || "";
  } else {
    if (signalCell) signalCell.innerHTML = renderLiveStatusChip(lineStatus);
    // Show progress in sub-row
    if ((status === "analyzing" || status === "repairing" || status === "collecting") && progressMsg) {
      let subRow = positionsTbodyNode.querySelector(`tr.pos-sub-row[data-ticker="${upperTicker}"]`);
      if (!subRow) {
        subRow = document.createElement("tr");
        subRow.className = "pos-sub-row";
        subRow.dataset.ticker = upperTicker;
        row.insertAdjacentElement("afterend", subRow);
      }
      subRow.innerHTML = `<td colspan="9" class="pos-rec-row"><span class="live-progress-detail">${escapeHtml(progressMsg)}</span></td>`;
    }
  }
}

// ── Top bar progress ─────────────────────────────────────────────

export function renderTopBarProgress(runSummary) {
  if (!topBarProgressNode || !topBarProgressFillNode) return;
  const isRunning = runSummary?.status === "running";
  topBarProgressNode.classList.toggle("hidden", !isRunning);
  if (isRunning) {
    const lineProgress = runSummary?.line_progress || {};
    const completed = lineProgress.completed || 0;
    const total = lineProgress.total || 1;
    const pct = Math.min(100, Math.round((completed / total) * 100));
    topBarProgressFillNode.style.width = `${pct}%`;
  }
}

// ── Pipeline bar ─────────────────────────────────────────────────

const PIPELINE_STEPS = [
  { key: "collecting", label: "Collecting" },
  { key: "analyzing", label: "Analyzing" },
  { key: "synthesis", label: "Global Synthesis" },
  { key: "done", label: "Done" }
];

const STEP_ORDER = Object.fromEntries(PIPELINE_STEPS.map((s, i) => [s.key, i]));
let pipelineHighWaterMark = -1;
let pipelineActiveKeys = new Set();

/// Derive active pipeline stages from actual line statuses.
function deriveActiveStages(lineStatus) {
  const stages = new Set();
  if (!lineStatus) return stages;
  for (const [ticker, raw] of Object.entries(lineStatus)) {
    if (ticker === "__synthesis__") {
      const s = typeof raw === "object" ? raw?.status : raw;
      if (s === "generating") stages.add("synthesis");
      continue;
    }
    const s = typeof raw === "object" ? raw?.status : raw;
    if (s === "waiting" || s === "collecting") stages.add("collecting");
    else if (s === "analyzing" || s === "repairing") stages.add("analyzing");
  }
  return stages;
}

/// Update pipeline bar from line statuses (called after renderLivePositions).
export function updatePipelineFromLineStatus(lineStatus) {
  const derived = deriveActiveStages(lineStatus);
  if (derived.size > 0) {
    pipelineActiveKeys = derived;
    renderPipelineBarInner();
  }
}

export function renderPipelineBar(stage) {
  if (!runPipelineBarNode) return;
  // Map orchestration stages to pipeline step keys
  const stageMap = {
    starting: "collecting",
    collecting_data: "collecting",
    collecting_market_data: "collecting",
    analyzing_lines: "analyzing",
    analyzing_stale: "analyzing",
    generating_watchlist: "analyzing",
    llm_generating: "synthesis",
    composing_report: "synthesis",
    syncing_line_memory: "done",
    completed: "done",
    completed_degraded: "done",
    failed: "done"
  };
  const key = stageMap[String(stage || "").trim().toLowerCase()] || "collecting";
  // Only use orchestration stage as fallback — line-status-derived stages
  // (set by updatePipelineFromLineStatus) are more accurate/real-time.
  // Orchestration stage can advance the bar forward but never regress it.
  const STEP_RANK = { collecting: 0, analyzing: 1, synthesis: 2, done: 3 };
  const newRank = STEP_RANK[key] ?? 0;
  const currentMax = Math.max(...[...pipelineActiveKeys].map((k) => STEP_RANK[k] ?? 0), -1);
  if (newRank >= currentMax) {
    pipelineActiveKeys = new Set([key]);
  }
  renderPipelineBarInner();
}

function renderPipelineBarInner() {
  if (!runPipelineBarNode) return;
  // Find highest active key for monotonic progress
  let highestActive = 0;
  for (const key of pipelineActiveKeys) {
    highestActive = Math.max(highestActive, STEP_ORDER[key] ?? 0);
  }
  if (highestActive > pipelineHighWaterMark) pipelineHighWaterMark = highestActive;

  const isFailed = pipelineActiveKeys.has("failed");
  const isDone = pipelineActiveKeys.has("done");
  const parts = [];
  for (let i = 0; i < PIPELINE_STEPS.length; i++) {
    const step = PIPELINE_STEPS[i];
    let cls = "run-pipeline-step";
    let icon = "";
    const isActive = pipelineActiveKeys.has(step.key);
    if (step.key === "done" && isDone) {
      cls += isFailed ? " step-failed" : " step-done";
      icon = isFailed ? "\u2717 " : "\u2713 ";
    } else if (isActive) {
      cls += " step-active";
      icon = `<span class="pipeline-spinner"></span>`;
    } else if (i <= pipelineHighWaterMark && !isDone) {
      // Past steps that are no longer active
      cls += (STEP_ORDER[step.key] < highestActive && !isActive) ? " step-done" : "";
      if (cls.includes("step-done")) icon = "\u2713 ";
    }
    parts.push(`<span class="${cls}">${icon}${escapeHtml(step.label)}</span>`);
    if (i < PIPELINE_STEPS.length - 1) {
      parts.push(`<span class="run-pipeline-sep">\u203a</span>`);
    }
  }
  runPipelineBarNode.innerHTML = parts.join("");
  runPipelineBarNode.classList.remove("hidden");
}

export function clearRunPipelineBar() {
  pipelineHighWaterMark = -1;
  pipelineActiveKeys = new Set();
  if (runPipelineBarNode) {
    runPipelineBarNode.innerHTML = "";
    runPipelineBarNode.classList.add("hidden");
  }
  if (reportSynthesisCardNode) {
    reportSynthesisCardNode.classList.remove("synthesis-pending");
  }
}

function formatNum(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "\u2014";
  return n.toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 });
}

// ── Internal helpers ─────────────────────────────────────────────

function lineRowClass(status) {
  if (status === "done" || status === "failed" || status === "aborted") return "line-done";
  if (status === "collecting" || status === "analyzing" || status === "repairing") return "line-active";
  return "line-waiting";
}

function updateProgressCounter(lineStatus, tickers) {
  if (!positionsProgressNode) return;
  const realTickers = tickers.filter((t) => t !== "__synthesis__");
  const total = realTickers.length;
  const counts = { done: 0, failed: 0, collecting: 0, analyzing: 0, repairing: 0, waiting: 0 };
  for (const t of realTickers) {
    const s = parseLineStatus(lineStatus[t]);
    if (s === "done") counts.done++;
    else if (s === "failed" || s === "aborted") counts.failed++;
    else if (s === "collecting") counts.collecting++;
    else if (s === "repairing") counts.repairing++;
    else if (s === "analyzing") counts.analyzing++;
    else counts.waiting++;
  }
  const parts = [`${counts.done}/${total} done`];
  if (counts.failed > 0) parts.push(`${counts.failed} failed`);
  if (counts.collecting > 0) parts.push(`${counts.collecting} collecting`);
  if (counts.analyzing > 0) parts.push(`${counts.analyzing} analyzing`);
  if (counts.repairing > 0) parts.push(`${counts.repairing} repairing`);
  if (counts.waiting > 0) parts.push(`${counts.waiting} waiting`);
  positionsProgressNode.textContent = parts.join(" \u00b7 ");
  positionsProgressNode.classList.remove("hidden");
}

function updateSynthesisCardDuringRun(lineStatus, tickers) {
  if (!activeRunInProgress) return;
  const synthesis = document.getElementById("report-synthesis");
  if (!synthesis || !reportSynthesisCardNode) return;
  reportSynthesisCardNode.classList.add("synthesis-pending");

  // Check for synthesis generation progress (written by LLM streaming)
  const synthProgress = lineStatus["__synthesis__"];
  if (synthProgress && typeof synthProgress === "object" && synthProgress.progress) {
    synthesis.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>Generating global synthesis\u2026 ${synthProgress.progress}</span>`;
    return;
  }

  const realTickers = tickers.filter((t) => t !== "__synthesis__");
  const total = realTickers.length;
  const done = realTickers.filter((t) => {
    const s = parseLineStatus(lineStatus[t]);
    return s === "done" || s === "failed" || s === "aborted";
  }).length;
  if (done < total) {
    synthesis.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>Analyzing positions\u2026 ${done}/${total} lines complete</span>`;
  } else {
    synthesis.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>All lines analyzed \u2014 waiting for global synthesis\u2026</span>`;
  }
}

function parseLineStatus(raw) {
  return typeof raw === "object" ? (raw?.status || "unknown") : String(raw || "");
}

function renderLiveStatusChip(raw) {
  const status = parseLineStatus(raw);
  const errorMsg = typeof raw === "object" ? (raw?.error || "") : "";
  const progressMsg = typeof raw === "object" ? (raw?.progress || "") : "";
  const spinner = (status === "collecting" || status === "analyzing" || status === "repairing") ? `<span class="pipeline-spinner"></span>` : "";
  const chipClass =
    status === "done" ? "s-completed" :
    status === "analyzing" ? "s-analyzing" :
    status === "repairing" ? "s-repairing" :
    status === "collecting" ? "s-collecting" :
    (status === "failed" || status === "aborted") ? "s-failed" :
    "s-queued";
  const label =
    status === "done" ? "\u2713 Done" :
    status === "collecting" ? "Collecting\u2026" :
    status === "analyzing" ? "Analyzing\u2026" :
    status === "repairing" ? "Repairing\u2026" :
    status === "failed" ? "\u2717 Failed" :
    status === "aborted" ? "Aborted" :
    status === "waiting" ? "Queued" :
    status || "\u2014";
  const chip = `<span class="pipeline-chip ${chipClass}">${spinner}${escapeHtml(label)}</span>`;
  if (errorMsg) {
    return `${chip}<span class="pipeline-error">${escapeHtml(errorMsg)}</span>`;
  }
  return chip;
}
