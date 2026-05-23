import test from "node:test";
import assert from "node:assert/strict";
import {
  aggregateBySector,
  sectorLabel,
  lookupSectorForTicker,
  SECTOR_LABELS_FOR_TEST,
} from "../src/desktop-shell/sector-allocation.js";

// ── P1-57 : sectorLabel ──────────────────────────────────────────────

test("sectorLabel: returns French label for known GICS slugs", () => {
  assert.equal(sectorLabel("energy"), "Énergie");
  assert.equal(sectorLabel("financials"), "Finance");
  assert.equal(sectorLabel("tech"), "Tech");
  assert.equal(sectorLabel("information_technology"), "Tech");
  assert.equal(sectorLabel("healthcare"), "Santé");
  assert.equal(sectorLabel("health_care"), "Santé");
  assert.equal(sectorLabel("consumer_discretionary"), "Consommation discrétionnaire");
  assert.equal(sectorLabel("consumer_staples"), "Consommation de base");
  assert.equal(sectorLabel("industrials"), "Industrie");
  assert.equal(sectorLabel("materials"), "Matériaux");
  assert.equal(sectorLabel("utilities"), "Services publics");
  assert.equal(sectorLabel("communication_services"), "Communications");
  assert.equal(sectorLabel("real_estate"), "Immobilier");
});

test("sectorLabel: case-insensitive and trims whitespace", () => {
  assert.equal(sectorLabel("  ENERGY  "), "Énergie");
  assert.equal(sectorLabel("TECH"), "Tech");
  assert.equal(sectorLabel("Health_Care"), "Santé");
});

test("sectorLabel: humanises unknown slugs (snake_case → Title Case)", () => {
  assert.equal(sectorLabel("biotech_growth"), "Biotech Growth");
  assert.equal(sectorLabel("emerging_markets"), "Emerging Markets");
  assert.equal(sectorLabel("ai"), "Ai");
});

test("sectorLabel: returns 'Autre' on empty / null / non-string inputs", () => {
  assert.equal(sectorLabel(""), "Autre");
  assert.equal(sectorLabel("   "), "Autre");
  assert.equal(sectorLabel(null), "Autre");
  assert.equal(sectorLabel(undefined), "Autre");
});

test("SECTOR_LABELS_FOR_TEST covers all common GICS top-level sectors", () => {
  // Required slugs from alfred-api static mapping (sector.rs) — keeps the dict
  // in sync with what the server emits.
  const required = [
    "energy",
    "financials",
    "tech",
    "healthcare",
    "consumer_discretionary",
    "consumer_staples",
    "industrials",
    "materials",
    "utilities",
    "communication_services",
    "real_estate",
  ];
  for (const slug of required) {
    assert.ok(
      Object.prototype.hasOwnProperty.call(SECTOR_LABELS_FOR_TEST, slug),
      `dict must cover '${slug}'`,
    );
  }
});

// ── P1-57 : lookupSectorForTicker ────────────────────────────────────

test("lookupSectorForTicker: reads market[ticker].sector first", () => {
  const snapshot = {
    latest_run: {
      market: {
        AAPL: { sector: "tech", prix_actuel: 150 },
      },
    },
  };
  assert.equal(lookupSectorForTicker(snapshot, "AAPL"), "tech");
  // Case-insensitive
  assert.equal(lookupSectorForTicker(snapshot, "aapl"), "tech");
});

test("lookupSectorForTicker: falls back to composed_payload.recommandations[].sector", () => {
  const snapshot = {
    latest_run: {
      market: {}, // no sector in market
      composed_payload: {
        recommandations: [
          { ticker: "AAPL", sector: "tech" },
          { ticker: "TTE", sector: "energy" },
        ],
      },
    },
  };
  assert.equal(lookupSectorForTicker(snapshot, "AAPL"), "tech");
  assert.equal(lookupSectorForTicker(snapshot, "TTE"), "energy");
});

test("lookupSectorForTicker: returns empty string when no source has it", () => {
  const snapshot = {
    latest_run: { market: {}, composed_payload: { recommandations: [] } },
  };
  assert.equal(lookupSectorForTicker(snapshot, "AAPL"), "");
  assert.equal(lookupSectorForTicker({}, "AAPL"), "");
  assert.equal(lookupSectorForTicker(null, "AAPL"), "");
  assert.equal(lookupSectorForTicker(snapshot, ""), "");
});

// ── P1-57 : aggregateBySector ────────────────────────────────────────

test("aggregateBySector: groups by slug, sums values, sorts desc", () => {
  const snapshot = {
    latest_run: {
      market: {
        AAPL: { sector: "tech" },
        MSFT: { sector: "tech" },
        TTE: { sector: "energy" },
        JPM: { sector: "financials" },
      },
      portfolio: {
        positions: [
          { ticker: "AAPL", valeur_actuelle: 5000 },
          { ticker: "MSFT", valeur_actuelle: 3000 },
          { ticker: "TTE", valeur_actuelle: 1500 },
          { ticker: "JPM", valeur_actuelle: 500 },
        ],
      },
    },
  };
  const rows = aggregateBySector(snapshot);
  assert.equal(rows.length, 3);
  // tech (8000), energy (1500), financials (500)
  assert.equal(rows[0].sector_slug, "tech");
  assert.equal(rows[0].label, "Tech");
  assert.equal(rows[0].total_value, 8000);
  assert.equal(rows[0].ticker_count, 2);
  assert.equal(rows[1].sector_slug, "energy");
  assert.equal(rows[1].total_value, 1500);
  assert.equal(rows[2].sector_slug, "financials");
  assert.equal(rows[2].total_value, 500);
  // Weights sum to 100
  const totalWeight = rows.reduce((s, r) => s + r.weight_pct, 0);
  assert.ok(Math.abs(totalWeight - 100) < 0.01, `weights must sum to 100 (got ${totalWeight})`);
});

test("aggregateBySector: adds 'other' bucket for sectorless positions", () => {
  const snapshot = {
    latest_run: {
      market: {
        AAPL: { sector: "tech" },
        // MSFT has no sector
      },
      portfolio: {
        positions: [
          { ticker: "AAPL", valeur_actuelle: 5000 },
          { ticker: "MSFT", valeur_actuelle: 2000 },
        ],
      },
    },
  };
  const rows = aggregateBySector(snapshot);
  assert.equal(rows.length, 2);
  const other = rows.find((r) => r.sector_slug === "");
  assert.ok(other, "must have an 'other' bucket");
  assert.equal(other.label, "Autre");
  assert.equal(other.total_value, 2000);
  assert.equal(other.ticker_count, 1);
});

test("aggregateBySector: returns [] on empty / missing snapshot", () => {
  assert.deepEqual(aggregateBySector(null), []);
  assert.deepEqual(aggregateBySector({}), []);
  assert.deepEqual(aggregateBySector({ latest_run: {} }), []);
  assert.deepEqual(
    aggregateBySector({ latest_run: { portfolio: { positions: [] } } }),
    [],
  );
});

test("aggregateBySector: derives value from quantite * prix_actuel when valeur_actuelle missing", () => {
  const snapshot = {
    latest_run: {
      market: { AAPL: { sector: "tech" } },
      portfolio: {
        positions: [{ ticker: "AAPL", quantite: 10, prix_actuel: 150 }],
      },
    },
  };
  const rows = aggregateBySector(snapshot);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].total_value, 1500);
});

test("aggregateBySector: skips positions with zero or negative value", () => {
  const snapshot = {
    latest_run: {
      market: { AAPL: { sector: "tech" }, MSFT: { sector: "tech" } },
      portfolio: {
        positions: [
          { ticker: "AAPL", valeur_actuelle: 0 },
          { ticker: "MSFT", valeur_actuelle: 5000 },
          { ticker: "", valeur_actuelle: 1000 }, // empty ticker
        ],
      },
    },
  };
  const rows = aggregateBySector(snapshot);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].total_value, 5000);
  assert.equal(rows[0].ticker_count, 1);
});

test("aggregateBySector: respects recommandations[] fallback when market.sector missing", () => {
  // Captures the production scenario where market enrichment populated
  // sector but ALSO recos carry it as backup.
  const snapshot = {
    latest_run: {
      market: { AAPL: {} }, // no sector
      composed_payload: {
        recommandations: [{ ticker: "AAPL", sector: "tech" }],
      },
      portfolio: {
        positions: [{ ticker: "AAPL", valeur_actuelle: 1000 }],
      },
    },
  };
  const rows = aggregateBySector(snapshot);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].sector_slug, "tech");
});
