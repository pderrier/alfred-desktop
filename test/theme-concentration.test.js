/**
 * Tests for theme concentration rendering logic from app.js
 *
 * Module-private functions that build theme concentration UI elements.
 * We replicate the pure logic (sorting, capping, buildThemeLi HTML structure).
 * Source of truth: apps/alfred-desktop/src/desktop-shell/app.js lines 590-673.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated helpers (exact copies from app.js / ui-display-utils.js) ──

function escapeHtml(str) {
  const s = String(str ?? "");
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function buildThemeLi(entry) {
  const themeLabel = escapeHtml(entry.theme || "?");
  const count = entry.count || 0;
  const tickers = (entry.tickers || []).map((t) => escapeHtml(t)).join(", ");
  // Return an object mirroring the innerHTML structure (no DOM in node:test)
  const innerHTML = `<span class="theme-slug">${themeLabel}</span> <span class="theme-count">(${count} positions)</span>: <span class="theme-tickers">${tickers}</span>`;
  return { tagName: "li", innerHTML };
}

/**
 * Replicated theme sorting + capping logic from renderThemeConcentration.
 * Returns { sorted, topThemes, overflowThemes, introText } for assertions.
 */
function processThemes(themeConcentration) {
  if (!themeConcentration) return null;
  const themes = themeConcentration.themes;
  if (!Array.isArray(themes) || themes.length === 0) return null;

  const sorted = [...themes].sort((a, b) => (b.count || 0) - (a.count || 0));
  const TOP_N = 5;
  const topThemes = sorted.slice(0, TOP_N);
  const overflowThemes = sorted.slice(TOP_N);
  const introText = `${sorted.length} theme${sorted.length > 1 ? "s" : ""} shared by 3+ positions — potential concentration risk.`;

  return { sorted, topThemes, overflowThemes, introText };
}

// ── Tests: buildThemeLi ─────────────────────────────────────────

test("buildThemeLi: produces correct HTML structure with theme, count, and tickers", () => {
  const result = buildThemeLi({
    theme: "Technology",
    count: 5,
    tickers: ["AAPL", "MSFT", "GOOG"]
  });
  assert.equal(result.tagName, "li");
  assert.ok(result.innerHTML.includes('class="theme-slug"'));
  assert.ok(result.innerHTML.includes("Technology"));
  assert.ok(result.innerHTML.includes("(5 positions)"));
  assert.ok(result.innerHTML.includes("AAPL, MSFT, GOOG"));
});

test("buildThemeLi: missing theme defaults to ?", () => {
  const result = buildThemeLi({ count: 3, tickers: ["X"] });
  assert.ok(result.innerHTML.includes(">?</span>"));
});

test("buildThemeLi: missing count defaults to 0", () => {
  const result = buildThemeLi({ theme: "Energy" });
  assert.ok(result.innerHTML.includes("(0 positions)"));
});

test("buildThemeLi: missing tickers produces empty tickers span", () => {
  const result = buildThemeLi({ theme: "Healthcare", count: 2 });
  assert.ok(result.innerHTML.includes('class="theme-tickers"></span>'));
});

test("buildThemeLi: escapes HTML in theme name and tickers", () => {
  const result = buildThemeLi({
    theme: "<script>alert(1)</script>",
    count: 1,
    tickers: ["A&B"]
  });
  assert.ok(!result.innerHTML.includes("<script>"));
  assert.ok(result.innerHTML.includes("&lt;script&gt;"));
  assert.ok(result.innerHTML.includes("A&amp;B"));
});

test("buildThemeLi: empty entry produces safe defaults", () => {
  const result = buildThemeLi({});
  assert.equal(result.tagName, "li");
  assert.ok(result.innerHTML.includes(">?</span>"));
  assert.ok(result.innerHTML.includes("(0 positions)"));
});

// ── Tests: Theme sorting ────────────────────────────────────────

test("theme sorting: themes sorted by count descending", () => {
  const result = processThemes({
    themes: [
      { theme: "A", count: 2 },
      { theme: "B", count: 5 },
      { theme: "C", count: 3 }
    ]
  });
  assert.deepEqual(result.sorted.map((t) => t.theme), ["B", "C", "A"]);
});

test("theme sorting: equal counts preserve relative order (stable sort)", () => {
  const result = processThemes({
    themes: [
      { theme: "X", count: 3 },
      { theme: "Y", count: 3 },
      { theme: "Z", count: 3 }
    ]
  });
  // All same count — stable sort preserves insertion order
  assert.equal(result.sorted.length, 3);
  assert.deepEqual(result.sorted.map((t) => t.theme), ["X", "Y", "Z"]);
});

test("theme sorting: missing count treated as 0", () => {
  const result = processThemes({
    themes: [
      { theme: "NoCount" },
      { theme: "HasCount", count: 4 }
    ]
  });
  assert.equal(result.sorted[0].theme, "HasCount");
  assert.equal(result.sorted[1].theme, "NoCount");
});

// ── Tests: Top 5 cap ────────────────────────────────────────────

test("top 5 cap: exactly 5 themes → all in top, none in overflow", () => {
  const themes = Array.from({ length: 5 }, (_, i) => ({
    theme: `T${i}`, count: 10 - i
  }));
  const result = processThemes({ themes });
  assert.equal(result.topThemes.length, 5);
  assert.equal(result.overflowThemes.length, 0);
});

test("top 5 cap: 7 themes → 5 in top, 2 in overflow", () => {
  const themes = Array.from({ length: 7 }, (_, i) => ({
    theme: `T${i}`, count: 20 - i
  }));
  const result = processThemes({ themes });
  assert.equal(result.topThemes.length, 5);
  assert.equal(result.overflowThemes.length, 2);
});

test("top 5 cap: 3 themes → all in top, none in overflow", () => {
  const themes = [
    { theme: "A", count: 5 },
    { theme: "B", count: 3 },
    { theme: "C", count: 1 }
  ];
  const result = processThemes({ themes });
  assert.equal(result.topThemes.length, 3);
  assert.equal(result.overflowThemes.length, 0);
});

test("top 5 cap: overflow contains the correct lower-count themes", () => {
  const themes = Array.from({ length: 8 }, (_, i) => ({
    theme: `T${i}`, count: 80 - i * 10
  }));
  const result = processThemes({ themes });
  // Top 5 should have counts 80, 70, 60, 50, 40
  assert.deepEqual(result.topThemes.map((t) => t.count), [80, 70, 60, 50, 40]);
  // Overflow should have counts 30, 20, 10
  assert.deepEqual(result.overflowThemes.map((t) => t.count), [30, 20, 10]);
});

// ── Tests: Empty / null themes ──────────────────────────────────

test("empty themes: null themeConcentration returns null", () => {
  assert.equal(processThemes(null), null);
});

test("empty themes: undefined themeConcentration returns null", () => {
  assert.equal(processThemes(undefined), null);
});

test("empty themes: empty themes array returns null", () => {
  assert.equal(processThemes({ themes: [] }), null);
});

test("empty themes: missing themes property returns null", () => {
  assert.equal(processThemes({}), null);
});

test("empty themes: non-array themes returns null", () => {
  assert.equal(processThemes({ themes: "not an array" }), null);
});

// ── Tests: Intro text ───────────────────────────────────────────

test("intro text: singular for 1 theme", () => {
  const result = processThemes({ themes: [{ theme: "A", count: 5 }] });
  assert.equal(result.introText, "1 theme shared by 3+ positions — potential concentration risk.");
});

test("intro text: plural for multiple themes", () => {
  const result = processThemes({
    themes: [
      { theme: "A", count: 5 },
      { theme: "B", count: 3 }
    ]
  });
  assert.equal(result.introText, "2 themes shared by 3+ positions — potential concentration risk.");
});
