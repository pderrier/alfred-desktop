<p align="center">
  <img src="src/desktop-shell/alfred-splash.png" alt="Alfred" width="280" />
</p>

<h1 align="center">Alfred Desktop</h1>

<p align="center">AI-powered portfolio analysis for retail investors.<br/>Connect your brokerage data, enrich it with market data and news, and get actionable investment recommendations powered by AI.</p>

## Download

**[Windows installer (MSI)](https://vps-c5793aab.vps.ovh.net/alfred/release/windows/latest)** — Alfred Desktop v0.2.6 for Windows 10/11 (x64)

> Note: The installer is not code-signed yet. Windows SmartScreen may show a warning — click "More info" → "Run anyway" to proceed.

**[macOS installer (DMG)](https://vps-c5793aab.vps.ovh.net/alfred/release/macos/latest)** — Alfred Desktop v0.2.6 for macOS 10.15+ (Apple Silicon)

> **Note: The DMG is not notarized yet.** macOS Gatekeeper will block the app on first launch. Three ways to bypass it:
>
> 1. **Right-click → Open** on the app in Finder, then click "Open" in the dialog. Simplest — works on most setups.
> 2. **System Settings → Privacy & Security → Open Anyway** — after a blocked launch attempt, a button appears at the bottom of the Privacy & Security pane.
> 3. **Terminal (power users):** `xattr -cr "/Applications/Alfred Desktop.app"` — strips the quarantine attribute entirely; no dialog needed after that.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for the full version history.

## Features

- **Portfolio sync** — connect Finary for automatic brokerage account sync, or import CSV exports (universal LLM-driven parser handles any broker format)
- **Market enrichment** — real-time prices, fundamentals, and news from multiple sources
- **AI analysis** — per-position technical, fundamental, and sentiment analysis powered by OpenAI
- **Dual LLM backend** — choose Codex (free tier, OAuth) or native OpenAI API (API key, pay-per-use)
- **Web search** — the AI searches and reads web pages during analysis to find missing data
- **Synthesis report** — portfolio-wide recommendations with conviction levels and action items
- **Chat with Alfred** — drill down on any position, action, or the whole portfolio; "Discuss about it" from any card. Insights can be saved to memory and reused in the next analysis
- **Save to Memory** — persist key reasoning, personal notes, and news themes per position; carried forward into future runs for richer context
- **Alfred overlay** — proactive assistant that surfaces welcome messages, accuracy nudges, run-completion summaries, and errors with priority-based preemption
- **Accuracy nudge** — Alfred flags when a past recommendation has aged badly (price moved 10%+ against the signal)
- **Signal scorecard** — "Was I Right?" accuracy tracker per position, based on signal history
- **Run diff / "What Changed"** — highlights signal upgrades/downgrades and price moves between consecutive runs
- **Theme concentration risk** — detects when 3+ positions share a news theme and warns in the synthesis and UI
- **Stale position alerts** — sidebar badge and overdue markers when positions need reanalysis
- **Onboarding wizard** — chat-based guided setup on first launch (portfolio source, LLM backend, first run)
- **Export to Obsidian/Drive** — export analysis results as markdown with YAML frontmatter, action table, and positions table
- **Watchlist** — AI-suggested positions to monitor based on your portfolio profile
- **Auto-update** — checks for new versions on startup with optional mandatory updates
- **Multi-account** — manage and analyze multiple brokerage accounts independently

## Required accounts

Alfred relies on two external services:

### OpenAI (required)

Alfred uses OpenAI models for AI-powered analysis — technical, fundamental, and sentiment analysis with actionable recommendations per position. An OpenAI account is required.

Two LLM backends are available (choose on first launch or in Settings):

| Backend | Auth | Cost | How it works |
|---|---|---|---|
| **Codex** (default) | OpenAI OAuth sign-in | Free tier available | Uses the [Codex CLI](https://github.com/openai/codex) app-server (bundled in installer). Generic context management. |
| **Native API** | OpenAI API key | Pay-per-use | Calls the [Responses API](https://platform.openai.com/docs/api-reference/responses) directly. Optimized for lower token usage — tools execute as native Rust calls (no IPC overhead), position-aware context windowing avoids redundant data. |

The native backend auto-detects the best available model (gpt-5.x > gpt-4.1 > o4 > o3), supports native web search (the model reads web pages to find missing data), and works with custom API endpoints (for proxies or compatible providers). It also supports reasoning model streaming (o-series thinking tokens shown in the UI).

Sign up at [platform.openai.com](https://platform.openai.com/signup). For the Codex backend, Alfred handles everything — it installs the CLI if needed and prompts you to sign in. For the native backend, generate an API key at [platform.openai.com/api-keys](https://platform.openai.com/api-keys) and enter it in the app.

### Finary (optional)

[Finary](https://finary.com) is a wealth management platform that connects to over 20,000 banks, brokers, and crypto platforms worldwide and provides a unified view of your holdings (stocks, funds, real estate, crypto, and more). If you have a Finary account, Alfred can sync your brokerage data automatically — no manual CSV export needed.

Without Finary, you can still use Alfred by importing CSV exports from your broker. The CSV parser uses a three-tier strategy: it first tries to detect known formats (Boursorama), then applies heuristic column matching for common header names, and finally falls back to LLM-assisted column mapping for unknown formats. In theory, this should handle any broker's CSV export — but it hasn't been tested with every format. If you encounter a CSV that doesn't parse correctly, please [open an issue](https://github.com/pderrier/alfred-desktop/issues) or contact the author.

## Architecture

Alfred Desktop is a [Tauri 2](https://v2.tauri.app/) application:

- **Frontend** — vanilla JavaScript with a custom shell UI (no framework)
- **Backend** — Rust (Tauri commands, async analysis worker, native enrichment)
- **LLM** — dual backend: [Codex CLI](https://github.com/openai/codex) app-server OR native OpenAI Responses API
- **Data** — local JSON files for run state, SQLite for structured data
- **Remote API** — market data and news enrichment (see below)
- **Auto-update** — version manifest check on startup, mandatory/optional update flows

```
src/
  desktop-shell/    # Main UI (HTML + vanilla JS)
  shared/           # Tauri IPC bridge + shared utilities
src-tauri/
  src/              # Rust backend
    main.rs         # Tauri app entry point + command handlers
    llm_backend.rs  # Backend dispatcher (Codex or native)
    openai_client.rs # Native OpenAI Responses API client + tool-use loop
    codex.rs        # Codex app-server client (JSON-RPC over stdio)
    llm.rs          # LLM generation (line analysis, synthesis, watchlist)
    mcp_server.rs   # 10 analysis tools (data fetch, validation, persistence)
    updater.rs      # Auto-update mechanism (manifest check, download, install)
    finary.rs       # Finary connector (CDP browser automation)
    enrichment.rs   # Remote API client for market data + news enrichment
    services/       # Native collection, enrichment, MCP analysis
  Cargo.toml        # Rust dependencies
  tauri.conf.json   # Tauri configuration
```

## Market Data API

Alfred Desktop connects to a remote API server that provides market prices, fundamentals, news, and shared analysis insights. The API handles all data collection and caching server-side.

**A hosted instance is provided by default and used transparently — no setup needed.**

The API server itself is not included in this repository (separate private service, Rust/Axum + Redis + SearXNG). Without it, the app cannot fetch market data or news, and the LLM analysis would lack context to produce useful results.

### Privacy — what is sent to the API

**No personal data is ever sent.** Alfred **does not** transmit your portfolio positions, quantities, account names, balances, or any financial details about you. The API only receives public market identifiers.

Data **sent** to the API (all public market data, no personal information):

| What | Example | Why |
|---|---|---|
| Ticker symbols | `MC`, `AAPL` | Fetch cached market prices and fundamentals |
| ISIN codes | `FR0000121014` | Disambiguate tickers across markets |
| Company names | `LVMH` | Improve search relevance |
| LLM-extracted fundamentals | `{ pe_ratio: 25.3 }` | Cache public financial metrics to avoid recollecting |
| Generic analysis snippets | `"Technical: RSI oversold..."` | Share generic market insights (not related to your portfolio) across users |
| News article summaries | `{ url, summary, quality_score }` | Cache article summaries to avoid re-reading |

Data **never sent**:

- Portfolio positions, quantities, or weights
- Account names, balances, or cash amounts
- Purchase prices, gains/losses, or transaction history
- Investment guidelines or personal preferences
- Any user-identifiable information (see auth section below)

**Your credentials never leave your device.** Your OpenAI JWT token and your Finary session cookies stay local — they are **not** transmitted to the Alfred API. Authentication uses an HMAC signature derived from a build-time secret. A one-way hash of your local session is sent as an opaque identifier for rate-limiting only; it cannot be reversed to recover your tokens or identity.

### Important note about third-party data sharing

**OpenAI** receives your full portfolio data (positions, quantities, prices, account names) as part of the LLM analysis prompts — this is inherent to how AI-powered analysis works. Your data is subject to [OpenAI's usage policies](https://openai.com/policies/usage-policies). If you use Finary as a data source, Finary already has access to your brokerage data through their own platform.

### Configuration

The API is **enabled by default** and points to a hosted instance. No setup is required — it works out of the box.

| Variable | Default | Description |
|---|---|---|
| `ALFRED_API_URL` | hosted instance | Override to point to a self-hosted API |
| `ALFRED_API_ENABLED` | `true` | Set to `0` to disable remote API calls (not recommended — analysis will lack market context) |

### Self-hosting the API

If you want to run your own enrichment API, you'll need:
- A Redis instance (caching)
- A SearXNG instance (web search)
- The API server (Rust/Axum, not open-source — contact the author)

Set `ALFRED_API_URL` to your instance URL once deployed.

## Prerequisites

**End users:**
- **Windows 10/11 (x64):** install the MSI package. SmartScreen may warn — click "More info" → "Run anyway".
- **macOS 10.15+ (Apple Silicon / arm64):** mount the DMG, drag Alfred to Applications. Gatekeeper will block the first launch — see the note under the download link above for bypass instructions.
- No additional dependencies in either case. The Codex CLI is bundled in the installer; for the native API backend, only your OpenAI API key is needed.

**Developers:**
- [Node.js](https://nodejs.org/) 18+ (for Tauri CLI and Codex bundling)
- [Rust](https://rustup.rs/) 1.75+
- An OpenAI account (API key or Codex access)
- Native system libraries required by Tauri/WebKit:
  - **Ubuntu/Debian:** `sudo apt-get update && sudo apt-get install -y pkg-config libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev`
  - **Fedora:** `sudo dnf install -y pkgconf-pkg-config glib2-devel gtk3-devel webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel`
  - **Arch:** `sudo pacman -S --needed pkgconf glib2 gtk3 webkit2gtk libappindicator-gtk3 librsvg`

## Getting started

```bash
# Install dependencies
npm install

# Run in development mode
npm run dev

# Run Rust tests
cargo test --manifest-path src-tauri/Cargo.toml

# Build for production (Windows)
powershell -ExecutionPolicy Bypass -File scripts/prepare-codex-bundle.ps1
npm run build:windows

# Build for production (macOS)
bash scripts/prepare-codex-bundle.sh
npm run build:macos
```

If `cargo test` fails with:

`The system library glib-2.0 required by crate glib-sys was not found`

install the Linux packages above (notably `pkg-config` + `libglib2.0-dev` on Debian/Ubuntu), then retry.

## Configuration

Alfred stores data in `<exe_dir>/data/` (production) or `src-tauri/../data/` (development).

| Variable | Default | Description |
|---|---|---|
| `ALFRED_DATA_DIR` | auto-detected | Override data directory |
| `ALFRED_STATE_DIR` | `<data>/runtime-state` | Run state files |
| `ALFRED_REPORTS_DIR` | `<data>/reports` | Report artifacts |
| `ALFRED_DEBUG_LOG_PATH` | `<data>/debug.log` | Debug log file |
| `ALFRED_API_URL` | hosted instance | Remote enrichment API |
| `ALFRED_API_ENABLED` | `true` | Disable with `0` for offline mode |
| `ALFRED_LLM_BACKEND` | `codex` | `codex` or `native` (OpenAI API) |
| `OPENAI_API_KEY` | — | API key for native backend |
| `OPENAI_API_BASE` | `https://api.openai.com/v1` | Custom API endpoint |
| `ALFRED_MODEL` | auto-detected | Override model (e.g. `gpt-4.1`) |

## License

[MIT](LICENSE)
