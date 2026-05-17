//! Compile-time admin allowlist — v0.4.0 P0-14.
//!
//! The desktop client hides the Admin tab in settings unless the current
//! user's `client_hash` (FNV-1a of OpenAI JWT, computed in
//! `alfred_api_client.rs::get_client_hash`) appears in
//! [`ADMIN_HASHES_WHITELIST`]. This is the client-side mirror of the
//! server-side `ALFRED_ADMIN_HASHES` env allowlist — see
//! `docs/monetization-architecture.md` § "Admin whitelist baked-in".
//!
//! ## Rationale
//!
//! - The desktop binary is open-source (`pderrier/alfred-desktop`). If the
//!   admin tab was always visible, a curious user reading the JS could
//!   trigger `/admin/usage` and the server would 403 — visible but
//!   inaccessible, which is bad UX. By gating client-side too, the tab is
//!   simply absent for non-admins.
//! - Adding an admin = rebuild + redeploy the desktop binary. Acceptable
//!   for v0.4.0 launch scale; tooling for a remote allowlist update is
//!   deferred (see plan § "v2").
//! - The whitelist is intentionally checked in EMPTY so this constant
//!   never leaks an admin hash via the public repo. Pierre adds his hash
//!   in a follow-up commit (or via env var override at compile time —
//!   future work).
//!
//! ## Hash format
//!
//! FNV-1a 64-bit of the OpenAI JWT, hex-encoded → 16 chars lowercase.
//! Computed by `alfred_api_client::get_client_hash`. Compare
//! case-insensitively against this list (matches the server-side
//! `auth::is_admin_hash` convention).

/// Hardcoded admin allowlist. Adding an admin = rebuild + redeploy.
///
/// EMPTY by default — the public repo never carries an actual admin hash.
/// Pierre adds his own hash in a separate, never-published-publicly commit
/// (or as a build-time override). With this list empty, no user sees the
/// Admin tab — which is the safe default for any external build.
pub const ADMIN_HASHES_WHITELIST: &[&str] = &[
    // Placeholder. Pierre will fill in his own hash via a follow-up.
];

/// Pure helper: is `user_hash` on the desktop-side whitelist? Used to
/// gate the Admin tab visibility.
///
/// Compares case-insensitively (hex is conventionally case-insensitive,
/// and the server-side `is_admin_hash` does the same).
pub fn is_whitelisted_admin(user_hash: &str) -> bool {
    if user_hash.is_empty() {
        return false;
    }
    ADMIN_HASHES_WHITELIST
        .iter()
        .any(|h| h.eq_ignore_ascii_case(user_hash))
}

/// Test-only matcher mirror of [`is_whitelisted_admin`] that accepts an
/// explicit list, used to exercise the match LOGIC against a non-empty
/// whitelist without mutating the shipping `&[&str]` constant (which is
/// compile-time and must stay empty for the public build).
///
/// Kept under `#[cfg(test)]` so it never compiles into production binaries.
#[cfg(test)]
fn is_whitelisted_admin_with_list(list: &[&str], user_hash: &str) -> bool {
    if user_hash.is_empty() {
        return false;
    }
    list.iter().any(|h| h.eq_ignore_ascii_case(user_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_tab_hidden_when_whitelist_empty() {
        // Public builds ship an empty whitelist. Verify the public-shape
        // contract : no user can pass the gate, even an empty/malformed
        // hash. The TEST_WHITELIST shadow below verifies the LOGIC; this
        // test pins the value of the SHIPPING constant.
        assert!(
            ADMIN_HASHES_WHITELIST.is_empty(),
            "shipping whitelist must be empty in the public repo — a non-empty list \
             would leak an admin hash. Pierre adds his own hash via a private patch \
             during build, not a checked-in change.",
        );
        // With an empty whitelist, no hash matches.
        assert!(!is_whitelisted_admin(""));
        assert!(!is_whitelisted_admin("abc123"));
        assert!(!is_whitelisted_admin("0123456789abcdef"));
    }

    #[test]
    fn admin_whitelist_lookup_returns_false_for_unmatched_hash() {
        // Spec-asked v0.4.0 P0-14 contract test. With a known non-empty
        // whitelist, a hash not on the list must return false.
        let list = &["0123456789abcdef"];
        assert!(!is_whitelisted_admin_with_list(list, "fedcba9876543210"));
        assert!(!is_whitelisted_admin_with_list(list, ""));
        // Length-off-by-one defends against a "starts_with" mis-impl.
        assert!(!is_whitelisted_admin_with_list(list, "0123456789abcde"));
        // A second whitelist entry must not match unrelated hashes.
        let list_multi = &["aaa", "bbb"];
        assert!(!is_whitelisted_admin_with_list(list_multi, "ccc"));
    }

    #[test]
    fn admin_whitelist_lookup_returns_true_for_matched_hash() {
        // Spec-asked v0.4.0 P0-14 contract test. With a non-empty
        // whitelist (test override), a hash on the list must return true.
        let list = &["0123456789abcdef"];
        assert!(is_whitelisted_admin_with_list(list, "0123456789abcdef"));
        // Case-insensitive (hex is conventionally case-insensitive, and
        // the server-side `is_admin_hash` does the same).
        assert!(is_whitelisted_admin_with_list(list, "0123456789ABCDEF"));
        // Multiple entries — any one match suffices.
        let list_multi = &["aaa", "bbb", "ccc"];
        assert!(is_whitelisted_admin_with_list(list_multi, "bbb"));
        assert!(is_whitelisted_admin_with_list(list_multi, "CCC"));
    }
}
