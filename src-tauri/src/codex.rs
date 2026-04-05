//! Codex app-server client — JSON-RPC 2.0 over stdio.
//!
//! Manages a long-lived `codex app-server` child process for LLM generation.
//! Provides structured streaming, proper cancellation (`turn/interrupt`),
//! and reuses the user's Codex auth session (no OPENAI_API_KEY env var needed).
//!
//! Public API consumed by `llm.rs`:
//! - `run_codex_prompt_with_progress()` — same signature as the legacy proxy
//! - `kill_all_active()` — sends `turn/interrupt` instead of process kill
//! - `ensure_codex_available()` — checks binary + app-server capability
//! - `stop_app_server()` — clean shutdown on app exit

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

// ── Windows: hide child process console windows ──────────────────────
/// Apply CREATE_NO_WINDOW flag on Windows to prevent console flashing.
#[cfg(target_os = "windows")]
fn hide_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
}
#[cfg(not(target_os = "windows"))]
fn hide_console_window(_cmd: &mut Command) {}

// ── Binary resolution ──────────────────────────────────────────────

fn codex_install_dir() -> PathBuf {
    if let Ok(appdata) = env::var("APPDATA") {
        PathBuf::from(appdata).join("alfred").join("bin")
    } else {
        PathBuf::from("data").join("bin")
    }
}

/// Directory containing the bundled Node.js + codex shipped with the installer.
/// Located at `<exe_dir>/codex-runtime/` in production builds.
fn bundled_codex_dir() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let dir = exe_dir.join("codex-runtime");
    if dir.exists() { Some(dir) } else { None }
}

/// Prepare a Command for codex execution: hide console on Windows and
/// prepend the bundled codex-runtime dir + path/ subdir (rg.exe) to PATH.
fn prepare_codex_cmd(cmd: &mut Command) {
    if let Some(dir) = bundled_codex_dir() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let current = env::var("PATH").unwrap_or_default();
        let tools_path = dir.join("path");
        if tools_path.exists() {
            cmd.env("PATH", format!("{}{sep}{}{sep}{current}", dir.display(), tools_path.display()));
        } else {
            cmd.env("PATH", format!("{}{sep}{current}", dir.display()));
        }
    }
    hide_console_window(cmd);
}

fn resolve_codex_binary() -> Result<PathBuf> {
    // Explicit override
    if let Ok(path) = env::var("CODEX_PROXY_CLI_CMD") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    // 1. Check bundled codex-runtime (shipped with installer)
    //    Contains the native codex.exe directly (no node.js / cmd.exe wrapper).
    if let Some(bundle_dir) = bundled_codex_dir() {
        let candidates: &[&str] = if cfg!(windows) {
            &["codex.exe", "codex.cmd", "codex"]
        } else {
            &["codex"]
        };
        for name in candidates {
            let path = bundle_dir.join(name);
            if path.exists() {
                crate::debug_log(&format!("codex: using bundled binary {}", path.display()));
                return Ok(path);
            }
        }
    }

    // Fallback candidates for system-level installs (node.js-based)
    let system_cmd = if cfg!(windows) { "codex.cmd" } else { "codex" };

    // 2. Check legacy install dir (%APPDATA%/alfred/bin)
    let install_dir = codex_install_dir();
    for name in &[system_cmd, "codex.exe", "codex"] {
        let path = install_dir.join(name);
        if path.exists() {
            return Ok(path);
        }
    }

    // 3. Check system PATH
    let mut check = Command::new(system_cmd);
    check.arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
    hide_console_window(&mut check);
    if check.status().is_ok() {
        return Ok(PathBuf::from(system_cmd));
    }

    // 4. Fallback — try npm auto-install (requires Node.js on the system)
    match auto_install_codex(system_cmd) {
        Ok(path) => Ok(path),
        Err(e) => Err(anyhow!(
            "codex_not_found:codex-runtime bundle missing and npm auto-install failed: {e}. \
             Reinstall Alfred Desktop or install codex manually: npm install -g @openai/codex"
        )),
    }
}

fn auto_install_codex(system_cmd: &str) -> Result<PathBuf> {
    let install_dir = codex_install_dir();
    fs::create_dir_all(&install_dir)?;

    let npm_cmd = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let mut npm = Command::new(npm_cmd);
    npm.args(["install", "-g", "@openai/codex"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console_window(&mut npm);

    if let Ok(status) = npm.status() {
        if status.success() {
            let mut verify = Command::new(system_cmd);
            verify.arg("--version").stdout(Stdio::null()).stderr(Stdio::null());
            hide_console_window(&mut verify);
            if verify.status().is_ok() {
                return Ok(PathBuf::from(system_cmd));
            }
        }
    }

    Err(anyhow!("codex_auto_install_failed:npm_unavailable"))
}

/// Check if codex is available. Returns status for UI.
pub fn ensure_codex_available() -> Result<Value> {
    crate::debug_log("codex: ensure_codex_available called");
    let path = resolve_codex_binary()?;
    crate::debug_log(&format!("codex: found at {}", path.display()));
    let mut ver_cmd = Command::new(path.as_os_str());
    ver_cmd.arg("--version").stdout(Stdio::piped()).stderr(Stdio::piped());
    prepare_codex_cmd(&mut ver_cmd);
    let version = ver_cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // MCP registration deferred — happens lazily when the first analysis runs,
    // not during startup (avoid blocking the splash screen).

    Ok(json!({
        "ok": true,
        "path": path.display().to_string(),
        "version": version
    }))
}

/// Remove legacy [mcp_servers.alfred-mcp] from the user's global config.toml.
/// MCP config is now passed via -c flags at app-server spawn time (with auto_approve).
/// The global section (without auto_approve) would override the -c flags.
pub fn ensure_mcp_config() {
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| {
        let home = match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            Ok(h) => h,
            Err(_) => return,
        };
        let config_path = std::path::Path::new(&home).join(".codex").join("config.toml");
        let existing = match std::fs::read_to_string(&config_path) {
            Ok(s) if s.contains("[mcp_servers.alfred-mcp]") => s,
            _ => return,
        };

        // Strip the [mcp_servers.alfred-mcp] section
        let mut result = String::new();
        let mut skip = false;
        for line in existing.lines() {
            if line.trim() == "[mcp_servers.alfred-mcp]" {
                skip = true;
                continue;
            }
            if skip && line.starts_with('[') {
                skip = false;
            }
            if !skip {
                result.push_str(line);
                result.push('\n');
            }
        }

        match std::fs::write(&config_path, result.trim_end()) {
            Ok(_) => crate::debug_log("[mcp] removed legacy alfred-mcp section from global config"),
            Err(e) => crate::debug_log(&format!("[mcp] failed to clean global config: {e}")),
        }
    });
}

// ── JSON-RPC 2.0 types ───────────────────────────────────────────

/// A parsed JSON-RPC 2.0 message from the app-server.
#[derive(Debug)]
pub enum JsonRpcMessage {
    /// Response to a request we sent (has matching `id`).
    Response {
        id: u64,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// Server-initiated notification (no `id`).
    Notification { method: String, params: Value },
}

// ── AppServerClient ───────────────────────────────────────────────

/// Long-lived client managing a `codex app-server` child process.
/// Communicates via newline-delimited JSON-RPC 2.0 over stdio.
pub struct AppServerClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
    next_id: AtomicU64,
    initialized: bool,
    /// Active thread ID (set after `thread/start`).
    pub active_thread_id: Option<String>,
    /// Active turn ID (set after `turn/started` notification).
    pub active_turn_id: Option<String>,
    /// Best available model, resolved from model/list after init.
    pub best_model: Option<String>,
}

/// Pool of app-server processes for parallel line analysis.
/// Each worker thread takes a slot, uses it, returns it.
struct AppServerPool {
    slots: Vec<Mutex<Option<AppServerClient>>>,
    best_model: Mutex<Option<String>>,
}

static APP_SERVER_POOL: OnceLock<AppServerPool> = OnceLock::new();

fn pool_size() -> usize {
    crate::runtime_setting_integer_direct("line_analysis_concurrency", 2).clamp(1, 8) as usize
}

fn app_server_pool() -> &'static AppServerPool {
    APP_SERVER_POOL.get_or_init(|| {
        let n = pool_size();
        let slots = (0..n).map(|_| Mutex::new(None)).collect();
        AppServerPool {
            slots,
            best_model: Mutex::new(None),
        }
    })
}

impl AppServerClient {
    /// Spawn `codex app-server` and set up stdio pipes.
    /// When `ALFRED_MCP_ENABLED=1`, passes MCP server config so Codex
    /// can call alfred-mcp tools during turns.
    fn spawn() -> Result<Self> {
        let bin = resolve_codex_binary()?;
        let bin_str = bin.to_string_lossy().to_string();

        // Build MCP server config as -c flags so the app-server discovers
        // alfred-mcp tools without polluting the user's global config.toml.
        let self_binary = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let data_dir = crate::resolve_runtime_state_dir()
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());

        // On Windows, Rust's Command automatically wraps .cmd/.bat files with
        // cmd.exe /c using correct quoting. Manual cmd.exe wrapping with /s
        // strips quotes and breaks paths with spaces (e.g. "C:\Program Files\...").

        crate::debug_log(&format!("codex app-server: spawning {} app-server", bin_str));

        let mut cmd = Command::new(bin.as_os_str());
        cmd.arg("app-server");
        if !self_binary.is_empty() {
            // Use TOML single-quoted (literal) strings to avoid escaping issues
            // with backslashes and spaces in Windows paths.
            cmd.args(["-c", &format!("mcp_servers.alfred-mcp.command='{self_binary}'")]);
            cmd.args(["-c", &format!(
                "mcp_servers.alfred-mcp.args=['--mcp-server', '--data-dir', '{data_dir}']"
            )]);
            cmd.args(["-c", "mcp_servers.alfred-mcp.auto_approved=['*']"]);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        prepare_codex_cmd(&mut cmd);
        let mut child = cmd.spawn()
            .map_err(|e| anyhow!("codex_app_server_spawn_failed:{e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("codex_app_server_stdin_unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("codex_app_server_stdout_unavailable"))?;
        let stderr = child.stderr.take();

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            stderr,
            next_id: AtomicU64::new(0),
            initialized: false,
            active_thread_id: None,
            active_turn_id: None,
            best_model: None,
        })
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request (has `id`, expects a response). Returns the request ID.
    pub fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id();
        let msg = json!({"method": method, "id": id, "params": params});
        self.write_message(&msg)?;
        Ok(id)
    }

    /// Send a JSON-RPC notification (no `id`, no response expected).
    pub fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"method": method, "params": params});
        self.write_message(&msg)
    }

    fn write_message(&mut self, msg: &Value) -> Result<()> {
        let serialized = serde_json::to_string(msg)
            .map_err(|e| anyhow!("codex_app_server_serialize_failed:{e}"))?;
        crate::debug_log(&format!("codex app-server TX: {}", truncate(&serialized, 200)));
        self.stdin
            .write_all(serialized.as_bytes())
            .map_err(|e| anyhow!("codex_app_server_write_failed:{e}"))?;
        self.stdin
            .write_all(b"\n")
            .map_err(|e| anyhow!("codex_app_server_write_failed:{e}"))?;
        self.stdin
            .flush()
            .map_err(|e| anyhow!("codex_app_server_flush_failed:{e}"))?;
        Ok(())
    }

    /// Read one JSON-RPC message from stdout (blocking).
    pub fn recv(&mut self) -> Result<JsonRpcMessage> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .map_err(|e| anyhow!("codex_app_server_read_failed:{e}"))?;
        if bytes_read == 0 {
            // Capture stderr to understand why the process died
            if let Some(ref mut stderr) = self.stderr {
                let mut err = String::new();
                let _ = std::io::Read::read_to_string(stderr, &mut err);
                let err = err.trim();
                if !err.is_empty() {
                    crate::debug_log(&format!("codex app-server stderr: {}", truncate(err, 500)));
                }
            }
            return Err(anyhow!("codex_app_server_eof:process exited"));
        }
        let trimmed = line.trim();
        // Only log non-delta messages to avoid flooding debug.log
        if !trimmed.contains("agentMessage/delta") && !trimmed.contains("reasoning/text") {
            crate::debug_log(&format!("codex app-server RX: {}", truncate(trimmed, 200)));
        }

        let msg: Value = serde_json::from_str(trimmed)
            .map_err(|e| anyhow!("codex_app_server_parse_failed:{e}:line={}", truncate(trimmed, 100)))?;

        let has_method = msg.get("method").and_then(|v| v.as_str()).is_some();

        // Server request: has both "method" and "id" (e.g. elicitation).
        // Auto-approve alfred-mcp requests, deny others.
        if has_method {
            if let Some(id_val) = msg.get("id") {
                let req_id = id_val.as_u64().unwrap_or(0);
                let method = msg["method"].as_str().unwrap_or("");
                if method == "mcpServer/elicitation/request" {
                    let server = msg.pointer("/params/serverName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let action = if server == "alfred-mcp" { "accept" } else { "decline" };
                    crate::debug_log(&format!(
                        "codex app-server: {action} elicitation for '{server}' id={req_id}"
                    ));
                    let _ = self.write_message(&json!({
                        "id": req_id,
                        "result": { "action": action }
                    }));
                }
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                return Ok(JsonRpcMessage::Notification {
                    method: method.to_string(),
                    params,
                });
            }
        }

        // Response: has "id" but no "method"
        if let Some(id_val) = msg.get("id") {
            let id = id_val.as_u64().unwrap_or(0);
            return Ok(JsonRpcMessage::Response {
                id,
                result: msg.get("result").cloned(),
                error: msg.get("error").cloned(),
            });
        }

        // Pure notification: has "method" but no "id"
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        Ok(JsonRpcMessage::Notification { method, params })
    }

    /// Read messages until we get a Response with the given `id`.
    /// Notifications received along the way are passed to `on_notification`.
    pub fn recv_response(
        &mut self,
        expected_id: u64,
        mut on_notification: impl FnMut(&str, &Value),
    ) -> Result<Value> {
        loop {
            match self.recv()? {
                JsonRpcMessage::Response { id, result, error } if id == expected_id => {
                    if let Some(err) = error {
                        return Err(map_rpc_error(&err));
                    }
                    return Ok(result.unwrap_or(json!(null)));
                }
                JsonRpcMessage::Response { id, .. } => {
                    crate::debug_log(&format!(
                        "codex app-server: ignoring response id={id} (expected {expected_id})"
                    ));
                }
                JsonRpcMessage::Notification { method, params } => {
                    on_notification(&method, &params);
                }
            }
        }
    }

    /// Perform the initialize handshake (must be called once after spawn).
    pub fn initialize(&mut self) -> Result<Value> {
        if self.initialized {
            return Err(anyhow!("codex_app_server_already_initialized"));
        }

        let version = env!("CARGO_PKG_VERSION");
        let id = self.send_request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "alfred",
                    "title": "Alfred Desktop",
                    "version": version,
                }
            }),
        )?;

        let result = self.recv_response(id, |method, _params| {
            crate::debug_log(&format!("codex app-server: notification during init: {method}"));
        })?;

        self.send_notification("initialized", json!({}))?;
        self.initialized = true;
        crate::debug_log("codex app-server: initialized successfully");
        Ok(result)
    }

    /// Check if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the child process.
    pub fn stop(&mut self) {
        crate::debug_log("codex app-server: stopping");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AppServerClient {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Error mapping ─────────────────────────────────────────────────

/// Map a JSON-RPC error object to an anyhow error with codex_* prefix.
fn map_rpc_error(err: &Value) -> anyhow::Error {
    let message = err.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
    let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);

    if let Some(info) = err
        .get("codexErrorInfo")
        .or_else(|| err.get("additionalDetails").and_then(|d| d.get("codexErrorInfo")))
    {
        let error_type = info
            .as_str()
            .unwrap_or(info.get("type").and_then(|v| v.as_str()).unwrap_or("unknown"));
        match error_type {
            "UsageLimitExceeded" => return anyhow!("codex_rate_limited:{message}"),
            "HttpConnectionFailed" => {
                let status = info.get("httpStatusCode").and_then(|v| v.as_u64()).unwrap_or(0);
                if status == 429 {
                    return anyhow!("codex_rate_limited:http_429:{message}");
                }
                return anyhow!("codex_http_failed:status={status}:{message}");
            }
            "Unauthorized" => return anyhow!("codex_unauthorized:{message}"),
            "ContextWindowExceeded" => return anyhow!("codex_context_exceeded:{message}"),
            _ => {}
        }
    }

    anyhow!("codex_rpc_error:code={code}:{message}")
}

// ── Singleton public API ──────────────────────────────────────────

/// Ensure a specific pool slot has a running app-server.
fn ensure_slot(slot_idx: usize) -> Result<()> {
    let pool = app_server_pool();
    let mut guard = pool.slots[slot_idx]
        .lock()
        .map_err(|e| anyhow!("codex_app_server_lock_poisoned:{e}"))?;

    if let Some(ref mut client) = *guard {
        if client.is_alive() {
            return Ok(());
        }
        crate::debug_log(&format!("codex app-server[{slot_idx}]: process died, restarting"));
    }

    // Clean legacy global config before first spawn so it doesn't
    // override the -c flags (which include auto_approve).
    ensure_mcp_config();

    let mut client = AppServerClient::spawn()?;
    client.initialize()?;

    // Resolve best model once (shared across pool)
    {
        let mut best = pool.best_model.lock().unwrap_or_else(|e| e.into_inner());
        if best.is_none() {
            if let Ok(model) = resolve_best_model(&mut client) {
                crate::debug_log(&format!("codex app-server: best model = {model}"));
                *best = Some(model.clone());
                client.best_model = Some(model);
            }
        } else {
            client.best_model = best.clone();
        }
    }

    crate::debug_log(&format!("codex app-server[{slot_idx}]: ready"));
    *guard = Some(client);
    Ok(())
}

/// Initialize the first slot (used for session_status, login, etc.)
pub fn get_or_start_app_server() -> Result<()> {
    ensure_slot(0)
}

/// Stop all app-server processes in the pool.
pub fn stop_app_server() {
    let pool = app_server_pool();
    for (i, slot) in pool.slots.iter().enumerate() {
        if let Ok(mut guard) = slot.lock() {
            if let Some(ref mut client) = *guard {
                crate::debug_log(&format!("codex app-server[{i}]: stopping"));
                client.stop();
            }
            *guard = None;
        }
    }
    if let Ok(mut best) = pool.best_model.lock() {
        *best = None;
    }
}

/// Execute a closure with a pooled app-server client.
/// Picks the first available slot, starts it if needed.
/// Multiple threads can use different slots concurrently.
fn with_app_server<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut AppServerClient) -> Result<R>,
{
    let pool = app_server_pool();

    // Try to find a free slot (non-blocking trylock)
    for (i, slot) in pool.slots.iter().enumerate() {
        if let Ok(mut guard) = slot.try_lock() {
            // Got a slot — ensure it's running
            if guard.as_mut().map(|c| !c.is_alive()).unwrap_or(true) {
                drop(guard);
                ensure_slot(i)?;
                guard = slot.lock().map_err(|e| anyhow!("codex_lock:{e}"))?;
            }
            let client = guard.as_mut().ok_or_else(|| anyhow!("codex_app_server_not_running"))?;
            return f(client);
        }
    }

    // All slots busy — wait for slot 0 (fallback, blocks)
    ensure_slot(0)?;
    let mut guard = pool.slots[0]
        .lock()
        .map_err(|e| anyhow!("codex_app_server_lock_poisoned:{e}"))?;
    let client = guard
        .as_mut()
        .ok_or_else(|| anyhow!("codex_app_server_not_running"))?;
    f(client)
}

/// Query model/list and pick the best available model.
/// Preference order: explicit ALFRED_MODEL env > highest gpt-5.x > o3-mini > first available.
fn resolve_best_model(client: &mut AppServerClient) -> Result<String> {
    // Explicit override
    if let Ok(model) = env::var("ALFRED_MODEL") {
        let trimmed = model.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let id = client.send_request("model/list", json!({}))?;
    let resp = client.recv_response(id, |_, _| {})?;

    let models: Vec<String> = resp
        .get("data")
        .or_else(|| resp.get("models"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").or(m.get("model")).and_then(|v| v.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();

    if models.is_empty() {
        return Err(anyhow!("codex_no_models_available"));
    }

    // Prefer gpt-5.x models (highest version), then o3/o4, then anything
    let mut best: Option<&str> = None;
    let mut best_score: i32 = -1;

    for model in &models {
        let score = if model.starts_with("gpt-5") {
            // gpt-5.4 > gpt-5.3 > gpt-5 etc.
            let version: f32 = model
                .strip_prefix("gpt-")
                .and_then(|s| s.split('-').next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(5.0);
            (version * 10.0) as i32
        } else if model.starts_with("o4") {
            40
        } else if model.starts_with("o3") {
            30
        } else if model.starts_with("gpt-4") {
            20
        } else {
            0
        };
        if score > best_score {
            best_score = score;
            best = Some(model);
        }
    }

    Ok(best.unwrap_or(&models[0]).to_string())
}

// ── Prompt execution (public API — same signature as legacy) ──────

/// Progress callback: receives (bytes_received, line_count, latest_line).
pub type CodexProgressFn = Box<dyn Fn(usize, usize, &str) + Send>;

/// Test-only mock: when set, `run_codex_prompt_with_progress` calls this
/// instead of the real Codex app-server. Thread-safe static function pointer.
pub type CodexMockFn = fn(&str) -> Result<Value>;
static CODEX_MOCK: std::sync::OnceLock<std::sync::Mutex<Option<CodexMockFn>>> = std::sync::OnceLock::new();

/// Set a mock function for testing. Pass None to clear.
#[allow(dead_code)] // Called from tests, not from binary
pub fn set_codex_mock(mock: Option<CodexMockFn>) {
    let slot = CODEX_MOCK.get_or_init(|| std::sync::Mutex::new(None));
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = mock;
}

/// Run a prompt via the Codex app-server.
/// Creates a thread, starts a turn, streams agent messages, and returns the
/// accumulated JSON result. In tests, can be overridden via `set_codex_mock`.
pub fn run_codex_prompt_with_progress(
    prompt: &str,
    _timeout_ms: u64,
    on_progress: Option<CodexProgressFn>,
) -> Result<Value> {
    // Test mock hook
    if let Some(slot) = CODEX_MOCK.get() {
        if let Ok(guard) = slot.lock() {
            if let Some(mock_fn) = *guard {
                return mock_fn(prompt);
            }
        }
    }
    with_app_server(|client| {
        let model = client.best_model.clone().unwrap_or_else(|| "gpt-5.4".to_string());
        // 1. Start a new thread
        let thread_id = {
            let id = client.send_request(
                "thread/start",
                json!({
                    "model": model,
                    "approvalPolicy": "never",
                }),
            )?;
            let resp = client.recv_response(id, |_, _| {})?;
            let tid = resp
                .get("thread")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("codex_app_server_no_thread_id"))?
                .to_string();
            client.active_thread_id = Some(tid.clone());
            tid
        };

        // 2. Start a turn with the prompt
        let turn_req_id = client.send_request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
            }),
        )?;

        // 3. Stream notifications until turn/completed
        let mut agent_text = String::new();
        let mut bytes_received = 0usize;
        let mut phase = "thinking";
        let mut reasoning_count = 0u32;
        let mut search_count = 0u32;

        loop {
            let msg = client.recv()?;
            match msg {
                JsonRpcMessage::Response { id, result, error } if id == turn_req_id => {
                    if let Some(err) = error {
                        client.active_turn_id = None;
                        return Err(map_rpc_error(&err));
                    }
                    if let Some(tid) = result
                        .as_ref()
                        .and_then(|r| r.get("turn"))
                        .and_then(|t| t.get("id"))
                        .and_then(|v| v.as_str())
                    {
                        client.active_turn_id = Some(tid.to_string());
                    }
                }
                JsonRpcMessage::Response { .. } => {}
                JsonRpcMessage::Notification { ref method, ref params } => {
                    match method.as_str() {
                        "item/agentMessage/delta" => {
                            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                                agent_text.push_str(delta);
                                bytes_received += delta.len();
                                if phase != "writing" {
                                    phase = "writing";
                                    if let Some(ref cb) = on_progress {
                                        cb(bytes_received, 0, "writing recommendation\u{2026}");
                                    }
                                } else if bytes_received % 800 < delta.len() {
                                    if let Some(ref cb) = on_progress {
                                        // Show a preview of the last ~50 chars being generated
                                        let preview: String = agent_text.chars().rev().take(60).collect::<String>().chars().rev().collect();
                                        let clean = preview.trim().replace('\n', " ");
                                        // Take last 50 chars safely (chars, not bytes)
                                        let display: String = clean.chars().rev().take(50).collect::<String>().chars().rev().collect();
                                        if display.chars().count() > 10 {
                                            cb(bytes_received, 0, &format!("\u{2026}{display}"));
                                        } else {
                                            let kb = bytes_received as f64 / 1024.0;
                                            cb(bytes_received, 0, &format!("writing ({kb:.1}kB)\u{2026}"));
                                        }
                                    }
                                }
                            }
                        }
                        "item/started" => {
                            let item_type = params
                                .get("item")
                                .and_then(|i| i.get("type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let label = match item_type {
                                "reasoning" => {
                                    reasoning_count += 1;
                                    match reasoning_count {
                                        1 => "analyzing data\u{2026}".to_string(),
                                        2 => "evaluating fundamentals\u{2026}".to_string(),
                                        3 => "assessing risks\u{2026}".to_string(),
                                        _ => format!("refining analysis (step {reasoning_count})\u{2026}"),
                                    }
                                }
                                "webSearch" => {
                                    search_count += 1;
                                    let query = params
                                        .get("item")
                                        .and_then(|i| i.get("query"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if query.is_empty() {
                                        format!("web search ({search_count})\u{2026}")
                                    } else {
                                        format!("searching: {}\u{2026}", if query.len() > 40 { &query[..40] } else { query })
                                    }
                                }
                                // MCP tool calls — Codex calling our alfred-mcp tools
                                "mcpToolCall" | "functionCall" | "tool_use" => {
                                    let tool_name = params
                                        .get("item")
                                        .and_then(|i| i.get("name").or_else(|| i.get("toolName")))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("tool");
                                    format!("calling {tool_name}\u{2026}")
                                }
                                "agentMessage" => String::new(), // handled by delta
                                _ => String::new(),
                            };
                            if !label.is_empty() {
                                phase = "active";
                                if let Some(ref cb) = on_progress {
                                    cb(bytes_received, 0, &label);
                                }
                            }
                        }
                        "turn/started" => {
                            if let Some(tid) = params
                                .get("turn")
                                .and_then(|t| t.get("id"))
                                .and_then(|v| v.as_str())
                            {
                                client.active_turn_id = Some(tid.to_string());
                            }
                            if let Some(ref cb) = on_progress {
                                cb(0, 0, "thinking\u{2026}");
                            }
                        }
                        "turn/completed" => {
                            client.active_turn_id = None;
                            let status = params
                                .get("turn")
                                .and_then(|t| t.get("status"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            if status == "failed" {
                                let error_info = params
                                    .get("turn")
                                    .and_then(|t| t.get("error"))
                                    .cloned()
                                    .unwrap_or(json!({"message": "turn failed"}));
                                return Err(map_rpc_error(&error_info));
                            }
                            if status == "interrupted" {
                                return Err(anyhow!("codex_child_killed:process was cancelled"));
                            }
                            break;
                        }
                        "item/completed" => {
                            if let Some("agentMessage") =
                                params.get("item").and_then(|i| i.get("type")).and_then(|v| v.as_str())
                            {
                                if let Some(text) =
                                    params.get("item").and_then(|i| i.get("text")).and_then(|v| v.as_str())
                                {
                                    if agent_text.is_empty() {
                                        agent_text = text.to_string();
                                    }
                                }
                            }
                        }
                        "turn/plan/updated" => {
                            if let Some(explanation) = params.get("explanation").and_then(|v| v.as_str()) {
                                if let Some(ref cb) = on_progress {
                                    let short = if explanation.len() > 60 { &explanation[..60] } else { explanation };
                                    cb(bytes_received, 0, &format!("planning: {short}\u{2026}"));
                                }
                            }
                        }
                        "thread/tokenUsage/updated" => {
                            if let Some(ref cb) = on_progress {
                                let total = params.get("tokenUsage")
                                    .and_then(|u| u.get("total"))
                                    .and_then(|t| t.get("totalTokens"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let input = params.get("tokenUsage")
                                    .and_then(|u| u.get("total"))
                                    .and_then(|t| t.get("inputTokens"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                let output = params.get("tokenUsage")
                                    .and_then(|u| u.get("total"))
                                    .and_then(|t| t.get("outputTokens"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if total > 0 {
                                    cb(bytes_received, 0, &format!("tokens:{total}:{input}:{output}"));
                                }
                            }
                        }
                        "account/rateLimits/updated" => {
                            if let Some(ref cb) = on_progress {
                                let used_pct = params.get("rateLimits")
                                    .and_then(|r| r.get("primary"))
                                    .and_then(|p| p.get("usedPercent"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0);
                                if used_pct > 0 {
                                    cb(bytes_received, 0, &format!("rate_limit:{used_pct}%"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // 4. Extract JSON from accumulated agent text.
        // In MCP mode, Codex produces commentary text + tool calls, not JSON.
        // If no JSON found but the turn completed, return a success marker.
        match extract_json_from_output(&agent_text) {
            Some(json) => Ok(json),
            None if agent_text.is_empty() => Ok(json!({"ok": true, "mcp_turn": true})),
            None => Ok(json!({"ok": true, "mcp_turn": true, "agent_text_chars": agent_text.len()})),
        }
    })
}

/// Interrupt all active turns across all pool slots.
/// Called on analysis cancellation.
pub fn kill_all_active() {
    let pool = app_server_pool();
    for (i, slot) in pool.slots.iter().enumerate() {
        if let Ok(mut guard) = slot.try_lock() {
            if let Some(ref mut client) = *guard {
                let thread_id = client.active_thread_id.clone();
                let turn_id = client.active_turn_id.clone();
                if let (Some(tid), Some(tuid)) = (thread_id, turn_id) {
                    crate::debug_log(&format!("codex app-server[{i}]: interrupting turn {tuid}"));
                    let _ = client.send_request(
                        "turn/interrupt",
                        json!({"threadId": tid, "turnId": tuid}),
                    );
                    client.active_turn_id = None;
                }
            }
        }
    }
}

// ── Session management (OpenAI auth) ──────────────────────────────

/// Check if the user has a valid Codex/OpenAI session.
/// Tries to start the app-server and run `model/list` — if auth fails, returns
/// a status indicating login is required.
pub fn session_status() -> Result<Value> {
    // First check: can we even find the binary?
    let binary_ok = resolve_codex_binary().is_ok();
    if !binary_ok {
        return Ok(json!({
            "status": "no_binary",
            "logged_in": false,
            "message": "Codex CLI not found. Reinstall Alfred Desktop or install manually: npm install -g @openai/codex"
        }));
    }

    // Try to start app-server + initialize (this validates auth)
    match get_or_start_app_server() {
        Ok(()) => {
            // App-server started — try model/list to confirm auth is valid
            let model_check = with_app_server(|client| {
                let id = client.send_request("model/list", json!({}))?;
                client.recv_response(id, |_, _| {})
            });
            match model_check {
                Ok(models) => {
                    let model_count = models
                        .get("models")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    Ok(json!({
                        "status": "connected",
                        "logged_in": true,
                        "models_available": model_count
                    }))
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("unauthorized") || err_str.contains("Unauthorized") {
                        Ok(json!({
                            "status": "requires_login",
                            "logged_in": false,
                            "message": "OpenAI session expired or not found. Please log in."
                        }))
                    } else {
                        Ok(json!({
                            "status": "error",
                            "logged_in": false,
                            "message": err_str
                        }))
                    }
                }
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            Ok(json!({
                "status": "error",
                "logged_in": false,
                "message": err_str
            }))
        }
    }
}

/// Run a codex subcommand (login/logout), restart the app-server afterward,
/// and return a JSON status result.
fn run_codex_session_cmd(subcmd: &str, ok_status: &str) -> Result<Value> {
    let bin = resolve_codex_binary()?;
    crate::debug_log(&format!("codex: launching 'codex {subcmd}'"));

    let mut cmd = Command::new(bin.as_os_str());
    cmd.arg(subcmd).stdout(Stdio::piped()).stderr(Stdio::piped());
    prepare_codex_cmd(&mut cmd);
    let output = cmd.output()
        .map_err(|e| anyhow!("codex_{subcmd}_spawn_failed:{e}"))?;

    // Always restart the app-server so it picks up the new auth state
    stop_app_server();

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        crate::debug_log(&format!("codex: {subcmd} completed successfully"));
        Ok(json!({
            "ok": true,
            "status": ok_status,
            "message": stdout.trim()
        }))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(anyhow!(
            "codex_{subcmd}_failed:exit_code={:?}:stdout={}:stderr={}",
            output.status.code(),
            truncate(&stdout, 200),
            truncate(&stderr, 200)
        ))
    }
}

/// Launch `codex login` in the user's default browser.
pub fn session_login() -> Result<Value> {
    run_codex_session_cmd("login", "logged_in")
}

/// Log out of the current Codex/OpenAI session.
pub fn session_logout() -> Result<Value> {
    run_codex_session_cmd("logout", "logged_out")
}

// ── JSON extraction (safety net for agent output) ─────────────────

fn extract_json_from_output(text: &str) -> Option<Value> {
    let trimmed = text.trim();

    // Direct parse
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() || value.is_array() {
            return Some(value);
        }
    }

    // Markdown fences
    for fence in ["```json", "```"] {
        if let Some(start) = trimmed.find(fence) {
            let content_start = start + fence.len();
            if let Some(end) = trimmed[content_start..].find("```") {
                let candidate = trimmed[content_start..content_start + end].trim();
                if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                    return Some(value);
                }
            }
        }
    }

    // Brace matching
    let first_brace = trimmed.find('{')?;
    let last_brace = trimmed.rfind('}')?;
    if first_brace < last_brace {
        let candidate = &trimmed[first_brace..=last_brace];
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            return Some(value);
        }
        // Try fixing common JSON issues: trailing commas, single quotes
        let cleaned = fix_common_json_issues(candidate);
        if let Ok(value) = serde_json::from_str::<Value>(&cleaned) {
            return Some(value);
        }
    }

    // Log the rejected text for diagnostics
    let preview: String = trimmed.chars().take(500).collect();
    eprintln!("[codex] JSON extraction failed ({} chars). Preview: {preview}", trimmed.len());

    None
}

/// Fix common LLM JSON output issues: trailing commas, single-line comments.
fn fix_common_json_issues(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escape_next = false;
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        let c = chars[i];
        if escape_next {
            result.push(c);
            escape_next = false;
            i += 1;
            continue;
        }
        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            result.push(c);
            i += 1;
            continue;
        }
        if in_string {
            result.push(c);
            i += 1;
            continue;
        }
        // Skip single-line comments
        if c == '/' && i + 1 < len && chars[i + 1] == '/' {
            while i < len && chars[i] != '\n' { i += 1; }
            continue;
        }
        // Remove trailing commas before } or ]
        if c == ',' {
            let rest = &raw[i + 1..];
            let next_non_ws = rest.trim_start();
            if next_non_ws.starts_with('}') || next_non_ws.starts_with(']') {
                i += 1;
                continue; // skip the trailing comma
            }
        }
        result.push(c);
        i += 1;
    }
    result
}

fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        // Find a valid char boundary at or before max_len
        let mut end = max_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &text[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_from_clean_output() {
        let output = r#"{"ok": true, "result": "hello"}"#;
        let result = extract_json_from_output(output).unwrap();
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn extract_json_from_mixed_output() {
        let output = "Some preamble text\n{\"ok\": true}\nSome trailing text";
        let result = extract_json_from_output(output).unwrap();
        assert_eq!(result["ok"], true);
    }

    #[test]
    fn extract_json_from_markdown_fence() {
        let output = "Here is the result:\n```json\n{\"draft\": \"test\"}\n```\nDone.";
        let result = extract_json_from_output(output).unwrap();
        assert_eq!(result["draft"], "test");
    }

    #[test]
    fn extract_json_returns_none_for_no_json() {
        assert!(extract_json_from_output("no json here").is_none());
    }

    #[test]
    fn map_rpc_error_rate_limited() {
        let err = json!({
            "code": -1,
            "message": "rate limited",
            "codexErrorInfo": "UsageLimitExceeded"
        });
        let e = map_rpc_error(&err);
        assert!(e.to_string().contains("codex_rate_limited"));
    }

    #[test]
    fn map_rpc_error_unauthorized() {
        let err = json!({
            "code": 401,
            "message": "invalid token",
            "codexErrorInfo": "Unauthorized"
        });
        let e = map_rpc_error(&err);
        assert!(e.to_string().contains("codex_unauthorized"));
    }

    #[test]
    fn map_rpc_error_generic() {
        let err = json!({"code": -32600, "message": "invalid request"});
        let e = map_rpc_error(&err);
        assert!(e.to_string().contains("codex_rpc_error"));
        assert!(e.to_string().contains("-32600"));
    }
}
