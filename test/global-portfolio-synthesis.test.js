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
