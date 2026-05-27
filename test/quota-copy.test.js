import test from "node:test";
import assert from "node:assert/strict";
import {
  formatResetDateFr,
  resolveResetEpochSecs,
  buildQuotaStripResetClause,
  buildFreeTierExhaustedModalCopy,
} from "../src/desktop-shell/quota-copy.js";

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
