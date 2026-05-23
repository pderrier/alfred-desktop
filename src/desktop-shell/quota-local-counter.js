/**
 * Quota local counter — P0-20 (2026-05-23) home tier+quota header strip.
 *
 * Wraps the `runs_count_last_7d_local` Tauri command and memoises the
 * result for 60 s. The home page calls this once per render to decide
 * whether to show `Gratuit · X/3 cette semaine` (free tier) or
 * `Premium · illimité` (paid tier — counter is hidden in that case).
 *
 * Two design choices :
 *   1. Local count instead of a server probe. The desktop reads
 *      `run-index.json` directly via the Tauri command — no network
 *      round-trip, instant render. P3-31 will introduce a
 *      `GET /quota/status` server endpoint that this helper switches
 *      to once it lands ; the 60 s memoisation keeps the API surface
 *      compatible (consumers see the same `{count, limit, period}`
 *      payload regardless of source).
 *   2. Drift tolerance. Pierre is the only user today, so the local
 *      count matches the server count 1:1. When public launch widens
 *      the user base or Pierre uses two devices, the local count may
 *      drift by ±1 vs the server — acceptable for an indicative home
 *      strip (the enforcement gate remains the server-side `start_run`
 *      response, which always wins).
 *
 * Voice — the home strip itself is rendered in `app.js` ; this module
 * only returns the raw `{count, limit, period}` shape.
 */

let _cache = null;
let _cacheAt = 0;
const TTL_MS = 60_000;

/**
 * Returns the cached quota state, refreshing if older than 60 s.
 * Safe to call repeatedly during a render — only one Tauri round-trip
 * per minute.
 *
 * On failure, returns `{count: 0, limit: 3, period: "rolling_7d", error: <msg>}`
 * — the home renders the strip without a count rather than erroring
 * out (failure visibility is not critical for an indicative widget).
 */
export async function getLocalQuotaState(bridge) {
  const now = Date.now();
  if (_cache && now - _cacheAt < TTL_MS) return _cache;

  try {
    const payload = await bridge.runsCountLast7d();
    // Stricter validation than `Number(x)` — `Number(null)` is 0, but
    // semantically a null limit means "missing from payload" and we
    // want to fall back to the desktop default of 3. Same for count :
    // null means unknown, not "zero runs done".
    const rawCount = payload?.count;
    const rawLimit = payload?.limit;
    const count = (typeof rawCount === "number" && Number.isFinite(rawCount)) ? rawCount : 0;
    const limit = (typeof rawLimit === "number" && Number.isFinite(rawLimit) && rawLimit > 0)
      ? rawLimit : 3;
    _cache = {
      count,
      limit,
      period: payload?.period || "rolling_7d",
    };
    _cacheAt = now;
    return _cache;
  } catch (e) {
    return {
      count: 0,
      limit: 3,
      period: "rolling_7d",
      error: String(e?.message || e || "unknown"),
    };
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
