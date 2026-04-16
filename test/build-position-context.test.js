/**
 * Tests for buildPositionContext() from app-line-modal.js
 *
 * The function IS exported but the module has browser-path imports and
 * top-level DOM access, so we replicate the exact logic here.
 * Source of truth: apps/alfred-desktop/src/desktop-shell/app-line-modal.js lines 242-305.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated function (exact copy from app-line-modal.js) ──────

function buildPositionContext(rec) {
  const ticker = rec?.ticker || "N/A";
  const name = rec?.name || "";
  const signal = rec?.signal || "N/A";
  const conviction = rec?.conviction || "N/A";
  const summary = rec?.summary || "";
  const details = rec?.details || {};
  const memory = rec?.lineMemory || {};
  const analysis = details.analysis || {};
  const position = details.position || {};
  const market = details.market || {};
  const news = details.news || [];

  const sections = [];
  sections.push(`Position: ${ticker}${name ? ` (${name})` : ""}`);
  sections.push(`Signal: ${signal} | Conviction: ${conviction}`);
  if (summary) sections.push(`Recommendation: ${summary}`);

  const posMetrics = [];
  if (position.quantite != null) posMetrics.push(`Qty: ${position.quantite}`);
  if (position.poids_pct != null) posMetrics.push(`Weight: ${position.poids_pct}%`);
  if (position.prix_actuel != null) posMetrics.push(`Price: ${position.prix_actuel}`);
  if (position.plus_moins_value_pct != null) posMetrics.push(`P/L: ${position.plus_moins_value_pct}%`);
  if (posMetrics.length > 0) sections.push(`Position: ${posMetrics.join(", ")}`);

  const mktMetrics = [];
  if (market.prix_actuel != null) mktMetrics.push(`Price: ${market.prix_actuel}`);
  if (market.pe_ratio != null) mktMetrics.push(`P/E: ${market.pe_ratio}`);
  if (market.revenue_growth != null) mktMetrics.push(`Rev Growth: ${market.revenue_growth}%`);
  if (market.profit_margin != null) mktMetrics.push(`Margin: ${market.profit_margin}%`);
  if (market.debt_to_equity != null) mktMetrics.push(`D/E: ${market.debt_to_equity}`);
  if (mktMetrics.length > 0) sections.push(`Market: ${mktMetrics.join(", ")}`);

  if (analysis.analyse_technique) sections.push(`Technical: ${analysis.analyse_technique}`);
  if (analysis.analyse_fondamentale) sections.push(`Fundamental: ${analysis.analyse_fondamentale}`);
  if (analysis.analyse_sentiment) sections.push(`Sentiment: ${analysis.analyse_sentiment}`);
  if (analysis.deep_news_summary) sections.push(`News analysis: ${analysis.deep_news_summary}`);

  const reasons = analysis.raisons_principales || [];
  if (reasons.length > 0) sections.push(`Key reasons: ${reasons.slice(0, 5).join("; ")}`);
  const risks = analysis.risques || [];
  if (risks.length > 0) sections.push(`Risks: ${risks.slice(0, 5).join("; ")}`);
  const catalysts = analysis.catalyseurs || [];
  if (catalysts.length > 0) sections.push(`Catalysts: ${catalysts.slice(0, 5).join("; ")}`);

  if (news.length > 0) {
    const headlines = news.slice(0, 5).map((a) => a.title || "Untitled").join("; ");
    sections.push(`Recent news: ${headlines}`);
  }

  if (memory.llm_memory_summary) sections.push(`Memory: ${memory.llm_memory_summary}`);
  const signals = memory.llm_strong_signals || [];
  if (signals.length > 0) sections.push(`Signals: ${signals.join(", ")}`);
  const keyHistory = memory.llm_key_history || [];
  if (keyHistory.length > 0) sections.push(`History: ${keyHistory.slice(0, 5).join("; ")}`);

  return `The user is inspecting their ${ticker}${name ? ` (${name})` : ""} position. Here is the full analysis context:\n\n${sections.join("\n")}\n\nYou are a portfolio analysis assistant. Answer questions about this position based on the context above. This is a read-only discussion — you cannot change recommendations or portfolio state. Be concise and specific. Only answer questions related to the portfolio, positions, and financial analysis. Politely decline any off-topic requests.`;
}

// ── Tests ────────────────────────────────────────────────────────

test("buildPositionContext: includes ticker and name in preamble", () => {
  const result = buildPositionContext({ ticker: "MC", name: "LVMH" });
  assert.ok(result.includes("Position: MC (LVMH)"));
  assert.ok(result.includes("inspecting their MC (LVMH) position"));
});

test("buildPositionContext: signal and conviction appear in output", () => {
  const result = buildPositionContext({
    ticker: "MC", signal: "ACHETER", conviction: "high"
  });
  assert.ok(result.includes("Signal: ACHETER | Conviction: high"));
});

test("buildPositionContext: position metrics included when present", () => {
  const result = buildPositionContext({
    ticker: "MC",
    details: {
      position: { quantite: 10, poids_pct: 5.2, prix_actuel: 850, plus_moins_value_pct: 12.3 }
    }
  });
  assert.ok(result.includes("Qty: 10"));
  assert.ok(result.includes("Weight: 5.2%"));
  assert.ok(result.includes("P/L: 12.3%"));
});

test("buildPositionContext: market data included when present", () => {
  const result = buildPositionContext({
    ticker: "AI",
    details: {
      market: { prix_actuel: 142.5, pe_ratio: 28.3, revenue_growth: 15.2, profit_margin: 8.1, debt_to_equity: 0.45 }
    }
  });
  assert.ok(result.includes("P/E: 28.3"));
  assert.ok(result.includes("Rev Growth: 15.2%"));
  assert.ok(result.includes("Margin: 8.1%"));
  assert.ok(result.includes("D/E: 0.45"));
});

test("buildPositionContext: analysis sections included when present", () => {
  const result = buildPositionContext({
    ticker: "MC",
    details: {
      analysis: {
        analyse_technique: "Bullish momentum",
        analyse_fondamentale: "Strong earnings",
        analyse_sentiment: "Positive",
        deep_news_summary: "Luxury sector rally",
        raisons_principales: ["Revenue growth", "Market share"],
        risques: ["Currency risk"],
        catalyseurs: ["Asia expansion"]
      }
    }
  });
  assert.ok(result.includes("Technical: Bullish momentum"));
  assert.ok(result.includes("Fundamental: Strong earnings"));
  assert.ok(result.includes("Sentiment: Positive"));
  assert.ok(result.includes("News analysis: Luxury sector rally"));
  assert.ok(result.includes("Key reasons: Revenue growth; Market share"));
  assert.ok(result.includes("Risks: Currency risk"));
  assert.ok(result.includes("Catalysts: Asia expansion"));
});

test("buildPositionContext: news headlines included (max 5)", () => {
  const news = Array.from({ length: 8 }, (_, i) => ({ title: `Article ${i + 1}` }));
  const result = buildPositionContext({ ticker: "MC", details: { news } });
  assert.ok(result.includes("Recent news: Article 1; Article 2; Article 3; Article 4; Article 5"));
  assert.ok(!result.includes("Article 6"));
});

test("buildPositionContext: line memory fields included when present", () => {
  const result = buildPositionContext({
    ticker: "MC",
    lineMemory: {
      llm_memory_summary: "Strong performer since Q2",
      llm_strong_signals: ["momentum", "volume"],
      llm_key_history: ["Q1 beat", "Dividend raised", "CEO replaced"]
    }
  });
  assert.ok(result.includes("Memory: Strong performer since Q2"));
  assert.ok(result.includes("Signals: momentum, volume"));
  assert.ok(result.includes("History: Q1 beat; Dividend raised; CEO replaced"));
});

test("buildPositionContext: empty/missing lineMemory does not add phantom sections", () => {
  const result = buildPositionContext({ ticker: "MC" });
  assert.ok(!result.includes("Memory:"));
  assert.ok(!result.includes("Signals:"));
  assert.ok(!result.includes("History:"));
});

test("buildPositionContext: handles missing rec gracefully", () => {
  const result = buildPositionContext(undefined);
  assert.ok(result.includes("Position: N/A"));
  assert.ok(result.includes("Signal: N/A"));
});

test("buildPositionContext: empty details/market/analysis produce no phantom sections", () => {
  const result = buildPositionContext({ ticker: "MC", details: {} });
  assert.ok(!result.includes("Technical:"));
  assert.ok(!result.includes("Fundamental:"));
  assert.ok(!result.includes("Market:"));
  assert.ok(!result.includes("Recent news:"));
});

test("buildPositionContext: reasons/risks/catalysts capped at 5", () => {
  const result = buildPositionContext({
    ticker: "MC",
    details: {
      analysis: {
        raisons_principales: ["a", "b", "c", "d", "e", "f", "g"],
        risques: ["r1", "r2", "r3", "r4", "r5", "r6"]
      }
    }
  });
  // 5 reasons joined by "; " — 6th and 7th should be excluded
  assert.ok(result.includes("Key reasons: a; b; c; d; e"));
  assert.ok(!result.includes("; f;"), "6th reason 'f' must not appear in reasons list");
  assert.ok(!result.includes("; f\n"), "6th reason 'f' must not appear at end of reasons");
  assert.ok(result.includes("Risks: r1; r2; r3; r4; r5"));
  assert.ok(!result.includes("r6"));
});
