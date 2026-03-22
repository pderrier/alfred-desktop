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
  updateSingleLineProgress
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

    // Run stage changes — instant pipeline bar update
    window.__TAURI__.event.listen("alfred://run-stage", (event) => {
      const { stage, line_progress } = event.payload || {};
      if (!stage || !getActiveRunId()) return;
      renderPipelineBar(stage);
      renderTopBarProgress({ status: "running", line_progress });
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
