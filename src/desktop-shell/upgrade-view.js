/**
 * Upgrade-flow module — v0.4.0 P0-15.
 *
 * Owns the Lemon Squeezy checkout overlay + activation handshake. The
 * upgrade flow lives behind the gate dispatched by P0-13's "Upgrade —
 * 9€/an" CTA (`alfred://upgrade-requested` window event), and ends with
 * a Premium toast + a refreshHealthPill so the next analysis is
 * immediately unlocked.
 *
 * # Activation pipeline
 *
 * Two parallel entry points feed the same activation handler so the
 * code path is one regardless of overlay outcome:
 *
 * 1. **Primary — LS overlay success callback.** `LemonSqueezy.Setup({
 *    eventHandler })` registers a window callback. On `Checkout.Success`
 *    LS hands us the license_key in `event.data`. We forward it to
 *    `bridge.licenseActivate(key, instance_name)` which proxies through
 *    `POST /license/activate`. Success → fire `alfred://upgrade-activated`
 *    on window + close overlay.
 *
 * 2. **Fallback — `alfred://license-activated` Tauri event.** When the
 *    LS overlay can't close cleanly (Linux WebKitGTK quirk), LS
 *    redirects the system browser to a server endpoint that 302s to
 *    `alfred://license-activated?key=<key>`. The Tauri deep-link
 *    listener (main.rs setup hook) parses the key and emits the event
 *    to JS. The same `handleLicenseKey` runs.
 *
 * # Provider-not-configured graceful path
 *
 * When the server returns 503 `license_provider_not_configured` (empty
 * `LEMON_SQUEEZY_API_KEY` — typical on first deploy before Pierre wires
 * his LS account), the Rust client maps it to the structured code
 * `alfred_license_provider_not_configured`. We surface a clean French
 * banner ("Activation Premium temporairement indisponible — réessaie
 * dans quelques heures.") rather than a crash or a confusing HTTP error.
 *
 * # Why a dedicated module
 *
 * - Lazy-loads `lemon.js` so the splash doesn't pay a network hit until
 *   the user actually clicks Upgrade. Critical for cold-start latency
 *   per `docs/desktop-api-integration.md` § splash screen probe.
 * - Single source of truth for the LS checkout URL (sourced from Rust
 *   via `license_checkout_url_local` → compile-time
 *   `ALFRED_LS_CHECKOUT_URL` env var). Never hard-coded in JS.
 * - Testable: pure factory `installUpgradeFlow({doc, bridge})` returns
 *   `{open, close}` — no implicit DOM/window lookups beyond `doc` and
 *   `window`-via-`doc.defaultView`.
 */

/**
 * URL of the Lemon Squeezy overlay script. Loaded lazily on first
 * `open()` call — never on splash. Same URL across sandbox + prod
 * environments (LS handles environment scoping via the checkout URL,
 * not the script tag).
 */
const LEMON_JS_URL = "https://app.lemonsqueezy.com/js/lemon.js";

/**
 * Window event the success callback dispatches. Consumed by app.js to
 * fire the "Premium activé" toast + refreshHealthPill so the next
 * analysis picks up the new tier without a manual reload.
 */
const UPGRADE_ACTIVATED_EVENT = "alfred://upgrade-activated";

/**
 * Tauri event emitted by the deep-link listener (main.rs setup hook)
 * when LS redirects through the system browser. Same payload shape as
 * the LS overlay success callback ({key: string}) so the activation
 * pipeline is one.
 */
const TAURI_LICENSE_ACTIVATED_EVENT = "alfred://license-activated";

/**
 * Lazy-load the LS overlay script. Idempotent — returns the cached
 * promise on subsequent calls so the script tag is never injected
 * twice. The script writes to `window.LemonSqueezy` once parsed.
 *
 * @param {Document} doc
 * @returns {Promise<void>}
 */
function ensureLemonJsLoaded(doc) {
  if (typeof window !== "undefined" && window.LemonSqueezy) {
    return Promise.resolve();
  }
  // Already injected but still parsing — wait for the script tag's load
  // event rather than re-injecting.
  const existing = doc.querySelector(`script[data-alfred-lemon-loader="1"]`);
  if (existing) {
    if (existing.__alfredReadyPromise) {
      return existing.__alfredReadyPromise;
    }
    existing.__alfredReadyPromise = new Promise((resolve, reject) => {
      existing.addEventListener("load", () => resolve());
      existing.addEventListener("error", () =>
        reject(new Error("lemon_js_load_failed")));
    });
    return existing.__alfredReadyPromise;
  }
  return new Promise((resolve, reject) => {
    const script = doc.createElement("script");
    script.src = LEMON_JS_URL;
    script.async = true;
    script.setAttribute("data-alfred-lemon-loader", "1");
    script.addEventListener("load", () => resolve());
    script.addEventListener("error", () =>
      reject(new Error("lemon_js_load_failed")));
    script.__alfredReadyPromise = new Promise((res, rej) => {
      script.addEventListener("load", () => res());
      script.addEventListener("error", () =>
        rej(new Error("lemon_js_load_failed")));
    });
    (doc.head || doc.body || doc.documentElement).appendChild(script);
  });
}

/**
 * Detect whether the embedded `lemon.js` is available + ready. Mirrors
 * the LS overlay contract: `window.LemonSqueezy.Setup` + `Url.Open`.
 */
function isLemonSqueezyReady() {
  return Boolean(
    typeof window !== "undefined" &&
    window.LemonSqueezy &&
    typeof window.LemonSqueezy.Setup === "function" &&
    window.LemonSqueezy.Url &&
    typeof window.LemonSqueezy.Url.Open === "function"
  );
}

/**
 * Pure helper: classify a license activation error code. Used to
 * branch the modal copy between "provider not configured" (state we
 * expect during the first days post-launch before Pierre's LS account
 * is wired) and any other error.
 *
 * @param {Error|string|unknown} error
 * @returns {{code: string, isProviderNotConfigured: boolean, message: string}}
 */
export function classifyLicenseError(error) {
  const raw = String(error?.message || error?.code || error || "");
  const isProviderNotConfigured = raw.includes(
    "alfred_license_provider_not_configured"
  );
  return {
    code: raw,
    isProviderNotConfigured,
    message: raw,
  };
}

/**
 * Detect the host operating system for the LS `instance_name`. The LS
 * dashboard shows this string so the user can identify "this machine"
 * later (P2-8 multi-device deactivation). Best-effort — the Rust side
 * also derives a hostname when this is empty.
 */
function detectInstanceName() {
  if (typeof navigator === "undefined" || !navigator.userAgent) {
    return "";
  }
  const ua = navigator.userAgent;
  if (/Mac OS X|Macintosh/.test(ua)) return "alfred-desktop@macOS";
  if (/Windows/.test(ua)) return "alfred-desktop@Windows";
  if (/Linux|X11/.test(ua)) return "alfred-desktop@Linux";
  return "alfred-desktop";
}

/**
 * Install the upgrade flow. Returns `{open, close}` — call `open()`
 * from the upgrade-requested handler in app.js. Idempotent on the
 * factory level: calling `installUpgradeFlow` twice with the same
 * `{doc, bridge}` returns an independent controller, but the underlying
 * `lemon.js` is only loaded once per document.
 *
 * @param {{
 *   doc?: Document,
 *   bridge: object,
 *   showToast?: (message: string, type?: string) => void,
 * }} options
 */
export function installUpgradeFlow({ doc = (typeof document !== "undefined" ? document : null), bridge, showToast = null } = {}) {
  if (!doc) {
    throw new Error("upgrade_flow_requires_document");
  }
  if (!bridge || typeof bridge.licenseActivate !== "function") {
    throw new Error("upgrade_flow_requires_bridge_with_licenseActivate");
  }

  let isOpen = false;
  let tauriUnlisten = null;
  // Guard against double-fire of activation (overlay success + deep-link
  // racing). The first call wins, the second is a no-op.
  let activationInProgress = false;

  /**
   * Activate the license key against the server. Used by both the LS
   * overlay success callback and the deep-link fallback path.
   *
   * @param {string} licenseKey
   */
  async function handleLicenseKey(licenseKey) {
    if (activationInProgress) {
      return;
    }
    activationInProgress = true;
    const instanceName = detectInstanceName();
    try {
      const result = await bridge.licenseActivate(licenseKey, instanceName);
      // Fire the upgrade-activated event for app.js consumer (toast +
      // refreshHealthPill).
      if (typeof window !== "undefined" && typeof window.dispatchEvent === "function") {
        const event = typeof CustomEvent !== "undefined"
          ? new CustomEvent(UPGRADE_ACTIVATED_EVENT, { detail: result || {} })
          : { type: UPGRADE_ACTIVATED_EVENT, detail: result || {} };
        window.dispatchEvent(event);
      }
      close();
    } catch (err) {
      activationInProgress = false;
      const classified = classifyLicenseError(err);
      if (showToast) {
        if (classified.isProviderNotConfigured) {
          showToast(
            "Activation Premium temporairement indisponible — réessaie dans quelques heures.",
            "error"
          );
        } else {
          showToast(
            `Activation Premium échouée : ${classified.message}`,
            "error"
          );
        }
      }
    }
  }

  /**
   * Subscribe to the Tauri `alfred://license-activated` event so the
   * deep-link fallback path lands in the same activation handler as
   * the LS overlay callback. Returns the unsubscribe function (or null
   * when Tauri is not available — e.g. unit test environment).
   */
  async function subscribeToDeepLink() {
    if (typeof window === "undefined") return null;
    const tauriEvent = window.__TAURI__?.event;
    if (!tauriEvent || typeof tauriEvent.listen !== "function") {
      return null;
    }
    try {
      const unlisten = await tauriEvent.listen(
        TAURI_LICENSE_ACTIVATED_EVENT,
        (event) => {
          const key = event?.payload?.key;
          if (typeof key === "string" && key.length > 0) {
            handleLicenseKey(key);
          }
        }
      );
      return unlisten;
    } catch (_err) {
      return null;
    }
  }

  /**
   * Open the LS overlay. Lazy-loads lemon.js on first call, registers
   * the success callback, and dispatches `LemonSqueezy.Url.Open`.
   *
   * Steps:
   *   1. Fetch the checkout URL from Rust (compile-time env, never
   *      hard-coded in JS).
   *   2. If unconfigured, show the "temporarily unavailable" toast.
   *   3. Otherwise, lazy-load lemon.js, register the eventHandler, and
   *      open the overlay.
   *   4. Subscribe to the Tauri deep-link event for the fallback path.
   */
  async function open() {
    if (isOpen) return;
    isOpen = true;
    activationInProgress = false;

    // Pull the checkout URL from Rust. Returns
    // `{url: string|null, configured: bool}` — `configured: false` is
    // the public-repo default before Pierre wires LS.
    let checkoutEnvelope;
    try {
      checkoutEnvelope = await bridge.licenseCheckoutUrl();
    } catch (err) {
      isOpen = false;
      if (showToast) {
        showToast(
          `Activation Premium indisponible : ${String(err?.message || err)}`,
          "error"
        );
      }
      return;
    }
    if (!checkoutEnvelope?.configured || !checkoutEnvelope?.url) {
      isOpen = false;
      if (showToast) {
        showToast(
          "Activation Premium temporairement indisponible — réessaie dans quelques heures.",
          "info"
        );
      }
      return;
    }

    // Wire the deep-link fallback BEFORE opening the overlay so a fast
    // overlay close doesn't race past the listener.
    tauriUnlisten = await subscribeToDeepLink();

    // Load lemon.js (no-op if already cached).
    try {
      await ensureLemonJsLoaded(doc);
    } catch (err) {
      isOpen = false;
      if (showToast) {
        showToast(
          `Activation Premium indisponible (script LS) : ${String(err?.message || err)}`,
          "error"
        );
      }
      return;
    }

    if (!isLemonSqueezyReady()) {
      isOpen = false;
      if (showToast) {
        showToast(
          "Activation Premium indisponible — le script Lemon Squeezy n'a pas pu être chargé.",
          "error"
        );
      }
      return;
    }

    // Register the success callback on the LS event channel. Documented
    // LS contract: `Checkout.Success` fires with `data.license_key`
    // populated when the product has License keys enabled (which v0.4.0
    // requires — see `docs/p0-15-launch-prereqs.md` § 1.3).
    window.LemonSqueezy.Setup({
      eventHandler: ({ event, data } = {}) => {
        if (event !== "Checkout.Success") return;
        // LS license key shape varies between sandbox and prod —
        // try the documented `license_key.key` path first, then a
        // couple of fallbacks observed in LS docs / community reports.
        const key =
          data?.license_key?.key ||
          data?.license_key ||
          data?.order?.license_key?.key ||
          null;
        if (typeof key !== "string" || key.length === 0) {
          if (showToast) {
            showToast(
              "Paiement reçu mais aucune licence détectée — contacte le support.",
              "error"
            );
          }
          return;
        }
        handleLicenseKey(key);
      },
    });

    // Open the overlay. LS handles the iframe lifecycle from here.
    try {
      window.LemonSqueezy.Url.Open(checkoutEnvelope.url);
    } catch (err) {
      isOpen = false;
      if (showToast) {
        showToast(
          `Impossible d'ouvrir le checkout : ${String(err?.message || err)}`,
          "error"
        );
      }
    }
  }

  /**
   * Close the overlay + tear down listeners. Idempotent.
   */
  function close() {
    isOpen = false;
    activationInProgress = false;
    if (typeof tauriUnlisten === "function") {
      try { tauriUnlisten(); } catch (_err) { /* noop */ }
    }
    tauriUnlisten = null;
    if (typeof window !== "undefined" && window.LemonSqueezy?.Url?.Close) {
      try { window.LemonSqueezy.Url.Close(); } catch (_err) { /* noop */ }
    }
  }

  return { open, close, handleLicenseKey };
}
