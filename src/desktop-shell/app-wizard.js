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
      const guidelines = agentGuidelinesInputNode?.value || "";
      const analysisMode = wizardAnalysisModeNode?.value || "full_run";
      const options = buildRunAnalysisOptions({
        source, account,
        csvText: wizardCsvTextNode?.value || "",
        csvExportPath: wizardCsvPathNode?.value || "",
        agentGuidelines: guidelines,
        runMode: analysisMode
      });
      if (account && guidelines) saveGuidelinesForAccount(account, guidelines);
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
