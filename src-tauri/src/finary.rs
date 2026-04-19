//! Finary connector — native Rust implementation.
//!
//! Replaces the JS finary-connector sidecar + Playwright scripts.
//! Opens the system browser for interactive login and
//! ureq for Finary API calls with stored session cookies.

use std::{env, fs, path::PathBuf, thread, time::Duration};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::helpers::now_iso_string;
use crate::storage::{read_json_file, write_json_file};

// ── Clerk token refresh (no browser needed) ────────────────────────

/// Get a fresh Finary session token.
/// 1. Try refreshing via Clerk REST API using stored __client cookie
/// 2. Fall back to stored session token (may be expired)
fn get_fresh_token() -> Result<String> {
    // Try Clerk REST refresh first
    match refresh_clerk_token() {
        Ok(token) => {
            crate::debug_log("finary: clerk token refresh succeeded");
            return Ok(token);
        }
        Err(e) => {
            crate::debug_log(&format!("finary: clerk token refresh failed: {e}"));
            // If Clerk credentials aren't set up yet, fall back to stored token
            let msg = e.to_string();
            if msg.contains("clerk_state_not_found")
                || msg.contains("clerk_no_client_cookie")
                || msg.contains("clerk_refresh_no_client_cookie")
            {
                let session = load_session()?;
                return extract_session_token(&session)
                    .ok_or_else(|| anyhow!("finary_no_session_token:clerk refresh not configured and no stored token"));
            }
            // Clerk credentials exist but refresh failed — propagate the real error
            return Err(e);
        }
    }
}

const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Refresh the __session JWT using the long-lived __client cookie via Clerk REST API.
fn refresh_clerk_token() -> Result<String> {
    let client_cookie = resolve_client_cookie()?;
    if client_cookie.is_empty() {
        return Err(anyhow!("clerk_refresh_no_client_cookie"));
    }

    // Resolve session ID: from clerk-state.json or by querying Clerk /v1/client
    let session_id = resolve_clerk_session_id(&client_cookie)?;
    if session_id.is_empty() {
        return Err(anyhow!("clerk_refresh_no_session_id"));
    }

    // POST /v1/client/sessions/{sid}/tokens to get a fresh JWT
    let url = format!("https://clerk.finary.com/v1/client/sessions/{session_id}/tokens?_clerk_js_version=5");
    let response: Value = ureq::post(&url)
        .set("Cookie", &format!("__client={client_cookie}"))
        .set("Origin", "https://app.finary.com")
        .set("Referer", "https://app.finary.com/")
        .set("User-Agent", BROWSER_USER_AGENT)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .send_string("")
        .map_err(|e| anyhow!("clerk_token_refresh_failed:{e}"))?
        .into_json()
        .map_err(|e| anyhow!("clerk_token_parse_failed:{e}"))?;
    let jwt = response.get("jwt").and_then(|v| v.as_str()).unwrap_or_default();
    if jwt.is_empty() {
        return Err(anyhow!("clerk_token_refresh_empty_jwt"));
    }

    // Persist for future use
    let _ = save_clerk_state(&client_cookie, &session_id);
    let ts = now_iso_string();
    let native_auth = json!({ "session_token": jwt, "updated_at": ts });
    let _ = write_json_file(&resolve_session_dir().join("native-auth.json"), &native_auth);
    Ok(jwt.to_string())
}

/// Find the __client cookie from: clerk-state.json → playwright-state.json → session.json cookies
fn resolve_client_cookie() -> Result<String> {
    // 1. clerk-state.json
    if let Ok(state) = read_json_file(&clerk_state_path()) {
        let cookie = state.get("client_cookie").and_then(|v| v.as_str()).unwrap_or_default();
        if !cookie.is_empty() {
            return Ok(cookie.to_string());
        }
    }
    // 2. playwright-state.json cookies
    let pw_path = resolve_session_dir().join("playwright-state.json");
    if pw_path.exists() {
        if let Ok(state) = read_json_file(&pw_path) {
            if let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) {
                for cookie in cookies {
                    if cookie.get("name").and_then(|v| v.as_str()) == Some("__client") {
                        let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or_default();
                        if !value.is_empty() {
                            return Ok(value.to_string());
                        }
                    }
                }
            }
        }
    }
    // 3. session.json cookies array (from old headless_chrome flow)
    let session_path = resolve_session_dir().join("session.json");
    if session_path.exists() {
        if let Ok(state) = read_json_file(&session_path) {
            if let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) {
                for cookie in cookies {
                    if cookie.get("name").and_then(|v| v.as_str()) == Some("__client") {
                        let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or_default();
                        if !value.is_empty() {
                            return Ok(value.to_string());
                        }
                    }
                }
            }
        }
    }
    Err(anyhow!("clerk_no_client_cookie:no __client cookie found in any session file"))
}

/// Resolve the Clerk session ID from: clerk-state.json → Clerk REST API
fn resolve_clerk_session_id(client_cookie: &str) -> Result<String> {
    // 1. clerk-state.json
    if let Ok(state) = read_json_file(&clerk_state_path()) {
        let sid = state.get("clerk_session_id").and_then(|v| v.as_str()).unwrap_or_default();
        if !sid.is_empty() {
            return Ok(sid.to_string());
        }
    }
    // 2. Query Clerk REST API
    let client_info: Value = ureq::get("https://clerk.finary.com/v1/client?_clerk_js_version=5")
        .set("Cookie", &format!("__client={client_cookie}"))
        .set("Origin", "https://app.finary.com")
        .set("Referer", "https://app.finary.com/")
        .set("User-Agent", BROWSER_USER_AGENT)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| anyhow!("clerk_client_query_failed:{e}"))?
        .into_json()
        .map_err(|e| anyhow!("clerk_client_parse_failed:{e}"))?;
    let sessions = client_info.get("response")
        .and_then(|r| r.get("sessions"))
        .and_then(|s| s.as_array());
    if let Some(sessions) = sessions {
        for session in sessions {
            let status = session.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status == "active" {
                let sid = session.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if !sid.is_empty() {
                    return Ok(sid.to_string());
                }
            }
        }
    }
    Err(anyhow!("clerk_no_active_session"))
}

fn clerk_state_path() -> PathBuf {
    resolve_session_dir().join("clerk-state.json")
}

fn save_clerk_state(client_cookie: &str, session_id: &str) -> Result<()> {
    let state = json!({
        "client_cookie": client_cookie,
        "clerk_session_id": session_id,
        "updated_at": now_iso_string()
    });
    write_json_file(&clerk_state_path(), &state)
}

// ── Session file management ────────────────────────────────────────

fn resolve_session_dir() -> PathBuf {
    env::var("FINARY_SESSION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            crate::paths::default_data_dir().join("finary-session")
        })
}

fn load_session() -> Result<Value> {
    let dir = resolve_session_dir();

    // Try native-auth.json first (has session_token from Rust bridge)
    let native_auth_path = dir.join("native-auth.json");
    if native_auth_path.exists() {
        if let Ok(auth) = read_json_file(&native_auth_path) {
            let token = auth.get("session_token")
                .or_else(|| auth.get("access_token"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !token.is_empty() {
                return Ok(json!({
                    "session_state": "valid",
                    "valid": true,
                    "requires_reauth": false,
                    "token": token,
                    "source": "native-auth.json",
                    "updated_at": auth.get("updated_at")
                }));
            }
        }
    }

    // Try jwt.json (has session_token from Clerk extraction)
    let jwt_path = dir.join("jwt.json");
    if jwt_path.exists() {
        if let Ok(jwt) = read_json_file(&jwt_path) {
            let token = jwt.get("session_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !token.is_empty() {
                return Ok(json!({
                    "session_state": "valid",
                    "valid": true,
                    "requires_reauth": false,
                    "token": token,
                    "source": "jwt.json"
                }));
            }
        }
    }

    // Try playwright-state.json (extract token from cookies)
    let pw_path = dir.join("playwright-state.json");
    if pw_path.exists() {
        if let Ok(state) = read_json_file(&pw_path) {
            if let Some(cookies) = state.get("cookies").and_then(|v| v.as_array()) {
                // Look for session token in cookies
                let mut best_token = String::new();
                for cookie in cookies {
                    let name = cookie.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                    let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or_default();
                    if (name == "__session" || name == "__client" || name.starts_with("__clerk"))
                        && value.len() > best_token.len()
                    {
                        best_token = value.to_string();
                    }
                }
                if best_token.is_empty() {
                    // Try any JWT-like cookie
                    for cookie in cookies {
                        let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or_default();
                        if value.contains('.') && value.len() > 100 {
                            best_token = value.to_string();
                            break;
                        }
                    }
                }
                if !best_token.is_empty() {
                    return Ok(json!({
                        "session_state": "valid",
                        "valid": true,
                        "requires_reauth": false,
                        "token": best_token,
                        "source": "playwright-state.json"
                    }));
                }
            }
        }
    }

    // Try session.json (native format)
    let session_path = dir.join("session.json");
    if session_path.exists() {
        return read_json_file(&session_path);
    }

    Ok(json!({
        "session_state": "missing",
        "valid": false,
        "requires_reauth": true
    }))
}

fn save_session(session: &Value) -> Result<()> {
    let dir = resolve_session_dir();
    fs::create_dir_all(&dir)?;
    write_json_file(&resolve_session_dir().join("session.json"), session)
}

fn is_session_usable(payload: &Value) -> bool {
    payload.get("session_state").and_then(|v| v.as_str()) == Some("valid")
        && payload.get("requires_reauth").and_then(|v| v.as_bool()) != Some(true)
}

fn extract_session_token(session: &Value) -> Option<String> {
    session
        .get("token")
        .or_else(|| session.get("session_token"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ── Finary API calls ───────────────────────────────────────────────

const FINARY_API_BASE: &str = "https://api.finary.com";

fn finary_api_get(path: &str, token: &str) -> Result<Value> {
    let url = format!("{FINARY_API_BASE}{path}");
    let response: Value = ureq::get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .set("User-Agent", BROWSER_USER_AGENT)
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| anyhow!("finary_api_request_failed:{e}"))?
        .into_json()
        .map_err(|e| anyhow!("finary_api_parse_failed:{e}"))?;
    Ok(response)
}

fn fetch_finary_snapshot_with_token(token: &str) -> Result<Value> {
    let investments = finary_api_get("/users/me/securities", token)
        .map_err(|e| anyhow!("finary_securities_fetch_failed:{e}"))?;
    let holdings = finary_api_get("/users/me/holdings_accounts", token).unwrap_or_else(|_| json!({}));
    let transactions = finary_api_get("/users/me/transactions", token).unwrap_or_else(|_| json!({}));
    let orders = match finary_api_get("/users/me/orders", token) {
        Ok(payload) => {
            let count = payload.get("result")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if count == 0 {
                eprintln!("[finary] /users/me/orders returned 0 items (keys: {:?})",
                    payload.as_object().map(|o| o.keys().collect::<Vec<_>>()).unwrap_or_default());
            }
            payload
        }
        Err(e) => {
            eprintln!("[finary] /users/me/orders failed: {e} — trying /users/me/securities_orders");
            finary_api_get("/users/me/securities_orders", token).unwrap_or_else(|e2| {
                eprintln!("[finary] /users/me/securities_orders also failed: {e2}");
                json!({})
            })
        }
    };

    // Use the JS mapper logic replicated in Rust via the snapshot mapper
    Ok(json!({
        "investmentsPayload": investments,
        "accountsPayload": holdings,
        "transactionsPayload": transactions,
        "ordersPayload": orders
    }))
}

// ── Browser login ──────────────────────────────────────────────────

fn run_browser_login() -> Result<Value> {
    let session_dir = resolve_session_dir();
    let user_data_dir = session_dir.join("playwright-user-data");
    fs::create_dir_all(&user_data_dir)?;

    // Launch Chrome with the persistent profile and a remote debugging port
    let debug_port = 19222;
    let chrome = find_chrome_binary()?;
    let mut child = std::process::Command::new(&chrome)
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg(format!("--remote-debugging-port={debug_port}"))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("https://app.finary.com/login")
        .spawn()
        .map_err(|e| anyhow!("finary_browser_launch_failed:{chrome}:{e}"))?;

    // Poll until we can extract a valid __session cookie via CDP, or Chrome exits
    let timeout = Duration::from_secs(300);
    let poll_interval = Duration::from_secs(3);
    let start = std::time::Instant::now();

    // Give Chrome a moment to start the debug server
    thread::sleep(Duration::from_secs(2));

    let result = loop {
        if start.elapsed() > timeout {
            let _ = child.kill();
            break Err(anyhow!("finary_browser_login_timeout"));
        }

        // Try extracting session + Clerk credentials via CDP
        if let Ok(extraction) = extract_session_via_cdp(debug_port) {
            if !extraction.session_token.is_empty() {
                if finary_api_get("/users/me", &extraction.session_token).is_ok() {
                    let ts = now_iso_string();
                    let session = json!({
                        "session_state": "valid",
                        "valid": true,
                        "requires_reauth": false,
                        "token": extraction.session_token,
                        "updated_at": ts,
                        "source": "browser_cdp"
                    });
                    save_session(&session)?;
                    let native_auth = json!({
                        "session_token": extraction.session_token,
                        "updated_at": ts
                    });
                    let _ = write_json_file(&resolve_session_dir().join("native-auth.json"), &native_auth);
                    // Save Clerk credentials for future token refreshes (no browser needed)
                    if !extraction.client_cookie.is_empty() && !extraction.clerk_session_id.is_empty() {
                        let _ = save_clerk_state(&extraction.client_cookie, &extraction.clerk_session_id);
                    }
                    // Done — kill Chrome, we can refresh tokens via Clerk REST API
                    let _ = child.kill();
                    break Ok(session);
                }
            }
        }

        // Check if Chrome was closed by user
        if let Ok(Some(_exit)) = child.try_wait() {
            break Err(anyhow!("finary_browser_closed:browser was closed before a valid session was detected"));
        }

        thread::sleep(poll_interval);
    };

    result
}

fn find_chrome_binary() -> Result<String> {
    // Windows paths
    for candidate in [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }
    // Try PATH
    if let Ok(output) = std::process::Command::new("where").arg("chrome").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().lines().next().unwrap_or("").to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }
    // macOS
    let mac_path = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    if std::path::Path::new(mac_path).exists() {
        return Ok(mac_path.to_string());
    }
    // Linux
    for candidate in ["google-chrome", "google-chrome-stable", "chromium-browser", "chromium"] {
        if std::process::Command::new("which").arg(candidate).output()
            .map(|o| o.status.success()).unwrap_or(false)
        {
            return Ok(candidate.to_string());
        }
    }
    Err(anyhow!("finary_browser_not_found:could not find Chrome or Chromium"))
}

/// Result of CDP extraction: session token + Clerk credentials for future refreshes.
struct CdpExtraction {
    session_token: String,
    client_cookie: String,
    clerk_session_id: String,
}

/// Extract session token + Clerk refresh credentials from Chrome via CDP.
fn extract_session_via_cdp(port: u16) -> Result<CdpExtraction> {
    use tungstenite::connect;

    let pages: Vec<Value> = ureq::get(&format!("http://127.0.0.1:{port}/json/list"))
        .timeout(Duration::from_secs(2))
        .call()
        .map_err(|e| anyhow!("cdp_list_failed:{e}"))?
        .into_json()
        .map_err(|e| anyhow!("cdp_list_parse_failed:{e}"))?;

    let finary_page = pages.iter().find(|p| {
        let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
        url.contains("finary.com") && !url.contains("/login") && !url.contains("/signup")
    });

    let page = finary_page.ok_or_else(|| anyhow!("cdp_no_finary_page:user has not completed login yet"))?;
    let ws_url = page.get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("cdp_no_ws_url"))?;

    let (mut socket, _) = connect(ws_url)
        .map_err(|e| anyhow!("cdp_ws_connect_failed:{e}"))?;

    // 1a. Get fresh JWT via Clerk.session.getToken()
    let token_request = json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": {
            "expression": "window.Clerk?.session?.getToken?.()",
            "awaitPromise": true
        }
    });
    socket.send(tungstenite::Message::Text(token_request.to_string()))
        .map_err(|e| anyhow!("cdp_ws_send_failed:{e}"))?;

    let mut session_token = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(tungstenite::Message::Text(text)) = socket.read() {
            let response: Value = serde_json::from_str(&text).unwrap_or_default();
            if response.get("id").and_then(|v| v.as_i64()) == Some(1) {
                session_token = response
                    .get("result").and_then(|r| r.get("result"))
                    .and_then(|r| r.get("value")).and_then(|v| v.as_str())
                    .unwrap_or("").to_string();
                break;
            }
        }
    }

    if session_token.is_empty() || !session_token.contains('.') {
        let _ = socket.close(None);
        return Err(anyhow!("cdp_clerk_no_token"));
    }

    // 1b. Get Clerk session ID
    let sid_request = json!({
        "id": 10,
        "method": "Runtime.evaluate",
        "params": { "expression": "String(window.Clerk?.session?.id || '')" }
    });
    let mut clerk_session_id = String::new();
    if socket.send(tungstenite::Message::Text(sid_request.to_string())).is_ok() {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if let Ok(tungstenite::Message::Text(text)) = socket.read() {
                let response: Value = serde_json::from_str(&text).unwrap_or_default();
                if response.get("id").and_then(|v| v.as_i64()) == Some(10) {
                    clerk_session_id = response
                        .get("result").and_then(|r| r.get("result"))
                        .and_then(|r| r.get("value")).and_then(|v| v.as_str())
                        .unwrap_or("").to_string();
                    break;
                }
            }
        }
    }

    // 2. Get __client cookie via Network.getAllCookies
    let cookie_request = json!({ "id": 2, "method": "Network.getAllCookies" });
    let _ = socket.send(tungstenite::Message::Text(cookie_request.to_string()));

    let mut client_cookie = String::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if let Ok(tungstenite::Message::Text(text)) = socket.read() {
            let response: Value = serde_json::from_str(&text).unwrap_or_default();
            if response.get("id").and_then(|v| v.as_i64()) == Some(2) {
                if let Some(cookies) = response.get("result")
                    .and_then(|r| r.get("cookies")).and_then(|c| c.as_array())
                {
                    for cookie in cookies {
                        let name = cookie.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or("");
                        if name == "__client" && !value.is_empty() {
                            client_cookie = value.to_string();
                            break;
                        }
                    }
                }
                break;
            }
        }
    }

    let _ = socket.close(None);

    Ok(CdpExtraction { session_token, client_cookie, clerk_session_id })
}

// ── Public API (replaces finary-connector sidecar endpoints) ───────

pub fn session_status() -> Result<Value> {
    crate::debug_log("finary: session_status check");
    // Try getting a fresh token (Clerk refresh → stored file fallback)
    match get_fresh_token() {
        Ok(token) if !token.is_empty() => {
            match finary_api_get("/users/me", &token) {
                Ok(_) => Ok(json!({
                    "session_state": "valid",
                    "valid": true,
                    "requires_reauth": false,
                    "updated_at": now_iso_string()
                })),
                Err(_) => Ok(json!({
                    "session_state": "needs_reauth",
                    "valid": false,
                    "requires_reauth": true,
                    "last_error_code": "session_token_expired"
                })),
            }
        }
        _ => {
            let session = load_session()?;
            let token = extract_session_token(&session).unwrap_or_default();
            if token.is_empty() {
                return Ok(session);
            }
            Ok(json!({
                "session_state": "needs_reauth",
                "valid": false,
                "requires_reauth": true,
                "last_error_code": "session_token_expired"
            }))
        }
    }
}

pub fn session_connect(payload: Option<Value>) -> Result<Value> {
    let session = load_session()?;
    if is_session_usable(&session) {
        return Ok(session);
    }
    // Try to use provided credentials if any
    if let Some(creds) = payload {
        let email = creds.get("email").and_then(|v| v.as_str()).unwrap_or_default();
        let password = creds.get("password").and_then(|v| v.as_str()).unwrap_or_default();
        if !email.is_empty() && !password.is_empty() {
            // Attempt direct API login
            let body = json!({ "email": email, "password": password });
            match ureq::post(&format!("{FINARY_API_BASE}/auth/signin"))
                .set("Content-Type", "application/json")
                .timeout(Duration::from_secs(10))
                .send_json(&body)
            {
                Ok(resp) => {
                    let result: Value = resp.into_json().unwrap_or_else(|_| json!({}));
                    if result.get("token").is_some() || result.get("session_token").is_some() {
                        let new_session = json!({
                            "session_state": "valid",
                            "valid": true,
                            "requires_reauth": false,
                            "token": result.get("token").or_else(|| result.get("session_token")),
                            "updated_at": now_iso_string(),
                            "source": "api_signin"
                        });
                        save_session(&new_session)?;
                        return Ok(new_session);
                    }
                }
                Err(_) => {}
            }
        }
    }
    Err(anyhow!("reauth_required:session_invalid"))
}

pub fn session_refresh() -> Result<Value> {
    let session = load_session()?;
    if is_session_usable(&session) {
        return Ok(session);
    }
    Err(anyhow!("reauth_required:session_expired"))
}

pub fn session_browser_start() -> Result<Value> {
    Ok(json!({
        "ok": true,
        "message": "Browser login ready. A Chrome window will open for you to log in to Finary."
    }))
}

pub fn session_browser_complete() -> Result<Value> {
    let session = load_session()?;
    if is_session_usable(&session) {
        return Ok(session);
    }
    Err(anyhow!("browser_session_not_materialized"))
}

pub fn session_browser_playwright() -> Result<Value> {
    run_browser_login()
}

pub fn session_browser_reuse() -> Result<Value> {
    let session = load_session()?;
    if is_session_usable(&session) {
        return Ok(session);
    }
    run_browser_login()
}

pub fn fetch_snapshot() -> Result<Value> {
    let token = get_fresh_token()
        .map_err(|e| anyhow!("finary_snapshot_failed:{e}"))?;
    fetch_finary_snapshot_with_token(&token)
}

pub fn list_accounts() -> Result<Value> {
    let snapshot = fetch_snapshot()?;
    let accounts = snapshot
        .get("accountsPayload")
        .and_then(|v| v.get("result"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let summary: Vec<Value> = accounts
        .into_iter()
        .map(|acct| {
            json!({
                "id": acct.get("id").cloned().unwrap_or(Value::Null),
                "name": acct.get("name").cloned().unwrap_or(Value::Null),
                "slug": acct.get("slug").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let count = summary.len();
    Ok(json!({ "accounts": summary, "count": count }))
}

