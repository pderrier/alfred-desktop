<p align="center">
  <img src="src/desktop-shell/alfred-splash.png" alt="Alfred" width="280" />
</p>

<h1 align="center">Alfred Desktop</h1>

<p align="center">AI-powered portfolio analysis for retail investors.<br/>Connect your brokerage data, enrich it with market data and news, and get actionable investment recommendations powered by AI.</p>

## Download

**[Windows installer (MSI)](https://vps-c5793aab.vps.ovh.net/alfred/release/windows/latest)** — Alfred Desktop v0.1.0 for Windows 10/11 (x64)

> Note: The installer is not code-signed yet. Windows SmartScreen may show a warning — click "More info" → "Run anyway" to proceed.

## Features

- **Portfolio sync** — connect Finary for automatic brokerage account sync, or import CSV exports
- **Market enrichment** — real-time prices, fundamentals, and news from multiple sources
- **AI analysis** — per-position technical, fundamental, and sentiment analysis via OpenAI Codex
- **Synthesis report** — portfolio-wide recommendations with conviction levels and action items
- **Watchlist** — AI-suggested positions to monitor based on your portfolio profile
- **Multi-account** — manage and analyze multiple brokerage accounts independently

## Required accounts

Alfred relies on two external services:

### OpenAI (required)

Alfred uses [OpenAI Codex](https://github.com/openai/codex) to perform AI-powered analysis of each position in your portfolio — technical, fundamental, and sentiment analysis with actionable recommendations. An OpenAI account is required. Alfred communicates with Codex locally via the Codex CLI app-server protocol (JSON-RPC over stdio); your prompts and portfolio data are sent directly to OpenAI, not through any intermediary.

Sign up at [platform.openai.com](https://platform.openai.com/signup) if you don't have an account. Alfred handles the rest automatically — it installs the Codex CLI if needed and prompts you to sign in on first launch.

### Finary (optional)

[Finary](https://finary.com) is a wealth management platform that connects to over 20,000 banks, brokers, and crypto platforms worldwide and provides a unified view of your holdings (stocks, funds, real estate, crypto, and more). If you have a Finary account, Alfred can sync your brokerage data automatically — no manual CSV export needed.

Without Finary, you can still use Alfred by importing CSV exports from your broker. The CSV parser uses a three-tier strategy: it first tries to detect known formats (Boursorama), then applies heuristic column matching for common header names, and finally falls back to LLM-assisted column mapping for unknown formats. In theory, this should handle any broker's CSV export — but it hasn't been tested with every format. If you encounter a CSV that doesn't parse correctly, please [open an issue](https://github.com/pderrier/alfred-desktop/issues) or contact the author.

## Architecture

Alfred Desktop is a [Tauri 2](https://v2.tauri.app/) application:

- **Frontend** — vanilla JavaScript with a custom shell UI (no framework)
- **Backend** — Rust (Tauri commands, async analysis worker, native enrichment)
- **LLM** — OpenAI Codex via the [Codex CLI](https://github.com/openai/codex) app-server protocol
- **Data** — local JSON files for run state, SQLite for structured data
- **Remote API** — market data and news enrichment (see below)

```
src/
  desktop-shell/    # Main UI (HTML + vanilla JS)
  shared/           # Tauri IPC bridge + shared utilities
src-tauri/
  src/              # Rust backend
    main.rs         # Tauri app entry point + command handlers
    codex.rs        # Codex app-server client (JSON-RPC over stdio)
    llm.rs          # LLM generation (line analysis, synthesis, watchlist)
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

- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/) 1.75+
- [OpenAI Codex CLI](https://github.com/openai/codex) (`npm install -g @openai/codex`)
- An OpenAI account with Codex access

## Getting started

```bash
# Install dependencies
npm install

# Run in development mode
npm run dev

# Build for production
npm run build            # Linux / macOS
npm run build:windows    # Windows (MSI + NSIS)
```

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

## License

[MIT](LICENSE)
