// ── Shared text utilities (deduplicated from 3+ modules) ────────

export function asText(value, fallback = "") {
  const normalized = String(value || "").trim();
  return normalized || fallback;
}

export function asNumber(value, fallback = null) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return fallback;
}

export function escapeHtml(text) {
  return String(text || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

export function truncate(text, max = 120) {
  const s = String(text || "");
  return s.length > max ? s.slice(0, max) + "\u2026" : s;
}

// ── Formatting ──────────────────────────────────────────────────

export function formatCurrency(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return "—";
  }
  return new Intl.NumberFormat("fr-FR", {
    style: "currency",
    currency: "EUR",
    maximumFractionDigits: 2
  }).format(value);
}

export function mergeEvents(snapshotEvents = [], uiEvents = []) {
  const merged = [...(Array.isArray(snapshotEvents) ? snapshotEvents : []), ...(Array.isArray(uiEvents) ? uiEvents : [])];
  merged.sort((a, b) => String(b?.ts || "").localeCompare(String(a?.ts || "")));
  return merged.slice(0, 50);
}

/**
 * Single source of truth mapping a recommendation signal/verdict string to a
 * CSS tone class. Used by every signal-display site (action cards, the
 * positions/recommendations table badge, the live-run badge) so the tone
 * mapping cannot drift between them.
 *
 * Watchlist Curation v2 (D1): the watchlist verdict vocabulary
 * (ENTRER | ACHAT_SUR_REPLI | SURVEILLER | ECARTER) maps to dedicated tones:
 *   - ENTRER / ACHAT_SUR_REPLI → "tone-entry"  (validated opportunity)
 *   - ECARTER                  → "tone-discard" (rejected proposal)
 *   - SURVEILLER               → "tone-neutral"
 * These exact-match checks run BEFORE the held-position substring checks so
 * ACHAT_SUR_REPLI is not swallowed by the generic "ACHAT" → tone-buy rule and
 * ECARTER never falls through to a buy/sell tone.
 */
export function signalToneClass(signal) {
  const s = String(signal || "").toUpperCase();
  if (s === "ECARTER") return "tone-discard";
  if (s === "ENTRER" || s === "ACHAT_SUR_REPLI") return "tone-entry";
  if (s === "SURVEILLER") return "tone-neutral";
  if (s.includes("ACHAT") || s.includes("ACHETER") || s.includes("RENFORC") || s.includes("BUY")) return "tone-buy";
  if (s.includes("VENTE") || s.includes("VENDR") || s.includes("ALLEG") || s.includes("SELL")) return "tone-sell";
  return "tone-neutral";
}

export function renderRecommendationDetail(rec) {
  if (!rec) {
    return "Select a recommendation to inspect details.";
  }
  const synthese = rec.synthese || "No synthesis provided.";
  const technique = rec.analyse_technique || "—";
  const fondamentale = rec.analyse_fondamentale || "—";
  const sentiment = rec.analyse_sentiment || "—";
  const raisons = Array.isArray(rec.raisons_principales) ? rec.raisons_principales : [];
  const risques = Array.isArray(rec.risques) ? rec.risques : [];
  const catalyseurs = Array.isArray(rec.catalyseurs) ? rec.catalyseurs : [];
  return [
    `<h4>${rec.nom || rec.ticker || "Recommendation"}</h4>`,
    `<p><strong>Signal:</strong> ${rec.signal || "—"} | <strong>Conviction:</strong> ${rec.conviction || "—"}</p>`,
    `<p>${synthese}</p>`,
    `<p><strong>Technique:</strong> ${technique}</p>`,
    `<p><strong>Fondamentale:</strong> ${fondamentale}</p>`,
    `<p><strong>Sentiment:</strong> ${sentiment}</p>`,
    `<p><strong>Raisons:</strong> ${raisons.join(", ") || "—"}</p>`,
    `<p><strong>Risques:</strong> ${risques.join(", ") || "—"}</p>`,
    `<p><strong>Catalyseurs:</strong> ${catalyseurs.join(", ") || "—"}</p>`
  ].join("");
}

export function toMetricRows(record = {}, labels = {}) {
  return Object.entries(labels).map(([key, label]) => `${label}: ${record?.[key] ?? "—"}`);
}
