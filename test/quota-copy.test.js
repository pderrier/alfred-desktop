import test from "node:test";
import assert from "node:assert/strict";
import {
  formatResetDateFr,
  resolveResetEpochSecs,
  buildQuotaStripResetClause,
  buildFreeTierExhaustedModalCopy,
  displayQuotaAwareError,
} from "../src/desktop-shell/quota-copy.js";
import { parseStructuredErrorCode } from "../src/shared/bridge-client.js";

// ── P3-31: quota copy + reset-date formatting ───────────────────────
//
// Pure functions — no DOM. The date assertions recompute the expected
// `JJ/MM` via the same Intl formatter so they're timezone-independent
// (the formatter renders in the runner's local TZ, which we can't pin).

/** Expected `JJ/MM` for an epoch (secs), in the runner's local TZ. */
function expectedFr(epochSecs) {
  return new Intl.DateTimeFormat("fr-FR", { day: "2-digit", month: "2-digit" }).format(
    new Date(epochSecs * 1000)
  );
}

test("formatResetDateFr: formats epoch secs as JJ/MM", () => {
  const epoch = 1_704_882_600; // 2024-01-10 ~10:30 UTC
  const out = formatResetDateFr(epoch);
  assert.match(out, /^\d{2}\/\d{2}$/, "must be two-digit day/month");
  assert.equal(out, expectedFr(epoch));
});

test("formatResetDateFr: returns null for missing / non-positive / non-numeric input", () => {
  assert.equal(formatResetDateFr(null), null);
  assert.equal(formatResetDateFr(undefined), null);
  assert.equal(formatResetDateFr(0), null);
  assert.equal(formatResetDateFr(-5), null);
  assert.equal(formatResetDateFr("soon"), null);
  assert.equal(formatResetDateFr(NaN), null);
});

test("resolveResetEpochSecs: prefers absolute reset_at over relative retryAfter", () => {
  const out = resolveResetEpochSecs({ reset_at: 1_700_000_000, retryAfter: 999 });
  assert.equal(out, 1_700_000_000);
});

test("resolveResetEpochSecs: derives from relative retryAfter when no absolute", () => {
  const fixedNowMs = 1_700_000_000_000; // → 1_700_000_000 secs
  const out = resolveResetEpochSecs({ retryAfter: 86_400 }, () => fixedNowMs);
  assert.equal(out, 1_700_000_000 + 86_400);
});

test("resolveResetEpochSecs: accepts snake_case retry_after too (server envelope)", () => {
  const fixedNowMs = 1_700_000_000_000;
  const out = resolveResetEpochSecs({ retry_after: 3_600 }, () => fixedNowMs);
  assert.equal(out, 1_700_000_000 + 3_600);
});

test("resolveResetEpochSecs: returns null when neither field is usable", () => {
  assert.equal(resolveResetEpochSecs({}), null);
  assert.equal(resolveResetEpochSecs({ retryAfter: 0 }), null);
  assert.equal(resolveResetEpochSecs({ retryAfter: -10 }), null);
  assert.equal(resolveResetEpochSecs(null), null);
});

test("buildQuotaStripResetClause: appends ' · réinitialisation le JJ/MM' when known", () => {
  const epoch = 1_704_882_600;
  const clause = buildQuotaStripResetClause(epoch);
  assert.equal(clause, ` · réinitialisation le ${expectedFr(epoch)}`);
});

test("buildQuotaStripResetClause: empty string when reset date unknown", () => {
  assert.equal(buildQuotaStripResetClause(null), "");
  assert.equal(buildQuotaStripResetClause(0), "");
  assert.equal(buildQuotaStripResetClause("nope"), "");
});

test("buildFreeTierExhaustedModalCopy: includes the reset date from retryAfter", () => {
  const fixedNowMs = 1_700_000_000_000;
  const resetEpoch = 1_700_000_000 + 3 * 86_400;
  const copy = buildFreeTierExhaustedModalCopy(
    { retryAfter: 3 * 86_400, limit: 3, period: "rolling_7d" },
    () => fixedNowMs
  );
  assert.equal(copy.title, "Quota atteint");
  assert.match(copy.message, /Vous avez utilisé vos 3 analyses gratuites/);
  assert.equal(
    copy.message.includes(`Prochaine analyse disponible le ${expectedFr(resetEpoch)}.`),
    true,
    "message must carry the exact reset date"
  );
  assert.equal(copy.hint, "Mode illimité bientôt disponible — contactez l'auteur !");
  assert.equal(copy.cta.label, "Contacter l'auteur");
});

test("buildFreeTierExhaustedModalCopy: honours absolute reset_at (home/quota path)", () => {
  const resetEpoch = 1_704_882_600;
  const copy = buildFreeTierExhaustedModalCopy({ reset_at: resetEpoch, limit: 3 });
  assert.equal(
    copy.message.includes(`Prochaine analyse disponible le ${expectedFr(resetEpoch)}.`),
    true
  );
});

test("buildFreeTierExhaustedModalCopy: falls back to coarse copy when no reset info", () => {
  const copy = buildFreeTierExhaustedModalCopy({ limit: 3 });
  assert.match(copy.message, /Une nouvelle analyse sera disponible plus tard cette semaine\./);
  // Never a NaN or a literal "le undefined".
  assert.equal(/undefined|NaN/.test(copy.message), false);
});

test("buildFreeTierExhaustedModalCopy: defaults limit to 3 when missing/invalid", () => {
  const copy = buildFreeTierExhaustedModalCopy({ retryAfter: 100, limit: 0 }, () => 1_700_000_000_000);
  assert.match(copy.message, /vos 3 analyses gratuites/);
  const copy2 = buildFreeTierExhaustedModalCopy({}, () => 1_700_000_000_000);
  assert.match(copy2.message, /vos 3 analyses gratuites/);
});

test("buildFreeTierExhaustedModalCopy: passes the envelope through as cta.detail", () => {
  const env = { retryAfter: 200, limit: 3, period: "rolling_7d" };
  const copy = buildFreeTierExhaustedModalCopy(env, () => 1_700_000_000_000);
  assert.deepEqual(copy.cta.detail, env);
});

// ── v0.4.8 P0 — displayQuotaAwareError routing ──────────────────────
//
// Background: v0.4.7 shipped quota-aware routing inline only in
// `app-wizard.js::displayError`. The run-progress error handler in
// `app.js` (run.failed → showErrorModal) bypassed it, surfacing the raw
// code (`alfred_free_tier_exhausted:179364:3:rolling_7d`) to the user.
// The shared helper centralises the routing — both sites delegate.

test("displayQuotaAwareError: routes friendly modal for free_tier_exhausted (raw run.failed message)", () => {
  // The exact shape the user saw on v0.4.7 — the literal Pierre
  // reported on 2026-05-29 18:25 UTC.
  const raw = "alfred_free_tier_exhausted:179364:3:rolling_7d";
  const calls = [];
  const fixedNowMs = 1_748_000_000_000; // arbitrary fixed clock
  const routed = displayQuotaAwareError(
    { message: raw },
    () => calls.push({ kind: "fallback" }),
    {
      parseStructuredErrorCode,
      showErrorModal: (title, message, hint, cta) =>
        calls.push({ kind: "modal", title, message, hint, cta }),
      nowFn: () => fixedNowMs,
    }
  );
  assert.equal(routed, true, "must return true when the structured branch fires");
  assert.equal(calls.length, 1, "must invoke showErrorModal exactly once and NOT the fallback");
  assert.equal(calls[0].kind, "modal");
  assert.equal(calls[0].title, "Quota atteint");
  assert.match(calls[0].message, /Vous avez utilisé vos 3 analyses gratuites/);
  // Reset-date copy derived from retry_after=179364s + fixed clock.
  const expectedReset = new Intl.DateTimeFormat("fr-FR", { day: "2-digit", month: "2-digit" }).format(
    new Date(fixedNowMs + 179364 * 1000)
  );
  assert.match(calls[0].message, new RegExp(`Prochaine analyse disponible le ${expectedReset}\\.`));
  assert.equal(calls[0].hint, "Mode illimité bientôt disponible — contactez l'auteur !");
  assert.equal(calls[0].cta.label, "Contacter l'auteur");
  // CRITICAL: the raw code must NEVER leak into the modal body.
  assert.equal(
    /alfred_free_tier_exhausted/.test(calls[0].message),
    false,
    "raw code must not surface in the user-facing message"
  );
  assert.equal(
    /alfred_free_tier_exhausted/.test(calls[0].title),
    false,
    "raw code must not surface in the modal title"
  );
});

test("displayQuotaAwareError: accepts a bare string error too (defensive)", () => {
  const calls = [];
  const routed = displayQuotaAwareError(
    "alfred_free_tier_exhausted:3600:3:rolling_7d",
    () => calls.push({ kind: "fallback" }),
    {
      parseStructuredErrorCode,
      showErrorModal: (title) => calls.push({ kind: "modal", title }),
    }
  );
  assert.equal(routed, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].title, "Quota atteint");
});

test("displayQuotaAwareError: invokes fallback for non-quota errors", () => {
  const calls = [];
  const routed = displayQuotaAwareError(
    { message: "network_unreachable: ECONNREFUSED" },
    (err) => calls.push({ kind: "fallback", err }),
    {
      parseStructuredErrorCode,
      showErrorModal: () => calls.push({ kind: "modal" }),
    }
  );
  assert.equal(routed, false);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].kind, "fallback");
  assert.equal(calls[0].err.message, "network_unreachable: ECONNREFUSED");
});

test("displayQuotaAwareError: invokes fallback for empty / null / undefined errors", () => {
  for (const err of [null, undefined, "", { message: "" }]) {
    const calls = [];
    const routed = displayQuotaAwareError(
      err,
      (e) => calls.push(e),
      { parseStructuredErrorCode, showErrorModal: () => {} }
    );
    assert.equal(routed, false, `must fall back for: ${JSON.stringify(err)}`);
    assert.equal(calls.length, 1);
  }
});

test("displayQuotaAwareError: does not invoke fallback when none provided (defensive no-op)", () => {
  assert.doesNotThrow(() => {
    displayQuotaAwareError(
      { message: "generic_error" },
      undefined,
      { parseStructuredErrorCode, showErrorModal: () => {} }
    );
  });
});

test("displayQuotaAwareError: still falls back when deps.showErrorModal is missing (defensive)", () => {
  const calls = [];
  const routed = displayQuotaAwareError(
    { message: "alfred_free_tier_exhausted:60:3:rolling_7d" },
    (err) => calls.push({ kind: "fallback", err }),
    { parseStructuredErrorCode }
  );
  // Without showErrorModal we cannot route; treat as fallback so the
  // user still sees SOMETHING.
  assert.equal(routed, false);
  assert.equal(calls.length, 1);
});
