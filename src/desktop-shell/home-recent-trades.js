/**
 * Home Section 4 — "Tes derniers ordres Finary"
 *
 * P1-22 (2026-05-23) — pure helpers to surface the last trade-like
 * transactions from `snapshot.transactions` on the home welcome view.
 *
 * Note (diagnostic 2026-05-23) — Finary's `/users/me/orders` returns
 * 0 rows for Pierre's account ; the trade data lives in
 * `/users/me/transactions` instead, mixed with bank fees, deposits,
 * etc. We filter to entries that look like security trades :
 *   - `transaction_type === "market_order"`     (FR PEA: ACHAT/VENTE COMPTANT)
 *   - OR `category.name === "investment"`        (broker investment moves)
 *   - OR `display_name` matches /^(ACHAT|VENTE|DIVIDENDE)/i (extra safety)
 *
 * The original po-plan sketch assumed a dedicated `orders` array was
 * populated — confirmed empty in 5 production snapshots, so we work
 * from `transactions` directly. P1-22 v2 (deferred) will look for a
 * Finary endpoint that surfaces orders explicitly with the security
 * symbol attached.
 */

const TRADE_PREFIX_RE = /^(ACHAT|VENTE|DIVIDENDE|SOUSCRIPTION|RACHAT)/i;

/**
 * Filter `snapshot.transactions` to trade-like rows, normalise the
 * key fields, and return up to `limit` most recent.
 *
 * Returns `[]` on null/non-array input, or when no rows match.
 */
export function extractTradeMoves(snapshot, limit = 5) {
  const transactions = Array.isArray(snapshot?.transactions) ? snapshot.transactions : [];
  const orders = Array.isArray(snapshot?.orders) ? snapshot.orders : [];
  // Concat both (orders is usually empty but defensive — schema
  // contract says it CAN be populated for some brokers).
  const candidates = [...orders, ...transactions];
  const trades = [];
  for (const t of candidates) {
    if (!t || typeof t !== "object") continue;
    const txType = String(t.transaction_type || "").toLowerCase();
    const catName = String(t.category?.name || "").toLowerCase();
    const displayName = String(t.display_name || t.name || "");
    const isTrade = txType === "market_order"
      || catName === "investment"
      || TRADE_PREFIX_RE.test(displayName);
    if (!isTrade) continue;
    const date = t.date || t.display_date || "";
    if (!date) continue;
    trades.push({
      date,
      raw_action: extractAction(displayName, t.value),
      security_name: extractSecurityName(displayName),
      value: typeof t.value === "number" ? t.value : Number(t.value) || 0,
      currency_symbol: t.currency?.symbol || "€",
      display_name: displayName,
    });
  }
  // Sort by date desc, take top `limit`.
  trades.sort((a, b) => (b.date < a.date ? -1 : b.date > a.date ? 1 : 0));
  return trades.slice(0, limit);
}

/**
 * Derive the trade action from the display name (preferred) or the
 * signed value (fallback). Returns one of :
 *   - "buy"      (achat / souscription, OR negative value)
 *   - "sell"     (vente / rachat, OR positive value > 0)
 *   - "dividend" (dividende)
 *   - "other"    (couldn't classify)
 */
export function extractAction(displayName, value) {
  const upper = String(displayName || "").toUpperCase();
  if (upper.startsWith("ACHAT") || upper.startsWith("SOUSCRIPTION")) return "buy";
  if (upper.startsWith("VENTE") || upper.startsWith("RACHAT")) return "sell";
  if (upper.startsWith("DIVIDENDE")) return "dividend";
  if (typeof value === "number") {
    if (value < 0) return "buy";
    if (value > 0) return "sell";
  }
  return "other";
}

/**
 * Strip the FR Boursorama-style prefix (`ACHAT COMPTANT - `, etc.)
 * from a display_name to get the bare security name. Returns the
 * input unchanged when no prefix is recognised.
 *
 * Examples :
 *   "ACHAT COMPTANT - THERMADOR GROUPE" → "THERMADOR GROUPE"
 *   "VENTE - LVMH"                       → "LVMH"
 *   "DIVIDENDE TOTALENERGIES"            → "TOTALENERGIES"
 */
export function extractSecurityName(displayName) {
  const raw = String(displayName || "").trim();
  if (!raw) return "";
  // Try the strict "<action> ... - <name>" pattern first.
  const dashSplit = raw.split(/\s+-\s+/);
  if (dashSplit.length >= 2) return dashSplit[dashSplit.length - 1].trim();
  // Fallback : remove a leading action keyword if present.
  const trimmed = raw.replace(TRADE_PREFIX_RE, "").trim();
  return trimmed || raw;
}

/**
 * Format a trade row into the home-strip line. Pierre-voice :
 * tutoiement FR, factuel, one short sentence per row.
 *
 *   buy      → "Tu as renforcé THERMADOR le 14 mai · -139,63 €"
 *   sell     → "Tu as allégé LVMH le 14 mai · +443,01 €"
 *   dividend → "Tu as encaissé un dividende TOTAL le 12 mai · +12,40 €"
 *
 * Date is rendered as "<day> <month-short>" in FR locale.
 */
export function formatTradeRow(trade) {
  if (!trade || typeof trade !== "object") return "";
  const verb = ({
    buy: "renforcé",
    sell: "allégé",
    dividend: "encaissé un dividende",
    other: "effectué un ordre sur",
  })[trade.raw_action] || "effectué un ordre sur";
  const dateStr = formatFrenchDate(trade.date);
  const value = Math.abs(Number(trade.value) || 0);
  const sign = trade.value < 0 ? "-" : "+";
  const symbol = trade.currency_symbol || "€";
  const security = trade.security_name || "?";
  const valueStr = `${sign}${value.toFixed(2).replace(".", ",")} ${symbol}`;
  if (trade.raw_action === "dividend") {
    return `Tu as ${verb} ${security} le ${dateStr} · ${valueStr}`;
  }
  return `Tu as ${verb} ${security} le ${dateStr} · ${valueStr}`;
}

function formatFrenchDate(iso) {
  if (!iso || typeof iso !== "string") return "?";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const months = [
    "janvier", "février", "mars", "avril", "mai", "juin",
    "juillet", "août", "septembre", "octobre", "novembre", "décembre",
  ];
  return `${d.getDate()} ${months[d.getMonth()] || "?"}`;
}
