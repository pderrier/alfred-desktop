/**
 * Tests for formatFreshness() and qualityDot() from app-line-modal.js.
 *
 * The module has browser-path imports ("/desktop-shell/ui-display-utils.js")
 * so we replicate the exact logic here (same pattern as
 * build-position-context.test.js). Source of truth:
 * apps/alfred-desktop/src/desktop-shell/app-line-modal.js — search for
 * `export function formatFreshness` and `export function qualityDot`.
 */
import test from "node:test";
import assert from "node:assert/strict";

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

// ── formatFreshness ──────────────────────────────────────────────

test("formatFreshness: empty/invalid input returns empty string", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  assert.equal(formatFreshness("", now), "");
  assert.equal(formatFreshness(null, now), "");
  assert.equal(formatFreshness(undefined, now), "");
  assert.equal(formatFreshness("not-a-date", now), "");
});

test("formatFreshness: future timestamp clamps to 'aujourd'hui'", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const future = "2026-05-15T13:00:00Z";
  assert.equal(formatFreshness(future, now), "aujourd'hui");
});

test("formatFreshness: <1h returns 'il y a <1h'", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const recent = "2026-05-15T11:30:00Z";
  assert.equal(formatFreshness(recent, now), "il y a <1h");
});

test("formatFreshness: 2h-5h ago returns hour count", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  assert.equal(formatFreshness("2026-05-15T10:00:00Z", now), "il y a 2h");
  assert.equal(formatFreshness("2026-05-15T07:00:00Z", now), "il y a 5h");
});

test("formatFreshness: same calendar day but >6h ago says 'aujourd'hui'", () => {
  const now = new Date("2026-05-15T20:00:00Z");
  // 8h ago on same day
  assert.equal(formatFreshness("2026-05-15T12:00:00Z", now), "aujourd'hui");
});

test("formatFreshness: 1 day ago returns J-1", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const yesterday = "2026-05-14T12:00:00Z";
  assert.equal(formatFreshness(yesterday, now), "J-1");
});

test("formatFreshness: 3 days ago returns J-3", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const day = "2026-05-12T12:00:00Z";
  assert.equal(formatFreshness(day, now), "J-3");
});

test("formatFreshness: 20 days ago switches to weeks", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const day = "2026-04-25T12:00:00Z"; // 20 days
  // 20 / 7 = 2.86, rounds to 3
  assert.equal(formatFreshness(day, now), "3w");
});

test("formatFreshness: 90 days ago switches to months", () => {
  const now = new Date("2026-05-15T12:00:00Z");
  const day = "2026-02-14T12:00:00Z"; // ~90 days
  // 90 / 30 = 3
  const result = formatFreshness(day, now);
  assert.ok(/^[23]mo$/.test(result), `expected 2mo|3mo, got ${result}`);
});

// ── qualityDot ───────────────────────────────────────────────────

test("qualityDot: fresh → 'fresh'", () => {
  assert.equal(qualityDot("fresh"), "fresh");
  assert.equal(qualityDot("FRESH"), "fresh");
});

test("qualityDot: stale → 'stale'", () => {
  assert.equal(qualityDot("stale"), "stale");
});

test("qualityDot: degraded → 'degraded'", () => {
  assert.equal(qualityDot("degraded"), "degraded");
});

test("qualityDot: unknown/empty/null → 'unavailable'", () => {
  assert.equal(qualityDot("unavailable"), "unavailable");
  assert.equal(qualityDot("bogus"), "unavailable");
  assert.equal(qualityDot(""), "unavailable");
  assert.equal(qualityDot(null), "unavailable");
  assert.equal(qualityDot(undefined), "unavailable");
});
