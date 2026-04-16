/**
 * Tests for normalizeLineMemory() from report-view-model.js
 *
 * The function is module-private, so we replicate the exact logic here
 * (same pattern as apps/desktop-ui/test/ legacy tests). The source of truth
 * is apps/alfred-desktop/src/desktop-shell/report-view-model.js lines 39-71.
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
        : []
  };
}

// ── Tests ────────────────────────────────────────────────────────

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

  const fromNull = normalizeLineMemory(null, null);
  assert.equal(fromNull.llm_memory_summary, "");
  assert.deepEqual(fromNull.llm_strong_signals, []);

  const fromEmpty = normalizeLineMemory();
  assert.equal(fromEmpty.llm_memory_summary, "");
});

test("normalizeLineMemory: non-object inputs treated as empty", () => {
  const fromString = normalizeLineMemory("not an object", 42);
  assert.equal(fromString.llm_memory_summary, "");
  assert.deepEqual(fromString.llm_strong_signals, []);
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
