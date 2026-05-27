/**
 * Quota state for the home tier+quota header strip — P0-20 (2026-05-23),
 * server-authoritative since v0.4.7 P3-31.
 *
 * The home page calls `getLocalQuotaState` once per render to decide
 * whether to show `Gratuit · X/3 cette semaine` (free tier) or
 * `Premium · illimité` (paid tier — counter hidden in that case).
 *
 * ## Source priority (v0.4.7 P3-31)
 *
 *   1. PRIMARY — `bridge.quotaStatus()` → `GET /quota/status`. Returns
 *      the SAME count the enforcement gate (`quota::start_run`) sees, so
 *      the home strip and the run-start 429 can never disagree. Adds
 *      `reset_at` (epoch secs) so the strip can render the reset date.
 *   2. FALLBACK — `bridge.runsCountLast7d()` (local `run-index.json`
 *      count). Used ONLY when the server probe fails (offline / API
 *      down). The local count can drift ±1 vs the server ZSET, so it's a
 *      degraded approximation — never the primary.
 *
 * ## Why the switch
 *
 * Before P3-31 the strip read the local count exclusively. That count
 * drifts ±1 vs the authoritative server ZSET, producing the launch bug
 * where the strip showed `Gratuit · 2/3` while the server BLOCKED the
 * run at 3/3 (`docs/monetization-architecture.md` § "Quota exposure to
 * UI"). Reading the server count kills that divergence.
 *
 * ## Return shape
 *
 * `{count, limit, period, reset_at}` (+ optional `error` when even the
 * fallback failed). `reset_at` is epoch secs or `null` (empty window /
 * fallback path which has no reset info). `limit` is always a number
 * (the server's `"unlimited"` paid sentinel coerces to the desktop
 * default 3 — paid tier renders from `licenseStatus().tier`, not from
 * this limit). Memoised 60 s — safe to call repeatedly per render.
 *
 * Voice — the home strip itself is rendered in `app.js` ; this module
 * only returns the raw `{count, limit, period, reset_at}` shape.
 */

let _cache = null;
let _cacheAt = 0;
const TTL_MS = 60_000;

/** Strict numeric coercion — `null`/`undefined`/non-number → fallback. */
function coerceNumber(value, fallback) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

/**
 * Normalise a raw quota payload (from either source) into the canonical
 * `{count, limit, period, reset_at}` shape. Stricter than `Number(x)` —
 * `Number(null)` is 0, but a null `limit` means "missing from payload"
 * (fall back to the desktop default 3) and a null `count` means
 * "unknown" (0). The server's `"unlimited"` string limit is non-numeric
 * → coerces to 3, which is correct: the limit is never shown for paid
 * tier (the strip branches on `licenseStatus().tier`).
 */
function normalizeQuotaPayload(payload) {
  const limit = coerceNumber(payload?.limit, 0);
  const resetAt = coerceNumber(payload?.reset_at, null);
  return {
    count: coerceNumber(payload?.count, 0),
    limit: limit > 0 ? limit : 3,
    period: payload?.period || "rolling_7d",
    reset_at: resetAt !== null && resetAt > 0 ? resetAt : null,
  };
}

/**
 * Returns the cached quota state, refreshing if older than 60 s.
 * Safe to call repeatedly during a render — at most one network probe
 * per minute.
 *
 * Source order: `quotaStatus()` (server-authoritative) first, then
 * `runsCountLast7d()` (local count) as a fallback. Only when BOTH fail
 * do we return the inert `{count: 0, limit: 3, ...}` with an `error`
 * field — the home renders the strip without a real count rather than
 * erroring out (failure visibility is not critical for this indicative
 * widget; the enforcement gate is server-side and always wins).
 */
export async function getLocalQuotaState(bridge) {
  const now = Date.now();
  if (_cache && now - _cacheAt < TTL_MS) return _cache;

  // PRIMARY — authoritative server count (P3-31).
  try {
    const payload = await bridge.quotaStatus();
    _cache = normalizeQuotaPayload(payload);
    _cacheAt = now;
    return _cache;
  } catch (primaryErr) {
    // FALLBACK — local run-index count. No reset_at available locally.
    try {
      const payload = await bridge.runsCountLast7d();
      _cache = normalizeQuotaPayload(payload);
      _cacheAt = now;
      return _cache;
    } catch (fallbackErr) {
      // Both sources down — don't cache the inert value so the next
      // render retries instead of being stuck at 0/3 for 60 s.
      return {
        count: 0,
        limit: 3,
        period: "rolling_7d",
        reset_at: null,
        error: String(
          fallbackErr?.message || primaryErr?.message || fallbackErr || primaryErr || "unknown"
        ),
      };
    }
  }
}

/**
 * Reset the in-memory cache. Used by tests and after a successful
 * /run/start (where the server-side count just incremented and the
 * desktop should reflect that on next render).
 */
export function resetLocalQuotaCache() {
  _cache = null;
  _cacheAt = 0;
}
