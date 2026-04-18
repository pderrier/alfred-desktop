/**
 * Run wizard — extracted from app.js for SOLID separation.
 * Handles: wizard modal, source/mode selection, account selection, guidelines, submit.
 */
import {
  deriveRunWizardModeOptions,
  isFinarySessionRunnable,
  hasCsvRunnableInput
} from "/desktop-shell/run-wizard-policy.js";
import { buildRunAnalysisOptions } from "/desktop-shell/report-view-model.js";
import { formatBridgeError, isErrorCritical, extractErrorCode } from "/shared/run-operations-controller.js";
import { openCashMatchingWizard, openChatWizard } from "/desktop-shell/app-chat-wizard.js";

// ── Helpers ──────────────────────────────────────────────────────

/**
 * Strip trailing parenthesized display text and resolve against canonical names.
 * E.g. "Livret A (24,029.00 €)" → "Livret A" → matched to canonical "Livret A".
 */
function resolveWizardName(rawName, canonicalNames) {
  const stripped = (rawName || "").replace(/\s*\([^)]*\)\s*$/, "").trim();
  if (canonicalNames.includes(stripped)) return stripped;
  const lower = stripped.toLowerCase();
  const ci = canonicalNames.find((n) => n.toLowerCase() === lower);
  if (ci) return ci;
  const sub = canonicalNames.find(
    (n) => lower.includes(n.toLowerCase()) || n.toLowerCase().includes(lower)
  );
  if (sub) return sub;
  return stripped;
}

// ── DOM nodes ────────────────────────────────────────────────────

const runWizardNode = document.getElementById("run-wizard");
const wizardCloseBtn = document.getElementById("wizard-close-btn");
const wizardStatusNode = document.getElementById("wizard-status");
const wizardRunModeNode = document.getElementById("wizard-run-mode");
const wizardRunModeHelpNode = document.getElementById("wizard-run-mode-help");
const wizardRunSubmitBtn = document.getElementById("wizard-run-submit-btn");
const wizardCsvInputsNode = document.getElementById("wizard-csv-inputs");
const wizardFinaryConnectBtn = document.getElementById("wizard-finary-connect-btn");
const wizardCsvTextNode = document.getElementById("wizard-csv-text");
const wizardCsvPathNode = document.getElementById("wizard-csv-path");
const wizardCsvFileInput = document.getElementById("wizard-csv-file");
const wizardCsvFileText = document.getElementById("wizard-csv-file-text");
const wizardCsvFileList = document.getElementById("wizard-csv-file-list");

// File picker → read contents into hidden textarea
wizardCsvFileInput?.addEventListener("change", async () => {
  const files = Array.from(wizardCsvFileInput.files || []);
  if (wizardCsvFileList) {
    wizardCsvFileList.innerHTML = files.map((f) => `<li>${f.name} (${(f.size / 1024).toFixed(0)}KB)</li>`).join("");
  }
  if (wizardCsvFileText) {
    wizardCsvFileText.textContent = files.length > 0 ? `${files.length} file(s) selected` : "Choose CSV files...";
  }
  // Read all file contents and concatenate into the hidden textarea
  const contents = [];
  for (const file of files) {
    try {
      const text = await file.text();
      contents.push(`--- FILE: ${file.name} ---\n${text}`);
    } catch { /* skip unreadable files */ }
  }
  if (wizardCsvTextNode) {
    wizardCsvTextNode.value = contents.join("\n\n");
  }
});
const agentGuidelinesInputNode = document.getElementById("agent-guidelines-input");
const wizardAnalysisModeNode = document.getElementById("wizard-analysis-mode");
const wizardAnalysisModeHelpNode = document.getElementById("wizard-analysis-mode-help");
const wizardAccountSelect = document.getElementById("wizard-account-select");
const wizardNewAccountName = document.getElementById("wizard-new-account-name");

// Show/hide the new account text input based on combo selection
wizardAccountSelect?.addEventListener("change", () => {
  if (wizardNewAccountName) {
    const isNew = wizardAccountSelect.value === "__new__";
    wizardNewAccountName.classList.toggle("hidden", !isNew);
    if (isNew) wizardNewAccountName.focus();
  }
});

// ── Analysis mode help ───────────────────────────────────────────

const ANALYSIS_MODE_HELP = {
  full_run: "Collect fresh data, analyze every position, and generate a full synthesis. Most thorough but uses the most tokens.",
  refresh_synthesis: "Re-collect market data and news, reuse existing recommendations (except expired ones), and regenerate the synthesis. Fast and cheap.",
  retry_failed: "Re-analyze only failed/aborted lines from the last run, merge with existing recommendations, then regenerate the synthesis."
};

// ── Public API ───────────────────────────────────────────────────

/**
 * Initialize the wizard module.
 * @param {Object} deps - external dependencies from app.js
 * @param {Function} deps.getLatestDashboardPayload
 * @param {Function} deps.getLatestFinarySessionPayload
 * @param {Object}   deps.bridge
 * @param {Object}   deps.runOperations
 * @param {Function} deps.refreshFinarySessionStatus
 * @param {Function} deps.resolveCurrentFinarySession
 * @param {Function} deps.getSelectedAccount
 * @param {Function} deps.showErrorModal
 * @param {Function} deps.showToast
 * @param {Function} deps.populateWizardAccounts
 * @param {Function} deps.setRetrySynthesisVisible
 */
export function initWizard(deps) {
  const {
    getLatestDashboardPayload,
    getLatestFinarySessionPayload,
    bridge,
    runOperations,
    refreshFinarySessionStatus,
    resolveCurrentFinarySession,
    getSelectedAccount,
    showErrorModal,
    showToast,
    populateWizardAccounts,
    setRetrySynthesisVisible
  } = deps;

  // ── Internal helpers ─────────────────────────────────────────

  function setWizardVisible(visible) {
    runWizardNode?.classList.toggle("hidden", !visible);
    if (!visible && wizardStatusNode) {
      wizardStatusNode.className = "status-idle";
      wizardStatusNode.textContent = "Choose how Alfred should get your portfolio data.";
    }
  }

  function setWizardStatus(text, cls = "status-idle") {
    if (wizardStatusNode) {
      wizardStatusNode.className = cls;
      wizardStatusNode.textContent = text;
    }
  }

  function setWizardStep() {
    const selected = selectedRunModeOption();
    const isCsv = selected?.csv === true;
    wizardCsvInputsNode?.classList.toggle("hidden", !isCsv);
  }

  function refreshWizardSourcePolicy(sessionPayload) {
    const snapshot = getLatestDashboardPayload()?.snapshot || {};
    const options = deriveRunWizardModeOptions({
      sessionPayload,
      latestFinarySnapshot: snapshot.latest_finary_snapshot || null
    });
    if (wizardRunModeNode) {
      wizardRunModeNode.innerHTML = "";
      for (const opt of options) {
        const el = document.createElement("option");
        el.value = opt.value;
        el.textContent = opt.label;
        // Disable options that require a Finary session when not connected
        if (opt.requiresRunnableSession && !isFinarySessionRunnable(sessionPayload)) {
          el.disabled = true;
          el.textContent += " (Finary required)";
        }
        wizardRunModeNode.appendChild(el);
      }
      // Auto-select first enabled option
      const firstEnabled = wizardRunModeNode.querySelector("option:not(:disabled)");
      if (firstEnabled) wizardRunModeNode.value = firstEnabled.value;
    }
    // Disable submit when OpenAI not connected (mirrors top bar Run Analysis state)
    if (wizardRunSubmitBtn) {
      const openaiDisabled = document.getElementById("cmd-run-analysis")?.disabled === true;
      wizardRunSubmitBtn.disabled = openaiDisabled;
      wizardRunSubmitBtn.title = openaiDisabled ? "Connect OpenAI first" : "";
    }
  }

  function selectedRunModeOption() {
    const value = wizardRunModeNode?.value || "";
    const snapshot = getLatestDashboardPayload()?.snapshot || {};
    const options = deriveRunWizardModeOptions({
      sessionPayload: getLatestFinarySessionPayload(),
      latestFinarySnapshot: snapshot.latest_finary_snapshot || null
    });
    return options.find((opt) => opt.value === value) || options[0] || null;
  }

  function refreshWizardAnalysisModes() {
    if (!wizardAnalysisModeNode) return;
    const snapshot = getLatestDashboardPayload()?.snapshot || {};
    const latestRun = snapshot.latest_run || {};
    const lineStatus = latestRun.line_status || {};
    const hasFailedLines = Object.values(lineStatus).some((v) => {
      const s = typeof v === "object" ? (v?.status || "") : String(v || "");
      return s === "failed" || s === "aborted" || s === "error";
    });
    const hasAnyRun = !!latestRun.run_id;
    for (const opt of wizardAnalysisModeNode.options) {
      if (opt.value === "retry_failed") {
        opt.disabled = !hasFailedLines;
        opt.textContent = hasFailedLines
          ? `Retry failed lines + synthesis (${Object.values(lineStatus).filter((v) => { const s = typeof v === "object" ? (v?.status || "") : String(v || ""); return s === "failed" || s === "aborted" || s === "error"; }).length} failed)`
          : "Retry failed lines (no failures)";
      }
      if (opt.value === "refresh_synthesis") {
        opt.disabled = !hasAnyRun;
      }
    }
  }

  async function loadGuidelinesForAccount(account) {
    if (!agentGuidelinesInputNode || !account) return;
    try {
      const tauriInvoke = window?.__TAURI__?.core?.invoke;
      if (!tauriInvoke) return;
      const prefs = await tauriInvoke("get_user_preferences_local") || {};
      const guidelines = prefs?.guidelines_by_account?.[account] || "";
      agentGuidelinesInputNode.value = guidelines;
    } catch { /* no prefs yet */ }
  }

  async function saveGuidelinesForAccount(account, guidelines) {
    try {
      const tauriInvoke = window?.__TAURI__?.core?.invoke;
      if (!tauriInvoke) return;
      const prefs = await tauriInvoke("get_user_preferences_local") || {};
      if (!prefs.guidelines_by_account) prefs.guidelines_by_account = {};
      prefs.guidelines_by_account[account] = guidelines;
      await tauriInvoke("save_user_preferences_local", { prefs });
    } catch { /* save failed */ }
  }

  // ── Account mismatch modal ─────────────────────────────────────

  /**
   * Parse `account_mismatch:<selected>:<["csv_acct1","csv_acct2"]>:<N> positions`
   * and show a modal letting the user pick: use CSV account, keep selected, or cancel.
   */
  async function handleAccountMismatch(errorMsg, originalOptions) {
    // Parse `account_mismatch:<selected>:<["csv_acct1","csv_acct2"]>:<N> positions`
    const parts = errorMsg.split("account_mismatch:").pop() || "";
    const colonIdx1 = parts.indexOf(":");
    const selectedAccount = parts.substring(0, colonIdx1);
    const rest = parts.substring(colonIdx1 + 1);
    let csvAccounts = [];
    try {
      const jsonEnd = rest.indexOf("]") + 1;
      csvAccounts = JSON.parse(rest.substring(0, jsonEnd));
    } catch { /* fallback */ }

    if (csvAccounts.length === 0) return false;

    return new Promise((resolve) => {
      let modal = document.getElementById("account-mismatch-modal");
      if (modal) modal.remove();

      modal = document.createElement("div");
      modal.id = "account-mismatch-modal";
      modal.className = "modal-overlay";

      // Build CSV account buttons
      const csvList = csvAccounts
        .map((a) => `<button class="cmd-btn csv-acct-btn" data-acct="${a}">${a}</button>`)
        .join("");

      const multipleAccounts = csvAccounts.length > 1;
      const subtitle = multipleAccounts
        ? `The CSV contains <strong>${csvAccounts.length} accounts</strong>. Pick the one to import (one account per run):`
        : `The CSV account is "<strong>${csvAccounts[0]}</strong>".`;

      modal.innerHTML = `
        <div class="modal-card" style="max-width:30rem">
          <h3 style="margin:0 0 0.6rem">Account name mismatch</h3>
          <p style="font-size:0.85rem;color:var(--sea-muted);margin:0 0 0.8rem">
            You selected "<strong>${selectedAccount}</strong>" but no positions match.
            ${subtitle}
          </p>
          <div style="display:flex;flex-direction:column;gap:0.4rem;margin-bottom:1rem">
            <p style="font-size:0.75rem;color:var(--sea-muted);margin:0">Use the CSV account name:</p>
            <div style="display:flex;flex-wrap:wrap;gap:0.4rem">${csvList}</div>
          </div>
          <div style="display:flex;flex-direction:column;gap:0.4rem;margin-bottom:1rem">
            <p style="font-size:0.75rem;color:var(--sea-muted);margin:0">Or rename all positions to your chosen name:</p>
            <button id="acct-mismatch-keep" class="cmd-btn ghost-btn">Import as "${selectedAccount}"</button>
          </div>
          <div style="display:flex;justify-content:flex-end">
            <button id="acct-mismatch-cancel" class="cmd-btn ghost-btn">Cancel</button>
          </div>
        </div>
      `;
      document.body.appendChild(modal);

      function close() { modal.remove(); }

      async function rerunWith(acct, forceRename) {
        close();
        if (forceRename) {
          originalOptions.__force_account = acct;
        } else {
          originalOptions.account = acct;
        }
        try { await runOperations.runAnalysis(originalOptions); } catch { /* handled elsewhere */ }
        resolve(true);
      }

      // Pick a CSV account → rerun with that exact account
      for (const btn of modal.querySelectorAll(".csv-acct-btn")) {
        btn.addEventListener("click", () => rerunWith(btn.dataset.acct, false));
      }

      // Keep selected name → force-rename positions
      document.getElementById("acct-mismatch-keep")?.addEventListener("click", () => {
        rerunWith(selectedAccount, true);
      });

      // Cancel
      document.getElementById("acct-mismatch-cancel")?.addEventListener("click", () => {
        close();
        resolve(false);
      });

      modal.addEventListener("click", (e) => { if (e.target === modal) { close(); resolve(false); } });
    });
  }

  function displayError(error, context) {
    const formatted = formatBridgeError(error);
    if (isErrorCritical(error)) {
      const code = extractErrorCode(error);
      const title = context ? `${context} failed` : "Error";
      const hint = formatted.includes("(hint:") ? formatted.split("(hint: ")[1]?.replace(")", "") : "";
      showErrorModal(title, error?.message || code, hint);
    } else {
      showToast(formatted, "error");
    }
  }

  function getSelectedWizardAccount() {
    const val = wizardAccountSelect?.value || "";
    if (val === "__new__") {
      return wizardNewAccountName?.value?.trim() || "";
    }
    return val;
  }

  // ── Public methods ───────────────────────────────────────────

  function open({ statusText, statusClass, account } = {}) {
    refreshWizardSourcePolicy(getLatestFinarySessionPayload());
    setWizardStep();
    if (wizardFinaryConnectBtn) wizardFinaryConnectBtn.classList.add("hidden");
    // Account priority: explicit param > sidebar selection > first in list
    const targetAccount = account || getSelectedAccount() || getSelectedWizardAccount();
    if (targetAccount && wizardAccountSelect) {
      wizardAccountSelect.value = targetAccount;
    }
    loadGuidelinesForAccount(targetAccount || getSelectedWizardAccount());
    refreshWizardAnalysisModes();
    setWizardVisible(true);
    // Notify Alfred — strategy refinement trigger checks if guidelines are empty
    window.__alfredOverlay?.notify?.("run-wizard-opened", { account: targetAccount || getSelectedWizardAccount() });
    if (statusText) setWizardStatus(statusText, statusClass || "status-idle");
  }

  function close() {
    setWizardVisible(false);
  }

  function refreshSourcePolicy(sessionPayload) {
    refreshWizardSourcePolicy(sessionPayload);
    const snapshot = getLatestDashboardPayload()?.snapshot || {};
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
  }

  // ── Event wiring ─────────────────────────────────────────────

  wizardCloseBtn?.addEventListener("click", close);
  wizardRunModeNode?.addEventListener("change", setWizardStep);
  wizardAnalysisModeNode?.addEventListener("change", () => {
    const mode = wizardAnalysisModeNode.value || "full_run";
    if (wizardAnalysisModeHelpNode) wizardAnalysisModeHelpNode.textContent = ANALYSIS_MODE_HELP[mode] || "";
  });
  wizardAccountSelect?.addEventListener("change", (e) => {
    loadGuidelinesForAccount(e.target.value);
  });

  wizardRunSubmitBtn?.addEventListener("click", async () => {
    try {
      const selected = selectedRunModeOption();
      if (!selected) { setWizardStatus("Choose a valid source.", "status-error"); return; }
      let source = selected.source;
      if (source === "finary") {
        const current = await resolveCurrentFinarySession();
        if (!isFinarySessionRunnable(current)) {
          setWizardStatus("Finary session expired. Reconnect or use CSV.", "status-error");
          if (wizardFinaryConnectBtn) wizardFinaryConnectBtn.classList.remove("hidden");
          return;
        }
      } else if (source === "csv") {
        if (!hasCsvRunnableInput({ csvText: wizardCsvTextNode?.value, csvExportPath: wizardCsvPathNode?.value })) {
          setWizardStatus("Provide CSV text or an export path.", "status-error");
          return;
        }
      } else {
        source = "finary_cached";
      }
      const account = getSelectedWizardAccount();
      if (!account) {
        setWizardStatus("Enter an account name.", "status-error");
        if (wizardNewAccountName) wizardNewAccountName.focus();
        return;
      }
      const guidelines = agentGuidelinesInputNode?.value || "";
      const analysisMode = wizardAnalysisModeNode?.value || "full_run";

      // ── CSV preview + chat confirmation flow ─────────────────────
      // For CSV text imports, preview the parsing before committing.
      // If transaction history is detected with warnings or unknown format,
      // open the chat wizard for user confirmation.
      if (source === "csv" && (wizardCsvTextNode?.value || "").trim()) {
        try {
          const tauriInvoke = window?.__TAURI__?.core?.invoke;
          if (tauriInvoke) {
            setWizardStatus("Analyzing CSV format...", "status-loading");
            const preview = await tauriInvoke("preview_csv_import_local", {
              csvText: wizardCsvTextNode.value,
              account: account || ""
            });
            if (preview && preview.preview) {
              const format = preview.detected_format || "unknown";
              const previewSource = preview.source || "llm";
              const confidence = preview.confidence || "high";
              const warnings = Array.isArray(preview.warnings) ? preview.warnings : [];
              const positions = Array.isArray(preview.positions) ? preview.positions : [];
              const stats = preview.stats || {};
              const needsChat = preview.needs_chat_confirmation || confidence === "low";
              const posCount = positions.length;

              // Show source-aware status message
              if (previewSource === "cache" && posCount > 0) {
                const label = format === "transaction_history"
                  ? `Recognized format -- ${posCount} position(s)`
                  : `Recognized format -- ${posCount} position(s)`;
                setWizardStatus(label, "status-success");
                showToast(label, "success");
              } else if (previewSource === "llm" && posCount > 0) {
                const label = format === "transaction_history"
                  ? `Format analyzed by AI -- ${posCount} position(s) from ${stats.total_trades || "?"} trades`
                  : `Format analyzed by AI -- ${posCount} position(s)`;
                setWizardStatus(label, "status-success");
                showToast(label, warnings.length > 0 ? "warning" : "success");
              }

              // Low confidence or unknown format → open chat wizard for confirmation
              if (needsChat) {
                setWizardStatus("Waiting for CSV confirmation...", "status-loading");
                const posTable = positions.map(p => {
                  const t = p.ticker || "?";
                  const q = (p.quantite || 0).toFixed(2);
                  const c = (p.prix_revient || 0).toFixed(2);
                  return `  ${t}: ${q} shares @ ${c} EUR avg cost`;
                }).join("\n");
                const warnText = warnings.length > 0
                  ? "\n\nWarnings:\n" + warnings.map(w => `  - ${w}`).join("\n")
                  : "";
                const headerList = (preview.headers || []).map((h, i) => `  [${i}] ${h}`).join("\n");

                const systemContext = format === "unknown"
                  ? `You are helping the user import a CSV file into their portfolio tracker. The format could not be automatically detected. Here are the headers and first rows:\n\nHeaders:\n${headerList}\n\nSample data:\n${(preview.sample_rows || []).slice(0, 3).map((row, i) => "  Row " + (i + 1) + ": " + row.join(" | ")).join("\n")}\n\nHelp the user understand what format this is. If they confirm, the import will proceed.`
                  : `You are helping the user validate a CSV import. The CSV was detected as a ${format}. AI confidence: ${confidence}. Here is the parsed preview:\n\nPositions (${posCount}):\n${posTable}${warnText}\n\nStats: ${JSON.stringify(stats)}\n\nHeaders: ${JSON.stringify(preview.headers || [])}\nSample rows: ${JSON.stringify((preview.sample_rows || []).slice(0, 3))}\n\nIf the user confirms, the import proceeds. If they point out issues, suggest they re-export or correct the CSV.`;
                const initialMessage = format === "unknown"
                  ? `I could not automatically detect the format of your CSV. Here are the columns I found:\n\n${headerList}\n\nDoes this look like a **position snapshot** (current holdings) or a **transaction history** (buy/sell orders)?`
                  : `I parsed your CSV as a **${format}**.\n\n**${posCount} open position(s)**${stats.total_trades ? ` from **${stats.total_trades} trades**` : ""}.${stats.date_range ? `\nDate range: ${stats.date_range}` : ""}${warnText ? `\n\n${warnText}` : ""}\n\nDoes this look correct? Say **yes** to proceed.`;

                const chatResult = await openChatWizard({
                  title: format === "unknown" ? "Identify CSV Format" : "Confirm CSV Import",
                  systemContext,
                  initialMessage,
                  extractResult: (history) => {
                    const lastUserMsg = [...history].reverse().find(m => m.role === "user");
                    const text = (lastUserMsg?.content || "").toLowerCase().trim();
                    const confirmed = /^(yes|ok|confirm|correct|proceed|looks?\s*good|go\s*ahead|lgtm)/i.test(text);
                    return confirmed ? { confirmed: true } : null;
                  }
                });

                if (!chatResult || !chatResult.confirmed) {
                  setWizardStatus("CSV import cancelled.", "status-idle");
                  return;
                }
                setWizardStatus("CSV confirmed. Starting analysis...", "status-success");
              }
            }
          }
        } catch (previewErr) {
          // Preview failed — proceed with normal parsing (the backend will handle it)
          const msg = String(previewErr?.message || previewErr || "");
          if (msg.includes("csv_upload_empty") || msg.includes("csv_upload_no_positions")) {
            setWizardStatus("CSV appears empty or unreadable. Check the file.", "status-error");
            return;
          }
          // For other errors, log and continue — the analysis pipeline will re-parse
          console.warn("[csv-preview] preview failed, proceeding:", msg);
        }
      }

      const options = buildRunAnalysisOptions({
        source, account,
        csvText: wizardCsvTextNode?.value || "",
        csvExportPath: wizardCsvPathNode?.value || "",
        agentGuidelines: guidelines,
        runMode: analysisMode
      });
      if (account && guidelines) saveGuidelinesForAccount(account, guidelines);

      // Bug 3 fix: Check for unresolved ambiguous cash groups BEFORE starting the run.
      // If the source is Finary-based and there are uncovered cash mappings, show the
      // cash wizard first so the mapping is saved before the pipeline reads it.
      if (source === "finary" || source === "finary_cached") {
        try {
          const tauriInvoke = window?.__TAURI__?.core?.invoke;
          if (tauriInvoke) {
            const preRunPrefs = await tauriInvoke("get_user_preferences_local") || {};
            const finaryMeta = getLatestDashboardPayload()?.snapshot?.latest_finary_snapshot || {};
            const groups = Array.isArray(finaryMeta.ambiguous_cash_groups)
              ? finaryMeta.ambiguous_cash_groups
              : [];
            const savedLinks = preRunPrefs.cash_account_links || {};
            const uncoveredGroups = groups.filter((g) =>
              (g.investment_accounts || []).some((a) => !savedLinks[a.name])
            );
            if (uncoveredGroups.length > 0) {
              // Show cash wizard for each uncovered group before proceeding
              for (const group of uncoveredGroups) {
                const investmentAccounts = group.investment_accounts || [];
                const cashAccounts = group.cash_accounts || [];
                if (investmentAccounts.length === 0 || cashAccounts.length === 0) continue;
                const currentMapping = {};
                for (let i = 0; i < investmentAccounts.length; i++) {
                  if (cashAccounts[i]) {
                    currentMapping[investmentAccounts[i].name] = cashAccounts[i].fiats_sum;
                  }
                }
                const wizResult = await openCashMatchingWizard({
                  investmentAccounts,
                  cashAccounts,
                  currentMapping,
                });
                if (wizResult) {
                  const prefs = await tauriInvoke("get_user_preferences_local") || {};
                  if (!prefs.cash_account_links) prefs.cash_account_links = {};
                  // Strip trailing parenthesized display text (e.g. "Livret A (24,029.00 €)" → "Livret A")
                  // and resolve against known canonical names to prevent saving decorated strings.
                  const knownInvNames = investmentAccounts.map((a) => a.name);
                  const knownCashNames = cashAccounts.map((a) => a.name);
                  for (const [rawKey, rawVal] of Object.entries(wizResult)) {
                    if (rawKey === "confirmed") continue;
                    const cleanKey = resolveWizardName(rawKey, knownInvNames);
                    if (rawVal === "__none__") {
                      prefs.cash_account_links[cleanKey] = "__none__";
                    } else {
                      prefs.cash_account_links[cleanKey] = resolveWizardName(rawVal, knownCashNames);
                    }
                  }
                  await tauriInvoke("save_user_preferences_local", { prefs });
                  showToast("Cash account mapping saved", "success");
                } else {
                  // User cancelled — save "__none__" sentinel so we don't re-prompt next session
                  try {
                    const prefs = await tauriInvoke("get_user_preferences_local") || {};
                    if (!prefs.cash_account_links) prefs.cash_account_links = {};
                    for (const inv of investmentAccounts) {
                      if (!prefs.cash_account_links[inv.name]) {
                        prefs.cash_account_links[inv.name] = "__none__";
                      }
                    }
                    await tauriInvoke("save_user_preferences_local", { prefs });
                  } catch { /* best effort */ }
                  showToast("Cash mapping skipped — cash values may be zero", "warning");
                }
              }
            }
          }
        } catch { /* cash pre-check failed — proceed anyway */ }
      }

      setWizardVisible(false);
      try {
        await runOperations.runAnalysis(options);
      } catch (error) {
        const msg = String(error?.message || error || "");
        // Account mismatch: ask the user to pick the CSV account or cancel
        if (msg.includes("account_mismatch:")) {
          const resolved = await handleAccountMismatch(msg, options);
          if (resolved) return; // re-launched with corrected account
        }
        setWizardStatus(formatBridgeError(error), "status-error");
        displayError(error, "Analysis");
      }
    } catch (error) {
      setWizardStatus(formatBridgeError(error), "status-error");
      displayError(error, "Analysis");
    }
  });

  wizardFinaryConnectBtn?.addEventListener("click", async () => {
    try {
      setWizardStatus("Reconnecting Finary...", "status-loading");
      wizardFinaryConnectBtn.disabled = true;
      await bridge.runFinaryPlaywrightBrowserSession();
      await refreshFinarySessionStatus();
      refreshWizardSourcePolicy(getLatestFinarySessionPayload());
      wizardFinaryConnectBtn.classList.add("hidden");
      wizardFinaryConnectBtn.disabled = false;
      if (isFinarySessionRunnable(getLatestFinarySessionPayload())) {
        setWizardStatus("Finary reconnected. You can start analysis.", "status-success");
      } else {
        setWizardStatus("Reconnect did not succeed. Try again or use CSV.", "status-error");
      }
    } catch (error) {
      wizardFinaryConnectBtn.disabled = false;
      setWizardStatus(formatBridgeError(error), "status-error");
    }
  });

  return { open, close, refreshSourcePolicy, loadGuidelinesForAccount };
}
