import test from "node:test";
import assert from "node:assert/strict";
import { buildGlobalPortfolioSynthesis } from "../src/desktop-shell/global-portfolio-synthesis.js";

test("buildGlobalPortfolioSynthesis: computes totals and concentration verdict", () => {
  const result = buildGlobalPortfolioSynthesis({
    latest_finary_snapshot: {
      accounts: [
        { name: "PEA", total_value: 70000, cash: 5000, total_gain: 9000 },
        { name: "CTO", total_value: 30000, cash: 1000, total_gain: 2000 },
      ]
    },
    latest_run: {
      portfolio: {
        positions: [
          { compte: "PEA", ticker: "CW8", nom: "World ETF", montant: 40000 },
          { compte: "PEA", ticker: "MC", nom: "LVMH", montant: 25000 },
          { compte: "CTO", ticker: "TTE", nom: "TotalEnergies", montant: 20000 },
        ]
      }
    }
  });

  assert.equal(result.accountCount, 2);
  assert.equal(result.totalValue, 100000);
  assert.equal(result.totalCash, 6000);
  assert.equal(result.verdict, "High concentration risk");
  assert.ok(result.suggestions.length > 0);
});

test("buildGlobalPortfolioSynthesis: flags support concentration", () => {
  const result = buildGlobalPortfolioSynthesis({
    latest_run: {
      portfolio: {
        positions: [
          { ticker: "AAPL", type: "stock", montant: 9000 },
          { ticker: "MSFT", type: "stock", montant: 7000 },
          { ticker: "BOND", type: "bond", montant: 1000 },
        ]
      }
    }
  });

  const topSupport = result.supportBreakdown[0];
  assert.equal(topSupport.name, "Stocks");
  assert.ok(topSupport.weightPct > 90);
  assert.ok(result.suggestions.some((s) => s.includes("support types")));
});

// ── P0-19: supportBreakdown fallback to Finary accounts ─────────────

test("P0-19: supportBreakdown falls back to Finary accounts when latest_run.positions is empty", () => {
  // Reproduces the home "Top support: Not enough data" bug. Pierre's
  // home showed that label even though Finary had complete account data.
  const result = buildGlobalPortfolioSynthesis({
    latest_finary_snapshot: {
      accounts: [
        { name: "PEA Bourse", total_value: 50000, cash: 3000, total_gain: 4000 },
        { name: "CTO Degiro", total_value: 20000, cash: 500, total_gain: 1000 },
      ],
    },
    latest_run: { portfolio: { positions: [] } },
  });

  assert.ok(result.supportBreakdown.length > 0, "supportBreakdown must be non-empty (no more 'Not enough data')");
  const cashBucket = result.supportBreakdown.find((b) => b.name === "Cash");
  assert.ok(cashBucket, "Cash bucket must surface");
  assert.equal(cashBucket.value, 3500, "Cash bucket aggregates both accounts' cash");
  const stocksBucket = result.supportBreakdown.find((b) => b.name === "Stocks");
  assert.ok(stocksBucket, "Stocks bucket (PEA/CTO default) must surface");
  assert.equal(stocksBucket.value, 50000 - 3000 + 20000 - 500, "Stocks bucket = sum of (total_value - cash) per account");
});

test("P0-19: fallback handles cash-only accounts (total_value == cash)", () => {
  // Livret A scenario — pure cash, no invested portion.
  const result = buildGlobalPortfolioSynthesis({
    latest_finary_snapshot: {
      accounts: [{ name: "Livret A", total_value: 12000, cash: 12000, total_gain: 0 }],
    },
    latest_run: { portfolio: { positions: [] } },
  });

  assert.equal(result.supportBreakdown.length, 1, "only Cash bucket — no invested slice when total_value == cash");
  assert.equal(result.supportBreakdown[0].name, "Cash");
  assert.equal(result.supportBreakdown[0].value, 12000);
});

test("P0-19: fallback does NOT trigger when latest_run has valid positions (non-regression)", () => {
  // Sanity: the existing Finary→positions path keeps working. Finary
  // accounts are still present (they always are in production) but
  // because the run has valid invested value, the fallback branch
  // must NOT activate — otherwise we'd double-count.
  const result = buildGlobalPortfolioSynthesis({
    latest_finary_snapshot: {
      accounts: [{ name: "PEA", total_value: 50000, cash: 0, total_gain: 4000 }],
    },
    latest_run: {
      portfolio: {
        positions: [
          { compte: "PEA", ticker: "CW8", nom: "World ETF", montant: 30000 },
          { compte: "PEA", ticker: "MC", nom: "LVMH", montant: 20000 },
        ],
      },
    },
  });

  // Total invested from run positions = 50000, all "ETFs/Funds" or "Stocks".
  // No Finary fallback double-count.
  const totalSupportValue = result.supportBreakdown.reduce((s, b) => s + b.value, 0);
  assert.equal(totalSupportValue, 50000, "fallback must not double-count when run has invested positions");
});

test("P0-19: fallback no-op when both run and Finary accounts are empty (degenerate)", () => {
  const result = buildGlobalPortfolioSynthesis({
    latest_finary_snapshot: { accounts: [] },
    latest_run: { portfolio: { positions: [] } },
  });

  assert.equal(result.supportBreakdown.length, 0, "supportBreakdown stays empty when no data anywhere");
});
