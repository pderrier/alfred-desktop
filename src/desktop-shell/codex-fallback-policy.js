/**
 * Codex OAuth -> API-key auto-fallback decision policy.
 *
 * Pure function used by the splash startup check in `app-bootstrap.js` AND
 * by the unit-test suite. Keeping the decision tree in one place prevents
 * the test replica from drifting from production behavior.
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
 *   openaiOk: boolean — what the splash should treat as the "connected" state
 *                       after the decision is applied
 */

/**
 * @typedef {{
 *   logged_in?: boolean,
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
 * }} input
 * @returns {{
 *   action: "noop"|"switch-to-native"|"reveal-key-input"|"clear-fallback-flag"|"auto-restore-oauth",
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
  } = input;

  if (llmBackend === "native") {
    if (nativeKeyOk && autoFallbackActive && probeResult?.logged_in === true) {
      return { action: "auto-restore-oauth", openaiOk: true };
    }
    return { action: "noop", openaiOk: nativeKeyOk === true };
  }

  // codex | native-oauth
  const probeOk = probeResult?.logged_in === true;

  if (probeOk && llmBackend === "native-oauth" && autoFallbackActive) {
    // Clear flag only — doesn't mutate openaiOk
    return { action: "clear-fallback-flag", openaiOk: true };
  }

  if (probeOk) {
    return { action: "noop", openaiOk: true };
  }

  if (llmBackend === "native-oauth" && probeResult?.failure_reason === "rate_limited") {
    const apiKey = (savedApiKey || "").trim();
    if (!apiKey) {
      return { action: "reveal-key-input", openaiOk: false };
    }
    return { action: "switch-to-native", openaiOk: nativeKeyOk === true };
  }

  return { action: "noop", openaiOk: false };
}
