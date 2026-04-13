/**
 * Alfred Trigger Catalog — Named trigger definitions and context builders.
 *
 * Phase A: Welcome / demo trigger (alfred-welcome).
 * Phase B: Reactive triggers — error guidance (alfred-error-analysis-failed)
 *          and post-analysis summary (alfred-run-completed). These declare
 *          `autoFireOn` so the overlay bus fires them automatically when
 *          matching events arrive via notify().
 * Phase C+: Stale positions, idle, onboarding triggers (registered but disabled).
 *
 * Usage:
 *   import { registerDefaultTriggers } from "/desktop-shell/app-alfred-triggers.js";
 *   registerDefaultTriggers(alfredOverlay);
 */

/**
 * Register all default triggers with the Alfred overlay bus.
 * @param {Object} overlay — the overlay API returned by initAlfredOverlay()
 */
export function registerDefaultTriggers(overlay) {
  if (!overlay || typeof overlay.registerTrigger !== "function") return;

  // ── Phase A: Welcome / demo trigger ────────────────────────────
  overlay.registerTrigger({
    id: "alfred-welcome",
    priority: 1,
    cooldown: 86400000, // 24 hours
    label: "Bienvenue",
    contextBuilder: () => ({
      initialMessage:
        "Bienvenue\u00a0! Je suis Alfred, votre assistant. " +
        "Je vous signalerai les \u00e9l\u00e9ments importants.",
      actions: [
        { label: "OK", dismiss: true }
      ]
    }),
    enabled: true
  });

  // ── Phase B: Reactive triggers ─────────────────────────────────

  overlay.registerTrigger({
    id: "alfred-error-analysis-failed",
    priority: 8,
    cooldown: 0,
    autoFireOn: "run-failed",
    label: "Analysis Failed",
    contextBuilder: (extra) => {
      const errorMsg = extra?.message || extra?.error || "An unknown error occurred.";
      const guidance =
        "The analysis encountered an issue. Common causes: " +
        "API key expired, network timeout, LLM rate limit.";
      return {
        initialMessage: `The analysis didn\u2019t complete. ${errorMsg}\n\n${guidance}`,
        actions: [
          {
            label: "Retry Analysis",
            callback: () => {
              document.getElementById("cmd-run-analysis")?.click();
            }
          },
          {
            label: "Check Settings",
            callback: () => {
              // Navigate to the settings tab
              const settingsLink = document.querySelector('[data-tab="settings"]');
              if (settingsLink) {
                settingsLink.click();
              }
            }
          },
          { label: "Got it", dismiss: true }
        ],
        systemContext:
          "The user's portfolio analysis just failed with error: " + errorMsg +
          ". Help them diagnose the issue and suggest next steps. Common causes " +
          "include expired API keys, network timeouts, and LLM rate limits. " +
          "Be concise and practical.",
        chatMessage:
          "The analysis encountered an error: " + errorMsg +
          "\n\nLet me help you understand what happened and how to fix it."
      };
    },
    enabled: true
  });

  overlay.registerTrigger({
    id: "alfred-run-completed",
    priority: 5,
    cooldown: 0,
    autoFireOn: "run-completed",
    label: "Analysis Complete",
    contextBuilder: async (extra) => {
      // Fetch stale positions and run diff from Tauri backend
      const invoke = window.__TAURI__?.core?.invoke;
      let staleCount = 0;
      let signalChanges = 0;
      let diffSummaryText = "";

      if (invoke) {
        try {
          const staleResult = await invoke("get_stale_positions_local");
          staleCount = staleResult?.stale_count || 0;
        } catch (e) {
          console.warn("[Alfred] get_stale_positions_local failed:", e);
        }

        try {
          const diff = await invoke("get_run_diff_local");
          if (diff?.has_previous && diff.summary) {
            const s = diff.summary;
            signalChanges = s.signal_changes || 0;
            const parts = [];
            if (signalChanges > 0) {
              parts.push(`${signalChanges} signal change${signalChanges !== 1 ? "s" : ""}`);
            }
            if (s.significant_moves > 0) {
              parts.push(`${s.significant_moves} significant price move${s.significant_moves !== 1 ? "s" : ""}`);
            }
            diffSummaryText = parts.join(", ");
          }
        } catch (e) {
          console.warn("[Alfred] get_run_diff_local failed:", e);
        }
      }

      // Build the summary message
      const highlights = [];
      if (diffSummaryText) {
        highlights.push(diffSummaryText);
      }
      if (staleCount > 0) {
        highlights.push(
          `${staleCount} position${staleCount !== 1 ? "s" : ""} need${staleCount === 1 ? "s" : ""} reanalysis`
        );
      }

      let message = "Analysis complete!";
      if (highlights.length > 0) {
        message += " " + highlights.join(". ") + ".";
      } else {
        message += " Your portfolio has been updated.";
      }

      return {
        initialMessage: message,
        actions: [
          { label: "Talk to Alfred", chat: true },
          {
            label: "View Details",
            callback: () => {
              const diffSection = document.querySelector(".run-diff-section") ||
                                  document.getElementById("run-diff-container");
              if (diffSection) {
                diffSection.scrollIntoView({ behavior: "smooth", block: "start" });
              }
            }
          },
          { label: "OK", dismiss: true }
        ],
        systemContext:
          "The user just completed a portfolio analysis. " +
          (signalChanges > 0
            ? `There were ${signalChanges} signal changes since the last run. `
            : "No signal changes were detected. ") +
          (staleCount > 0
            ? `${staleCount} positions are overdue for reanalysis. `
            : "") +
          "Offer to explain the key findings and recommendations. " +
          "Be concise and practical.",
        chatMessage: message +
          " Let me walk you through the key findings and recommendations."
      };
    },
    enabled: true
  });

  overlay.registerTrigger({
    id: "alfred-stale-positions",
    priority: 3,
    cooldown: 86400000, // 24 hours
    label: "Stale Positions",
    contextBuilder: (extra) => ({
      initialMessage:
        `${extra?.count || "Some"} positions are overdue for reanalysis. ` +
        "Want to run a targeted update?",
      actions: [
        { label: "Run analysis", callback: extra?.runAnalysis },
        { label: "Show me which ones", callback: extra?.showStale },
        { label: "Not now", dismiss: true }
      ]
    }),
    enabled: false // Phase C
  });
}
