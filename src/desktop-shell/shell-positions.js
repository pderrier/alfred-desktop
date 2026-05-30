/**
 * Shell Positions — positions table rendering, watchlist rows, signal badges.
 *
 * Extracted from shell-layout.js for single-responsibility. Renders the
 * static positions table from report data (not live run updates).
 */

import { formatCurrency, escapeHtml, truncate, signalToneClass } from "/desktop-shell/ui-display-utils.js";
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
  // Only skip when selectedRunId strictly equals the active run — the !selectedRunId fallback
  // was too broad and prevented proper clearing when navigating between runs.
  if (isActiveRunInProgress() && latestRun?.orchestration?.status === "running") return;
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
    // When every market provider fails, `sync_position_from_market` leaves
    // `prix_actuel = 0` and `market.source = "none"`. Rendering "0 €" /
    // "-100%" then looks like the position is genuinely worthless — far
    // more misleading than the previous (stale) behaviour. Detect that
    // "no price" state on positions with a real holding (qty>0) and render
    // an em-dash sentinel for every price-derived cell. Watchlist items
    // legitimately carry qty=0 and must keep their existing neutral cells.
    const priceUnavailable = isPriceUnavailable(pos);
    const pv = pos.plus_moins_value || 0;
    const pvPct = pos.plus_moins_value_pct || 0;
    const pvClass = pv >= 0 ? "pos-pv-positive" : "pos-pv-negative";
    const value = pos.valeur_actuelle || 0;
    const weight = portfolioTotal > 0 ? (value * 100 / portfolioTotal) : 0;

    const tr = document.createElement("tr");
    tr.className = "pos-main-row";
    tr.dataset.ticker = ticker;

    // Value + weight badge next to ticker. With no price we cannot compute
    // a meaningful valeur_actuelle (qty * 0 = 0), so suppress the badge
    // rather than show a misleading "0 €".
    const valueBadge = value > 0 && !priceUnavailable
      ? ` <span class="pos-value-badge">${formatCurrency(value)} <span class="pos-weight">(${weight.toFixed(1)}%)</span></span>`
      : "";
    // Next analysis date — overdue check
    const reanalyseAfter = rec?.reanalyseAfter || "";
    const isOverdue = reanalyseAfter && reanalyseAfter.length >= 10
      && reanalyseAfter.slice(0, 10) <= new Date().toISOString().slice(0, 10);
    const reanalyseIcon = isOverdue ? "\u23F0" : "\u{1F4C5}";
    const reanalyseCls = isOverdue ? "pos-reanalyse pos-reanalyse-overdue" : "pos-reanalyse";
    const reanalyseTitle = isOverdue && rec?.reanalyseReason
      ? ` title="${escapeHtml(rec.reanalyseReason)}"`
      : "";
    const reanalyseCell = reanalyseAfter
      ? `<span class="${reanalyseCls}"${reanalyseTitle}>${reanalyseIcon} ${escapeHtml(reanalyseAfter)}</span>`
      : "";

    // Price / P&L cells. With no spot price pv/pvPct are not trustworthy
    // either (they derive from prix_actuel), so substitute the em-dash
    // sentinel for all four price-derived cells and drop the PnL tone
    // class — there is no sign to communicate.
    const priceCell = priceUnavailable
      ? `<span class="pos-price-na" title="Prix indisponible — tous les fournisseurs ont échoué">—</span>`
      : formatNum(pos.prix_actuel);
    const pvCellText = priceUnavailable ? "—" : `${pv >= 0 ? "+" : ""}${formatNum(pv)}`;
    const pvPctCellText = priceUnavailable ? "—" : `${pvPct >= 0 ? "+" : ""}${pvPct.toFixed(1)}%`;
    const pvCellClass = priceUnavailable ? "num" : `num ${pvClass}`;

    // Main row
    tr.innerHTML = `
      <td><strong>${escapeHtml(ticker)}</strong>${valueBadge}</td>
      <td>${escapeHtml(pos.nom || "")}</td>
      <td class="num">${pos.quantite ?? ""}</td>
      <td class="num">${formatNum(pos.prix_revient)}</td>
      <td class="num">${priceCell}</td>
      <td class="${pvCellClass}">${pvCellText}</td>
      <td class="${pvCellClass}">${pvPctCellText}</td>
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
      // Watchlist Curation v2 (D1): a verdict the LLM ECARTE-d (rejected) is
      // rendered de-emphasised — Alfred validated the proposal as a
      // non-opportunity. Driven by signal OR verdict_validation so the row
      // dims even if only one field is present.
      const discarded = String(rec.signal || "").toUpperCase() === "ECARTER"
        || String(rec.verdictValidation || "").toLowerCase() === "ecartee";
      const tr = document.createElement("tr");
      tr.className = `pos-main-row pos-watchlist-row${discarded ? " pos-watchlist-discarded" : ""}`;
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
        subTr.className = `pos-sub-row pos-watchlist-row${discarded ? " pos-watchlist-discarded" : ""}`;
        subTr.dataset.ticker = rec.ticker;
        const discardPrefix = discarded ? "écartée par Alfred — " : "";
        subTr.innerHTML = `<td colspan="9" class="pos-rec-row">${escapeHtml(discardPrefix)}${escapeHtml(rec.signal || "")} · ${escapeHtml(rec.conviction || "")} · ${escapeHtml(truncate(rec.summary, 120))}</td>`;
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
  return `<span class="signal-badge ${signalToneClass(signal)}">${escapeHtml(signal || "?")}</span>`;
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

  return `<span class="signal-badge ${signalToneClass(signal)}">${escapeHtml(rec.signal || "?")}</span>`;
}

// ── Helpers ──────────────────────────────────────────────────────

function formatNum(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  return n.toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 });
}

/**
 * Detect the "no provider price" state.
 *
 * Backend contract: when every market provider fails for a ticker,
 * `native_collection_helpers::sync_position_from_market` rejects the
 * fallback (`source == "none"`) and leaves `position.prix_actuel = 0`.
 * The UI must distinguish that genuinely-unknown-price state from a real
 * zero so we don't render "0 €" / "-100%" for a live holding.
 *
 * Heuristic: a position with a real holding (`quantite > 0`) but a
 * zero/null `prix_actuel` is the no-price sentinel. Watchlist items
 * legitimately carry `quantite = 0` and must NOT trigger the sentinel.
 *
 * Exported so the line modal can apply the same rule consistently.
 */
export function isPriceUnavailable(position) {
  if (!position || typeof position !== "object") return false;
  const qty = Number(position.quantite);
  if (!Number.isFinite(qty) || qty <= 0) return false;
  const price = Number(position.prix_actuel);
  return !Number.isFinite(price) || price === 0;
}
