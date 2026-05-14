/**
 * Tests for the codex-OAuth -> API-key auto-fallback decision logic.
 *
 * Pulls `decideFallback` from the single source of truth at
 * `src/desktop-shell/codex-fallback-policy.js`. The bootstrap module
 * (`app-bootstrap.js`) imports the same function, so these tests catch
 * real bugs rather than a drifting replica.
 *
 * Decision outputs:
 *   { action: "noop" | "switch-to-native" | "reveal-key-input"
 *           | "clear-fallback-flag" | "auto-restore-oauth",
 *     openaiOk: boolean }
 */
import test from "node:test";
import assert from "node:assert/strict";
import { decideFallback } from "../src/desktop-shell/codex-fallback-policy.js";

// ── native-oauth: rate-limited fallback ────────────────────────────

test("native-oauth + rate-limited + saved API key -> switch-to-native (ok)", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "switch-to-native", openaiOk: true });
});

test("native-oauth + rate-limited + saved API key but invalid -> switch-to-native (not ok)", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "sk-bad",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "switch-to-native", openaiOk: false });
});

test("native-oauth + rate-limited + no API key -> reveal-key-input", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "reveal-key-input", openaiOk: false });
});

test("native-oauth + rate-limited + whitespace-only API key -> reveal-key-input", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "   ",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "reveal-key-input", openaiOk: false });
});

// ── native-oauth: other failure modes do NOT trigger fallback ──────

test("native-oauth + auth failure -> noop (let user re-login manually)", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "auth" },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: false });
});

test("native-oauth + network failure -> noop", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "network" },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: false });
});

test("native-oauth + unknown failure (null reason) -> noop", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: null },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: false });
});

// ── codex (legacy mode) is never auto-switched ─────────────────────

test("codex + rate-limited -> noop (legacy mode never auto-switches)", () => {
  const out = decideFallback({
    llmBackend: "codex",
    autoFallbackActive: false,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: false });
});

// ── auto-restore: native + flag set + OAuth back ───────────────────

test("native + auto-fallback flag + OAuth probe ok -> auto-restore-oauth", () => {
  const out = decideFallback({
    llmBackend: "native",
    autoFallbackActive: true,
    probeResult: { logged_in: true, failure_reason: null },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "auto-restore-oauth", openaiOk: true });
});

test("native + auto-fallback flag + OAuth still rate-limited -> noop (stay on native)", () => {
  const out = decideFallback({
    llmBackend: "native",
    autoFallbackActive: true,
    probeResult: { logged_in: false, failure_reason: "rate_limited" },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: true });
});

test("native + no auto-fallback flag -> noop (user picked native explicitly)", () => {
  const out = decideFallback({
    llmBackend: "native",
    autoFallbackActive: false,
    probeResult: { logged_in: true, failure_reason: null },
    savedApiKey: "sk-abc",
    nativeKeyOk: true,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: true });
});

test("native + flag + invalid API key -> noop (don't trigger restore on broken state)", () => {
  const out = decideFallback({
    llmBackend: "native",
    autoFallbackActive: true,
    probeResult: { logged_in: true, failure_reason: null },
    savedApiKey: "sk-abc",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: false });
});

// ── clear-fallback-flag housekeeping ───────────────────────────────

test("native-oauth + probe ok + flag set -> clear-fallback-flag (housekeeping)", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: true,
    probeResult: { logged_in: true, failure_reason: null },
    savedApiKey: "",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "clear-fallback-flag", openaiOk: true });
});

test("native-oauth + probe ok + no flag -> noop", () => {
  const out = decideFallback({
    llmBackend: "native-oauth",
    autoFallbackActive: false,
    probeResult: { logged_in: true, failure_reason: null },
    savedApiKey: "",
    nativeKeyOk: false,
  });
  assert.deepEqual(out, { action: "noop", openaiOk: true });
});
