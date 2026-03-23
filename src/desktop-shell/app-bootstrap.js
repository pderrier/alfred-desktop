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
        setTimeout(() => {
          splash.remove();
          // Show optional update banner after splash is gone
          if (pendingUpdate) showOptionalUpdateBanner(pendingUpdate);
        }, 600);
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

  // ── Update state (shared between startup and post-splash) ────────
  let pendingUpdate = null; // { mandatory, latest_version, release_notes, installer_url }

  function showMandatoryUpdateUI(update) {
    const splash = document.getElementById("splash-screen");
    if (!splash) return;
    const loaderNode = document.getElementById("splash-loader");
    const connectNode = document.getElementById("splash-connect");
    if (loaderNode) loaderNode.classList.add("hidden");
    if (connectNode) connectNode.classList.add("hidden");

    setSplashStatus("");
    const statusNode = document.getElementById("splash-status");
    if (!statusNode) return;

    statusNode.innerHTML = "";
    const wrap = document.createElement("div");
    wrap.className = "update-mandatory";
    wrap.innerHTML = `
      <h3 style="margin:0 0 0.4rem;color:#fff">Update Required</h3>
      <p style="margin:0 0 0.6rem;color:rgba(255,255,255,0.7);font-size:0.8rem">
        Version ${update.latest_version} is available (you have ${update.current_version}).
      </p>
      ${update.release_notes ? `<p style="margin:0 0 0.8rem;color:rgba(255,255,255,0.5);font-size:0.72rem">${update.release_notes}</p>` : ""}
      <div class="update-progress hidden" style="margin:0 0 0.6rem">
        <div class="splash-loader" style="display:block"><div class="splash-loader-bar" style="width:0%;animation:none"></div></div>
        <span class="update-progress-text" style="font-size:0.7rem;color:rgba(255,255,255,0.5)"></span>
      </div>
      <button class="cmd-btn update-download-btn">Download &amp; Install</button>
      <p class="update-error hidden" style="margin:0.5rem 0 0;color:#f08a77;font-size:0.72rem"></p>
    `;
    statusNode.appendChild(wrap);

    const downloadBtn = wrap.querySelector(".update-download-btn");
    const progressWrap = wrap.querySelector(".update-progress");
    const progressBar = wrap.querySelector(".splash-loader-bar");
    const progressText = wrap.querySelector(".update-progress-text");
    const errorText = wrap.querySelector(".update-error");

    downloadBtn.addEventListener("click", () =>
      runDownloadAndInstall(update, downloadBtn, progressWrap, progressBar, progressText, errorText)
    );
  }

  function showOptionalUpdateBanner(update) {
    const layout = document.querySelector(".app-layout") || document.body;
    const existing = document.getElementById("update-banner");
    if (existing) existing.remove();

    const banner = document.createElement("div");
    banner.id = "update-banner";
    banner.className = "update-banner";
    banner.innerHTML = `
      <span>Update available: <strong>v${update.latest_version}</strong>${update.release_notes ? " — " + update.release_notes : ""}</span>
      <button class="cmd-btn-sm update-banner-install">Update</button>
      <button class="cmd-btn-sm update-banner-dismiss" style="background:transparent;border:1px solid rgba(255,255,255,0.2)">Dismiss</button>
    `;
    layout.prepend(banner);

    const progressWrap = document.createElement("div");
    progressWrap.className = "update-progress hidden";
    progressWrap.innerHTML = `
      <div class="splash-loader" style="display:block;margin:0.3rem 0"><div class="splash-loader-bar" style="width:0%;animation:none"></div></div>
      <span class="update-progress-text" style="font-size:0.7rem;color:rgba(255,255,255,0.5)"></span>
      <p class="update-error hidden" style="margin:0.3rem 0 0;color:#f08a77;font-size:0.72rem"></p>
    `;
    banner.appendChild(progressWrap);

    banner.querySelector(".update-banner-dismiss").addEventListener("click", async () => {
      banner.remove();
      // Remember dismissed version
      try {
        const tauriInvoke = window?.__TAURI__?.core?.invoke;
        if (tauriInvoke) await tauriInvoke("save_user_preferences_local", {
          prefs: { dismissed_update_version: update.latest_version }
        });
      } catch { /* not critical */ }
    });

    banner.querySelector(".update-banner-install").addEventListener("click", () => {
      const installBtn = banner.querySelector(".update-banner-install");
      const dismissBtn = banner.querySelector(".update-banner-dismiss");
      const errorText = progressWrap.querySelector(".update-error");
      dismissBtn.classList.add("hidden");
      runDownloadAndInstall(update, installBtn, progressWrap,
        progressWrap.querySelector(".splash-loader-bar"),
        progressWrap.querySelector(".update-progress-text"),
        errorText
      );
    });
  }

  async function runDownloadAndInstall(update, btn, progressWrap, progressBar, progressText, errorText) {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) return;

    btn.disabled = true;
    btn.textContent = "Downloading\u2026";
    progressWrap.classList.remove("hidden");
    errorText.classList.add("hidden");

    // Listen for progress events
    let unlisten = null;
    try {
      const { listen } = window.__TAURI__.event;
      unlisten = await listen("update-download-progress", (ev) => {
        const { downloaded, total } = ev.payload;
        if (total > 0) {
          const pct = Math.round((downloaded / total) * 100);
          progressBar.style.width = pct + "%";
          progressText.textContent = `${(downloaded / 1048576).toFixed(1)} / ${(total / 1048576).toFixed(1)} MB`;
        }
      });
    } catch { /* event API unavailable */ }

    try {
      const result = await tauriInvoke("download_update_local", {
        url: update.installer_url, sha256: null
      });
      btn.textContent = "Installing\u2026";
      await tauriInvoke("install_update_local", { path: result.path });
      // App exits after this — if we're still here, something went wrong
    } catch (err) {
      btn.disabled = false;
      btn.textContent = "Retry";
      errorText.classList.remove("hidden");
      errorText.textContent = typeof err === "string" ? err : (err?.message || "Download failed");
    } finally {
      if (unlisten) unlisten();
    }
  }

  async function runStartupSessionCheck() {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    const loaderNode = document.getElementById("splash-loader");

    // 0. Check for updates (non-blocking on failure)
    if (tauriInvoke) {
      try {
        setSplashStatus("Checking for updates\u2026");
        const update = await tauriInvoke("check_for_update_local");
        if (update?.update_available) {
          if (update.mandatory) {
            showMandatoryUpdateUI(update);
            return; // Block — user must update
          }
          // Optional — check if user already dismissed this version
          try {
            const prefs = await tauriInvoke("get_user_preferences_local");
            if (prefs?.dismissed_update_version !== update.latest_version) {
              pendingUpdate = update;
            }
          } catch { pendingUpdate = update; }
        }
      } catch { /* update check failed — continue normally */ }
    }

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

    // Detect LLM backend
    let llmBackend = "codex";
    try {
      if (tauriInvoke) {
        const settings = await tauriInvoke("runtime_settings_local");
        llmBackend = settings?.values?.llm_backend || "codex";
      }
    } catch { /* default to codex */ }

    if (llmBackend === "native") {
      // Native backend — validate API key
      setSplashStatus("Validating API key\u2026");
      try {
        if (tauriInvoke) {
          const result = await tauriInvoke("check_openai_api_key_local");
          openaiOk = result?.ok === true;
        }
      } catch { openaiOk = false; }
    } else {
      // Codex backend — check session
      setSplashStatus("Checking OpenAI\u2026");
      try {
        const status = await bridge.getCodexSessionStatus();
        const r = status?.result || status;
        if (r?.status === "no_binary") {
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
    }

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

    const nativeLabel = llmBackend === "native" ? "API key missing" : "not connected";
    setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, openaiOk, nativeLabel);
    if (llmBackend === "native" && openaiBtn) openaiBtn.textContent = "Configure";
    setRowStatus(finaryIconNode, finaryStatusNode, finaryBtn, finaryOk, "session expired");

    // Enable "Continue" only when OpenAI is connected (required)
    const continueBtn = document.getElementById("splash-continue");
    function updateContinueBtn() {
      if (continueBtn) continueBtn.disabled = !openaiOk;
    }
    updateContinueBtn();

    // OpenAI connect handler
    openaiBtn?.addEventListener("click", async function handler() {
      if (llmBackend === "native") {
        // Native backend — open settings to configure API key
        if (hintNode) hintNode.textContent = "Set your API key in Settings (gear icon) then restart.";
        return;
      }
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
