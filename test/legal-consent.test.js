/**
 * Unit tests for the first-launch legal/liability consent gate
 * (`src/desktop-shell/app-legal-consent.js`).
 *
 * Unlike app-csv-confirm-modal (which touches document/Tauri at import time and
 * therefore has to be tested via replicated logic), app-legal-consent.js does no
 * DOM/Tauri work at import time — every browser API is reached lazily inside a
 * function. So we import the REAL module and exercise the actual exported
 * helpers, which is a stronger contract than replication.
 *
 * Coverage:
 *   - needsLegalConsent: the gate predicate across missing / older / current /
 *     newer / malformed prefs.
 *   - allAcknowledged: all/any/missing checkbox states.
 *   - buildConsentRecord: the exact persisted shape (pins the audit contract).
 *   - enforceLegalConsentGate: reads prefs, skips when already consented,
 *     prompts + persists on accept, and is a graceful no-op without a bridge.
 *   - the modal's accept-enable wiring (allAcknowledged driven from the DOM).
 */
import test from "node:test";
import assert from "node:assert/strict";

import {
  LEGAL_CONSENT_VERSION,
  needsLegalConsent,
  allAcknowledged,
  buildConsentRecord,
  enforceLegalConsentGate,
  openLegalConsentModal,
  __test,
} from "../src/desktop-shell/app-legal-consent.js";

// ── needsLegalConsent ─────────────────────────────────────────────

test("needsLegalConsent: no prefs at all → true", () => {
  assert.equal(needsLegalConsent(null, 1), true);
  assert.equal(needsLegalConsent(undefined, 1), true);
  assert.equal(needsLegalConsent({}, 1), true);
});

test("needsLegalConsent: older recorded version → true", () => {
  assert.equal(needsLegalConsent({ legal_consent: { version: 0 } }, 1), true);
});

test("needsLegalConsent: current recorded version → false", () => {
  assert.equal(needsLegalConsent({ legal_consent: { version: 1 } }, 1), false);
});

test("needsLegalConsent: recorded version greater than current → false", () => {
  // A user who accepted a newer schema (e.g. after a downgrade) is still covered.
  assert.equal(needsLegalConsent({ legal_consent: { version: 2 } }, 1), false);
});

test("needsLegalConsent: malformed legal_consent → true", () => {
  assert.equal(needsLegalConsent({ legal_consent: {} }, 1), true);
  assert.equal(needsLegalConsent({ legal_consent: null }, 1), true);
  assert.equal(needsLegalConsent({ legal_consent: { version: "1" } }, 1), true);
  assert.equal(needsLegalConsent({ legal_consent: { version: Number.NaN } }, 1), true);
  assert.equal(needsLegalConsent("not-an-object", 1), true);
});

// ── allAcknowledged ───────────────────────────────────────────────

test("allAcknowledged: all three true → true", () => {
  assert.equal(
    allAcknowledged({
      tool_not_advisor: true,
      ai_risk_no_professional: true,
      data_sent_to_provider: true,
    }),
    true
  );
});

test("allAcknowledged: any false → false", () => {
  assert.equal(
    allAcknowledged({
      tool_not_advisor: true,
      ai_risk_no_professional: false,
      data_sent_to_provider: true,
    }),
    false
  );
});

test("allAcknowledged: a missing key → false", () => {
  assert.equal(
    allAcknowledged({ tool_not_advisor: true, ai_risk_no_professional: true }),
    false
  );
  assert.equal(allAcknowledged({}), false);
  assert.equal(allAcknowledged(null), false);
  assert.equal(allAcknowledged(undefined), false);
});

test("allAcknowledged: non-boolean truthy values do not count", () => {
  // Only strict `true` satisfies a checkbox — defends against a checkbox that
  // reports its state as a truthy non-boolean.
  assert.equal(
    allAcknowledged({
      tool_not_advisor: "true",
      ai_risk_no_professional: 1,
      data_sent_to_provider: {},
    }),
    false
  );
});

// ── buildConsentRecord ────────────────────────────────────────────

test("buildConsentRecord pins the exact persisted shape", () => {
  const record = buildConsentRecord(1, "0.4.8", "2026-05-30T10:00:00.000Z");
  assert.deepEqual(record, {
    legal_consent: {
      version: 1,
      accepted_at: "2026-05-30T10:00:00.000Z",
      app_version: "0.4.8",
      acknowledgements: {
        tool_not_advisor: true,
        ai_risk_no_professional: true,
        data_sent_to_provider: true,
      },
    },
  });
});

test("buildConsentRecord ack keys match ACK_KEYS exactly", () => {
  // Guard against the record's ack object drifting from the canonical key set
  // that allAcknowledged enforces.
  const record = buildConsentRecord(LEGAL_CONSENT_VERSION, "unknown", "x");
  assert.deepEqual(
    Object.keys(record.legal_consent.acknowledgements).sort(),
    [...__test.ACK_KEYS].sort()
  );
});

// ── enforceLegalConsentGate ───────────────────────────────────────

test("enforceLegalConsentGate is a no-op without a Tauri invoke", async () => {
  // Web / test context: no bridge → no prefs read, no persist, no throw.
  await assert.doesNotReject(() => enforceLegalConsentGate({}));
});

test("enforceLegalConsentGate skips the modal when already consented", async () => {
  const calls = [];
  const invoke = async (cmd) => {
    calls.push(cmd);
    if (cmd === "get_user_preferences_local") {
      return { legal_consent: { version: LEGAL_CONSENT_VERSION } };
    }
    return undefined;
  };
  await enforceLegalConsentGate({ invoke });
  assert.deepEqual(calls, ["get_user_preferences_local"]);
  assert.ok(
    !calls.includes("save_user_preferences_local"),
    "must not persist when consent already current"
  );
});

test("enforceLegalConsentGate prompts + persists on accept", async () => {
  const dom = installDom();
  try {
    const saved = [];
    const invoke = async (cmd, args) => {
      if (cmd === "get_user_preferences_local") return {}; // fresh install
      if (cmd === "save_user_preferences_local") {
        saved.push(args);
        return undefined;
      }
      return undefined;
    };

    const gatePromise = enforceLegalConsentGate({ invoke });

    // The modal is appended asynchronously after the prefs read resolves.
    await flush();
    const overlay = dom.body.children.find(
      (c) => c.dataset && c.dataset.testid === "legal-consent-modal"
    );
    assert.ok(overlay, "consent overlay must be appended to body");
    assert.equal(overlay.style.zIndex, "10001", "overlay must sit above the splash");

    const acceptBtn = overlay.querySelector(".legal-consent-accept");
    assert.equal(acceptBtn.disabled, true, "accept disabled until all boxes checked");

    // Check the three boxes, firing change after each.
    const boxes = overlay.querySelectorAll(".legal-consent-checkbox");
    assert.equal(boxes.length, 3, "exactly three acknowledgement checkboxes");
    boxes[0].checked = true; boxes[0].dispatchEvent({ type: "change" });
    assert.equal(acceptBtn.disabled, true, "still disabled after one box");
    boxes[1].checked = true; boxes[1].dispatchEvent({ type: "change" });
    assert.equal(acceptBtn.disabled, true, "still disabled after two boxes");
    boxes[2].checked = true; boxes[2].dispatchEvent({ type: "change" });
    assert.equal(acceptBtn.disabled, false, "enabled once all three checked");

    acceptBtn.click();
    await gatePromise;

    assert.equal(saved.length, 1, "must persist exactly once on accept");
    const record = saved[0].prefs;
    assert.equal(record.legal_consent.version, LEGAL_CONSENT_VERSION);
    assert.deepEqual(record.legal_consent.acknowledgements, {
      tool_not_advisor: true,
      ai_risk_no_professional: true,
      data_sent_to_provider: true,
    });
    assert.equal(typeof record.legal_consent.accepted_at, "string");
    assert.ok(record.legal_consent.accepted_at.length > 0);
    assert.equal(record.legal_consent.app_version, "unknown"); // no Tauri app API in shim
    assert.equal(
      dom.body.children.includes(overlay),
      false,
      "overlay must be removed from body after accept"
    );
  } finally {
    uninstallDom();
  }
});

test("enforceLegalConsentGate errs toward the gate when reading prefs throws", async () => {
  // If get_user_preferences_local fails we must NOT silently skip the gate —
  // we treat the read as "needs consent" and still show the modal.
  const dom = installDom();
  try {
    const invoke = async (cmd) => {
      if (cmd === "get_user_preferences_local") throw new Error("prefs read failed");
      return undefined;
    };

    // Leave the gate promise pending (the user hasn't accepted): we only assert
    // that the modal was shown. Swallow any later rejection.
    enforceLegalConsentGate({ invoke }).catch(() => {});
    await flush();

    const overlay = dom.body.children.find(
      (c) => c.dataset && c.dataset.testid === "legal-consent-modal"
    );
    assert.ok(overlay, "modal must be shown when the prefs read throws");
  } finally {
    uninstallDom();
  }
});

test("openLegalConsentModal: clicking the overlay backdrop does not resolve and keeps the modal up", async () => {
  const dom = installDom();
  try {
    let resolved = false;
    openLegalConsentModal().then(() => { resolved = true; });
    await flush();
    const overlay = dom.body.children.find(
      (c) => c.dataset && c.dataset.testid === "legal-consent-modal"
    );
    assert.ok(overlay, "consent overlay must be appended to body");

    // Click the backdrop element itself (not a button inside it).
    overlay.click();
    await flush();

    assert.equal(resolved, false, "backdrop click must NOT resolve the gate promise");
    assert.equal(
      dom.body.children.includes(overlay),
      true,
      "overlay must stay present after a backdrop click"
    );
  } finally {
    uninstallDom();
  }
});

test("openLegalConsentModal: refuse closes the app and never resolves", async () => {
  const dom = installDom();
  let closeCalls = 0;
  globalThis.window.__TAURI__ = {
    window: { getCurrentWindow: () => ({ close: async () => { closeCalls += 1; } }) },
  };
  try {
    let resolved = false;
    openLegalConsentModal().then(() => { resolved = true; });
    await flush();
    const overlay = dom.body.children.find(
      (c) => c.dataset && c.dataset.testid === "legal-consent-modal"
    );
    overlay.querySelector(".legal-consent-refuse").click();
    await flush();
    assert.equal(closeCalls, 1, "refuse must close the window");
    assert.equal(resolved, false, "refuse must NOT resolve the gate promise");
  } finally {
    uninstallDom();
  }
});

// ── buildModalHtml renders the verbatim legal copy ────────────────

test("buildModalHtml embeds the verbatim title + intro + scroll hint + all three acknowledgements", () => {
  const html = __test.buildModalHtml();
  assert.ok(html.includes(__test.LEGAL_COPY.title));
  assert.ok(html.includes(__test.LEGAL_COPY.intro));
  assert.ok(html.includes(__test.LEGAL_COPY.scrollHint), "missing scroll-hint affordance copy");
  for (const ack of __test.LEGAL_COPY.acknowledgements) {
    assert.ok(html.includes(ack.text), `missing copy for ${ack.key}`);
    assert.ok(html.includes(`data-ack="${ack.key}"`), `missing checkbox for ${ack.key}`);
  }
  assert.ok(html.includes(__test.LEGAL_COPY.accept));
  assert.ok(html.includes(__test.LEGAL_COPY.refuse));
});

// Pin the two product-owner-mandated sentences verbatim so the legal copy is
// contract-tested and cannot silently drift away from the approved wording.
test("legal copy pins the legal-capacity sentence on the AI-risk acknowledgement", () => {
  const ack = __test.LEGAL_COPY.acknowledgements.find(
    (a) => a.key === "ai_risk_no_professional"
  );
  assert.ok(ack, "ai_risk_no_professional acknowledgement must exist");
  assert.ok(
    ack.text.includes(
      "Je confirme être majeur et disposer de la capacité juridique pour accepter ces conditions en mon nom propre."
    ),
    "missing the legal-capacity / majority sentence"
  );
});

test("legal copy pins the accurate enrichment-API data-flow sentence on the data-transmission acknowledgement", () => {
  const ack = __test.LEGAL_COPY.acknowledgements.find(
    (a) => a.key === "data_sent_to_provider"
  );
  assert.ok(ack, "data_sent_to_provider acknowledgement must exist");
  // The consent must be accurate: Alfred runs a hosted enrichment API that
  // receives ONLY public market identifiers/data — never positions, amounts,
  // balances, or identifying info. It must NOT overclaim zero transmission.
  assert.equal(
    ack.text,
    "Je comprends qu'en connectant un fournisseur d'IA (par exemple OpenAI / ChatGPT), mes données financières personnelles (positions, montants, composition de mon portefeuille) sont transmises à ce fournisseur tiers afin d'être analysées, et qu'elles sont alors soumises aux conditions d'utilisation et à la politique de confidentialité de ce fournisseur, sur lesquels Alfred n'exerce aucun contrôle. Je comprends également qu'Alfred utilise un service d'enrichissement (API) qui ne reçoit que des identifiants et données de marché publics (symboles boursiers, codes ISIN, noms d'émetteurs, indicateurs financiers publics, résumés d'actualité) afin de récupérer cours et informations — jamais mes positions, mes montants, mes soldes, ni aucune information permettant de m'identifier. J'accepte ces transmissions de données.",
    "data_sent_to_provider copy must match the approved accurate enrichment-API wording verbatim"
  );
  // Guard against any resurgence of the misleading "Alfred transmits nothing" claim.
  assert.ok(
    !ack.text.includes("ne collecte, ne stocke, ni ne transmet"),
    "must NOT overclaim that Alfred transmits/stores nothing — it runs a hosted enrichment API"
  );
  assert.ok(
    ack.text.endsWith("J'accepte ces transmissions de données."),
    "the acknowledgement must end on the (plural) data-transmissions acceptance sentence"
  );
});

// ── Minimal DOM shim ──────────────────────────────────────────────
//
// Mirrors the lightweight shim in free-tier-modal.test.js: just enough of
// HTMLElement + document + window for the modal to construct, query, toggle
// checkbox state, and fire click/change handlers under `node --test`.

function flush() {
  // Let queued microtasks (the awaited prefs read, then modal append) settle.
  return new Promise((resolve) => setTimeout(resolve, 0));
}

class MockElement {
  constructor(tagName) {
    this.tagName = String(tagName || "div").toUpperCase();
    this.children = [];
    this.parentNode = null;
    this._className = "";
    this._innerHTML = "";
    this.dataset = {};
    this.style = {};
    this.checked = false;
    this.disabled = false;
    this._listeners = {};
  }
  get className() { return this._className; }
  set className(v) { this._className = String(v || ""); }
  get innerHTML() { return this._innerHTML; }
  set innerHTML(html) {
    this._innerHTML = String(html || "");
    this.children = [];
    // Parse the flat template: recognise tag + class + data-ack so
    // querySelector(All) works. The template has no nesting we need to model
    // beyond "all descendants of the card", so a flat tag scan suffices.
    const tagRegex = /<(\w+)([^>]*)>/g;
    let m;
    while ((m = tagRegex.exec(this._innerHTML)) !== null) {
      const node = new MockElement(m[1]);
      const attrs = m[2] || "";
      const classMatch = attrs.match(/class="([^"]+)"/);
      if (classMatch) node.className = classMatch[1];
      const ackMatch = attrs.match(/data-ack="([^"]+)"/);
      if (ackMatch) node.dataset.ack = ackMatch[1];
      if (/\bdisabled\b/.test(attrs)) node.disabled = true;
      node.parentNode = this;
      this.children.push(node);
    }
  }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  removeChild(child) {
    const i = this.children.indexOf(child);
    if (i >= 0) { this.children.splice(i, 1); child.parentNode = null; }
    return child;
  }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  _all() {
    const out = [];
    for (const c of this.children) { out.push(c); if (c._all) out.push(...c._all()); }
    return out;
  }
  _matches(sel) {
    if (sel.startsWith(".")) return this._className.split(/\s+/).includes(sel.slice(1));
    return this.tagName === sel.toUpperCase();
  }
  querySelector(sel) { return this._all().find((d) => d._matches(sel)) || null; }
  querySelectorAll(sel) { return this._all().filter((d) => d._matches(sel)); }
  addEventListener(type, fn) {
    (this._listeners[type] = this._listeners[type] || []).push(fn);
  }
  dispatchEvent(ev) {
    for (const fn of this._listeners[ev.type] || []) fn({ target: this, ...ev });
    return true;
  }
  click() { this.dispatchEvent({ type: "click", target: this }); }
}

function installDom() {
  const body = new MockElement("body");
  globalThis.document = {
    body,
    createElement: (tag) => new MockElement(tag),
  };
  globalThis.window = {};
  return { body };
}

function uninstallDom() {
  delete globalThis.document;
  delete globalThis.window;
}
