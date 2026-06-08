//! Client for the remote Alfred API server.
//!
//! Calls /api/market, /api/news, /api/search on the remote server.
//! Auth: HMAC-signed requests — the API secret is embedded at compile time
//! (via CI), never exposed in source. The OpenAI JWT never leaves the device.

use std::env;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_API_URL: &str = "https://vps-c5793aab.vps.ovh.net/alfred/api";
const TIMEOUT_SECS: u64 = 10;
/// Timeout for `POST /run/start`. Short on purpose — if the session
/// endpoint isn't responding in 8s, the desktop should fail loudly so
/// the user knows the API is down before they're sunk into a 10-min run.
const RUN_START_TIMEOUT_SECS: u64 = 8;

/// API secret embedded at compile time by CI (ALFRED_API_SECRET env var).
/// In dev builds without the secret, HMAC auth is skipped (API falls back to permissive mode).
const API_SECRET: Option<&str> = option_env!("ALFRED_API_SECRET");

fn api_url() -> Option<String> {
    let enabled = env::var("ALFRED_API_ENABLED")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    Some(
        env::var("ALFRED_API_URL")
            .unwrap_or_else(|_| DEFAULT_API_URL.to_string())
            .trim_end_matches('/')
            .to_string(),
    )
}

// ── HMAC request signing ────────────────────────────────────────────

/// Deterministic HMAC using FNV-1a (stable across compilations, unlike DefaultHasher).
fn hmac_sign(path: &str, timestamp: u64, secret: &str) -> String {
    let msg = format!("{path}:{timestamp}:{secret}");
    let a = fnv1a_64(msg.as_bytes());
    let msg2 = format!("{a:016x}:{secret}:alfred");
    let b = fnv1a_64(msg2.as_bytes());
    format!("{a:016x}{b:016x}")
}

/// FNV-1a 64-bit hash — deterministic, no random seed.
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Get a client hash for rate-limiting (from local JWT, never sent raw).
fn get_client_hash() -> Option<String> {
    let jwt = get_local_jwt()?;
    Some(format!("{:016x}", fnv1a_64(jwt.as_bytes())))
}

/// Apply auth headers to a ureq request: HMAC signature + client hash + timestamp.
///
/// v0.4.0 (P0-11): also injects `X-Run-Session: <id>` when a run-session
/// has been set via `set_active_run_session` (done by
/// `analysis_ops::start_analysis` right after `/run/start` succeeds). The
/// 14 gated analysis endpoints require this header server-side; without it
/// they 401 with `run_session_invalid`. `/run/start` itself does NOT
/// require the header (it's the source of sessions) — the active-session
/// context is `None` when this function runs for that endpoint.
///
/// v0.4.0 (P0-14 + P0-15): account-level endpoints are explicitly exempted
/// from X-Run-Session injection even when the slot is filled. Rationale: if
/// an admin clicks the Admin tab while a run is in flight, or if the user
/// clicks Upgrade and the LS overlay activation handler hits `/license/*`,
/// the active session slot is non-empty, but those endpoints are NOT
/// mounted under `require_run_session` server-side (see
/// `apps/alfred-api/src/lib.rs` — admin routes layered with `require_admin`,
/// license routes layered with `require_auth` only, neither under
/// `require_run_session`). Sending the header would not break anything
/// (the server middleware simply doesn't consume it), but the contract is
/// "endpoints get exactly the auth headers they require, no more".
/// Defense-in-depth — and it keeps the request payload smaller for
/// account-scope polls. See `is_session_exempt_path` for the path-prefix
/// rule.
fn apply_auth(req: ureq::Request, path: &str) -> ureq::Request {
    let ts = now_epoch_secs();
    let client_hash = get_client_hash().unwrap_or_default();
    // Sign only the path component (no query string) — must match server-side req.uri().path()
    let sign_path = path.split('?').next().unwrap_or(path);
    let mut req = req
        .set("X-Client-Hash", &client_hash)
        .set("X-Timestamp", &ts.to_string());
    if !is_session_exempt_path(sign_path) {
        if let Some(session_id) = active_run_session() {
            req = req.set("X-Run-Session", &session_id);
        }
    }

    // ── MON-A: device auth (PREFERRED on the server) ──────────────────
    // When a server-issued device identity exists (registered on first run),
    // add `X-Device-Id` + `X-Device-Signature = HMAC(device_secret, path:ts)`.
    // The redeployed server prefers this stable, non-forgeable identity over
    // `X-Client-Hash`. We ALSO keep the legacy headers below so a NEW desktop
    // build still authenticates against an OLD server (no device-auth branch
    // yet) during the API rollout — the old server simply ignores the device
    // headers and falls back to the legacy HMAC. Additive, never replacing.
    if let Some((device_id, device_secret)) = device_identity() {
        let dsig = hmac_sign(sign_path, ts, &device_secret);
        req = req
            .set("X-Device-Id", &device_id)
            .set("X-Device-Signature", &dsig);
    }

    let runtime_secret = env::var("ALFRED_API_SECRET").ok();
    let secret = API_SECRET.or(runtime_secret.as_deref());
    if let Some(s) = secret {
        let sig = hmac_sign(sign_path, ts, s);
        req.set("X-Signature", &sig)
    } else {
        req
    }
}

/// Pure helper: is this request path an account-level surface that does
/// NOT live under the `require_run_session` middleware? Used by
/// `apply_auth` to skip `X-Run-Session` injection — sending the header
/// would be dead weight (the server middleware doesn't consume it on
/// these routes) and silently leaks a per-run identifier into an
/// account-scope call.
///
/// Match rules:
///   - `/admin` or anything under `/admin/` (v0.4.0 P0-14 — admin
///     observability endpoints, gated by `require_admin` server-side)
///   - `/license` or anything under `/license/` (v0.4.0 P0-15 — LS
///     activation/validation/status, gated by `require_auth` only)
///   - `/quota` or anything under `/quota/` (v0.4.7 P3-31 — read-only
///     home-strip quota probe, mounted at root alongside `/run/start`,
///     gated by `require_auth` only — see `handlers::quota_routes`)
///   - `/device` or anything under `/device/` (MON-A — device identity
///     bootstrap `POST /device/register`, mounted at root, gated by
///     `require_auth` only — see `device::device_routes`)
///   - `/redeem` (MON-B — comp-code redemption, mounted at root, gated by
///     `require_auth` only — see `redeem::redeem_routes`)
///   - `/api/macro` (BUG-1, 2026-06-08 — the home macro briefing is a
///     PRE-RUN, globally-cached read (`macro:briefing:v1`) with no
///     per-user quota/session. Server-side it was moved OUT of the
///     `require_run_session`-gated `analysis_routes` to sit behind
///     `require_auth` only — see `build_router` in `alfred-api/src/lib.rs`.
///     EXACT match: `/api/market`, `/api/news`, etc. stay gated.)
///
/// Anything else returns false — there is no fuzzy / partial /
/// case-insensitive match because the server-side routes are
/// case-sensitive too.
///
/// Renamed from `is_admin_path` in v0.4.0 P0-15 (P3-40 follow-up) when
/// `/license/*` joined the exemption set. `/quota/*` joined in v0.4.7
/// (P3-31). `/device*` + `/redeem` joined in MON (2026-05-31).
/// `/api/macro` joined in BUG-1 (2026-06-08). The behavioural contract is
/// pinned by `session_exempt_endpoints_skip_run_session_header`.
fn is_session_exempt_path(path: &str) -> bool {
    path == "/admin"
        || path.starts_with("/admin/")
        || path == "/license"
        || path.starts_with("/license/")
        || path == "/quota"
        || path.starts_with("/quota/")
        || path == "/device"
        || path.starts_with("/device/")
        || path == "/redeem"
        || path == "/api/macro"
}

// ── Run-session context (v0.4.0 P0-11; disk bridge 2026-06-04) ───────
//
// The desktop holds a single, process-wide "active run-session" slot.
// `analysis_ops::start_analysis` calls `set_active_run_session` after a
// successful `POST /run/start`; the worker thread inherits this context
// (all `api_get` / `api_post` calls on that thread will inject the header
// via `apply_auth`). At run completion, `clear_active_run_session`
// resets the slot.
//
// Why a global instead of explicit threading? The enrichment helpers
// (`enrichment::fetch_market`, `remote_fetch_news`, etc.) are called from
// 20+ call sites and pass `Position` / `Ticker` types — adding an
// optional `session_id` everywhere would be a huge surface change for no
// runtime benefit. The desktop runs ONE analysis at a time (verified by
// the cancellation registry and ops_store invariants — see
// `analysis_ops::ops_store`), so a single global slot is correct.
//
// ── Cross-process bridge (2026-06-04) ───────────────────────────────
// The in-memory slot lives in the MAIN process. But codex spawns the MCP
// server as a SEPARATE subprocess (`codex.rs::spawn_with_tools` →
// `mcp_servers.alfred-mcp.command = <self_binary> --mcp-server --data-dir <dir>`).
// That subprocess has its OWN copy of this `static` slot, which nobody ever
// sets — only the main process calls `set_active_run_session`. So when codex
// invokes the `persist_*` MCP tools, the subprocess's `active_run_session()`
// returned `None`, `apply_auth` omitted `X-Run-Session`, and the
// session-gated `/api/insights` etc. endpoints 401'd with
// `run_session_invalid`. Because `api_post` is fire-and-forget, the failure
// was swallowed and the write silently lost (prod: shared-insights frozen
// since ~2026-05-16, the date run-session enforcement deployed).
//
// Fix: `set_active_run_session` ALSO writes the session id + its server
// `expires_at` to a small JSON file in the runtime-state dir; `clear` removes
// it. When the in-memory slot is `None` (i.e. in the MCP subprocess), the
// reader FALLS BACK to that file. The file path is computed identically by
// both processes — see `resolve_session_runtime_state_dir` /
// `session_file_path` below for the make-or-break detail.

/// Filename (inside the runtime-state dir) of the cross-process run-session
/// bridge. Held next to the other `runtime-state/*.json` run artefacts.
const SESSION_FILE_NAME: &str = "active-run-session.json";

static ACTIVE_RUN_SESSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn session_slot() -> &'static Mutex<Option<String>> {
    ACTIVE_RUN_SESSION.get_or_init(|| Mutex::new(None))
}

/// Process-global override for the runtime-state dir used by the session
/// bridge file. Set ONLY by the `--mcp-server` entrypoint
/// (`main.rs::main`) to `<--data-dir>/runtime-state`. In the main process
/// this stays `None` and the bridge derives the dir from
/// `crate::resolve_runtime_state_dir()` directly.
static MCP_RUNTIME_STATE_DIR: OnceLock<Mutex<Option<std::path::PathBuf>>> = OnceLock::new();

fn mcp_runtime_state_dir_slot() -> &'static Mutex<Option<std::path::PathBuf>> {
    MCP_RUNTIME_STATE_DIR.get_or_init(|| Mutex::new(None))
}

/// Pin the runtime-state dir for the session bridge. Called once by the
/// `--mcp-server` entrypoint with `<--data-dir>/runtime-state` so the MCP
/// subprocess reads the session file from the SAME absolute path the main
/// process wrote it to.
///
/// Path-identity invariant (the make-or-break detail):
/// - Main process writes to `crate::resolve_runtime_state_dir()`.
/// - codex.rs spawns the subprocess with
///   `--data-dir = resolve_runtime_state_dir().parent()` (the `data/` dir).
/// - The entrypoint passes `<data-dir>/runtime-state` here, i.e.
///   `resolve_runtime_state_dir().parent().join("runtime-state")`, which
///   equals `resolve_runtime_state_dir()` for the canonical layout — the
///   SAME invariant codex.rs already relies on for every other
///   `runtime-state/<run_id>_*.jsonl` file the subprocess reads/writes.
pub fn set_mcp_runtime_state_dir(dir: impl Into<std::path::PathBuf>) {
    if let Ok(mut slot) = mcp_runtime_state_dir_slot().lock() {
        *slot = Some(dir.into());
    }
}

/// Resolve the runtime-state dir the session bridge file lives in. Honours
/// the MCP-subprocess override when set, otherwise falls back to the
/// canonical main-process resolver.
fn resolve_session_runtime_state_dir() -> std::path::PathBuf {
    if let Ok(slot) = mcp_runtime_state_dir_slot().lock() {
        if let Some(dir) = slot.as_ref() {
            return dir.clone();
        }
    }
    crate::resolve_runtime_state_dir()
}

/// Pure helper: compute the absolute session-bridge file path from a
/// runtime-state dir. Single source of truth so the writer and the reader
/// can NEVER drift on the filename.
fn session_file_path(runtime_state_dir: &std::path::Path) -> std::path::PathBuf {
    runtime_state_dir.join(SESSION_FILE_NAME)
}

/// Serialise `{session_id, expires_at}` and atomically write it to the
/// bridge file. Best-effort: a write failure logs but never blocks the run
/// (the in-memory slot still carries the session for the main process).
fn write_session_file(session_id: &str, expires_at: u64) {
    let dir = resolve_session_runtime_state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        crate::debug_log(&format!(
            "alfred-api: run-session bridge: create_dir_all({}) failed: {e}",
            dir.display()
        ));
        return;
    }
    let path = session_file_path(&dir);
    let payload = serde_json::json!({ "session_id": session_id, "expires_at": expires_at });
    let body = serde_json::to_string(&payload).unwrap_or_default();
    // Atomic write: temp file in the same dir + rename, so a concurrent
    // reader never sees a half-written file.
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, body.as_bytes()) {
        crate::debug_log(&format!(
            "alfred-api: run-session bridge: write tmp {} failed: {e}",
            tmp.display()
        ));
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        crate::debug_log(&format!(
            "alfred-api: run-session bridge: rename to {} failed: {e}",
            path.display()
        ));
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Best-effort remove the bridge file (run completion / cancellation).
fn remove_session_file() {
    let path = session_file_path(&resolve_session_runtime_state_dir());
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            crate::debug_log(&format!(
                "alfred-api: run-session bridge: remove {} failed: {e}",
                path.display()
            ));
        }
    }
}

/// Pure helper: parse a bridge-file body into `Some(session_id)` when it is
/// well-formed AND not expired relative to `now_secs`. Returns `None` for a
/// malformed body or an `expires_at` in the past — a known-expired session
/// must never be sent (it would 401 with `run_session_invalid`). `expires_at`
/// is treated as a hard boundary; `0` (missing/older writer) is treated as
/// "no expiry known" and accepted, since the server is still the authority.
fn parse_session_file(body: &str, now_secs: u64) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let expires_at = parsed.get("expires_at").and_then(|v| v.as_u64()).unwrap_or(0);
    if expires_at != 0 && expires_at <= now_secs {
        return None;
    }
    Some(session_id.to_string())
}

/// Read the session id from the bridge file, honouring `expires_at`. Used as
/// the fallback when the in-memory slot is `None` (the MCP subprocess case).
/// An expired or malformed file is best-effort deleted so it doesn't linger.
fn read_session_file() -> Option<String> {
    let path = session_file_path(&resolve_session_runtime_state_dir());
    let body = std::fs::read_to_string(&path).ok()?;
    match parse_session_file(&body, now_epoch_secs()) {
        Some(id) => Some(id),
        None => {
            // Stale / corrupt — clean it up so we never re-read it.
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Set the active run-session ID + its server `expires_at`. Called by
/// `analysis_ops::start_analysis` after the server issues one via
/// `POST /run/start`.
///
/// Writes BOTH the in-memory slot (fast path, main process) AND the disk
/// bridge file (so the codex-spawned MCP subprocess can read the session —
/// see the module comment). Mutex-protected for the (rare) cancellation /
/// worker completion race. Replaces any prior value — the contract is "one
/// run, one session", and a fresh `/run/start` always supersedes a stale
/// slot.
pub fn set_active_run_session(session_id: impl Into<String>, expires_at: u64) {
    let session_id = session_id.into();
    if let Ok(mut slot) = session_slot().lock() {
        *slot = Some(session_id.clone());
    }
    write_session_file(&session_id, expires_at);
}

/// Clear the active run-session ID. Called on run completion / failure /
/// cancellation so a subsequent `api_get` outside of a run does NOT
/// silently use a stale session header (which would 401 with confusing
/// `run_session_invalid` instead of the cleaner "no session" path). Removes
/// the disk bridge file too, so the MCP subprocess stops reading a dead
/// session.
pub fn clear_active_run_session() {
    if let Ok(mut slot) = session_slot().lock() {
        *slot = None;
    }
    remove_session_file();
}

/// Read the active run-session ID (cloned to avoid holding the lock
/// across the HTTP call). Returns `None` when no run is in flight.
///
/// In-memory slot first (main process). When it's empty — the codex MCP
/// subprocess, which never had the slot set — fall back to the disk bridge
/// file written by the main process. The file read enforces `expires_at`
/// so a known-expired session is never returned.
pub fn active_run_session() -> Option<String> {
    if let Some(id) = session_slot().lock().ok().and_then(|s| s.clone()) {
        return Some(id);
    }
    read_session_file()
}

/// Single canonical serialization lock for tests that read or mutate the
/// process-global `ACTIVE_RUN_SESSION` slot.
///
/// `ACTIVE_RUN_SESSION` is process-wide, so ANY test touching it — whether
/// it lives in `alfred_api_client::tests` (the `apply_auth` / exempt-path
/// suites) or in `analysis_ops::run_session_tests` (`acquire_run_session_*`)
/// — must serialize against EVERY other such test, across module
/// boundaries. A per-module `Mutex` only serializes that module's own
/// tests and lets a sibling module clear the slot mid-assertion (this was
/// the MON-F1 flake: `acquire_run_session_skips_when_api_disabled` cleared
/// the slot while `session_exempt_endpoints_skip_run_session_header` was
/// asserting a non-exempt path still carried `X-Run-Session`).
///
/// Defined next to the slot it guards and exposed `pub(crate)` so both
/// test modules acquire the SAME mutex. Poisoning is tolerated (a panic in
/// one test must not cascade into spurious failures in the next).
#[cfg(test)]
pub(crate) fn run_session_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Test-only: point the session-bridge file at a process-unique temp dir so
/// session-touching tests never read or pollute the real
/// `data/runtime-state/active-run-session.json` (and never race a parallel
/// test binary on that path). Idempotent — repeated calls re-point at a
/// fresh empty dir. Callers already hold `run_session_test_lock`, so the
/// process-global override is uncontended for the duration of the test.
#[cfg(test)]
pub(crate) fn redirect_session_bridge_to_temp_dir() {
    let unique = format!(
        "alfred-session-bridge-test-{}-{}",
        std::process::id(),
        now_epoch_secs(),
    );
    let dir = std::env::temp_dir().join(unique);
    let _ = std::fs::create_dir_all(&dir);
    // Start from a clean slate so a leftover file from a prior test in the
    // same dir can't leak in.
    let _ = std::fs::remove_file(session_file_path(&dir));
    set_mcp_runtime_state_dir(dir);
}

// ── Device identity (MON-A, 2026-05-31) ─────────────────────────────
//
// A server-issued `device_id` + `device_secret` (see
// `apps/alfred-api/src/device.rs`) replaces the volatile/forgeable
// `X-Client-Hash` as the primary identity. Stored in user-preferences
// (merge-only) so it survives across launches; cached in a process-global
// slot to avoid re-reading the file on every `apply_auth`. The cache is
// invalidated by `set_device_identity` after a fresh registration.
//
// Storage note: user-preferences.json is the same store that already holds
// `tier` / `license_key`. An OS-keychain backing is a future hardening
// (the secret is a bearer credential) — flagged in the report, not done
// here to keep the transition surface small.

static DEVICE_IDENTITY: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

fn device_identity_slot() -> &'static Mutex<Option<(String, String)>> {
    DEVICE_IDENTITY.get_or_init(|| Mutex::new(None))
}

/// Return the cached `(device_id, device_secret)` pair, loading it from
/// user-preferences on first access. `None` when the device hasn't been
/// registered yet (first run before `register_device`, or an older install
/// that predates MON-A — both fall back to legacy `X-Client-Hash` auth).
pub fn device_identity() -> Option<(String, String)> {
    {
        let slot = device_identity_slot().lock().ok()?;
        if let Some(pair) = slot.as_ref() {
            return Some(pair.clone());
        }
    }
    // Cache miss → read from preferences once and memoise.
    let pair = read_device_identity_from_prefs()?;
    if let Ok(mut slot) = device_identity_slot().lock() {
        *slot = Some(pair.clone());
    }
    Some(pair)
}

/// Pure helper: extract a `(device_id, device_secret)` pair from a
/// preferences value. Both fields must be present + non-empty. Extracted so
/// the parse contract is unit-testable without touching the filesystem.
pub(crate) fn parse_device_identity(prefs: &Value) -> Option<(String, String)> {
    let id = prefs.get("device_id").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())?;
    let secret = prefs.get("device_secret").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty())?;
    Some((id.to_string(), secret.to_string()))
}

fn read_device_identity_from_prefs() -> Option<(String, String)> {
    let prefs = crate::runtime_settings::get_user_preferences();
    parse_device_identity(&prefs)
}

/// Persist a freshly-registered device identity to user-preferences and
/// update the in-process cache. Called by `register_device` after a
/// successful `POST /device/register`.
fn set_device_identity(device_id: &str, device_secret: &str) {
    let prefs = serde_json::json!({
        "device_id": device_id,
        "device_secret": device_secret,
    });
    if let Err(e) = crate::runtime_settings::save_user_preferences(&prefs) {
        crate::debug_log(&format!("alfred-api: failed to persist device identity: {e}"));
        return;
    }
    if let Ok(mut slot) = device_identity_slot().lock() {
        *slot = Some((device_id.to_string(), device_secret.to_string()));
    }
}

// ── MON-E (P0-83): stable per-machine fingerprint ───────────────────
//
// The server's `/redeem` now binds a comp code to up to 2 machines, keyed
// by a stable machine fingerprint, within a 90-day shared window. The
// desktop sends `machine_hash` on `/device/register` and `/redeem` so the
// same physical machine re-uses its slot across re-installs and code
// re-entries instead of burning a fresh slot each time.
//
// Privacy (feedback_no_leak_tokens / feedback_no_leak_creds): the raw OS
// machine id is NEVER transmitted or logged — only a salted FNV-1a hash.
// The salt (`alfred-mach-v1:`) namespaces the hash to this app + scheme
// version so the same machine id can't be correlated across apps and so a
// future scheme bump (`v2:`) yields fresh slots cleanly.

/// Domain-separation salt for the machine fingerprint. Bump the version
/// suffix to force every machine onto a fresh slot (e.g. if the hash input
/// ever changes shape). Kept here, next to the hasher, so the contract is
/// in one place.
const MACHINE_FINGERPRINT_SALT: &str = "alfred-mach-v1:";

/// User-preferences key holding the persisted random fallback id used when
/// `machine_uid::get()` is unavailable (locked-down OS, container without
/// a machine-id file, etc.). Stored once, then stable — guaranteeing
/// exactly one slot per machine even without an OS machine id.
const DEVICE_MACHINE_FALLBACK_KEY: &str = "device_machine_fallback";

/// Pure helper: salt + FNV-1a hash a raw machine id into the hex string sent
/// as `machine_hash`. Extracted so the hashing contract (salt + encoding) is
/// unit-testable and identical for both the OS-id and fallback-id paths.
fn fingerprint_hash(raw: &str) -> String {
    let salted = format!("{MACHINE_FINGERPRINT_SALT}{raw}");
    format!("{:016x}", fnv1a_64(salted.as_bytes()))
}

/// Generate a fresh random fallback id with no extra rng crate. The id only
/// needs to be unique-enough and is persisted on first generation, so a
/// one-shot mix of the high-resolution clock (nanos), the process id, and an
/// address-space value run through the FNV-1a avalanche is sufficient —
/// after the first call the value is read back from preferences, so
/// determinism is provided by persistence, not by the generator.
fn generate_fallback_machine_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    // A stack address gives a little ASLR entropy without any crate.
    let stack_marker = &nanos as *const u64 as u64;
    let mixed = fnv1a_64(&nanos.to_le_bytes())
        ^ fnv1a_64(&pid.to_le_bytes())
        ^ fnv1a_64(&stack_marker.to_le_bytes());
    format!("{mixed:016x}{nanos:016x}")
}

/// Pure helper: resolve the fallback machine hash from a preferences value.
/// Returns `(hash, patch)` where `patch` is `Some(json)` ONLY when a new id
/// had to be generated (so the caller can persist it merge-only). When the
/// prefs already hold a non-empty `device_machine_fallback`, `patch` is
/// `None` (nothing to write). Splitting the I/O out keeps the
/// generate-or-read decision testable without the filesystem.
fn fallback_fingerprint_from_prefs(prefs: &Value) -> (String, Option<Value>) {
    let existing = prefs
        .get(DEVICE_MACHINE_FALLBACK_KEY)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match existing {
        Some(id) => (fingerprint_hash(id), None),
        None => {
            let id = generate_fallback_machine_id();
            let patch = serde_json::json!({ DEVICE_MACHINE_FALLBACK_KEY: id });
            (fingerprint_hash(&id), Some(patch))
        }
    }
}

/// Return the stable per-machine fingerprint hash sent as `machine_hash`.
///
/// - Primary: the OS-native machine id (`machine_uid::get()`), salted +
///   hashed. Stable across re-installs on the same machine.
/// - Fallback (machine id unavailable): a persisted random id from the
///   `device_machine_fallback` preference, generated once then reused. This
///   guarantees exactly one slot per machine even without an OS machine id.
///
/// NEVER returns or logs the raw machine id — only the salted hash.
pub fn machine_fingerprint() -> String {
    match machine_uid::get() {
        Ok(raw) if !raw.trim().is_empty() => fingerprint_hash(raw.trim()),
        _ => {
            let prefs = crate::runtime_settings::get_user_preferences();
            let (hash, patch) = fallback_fingerprint_from_prefs(&prefs);
            if let Some(patch) = patch {
                if let Err(e) = crate::runtime_settings::save_user_preferences(&patch) {
                    crate::debug_log(&format!(
                        "alfred-api: failed to persist machine fallback id: {e}"
                    ));
                }
            }
            hash
        }
    }
}

/// Read the OpenAI JWT from local Codex session (never sent to the API).
fn get_local_jwt() -> Option<String> {
    if let Some(token) = env::var("ALFRED_API_TOKEN").ok().filter(|t| !t.is_empty()) {
        return Some(token);
    }
    let home = env::var("HOME").or_else(|_| env::var("USERPROFILE")).ok()?;
    let auth_path = format!("{home}/.codex/auth.json");
    if let Ok(content) = std::fs::read_to_string(&auth_path) {
        if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
            if let Some(tokens) = parsed.get("tokens") {
                for key in &["access_token", "id_token"] {
                    if let Some(token) = tokens.get(key).and_then(|v| v.as_str()) {
                        if !token.is_empty() {
                            return Some(token.to_string());
                        }
                    }
                }
            }
            for key in &["access_token", "token", "jwt", "id_token", "OPENAI_API_KEY"] {
                if let Some(token) = parsed.get(key).and_then(|v| v.as_str()) {
                    if !token.is_empty() {
                        return Some(token.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Outcome of a single HTTP attempt. Separates 429 from other failures so
/// the retry loop can read `Retry-After` and back off intelligently.
///
/// v0.3.3 (P0-9): introduced to handle alfred-api 429 responses (the P0-1
/// retry-with-backoff only covers Yahoo `/technicals` and treats every
/// transport error identically). Production incident `019e2da21154`
/// (2026-05-15) showed a 28-ticker run firing 34 `alfred_api_rate_limited`
/// errors at run start; the desktop client must respect the server's
/// `Retry-After` instead of surfacing the error to the user.
pub(crate) enum ApiGetOutcome {
    Ok(Value),
    /// HTTP 429 — caller should sleep then retry. `retry_after_secs` is
    /// the parsed `Retry-After` header value (when present + valid).
    RateLimited { retry_after_secs: Option<u64> },
    /// Any other error (4xx, 5xx, parse failure, transport failure) —
    /// not retried, surfaced immediately.
    Err(anyhow::Error),
}

/// Retry-on-429 policy. 1 initial attempt + up to 3 retries = 4 calls max.
///
/// Backoff schedule (used when the server does NOT send `Retry-After`):
/// 500ms → 1500ms → 4500ms (×3 multiplier). Total wait worst case ~6.5s
/// before surfacing the error. The server-side `apply_rate_limit` ticks
/// in 1-minute buckets, so a 60s `Retry-After` is the natural ceiling
/// when the header is honoured.
pub(crate) const ALFRED_API_MAX_RETRIES_ON_429: u32 = 3;
const ALFRED_API_429_BACKOFFS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_millis(4500),
];

fn default_sleep(d: Duration) {
    std::thread::sleep(d);
}

/// One-shot HTTP attempt — surfaces 429 with parsed `Retry-After` header
/// so the retry layer can choose its sleep budget. All other outcomes
/// collapse into the existing `map_api_error` classification.
///
/// v0.4.0 (P0-13): the 429 path now reads the response body so the
/// `error` field can distinguish:
/// - `rate_limited` → retryable transient → `RateLimited { retry_after_secs }`
/// - `free_tier_exhausted` → permanent until quota slides → `Err(...)`
///   surfaced immediately so the retry loop never wastes sleep budget
///   on a condition the user must address (upgrade) rather than wait
///   out a few seconds. See `docs/monetization-architecture.md` for the
///   server-side semantics.
///
/// Same body-aware split for 401: a `run_session_invalid` is distinct
/// from a hard auth failure (HMAC mismatch / missing client hash) — the
/// caller can surface a "session expired, click Run again" hint instead
/// of the generic "API unauthorized" message.
fn api_get_once(path: &str, timeout: u64) -> ApiGetOutcome {
    let base = match api_url() {
        Some(u) => u,
        None => return ApiGetOutcome::Err(anyhow!("alfred_api_not_configured")),
    };
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::get(&url), path).timeout(Duration::from_secs(timeout));
    match req.call() {
        Ok(resp) => match resp.into_json() {
            Ok(value) => ApiGetOutcome::Ok(value),
            Err(e) => ApiGetOutcome::Err(anyhow!("alfred_api_parse_failed:{e}")),
        },
        Err(ureq::Error::Status(429, resp)) => {
            // Read header BEFORE consuming the body — `into_string()` takes
            // self so the response is gone afterwards.
            let retry_after_secs = resp
                .header("Retry-After")
                .and_then(|v| v.trim().parse::<u64>().ok());
            let body = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if let Some(structured) = classify_429_body(&parsed, retry_after_secs) {
                // Quota exhaustion is NOT a transient error — surface it
                // immediately so `api_get_with_retry` doesn't burn its
                // budget waiting for a condition that won't resolve.
                ApiGetOutcome::Err(anyhow!("{structured}"))
            } else {
                ApiGetOutcome::RateLimited { retry_after_secs }
            }
        }
        Err(ureq::Error::Status(401, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            ApiGetOutcome::Err(anyhow!("{}", classify_401_body(&parsed)))
        }
        Err(e) => ApiGetOutcome::Err(map_api_error(e)),
    }
}

/// Classify a 429 response body. Returns:
/// - `Some(message)` when the body identifies a non-retryable quota
///   exhaustion (`error == "free_tier_exhausted"`). The message follows
///   the colon-delimited contract `alfred_free_tier_exhausted:{retry_after}:{limit}:{period}`
///   so the JS layer's `inferCodedErrorFromText` regex (extended for
///   structured codes in v0.4.0) can parse out the params for the
///   upgrade modal.
/// - `None` when the body says `rate_limited` or is empty/unparseable —
///   the caller falls through to the existing retry path. We don't
///   raise an error on missing fields because the server may be older
///   than the desktop client; missing fields just mean "no extra hint".
///
/// `retry_after_header` is the parsed `Retry-After` header (caller
/// already extracted it). When the body field `retry_after` is missing
/// or invalid we fall back to the header, then to `0`. Either way the
/// modal renders something — never a `NaN`/`null` placeholder.
pub(crate) fn classify_429_body(body: &Value, retry_after_header: Option<u64>) -> Option<String> {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code != "free_tier_exhausted" {
        return None;
    }
    let retry_after = body
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .or(retry_after_header)
        .unwrap_or(0);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let period = body
        .get("period")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("rolling_7d");
    Some(format!(
        "alfred_free_tier_exhausted:{retry_after}:{limit}:{period}"
    ))
}

/// Classify a 401 response body. Returns:
/// - `"alfred_run_session_invalid"` when the body identifies an expired
///   or absent run-session token (v0.4.0 P0-11). This is recoverable —
///   the desktop just needs to call `/run/start` again to get a fresh
///   session. The JS layer can show a soft hint rather than the global
///   "API unauthorized" modal.
/// - `"alfred_api_unauthorized"` otherwise (HMAC mismatch, missing
///   `X-Client-Hash`, or an older server that doesn't set the body
///   field). Same wire shape as v0.3.x so existing callers keep working.
pub(crate) fn classify_401_body(body: &Value) -> &'static str {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code == "run_session_invalid" {
        "alfred_run_session_invalid"
    } else {
        "alfred_api_unauthorized"
    }
}

/// Pure retry loop — fetcher + sleeper injected so the policy
/// (max retries, backoff schedule, `Retry-After` honouring) is fully
/// unit-testable without an HTTP runtime. See `tests::alfred_api_client_*`
/// in `tests.rs`.
///
/// Decision tree:
/// - `Ok(value)` → return immediately
/// - `RateLimited { retry_after_secs }` → if budget remains, sleep
///   `retry_after_secs` (clamped to a sane cap) or the next backoff
///   slot, then retry. If exhausted, surface `alfred_api_rate_limited`.
/// - `Err(e)` → surface immediately (don't retry on auth / server /
///   parse errors — those are not transient).
///
/// The `Retry-After` clamp caps at 30s so a hostile server (or a clock
/// skew) can't stall the desktop client indefinitely; beyond that we
/// fall back to the local backoff schedule.
pub(crate) fn api_get_with_retry<F, S>(fetcher: &F, sleeper: &S) -> Result<Value>
where
    F: Fn() -> ApiGetOutcome,
    S: Fn(Duration),
{
    const RETRY_AFTER_CLAMP: Duration = Duration::from_secs(30);
    let mut attempt: u32 = 0;
    loop {
        match fetcher() {
            ApiGetOutcome::Ok(value) => return Ok(value),
            ApiGetOutcome::Err(e) => return Err(e),
            ApiGetOutcome::RateLimited { retry_after_secs } => {
                if attempt >= ALFRED_API_MAX_RETRIES_ON_429 {
                    // Exhausted retry budget — surface the classified error
                    // so existing callers (`enrichment::classify_api_error`
                    // and friends) keep their wire shape.
                    return Err(anyhow!("alfred_api_rate_limited"));
                }
                let backoff = retry_after_secs
                    .map(|s| Duration::from_secs(s).min(RETRY_AFTER_CLAMP))
                    .unwrap_or(ALFRED_API_429_BACKOFFS[attempt as usize]);
                crate::debug_log(&format!(
                    "alfred-api: 429 received (attempt {}/{}), sleeping {}ms before retry (retry_after_header={:?})",
                    attempt + 1,
                    ALFRED_API_MAX_RETRIES_ON_429 + 1,
                    backoff.as_millis(),
                    retry_after_secs,
                ));
                sleeper(backoff);
                attempt += 1;
            }
        }
    }
}

/// Authenticated GET request to the API.
///
/// v0.3.3 (P0-9): now retries on HTTP 429 with `Retry-After` honoured.
/// All other errors (auth, 5xx, transport) surface on the first attempt
/// — only rate-limit errors are transient by design.
fn api_get(path: &str, timeout: u64) -> Result<Value> {
    let fetcher = || api_get_once(path, timeout);
    api_get_with_retry(&fetcher, &default_sleep)
}

/// Authenticated POST request to the API (fire-and-forget).
///
/// Stays fire-and-forget by contract (`-> ()`, never propagates to callers —
/// the `persist_*` writes are best-effort enrichment, not a reason to fail a
/// run). But the result is NO LONGER silently discarded: non-2xx statuses and
/// transport errors are logged via `crate::debug_log`. This visibility is
/// exactly what was missing for ~3 weeks — the codex MCP subprocess was
/// 401'ing on every `persist_*` write (no `X-Run-Session`) and the
/// `let _ = req.send_string(...)` swallowed it, so the desktop logged
/// "persisted" while the server never stored anything.
fn api_post(path: &str, body: &Value) {
    let base = match api_url() { Some(u) => u, None => return };
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(5));
    let outcome = req.send_string(&serde_json::to_string(body).unwrap_or_default());
    if let Some(msg) = classify_post_outcome(path, outcome) {
        crate::debug_log(&msg);
    }
}

/// Pure classifier for an `api_post` outcome → optional log line.
///
/// Returns:
/// - `None` on a 2xx success (the happy path stays quiet).
/// - `Some("alfred-api: POST {path} -> {status}[ body=...]")` on a non-2xx
///   HTTP status. For 4xx with a small body (≤ `MAX_LOGGED_BODY` bytes) the
///   body is appended so the failure cause (e.g. `run_session_invalid`) is
///   visible in the log. Larger / 5xx bodies are omitted to keep the log
///   readable — the status code is the actionable signal there.
/// - `Some("alfred-api: POST {path} failed: {err}")` on a transport error
///   (DNS, timeout, TLS, connection refused).
///
/// Split out from `api_post` so the classification is unit-testable without
/// an HTTP round-trip. `api_post` itself does only the I/O + the log call.
fn classify_post_outcome(
    path: &str,
    outcome: Result<ureq::Response, ureq::Error>,
) -> Option<String> {
    /// Cap on how many bytes of a 4xx body we inline into the log line.
    const MAX_LOGGED_BODY: usize = 512;
    match outcome {
        Ok(resp) => {
            let status = resp.status();
            if (200..300).contains(&status) {
                None
            } else {
                // 2xx-other (shouldn't happen for these endpoints) — still
                // surface it; the body is unlikely to be useful, so skip it.
                Some(format!("alfred-api: POST {path} -> {status}"))
            }
        }
        Err(ureq::Error::Status(status, resp)) => {
            // Read the body for client errors — it carries the structured
            // `{"error": "..."}` envelope that tells us WHY (the missing
            // `X-Run-Session` → `run_session_invalid` was invisible before).
            if (400..500).contains(&status) {
                let body = resp.into_string().unwrap_or_default();
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    Some(format!("alfred-api: POST {path} -> {status}"))
                } else {
                    let snippet: String = trimmed.chars().take(MAX_LOGGED_BODY).collect();
                    Some(format!("alfred-api: POST {path} -> {status} body={snippet}"))
                }
            } else {
                Some(format!("alfred-api: POST {path} -> {status}"))
            }
        }
        Err(ureq::Error::Transport(t)) => {
            Some(format!("alfred-api: POST {path} failed: {t}"))
        }
    }
}

/// Fetch market data from the remote API.
pub fn remote_fetch_market(ticker: &str, name: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/market?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch news from the remote API (SearXNG-backed).
///
/// When `canonical` is `Some(symbol)`, the server uses it for upstream news
/// queries (Yahoo finance, news aggregators) instead of the broker `ticker` —
/// this is how a French PEA `STMPA` gets routed to the same news bucket as a
/// CTO `STM.MI`. Server falls back to `ticker` when absent (backward compat).
pub fn remote_fetch_news(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/news?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
}

/// Fetch shared insights for a ticker from the API cache.
pub fn remote_fetch_insights(ticker: &str, isin: &str) -> Result<Value> {
    api_get(&format!("/api/insights?ticker={}&isin={}", urlenc(ticker), urlenc(isin)), TIMEOUT_SECS)
}

/// Fetch sector classification for a ticker from the API.
///
/// `canonical` lets the server route on the resolved Yahoo symbol so PEA
/// vs CTO duplicates of the same security share a single sector lookup. See
/// `remote_fetch_news` for the full rationale.
pub fn remote_fetch_sector(ticker: &str, name: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/sector?ticker={}&name={}&isin={}", urlenc(ticker), urlenc(name), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
}

/// Fetch technical snapshot (SMA/RSI/MACD/ATR/52w) computed server-side from
/// ~250 trading days of OHLC. Returns the parsed JSON envelope verbatim — the
/// caller is responsible for unwrapping `technical_snapshot`.
///
/// `isin` is forwarded so the server-side source_router can classify a bare
/// French/EU ticker (e.g. `EXA`, `LBIRD`) as European and derive the correct
/// Yahoo exchange suffix (`.PA`). Without it, the regex `^[A-Z]{1,5}$`
/// classifies these as US and the Yahoo OHLC fetch returns
/// `yahoo:yahoo_no_timestamps` — see `docs/technical-snapshot.md` and the
/// server `source_router::infer_exchange_suffix` helper.
///
/// Server endpoint may not be deployed yet — callers must handle errors
/// gracefully (typically map to `None`).
pub fn remote_fetch_technicals(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> Result<Value> {
    api_get(&build_technicals_path(ticker, isin, canonical), TIMEOUT_SECS)
}

/// Build the `/api/market/technicals` query string for `(ticker, isin, canonical)`.
/// Extracted so the URL contract is unit-testable without an HTTP round trip.
///
/// The `isin` argument is included only when present and non-empty after
/// trimming — empty strings degrade gracefully to a ticker-only query so
/// existing callers without ISIN keep their previous wire shape. `canonical`
/// follows the same trim-and-empty rule and is appended last.
fn build_technicals_path(ticker: &str, isin: Option<&str>, canonical: Option<&str>) -> String {
    let trimmed_isin = isin.map(str::trim).filter(|s| !s.is_empty());
    let base = match trimmed_isin {
        Some(code) => format!(
            "/api/market/technicals?ticker={}&isin={}",
            urlenc(ticker),
            urlenc(code),
        ),
        None => format!("/api/market/technicals?ticker={}", urlenc(ticker)),
    };
    append_canonical(base, canonical)
}

/// Fetch COT data for a ticker from the API.
///
/// `canonical` lets the server use the resolved Yahoo symbol for upstream
/// COT lookups — see `remote_fetch_news` for the full rationale.
pub fn remote_fetch_cot(ticker: &str, isin: &str, canonical: Option<&str>) -> Result<Value> {
    let path = format!("/api/cot?ticker={}&isin={}", urlenc(ticker), urlenc(isin));
    api_get(&append_canonical(path, canonical), TIMEOUT_SECS)
}

/// Fetch the global macro briefing (US 10Y, VIX, EUR/USD, Brent) from
/// `GET /api/macro` (P1-60).
///
/// Portfolio-agnostic, no parameters. The server caches the briefing
/// globally for 30 min, so the desktop never needs its own caching layer.
/// Server endpoint may not be deployed on older API instances (returns
/// 404 in that case) — callers must degrade gracefully (treat as "no
/// macro briefing available", same pattern as `remote_fetch_sector`).
pub fn remote_fetch_macro_briefing() -> Result<Value> {
    api_get("/api/macro", TIMEOUT_SECS)
}

/// Append `&canonical=<symbol>` to a query path when a non-empty resolved
/// Yahoo symbol is present. Centralised so the trim/empty-rejection rule is
/// applied identically across every endpoint that accepts canonical routing
/// — keeps the v0.3 parity contract honest: all 3 LLM modes see the same
/// canonical enrichment data (`product_llm_mode_parity_2026_04`).
fn append_canonical(path: String, canonical: Option<&str>) -> String {
    match canonical.map(str::trim).filter(|s| !s.is_empty()) {
        Some(symbol) => format!("{path}&canonical={}", urlenc(symbol)),
        None => path,
    }
}

/// Resolve a canonical Yahoo symbol for the given ISIN via
/// `GET /api/resolve?isin=X` (deployed in server-side v0.3 — feat/v0.3-server-bundle).
///
/// Returns `Ok(Some(symbol))` when the server returns a non-null symbol
/// (`source = "yahoo_search"`), `Ok(None)` when the resolver returned no match
/// (`source = "none"` or `symbol = null`), and `Err` on transport/auth errors
/// so the caller can decide whether to log or swallow.
///
/// Server-side cache: 180 days positive, 15s negative — desktop callers do not
/// need their own caching. Endpoint may not be deployed on older servers, so
/// callers must degrade gracefully (treat error as "no resolution available").
pub fn remote_fetch_resolve(isin: &str) -> Result<Option<String>> {
    let resp = api_get(&build_resolve_path(isin), TIMEOUT_SECS)?;
    // Source = "none" means the resolver searched but found nothing — treat
    // identically to a null symbol so callers don't store the sentinel.
    let source = resp.get("source").and_then(|v| v.as_str()).unwrap_or("");
    if source == "none" {
        return Ok(None);
    }
    let symbol = resp
        .get("symbol")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(symbol)
}

/// Build the `/api/resolve` query string for a given ISIN. Extracted so the
/// URL contract is unit-testable without a live HTTP round trip and so the
/// trim/empty-rejection rule is colocated with the other path builders.
fn build_resolve_path(isin: &str) -> String {
    let code = isin.trim();
    format!("/api/resolve?isin={}", urlenc(code))
}

// ── Admin observability (v0.4.0 P0-14) ───────────────────────────────

/// Typed mirror of the `/admin/usage` response envelope.
///
/// Pinning the shape via serde — instead of leaving it as `serde_json::Value`
/// — gives us a parse-time contract test: if the server-side
/// `apps/alfred-api/src/admin.rs::admin_usage_handler` renames or drops a
/// field, deserialization fails loudly here rather than silently rendering
/// an empty table client-side. All fields default to safe zeros / empty
/// collections so an older server that doesn't yet emit a particular field
/// degrades gracefully.
///
/// The server hash is already truncated to 8 chars before wire (see
/// `admin.rs::anonymize_user_hash`); this struct simply forwards the
/// anonymised value — no further truncation needed.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminUsage {
    /// Top-N users by `runs_7d`, anonymised hashes only.
    #[serde(default)]
    pub top_users: Vec<AdminTopUser>,
    /// Most-recently-seen tickers (ISIN + epoch timestamp).
    #[serde(default)]
    pub top_tickers: Vec<AdminTopTicker>,
    /// Sum of `runs_7d` across all users.
    #[serde(default)]
    pub runs_7d: u64,
    /// Sum of `runs_24h` across all users.
    #[serde(default)]
    pub runs_24h: u64,
    /// Today's `metrics:429:<date>` counter.
    #[serde(default)]
    pub errors_429_today: u64,
    /// v1 placeholder — per-endpoint counters deferred. Forwarded verbatim
    /// so the UI can surface the server's "tracked: false" hint.
    #[serde(default)]
    pub by_endpoint: Value,
    /// Server timestamp at response emission (epoch seconds).
    #[serde(default)]
    pub generated_at: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminTopUser {
    pub user_hash: String,
    pub runs_7d: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminTopTicker {
    pub isin: String,
    pub last_seen: u64,
}

/// Typed mirror of the `/admin/vps-stats` response envelope. Same contract
/// rationale as [`AdminUsage`]: parse-time validation against the server's
/// `admin_vps_stats_handler` shape.
///
/// All numeric fields default to 0 so an older server that doesn't yet
/// emit a particular sub-field degrades gracefully.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AdminVpsStats {
    #[serde(default)]
    pub redis: AdminRedisStats,
    #[serde(default)]
    pub process: AdminProcessStats,
    #[serde(default)]
    pub generated_at: u64,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AdminRedisStats {
    #[serde(default)]
    pub memory_used: Option<u64>,
    #[serde(default)]
    pub memory_peak: Option<u64>,
    #[serde(default)]
    pub connected_clients: Option<u64>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct AdminProcessStats {
    #[serde(default)]
    pub rss_bytes: u64,
    #[serde(default)]
    pub uptime_secs: u64,
}

/// P0-16 (2026-05-23) — server-driven admin tab visibility probe.
/// Calls `GET /admin/check`, returns `Ok(true)` on 204 (admin),
/// `Ok(false)` on 403 (not admin). Other failures (5xx, network) are
/// surfaced as `Err` so the caller can log the diagnostic instead of
/// silently flipping the tab off.
///
/// Replaces the desktop's previous `admin_config::is_whitelisted_admin`
/// path — the allowlist now lives only in the server's
/// `ALFRED_ADMIN_HASHES` env var, so adding an admin no longer requires
/// a desktop rebuild.
pub fn admin_check() -> Result<bool> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}/admin/check");
    let req = apply_auth(ureq::get(&url), "/admin/check")
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    match req.call() {
        // 204 NoContent is the exact contract from the server handler.
        Ok(resp) if resp.status() == 204 => Ok(true),
        Ok(resp) => {
            // 2xx other than 204 isn't expected — surface as error so
            // a future server-side regression doesn't silently degrade
            // to "everyone is admin".
            Err(anyhow!(
                "alfred_api_admin_check_unexpected_status:{}",
                resp.status()
            ))
        }
        Err(ureq::Error::Status(403, _)) => Ok(false),
        Err(e) => Err(map_api_error(e)),
    }
}

/// Fetch `/admin/usage`. Returns the raw envelope :
/// `{ok, top_users, top_tickers, runs_7d, runs_24h, errors_429_today,
///   by_endpoint, generated_at}`.
///
/// Requires the user's client_hash to be on the server-side
/// `ALFRED_ADMIN_HASHES` allowlist; otherwise the server responds 403 and
/// this function surfaces `alfred_api_http_error:403`. The desktop UI
/// hides the Admin tab unless the cold-start `admin_check` probe
/// returned 204 (P0-16, 2026-05-23) so the 403 path is normally
/// unreachable for non-admins.
///
/// Admin endpoints are NOT under the run-session middleware (verified in
/// `apps/alfred-api/src/lib.rs::build_router` — admin_routes are mounted
/// at the root, not nested under `/api/`). `apply_auth` exempts
/// `/admin/*` paths from `X-Run-Session` injection so the header is
/// guaranteed absent here — see `is_admin_path` and the contract test
/// `admin_endpoint_calls_do_not_inject_run_session_header`.
///
/// The response is parsed through [`AdminUsage`] for contract validation
/// (a server-side field rename will surface as a parse error rather than
/// a silently-empty UI), then re-serialised back to `Value` for forwarding
/// through Tauri to the JS layer. JS reads the same key shape as before.
pub fn get_admin_usage() -> Result<Value> {
    let raw = api_get("/admin/usage", TIMEOUT_SECS)?;
    let parsed: AdminUsage = serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:admin_usage:{e}"))?;
    serde_json::to_value(parsed)
        .map_err(|e| anyhow!("alfred_api_serialize_failed:admin_usage:{e}"))
}

/// Fetch `/admin/vps-stats`. Returns the raw envelope :
/// `{ok, redis: {memory_used, memory_peak, connected_clients},
///   process: {rss_bytes, uptime_secs}, generated_at}`.
///
/// Same auth contract as [`get_admin_usage`]. Parsed through
/// [`AdminVpsStats`] for contract validation then re-serialised back to
/// `Value` for Tauri forwarding.
pub fn get_admin_vps_stats() -> Result<Value> {
    let raw = api_get("/admin/vps-stats", TIMEOUT_SECS)?;
    let parsed: AdminVpsStats = serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:admin_vps_stats:{e}"))?;
    serde_json::to_value(parsed)
        .map_err(|e| anyhow!("alfred_api_serialize_failed:admin_vps_stats:{e}"))
}

/// Compute the current user's client hash. Exposes the previously-private
/// `get_client_hash` so the admin tab visibility check can run on the
/// same value the server sees in `X-Client-Hash`. Returns `None` when no
/// OpenAI JWT is available locally (e.g. user not signed in to Codex).
///
/// Kept under a dedicated public function rather than re-exporting the
/// internal helper, so the contract ("the hash sent on every request") is
/// stable and the doc-comment can pin the format.
pub fn current_user_hash() -> Option<String> {
    get_client_hash()
}

// ── License client (v0.4.0 P0-15) ─────────────────────────────────────
//
// Proxies the Lemon Squeezy License API via the alfred-api server (the
// desktop never talks to LS directly — see `docs/monetization-architecture.md`
// § "Lemon Squeezy authoritative + Redis cache"). All requests follow the
// HMAC auth contract (`X-Client-Hash` + `X-Timestamp` + `X-Signature`)
// but are exempt from `X-Run-Session` injection — license endpoints are
// account-level, not per-run (see `is_session_exempt_path`).
//
// Server response shapes pinned in `apps/alfred-api/src/license.rs`. Each
// struct here uses `#[serde(default)]` on every field so an older server
// (one wire-compatible release behind) degrades gracefully — missing
// fields default to None / empty rather than parse-failing.

/// Typed mirror of the `/license/activate` success response envelope.
///
/// Wire shape (from `license.rs::activate_handler` Activated arm):
/// ```json
/// { "ok": true, "tier": "paid", "instance_id": "...", "expires_at": "ISO",
///   "validated_at": 1700000000 }
/// ```
///
/// The error path (LS rejected, key revoked, etc.) is HTTP 400 with a
/// `{error: "license_invalid", reason: "..."}` body that surfaces through
/// the standard `map_api_error` chain — not this struct.
///
/// The 503 `license_provider_not_configured` path (LEMON_SQUEEZY_API_KEY
/// empty) is mapped to the structured error code
/// `alfred_license_provider_not_configured` in `map_license_error`
/// below so the desktop UI can render a clean banner instead of a
/// vague HTTP error.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LicenseActivationResponse {
    /// Always `true` on success; defaults to `false` if the server omits.
    #[serde(default)]
    pub ok: bool,
    /// `"paid"` on success — pinned for an extra sanity check at the call
    /// site (we can assert this in tests + when persisting to user-prefs).
    #[serde(default)]
    pub tier: String,
    /// LS-assigned instance identifier. Used by P2-8 multi-device
    /// deactivation. May be `None` on older server versions.
    #[serde(default)]
    pub instance_id: Option<String>,
    /// ISO-8601 expiry timestamp (LS-native format). `None` when the
    /// subscription has no fixed end date.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Epoch seconds when the activation was validated (server clock).
    /// Used as the source for the desktop's `license_validated_at`
    /// user-preference.
    #[serde(default)]
    pub validated_at: u64,
}

/// Typed mirror of the `/license/status` response envelope.
///
/// Wire shape (from `license.rs::status_handler`):
/// ```json
/// { "ok": true, "tier": "free"|"paid", "expires_at": <epoch>|null,
///   "validated_at": <epoch>|null, "pending_notice": "refunded"|"expired"|null }
/// ```
///
/// Note `expires_at` is an epoch second here (different from the
/// activation response's ISO string) — the status endpoint reads back
/// from the Redis `TierRecord` which stores epoch seconds.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LicenseStatusResponse {
    #[serde(default)]
    pub ok: bool,
    /// `"free"` or `"paid"`. Defaults to empty string if the server omits.
    #[serde(default)]
    pub tier: String,
    /// Epoch seconds when the subscription expires. `None` when the user
    /// is on the free tier or when the paid tier has no fixed end.
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Epoch seconds of the last successful activation/revalidation.
    #[serde(default)]
    pub validated_at: Option<u64>,
    /// Sticky banner set by webhook events: `"refunded"`, `"expired"`,
    /// or `None`. The desktop reads this on cold start to surface a
    /// one-shot notice to the user (P1-8 — separate item).
    #[serde(default)]
    pub pending_notice: Option<String>,
}

/// Activate a Lemon Squeezy license key against the server.
///
/// `POST /license/activate` body `{license_key, instance_name}`. The
/// server proxies to LS, sets `tier:<hash> = paid` on success, and
/// returns the typed envelope above.
///
/// Error semantics:
/// - HTTP 400 `license_invalid` → bubbled up as
///   `alfred_api_http_error:400` through `map_api_error` (caller can
///   parse the body via `inferCodedErrorFromText` JS-side).
/// - HTTP 503 `license_provider_not_configured` → mapped to the clean
///   structured code `alfred_license_provider_not_configured` so the
///   desktop renders a "service temporarily unavailable" banner
///   instead of a confusing "auth failure" message.
/// - Any other status / transport error → standard `map_api_error`.
///
/// `instance_name` is forwarded to LS for display in the user's LS
/// dashboard ("Activated from Alfred on Pierre's MacBook Pro"). Pass
/// the OS hostname or a fixed `"alfred-desktop"` when unknown — the
/// server defaults to the latter when this is empty.
pub fn activate_license(
    license_key: &str,
    instance_name: &str,
) -> Result<LicenseActivationResponse> {
    let body = serde_json::json!({
        "license_key": license_key,
        "instance_name": instance_name,
    });
    let raw = api_post_json("/license/activate", &body)?;
    serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:license_activate:{e}"))
}

/// Validate a Lemon Squeezy license key against the server.
///
/// `POST /license/validate` body `{license_key}`. The server proxies to
/// the LS validate endpoint and returns the raw LS response — full
/// integration deferred to P1-6 (cold-start revalidation orchestration).
/// Today wired so the desktop API client has a stable surface to call
/// when P1-6 lands; this method just returns the parsed JSON envelope.
///
/// Same error semantics as [`activate_license`].
pub fn validate_license(license_key: &str) -> Result<Value> {
    let body = serde_json::json!({
        "license_key": license_key,
    });
    api_post_json("/license/validate", &body)
}

/// Fetch the cached license status for the current user.
///
/// `GET /license/status`. Reads the Redis `tier:<hash>` record (no LS
/// round-trip). Returns the typed envelope above.
///
/// The desktop reads this on cold start (P1-6 separate item) and after
/// every successful activation to refresh the user-pref cache. A
/// non-paid `tier` here means the cache is stale or the user has never
/// upgraded — the `pending_notice` field surfaces refund/expired
/// banners set by webhooks.
pub fn get_license_status() -> Result<LicenseStatusResponse> {
    let raw = api_get("/license/status", TIMEOUT_SECS)?;
    serde_json::from_value(raw)
        .map_err(|e| anyhow!("alfred_api_parse_failed:license_status:{e}"))
}

/// Fetch the authoritative rolling-7d quota for the current user.
///
/// `GET /quota/status` (v0.4.7 P3-31). READ-ONLY — the server reuses
/// `quota::get_quota_info`, whose only Redis write is the expired-entry
/// prune; it NEVER consumes a run. This is the primary source for the
/// home strip count, replacing the local `run-index.json` count that
/// drifts ±1 vs the server ZSET (`docs/monetization-architecture.md`
/// § "Quota exposure to UI").
///
/// Returns the raw server envelope `{count, limit, period, reset_at}`
/// unparsed: `limit` is a NUMBER for free tier but the STRING
/// `"unlimited"` for paid tier, so a typed `u32` mirror would fail on
/// paid users. The JS layer (`quota-local-counter.js`) does the
/// numeric coercion and treats a non-numeric `limit` as the desktop
/// default of 3 — paid tier is rendered from `licenseStatus().tier`,
/// not from this `limit`.
///
/// `/quota/status` is auth-gated (HMAC + `X-Client-Hash`) but NOT under
/// `require_run_session` — `is_session_exempt_path` skips injecting the
/// `X-Run-Session` header so a cold-start home render works before any
/// run session exists.
pub fn get_quota_status() -> Result<Value> {
    api_get("/quota/status", TIMEOUT_SECS)
}

// ── MON-A: device registration ──────────────────────────────────────

/// Ensure the device has a server-issued identity. No-op when one already
/// exists. On first run, calls `POST /device/register` (authenticated via
/// the LEGACY HMAC bootstrap) and persists `device_id` + `device_secret`.
///
/// Fail-soft: if the API is down or doesn't yet support `/device/register`
/// (older deployment — 404), the desktop keeps working on legacy
/// `X-Client-Hash` auth. Registration is retried on the next launch. Called
/// once at startup (see `command_handlers::register_device_local`).
pub fn ensure_device_registered() -> Result<bool> {
    if device_identity().is_some() {
        return Ok(false); // already registered
    }
    // MON-E (P0-83): send the salted machine fingerprint so the server can
    // bind this device to a stable slot (the raw machine id never leaves the
    // device). An older server that predates MON-E ignores the extra field.
    let resp = api_post_json(
        "/device/register",
        &serde_json::json!({ "machine_hash": machine_fingerprint() }),
    )?;
    let device_id = resp.get("device_id").and_then(|v| v.as_str()).unwrap_or_default();
    let device_secret = resp.get("device_secret").and_then(|v| v.as_str()).unwrap_or_default();
    if device_id.is_empty() || device_secret.is_empty() {
        return Err(anyhow!("alfred_device_register_incomplete"));
    }
    set_device_identity(device_id, device_secret);
    crate::debug_log("alfred-api: device identity registered");
    Ok(true)
}

// ── MON-B/C: comp-code redemption ───────────────────────────────────

/// Redeem an activation (comp) code via `POST /redeem`. On success the
/// server binds the code to this device and returns `{tier, expires_at}`.
///
/// Maps the structured error bodies to stable codes the UI routes on:
/// - 400 `code_invalid`       → `alfred_redeem_invalid`
/// - 409 `code_already_used`  → `alfred_redeem_already_used`
/// - 410 `code_expired`       → `alfred_redeem_expired`       (MON-E)
/// - 409 `code_device_limit`  → `alfred_redeem_device_limit`  (MON-E)
/// - other 4xx/5xx / transport → existing `alfred_api_*` codes
///
/// MON-E (P0-83): the body carries `machine_hash` so the server binds the
/// code to this stable machine slot (up to 2 machines / code, 90-day shared
/// window). The raw machine id never leaves the device — only the salted
/// hash. An older server ignores the extra field (one-slot-per-device).
pub fn redeem_code(code: &str) -> Result<Value> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("alfred_redeem_invalid"));
    }
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}/redeem");
    let body = serde_json::json!({ "code": trimmed, "machine_hash": machine_fingerprint() });
    let req = apply_auth(ureq::post(&url), "/redeem")
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    match req.send_string(&serde_json::to_string(&body).unwrap_or_default()) {
        Ok(resp) => resp
            .into_json::<Value>()
            .map_err(|e| anyhow!("alfred_api_parse_failed:{e}")),
        Err(ureq::Error::Status(code_status, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
            Err(anyhow!("{}", classify_redeem_error(code_status, &parsed)))
        }
        Err(e) => Err(map_api_error(e)),
    }
}

/// Pure helper: classify a non-2xx `/redeem` response into a stable code.
/// Extracted so the mapping is unit-testable without an HTTP round trip.
///
/// We route on the body `error` string FIRST (status-independent) because the
/// server half owns the exact HTTP status per code and the desktop must not
/// couple to it — a `code_device_limit` is the same user-facing state whether
/// the server returns 409 or 403. The `status` only drives the generic
/// `alfred_api_http_error:<status>` fallback when the body carries no known
/// `error` code.
///
/// MON-E (P0-83) adds `code_expired` + `code_device_limit`; the existing
/// `code_invalid` / `code_already_used` mappings are preserved verbatim.
pub(crate) fn classify_redeem_error(status: u16, body: &Value) -> String {
    let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    match err {
        "code_invalid" => "alfred_redeem_invalid".to_string(),
        "code_already_used" => "alfred_redeem_already_used".to_string(),
        "code_expired" => "alfred_redeem_expired".to_string(),
        "code_device_limit" => "alfred_redeem_device_limit".to_string(),
        _ => format!("alfred_api_http_error:{status}"),
    }
}

/// Authenticated POST request with a JSON body that parses and returns
/// the response envelope. Used by `/license/*` calls where the server
/// returns a structured response we want to consume (unlike `api_post`
/// which is fire-and-forget).
///
/// Centralises the body-shape handling for the license codes:
/// - HTTP 503 with body `error=license_provider_not_configured` →
///   structured code `alfred_license_provider_not_configured`.
/// - Other 4xx/5xx → `alfred_api_http_error:<code>` (existing contract).
/// - Transport failures → `alfred_api_request_failed:<e>` (existing).
fn api_post_json(path: &str, body: &Value) -> Result<Value> {
    let base = api_url().ok_or_else(|| anyhow!("alfred_api_not_configured"))?;
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(TIMEOUT_SECS));
    let result = req.send_string(&serde_json::to_string(body).unwrap_or_default());
    match result {
        Ok(resp) => resp
            .into_json::<Value>()
            .map_err(|e| anyhow!("alfred_api_parse_failed:{e}")),
        Err(ureq::Error::Status(503, resp)) => {
            let body_text = resp.into_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
            Err(anyhow!("{}", classify_503_body(&parsed)))
        }
        Err(e) => Err(map_api_error(e)),
    }
}

/// Classify a 503 response body. Returns:
/// - `"alfred_license_provider_not_configured"` when the body identifies
///   an unconfigured LS provider (server-side `LEMON_SQUEEZY_API_KEY`
///   env var empty). The desktop renders a clean "temporarily unavailable"
///   banner rather than a vague HTTP error — see `monetization-architecture.md`.
/// - `"alfred_api_http_error:503"` for any other 503 (server overloaded,
///   upstream failure with no body, etc.) so existing transport handling
///   continues to work.
pub(crate) fn classify_503_body(body: &Value) -> String {
    let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if code == "license_provider_not_configured" {
        "alfred_license_provider_not_configured".to_string()
    } else {
        "alfred_api_http_error:503".to_string()
    }
}

/// Persist shared insights (generic analysis) back to the API for other users.
/// Optionally includes sector classification and sector analysis memo.
pub fn persist_shared_insights(ticker: &str, isin: &str, insights: &Value, sector: Option<&str>, sector_analysis: Option<&str>) {
    if !insights.is_object() || insights.as_object().map(|o| o.is_empty()).unwrap_or(true) { return; }
    let mut body = serde_json::json!({ "ticker": ticker, "isin": isin, "insights": insights });
    if let Some(s) = sector {
        body["sector"] = serde_json::json!(s);
    }
    if let Some(sa) = sector_analysis {
        body["sector_analysis"] = serde_json::json!(sa);
    }
    api_post("/api/insights", &body);
    crate::debug_log(&format!("alfred-api: persisted shared insights for {ticker}"));
}

/// Persist LLM-extracted fundamental values back to the API cache.
pub fn persist_extracted_fundamentals(ticker: &str, isin: &str, extracted: &Value) {
    if !extracted.is_object() || extracted.as_object().map(|o| o.is_empty()).unwrap_or(true) { return; }
    api_post("/api/market/extracted", &serde_json::json!({ "ticker": ticker, "isin": isin, "extracted_fundamentals": extracted }));
    crate::debug_log(&format!("alfred-api: persisted extracted fundamentals for {ticker}"));
}

/// Persist a deep news summary for a specific article URL to the API cache.
pub fn persist_deep_news_summary(
    ticker: &str, isin: &str,
    article_url: &str, title: &str, summary: &str,
    quality_score: u64, relevance: &str, staleness: &str,
) {
    if summary.is_empty() || article_url.is_empty() { return; }
    api_post("/api/deep-news", &serde_json::json!({
        "ticker": ticker, "isin": isin, "url": article_url, "title": title,
        "summary": summary, "quality_score": quality_score, "relevance": relevance, "staleness": staleness,
    }));
    crate::debug_log(&format!("alfred-api: persisted deep news for {ticker}"));
}

/// Ban a news URL as noise for a ticker.
pub fn ban_deep_news_url(ticker: &str, isin: &str, article_url: &str, reason: &str) {
    if article_url.is_empty() { return; }
    api_post("/api/deep-news/ban", &serde_json::json!({ "ticker": ticker, "isin": isin, "url": article_url, "reason": reason }));
}

// ── /run/start client (v0.4.0 P0-11) ─────────────────────────────────

/// Result of calling `POST /run/start`. Two structured outcomes for the
/// caller to render differently:
/// - `Ok` → caller stores `session_id`, proceeds with the run.
/// - `QuotaExhausted` → caller surfaces the upgrade modal (P0-13 in a
///   separate worktree); the run NEVER starts.
/// - `Err` → unrelated transport/auth failure; caller fails the run with
///   the usual error path.
///
/// The 3-way shape matters because "quota exhausted" is a perfectly
/// understandable user state (you've done 3 analyses this week), not a
/// runtime error — collapsing it into `Err` would lose the structured
/// fields the UI needs to render the upgrade CTA.
#[derive(Debug)]
pub enum RunStartOutcome {
    /// `/run/start` succeeded — session is valid for `expires_at` seconds.
    Ok(RunStartSuccess),
    /// 429 `free_tier_exhausted` — surface upgrade CTA, do NOT start the run.
    QuotaExhausted(QuotaExhaustedInfo),
}

#[derive(Debug, Clone)]
pub struct RunStartSuccess {
    pub run_session_id: String,
    pub expires_at: u64,
    pub runs_this_week: u32,
    /// `None` for paid tier (the server returns the string "unlimited").
    /// Allowed-dead: consumed by P0-13 modal (separate worktree W-B)
    /// when it renders the "X of Y runs used" line. We log it from
    /// `acquire_run_session_or_bail` for diagnostics but don't surface
    /// it via the bridge yet — keeping the field on the struct pins the
    /// wire contract so W-B doesn't have to re-parse the JSON.
    #[allow(dead_code)]
    pub limit: Option<u32>,
    /// `"free"` or `"paid"`. The desktop renders different UI based on this.
    pub tier: String,
}

#[derive(Debug, Clone)]
pub struct QuotaExhaustedInfo {
    pub retry_after: u64,
    /// Surfaced today via the structured `alfred_free_tier_exhausted:…`
    /// error code (see `analysis_ops::acquire_run_session_or_bail`) — the
    /// JS upgrade modal (P0-13, separate worktree W-B) reads it back from
    /// the message body for display. Allowed-dead until W-B lands.
    #[allow(dead_code)]
    pub runs_this_week: u32,
    pub limit: u32,
    /// Always `"rolling_7d"` in v0.4.0; pinned so the UI can format the
    /// reset hint correctly ("rolling 7 days").
    pub period: String,
}

/// Issue a run-session via `POST /run/start`. Decrements the server-side
/// rolling-7d quota. Caller is responsible for storing the returned
/// `run_session_id` via `set_active_run_session` so downstream `api_get`
/// calls inject the header automatically.
///
/// Errors:
/// - `Ok(QuotaExhausted)` for HTTP 429 `free_tier_exhausted` (graceful UX path).
/// - `Err(_)` for transport / auth / parse failures (treated as run-start failure).
///
/// Why not retry on 429? Unlike rate-limit 429s, a free-tier exhaustion
/// won't resolve in seconds — the user has to wait for the rolling window
/// or upgrade. Retrying would just consume the user's quota again the
/// moment it frees up (a hostile pattern). Surface the structured info
/// to the UI instead.
pub fn start_run_session() -> Result<RunStartOutcome> {
    let base = match api_url() {
        Some(u) => u,
        None => return Err(anyhow!("alfred_api_not_configured")),
    };
    let path = "/run/start";
    let url = format!("{base}{path}");
    let req = apply_auth(ureq::post(&url), path)
        .set("Content-Type", "application/json")
        .timeout(Duration::from_secs(RUN_START_TIMEOUT_SECS));

    let response = req.send_string("{}");
    parse_run_start_response(response)
}

/// Pure-ish helper: classify the `/run/start` response into a
/// `RunStartOutcome`. Split from `start_run_session` so the classification
/// logic is unit-testable (mocked `ureq::Response` shapes).
fn parse_run_start_response(
    result: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<RunStartOutcome> {
    match result {
        Ok(resp) => {
            let body: Value = resp
                .into_json()
                .map_err(|e| anyhow!("alfred_api_parse_failed:{e}"))?;
            classify_run_start_ok(body)
        }
        Err(ureq::Error::Status(429, resp)) => {
            let body: Value = resp.into_json().unwrap_or(Value::Null);
            classify_run_start_429(body)
        }
        Err(e) => Err(map_api_error(e)),
    }
}

fn classify_run_start_ok(body: Value) -> Result<RunStartOutcome> {
    let run_session_id = body
        .get("run_session_id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("alfred_api_parse_failed:missing run_session_id"))?;
    let expires_at = body
        .get("expires_at")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("alfred_api_parse_failed:missing expires_at"))?;
    let runs_this_week = body
        .get("runs_this_week")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    // `limit` is integer for Free, the string "unlimited" for Paid (the
    // server caps at u32::MAX which we'd never want to display as-is).
    let limit = body.get("limit").and_then(|v| v.as_u64()).map(|n| n as u32);
    let tier = body
        .get("tier")
        .and_then(|v| v.as_str())
        .unwrap_or("free")
        .to_string();
    Ok(RunStartOutcome::Ok(RunStartSuccess {
        run_session_id,
        expires_at,
        runs_this_week,
        limit,
        tier,
    }))
}

fn classify_run_start_429(body: Value) -> Result<RunStartOutcome> {
    // The server always sends `error: "free_tier_exhausted"` for 429 on
    // /run/start. If it ever returns the generic `rate_limited` here
    // (e.g. global per-IP throttle), treat that as a transport-level
    // 429 and surface to the caller via Err so the existing rate-limit
    // retry path handles it.
    let error_code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if error_code != "free_tier_exhausted" {
        return Err(anyhow!("alfred_api_rate_limited"));
    }
    let retry_after = body
        .get("retry_after")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let runs_this_week = body
        .get("runs_this_week")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(0);
    let period = body
        .get("period")
        .and_then(|v| v.as_str())
        .unwrap_or("rolling_7d")
        .to_string();
    Ok(RunStartOutcome::QuotaExhausted(QuotaExhaustedInfo {
        retry_after,
        runs_this_week,
        limit,
        period,
    }))
}

fn urlenc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || "-._~".contains(c) => c.to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// Classify residual ureq errors after `api_get_once` has already peeled
/// off 401 and 429 (which need body-aware handling for v0.4.0 quota /
/// session codes). What lands here is the non-status transport tail
/// (timeouts, DNS, TLS) plus any non-401/429 HTTP status (4xx other than
/// auth, 5xx). Keep the colon-delimited shape (`alfred_api_http_error:<code>`)
/// because downstream classifiers in `enrichment::classify_api_error`
/// regex on that prefix.
fn map_api_error(e: ureq::Error) -> anyhow::Error {
    match &e {
        ureq::Error::Status(code, _) => anyhow!("alfred_api_http_error:{code}"),
        _ => anyhow!("alfred_api_request_failed:{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── MON-A: device identity parse ────────────────────────────────
    #[test]
    fn parse_device_identity_returns_pair_when_both_present() {
        let prefs = serde_json::json!({
            "device_id": "abc123",
            "device_secret": "deadbeef",
            "tier": "free"
        });
        assert_eq!(
            parse_device_identity(&prefs),
            Some(("abc123".to_string(), "deadbeef".to_string()))
        );
    }

    #[test]
    fn parse_device_identity_none_when_either_missing_or_empty() {
        assert_eq!(parse_device_identity(&serde_json::json!({})), None);
        assert_eq!(
            parse_device_identity(&serde_json::json!({"device_id": "abc"})),
            None
        );
        assert_eq!(
            parse_device_identity(&serde_json::json!({"device_id": "", "device_secret": "x"})),
            None
        );
        assert_eq!(
            parse_device_identity(&serde_json::json!({"device_id": "  ", "device_secret": "x"})),
            None
        );
    }

    #[test]
    fn parse_device_identity_trims_whitespace() {
        let prefs = serde_json::json!({"device_id": " id ", "device_secret": " sec "});
        assert_eq!(
            parse_device_identity(&prefs),
            Some(("id".to_string(), "sec".to_string()))
        );
    }

    // ── MON-C / MON-E: redeem error classification ──────────────────
    #[test]
    fn classify_redeem_error_maps_invalid_and_already_used() {
        let invalid = serde_json::json!({"error": "code_invalid"});
        assert_eq!(classify_redeem_error(400, &invalid), "alfred_redeem_invalid");

        let used = serde_json::json!({"error": "code_already_used"});
        assert_eq!(
            classify_redeem_error(409, &used),
            "alfred_redeem_already_used"
        );
    }

    #[test]
    fn classify_redeem_error_maps_mon_e_expired_and_device_limit() {
        // MON-E (P0-83): two new server codes. We route on the body `error`
        // string regardless of HTTP status (the server half owns the status).
        let expired = serde_json::json!({"error": "code_expired"});
        assert_eq!(classify_redeem_error(410, &expired), "alfred_redeem_expired");
        // Status-independence: same code via 400 still maps to expired.
        assert_eq!(classify_redeem_error(400, &expired), "alfred_redeem_expired");

        let limit = serde_json::json!({"error": "code_device_limit"});
        assert_eq!(
            classify_redeem_error(409, &limit),
            "alfred_redeem_device_limit"
        );
        assert_eq!(
            classify_redeem_error(403, &limit),
            "alfred_redeem_device_limit"
        );
    }

    #[test]
    fn classify_redeem_error_falls_back_to_http_code() {
        let other = serde_json::json!({"error": "something_else"});
        assert_eq!(classify_redeem_error(500, &other), "alfred_api_http_error:500");
        assert_eq!(
            classify_redeem_error(400, &serde_json::Value::Null),
            "alfred_api_http_error:400"
        );
    }

    // ── MON-E: machine fingerprint ──────────────────────────────────
    #[test]
    fn fingerprint_hash_is_deterministic_and_salted() {
        // Same raw id → same hash (determinism is the whole point: the same
        // machine must re-use its server slot across re-installs).
        let a = fingerprint_hash("raw-machine-id-123");
        let b = fingerprint_hash("raw-machine-id-123");
        assert_eq!(a, b, "hash must be deterministic for a given raw id");
        // 16 hex chars (64-bit FNV-1a).
        assert_eq!(a.len(), 16, "hash is a 16-char hex string, got: {a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // The raw id never appears in the hash (no-leak contract).
        assert!(!a.contains("raw-machine-id-123"));
        // Different raw ids → different hashes.
        assert_ne!(fingerprint_hash("id-a"), fingerprint_hash("id-b"));
        // The salt actually participates: hashing the raw id WITHOUT the salt
        // (i.e. the bare value) must differ from the salted fingerprint.
        let unsalted = format!("{:016x}", fnv1a_64("id-a".as_bytes()));
        assert_ne!(fingerprint_hash("id-a"), unsalted, "salt must be applied");
    }

    #[test]
    fn fallback_fingerprint_reuses_persisted_id_without_patch() {
        // When prefs already hold a fallback id, the hash is derived from it
        // and NO patch is emitted (nothing to persist) — so the slot is
        // stable across launches.
        let prefs = serde_json::json!({ "device_machine_fallback": "persisted-xyz" });
        let (hash, patch) = fallback_fingerprint_from_prefs(&prefs);
        assert_eq!(hash, fingerprint_hash("persisted-xyz"));
        assert!(patch.is_none(), "existing id must not be regenerated/persisted");
    }

    #[test]
    fn fallback_fingerprint_generates_and_emits_patch_when_absent() {
        // First run (no fallback id): a fresh id is generated, the hash is
        // derived from it, and a merge-only patch is returned so the caller
        // can persist it. The patch's id must hash to the returned hash.
        let (hash, patch) = fallback_fingerprint_from_prefs(&serde_json::json!({}));
        let patch = patch.expect("a fresh id must yield a persistence patch");
        let id = patch
            .get("device_machine_fallback")
            .and_then(|v| v.as_str())
            .expect("patch carries the new id under the canonical key");
        assert!(!id.is_empty(), "generated id must be non-empty");
        assert_eq!(hash, fingerprint_hash(id), "returned hash must match the persisted id");
        // Empty/whitespace stored value is treated as absent (regenerated).
        let (_, patch_blank) =
            fallback_fingerprint_from_prefs(&serde_json::json!({"device_machine_fallback": "  "}));
        assert!(patch_blank.is_some(), "blank stored id is treated as absent");
    }

    #[test]
    fn machine_fingerprint_is_stable_hex_within_a_process() {
        // Whether backed by the OS machine id or the persisted fallback, the
        // public entry point must return a stable 16-char hex hash within a
        // process (two calls agree) and never leak a raw id shape.
        let a = machine_fingerprint();
        let b = machine_fingerprint();
        assert_eq!(a, b, "fingerprint must be stable within a process");
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn technicals_path_without_isin_keeps_ticker_only_shape() {
        // Existing callers (US tickers, watchlist entries without ISIN) must
        // continue to produce the original wire path so cached server-side
        // responses keyed by URL don't fragment.
        let p = build_technicals_path("AAPL", None, None);
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_with_isin_appends_isin_param() {
        // The whole point of v0.2.18: French small/mid caps need ISIN so the
        // server can route to the EU OHLC chain (Yahoo with `.PA` suffix).
        let p = build_technicals_path("EXA", Some("FR0010163345"), None);
        assert_eq!(p, "/api/market/technicals?ticker=EXA&isin=FR0010163345");
    }

    #[test]
    fn technicals_path_empty_isin_treated_as_absent() {
        // Position::isin is Option<String>; an `Some("")` slips through
        // serde for legacy rows. Strip it so we don't ship `isin=` (which
        // some axum query parsers reject) — match the trim-and-empty rule
        // used by the server handlers (handlers.rs:OhlcParams).
        let p = build_technicals_path("AAPL", Some(""), None);
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
        let p2 = build_technicals_path("AAPL", Some("   "), None);
        assert_eq!(p2, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn technicals_path_url_encodes_both_params() {
        // Defensive: ISIN is always alphanumeric, but ticker fields from CSVs
        // can carry surprising characters. Confirm urlenc is applied to both.
        let p = build_technicals_path("A B", Some("FR 1234"), None);
        assert_eq!(p, "/api/market/technicals?ticker=A+B&isin=FR+1234");
    }

    // ── &canonical= passthrough (v0.3) ──────────────────────────────

    #[test]
    fn technicals_path_with_canonical_appends_after_isin() {
        // v0.3 parity: when the desktop has resolved the canonical Yahoo
        // symbol, the server must route on `canonical=` instead of the raw
        // ticker. The query order is deterministic (ticker, isin, canonical)
        // so URL-keyed caches stay stable across runs.
        let p = build_technicals_path("STMPA", Some("NL0000226223"), Some("STMPA.PA"));
        assert_eq!(
            p,
            "/api/market/technicals?ticker=STMPA&isin=NL0000226223&canonical=STMPA.PA"
        );
    }

    #[test]
    fn technicals_path_canonical_without_isin_still_appended() {
        // Watchlist entries can have a resolved symbol from earlier resolve
        // calls but no ISIN at all. The canonical param must still pass
        // through so the server routes on it.
        let p = build_technicals_path("STMPA", None, Some("STMPA.PA"));
        assert_eq!(p, "/api/market/technicals?ticker=STMPA&canonical=STMPA.PA");
    }

    #[test]
    fn technicals_path_canonical_empty_treated_as_absent() {
        // `Position::resolved_symbol` is Option<String>; an empty/whitespace
        // string slipped in by serde must NOT emit `canonical=` (server
        // would treat it as a query for the empty symbol).
        let p = build_technicals_path("AAPL", None, Some(""));
        assert_eq!(p, "/api/market/technicals?ticker=AAPL");
        let p2 = build_technicals_path("AAPL", None, Some("   "));
        assert_eq!(p2, "/api/market/technicals?ticker=AAPL");
    }

    #[test]
    fn append_canonical_idempotent_when_absent() {
        // append_canonical is the single helper applied by every endpoint
        // (news/sector/cot/technicals/market). Verify the no-op branch never
        // mutates the path so legacy server caches keyed by URL don't
        // fragment when resolver is disabled.
        let base = "/api/news?ticker=AAPL&name=Apple&isin=US0378331005".to_string();
        let out = append_canonical(base.clone(), None);
        assert_eq!(out, base);
    }

    #[test]
    fn append_canonical_url_encodes_symbol() {
        // Yahoo suffixes are dotted (`.PA`, `.HK`) and dots are urlenc-safe,
        // but exotic suffixes or whitespace must still round-trip cleanly.
        let p = append_canonical("/api/sector?ticker=X".to_string(), Some("X Y"));
        assert_eq!(p, "/api/sector?ticker=X&canonical=X+Y");
    }

    // ── /api/resolve path builder (v0.3) ────────────────────────────

    #[test]
    fn resolve_path_emits_canonical_isin_query() {
        // Canonical shape the server-side resolver expects — must mirror
        // `feat/v0.3-server-bundle` commit 877f18f.
        let p = build_resolve_path("NL0000226223");
        assert_eq!(p, "/api/resolve?isin=NL0000226223");
    }

    #[test]
    fn resolve_path_trims_whitespace_before_encoding() {
        // ISIN slips through with leading/trailing whitespace from CSV imports.
        // Trim BEFORE urlenc so we emit the canonical shape — otherwise we'd
        // ship `%20FR0010163345%20` and split the server-side 180d positive
        // cache key across whitespace variants of the same instrument.
        let p = build_resolve_path("  FR0010163345  ");
        assert_eq!(p, "/api/resolve?isin=FR0010163345");
    }

    #[test]
    fn resolve_path_url_encodes_unexpected_chars() {
        // Defensive: malformed ISINs from broker CSVs must round-trip safely
        // — never crash, never emit a raw `+` or space into the query string.
        let p = build_resolve_path("FR 12+34");
        assert_eq!(p, "/api/resolve?isin=FR+12%2B34");
    }

    // ── X-Run-Session propagation (v0.4.0 P0-11) ────────────────────
    //
    // Every test below serializes on the crate-wide
    // `run_session_test_lock` (defined next to the `ACTIVE_RUN_SESSION`
    // slot in the parent module, in scope here via `use super::*`). It is
    // the SAME mutex used by `analysis_ops::run_session_tests`, so the two
    // suites cannot race on the shared global slot.

    #[test]
    fn active_run_session_returns_none_when_unset() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        assert!(active_run_session().is_none());
    }

    #[test]
    fn set_active_run_session_round_trips_through_active_run_session() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        set_active_run_session("abc123", u64::MAX);
        assert_eq!(active_run_session().as_deref(), Some("abc123"));
        clear_active_run_session();
    }

    #[test]
    fn clear_active_run_session_resets_slot() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        set_active_run_session("xyz789", u64::MAX);
        assert!(active_run_session().is_some());
        clear_active_run_session();
        assert!(active_run_session().is_none());
    }

    #[test]
    fn set_active_run_session_overwrites_previous_value() {
        // Contract: one run, one session — if start_analysis fires a
        // second /run/start (which would be a bug, but defend anyway),
        // the new session must supersede the stale one rather than
        // being silently ignored.
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        set_active_run_session("first", u64::MAX);
        set_active_run_session("second", u64::MAX);
        assert_eq!(active_run_session().as_deref(), Some("second"));
        clear_active_run_session();
    }

    // ── Cross-process disk bridge (2026-06-04) ──────────────────────
    //
    // The codex MCP subprocess never has the in-memory slot set; it must
    // read the session from the bridge file the main process wrote. These
    // tests simulate that by writing the file, clearing the in-memory slot,
    // and asserting `active_run_session()` still resolves from disk.

    #[test]
    fn set_writes_bridge_file_and_active_session_reads_it_when_in_memory_none() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();

        // Main process writes both slot + file.
        set_active_run_session("disk-session-id", u64::MAX);
        // The file exists at the resolved path.
        let path = session_file_path(&resolve_session_runtime_state_dir());
        assert!(path.exists(), "set_active_run_session must write the bridge file");

        // Simulate the SUBPROCESS: in-memory slot empty, only the file
        // carries the session. We clear ONLY the in-memory slot here
        // (clear_active_run_session would remove the file too).
        if let Ok(mut slot) = session_slot().lock() {
            *slot = None;
        }
        assert_eq!(
            active_run_session().as_deref(),
            Some("disk-session-id"),
            "subprocess must fall back to the bridge file when the in-memory slot is None",
        );

        clear_active_run_session();
    }

    #[test]
    fn clear_removes_bridge_file() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        set_active_run_session("to-be-cleared", u64::MAX);
        let path = session_file_path(&resolve_session_runtime_state_dir());
        assert!(path.exists());

        clear_active_run_session();
        assert!(!path.exists(), "clear_active_run_session must remove the bridge file");
        assert!(active_run_session().is_none());
    }

    #[test]
    fn expired_bridge_file_is_ignored_and_deleted() {
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();

        // Write a session whose expiry is already in the past.
        let past = now_epoch_secs().saturating_sub(60);
        write_session_file("expired-session", past);
        let path = session_file_path(&resolve_session_runtime_state_dir());
        assert!(path.exists());

        // In-memory slot empty → reader hits the file → must reject the
        // expired session AND best-effort delete the stale file so it is
        // never re-read.
        if let Ok(mut slot) = session_slot().lock() {
            *slot = None;
        }
        assert!(
            active_run_session().is_none(),
            "a known-expired session must never be returned (would 401 run_session_invalid)",
        );
        assert!(!path.exists(), "expired bridge file must be cleaned up on read");
    }

    #[test]
    fn parse_session_file_honours_expiry_boundary() {
        // expires_at strictly in the future → accepted.
        let body = r#"{"session_id":"s1","expires_at":1000}"#;
        assert_eq!(parse_session_file(body, 999), Some("s1".to_string()));
        // expires_at == now → expired (boundary is inclusive on the dead side).
        assert_eq!(parse_session_file(body, 1000), None);
        // expires_at in the past → expired.
        assert_eq!(parse_session_file(body, 1001), None);
        // expires_at == 0 → "no expiry known" (older writer) → accepted.
        let body0 = r#"{"session_id":"s2","expires_at":0}"#;
        assert_eq!(parse_session_file(body0, 9_999_999), Some("s2".to_string()));
        // Missing expires_at → treated as 0 → accepted.
        let body_no_exp = r#"{"session_id":"s3"}"#;
        assert_eq!(parse_session_file(body_no_exp, 9_999_999), Some("s3".to_string()));
        // Empty / missing session_id → rejected.
        assert_eq!(parse_session_file(r#"{"session_id":"","expires_at":0}"#, 0), None);
        assert_eq!(parse_session_file(r#"{"expires_at":0}"#, 0), None);
        // Malformed JSON → rejected, never panics.
        assert_eq!(parse_session_file("not json", 0), None);
    }

    #[test]
    fn bridge_file_path_matches_between_main_process_and_mcp_subprocess() {
        // THE make-or-break invariant: the path the main process writes and
        // the path the `--mcp-server` subprocess reads must be byte-identical.
        //
        // Main process writer derives the dir from
        // `crate::resolve_runtime_state_dir()`. codex.rs spawns the
        // subprocess with `--data-dir = resolve_runtime_state_dir().parent()`,
        // and `main.rs` then pins the bridge dir to `<data-dir>/runtime-state`.
        // For the canonical layout these MUST be the same absolute file.
        let _guard = run_session_test_lock();

        let main_dir = crate::resolve_runtime_state_dir();
        let main_path = session_file_path(&main_dir);

        // Reproduce exactly what codex.rs + main.rs compute for the
        // subprocess, then point the override at it (as the entrypoint does).
        let data_dir = main_dir
            .parent()
            .expect("runtime-state dir always has a parent")
            .to_path_buf();
        set_mcp_runtime_state_dir(data_dir.join("runtime-state"));
        let subprocess_path = session_file_path(&resolve_session_runtime_state_dir());

        assert_eq!(
            main_path, subprocess_path,
            "main-process writer path and MCP-subprocess reader path diverged — \
             the cross-process bridge would silently never connect",
        );

        // Reset the override so we don't leak it into sibling tests.
        if let Ok(mut slot) = mcp_runtime_state_dir_slot().lock() {
            *slot = None;
        }
    }

    // ── api_post visibility (#3-B) ──────────────────────────────────

    #[test]
    fn classify_post_outcome_quiet_on_success() {
        let resp = ureq::Response::new(200, "OK", "{\"ok\":true}").unwrap();
        assert_eq!(classify_post_outcome("/api/insights", Ok(resp)), None);
        let resp = ureq::Response::new(204, "No Content", "").unwrap();
        assert_eq!(classify_post_outcome("/api/deep-news", Ok(resp)), None);
    }

    #[test]
    fn classify_post_outcome_logs_4xx_with_body() {
        // The exact failure mode that was invisible for 3 weeks: a
        // session-gated endpoint 401'ing with run_session_invalid.
        let resp = ureq::Response::new(401, "Unauthorized", "{\"error\":\"run_session_invalid\"}").unwrap();
        let msg = classify_post_outcome("/api/insights", Err(ureq::Error::Status(401, resp)))
            .expect("non-2xx must produce a log line");
        assert!(msg.contains("/api/insights"), "msg={msg}");
        assert!(msg.contains("401"), "msg={msg}");
        assert!(msg.contains("run_session_invalid"), "4xx body must be surfaced: {msg}");
    }

    #[test]
    fn classify_post_outcome_logs_4xx_empty_body_without_body_suffix() {
        let resp = ureq::Response::new(400, "Bad Request", "").unwrap();
        let msg = classify_post_outcome("/api/market/extracted", Err(ureq::Error::Status(400, resp)))
            .expect("non-2xx must produce a log line");
        assert_eq!(msg, "alfred-api: POST /api/market/extracted -> 400");
    }

    #[test]
    fn classify_post_outcome_logs_5xx_without_body() {
        let resp = ureq::Response::new(500, "Internal Server Error", "stacktrace blah blah").unwrap();
        let msg = classify_post_outcome("/api/deep-news", Err(ureq::Error::Status(500, resp)))
            .expect("5xx must produce a log line");
        assert_eq!(msg, "alfred-api: POST /api/deep-news -> 500");
        assert!(!msg.contains("stacktrace"), "5xx body must NOT be inlined");
    }

    #[test]
    fn classify_post_outcome_logs_transport_error() {
        // Connecting to port 0 on loopback is guaranteed to fail at the
        // transport layer (no listener), exercising the Transport arm
        // without any network dependency.
        let outcome = ureq::post("http://127.0.0.1:0/x")
            .timeout(Duration::from_millis(200))
            .send_string("{}");
        assert!(
            matches!(outcome, Err(ureq::Error::Transport(_))),
            "expected a transport error connecting to 127.0.0.1:0",
        );
        let msg = classify_post_outcome("/api/insights", outcome)
            .expect("transport error must produce a log line");
        assert!(msg.contains("/api/insights"), "msg={msg}");
        assert!(msg.contains("failed:"), "transport errors use the 'failed:' shape: {msg}");
    }

    #[test]
    fn apply_auth_injects_run_session_header_when_set() {
        // Smoke-test apply_auth's contract: when a session is set, the
        // resulting ureq::Request carries X-Run-Session. Built using
        // a dummy base URL since ureq::get accepts arbitrary strings
        // and apply_auth doesn't fire the request.
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        set_active_run_session("test-session-id", u64::MAX);

        let raw = ureq::get("https://example.test/api/news");
        let signed = apply_auth(raw, "/api/news");
        // ureq::Request doesn't expose headers via a public iterator,
        // but the request can be converted back to a fmt::Debug shape
        // that includes them. Cheap proxy: compare the Debug output.
        let dbg = format!("{signed:?}");
        assert!(
            dbg.contains("X-Run-Session") || dbg.contains("x-run-session"),
            "X-Run-Session header missing from signed request — apply_auth contract broken. Debug: {dbg}",
        );

        clear_active_run_session();
    }

    #[test]
    fn apply_auth_omits_run_session_header_when_unset() {
        // /run/start itself runs through apply_auth with no active
        // session — must NOT inject a stale X-Run-Session.
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();

        let raw = ureq::get("https://example.test/run/start");
        let signed = apply_auth(raw, "/run/start");
        let dbg = format!("{signed:?}");
        assert!(
            !dbg.to_lowercase().contains("x-run-session"),
            "X-Run-Session must not be injected when no session is active — request would 401",
        );
    }

    // ── Session-exempt path exemption from X-Run-Session ────────────
    //
    // v0.4.0 P0-14 added /admin/*; v0.4.0 P0-15 (P3-40) extended to
    // /license/* and renamed `is_admin_path` → `is_session_exempt_path`.
    //
    // CRITICAL contract — if a future refactor of apply_auth ever
    // injects X-Run-Session unconditionally, account-level calls fired
    // during an active run would leak a session header into endpoints
    // that are NOT under `require_run_session`. The server would ignore
    // the header (no harm), but the contract is "endpoints get exactly
    // the auth headers they need". Pinned here so a regression fails CI
    // instead of degrading silently to a payload bloat.

    #[test]
    fn is_session_exempt_path_matches_admin_root_and_subroutes() {
        // /admin equals the collection root (defensive — not actually
        // a server-side route today but the rule must cover it).
        assert!(is_session_exempt_path("/admin"));
        // Documented sub-routes from `apps/alfred-api/src/admin.rs`.
        assert!(is_session_exempt_path("/admin/usage"));
        assert!(is_session_exempt_path("/admin/vps-stats"));
        // Future admin routes must inherit the rule.
        assert!(is_session_exempt_path("/admin/anything/nested/here"));
    }

    #[test]
    fn is_session_exempt_path_matches_license_root_and_subroutes() {
        // v0.4.0 P0-15: /license/* added to the exempt set. License
        // endpoints are account-level (activation / validation /
        // status) and not under `require_run_session` server-side.
        assert!(is_session_exempt_path("/license"));
        // Documented sub-routes from `apps/alfred-api/src/license.rs`.
        assert!(is_session_exempt_path("/license/activate"));
        assert!(is_session_exempt_path("/license/validate"));
        assert!(is_session_exempt_path("/license/status"));
        // Future license routes must inherit the rule (e.g. P2-8
        // device deactivation).
        assert!(is_session_exempt_path("/license/devices/abc/deactivate"));
    }

    #[test]
    fn is_session_exempt_path_matches_quota_root_and_subroutes() {
        // v0.4.7 P3-31: /quota/* added to the exempt set. The home strip
        // probes `GET /quota/status` at cold start AND during an active
        // run (when the X-Run-Session slot is populated). The endpoint is
        // NOT under `require_run_session` server-side (mounted at root via
        // `handlers::quota_routes`), so the header must be suppressed —
        // otherwise an active-run home re-render would leak a session
        // header into a non-session endpoint.
        assert!(is_session_exempt_path("/quota"));
        assert!(is_session_exempt_path("/quota/status"));
        // Future quota routes must inherit the rule.
        assert!(is_session_exempt_path("/quota/anything/nested"));
    }

    #[test]
    fn is_session_exempt_path_matches_device_and_redeem() {
        // MON-A / MON-B (2026-05-31): /device* + /redeem are account-scope
        // ops mounted at root server-side (`device::device_routes`,
        // `redeem::redeem_routes`), gated by `require_auth` only and NOT
        // under `require_run_session`. They must skip X-Run-Session
        // injection like the other account-level surfaces.
        assert!(is_session_exempt_path("/device"));
        assert!(is_session_exempt_path("/device/register"));
        // Future device sub-routes inherit the rule.
        assert!(is_session_exempt_path("/device/anything/nested"));
        // /redeem is an exact path (no documented sub-routes).
        assert!(is_session_exempt_path("/redeem"));
        // ...so a nested path under it is NOT exempt (would be a new route
        // we'd have to add deliberately).
        assert!(!is_session_exempt_path("/redeem/extra"));
    }

    #[test]
    fn is_session_exempt_path_matches_api_macro() {
        // BUG-1 (2026-06-08): the home macro briefing (`GET /api/macro`) is a
        // PRE-RUN, globally-cached read with no per-user quota/session.
        // Server-side it was moved OUT of `require_run_session` to sit behind
        // `require_auth` only, so the client must NOT inject X-Run-Session —
        // doing so 401'd the home briefing forever (`run_session_invalid`).
        assert!(is_session_exempt_path("/api/macro"));
        // EXACT match only — the other /api/* analysis endpoints stay gated
        // (also pinned in is_session_exempt_path_rejects_non_exempt_paths).
        assert!(!is_session_exempt_path("/api/macro/extra"));
        assert!(!is_session_exempt_path("/api/market"));
    }

    #[test]
    fn is_session_exempt_path_rejects_non_exempt_paths() {
        // Make sure we don't accidentally over-match. Every gated
        // analysis endpoint must continue to receive X-Run-Session.
        assert!(!is_session_exempt_path("/api/market"));
        assert!(!is_session_exempt_path("/api/news"));
        assert!(!is_session_exempt_path("/api/admin"));        // suffix, not prefix
        assert!(!is_session_exempt_path("/api/license"));      // suffix, not prefix
        assert!(!is_session_exempt_path("/api/quota"));        // suffix, not prefix
        assert!(!is_session_exempt_path("/run/start"));
        assert!(!is_session_exempt_path("/healthz"));
        assert!(!is_session_exempt_path(""));
        assert!(!is_session_exempt_path("/"));
        // Case-sensitive — server routes are case-sensitive too.
        assert!(!is_session_exempt_path("/Admin/usage"));
        assert!(!is_session_exempt_path("/ADMIN/usage"));
        assert!(!is_session_exempt_path("/License/activate"));
        assert!(!is_session_exempt_path("/LICENSE/activate"));
    }

    #[test]
    fn session_exempt_endpoints_skip_run_session_header() {
        // CRITICAL: when an admin clicks the Admin tab while a run is
        // active, or the user clicks Upgrade and the LS activation
        // handler fires `/license/activate`, the session slot is
        // populated. apply_auth must NOT leak X-Run-Session into these
        // account-level requests — those endpoints are not gated by
        // `require_run_session` server-side.
        //
        // Renamed from `admin_endpoint_calls_do_not_inject_run_session_header`
        // in v0.4.0 P0-15 (P3-40) when /license/* joined the exempt
        // set. Same assertions, expanded coverage.
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        set_active_run_session("active-run-while-account-call-fires", u64::MAX);

        for path in [
            "/admin/usage",
            "/admin/vps-stats",
            "/license/activate",
            "/license/validate",
            "/license/status",
            // MON-A / MON-B: device bootstrap + comp-code redemption are
            // account-scope, mounted at root, not under require_run_session.
            "/device/register",
            "/redeem",
            "/api/macro", // BUG-1 (2026-06-08): home macro briefing, pre-run, auth-only
        ] {
            let raw = ureq::get(&format!("https://example.test{path}"));
            let signed = apply_auth(raw, path);
            let dbg = format!("{signed:?}");
            assert!(
                !dbg.to_lowercase().contains("x-run-session"),
                "X-Run-Session must be exempt for {path} — leaked into account-level call. Debug: {dbg}",
            );
        }

        // Sanity: a non-exempt path during the same run still gets the
        // header. Pins the exemption to the path, not a global toggle.
        let raw = ureq::get("https://example.test/api/news");
        let signed = apply_auth(raw, "/api/news");
        let dbg = format!("{signed:?}");
        assert!(
            dbg.to_lowercase().contains("x-run-session"),
            "Non-exempt paths must still receive X-Run-Session during an active run",
        );

        clear_active_run_session();
    }

    #[test]
    fn session_exempt_endpoints_still_inject_hmac_auth() {
        // The X-Run-Session exemption must NOT cascade into the HMAC
        // signing path — admin AND license endpoints are still wrapped
        // by `require_auth` server-side, so X-Client-Hash + X-Timestamp
        // + X-Signature are mandatory. Verify all three headers survive
        // the session-exempt branch.
        //
        // Renamed from `admin_endpoint_calls_still_inject_hmac_auth` in
        // v0.4.0 P0-15 when license was added to the exempt set.
        let _guard = run_session_test_lock();
        redirect_session_bridge_to_temp_dir();
        clear_active_run_session();
        // Set a compile-time-style secret via the runtime env fallback
        // so apply_auth signs the request even in test builds (the
        // option_env! const is None when ALFRED_API_SECRET wasn't
        // injected at compile time).
        std::env::set_var("ALFRED_API_SECRET", "test-secret-for-hmac");

        for path in ["/admin/usage", "/license/activate"] {
            let raw = ureq::get(&format!("https://example.test{path}"));
            let signed = apply_auth(raw, path);
            let dbg = format!("{signed:?}").to_lowercase();
            assert!(
                dbg.contains("x-client-hash"),
                "X-Client-Hash header missing from {path} request. Debug: {dbg}",
            );
            assert!(
                dbg.contains("x-timestamp"),
                "X-Timestamp header missing from {path} request. Debug: {dbg}",
            );
            assert!(
                dbg.contains("x-signature"),
                "X-Signature header missing from {path} request. Debug: {dbg}",
            );
        }

        std::env::remove_var("ALFRED_API_SECRET");
    }

    // ── Admin response shape contract (v0.4.0 P0-14) ────────────────

    #[test]
    fn admin_usage_deserializes_full_server_envelope() {
        // Mirror the exact shape emitted by `admin_usage_handler` in
        // apps/alfred-api/src/admin.rs. If the server ever renames or
        // drops a field, this parse fails loudly — a much better
        // failure mode than silently rendering an empty table.
        let body = serde_json::json!({
            "ok": true,
            "top_users": [
                { "user_hash": "abc12345", "runs_7d": 12 },
                { "user_hash": "def67890", "runs_7d": 3 }
            ],
            "top_tickers": [
                { "isin": "FR0010163345", "last_seen": 1_700_000_000u64 }
            ],
            "runs_7d": 15u64,
            "runs_24h": 4u64,
            "errors_429_today": 2u64,
            "by_endpoint": { "tracked": false, "note": "deferred" },
            "generated_at": 1_700_000_100u64,
        });
        let parsed: AdminUsage = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.top_users.len(), 2);
        assert_eq!(parsed.top_users[0].user_hash, "abc12345");
        assert_eq!(parsed.top_users[0].runs_7d, 12);
        assert_eq!(parsed.top_tickers.len(), 1);
        assert_eq!(parsed.top_tickers[0].isin, "FR0010163345");
        assert_eq!(parsed.runs_7d, 15);
        assert_eq!(parsed.runs_24h, 4);
        assert_eq!(parsed.errors_429_today, 2);
        assert_eq!(parsed.generated_at, 1_700_000_100);
        assert!(parsed.by_endpoint.is_object());
    }

    #[test]
    fn admin_usage_degrades_gracefully_on_missing_fields() {
        // An older server (deployed before today's full payload) might
        // omit a sub-field. We default to zero/empty so the UI renders
        // a benign "nothing yet" state instead of erroring out.
        let body = serde_json::json!({ "ok": true });
        let parsed: AdminUsage = serde_json::from_value(body).unwrap();
        assert!(parsed.top_users.is_empty());
        assert!(parsed.top_tickers.is_empty());
        assert_eq!(parsed.runs_7d, 0);
        assert_eq!(parsed.runs_24h, 0);
        assert_eq!(parsed.errors_429_today, 0);
        assert_eq!(parsed.generated_at, 0);
    }

    #[test]
    fn admin_vps_stats_deserializes_full_server_envelope() {
        let body = serde_json::json!({
            "ok": true,
            "redis": {
                "memory_used": 1_048_576u64,
                "memory_peak": 2_097_152u64,
                "connected_clients": 3u64,
            },
            "process": {
                "rss_bytes": 52_428_800u64,
                "uptime_secs": 3600u64,
            },
            "generated_at": 1_700_000_100u64,
        });
        let parsed: AdminVpsStats = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.redis.memory_used, Some(1_048_576));
        assert_eq!(parsed.redis.memory_peak, Some(2_097_152));
        assert_eq!(parsed.redis.connected_clients, Some(3));
        assert_eq!(parsed.process.rss_bytes, 52_428_800);
        assert_eq!(parsed.process.uptime_secs, 3600);
        assert_eq!(parsed.generated_at, 1_700_000_100);
    }

    #[test]
    fn admin_vps_stats_handles_optional_redis_fields() {
        // Redis INFO parse can return None for any field (older Redis,
        // non-numeric value). The wire shape sends explicit null in
        // that case; deserialization must accept it as Option::None
        // rather than erroring.
        let body = serde_json::json!({
            "ok": true,
            "redis": {
                "memory_used": null,
                "memory_peak": null,
                "connected_clients": null,
            },
            "process": {
                "rss_bytes": 0,
                "uptime_secs": 0,
            },
            "generated_at": 0,
        });
        let parsed: AdminVpsStats = serde_json::from_value(body).unwrap();
        assert!(parsed.redis.memory_used.is_none());
        assert!(parsed.redis.memory_peak.is_none());
        assert!(parsed.redis.connected_clients.is_none());
    }

    // ── /run/start response classification (v0.4.0 P0-11) ───────────

    #[test]
    fn classify_run_start_ok_parses_success_envelope() {
        let body = serde_json::json!({
            "ok": true,
            "run_session_id": "abcd1234deadbeef00000000aabbccdd",
            "expires_at": 1_700_000_600u64,
            "runs_this_week": 1u64,
            "limit": 3u64,
            "period": "rolling_7d",
            "tier": "free",
        });
        let outcome = classify_run_start_ok(body).unwrap();
        match outcome {
            RunStartOutcome::Ok(s) => {
                assert_eq!(s.run_session_id, "abcd1234deadbeef00000000aabbccdd");
                assert_eq!(s.expires_at, 1_700_000_600);
                assert_eq!(s.runs_this_week, 1);
                assert_eq!(s.limit, Some(3));
                assert_eq!(s.tier, "free");
            }
            _ => panic!("expected Ok outcome"),
        }
    }

    #[test]
    fn classify_run_start_ok_handles_paid_unlimited_limit() {
        // Paid users get `limit: "unlimited"` (string sentinel), not a
        // numeric value. The Rust client maps that to Option::None so
        // the UI doesn't render "u32::MAX runs/week".
        let body = serde_json::json!({
            "ok": true,
            "run_session_id": "deadbeefdeadbeefdeadbeefdeadbeef",
            "expires_at": 1_700_000_600u64,
            "runs_this_week": 5u64,
            "limit": "unlimited",
            "period": "rolling_7d",
            "tier": "paid",
        });
        let outcome = classify_run_start_ok(body).unwrap();
        match outcome {
            RunStartOutcome::Ok(s) => {
                assert_eq!(s.tier, "paid");
                assert!(s.limit.is_none());
            }
            _ => panic!("expected Ok outcome"),
        }
    }

    #[test]
    fn classify_run_start_ok_rejects_missing_session_id() {
        let body = serde_json::json!({"expires_at": 1_700_000_600u64});
        let err = classify_run_start_ok(body).expect_err("missing session_id must error");
        assert!(err.to_string().contains("missing run_session_id"));
    }

    #[test]
    fn classify_run_start_429_parses_free_tier_exhausted_envelope() {
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 86400u64,
            "runs_this_week": 3u64,
            "limit": 3u64,
            "period": "rolling_7d",
        });
        let outcome = classify_run_start_429(body).unwrap();
        match outcome {
            RunStartOutcome::QuotaExhausted(info) => {
                assert_eq!(info.retry_after, 86400);
                assert_eq!(info.runs_this_week, 3);
                assert_eq!(info.limit, 3);
                assert_eq!(info.period, "rolling_7d");
            }
            _ => panic!("expected QuotaExhausted outcome"),
        }
    }

    #[test]
    fn classify_run_start_429_with_rate_limited_falls_back_to_err() {
        // If the server ever returns a generic `rate_limited` on
        // /run/start (e.g. global per-IP throttle), the desktop must
        // surface it as a transport-level 429 — not a free-tier-quota
        // event. The classification helper distinguishes the two so
        // the UI doesn't show the upgrade modal for a transient limit.
        let body = serde_json::json!({"error": "rate_limited", "retry_after": 30u64});
        let err = classify_run_start_429(body).expect_err("rate_limited must Err");
        assert_eq!(err.to_string(), "alfred_api_rate_limited");
    }

    #[test]
    fn classify_run_start_429_empty_body_falls_back_to_rate_limited() {
        // Defensive: a 429 with empty body (server bug or proxy
        // truncation) maps to alfred_api_rate_limited, not a phantom
        // quota event.
        let body = serde_json::Value::Null;
        let err = classify_run_start_429(body).expect_err("empty body must Err");
        assert_eq!(err.to_string(), "alfred_api_rate_limited");
    }

    // ── 429 / 401 body classification (v0.4.0 P0-13) ────────────────

    #[test]
    fn classify_429_body_detects_free_tier_exhausted_with_full_payload() {
        // Server-side P0-12 emits this exact envelope on quota exhaustion
        // (see docs/monetization-architecture.md). The desktop must surface
        // ALL three params (retry_after, limit, period) so the modal can
        // render "limite 3 / 7 jours, reset dans Xj" without re-querying.
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 86400,
            "runs_this_week": 3,
            "limit": 3,
            "period": "rolling_7d"
        });
        let classified = classify_429_body(&body, None);
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:86400:3:rolling_7d"),
            "structured code must encode retry_after:limit:period in order"
        );
    }

    #[test]
    fn classify_429_body_falls_back_to_header_when_body_retry_after_missing() {
        // Defensive: if the server forgets `retry_after` in the body but
        // sets the standard HTTP header, use the header — the modal still
        // needs *something* to compute "reset in Xj".
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "limit": 3,
            "period": "rolling_7d"
        });
        let classified = classify_429_body(&body, Some(43200));
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:43200:3:rolling_7d"),
        );
    }

    #[test]
    fn classify_429_body_uses_default_period_when_missing() {
        // Older servers might omit `period` — default to `rolling_7d`
        // because that's the only quota policy v0.4.0 ships.
        let body = serde_json::json!({
            "error": "free_tier_exhausted",
            "retry_after": 100,
            "limit": 3
        });
        let classified = classify_429_body(&body, None);
        assert_eq!(
            classified.as_deref(),
            Some("alfred_free_tier_exhausted:100:3:rolling_7d"),
        );
    }

    #[test]
    fn classify_429_body_returns_none_for_rate_limited() {
        // The existing P0-9 global RPM rate-limit code (`rate_limited`) must
        // NOT match — that one is retryable and stays on the transient path.
        let body = serde_json::json!({"error": "rate_limited", "retry_after": 12});
        assert_eq!(classify_429_body(&body, None), None);
    }

    #[test]
    fn classify_429_body_returns_none_for_empty_or_unparseable() {
        // An older server (or a proxy that strips bodies) might give us an
        // empty 429. We must fall through to the retry path — assuming
        // "rate_limited" — rather than ship a bogus modal.
        assert_eq!(classify_429_body(&Value::Null, None), None);
        assert_eq!(classify_429_body(&serde_json::json!({}), None), None);
    }

    #[test]
    fn classify_401_body_detects_run_session_invalid() {
        // P0-11 session token expired or revoked — the desktop should call
        // /run/start to get a fresh one. Distinct from HMAC failure.
        let body = serde_json::json!({"error": "run_session_invalid", "hint": "session expired"});
        assert_eq!(classify_401_body(&body), "alfred_run_session_invalid");
    }

    #[test]
    fn classify_401_body_defaults_to_unauthorized() {
        // HMAC failure, missing X-Client-Hash, dev-mode rejection, or older
        // server that doesn't set the body field — all map to the generic
        // `alfred_api_unauthorized` (the v0.3.x wire shape).
        assert_eq!(classify_401_body(&Value::Null), "alfred_api_unauthorized");
        assert_eq!(
            classify_401_body(&serde_json::json!({})),
            "alfred_api_unauthorized",
        );
        assert_eq!(
            classify_401_body(&serde_json::json!({"error": "unauthorized"})),
            "alfred_api_unauthorized",
        );
    }

    // ── License response contract (v0.4.0 P0-15) ────────────────────

    #[test]
    fn license_activation_response_parses_full_envelope() {
        // Mirror the exact shape emitted by `activate_handler` in
        // apps/alfred-api/src/license.rs (Activated arm). A server-side
        // rename or drop of any of these fields would surface here as a
        // parse error rather than a silently-empty user-pref write.
        let body = serde_json::json!({
            "ok": true,
            "tier": "paid",
            "instance_id": "inst-abc123",
            "expires_at": "2027-05-17T00:00:00Z",
            "validated_at": 1_700_000_100u64,
        });
        let parsed: LicenseActivationResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert_eq!(parsed.instance_id.as_deref(), Some("inst-abc123"));
        assert_eq!(parsed.expires_at.as_deref(), Some("2027-05-17T00:00:00Z"));
        assert_eq!(parsed.validated_at, 1_700_000_100);
    }

    #[test]
    fn license_activation_response_degrades_on_missing_fields() {
        // A wire-compatible older server (one ship behind) may omit a
        // sub-field — defaults must keep parse green so the desktop
        // doesn't dead-end on an upgrade flow.
        let body = serde_json::json!({ "ok": true, "tier": "paid" });
        let parsed: LicenseActivationResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert!(parsed.instance_id.is_none());
        assert!(parsed.expires_at.is_none());
        assert_eq!(parsed.validated_at, 0);
    }

    #[test]
    fn license_status_response_parses_full_envelope() {
        // Mirror the exact shape emitted by `status_handler` in
        // apps/alfred-api/src/license.rs. Note: `expires_at` is an
        // epoch second here (distinct from activate's ISO string) —
        // pin the type so they don't drift.
        let body = serde_json::json!({
            "ok": true,
            "tier": "paid",
            "expires_at": 1_810_857_600u64,
            "validated_at": 1_700_000_100u64,
            "pending_notice": "refunded",
        });
        let parsed: LicenseStatusResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "paid");
        assert_eq!(parsed.expires_at, Some(1_810_857_600));
        assert_eq!(parsed.validated_at, Some(1_700_000_100));
        assert_eq!(parsed.pending_notice.as_deref(), Some("refunded"));
    }

    #[test]
    fn license_status_response_handles_free_tier_null_fields() {
        // Free-tier users have `tier: "free"` and the timestamp +
        // notice fields are explicit nulls (default TierRecord).
        let body = serde_json::json!({
            "ok": true,
            "tier": "free",
            "expires_at": null,
            "validated_at": null,
            "pending_notice": null,
        });
        let parsed: LicenseStatusResponse = serde_json::from_value(body).unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.tier, "free");
        assert!(parsed.expires_at.is_none());
        assert!(parsed.validated_at.is_none());
        assert!(parsed.pending_notice.is_none());
    }

    #[test]
    fn classify_503_body_detects_license_provider_not_configured() {
        // The server returns this when LEMON_SQUEEZY_API_KEY is empty —
        // common on first deploy before Pierre wires the LS account.
        // The desktop must show a clean "temporarily unavailable" banner.
        let body = serde_json::json!({
            "error": "license_provider_not_configured",
            "hint": "Server admin must populate LEMON_SQUEEZY_API_KEY",
        });
        assert_eq!(
            classify_503_body(&body),
            "alfred_license_provider_not_configured",
        );
    }

    #[test]
    fn classify_503_body_falls_back_to_generic_http_error() {
        // Any other 503 (overload, upstream failure with no body) must
        // surface as the standard `alfred_api_http_error:503` so existing
        // transport handling continues to work.
        assert_eq!(
            classify_503_body(&Value::Null),
            "alfred_api_http_error:503",
        );
        assert_eq!(
            classify_503_body(&serde_json::json!({})),
            "alfred_api_http_error:503",
        );
        assert_eq!(
            classify_503_body(&serde_json::json!({"error": "service_unavailable"})),
            "alfred_api_http_error:503",
        );
    }
}
