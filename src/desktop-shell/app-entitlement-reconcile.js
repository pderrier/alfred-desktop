/**
 * Entitlement reconciliation — MON-E follow-up (2026-06-18).
 *
 * Root-cause fix for the "paying user hits the free wall" UX bug
 * (`docs/desktop-api-integration.md` § License flow; `docs/monetization-architecture.md`
 * § "Codes de redeem").
 *
 * Symptom: a device that has already redeemed a comp code (so it holds a
 * valid `redeemed_code` in user-preferences) could still fall back to the
 * free tier and hit "Quota atteint". This happens whenever the server-side
 * entitlement `tier:<device_id>` is no longer `paid` while the code itself
 * is still valid:
 *   - the 24h Redis cache for the tier lapsed and was never re-primed,
 *   - the `device_id` churned (AppData wipe + re-register on the same
 *     machine → a fresh device_id with no tier row yet), or
 *   - a re-slot / rebind on the server side.
 *
 * The code is the durable entitlement (MON-E), not the volatile device
 * identity. So the right behaviour is: if we hold a valid code locally and
 * the server doesn't currently grant us `paid`, silently re-redeem the code
 * BEFORE ever showing the free wall. On the server, `decide_redeem` reuses
 * the existing machine slot for the same `machine_hash` (0 slot consumed)
 * and re-writes `tier:<device_id> = paid`.
 *
 * BINDING `feedback_free_trial_never_blocked_by_antiabuse` /
 * `feedback_failure_visibility`: this is fail-SOFT. A network/server failure
 * never blocks the user — we just leave them where they were (the run keeps
 * going on whatever tier the server grants). It never invents an entitlement
 * client-side; the server stays authoritative.
 *
 * Idempotency: `reconcileEntitlement` is guarded so at most ONE re-redeem
 * attempt fires per process (per `state` object the caller owns). No redeem
 * storms, no loops.
 *
 * Pure decision logic is split from I/O so it is unit-testable without a
 * bridge. Exported under `__test`.
 */

import { normalizeRedeemedCode } from "/desktop-shell/app-redeem-modal.js";

/**
 * Pure helper: should we attempt a silent re-redeem of the locally-held
 * code?
 *
 * @param {Object} args
 * @param {string} [args.redeemedCode] — the persisted `redeemed_code`.
 * @param {string} [args.tier] — the tier the server currently grants
 *   (`license_status_local` → Redis cache). Anything other than "paid" is
 *   treated as "not currently entitled".
 * @returns {{ shouldReactivate: boolean, code: string, reason: string }}
 *   `reason` is for logging/telemetry only:
 *     - "no-code"        — nothing stored, normal free user, do nothing.
 *     - "already-paid"   — server already grants paid, nothing to do.
 *     - "reactivate"     — code held + not paid → re-redeem.
 */
export function decideSilentReactivation({ redeemedCode, tier } = {}) {
  const code = normalizeRedeemedCode(redeemedCode);
  if (!code) {
    return { shouldReactivate: false, code: "", reason: "no-code" };
  }
  if (typeof tier === "string" && tier.trim().toLowerCase() === "paid") {
    return { shouldReactivate: false, code, reason: "already-paid" };
  }
  return { shouldReactivate: true, code, reason: "reactivate" };
}

/**
 * Reconcile the local entitlement with the server at startup.
 *
 * If a valid `redeemed_code` is stored locally and the server does not
 * currently grant `paid`, silently re-redeem the code (re-binding this
 * device to the still-valid code) so a legitimate paying user never lands
 * on the free wall. Fail-soft and idempotent.
 *
 * @param {Object} deps
 * @param {Object} deps.bridge — must expose `licenseStatus()` and
 *   `redeemCode(code)`.
 * @param {Function} deps.getPreferences — async `() => prefs` (reads
 *   `redeemed_code`). May reject / return null — handled.
 * @param {Function} [deps.dispatchActivated] — called on a successful
 *   re-redeem so the caller can refresh tier-dependent UI (health pill,
 *   home header). Receives the redeem response payload.
 * @param {Object} [deps.state] — idempotency latch owned by the caller;
 *   defaults to a module-local singleton. `state.attempted` is set to true
 *   on the first invocation that gets as far as deciding, so a second call
 *   in the same session is a no-op.
 * @param {Function} [deps.log] — optional `(msg) => void` for diagnostics.
 * @returns {Promise<{ outcome: string, reason?: string }>}
 *   outcome ∈ "skipped" | "reactivated" | "failed".
 */
const moduleReconcileState = { attempted: false };

export async function reconcileEntitlement(deps = {}) {
  const {
    bridge,
    getPreferences,
    dispatchActivated,
    state = moduleReconcileState,
    log,
  } = deps;

  // Idempotency: at most one attempt per session. Set the latch up-front so
  // a concurrent / repeat call can't race a second redeem.
  if (state.attempted) {
    return { outcome: "skipped", reason: "already-attempted" };
  }
  if (!bridge || typeof bridge.redeemCode !== "function") {
    return { outcome: "skipped", reason: "no-bridge" };
  }

  // Read the stored code first — if there's nothing to re-activate we can
  // skip the (cheap but non-zero) licenseStatus round-trip entirely.
  let redeemedCode = "";
  try {
    const prefs = (typeof getPreferences === "function" ? await getPreferences() : null) || {};
    redeemedCode = prefs.redeemed_code || "";
  } catch {
    // Prefs unreadable — treat as "no code", fail-soft.
    return { outcome: "skipped", reason: "prefs-unavailable" };
  }
  if (!normalizeRedeemedCode(redeemedCode)) {
    return { outcome: "skipped", reason: "no-code" };
  }

  // We hold a code — now check whether the server already grants paid.
  let tier = "";
  try {
    const status = typeof bridge.licenseStatus === "function" ? await bridge.licenseStatus() : null;
    tier = status?.tier || "";
  } catch {
    // licenseStatus failed (offline / server down). We can't confirm the
    // tier, so we DON'T re-redeem blindly here — a redeem call would fail
    // too, and we must not block startup. The next run / health probe will
    // surface a clear error if the server is genuinely unreachable.
    return { outcome: "skipped", reason: "status-unavailable" };
  }

  const decision = decideSilentReactivation({ redeemedCode, tier });
  if (!decision.shouldReactivate) {
    state.attempted = true;
    return { outcome: "skipped", reason: decision.reason };
  }

  // Latch BEFORE the network call so a failure still counts as the one
  // allowed attempt this session (no retry storm on a dead server).
  state.attempted = true;
  try {
    const res = await bridge.redeemCode(decision.code);
    if (typeof log === "function") log("entitlement: silently re-activated stored code");
    if (typeof dispatchActivated === "function") {
      try {
        dispatchActivated(res);
      } catch {
        /* UI refresh is best-effort */
      }
    }
    return { outcome: "reactivated" };
  } catch {
    // Re-redeem failed (server rejected, expired code, network). Fail-soft:
    // leave the user on whatever tier the server grants. If the code is
    // genuinely expired the normal free-wall → renew flow takes over, which
    // is the correct surface for that case.
    if (typeof log === "function") log("entitlement: silent re-activation failed (fail-soft)");
    return { outcome: "failed", reason: decision.reason };
  }
}

export const __test = {
  decideSilentReactivation,
  resetModuleState() {
    moduleReconcileState.attempted = false;
  },
};
