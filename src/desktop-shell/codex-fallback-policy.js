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
