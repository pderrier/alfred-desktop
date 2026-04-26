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

import { openChatWizard } from "/desktop-shell/app-chat-wizard.js";

// ── Onboarding chat wizard ─────────────────────────────────────

const ONBOARDING_SYSTEM_CONTEXT = `Tu es Alfred, l'assistant intelligent de gestion de portefeuille. L'utilisateur vient de lancer l'application pour la première fois et n'a pas encore configuré son compte.

Guide-le à travers les 3 étapes de configuration, une à la fois. Sois accueillant, concis et utile.

**Étape 1 : Source du portefeuille**
Demande à l'utilisateur comment il souhaite connecter son portefeuille :
- **Finary** (synchronisation automatique) : Alfred ouvrira une fenêtre de connexion Finary dans le navigateur. L'utilisateur se connecte normalement sur finary.com et Alfred récupère la session automatiquement. Aucun token ou cookie à copier manuellement.
- **Import CSV** : il peut importer un fichier CSV avec ses positions. Explique le format attendu (colonnes : ISIN ou ticker, quantité, prix d'achat). Après la configuration, il pourra utiliser le bouton "Import CSV" dans l'interface.

**Étape 2 : Backend LLM (moteur d'analyse)**
Explique les 3 modes disponibles pour l'analyse IA :
- **Codex (gratuit)** : utilise le serveur Alfred, aucune clé API nécessaire. Bon pour commencer.
- **Native (clé API personnelle)** : utilise directement l'API OpenAI avec sa propre clé. Plus rapide, plus de contrôle.
- **Native OAuth** : utilise l'app Codex comme proxy OAuth. Pas de clé API nécessaire, authentification via navigateur.
Recommande le mode Codex pour commencer si l'utilisateur n'est pas sûr.

**Étape 3 : Première analyse**
Une fois les choix faits, propose de lancer la première analyse du portefeuille. Explique que l'analyse prend environ 2-3 minutes et couvre : évaluation fondamentale, actualités récentes, diversification, et recommandations.

Règles :
- Parle en français.
- Une étape à la fois — ne submerge pas l'utilisateur.
- Si l'utilisateur dit "skip" ou veut passer une étape, accepte et passe à la suivante.
- Sois encourageant et montre que la configuration est simple.
- Ne demande PAS de données sensibles. La connexion Finary se fait via le navigateur, pas par token.`;

const ONBOARDING_INITIAL_MESSAGE = `Bienvenue dans Alfred ! Je suis votre assistant de gestion de portefeuille.

Je vais vous guider à travers la configuration en 3 étapes rapides :
1. Connecter votre portefeuille (Finary ou CSV)
2. Choisir votre moteur d'analyse IA
3. Lancer votre première analyse

Commençons ! Comment souhaitez-vous connecter votre portefeuille ?
- **Finary** : synchronisation automatique de vos comptes
- **CSV** : import manuel de vos positions`;

/**
 * Launch the onboarding chat wizard modal.
 * On completion (close/done), delegates to the app-level processOnboardingResult
 * (exposed as window.__processOnboardingResult) which handles intent extraction,
 * API key saving, Finary connection, and first-run triggering.
 * Falls back to basic onboarding_complete save if the bridge isn't registered yet.
 */
async function launchOnboardingWizard() {
  const invoke = window.__TAURI__?.core?.invoke;

  const history = await openChatWizard({
    title: "Configuration d'Alfred",
    systemContext: ONBOARDING_SYSTEM_CONTEXT,
    initialMessage: ONBOARDING_INITIAL_MESSAGE,
    returnHistoryOnClose: true,
  });

  // Delegate to app.js rich result processor if available
  if (typeof window.__processOnboardingResult === "function") {
    await window.__processOnboardingResult(history || []);
    return;
  }

  // Fallback: just mark onboarding complete
  if (invoke) {
    try {
      await invoke("save_user_preferences_local", {
        prefs: { onboarding_complete: true },
      });
    } catch { /* non-critical */ }
  }
}

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
    label: "Configuration",
    contextBuilder: async (_extra) => {
      const invoke = window.__TAURI__?.core?.invoke;
      if (!invoke) return null;
      try {
        const prefs = await invoke("get_user_preferences_local");
        if (prefs?.onboarding_complete === true) return null; // already done
      } catch { /* no prefs yet — show the prompt */ }

      // Launch the chat wizard directly — no intermediate panel.
      // Fire-and-forget: the wizard handles its own lifecycle.
      launchOnboardingWizard();

      // Return null so the overlay bus does not show a panel.
      return null;
    },
    enabled: true,
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

  // ── Item 11: Mid-run overlay commentary ──────────────────────────

  overlay.registerTrigger({
    id: "alfred-analysis-progress",
    priority: 2,
    cooldown: 30000, // 30s between messages to avoid spam
    autoFireOn: "line-analyzed",
    label: "Analysis Progress",
    contextBuilder: (extra) => {
      const { completedCount, totalCount, latestTicker } = extra || {};
      if (!completedCount || !totalCount) return null;
      const msg = `Analyzed ${completedCount}/${totalCount} positions\u2026 just finished ${latestTicker || "a position"}.`;
      return {
        initialMessage: msg,
        autoDismissMs: 8000,
        actions: [] // No actions — informational only
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
    enabled: true
  });

  // ── Accuracy Nudge — post-run check for signals that aged badly ──
  overlay.registerTrigger({
    id: "alfred-accuracy-nudge",
    priority: 4,
    cooldown: 0, // per-run (fires once per run completion)
    autoFireOn: "run-completed-with-data",
    label: "Accuracy Check",
    contextBuilder: (extra) => {
      const recommendations = extra?.recommendations;
      if (!Array.isArray(recommendations) || recommendations.length === 0) return null;

      // Per-ticker 24h cooldown via localStorage
      const COOLDOWN_KEY = "alfred-accuracy-nudge-cooldowns";
      const COOLDOWN_MS = 86400000; // 24 hours
      const now = Date.now();
      let tickerCooldowns = {};
      try { tickerCooldowns = JSON.parse(localStorage.getItem(COOLDOWN_KEY) || "{}"); } catch { /* ignore */ }

      const BULLISH_SIGNALS = new Set(["RENFORCER", "CONSERVER", "ACHETER", "BUY", "HOLD", "REINFORCE"]);
      const BEARISH_SIGNALS = new Set(["ALLEGER", "VENDRE", "SELL", "REDUCE"]);
      const THRESHOLD_PCT = 10;

      const misaligned = [];
      for (const rec of recommendations) {
        const lm = rec.lineMemory || rec.memoire_ligne || {};
        const pt = lm.price_tracking;
        if (!pt || pt.return_since_signal_pct == null) continue;
        const returnPct = Number(pt.return_since_signal_pct);
        if (!Number.isFinite(returnPct)) continue;
        const signal = String(rec.signal || "").toUpperCase();
        const isBullish = BULLISH_SIGNALS.has(signal);
        const isBearish = BEARISH_SIGNALS.has(signal);
        // Misalignment: bullish signal but price dropped, or bearish signal but price rose
        const misalignedAmt = isBullish ? -returnPct : isBearish ? returnPct : 0;
        const ticker = rec.ticker || rec.nom || "?";
        if (misalignedAmt >= THRESHOLD_PCT) {
          const lastNudged = tickerCooldowns[ticker] || 0;
          if (now - lastNudged < COOLDOWN_MS) continue;
          misaligned.push({ ticker, signal, returnPct, misalignedAmt });
        }
      }
      if (misaligned.length === 0) return null;

      // Cap at top 2 worst-performing
      misaligned.sort((a, b) => b.misalignedAmt - a.misalignedAmt);
      const top = misaligned.slice(0, 2);

      // Record per-ticker cooldown
      for (const m of top) { tickerCooldowns[m.ticker] = now; }
      try { localStorage.setItem(COOLDOWN_KEY, JSON.stringify(tickerCooldowns)); } catch { /* ignore */ }

      const lines = top.map((m) => {
        const dir = m.returnPct >= 0 ? "+" : "";
        return `**${m.ticker}**: price moved ${dir}${m.returnPct.toFixed(1)}% against ${m.signal} signal`;
      });

      const firstTicker = top[0].ticker;
      return {
        initialMessage:
          `My call on ${top.length === 1 ? firstTicker : `${top.length} positions`} hasn't aged well.\n\n` +
          lines.join("\n") +
          "\n\nWant to revisit?",
        actions: top.map((m) => ({
          label: `Revisit ${m.ticker}`,
          callback: () => {
            if (window.__openLineMemoryModal) {
              const rec = recommendations.find((r) => (r.ticker || r.nom) === m.ticker);
              if (rec) window.__openLineMemoryModal(rec);
            }
          },
        })).concat([
          { label: "Dismiss", dismiss: true },
        ]),
        systemContext:
          `The user's portfolio has ${top.length} position(s) where the recommendation signal is not aligned with price movement:\n` +
          top.map((m) => `- ${m.ticker}: signal ${m.signal}, return ${m.returnPct.toFixed(1)}%`).join("\n") +
          "\nHelp them evaluate whether to adjust their position or maintain conviction. Be concise and practical.",
        chatMessage:
          `I've noticed ${top.length} position${top.length > 1 ? "s" : ""} where the price has moved against the recommendation signal. Let me help you evaluate whether to adjust.`,
      };
    },
    enabled: true,
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
        const gl = guidelines.toLowerCase();
        // Check which strategy aspects are covered (not just length)
        const aspects = [
          { key: "horizon", patterns: /horizon|terme|court[\s-]?terme|long[\s-]?terme|moyen[\s-]?terme|short[\s-]?term|long[\s-]?term|medium[\s-]?term|\d+\s*ans?|\d+\s*years?|dur[eé]e|temporalit|placement.*dur|p[eé]riode|holding period/ },
          { key: "risk", patterns: /risque|risk|conservat|agress|defensive|d[eé]fensif|balanced|[eé]quilibr|prudent|dynamique|mod[eé]r[eé]|tol[eé]rance|volatilit|s[eé]curit|protection|drawdown|perte max/ },
          { key: "style", patterns: /value|croissance|growth|dividende|dividend|income|rendement|passif|passive|index|etf|stock.pick|fond|opcvm|buy.and.hold|momentum|quality|contrarian|small.cap|large.cap|mid.cap|blend/ },
          { key: "sectors", patterns: /secteur|sector|tech|sant[eé]|health|[eé]nergie|energy|financ|industriel|pharma|luxe|d[eé]fense|immobilier|reit|consumer|consomm|telecom|mat[eé]riaux|materials|automobile|a[eé]ro|spatial|ia\b|intelligence artificielle|biotech|crypto|minier|utilities|infra/ },
          { key: "geography", patterns: /g[eé]ograph|europe|france|fran[cç]ais|us\b|usa|am[eé]ric|mondial|global|[eé]mergent|emerging|asie|asia|japon|japan|chine|china|international|domestique|zone euro|eurozone|uk|royaume.uni|nordic|scandina|afrique|latam|br[eé]sil/ },
          { key: "constraints", patterns: /pea|cto|compte[\s-]?titre|enveloppe|fiscal|tax|assurance[\s-]?vie|av\b|per\b|[eé]ligib|wrapper|d[eé]duction|imp[oô]t|niche|plafond|versement|retrait/ },
          { key: "goals", patterns: /objectif|goal|retraite|retirement|immobilier|[eé]pargne|patrimoine|wealth|compl[eé]ment.*revenu|rente|capital|ind[eé]pendance|libert[eé]|[eé]tudes|education|enfant|child|succession|h[eé]ritage|achat|maison|house|voyage|projet|s[eé]curit[eé].*financi/ },
        ];
        const covered = aspects.filter(a => a.patterns.test(gl));
        const missing = aspects.filter(a => !a.patterns.test(gl));
        // Don't fire if 5+ aspects are covered
        if (covered.length >= 5) return null;
        const isEmpty = guidelines.length === 0;
        const missingLabels = { horizon: "time horizon", risk: "risk tolerance", style: "investment style", sectors: "sector preferences", geography: "geographic focus", constraints: "account type (PEA/CTO)", goals: "investment goals" };
        const missingList = missing.slice(0, 3).map(a => missingLabels[a.key]).join(", ");
        const msg = isEmpty
          ? "Your portfolio analysis runs without an investment strategy defined. " +
            "I can help you build one through a quick conversation — covering risk tolerance, " +
            "time horizon, sector preferences, and goals."
          : `Your strategy covers ${covered.length}/7 aspects. ` +
            `Missing: ${missingList}${missing.length > 3 ? "..." : ""}. ` +
            "A complete strategy helps me give better-targeted recommendations.";
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
                (guidelines ? `Current guidelines (user wrote): "${guidelines}"\nBuild on these — skip aspects already covered.\n` : "") +
                (missing.length < 7 ? `Already covered: ${covered.map(a => a.key).join(", ")}. Focus on what's missing: ${missing.map(a => a.key).join(", ")}.\n` : "") +
                "Be concise, friendly, use French if the user responds in French. " +
                "One question at a time. Summarize at the end.",
              chatMessage: isEmpty
                ? "Let's define your investment strategy! First — what's your investment horizon? Are you investing for the short term (less than a year), medium term (1-5 years), or long term (5+ years)?"
                : `Your strategy covers ${covered.map(a => a.key).join(", ")} — nice! Let me help fill in the gaps. ${missing.length > 0 ? `First: ${missingLabels[missing[0].key]}?` : "Anything you'd like to refine?"}`
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
