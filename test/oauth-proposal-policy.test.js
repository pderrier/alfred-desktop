/**
 * Tests for the v0.2.16 OAuth-availability proposal banner policy.
 *
 * Pulls `decideOauthProposal` from the single source of truth at
 * `src/desktop-shell/codex-fallback-policy.js`. The bootstrap module
 * (`app-bootstrap.js`) imports the same function so these tests catch
 * real bugs rather than a drifting replica.
 *
 * Decision outputs:
 *   { kind: "none" | "propose-native-to-codex" | "propose-restore-oauth" }
 */
import test from "node:test";
import assert from "node:assert/strict";
import { decideOauthProposal } from "../src/desktop-shell/codex-fallback-policy.js";

const NOW = 1_700_000_000_000;

// ── native mode (paid API key) ────────────────────────────────────────

test("native + OAuth probe ok -> propose-native-to-codex", () => {
  const out = decideOauthProposal({
    llmBackend: "native",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "propose-native-to-codex" });
});

test("native + OAuth probe fails -> none", () => {
  const out = decideOauthProposal({
    llmBackend: "native",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: false,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

// ── codex + apikey paths ──────────────────────────────────────────────

test("codex+apikey + auto-fallback flag set + probe ok -> none (let auto-restore run)", () => {
  // v0.2.14 auto-restore owns recovery here; the banner must NOT compete.
  const out = decideOauthProposal({
    llmBackend: "codex",
    codexCurrentAuth: "apikey",
    hasOauthBackup: true,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: true,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

test("codex+apikey + no auto-fallback flag + backup exists + probe ok -> propose-restore-oauth", () => {
  // User swapped manually (or carried over from old app version).
  // The auto-restore path doesn't trigger — banner takes the slot.
  const out = decideOauthProposal({
    llmBackend: "codex",
    codexCurrentAuth: "apikey",
    hasOauthBackup: true,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "propose-restore-oauth" });
});

test("codex+apikey + no backup -> none (can't restore without backup)", () => {
  // No on-disk OAuth backup => the banner can't offer a one-click restore
  // (a full device-auth flow is out of scope for the non-blocking banner).
  const out = decideOauthProposal({
    llmBackend: "codex",
    codexCurrentAuth: "apikey",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

test("codex+chatgpt -> none (already optimal)", () => {
  const out = decideOauthProposal({
    llmBackend: "codex",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

// ── native-oauth: already on OAuth path ───────────────────────────────

test("native-oauth + probe ok -> none (already on OAuth)", () => {
  const out = decideOauthProposal({
    llmBackend: "native-oauth",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

// ── dismiss states ────────────────────────────────────────────────────

test("dismissedUntilMs > now -> none (snoozed)", () => {
  const out = decideOauthProposal({
    llmBackend: "native",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: NOW + 1000,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

test("dismissedUntilMs == now -> propose (boundary: snooze just expired)", () => {
  const out = decideOauthProposal({
    llmBackend: "native",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: false,
    oauthProbeOk: true,
    dismissedUntilMs: NOW,
    permanentlyDismissed: false,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "propose-native-to-codex" });
});

test("permanentlyDismissed=true -> none (even with everything else green)", () => {
  const out = decideOauthProposal({
    llmBackend: "native",
    codexCurrentAuth: "chatgpt",
    hasOauthBackup: true,
    oauthProbeOk: true,
    dismissedUntilMs: 0,
    permanentlyDismissed: true,
    codexAuthAutoFallbackActive: false,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "none" });
});

// ── back-compat: missing inputs default safely ────────────────────────

test("missing inputs (empty object) -> none (back-compat: nothing to propose)", () => {
  const out = decideOauthProposal({});
  assert.deepEqual(out, { kind: "none" });
});

test("undefined input -> none (defensive)", () => {
  const out = decideOauthProposal(undefined);
  assert.deepEqual(out, { kind: "none" });
});

test("native + probe ok + dismiss fields missing -> propose (defaults treated as not-dismissed)", () => {
  // Older callers may not pass dismissedUntilMs / permanentlyDismissed.
  // Defaults must not suppress the banner.
  const out = decideOauthProposal({
    llmBackend: "native",
    oauthProbeOk: true,
    nowMs: NOW,
  });
  assert.deepEqual(out, { kind: "propose-native-to-codex" });
});
