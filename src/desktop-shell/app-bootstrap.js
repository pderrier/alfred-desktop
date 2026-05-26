/**
 * App Bootstrap — splash screen, session checks, Codex/Finary connection.
 *
 * Extracted from app.js for single-responsibility. Owns the splash screen
 * lifecycle and startup connection flow.
 */

import { formatBridgeError } from "/shared/run-operations-controller.js";
import { isFinarySessionRunnable } from "/desktop-shell/run-wizard-policy.js";
import { showToast, clearErrorToasts } from "/desktop-shell/shell-layout.js";
import { decideFallback, decideOauthProposal } from "/desktop-shell/codex-fallback-policy.js";
import {
  shouldHaltForMandatoryUpdate,
  buildMandatoryUpdateStrings,
} from "/desktop-shell/update-gate-policy.js";

export { decideFallback, decideOauthProposal };

/**
 * Cross-bootstrap state shared with the report renderer.
 *
 * The v0.2.16 OAuth-availability proposal is decided during the splash
 * bootstrap (we already pay for the codex probe there) but rendered later
 * in the report area. The renderer reads this module-level value at draw
 * time; the action handlers (`Switch` / `Later` / `Don't ask`) mutate it
 * back to `null` so subsequent renders don't redraw the dismissed banner.
 *
 * Shape: one of "propose-native-to-codex", "propose-restore-oauth", or null.
 */
let pendingOauthProposal = null;
export function getPendingOauthProposal() {
  return pendingOauthProposal;
}
export function clearPendingOauthProposal() {
  pendingOauthProposal = null;
}

export function initBootstrap(deps) {
  const {
    bridge,
    refreshDashboard,
    refreshFinarySessionStatus,
    refreshWizardSourcePolicy,
    refreshHealthPill,
    refreshAccountStatus,
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
    // Sync auth state (pills, wizard) so the main app reflects splash choices
    refreshHealthPill();
    refreshAccountStatus();
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

  /**
   * Reveal the inline API-key fields on the splash connect card so the user
   * can type their OpenAI key without leaving the splash.
   *
   * Two callers, two semantics:
   *  - native-oauth -> native fallback: `{ selectBackend: "native" }`
   *    selects the native radio (the backend is being switched to native).
   *  - codex internal-auth fallback: `{ selectBackend: "codex" }` leaves
   *    the codex radio selected — the backend stays on codex; only the
   *    codex CLI's internal auth will be swapped to apikey on submit.
   *
   * The validation/submit flow is wired on the existing splash
   * "Connect/Validate" button (handler below in this file); this helper
   * only un-hides the fields and brings focus to the key input.
   */
  function revealSplashApiKeyInput(opts = {}) {
    const selectBackend = opts.selectBackend || "native";
    const connectNode = document.getElementById("splash-connect");
    const loaderNode = document.getElementById("splash-loader");
    const backendSelector = document.getElementById("splash-backend-selector");
    const nativeFields = document.getElementById("splash-native-fields");
    const apiKey = document.getElementById("splash-api-key");
    const targetRadio = document.querySelector(
      `input[name="splash-backend"][value="${selectBackend}"]`
    );
    const openaiBtn = document.getElementById("splash-openai-btn");
    const openaiStatus = document.getElementById("splash-openai-status");

    if (loaderNode) loaderNode.classList.add("hidden");
    if (connectNode) connectNode.classList.remove("hidden");
    if (backendSelector) backendSelector.classList.remove("hidden");
    if (targetRadio) targetRadio.checked = true;
    // Native-key fields are always shown when the user needs to paste a
    // key, regardless of which backend the radio is currently on.
    if (nativeFields) nativeFields.classList.remove("hidden");
    if (openaiBtn) {
      openaiBtn.textContent = "Validate";
      openaiBtn.classList.remove("hidden");
    }
    if (openaiStatus) openaiStatus.textContent = "API key required";
    if (apiKey) {
      try { apiKey.focus(); } catch { /* focus not critical */ }
    }
  }

  // ── Update state (shared between startup and post-splash) ────────
  let pendingUpdate = null; // { mandatory, latest_version, release_notes, installer_url }

  /**
   * Render the mandatory-update overlay as a TOP-LEVEL element (P0-80).
   *
   * Previously this injected into the splash `splash-status` node, which
   * meant the full splash (loader bar + status text) flashed before the
   * update prompt appeared. We now render a dedicated full-screen overlay
   * (reusing the `.update-modal-overlay` style, z-index 10000, above the
   * splash) so the prompt is the FIRST and ONLY thing the user sees when an
   * update is mandatory — nothing heavy loads behind it. Unlike the optional
   * banner there is no "Not Now" dismiss: the user must upgrade.
   *
   * Copy is FR-tutoiement (see `buildMandatoryUpdateStrings`). Dynamic text
   * (version, release notes) is set via `textContent` rather than
   * interpolated into `innerHTML`, so server-supplied `release_notes` can't
   * inject markup.
   */
  function showMandatoryUpdateUI(update) {
    const strings = buildMandatoryUpdateStrings(update);

    const existing = document.getElementById("update-modal-overlay");
    if (existing) existing.remove();

    const overlay = document.createElement("div");
    overlay.id = "update-modal-overlay";
    overlay.className = "update-modal-overlay update-modal-overlay-mandatory";

    const modal = document.createElement("div");
    modal.className = "update-modal";

    const title = document.createElement("h2");
    title.className = "update-modal-title";
    title.textContent = strings.title;
    modal.appendChild(title);

    const body = document.createElement("p");
    body.className = "update-modal-version";
    body.textContent = strings.body;
    modal.appendChild(body);

    if (strings.releaseNotes) {
      const notes = document.createElement("p");
      notes.className = "update-modal-notes";
      notes.textContent = strings.releaseNotes;
      modal.appendChild(notes);
    }

    const progressWrap = document.createElement("div");
    progressWrap.className = "update-progress hidden";
    progressWrap.innerHTML = `
      <div class="splash-loader" style="display:block;margin:0.5rem 0"><div class="splash-loader-bar" style="width:0%;animation:none"></div></div>
      <span class="update-progress-text"></span>
      <p class="update-error hidden"></p>
    `;
    modal.appendChild(progressWrap);

    const actions = document.createElement("div");
    actions.className = "update-modal-actions";
    const downloadBtn = document.createElement("button");
    downloadBtn.type = "button";
    downloadBtn.className = "cmd-btn update-download-btn";
    downloadBtn.textContent = strings.downloadBtn;
    actions.appendChild(downloadBtn);
    modal.appendChild(actions);

    overlay.appendChild(modal);
    document.body.appendChild(overlay);

    const progressBar = progressWrap.querySelector(".splash-loader-bar");
    const progressText = progressWrap.querySelector(".update-progress-text");
    const errorText = progressWrap.querySelector(".update-error");

    downloadBtn.addEventListener("click", () =>
      runDownloadAndInstall(update, downloadBtn, progressWrap, progressBar, progressText, errorText, strings)
    );
  }

  function showOptionalUpdateBanner(update) {
    const existing = document.getElementById("update-modal-overlay");
    if (existing) existing.remove();

    const overlay = document.createElement("div");
    overlay.id = "update-modal-overlay";
    overlay.className = "update-modal-overlay";
    overlay.innerHTML = `
      <div class="update-modal">
        <h2 class="update-modal-title">Update Available</h2>
        <p class="update-modal-version">Version <strong>v${update.latest_version}</strong> is ready to install.</p>
        ${update.release_notes ? `<p class="update-modal-notes">${update.release_notes}</p>` : ""}
        <div class="update-progress hidden">
          <div class="splash-loader" style="display:block;margin:0.5rem 0"><div class="splash-loader-bar" style="width:0%;animation:none"></div></div>
          <span class="update-progress-text"></span>
          <p class="update-error hidden"></p>
        </div>
        <div class="update-modal-actions">
          <button class="cmd-btn update-modal-install" type="button">Install Update</button>
          <button class="ghost-btn update-modal-dismiss" type="button">Not Now</button>
        </div>
      </div>
    `;
    document.body.appendChild(overlay);

    const progressWrap = overlay.querySelector(".update-progress");

    overlay.querySelector(".update-modal-dismiss").addEventListener("click", async () => {
      overlay.remove();
      try {
        const tauriInvoke = window?.__TAURI__?.core?.invoke;
        if (tauriInvoke) await tauriInvoke("save_user_preferences_local", {
          prefs: { dismissed_update_version: update.latest_version }
        });
      } catch { /* not critical */ }
    });

    overlay.querySelector(".update-modal-install").addEventListener("click", () => {
      const installBtn = overlay.querySelector(".update-modal-install");
      const dismissBtn = overlay.querySelector(".update-modal-dismiss");
      const errorText = progressWrap.querySelector(".update-error");
      dismissBtn.classList.add("hidden");
      runDownloadAndInstall(update, installBtn, progressWrap,
        progressWrap.querySelector(".splash-loader-bar"),
        progressWrap.querySelector(".update-progress-text"),
        errorText
      );
    });
  }

  async function runDownloadAndInstall(update, btn, progressWrap, progressBar, progressText, errorText, strings) {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    if (!tauriInvoke) return;

    // `strings` is supplied by the mandatory overlay (FR-tutoiement); the
    // optional banner calls without it and keeps its own labels.
    const downloadingLabel = "Downloading\u2026";
    const installingLabel = strings?.installingBtn || "Installing\u2026";
    const retryLabel = strings?.retryBtn || "Retry";
    const failedLabel = strings?.downloadFailed || "Download failed";

    btn.disabled = true;
    btn.textContent = downloadingLabel;
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
      btn.textContent = installingLabel;
      await tauriInvoke("install_update_local", { path: result.path });
      // App exits after this — if we're still here, something went wrong
    } catch (err) {
      btn.disabled = false;
      btn.textContent = retryLabel;
      errorText.classList.remove("hidden");
      errorText.textContent = typeof err === "string" ? err : (err?.message || failedLabel);
    } finally {
      if (unlisten) unlisten();
    }
  }

  async function runStartupSessionCheck() {
    const tauriInvoke = window?.__TAURI__?.core?.invoke;
    const loaderNode = document.getElementById("splash-loader");
    const connectNode = document.getElementById("splash-connect");

    // 0. Check for updates FIRST, before any heavy/visible startup work.
    //
    // P0-80: a mandatory update must be surfaced as early as possible. The
    // splash HTML mounts with the loader bar + "Starting up..." visible and
    // the connect card hidden; if we let those render and only swapped to
    // the update prompt after the (heavy) bootstrap, the user would watch
    // the whole splash run just to be told "upgrade or quit". Instead we
    // collapse the splash to a minimal "checking updates" state, await the
    // check, and only reveal the loader + continue when no mandatory update
    // is pending. A mandatory update halts here behind a top-level overlay.
    if (loaderNode) loaderNode.classList.add("hidden");
    if (connectNode) connectNode.classList.add("hidden");
    setSplashStatus("V\u00e9rification des mises \u00e0 jour\u2026");

    if (tauriInvoke) {
      try {
        const update = await tauriInvoke("check_for_update_local");
        if (shouldHaltForMandatoryUpdate(update)) {
          showMandatoryUpdateUI(update);
          return; // Block — user must update (overlay stays, nothing loads)
        }
        if (update?.update_available) {
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

    // No mandatory update \u2014 reveal the loader and resume normal bootstrap.
    if (loaderNode) loaderNode.classList.remove("hidden");

    // 1. Load cached dashboard (fast, local)
    setSplashStatus("Loading cached dashboard\u2026");
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

    // Detect LLM backend + auto-fallback state.
    //
    // `runtime_settings_local` is wrapped in a Tauri envelope
    // `{ok, action, result: {ok, settings: {values, overrides, ...}}}`.
    // Use `bridge.getRuntimeSettings()` which unwraps to `{values, overrides, ...}`
    // -- reading the raw invoke result is a known footgun.
    setSplashStatus("Reading settings\u2026");
    let llmBackend = "codex";
    let settingsValues = {};
    try {
      const settings = await bridge.getRuntimeSettings();
      settingsValues = settings?.values || {};
      llmBackend = settingsValues.llm_backend || "codex";
    } catch { /* default to codex */ }

    // Auto-fallback flag is stored as 0/1 integer; coerce to boolean.
    const autoFallbackActive = Number(settingsValues.llm_backend_auto_fallback) === 1;
    const codexAuthAutoFallbackActive =
      Number(settingsValues.codex_auth_auto_fallback) === 1;

    // Collect probe + native-key facts, then run them through the pure
    // `decideFallback` policy so the bootstrap branch matches the unit-test
    // expectations exactly. Side-effects (settings writes, toasts, DOM
    // mutations) are applied below based on the resolved action.
    let probeResult = null;
    let nativeKeyOk = false;
    let codexCurrentAuth = "none";

    if (llmBackend === "native") {
      setSplashStatus("Validating API key\u2026");
      try {
        if (tauriInvoke) {
          const result = await tauriInvoke("check_openai_api_key_local");
          nativeKeyOk = result?.ok === true;
        }
      } catch { nativeKeyOk = false; }

      // Only probe OAuth when we actually need it (auto-restore path).
      if (nativeKeyOk && autoFallbackActive) {
        try {
          const probe = await bridge.getCodexSessionStatus();
          probeResult = probe?.result || probe;
        } catch { probeResult = null; }
      }
    } else if (llmBackend === "codex") {
      // Codex (legacy) mode \u2014 probe the real CLI quota via `codex exec`. The
      // codex pipeline keeps running on either auth (chatgpt OAuth tokens or
      // an API key), so we also read the codex CLI's current internal auth
      // mode so the policy can decide between swap / restore / noop.
      setSplashStatus("Checking Codex availability\u2026");
      try {
        if (tauriInvoke) {
          // Ensure the binary exists before probing \u2014 otherwise the probe
          // returns "no_binary" and we cannot detect rate-limit state.
          const sanity = await bridge.getCodexSessionStatus();
          if ((sanity?.result || sanity)?.status === "no_binary") {
            setSplashStatus("Installing Codex CLI...");
            try { await tauriInvoke("ensure_codex_local"); } catch { /* surface later */ }
          }
          const probeEnvelope = await tauriInvoke("probe_codex_quota_local");
          const probe = probeEnvelope?.result || probeEnvelope;
          // Normalize to the {logged_in, failure_reason} shape used by
          // decideFallback. `logged_in` is true iff the probe ran the
          // codex exec successfully (status === "ok").
          probeResult = {
            logged_in: probe?.status === "ok",
            status: probe?.status,
            failure_reason: probe?.failure_reason ?? null,
            message: probe?.message,
          };
          const authEnvelope = await tauriInvoke("codex_auth_mode_local");
          codexCurrentAuth = (authEnvelope?.result || authEnvelope)?.mode || "none";
        }
      } catch { probeResult = null; }
    } else {
      // native-oauth \u2014 same probe as before (model/list via app-server).
      setSplashStatus("Checking OpenAI\u2026");
      try {
        const status = await bridge.getCodexSessionStatus();
        const r = status?.result || status;
        if (r?.status === "no_binary") {
          setSplashStatus("Installing Codex CLI...");
          try {
            if (tauriInvoke) await tauriInvoke("ensure_codex_local");
            const status2 = await bridge.getCodexSessionStatus();
            probeResult = status2?.result || status2;
          } catch { probeResult = null; }
        } else {
          probeResult = r;
        }
      } catch { probeResult = null; }
    }

    const decision = decideFallback({
      llmBackend,
      autoFallbackActive,
      probeResult,
      savedApiKey: settingsValues.openai_api_key || "",
      nativeKeyOk,
      codexAuthAutoFallbackActive,
      codexCurrentAuth,
    });
    openaiOk = decision.openaiOk;

    if (decision.action === "auto-restore-oauth" && tauriInvoke) {
      try {
        await tauriInvoke("runtime_settings_update_local", {
          settings: { llm_backend: "native-oauth", llm_backend_auto_fallback: 0 }
        });
        llmBackend = "native-oauth";
        showToast("OpenAI OAuth quota restored \u2014 switched back to your ChatGPT subscription.");
      } catch { /* not critical -- stay on native */ }
    } else if (decision.action === "clear-fallback-flag" && tauriInvoke) {
      try {
        await tauriInvoke("runtime_settings_update_local", {
          settings: { llm_backend_auto_fallback: 0 }
        });
      } catch { /* not critical */ }
    } else if (decision.action === "switch-to-native" && tauriInvoke) {
      try {
        await tauriInvoke("runtime_settings_update_local", {
          settings: { llm_backend: "native", llm_backend_auto_fallback: 1 }
        });
        llmBackend = "native";
        showToast("OpenAI OAuth quota exhausted \u2014 falling back to your saved API key.");
        const result = await tauriInvoke("check_openai_api_key_local");
        openaiOk = result?.ok === true;
      } catch {
        openaiOk = false;
      }
    } else if (decision.action === "reveal-key-input") {
      // Persist the mode switch BEFORE the connect-card render runs (further
      // below) -- otherwise that render would re-select the "native-oauth"
      // radio from the stale local state and clobber `revealSplashApiKeyInput`.
      // Mirrors the "switch-to-native" path; the only difference is that we
      // need the user to type a key before the validation step.
      if (tauriInvoke) {
        try {
          await tauriInvoke("runtime_settings_update_local", {
            settings: { llm_backend: "native", llm_backend_auto_fallback: 1 }
          });
        } catch { /* not critical */ }
      }
      llmBackend = "native";
      showToast(
        "OpenAI OAuth quota exhausted \u2014 enter your OpenAI API key to continue.",
        "error"
      );
      revealSplashApiKeyInput();
      // Leave openaiOk=false so the splash renders the connect card and the
      // existing API-key validation handler takes over.
    } else if (decision.action === "codex-swap-to-apikey" && tauriInvoke) {
      // Codex mode is self-healing: keep the codex pipeline, swap the codex
      // CLI's stored auth from chatgpt OAuth to apikey using the saved key.
      setSplashStatus("OAuth quota exhausted \u2014 switching codex to API key\u2026");
      try {
        await tauriInvoke("swap_codex_to_apikey_local", {
          apiKey: settingsValues.openai_api_key || ""
        });
        await tauriInvoke("runtime_settings_update_local", {
          settings: { codex_auth_auto_fallback: 1 }
        });
        showToast(
          "OAuth quota exhausted \u2014 codex now using your OpenAI API key."
        );
        codexCurrentAuth = "apikey";
        openaiOk = true;
      } catch {
        // Swap failed \u2014 surface as not-connected so the splash forces a
        // visible recovery step instead of pretending we succeeded.
        openaiOk = false;
      }
    } else if (decision.action === "codex-reveal-key-input") {
      // No saved API key \u2014 keep llm_backend=codex (the pipeline doesn't
      // change). The validate handler below detects "codex + no auth"
      // and runs swap_codex_to_apikey_local on submit instead of switching
      // backends.
      showToast(
        "OAuth quota exhausted \u2014 paste your OpenAI API key to keep using Codex.",
        "error"
      );
      revealSplashApiKeyInput({ selectBackend: "codex" });
      // Leave openaiOk=false so the connect card stays up.
    } else if (decision.action === "codex-restore-oauth" && tauriInvoke) {
      // Throttle restore attempts: if a previous restore failed recently the
      // setting `codex_auth_oauth_retry_after_ms` holds a future epoch-ms.
      // We skip the swap until that timestamp passes -- avoids
      // backup -> restore -> handshake -> rollback thrashing on every launch
      // when OAuth quota is still exhausted.
      const retryAfter = Number(settingsValues.codex_auth_oauth_retry_after_ms) || 0;
      const now = Date.now();
      if (retryAfter > now) {
        // Too soon to retry. Stay on apikey, codex pipeline still works.
        openaiOk = true;
      } else {
        setSplashStatus("OAuth quota restored \u2014 switching codex back\u2026");
        try {
          const envelope = await tauriInvoke("swap_codex_to_oauth_local");
          const restored = (envelope?.result || envelope)?.restored === true;
          if (restored) {
            await tauriInvoke("runtime_settings_update_local", {
              settings: {
                codex_auth_auto_fallback: 0,
                codex_auth_oauth_retry_after_ms: 0,
              }
            });
            showToast(
              "OAuth quota restored \u2014 codex back on your ChatGPT subscription."
            );
            codexCurrentAuth = "chatgpt";
          } else {
            // Restore failed (quota likely still exhausted). Throttle next
            // attempt by 6h so we don't thrash on every launch.
            await tauriInvoke("runtime_settings_update_local", {
              settings: { codex_auth_oauth_retry_after_ms: now + 6 * 60 * 60 * 1000 }
            });
          }
          // Either way we're "connected" \u2014 the codex pipeline still works
          // on whichever auth we ended up with.
          openaiOk = true;
        } catch {
          // Throw path (e.g. Tauri invoke failed). Throttle as well.
          try {
            await tauriInvoke("runtime_settings_update_local", {
              settings: { codex_auth_oauth_retry_after_ms: now + 6 * 60 * 60 * 1000 }
            });
          } catch { /* not critical */ }
          openaiOk = true;
        }
      }
    }

    // v0.2.16: OAuth-availability proposal banner
    //
    // After the v0.2.14 auto-restore decision has been applied, check whether
    // we should *propose* (non-blocking) a switch to OAuth in the report UI.
    // This covers two gaps the auto-restore can't:
    //   - User on `native` (paid API key): never auto-touched. Probe codex
    //     here and offer a switch if OAuth is live.
    //   - User on `codex` with manually-swapped apikey + auto-fallback flag=0:
    //     auto-restore won't fire; if a backup exists we can offer to restore.
    //
    // Skip the probe entirely when the user has dismissed permanently or
    // within the snooze window \u2014 burning an OAuth quota token for a banner
    // we won't render is wasteful.
    try {
      const oauthDismissedUntil =
        Number(settingsValues.oauth_proposal_dismissed_until_ms) || 0;
      const oauthPermanentlyDismissed =
        Number(settingsValues.oauth_proposal_permanently_dismissed) === 1;
      const nowMs = Date.now();
      const dismissalGate =
        oauthPermanentlyDismissed || oauthDismissedUntil > nowMs;

      // Decide whether we need a fresh probe for the proposal decision.
      // The auto-restore path already runs the codex probe for `llmBackend
      // === "codex"`; we can reuse it. For `llmBackend === "native"` it
      // was NOT run in the existing flow, so we run it once here unless
      // gated by the dismissal state.
      let proposalProbeResult = probeResult;
      let proposalCodexAuth = codexCurrentAuth;
      let proposalHasBackup = false;
      if (!dismissalGate && tauriInvoke) {
        if (llmBackend === "native") {
          // Probe codex (independent of the app's llm_backend) so we can
          // tell whether OAuth is live on the codex CLI side.
          try {
            const sanity = await bridge.getCodexSessionStatus();
            if ((sanity?.result || sanity)?.status === "no_binary") {
              // No codex binary -> no OAuth path to propose. Leave probe null.
              proposalProbeResult = null;
            } else {
              const probeEnvelope = await tauriInvoke("probe_codex_quota_local");
              const probe = probeEnvelope?.result || probeEnvelope;
              proposalProbeResult = {
                logged_in: probe?.status === "ok",
                status: probe?.status,
                failure_reason: probe?.failure_reason ?? null,
              };
              const authEnvelope = await tauriInvoke("codex_auth_mode_local");
              proposalCodexAuth =
                (authEnvelope?.result || authEnvelope)?.mode || "none";
            }
          } catch { proposalProbeResult = null; }
        }
        // For the codex-restore-oauth proposal we need to know whether a
        // backup file is on disk. Cheap call (Path::exists).
        if (llmBackend === "codex" && proposalCodexAuth === "apikey") {
          try {
            const envelope = await tauriInvoke("codex_has_oauth_backup_local");
            proposalHasBackup =
              (envelope?.result || envelope)?.has_backup === true;
          } catch { proposalHasBackup = false; }
        }
      }

      const proposal = decideOauthProposal({
        llmBackend,
        codexCurrentAuth: proposalCodexAuth,
        hasOauthBackup: proposalHasBackup,
        oauthProbeOk: proposalProbeResult?.logged_in === true,
        dismissedUntilMs: oauthDismissedUntil,
        permanentlyDismissed: oauthPermanentlyDismissed,
        codexAuthAutoFallbackActive,
        nowMs,
      });
      pendingOauthProposal = proposal.kind === "none" ? null : proposal.kind;
    } catch { pendingOauthProposal = null; }

    // Check Finary
    setSplashStatus("Refreshing Finary session\u2026");
    try {
      await refreshFinarySessionStatus();
      finaryOk = isFinarySessionRunnable(getLatestFinarySessionPayload());
    } catch { finaryOk = false; }

    if (finaryOk) {
      // Load cached portfolio, or fetch from Finary API if no cache exists
      setSplashStatus("Syncing portfolio from Finary\u2026");
      try {
        if (tauriInvoke) await tauriInvoke("finary_sync_snapshot_local");
        setSplashStatus("Updating dashboard\u2026");
        await refreshDashboard();
      } catch { /* will show on welcome page */ }
    }

    refreshWizardSourcePolicy(getLatestFinarySessionPayload());

    // 2b. Health check — includes auth verification when OpenAI is connected
    setSplashStatus("Running health check\u2026");
    await refreshHealthPill(openaiOk);

    // 3. All OK → dismiss
    if (openaiOk && finaryOk) {
      dismissSplash();
      return;
    }

    // 4. Show unified connection card (connectNode declared at fn top — P0-80)
    if (loaderNode) loaderNode.classList.add("hidden");
    setSplashStatus("Connect your accounts to get started");
    if (connectNode) connectNode.classList.remove("hidden");

    // Show backend selector when OpenAI is not connected
    const backendSelector = document.getElementById("splash-backend-selector");
    const splashNativeFields = document.getElementById("splash-native-fields");
    const splashApiKey = document.getElementById("splash-api-key");
    const splashApiBase = document.getElementById("splash-api-base");
    const splashApikeyHint = document.getElementById("splash-apikey-hint");
    const backendRadios = document.querySelectorAll('input[name="splash-backend"]');

    if (!openaiOk && backendSelector) {
      backendSelector.classList.remove("hidden");
      // Pre-select current backend
      const currentRadio = document.querySelector(`input[name="splash-backend"][value="${llmBackend}"]`);
      if (currentRadio) currentRadio.checked = true;
      if (llmBackend === "native") splashNativeFields?.classList.remove("hidden");

      // Toggle native fields on radio change
      for (const radio of backendRadios) {
        radio.addEventListener("change", () => {
          const selected = document.querySelector('input[name="splash-backend"]:checked')?.value;
          const isNative = selected === "native";
          splashNativeFields?.classList.toggle("hidden", !isNative);
          // Update OpenAI row label
          if (openaiBtn) openaiBtn.textContent = isNative ? "Validate" : "Connect";
          if (openaiStatusNode) openaiStatusNode.textContent = isNative ? "API key required" : "not connected";
          if (splashApikeyHint) splashApikeyHint.textContent = "";
        });
      }
    }

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

    const nativeLabel = llmBackend === "native" ? "API key required" : "not connected";
    setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, openaiOk, nativeLabel);
    if (llmBackend === "native" && openaiBtn) openaiBtn.textContent = "Validate";
    setRowStatus(finaryIconNode, finaryStatusNode, finaryBtn, finaryOk, "session expired");

    // Enable "Continue" only when OpenAI is connected (required)
    const continueBtn = document.getElementById("splash-continue");
    function updateContinueBtn() {
      if (continueBtn) continueBtn.disabled = !openaiOk;
    }
    updateContinueBtn();

    // OpenAI connect handler — adapts to selected backend
    openaiBtn?.addEventListener("click", async function handler() {
      const selectedBackend = document.querySelector('input[name="splash-backend"]:checked')?.value || llmBackend;

      // Codex internal-auth-fallback path: the splash revealed the API-key
      // input under the codex radio (decision="codex-reveal-key-input").
      // Detect this state by: backend=codex AND the native-fields card is
      // visible. On submit, swap the codex CLI's internal auth chatgpt ->
      // apikey instead of switching backends.
      const codexNeedsApiKey =
        selectedBackend === "codex"
        && splashNativeFields
        && !splashNativeFields.classList.contains("hidden");

      if (codexNeedsApiKey) {
        const key = splashApiKey?.value?.trim();
        if (!key) {
          if (splashApikeyHint) splashApikeyHint.textContent = "Enter your OpenAI API key above.";
          return;
        }
        this.disabled = true;
        this.textContent = "Switching codex…";
        if (splashApikeyHint) splashApikeyHint.textContent = "";
        try {
          if (tauriInvoke) {
            // Persist the key + mark the auth-fallback active BEFORE the
            // swap so a crash mid-swap can be picked up next launch.
            // llm_backend stays codex; llm_backend_auto_fallback is a
            // separate (inter-mode) flag and must not be touched here.
            await tauriInvoke("runtime_settings_update_local", {
              settings: {
                openai_api_key: key,
                codex_auth_auto_fallback: 1,
              }
            });
            await tauriInvoke("swap_codex_to_apikey_local", { apiKey: key });
          }
          openaiOk = true;
          clearErrorToasts();
          showToast("Codex now using your OpenAI API key.");
          setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, true, "connected");
          backendSelector?.classList.add("hidden");
          if (splashApikeyHint) {
            splashApikeyHint.textContent = "Codex internal auth swapped to API key.";
            splashApikeyHint.style.color = "#2f8f5d";
          }
          updateContinueBtn();
          if (openaiOk && finaryOk) dismissSplash();
        } catch (error) {
          this.textContent = "Retry";
          this.disabled = false;
          if (splashApikeyHint) splashApikeyHint.textContent = typeof error === "string" ? error : (error?.message || "Codex auth swap failed");
        }
        return;
      }

      if (selectedBackend === "native") {
        // Native backend — validate API key from splash input
        const key = splashApiKey?.value?.trim();
        if (!key) {
          if (splashApikeyHint) splashApikeyHint.textContent = "Enter your OpenAI API key above.";
          return;
        }
        this.disabled = true;
        this.textContent = "Validating\u2026";
        if (splashApikeyHint) splashApikeyHint.textContent = "";

        try {
          // Save backend + key to settings, then validate.
          // Manual user action — clear BOTH auto-fallback flags so we don't
          // try to auto-restore on the next launch.
          const apiBase = splashApiBase?.value?.trim() || "";
          if (tauriInvoke) {
            await tauriInvoke("runtime_settings_update_local", {
              settings: {
                llm_backend: "native",
                openai_api_key: key,
                llm_backend_auto_fallback: 0,
                codex_auth_auto_fallback: 0,
                ...(apiBase ? { openai_api_base: apiBase } : {}),
              }
            });
          }
          const result = tauriInvoke ? await tauriInvoke("check_openai_api_key_local") : null;
          openaiOk = result?.ok === true;
          setRowStatus(openaiIconNode, openaiStatusNode, openaiBtn, openaiOk, "API key invalid");
          if (openaiOk) {
            // Clear the persistent "OAuth quota exhausted" toast (if any) — the
            // user has now resolved the condition by providing a working key.
            clearErrorToasts();
            if (splashApikeyHint) splashApikeyHint.textContent = `Connected (${result.models_available} models)`;
            splashApikeyHint.style.color = "#2f8f5d";
            backendSelector?.classList.add("hidden");
          } else {
            if (splashApikeyHint) splashApikeyHint.textContent = "API key validation failed. Check your key.";
            this.textContent = "Validate";
            this.disabled = false;
          }
          updateContinueBtn();
          if (openaiOk && finaryOk) dismissSplash();
        } catch (error) {
          this.textContent = "Retry";
          this.disabled = false;
          if (splashApikeyHint) splashApikeyHint.textContent = typeof error === "string" ? error : (error?.message || "Validation failed");
        }
        return;
      }

      // Codex or native-oauth backend — save backend choice, then do OAuth login.
      // Manual user action — clear BOTH auto-fallback flags.
      if (tauriInvoke) {
        try {
          await tauriInvoke("runtime_settings_update_local", {
            settings: {
              llm_backend: selectedBackend,
              llm_backend_auto_fallback: 0,
              codex_auth_auto_fallback: 0,
            }
          });
        } catch {}
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
        if (openaiOk) {
          // Clear the persistent "OAuth quota exhausted" toast (if any) — the
          // user has now resolved the condition by signing back in.
          clearErrorToasts();
          backendSelector?.classList.add("hidden");
        }
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
