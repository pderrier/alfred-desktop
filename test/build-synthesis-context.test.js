/**
 * Tests for buildSynthesisContext() from app.js
 *
 * The function is module-private and uses formatCurrency from ui-display-utils.js,
 * so we replicate both here.
 * Source of truth: apps/alfred-desktop/src/desktop-shell/app.js lines 334-353.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated helpers ───────────────────────────────────────────

function formatCurrency(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "\u2014";
  }
  return new Intl.NumberFormat("fr-FR", {
    style: "currency",
    currency: "EUR",
    minimumFractionDigits: 0,
    maximumFractionDigits: 0
  }).format(value);
}

function buildSynthesisContext(model) {
  const sections = [];
  if (model.account) sections.push(`Account: ${model.account}`);
  sections.push(`Date: ${model.lastUpdate || "n/a"}`);
  const metrics = [];
  if (model.value != null) metrics.push(`Portfolio value: ${formatCurrency(model.value)}`);
  if (model.gain != null) metrics.push(`Gain: ${formatCurrency(model.gain)}`);
  if (model.cash != null) metrics.push(`Cash: ${formatCurrency(model.cash)}`);
  if (metrics.length > 0) sections.push(metrics.join(" | "));
  sections.push(`Positions: ${model.recommendations?.length || 0}`);
  sections.push(`Synthesis: ${model.synthesis || "N/A"}`);
  if (Array.isArray(model.actionsNow) && model.actionsNow.length > 0) {
    const actionList = model.actionsNow
      .slice(0, 10)
      .map((a) => `${a.action} ${a.ticker || a.nom || "?"}`)
      .join(", ");
    sections.push(`Actions (${model.actionsNow.length}): ${actionList}`);
  }
  return `You are a senior portfolio analyst. The user wants to discuss the portfolio-level synthesis. Answer questions about strategy, macro context, or reasoning. Be concise. Only answer questions related to the portfolio and financial analysis. Politely decline any off-topic requests.\n\n${sections.join("\n")}`;
}

// ── Tests ────────────────────────────────────────────────────────

test("buildSynthesisContext: includes system preamble", () => {
  const result = buildSynthesisContext({});
  assert.ok(result.startsWith("You are a senior portfolio analyst."));
});

test("buildSynthesisContext: account and date appear", () => {
  const result = buildSynthesisContext({
    account: "PEA Bourso",
    lastUpdate: "2026-04-15"
  });
  assert.ok(result.includes("Account: PEA Bourso"));
  assert.ok(result.includes("Date: 2026-04-15"));
});

test("buildSynthesisContext: date defaults to n/a when missing", () => {
  const result = buildSynthesisContext({});
  assert.ok(result.includes("Date: n/a"));
});

test("buildSynthesisContext: portfolio metrics included when present", () => {
  const result = buildSynthesisContext({
    value: 50000,
    gain: 5000,
    cash: 2000
  });
  // formatCurrency produces locale-dependent output; just check the label is there
  assert.ok(result.includes("Portfolio value:"));
  assert.ok(result.includes("Gain:"));
  assert.ok(result.includes("Cash:"));
});

test("buildSynthesisContext: no metrics line when all null", () => {
  const result = buildSynthesisContext({});
  // Should NOT have "Portfolio value:" etc.
  assert.ok(!result.includes("Portfolio value:"));
  assert.ok(!result.includes("Gain:"));
  assert.ok(!result.includes("Cash:"));
});

test("buildSynthesisContext: positions count from recommendations array", () => {
  const result = buildSynthesisContext({
    recommendations: [{ ticker: "MC" }, { ticker: "AI" }, { ticker: "OR" }]
  });
  assert.ok(result.includes("Positions: 3"));
});

test("buildSynthesisContext: positions defaults to 0 when no recommendations", () => {
  const result = buildSynthesisContext({});
  assert.ok(result.includes("Positions: 0"));
});

test("buildSynthesisContext: synthesis text included", () => {
  const result = buildSynthesisContext({
    synthesis: "Portfolio is well-diversified with strong tech exposure."
  });
  assert.ok(result.includes("Synthesis: Portfolio is well-diversified with strong tech exposure."));
});

test("buildSynthesisContext: synthesis defaults to N/A", () => {
  const result = buildSynthesisContext({});
  assert.ok(result.includes("Synthesis: N/A"));
});

test("buildSynthesisContext: actionsNow included (up to 10)", () => {
  const actions = Array.from({ length: 15 }, (_, i) => ({
    action: "ACHETER",
    ticker: `T${i}`
  }));
  const result = buildSynthesisContext({ actionsNow: actions });
  assert.ok(result.includes(`Actions (15):`));
  // First 10 should be listed
  assert.ok(result.includes("ACHETER T0"));
  assert.ok(result.includes("ACHETER T9"));
  // 11th should NOT appear in the list text
  assert.ok(!result.includes("T10"));
});

test("buildSynthesisContext: actions use nom as fallback when ticker missing", () => {
  const result = buildSynthesisContext({
    actionsNow: [{ action: "VENDRE", nom: "LVMH" }]
  });
  assert.ok(result.includes("VENDRE LVMH"));
});

test("buildSynthesisContext: actions use ? when neither ticker nor nom", () => {
  const result = buildSynthesisContext({
    actionsNow: [{ action: "ACHETER" }]
  });
  assert.ok(result.includes("ACHETER ?"));
});

test("buildSynthesisContext: no actions section when array empty", () => {
  const result = buildSynthesisContext({ actionsNow: [] });
  assert.ok(!result.includes("Actions"));
});

test("buildSynthesisContext: no actions section when not an array", () => {
  const result = buildSynthesisContext({ actionsNow: "not an array" });
  assert.ok(!result.includes("Actions"));
});
