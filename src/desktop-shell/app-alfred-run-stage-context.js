/**
 * Pure context builder for the `alfred-run-stage-toast` trigger (P1-2).
 *
 * The Rust backend emits `alfred://run-stage` events during the early
 * pre-LLM phase of a run — Finary fetch, snapshot receipt, market
 * enrichment. Before v0.3.2 this phase was 100 % silent for the user.
 * This module maps each granular stage to a short FR toast message so the
 * overlay can show progressive feedback while Alfred is still preparing
 * data.
 *
 * Extracted into a standalone, dependency-free module so it can be
 * imported and unit-tested from Node (no Tauri webview).
 */

/**
 * Map a run-stage event payload to the overlay context.
 *
 * Returns null when the stage is not one of the early-run stages we
 * narrate (e.g. `line_analysis`, `synthesis`, `done` — those are owned
 * by the LLM narration trigger and the pipeline bar, not by this toast).
 * Returning null aborts firing the trigger, per the overlay contract.
 *
 * @param {object|null|undefined} extra — Tauri payload from app-events.js
 *   `{ stage, collection_progress?: {completed,total}, snapshot_summary?: {positions_count} }`
 * @returns {object|null} Overlay context with `initialMessage` + `autoDismissMs`.
 */
export function buildRunStageContext(extra) {
  if (!extra || typeof extra !== "object") return null;
  const stage = typeof extra.stage === "string" ? extra.stage : "";
  if (!stage) return null;

  const message = formatStageMessage(stage, extra);
  if (!message) return null;

  return {
    initialMessage: message,
    autoDismissMs: 5000,
    actions: []
  };
}

/**
 * Pure stage → FR message mapping. Exposed for direct unit testing.
 * Stages not present in the map return "" (the trigger then returns null
 * and the overlay skips firing).
 */
export function formatStageMessage(stage, extra) {
  switch (stage) {
    case "finary_fetching":
      return "Récupération du portefeuille Finary…";
    case "snapshot_received": {
      const count = extractPositionsCount(extra);
      return count > 0
        ? `✓ Snapshot reçu (${count} lignes)`
        : "✓ Snapshot reçu";
    }
    case "enriching_market": {
      const cp = extra?.collection_progress;
      if (cp && Number.isFinite(cp.total) && cp.total > 0) {
        const completed = Number.isFinite(cp.completed) ? cp.completed : 0;
        return `Cours marché (${completed}/${cp.total})…`;
      }
      return "Récupération des cours marché…";
    }
    default:
      return "";
  }
}

function extractPositionsCount(extra) {
  // Server may pass the count via `snapshot_summary.positions_count`
  // (preferred) or `collection_progress.total` (fallback — the initial
  // collecting_data event carries it). Both shapes accepted.
  const snap = extra?.snapshot_summary;
  if (snap && Number.isFinite(snap.positions_count)) return snap.positions_count;
  const cp = extra?.collection_progress;
  if (cp && Number.isFinite(cp.total)) return cp.total;
  return 0;
}
