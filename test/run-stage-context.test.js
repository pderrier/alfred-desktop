/**
 * Tests for the `alfred-run-stage-toast` trigger contextBuilder (P1-2).
 *
 * Verifies the pure FR-message mapping for the granular early-run stages
 * emitted by the Rust backend before the LLM phase begins:
 *   - finary_fetching
 *   - snapshot_received
 *   - enriching_market
 *
 * Source of truth: apps/alfred-desktop/src/desktop-shell/app-alfred-run-stage-context.js
 */
import test from "node:test";
import assert from "node:assert/strict";

import {
  buildRunStageContext,
  formatStageMessage
} from "../src/desktop-shell/app-alfred-run-stage-context.js";

test("buildRunStageContext returns null when extra is missing", () => {
  assert.equal(buildRunStageContext(null), null);
  assert.equal(buildRunStageContext(undefined), null);
  assert.equal(buildRunStageContext({}), null);
  assert.equal(buildRunStageContext({ stage: "" }), null);
});

test("buildRunStageContext returns null for non-early-run stages", () => {
  // line_analysis, synthesis, done are owned by other triggers
  assert.equal(buildRunStageContext({ stage: "line_analysis" }), null);
  assert.equal(buildRunStageContext({ stage: "synthesis" }), null);
  assert.equal(buildRunStageContext({ stage: "done" }), null);
  assert.equal(buildRunStageContext({ stage: "unknown_stage" }), null);
});

test("finary_fetching → FR fetch message with 5s autoDismiss", () => {
  const ctx = buildRunStageContext({ stage: "finary_fetching" });
  assert.ok(ctx, "expected context to be returned");
  assert.match(ctx.initialMessage, /Récupération du portefeuille Finary/);
  assert.equal(ctx.autoDismissMs, 5000);
  assert.deepEqual(ctx.actions, []);
});

test("snapshot_received with positions_count → 'Snapshot reçu (N lignes)'", () => {
  const ctx = buildRunStageContext({
    stage: "snapshot_received",
    snapshot_summary: { positions_count: 28 }
  });
  assert.ok(ctx);
  assert.equal(ctx.initialMessage, "✓ Snapshot reçu (28 lignes)");
});

test("snapshot_received with collection_progress.total → uses total", () => {
  const ctx = buildRunStageContext({
    stage: "snapshot_received",
    collection_progress: { completed: 0, total: 12 }
  });
  assert.ok(ctx);
  assert.equal(ctx.initialMessage, "✓ Snapshot reçu (12 lignes)");
});

test("snapshot_received with no count → message without count", () => {
  const ctx = buildRunStageContext({ stage: "snapshot_received" });
  assert.ok(ctx);
  assert.equal(ctx.initialMessage, "✓ Snapshot reçu");
});

test("enriching_market with progress → 'Cours marché (C/T)…'", () => {
  const ctx = buildRunStageContext({
    stage: "enriching_market",
    collection_progress: { completed: 5, total: 28 }
  });
  assert.ok(ctx);
  assert.equal(ctx.initialMessage, "Cours marché (5/28)…");
});

test("enriching_market without progress → generic fetch message", () => {
  const ctx = buildRunStageContext({ stage: "enriching_market" });
  assert.ok(ctx);
  assert.match(ctx.initialMessage, /Récupération des cours marché/);
});

test("enriching_market with zero total → generic fetch message", () => {
  const ctx = buildRunStageContext({
    stage: "enriching_market",
    collection_progress: { completed: 0, total: 0 }
  });
  assert.ok(ctx);
  assert.match(ctx.initialMessage, /Récupération des cours marché/);
});

test("formatStageMessage is the pure mapping unit", () => {
  // Direct exposure of the pure function so future stages can be added
  // with a single line of test coverage.
  assert.match(formatStageMessage("finary_fetching", {}), /Finary/);
  assert.equal(formatStageMessage("not_a_stage", {}), "");
});

test("context shape is stable — only initialMessage, autoDismissMs, actions", () => {
  const ctx = buildRunStageContext({ stage: "finary_fetching" });
  const keys = Object.keys(ctx).sort();
  assert.deepEqual(keys, ["actions", "autoDismissMs", "initialMessage"]);
});
