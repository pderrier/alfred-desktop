# Alfred Desktop

AI-powered portfolio analysis for retail investors. Alfred connects to your brokerage data (via Finary or CSV import), enriches it with market data and news, then uses LLM analysis to generate actionable investment recommendations.

## Features

- **Portfolio sync** — connect Finary for automatic brokerage account sync, or import CSV exports
- **Market enrichment** — real-time prices, fundamentals, and news from multiple sources
- **AI analysis** — per-position technical, fundamental, and sentiment analysis via OpenAI Codex
- **Synthesis report** — portfolio-wide recommendations with conviction levels and action items
- **Watchlist** — AI-suggested positions to monitor based on your portfolio profile
- **Multi-account** — manage and analyze multiple brokerage accounts independently

## Architecture

Alfred Desktop is a [Tauri 2](https://v2.tauri.app/) application:

- **Frontend** — vanilla JavaScript with a custom shell UI (no framework)
- **Backend** — Rust (Tauri commands, async analysis worker, native enrichment)
- **LLM** — OpenAI Codex via the [Codex CLI](https://github.com/openai/codex) app-server protocol
- **Data** — local JSON files for run state, SQLite for structured data

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
    services/       # Native collection, enrichment, MCP analysis
  Cargo.toml        # Rust dependencies
  tauri.conf.json   # Tauri configuration
```

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

Environment variables (optional):

| Variable | Default | Description |
|---|---|---|
| `ALFRED_DATA_DIR` | auto-detected | Override data directory |
| `ALFRED_STATE_DIR` | `<data>/runtime-state` | Run state files |
| `ALFRED_REPORTS_DIR` | `<data>/reports` | Report artifacts |
| `ALFRED_DEBUG_LOG_PATH` | `<data>/debug.log` | Debug log file |

## License

[MIT](LICENSE)
