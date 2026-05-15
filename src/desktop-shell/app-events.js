/**
 * App Events — Tauri event listeners, real-time line progress, news links.
 *
 * Extracted from app.js for single-responsibility. Registers all push-based
 * event handlers for live run updates.
 */

import { escapeHtml } from "/desktop-shell/ui-display-utils.js";
import {
  renderTopBarProgress,
  renderPipelineBar,
  updateSingleLineProgress,
  setNarrationDegradedBadge,
  updateMarketSynthesisEarlyStage
} from "/desktop-shell/shell-layout.js";

export function initEvents(deps) {
  const {
    bridge,
    getActiveRunId,
  } = deps;

  // ── Real-time line progress via Tauri events (no polling delay) ──
  if (window?.__TAURI__?.event?.listen) {
    window.__TAURI__.event.listen("alfred://line-progress", (event) => {
      const { ticker, line_status } = event.payload || {};
      if (!ticker || !line_status || !getActiveRunId()) return;
      // Build a minimal lineStatus object and re-render just that line
      updateSingleLineProgress(ticker, line_status);
    });

    // Synthesis progress — instant UI update
    window.__TAURI__.event.listen("alfred://synthesis-progress", (event) => {
      const { progress } = event.payload || {};
      if (!progress || !getActiveRunId()) return;
      const synthesis = document.getElementById("report-synthesis");
      if (synthesis) {
        synthesis.innerHTML = `<span class="synthesis-pending-label"><span class="pipeline-spinner"></span>Generating global synthesis\u2026 ${escapeHtml(progress)}</span>`;
      }
    });

    // Line done — instant recommendation display (no polling delay)
    window.__TAURI__.event.listen("alfred://line-done", (event) => {
      const { ticker, recommendation, line_progress } = event.payload || {};
      if (!ticker || !getActiveRunId()) return;
      // Update the row immediately with recommendation data
      updateSingleLineProgress(ticker, { status: "done", recommendation });
      // Update progress counter
      if (line_progress) {
        renderTopBarProgress({ status: "running", line_progress });
      }
    });

    // Run stage changes — instant pipeline bar update + early-run toast.
    // P1-2: the granular `finary_fetching` / `snapshot_received` /
    // `enriching_market` stages also drive the `alfred-run-stage-toast`
    // overlay trigger so the user has feedback during the pre-LLM phase,
    // and the Market Synthesis card shows progressive status text via the
    // synthesis card writer below.
    window.__TAURI__.event.listen("alfred://run-stage", (event) => {
      const payload = event.payload || {};
      const { stage, line_progress, collection_progress } = payload;
      if (!stage || !getActiveRunId()) return;
      renderPipelineBar(stage);
      renderTopBarProgress({ status: "running", line_progress });
      // Surface early-run progress in the Market Synthesis card so the
      // user sees "Récupération Finary…" / "Snapshot reçu (N)" /
      // "Cours marché (C/T)…" instead of static "Waiting for…" text.
      updateMarketSynthesisEarlyStage(stage, payload);
      // Fan out to the overlay trigger registry. The overlay picks up
      // `run-stage` via the `alfred-run-stage-toast` trigger registered
      // in app-alfred-triggers.js.
      window.__alfredOverlay?.notify?.("run-stage", {
        stage,
        collection_progress,
        line_progress,
        snapshot_summary: payload.snapshot_summary,
      });
    });

    // Run narration — LLM-generated 1-sentence summary of the last 10s of events.
    // Routed to the alfred overlay as a "run-narration" notification, which the
    // alfred-analysis-narration trigger picks up. Empty payload or no active run → ignored.
    window.__TAURI__.event.listen("alfred://run-narration", (event) => {
      const { message } = event.payload || {};
      if (!message || !getActiveRunId()) return;
      window.__alfredOverlay?.notify?.("run-narration", { message });
    });

    // Run-narration health — surfaces an amber "narration: mode dégradé"
    // badge when the Rust narrator gives up after FAILURE_DEGRADE_THRESHOLD
    // consecutive LLM failures (P0-2). The run itself keeps going; only
    // the narrator toast cadence is paused. Reset on every fresh run via
    // `clearRunPipelineBar`.
    window.__TAURI__.event.listen("alfred://run-narration-status", (event) => {
      const { mode, reason } = event.payload || {};
      if (!getActiveRunId()) return;
      if (mode === "degraded") {
        setNarrationDegradedBadge(true, reason || "");
      } else {
        setNarrationDegradedBadge(false, "");
      }
    });
  }

  // ── News article links → open in external browser ──
  document.addEventListener("click", (e) => {
    const link = e.target.closest(".news-article-link");
    if (link) {
      e.preventDefault();
      e.stopPropagation();
      const url = link.dataset.url;
      if (url) {
        bridge.openExternalUrl(url).catch(() => {
          window.open(url, "_blank");
        });
      }
    }
  });

  // ── Modal overlay close on backdrop click ──
  document.querySelectorAll(".modal-overlay").forEach((overlay) => {
    overlay.addEventListener("click", (event) => {
      if (event.target === overlay) overlay.classList.add("hidden");
    });
  });
}
