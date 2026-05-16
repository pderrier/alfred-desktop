# Changelog

## v0.3.3

Six items shipped in one bundle across four worktrees: scorecard correctness, rate-limit hardening, LLM post-processing, line-memory sync fixes. No mandatory upgrade flag — real-provider QA gate (P3-15/P3-16) must pass before flipping `mandatory: true`.

### P0 fixes
- **Scorecard truly accurate** — `run_get_signal_scorecard` now scores CONSERVER correctly (price-vs-anchor band → correct/incorrect/neutral), treats SURVEILLANCE as a watch signal (icon 👁️, raises `flag: watch_missed_drop` if return falls below -5%), and marks recent signals (<5d, |return|<2%) as `pending` instead of incorrect. New helper `resolve_current_price` extracted as a pure 4-tier price chain (`prix_actuel` → `market.spot` → derived from `technicals.high_52w × (1 + current_vs_high_52w_pct/100)` → last-known-good with `stale: true`) and never writes `0` to `price_tracking.current_price`. Contract tests pin `scorecard.current_price == line_modal.header_price == portfolio.positions[i].prix_actuel`. JS column "Return" renamed to "Price drift" with tooltip. **Tests**: 13 new (4 scorecard + 5 sync + 2 contract + 2 bonus).
- **alfred-api rate-limit sized for real portfolios** — `RATE_LIMIT_RPM` default raised 60 → 600 (`DEFAULT_RATE_LIMIT_RPM` const in `apps/alfred-api/src/config.rs`); pure decision helper `evaluate_rate_limit` extracted from `check_rate_limit` so the "50 tickers × 4 endpoints does not trip default" contract is testable without Redis. Desktop side adds `ApiGetOutcome` enum + `api_get_with_retry(fetcher, sleeper)` pure helper in `alfred_api_client.rs`: max 3 retries (500ms / 1500ms / 4500ms backoff), `Retry-After` header honoured and clamped at 30s. `deploy/alfred-api.env.example` documents the value with incident history. New pre-release real-provider QA checklist in `docs/update-manifest-and-mandatory-upgrade.md` blocks `mandatory: true` until a 28-ticker portfolio reaches ≥90% technicals coverage with 0 rate-limit errors. **Tests**: 11 new (6 server + 5 desktop).

### LLM post-processing (P2-6 + P2-7)
- **Acquisition signal without fundamentals → conviction degraded + UI badge** — `mcp_server::tool_validate_recommendation` now runs `enforce_data_quality_guards`: if `signal ∈ {ACHAT, ACHAT_FORT, RENFORCEMENT}` and `analyse_fondamentale` is empty or contains `"indispon"`, the payload gets `data_quality: "fundamentals_missing"` and `conviction` is downgraded to `"degradee"`. CONSERVER and SURVEILLANCE are exempt (no new capital committed). The signal itself is never modified — the thesis can remain valid from memory. UI surfaces `⚠ données partielles` badge in the line modal via the new `updateDataQualityBadge` helper (`line-modal-helpers.js`). Single MCP hook covers all 3 LLM modes (codex / native / native-oauth) per the parity contract.
- **Action prices backfilled from rationale** — new `services/llm_post_processing.rs` module exposes `backfill_action_immediate(action)`: when `limit_price` is null but the `rationale` contains an EUR-denominated number (French comma + decimal point + €), the price is extracted via a regex that requires an `EUR` / `€` suffix (so bare quantities like "1000 titres" are not misread). When `limit_price × quantity` is computable but `estimated_amount_eur` is null, the product is filled. Hooked into both `mcp_server::tool_validate_synthesis` (MCP path) and `report::persist_retry_global_synthesis` (codex retry + native fallback) — idempotent on both call sites. System prompts (`llm_prompts.rs`, `services/native_mcp_analysis.rs`) reinforced with "TU DOIS peupler limit_price + estimated_amount_eur". **Tests**: 15 Rust + 8 JS new.

### Line memory sync (P1-4 + P1-5)
- **`key_reasoning` persisted for every ticker including .PA suffixed** — fix for the 132/150 tickers that had `key_reasoning = null` post-rename. `sync_line_memory` now writes via a 3-tier fallback: `rec.key_reasoning` → `extract_first_sentences(synthese, 3)` → prior value (a degraded run with empty synthese never wipes a captured thesis). Synthetic keys like `_PORTFOLIO` and unsuffixed tickers like `AAPL` continue to work (non-regression tests). **Tests**: 4 new.
- **`price_at_signal` guard + lazy migration of legacy zero anchors** — addresses the 341/723 historical signals contaminated by the prior v0.3 `current_price=0` outage. Two mechanisms in `sync_line_memory`: (a) write guard skips the `signal_history` prepend entirely when no usable price is available — non-scorable rows would pollute the history; (b) `migrate_zero_price_at_signal_entries` walks the prior history and backfills any entry with `price_at_signal <= 0` using the run's current price as a best-effort proxy, tagged `"price_at_signal_source": "migration_proxy"` for audit. Idempotent (only touches `<=0` entries) and runs per-ticker per-sync (no separate startup task). A contaminated ticker repairs itself the next time it's analysed. **Tests**: 5 + 1 contract.

### Tests / infrastructure
- **alfred-desktop Rust**: 218 → **260 cargo tests** (+42 across the 4 worktrees A/B/C/D).
- **alfred-api Rust**: 113 → **119 cargo tests** (+6 rate-limit + config contract).
- **alfred-desktop JS**: 278 → **286 tests** (+8 data-quality badge, wired into `npm test`).
- **Zero new clippy warnings** on touched files across both crates (pre-existing baseline of 76 desktop + 20 alfred-api unchanged — tracked in P3-28).

### Known caveats (tracked as P3)
- P3-24..30 cover follow-up tests (UI scorecard render, regex bare-quantity, `_PORTFOLIO` prod-writer path, `migration_proxy` UX surface), one cleanup (`persist_deep_news_summary` 8-arg refactor), and one process gate (real-provider QA pass before `mandatory: true`).

---

## v0.3.2

**Mandatory upgrade.** v0.3.2 fixes 6 production bugs and ships 3 UX improvements in a single batch.

### P0 bug fixes
- **Technical-snapshot coverage 10/28 → ≥90%** — `fetch_technical_snapshot` now wraps each call in a 3-attempt retry-with-backoff (500 ms / 1500 ms) so transient Yahoo rate-limit spikes no longer drop tickers silently. Emits `alfred://ticker-collection-event` so the UI can show per-ticker retry/failure status (see Retry visibility below).
- **Run-narrator no longer silent** — first-tick LLM timeout raised from 8 s to 15 s (heavy initial narration call), subsequent ticks from 8 s to 10 s, failure-degrade threshold raised 3 → 10. When the narrator does enter degraded mode it emits `alfred://run-narration-status` and the run-in-progress card shows an amber « narration: mode dégradé » badge.
- **Signal Accuracy scorecard now visible** — `run_get_signal_scorecard` reads `by_ticker` via the new `resolve_line_memory_key()` helper which checks the canonical Yahoo symbol first and falls back to the raw ticker. Closes the v0.3.0 regression where line-memory keyed by canonical symbol was invisible to scorecard readers keyed by raw ticker.
- **Canonical-key audit guard** — `resolve_line_memory_key()` is the single shared reader for every `by_ticker[...]` access (command_handlers, mcp_server, native_mcp_analysis). New lint-as-test `lint_no_raw_by_ticker_lookup_outside_helpers` walks the source tree and asserts no raw `.get(&ticker)` survives outside the helper, preventing the 3rd same-class regression.
- **Settings → Storage panel works again** — three Tauri handlers (`storage_usage_local`, `storage_prune_local`, `storage_clear_log_local`) now wrap their result in the canonical `{ ok, action, result }` bridge envelope. New `bridge_envelope()` helper + `assert_bridge_envelope` test pin the convention.
- **Per-ticker retry/failure badge** — the run-in-progress card surfaces a live aggregator badge (`⚠ 3 retries · ✗ 1 échec`) consuming the new `alfred://ticker-collection-event` stream. Aggregator is pure (`ticker-collection-status.js`), reset is `run_id`-driven, and the listener no-ops without an active run.

### UX improvements
- **Granular early-run stages** — `finary_fetching` / `snapshot_received` / `enriching_market` Rust stages drive the new `alfred-run-stage-toast` overlay trigger and progressive « Récupération Finary… / Snapshot reçu (N) / Cours marché (C/T)… » text in the Market Synthesis card, so the user no longer stares at a blank pipeline bar for the first 5–10 s of a run.
- **Notes & Discussion in the line modal** — `app-line-modal.js` now reads `getDiscussionThreads` and renders the 5 most recent threads, clickable to reopen the conversation. Pure helper `line-modal-helpers.js` covers the formatting logic.
- **Token cost in the run report** — `report-view-model.js` computes per-mode token usage (codex / native / native-oauth) and surfaces a footer with the per-run €/EUR cost and the model that responded. Backed by `build_token_usage_view`.

### Tests / infrastructure
- **alfred-desktop Rust**: 188 → **218 cargo tests** (+30 across the 5 worktrees A/B/C/D/E + 6 retry/event contract tests in worktree A).
- **alfred-desktop JS**: 244 → **278 tests** (+11 ticker-collection-status aggregator/listener/glyph, +12 quick-wins line-modal & token usage, +11 run-stage context).
- **Flaky parallelism test fixed** — `native_collection_runs_enrichment_with_configured_parallelism` was flaky at 12/30 (40%) under WSL2 CPU contention. Two root causes: (1) `enrichment::fetch_resolved_symbol` was making real HTTPS calls to `/api/resolve` bypassing the in-test mock; fixed by setting `ALFRED_API_ENABLED=0` in `env_lock()` — all 44 tests using the lock are now hermetic w.r.t. the API client. (2) The `>= 2` concurrency assertion relied on OS-scheduling overlap; replaced with an opt-in `CollectionSyncPoint` (Condvar+Mutex, 5 s timeout) that both workers must reach before either proceeds. Post-fix: 30/30 pass under `yes >/dev/null` triple contention.
- **Contract tests added** for: technicals retry happy path, retry-then-fail, event payload, scorecard canonical lookup, canonical lint guard, bridge envelope shape on the three storage handlers.

---

## v0.3.1

**Mandatory hotfix.** Single bug, immediate ship.

- **Persist whitelist now includes `technicals`** — `persist_native_collection_state` was emitting all `build_collection_state` fields except `technicals`, so server-computed 250-day technical indicators reached the run JSON but never the LLM prompt. Fixed by adding `technicals` to the whitelist and adding a permanent contract test (`persist_whitelist_covers_all_build_collection_state_fields`) that diffs emitted vs persisted top-level keys.

---

## v0.3.0

### Technical-snapshot pipeline maturity
- **ISIN→canonical Yahoo symbol resolver** — 180d positive / 15s negative Redis cache. Covers all geographies tested live (US/UK/DE/IT/JP/HK/AU/IN/BR/IE/multi-listed NL/crypto). See `docs/technical-snapshot.md`.
- **`GET /api/resolve?isin=X` public endpoint** — single source of truth for canonical symbol resolution across the enrichment API.
- **Yahoo spot price as `/market` fallback** — fills tickers where Boursorama/AlphaVantage/Google return `source=none`.
- **`canonical=` query param on `/news`, `/sector`, `/cot`** — improves upstream relevance for EU/Asia equities.
- **Desktop collection pipeline wired** — cross-account dedup via canonical symbol; line modal displays venue (e.g. "STMPA · Paris").

### Run-narration v2
- **Progress anchoring** — `alfred-analysis-progress` data (X/Y + latest ticker) injected into the LLM prompt for factual anchoring (`build_narration_prompt` gains `Option<&ProgressSnapshot>`).
- **Narrative continuity** — `last_narration` stored in `NarratorState`; fed back as « Précédent résumé » on the next tick so the LLM builds a thread instead of restarting.
- **Ticker/agent disambiguation** — explicit prompt instruction prevents the LLM from treating uppercase ticker codes as agents. See `docs/run-narration-feature.md` (Iteration 2).

### Operations
- **Mandatory upgrade** — clients ≤ v0.2.18 will fail HMAC authentication after the secret rotation included in this release. See `docs/update-manifest-and-mandatory-upgrade.md`.

### Tests / infrastructure
- **alfred-api**: 90 → **113 cargo tests** (+23: 6 for `/api/resolve` shape, 9 for Yahoo spot fallback, 14 for canonical query helpers). Zero new warnings.
- **alfred-desktop Rust**: 170 → **185 cargo tests** (+15: resolved_symbol plumbing, canonical-key line-memory dedup, venue mapping).
- **alfred-desktop JS**: 229 → **244 tests** (+15: 21 venue-suffix mappings, byte-identical legacy layout pin, watchlist canonical resolution).
- **Negative caching capped at 15 seconds** across `yahoo_symbol-neg`, `ohlc-neg`, `technicals-neg` (was 24h / 1h / 1h). Pinned by `*_ttl_is_15_seconds` tests. Flood protection only — production incident on 2026-05-15 showed a 1h negative cache blocked a freshly-deployed fix from surfacing.
- **`deploy/deploy-alfred-api.sh`** merges `~/.alfred-secrets.env` (mode 600, gitignored, HOME-resident) into the VPS env file before SCP — closes the loop where each deploy was silently wiping `ALFRED_API_SECRET`.
- **CHANGELOG backfilled** for v0.2.8–v0.2.18 (10 versions) from git history; README features list updated.

---

## v0.2.18

### Fixes
- **PEA PME ISIN routing fix** — desktop was calling `/api/market/technicals?ticker=X` with no ISIN. French small/mid-caps (EXA, LBIRD, VETO, …) have short US-shape tickers that the server routed to Yahoo without the `.PA` exchange suffix, failing systematically (`yahoo_no_timestamps`). Desktop now sends the ISIN; server maps country prefixes to Yahoo suffixes.
- `alfred_api_client::remote_fetch_technicals(ticker, isin)` — new signature; `build_technicals_path()` pure helper with 4 unit tests. Empty/whitespace ISIN degrades to ticker-only gracefully.
- `enrichment::fetch_technical_snapshot(ticker, isin)` — forwards ISIN, root-cause doc-comment added.
- `native_collection_dispatch::process_collection_task` — passes row ISIN (empty string normalised to None).

### Infrastructure
- **168/168 cargo tests pass** including 4 new path-builder tests; zero new warnings.
- See `docs/technical-snapshot.md` and `docs/update-manifest-and-mandatory-upgrade.md`.

## v0.2.17

### Features
- **Technical snapshot pipeline** (`docs/technical-snapshot.md` — Phase 0→2):
  - Desktop Rust wires `/api/market/technicals` via `alfred_api_client::remote_fetch_technicals()`. Silent `None` on 404/network/missing `indicators` — a rollout-pending server endpoint never breaks runs.
  - `TechnicalSnapshot` / `TechnicalIndicators` / `Macd` structs (all fields `Option<f64>` for partial data).
  - `CollectionResult` gains `technical_snapshot` field; `process_collection_task()` fetches it sequentially after market/news.
  - `tool_get_line_data` (MCP) exposes `technical_snapshot` + `collection_quality` (per-block `{source, as_of, quality}`) — additive, snapshot UI contract preserved.
  - **TECHNIQUE section in 4 prompt builders** — `build_technical_section()` shared across all 3 LLM modes (parity contract honored). Per-indicator French interpretation hints (RSI, MACD hist, ATR, 52w hi/lo, trend). `build_watchlist_prompt` intentionally exempt (no snapshot available for unowned tickers).
  - **UI badges + TECHNIQUE rows** — "Données collectées" panel gains 7-row per-block badge grid (Spot/Fondamentaux/Technique/News/Secteur+COT/Insights/Memoire) + 6 TECHNIQUE indicator rows (vs SMA200, RSI, MACD hist, ATR, 52w hi/lo, tendance). Backward-compatible fallback when `collection_quality` absent.

- **Run-narration** (v1):
  - Rust `run_narrator` module — per-run SSE ring buffer (cap 200, oldest evicted) + 10s LLM tick loop. Emits `alfred://run-narration`; 3 consecutive failures degrade to silent mode.
  - Reuses `llm_backend::run_prompt` — parity across codex/native/native-oauth.
  - Frontend wires `alfred://run-narration` → Alfred overlay `notify("run-narration", { message })`. `alfred-analysis-narration` trigger (priority 3, cooldown 9s) registered alongside the existing `alfred-analysis-progress` fallback.

### Fixes
- **Data-quality sprint** (independent; included in this release):
  - Position price sync — `sync_position_from_market()` updates PV/MV/Price only when `market.source` is a real provider (not PRU fallback).
  - Watchlist persist-before-LLM — `apply_watchlist_collection_result()` persists collection state before MCP dispatch so `get_line_data` sees market data when analyzing watchlist items.
  - Line-memory zero-price repair — `repair_line_memory_zero_prices()` flags contaminated tickers with `price_data_unavailable=true`; auto-cleared when a real price returns.
  - Signal history dedup — `sync_line_memory` replaces the head entry when (date, signal) matches; `scored_count` counts distinct dates; `dedupe_signal_history_by_date_signal` shared between Rust prompt context and the UI widget.
  - Source-bypass chain — `apply_pru_fallback_to_market_row` now sets `source=none` honestly; `extract_market_price_from_run_state` returns 0.0 when source=none.
  - UI `0€ / -100%` rendering — `isPriceUnavailable(pos)` heuristic renders "—" for unavailable prices; tooltip "Prix indisponible — tous les fournisseurs ont échoué".
  - Boot blocking I/O — zero-price repair wrapped in `spawn_blocking`; first-paint never waits on line-memory.json scan.

### Infrastructure
- **229 JS + 150 cargo tests pass** (was 129 Rust pre-sprint); +25 Rust unit tests (Fix 1/3/4/5/P1/P2), +79 JS (collection quality, technical indicators, freshness).

## v0.2.16

### Features
- **OAuth-availability proposal banner** — when the user is on a paid path (native mode or codex with swapped API-key auth) and the OAuth ChatGPT Plus subscription is detected as available, a non-blocking banner proposes switching back. Three actions: Switch, Later (7-day snooze), Don't ask (permanent).
- Decision matrix in `codex-fallback-policy.js::decideOauthProposal()` — pure function shared with test suite. Banner is skipped if dismissed/permanent (saves a token probe on launch).
- `codex.rs::has_oauth_backup()` + Tauri command `codex_has_oauth_backup_local`; two new runtime settings (`oauth_proposal_dismissed_until_ms`, `oauth_proposal_permanently_dismissed`).

### Infrastructure
- **Rust 129 tests, JS 195 tests** (+13 JS for `decideOauthProposal` decision matrix + dismiss + back-compat).

## v0.2.15

### Features
- **gpt-5 default model** — `DEFAULT_MODEL` updated to `gpt-5`; `resolve_best_model()` cached via `OnceLock` for process lifetime (was re-fetching `/v1/models` on every line analysis); `/v1/models` timeout raised to 30s; debug-log on every failure path.
- **Non-blocking run display** — `has_partial_artifacts` logic fixed: previously returned `true` for any run with artifacts (labeling successful runs as "Partial"). Now: partial iff artifacts present AND orchestration status is running/failed/aborted or synthesis is missing.
- **Synthesis validation warnings surfaced** — `validation_warnings` / `validation_warning_count` exposed in `build_run_summary`; `renderValidationWarnings` draws a non-blocking ⚠ notice on the synthesis card with collapsible details. Always shows synthesis — warnings are visibility, not a gate.

### Infrastructure
- **Rust 110→126 (+16 tests), JS 182 tests** — `pick_best_model` 6 unit tests, `has_partial_artifacts` 10 unit tests.

## v0.2.14

### Features
- **Codex intra-mode auth fallback** — new splash probe `probe_codex_quota_local` runs a real `codex exec "OK"` (~3s) to detect rate-limit. On quota exhaustion: if an OpenAI API key is saved, swaps `codex` auth to API key silently (auth.json backed up to `auth.json.oauth.bak`); on restore, throttled to 6h backoff via `codex_auth_oauth_retry_after_ms`.
- New Rust commands: `codex_auth_mode_local`, `swap_codex_to_apikey_local`, `swap_codex_to_oauth_local`. Decision matrix: `codex-swap-to-apikey`, `codex-reveal-key-input`, `codex-restore-oauth`.
- **Splash status granularity** — users see actual phases during the ~10s splash: Reading settings → Checking Codex availability → Refreshing Finary session → Syncing portfolio → Updating dashboard → Running health check.

### Infrastructure
- **Rust 102→110 (+8), JS 173→182 (+9)** — probe parser, auth_mode reader, swap round-trip, classifier stderr cases; codex decision matrix.

## v0.2.13

### Features
- **Codex OAuth → API key auto-fallback at splash** — when native-oauth is rate-limited and an `openai_api_key` is saved, switches to native mode silently with informational toast; persists `llm_backend_auto_fallback=1`. On next launch, auto-restores OAuth when quota is back (any manual backend change clears the flag).
- `classify_session_failure()` in `codex.rs` exposes `failure_reason` field; `decideFallback()` in `codex-fallback-policy.js` is the single pure source of truth shared with tests.

### Fixes
- `alfred-cli`: include `agentos_artifacts` module via `#[path]` (build fix).

### Infrastructure
- **Rust 98→102 (+4), JS 159→173 (+14)** — classification tests, fallback decision matrix (swap, reveal, restore, retry-throttle no-op, back-compat).

## v0.2.12

### Cross-account context dans la synthèse par compte
- **Snapshot enrichi (LLM only)** — nouveau champ `snapshot.holdings_accounts` exposant tous les comptes Finary (investment, cash_only, liability, other) avec `kind` structurel, `institution_provider_categories` brut, cash multi-devises (`cash_by_currency`)
- **`snapshot.portfolio_summary`** — agrégats `value_by_kind` et `value_by_institution_provider_category` portfolio-level
- **`run_state.cross_account_context`** — résumé compact des autres comptes (top positions, cash mobilisable, kind) + thèmes cross-account injecté dans `build_synthesis_prompt` ET `build_report_prompt` (parité 3 modes LLM)
- **Section "Contexte cross-account"** dans les prompts — instruction explicite au LLM de rester centré sur le compte cible, et d'utiliser le contexte uniquement pour : éviter de vendre quand du cash existe ailleurs (Livret, compte courant), éviter la redondance de positions cross-account, cohérence thématique
- **Thèmes cross-account agrégés à prompt-build-time** depuis `line-memory.json` (filtrés à ≥ 2 comptes)
- **Contrat UI préservé** — `snapshot.accounts` reste byte-identique, pinné par `test_snapshot_accounts_ui_contract_unchanged`

### Infrastructure
- **98 tests Rust (87 → 98, +11 nouveaux), 0 failure, 0 warning nouveau**
- Nouveaux helpers dans `native_collection_helpers.rs` : `build_holdings_metadata`, `classify_holding_kind`, `build_portfolio_summary`, `aggregate_cash_by_currency`, `build_cross_account_context`
- Docs : `docs/finary-snapshot-schema.md` et `docs/synthesis-pipeline.md` mis à jour

## v0.2.11

### Internal
- **CI version-bump portability** — `bump-version.sh` sed edits now macOS-compatible (BSD sed `-i ''` syntax).

## v0.2.10

### Internal
- **CI version-bump robustness** — pass `github.ref` expression directly to the tag sync step (fixes indirect variable expansion in CI context).

## v0.2.9

### Internal
- **CI version tagging** — sync project versions from release tag before build using direct `github.ref` expression.

## v0.2.8

### Features
- **LLM-authored memory narrative** — during line analysis, the LLM now writes the `memory_narrative` field directly, creating a living analysis memory that evolves across runs.
- **Evolving deep-news memory** — deep-news analysis is now LLM-authored and accumulates across runs rather than being overwritten each time.

### Fixes
- **Monotonic run progress bar** — the top run progress bar now only advances forward per active run, never regresses to an earlier step.
- **Signal scorecard price parsing** — fixed accuracy and price parsing in the "Was I Right?" scorecard widget.

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
