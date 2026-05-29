/**
 * Quota user-facing copy + date formatting — v0.4.7 P3-31 quota-UX bundle.
 *
 * Pure functions (no DOM, no bridge) so the copy contract is unit-testable
 * without jsdom. Two consumers share this module:
 *   - the home tier+quota strip (`app.js`) — appends a reset date to
 *     `Gratuit · X/3 cette semaine`.
 *   - the free-tier-exhausted modal (`app.js` splash path + `app-wizard.js`
 *     run-start path) — `buildFreeTierExhaustedModalCopy`.
 *
 * Centralised so the date formatting + reset-clause wording live in ONE
 * place — a tweak to the FR phrasing (or the JJ/MM format) updates every
 * surface at once.
 */

/**
 * Format an absolute reset moment (epoch SECONDS) as a French day/month
 * string `JJ/MM`. Returns `null` for missing / non-finite / non-positive
 * input so callers can decide whether to render the clause at all.
 *
 * `nowFn` is injectable for deterministic tests; unused here but kept in
 * the sibling `resolveResetEpochSecs` for the relative→absolute path.
 */
export function formatResetDateFr(epochSecs) {
  if (typeof epochSecs !== "number" || !Number.isFinite(epochSecs) || epochSecs <= 0) {
    return null;
  }
  const date = new Date(epochSecs * 1000);
  if (Number.isNaN(date.getTime())) return null;
  // FR locale, day + month only (e.g. "03/06"). Two-digit so the strip
  // width is stable across the month boundary.
  return new Intl.DateTimeFormat("fr-FR", { day: "2-digit", month: "2-digit" }).format(date);
}

/**
 * Resolve an absolute reset epoch (seconds) from an envelope that may
 * carry EITHER an absolute `reset_at` (home strip / `/quota/status`) OR a
 * relative `retryAfter` in seconds (the run-start 429 error code, which
 * only encodes `retry_after`). Absolute wins when both present.
 *
 * `nowFn` returns epoch MILLISECONDS (defaults to `Date.now`) so tests can
 * pin the clock. Returns `null` when neither field yields a usable moment.
 */
export function resolveResetEpochSecs(envelope, nowFn = Date.now) {
  const absolute = envelope?.reset_at;
  if (typeof absolute === "number" && Number.isFinite(absolute) && absolute > 0) {
    return absolute;
  }
  const retryAfter = envelope?.retryAfter ?? envelope?.retry_after;
  if (typeof retryAfter === "number" && Number.isFinite(retryAfter) && retryAfter > 0) {
    return Math.floor(nowFn() / 1000) + retryAfter;
  }
  return null;
}

/**
 * Reset clause for the home strip, appended after `Gratuit · X/3 cette
 * semaine`. Returns ` · réinitialisation le JJ/MM` (note the leading
 * separator) or `""` when no reset date is derivable (empty window /
 * fallback path).
 */
export function buildQuotaStripResetClause(resetAtEpochSecs) {
  const formatted = formatResetDateFr(resetAtEpochSecs);
  return formatted ? ` · réinitialisation le ${formatted}` : "";
}

/**
 * Build the user-facing copy for the free-tier-exhausted upgrade modal.
 * Returns `{title, message, hint, cta}` ready to pass to `showErrorModal`.
 *
 * The envelope is the structured payload from `parseStructuredErrorCode`
 * (`{retryAfter, limit, period}`) on the run-start path, or the splash
 * health diagnostics (`{retryAfter, limit, period}`) — both carry a
 * relative `retryAfter`. An absolute `reset_at` is also honoured when
 * present. The message appends the exact reset date
 * (`Prochaine analyse disponible le JJ/MM.`) when derivable, falling back
 * to a coarse "plus tard cette semaine" phrasing otherwise — never a
 * `NaN` or a bare code.
 *
 * `nowFn` (epoch ms) is injectable for deterministic tests.
 */
export function buildFreeTierExhaustedModalCopy(envelope, nowFn = Date.now) {
  const limit = envelope?.limit && envelope.limit > 0 ? envelope.limit : 3;
  const resetEpochSecs = resolveResetEpochSecs(envelope, nowFn);
  const resetDate = formatResetDateFr(resetEpochSecs);
  const resetClause = resetDate
    ? `Prochaine analyse disponible le ${resetDate}.`
    : "Une nouvelle analyse sera disponible plus tard cette semaine.";
  return {
    title: "Quota atteint",
    message: `Vous avez utilisé vos ${limit} analyses gratuites pour les 7 derniers jours. ${resetClause}`,
    hint: "Mode illimité bientôt disponible — contactez l'auteur !",
    cta: {
      label: "Contacter l'auteur",
      detail: envelope || {},
    },
  };
}

/**
 * v0.4.8 P0 — single routing helper for ALL user-facing error display
 * sites that can possibly carry a structured quota code.
 *
 * Background: v0.4.7 shipped the friendly "Quota atteint" modal only on
 * the run-wizard `displayError` path. The run-progress event handler
 * (`run.failed`) was a SECOND display site that bypassed the routing
 * entirely and rendered the raw code
 * (`alfred_free_tier_exhausted:179364:3:rolling_7d`) as the modal body.
 * Two paths, one fix — Pierre saw the raw code on v0.4.7.
 *
 * Contract:
 *   - If `error` stringifies to a recognised `alfred_free_tier_exhausted`
 *     code → call `deps.showErrorModal` with the friendly copy
 *     (title / message / hint / cta — including reset date).
 *   - Otherwise → invoke `fallback(error)` (the caller decides what the
 *     default display looks like — `showErrorModal("Analysis Failed", …)`
 *     on the run-progress path, `formatBridgeError`/`showToast` on the
 *     wizard path).
 *
 * `deps` is injected so this module stays DOM-/bridge-free and remains
 * unit-testable without jsdom. Production callers pass
 * `{ parseStructuredErrorCode, showErrorModal }`; tests pass spies.
 *
 * Returns `true` when the structured-quota branch fired, `false` when
 * the fallback was invoked. Useful for tests; production callers can
 * ignore.
 */
export function displayQuotaAwareError(error, fallback, deps) {
  const parseStructuredErrorCode = deps?.parseStructuredErrorCode;
  const showErrorModal = deps?.showErrorModal;
  const nowFn = deps?.nowFn || Date.now;
  const rawText = String(error?.message || error || "");
  const structured =
    typeof parseStructuredErrorCode === "function"
      ? parseStructuredErrorCode(rawText)
      : null;
  if (structured?.code === "alfred_free_tier_exhausted" && typeof showErrorModal === "function") {
    const copy = buildFreeTierExhaustedModalCopy(structured, nowFn);
    showErrorModal(copy.title, copy.message, copy.hint, copy.cta);
    return true;
  }
  if (typeof fallback === "function") {
    fallback(error);
  }
  return false;
}
