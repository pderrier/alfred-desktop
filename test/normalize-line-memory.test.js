/**
 * Tests for normalizeLineMemory() from report-view-model.js
 *
 * The function is module-private, so we replicate the exact logic here
 * (same pattern as apps/desktop-ui/test/ legacy tests). The source of truth
 * is apps/alfred-desktop/src/desktop-shell/report-view-model.js lines 39-89.
 */
import test from "node:test";
import assert from "node:assert/strict";

// ── Replicated helpers (exact copies from report-view-model.js) ──

function asText(value, fallback = "") {
  const normalized = String(value || "").trim();
  return normalized || fallback;
}

function normalizeLineMemory(raw = {}, fallback = {}) {
  const nested = raw && typeof raw === "object" ? raw : {};
  const base = fallback && typeof fallback === "object" ? fallback : {};
  return {
    llm_memory_summary: asText(
      nested?.llm_memory_summary || base?.llm_memory_summary || base?.summary || base?.synthese
    ),
    llm_strong_signals: Array.isArray(nested?.llm_strong_signals)
      ? nested.llm_strong_signals
      : Array.isArray(base?.llm_strong_signals)
        ? base.llm_strong_signals
        : [],
    llm_key_history: Array.isArray(nested?.llm_key_history)
      ? nested.llm_key_history
      : Array.isArray(base?.llm_key_history)
        ? base.llm_key_history
        : [],
    deep_news_memory_summary: asText(
      nested?.deep_news_memory_summary || base?.deep_news_memory_summary || base?.deep_news_summary
    ),
    deep_news_selected_url: asText(nested?.deep_news_selected_url || base?.deep_news_selected_url),
    deep_news_seen_urls: Array.isArray(nested?.deep_news_seen_urls)
      ? nested.deep_news_seen_urls
      : Array.isArray(base?.deep_news_seen_urls)
        ? base.deep_news_seen_urls
        : [],
    deep_news_banned_urls: Array.isArray(nested?.deep_news_banned_urls)
      ? nested.deep_news_banned_urls
      : Array.isArray(base?.deep_news_banned_urls)
        ? base.deep_news_banned_urls
        : [],
    // V2 fields — pass through from raw line memory
    signal_history: Array.isArray(nested?.signal_history)
      ? nested.signal_history
      : Array.isArray(base?.signal_history)
        ? base.signal_history
        : [],
    key_reasoning: asText(nested?.key_reasoning || base?.key_reasoning),
    price_tracking: (nested?.price_tracking && typeof nested.price_tracking === "object")
      ? nested.price_tracking
      : (base?.price_tracking && typeof base.price_tracking === "object")
        ? base.price_tracking
        : null,
    news_themes: Array.isArray(nested?.news_themes)
      ? nested.news_themes
      : Array.isArray(base?.news_themes)
        ? base.news_themes
        : [],
    trend: asText(nested?.trend || base?.trend)
  };
}

// ── Tests: V1 fields ────────────────────────────────────────────

test("normalizeLineMemory: V1 fields pass through from nested", () => {
  const result = normalizeLineMemory({
    llm_memory_summary: "Stock is bullish",
    llm_strong_signals: ["momentum up", "earnings beat"],
    llm_key_history: ["Q1 results strong", "CEO change"]
  });
  assert.equal(result.llm_memory_summary, "Stock is bullish");
  assert.deepEqual(result.llm_strong_signals, ["momentum up", "earnings beat"]);
  assert.deepEqual(result.llm_key_history, ["Q1 results strong", "CEO change"]);
});

test("normalizeLineMemory: V1 fields fall back to base when nested is empty", () => {
  const result = normalizeLineMemory({}, {
    llm_memory_summary: "From base",
    llm_strong_signals: ["signal1"],
    llm_key_history: ["history1"]
  });
  assert.equal(result.llm_memory_summary, "From base");
  assert.deepEqual(result.llm_strong_signals, ["signal1"]);
  assert.deepEqual(result.llm_key_history, ["history1"]);
});

test("normalizeLineMemory: base.summary and base.synthese used as fallback for llm_memory_summary", () => {
  const fromSummary = normalizeLineMemory({}, { summary: "summary fallback" });
  assert.equal(fromSummary.llm_memory_summary, "summary fallback");

  const fromSynthese = normalizeLineMemory({}, { synthese: "synthese fallback" });
  assert.equal(fromSynthese.llm_memory_summary, "synthese fallback");
});

test("normalizeLineMemory: nested path preferred over base path", () => {
  const result = normalizeLineMemory(
    { llm_memory_summary: "nested wins", llm_strong_signals: ["nested"] },
    { llm_memory_summary: "base loses", llm_strong_signals: ["base"] }
  );
  assert.equal(result.llm_memory_summary, "nested wins");
  assert.deepEqual(result.llm_strong_signals, ["nested"]);
});

test("normalizeLineMemory: deep_news fields pass through", () => {
  const result = normalizeLineMemory({
    deep_news_memory_summary: "Market turmoil",
    deep_news_selected_url: "https://example.com/article",
    deep_news_seen_urls: ["https://a.com", "https://b.com"],
    deep_news_banned_urls: ["https://bad.com"]
  });
  assert.equal(result.deep_news_memory_summary, "Market turmoil");
  assert.equal(result.deep_news_selected_url, "https://example.com/article");
  assert.deepEqual(result.deep_news_seen_urls, ["https://a.com", "https://b.com"]);
  assert.deepEqual(result.deep_news_banned_urls, ["https://bad.com"]);
});

test("normalizeLineMemory: base.deep_news_summary used as fallback for deep_news_memory_summary", () => {
  const result = normalizeLineMemory({}, { deep_news_summary: "legacy summary" });
  assert.equal(result.deep_news_memory_summary, "legacy summary");
});

test("normalizeLineMemory: missing/null/undefined inputs return safe defaults", () => {
  const fromUndef = normalizeLineMemory(undefined, undefined);
  assert.equal(fromUndef.llm_memory_summary, "");
  assert.deepEqual(fromUndef.llm_strong_signals, []);
  assert.deepEqual(fromUndef.llm_key_history, []);
  assert.equal(fromUndef.deep_news_memory_summary, "");
  assert.equal(fromUndef.deep_news_selected_url, "");
  assert.deepEqual(fromUndef.deep_news_seen_urls, []);
  assert.deepEqual(fromUndef.deep_news_banned_urls, []);
  // V2 defaults
  assert.deepEqual(fromUndef.signal_history, []);
  assert.equal(fromUndef.key_reasoning, "");
  assert.equal(fromUndef.price_tracking, null);
  assert.deepEqual(fromUndef.news_themes, []);
  assert.equal(fromUndef.trend, "");

  const fromNull = normalizeLineMemory(null, null);
  assert.equal(fromNull.llm_memory_summary, "");
  assert.deepEqual(fromNull.llm_strong_signals, []);
  assert.deepEqual(fromNull.signal_history, []);
  assert.equal(fromNull.key_reasoning, "");

  const fromEmpty = normalizeLineMemory();
  assert.equal(fromEmpty.llm_memory_summary, "");
  assert.deepEqual(fromEmpty.signal_history, []);
});

test("normalizeLineMemory: non-object inputs treated as empty", () => {
  const fromString = normalizeLineMemory("not an object", 42);
  assert.equal(fromString.llm_memory_summary, "");
  assert.deepEqual(fromString.llm_strong_signals, []);
  assert.deepEqual(fromString.signal_history, []);
  assert.equal(fromString.key_reasoning, "");
});

test("normalizeLineMemory: non-array llm_strong_signals falls back to base", () => {
  const result = normalizeLineMemory(
    { llm_strong_signals: "not an array" },
    { llm_strong_signals: ["from base"] }
  );
  assert.deepEqual(result.llm_strong_signals, ["from base"]);
});

test("normalizeLineMemory: non-array in both nested and base returns empty array", () => {
  const result = normalizeLineMemory(
    { llm_strong_signals: "str" },
    { llm_strong_signals: "str" }
  );
  assert.deepEqual(result.llm_strong_signals, []);
});

// ── Tests: V2 fields ────────────────────────────────────────────

test("normalizeLineMemory: V2 signal_history passes through from nested", () => {
  const history = [
    { date: "2026-04-01", signal: "ACHAT", conviction: "forte", price_at_signal: 142.5 },
    { date: "2026-03-15", signal: "CONSERVER", conviction: "moderee", price_at_signal: 138.0 }
  ];
  const result = normalizeLineMemory({ signal_history: history });
  assert.deepEqual(result.signal_history, history);
});

test("normalizeLineMemory: V2 signal_history falls back to base", () => {
  const history = [{ date: "2026-04-01", signal: "ACHAT", conviction: "forte", price_at_signal: 142.5 }];
  const result = normalizeLineMemory({}, { signal_history: history });
  assert.deepEqual(result.signal_history, history);
});

test("normalizeLineMemory: V2 key_reasoning passes through", () => {
  const result = normalizeLineMemory({ key_reasoning: "Strong growth thesis based on margin expansion." });
  assert.equal(result.key_reasoning, "Strong growth thesis based on margin expansion.");
});

test("normalizeLineMemory: V2 key_reasoning falls back to base", () => {
  const result = normalizeLineMemory({}, { key_reasoning: "Base thesis" });
  assert.equal(result.key_reasoning, "Base thesis");
});

test("normalizeLineMemory: V2 price_tracking object passes through", () => {
  const pt = {
    last_signal: "ACHAT",
    last_signal_date: "2026-04-01",
    price_at_signal: 142.5,
    current_price: 151.2,
    return_since_signal_pct: 6.1,
    signal_accuracy: "correct"
  };
  const result = normalizeLineMemory({ price_tracking: pt });
  assert.deepEqual(result.price_tracking, pt);
});

test("normalizeLineMemory: V2 price_tracking falls back to base", () => {
  const pt = { last_signal: "VENTE", price_at_signal: 100.0 };
  const result = normalizeLineMemory({}, { price_tracking: pt });
  assert.deepEqual(result.price_tracking, pt);
});

test("normalizeLineMemory: V2 price_tracking null if not object", () => {
  const result = normalizeLineMemory({ price_tracking: "not an object" });
  assert.equal(result.price_tracking, null);
});

test("normalizeLineMemory: V2 news_themes passes through", () => {
  const result = normalizeLineMemory({ news_themes: ["tariffs", "margin_expansion", "CEO_transition"] });
  assert.deepEqual(result.news_themes, ["tariffs", "margin_expansion", "CEO_transition"]);
});

test("normalizeLineMemory: V2 news_themes falls back to base", () => {
  const result = normalizeLineMemory({}, { news_themes: ["trade_war"] });
  assert.deepEqual(result.news_themes, ["trade_war"]);
});

test("normalizeLineMemory: V2 trend passes through", () => {
  const result = normalizeLineMemory({ trend: "upgrading" });
  assert.equal(result.trend, "upgrading");
});

test("normalizeLineMemory: V2 trend falls back to base", () => {
  const result = normalizeLineMemory({}, { trend: "stable" });
  assert.equal(result.trend, "stable");
});

test("normalizeLineMemory: full V2 payload round-trip", () => {
  const full = {
    llm_memory_summary: "",
    llm_strong_signals: [],
    llm_key_history: [],
    deep_news_memory_summary: "Market analysis",
    deep_news_selected_url: "https://example.com",
    deep_news_seen_urls: ["https://a.com"],
    deep_news_banned_urls: [],
    signal_history: [
      { date: "2026-04-01", signal: "ACHAT", conviction: "forte", price_at_signal: 142.5 }
    ],
    key_reasoning: "Strong thesis.",
    price_tracking: {
      last_signal: "ACHAT",
      price_at_signal: 142.5,
      return_since_signal_pct: 6.1,
      signal_accuracy: "correct"
    },
    news_themes: ["tariffs", "earnings"],
    trend: "upgrading"
  };
  const result = normalizeLineMemory(full);
  assert.deepEqual(result.signal_history, full.signal_history);
  assert.equal(result.key_reasoning, "Strong thesis.");
  assert.deepEqual(result.price_tracking, full.price_tracking);
  assert.deepEqual(result.news_themes, ["tariffs", "earnings"]);
  assert.equal(result.trend, "upgrading");
});

test("normalizeLineMemory: nested V2 preferred over base V2", () => {
  const result = normalizeLineMemory(
    { key_reasoning: "nested wins", trend: "upgrading", news_themes: ["a"] },
    { key_reasoning: "base loses", trend: "stable", news_themes: ["b"] }
  );
  assert.equal(result.key_reasoning, "nested wins");
  assert.equal(result.trend, "upgrading");
  assert.deepEqual(result.news_themes, ["a"]);
});
