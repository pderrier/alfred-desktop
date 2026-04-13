/**
 * Alfred Trigger Catalog — Named trigger definitions and context builders.
 *
 * Phase A: Welcome / demo trigger (alfred-welcome).
 * Phase B: Reactive triggers — error guidance (alfred-error-analysis-failed)
 *          and post-analysis summary (alfred-run-completed). These declare
 *          `autoFireOn` so the overlay bus fires them automatically when
 *          matching events arrive via notify().
 * Phase C: Proactive triggers — idle suggestions, stale positions, onboarding
 *          incomplete, and unlinked cash accounts. These use async contextBuilders
 *          that query backend state and return null when nothing to show.
 * Phase D: Additional triggers (registered but disabled, ready for future enabling).
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

  // ── Phase C: Proactive triggers ─────────────────────────────────

  overlay.registerTrigger({
    id: "alfred-stale-positions",
    priority: 3,
    cooldown: 43200000, // 12 hours
    autoFireOn: "dashboard-loaded",
    label: "Stale Positions",
    contextBuilder: async (_extra) => {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) return null;
      try {
        const result = await invoke("get_stale_positions_local");
        const staleCount = result?.stale_count || 0;
        if (staleCount === 0) return null; // nothing to show
        const oldestDays = result?.oldest_days_overdue || result?.oldest_days || 0;
        let message = `${staleCount} position${staleCount !== 1 ? "s" : ""} haven't been analyzed recently.`;
        if (oldestDays > 0) {
          message += ` The oldest is overdue by ${oldestDays} day${oldestDays !== 1 ? "s" : ""}.`;
        }
        return {
          initialMessage: message,
          actions: [
            {
              label: "Run analysis",
              callback: () => {
                document.getElementById("cmd-run-analysis")?.click();
              }
            },
            {
              label: "View positions",
              callback: () => {
                const table = document.querySelector(".positions-card") ||
                              document.getElementById("positions-tbody");
                if (table) table.scrollIntoView({ behavior: "smooth", block: "start" });
              }
            },
            { label: "Dismiss", dismiss: true }
          ],
          systemContext:
            `The user has ${staleCount} positions that are overdue for reanalysis` +
            (oldestDays > 0 ? ` (oldest by ${oldestDays} days)` : "") +
            ". Help them prioritize which positions to review first. Be concise.",
          chatMessage: message + " Let me help you prioritize which ones to review first."
        };
      } catch (e) {
        console.warn("[Alfred] get_stale_positions_local failed:", e);
        return null;
      }
    },
    enabled: true
  });

  overlay.registerTrigger({
    id: "alfred-idle-suggestion",
    priority: 2,
    cooldown: 1800000, // 30 minutes
    autoFireOn: "idle",
    label: "Suggestion",
    contextBuilder: async (_extra) => {
      const invoke = window.__TAURI__?.core?.invoke;
      let staleCount = 0;
      if (invoke) {
        try {
          const result = await invoke("get_stale_positions_local");
          staleCount = result?.stale_count || 0;
        } catch { /* ignore */ }
      }
      if (staleCount > 0) {
        return {
          initialMessage:
            `You have ${staleCount} position${staleCount !== 1 ? "s" : ""} that need${staleCount === 1 ? "s" : ""} reanalysis. Want to review them?`,
          actions: [
            {
              label: "Review positions",
              callback: () => {
                const table = document.querySelector(".positions-card") ||
                              document.getElementById("positions-tbody");
                if (table) table.scrollIntoView({ behavior: "smooth", block: "start" });
              }
            },
            { label: "Ask Alfred", chat: true },
            { label: "Not now", dismiss: true }
          ],
          systemContext:
            `The user has been idle for 5 minutes. ${staleCount} positions are overdue for reanalysis. ` +
            "Offer helpful portfolio review suggestions. Be concise and friendly.",
          chatMessage:
            `You have ${staleCount} positions that need reanalysis. Would you like me to help you prioritize them?`
        };
      }
      return {
        initialMessage:
          "Need help with your portfolio? I can walk you through your latest analysis.",
        actions: [
          {
            label: "Review positions",
            callback: () => {
              const table = document.querySelector(".positions-card") ||
                            document.getElementById("positions-tbody");
              if (table) table.scrollIntoView({ behavior: "smooth", block: "start" });
            }
          },
          { label: "Ask Alfred", chat: true },
          { label: "Not now", dismiss: true }
        ],
        systemContext:
          "The user has been idle for 5 minutes. Offer helpful portfolio review suggestions. Be concise and friendly.",
        chatMessage:
          "I can help you review your portfolio, explore your latest analysis, or answer any investment questions."
      };
    },
    enabled: true
  });

  overlay.registerTrigger({
    id: "alfred-onboarding-incomplete",
    priority: 7,
    cooldown: 0, // fires once, then marked handled
    autoFireOn: "app-ready",
    label: "Setup",
    contextBuilder: async (_extra) => {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) return null;
      try {
        const prefs = await invoke("get_user_preferences_local");
        if (prefs?.onboarding_complete === true) return null; // already done
      } catch { /* no prefs yet — show the prompt */ }
      return {
        initialMessage:
          "It looks like setup isn't finished. Want me to help you connect your portfolio?",
        actions: [
          {
            label: "Start setup",
            callback: () => {
              // Trigger the onboarding chat wizard flow
              // Use the global checkOnboarding bridge or open wizard directly
              if (typeof window.__triggerOnboarding === "function") {
                window.__triggerOnboarding();
              } else {
                // Fallback: click run-analysis to prompt the wizard
                document.getElementById("cmd-run-analysis")?.click();
              }
            }
          },
          {
            label: "Skip",
            callback: async () => {
              // Mark onboarding complete so this never fires again
              const inv = window.__TAURI__?.core?.invoke;
              if (inv) {
                try {
                  await inv("save_user_preferences_local", {
                    prefs: { onboarding_complete: true }
                  });
                } catch { /* not critical */ }
              }
            },
            dismiss: true
          }
        ],
        systemContext:
          "The user has not completed initial setup. Help them get started with connecting " +
          "their portfolio (Finary or CSV) and configuring their LLM backend. Be welcoming and concise.",
        chatMessage:
          "Welcome! Let's get you set up. I can help you connect your portfolio and configure Alfred."
      };
    },
    enabled: true
  });

  overlay.registerTrigger({
    id: "alfred-unlinked-cash",
    priority: 4,
    cooldown: 86400000, // 24 hours
    autoFireOn: "dashboard-loaded",
    label: "Cash Accounts",
    contextBuilder: async (extra) => {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) return null;
      const finaryMeta = extra?.finaryMeta || extra || {};
      const groups = Array.isArray(finaryMeta.ambiguous_cash_groups)
        ? finaryMeta.ambiguous_cash_groups
        : [];
      if (groups.length === 0) return null;
      // Check if user already linked all ambiguous groups
      try {
        const prefs = await invoke("get_user_preferences_local") || {};
        const savedLinks = prefs?.cash_account_links || {};
        const allInvestmentNames = groups.flatMap((g) =>
          (g.investment_accounts || []).map((a) => a.name)
        );
        const allCovered = allInvestmentNames.every((name) => savedLinks[name]);
        if (allCovered) return null; // all already linked
      } catch { /* proceed */ }
      const uncoveredCount = groups.length;
      return {
        initialMessage:
          `I found ${uncoveredCount} cash account${uncoveredCount !== 1 ? "s" : ""} that ` +
          `${uncoveredCount !== 1 ? "aren't" : "isn't"} linked to investment accounts. ` +
          "This affects your cash balance accuracy.",
        actions: [
          {
            label: "Link now",
            callback: () => {
              // Trigger the cash matching wizard for the first uncovered group
              if (typeof window.__openCashWizard === "function") {
                window.__openCashWizard(groups);
              }
            }
          },
          { label: "Later", dismiss: true, snoozeDuration: 86400000 }
        ],
        systemContext:
          `There are ${uncoveredCount} ambiguous cash account group(s) that need linking. ` +
          "Help the user understand why cash account linking matters for accurate portfolio tracking.",
        chatMessage:
          `I found ${uncoveredCount} cash account${uncoveredCount !== 1 ? "s" : ""} that need linking. ` +
          "Let me explain why this matters and help you set it up."
      };
    },
    enabled: true
  });

  // ── Phase D triggers (registered but disabled, ready for Phase C to enable) ──

  overlay.registerTrigger({
    id: "alfred-theme-concentration",
    priority: 5,
    cooldown: 86400000, // 24 hours
    label: "Theme Concentration",
    contextBuilder: (extra) => {
      const count = extra?.themeCount || "several";
      const themes = extra?.themes || [];
      const themeList = themes.length > 0 ? themes.join(", ") : "multiple themes";
      return {
        initialMessage:
          `I notice ${count} themes are concentrated across your portfolio (${themeList}). ` +
          "This could indicate sector concentration risk. Want to discuss?",
        actions: [
          { label: "Talk to Alfred", chat: true },
          { label: "Not now", dismiss: true }
        ],
        systemContext:
          "The user's portfolio shows theme concentration. " +
          `Concentrated themes: ${themeList}. ` +
          "Help them understand concentration risk and potential diversification strategies. Be concise.",
        chatMessage:
          `I've noticed theme concentration in your portfolio across: ${themeList}. ` +
          "Let me help you understand what this means for your risk exposure."
      };
    },
    autoFireOn: "theme-concentration-detected",
    enabled: false // Phase D — ready for Phase C to enable
  });

  overlay.registerTrigger({
    id: "alfred-run-diff-highlight",
    priority: 4,
    cooldown: 0,
    label: "Signal Changes",
    contextBuilder: (extra) => {
      const changes = extra?.signalChanges || 0;
      const upgrades = extra?.upgrades || 0;
      const downgrades = extra?.downgrades || 0;
      let detail = `${changes} position${changes !== 1 ? "s" : ""} changed signal since last analysis`;
      if (upgrades > 0 || downgrades > 0) {
        const parts = [];
        if (upgrades > 0) parts.push(`${upgrades} upgrade${upgrades !== 1 ? "s" : ""}`);
        if (downgrades > 0) parts.push(`${downgrades} downgrade${downgrades !== 1 ? "s" : ""}`);
        detail += ` (${parts.join(", ")})`;
      }
      return {
        initialMessage: detail + ". Want me to walk you through the changes?",
        actions: [
          { label: "Talk to Alfred", chat: true },
          { label: "Not now", dismiss: true }
        ],
        systemContext:
          "The user's latest analysis shows signal changes compared to the previous run. " +
          `Summary: ${changes} signal changes, ${upgrades} upgrades, ${downgrades} downgrades. ` +
          "Walk them through the most significant changes concisely.",
        chatMessage:
          `Your latest analysis shows ${detail}. Let me highlight the most important changes.`
      };
    },
    autoFireOn: "run-diff-available",
    enabled: false // Phase D — ready for Phase C to enable
  });

  overlay.registerTrigger({
    id: "alfred-scorecard-review",
    priority: 3,
    cooldown: 604800000, // 7 days
    label: "Scorecard Review",
    contextBuilder: (extra) => {
      const trend = extra?.trend || "changed";
      const accuracy = extra?.accuracy != null ? `${extra.accuracy}%` : "recently changed";
      return {
        initialMessage:
          `Your signal accuracy has ${trend} \u2014 currently at ${accuracy}. ` +
          "Want to review your recommendation scorecard?",
        actions: [
          { label: "Talk to Alfred", chat: true },
          { label: "Not now", dismiss: true }
        ],
        systemContext:
          "The user's signal accuracy scorecard shows a change. " +
          `Current trend: ${trend}, accuracy: ${accuracy}. ` +
          "Help them understand their recommendation accuracy and what it means for their strategy. Be practical.",
        chatMessage:
          `Your signal accuracy is ${accuracy} and the trend is ${trend}. ` +
          "Let me help you review what this means for your investment decisions."
      };
    },
    autoFireOn: "scorecard-review-due",
    enabled: false // Phase D — ready for Phase C to enable
  });

  // ── Strategy Refinement — proactive help building investment guidelines ──
  overlay.registerTrigger({
    id: "alfred-strategy-refine",
    priority: 4,
    cooldown: 86400000 * 7, // 7 days
    label: "Investment Strategy",
    enabled: true,
    contextBuilder: async (extra) => {
      try {
        const invoke = window.__TAURI__?.core?.invoke;
        if (!invoke) return null;
        const prefs = await invoke("get_user_preferences_local") || {};
        const account = extra?.account || window.__selectedAccount || "";
        const guidelines = (prefs?.guidelines_by_account?.[account] || "").trim();
        // Don't fire if guidelines are already substantial (> 80 chars)
        if (guidelines.length > 80) return null;
        const hasRun = extra?.hasRun !== false;
        const isEmpty = guidelines.length === 0;
        const msg = isEmpty
          ? "Your portfolio analysis runs without an investment strategy defined. " +
            "I can help you build one through a quick conversation — covering risk tolerance, " +
            "time horizon, sector preferences, and goals."
          : "Your investment strategy is quite brief. A more detailed strategy helps me give " +
            "better-targeted recommendations. Want to refine it together?";
        return {
          initialMessage: msg,
          actions: [
            {
              label: "Refine my strategy",
              primary: true,
              chat: true,
              systemContext:
                "You are Alfred, a portfolio analysis assistant helping the user define their investment strategy. " +
                "Guide them through these aspects one at a time, in a conversational way:\n" +
                "1. **Investment horizon**: short-term (< 1 year), medium (1-5 years), long-term (5+ years)\n" +
                "2. **Risk tolerance**: conservative, balanced, or aggressive\n" +
                "3. **Investment style**: value, growth, income/dividends, index/passive, or mixed\n" +
                "4. **Sector preferences**: any sectors to favor or avoid? (tech, energy, healthcare, etc.)\n" +
                "5. **Geographic focus**: France/Europe only, global, emerging markets?\n" +
                "6. **Account constraints**: PEA (French tax wrapper — EU stocks only), CTO (no restrictions), both?\n" +
                "7. **Specific goals**: retirement, house purchase, education fund, wealth building?\n" +
                "8. **Current concerns**: anything worrying them about the market or their portfolio?\n\n" +
                "After gathering answers, produce a structured strategy summary in this format:\n" +
                "STRATEGY:\n" +
                "Horizon: ...\n" +
                "Risk: ...\n" +
                "Style: ...\n" +
                "Sectors: ...\n" +
                "Geography: ...\n" +
                "Constraints: ...\n" +
                "Goals: ...\n" +
                "Notes: ...\n\n" +
                (guidelines ? `Current guidelines (user wrote): "${guidelines}"\nBuild on these.\n` : "") +
                "Be concise, friendly, use French if the user responds in French. " +
                "One question at a time. Summarize at the end.",
              chatMessage: isEmpty
                ? "Let's define your investment strategy! First — what's your investment horizon? Are you investing for the short term (less than a year), medium term (1-5 years), or long term (5+ years)?"
                : `I see you've written: "${guidelines}". Let's flesh this out. What's your risk tolerance — conservative, balanced, or aggressive?`
            },
            { label: "Not now", dismiss: true }
          ],
          // After chat, extract the STRATEGY block and save to guidelines
          onChatComplete: async (history) => {
            try {
              const lastAssistant = [...history].reverse().find(m => m.role === "assistant");
              if (!lastAssistant) return;
              const strategyMatch = lastAssistant.content.match(/STRATEGY:\s*\n([\s\S]*?)(?:\n\n|$)/i);
              if (strategyMatch) {
                const strategy = strategyMatch[1].trim();
                const prefs2 = await invoke("get_user_preferences_local") || {};
                if (!prefs2.guidelines_by_account) prefs2.guidelines_by_account = {};
                prefs2.guidelines_by_account[account] = strategy;
                await invoke("save_user_preferences_local", { prefs: prefs2 });
                window.__SHELL_LAYOUT?.showToast?.("Investment strategy saved — next analysis will use it.", "success");
              }
            } catch { /* best effort */ }
          }
        };
      } catch { return null; }
    },
    autoFireOn: "run-wizard-opened"
  });
}
