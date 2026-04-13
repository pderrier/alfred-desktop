/**
 * Shell Layout — sidebar, command bar, status pill, gear panel, toasts.
 *
 * Positions table is in shell-positions.js, live run UI in shell-live-run.js.
 * This module re-exports their public API for backward compatibility with app.js.
 */

import { formatCurrency, escapeHtml, truncate } from "/desktop-shell/ui-display-utils.js";
import { renderPositionsTable } from "/desktop-shell/shell-positions.js";
import {
  renderLivePositions,
  updateSingleLineProgress,
  renderTopBarProgress,
  renderPipelineBar,
  clearRunPipelineBar,
  setActiveRunInProgress,
  isActiveRunInProgress,
  setLiveRunContext,
  setLiveRunActiveId,
  getLiveRunActiveId,
  setLiveRunViewingId,
} from "/desktop-shell/shell-live-run.js";

// Re-export for backward compat with app.js imports
export { renderLivePositions, updateSingleLineProgress, renderTopBarProgress, renderPipelineBar, clearRunPipelineBar, setLiveRunContext, setLiveRunActiveId, setLiveRunViewingId };

// ── DOM refs ──────────────────────────────────────────────────────

const sidebarAccountsNode = document.getElementById("sidebar-accounts");
const sidebarEmptyNode = document.getElementById("sidebar-empty");
const sidebarSyncBtn = document.getElementById("sidebar-sync-btn");
const sidebarNewRunBtn = document.getElementById("sidebar-new-run");
const mainWelcomeNode = document.getElementById("main-welcome");
const mainRunViewNode = document.getElementById("main-run-view");
const overviewPanelNode = document.getElementById("tab-overview-panel");
const positionsTbodyNode = document.getElementById("positions-tbody");
const positionsEmptyNode = document.getElementById("positions-empty");
const positionsProgressNode = document.getElementById("positions-progress");
const statusPillNode = document.getElementById("status-pill");
const statusPillLabelNode = document.getElementById("status-pill-label");
const statusPopoverNode = document.getElementById("status-popover");
const popoverFinaryStatusNode = document.getElementById("popover-finary-status");
const popoverServicesNode = document.getElementById("popover-services");
const popoverSnapshotTsNode = document.getElementById("popover-snapshot-ts");
const popoverRunStatusNode = document.getElementById("popover-run-status");
const gearBtn = document.getElementById("gear-btn");
const gearPanelNode = document.getElementById("gear-panel");
const gearCloseBtn = document.getElementById("gear-close-btn");
const cmdRunAnalysis = document.getElementById("cmd-run-analysis");
const cmdConnectOpenai = document.getElementById("cmd-connect-openai");
const cmdSyncFinary = document.getElementById("cmd-sync-finary");
const cmdStopAnalysis = document.getElementById("cmd-stop-analysis");
const cmdRetrySynthesis = document.getElementById("cmd-retry-synthesis");
const wizardAccountSelect = document.getElementById("wizard-account-select");
const popoverReconnectBtn = document.getElementById("popover-reconnect-btn");
const toastContainer = document.getElementById("toast-container");
const reportSynthesisCardNode = document.getElementById("report-synthesis-card");

let selectedRunId = null;
let selectedAccount = null;
export function getSelectedAccount() { return selectedAccount; }

// ── Account accent colors ─────────────────────────────────────────
const ACCOUNT_COLORS = [
  "#8ecae6", // sky blue
  "#f4c46a", // warm gold
  "#95d5b2", // soft green
  "#dda0dd", // plum
  "#f08a77", // coral
  "#80ced6", // teal
  "#c9b1ff", // lavender
  "#f9c74f", // amber
];
const accountColorMap = new Map();
export function accountAccentColor(accountName) {
  if (!accountName) return "rgba(73,100,126,0.4)";
  if (!accountColorMap.has(accountName)) {
    accountColorMap.set(accountName, ACCOUNT_COLORS[accountColorMap.size % ACCOUNT_COLORS.length]);
  }
  return accountColorMap.get(accountName);
}
let onRunSelected = null;
let onAccountSelected = null;
let onOpenWizard = null;
let onConnectOpenai = null;
let onSyncFinary = null;
let onEditGuidelines = null;
let onStopAnalysis = null;
let onRetrySynthesis = null;

// ── Public API ────────────────────────────────────────────────────

export function initShellLayout({ openWizard, connectOpenai, syncFinary, stopAnalysis, retrySynthesis, selectRun, selectAccount, editGuidelines }) {
  onOpenWizard = openWizard;
  onConnectOpenai = connectOpenai;
  onSyncFinary = syncFinary;
  onEditGuidelines = editGuidelines;
  onStopAnalysis = stopAnalysis;
  onRetrySynthesis = retrySynthesis;
  onAccountSelected = selectAccount;
  onRunSelected = selectRun;

  cmdRunAnalysis?.addEventListener("click", () => onOpenWizard?.());
  sidebarNewRunBtn?.addEventListener("click", () => onOpenWizard?.());
  sidebarSyncBtn?.addEventListener("click", () => onOpenWizard?.());
  cmdConnectOpenai?.addEventListener("click", () => onConnectOpenai?.());
  cmdSyncFinary?.addEventListener("click", () => onSyncFinary?.());
  cmdStopAnalysis?.addEventListener("click", () => onStopAnalysis?.());
  cmdRetrySynthesis?.addEventListener("click", () => onRetrySynthesis?.());
  popoverReconnectBtn?.addEventListener("click", () => {
    statusPopoverNode?.classList.add("hidden");
    onSyncFinary?.();
  });

  statusPillNode?.addEventListener("click", toggleStatusPopover);
  gearBtn?.addEventListener("click", () => setGearPanelVisible(true));
  gearCloseBtn?.addEventListener("click", () => setGearPanelVisible(false));

  document.addEventListener("click", (event) => {
    if (statusPopoverNode && !statusPopoverNode.classList.contains("hidden") &&
        !statusPopoverNode.contains(event.target) && event.target !== statusPillNode) {
      statusPopoverNode.classList.add("hidden");
    }
  });
}

export function renderSidebar(dashboardPayload) {
  const snapshot = dashboardPayload?.snapshot || {};
  const runs = Array.isArray(snapshot.runs) ? snapshot.runs : [];
  const latestRun = snapshot.latest_run || null;
  const finaryMeta = snapshot.latest_finary_snapshot || {};
  // Merge accounts from snapshot metadata + inferred from positions, deduplicated
  const accountMap = new Map();

  // Source 1: snapshot metadata accounts
  const metaAccounts = Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [];
  for (const acct of metaAccounts) {
    const name = acct.name || "";
    if (!name) continue;
    accountMap.set(name, { name, cash: acct.cash || 0, total_value: acct.total_value || 0, total_gain: acct.total_gain || 0 });
  }

  // Source 2: infer from all run positions (catches CSV accounts + stale snapshots)
  for (const run of runs) {
    const account = run.account || "";
    if (account && !accountMap.has(account)) {
      accountMap.set(account, { name: account, cash: 0, total_value: 0, total_gain: 0 });
    }
  }
  const positions = latestRun?.portfolio?.positions || [];
  for (const pos of positions) {
    const key = pos.compte || pos.account || "";
    if (!key || accountMap.has(key)) continue;
    accountMap.set(key, { name: key, cash: 0, total_value: 0, total_gain: 0 });
  }

  const accounts = Array.from(accountMap.values());

  const hasAccounts = accounts.length > 0 || runs.length > 0;

  if (!hasAccounts) {
    if (sidebarAccountsNode) sidebarAccountsNode.innerHTML = "";
    if (sidebarEmptyNode) sidebarEmptyNode.classList.remove("hidden");
    showMainWelcome(true);
    return;
  }

  if (sidebarEmptyNode) sidebarEmptyNode.classList.add("hidden");

  const accountFolders = buildAccountRunMap(accounts, runs);
  if (sidebarAccountsNode) {
    sidebarAccountsNode.innerHTML = "";
    for (const acct of accountFolders) {
      sidebarAccountsNode.appendChild(renderAccountFolder(acct));
    }
  }

  // Don't auto-select on first load — wait for user to pick an account/run
  if (selectedRunId) {
    highlightSelectedRun();
  }
}

export function renderAccountView(accountName, dashboardPayload, snapshotPositions = null) {
  const mainWelcome = document.getElementById("main-welcome");
  const mainRunView = document.getElementById("main-run-view");
  const mainAccountView = document.getElementById("main-account-view");
  if (mainWelcome) mainWelcome.classList.add("hidden");
  if (mainRunView) mainRunView.classList.add("hidden");
  if (mainAccountView) mainAccountView.classList.remove("hidden");

  const label = document.getElementById("account-view-label");
  if (label) label.textContent = accountName;

  // Populate positions from snapshot for this account
  const snapshot = dashboardPayload?.snapshot || {};
  const finaryMeta = snapshot.latest_finary_snapshot || {};
  const metaAccounts = Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [];
  const acct = metaAccounts.find((a) => a.name === accountName);

  // Use snapshot positions if provided, otherwise try latest run
  let positions = Array.isArray(snapshotPositions) && snapshotPositions.length > 0
    ? snapshotPositions
    : (snapshot.latest_run?.portfolio?.positions || [])
        .filter((p) => (p.compte || p.account || "") === accountName);

  const tbody = document.getElementById("account-positions-tbody");
  const empty = document.getElementById("account-positions-empty");
  if (!tbody) return;
  tbody.innerHTML = "";

  if (positions.length === 0) {
    if (empty) empty.classList.remove("hidden");
    return;
  }
  if (empty) empty.classList.add("hidden");

  for (const pos of positions) {
    const pv = pos.plus_moins_value || 0;
    const pvPct = pos.plus_moins_value_pct || 0;
    const pvClass = pv >= 0 ? "pos-pv-positive" : "pos-pv-negative";
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td><strong>${escapeHtml(pos.ticker || "")}</strong></td>
      <td>${escapeHtml(pos.nom || "")}</td>
      <td class="num">${pos.quantite ?? ""}</td>
      <td class="num">${formatNum(pos.prix_revient)}</td>
      <td class="num">${formatNum(pos.prix_actuel)}</td>
      <td class="num ${pvClass}">${pv >= 0 ? "+" : ""}${formatNum(pv)}</td>
      <td class="num ${pvClass}">${pvPct >= 0 ? "+" : ""}${pvPct.toFixed(1)}%</td>
    `;
    tbody.appendChild(tr);
  }
}

export function renderMainPanel(viewModel, dashboardPayload) {
  const hasRun = selectedRunId != null;
  const hasPositions = viewModel?.recommendations?.length > 0;
  const hasSynthesis = viewModel?.synthesis && viewModel.synthesis !== "No synthesis yet.";
  const hasKpi = viewModel?.value > 0 || viewModel?.gain !== 0;
  const hasData = hasRun && (hasPositions || hasSynthesis || hasKpi);
  // Hide account view when showing a run
  const mainAccountView = document.getElementById("main-account-view");
  if (mainAccountView && hasRun) mainAccountView.classList.add("hidden");
  showMainWelcome(!hasRun);

  if (hasData) {
    if (overviewPanelNode) overviewPanelNode.classList.remove("hidden");
  } else {
    if (overviewPanelNode) overviewPanelNode.classList.add("hidden");
  }

  // Show account + run context in header
  const snapshot = dashboardPayload?.snapshot || {};
  const latestRun = snapshot.latest_run || {};
  const accountLabel = document.getElementById("main-account-label");
  const runLabel = document.getElementById("main-run-label");
  if (accountLabel) {
    accountLabel.textContent = latestRun.account || "";
  }
  if (runLabel) {
    const status = latestRun.orchestration?.status || "";
    const ts = latestRun.updated_at ? new Date(latestRun.updated_at).toLocaleString() : "";
    runLabel.textContent = ts ? `${status} · ${ts}` : "";
  }

  if (hasRun) renderPositionsTable(viewModel, dashboardPayload);
}

let activeRunLineStatus = null;
let activeRunDashboardPayload = null;
let activeRunLastStage = null;

export function setActiveRunState(active) {
  setActiveRunInProgress(active);
  renderStatusPillFromState();
  renderActiveRunIndicator();
}

export function getStashedLineStatus() { return activeRunLineStatus; }

export function stashLiveRunState(lineStatus, dashboardPayload, stage) {
  activeRunLineStatus = lineStatus;
  activeRunDashboardPayload = dashboardPayload;
  if (stage) activeRunLastStage = stage;
}

export function restoreLiveRunView() {
  if (!isActiveRunInProgress()) return;
  // Reset viewing to the active run so push events resume updating the table
  setLiveRunViewingId(getLiveRunActiveId());
  clearReportSections();
  showRunView();
  if (activeRunLineStatus) {
    renderLivePositions(activeRunLineStatus, activeRunDashboardPayload);
  }
  if (activeRunLastStage) {
    renderPipelineBar(activeRunLastStage);
  }
}

function renderActiveRunIndicator() {
  let indicator = document.getElementById("sidebar-active-run");
  if (!isActiveRunInProgress()) {
    if (indicator) indicator.classList.add("hidden");
    return;
  }
  if (!indicator && sidebarAccountsNode) {
    indicator = document.createElement("button");
    indicator.id = "sidebar-active-run";
    indicator.type = "button";
    indicator.className = "active-run-indicator";
    indicator.addEventListener("click", () => restoreLiveRunView());
    sidebarAccountsNode.parentNode.insertBefore(indicator, sidebarAccountsNode);
  }
  if (indicator) {
    indicator.innerHTML = `<span class="pipeline-spinner"></span> Analysis running\u2026`;
    indicator.classList.remove("hidden");
  }
}

// ── Stale Reanalysis Badge (Phase 1b) ─────────────────────────

export async function refreshStaleBadge() {
  try {
    const { invoke } = window.__TAURI__.core;
    const result = await invoke("get_stale_positions_local");
    const count = result?.stale_count || 0;
    let badge = document.getElementById("sidebar-stale-badge");
    if (count > 0) {
      if (!badge && sidebarAccountsNode) {
        badge = document.createElement("div");
        badge.id = "sidebar-stale-badge";
        badge.className = "stale-badge";
        sidebarAccountsNode.parentNode.insertBefore(badge, sidebarAccountsNode.nextSibling);
      }
      if (badge) {
        badge.textContent = `\u23F0 ${count} position${count > 1 ? "s" : ""} need${count === 1 ? "s" : ""} reanalysis`;
        badge.classList.remove("hidden");
      }
    } else if (badge) {
      badge.classList.add("hidden");
    }
  } catch (e) {
    // Silently ignore — non-critical UI feature
  }
}

let lastStackHealth = null;

function renderStatusPillFromState() {
  if (isActiveRunInProgress()) {
    if (statusPillNode) statusPillNode.className = "status-pill tone-healthy";
    if (statusPillLabelNode) statusPillLabelNode.textContent = "Running...";
    return;
  }
  if (lastStackHealth) {
    const status = lastStackHealth.status || "unknown";
    const tone = status === "healthy" ? "tone-healthy" : status === "degraded" ? "tone-degraded" : "tone-error";
    const label = status === "healthy" ? "Ready" : status === "degraded" ? "Degraded" : "API Down";
    if (statusPillNode) statusPillNode.className = `status-pill ${tone}`;
    if (statusPillLabelNode) statusPillLabelNode.textContent = label;
  } else {
    if (statusPillNode) statusPillNode.className = "status-pill tone-unknown";
    if (statusPillLabelNode) statusPillLabelNode.textContent = "Checking...";
  }
}

export function renderStatusPill(stackHealth, finarySession, latestRunSummary) {
  if (stackHealth) {
    lastStackHealth = stackHealth;
    // Populate popover services list
    if (popoverServicesNode && stackHealth.services) {
      popoverServicesNode.innerHTML = stackHealth.services.map((svc) => {
        const ok = svc.ok || svc.accepted;
        const dot = ok ? "\u2713" : "\u2717";
        const color = ok ? "#2f8f5d" : svc.live ? "#d4a12a" : "#ba4b3a";
        const status = svc.status || (ok ? "ok" : "down");
        return `<div style="display:flex;gap:0.4rem;font-size:0.75rem"><span style="color:${color}">${dot}</span><span>${escapeHtml(svc.name)}</span><span style="color:var(--sea-muted)">${escapeHtml(status)}</span></div>`;
      }).join("");
    }
  }
  renderStatusPillFromState();

  // Popover content
  if (popoverFinaryStatusNode) {
    popoverFinaryStatusNode.textContent = "All services native (built-in)";
  }
  if (popoverRunStatusNode) {
    popoverRunStatusNode.textContent = `Run: ${latestRunSummary?.status || "idle"}`;
  }
}

export function renderStatusPopoverServices(healthEntries) {
  if (!popoverServicesNode) return;
  if (!Array.isArray(healthEntries) || healthEntries.length === 0) {
    popoverServicesNode.innerHTML = "<p class=\"popover-muted\" style=\"font-size:0.78rem\">No service data.</p>";
    return;
  }
  popoverServicesNode.innerHTML = healthEntries.map((entry) => {
    const name = entry.name || entry.service || "unknown";
    const ok = entry.ok || entry.ready;
    const dot = ok ? "🟢" : "🔴";
    return `<p style="font-size:0.78rem;margin:0">${dot} ${escapeHtml(name)}</p>`;
  }).join("");
}

export function updateSnapshotTimestamp(finaryMeta) {
  if (!popoverSnapshotTsNode) return;
  const ts = finaryMeta?.saved_at;
  popoverSnapshotTsNode.textContent = `Snapshot: ${ts ? new Date(ts).toLocaleString() : "none"}`;
}

/**
 * Clear report sections to "pending" state — used when starting a new run
 * or restoring a live run view to avoid showing stale completed-report data.
 */
function clearReportSections() {
  const synthesis = document.getElementById("report-synthesis");
  if (synthesis) {
    synthesis.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>Waiting for line analyses to complete\u2026</span>`;
  }
  const provenance = document.getElementById("report-provenance");
  if (provenance) provenance.textContent = "";
  const actionsNow = document.getElementById("actions-now");
  if (actionsNow) actionsNow.innerHTML = `<div class="empty-hint" style="opacity:0.5">Will be generated after global synthesis.</div>`;
  const nextAnalysis = document.getElementById("next-analysis");
  if (nextAnalysis) { nextAnalysis.textContent = ""; nextAnalysis.classList.add("hidden"); }
  const watchlistSummary = document.getElementById("watchlist-summary");
  if (watchlistSummary) { watchlistSummary.textContent = ""; watchlistSummary.classList.add("hidden"); }
  // Phase 2b: remove theme concentration card
  const themeCard = document.getElementById("theme-concentration-card");
  if (themeCard) themeCard.remove();
  if (positionsTbodyNode) positionsTbodyNode.innerHTML = "";
  if (positionsEmptyNode) positionsEmptyNode.classList.add("hidden");
  if (overviewPanelNode) overviewPanelNode.classList.add("hidden");
  if (reportSynthesisCardNode) reportSynthesisCardNode.classList.add("synthesis-pending");
  document.getElementById("report-synthesis-card")?.querySelector(".synthesis-ask-btn")?.remove();
}

export function showRunView({ starting = false, live = false } = {}) {
  const mainAccountView = document.getElementById("main-account-view");
  if (mainAccountView) mainAccountView.classList.add("hidden");
  if (mainWelcomeNode) mainWelcomeNode.classList.add("hidden");
  if (mainRunViewNode) mainRunViewNode.classList.remove("hidden");
  // Visual distinction: live analysis has a subtle accent border
  if (mainRunViewNode) {
    mainRunViewNode.classList.toggle("live-run", live || starting);
  }
  if (starting) {
    // Clear stale live run state from previous run/account
    activeRunLineStatus = null;
    activeRunDashboardPayload = null;
    activeRunLastStage = null;
    // New run starting — user is viewing it (null = follow active run)
    setLiveRunViewingId(null);
    // Clear stale data from previous run
    clearReportSections();
    const accountLabel = document.getElementById("main-account-label");
    if (accountLabel) accountLabel.textContent = "";
    const runLabel = document.getElementById("main-run-label");
    if (runLabel) runLabel.textContent = "Analysis in progress\u2026";
    if (positionsProgressNode) {
      positionsProgressNode.textContent = "Starting analysis\u2026";
      positionsProgressNode.classList.remove("hidden");
    }
    renderPipelineBar("starting");
  }
}

export function setStopAnalysisVisible(visible) {
  if (cmdStopAnalysis) {
    cmdStopAnalysis.classList.toggle("hidden", !visible);
  }
  if (cmdRunAnalysis) {
    cmdRunAnalysis.classList.toggle("hidden", visible);
  }
}

export function setRetrySynthesisVisible(visible) {
  if (cmdRetrySynthesis) {
    cmdRetrySynthesis.classList.toggle("hidden", !visible);
  }
}

export function showToast(message, tone = "") {
  if (!toastContainer) return;
  const item = document.createElement("div");
  item.className = `toast-item ${tone ? `tone-${tone}` : ""}`.trim();
  item.textContent = message;
  toastContainer.appendChild(item);
  setTimeout(() => item.remove(), 5000);
}

export function showErrorModal(title, message, hint) {
  let modal = document.getElementById("error-modal");
  if (!modal) {
    modal = document.createElement("div");
    modal.id = "error-modal";
    modal.className = "error-modal-overlay";
    modal.innerHTML = `
      <div class="error-modal-content">
        <h3 class="error-modal-title"></h3>
        <p class="error-modal-message"></p>
        <p class="error-modal-hint"></p>
        <button class="error-modal-dismiss">OK</button>
      </div>
    `;
    document.body.appendChild(modal);
    modal.querySelector(".error-modal-dismiss").addEventListener("click", () => {
      modal.classList.add("hidden");
    });
    modal.addEventListener("click", (e) => {
      if (e.target === modal) modal.classList.add("hidden");
    });
  }
  modal.querySelector(".error-modal-title").textContent = title || "Error";
  modal.querySelector(".error-modal-message").textContent = message || "";
  const hintNode = modal.querySelector(".error-modal-hint");
  if (hint) {
    hintNode.textContent = hint;
    hintNode.classList.remove("hidden");
  } else {
    hintNode.classList.add("hidden");
  }
  modal.classList.remove("hidden");
}

export function populateWizardAccounts(accounts) {
  if (!wizardAccountSelect) return;
  wizardAccountSelect.innerHTML = "";
  for (const acct of accounts) {
    const name = acct.name || "";
    if (!name) continue;
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    wizardAccountSelect.appendChild(opt);
  }
  // Always offer a "New account..." option for CSV imports
  const newOpt = document.createElement("option");
  newOpt.value = "__new__";
  newOpt.textContent = "+ New account...";
  wizardAccountSelect.appendChild(newOpt);
  // If "New account" is the only option, show the name input immediately
  const newAccountInput = document.getElementById("wizard-new-account-name");
  if (newAccountInput) {
    const isNew = wizardAccountSelect.value === "__new__";
    newAccountInput.classList.toggle("hidden", !isNew);
  }
}

export function getSelectedWizardAccount() {
  const val = wizardAccountSelect?.value || "";
  if (val === "__new__") {
    return document.getElementById("wizard-new-account-name")?.value?.trim() || "";
  }
  return val;
}

// ── Internal ──────────────────────────────────────────────────────

function showMainWelcome(show) {
  if (mainWelcomeNode) mainWelcomeNode.classList.toggle("hidden", !show);
  if (mainRunViewNode) mainRunViewNode.classList.toggle("hidden", show);
}

function selectRun(runId, account) {
  selectedRunId = runId;
  if (account) selectedAccount = account;
  // Track which run the user is viewing so live push events don't mutate the wrong table
  setLiveRunViewingId(runId);
  highlightSelectedRun();
  onRunSelected?.(runId);
}

function highlightSelectedRun() {
  document.querySelectorAll(".run-entry.is-selected").forEach((el) => el.classList.remove("is-selected"));
  document.querySelectorAll(".account-folder.is-selected").forEach((el) => el.classList.remove("is-selected"));
  if (selectedRunId) {
    const el = document.querySelector(`.run-entry[data-run-id="${CSS.escape(selectedRunId)}"]`);
    if (el) {
      el.classList.add("is-selected");
      // Highlight parent account folder
      const folder = el.closest(".account-folder");
      if (folder) folder.classList.add("is-selected");
    }
  }
}

function buildAccountRunMap(accounts, runs) {
  const map = new Map();
  for (const acct of accounts) {
    map.set(acct.name, { name: acct.name, cash: acct.cash || 0, total_value: acct.total_value || 0, total_gain: acct.total_gain || 0, runs: [] });
  }
  for (const run of runs) {
    const key = run.account || "All accounts";
    if (!map.has(key)) {
      map.set(key, { name: key, cash: 0, total_value: 0, total_gain: 0, runs: [] });
    }
    map.get(key).runs.push(run);
  }
  // Sort runs descending by date within each account
  for (const acct of map.values()) {
    acct.runs.sort((a, b) => (b.updated_at || "").localeCompare(a.updated_at || ""));
  }
  return Array.from(map.values());
}

function renderAccountFolder(acct) {
  const color = accountAccentColor(acct.name);
  const folder = document.createElement("div");
  folder.className = "account-folder";
  folder.style.borderLeftColor = color;

  const header = document.createElement("button");
  header.className = "account-header";
  header.type = "button";
  const valueStr = acct.total_value > 0 ? formatCurrency(acct.total_value) : "";
  const gainStr = acct.total_gain !== 0 ? ` (${acct.total_gain >= 0 ? "+" : ""}${formatCurrency(acct.total_gain)})` : "";
  header.innerHTML = `
    <span class="account-name">${escapeHtml(acct.name)}</span>
    <span class="account-summary">${escapeHtml(valueStr + gainStr)}</span>
    <button class="account-guidelines-btn" type="button" title="Edit guidelines">&#9998;</button>
  `;
  // Edit guidelines button
  header.querySelector(".account-guidelines-btn")?.addEventListener("click", (e) => {
    e.stopPropagation();
    onEditGuidelines?.(acct.name);
  });
  // Click account header → select most recent run, or show account view if no runs
  header.addEventListener("click", () => {
    if (acct.runs.length > 0) {
      selectRun(acct.runs[0].run_id, acct.name);
    } else {
      selectedAccount = acct.name;
      selectedRunId = null;
      // Mark that user navigated away from the active run
      setLiveRunViewingId("__account_view__");
      highlightSelectedRun();
      // Highlight this folder
      document.querySelectorAll(".account-folder.is-selected").forEach((el) => el.classList.remove("is-selected"));
      folder.classList.add("is-selected");
      onAccountSelected?.(acct.name);
    }
  });
  folder.appendChild(header);

  if (acct.runs.length > 0) {
    const runsList = document.createElement("div");
    runsList.className = "account-runs";
    for (const run of acct.runs.slice(0, 10)) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.dataset.runId = run.run_id;
      const status = run.status || "unknown";
      const isActiveRun = isActiveRunInProgress() && run.status === "running";
      btn.className = isActiveRun ? "run-entry run-entry-active" : "run-entry";
      const dotClass = isActiveRun ? "s-running" : status === "completed" ? "s-completed" : status === "running" ? "s-running" : (status === "failed" || status === "aborted") ? "s-failed" : "s-unknown";
      const ts = run.updated_at ? new Date(run.updated_at).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "?";
      const srcIcon = run.portfolio_source === "finary" ? "F" : "C";
      btn.innerHTML = isActiveRun
        ? `<span class="pipeline-spinner" style="width:0.5rem;height:0.5rem;border-width:1.5px"></span>
           <span class="run-ts" style="color:#8ecae6;font-weight:700">Running\u2026</span>`
        : `<span class="run-dot ${dotClass}"></span>
           <span class="run-ts">${escapeHtml(ts)}</span>
           <span class="run-source-icon">${srcIcon}</span>`;
      btn.addEventListener("click", () => selectRun(run.run_id, acct.name));
      runsList.appendChild(btn);
    }
    folder.appendChild(runsList);
  }

  return folder;
}

function toggleStatusPopover() {
  if (statusPopoverNode) {
    statusPopoverNode.classList.toggle("hidden");
  }
}

function setGearPanelVisible(visible) {
  if (gearPanelNode) {
    gearPanelNode.classList.toggle("hidden", !visible);
  }
}

// escapeHtml and truncate imported from ui-display-utils.js

function formatNum(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return "—";
  return n.toLocaleString(undefined, { minimumFractionDigits: 0, maximumFractionDigits: 2 });
}
