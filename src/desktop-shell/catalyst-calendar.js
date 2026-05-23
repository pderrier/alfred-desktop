/**
 * Home Section 7 — "Catalyseurs cette semaine"
 *
 * P2-25 (2026-05-23) — pure helpers to extract dated catalysts from
 * `composed_payload.recommandations[].catalyseurs[]` (free-form FR
 * strings written by the LLM), filter to the [now, now+30d] window,
 * and format for the home strip.
 *
 * Catalyst strings observed in production runs :
 *   - "AG du 27 mai 2026"               (FR explicit date)
 *   - "Publication trafic de mai le 5 juin 2026"
 *   - "Annonce clinique positive"      (no date — skipped)
 *   - "Hausse des marges de raffinage" (no date — skipped)
 *
 * v1 supports the FR explicit-date pattern (`<jour> <mois-fr> <année?>`).
 * Quarters (`Q[1-4]`) and relative ("semaine prochaine", "ce mois-ci")
 * are deferred to P2-25 v2 — keeping the regex tight avoids false
 * positives like "marge de 25 %" being parsed as a date.
 */

const FR_MONTHS = {
  janvier: 0, février: 1, fevrier: 1, mars: 2, avril: 3, mai: 4,
  juin: 5, juillet: 6, août: 7, aout: 7, septembre: 8, octobre: 9,
  novembre: 10, décembre: 11, decembre: 11,
};

const FR_MONTHS_DISPLAY = [
  "janvier", "février", "mars", "avril", "mai", "juin",
  "juillet", "août", "septembre", "octobre", "novembre", "décembre",
];

// Strict regex : day (1-2 digits) + FR month name + optional year (4 digits).
// Word boundary before the day prevents "25 %" / "75 %" from parsing.
const DATE_RE = new RegExp(
  "(?:^|[^\\d])(\\d{1,2})\\s+(janvier|février|fevrier|mars|avril|mai|juin|juillet|août|aout|septembre|octobre|novembre|décembre|decembre)(?:\\s+(\\d{4}))?",
  "i",
);

/**
 * Try to parse the first dated reference in a catalyst string. Returns
 * a `Date` (UTC midnight) on success, `null` when no date pattern is
 * found or the parsed components are out-of-range.
 *
 * `nowYear` is used as the default year when the catalyst omits it
 * ("AG du 27 mai" without year) — defaults to today's year.
 */
export function parseFrenchCatalystDate(text, nowYear = new Date().getUTCFullYear()) {
  if (!text || typeof text !== "string") return null;
  const match = DATE_RE.exec(text);
  if (!match) return null;
  const day = parseInt(match[1], 10);
  const monthKey = match[2].toLowerCase();
  const month = FR_MONTHS[monthKey];
  if (month === undefined) return null;
  const year = match[3] ? parseInt(match[3], 10) : nowYear;
  if (day < 1 || day > 31) return null;
  const d = new Date(Date.UTC(year, month, day));
  // Reject if the constructed date rolled over (e.g. Feb 30 → Mar 02)
  if (d.getUTCMonth() !== month || d.getUTCDate() !== day) return null;
  return d;
}

/**
 * Aggregate dated catalysts across all recommandations, filter to the
 * forward window [now, now + maxDaysAhead], sort by date asc, and
 * return up to `limit` rows.
 *
 * Returns `[]` on null/non-array input. Each row :
 *   { ticker, name, text, date, daysAhead }
 *
 * `daysAhead` is computed as UTC-midnight diff so a catalyst dated
 * "tomorrow" is exactly 1.
 */
export function extractDatedCatalysts(recommandations, options = {}) {
  if (!Array.isArray(recommandations)) return [];
  const {
    now = new Date(),
    maxDaysAhead = 30,
    limit = 3,
  } = options;
  const nowUtcMidnight = Date.UTC(
    now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(),
  );
  const cutoff = nowUtcMidnight + maxDaysAhead * 24 * 3_600 * 1000;
  const rows = [];
  for (const rec of recommandations) {
    if (!rec || typeof rec !== "object") continue;
    const catalysts = Array.isArray(rec.catalyseurs) ? rec.catalyseurs
                   : Array.isArray(rec.catalysts) ? rec.catalysts
                   : [];
    for (const text of catalysts) {
      const dt = parseFrenchCatalystDate(text, now.getUTCFullYear());
      if (!dt) continue;
      const ts = dt.getTime();
      if (ts < nowUtcMidnight || ts > cutoff) continue;
      rows.push({
        ticker: rec.ticker || rec.symbol || "?",
        name: rec.nom || rec.name || "",
        text: String(text),
        date: dt,
        daysAhead: Math.round((ts - nowUtcMidnight) / (24 * 3_600 * 1000)),
      });
    }
  }
  rows.sort((a, b) => a.date.getTime() - b.date.getTime());
  return rows.slice(0, limit);
}

/**
 * Format a catalyst row into a home-strip line.
 *
 *   "MC · AG du 27 mai 2026 · dans 4 jours"
 *   "BNP · Publication Q1 le 12 juin · demain"
 *   "STM · Earnings call · aujourd'hui"
 */
export function formatCatalystRow(row) {
  if (!row || typeof row !== "object") return "";
  // daysAhead must be a finite integer for the row to be renderable —
  // a malformed row returns the empty string rather than emitting
  // "dans NaN jours" or "dans undefined jours" to the UI.
  if (!Number.isFinite(row.daysAhead)) return "";
  const ticker = row.ticker || "?";
  const text = (row.text || "").trim();
  let countdown;
  if (row.daysAhead === 0) countdown = "aujourd'hui";
  else if (row.daysAhead === 1) countdown = "demain";
  else countdown = `dans ${row.daysAhead} jours`;
  return `${ticker} · ${text} · ${countdown}`;
}

/**
 * Format a Date as "<day> <month-fr>" — used by render code when it
 * wants the date alone (header strip / chip). Re-exported for tests.
 */
export function formatFrenchDateShort(date) {
  if (!(date instanceof Date) || Number.isNaN(date.getTime())) return "?";
  const day = date.getUTCDate();
  const month = FR_MONTHS_DISPLAY[date.getUTCMonth()] || "?";
  return `${day} ${month}`;
}
