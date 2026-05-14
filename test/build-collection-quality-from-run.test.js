/**
 * Tests for buildCollectionQualityFromRun() — the view-model helper that
 * derives the per-block collection_quality envelope from a `latestRun`
 * payload on the desktop side. Source of truth:
 * apps/alfred-desktop/src/desktop-shell/report-view-model.js.
 *
 * Mirrors the Rust helper in mcp_server.rs::build_collection_quality so the
 * UI badges match what the LLM sees via tool_get_line_data.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated function (exact copy from report-view-model.js) ───

function asText(value, fallback = "") {
  const normalized = String(value || "").trim();
  return normalized || fallback;
}

function buildCollectionQualityFromRun(latestRun, ticker, ctx) {
  const market = ctx?.market || {};
  const technical = ctx?.technical || null;
  const newsArticles = Array.isArray(ctx?.newsArticles) ? ctx.newsArticles : [];
  const enrichmentFailures = Array.isArray(ctx?.enrichmentFailures) ? ctx.enrichmentFailures : [];

  const hasPrice = typeof market?.prix_actuel === "number" && Number.isFinite(market.prix_actuel);
  const marketSource = asText(market?.source) || null;
  const runAsOf = asText(latestRun?.updated_at || latestRun?.completed_at) || new Date().toISOString();
  const failedScopes = new Set(enrichmentFailures.map((f) => asText(f?.scope).toLowerCase()));

  const spot = {
    source: marketSource,
    as_of: runAsOf,
    quality: hasPrice ? "fresh" : failedScopes.has("market") ? "unavailable" : "unavailable"
  };

  const fields = ["pe_ratio", "revenue_growth", "profit_margin", "debt_to_equity"];
  const present = fields.filter((k) => typeof market?.[k] === "number" && Number.isFinite(market[k])).length;
  const fundamentalsQuality = present >= 4 ? "fresh" : present >= 1 ? "degraded" : "unavailable";
  const fundamentals = { source: marketSource, as_of: runAsOf, quality: fundamentalsQuality };

  let technicalBlock;
  if (technical && typeof technical === "object") {
    const samples = typeof technical.samples === "number" ? technical.samples : 0;
    const serverQuality = asText(technical.quality);
    const fallbackQuality = samples >= 200 ? "fresh" : samples >= 60 ? "degraded" : "unavailable";
    technicalBlock = {
      source: asText(technical.source) || "alphavantage:daily",
      as_of: asText(technical.as_of) || runAsOf,
      quality: serverQuality || fallbackQuality,
      samples
    };
  } else {
    technicalBlock = { source: null, as_of: null, quality: "unavailable" };
  }

  let newsQuality;
  if (newsArticles.length === 0) newsQuality = "unavailable";
  else if (newsArticles.length < 2) newsQuality = "degraded";
  else newsQuality = "fresh";
  const news = { source: "searxng", as_of: runAsOf, quality: newsQuality, count: newsArticles.length };

  const sectorCot = { source: "cached", as_of: null, quality: "unavailable" };
  const insights = { source: "shared", as_of: null, quality: "unavailable" };
  const memory = { source: "local", as_of: null, quality: "unavailable" };

  return { spot, fundamentals, technical: technicalBlock, news, sector_cot: sectorCot, insights, memory };
}

// ── Tests ────────────────────────────────────────────────────────

test("buildCollectionQualityFromRun: complete data → all blocks 'fresh' except sector/insights/memory", () => {
  const latestRun = { updated_at: "2026-05-15T12:00:00Z" };
  const ctx = {
    market: {
      prix_actuel: 142.5,
      pe_ratio: 25.0,
      revenue_growth: 10.0,
      profit_margin: 15.0,
      debt_to_equity: 0.5,
      source: "alphavantage"
    },
    technical: {
      as_of: "2026-05-15",
      source: "alphavantage:daily",
      samples: 250,
      quality: "fresh",
      indicators: {}
    },
    newsArticles: [{ title: "a" }, { title: "b" }, { title: "c" }],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "AAPL", ctx);
  assert.equal(cq.spot.quality, "fresh");
  assert.equal(cq.fundamentals.quality, "fresh");
  assert.equal(cq.technical.quality, "fresh");
  assert.equal(cq.technical.samples, 250);
  assert.equal(cq.news.quality, "fresh");
  assert.equal(cq.news.count, 3);
  // Sector, insights, memory are unavailable from run_state alone
  assert.equal(cq.sector_cot.quality, "unavailable");
  assert.equal(cq.insights.quality, "unavailable");
  assert.equal(cq.memory.quality, "unavailable");
});

test("buildCollectionQualityFromRun: missing technical → unavailable", () => {
  const latestRun = { updated_at: "2026-05-15T12:00:00Z" };
  const ctx = {
    market: { prix_actuel: 100, source: "boursorama" },
    technical: null,
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "MC", ctx);
  assert.equal(cq.technical.quality, "unavailable");
  assert.equal(cq.technical.source, null);
});

test("buildCollectionQualityFromRun: partial fundamentals → 'degraded'", () => {
  const latestRun = {};
  const ctx = {
    market: { prix_actuel: 100, pe_ratio: 25.0 }, // only 1 of 4 fundamentals
    technical: null,
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.fundamentals.quality, "degraded");
});

test("buildCollectionQualityFromRun: no fundamentals → 'unavailable'", () => {
  const latestRun = {};
  const ctx = {
    market: { prix_actuel: 100 },
    technical: null,
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.fundamentals.quality, "unavailable");
});

test("buildCollectionQualityFromRun: single news article → 'degraded'", () => {
  const latestRun = {};
  const ctx = {
    market: { prix_actuel: 100 },
    technical: null,
    newsArticles: [{ title: "only one" }],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.news.quality, "degraded");
  assert.equal(cq.news.count, 1);
});

test("buildCollectionQualityFromRun: technical samples<60 → 'unavailable' fallback", () => {
  const latestRun = {};
  const ctx = {
    market: {},
    technical: { samples: 30 }, // server didn't set quality, samples too few
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.technical.quality, "unavailable");
});

test("buildCollectionQualityFromRun: technical samples between 60-200 → 'degraded' fallback", () => {
  const latestRun = {};
  const ctx = {
    market: {},
    technical: { samples: 120 },
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.technical.quality, "degraded");
});

test("buildCollectionQualityFromRun: server quality wins over sample-count fallback", () => {
  const latestRun = {};
  const ctx = {
    market: {},
    technical: { samples: 30, quality: "fresh" }, // server says fresh even with few samples
    newsArticles: [],
    enrichmentFailures: []
  };
  const cq = buildCollectionQualityFromRun(latestRun, "X", ctx);
  assert.equal(cq.technical.quality, "fresh");
});
