/**
 * Pure context builder for the `alfred-analysis-narration` trigger.
 *
 * Extracted into a standalone, dependency-free module so it can be imported
 * and unit-tested from outside the Tauri webview environment (Node tests
 * cannot resolve the webview's absolute `/desktop-shell/...` import paths).
 */

export function buildNarrationContext(extra) {
  const message = extra && typeof extra.message === "string" ? extra.message.trim() : "";
  if (!message) return null;
  return {
    initialMessage: message,
    autoDismissMs: 12000,
    actions: []
  };
}
