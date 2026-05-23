import test from "node:test";
import assert from "node:assert/strict";
import {
  extractTradeMoves,
  extractAction,
  extractSecurityName,
  formatTradeRow,
} from "../src/desktop-shell/home-recent-trades.js";

// ── P1-22: extractAction ────────────────────────────────────────────

test("extractAction: ACHAT prefix → buy", () => {
  assert.equal(extractAction("ACHAT COMPTANT - THERMADOR GROUPE"), "buy");
  assert.equal(extractAction("achat comptant - lvmh"), "buy");
});

test("extractAction: VENTE prefix → sell", () => {
  assert.equal(extractAction("VENTE - LVMH"), "sell");
});

test("extractAction: DIVIDENDE prefix → dividend", () => {
  assert.equal(extractAction("DIVIDENDE TOTALENERGIES"), "dividend");
});

test("extractAction: fallback on value sign when prefix absent", () => {
  assert.equal(extractAction("STM 8 PARTS", -123.45), "buy");
  assert.equal(extractAction("STM 8 PARTS", 123.45), "sell");
  assert.equal(extractAction("STM 8 PARTS", 0), "other");
});

test("extractAction: returns other when no signal at all", () => {
  assert.equal(extractAction("", null), "other");
  assert.equal(extractAction(undefined, undefined), "other");
});

// ── P1-22: extractSecurityName ──────────────────────────────────────

test("extractSecurityName: strips Boursorama-style ACHAT COMPTANT prefix", () => {
  assert.equal(
    extractSecurityName("ACHAT COMPTANT - THERMADOR GROUPE"),
    "THERMADOR GROUPE",
  );
});

test("extractSecurityName: handles multi-part names after dash", () => {
  assert.equal(extractSecurityName("VENTE - LVMH SE"), "LVMH SE");
});

test("extractSecurityName: removes leading action word when no dash", () => {
  assert.equal(extractSecurityName("DIVIDENDE TOTALENERGIES"), "TOTALENERGIES");
});

test("extractSecurityName: passes through unstructured names", () => {
  assert.equal(extractSecurityName("STMICROELECTRONICS NV"), "STMICROELECTRONICS NV");
});

test("extractSecurityName: returns empty string on null", () => {
  assert.equal(extractSecurityName(""), "");
  assert.equal(extractSecurityName(null), "");
});

// ── P1-22: extractTradeMoves ────────────────────────────────────────

function syntheticTrade(opts) {
  return {
    date: opts.date,
    transaction_type: opts.transaction_type || "market_order",
    display_name: opts.display_name || "ACHAT COMPTANT - " + (opts.name || "TEST"),
    value: opts.value ?? -100,
    currency: { symbol: "€" },
    category: { name: opts.category || "fees" },
  };
}

test("extractTradeMoves: filters to market_order and sorts most recent first", () => {
  const snapshot = {
    transactions: [
      syntheticTrade({ date: "2026-05-10T00:00:00Z", name: "A" }),
      syntheticTrade({ date: "2026-05-15T00:00:00Z", name: "B" }),
      // A bank transaction that must be skipped (no market_order, no
      // investment category, no trade prefix).
      { date: "2026-05-12T00:00:00Z", transaction_type: "card", category: { name: "leisure" }, display_name: "GROCERY", value: -50 },
      syntheticTrade({ date: "2026-05-14T00:00:00Z", name: "C" }),
    ],
  };
  const trades = extractTradeMoves(snapshot, 5);
  assert.equal(trades.length, 3);
  assert.equal(trades[0].security_name, "B"); // most recent
  assert.equal(trades[1].security_name, "C");
  assert.equal(trades[2].security_name, "A");
});

test("extractTradeMoves: respects the limit parameter", () => {
  const snapshot = {
    transactions: Array.from({ length: 10 }, (_, i) =>
      syntheticTrade({ date: `2026-05-${10 + i}T00:00:00Z`, name: `T${i}` }),
    ),
  };
  const trades = extractTradeMoves(snapshot, 5);
  assert.equal(trades.length, 5);
});

test("extractTradeMoves: returns [] on null/empty snapshot", () => {
  assert.deepEqual(extractTradeMoves(null), []);
  assert.deepEqual(extractTradeMoves({}), []);
  assert.deepEqual(extractTradeMoves({ transactions: [] }), []);
});

test("extractTradeMoves: includes orders[] when populated (defensive)", () => {
  const snapshot = {
    orders: [
      { date: "2026-05-15T00:00:00Z", display_name: "VENTE - MC", value: 1000, currency: { symbol: "€" }, category: { name: "investment" } },
    ],
    transactions: [],
  };
  const trades = extractTradeMoves(snapshot, 5);
  assert.equal(trades.length, 1);
  assert.equal(trades[0].raw_action, "sell");
});

test("extractTradeMoves: investment category without market_order still counts", () => {
  const snapshot = {
    transactions: [{
      date: "2026-05-15T00:00:00Z",
      transaction_type: "transfer",
      category: { name: "investment" },
      display_name: "ACHAT TOTALENERGIES",
      value: -200,
      currency: { symbol: "€" },
    }],
  };
  const trades = extractTradeMoves(snapshot, 5);
  assert.equal(trades.length, 1, "investment category alone must surface as a trade");
});

test("extractTradeMoves: skips rows without a date", () => {
  const snapshot = {
    transactions: [
      syntheticTrade({ date: "", name: "NO_DATE" }),
      syntheticTrade({ date: "2026-05-15T00:00:00Z", name: "OK" }),
    ],
  };
  const trades = extractTradeMoves(snapshot, 5);
  assert.equal(trades.length, 1);
  assert.equal(trades[0].security_name, "OK");
});

// ── P1-22: formatTradeRow ───────────────────────────────────────────

test("formatTradeRow: buy with negative value and French date", () => {
  const trade = {
    date: "2026-05-14T00:00:00Z",
    raw_action: "buy",
    security_name: "THERMADOR",
    value: -139.63,
    currency_symbol: "€",
  };
  const line = formatTradeRow(trade);
  assert.match(line, /^Tu as renforcé THERMADOR le 14 mai · -139,63 €$/);
});

test("formatTradeRow: sell with positive value", () => {
  const trade = {
    date: "2026-05-10T00:00:00Z",
    raw_action: "sell",
    security_name: "LVMH",
    value: 443.01,
    currency_symbol: "€",
  };
  const line = formatTradeRow(trade);
  assert.match(line, /^Tu as allégé LVMH le 10 mai · \+443,01 €$/);
});

test("formatTradeRow: dividend uses the encaissé verb", () => {
  const trade = {
    date: "2026-05-12T00:00:00Z",
    raw_action: "dividend",
    security_name: "TOTAL",
    value: 12.4,
    currency_symbol: "€",
  };
  const line = formatTradeRow(trade);
  assert.match(line, /^Tu as encaissé un dividende TOTAL le 12 mai · \+12,40 €$/);
});

test("formatTradeRow: 'other' action gets a generic verb", () => {
  const trade = {
    date: "2026-05-12T00:00:00Z",
    raw_action: "other",
    security_name: "STM",
    value: -50,
    currency_symbol: "€",
  };
  assert.match(formatTradeRow(trade), /^Tu as effectué un ordre sur STM le 12 mai/);
});

test("formatTradeRow: handles unknown date gracefully", () => {
  const trade = {
    date: "not-a-date",
    raw_action: "buy",
    security_name: "STM",
    value: -50,
    currency_symbol: "€",
  };
  assert.match(formatTradeRow(trade), /Tu as renforcé STM le not-a-date/);
});
