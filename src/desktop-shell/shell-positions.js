/**
 * Shell Positions — positions table rendering, watchlist rows, signal badges.
 *
 * Extracted from shell-layout.js for single-responsibility. Renders the
 * static positions table from report data (not live run updates).
 */

import { formatCurrency, escapeHtml, truncate } from "/desktop-shell/ui-display-utils.js";
import { isActiveRunInProgress } from "/desktop-shell/shell-live-run.js";

// ── DOM refs ──────────────────────────────────────────────────────

const positionsTbodyNode = document.getElementById("positions-tbody");
const positionsEmptyNode = document.getElementById("positions-empty");
const positionsProgressNode = document.getElementById("positions-progress");

// ── Positions table ──────────────────────────────────────────────

export function renderPositionsTable(viewModel, dashboardPayload) {
  if (!positionsTbodyNode) return;

  const snapshot = dashboardPayload?.snapshot || {};
  const latestRun = snapshot.latest_run || {};

  // During active run, if the user is viewing the active run (not browsing a different one),
  // push events exclusively manage positions — skip table rebuild to prevent flickering.
  const viewingActiveRun = isActiveRunInProgress() &&
    (!viewModel?.selectedRunId || viewModel.selectedRunId === latestRun?.run_id);
  if (viewingActiveRun && latestRun?.orchestration?.status === "running") return;
  const positions = latestRun?.portfolio?.positions || [];
  const recommendations = viewModel?.recommendations || [];
  const isRunning = latestRun?.orchestration?.status === "running";
  const stage = latestRun?.orchestration?.stage || "";
  const collectionProgress = latestRun?.orchestration?.collection_progress || {};
  const lineProgress = latestRun?.orchestration?.line_progress || {};

  // Don't overwrite progress when live polling is managing it
  if (positionsProgressNode && !isActiveRunInProgress()) {
    if (isRunning) {
      const completed = (lineProgress.completed || 0);
      const total = (lineProgress.total || positions.length);
      if (total > 0) {
        positionsProgressNode.textContent = `Analyzing ${completed} of ${total} positions...`;
      } else {
        const stageLabel = stage === "collecting_data" ? "Collecting portfolio data..."
          : stage === "bootstrapping" ? "Bootstrapping analysis..."
          : "Starting analysis...";
        positionsProgressNode.textContent = stageLabel;
      }
      positionsProgressNode.classList.remove("hidden");
    } else {
      positionsProgressNode.classList.add("hidden");
    }
  }

  if (positions.length === 0 && recommendations.length === 0) {
    // Don't wipe bootstrapped rows from renderLivePositions during a running analysis
    const hasBootstrappedRows = positionsTbodyNode.querySelectorAll("tr.pos-main-row").length > 0;
    if (isRunning && hasBootstrappedRows) return;
    if (!isRunning) positionsTbodyNode.innerHTML = "";
    if (positionsEmptyNode && !isRunning) positionsEmptyNode.classList.remove("hidden");
    return;
  }
  if (positionsEmptyNode) positionsEmptyNode.classList.add("hidden");

  const recByTicker = new Map();
  for (const rec of recommendations) {
    recByTicker.set((rec.ticker || "").toUpperCase(), rec);
  }

  // Compute portfolio total for weight calculation
  const portfolioTotal = latestRun?.portfolio?.valeur_totale
    || positions.reduce((sum, p) => sum + (p.valeur_actuelle || 0), 0)
    || 1;

  positionsTbodyNode.innerHTML = "";
  for (const pos of positions) {
    const ticker = (pos.ticker || "").toUpperCase();
    const rec = recByTicker.get(ticker);
    const pv = pos.plus_moins_value || 0;
    const pvPct = pos.plus_moins_value_pct || 0;
    const pvClass = pv >= 0 ? "pos-pv-positive" : "pos-pv-negative";
    const value = pos.valeur_actuelle || 0;
    const weight = portfolioTotal > 0 ? (value * 100 / portfolioTotal) : 0;

    const tr = document.createElement("tr");
    tr.className = "pos-main-row";
    tr.dataset.ticker = ticker;

    // Value + weight badge next to ticker
    const valueBadge = value > 0
      ? ` <span class="pos-value-badge">${formatCurrency(value)} <span class="pos-weight">(${weight.toFixed(1)}%)</span></span>`
      : "";
    // Next analysis date
    const reanalyseAfter = rec?.reanalyseAfter || "";
    const reanalyseCell = reanalyseAfter
      ? `<span class="pos-reanalyse">\u{1F4C5} ${escapeHtml(reanalyseAfter)}</span>`
      : "";

    // Main row
    tr.innerHTML = `
      <td><strong>${escapeHtml(ticker)}</strong>${valueBadge}</td>
      <td>${escapeHtml(pos.nom || "")}</td>
      <td class="num">${pos.quantite ?? ""}</td>
      <td class="num">${formatNum(pos.prix_revient)}</td>
      <td class="num">${formatNum(pos.prix_actuel)}</td>
      <td class="num ${pvClass}">${pv >= 0 ? "+" : ""}${formatNum(pv)}</td>
      <td class="num ${pvClass}">${pvPct >= 0 ? "+" : ""}${pvPct.toFixed(1)}%</td>
      <td>${renderSignalCell(rec, isRunning, stage, ticker, collectionProgress, lineProgress, latestRun?.orchestration?.status)}</td>
      <td class="num">${reanalyseCell}</td>
    `;
    positionsTbodyNode.appendChild(tr);

    // Recommendation summary sub-row
    if (rec && rec.summary) {
      const subTr = document.createElement("tr");
      subTr.className = "pos-sub-row";
      subTr.dataset.ticker = ticker;
      const reanalyse = rec.reanalyseAfter ? ` · <span class="reanalyse-hint">next: ${escapeHtml(rec.reanalyseAfter)}</span>` : "";
      subTr.innerHTML = `<td colspan="9" class="pos-rec-row">${escapeHtml(rec.signal || "")} · ${escapeHtml(rec.conviction || "")} · ${escapeHtml(truncate(rec.summary, 120))}${reanalyse}</td>`;
      positionsTbodyNode.appendChild(subTr);
    }
  }

  // Watchlist items — append after positions with separator
  const watchlistRecs = recommendations.filter((r) => r.type === "watchlist");
  if (watchlistRecs.length > 0) {
    const sepTr = document.createElement("tr");
    sepTr.className = "pos-watchlist-separator";
    sepTr.innerHTML = `<td colspan="9" class="watchlist-separator-cell">\u{1F50D} Watchlist — Opportunities (not held)</td>`;
    positionsTbodyNode.appendChild(sepTr);

    for (const rec of watchlistRecs) {
      const tr = document.createElement("tr");
      tr.className = "pos-main-row pos-watchlist-row";
      tr.dataset.ticker = rec.ticker;
      tr.innerHTML = `
        <td><strong>${escapeHtml(rec.ticker)}</strong> <span class="watchlist-badge">watchlist</span></td>
        <td>${escapeHtml(rec.name || "")}</td>
        <td class="num">—</td>
        <td class="num">—</td>
        <td class="num">—</td>
        <td class="num">—</td>
        <td class="num">—</td>
        <td>${renderSignalBadge(rec.signal)}</td>
        <td></td>
      `;
      positionsTbodyNode.appendChild(tr);

      if (rec.summary) {
        const subTr = document.createElement("tr");
        subTr.className = "pos-sub-row pos-watchlist-row";
        subTr.dataset.ticker = rec.ticker;
        subTr.innerHTML = `<td colspan="9" class="pos-rec-row">${escapeHtml(rec.signal || "")} · ${escapeHtml(rec.conviction || "")} · ${escapeHtml(truncate(rec.summary, 120))}</td>`;
        positionsTbodyNode.appendChild(subTr);
      }
    }
  }

  // Wire row clicks to open inspect modal
  positionsTbodyNode.querySelectorAll("tr[data-ticker]").forEach((row) => {
    row.addEventListener("click", () => {
      const ticker = row.dataset.ticker;
      const rec = recByTicker.get(ticker) || watchlistRecs.find((r) => r.ticker === ticker);
      if (rec && window.__openLineMemoryModal) {
        window.__openLineMemoryModal(rec);
      }
    });
  });
}

// ── Signal badge ─────────────────────────────────────────────────

export function renderSignalBadge(signal) {
  const s = (signal || "?").toUpperCase();
  const tone = s.includes("ACHAT") || s.includes("RENFORC") ? "tone-buy"
    : s.includes("VENTE") || s.includes("ALLEG") ? "tone-sell"
    : "tone-neutral";
  return `<span class="signal-badge ${tone}">${escapeHtml(signal || "?")}</span>`;
}

function renderSignalCell(rec, isRunning, stage, ticker, collectionProgress, lineProgress, runStatus) {
  const isAborted = runStatus === "aborted";
  if (!rec && isRunning) {
    if (stage === "collecting_data") {
      return "<span class=\"pipeline-chip s-collecting\">Collecting...</span>";
    }
    return "<span class=\"pipeline-chip s-waiting\">Waiting</span>";
  }
  if (!rec && isAborted) {
    return "<span class=\"pipeline-chip s-failed\">Aborted</span>";
  }
  if (!rec) return "";

  const signal = (rec.signal || "").toUpperCase();
  if (signal === "COLLECTED") {
    if (isRunning) {
      return "<span class=\"pipeline-chip s-analyzing\">Analyzing...</span>";
    }
    if (isAborted) {
      return "<span class=\"pipeline-chip s-failed\">Aborted</span>";
    }
    return "<span class=\"pipeline-chip s-waiting\">Collected</span>";
  }

  const tone = signal.includes("ACHAT") || signal.includes("ACHETER") || signal.includes("BUY") || signal.includes("RENFORC") ? "tone-buy"
    : signal.includes("VENTE") || signal.includes("VENDR") || signal.includes("SELL") || signal.includes("ALLEG") ? "tone-sell"
    : "tone-neutral";
  return `<span class="signal-badge ${tone}">${escapeHtml(rec.signal || "?")}</span>`;
}

// ── Helpers ──────────────────────────────────────────────────────

function formatNum(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  return n.toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 });
}
