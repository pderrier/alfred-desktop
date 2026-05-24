/**
 * Macro briefing home tile formatter (P2-70).
 *
 * Renders a one-line FR-tutoiement summary of the global macro snapshot
 * (US 10Y / VIX / EUR-USD / Brent) for the home page pre-run. The same
 * data is also injected at the top of the synthesis prompt by
 * `macro_briefing::build_macro_briefing_section` (Rust) — this module is
 * the retail-user view of the same snapshot so paid users can see the
 * macro context exists before running an analysis.
 *
 * Voice — tutoiement FR, factuel, glanceable. The "🌍 Contexte macro :"
 * prefix gives a visual hook ; segments are separated by `·` to match
 * other home sections (`globalSynthesisCard`, `sectorAllocationCard`).
 * Total length stays ≤ 90 chars to fit one line at the default home
 * container width (480 px max-width).
 *
 * Envelope shape from `/api/macro` (alfred-api `macro_handler`) :
 *   { ok: true,
 *     macro: { us_10y_yield: {value, as_of} | null,
 *              vix:          {value, as_of} | null,
 *              eur_usd:      {value, as_of} | null,
 *              brent_usd:    {value, as_of} | null } | null,
 *     cache_hit, source, as_of }
 *
 * The whole `macro` field is null when the server returned 503 or the
 * desktop wrapper degraded silently. Individual indicators may be null
 * when a single Yahoo symbol failed but at least one other succeeded.
 *
 * VIX bucket thresholds mirror `vix_bucket_label` in
 * `apps/alfred-desktop/src-tauri/src/services/macro_briefing.rs:140` :
 * <15 faible, 15–25 modérée, 25–35 élevée, ≥35 stress. The Rust side
 * renders the prefixed form "volatilité modérée" / "régime de stress" ;
 * the home tile uses the short bucket name to keep the line compact.
 */

const VIX_BUCKET_LOW = 15;
const VIX_BUCKET_MID = 25;
const VIX_BUCKET_HIGH = 35;

/**
 * Short VIX regime label for the home tile. Boundaries match the Rust
 * `vix_bucket_label` in `services/macro_briefing.rs` (inclusive on the
 * low side: 15 → modérée, 25 → élevée, 35 → stress). Returns one of
 * "faible" / "modérée" / "élevée" / "stress" — accents preserved for
 * display.
 */
export function vixBucketLabel(value) {
  // `Number(null)` is `0` (a finite number), which would silently classify
  // a missing indicator as "faible". Reject non-numeric inputs explicitly
  // before the bucket comparison.
  if (typeof value !== "number" || !Number.isFinite(value)) return "";
  if (value < VIX_BUCKET_LOW) return "faible";
  if (value < VIX_BUCKET_MID) return "modérée";
  if (value < VIX_BUCKET_HIGH) return "élevée";
  return "stress";
}

/**
 * Extract a numeric indicator value from the `{value, as_of}` shape.
 * Returns `null` when the field is absent, the object is null, the
 * value is not a finite number, or the field itself is shaped
 * incorrectly. Pure helper.
 */
function readIndicator(briefing, field) {
  if (!briefing || typeof briefing !== "object") return null;
  const entry = briefing[field];
  if (!entry || typeof entry !== "object") return null;
  const v = Number(entry.value);
  return Number.isFinite(v) ? v : null;
}

/**
 * Format the macro briefing envelope into a single-line FR string for
 * the home tile, or return `null` when no usable indicator is present
 * (envelope missing, `macro` null, or every indicator null).
 *
 * The function is tolerant of partial data : each missing indicator
 * simply drops its segment. The "🌍 Contexte macro :" prefix is only
 * emitted when at least one segment renders, so we never show an empty
 * tile with just the prefix.
 *
 * @param {object} envelope - The `/api/macro` envelope (or the bridge
 *   payload after `normalizeTauriPayload` unwraps `result`).
 * @returns {string | null}
 */
export function formatMacroTile(envelope) {
  if (!envelope || typeof envelope !== "object") return null;
  const briefing = envelope.macro;
  if (!briefing || typeof briefing !== "object") return null;

  const segments = [];

  const us10y = readIndicator(briefing, "us_10y_yield");
  if (us10y !== null) {
    segments.push(`10Y ${us10y.toFixed(2)} %`);
  }

  const vix = readIndicator(briefing, "vix");
  if (vix !== null) {
    const bucket = vixBucketLabel(vix);
    segments.push(bucket ? `VIX ${vix.toFixed(1)} (${bucket})` : `VIX ${vix.toFixed(1)}`);
  }

  const eurusd = readIndicator(briefing, "eur_usd");
  if (eurusd !== null) {
    segments.push(`EUR/USD ${eurusd.toFixed(3)}`);
  }

  const brent = readIndicator(briefing, "brent_usd");
  if (brent !== null) {
    segments.push(`Brent ${brent.toFixed(2)} $`);
  }

  if (segments.length === 0) return null;
  return `🌍 Contexte macro : ${segments.join(" · ")}`;
}
