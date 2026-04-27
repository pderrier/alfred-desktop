import { createDesktopBridgeClient } from "/shared/bridge-client.js";
import {
  createRunOperationsController,
  formatBridgeError,
  isErrorCritical,
  extractErrorCode
} from "/shared/run-operations-controller.js";
import {
  buildReportViewModel
} from "/desktop-shell/report-view-model.js";
import { resolveShellIntentRoute } from "/desktop-shell/shell-intent-router.js";
import { initWizard } from "/desktop-shell/app-wizard.js";
import { initLineModal, buildPositionContext, showSaveToMemoryPanel, synthesizeChatForMemoryWithUI } from "/desktop-shell/app-line-modal.js";
import { initBootstrap } from "/desktop-shell/app-bootstrap.js";
import { initEvents } from "/desktop-shell/app-events.js";
import { openChatWizard, openCashMatchingWizard } from "/desktop-shell/app-chat-wizard.js";
import {
  resolveAgentGuidanceInputValue
} from "/desktop-shell/agent-guidance-settings.js";
import {
  hydrateDashboardSnapshot,
  loadDashboardPayload
} from "/desktop-shell/dashboard-refresh-model.js";
import {
  mergeDashboardPayloads
} from "/desktop-shell/resilience-model.js";
import {
  isFinarySessionRunnable,
  hasLatestFinarySnapshot
} from "/desktop-shell/run-wizard-policy.js";
import { resolveRunnableFinarySession } from "/desktop-shell/finary-session-resolver.js";
import {
  formatCurrency,
  mergeEvents,
  escapeHtml,
  truncate
} from "/desktop-shell/ui-display-utils.js";
import { resolveShellRefreshPlan } from "/desktop-shell/refresh-policy.js";
import { buildGlobalPortfolioSynthesis } from "/desktop-shell/global-portfolio-synthesis.js";
import { openDiscussionHistoryModal, saveDiscussionThread } from "/desktop-shell/discussion-memory.js";
import {
  reduceRunActivityState
} from "/desktop-shell/run-activity-model.js";
import { initAlfredOverlay } from "/desktop-shell/app-alfred-overlay.js";
import { registerDefaultTriggers } from "/desktop-shell/app-alfred-triggers.js";
import { startIdleTimer } from "/desktop-shell/app-alfred-idle.js";
import {
  initShellLayout,
  renderSidebar,
  renderMainPanel,
  renderAccountView,
  renderStatusPill,
  renderTopBarProgress,
  renderLivePositions,
  showRunView,
  updateSnapshotTimestamp,
  setStopAnalysisVisible,
  setActiveRunState,
  setRetrySynthesisVisible,
  showToast,
  showErrorModal,
  populateWizardAccounts,
  getSelectedWizardAccount,
  renderPipelineBar,
  clearRunPipelineBar,
  stashLiveRunState,
  setLiveRunContext,
  setLiveRunActiveId,
  setLiveRunViewingId,
  restoreLiveRunView,
  getSelectedAccount,
  getStashedLineStatus,
  updateSingleLineProgress,
  accountAccentColor,
  renderCashLinkHint
} from "/desktop-shell/shell-layout.js";

// ── Live DOM nodes ───────────────────────────────────────────────

const reportValueNode = document.getElementById("report-value");
const reportGainNode = document.getElementById("report-gain");
const reportCashNode = document.getElementById("report-cash");
const reportRecoCountNode = document.getElementById("report-reco-count");
const reportSynthesisNode = document.getElementById("report-synthesis");
const reportProvenanceNode = document.getElementById("report-provenance");
const actionsNowNode = document.getElementById("actions-now");
const analysisEventsNode = document.getElementById("analysis-events");
const tabOverviewPanel = document.getElementById("tab-overview-panel");

// Settings DOM nodes
const settingsStatusNode = document.getElementById("settings-status");
const settingsDefaultRunModeNode = document.getElementById("settings-default-run-mode");
const settingsShellThemeNode = document.getElementById("settings-shell-theme");
const settingsCredentialsNode = document.getElementById("settings-credentials");
const settingsSaveBtn = document.getElementById("settings-save-btn");
const settingsResetBtn = document.getElementById("settings-reset-btn");
const settingsLlmBackendNode = document.getElementById("settings-llm-backend");
const settingsNativeFieldsNode = document.getElementById("settings-native-fields");
const settingsOpenaiApiKeyNode = document.getElementById("settings-openai-api-key");
const settingsOpenaiModelNode = document.getElementById("settings-openai-model");
const settingsOpenaiApiBaseNode = document.getElementById("settings-openai-api-base");
const settingsApikeyToggleNode = document.getElementById("settings-apikey-toggle");
const settingsApikeyStatusNode = document.getElementById("settings-apikey-status");
const settingsAlfredSuggestionsNode = document.getElementById("settings-alfred-suggestions");
const settingsCashLinksNode = document.getElementById("settings-cash-links");
const settingsCashLinksRefreshBtn = document.getElementById("settings-cash-links-refresh-btn");
const settingsCashLinksResetBtn = document.getElementById("settings-cash-links-reset-btn");
const settingsCashLinksRelinkBtn = document.getElementById("settings-cash-links-relink-btn");

// ── State ────────────────────────────────────────────────────────

const bridge = createDesktopBridgeClient();
let latestDashboardPayload = null;
let latestFinarySessionPayload = null;
let latestStackHealthPayload = null;
let latestRuntimeSettings = null;
let latestRunActivity = null;
let activeRunRefresh = false;
let activeRunId = null;
let lastDoneCount = 0;
let linesDoneCounter = 0; // Item 11: count done lines for overlay commentary
let lineTimingStart = new Map(); // Item 15: track per-line analysis start time
let lineTimingHistory = []; // Item 15: recent completion durations (ms)
let uiEvents = [];
let autoRefreshTimer = null;
let refreshInFlight = false;
let selectedReportArtifact = null;
let selectedHistoricalRun = null;
let selectedTimelineRunId = null;
let globalHomeSynthesis = { status: "idle", snapshotKey: null, data: null, error: null };

// ── Bridge + run operations ──────────────────────────────────────

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

function setBusy(busy) { /* no-op, UI handled by shell-layout */ }
function setStatus(text, cls) { /* no-op */ }

const runOperations = createRunOperationsController({
  bridge,
  setBusy,
  setStatus: (text, cls) => {},
  renderAnalysisResult: () => {},
  refreshAfterRun: async () => { await refreshDashboard(); },
  emitEvent: (event) => {
    pushUiEvent(event);
    latestRunActivity = reduceRunActivityState(latestRunActivity, event);
    if (event?.type === "run.started" || event?.type === "run.accepted") {
      activeRunRefresh = true;
      lastDoneCount = 0;
      linesDoneCounter = 0;
      lineTimingStart = new Map();
      lineTimingHistory = [];
      // Clear ALL stale run data — prevents showing previous account's positions/actions/synthesis
      if (latestDashboardPayload?.snapshot) {
        latestDashboardPayload.snapshot.latest_run = null;
        latestDashboardPayload.snapshot.latest_run_summary = null;
        latestDashboardPayload.snapshot.latest_report = null;
        latestDashboardPayload.snapshot.latest_report_summary = null;
      }
      syncAutoRefreshPolicy();
      setStopAnalysisVisible(true);
      setActiveRunState(true);
      showRunView({ starting: true, live: true });
      if (event?.run_id) {
        activeRunId = event.run_id;
        setLiveRunActiveId(event.run_id);
        // Auto-select the running entry in the sidebar
        renderSidebar(latestDashboardPayload);
      }
    }
    if (event?.type === "run.progress" && event?.run_id) {
      activeRunId = event.run_id;
      setLiveRunActiveId(event.run_id);
    }
    if (event?.type === "run.progress") {
      if (event?.line_status) {
        stashLiveRunState(event.line_status, latestDashboardPayload, event.stage);
        // During active run, push events own position rendering — no dashboard refresh needed
        renderLivePositions(event.line_status, latestDashboardPayload);

        // Item 11 + 15: Track per-line timing and done count for overlay + ETA
        const tickers = Object.keys(event.line_status).filter((t) => t !== "__synthesis__");
        let completedCount = 0;
        let latestDoneTicker = "";
        for (const ticker of tickers) {
          const raw = event.line_status[ticker];
          const s = typeof raw === "object" ? (raw?.status || "") : String(raw || "");
          // Track when lines start analyzing (for ETA calculation)
          if ((s === "analyzing" || s === "collecting") && !lineTimingStart.has(ticker)) {
            lineTimingStart.set(ticker, Date.now());
          }
          if (s === "done" || s === "failed" || s === "aborted") {
            completedCount++;
            // Record completion time for ETA + track newly completed ticker
            if (s === "done" && lineTimingStart.has(ticker)) {
              const elapsed = Date.now() - lineTimingStart.get(ticker);
              lineTimingHistory.push(elapsed);
              lineTimingStart.delete(ticker);
              latestDoneTicker = ticker; // capture the ticker that just finished
            }
          }
        }
        const totalCount = tickers.length;

        // Item 11: Notify overlay every 3 newly completed lines
        if (completedCount > linesDoneCounter) {
          const prevDone = linesDoneCounter;
          linesDoneCounter = completedCount;
          // Fire every 3 lines that reach done
          if (Math.floor(completedCount / 3) > Math.floor(prevDone / 3)) {
            alfredOverlay.notify("line-analyzed", { completedCount, totalCount, latestTicker: latestDoneTicker });
          }
        }

        // Item 15: Update ETA display
        updateAnalysisEta(completedCount, totalCount);
      }
      renderTopBarProgress({ status: event.status || "running", line_progress: event.line_progress });
      renderPipelineBar(event.stage || "collecting_data");
      // Show toast for newly failed lines
      if (event?.line_status) {
        for (const [ticker, status] of Object.entries(event.line_status)) {
          if (typeof status === "object" && status.status === "failed" && status.error) {
            const shortError = String(status.error).split(":").slice(0, 2).join(":");
            showToast(`${ticker}: ${shortError}`, "error");
          }
        }
      }
    }
    if (event?.type === "run.completed" || event?.type === "run.failed" || event?.type === "run.aborted") {
      activeRunRefresh = false;
      activeRunId = null;
      setLiveRunActiveId(null);
      setLiveRunViewingId(null);
      lastDoneCount = 0;
      syncAutoRefreshPolicy();
      setStopAnalysisVisible(false);
      setActiveRunState(false);
      renderTopBarProgress({ status: "completed" });
      clearRunPipelineBar();
      // Remove live-run visual distinction
      const mainRunView = document.getElementById("main-run-view");
      if (mainRunView) mainRunView.classList.remove("live-run");
      if (event?.type === "run.completed") {
        showToast("Analysis complete");
        // Notify Alfred overlay — Phase B: reactive triggers auto-fire
        alfredOverlay.notify("run-completed", event);
        // Refresh to show new results
        refreshDashboard().catch(() => {});
      } else {
        // Show clear failure state — don't silently fall back to old report
        const errorMsg = event?.message || "Analysis failed";
        showErrorModal("Analysis Failed", errorMsg, "You can retry the analysis from the sidebar.");
        // Notify Alfred overlay — Phase B: reactive triggers auto-fire
        alfredOverlay.notify("run-failed", event);
        // Show failure in synthesis card
        const synthNode = document.getElementById("report-synthesis");
        if (synthNode) {
          synthNode.innerHTML = `<span style="color:#ba4b3a">\u2717 ${escapeHtml(errorMsg)}</span>`;
        }
        // Refresh sidebar to show failed status (reads from in-memory index, fast)
        // Use a lightweight dashboard refresh — doesn't overwrite the failure view
        bridge.getDashboardSnapshot().then((snap) => {
          if (snap?.result?.snapshot?.runs) {
            if (latestDashboardPayload?.snapshot) {
              latestDashboardPayload.snapshot.runs = snap.result.snapshot.runs;
            }
            renderSidebar(latestDashboardPayload);
          }
        }).catch(() => {});
      }
    }
  },
  onError: () => {}
});

// ── Alfred Overlay (proactive assistant panel) ──────────────────

const alfredOverlay = initAlfredOverlay();
window.__alfredOverlay = alfredOverlay; // exposed for cross-module notify (run wizard, etc.)
registerDefaultTriggers(alfredOverlay);
startIdleTimer(() => alfredOverlay.notify("idle", {}), 300000);

// ── Wizard (delegated to app-wizard.js) ──────────────────────────

const lineModal = initLineModal();

const wizard = initWizard({
  getLatestDashboardPayload: () => latestDashboardPayload,
  getLatestFinarySessionPayload: () => latestFinarySessionPayload,
  bridge,
  runOperations,
  refreshFinarySessionStatus: () => refreshFinarySessionStatus(),
  resolveCurrentFinarySession: () => resolveCurrentFinarySession(),
  getSelectedAccount,
  showErrorModal,
  showToast,
  populateWizardAccounts,
  setRetrySynthesisVisible
});

// ── Events ───────────────────────────────────────────────────────

function pushUiEvent(event) {
  uiEvents = [{ ...event, ts: event?.ts || new Date().toISOString() }, ...uiEvents].slice(0, 50);
  renderAnalysisEvents(mergeEvents(latestDashboardPayload?.snapshot?.audit_events || [], uiEvents));
}

function renderAnalysisEvents(events = []) {
  if (!analysisEventsNode) return;
  analysisEventsNode.innerHTML = "";
  if (events.length === 0) {
    analysisEventsNode.innerHTML = "<li class=\"empty-hint\">No events yet.</li>";
    return;
  }
  for (const event of events.slice(0, 20)) {
    const li = document.createElement("li");
    const ts = event.ts ? new Date(event.ts).toLocaleTimeString() : "";
    li.innerHTML = `<span class="analysis-event-row">${ts} ${String(event.message || event.type || "")}</span>`;
    analysisEventsNode.appendChild(li);
  }
}

// escapeHtml imported from ui-display-utils.js

// ── Actions rendering ────────────────────────────────────────────

function recommendationActionClass(signal) {
  const raw = String(signal || "").toUpperCase();
  if (raw.includes("ACHAT") || raw.includes("ACHETER") || raw.includes("RENFORC") || raw.includes("BUY")) return "tone-buy";
  if (raw.includes("VENTE") || raw.includes("VENDR") || raw.includes("ALLEG") || raw.includes("SELL")) return "tone-sell";
  return "tone-neutral";
}

function renderKpiDelta(nodeId, delta) {
  const parent = document.getElementById(nodeId)?.parentElement;
  if (!parent) return;
  const existing = parent.querySelector(".kpi-delta");
  if (existing) existing.remove();
  if (delta === null || delta === undefined || !Number.isFinite(delta) || Math.abs(delta) < 0.5) return;
  const sign = delta >= 0 ? "+" : "";
  const tone = delta >= 0 ? "kpi-delta-up" : "kpi-delta-down";
  const el = document.createElement("span");
  el.className = `kpi-delta ${tone}`;
  el.textContent = `${sign}${formatCurrency(delta)}`;
  parent.appendChild(el);
}

const ALLOC_COLORS = {
  buy: "#2f8f5d", sell: "#ba4b3a", neutral: "#4a6a8a", cash: "#6b7280"
};
function renderAllocationBar(allocation) {
  const bar = document.getElementById("allocation-bar");
  if (!bar) return;
  if (!allocation || allocation.length === 0) {
    bar.classList.add("hidden");
    return;
  }
  bar.classList.remove("hidden");
  bar.innerHTML = `<div class="alloc-segments">${allocation.map((a) =>
    `<div class="alloc-segment" style="width:${Math.max(a.weight, 1.5).toFixed(1)}%;background:${ALLOC_COLORS[a.tone] || ALLOC_COLORS.neutral}" title="${a.ticker}: ${a.weight.toFixed(1)}% (${formatCurrency(a.value)})">${a.weight >= 5 ? a.ticker : ""}</div>`
  ).join("")}</div><div class="alloc-legend">${allocation.filter((a) => a.weight >= 3).map((a) =>
    `<span class="alloc-legend-item"><span class="alloc-legend-dot" style="background:${ALLOC_COLORS[a.tone] || ALLOC_COLORS.neutral}"></span>${a.ticker} ${a.weight.toFixed(0)}%</span>`
  ).join("")}</div>`;
}

function displayError(error, context = "") {
  const formatted = formatBridgeError(error);
  if (isErrorCritical(error)) {
    const code = extractErrorCode(error);
    const title = context ? `${context} failed` : "Error";
    const hint = formatted.includes("(hint:") ? formatted.split("(hint: ")[1]?.replace(")", "") : "";
    const message = error?.message || code;
    showErrorModal(title, message, hint);
  } else {
    showToast(formatted, "error");
  }
}

let latestReportModel = null;

const NON_ACTIONABLE = new Set(["CONSERVER", "SURVEILLANCE", "HOLD", "WATCH", "MONITOR"]);

// ── Chat context builders ───────────────────────────────────────

function buildSynthesisContext(model) {
  const sections = [];
  if (model.account) sections.push(`Account: ${model.account}`);
  sections.push(`Date: ${model.lastUpdate || "n/a"}`);
  const metrics = [];
  if (model.value != null) metrics.push(`Portfolio value: ${formatCurrency(model.value)}`);
  if (model.gain != null) metrics.push(`Gain: ${formatCurrency(model.gain)}`);
  if (model.cash != null) metrics.push(`Cash: ${formatCurrency(model.cash)}`);
  if (metrics.length > 0) sections.push(metrics.join(" | "));
  sections.push(`Positions: ${model.recommendations?.length || 0}`);
  sections.push(`Synthesis: ${model.synthesis || "N/A"}`);
  if (Array.isArray(model.actionsNow) && model.actionsNow.length > 0) {
    const actionList = model.actionsNow
      .slice(0, 10)
      .map((a) => `${a.action} ${a.ticker || a.nom || "?"}`)
      .join(", ");
    sections.push(`Actions (${model.actionsNow.length}): ${actionList}`);
  }
  return `You are a senior portfolio analyst. The user wants to discuss the portfolio-level synthesis. Answer questions about strategy, macro context, or reasoning. Be concise. Only answer questions related to the portfolio and financial analysis. Politely decline any off-topic requests.

Important: this is a chat flow. If relevant context is missing (portfolio composition, line analyses, synthesis/report details, line memory, deep-news memory), fetch it with available MCP/CLI tools before answering instead of asking the user to paste it. Mention briefly when you used a tool.\n\n${sections.join("\n")}`;
}

function buildActionContext(action, recommendations) {
  const sections = [];
  const ticker = action.ticker || "";
  const name = action.nom || "";
  sections.push(`Action: ${action.action} ${name || ticker}`);
  if (action.priority) sections.push(`Priority: ${action.priority}`);
  if (action.rationale) sections.push(`Rationale: ${action.rationale}`);
  if (typeof action.quantity === "number" && action.quantity > 0) sections.push(`Quantity: ${action.quantity}`);
  if (typeof action.estimatedAmountEur === "number" && action.estimatedAmountEur > 0) sections.push(`Estimated amount: ${formatCurrency(action.estimatedAmountEur)}`);
  if (action.orderType) sections.push(`Order type: ${action.orderType}`);
  if (typeof action.priceLimit === "number" && action.priceLimit > 0) sections.push(`Price limit: ${formatCurrency(action.priceLimit)}`);

  // If there's a matching recommendation, include full position context
  if (ticker && Array.isArray(recommendations)) {
    const rec = recommendations.find((r) => r.ticker === ticker);
    if (rec) {
      sections.push("\n--- Full position analysis ---");
      sections.push(buildPositionContext(rec));
    }
  }

  return `You are a portfolio analysis assistant. The user wants to understand this recommended action. Answer questions about rationale, risk, timing, or sizing. Be concise. Only answer questions related to the portfolio and financial analysis. Politely decline any off-topic requests.

Important: this is a chat flow. If relevant context is missing (portfolio composition, line analyses, synthesis/report details, line memory, deep-news memory), fetch it with available MCP/CLI tools before answering instead of asking the user to paste it. Mention briefly when you used a tool.\n\n${sections.join("\n")}`;
}

function renderActionsNow(items = [], recommendations = []) {
  if (!actionsNowNode) return;
  actionsNowNode.innerHTML = "";
  // Filter out non-actionable signals — only show buy/sell/reinforce/reduce
  const actionable = (items || []).filter((a) => !NON_ACTIONABLE.has((a.action || "").toUpperCase()));
  if (actionable.length === 0) {
    actionsNowNode.innerHTML = "<div class=\"empty-hint\">No immediate action proposed.</div>";
    return;
  }
  for (const action of actionable.slice(0, 5)) {
    const card = document.createElement("article");
    card.className = `action-card ${recommendationActionClass(action.action)}`;
    const displayName = action.nom || action.ticker || "";
    const tickerLabel = action.ticker ? `<span class="action-ticker">${escapeHtml(action.ticker)}</span>` : "";
    const nameHtml = displayName
      ? `${escapeHtml(displayName)}${action.nom && action.ticker ? " " + tickerLabel : ""}`
      : tickerLabel;
    const orderLabel = action.orderType === "LIMIT" ? "LIMIT" : "MARKET";
    const metrics = [];
    if (typeof action.quantity === "number" && Number.isFinite(action.quantity) && action.quantity > 0) metrics.push(`${action.quantity} titres`);
    if (typeof action.estimatedAmountEur === "number" && Number.isFinite(action.estimatedAmountEur) && action.estimatedAmountEur > 0) metrics.push(`~${formatCurrency(action.estimatedAmountEur)}`);
    if (typeof action.priceLimit === "number" && Number.isFinite(action.priceLimit) && action.priceLimit > 0) metrics.push(`prix cible ${formatCurrency(action.priceLimit)}`);
    const rationale = action.rationale || "";
    const shortRationale = rationale.length > 200 ? rationale.slice(0, 200) + "\u2026" : rationale;
    const hasMore = rationale.length > 200;
    card.innerHTML = `
      <p class="action-header">${action.priority}. ${nameHtml}<button class="ghost-btn action-ask-btn" style="margin-left:0.5rem;padding:0.2rem 0.6rem;font-size:0.75rem;vertical-align:middle;border-radius:6px" title="Ask about this action">\uD83D\uDCAC Ask</button></p>
      <p class="action-detail"><span class="action-signal-badge">${escapeHtml(action.action)}</span> <span class="action-order-type">${orderLabel}</span>${metrics.length > 0 ? ` · ${metrics.join(" · ")}` : ""}</p>
      <p class="action-rationale">${escapeHtml(shortRationale)}${hasMore ? ` <a href="#" class="action-expand">voir plus</a>` : ""}</p>
    `;
    if (hasMore) {
      const expandLink = card.querySelector(".action-expand");
      const rationaleNode = card.querySelector(".action-rationale");
      if (expandLink && rationaleNode) {
        expandLink.addEventListener("click", (e) => {
          e.preventDefault();
          rationaleNode.textContent = rationale;
        });
      }
    }
    // Wire up "Ask" button for this action
    const askBtn = card.querySelector(".action-ask-btn");
    if (askBtn) {
      askBtn.addEventListener("click", async (e) => {
        e.stopPropagation();
        const ticker = action.ticker || "";
        const name = action.nom || "";
        const label = name ? `${name} (${ticker})` : ticker;
        let doneHandled = false;
        const rec = ticker
          ? (recommendations.find((r) => r.ticker === ticker) || { ticker, name, details: {}, lineMemory: {} })
          : null;
        await openChatWizard({
          title: `Ask about ${action.action} ${ticker || name}`,
          systemContext: buildActionContext(action, recommendations),
          initialMessage: `This is the ${action.action} recommendation for ${label} (priority ${action.priority || "N/A"}). What would you like to know?`,
          returnHistoryOnClose: true,
          discussionScope: `run-action:${ticker || name || "unknown"}`,
          onDone: async (history) => {
            if (!rec) return;
            doneHandled = true;
            const hadConversation = history.some((m) => m.role === "user");
            const prefill = hadConversation
              ? await synthesizeChatForMemoryWithUI(ticker, name, history)
              : null;
            if (prefill?.keyReasoning || prefill?.userNote) {
              saveDiscussionThread({
                scope: `run-action:${ticker || name || "unknown"}`,
                title: `Action ${ticker || name || "unknown"}`,
                summary: prefill?.keyReasoning || "",
                note: prefill?.userNote || "",
              });
            }
            showSaveToMemoryPanel(rec, prefill);
          },
        });
        if (!doneHandled && rec) {
          showSaveToMemoryPanel(rec, null);
        }
      });
    }
    // Click on card body (not Ask button) → open line detail modal
    card.style.cursor = "pointer";
    card.addEventListener("click", (e) => {
      if (e.target.closest(".action-ask-btn") || e.target.closest(".action-expand")) return;
      const ticker = action.ticker || "";
      const rec = ticker
        ? recommendations.find((r) => r.ticker === ticker)
        : null;
      if (rec && window.__openLineMemoryModal) {
        window.__openLineMemoryModal(rec);
      }
    });
    actionsNowNode.appendChild(card);
  }
}

// ── Synthesis "Ask" button ───────────────────────────────────────

function injectSynthesisAskButton(synthCard, model) {
  if (!synthCard) return;
  // Remove any previous button
  synthCard.querySelector(".synthesis-ask-btn")?.remove();
  // Only show when synthesis is real (not pending placeholder or error)
  const synthesis = model.synthesis || "";
  const isPending = synthCard.classList.contains("synthesis-pending");
  const isPlaceholder = synthesis === "No synthesis yet." || synthesis.startsWith("\u2717");
  if (isPending || isPlaceholder || !synthesis) return;

  const btn = document.createElement("button");
  btn.className = "ghost-btn synthesis-ask-btn";
  btn.style.cssText = "margin-top:0.5rem;padding:0.3rem 0.8rem;font-size:0.8rem;border-radius:6px";
  btn.textContent = "\uD83D\uDCAC Ask about this synthesis";
  btn.addEventListener("click", async () => {
    const m = latestReportModel;
    if (!m) return;
    const account = m.account || "your";
    let doneHandled = false;
    const synthRec = { ticker: "_PORTFOLIO", name: account, details: {}, lineMemory: {} };
    await openChatWizard({
      title: "Ask about synthesis",
      systemContext: buildSynthesisContext(m),
      initialMessage: `This is the global synthesis for your ${account} portfolio. What would you like to explore?`,
      returnHistoryOnClose: true,
      discussionScope: `run-synthesis:${account}`,
      onDone: async (history) => {
        doneHandled = true;
        const hadConversation = history.some((m) => m.role === "user");
        const prefill = hadConversation
          ? await synthesizeChatForMemoryWithUI("_PORTFOLIO", account, history)
          : null;
        if (prefill?.keyReasoning || prefill?.userNote) {
          saveDiscussionThread({
            scope: `run-synthesis:${account}`,
            title: `Synthesis ${account}`,
            summary: prefill?.keyReasoning || "",
            note: prefill?.userNote || "",
          });
        }
        showSaveToMemoryPanel(synthRec, prefill);
      },
    });
    if (!doneHandled) {
      showSaveToMemoryPanel(synthRec, null);
    }
  });
  synthCard.appendChild(btn);
}

async function askAboutGlobalHomeSummary() {
  const data = globalHomeSynthesis?.data;
  if (!data) return;
  const context = [
    "You are helping the user review their cross-portfolio allocation.",
    `Verdict: ${data.verdict}`,
    `Total assets: ${formatCurrency(data.totalValue)}`,
    `Cash ratio: ${data.cashWeightPct.toFixed(1)}%`,
    `Support mix: ${(data.supportBreakdown || []).map((s) => `${s.name} ${s.weightPct.toFixed(1)}%`).join(", ")}`,
    `Suggestions: ${(data.suggestions || []).join(" | ") || "none"}`,
  ].join("\n");

  await openChatWizard({
    title: "Ask about portfolio-wide summary",
    systemContext: context,
    initialMessage: "I can help you review your global allocation and rebalancing options across all portfolios. What would you like to explore?",
    returnHistoryOnClose: true,
    discussionScope: "home:global_summary",
    onDone: async (history) => {
      const prefill = history.some((m) => m.role === "user")
        ? await synthesizeChatForMemoryWithUI("_GLOBAL_SUMMARY", "portfolio-wide summary", history)
        : null;
      if (prefill?.keyReasoning || prefill?.userNote) {
        saveDiscussionThread({
          scope: "home:global_summary",
          title: "Global portfolio synthesis",
          summary: prefill?.keyReasoning || "",
          note: prefill?.userNote || "",
        });
      }
    },
  });
}

window.__askGlobalHomeSummary = () => askAboutGlobalHomeSummary();
window.__openGlobalSummaryDiscussions = () => {
  openDiscussionHistoryModal({
    scopePrefix: "home:global_summary",
    title: "Previous discussions — portfolio-wide summary",
  });
};

// ── Run Diff (Phase 4a) ─────────────────────────────────────────

async function renderRunDiff(recommendations = []) {
  const container = document.getElementById("run-diff-container");
  if (!container) return;
  try {
    const invoke = window.__TAURI__?.core?.invoke;
    if (!invoke) { container.classList.add("hidden"); return; }
    const diff = await invoke("get_run_diff_local");
    if (!diff?.has_previous || !diff.changes?.length) { container.classList.add("hidden"); return; }
    // Scope diff to tickers in the current run's portfolio (prevents stale cross-account data)
    const runTickers = new Set(recommendations.map((r) => (r.ticker || "").toUpperCase()).filter(Boolean));
    const scopedChanges = runTickers.size > 0
      ? diff.changes.filter((c) => runTickers.has((c.ticker || "").toUpperCase()))
      : diff.changes;
    if (scopedChanges.length === 0) { container.classList.add("hidden"); return; }
    // Recompute summary from scoped changes
    const s = { signal_changes: 0, upgrades: 0, downgrades: 0, significant_moves: 0 };
    for (const c of scopedChanges) {
      if (c.signal_changed) {
        s.signal_changes++;
        if (c.curr_signal > c.prev_signal) s.upgrades++;
        else if (c.curr_signal < c.prev_signal) s.downgrades++;
      }
      if (c.significant_price_move) s.significant_moves++;
    }
    let html = `<section class="card run-diff-section">`;
    html += `<h2>What Changed</h2>`;
    html += `<p class="run-diff-summary">${s.signal_changes} signal change${s.signal_changes !== 1 ? "s" : ""} (${s.upgrades} upgrade${s.upgrades !== 1 ? "s" : ""}, ${s.downgrades} downgrade${s.downgrades !== 1 ? "s" : ""}), ${s.significant_moves} significant price move${s.significant_moves !== 1 ? "s" : ""}</p>`;
    html += `<ul class="run-diff-list">`;
    for (const c of scopedChanges) {
      const parts = [];
      if (c.signal_changed) {
        const cls = c.curr_signal > c.prev_signal ? "diff-upgrade" : "diff-downgrade";
        parts.push(`<span class="${cls}">${c.prev_signal} \u2192 ${c.curr_signal}</span>`);
      }
      if (c.conviction_changed) parts.push(`conviction: ${c.prev_conviction} \u2192 ${c.curr_conviction}`);
      if (c.significant_price_move) parts.push(`<span class="diff-price-move">${c.price_change_pct >= 0 ? "+" : ""}${c.price_change_pct}%</span>`);
      html += `<li><strong>${c.ticker}</strong> — ${parts.join(" · ")}</li>`;
    }
    html += `</ul></section>`;
    container.innerHTML = html;
    container.classList.remove("hidden");
  } catch {
    container.classList.add("hidden");
  }
}

// ── Theme Concentration (Phase 2b) ──────────────────────────────

function openThemeRiskModal(insight) {
  const existing = document.getElementById("theme-risk-modal");
  if (existing) existing.remove();

  const topThemes = Array.isArray(insight?.topThemes) ? insight.topThemes : [];
  const topRisks = Array.isArray(insight?.topRisks) ? insight.topRisks : [];
  const globalThemes = Array.isArray(insight?.globalThemes) ? insight.globalThemes : [];
  const highlights = Array.isArray(insight?.highlights) ? insight.highlights : [];

  const overlay = document.createElement("div");
  overlay.id = "theme-risk-modal";
  overlay.className = "error-modal-overlay";
  overlay.innerHTML = `
    <div class="error-modal-content theme-risk-modal-content">
      <h3 class="error-modal-title">Themes & Risks details</h3>
      <div class="theme-risk-section">
        <p class="theme-risk-section-title">Highlights</p>
        <ul class="theme-risk-modal-list">
          ${highlights.length > 0
            ? highlights.map((line) => `<li>${escapeHtml(line)}</li>`).join("")
            : "<li class=\"empty-hint\">No highlights available.</li>"}
        </ul>
      </div>
      <div class="theme-risk-grid">
        <div class="theme-risk-section">
          <p class="theme-risk-section-title">Top themes</p>
          <ul class="theme-risk-modal-list">
            ${topThemes.length > 0
              ? topThemes.map((entry) => `<li><strong>${escapeHtml(entry.theme)}</strong> · ${entry.count} positions${entry.tickers?.length ? ` · ${escapeHtml(entry.tickers.join(", "))}` : ""}</li>`).join("")
              : "<li class=\"empty-hint\">No concentrated themes found.</li>"}
          </ul>
        </div>
        <div class="theme-risk-section">
          <p class="theme-risk-section-title">Top risks</p>
          <ul class="theme-risk-modal-list">
            ${topRisks.length > 0
              ? topRisks.map((entry) => `<li><strong>${escapeHtml(entry.risk)}</strong> · ${entry.count} mention${entry.count > 1 ? "s" : ""}${entry.tickers?.length ? ` · ${escapeHtml(entry.tickers.join(", "))}` : ""}</li>`).join("")
              : "<li class=\"empty-hint\">No recurring risks found.</li>"}
          </ul>
        </div>
      </div>
      <div class="theme-risk-section">
        <p class="theme-risk-section-title">Cross-account view</p>
        <ul class="theme-risk-modal-list">
          ${globalThemes.length > 0
            ? globalThemes.map((row) => `<li><strong>${escapeHtml(row.theme)}</strong> · ${row.accountCount} account${row.accountCount > 1 ? "s" : ""}${row.inCurrentAccount ? " · current account" : ""}</li>`).join("")
            : "<li class=\"empty-hint\">No multi-account theme data available yet.</li>"}
        </ul>
      </div>
      <div style="display:flex;justify-content:flex-end;margin-top:0.7rem">
        <button class="error-modal-dismiss">Close</button>
      </div>
    </div>
  `;
  document.body.appendChild(overlay);
  const close = () => overlay.remove();
  overlay.querySelector(".error-modal-dismiss")?.addEventListener("click", close);
  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) close();
  });
}

function renderThemeConcentration(themeConcentration, themeRiskInsight) {
  // Remove any existing concentration card
  const existing = document.getElementById("theme-concentration-card");
  if (existing) existing.remove();

  const themes = Array.isArray(themeConcentration?.themes) ? themeConcentration.themes : [];
  const highlights = Array.isArray(themeRiskInsight?.highlights) ? themeRiskInsight.highlights : [];
  if (themes.length === 0 && highlights.length === 0) return;

  const card = document.createElement("section");
  card.id = "theme-concentration-card";
  card.className = "card theme-concentration-card";

  const title = document.createElement("h2");
  title.textContent = "Themes & Risks";
  card.appendChild(title);

  const intro = document.createElement("p");
  intro.className = "theme-concentration-intro";
  intro.textContent = "Condensed highlights only. Open details for full context and cross-account comparison.";
  card.appendChild(intro);

  const list = document.createElement("ul");
  list.className = "theme-concentration-list";
  const highlightLines = highlights.length > 0
    ? highlights
    : [`${themes.length} theme${themes.length > 1 ? "s" : ""} shared by 3+ positions — potential concentration risk.`];
  for (const line of highlightLines.slice(0, 3)) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="theme-slug">\u2728 Insight</span> <span class="theme-tickers">${escapeHtml(line)}</span>`;
    list.appendChild(li);
  }
  card.appendChild(list);

  const detailBtn = document.createElement("button");
  detailBtn.className = "ghost-btn theme-risk-detail-btn";
  detailBtn.type = "button";
  detailBtn.textContent = "Open themes/risks details";
  detailBtn.addEventListener("click", () => openThemeRiskModal(themeRiskInsight || {}));
  card.appendChild(detailBtn);

  // Insert between synthesis card and actions-now card
  const actionsCard = actionsNowNode?.closest(".actions-now-card");
  if (actionsCard && actionsCard.parentNode) {
    actionsCard.parentNode.insertBefore(card, actionsCard);
  }
}

// ── ETA display (Item 15) ────────────────────────────────────────

function updateAnalysisEta(completedCount, totalCount) {
  const progressNode = document.getElementById("positions-progress");
  if (!progressNode || totalCount === 0) return;
  const remaining = totalCount - completedCount;
  if (remaining <= 0) {
    progressNode.textContent = "";
    progressNode.classList.add("hidden");
    return;
  }
  if (lineTimingHistory.length === 0) {
    // No timing data yet — show count only
    progressNode.textContent = `Analyzing ${completedCount} of ${totalCount} positions\u2026`;
    return;
  }
  // Average of recent completions (last 10 for smoothing)
  const recent = lineTimingHistory.slice(-10);
  const avgMs = recent.reduce((sum, d) => sum + d, 0) / recent.length;
  // Account for parallelization — lineTimingStart.size = lines currently in-flight
  const concurrency = Math.max(lineTimingStart.size, 1);
  const etaMs = (remaining * avgMs) / concurrency;
  const etaMin = Math.ceil(etaMs / 60000);
  const etaLabel = etaMin <= 1 ? "< 1 min" : `~${etaMin} min`;
  progressNode.textContent = `Analyzing ${completedCount} of ${totalCount} positions (${etaLabel} remaining)`;
  progressNode.classList.remove("hidden");
}

// ── Report rendering ─────────────────────────────────────────────

function renderReport(payload) {
  const model = buildReportViewModel(payload);
  latestReportModel = model;
  if (reportValueNode) reportValueNode.textContent = formatCurrency(model.value);
  if (reportGainNode) reportGainNode.textContent = formatCurrency(model.gain);
  if (reportCashNode) reportCashNode.textContent = formatCurrency(model.cash);
  // Show cash account link below the Cash KPI
  const reportCashLink = document.getElementById("report-cash-link");
  if (reportCashLink && model.account) {
    const snapshot = payload?.snapshot || {};
    const finaryMeta = snapshot.latest_finary_snapshot || {};
    renderCashLinkHint(reportCashLink, model.account, finaryMeta);
  }
  if (reportRecoCountNode) reportRecoCountNode.textContent = String(model.recommendationCount || 0);
  // Show deltas vs previous run
  renderKpiDelta("report-value", model.previousRun?.valueDelta);
  renderKpiDelta("report-gain", model.previousRun?.gainDelta);
  renderAllocationBar(model.allocation);
  if (reportSynthesisNode) {
    reportSynthesisNode.textContent = model.synthesis;
    // Clear pending state when real report data arrives
    const synthCard = document.getElementById("report-synthesis-card");
    if (synthCard && model.synthesis) synthCard.classList.remove("synthesis-pending");
    // Inject "Ask" button for synthesis (only when real synthesis is present)
    injectSynthesisAskButton(synthCard, model);
  }
  if (reportProvenanceNode) {
    reportProvenanceNode.textContent = Array.isArray(model.provenanceSummary) && model.provenanceSummary.length > 0
      ? `Provenance: ${model.provenanceSummary.join(" · ")}`
      : "";
  }
  renderActionsNow(model.actionsNow, model.recommendations);
  // Item 12: Export button
  injectExportButton(model);
  // Phase 4a: run diff view — scoped to tickers in this run's portfolio
  renderRunDiff(model.recommendations);
  // Phase 2b: theme concentration risk card
  renderThemeConcentration(model.themeConcentration, model.themeRiskInsight);
  // Watchlist opportunities summary
  const watchlistSummaryNode = document.getElementById("watchlist-summary");
  if (watchlistSummaryNode) {
    if (model.watchlistSummary) {
      watchlistSummaryNode.textContent = model.watchlistSummary;
      watchlistSummaryNode.classList.remove("hidden");
    } else {
      watchlistSummaryNode.classList.add("hidden");
    }
  }
  // Next analysis recommendation
  const nextAnalysisNode = document.getElementById("next-analysis");
  if (nextAnalysisNode) {
    if (model.nextAnalysis) {
      nextAnalysisNode.textContent = model.nextAnalysis;
      nextAnalysisNode.classList.remove("hidden");
    } else {
      nextAnalysisNode.classList.add("hidden");
    }
  }
}

// ── Export button (Item 12) ──────────────────────────────────────

function injectExportButton(model) {
  // Insert into the KPI strip section (tab-overview-panel)
  const kpiStrip = document.getElementById("tab-overview-panel");
  if (!kpiStrip) return;
  // Remove existing export button if present
  kpiStrip.querySelector(".report-export-btn")?.remove();
  if (!model.synthesis || model.synthesis === "No synthesis yet.") return;

  const btn = document.createElement("button");
  btn.className = "ghost-btn report-export-btn";
  btn.style.cssText = "margin-left:auto;padding:0.3rem 0.8rem;font-size:0.78rem;border-radius:6px;white-space:nowrap";
  btn.textContent = "\uD83D\uDCE4 Export";
  btn.title = "Export report as Markdown";
  btn.addEventListener("click", async () => {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) {
      showToast("Export not available outside desktop app", "error");
      return;
    }
    btn.disabled = true;
    btn.textContent = "Exporting\u2026";
    try {
      const result = await tauriInvoke("export_report_markdown_local", {
        payload: JSON.parse(JSON.stringify(latestReportModel)),
      });
      const path = result?.path || result;
      showToast(`Report exported to ${path}`, "success");
    } catch (err) {
      showToast(`Export failed: ${err?.message || err}`, "error");
    }
    btn.disabled = false;
    btn.textContent = "\uD83D\uDCE4 Export";
  });
  kpiStrip.appendChild(btn);
}

function computeHomeSnapshotKey(snapshot) {
  const latestSnapshotTs = snapshot?.latest_finary_snapshot?.snapshot_at || "";
  const latestRunId = snapshot?.latest_run?.run_id || "";
  const latestRunTs = snapshot?.latest_run?.updated_at || "";
  const runsCount = Array.isArray(snapshot?.runs) ? snapshot.runs.length : 0;
  return `${latestSnapshotTs}|${latestRunId}|${latestRunTs}|${runsCount}`;
}

function scheduleGlobalHomeSynthesis(snapshot) {
  const snapshotKey = computeHomeSnapshotKey(snapshot || {});
  if (globalHomeSynthesis.snapshotKey === snapshotKey && globalHomeSynthesis.status !== "error") {
    return;
  }

  globalHomeSynthesis = {
    status: "loading",
    snapshotKey,
    data: globalHomeSynthesis.data,
    error: null,
  };
  renderWelcome();

  setTimeout(() => {
    try {
      const synthesized = buildGlobalPortfolioSynthesis(snapshot || {});
      globalHomeSynthesis = {
        status: "ready",
        snapshotKey,
        data: synthesized,
        error: null,
      };
    } catch (error) {
      globalHomeSynthesis = {
        status: "error",
        snapshotKey,
        data: null,
        error: String(error?.message || error || "global_synthesis_failed"),
      };
    }
    renderWelcome();
  }, 0);
}

// ── Dashboard refresh ────────────────────────────────────────────

async function refreshDashboard() {
  // Coalesce — skip if another refresh is already in flight
  if (refreshInFlight) return;
  refreshInFlight = true;
  try {
    await refreshDashboardInner();
  } finally {
    refreshInFlight = false;
  }
}

async function refreshDashboardInner() {
  // Single call — getDashboardSnapshot combines overview + details.
  // No need for 3 separate file-scanning Tauri commands.
  const payload = await loadDashboardPayload({
    getDashboardSnapshot: () => bridge.getDashboardSnapshot()
  });
  const hydratedSnapshot = await hydrateDashboardSnapshot(payload?.snapshot || {}, {
    getRunById: (runId) => bridge.getRunById(runId)
  });
  latestDashboardPayload = mergeDashboardPayloads(latestDashboardPayload, { ...payload, snapshot: hydratedSnapshot });
  const snapshot = latestDashboardPayload?.snapshot || {};
  scheduleGlobalHomeSynthesis(snapshot);

  // Keep live run context up to date with positions (for enriching done rows)
  const positions = snapshot.latest_run?.portfolio?.positions || [];
  if (positions.length > 0) {
    setLiveRunContext(positions, latestDashboardPayload);
  }

  if (!activeRunRefresh) {
    renderReport(latestDashboardPayload);
  }
  renderAnalysisEvents(mergeEvents(snapshot.audit_events || [], uiEvents));

  // New layout renders
  renderSidebar(latestDashboardPayload);
  // During active run, positions are managed by push events — don't overwrite with stale disk data
  if (!activeRunRefresh) {
    const viewModel = buildReportViewModel(latestDashboardPayload);
    renderMainPanel(viewModel, latestDashboardPayload);
  }
  renderStatusPill(null, latestFinarySessionPayload, snapshot.latest_run_summary || null);
  renderWelcome();
  updateSnapshotTimestamp(snapshot.latest_finary_snapshot || {});
  // Populate wizard account dropdown — merge snapshot + run positions, deduplicated
  const finaryMeta = snapshot.latest_finary_snapshot || {};
  const accountSet = new Map();
  for (const acct of (Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [])) {
    if (acct.name) accountSet.set(acct.name, acct);
  }
  for (const run of (Array.isArray(snapshot.runs) ? snapshot.runs : [])) {
    if (run.account && !accountSet.has(run.account)) accountSet.set(run.account, { name: run.account });
  }
  for (const pos of (snapshot.latest_run?.portfolio?.positions || [])) {
    const key = pos.compte || pos.account || "";
    if (key && !accountSet.has(key)) accountSet.set(key, { name: key });
  }
  populateWizardAccounts(Array.from(accountSet.values()));
  const canRetry = snapshot.latest_run_summary?.status === "completed_degraded" ||
    (snapshot.latest_run_summary?.status === "failed" && (snapshot.latest_run_summary?.pending_recommendations_count || 0) > 0);
  setRetrySynthesisVisible(canRetry);

  // Check for ambiguous cash account groups that need user confirmation
  checkAmbiguousCashGroups(finaryMeta).catch(() => {});

  // Phase C: notify Alfred overlay that dashboard data is loaded
  alfredOverlay.notify("dashboard-loaded", { finaryMeta });
}

/**
 * Given a raw name (possibly decorated with parenthesized amounts), find the
 * closest canonical name from a list. Strips trailing `(...)`, tries exact,
 * case-insensitive, and substring containment matches.
 */
function findClosestName(rawName, canonicalNames) {
  const cleaned = (rawName || "").replace(/\s*\([^)]*\)\s*$/, "").trim();
  if (canonicalNames.includes(cleaned)) return cleaned;
  const lower = cleaned.toLowerCase();
  const ci = canonicalNames.find((n) => n.toLowerCase() === lower);
  if (ci) return ci;
  const sub = canonicalNames.find(
    (n) => lower.includes(n.toLowerCase()) || n.toLowerCase().includes(lower)
  );
  if (sub) return sub;
  return cleaned; // best effort
}

/** Track whether we've already prompted for ambiguous cash groups this session */
let cashWizardShownThisSession = false;

async function checkAmbiguousCashGroups(finaryMeta) {
  if (cashWizardShownThisSession) return;
  const groups = Array.isArray(finaryMeta.ambiguous_cash_groups)
    ? finaryMeta.ambiguous_cash_groups
    : [];
  if (groups.length === 0) return;

  // Don't prompt if the user already has saved cash_account_links that cover all ambiguous accounts
  const tauriInvoke = window?.__TAURI__?.core?.invoke;
  if (!tauriInvoke) return;
  try {
    const prefs = await tauriInvoke("get_user_preferences_local") || {};
    const savedLinks = prefs?.cash_account_links || {};
    const allInvestmentNames = groups.flatMap((g) =>
      (g.investment_accounts || []).map((a) => a.name)
    );
    const allCovered = allInvestmentNames.every((name) => savedLinks[name]);
    if (allCovered) return; // User already confirmed all ambiguous mappings
  } catch { /* proceed to show wizard */ }

  cashWizardShownThisSession = true;

  for (const group of groups) {
    const investmentAccounts = group.investment_accounts || [];
    const cashAccounts = group.cash_accounts || [];
    if (investmentAccounts.length === 0 || cashAccounts.length === 0) continue;

    // Build current heuristic mapping for display
    const currentMapping = {};
    for (let i = 0; i < investmentAccounts.length; i++) {
      if (cashAccounts[i]) {
        currentMapping[investmentAccounts[i].name] = cashAccounts[i].fiats_sum;
      }
    }

    const result = await openCashMatchingWizard({
      investmentAccounts,
      cashAccounts,
      currentMapping,
    });

    if (result) {
      // Save confirmed mapping to user preferences
      try {
        const prefs = await tauriInvoke("get_user_preferences_local") || {};
        if (!prefs.cash_account_links) prefs.cash_account_links = {};

        if (result.confirmed) {
          // User confirmed the heuristic — save inv_name → cash_slug pairs
          for (let i = 0; i < investmentAccounts.length; i++) {
            if (cashAccounts[i]) {
              prefs.cash_account_links[investmentAccounts[i].name] = cashAccounts[i].slug || cashAccounts[i].name;
            }
          }
        } else {
          // User provided explicit mapping: { inv_name: cash_slug_or_name | "__none__" }
          const knownInvNames = investmentAccounts.map((a) => a.name);
          const knownCashSlugs = cashAccounts.map((a) => a.slug || a.name);
          for (const [rawKey, rawVal] of Object.entries(result)) {
            const cleanKey = findClosestName(rawKey, knownInvNames);
            if (rawVal === "__none__") {
              prefs.cash_account_links[cleanKey] = "__none__";
            } else {
              // Value is a slug (new format) or name (legacy) — save as-is
              const isSlug = knownCashSlugs.includes(rawVal);
              prefs.cash_account_links[cleanKey] = isSlug ? rawVal : findClosestName(rawVal, knownCashSlugs);
            }
          }
        }

        await tauriInvoke("save_user_preferences_local", { prefs });
        try {
          // Invalidate cache so sync re-computes cash mapping with the new links.
          await tauriInvoke("finary_invalidate_snapshot_local");
          await tauriInvoke("finary_sync_snapshot_local");
          await refreshDashboard();
        } catch { /* non-blocking */ }
        const savedCount = Object.keys(prefs.cash_account_links || {}).length;
        showToast(`Cash mapping saved (${savedCount} link${savedCount !== 1 ? "s" : ""}) — persisted for future runs.`, "success");
      } catch (err) {
        showToast(`Failed to save cash mapping: ${err?.message || err}`, "error");
      }
    }
  }
}

function syncAutoRefreshPolicy() {
  clearInterval(autoRefreshTimer);
  // No polling — push events (alfred://line-progress, alfred://line-done, alfred://run-stage)
  // handle all real-time updates during active runs. Polling caused flickering conflicts
  // by rebuilding the positions table every 2s while push events were updating individual rows.
}

// ── Finary session ───────────────────────────────────────────────

async function refreshFinarySessionStatus() {
  try {
    latestFinarySessionPayload = await bridge.getFinarySessionStatus();
  } catch { /* session unavailable */ }
  return latestFinarySessionPayload;
}

function refreshWizardSourcePolicy(sessionPayload) {
  wizard.refreshSourcePolicy(sessionPayload);
}

async function resolveCurrentFinarySession() {
  return resolveRunnableFinarySession({
    refreshStatus: refreshFinarySessionStatus,
    isRunnable: isFinarySessionRunnable,
    getCachedSession: () => latestFinarySessionPayload
  });
}

// ── Wizard (implementation in app-wizard.js) ────────────────────

function openRunWizard({ statusText, statusClass } = {}) {
  wizard.open({ statusText, statusClass });
}

// ── Line memory modal (implementation in app-line-modal.js) ──────

function openLineMemoryModal(rec) { lineModal.open(rec); }
function setLineMemoryModalVisible(visible) { if (!visible) lineModal.close(); }

// ── Settings ─────────────────────────────────────────────────────

function setSettingsStatus(text, cls = "status-idle") {
  if (settingsStatusNode) {
    settingsStatusNode.className = cls;
    settingsStatusNode.textContent = text;
  }
}

function collectRuntimeSettingsFormValues() {
  return {
    default_run_mode: settingsDefaultRunModeNode?.value || "finary_resync",
    shell_theme: settingsShellThemeNode?.value || "dark",
    llm_backend: settingsLlmBackendNode?.value || "codex",
    openai_api_key: settingsOpenaiApiKeyNode?.value || "",
    openai_model: settingsOpenaiModelNode?.value || "",
    openai_api_base: settingsOpenaiApiBaseNode?.value || "",
  };
}

function renderRuntimeSettings(settings) {
  latestRuntimeSettings = settings;
  const values = settings?.values || {};
  if (settingsDefaultRunModeNode) settingsDefaultRunModeNode.value = values.default_run_mode || "finary_resync";
  if (settingsShellThemeNode) settingsShellThemeNode.value = values.shell_theme || "dark";
  if (settingsLlmBackendNode) settingsLlmBackendNode.value = values.llm_backend || "codex";
  if (settingsOpenaiApiKeyNode) settingsOpenaiApiKeyNode.value = values.openai_api_key || "";
  if (settingsOpenaiModelNode) settingsOpenaiModelNode.value = values.openai_model || "gpt-4.1";
  if (settingsOpenaiApiBaseNode) settingsOpenaiApiBaseNode.value = values.openai_api_base || "";
  toggleNativeFields();
  const credentials = Array.isArray(settings?.credentials) ? settings.credentials : [];
  if (settingsCredentialsNode) {
    settingsCredentialsNode.innerHTML = credentials.length === 0 ? "<li class=\"empty-hint\">No credentials.</li>" : "";
    for (const cred of credentials) {
      const li = document.createElement("li");
      li.textContent = `${cred.label}: ${cred.status}`;
      settingsCredentialsNode.appendChild(li);
    }
  }
  // Phase C: load Alfred suggestions preference into checkbox
  if (settingsAlfredSuggestionsNode) {
    const tauriInv = window?.__TAURI__?.core?.invoke;
    if (tauriInv) {
      tauriInv("get_user_preferences_local").then((prefs) => {
        const enabled = prefs?.alfred_suggestions_enabled !== false; // default: on
        settingsAlfredSuggestionsNode.checked = enabled;
      }).catch(() => {
        settingsAlfredSuggestionsNode.checked = true; // default on
      });
    }
  }
  void refreshCashLinksSettings().catch(() => {});
}

async function refreshCashLinksSettings() {
  if (!settingsCashLinksNode) return;
  const tauriInv = window?.__TAURI__?.core?.invoke;
  if (!tauriInv) {
    settingsCashLinksNode.innerHTML = "<li class=\"empty-hint\">Unavailable outside desktop runtime.</li>";
    return;
  }
  try {
    const prefs = await tauriInv("get_user_preferences_local") || {};
    const links = prefs?.cash_account_links || {};
    const entries = Object.entries(links);
    if (entries.length === 0) {
      settingsCashLinksNode.innerHTML = "<li class=\"empty-hint\">No saved links yet.</li>";
      return;
    }
    settingsCashLinksNode.innerHTML = "";
    for (const [investmentName, cashNameRaw] of entries.sort((a, b) => a[0].localeCompare(b[0]))) {
      const cashName = cashNameRaw === "__none__" ? "No cash account" : String(cashNameRaw || "");
      const li = document.createElement("li");
      li.className = "memory-item";
      li.style.gap = "0.6rem";
      li.innerHTML = `
        <div class="memory-header" style="display:flex;align-items:center;justify-content:space-between;">
          <div>
            <span class="memory-label">${escapeHtml(investmentName)}</span>
            <span class="memory-value">${escapeHtml(cashName)}</span>
          </div>
          <button class="btn-icon cash-link-delete" title="Remove this link" style="font-size:0.8rem;opacity:0.6;cursor:pointer;background:none;border:none;color:var(--text-secondary);">✕</button>
        </div>
      `;
      li.querySelector(".cash-link-delete")?.addEventListener("click", async () => {
        try {
          const p = await tauriInv("get_user_preferences_local") || {};
          if (p.cash_account_links) {
            delete p.cash_account_links[investmentName];
            if (Object.keys(p.cash_account_links).length === 0) delete p.cash_account_links;
          }
          await tauriInv("save_user_preferences_local", { prefs: p });
          await refreshCashLinksSettings();
          showToast(`Removed link for ${investmentName}`, "success");
        } catch (err) {
          showToast(`Failed: ${err?.message || err}`, "error");
        }
      });
      settingsCashLinksNode.appendChild(li);
    }
  } catch {
    settingsCashLinksNode.innerHTML = "<li class=\"empty-hint\">Failed to load links.</li>";
  }
}

function toggleNativeFields() {
  const isNative = settingsLlmBackendNode?.value === "native";
  if (settingsNativeFieldsNode) settingsNativeFieldsNode.classList.toggle("hidden", !isNative);
}

// Backend selector toggle
settingsLlmBackendNode?.addEventListener("change", toggleNativeFields);

// API key show/hide toggle
settingsApikeyToggleNode?.addEventListener("click", () => {
  if (!settingsOpenaiApiKeyNode) return;
  const isPassword = settingsOpenaiApiKeyNode.type === "password";
  settingsOpenaiApiKeyNode.type = isPassword ? "text" : "password";
  settingsApikeyToggleNode.textContent = isPassword ? "Hide" : "Show";
});

async function refreshRuntimeSettings() {
  try {
    const settings = await bridge.getRuntimeSettings();
    renderRuntimeSettings(settings);
  } catch (error) {
    setSettingsStatus(formatBridgeError(error), "status-error");
  }
}

function setThemeMode(theme) {
  document.documentElement.setAttribute("data-theme", theme || "dark");
}

// ── Splash + startup (delegated to app-bootstrap.js) ─────────────

async function refreshHealthPill(checkAuth = false) {
  const tauriInvoke = window?.__TAURI__?.core?.invoke;
  if (!tauriInvoke) return;
  try {
    let payload;
    if (checkAuth) {
      // Full check: API reachability + HMAC auth verification
      const authResult = await tauriInvoke("check_api_auth_local");
      payload = authResult;
    } else {
      // Basic check: API reachability only
      const health = await tauriInvoke("stack_health_local");
      payload = health?.result || health;
    }
    latestStackHealthPayload = payload;
    renderStatusPill(payload, null, null);
  } catch {
    latestStackHealthPayload = { status: "unreachable", ok: false, services: [] };
    renderStatusPill(latestStackHealthPayload, null, null);
  }
}

function isApiHealthy() {
  if (!latestStackHealthPayload) return true; // unknown = assume ok
  const status = latestStackHealthPayload.status || "unknown";
  return status === "healthy" || status === "degraded";
}

const bootstrap = initBootstrap({
  bridge,
  refreshDashboard: () => refreshDashboard(),
  refreshFinarySessionStatus: () => refreshFinarySessionStatus(),
  refreshWizardSourcePolicy: (sp) => refreshWizardSourcePolicy(sp),
  refreshHealthPill,
  refreshAccountStatus,
  getLatestFinarySessionPayload: () => latestFinarySessionPayload,
});

// ── Intent routing ───────────────────────────────────────────────

function applyShellIntent(intentName) {
  const route = resolveShellIntentRoute(intentName);
  if (!route) return;
  if (route.type === "open_wizard") {
    openRunWizard({});
  } else if (route.type === "run_action" && route.action === "retry_global_synthesis") {
    const runId = latestDashboardPayload?.snapshot?.latest_run?.run_id;
    if (runId) {
      // Show live feedback during retry
      const synthNode = document.getElementById("report-synthesis");
      const synthCard = document.getElementById("report-synthesis-card");
      if (synthNode) {
        synthNode.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>Retrying global synthesis\u2026</span>`;
      }
      if (synthCard) synthCard.classList.add("synthesis-pending");
      renderPipelineBar("llm_generating");
      setRetrySynthesisVisible(false);
      showToast("Retrying global synthesis\u2026");
      bridge.retryGlobalSynthesis(runId)
        .then(async () => {
          clearRunPipelineBar();
          await refreshDashboard();
          showToast("Global synthesis completed");
        })
        .catch((err) => {
          clearRunPipelineBar();
          if (synthNode) synthNode.textContent = "Synthesis retry failed.";
          if (synthCard) synthCard.classList.remove("synthesis-pending");
          showToast(`Synthesis retry failed: ${err?.message || err}`, "error");
          setRetrySynthesisVisible(true);
        });
    }
  }
}

// ── Event wiring ─────────────────────────────────────────────────

// Shell layout
initShellLayout({
  openWizard: (mode) => openRunWizard({}),
  connectOpenai: async () => {
    const btn = document.getElementById("cmd-connect-openai");
    if (btn) { btn.disabled = true; btn.textContent = "Signing in..."; }
    try {
      await bridge.codexSessionLogin();
      if (btn) { btn.textContent = "Connected"; btn.classList.add("hidden"); }
      showToast("OpenAI connected successfully.", "success");
    } catch (error) {
      if (btn) { btn.textContent = "Connect OpenAI"; btn.disabled = false; }
      showToast(`OpenAI sign-in failed: ${formatBridgeError(error)}`, "error");
    }
  },
  editGuidelines: async (accountName) => {
    // Open the run wizard pre-selected on this account with guidelines visible
    openRunWizard({});
    const select = document.getElementById("wizard-account-select");
    if (select) select.value = accountName;
    await wizard.loadGuidelinesForAccount(accountName);
    // Focus the guidelines textarea
    const guidelinesInput = document.getElementById("wizard-agent-guidelines");
    if (guidelinesInput) {
      guidelinesInput.focus();
      guidelinesInput.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  },
  syncFinary: () => applyShellIntent("reconnect_finary"),
  stopAnalysis: () => {
    runOperations.requestAbort();
    setStopAnalysisVisible(false);
    showToast("Analysis stopped.", "warning");
  },
  retrySynthesis: () => applyShellIntent("retry_global_synthesis"),
  selectRun: async (runId) => {
    // If selecting the currently active run, restore live view
    if (activeRunRefresh && activeRunId && runId === activeRunId) {
      restoreLiveRunView();
      return;
    }
    // Browsing a different run — remove live-run tint
    const mainRunView = document.getElementById("main-run-view");
    if (mainRunView) mainRunView.classList.remove("live-run");
    try {
      const runData = await bridge.getRunById(runId);
      if (runData) {
        // Unified view: run state is authoritative. Keep existing report artifact
        // only if it belongs to the same run (for artifact fallback in the view model).
        const snapshot = latestDashboardPayload?.snapshot || {};
        const existingReport = snapshot.latest_report;
        const keepReport = existingReport?.run_id === runData.run_id ? existingReport : null;
        latestDashboardPayload = {
          ...latestDashboardPayload,
          snapshot: {
            ...snapshot,
            latest_run: runData,
            latest_run_summary: {
              run_id: runData.run_id, status: runData.orchestration?.status || "unknown",
              stage: runData.orchestration?.stage || null, account: runData.account || null,
              collected_positions_count: runData.portfolio?.positions?.length || 0,
              pending_recommendations_count: runData.pending_recommandations?.length || 0
            },
            latest_report: keepReport,
            latest_report_summary: keepReport,
            report_history: []
          }
        };
        renderReport(latestDashboardPayload);
        const model = buildReportViewModel(latestDashboardPayload);
        renderMainPanel(model, latestDashboardPayload);
        renderStatusPill(null, latestFinarySessionPayload, latestDashboardPayload.snapshot.latest_run_summary);
        clearRunPipelineBar();
        // Show retry synthesis when synthesis is missing, degraded, or too short
        const runStatus = runData.orchestration?.status || "";
        const recoCount = (runData.pending_recommandations || []).length;
        const syntheseLen = (runData.composed_payload?.synthese_marche || "").length;
        const canRetry = runStatus === "completed_degraded" ||
          (runStatus === "failed" && recoCount > 0) ||
          (runStatus === "completed" && recoCount > 0 && syntheseLen < 200);
        setRetrySynthesisVisible(canRetry);
      }
    } catch (err) {
      showToast(`Failed to load run: ${err?.message || err}`, "error");
    }
  },
  selectAccount: async (accountName) => {
    // Load positions for this account directly from the stored snapshot
    let positions = [];
    try {
      const tauriInvoke = window?.__TAURI__?.core?.invoke;
      if (tauriInvoke) {
        const result = await tauriInvoke("account_positions_local", { account: accountName });
        positions = Array.isArray(result?.positions) ? result.positions : [];
      }
    } catch { /* fallback to empty */ }

    renderAccountView(accountName, latestDashboardPayload, positions);
    const ctaBtn = document.getElementById("account-run-analysis-btn");
    if (ctaBtn) {
      ctaBtn.onclick = () => {
        openRunWizard({});
        const select = document.getElementById("wizard-account-select");
        if (select) select.value = accountName;
      };
    }
  }
});

// Position detail modal
window.__openLineMemoryModal = (rec) => { if (rec) openLineMemoryModal(rec); };
// lineMemoryCloseBtn handler now in app-line-modal.js
// Detail panel toggle buttons now in app-line-modal.js
document.getElementById("line-memory-close-top-btn")?.addEventListener("click", () => setLineMemoryModalVisible(false));

// ── Tauri events + modal overlays + news links (delegated to app-events.js) ──
initEvents({
  bridge,
  getActiveRunId: () => activeRunId,
});

// Wizard event handlers now in app-wizard.js

// Settings
settingsSaveBtn?.addEventListener("click", async () => {
  try {
    const updated = await bridge.updateRuntimeSettings(collectRuntimeSettingsFormValues());
    renderRuntimeSettings(updated);
    // Phase C: save Alfred suggestions preference
    const tauriInv = window?.__TAURI__?.core?.invoke;
    if (tauriInv && settingsAlfredSuggestionsNode) {
      const enabled = settingsAlfredSuggestionsNode.checked;
      await tauriInv("save_user_preferences_local", {
        prefs: { alfred_suggestions_enabled: enabled }
      });
      alfredOverlay.setAlfredSuggestionsEnabled(enabled);
    }
    showToast("Settings saved", "success");
    refreshWizardSourcePolicy(latestFinarySessionPayload);
  } catch (error) {
    showToast(`Save failed: ${formatBridgeError(error)}`, "error");
  }
});
settingsResetBtn?.addEventListener("click", async () => {
  try {
    const updated = await bridge.resetRuntimeSettings();
    renderRuntimeSettings(updated);
    showToast("Defaults restored", "success");
  } catch (error) {
    showToast(`Reset failed: ${formatBridgeError(error)}`, "error");
  }
});
settingsCashLinksRefreshBtn?.addEventListener("click", async () => {
  await refreshCashLinksSettings();
  showToast("Cash links refreshed", "success");
});
settingsCashLinksResetBtn?.addEventListener("click", async () => {
  try {
    const tauriInv = window?.__TAURI__?.core?.invoke;
    if (!tauriInv) return;
    // Block the auto-wizard BEFORE refreshDashboard so it doesn't
    // re-save the old name-based links immediately after we clear them.
    cashWizardShownThisSession = true;
    await tauriInv("save_user_preferences_local", { prefs: { cash_account_links: null } });
    try {
      await tauriInv("finary_invalidate_snapshot_local");
      await tauriInv("finary_sync_snapshot_local");
      await refreshDashboard();
    } catch { /* non-blocking */ }
    await refreshCashLinksSettings();
    showToast("Cash account links reset — use Re-link to re-assign, or run a new analysis", "success");
  } catch (error) {
    showToast(`Reset links failed: ${formatBridgeError(error)}`, "error");
  }
});
settingsCashLinksRelinkBtn?.addEventListener("click", async () => {
  try {
    const tauriInv = window?.__TAURI__?.core?.invoke;
    if (!tauriInv) return;
    // Reset saved links first, then re-fetch snapshot so all accounts become ambiguous
    await tauriInv("save_user_preferences_local", { prefs: { cash_account_links: null } });
    try {
      await tauriInv("finary_invalidate_snapshot_local");
      await tauriInv("finary_sync_snapshot_local");
      await refreshDashboard();
    } catch { /* non-blocking */ }
    // Use freshly-loaded dashboard data
    const finaryMeta = latestDashboardPayload?.snapshot?.latest_finary_snapshot || {};
    const groups = Array.isArray(finaryMeta.ambiguous_cash_groups)
      ? finaryMeta.ambiguous_cash_groups
      : [];
    if (groups.length === 0) {
      showToast("No ambiguous cash groups found — try refreshing your data first", "info");
      await refreshCashLinksSettings();
      return;
    }
    cashWizardShownThisSession = false;
    await checkAmbiguousCashGroups(finaryMeta);
    await refreshCashLinksSettings();
  } catch (error) {
    showToast(`Re-link failed: ${formatBridgeError(error)}`, "error");
  }
});
settingsShellThemeNode?.addEventListener("change", () => setThemeMode(settingsShellThemeNode.value));

// ── Account management (gear panel) ─────────────────────────────

const settingsOpenaiStatus = document.getElementById("settings-openai-status");
const settingsOpenaiLoginBtn = document.getElementById("settings-openai-login-btn");
const settingsOpenaiLogoutBtn = document.getElementById("settings-openai-logout-btn");
const settingsFinaryStatus = document.getElementById("settings-finary-status");
const settingsFinaryConnectBtn = document.getElementById("settings-finary-connect-btn");
const settingsFinaryReconnectBtn = document.getElementById("settings-finary-reconnect-btn");

// Auth pill elements
const authPillOpenai = document.getElementById("auth-pill-openai");
const authPillFinary = document.getElementById("auth-pill-finary");
const authPopover = document.getElementById("auth-popover");
const authPopoverOpenaiStatus = document.getElementById("auth-popover-openai-status");
const authPopoverOpenaiLogin = document.getElementById("auth-popover-openai-login");
const authPopoverOpenaiLogout = document.getElementById("auth-popover-openai-logout");
const authPopoverFinaryStatus = document.getElementById("auth-popover-finary-status");
const authPopoverFinaryConnect = document.getElementById("auth-popover-finary-connect");

let lastOpenaiOk = false;
let lastFinaryOk = false;

function updateAuthPills() {
  // OpenAI pill
  if (authPillOpenai) {
    authPillOpenai.className = lastOpenaiOk ? "auth-pill tone-ok" : "auth-pill tone-error";
  }
  // Finary pill
  if (authPillFinary) {
    authPillFinary.className = lastFinaryOk ? "auth-pill tone-ok" : "auth-pill tone-error";
  }
  // Main status pill — if either auth is bad, show warning
  if (!lastOpenaiOk) {
    const pill = document.getElementById("status-pill");
    const label = document.getElementById("status-pill-label");
    if (pill) pill.className = "status-pill tone-error";
    if (label) label.textContent = "OpenAI not connected";
  }
  // Auth popover statuses
  if (authPopoverOpenaiStatus) {
    authPopoverOpenaiStatus.textContent = lastOpenaiOk ? "connected" : "not connected";
    authPopoverOpenaiStatus.style.color = lastOpenaiOk ? "#2f8f5d" : "#ba4b3a";
  }
  if (authPopoverOpenaiLogin) authPopoverOpenaiLogin.classList.toggle("hidden", lastOpenaiOk);
  if (authPopoverOpenaiLogout) authPopoverOpenaiLogout.classList.toggle("hidden", !lastOpenaiOk);
  if (authPopoverFinaryStatus) {
    authPopoverFinaryStatus.textContent = lastFinaryOk ? "connected" : "session expired";
    authPopoverFinaryStatus.style.color = lastFinaryOk ? "#2f8f5d" : "#ba4b3a";
  }
  if (authPopoverFinaryConnect) {
    authPopoverFinaryConnect.textContent = lastFinaryOk ? "Reconnect" : "Connect";
  }
  // Disable Run Analysis when OpenAI not connected or API unreachable
  const cmdRunAnalysis = document.getElementById("cmd-run-analysis");
  if (cmdRunAnalysis) {
    const apiOk = isApiHealthy();
    const canRun = lastOpenaiOk && apiOk;
    cmdRunAnalysis.disabled = !canRun;
    cmdRunAnalysis.title = !lastOpenaiOk ? "Connect OpenAI first" : !apiOk ? "API unreachable — analysis needs market data" : "";
  }
}

async function refreshAccountStatus() {
  // OpenAI status
  try {
    const status = await bridge.getCodexSessionStatus();
    const r = status?.result || status;
    lastOpenaiOk = r?.logged_in === true;
    if (settingsOpenaiStatus) {
      settingsOpenaiStatus.textContent = lastOpenaiOk ? "connected" : "not connected";
      settingsOpenaiStatus.className = lastOpenaiOk ? "settings-account-status status-ok" : "settings-account-status status-error";
    }
    if (settingsOpenaiLoginBtn) settingsOpenaiLoginBtn.classList.toggle("hidden", lastOpenaiOk);
    if (settingsOpenaiLogoutBtn) settingsOpenaiLogoutBtn.classList.toggle("hidden", !lastOpenaiOk);
  } catch {
    lastOpenaiOk = false;
    if (settingsOpenaiStatus) { settingsOpenaiStatus.textContent = "error"; settingsOpenaiStatus.className = "settings-account-status status-error"; }
  }
  // Finary status
  try {
    await refreshFinarySessionStatus();
    lastFinaryOk = isFinarySessionRunnable(latestFinarySessionPayload);
    if (settingsFinaryStatus) {
      settingsFinaryStatus.textContent = lastFinaryOk ? "connected" : "session expired";
      settingsFinaryStatus.className = lastFinaryOk ? "settings-account-status status-ok" : "settings-account-status status-error";
    }
    if (settingsFinaryConnectBtn) settingsFinaryConnectBtn.classList.toggle("hidden", lastFinaryOk);
    if (settingsFinaryReconnectBtn) settingsFinaryReconnectBtn.classList.toggle("hidden", !lastFinaryOk);
  } catch {
    lastFinaryOk = false;
    if (settingsFinaryStatus) { settingsFinaryStatus.textContent = "error"; settingsFinaryStatus.className = "settings-account-status status-error"; }
    if (settingsFinaryConnectBtn) settingsFinaryConnectBtn.classList.remove("hidden");
    if (settingsFinaryReconnectBtn) settingsFinaryReconnectBtn.classList.add("hidden");
  }
  updateAuthPills();
}

// Auth pill click → toggle auth popover
function toggleAuthPopover() {
  if (authPopover) authPopover.classList.toggle("hidden");
}
authPillOpenai?.addEventListener("click", toggleAuthPopover);
authPillFinary?.addEventListener("click", toggleAuthPopover);
// Close auth popover on outside click
document.addEventListener("click", (e) => {
  if (authPopover && !authPopover.classList.contains("hidden") &&
      !authPopover.contains(e.target) &&
      e.target !== authPillOpenai && e.target !== authPillFinary &&
      !authPillOpenai?.contains(e.target) && !authPillFinary?.contains(e.target)) {
    authPopover.classList.add("hidden");
  }
});

// Auth popover actions
authPopoverOpenaiLogin?.addEventListener("click", async () => {
  authPopoverOpenaiLogin.disabled = true;
  authPopoverOpenaiLogin.textContent = "Signing in...";
  try {
    await bridge.codexSessionLogin();
    showToast("OpenAI connected", "success");
  } catch (error) {
    showToast(`Sign-in failed: ${formatBridgeError(error)}`, "error");
  }
  authPopoverOpenaiLogin.textContent = "Sign in";
  authPopoverOpenaiLogin.disabled = false;
  await refreshAccountStatus();
});
authPopoverOpenaiLogout?.addEventListener("click", async () => {
  authPopoverOpenaiLogout.disabled = true;
  try {
    await bridge.codexSessionLogout();
    lastOpenaiOk = false;
    updateAuthPills();
    showToast("OpenAI signed out", "warning");
  } catch (error) {
    showToast(`Sign-out failed: ${formatBridgeError(error)}`, "error");
  }
  authPopoverOpenaiLogout.disabled = false;
});
authPopoverFinaryConnect?.addEventListener("click", async () => {
  authPopoverFinaryConnect.disabled = true;
  authPopoverFinaryConnect.textContent = "Connecting...";
  try {
    await bridge.runFinaryPlaywrightBrowserSession();
    await refreshFinarySessionStatus();
    refreshWizardSourcePolicy(latestFinarySessionPayload);
    showToast("Finary connected", "success");
  } catch (error) {
    showToast(`Connection failed: ${formatBridgeError(error)}`, "error");
  }
  authPopoverFinaryConnect.textContent = lastFinaryOk ? "Reconnect" : "Connect";
  authPopoverFinaryConnect.disabled = false;
  await refreshAccountStatus();
});

// Refresh auth pills after startup
void refreshAccountStatus().catch(() => {});

settingsOpenaiLoginBtn?.addEventListener("click", async () => {
  settingsOpenaiLoginBtn.disabled = true;
  settingsOpenaiLoginBtn.textContent = "Signing in...";
  try {
    await bridge.codexSessionLogin();
    showToast("OpenAI connected", "success");
  } catch (error) {
    showToast(`OpenAI sign-in failed: ${formatBridgeError(error)}`, "error");
  }
  settingsOpenaiLoginBtn.textContent = "Sign in";
  settingsOpenaiLoginBtn.disabled = false;
  await refreshAccountStatus();
});

settingsOpenaiLogoutBtn?.addEventListener("click", async () => {
  settingsOpenaiLogoutBtn.disabled = true;
  settingsOpenaiLogoutBtn.textContent = "Signing out...";
  try {
    await bridge.codexSessionLogout();
    // Update UI immediately — don't wait for status check (app-server is stopped)
    if (settingsOpenaiStatus) {
      settingsOpenaiStatus.textContent = "signed out";
      settingsOpenaiStatus.className = "settings-account-status status-error";
    }
    if (settingsOpenaiLoginBtn) settingsOpenaiLoginBtn.classList.remove("hidden");
    settingsOpenaiLogoutBtn.classList.add("hidden");
    showToast("OpenAI signed out — sign in again to use a different account", "warning");
  } catch (error) {
    showToast(`Sign-out failed: ${formatBridgeError(error)}`, "error");
  }
  settingsOpenaiLogoutBtn.textContent = "Sign out";
  settingsOpenaiLogoutBtn.disabled = false;
});

async function handleFinaryConnect(btn, label) {
  btn.disabled = true;
  btn.textContent = "Connecting...";
  try {
    await bridge.runFinaryPlaywrightBrowserSession();
    await refreshFinarySessionStatus();
    refreshWizardSourcePolicy(latestFinarySessionPayload);
    showToast("Finary connected", "success");
  } catch (error) {
    showToast(`Finary connection failed: ${formatBridgeError(error)}`, "error");
  }
  btn.textContent = label;
  btn.disabled = false;
  await refreshAccountStatus();
}
settingsFinaryConnectBtn?.addEventListener("click", () => handleFinaryConnect(settingsFinaryConnectBtn, "Connect"));
settingsFinaryReconnectBtn?.addEventListener("click", () => handleFinaryConnect(settingsFinaryReconnectBtn, "Reconnect"));

// Refresh account status + storage when gear panel opens
document.getElementById("gear-btn")?.addEventListener("click", () => {
  refreshAccountStatus();
  refreshStorageUsage();
});

// ── Storage management ──────────────────────────────────────────

const storageUsageNode = document.getElementById("storage-usage");
const storageResultNode = document.getElementById("storage-result");

async function refreshStorageUsage() {
  if (!storageUsageNode) return;
  try {
    const usage = await bridge.getStorageUsage();
    storageUsageNode.textContent =
      `${usage.run_files || 0} run files (${usage.run_mb || "?"}MB) · Debug log: ${usage.log_mb || "?"}MB · Total: ${usage.total_mb || "?"}MB`;
  } catch {
    storageUsageNode.textContent = "Could not read storage usage";
  }
}

document.getElementById("storage-prune-btn")?.addEventListener("click", async () => {
  const btn = document.getElementById("storage-prune-btn");
  if (btn) { btn.disabled = true; btn.textContent = "Pruning..."; }
  if (storageResultNode) storageResultNode.textContent = "";
  try {
    const result = await bridge.pruneStorage(10);
    if (storageResultNode) {
      storageResultNode.textContent = `Removed ${result.removed || 0} files, freed ${result.freed_mb || "?"}MB`;
      storageResultNode.style.color = "#2f8f5d";
    }
    showToast(`Pruned ${result.removed || 0} old runs, freed ${result.freed_mb || "?"}MB`, "success");
  } catch (error) {
    if (storageResultNode) {
      storageResultNode.textContent = `Prune failed: ${formatBridgeError(error)}`;
      storageResultNode.style.color = "#ba4b3a";
    }
  }
  if (btn) { btn.disabled = false; btn.textContent = "Prune old runs (keep 10)"; }
  await refreshStorageUsage();
});

document.getElementById("storage-clear-log-btn")?.addEventListener("click", async () => {
  const btn = document.getElementById("storage-clear-log-btn");
  if (btn) { btn.disabled = true; btn.textContent = "Clearing..."; }
  if (storageResultNode) storageResultNode.textContent = "";
  try {
    const result = await bridge.clearDebugLog();
    if (storageResultNode) {
      storageResultNode.textContent = `Debug log cleared, freed ${result.freed_mb || "?"}MB`;
      storageResultNode.style.color = "#2f8f5d";
    }
    showToast(`Debug log cleared (${result.freed_mb || "?"}MB)`, "success");
  } catch (error) {
    if (storageResultNode) {
      storageResultNode.textContent = `Clear failed: ${formatBridgeError(error)}`;
      storageResultNode.style.color = "#ba4b3a";
    }
  }
  if (btn) { btn.disabled = false; btn.textContent = "Clear debug log"; }
  await refreshStorageUsage();
});

// ── Smart welcome screen ─────────────────────────────────────────

function renderWelcome() {
  const titleNode = document.getElementById("welcome-title");
  const contentNode = document.getElementById("welcome-content");
  if (!contentNode) return;

  const snapshot = latestDashboardPayload?.snapshot || {};
  const runs = Array.isArray(snapshot.runs) ? snapshot.runs : [];
  const latestRun = snapshot.latest_run || {};
  const finaryMeta = snapshot.latest_finary_snapshot || {};
  const accounts = Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [];
  const hasSnapshot = hasLatestFinarySnapshot(finaryMeta);
  const hasPositions = (latestRun.portfolio?.positions || []).length > 0;
  const recos = latestRun.pending_recommandations || latestRun.composed_payload?.recommandations || [];
  const hasSynthesis = !!(latestRun.composed_payload?.synthese_marche);
  const latestStatus = latestRun.orchestration?.status || "";
  const latestAccount = latestRun.account || "";

  let html = "";

  // ── API health warning (shown above everything else) ──
  if (!isApiHealthy()) {
    const apiStatus = latestStackHealthPayload?.status || "unreachable";
    html += `
      <div class="welcome-step welcome-step-error">
        <h3>API ${apiStatus === "unreachable" ? "Unreachable" : "Degraded"}</h3>
        <p>The enrichment API is ${apiStatus}. Analysis is disabled — market data and news cannot be fetched.</p>
        <p style="font-size:0.8rem;color:var(--sea-muted)">You can still browse existing reports and data.</p>
      </div>
    `;
  }

  // ── Step 1: OpenAI connection ──
  if (!lastOpenaiOk) {
    if (titleNode) titleNode.textContent = "Connect to OpenAI";
    html += `
      <div class="welcome-step welcome-step-error">
        <h3>OpenAI / Codex not connected</h3>
        <p>Alfred needs an OpenAI account to analyze your portfolio. Sign up at <a href="https://platform.openai.com/signup" target="_blank" style="color:#8ecae6">platform.openai.com</a> if you don't have one.</p>
        <button class="cmd-btn" onclick="window.__connectOpenai()">Sign in to OpenAI</button>
      </div>
    `;
    if (!lastFinaryOk) {
      const snapshotHint = hasSnapshot
        ? "A previous Finary snapshot is available — you can also <strong>analyze the latest snapshot</strong> or <strong>import a CSV</strong>."
        : "You'll also need portfolio data — either <strong>connect Finary</strong> for automatic sync or <strong>import a CSV</strong> file.";
      html += `
        <div class="welcome-step" style="margin-top:0.6rem">
          <p style="font-size:0.82rem;color:var(--sea-muted)">${snapshotHint}</p>
          <button class="cmd-btn ghost-btn cmd-btn-sm" style="margin-top:0.4rem" onclick="window.__connectFinary()">Connect Finary</button>
        </div>
      `;
    }
    contentNode.innerHTML = html;
    return;
  }

  // ── Step 2: Finary / data source ──
  if (!lastFinaryOk && accounts.length === 0 && runs.length === 0) {
    if (titleNode) titleNode.textContent = "Import your portfolio";
    html += `
      <div class="welcome-step">
        <h3 style="margin-bottom:0.5rem">Connect Finary or import a CSV</h3>
        <p style="color:var(--sea-muted);font-size:0.85rem;margin-bottom:1rem">Alfred needs your portfolio data to get started.</p>
        <div style="display:flex;gap:0.5rem;justify-content:center">
          <button class="cmd-btn" onclick="window.__connectFinary()">Connect Finary</button>
          <button class="cmd-btn ghost-btn" onclick="document.getElementById('cmd-run-analysis')?.click()">Import CSV</button>
        </div>
        <p style="color:var(--sea-muted);font-size:0.75rem;margin-top:0.8rem">Finary syncs your brokerage accounts automatically. CSV import supports any broker format.</p>
      </div>
    `;
    contentNode.innerHTML = html;
    return;
  }

  // ── Step 3: Connected but no accounts in Finary ──
  // Only show this if we actually have a snapshot (accounts fetched) — not if snapshot is pending
  if (lastFinaryOk && hasSnapshot && accounts.length === 0 && runs.length === 0) {
    if (titleNode) titleNode.textContent = "Setup your accounts";
    html += `
      <div class="welcome-step">
        <h3>No accounts found in Finary</h3>
        <p>Your Finary session is connected but no brokerage accounts were found.<br/>Make sure you have linked your PEA, CTO, or other accounts in Finary first.</p>
        <p style="font-size:0.8rem;color:var(--sea-muted)">Go to <a href="https://app.finary.com/connections" target="_blank" style="color:#8ecae6">Finary Connections</a> to add your accounts, then come back and run analysis.</p>
        <div style="display:flex;gap:0.5rem;margin-top:0.6rem;justify-content:center">
          <button class="cmd-btn" onclick="document.getElementById('cmd-run-analysis')?.click()">Run Analysis</button>
          <button class="cmd-btn ghost-btn" onclick="document.getElementById('cmd-run-analysis')?.click()">Import CSV instead</button>
        </div>
      </div>
    `;
    contentNode.innerHTML = html;
    return;
  }

  // ── Step 3b: Finary connected, snapshot not loaded yet, no runs ──
  if (lastFinaryOk && !hasSnapshot && runs.length === 0) {
    if (titleNode) titleNode.textContent = "Ready to analyze";
    html += `
      <div class="welcome-step">
        <h3 style="margin-bottom:0.5rem">Finary connected</h3>
        <p style="color:var(--sea-muted);font-size:0.85rem;margin-bottom:1rem">Run your first analysis to sync portfolio data and generate recommendations.</p>
        <button class="cmd-btn" onclick="document.getElementById('cmd-run-analysis')?.click()">Run Analysis</button>
      </div>
    `;
    contentNode.innerHTML = html;
    return;
  }


  const globalSynthesisCard = (() => {
    if (accounts.length === 0 && runs.length === 0) return "";
    if (globalHomeSynthesis.status === "loading") {
      return `
      <div class="welcome-step welcome-global-summary">
        <h3>Portfolio-wide summary</h3>
        <p class="welcome-global-loading">Computing cross-portfolio allocation insights…</p>
      </div>
      `;
    }
    if (globalHomeSynthesis.status === "error") {
      return `
      <div class="welcome-step welcome-global-summary">
        <h3>Portfolio-wide summary</h3>
        <p style="color:var(--sea-muted)">Global synthesis is temporarily unavailable (${escapeHtml(globalHomeSynthesis.error || "unknown_error")}).</p>
      </div>
      `;
    }
    const data = globalHomeSynthesis.data;
    if (!data) return "";
    const topSupport = Array.isArray(data.supportBreakdown) ? data.supportBreakdown[0] : null;
    const suggestions = Array.isArray(data.suggestions) ? data.suggestions.slice(0, 3) : [];
    return `
      <div class="welcome-step welcome-global-summary">
        <h3>Portfolio-wide summary</h3>
        <p style="margin-bottom:0.45rem"><strong>${escapeHtml(data.verdict)}</strong> · ${data.accountCount} account(s) · Cash ${data.cashWeightPct.toFixed(1)}%</p>
        <p style="margin-bottom:0.45rem">Total assets: <strong>${formatCurrency(data.totalValue)}</strong> · P/L: <strong>${formatCurrency(data.totalGain)}</strong></p>
        <p style="color:var(--sea-muted);margin-bottom:0.5rem">Top support: ${topSupport ? `${escapeHtml(topSupport.name)} (${topSupport.weightPct.toFixed(1)}%)` : "Not enough data"}</p>
        ${suggestions.length > 0
          ? `<ul class="welcome-global-suggestions">${suggestions.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul>`
          : `<p style="color:var(--sea-muted)">No major imbalance detected across your current account and support mix.</p>`}
        <div style="display:flex;gap:0.4rem;margin-top:0.6rem">
          <button class="ghost-btn" type="button" onclick="window.__askGlobalHomeSummary()">💬 Ask about this</button>
          <button class="ghost-btn" type="button" onclick="window.__openGlobalSummaryDiscussions()">Previous discussions</button>
        </div>
      </div>
    `;
  })();

  // ── Step 4: Has runs — show latest status per account ──
  const accountRuns = new Map();
  for (const run of runs.slice(0, 20)) {
    const acct = run.account || "All accounts";
    if (!accountRuns.has(acct)) accountRuns.set(acct, run);
  }

  if (titleNode) titleNode.textContent = accountRuns.size > 0 ? "Latest runs" : "Ready";

  if (globalSynthesisCard) html += globalSynthesisCard;

  if (accountRuns.size > 0) {
    html += `<div class="welcome-accounts">`;
    for (const [acct, run] of accountRuns) {
      const status = run.status || "unknown";
      const recoCount = run.recommendation_count || run.pending_recommendations_count || 0;
      const updatedAt = run.updated_at ? new Date(run.updated_at).toLocaleDateString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "?";
      const statusDot = status === "completed" ? "dot-ok" : status === "completed_degraded" ? "dot-degraded" : status === "failed" ? "dot-error" : "dot-unknown";
      const statusLabel = status === "completed" ? "Analysis complete" : status === "completed_degraded" ? "Partial analysis" : status === "failed" ? "Failed" : status === "running" ? "Running..." : "Unknown";
      const runId = run.run_id || "";

      const borderColor = accountAccentColor(acct);
      const cardClick = runId ? `onclick="window.__viewRun('${escapeHtml(runId)}')"` : "";
      html += `
        <div class="welcome-account-card ${runId ? 'clickable' : ''}" ${cardClick} style="border-left: 3px solid ${borderColor}">
          <div class="welcome-account-row">
            <div class="welcome-account-info">
              <div class="welcome-account-header">
                <span class="welcome-dot ${statusDot}"></span>
                <strong>${escapeHtml(acct)}</strong>
                <span class="welcome-meta">${updatedAt}</span>
              </div>
              <div class="welcome-account-stats">
                <span>${recoCount} recommendations</span>
                <span>${statusLabel}</span>
              </div>
            </div>
            <div class="welcome-account-actions">
              <button class="cmd-btn cmd-btn-sm ghost-btn" onclick="event.stopPropagation(); window.__openWizardForAccount('${escapeHtml(acct)}')">New analysis</button>
            </div>
          </div>
        </div>
      `;
    }
    html += `</div>`;
    if (!lastFinaryOk) {
      const hint = hasSnapshot
        ? "Finary not connected — analysis on latest snapshot or CSV import available."
        : "Finary not connected — only CSV import is available.";
      html += `
        <div style="display:flex;align-items:center;gap:0.5rem;margin-top:0.8rem;padding:0.5rem 0.7rem;border:1px solid rgba(73,100,126,0.25);border-radius:8px;background:rgba(14,24,34,0.6)">
          <span style="font-size:0.82rem;color:var(--sea-muted);flex:1">${hint}</span>
          <button class="cmd-btn cmd-btn-sm ghost-btn" onclick="window.__connectFinary()">Connect Finary</button>
        </div>
      `;
    }
  } else {
    html += `
      <div class="welcome-step">
        <p>Everything is connected. Start your first analysis!</p>
        <button class="cmd-btn" onclick="document.getElementById('cmd-run-analysis')?.click()">Run Analysis</button>
      </div>
    `;
  }

  contentNode.innerHTML = html;
}

// ── Onboarding result processing (Phase 4b) ────────────────────
// The onboarding chat wizard is launched by the alfred-onboarding-incomplete
// trigger in app-alfred-triggers.js. This module handles the result processing
// (intent extraction, API key saving, Finary connection, first-run trigger)
// because it has access to app-level functions (showToast, refreshAccountStatus, etc).

/**
 * Parse onboarding conversation history and trigger appropriate actions.
 * Called by launchOnboardingWizard() in app-alfred-triggers.js via window bridge.
 */
async function processOnboardingResult(history) {
  const tauriInvoke = window?.__TAURI__?.core?.invoke;

  // Scan conversation for user intent signals
  const allText = history.map((m) => m.content).join("\n").toLowerCase();
  const userMessages = history.filter((m) => m.role === "user").map((m) => m.content.toLowerCase());
  const lastUserMsg = userMessages[userMessages.length - 1] || "";

  let wantsFinary = false;
  let wantsNativeApi = false;
  let apiKeyProvided = null;
  let wantsRunAnalysis = false;

  // Detect Finary intent
  const finaryYesPatterns = /\b(finary|oui.*finary|yes.*finary|j'ai.*finary|i have.*finary|connect.*finary)\b/;
  if (finaryYesPatterns.test(allText)) {
    wantsFinary = true;
  }

  // Detect LLM backend choice
  let wantsCodex = false;
  const nativePatterns = /\b(native|api key|api.key|cl[e\u00e9].*api|openai.*key|pay.per.use)\b/;
  const codexPatterns = /\b(codex|free|gratuit|oauth|option 1|choix 1)\b/;
  if (nativePatterns.test(allText)) {
    wantsNativeApi = true;
  } else if (codexPatterns.test(allText)) {
    wantsCodex = true;
  }

  // Detect API key in user messages (OpenAI keys start with sk-)
  for (const msg of history) {
    if (msg.role !== "user") continue;
    const keyMatch = msg.content.match(/\b(sk-[a-zA-Z0-9_-]{20,})\b/);
    if (keyMatch) {
      apiKeyProvided = keyMatch[1];
      wantsNativeApi = true;
    }
  }

  // Detect "run analysis" intent
  const runPatterns = /\b(oui|yes|ok|go|lance|start|run|d[e\u00e9]marr|analys)\b/;
  if (runPatterns.test(lastUserMsg)) {
    wantsRunAnalysis = true;
  }

  // Execute actions based on detected intent
  // 1. Save API key if provided
  if (apiKeyProvided && tauriInvoke) {
    try {
      await tauriInvoke("runtime_settings_update_local", {
        settings: {
          llm_backend: "native",
          openai_api_key: apiKeyProvided,
        }
      });
      showToast("API key saved", "success");
      await refreshRuntimeSettings();
      await refreshAccountStatus();
      updateAuthPills();
    } catch (err) {
      showToast(`Failed to save API key: ${formatBridgeError(err)}`, "error");
    }
  }

  // 2. Trigger Finary connection if requested (non-blocking — runs in background)
  if (wantsFinary) {
    window.__connectFinary?.();
  }

  // 2b. Trigger Codex OAuth login if user chose Codex (and no API key was provided)
  if (wantsCodex && !apiKeyProvided) {
    try {
      await bridge.codexSessionLogin();
      showToast("OpenAI connected via Codex", "success");
      await refreshAccountStatus();
      updateAuthPills();
    } catch (err) {
      showToast(`Codex sign-in: ${formatBridgeError(err)}`, "error");
    }
  }

  // 3. Mark onboarding complete
  if (tauriInvoke) {
    try {
      await tauriInvoke("save_user_preferences_local", {
        prefs: { onboarding_complete: true }
      });
    } catch { /* not critical */ }
  }

  // 4. Refresh state after onboarding
  await refreshAccountStatus();
  updateAuthPills();
  renderWelcome();

  // 5. Trigger run wizard if user wants to run analysis
  if (wantsRunAnalysis) {
    // Small delay so the welcome screen renders first
    setTimeout(() => openRunWizard({}), 400);
  }
}

// Open wizard with pre-selected account (used by welcome cards + sidebar)
window.__openWizardForAccount = (accountName) => {
  wizard.open({ account: accountName });
};

// Phase C: bridge functions for proactive triggers
window.__processOnboardingResult = (history) => processOnboardingResult(history);
window.__openCashWizard = (groups) => {
  if (groups && groups.length > 0) {
    const group = groups[0];
    const investmentAccounts = group.investment_accounts || [];
    const cashAccounts = group.cash_accounts || [];
    if (investmentAccounts.length > 0 && cashAccounts.length > 0) {
      openCashMatchingWizard({
        investmentAccounts,
        cashAccounts,
        title: "Link Cash Accounts"
      });
    }
  }
};

// View a specific run from the welcome page
window.__viewRun = (runId) => {
  // Programmatically click the run entry in the sidebar if it exists
  const entry = document.querySelector(`.run-entry[data-run-id="${runId}"]`);
  if (entry) {
    entry.click();
  }
};

// Connect OpenAI from welcome page
window.__connectOpenai = async () => {
  try {
    await bridge.codexSessionLogin();
    await refreshAccountStatus();
    updateAuthPills();
    renderWelcome();
    showToast("OpenAI connected successfully.", "success");
  } catch (error) {
    showToast(`OpenAI sign-in failed: ${formatBridgeError(error)}`, "error");
  }
};

// Connect Finary from welcome page
window.__connectFinary = async () => {
  try {
    showToast("Connecting to Finary...", "info");
    await bridge.runFinaryPlaywrightBrowserSession();
    await refreshAccountStatus();
    updateAuthPills();
    // Only sync from Finary API if we don't have accounts yet
    const snap = latestDashboardPayload?.snapshot || {};
    const fMeta = snap.latest_finary_snapshot || {};
    const accts = Array.isArray(fMeta.accounts) ? fMeta.accounts : [];
    const tauriInv = window?.__TAURI__?.core?.invoke;
    if (accts.length === 0 && tauriInv) {
      showToast("Syncing portfolio\u2026", "info");
      try {
        await tauriInv("finary_sync_snapshot_local");
        await refreshDashboard();
      } catch (err) {
        showToast(`Portfolio sync failed: ${formatBridgeError(err)}. You can use CSV import instead.`, "error");
      }
    }
    renderWelcome();
    showToast("Finary connected", "success");
  } catch (error) {
    showToast(`Finary connection failed: ${formatBridgeError(error)}`, "error");
  }
};

// Home button → show welcome screen, clear run view
document.getElementById("home-btn")?.addEventListener("click", () => {
  const mainWelcome = document.getElementById("main-welcome");
  const mainRunView = document.getElementById("main-run-view");
  const mainAccountView = document.getElementById("main-account-view");
  const overviewPanel = document.getElementById("tab-overview-panel");
  if (mainWelcome) mainWelcome.classList.remove("hidden");
  if (mainRunView) mainRunView.classList.add("hidden");
  if (mainAccountView) mainAccountView.classList.add("hidden");
  if (overviewPanel) overviewPanel.classList.add("hidden");
  // Deselect in sidebar
  document.querySelectorAll(".run-entry.is-selected, .account-folder.is-selected").forEach((el) => el.classList.remove("is-selected"));
  setRetrySynthesisVisible(false);
  clearRunPipelineBar();
  renderWelcome();
});

// Boot
wizard.close();
setLineMemoryModalVisible(false);
void refreshRuntimeSettings().catch(() => {});
bootstrap.runStartupSessionCheck().then(async () => {
  await refreshAccountStatus();
  updateAuthPills();
  renderWelcome();
  // If Finary is connected but we have no accounts, sync from Finary API
  const snapshot = latestDashboardPayload?.snapshot || {};
  const finaryMeta = snapshot.latest_finary_snapshot || {};
  const accounts = Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [];
  const tauriInvokePost = window?.__TAURI__?.core?.invoke;
  if (lastFinaryOk && accounts.length === 0 && tauriInvokePost) {
    const welcomeTitle = document.getElementById("welcome-title");
    if (welcomeTitle) welcomeTitle.textContent = "Syncing portfolio\u2026";
    try {
      await tauriInvokePost("finary_sync_snapshot_local");
      await refreshDashboard();
    } catch (err) {
      showToast(`Portfolio sync failed: ${formatBridgeError(err)}. You can use CSV import instead.`, "error");
    }
    renderWelcome();
  }

  // Phase C: load Alfred suggestions preference and notify app-ready
  if (tauriInvokePost) {
    try {
      const prefsForAlfred = await tauriInvokePost("get_user_preferences_local") || {};
      const suggestionsEnabled = prefsForAlfred?.alfred_suggestions_enabled !== false; // default: on
      alfredOverlay.setAlfredSuggestionsEnabled(suggestionsEnabled);
    } catch { /* not critical — default is enabled */ }
  }
  alfredOverlay.notify("app-ready", {});
}).catch((err) => {
  bootstrap.showStartupError("Startup failed", String(err?.message || err));
});
