# Changelog

## v0.2.7

### Cash mapping fixes
- **Duplicate cash account names** — when multiple Finary accounts share the same name (e.g. two "Compte espèce PEA"), the mapping now disambiguates by `connection_id` instead of picking the last one
- **Reset cash links** — actually deletes from disk (null-sentinel) and invalidates the cached Finary snapshot so the next sync re-computes mapping from scratch
- **Cash link visible on dashboard** — each account page and run report shows which cash account is linked below the Cash KPI, with click-to-change dropdown
- **Per-link delete in settings** — individual cash links can be removed from the settings panel

### Line memory cleanup
- **Renamed `key_reasoning` → `memory_narrative`** — the self-maintaining analysis memory field now has a clear name across Rust, JS, and UI
- **Removed dead V1 fields** — `llm_memory_summary`, `llm_strong_signals`, `llm_key_history` removed from normalizer, modal, and HTML (never written since V2)
- **Fixed memory/synthesis duplication** — the line detail modal no longer shows the same text in "Memory" and "Synthesis" sections

### Live run UX
- **Running run auto-selected** — sidebar highlights the active run immediately when analysis starts
- **Signal badges on done lines** — completed lines show their actual signal (ACHAT/VENTE/CONSERVER) instead of generic "Done" when recommendation data is available
- **Accurate ETA** — time remaining now accounts for parallelization (divides by in-flight line count)
- **Pipeline bar no longer regresses** — the breadcrumb progress bar only advances forward, never back to "Collecting" when already analyzing

### UI improvements
- **Action cards clickable** — clicking a recommended action card opens the full position detail modal
- **KPI strip in line modal** — position detail modal shows SIGNAL | Qty | PRU | Price | PV% | Total at the top
- **Chat typing indicator** — animated bouncing dots appear while waiting for LLM response in chat wizard
- **Safe preferences save** — `save_user_preferences` uses merge-only semantics; explicit null sentinel for key deletion prevents accidental data loss

### Infrastructure
- **84 Rust + 40 JS tests, 0 failures**

## v0.2.5

- **Q3 Sprint 1** — foundation fixes for Alfred-D
- **V2 line memory in chat context** — signal_history, key_reasoning, price_tracking, news_themes now fully passed through to all chat builders
- **`_PORTFOLIO` filter** — synthetic key excluded from stale positions, run-diff, and theme concentration
- **Run-diff scoped by account** — switching accounts no longer shows stale "What Changed" data
- **76 Rust tests + 161 JS tests, 0 failures, 0 warnings**

## v0.2.4

- **Q2 roadmap complete** — all phases shipped
- **Alfred-B fixed** — accuracy-nudge trigger restored, theme-concentration re-enabled, onChatComplete wired
- **Phase 4b — Onboarding wizard** — chat-based guided setup (portfolio source, LLM backend, first run) in French
- **LLM parsing facade** — `extract_draft_from_response()` handles all backend formats transparently
- **Cash mapping** — wizard saves clean account names, not display text
- **71 tests, 0 failures, 0 warnings**

## v0.2.3

- **Universal CSV parser** — replaces hardcoded broker templates with LLM-driven format analysis. Any CSV format (position snapshot or transaction history) is auto-detected via AI, with per-column regex patterns for robust value extraction. Cached after first use for instant repeat imports.
- **Fix: Revolut CSV import** — amounts prefixed with currency codes (e.g. `USD 235.56`) now parse correctly.

## v0.2.2

- **Chat quality overhaul** — V2 line memory fields (signal history, key reasoning, price tracking, trends, themes) now injected into all chat context builders. Position, action, and synthesis chats all have richer, more accurate context
- **Accuracy nudge** — Alfred proactively alerts when a recommendation signal has aged badly (price moved 10%+ against the signal direction). Top 2 worst per run, 24h per-ticker cooldown
- **Mid-run overlay commentary** — Alfred comments on analysis progress every few positions ("Analyzed 8/15... just finished AAPL")
- **ETA during analysis** — "Analyzing 8 of 15 positions (~3 min remaining)" based on rolling per-position timing
- **Cash matching dropdown** — replaced verbose LLM text wizard with a clean dropdown UI. Pre-selects heuristic match, "No cash account" option, optional "Ask Alfred" fallback for edge cases
- **Persistent cash dismiss** — accounts with no cash mapping are remembered across sessions (no more re-prompting)
- **Export to Obsidian/Drive** — export analysis results as markdown with YAML frontmatter, action table, and positions table. Saves to `data/exports/`
- **Queued status chips** — positions show "Queued" immediately on run start instead of blank screen
- **Action cards → line modal** — click any recommended action badge to open the full position detail modal
- **Theme concentration top-5** — themes sorted by relevance, capped at top 5 with "Show more" toggle + "Ask Alfred" button
- **Persist error toasts** — error notifications stay visible until manually dismissed (no more 5-second auto-dismiss)
- **onChatComplete wiring** — overlay chat sessions now properly fire completion callbacks (strategy refinement results saved)
- **Stale run diff fix** — "What Changed" panel now clears correctly on account switch
- **QA critical fixes** — 10 `unwrap()` calls replaced with safe alternatives, `doneHandled` race fixed, dead code removed
- **148 JS + 4 Rust unit tests** — full test coverage for context builders, accuracy nudge logic, live run, theme concentration, cash sentinel

## v0.2.1

- **Stale position alerts** — sidebar badge + overdue markers when positions need reanalysis
- **Theme concentration risk** — detects when 3+ positions share a news theme, warns in synthesis and UI
- **Signal scorecard** — "Was I Right?" accuracy tracker per position in the detail modal
- **Run diff view** — "What Changed" summary at top of report (signal upgrades/downgrades, price moves)
- **Alfred overlay** — proactive assistant infrastructure (trigger system, idle detection, panel renderer)
- **Chat drill-down** — "Discuss about it" button now at top of position modal (always visible)
- **Unified view architecture** — single data source for all rendering paths, no more display glitches when browsing
- **Cash mapping fixes** — 5 bugs fixed: semantic name matching, pre-run wizard timing, save persistence feedback
- **Update modal** — proper centered dialog replaces the old inline banner
- **Cache race fix** — synthesis results no longer lost when MCP server and main process write concurrently
