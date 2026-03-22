/**
 * App Bootstrap — splash screen, session checks, Codex/Finary connection.
 *
 * Extracted from app.js for single-responsibility. Owns the splash screen
 * lifecycle and startup connection flow.
 */

import { formatBridgeError } from "/shared/run-operations-controller.js";
import { isFinarySessionRunnable } from "/desktop-shell/run-wizard-policy.js";

export function initBootstrap(deps) {
  const {
    bridge,
    refreshDashboard,
    refreshFinarySessionStatus,
    refreshWizardSourcePolicy,
    refreshHealthPill,
    getLatestFinarySessionPayload,
    dismissSplash: externalDismissSplash,
  } = deps;

  function setSplashStatus(text) {
    const node = document.getElementById("splash-status");
    if (node) node.textContent = text;
  }

  function dismissSplash() {
    const splash = document.getElementById("splash-screen");
    if (!splash) return;
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        splash.classList.add("fade-out");
        setTimeout(() => splash.remove(), 600);
      });
    });
    // Trigger background health check to update status pill
    refreshHealthPill();
    if (externalDismissSplash) externalDismissSplash();
  }

  function showStartupError(title, detail) {
    const loaderNode = document.getElementById("splash-loader");
    const statusNode = document.getElementById("splash-status");
    const setupNode = document.getElementById("splash-connect");
    if (loaderNode) loaderNode.classList.add("hidden");
    if (statusNode) {
      statusNode.style.color = "#f08a77";
      statusNode.innerHTML = `<strong>${title}</strong><br><span style="font-size:0.72rem;color:#bfd0df">${detail}</span>`;
    }
    // Show continue button so user isn't stuck
    if (setupNode) {
      setupNode.classList.remove("hidden");
      setupNode.innerHTML = "";
      const btn = document.createElement("button");
      btn.className = "cmd-btn splash-continue";
      btn.textContent = "Continue anyway";
      btn.addEventListener("click", () => dismissSplash());
      setupNode.appendChild(btn);
    }
  }

  async function runStartupSessionCheck() {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    const loaderNode = document.getElementById("splash-loader");

    // 1. Load cached dashboard (fast, local)
    setSplashStatus("Loading cached data\u2026");
    try {
      await refreshDashboard();
    } catch { /* no cached data yet — fine */ }

    // 2. Check connections (OpenAI + Finary)
    const openaiIconNode = document.getElementById("splash-openai-icon");
    const openaiStatusNode = document.getElementById("splash-openai-status");
    const openaiBtn = document.getElementById("splash-openai-btn");
    const finaryIconNode = document.getElementById("splash-finary-icon");
    const finaryStatusNode = document.getElementById("splash-finary-status");
    const finaryBtn = document.getElementById("splash-finary-btn");
    const hintNode = document.getElementById("splash-connect-hint");

    let openaiOk = false;
    let finaryOk = false;

    setSplashStatus("Checking OpenAI\u2026");

    // Check OpenAI
    try {
      const status = await bridge.getCodexSessionStatus();
      const r = status?.result || status;
      if (r?.status === "no_binary") {
        // Auto-install codex, then recheck
        setSplashStatus("Installing Codex CLI...");
        try {
          if (tauriInvoke) await tauriInvoke("ensure_codex_local");
          const status2 = await bridge.getCodexSessionStatus();
          const r2 = status2?.result || status2;
          openaiOk = r2?.logged_in === true;
        } catch { openaiOk = false; }
      } else {
        openaiOk = r?.logged_in === true;
      }
    } catch { openaiOk = false; }

    // Check Finary
    setSplashStatus("Checking Finary\u2026");
    try {
      await refreshFinarySessionStatus();
      finaryOk = isFinarySessionRunnable(getLatestFinarySessionPayload());
    } catch { finaryOk = false; }

    if (finaryOk) {
      setSplashStatus("Loading portfolio...");
      try { await refreshDashboard(); } catch { /* cached data is fine */ }
    }

    refreshWizardSourcePolicy(getLatestFinarySessionPayload());

    // 3. All OK → dismiss
    if (openaiOk && finaryOk) {
      dismissSplash();
      return;
    }

    // 4. Show unified connection card
    if (loaderNode) loaderNode.classList.add("hidden");
    setSplashStatus("Connect your accounts to get started");
    const connectNode = document.getElementById("splash-connect");
    if (connectNode) connectNode.classList.remove("hidden");

    function setRowStatus(iconNode, statusNode, btn, ok, label) {
      if (ok) {
        if (iconNode) { iconNode.textContent = "\u2713"; iconNode.style.color = "#2f8f5d"; }
        if (statusNode) statusNode.textContent = "connected";
        if (btn) btn.classList.add("hidden");
      } else {
        if (iconNode) { iconNode.textContent = "\u25CB"; iconNode.style.color = "#c9873a"; }
        if (statusNode) statusNode.textContent = label;
        if (btn) btn.classList.remove("hidden");
      }
    }

    setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, openaiOk, "not connected");
    setRowStatus(finaryIconNode, finaryStatusNode, finaryBtn, finaryOk, "session expired");

    // Enable "Continue" only when OpenAI is connected (required)
    const continueBtn = document.getElementById("splash-continue");
    function updateContinueBtn() {
      if (continueBtn) continueBtn.disabled = !openaiOk;
    }
    updateContinueBtn();

    // OpenAI connect handler
    openaiBtn?.addEventListener("click", async function handler() {
      this.disabled = true;
      this.textContent = "Signing in...";
      if (hintNode) hintNode.textContent = "A browser window will open for sign-in.";
      try {
        await bridge.codexSessionLogin();
        const status = await bridge.getCodexSessionStatus();
        const r = status?.result || status;
        openaiOk = r?.logged_in === true;
        setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, openaiOk, "not connected");
        if (hintNode) hintNode.textContent = openaiOk ? "" : "Sign-in did not complete.";
        if (!openaiOk) { this.textContent = "Connect"; this.disabled = false; }
        updateContinueBtn();
        if (openaiOk && finaryOk) dismissSplash();
      } catch (error) {
        this.textContent = "Retry";
        this.disabled = false;
        if (hintNode) hintNode.textContent = formatBridgeError(error);
      }
    });

    // Finary connect handler
    finaryBtn?.addEventListener("click", async function handler() {
      this.disabled = true;
      this.textContent = "Connecting...";
      if (hintNode) hintNode.textContent = "";
      try {
        await bridge.runFinaryPlaywrightBrowserSession();
        await refreshFinarySessionStatus();
        finaryOk = isFinarySessionRunnable(getLatestFinarySessionPayload());
        setRowStatus(finaryIconNode, finaryStatusNode, finaryBtn, finaryOk, "session expired");
        refreshWizardSourcePolicy(getLatestFinarySessionPayload());
        if (!finaryOk) {
          this.textContent = "Retry";
          this.disabled = false;
          if (hintNode) hintNode.textContent = "Session still invalid.";
        }
        if (openaiOk && finaryOk) dismissSplash();
      } catch (error) {
        this.textContent = "Retry";
        this.disabled = false;
        if (hintNode) hintNode.textContent = formatBridgeError(error);
      }
    });

    // Continue (when OpenAI connected, Finary optional)
    continueBtn?.addEventListener("click", () => {
      refreshWizardSourcePolicy(getLatestFinarySessionPayload());
      if (!openaiOk) {
        document.getElementById("cmd-connect-openai")?.classList.remove("hidden");
      }
      dismissSplash();
    });

    // CSV bypass — skip Finary entirely for this session
    document.getElementById("splash-csv-bypass")?.addEventListener("click", () => {
      finaryOk = true;
      setRowStatus(finaryIconNode, finaryStatusNode, finaryBtn, true, "");
      if (finaryStatusNode) finaryStatusNode.textContent = "skipped (CSV)";
      refreshWizardSourcePolicy(null);
      if (openaiOk) dismissSplash();
      updateContinueBtn();
    });

    // Mark first run complete
    try {
      if (tauriInvoke) await tauriInvoke("save_user_preferences_local", {
        prefs: { first_run_completed: true }
      });
    } catch { /* save failed, not critical */ }
  }

  return { runStartupSessionCheck, showStartupError, dismissSplash };
}
