/**
 * Tests for the activation-code (comp-code) modal — MON-C / MON-D / MON-E
 * (`src/desktop-shell/app-redeem-modal.js`).
 *
 * Unlike the CSV/watchlist modals (which touch document/Tauri at import time
 * and are therefore tested via replicated logic), app-redeem-modal.js does no
 * DOM work at import time — every browser API is reached lazily inside a
 * function. So we import the REAL module and exercise the actual exported
 * helpers (a stronger contract than replication, same philosophy as
 * legal-consent.test.js).
 *
 * The module's only top-level dependency is `escapeHtml` imported from the
 * Tauri-WebView-absolute path `/desktop-shell/ui-display-utils.js`, which Node
 * cannot resolve. We register an in-process ESM resolve hook (Node ≥18.19's
 * `module.register`) that rewrites `/desktop-shell/*` to the real `src/`
 * directory — so the import resolves to the genuine source, not a copy.
 *
 * Coverage:
 *   - buildRedeemResultMessage: success + every error code, INCLUDING the
 *     two MON-E codes (`alfred_redeem_expired`, `alfred_redeem_device_limit`)
 *     with the exact FR-tutoiement copy from the spec.
 *   - buildRedeemIntro: activate vs renew.
 *   - formatExpiryFr: epoch → FR date, and the invalid-input guards.
 *   - normalizeRedeemedCode / buildRedeemedCodeRowHtml: MON-E read-only code
 *     row (present only when a code exists; escaped; absent when blank).
 *   - bridgeErrorCode: string vs {code} vs {message} extraction.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// ── In-process resolver: /desktop-shell/* → real src/desktop-shell/* ──
const SRC_ROOT = pathToFileURL(
  path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../src/") + "/"
).href;
register(
  "data:text/javascript," +
    encodeURIComponent(
      `export async function resolve(spec, ctx, next){` +
        `if(spec.startsWith("/desktop-shell/")){` +
        `return next(${JSON.stringify(SRC_ROOT)}+spec.slice(1), ctx);}` +
        `return next(spec, ctx);}`
    ),
  import.meta.url
);

const mod = await import("../src/desktop-shell/app-redeem-modal.js");
const {
  buildRedeemResultMessage,
  buildRedeemIntro,
  formatExpiryFr,
  normalizeRedeemedCode,
  buildRedeemedCodeRowHtml,
  bridgeErrorCode,
} = mod.__test;

// ── buildRedeemResultMessage — success ───────────────────────────────

test("success message includes the expiry date when present", () => {
  // 2027-01-15 00:00 UTC — formatExpiryFr renders local date; assert the
  // success tone + that an expiry clause is appended.
  const msg = buildRedeemResultMessage({ kind: "success", expiresAt: 1_800_000_000 });
  assert.equal(msg.tone, "success");
  assert.match(msg.text, /Code activé/);
  assert.match(msg.text, /valide jusqu'au/);
});

test("success message omits the expiry clause when expiry is absent/invalid", () => {
  const msg = buildRedeemResultMessage({ kind: "success", expiresAt: null });
  assert.equal(msg.tone, "success");
  assert.match(msg.text, /accès illimité débloqué/);
  assert.doesNotMatch(msg.text, /valide jusqu'au/);
});

// ── buildRedeemResultMessage — error codes ───────────────────────────

test("maps alfred_redeem_invalid to the invalid-code copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "alfred_redeem_invalid" });
  assert.equal(msg.tone, "error");
  assert.match(msg.text, /Code invalide/);
});

test("maps alfred_redeem_already_used to the already-used copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "alfred_redeem_already_used" });
  assert.equal(msg.tone, "error");
  assert.match(msg.text, /déjà été utilisé/);
});

test("MON-E: maps alfred_redeem_expired to the exact spec copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "alfred_redeem_expired" });
  assert.equal(msg.tone, "error");
  assert.equal(
    msg.text,
    "Ce code a expiré. Demande un nouveau code pour réactiver l'accès illimité."
  );
});

test("MON-E: maps alfred_redeem_device_limit to the exact spec copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "alfred_redeem_device_limit" });
  assert.equal(msg.tone, "error");
  assert.equal(
    msg.text,
    "Ce code est déjà utilisé sur 2 appareils. Écris à l'auteur pour obtenir un nouveau code."
  );
});

test("maps alfred_api_not_configured to the server-unavailable copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "alfred_api_not_configured" });
  assert.equal(msg.tone, "error");
  assert.match(msg.text, /serveur indisponible/);
});

test("unknown error codes fall back to the generic retry copy", () => {
  const msg = buildRedeemResultMessage({ kind: "error", code: "something_unexpected" });
  assert.equal(msg.tone, "error");
  assert.match(msg.text, /Réessaie plus tard/);
});

// ── buildRedeemIntro — mode pivot ────────────────────────────────────

test("intro defaults to the activate copy", () => {
  const { title, intro } = buildRedeemIntro();
  assert.match(title, /Activer l'accès illimité/);
  assert.match(intro, /en échange d'un feedback/);
});

test("intro pivots to the renewal copy in renew mode", () => {
  const { title, intro } = buildRedeemIntro("renew");
  assert.match(title, /renouvellement/);
  assert.match(intro, /expiré/);
});

// ── formatExpiryFr — guards ──────────────────────────────────────────

test("formatExpiryFr returns empty string for non-finite / non-positive input", () => {
  assert.equal(formatExpiryFr(0), "");
  assert.equal(formatExpiryFr(-1), "");
  assert.equal(formatExpiryFr(NaN), "");
  assert.equal(formatExpiryFr("nope"), "");
});

test("formatExpiryFr renders a zero-padded JJ/MM/AAAA date", () => {
  const out = formatExpiryFr(1_800_000_000);
  assert.match(out, /^\d{2}\/\d{2}\/\d{4}$/);
});

// ── MON-E: redeemed-code row helpers ─────────────────────────────────

test("normalizeRedeemedCode trims strings and rejects non-strings/blank", () => {
  assert.equal(normalizeRedeemedCode("  ABC-123  "), "ABC-123");
  assert.equal(normalizeRedeemedCode(""), "");
  assert.equal(normalizeRedeemedCode("   "), "");
  assert.equal(normalizeRedeemedCode(undefined), "");
  assert.equal(normalizeRedeemedCode(null), "");
  assert.equal(normalizeRedeemedCode(42), "");
});

test("buildRedeemedCodeRowHtml renders a copyable read-only row for a code", () => {
  const html = buildRedeemedCodeRowHtml("ABCD-1234");
  assert.match(html, /redeem-active-code-row/);
  assert.match(html, /redeem-copy-code-btn/);
  assert.match(html, /ABCD-1234/);
  assert.match(html, /Ton code d'activation/);
});

test("buildRedeemedCodeRowHtml is empty when there is no code (fully additive)", () => {
  assert.equal(buildRedeemedCodeRowHtml(""), "");
  assert.equal(buildRedeemedCodeRowHtml("   "), "");
  assert.equal(buildRedeemedCodeRowHtml(undefined), "");
});

test("buildRedeemedCodeRowHtml escapes HTML in the code (no injection)", () => {
  const html = buildRedeemedCodeRowHtml('<img src=x onerror=alert(1)>');
  assert.doesNotMatch(html, /<img/);
  assert.match(html, /&lt;img/);
});

// ── bridgeErrorCode — error extraction ───────────────────────────────

test("bridgeErrorCode extracts a code from string / object / message errors", () => {
  assert.equal(bridgeErrorCode("alfred_redeem_expired"), "alfred_redeem_expired");
  assert.equal(bridgeErrorCode({ code: "alfred_redeem_device_limit" }), "alfred_redeem_device_limit");
  assert.equal(bridgeErrorCode({ message: "alfred_redeem_invalid" }), "alfred_redeem_invalid");
  assert.equal(bridgeErrorCode(null), "");
});
