/**
 * LLM-backend auto-fallback decision policy.
 *
 * Pure function used by the splash startup check in `app-bootstrap.js` AND
 * by the unit-test suite. Keeping the decision tree in one place prevents
 * the test replica from drifting from production behavior.
 *
 * Two distinct fallback layers are decided here:
 *
 *  - **Inter-mode** (v0.2.13) — when `llm_backend === "native-oauth"` runs
 *    out of OAuth quota, swap to `llm_backend === "native"` (API key). The
 *    auto-restore path flips it back when quota returns.
 *
 *  - **Intra-mode codex** (new) — when `llm_backend === "codex"` runs out
 *    of OAuth quota, keep the codex pipeline unchanged but swap the codex
 *    CLI's stored auth from `chatgpt` (OAuth tokens) to `apikey` (OpenAI
 *    API key). The pipeline (prompts, tools, MCP) is untouched. Auto-
 *    restore puts the OAuth credentials back when quota returns.
 *
 * Decision outputs:
 *   action: one of
 *     - "noop"                 — nothing to switch; openaiOk reflects probe/key state
 *     - "switch-to-native"     — native-oauth rate-limited, saved API key exists,
 *                                persist {llm_backend=native, auto_fallback=1}
 *     - "reveal-key-input"     — native-oauth rate-limited, no saved key; ask user
 *                                for a key inline on the splash. Caller persists
 *                                {llm_backend=native, auto_fallback=1} BEFORE
 *                                falling through to the connect-card render so the
 *                                radio + button labels match the new mode.
 *     - "clear-fallback-flag"  — probe OK on native-oauth while flag was set; just
 *                                clear the flag (no mode change)
 *     - "auto-restore-oauth"   — on native with the flag set, OAuth probe came back
 *                                OK; switch back to native-oauth and clear flag
 *     - "codex-swap-to-apikey" — codex mode + rate-limited probe + apiKey present:
 *                                swap the codex CLI's internal auth chatgpt -> apikey
 *                                and set codex_auth_auto_fallback=1.
 *     - "codex-reveal-key-input" — codex mode + rate-limited probe + no apiKey:
 *                                reveal the splash API key input. Caller does NOT
 *                                pre-persist anything for codex mode (the mode
 *                                stays "codex"); the validate handler will run
 *                                swap_codex_to_apikey_local on submit.
 *     - "codex-restore-oauth"  — codex mode + flag set + probe OK + currentAuth=apikey:
 *                                restore OAuth auth from the on-disk backup.
 *   openaiOk: boolean — what the splash should treat as the "connected" state
 *                       after the decision is applied
 */

/**
 * @typedef {{
 *   logged_in?: boolean,
 *   status?: string,
 *   failure_reason?: "rate_limited" | "auth" | "network" | null,
 * }} ProbeResult
 */

/**
 * @param {{
 *   llmBackend: "codex"|"native"|"native-oauth",
 *   autoFallbackActive: boolean,
 *   probeResult: ProbeResult | null,
 *   savedApiKey: string,
 *   nativeKeyOk: boolean,
 *   codexAuthAutoFallbackActive?: boolean,
 *   codexCurrentAuth?: "chatgpt"|"apikey"|"none",
 * }} input
 * @returns {{
 *   action: "noop"|"switch-to-native"|"reveal-key-input"|"clear-fallback-flag"
 *         |"auto-restore-oauth"|"codex-swap-to-apikey"|"codex-reveal-key-input"
 *         |"codex-restore-oauth",
 *   openaiOk: boolean,
 * }}
 */
export function decideFallback(input) {
  const {
    llmBackend,
    autoFallbackActive,
    probeResult,
    savedApiKey,
    nativeKeyOk,
    codexAuthAutoFallbackActive,
    codexCurrentAuth,
  } = input;

  if (llmBackend === "native") {
    if (nativeKeyOk && autoFallbackActive && probeResult?.logged_in === true) {
      return { action: "auto-restore-oauth", openaiOk: true };
    }
    return { action: "noop", openaiOk: nativeKeyOk === true };
  }

  const probeOk = probeResult?.logged_in === true;
  const rateLimited = probeResult?.failure_reason === "rate_limited";

  if (llmBackend === "codex") {
    // Codex mode is treated as "logged in" via the codex CLI's stored auth
    // (either chatgpt OAuth tokens or an OPENAI_API_KEY). Quota state comes
    // from the probe; auth identity comes from `codexCurrentAuth`.

    // Auto-restore: flag set, quota now OK, current auth is the apikey we
    // installed during the previous fallback -> swap back to OAuth.
    if (
      codexAuthAutoFallbackActive === true
      && probeOk
      && codexCurrentAuth === "apikey"
    ) {
      return { action: "codex-restore-oauth", openaiOk: true };
    }

    if (rateLimited) {
      const apiKey = (savedApiKey || "").trim();
      if (!apiKey) {
        return { action: "codex-reveal-key-input", openaiOk: false };
      }
      return { action: "codex-swap-to-apikey", openaiOk: true };
    }

    // Probe OK (or any non-rate-limit failure) — leave codex alone.
    return { action: "noop", openaiOk: probeOk };
  }

  // native-oauth
  if (probeOk && llmBackend === "native-oauth" && autoFallbackActive) {
    // Clear flag only — doesn't mutate openaiOk
    return { action: "clear-fallback-flag", openaiOk: true };
  }

  if (probeOk) {
    return { action: "noop", openaiOk: true };
  }

  if (llmBackend === "native-oauth" && rateLimited) {
    const apiKey = (savedApiKey || "").trim();
    if (!apiKey) {
      return { action: "reveal-key-input", openaiOk: false };
    }
    return { action: "switch-to-native", openaiOk: nativeKeyOk === true };
  }

  return { action: "noop", openaiOk: false };
}

/**
 * Decide whether to surface the v0.2.16 "OAuth ChatGPT Plus is available"
 * banner in the report area.
 *
 * This is a *non-blocking* proposal — it never auto-switches the user.
 * It exists for two cases the v0.2.14 auto-restore path does NOT cover:
 *
 *   1. The user is on `llm_backend === "native"` (paid API key path).
 *      The v0.2.14 path only auto-restores when `llm_backend_auto_fallback`
 *      is set (i.e. the app put them on native). If they chose native
 *      explicitly, they never see a nudge — even when OAuth quota is fine.
 *
 *   2. The user is on `llm_backend === "codex"` with `auth.json` already
 *      in apikey mode but `codex_auth_auto_fallback === 0` (manual swap,
 *      or carried over from an older app version). The v0.2.14 restore
 *      only fires when the flag is set; without it, we never propose a
 *      switch back, even when an OAuth backup exists on disk.
 *
 * Proposal kinds:
 *   - `none`                       — do not show the banner
 *   - `propose-native-to-codex`    — user is on native; switching to codex
 *                                    will pick up the live ChatGPT Plus auth
 *   - `propose-restore-oauth`      — user is on codex+apikey but an OAuth
 *                                    backup exists; we can restore it
 *
 * Inputs:
 *   - `llmBackend`              current backend setting
 *   - `codexCurrentAuth`        codex CLI's auth.json mode ("chatgpt"|"apikey"|"none")
 *   - `hasOauthBackup`          true iff `~/.codex/auth.json.oauth.bak` exists
 *   - `oauthProbeOk`            true iff a probe (codex_session_status or
 *                                 codex_exec) returned with no failure_reason
 *   - `dismissedUntilMs`        epoch-ms threshold from settings; while
 *                                 `dismissedUntilMs > nowMs`, skip
 *   - `permanentlyDismissed`    true iff user clicked "Don't ask"
 *   - `codexAuthAutoFallbackActive`
 *                               true iff the v0.2.14 flag is set; when set,
 *                                 the auto-restore path owns recovery and
 *                                 we must NOT show a competing banner
 *   - `nowMs`                   current epoch-ms (injected for testability)
 *
 * @param {{
 *   llmBackend: "codex"|"native"|"native-oauth",
 *   codexCurrentAuth?: "chatgpt"|"apikey"|"none",
 *   hasOauthBackup?: boolean,
 *   oauthProbeOk?: boolean,
 *   dismissedUntilMs?: number,
 *   permanentlyDismissed?: boolean,
 *   codexAuthAutoFallbackActive?: boolean,
 *   nowMs?: number,
 * }} input
 * @returns {{ kind: "none"|"propose-native-to-codex"|"propose-restore-oauth" }}
 */
export function decideOauthProposal(input) {
  const {
    llmBackend,
    codexCurrentAuth,
    hasOauthBackup,
    oauthProbeOk,
    dismissedUntilMs,
    permanentlyDismissed,
    codexAuthAutoFallbackActive,
    nowMs,
  } = input || {};

  // Explicit "don't ask" — never surface
  if (permanentlyDismissed === true) {
    return { kind: "none" };
  }

  // "Later" snooze active
  const dismissedUntil = Number(dismissedUntilMs) || 0;
  const now = Number(nowMs) || 0;
  if (dismissedUntil > now) {
    return { kind: "none" };
  }

  // No OAuth availability — never propose
  if (oauthProbeOk !== true) {
    return { kind: "none" };
  }

  if (llmBackend === "native") {
    // OAuth is live on the codex CLI and the user is paying via API key.
    // Propose the switch (codex mode uses the codex CLI's auth.json, which
    // is independent of the app's llm_backend setting).
    return { kind: "propose-native-to-codex" };
  }

  if (llmBackend === "codex") {
    // Auto-restore (v0.2.14) owns recovery when its flag is set. Don't
    // compete with it; the splash will swap back transparently.
    if (codexAuthAutoFallbackActive === true) {
      return { kind: "none" };
    }
    if (codexCurrentAuth === "apikey" && hasOauthBackup === true) {
      return { kind: "propose-restore-oauth" };
    }
    // codex + chatgpt — already optimal. codex + apikey w/o backup —
    // we can't propose a restore (would need full device-auth flow,
    // out of scope for a non-blocking banner).
    return { kind: "none" };
  }

  // native-oauth — already on the OAuth path; nothing to propose
  return { kind: "none" };
}
