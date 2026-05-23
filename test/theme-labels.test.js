import test from "node:test";
import assert from "node:assert/strict";
import {
  getThemeLabel,
  humanizeSlug,
  MANUAL_DICT_FOR_TEST,
} from "../src/desktop-shell/theme-labels.js";

// ── P0-20: dictionary coverage ──────────────────────────────────────

test("getThemeLabel: resolves a known slug from the manual dict", () => {
  const result = getThemeLabel("tariffs");
  assert.equal(result.label, "Tarifs douaniers");
  assert.match(result.blurb, /US-Chine|macro/);
  assert.equal(result.pending, false);
});

test("getThemeLabel: case-insensitive match against the dict", () => {
  const upper = getThemeLabel("MARGIN_EXPANSION");
  assert.equal(upper.label, "Expansion de marges");
  assert.equal(upper.pending, false);
});

test("getThemeLabel: trims whitespace before resolving", () => {
  const padded = getThemeLabel("  tariffs  ");
  assert.equal(padded.label, "Tarifs douaniers");
  assert.equal(padded.pending, false);
});

test("getThemeLabel: humanises an unknown slug and flags pending", () => {
  const unknown = getThemeLabel("brand_new_emerging_theme");
  assert.equal(unknown.label, "Brand New Emerging Theme");
  assert.equal(unknown.blurb, null);
  assert.equal(unknown.pending, true, "unknown slugs trigger P1-23 fetch path");
});

test("getThemeLabel: rejects empty/null inputs without throwing", () => {
  assert.equal(getThemeLabel("").label, "Thème inconnu");
  assert.equal(getThemeLabel(null).label, "Thème inconnu");
  assert.equal(getThemeLabel(undefined).label, "Thème inconnu");
  assert.equal(getThemeLabel(123).label, "Thème inconnu");
});

test("humanizeSlug: snake_case to Title Case", () => {
  assert.equal(humanizeSlug("design_win"), "Design Win");
  assert.equal(humanizeSlug("auto_cycle"), "Auto Cycle");
  assert.equal(humanizeSlug("ai"), "Ai");
  assert.equal(humanizeSlug(""), "");
});

// ── Pin all required production slugs ──────────────────────────────

test("dict covers slugs observed in tests.rs production fixtures", () => {
  // From `apps/alfred-desktop/src-tauri/src/tests.rs`:
  // `news_themes: ["tariffs", "margin_expansion", "design_win", "auto_cycle", "tesla", "infineon"]`
  // Tesla / Infineon are company names not generic themes, so we
  // don't translate them — but we DO want the generic finance themes.
  const required = ["tariffs", "margin_expansion", "design_win", "auto_cycle"];
  for (const slug of required) {
    assert.ok(
      Object.prototype.hasOwnProperty.call(MANUAL_DICT_FOR_TEST, slug),
      `manual dict must cover '${slug}' (observed in production fixtures)`,
    );
  }
});

test("dict entries all carry a French label + non-empty blurb", () => {
  for (const [slug, entry] of Object.entries(MANUAL_DICT_FOR_TEST)) {
    assert.ok(entry.label && typeof entry.label === "string", `${slug}: label must be a non-empty string`);
    assert.ok(entry.blurb && typeof entry.blurb === "string", `${slug}: blurb must be a non-empty string`);
    // Voice convention: blurbs are short — they render on one home line.
    assert.ok(entry.blurb.length <= 100, `${slug}: blurb too long (${entry.blurb.length} > 100) — keep it one-line`);
  }
});
