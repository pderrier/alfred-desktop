/**
 * Admin tab UI module — v0.4.0 P0-14.
 *
 * Renders a new `settings-card` inside the gear panel listing live
 * usage and VPS health metrics for whitelisted admins only. The tab is
 * built dynamically in JS (no static HTML) so a non-admin user has zero
 * trace of the surface in their DOM — the card is not just hidden, it
 * never exists.
 *
 * Architecture invariants (per spec):
 *   1. Two-layer whitelist : compile-time `ADMIN_HASHES_WHITELIST`
 *      Rust-side controls visibility, `ALFRED_ADMIN_HASHES` server-side
 *      controls authorisation. The desktop never invokes admin
 *      endpoints unless the local boolean check (`isAdminUser`) returns
 *      true. The server is the authoritative gate — a 403 from the
 *      server tears down the tab even if the local cache disagreed.
 *   2. No PII : `top_users` arrives pre-anonymised (8-char hash prefix).
 *      The frontend never displays raw user hashes.
 *   3. 30s refresh : server-side SCAN on Redis is moderately expensive,
 *      so the polling cadence is fixed at 30s and pauses when the
 *      gear panel closes.
 *   4. Empty whitelist = invisible tab : current production default.
 *      Pierre rebuilds with his hash post-merge to activate.
 *
 * Module is pure — receives its `bridge` + `host` element as args, no
 * implicit DOM lookups beyond the host. This is the testability rule
 * the rest of the desktop-shell modules follow (see e.g.
 * `dashboard-refresh-model.js`).
 */

const REFRESH_INTERVAL_MS = 30_000;
const CARD_ID = "settings-admin-section";

/**
 * Build (or return the already-built) admin card element. The card is
 * an `<article class="settings-card">` so it inherits the gear-panel
 * spacing and theme tokens. ID is stable so subsequent renders update
 * in place instead of stacking multiple cards.
 *
 * @param {Document} doc
 * @returns {HTMLElement}
 */
function buildAdminCard(doc) {
  const existing = doc.getElementById(CARD_ID);
  if (existing) {
    return existing;
  }
  const card = doc.createElement("article");
  card.className = "settings-card";
  card.id = CARD_ID;
  card.setAttribute("data-testid", "settings-admin-section");
  card.innerHTML = `
    <h3>Admin observability</h3>
    <p class="settings-note">Live metrics from the Alfred API server. Refreshes every 30s while open.</p>
    <div class="settings-admin-grid">
      <div class="settings-admin-metric" data-testid="admin-runs-7d">
        <span class="settings-admin-label">Runs (7d)</span>
        <span class="settings-admin-value" data-field="runs_7d">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-runs-24h">
        <span class="settings-admin-label">Runs (24h)</span>
        <span class="settings-admin-value" data-field="runs_24h">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-errors-429">
        <span class="settings-admin-label">429s today</span>
        <span class="settings-admin-value" data-field="errors_429_today">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-redis-mem">
        <span class="settings-admin-label">Redis memory</span>
        <span class="settings-admin-value" data-field="redis_memory">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-process-rss">
        <span class="settings-admin-label">Process RSS</span>
        <span class="settings-admin-value" data-field="process_rss">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-uptime">
        <span class="settings-admin-label">Uptime</span>
        <span class="settings-admin-value" data-field="uptime">—</span>
      </div>
      <div class="settings-admin-metric" data-testid="admin-redis-clients">
        <span class="settings-admin-label">Redis clients</span>
        <span class="settings-admin-value" data-field="redis_clients">—</span>
      </div>
    </div>
    <h4 class="settings-admin-subtitle">Top users (last 7d)</h4>
    <table class="settings-admin-table" data-testid="admin-top-users">
      <thead><tr><th>Hash</th><th>Runs</th></tr></thead>
      <tbody data-section="top_users"><tr><td colspan="2" class="settings-note">Loading…</td></tr></tbody>
    </table>
    <h4 class="settings-admin-subtitle">Top tickers (most recently analysed)</h4>
    <table class="settings-admin-table" data-testid="admin-top-tickers">
      <thead><tr><th>ISIN</th><th>Last seen</th></tr></thead>
      <tbody data-section="top_tickers"><tr><td colspan="2" class="settings-note">Loading…</td></tr></tbody>
    </table>
    <p class="settings-note settings-admin-status" data-testid="admin-status"></p>
  `;
  return card;
}

/**
 * Format bytes as a human-readable string. Falls back to `—` for
 * nullish values so the UI never renders "NaN" or "undefined".
 */
function formatBytes(value) {
  if (value === null || value === undefined) {
    return "—";
  }
  const n = Number(value);
  if (!Number.isFinite(n) || n < 0) {
    return "—";
  }
  if (n < 1024) {
    return `${n} B`;
  }
  if (n < 1024 * 1024) {
    return `${(n / 1024).toFixed(1)} KB`;
  }
  if (n < 1024 * 1024 * 1024) {
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

/**
 * Format an integer count. Falls back to `—` for nullish.
 */
function formatCount(value) {
  if (value === null || value === undefined) {
    return "—";
  }
  const n = Number(value);
  if (!Number.isFinite(n)) {
    return "—";
  }
  return String(Math.max(0, Math.trunc(n)));
}

/**
 * Format an uptime in seconds as a compact "Xd Yh Zm" string. Caller
 * controls precision — we show days+hours+minutes when uptime ≥ 1 day,
 * hours+minutes when ≥ 1h, minutes+seconds otherwise. Empty fallback
 * keeps the cell flat-aligned with the others.
 */
function formatUptime(secs) {
  if (secs === null || secs === undefined) {
    return "—";
  }
  const total = Math.max(0, Math.trunc(Number(secs) || 0));
  if (total === 0) {
    return "0s";
  }
  const days = Math.floor(total / 86400);
  const hours = Math.floor((total % 86400) / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (days > 0) {
    return `${days}d ${hours}h ${minutes}m`;
  }
  if (hours > 0) {
    return `${hours}h ${minutes}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${seconds}s`;
  }
  return `${seconds}s`;
}

/**
 * Format an epoch-seconds timestamp as a short ISO string (UTC). Used
 * for the "Last seen" column in the top-tickers table. Falls back to
 * `—` for nullish so a stale entry with no timestamp renders cleanly.
 */
function formatEpochSecs(secs) {
  if (secs === null || secs === undefined) {
    return "—";
  }
  const n = Number(secs);
  if (!Number.isFinite(n) || n <= 0) {
    return "—";
  }
  // toISOString returns e.g. "2026-05-17T12:34:56.000Z"; strip the
  // milliseconds suffix for compact display.
  return new Date(n * 1000).toISOString().replace(/\.\d+Z$/, "Z");
}

/**
 * Render the parsed `AdminUsage` + `AdminVpsStats` envelopes into the
 * card. Both payloads are independently nullable — if one fetch failed
 * the other still renders. Empty arrays produce a "no data" row rather
 * than an empty table.
 *
 * Exported so the test harness can drive renders directly without going
 * through the full bridge.
 */
export function renderAdminPanel(card, usage, vps) {
  // Top-level scalars from usage
  setField(card, "runs_7d", formatCount(usage?.runs_7d));
  setField(card, "runs_24h", formatCount(usage?.runs_24h));
  setField(card, "errors_429_today", formatCount(usage?.errors_429_today));
  // VPS stats
  setField(card, "redis_memory", formatBytes(vps?.redis?.memory_used));
  setField(card, "process_rss", formatBytes(vps?.process?.rss_bytes));
  setField(card, "uptime", formatUptime(vps?.process?.uptime_secs));
  setField(card, "redis_clients", formatCount(vps?.redis?.connected_clients));
  // Top users table
  renderTopUsers(card, usage?.top_users || []);
  // Top tickers table
  renderTopTickers(card, usage?.top_tickers || []);
}

function setField(card, name, text) {
  const el = card.querySelector(`[data-field="${name}"]`);
  if (el) {
    el.textContent = text;
  }
}

function renderTopUsers(card, users) {
  const tbody = card.querySelector('[data-section="top_users"]');
  if (!tbody) return;
  tbody.innerHTML = "";
  if (!users.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 2;
    td.className = "settings-note";
    td.textContent = "No runs yet.";
    tr.appendChild(td);
    tbody.appendChild(tr);
    return;
  }
  for (const u of users) {
    // u.user_hash arrives already truncated to 8 chars (defense-in-depth
    // re-truncate so a server bug doesn't leak full hashes through us).
    const hash = String(u.user_hash || "").slice(0, 8);
    const tr = document.createElement("tr");
    const tdHash = document.createElement("td");
    tdHash.textContent = hash || "—";
    tdHash.setAttribute("data-testid", "admin-user-hash");
    const tdRuns = document.createElement("td");
    tdRuns.textContent = formatCount(u.runs_7d);
    tr.appendChild(tdHash);
    tr.appendChild(tdRuns);
    tbody.appendChild(tr);
  }
}

function renderTopTickers(card, tickers) {
  const tbody = card.querySelector('[data-section="top_tickers"]');
  if (!tbody) return;
  tbody.innerHTML = "";
  if (!tickers.length) {
    const tr = document.createElement("tr");
    const td = document.createElement("td");
    td.colSpan = 2;
    td.className = "settings-note";
    td.textContent = "No tickers yet.";
    tr.appendChild(td);
    tbody.appendChild(tr);
    return;
  }
  for (const t of tickers) {
    const tr = document.createElement("tr");
    const tdIsin = document.createElement("td");
    tdIsin.textContent = String(t.isin || "—");
    const tdLastSeen = document.createElement("td");
    tdLastSeen.textContent = formatEpochSecs(t.last_seen);
    tr.appendChild(tdIsin);
    tr.appendChild(tdLastSeen);
    tbody.appendChild(tr);
  }
}

/**
 * Display a status message in the card (last refresh time, error, etc.).
 * Status auto-clears when the next successful refresh paints; errors
 * persist until they resolve.
 */
function setStatus(card, message, level = "info") {
  const el = card.querySelector('[data-testid="admin-status"]');
  if (!el) return;
  el.textContent = message || "";
  el.classList.toggle("settings-admin-status-error", level === "error");
}

/**
 * Server-403 sentinel detection. When the desktop whitelist drifts from
 * the server whitelist (e.g. user was demoted), the server returns 403
 * and we tear down the tab to match the authoritative view. The error
 * shape is the colon-suffixed `alfred_api_http_error:403` produced by
 * `alfred_api_client::map_api_error`.
 */
function isForbiddenError(err) {
  const text = String(err?.message || err || "");
  return /alfred_api_http_error:403\b/.test(text);
}

/**
 * Install the admin panel onto `host` (typically the gear-panel content
 * container). Returns a controller object with `start()` / `stop()` /
 * `destroy()` methods.
 *
 * The controller is responsible for:
 *   - The 30s refresh loop (only when started).
 *   - Stopping the loop when the gear panel closes.
 *   - Removing the card from the DOM and clearing the timer if the
 *     server tears down admin privileges mid-session.
 *
 * Callers (`app.js#gear-btn` handler) gate the install on
 * `bridge.isAdminUser()` so this function is never invoked for
 * non-admins. The card is therefore never created in their DOM — it's
 * not just hidden, it's absent.
 */
export function installAdminPanel({ host, bridge, doc = document, refreshIntervalMs = REFRESH_INTERVAL_MS }) {
  if (!host || !bridge) {
    throw new Error("installAdminPanel: host and bridge required");
  }
  const card = buildAdminCard(doc);
  host.appendChild(card);

  let timer = null;
  let running = false;
  let destroyed = false;

  async function refreshOnce() {
    if (destroyed) return;
    setStatus(card, "Refreshing…", "info");
    let usage = null;
    let vps = null;
    let forbidden = false;
    let errMsg = null;
    try {
      usage = await bridge.getAdminUsage();
    } catch (e) {
      if (isForbiddenError(e)) {
        forbidden = true;
      } else {
        errMsg = `Usage fetch failed: ${e?.message || e}`;
      }
    }
    if (forbidden) {
      destroy();
      return;
    }
    try {
      vps = await bridge.getAdminVpsStats();
    } catch (e) {
      if (isForbiddenError(e)) {
        forbidden = true;
      } else {
        errMsg = errMsg
          ? `${errMsg}; VPS fetch failed: ${e?.message || e}`
          : `VPS fetch failed: ${e?.message || e}`;
      }
    }
    if (forbidden) {
      destroy();
      return;
    }
    renderAdminPanel(card, usage, vps);
    if (errMsg) {
      setStatus(card, errMsg, "error");
    } else {
      const stamp = new Date().toISOString().replace(/\.\d+Z$/, "Z");
      setStatus(card, `Last refresh: ${stamp}`, "info");
    }
  }

  function start() {
    if (running || destroyed) return;
    running = true;
    refreshOnce();
    timer = setInterval(refreshOnce, refreshIntervalMs);
  }

  function stop() {
    running = false;
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }

  function destroy() {
    if (destroyed) return;
    destroyed = true;
    stop();
    if (card && card.parentNode) {
      card.parentNode.removeChild(card);
    }
  }

  return { start, stop, destroy, refreshOnce, card };
}

// ── Test-only exports ──────────────────────────────────────────────
// Internal helpers exported for unit tests. Not used at runtime by
// other shell modules — keep the public surface minimal.
export const __test_only__ = {
  buildAdminCard,
  formatBytes,
  formatCount,
  formatUptime,
  formatEpochSecs,
  isForbiddenError,
  CARD_ID,
  REFRESH_INTERVAL_MS
};
