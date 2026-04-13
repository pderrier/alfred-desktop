/**
 * Alfred Trigger Catalog — Named trigger definitions and context builders.
 *
 * Phase A ships one demo trigger (alfred-welcome). Future phases add error
 * triggers, post-analysis triggers, idle triggers, and onboarding triggers.
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

  // ── Future Phase B triggers (registered but disabled) ──────────

  overlay.registerTrigger({
    id: "alfred-error-analysis-failed",
    priority: 8,
    cooldown: 0,
    label: "Analysis Failed",
    contextBuilder: (extra) => ({
      initialMessage:
        `The analysis didn\u2019t complete. ${extra?.message || "An error occurred."}` +
        " Want me to walk you through what went wrong?",
      actions: [
        { label: "Talk to Alfred", chat: true },
        { label: "Not now", dismiss: true }
      ],
      systemContext:
        "The user's portfolio analysis just failed. Help them diagnose the issue " +
        "and suggest next steps. Be concise and practical.",
      chatMessage:
        "The analysis encountered an error. Let me help you understand what happened and how to fix it."
    }),
    enabled: false // Phase B
  });

  overlay.registerTrigger({
    id: "alfred-run-completed",
    priority: 5,
    cooldown: 0,
    label: "Analysis Complete",
    contextBuilder: (extra) => ({
      initialMessage:
        "Analysis complete. Want a walkthrough of the results?",
      actions: [
        { label: "Walk me through it", chat: true },
        { label: "I\u2019ll explore myself", dismiss: true }
      ],
      systemContext:
        "The user just completed a portfolio analysis. Offer to explain the key findings.",
      chatMessage:
        "Your analysis is ready. Let me walk you through the key findings and recommendations."
    }),
    enabled: false // Phase B
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
