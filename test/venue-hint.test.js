/**
 * Tests for venueLabelFromSymbol() and renderVenueHint() from app-line-modal.js
 * (v0.3 #23 — UI venue display).
 *
 * The module has browser-path imports so we replicate the pure helpers here,
 * same pattern as render-collection-quality.test.js. Source of truth:
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

// ── Replicated helpers (exact copy from app-line-modal.js) ───────

const YAHOO_VENUE_BY_SUFFIX = {
  ".PA": "Paris",
  ".AS": "Amsterdam",
  ".DE": "Frankfurt",
  ".L":  "London",
  ".MI": "Milan",
  ".MC": "Madrid",
  ".BR": "Brussels",
  ".LS": "Lisbon",
  ".SW": "Swiss",
  ".HE": "Helsinki",
  ".ST": "Stockholm",
  ".CO": "Copenhagen",
  ".OL": "Oslo",
  ".VI": "Vienna",
  ".T":  "Tokyo",
  ".HK": "Hong Kong",
  ".SA": "São Paulo",
  ".AX": "Sydney",
  ".NS": "Mumbai",
  ".SS": "Shanghai",
  ".SZ": "Shenzhen"
};

function venueLabelFromSymbol(symbol) {
  const s = String(symbol || "").trim();
  if (!s) return "";
  const dot = s.lastIndexOf(".");
  if (dot < 0 || dot === s.length - 1) return "";
  const suffix = s.slice(dot);
  return YAHOO_VENUE_BY_SUFFIX[suffix] || "";
}

function renderVenueHint(resolvedSymbol, ticker) {
  const resolved = String(resolvedSymbol || "").trim();
  const bare = String(ticker || "").trim();
  if (!resolved) return "";
  if (resolved.toUpperCase() === bare.toUpperCase()) return "";
  const venue = venueLabelFromSymbol(resolved);
  const tail = venue ? ` · ${escapeHtml(venue)}` : "";
  return `<span class="collection-venue-hint" title="Canonical Yahoo symbol resolved from ISIN">${escapeHtml(resolved)}${tail}</span>`;
}

// ── venueLabelFromSymbol ─────────────────────────────────────────

test("venueLabelFromSymbol: maps every documented Yahoo suffix to its city", () => {
  // Spot-check every suffix in YAHOO_VENUE_BY_SUFFIX — if any mapping
  // regresses, this test fires the exact entry that broke.
  const expectations = [
    ["STMPA.PA", "Paris"],
    ["ASML.AS", "Amsterdam"],
    ["SAP.DE", "Frankfurt"],
    ["BARC.L", "London"],
    ["STLAM.MI", "Milan"],
    ["TEF.MC", "Madrid"],
    ["KBC.BR", "Brussels"],
    ["JMT.LS", "Lisbon"],
    ["NESN.SW", "Swiss"],
    ["NOKIA.HE", "Helsinki"],
    ["ERIC-B.ST", "Stockholm"],
    ["NOVO-B.CO", "Copenhagen"],
    ["EQNR.OL", "Oslo"],
    ["EBS.VI", "Vienna"],
    ["7203.T", "Tokyo"],
    ["0700.HK", "Hong Kong"],
    ["PETR4.SA", "São Paulo"],
    ["BHP.AX", "Sydney"],
    ["TCS.NS", "Mumbai"],
    ["600519.SS", "Shanghai"],
    ["000001.SZ", "Shenzhen"]
  ];
  for (const [symbol, expected] of expectations) {
    assert.equal(
      venueLabelFromSymbol(symbol),
      expected,
      `${symbol} should map to ${expected}`
    );
  }
});

test("venueLabelFromSymbol: bare US ticker returns empty string", () => {
  // US tickers (AAPL, MSFT, …) have no Yahoo suffix — the canonical symbol
  // and the broker ticker are identical, so we render nothing extra.
  assert.equal(venueLabelFromSymbol("AAPL"), "");
  assert.equal(venueLabelFromSymbol("MSFT"), "");
  assert.equal(venueLabelFromSymbol("BRK-B"), "");
});

test("venueLabelFromSymbol: unknown suffix returns empty string", () => {
  // A new exchange suffix not yet in our mapping (e.g. ".XX") should NOT
  // throw nor render "undefined". Empty string lets the caller decide a
  // graceful fallback.
  assert.equal(venueLabelFromSymbol("FOO.XX"), "");
  assert.equal(venueLabelFromSymbol("BAR.UNKNOWN"), "");
});

test("venueLabelFromSymbol: handles null/undefined/empty gracefully", () => {
  assert.equal(venueLabelFromSymbol(null), "");
  assert.equal(venueLabelFromSymbol(undefined), "");
  assert.equal(venueLabelFromSymbol(""), "");
  assert.equal(venueLabelFromSymbol("   "), "");
});

test("venueLabelFromSymbol: dot at the end yields empty string", () => {
  // Defensive: a malformed symbol like "AAPL." should not match any suffix
  // — empty trailing suffix is meaningless.
  assert.equal(venueLabelFromSymbol("AAPL."), "");
});

test("venueLabelFromSymbol: trims whitespace before parsing", () => {
  // Defensive: a symbol coming through serde could carry leading/trailing
  // whitespace from a CSV column. Trim before parsing.
  assert.equal(venueLabelFromSymbol("  STMPA.PA  "), "Paris");
});

// ── renderVenueHint ─────────────────────────────────────────────

test("renderVenueHint: surfaces 'symbol · venue' when resolved differs from ticker", () => {
  // Canonical case — French PEA holding STMPA whose ISIN resolves to STMPA.PA
  // should render "STMPA.PA · Paris" in the line modal.
  const html = renderVenueHint("STMPA.PA", "STMPA");
  assert.ok(html.includes("STMPA.PA"), `expected canonical symbol in HTML:\n${html}`);
  assert.ok(html.includes("Paris"), `expected venue label in HTML:\n${html}`);
  assert.ok(html.includes("collection-venue-hint"), "expected CSS hook class");
});

test("renderVenueHint: shows just the canonical when resolved differs and venue unknown", () => {
  // Edge case — a new Yahoo suffix we haven't mapped yet should still
  // surface the canonical symbol (it carries useful info), just without
  // a city tail.
  const html = renderVenueHint("FOO.XX", "FOO");
  assert.ok(html.includes("FOO.XX"));
  assert.ok(!html.includes(" · "), `unknown suffix must not render trailing dot:\n${html}`);
});

test("renderVenueHint: empty string when resolved equals ticker (case-insensitive)", () => {
  // No useful extra info to show — preserves the legacy modal layout
  // byte-for-byte (snapshot UI contract).
  assert.equal(renderVenueHint("AAPL", "AAPL"), "");
  assert.equal(renderVenueHint("aapl", "AAPL"), "");
  assert.equal(renderVenueHint("AAPL", "aapl"), "");
});

test("renderVenueHint: empty string when resolvedSymbol is missing/null/blank", () => {
  // Legacy rows (older server, watchlist without ISIN, /api/resolve down)
  // must render nothing — modal layout stays byte-identical.
  assert.equal(renderVenueHint(null, "STMPA"), "");
  assert.equal(renderVenueHint(undefined, "STMPA"), "");
  assert.equal(renderVenueHint("", "STMPA"), "");
  assert.equal(renderVenueHint("   ", "STMPA"), "");
});

test("renderVenueHint: escapes HTML in resolved symbol", () => {
  // Defensive: if a malformed symbol somehow ends in characters that look
  // like HTML, never inject them raw — that'd break the modal and create
  // an XSS surface for any future user-typed watchlist add.
  const html = renderVenueHint("<script>.PA", "STMPA");
  assert.ok(!html.includes("<script>"), "raw <script> tag must NOT appear in output");
  assert.ok(html.includes("&lt;script&gt;"), "expected HTML-escaped form:\n" + html);
});

// ── Backward compat (snapshot UI contract) ──────────────────────

test("renderVenueHint: pre-v0.3 details (no resolvedSymbol) renders byte-identical to before", () => {
  // The v0.3 venue hint MUST degrade to an empty string for older payloads
  // so the modal HTML is unchanged. We verify the contract end-to-end by
  // comparing the concatenation `venueHint + header` against `header` alone.
  const header = `<div class="collection-source-badge">boursorama</div>`;
  const venueHint = renderVenueHint(undefined, "AAPL"); // pre-v0.3 details
  assert.equal(venueHint + header, header, "venue hint must not mutate legacy header bytes");
});

// ── renderCollectionDetail composition contract ────────────────
//
// renderCollectionDetail() requires DOM nodes (panel, grid) which aren't
// available under node:test. Instead, we test the HTML composition contract
// directly: the grid innerHTML is `venueHint + header + body...`. So a test
// with details.resolvedSymbol = "STMPA.PA" and details.ticker = "STMPA" must
// emit "Paris" *before* the rest of the grid; a test with no resolution
// must produce HTML identical to a pre-v0.3 build.

test("renderCollectionDetail composition: resolvedSymbol prepends venue hint to grid", () => {
  const details = {
    ticker: "STMPA",
    resolvedSymbol: "STMPA.PA",
    market: {},
    quality: {},
    collectionQuality: null,
    technicalSnapshot: null,
    marketSource: "boursorama:spot"
  };
  // Mirror the renderCollectionDetail body (header-fallback branch + venue
  // hint) byte-for-byte. Source of truth: app-line-modal.js.
  const venueHint = renderVenueHint(details.resolvedSymbol, details.ticker);
  const source = details.marketSource;
  const sourceParts = source.split(":");
  const sourceProvider = sourceParts[0] || "unknown";
  const sourceType = sourceParts[1] || "";
  const header = `<div class="collection-source-badge">${escapeHtml(sourceProvider)}${sourceType ? ` <span class="source-type">${escapeHtml(sourceType)}</span>` : ""}</div>`;
  const gridHtml = venueHint + header;

  assert.ok(gridHtml.includes("Paris"), `expected 'Paris' in grid HTML:\n${gridHtml}`);
  assert.ok(gridHtml.includes("STMPA.PA"), `expected canonical symbol in grid HTML:\n${gridHtml}`);
  // Venue hint must come BEFORE the header div (precedes it in the string).
  const venueIdx = gridHtml.indexOf("collection-venue-hint");
  const headerIdx = gridHtml.indexOf("collection-source-badge");
  assert.ok(venueIdx >= 0 && headerIdx >= 0);
  assert.ok(venueIdx < headerIdx, "venue hint must precede source badge in DOM order");
});

test("renderCollectionDetail composition: no resolution → grid HTML byte-equal to pre-v0.3", () => {
  // Snapshot UI contract: a payload without `resolvedSymbol` must produce the
  // exact same grid HTML as before the v0.3 change.
  const pre = {
    ticker: "AAPL",
    market: {},
    quality: {},
    collectionQuality: null,
    technicalSnapshot: null,
    marketSource: "alphavantage:fundamentals"
  };
  const v03 = { ...pre, resolvedSymbol: null };
  const composeGrid = (details) => {
    const venueHint = renderVenueHint(details.resolvedSymbol, details.ticker);
    const source = details.marketSource;
    const parts = source.split(":");
    const header = `<div class="collection-source-badge">${escapeHtml(parts[0] || "unknown")}${parts[1] ? ` <span class="source-type">${escapeHtml(parts[1])}</span>` : ""}</div>`;
    return venueHint + header;
  };
  assert.equal(
    composeGrid(v03),
    composeGrid(pre),
    "v0.3 payload without resolution must produce identical grid HTML to pre-v0.3"
  );
});

test("renderCollectionDetail composition: resolved equals ticker → no venue hint", () => {
  // US ticker AAPL whose resolution returned AAPL — no useful "venue" to
  // surface. Grid HTML must be byte-equal to the pre-v0.3 shape.
  const details = {
    ticker: "AAPL",
    resolvedSymbol: "AAPL",
    market: {},
    quality: {},
    collectionQuality: null,
    technicalSnapshot: null,
    marketSource: "alphavantage:fundamentals"
  };
  const venueHint = renderVenueHint(details.resolvedSymbol, details.ticker);
  assert.equal(venueHint, "", "resolved = ticker must produce empty hint");
});
