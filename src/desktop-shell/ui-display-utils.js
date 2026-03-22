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

export function recommendationActionClass(signal) {
  const normalized = String(signal || "").toUpperCase();
  if (normalized.includes("ACHAT")) return "positive";
  if (normalized.includes("VENTE")) return "negative";
  return "neutral";
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
