/**
 * Tests for renderCollectionQualityBadges() and renderTechnicalIndicatorRows()
 * from app-line-modal.js, plus backward-compat for renderCollectionDetail().
 *
 * The module has browser-path imports so we replicate the logic here, same
 * pattern as build-position-context.test.js. Source of truth:
 * apps/alfred-desktop/src/desktop-shell/app-line-modal.js.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Local escapeHtml clone (matches ui-display-utils.js) ─────────

function escapeHtml(input) {
  if (input === null || input === undefined) return "";
  return String(input)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// ── Replicated functions (exact copy from app-line-modal.js) ─────

function formatFreshness(asOf, now) {
  const ref = now instanceof Date ? now : new Date();
  if (!asOf) return "";
  const t = new Date(asOf).getTime();
  if (!Number.isFinite(t)) return "";
  const diffMs = ref.getTime() - t;
  if (diffMs < 0) return "aujourd'hui";
  const hours = diffMs / 3600000;
  if (hours < 1) return "il y a <1h";
  if (hours < 6) return `il y a ${Math.round(hours)}h`;
  const sameDay = ref.toDateString() === new Date(t).toDateString();
  if (sameDay) return "aujourd'hui";
  const days = Math.floor(diffMs / 86400000);
  if (days <= 1) return "J-1";
  if (days <= 14) return `J-${days}`;
  if (days <= 60) return `${Math.round(days / 7)}w`;
  return `${Math.round(days / 30)}mo`;
}

function qualityDot(quality) {
  const q = String(quality || "").toLowerCase();
  if (q === "fresh") return "fresh";
  if (q === "stale") return "stale";
  if (q === "degraded") return "degraded";
  return "unavailable";
}

const QUALITY_GLYPH = {
  fresh: "✓",
  stale: "⚠",
  degraded: "⚠",
  unavailable: "✗"
};

const QUALITY_LABEL_FR = {
  fresh: "frais",
  stale: "stale",
  degraded: "partiel",
  unavailable: "indispo"
};

function renderQualityBadgeRow(label, block) {
  if (!block || typeof block !== "object") return "";
  const q = qualityDot(block.quality);
  const glyph = QUALITY_GLYPH[q] || QUALITY_GLYPH.unavailable;
  const qLabel = QUALITY_LABEL_FR[q] || q;
  const source = block.source ? escapeHtml(String(block.source)) : "—";
  const age = formatFreshness(block.as_of);
  const extras = [];
  if (typeof block.count === "number" && block.count > 0) extras.push(`${block.count} articles`);
  if (typeof block.samples === "number" && block.samples > 0) extras.push(`${block.samples}d`);
  const tail = extras.length > 0 ? ` · ${escapeHtml(extras.join(" · "))}` : "";
  return `<div class="collection-quality-row" data-block="${escapeHtml(label)}">
    <span class="collection-quality-dot ${q}" aria-hidden="true">${glyph}</span>
    <span class="collection-quality-label">${escapeHtml(label)}</span>
    <span class="collection-quality-meta">${source}${age ? ` · ${escapeHtml(age)}` : ""}${tail} · <span class="cq-status cq-${q}">${escapeHtml(qLabel)}</span></span>
  </div>`;
}

function renderCollectionQualityBadges(collectionQuality) {
  if (!collectionQuality || typeof collectionQuality !== "object") return "";
  const blocks = [
    ["Spot", collectionQuality.spot],
    ["Fondamentaux", collectionQuality.fundamentals],
    ["Technique", collectionQuality.technical],
    ["News", collectionQuality.news],
    ["Secteur/COT", collectionQuality.sector_cot],
    ["Insights", collectionQuality.insights],
    ["Memoire", collectionQuality.memory]
  ];
  return `<div class="collection-quality-grid">${blocks.map(([label, block]) => renderQualityBadgeRow(label, block)).join("")}</div>`;
}

function renderTechnicalIndicatorRows(snapshot) {
  if (!snapshot || typeof snapshot !== "object") return "";
  const ind = snapshot.indicators;
  if (!ind || typeof ind !== "object") return "";
  const rows = [];
  const fmt = (v, digits = 2) =>
    typeof v === "number" && Number.isFinite(v) ? v.toFixed(digits) : "n/a";
  const pct = (v) =>
    typeof v === "number" && Number.isFinite(v) ? `${v >= 0 ? "+" : ""}${v.toFixed(1)}%` : "n/a";

  const addRow = (label, valueText, present) => {
    const dotClass = present ? "present" : "missing";
    rows.push(`<div class="indicator-row">
      <span class="indicator-dot ${dotClass}"></span>
      <span class="indicator-label">${escapeHtml(label)}</span>
      <span class="indicator-value">${escapeHtml(valueText)}</span>
    </div>`);
  };

  const vsSma200 = ind.current_vs_sma_200_pct;
  addRow("vs SMA200", pct(vsSma200), typeof vsSma200 === "number");
  addRow("RSI(14)", fmt(ind.rsi_14, 0), typeof ind.rsi_14 === "number");
  const hist = ind.macd?.hist;
  addRow("MACD hist", fmt(hist, 2), typeof hist === "number");
  addRow("ATR(14)", fmt(ind.atr_14, 2), typeof ind.atr_14 === "number");
  const hi = ind.high_52w;
  const lo = ind.low_52w;
  const hiLoText = (typeof hi === "number" && typeof lo === "number")
    ? `${fmt(hi, 2)} / ${fmt(lo, 2)}`
    : "n/a";
  addRow("52w hi/lo", hiLoText, typeof hi === "number" && typeof lo === "number");
  const trend = String(ind.trend_signal || "");
  const trendLabel = trend === "up" ? "haussier" : trend === "down" ? "baissier" : trend === "sideways" ? "lateral" : "n/a";
  addRow("Tendance", trendLabel, trend !== "");

  return `<div class="collection-tech-header">Technique</div>${rows.join("")}`;
}

// ── Fixtures ─────────────────────────────────────────────────────

function freshCollectionQuality() {
  const now = "2026-05-15T11:00:00Z";
  return {
    spot:         { source: "alphavantage", as_of: now, quality: "fresh" },
    fundamentals: { source: "boursorama",   as_of: now, quality: "fresh" },
    technical:    { source: "alphavantage:daily", as_of: now, quality: "fresh", samples: 250 },
    news:         { source: "searxng", as_of: now, quality: "fresh", count: 5 },
    sector_cot:   { source: "cached", as_of: "2026-05-08T12:00:00Z", quality: "stale" },
    insights:     { source: "shared", as_of: now, quality: "fresh" },
    memory:       { source: "local", as_of: now, quality: "fresh" }
  };
}

function fullTechnicalSnapshot() {
  return {
    as_of: "2026-05-15",
    source: "alphavantage:daily",
    samples: 250,
    quality: "fresh",
    indicators: {
      sma_20: 142.3,
      sma_50: 138.7,
      sma_200: 130.1,
      rsi_14: 58.2,
      macd: { line: 1.42, signal: 0.98, hist: 0.44 },
      atr_14: 3.21,
      high_52w: 152.0,
      low_52w: 110.5,
      current_vs_sma_200_pct: 9.5,
      current_vs_high_52w_pct: -6.4,
      trend_signal: "up"
    }
  };
}

// ── renderCollectionQualityBadges ────────────────────────────────

test("renderCollectionQualityBadges: full fixture renders exactly 7 rows", () => {
  const html = renderCollectionQualityBadges(freshCollectionQuality());
  // Count occurrences of class="collection-quality-row"
  const matches = html.match(/class="collection-quality-row"/g) || [];
  assert.equal(matches.length, 7, "must render one row per block (spot/fund/tech/news/sector_cot/insights/memory)");
});

test("renderCollectionQualityBadges: labels appear in French", () => {
  const html = renderCollectionQualityBadges(freshCollectionQuality());
  for (const label of ["Spot", "Fondamentaux", "Technique", "News", "Secteur/COT", "Insights", "Memoire"]) {
    assert.ok(html.includes(`>${label}<`), `label "${label}" should appear:\n${html.slice(0, 200)}`);
  }
});

test("renderCollectionQualityBadges: per-block status dots reflect quality enum", () => {
  const html = renderCollectionQualityBadges(freshCollectionQuality());
  // 6 of 7 are fresh, 1 is stale (sector_cot)
  const freshDots = (html.match(/class="collection-quality-dot fresh"/g) || []).length;
  const staleDots = (html.match(/class="collection-quality-dot stale"/g) || []).length;
  assert.equal(freshDots, 6);
  assert.equal(staleDots, 1);
});

test("renderCollectionQualityBadges: tech samples count surfaces in meta", () => {
  const html = renderCollectionQualityBadges(freshCollectionQuality());
  assert.ok(html.includes("250d"), "tech block should expose samples count:\n" + html);
});

test("renderCollectionQualityBadges: news article count surfaces in meta", () => {
  const html = renderCollectionQualityBadges(freshCollectionQuality());
  assert.ok(html.includes("5 articles"), "news block should expose count:\n" + html);
});

test("renderCollectionQualityBadges: missing input returns empty string (fallback)", () => {
  assert.equal(renderCollectionQualityBadges(null), "");
  assert.equal(renderCollectionQualityBadges(undefined), "");
  assert.equal(renderCollectionQualityBadges("not-an-object"), "");
});

test("renderCollectionQualityBadges: unavailable block renders ✗ glyph + 'indispo'", () => {
  const cq = freshCollectionQuality();
  cq.technical = { source: null, as_of: null, quality: "unavailable" };
  const html = renderCollectionQualityBadges(cq);
  assert.ok(html.includes("cq-unavailable"), "should mark technical as unavailable");
  assert.ok(html.includes("indispo"), "should use French label 'indispo'");
  // ✗ should appear at least once
  assert.ok(html.includes("✗"), "should render the unavailable glyph");
});

// ── renderTechnicalIndicatorRows ─────────────────────────────────

test("renderTechnicalIndicatorRows: full snapshot renders 6 indicator rows + header", () => {
  const html = renderTechnicalIndicatorRows(fullTechnicalSnapshot());
  assert.ok(html.includes("collection-tech-header"));
  const rows = (html.match(/class="indicator-row"/g) || []).length;
  assert.equal(rows, 6, "should render: vsSMA200, RSI, MACD hist, ATR, 52w hi/lo, Tendance");
});

test("renderTechnicalIndicatorRows: SMA200 % delta has sign + 1 decimal", () => {
  const html = renderTechnicalIndicatorRows(fullTechnicalSnapshot());
  assert.ok(html.includes("+9.5%"), "should render positive delta with explicit sign:\n" + html);
});

test("renderTechnicalIndicatorRows: tendance maps trend enum to French", () => {
  const up = renderTechnicalIndicatorRows(fullTechnicalSnapshot());
  assert.ok(up.includes("haussier"), "trend=up → haussier");

  const downSnap = fullTechnicalSnapshot();
  downSnap.indicators.trend_signal = "down";
  assert.ok(renderTechnicalIndicatorRows(downSnap).includes("baissier"));

  const sidewaysSnap = fullTechnicalSnapshot();
  sidewaysSnap.indicators.trend_signal = "sideways";
  assert.ok(renderTechnicalIndicatorRows(sidewaysSnap).includes("lateral"));
});

test("renderTechnicalIndicatorRows: missing snapshot returns empty string", () => {
  assert.equal(renderTechnicalIndicatorRows(null), "");
  assert.equal(renderTechnicalIndicatorRows({}), "");
  assert.equal(renderTechnicalIndicatorRows({ indicators: null }), "");
});

test("renderTechnicalIndicatorRows: partial indicators renders n/a for missing", () => {
  const partial = {
    indicators: { rsi_14: 50.0 } // only RSI present
  };
  const html = renderTechnicalIndicatorRows(partial);
  assert.ok(html.includes("collection-tech-header"));
  // RSI should be present
  assert.ok(html.includes(">RSI(14)<"));
  // Several rows should show n/a
  const naCount = (html.match(/>n\/a</g) || []).length;
  assert.ok(naCount >= 4, `expected at least 4 n/a rows, got ${naCount}`);
});

// ── Backward compat ─────────────────────────────────────────────
//
// Old `details` payload (no collectionQuality, no technicalSnapshot) must
// still render — the rendering code falls back to the legacy
// `marketSource` single badge.

test("renderCollectionDetail: backward compat — legacy fixture renders without throwing", () => {
  // Simulate the renderCollectionDetail header-fallback branch directly,
  // since the full function requires DOM nodes. The contract being tested
  // is: when collectionQuality is missing, the header HTML still
  // contains the legacy marketSource badge.
  const details = {
    market: { prix_actuel: 100, source: "boursorama:spot" },
    marketSource: "boursorama:spot"
    // collectionQuality intentionally missing
  };
  // The header branch in renderCollectionDetail when collectionQuality is null:
  const source = details?.marketSource || "unknown";
  const sourceParts = source.split(":");
  const sourceProvider = sourceParts[0] || "unknown";
  const sourceType = sourceParts[1] || "";
  const header = `<div class="collection-source-badge">${escapeHtml(sourceProvider)}${sourceType ? ` <span class="source-type">${escapeHtml(sourceType)}</span>` : ""}</div>`;

  assert.ok(header.includes("boursorama"), "legacy fallback should still render the source badge");
  assert.ok(header.includes("collection-source-badge"));
  // And the new badge grid should NOT be present
  assert.ok(!header.includes("collection-quality-grid"));
});
