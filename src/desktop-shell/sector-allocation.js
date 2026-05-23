/**
 * Sector allocation aggregation + label resolution (P1-57).
 *
 * Aggregates portfolio value by GICS sector slug for the home widget
 * "Allocation sectorielle" and resolves slugs to French display labels for
 * both the home widget and the line-modal chip.
 *
 * Sector slugs flow from `/api/sector` (alfred-api) → enriched into
 * `run_state.market[ticker].sector` by `mcp_server::tool_get_line_data`
 * (Rust, persisted via `apply_sector_to_market`). The home widget reads
 * `latest_run.market[ticker].sector` for each position and aggregates.
 *
 * Voice convention — tutoiement FR, factuel ou actionnable
 * (`docs/launch-comms-linkedin-v3.md`). Labels mirror the GICS standard
 * but rendered in French Pierre uses naturally.
 */

// French labels for the canonical GICS sectors observed in production runs.
// Slugs come from `apps/alfred-api/src/sector.rs` static mapping (CAC40, DAX,
// major US) and SearXNG fallback. Pinned by `sectorLabel returns FR for known
// slugs` test in `test/sector-allocation.test.js`.
const SECTOR_LABELS = {
  energy: "Énergie",
  financials: "Finance",
  finance: "Finance",
  tech: "Tech",
  information_technology: "Tech",
  healthcare: "Santé",
  health_care: "Santé",
  consumer_discretionary: "Consommation discrétionnaire",
  consumer_staples: "Consommation de base",
  industrials: "Industrie",
  materials: "Matériaux",
  utilities: "Services publics",
  communication_services: "Communications",
  communication: "Communications",
  real_estate: "Immobilier",
};

/**
 * French label for a GICS sector slug. Returns "Autre" for empty/null,
 * the curated label for a known slug, or a humanised fallback (snake_case
 * to Title Case) for an unknown slug. Pure, no I/O.
 */
export function sectorLabel(slug) {
  if (slug === null || slug === undefined) return "Autre";
  const raw = String(slug).trim();
  if (!raw) return "Autre";
  const key = raw.toLowerCase();
  if (Object.prototype.hasOwnProperty.call(SECTOR_LABELS, key)) {
    return SECTOR_LABELS[key];
  }
  // Unknown slug — humanise snake_case → Title Case. Keeps the home from
  // showing cryptic underscores when alfred-api emits a sector we haven't
  // curated yet.
  return key
    .replace(/_/g, " ")
    .split(/\s+/)
    .filter((w) => w.length > 0)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join(" ");
}

/**
 * Extract the sector slug for a ticker from a snapshot. Reads
 * `latest_run.market[ticker].sector` first (authoritative — written by
 * Rust during enrichment), then falls back to a reco match
 * (`composed_payload.recommandations[i].sector`) for back-compat when the
 * sector hasn't been persisted to market yet.
 *
 * Pure helper, exported for unit tests.
 */
export function lookupSectorForTicker(snapshot, ticker) {
  const t = String(ticker || "").trim();
  if (!t) return "";
  const latestRun = snapshot?.latest_run || {};
  const tickerUpper = t.toUpperCase();
  const market = latestRun.market || {};
  const marketEntry = market[tickerUpper] || market[t.toLowerCase()] || market[t] || null;
  const marketSector = marketEntry && typeof marketEntry === "object"
    ? String(marketEntry.sector || "").trim()
    : "";
  if (marketSector) return marketSector;

  // Fallback : recos may carry sector when persisted in composed_payload.
  const recos = Array.isArray(latestRun?.composed_payload?.recommandations)
    ? latestRun.composed_payload.recommandations
    : Array.isArray(latestRun?.composed_payload?.recommendations)
      ? latestRun.composed_payload.recommendations
      : [];
  for (const r of recos) {
    if (!r || typeof r !== "object") continue;
    const rt = String(r.ticker || "").trim().toUpperCase();
    if (rt === tickerUpper) {
      const s = String(r.sector || "").trim();
      if (s) return s;
    }
  }
  return "";
}

function positionValue(pos) {
  // Same field precedence the home widget uses (valeur_actuelle > valorisation
  // > quantite*prix_actuel). Returns 0 for sectorless empty positions so the
  // "other" bucket doesn't inflate from missing data.
  if (!pos || typeof pos !== "object") return 0;
  const candidates = [pos.valeur_actuelle, pos.valorisation];
  for (const c of candidates) {
    if (typeof c === "number" && Number.isFinite(c) && c > 0) return c;
  }
  const qty = typeof pos.quantite === "number" ? pos.quantite : 0;
  const price = typeof pos.prix_actuel === "number" ? pos.prix_actuel : 0;
  const derived = qty * price;
  return Number.isFinite(derived) && derived > 0 ? derived : 0;
}

/**
 * Aggregate portfolio value by GICS sector slug.
 *
 * Input : snapshot in the shape produced by `latestDashboardPayload.snapshot`
 * (`{ latest_run: { portfolio: { positions }, market, composed_payload } }`).
 * Tolerates missing/partial snapshots — returns `[]` when there's nothing
 * to aggregate.
 *
 * Returns : `[{ sector_slug, label, total_value, weight_pct, ticker_count }]`
 * sorted by `total_value` descending. Positions with no sector resolved
 * land in an "other" bucket with `sector_slug: ""`. Weight is computed
 * against the aggregated total (sum across all buckets), not the raw
 * portfolio total — keeps weights to 100 % for the buckets shown.
 *
 * Pure, no DOM, no globals.
 */
export function aggregateBySector(snapshot) {
  const latestRun = snapshot?.latest_run || {};
  const positions = Array.isArray(latestRun?.portfolio?.positions)
    ? latestRun.portfolio.positions
    : [];
  if (positions.length === 0) return [];

  const buckets = new Map(); // slug → { sector_slug, total_value, ticker_count }
  for (const pos of positions) {
    const ticker = String(pos?.ticker || "").trim();
    if (!ticker) continue;
    const value = positionValue(pos);
    if (value <= 0) continue;
    const slug = lookupSectorForTicker(snapshot, ticker);
    const key = slug || ""; // empty string is the "other" bucket
    if (!buckets.has(key)) {
      buckets.set(key, { sector_slug: key, total_value: 0, ticker_count: 0 });
    }
    const b = buckets.get(key);
    b.total_value += value;
    b.ticker_count += 1;
  }

  if (buckets.size === 0) return [];

  const totalValue = Array.from(buckets.values()).reduce((sum, b) => sum + b.total_value, 0);
  const rows = Array.from(buckets.values()).map((b) => ({
    sector_slug: b.sector_slug,
    label: b.sector_slug ? sectorLabel(b.sector_slug) : "Autre",
    total_value: b.total_value,
    weight_pct: totalValue > 0 ? (b.total_value / totalValue) * 100 : 0,
    ticker_count: b.ticker_count,
  }));

  rows.sort((a, b) => b.total_value - a.total_value);
  return rows;
}

/**
 * Test-only export — read by `test/sector-allocation.test.js` to pin the
 * curated label dictionary against unintentional drift.
 */
export const SECTOR_LABELS_FOR_TEST = SECTOR_LABELS;
