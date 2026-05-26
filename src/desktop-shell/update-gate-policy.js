/**
 * Mandatory-update gate policy + FR-voice strings.
 *
 * Pure module (no `document`, no Tauri) used by the splash startup check in
 * `app-bootstrap.js` AND by the unit-test suite — same pattern as
 * `codex-fallback-policy.js`. Keeping the decision + the user-facing copy in
 * one importable place lets the Node `node --test` runner (which has no
 * jsdom) cover them without touching the DOM, and prevents the bootstrap
 * from drifting from what the tests assert.
 *
 * Two concerns live here:
 *
 *  - `shouldHaltForMandatoryUpdate(updateResult)` — the boolean gate. When
 *    `true`, the bootstrap must render the mandatory overlay and stop all
 *    further (heavy) startup work. P0-80: this is checked FIRST, before any
 *    splash loader/connect section is revealed, so the user never sees the
 *    full splash flash before being told an upgrade is required.
 *
 *  - `buildMandatoryUpdateStrings(update)` — the FR-tutoiement copy for the
 *    overlay. The app voice is French + tutoiement (see
 *    docs/home-widget-architecture.md); the previous English strings were a
 *    voice regression. Centralised so a test can assert the FR strings are
 *    present and the old EN strings are gone.
 */

/**
 * @typedef {{
 *   update_available?: boolean,
 *   mandatory?: boolean,
 *   current_version?: string,
 *   latest_version?: string,
 *   release_notes?: string | null,
 *   installer_url?: string | null,
 * }} UpdateCheckResult
 */

/**
 * Decide whether the bootstrap must halt and force an upgrade.
 *
 * An update gates the app only when BOTH flags are present: the manifest
 * says an update is available AND it is mandatory. A null/undefined result
 * (update check failed or returned nothing) never halts — the app continues
 * normally, matching the existing "update check failed — continue" branch.
 *
 * @param {UpdateCheckResult | null | undefined} updateResult
 * @returns {boolean}
 */
export function shouldHaltForMandatoryUpdate(updateResult) {
  if (!updateResult) return false;
  return updateResult.update_available === true && updateResult.mandatory === true;
}

/**
 * Build the FR-tutoiement copy for the mandatory-update overlay.
 *
 * Returns plain strings (no HTML) so callers control escaping/markup. The
 * `body` line composes the version delta + the obligatory-upgrade sentence;
 * the version numbers are taken verbatim from the update result.
 *
 * @param {UpdateCheckResult} update
 * @returns {{
 *   title: string,
 *   body: string,
 *   releaseNotes: string | null,
 *   downloadBtn: string,
 *   installingBtn: string,
 *   retryBtn: string,
 *   downloadFailed: string,
 * }}
 */
export function buildMandatoryUpdateStrings(update) {
  const latest = update?.latest_version ?? "";
  const current = update?.current_version ?? "";
  const notes = update?.release_notes;
  return {
    title: "Mise à jour requise",
    body: `La version ${latest} est disponible (tu as la ${current}). Cette mise à jour est obligatoire pour continuer.`,
    releaseNotes: notes ? String(notes) : null,
    downloadBtn: "Télécharger et installer",
    installingBtn: "Installation…",
    retryBtn: "Réessayer",
    downloadFailed: "Échec du téléchargement",
  };
}
