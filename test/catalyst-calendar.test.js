import test from "node:test";
import assert from "node:assert/strict";
import {
  parseFrenchCatalystDate,
  extractDatedCatalysts,
  formatCatalystRow,
  formatFrenchDateShort,
} from "../src/desktop-shell/catalyst-calendar.js";

// ── P2-25: parseFrenchCatalystDate ─────────────────────────────────

test("parseFrenchCatalystDate: full FR date with year (production sample)", () => {
  const d = parseFrenchCatalystDate("AG du 27 mai 2026");
  assert.ok(d instanceof Date);
  assert.equal(d.getUTCFullYear(), 2026);
  assert.equal(d.getUTCMonth(), 4); // 0-indexed → May
  assert.equal(d.getUTCDate(), 27);
});

test("parseFrenchCatalystDate: handles all FR month names + accents", () => {
  assert.equal(parseFrenchCatalystDate("le 1 février 2026")?.getUTCMonth(), 1);
  assert.equal(parseFrenchCatalystDate("le 1 fevrier 2026")?.getUTCMonth(), 1);
  assert.equal(parseFrenchCatalystDate("le 5 août 2026")?.getUTCMonth(), 7);
  assert.equal(parseFrenchCatalystDate("le 5 aout 2026")?.getUTCMonth(), 7);
  assert.equal(parseFrenchCatalystDate("le 12 décembre 2026")?.getUTCMonth(), 11);
});

test("parseFrenchCatalystDate: defaults to nowYear when year is missing", () => {
  const d = parseFrenchCatalystDate("AG du 27 mai", 2027);
  assert.equal(d.getUTCFullYear(), 2027);
  assert.equal(d.getUTCMonth(), 4);
  assert.equal(d.getUTCDate(), 27);
});

test("parseFrenchCatalystDate: rejects percentage-like patterns (no false positives)", () => {
  // Pure number-only patterns must not parse as a date.
  assert.equal(parseFrenchCatalystDate("Marge de 25 %"), null);
  assert.equal(parseFrenchCatalystDate("Croissance 7 % à 12 % en 2026"), null);
});

test("parseFrenchCatalystDate: rejects out-of-range day", () => {
  assert.equal(parseFrenchCatalystDate("32 mai 2026"), null);
  // Feb 30 must not auto-roll to March
  assert.equal(parseFrenchCatalystDate("30 février 2026"), null);
});

test("parseFrenchCatalystDate: handles null/empty input gracefully", () => {
  assert.equal(parseFrenchCatalystDate(""), null);
  assert.equal(parseFrenchCatalystDate(null), null);
  assert.equal(parseFrenchCatalystDate(undefined), null);
  assert.equal(parseFrenchCatalystDate(123), null);
});

test("parseFrenchCatalystDate: extracts from longer text (production phrasing)", () => {
  // Sample from `019e17dec61e.json`.
  const d = parseFrenchCatalystDate("Publication trafic de mai le 5 juin 2026");
  assert.equal(d.getUTCMonth(), 5); // June
  assert.equal(d.getUTCDate(), 5);
});

// ── P2-25: extractDatedCatalysts ────────────────────────────────────

const NOW = new Date("2026-05-23T10:00:00Z");

test("extractDatedCatalysts: filters to [now, now+30d] window", () => {
  const recos = [
    { ticker: "GET", catalyseurs: ["AG du 27 mai 2026"] },          // in window
    { ticker: "ORA", catalyseurs: ["AG du 19 mai 2026"] },          // before now → excluded
    { ticker: "BNP", catalyseurs: ["Publication le 5 juillet 2026"] }, // beyond 30d → excluded
    { ticker: "MC",  catalyseurs: ["Earnings le 5 juin 2026"] },    // in window
  ];
  const rows = extractDatedCatalysts(recos, { now: NOW, limit: 10 });
  assert.equal(rows.length, 2, "only events in [now, now+30d] are kept");
  assert.equal(rows[0].ticker, "GET");
  assert.equal(rows[1].ticker, "MC");
});

test("extractDatedCatalysts: sorts ascending by date", () => {
  const recos = [
    { ticker: "Z", catalyseurs: ["AG du 15 juin 2026"] },
    { ticker: "A", catalyseurs: ["AG du 25 mai 2026"] },
    { ticker: "M", catalyseurs: ["AG du 1 juin 2026"] },
  ];
  const rows = extractDatedCatalysts(recos, { now: NOW, limit: 10 });
  assert.deepEqual(rows.map((r) => r.ticker), ["A", "M", "Z"]);
});

test("extractDatedCatalysts: respects the limit", () => {
  const recos = Array.from({ length: 10 }, (_, i) => ({
    ticker: `T${i}`,
    catalyseurs: [`AG du ${i + 1} juin 2026`],
  }));
  const rows = extractDatedCatalysts(recos, { now: NOW, limit: 3 });
  assert.equal(rows.length, 3);
});

test("extractDatedCatalysts: computes daysAhead as integer", () => {
  const recos = [{ ticker: "X", catalyseurs: ["AG du 24 mai 2026"] }];
  const rows = extractDatedCatalysts(recos, { now: NOW, limit: 1 });
  assert.equal(rows[0].daysAhead, 1, "24 May vs 23 May (now) → 1 day ahead");
});

test("extractDatedCatalysts: ignores undated catalysts", () => {
  const recos = [{ ticker: "X", catalyseurs: ["Annonce clinique positive", "Avancée réglementaire"] }];
  const rows = extractDatedCatalysts(recos, { now: NOW });
  assert.equal(rows.length, 0);
});

test("extractDatedCatalysts: handles null/empty input safely", () => {
  assert.deepEqual(extractDatedCatalysts(null), []);
  assert.deepEqual(extractDatedCatalysts([]), []);
  assert.deepEqual(extractDatedCatalysts([{ ticker: "X" }]), []);
});

// ── P2-25: formatCatalystRow ────────────────────────────────────────

test("formatCatalystRow: countdown — today/demain/dans X jours", () => {
  const ticker = "MC";
  const text = "Earnings call";
  assert.match(formatCatalystRow({ ticker, text, daysAhead: 0 }), /· aujourd'hui$/);
  assert.match(formatCatalystRow({ ticker, text, daysAhead: 1 }), /· demain$/);
  assert.match(formatCatalystRow({ ticker, text, daysAhead: 7 }), /· dans 7 jours$/);
});

test("formatCatalystRow: empty/null input returns empty string", () => {
  assert.equal(formatCatalystRow(null), "");
  assert.equal(formatCatalystRow({}), "", "malformed row with no daysAhead must not emit 'NaN/undefined' fragments");
});

// ── P2-25: formatFrenchDateShort ────────────────────────────────────

test("formatFrenchDateShort: '<day> <month-fr>' format", () => {
  assert.equal(formatFrenchDateShort(new Date("2026-05-27T10:00:00Z")), "27 mai");
  assert.equal(formatFrenchDateShort(new Date("2026-12-01T10:00:00Z")), "1 décembre");
});

test("formatFrenchDateShort: invalid/null returns '?'", () => {
  assert.equal(formatFrenchDateShort(null), "?");
  assert.equal(formatFrenchDateShort(new Date("not-a-date")), "?");
});
