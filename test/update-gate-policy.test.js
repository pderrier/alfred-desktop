/**
 * Tests for the mandatory-update gate policy — P0-80 (v0.4.5).
 *
 * `update-gate-policy.js` is a pure module (no `document`, no Tauri), so we
 * import it directly under the Node `node --test` runner — unlike the
 * DOM-touching `app-bootstrap.js`, which can't be imported here. This pins:
 *   - `shouldHaltForMandatoryUpdate` truth table (the early-gate decision),
 *   - `buildMandatoryUpdateStrings` FR-tutoiement copy (no English left).
 *
 * The bootstrap consumes these two functions for the early mandatory-update
 * overlay; keeping the decision + copy here means the gate behaviour and the
 * user-facing voice are verified without a DOM.
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  shouldHaltForMandatoryUpdate,
  buildMandatoryUpdateStrings,
} from "../src/desktop-shell/update-gate-policy.js";

// ── shouldHaltForMandatoryUpdate ────────────────────────────────────

test("shouldHaltForMandatoryUpdate halts when update available AND mandatory", () => {
  assert.equal(
    shouldHaltForMandatoryUpdate({ update_available: true, mandatory: true }),
    true
  );
});

test("shouldHaltForMandatoryUpdate does NOT halt when mandatory but no update available", () => {
  // Defensive: a manifest could mark mandatory while the client is already
  // up to date. No update to install → nothing to force.
  assert.equal(
    shouldHaltForMandatoryUpdate({ update_available: false, mandatory: true }),
    false
  );
});

test("shouldHaltForMandatoryUpdate does NOT halt for an available-but-optional update", () => {
  assert.equal(
    shouldHaltForMandatoryUpdate({ update_available: true, mandatory: false }),
    false
  );
});

test("shouldHaltForMandatoryUpdate does NOT halt on null/undefined (check failed)", () => {
  // Mirrors the bootstrap "update check failed — continue normally" branch.
  assert.equal(shouldHaltForMandatoryUpdate(null), false);
  assert.equal(shouldHaltForMandatoryUpdate(undefined), false);
  assert.equal(shouldHaltForMandatoryUpdate({}), false);
});

// ── buildMandatoryUpdateStrings (FR voice) ──────────────────────────

test("buildMandatoryUpdateStrings renders FR-tutoiement copy, not English", () => {
  const s = buildMandatoryUpdateStrings({
    latest_version: "0.4.5",
    current_version: "0.4.4",
    release_notes: null,
  });

  // FR strings present.
  assert.equal(s.title, "Mise à jour requise");
  assert.match(s.body, /La version 0\.4\.5 est disponible \(tu as la 0\.4\.4\)\./);
  assert.match(s.body, /obligatoire pour continuer/);
  assert.equal(s.downloadBtn, "Télécharger et installer");
  assert.equal(s.installingBtn, "Installation…");
  assert.equal(s.retryBtn, "Réessayer");
  assert.equal(s.downloadFailed, "Échec du téléchargement");

  // The old English voice must be gone from every string.
  const all = Object.values(s).filter((v) => typeof v === "string").join(" ");
  assert.doesNotMatch(all, /Update Required/);
  assert.doesNotMatch(all, /Download & Install/);
  assert.doesNotMatch(all, /Download failed/);
  assert.doesNotMatch(all, /\bRetry\b/);
  assert.doesNotMatch(all, /\bInstalling\b/);
  assert.doesNotMatch(all, /is available \(you have/);
});

test("buildMandatoryUpdateStrings passes through release notes when present", () => {
  const s = buildMandatoryUpdateStrings({
    latest_version: "1.0.0",
    current_version: "0.9.0",
    release_notes: "Rotation du secret API — les anciens clients ne peuvent plus s'authentifier.",
  });
  assert.equal(
    s.releaseNotes,
    "Rotation du secret API — les anciens clients ne peuvent plus s'authentifier."
  );
});

test("buildMandatoryUpdateStrings yields null release notes when absent", () => {
  assert.equal(
    buildMandatoryUpdateStrings({ latest_version: "1.0.0", current_version: "0.9.0" }).releaseNotes,
    null
  );
  assert.equal(
    buildMandatoryUpdateStrings({ latest_version: "1.0.0", current_version: "0.9.0", release_notes: "" }).releaseNotes,
    null
  );
});

test("buildMandatoryUpdateStrings tolerates missing version fields", () => {
  // The Rust UpdateCheckResult always supplies both, but the helper must not
  // throw if a field is undefined (defensive — keeps the overlay renderable).
  const s = buildMandatoryUpdateStrings({});
  assert.equal(s.title, "Mise à jour requise");
  assert.match(s.body, /La version  est disponible \(tu as la \)\./);
});
