/**
 * Theme labels — French translations for the `news_themes` slug
 * vocabulary used in line-memory aggregation (`composed_payload.theme_concentration`).
 *
 * P0-20 (2026-05-23) — first delivery is the manual dictionary below.
 * P1-23 (deferred) will add a `/themes/describe` LLM-cached endpoint
 * for slugs not covered here, with results merged via
 * `theme-labels-fetcher.js`.
 *
 * Curation source — the slugs below are derived from :
 *   - Production test fixtures in `apps/alfred-desktop/src-tauri/src/tests.rs`
 *     (`["tariffs", "margin_expansion", "design_win", "auto_cycle",
 *     "tesla", "infineon", "tech", "macro"]`).
 *   - Common French finance vocabulary Pierre uses in his runs.
 *   - The `news_themes` write path in
 *     `apps/alfred-desktop/src-tauri/src/main.rs` (chat-wizard checkbox UI).
 *
 * Voice — tutoiement FR, factuel ou actionnable. Reference :
 * `docs/launch-comms-linkedin-v3.md`. Each blurb is one short sentence
 * (≤ 100 chars) that surfaces WHY this theme matters for the position.
 */

const MANUAL_DICT = {
  // ── Macro / cycle ────────────────────────────────────────────────
  tariffs: {
    label: "Tarifs douaniers",
    blurb: "Sensible aux décisions US-Chine et au cycle macro mondial.",
  },
  macro: {
    label: "Cycle macro",
    blurb: "Exposition aux taux, inflation, croissance globale.",
  },
  inflation: {
    label: "Inflation",
    blurb: "Pricing power et coûts d'intrants sous tension.",
  },
  rate_hike: {
    label: "Hausse des taux",
    blurb: "Coût du capital qui pèse sur les valorisations et la dette.",
  },
  recession_risk: {
    label: "Risque de récession",
    blurb: "Volumes et marges menacés si la demande recule.",
  },
  fx: {
    label: "Effets de change",
    blurb: "Exposition aux devises étrangères dans le chiffre d'affaires.",
  },

  // ── Fundamentals ─────────────────────────────────────────────────
  margin_expansion: {
    label: "Expansion de marges",
    blurb: "Pricing power validé sur les derniers trimestres.",
  },
  margin_compression: {
    label: "Compression de marges",
    blurb: "Coûts qui montent plus vite que les prix de vente.",
  },
  pricing_power: {
    label: "Pricing power",
    blurb: "Capacité à augmenter les prix sans perdre de volume.",
  },
  free_cash_flow: {
    label: "Free cash flow",
    blurb: "Génération de liquidités forte, base solide pour le retour actionnaire.",
  },
  debt_load: {
    label: "Endettement élevé",
    blurb: "Bilan tendu, sensibilité accrue aux taux et au refinancement.",
  },
  buyback: {
    label: "Rachats d'actions",
    blurb: "Programme de rachat actif, soutient le BNA par titre.",
  },
  dividend: {
    label: "Dividende",
    blurb: "Politique de distribution suivie de près par les holders.",
  },

  // ── Sector / industry ────────────────────────────────────────────
  tech: {
    label: "Tech",
    blurb: "Cyclique sur la demande software et hardware.",
  },
  ai: {
    label: "IA",
    blurb: "Surfe sur l'investissement infrastructure et applications.",
  },
  semiconductors: {
    label: "Semi-conducteurs",
    blurb: "Cycle d'offre/demande long, exposé à la géopolitique.",
  },
  auto_cycle: {
    label: "Cycle automobile",
    blurb: "Sensible aux ventes véhicules neufs et à la transition EV.",
  },
  ev_transition: {
    label: "Transition électrique",
    blurb: "Investissements lourds, pression compétitive de la Chine.",
  },
  energy_transition: {
    label: "Transition énergétique",
    blurb: "Politique publique + capex long terme drivers principaux.",
  },
  defense: {
    label: "Défense",
    blurb: "Cycle budgétaire OTAN, contrats pluriannuels visibles.",
  },
  luxury: {
    label: "Luxe",
    blurb: "Consommation discrétionnaire haut de gamme, exposition Asie.",
  },
  banking: {
    label: "Banques",
    blurb: "Marge nette d'intérêt et qualité du portefeuille de prêts.",
  },
  insurance: {
    label: "Assurance",
    blurb: "Combined ratio et rendement du portefeuille obligataire.",
  },
  pharma: {
    label: "Pharma",
    blurb: "Pipeline produits + risque brevets / FDA.",
  },
  retail: {
    label: "Distribution",
    blurb: "Trafic en magasin et marge brute sous pression.",
  },
  oil_gas: {
    label: "Pétrole & gaz",
    blurb: "Prix du baril et discipline capex du secteur.",
  },

  // ── Catalysts / events ───────────────────────────────────────────
  design_win: {
    label: "Design win",
    blurb: "Nouveau contrat client, revenu récurrent à plusieurs trimestres.",
  },
  guidance_raise: {
    label: "Guidance relevée",
    blurb: "Direction confiante, consensus probablement à ajuster.",
  },
  guidance_cut: {
    label: "Guidance abaissée",
    blurb: "Direction prudente, attentes à recalibrer à la baisse.",
  },
  m_and_a: {
    label: "M&A",
    blurb: "Opération de croissance externe en cours ou attendue.",
  },
  earnings_beat: {
    label: "Résultats au-dessus",
    blurb: "Trimestre supérieur au consensus sur revenus et/ou BNA.",
  },
  earnings_miss: {
    label: "Résultats en-dessous",
    blurb: "Trimestre inférieur au consensus, réaction marché négative.",
  },
  activist: {
    label: "Activiste",
    blurb: "Fonds activiste au capital, pression sur la gouvernance.",
  },
};

/**
 * Resolve a `news_themes` slug to its French label + blurb.
 * Returns `{label, blurb, pending}` :
 *   - `pending: false` when the dictionary has an entry.
 *   - `pending: true` when we humanize the slug as a fallback (the
 *     blurb is null and the home renders the label alone). P1-23 will
 *     populate this gap via the LLM-cached `/themes/describe` endpoint.
 */
export function getThemeLabel(slug) {
  if (!slug || typeof slug !== "string") {
    return { label: "Thème inconnu", blurb: null, pending: false };
  }
  const key = slug.trim().toLowerCase();
  if (!key) return { label: "Thème inconnu", blurb: null, pending: false };
  if (Object.prototype.hasOwnProperty.call(MANUAL_DICT, key)) {
    return { ...MANUAL_DICT[key], pending: false };
  }
  return { label: humanizeSlug(key), blurb: null, pending: true };
}

/**
 * Format an unknown slug into a readable display string. Falls back
 * gracefully when the manual dict doesn't cover the slug — P1-23 will
 * replace this with a fetched LLM label, but the humanised form is the
 * safe default in the meantime.
 *
 * Example : `margin_expansion_q3_2026` → `Margin Expansion Q3 2026`.
 */
export function humanizeSlug(slug) {
  return String(slug || "")
    .replace(/_/g, " ")
    .trim()
    .split(/\s+/)
    .map((w) => (w.length > 0 ? w[0].toUpperCase() + w.slice(1) : w))
    .join(" ");
}

/**
 * Test-only export — read by `apps/alfred-desktop/test/theme-labels.test.js`.
 * Lets the test exercise dictionary coverage without re-declaring the
 * keys (any drift between code and test would be noisy).
 */
export const MANUAL_DICT_FOR_TEST = MANUAL_DICT;
