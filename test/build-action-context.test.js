/**
 * Tests for buildActionContext() from app.js
 *
 * Module-private function that builds chat context for action drill-downs.
 * Uses buildPositionContext internally for matched recommendations.
 * Source of truth: apps/alfred-desktop/src/desktop-shell/app.js lines 355-377.
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

// Simplified buildPositionContext stub for nested call — full version tested separately
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

function buildActionContext(action, recommendations) {
  const sections = [];
  const ticker = action.ticker || "";
  const name = action.nom || "";
  sections.push(`Action: ${action.action} ${name || ticker}`);
  if (action.priority) sections.push(`Priority: ${action.priority}`);
  if (action.rationale) sections.push(`Rationale: ${action.rationale}`);
  if (typeof action.quantity === "number" && action.quantity > 0) sections.push(`Quantity: ${action.quantity}`);
  if (typeof action.estimatedAmountEur === "number" && action.estimatedAmountEur > 0) sections.push(`Estimated amount: ${formatCurrency(action.estimatedAmountEur)}`);
  if (action.orderType) sections.push(`Order type: ${action.orderType}`);
  if (typeof action.priceLimit === "number" && action.priceLimit > 0) sections.push(`Price limit: ${formatCurrency(action.priceLimit)}`);

  if (ticker && Array.isArray(recommendations)) {
    const rec = recommendations.find((r) => r.ticker === ticker);
    if (rec) {
      sections.push("\n--- Full position analysis ---");
      sections.push(buildPositionContext(rec));
    }
  }

  return `You are a portfolio analysis assistant. The user wants to understand this recommended action. Answer questions about rationale, risk, timing, or sizing. Be concise. Only answer questions related to the portfolio and financial analysis. Politely decline any off-topic requests.\n\n${sections.join("\n")}`;
}

// ── Tests ────────────────────────────────────────────────────────

test("buildActionContext: includes system preamble", () => {
  const result = buildActionContext({ action: "ACHETER" }, []);
  assert.ok(result.startsWith("You are a portfolio analysis assistant."));
});

test("buildActionContext: action name uses nom when present", () => {
  const result = buildActionContext({ action: "ACHETER", nom: "LVMH", ticker: "MC" }, []);
  assert.ok(result.includes("Action: ACHETER LVMH"));
});

test("buildActionContext: action name falls back to ticker when nom is empty", () => {
  const result = buildActionContext({ action: "ACHETER", ticker: "MC" }, []);
  assert.ok(result.includes("Action: ACHETER MC"));
});

test("buildActionContext: priority, rationale, quantity, amount, order type, price limit", () => {
  const result = buildActionContext({
    action: "ACHETER",
    ticker: "MC",
    priority: "high",
    rationale: "Strong momentum",
    quantity: 5,
    estimatedAmountEur: 4250,
    orderType: "limit",
    priceLimit: 850
  }, []);
  assert.ok(result.includes("Priority: high"));
  assert.ok(result.includes("Rationale: Strong momentum"));
  assert.ok(result.includes("Quantity: 5"));
  assert.ok(result.includes("Estimated amount:"));
  assert.ok(result.includes("Order type: limit"));
  assert.ok(result.includes("Price limit:"));
});

test("buildActionContext: zero quantity and zero amount not included", () => {
  const result = buildActionContext({
    action: "ACHETER",
    ticker: "MC",
    quantity: 0,
    estimatedAmountEur: 0
  }, []);
  assert.ok(!result.includes("Quantity:"));
  assert.ok(!result.includes("Estimated amount:"));
});

test("buildActionContext: ticker-based rec lookup works", () => {
  const recs = [
    { ticker: "MC", signal: "ACHETER", conviction: "high", summary: "Buy LVMH" },
    { ticker: "AI", signal: "CONSERVER", conviction: "medium" }
  ];
  const result = buildActionContext({ action: "ACHETER", ticker: "MC" }, recs);
  assert.ok(result.includes("--- Full position analysis ---"));
  assert.ok(result.includes("Signal: ACHETER | Conviction: high"));
});

test("buildActionContext: nom-based fallback — no match when ticker doesn't match any rec", () => {
  // buildActionContext matches by ticker only, not by nom
  const recs = [{ ticker: "MC", signal: "ACHETER" }];
  const result = buildActionContext({ action: "ACHETER", nom: "LVMH" }, recs);
  // No ticker on action, so no rec lookup possible
  assert.ok(!result.includes("--- Full position analysis ---"));
});

test("buildActionContext: no match returns action-only context (no position context)", () => {
  const recs = [{ ticker: "AI", signal: "CONSERVER" }];
  const result = buildActionContext({ action: "ACHETER", ticker: "MC" }, recs);
  assert.ok(!result.includes("--- Full position analysis ---"));
  assert.ok(result.includes("Action: ACHETER MC"));
});

test("buildActionContext: null recommendations handled gracefully", () => {
  const result = buildActionContext({ action: "ACHETER", ticker: "MC" }, null);
  assert.ok(result.includes("Action: ACHETER MC"));
  assert.ok(!result.includes("--- Full position analysis ---"));
});

test("buildActionContext: missing recommendations param handled gracefully", () => {
  const result = buildActionContext({ action: "ACHETER", ticker: "MC" });
  assert.ok(result.includes("Action: ACHETER MC"));
});
