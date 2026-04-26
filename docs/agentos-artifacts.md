# AgentOS artifact instrumentation (Alfred Desktop)

This document explains how to enable and consume Alfred's runtime instrumentation artifacts from an external AgentOS workflow (for example with [Nodal-Rabbit](https://github.com/pderrier/Nodal-Rabbit)).

## What gets emitted

When enabled, each `llm_backend::run_prompt` turn writes files under:

```text
<ALFRED_DATA_DIR>/agentos-artifacts/<run_id>/
```

With:

- `meta.json` — run metadata (`run_id`, intent, start timestamp, metadata payload).
- `decisions.json` — appendable array of decision events (`decisions: []`).
- `outcome.json` — single final outcome (`success` / `failure` / `timeout`) with timing/backend/model and optional token usage.

## Enable instrumentation (default is OFF)

Use either:

1. **Environment variable**
   - `ALFRED_AGENTOS_ARTIFACTS_ENABLED=1`
2. **Runtime setting**
   - `agentos_artifacts_enabled = 1`

Examples:

```bash
# Linux/macOS
export ALFRED_AGENTOS_ARTIFACTS_ENABLED=1
npm run dev
```

```powershell
# Windows PowerShell
$env:ALFRED_AGENTOS_ARTIFACTS_ENABLED = "1"
npm run dev
```

## Decision events currently emitted

- `llm.backend.route` — backend routing decision.
- `llm.tool.dispatch` — per function/tool execution (tool name + args fingerprint).
- `llm.retry.policy` — retry attempts from OpenAI streaming path.
- `llm.output.parse` — whether structured JSON was extracted.

## Feeding artifacts into AgentOS / Nodal-Rabbit

The artifact format is designed to be simple JSON files that can be polled or batch-ingested by a separate process.

Suggested flow:

1. Watch `<data_dir>/agentos-artifacts/*/outcome.json`.
2. For each completed run:
   - load `meta.json`
   - load `decisions.json`
   - load `outcome.json`
3. Transform into your AgentOS envelope/event schema.
4. Push to your graph/store/analytics pipeline.

Minimal CLI extraction example:

```bash
RUN_DIR="data/agentos-artifacts/<run_id>"
jq -c '{run_id, intent, started_at, metadata}' "$RUN_DIR/meta.json"
jq -c '.decisions[] | {step_id, decision_key, output, evidence, candidate, ts}' "$RUN_DIR/decisions.json"
jq -c '{status, data, finished_at}' "$RUN_DIR/outcome.json"
```

## AgentOS CLI commands you can use directly

Below is a practical CLI flow aligned with the AgentOS MVP README for Nodal-Rabbit.

### 1) Wrap an Alfred-triggered worker run

```bash
python -m agentos wrap \
  --intent alfred.analysis.turn \
  --decision-file data/agentos-artifacts/<run_id>/decisions.json \
  --strict-decisions \
  -- ./run-alfred-task.sh
```

If you want marker-based capture instead of a decision file:

```bash
python -m agentos wrap \
  --intent alfred.analysis.turn \
  --parse-decision-markers \
  --strict-decisions \
  -- ./run-alfred-task.sh
```

### 2) Inspect captured runs and decisions

```bash
python -m agentos runs list
python -m agentos runs trace <run_id>
python -m agentos decision list --limit 20
```

### 3) Build deterministic candidates from repeated decisions

```bash
python -m agentos patterns list --min-support 2 --limit 20
# equivalent alias
python -m agentos compile candidates --min-support 2 --limit 20
```

### 4) Backtest and promote conservative rules

```bash
python -m agentos backtest run \
  --decision-key llm.backend.route \
  --min-history 3 \
  --min-confidence 0.8

# equivalent alias
python -m agentos compile backtest \
  --decision-key llm.backend.route \
  --min-history 3 \
  --min-confidence 0.8
```

```bash
python -m agentos rules promote \
  --decision-key llm.backend.route \
  --min-history 3 \
  --min-confidence 0.8 \
  --min-accuracy 1.0

# equivalent alias
python -m agentos compile promote \
  --decision-key llm.backend.route \
  --min-history 3 \
  --min-confidence 0.8 \
  --min-accuracy 1.0
```

If a candidate should not be promoted:

```bash
python -m agentos rules reject --decision-key llm.backend.route --reason "manual_review_required"
# equivalent alias
python -m agentos compile reject --decision-key llm.backend.route --reason "manual_review_required"
```

## Notes

- Instrumentation is side-effect only: prompt generation is unchanged.
- If disabled, no artifact folder/files are written.
- `run_id` is generated per prompt execution and is deterministic for file layout.
