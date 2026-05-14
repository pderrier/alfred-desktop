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
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = env::var("APPDATA") {
        return PathBuf::from(appdata).join("alfred").join("bin");
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join("Library/Application Support/alfred/bin");
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".alfred/bin");
    }
    PathBuf::from("data").join("bin")
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
        Self::spawn_with_tools(None)
    }

    fn spawn_with_tools(tool_filter: Option<&[&str]>) -> Result<Self> {
        let bin = resolve_codex_binary()?;
        let bin_str = bin.to_string_lossy().to_string();

        let self_binary = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let data_dir = crate::resolve_runtime_state_dir()
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());

        crate::debug_log(&format!("codex app-server: spawning {} app-server", bin_str));

        let mut cmd = Command::new(bin.as_os_str());
        cmd.arg("app-server");
        if !self_binary.is_empty() {
            // Use TOML single-quoted (literal) strings to avoid escaping issues
            // with backslashes and spaces in Windows paths.
            let tools_arg = if let Some(filter) = tool_filter {
                format!("'--mcp-server', '--data-dir', '{data_dir}', '--tools', '{}'",
                    filter.join(","))
            } else {
                format!("'--mcp-server', '--data-dir', '{data_dir}'")
            };
            cmd.args(["-c", &format!("mcp_servers.alfred-mcp.command='{self_binary}'")]);
            cmd.args(["-c", &format!("mcp_servers.alfred-mcp.args=[{tools_arg}]")]);
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

    /// Spawn an app-server with NO MCP tools configured.
    /// Used by native-oauth mode where Rust handles tool calls directly
    /// and the app-server is just an OAuth-authenticated LLM proxy.
    fn spawn_no_tools() -> Result<Self> {
        let bin = resolve_codex_binary()?;
        let bin_str = bin.to_string_lossy().to_string();

        crate::debug_log(&format!("codex app-server (no-tools): spawning {} app-server", bin_str));

        let mut cmd = Command::new(bin.as_os_str());
        cmd.arg("app-server");
        // No -c flags for MCP servers — pure text proxy
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
                                        format!("searching: {}\u{2026}", { let mut e = 40.min(query.len()); while !query.is_char_boundary(e) { e -= 1; } &query[..e] })
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
                                    // Keep the last (longest) agent message — the final answer
                                    if text.len() > agent_text.len() {
                                        agent_text = text.to_string();
                                    }
                                }
                            }
                        }
                        "turn/plan/updated" => {
                            if let Some(explanation) = params.get("explanation").and_then(|v| v.as_str()) {
                                if let Some(ref cb) = on_progress {
                                    let short = if explanation.len() > 60 { let mut e = 60; while !explanation.is_char_boundary(e) { e -= 1; } &explanation[..e] } else { explanation };
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
            None => Ok(json!({"ok": true, "mcp_turn": true, "agent_text": agent_text})),
        }
    })
}

/// Run a simple text-in/text-out prompt through the Codex app-server.
/// No MCP tools configured — the app-server acts as a dumb LLM proxy.
/// Used by native-oauth mode: Rust orchestrates tool calls, the app-server
/// only provides OAuth-authenticated LLM generation.
pub fn run_simple_prompt(
    prompt: &str,
    on_progress: Option<CodexProgressFn>,
) -> Result<String> {
    // Test mock hook
    if let Some(slot) = CODEX_MOCK.get() {
        if let Ok(guard) = slot.lock() {
            if let Some(mock_fn) = *guard {
                let result = mock_fn(prompt)?;
                return Ok(result.as_str().unwrap_or("").to_string());
            }
        }
    }

    // Spawn a dedicated app-server with NO MCP tools.
    let mut client = AppServerClient::spawn_no_tools()?;
    client.initialize()?;
    if let Ok(model) = resolve_best_model(&mut client) {
        client.best_model = Some(model);
    }

    let model = client.best_model.clone().unwrap_or_else(|| "gpt-5.4".to_string());
    let id = client.send_request(
        "thread/start",
        json!({"model": model, "approvalPolicy": "never"}),
    )?;
    let resp = client.recv_response(id, |_, _| {})?;
    let thread_id = resp
        .get("thread")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("codex_simple_no_thread_id"))?
        .to_string();
    client.active_thread_id = Some(thread_id.clone());

    let turn_req_id = client.send_request(
        "turn/start",
        json!({
            "threadId": thread_id,
            "input": [{"type": "text", "text": prompt}],
        }),
    )?;

    let mut agent_text = String::new();
    let mut bytes_received = 0usize;
    loop {
        let msg = client.recv()?;
        match msg {
            JsonRpcMessage::Response { id, error, .. } if id == turn_req_id => {
                if let Some(err) = error {
                    return Err(map_rpc_error(&err));
                }
            }
            JsonRpcMessage::Response { .. } => {}
            JsonRpcMessage::Notification { ref method, ref params } => {
                match method.as_str() {
                    "item/agentMessage/delta" => {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            agent_text.push_str(delta);
                            bytes_received += delta.len();
                            if bytes_received % 800 < delta.len() {
                                if let Some(ref cb) = on_progress {
                                    let kb = bytes_received as f64 / 1024.0;
                                    cb(bytes_received, 0, &format!("writing ({kb:.1}kB)\u{2026}"));
                                }
                            }
                        }
                    }
                    "item/completed" => {
                        if let Some("agentMessage") =
                            params.get("item").and_then(|i| i.get("type")).and_then(|v| v.as_str())
                        {
                            if let Some(text) =
                                params.get("item").and_then(|i| i.get("text")).and_then(|v| v.as_str())
                            {
                                if text.len() > agent_text.len() {
                                    agent_text = text.to_string();
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
                        if let Some(ref cb) = on_progress {
                            let label = match item_type {
                                "reasoning" => "thinking\u{2026}".to_string(),
                                _ => String::new(),
                            };
                            if !label.is_empty() {
                                cb(bytes_received, 0, &label);
                            }
                        }
                    }
                    "turn/completed" => {
                        let status = params
                            .get("turn")
                            .and_then(|t| t.get("status"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        if status == "failed" {
                            let err = params
                                .get("turn")
                                .and_then(|t| t.get("error"))
                                .cloned()
                                .unwrap_or(json!({"message": "simple prompt turn failed"}));
                            return Err(map_rpc_error(&err));
                        }
                        if status == "interrupted" {
                            return Err(anyhow!("codex_child_killed:process was cancelled"));
                        }
                        break;
                    }
                    "thread/tokenUsage/updated" => {
                        if let Some(ref cb) = on_progress {
                            let total = params.pointer("/tokenUsage/total/totalTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let input = params.pointer("/tokenUsage/total/inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let output = params.pointer("/tokenUsage/total/outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            if total > 0 {
                                cb(bytes_received, 0, &format!("tokens:{total}:{input}:{output}"));
                            }
                        }
                    }
                    "account/rateLimits/updated" => {
                        if let Some(ref cb) = on_progress {
                            let used_pct = params.pointer("/rateLimits/primary/usedPercent").and_then(|v| v.as_u64()).unwrap_or(0);
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

    crate::debug_log(&format!("codex simple_prompt: completed, {} chars", agent_text.len()));
    Ok(agent_text)
}

/// Run a synthesis prompt with a dedicated app-server that only exposes
/// the 4 synthesis tools (get_run_context, check_coverage, validate_synthesis,
/// finalize_report). Prevents the model from re-analyzing individual lines.
pub fn run_synthesis_prompt(
    prompt: &str,
    on_progress: Option<CodexProgressFn>,
) -> Result<Value> {
    const SYNTHESIS_TOOLS: &[&str] = &[
        "get_run_context", "check_coverage", "validate_synthesis", "finalize_report",
    ];
    let mut client = AppServerClient::spawn_with_tools(Some(SYNTHESIS_TOOLS))?;
    client.initialize()?;
    if let Ok(model) = resolve_best_model(&mut client) {
        client.best_model = Some(model);
    }

    let model = client.best_model.clone().unwrap_or_else(|| "gpt-5.4".to_string());
    let id = client.send_request("thread/start", json!({"model": model, "approvalPolicy": "never"}))?;
    let resp = client.recv_response(id, |_, _| {})?;
    let thread_id = resp.get("thread").and_then(|t| t.get("id")).and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("codex_synthesis_no_thread_id"))?.to_string();
    client.active_thread_id = Some(thread_id.clone());

    let turn_req_id = client.send_request("turn/start", json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": prompt}],
    }))?;

    let mut agent_text = String::new();
    let mut bytes_received = 0usize;
    loop {
        let msg = client.recv()?;
        match msg {
            JsonRpcMessage::Response { id, .. } if id == turn_req_id => {}
            JsonRpcMessage::Response { .. } => {}
            JsonRpcMessage::Notification { ref method, ref params } => {
                match method.as_str() {
                    "item/agentMessage/delta" => {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            agent_text.push_str(delta);
                            bytes_received += delta.len();
                            if let Some(ref cb) = on_progress {
                                cb(bytes_received, 0, "writing recommendation\u{2026}");
                            }
                        }
                    }
                    "item/completed" => {
                        if let Some("agentMessage") = params.get("item").and_then(|i| i.get("type")).and_then(|v| v.as_str()) {
                            if let Some(text) = params.get("item").and_then(|i| i.get("text")).and_then(|v| v.as_str()) {
                                if text.len() > agent_text.len() { agent_text = text.to_string(); }
                            }
                        }
                    }
                    "item/started" => {
                        let item_type = params.get("item").and_then(|i| i.get("type")).and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(ref cb) = on_progress {
                            let label = match item_type {
                                "reasoning" => "analyzing data\u{2026}".to_string(),
                                "mcpToolCall" => {
                                    let tool = params.get("item").and_then(|i| i.get("tool")).and_then(|v| v.as_str()).unwrap_or("tool");
                                    format!("calling {tool}\u{2026}")
                                }
                                _ => String::new(),
                            };
                            if !label.is_empty() { cb(bytes_received, 0, &label); }
                        }
                    }
                    "turn/completed" => {
                        let status = params.get("turn").and_then(|t| t.get("status")).and_then(|v| v.as_str()).unwrap_or("unknown");
                        if status == "failed" {
                            let err = params.get("turn").and_then(|t| t.get("error")).cloned().unwrap_or(json!({"message": "synthesis turn failed"}));
                            return Err(map_rpc_error(&err));
                        }
                        break;
                    }
                    "thread/tokenUsage/updated" => {
                        if let Some(ref cb) = on_progress {
                            let total = params.pointer("/tokenUsage/total/totalTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let input = params.pointer("/tokenUsage/total/inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            let output = params.pointer("/tokenUsage/total/outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                            cb(bytes_received, 0, &format!("tokens:{total}:{input}:{output}"));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    match extract_json_from_output(&agent_text) {
        Some(json) => Ok(json),
        None if agent_text.is_empty() => Ok(json!({"ok": true, "mcp_turn": true})),
        None => Ok(json!({"ok": true, "mcp_turn": true, "agent_text": agent_text})),
    }
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

/// Classify a stringified codex error into one of the structured
/// `failure_reason` values surfaced to the JS bootstrap layer:
/// - `rate_limited`: OAuth quota exhausted (UsageLimitExceeded or HTTP 429)
/// - `auth`: token invalid/expired, login required
/// - `network`: connection / transport error, retryable
/// - `null` (None): unknown / generic error
///
/// Match only specific known prefixes (`codex_*:`), a single well-known
/// reqwest/tokio variant (`HttpConnectionFailed`), and a small set of exact
/// codex CLI stderr strings emitted by `codex exec` on quota exhaustion or
/// login failure. Loose substrings like "connection" or "network" are NOT
/// matched — they appear in unrelated user-facing messages (e.g. an account
/// name containing "Connection") and would mis-classify generic errors as
/// transient transport failures.
fn classify_session_failure(err_str: &str) -> Option<&'static str> {
    if err_str.contains("codex_rate_limited") {
        return Some("rate_limited");
    }
    if err_str.contains("codex_unauthorized") {
        return Some("auth");
    }
    if err_str.contains("codex_http_failed:")
        || err_str.contains("codex_network:")
        || err_str.contains("HttpConnectionFailed")
    {
        return Some("network");
    }

    // codex CLI stderr signals from `codex exec` — match exact phrases the
    // CLI prints when the ChatGPT-subscription OAuth quota is exhausted.
    // Keep this list tight so it can't fire on unrelated text.
    if err_str.contains("You've hit your usage limit")
        || err_str.contains("usage limit reached")
        || err_str.contains("Upgrade to Pro")
        || err_str.contains("try again at ")
        || err_str.contains("Rate limit exceeded")
        || err_str.contains("429 Too Many Requests")
    {
        return Some("rate_limited");
    }

    // codex CLI auth-required stderr signals
    if err_str.contains("Please run `codex login`")
        || err_str.contains("not logged in")
        || err_str.contains("authentication required")
        || err_str.contains("401 Unauthorized")
    {
        return Some("auth");
    }

    None
}

// ── Internal codex auth fallback (chatgpt OAuth → apikey) ────────────
//
// Self-healing path for the legacy `codex` mode: when the user's ChatGPT
// subscription quota is exhausted we keep using the same codex pipeline
// (prompts, tools, MCP integration) but swap the codex CLI's stored auth
// from `chatgpt` (OAuth tokens) to `apikey` (OpenAI API key). The auth
// file ($HOME/.codex/auth.json) is backed up to `auth.json.oauth.bak`
// so the OAuth credentials can be restored later.

/// Resolve the codex CLI's auth file path: `$HOME/.codex/auth.json`.
/// Returns an error if neither HOME nor USERPROFILE is set.
fn codex_auth_path() -> Result<PathBuf> {
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .map_err(|_| anyhow!("codex_auth_no_home"))?;
    Ok(PathBuf::from(home).join(".codex").join("auth.json"))
}

/// Backup path used to preserve the chatgpt OAuth auth when we swap to apikey.
fn codex_auth_backup_path() -> Result<PathBuf> {
    Ok(codex_auth_path()?.with_extension("json.oauth.bak"))
}

/// Report whether an OAuth backup exists at `~/.codex/auth.json.oauth.bak`.
///
/// Used by the v0.2.16 OAuth-availability proposal banner: if a user is on
/// codex+apikey *without* the auto-fallback flag set (i.e. they swapped
/// manually or carried over from an earlier app version), we can only
/// propose a "restore to OAuth" when this backup is present. Without it,
/// restoring would require a fresh device-auth flow which is out of scope
/// for the non-blocking splash banner.
///
/// Cheap: `Path::exists` only — no read, no parse.
pub fn has_oauth_backup() -> bool {
    match codex_auth_backup_path() {
        Ok(path) => path.exists(),
        Err(_) => false,
    }
}

/// Read codex CLI auth mode from `auth.json`.
/// Returns `"chatgpt"` when OAuth tokens are stored, `"apikey"` when a raw
/// `OPENAI_API_KEY` is stored, or `"none"` when no usable credential exists.
pub fn auth_mode() -> Result<&'static str> {
    let path = codex_auth_path()?;
    if !path.exists() {
        return Ok("none");
    }
    let content = fs::read_to_string(&path)
        .map_err(|e| anyhow!("codex_auth_read_failed:{e}"))?;
    if content.trim().is_empty() {
        return Ok("none");
    }
    let parsed: Value = serde_json::from_str(&content)
        .map_err(|e| anyhow!("codex_auth_parse_failed:{e}"))?;

    // chatgpt OAuth: presence of a `tokens` object with an access_token
    if let Some(tokens) = parsed.get("tokens").and_then(|v| v.as_object()) {
        let has_access = tokens
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let has_id = tokens
            .get("id_token")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if has_access || has_id {
            return Ok("chatgpt");
        }
    }

    // apikey mode: codex CLI writes the key at the top level as `OPENAI_API_KEY`
    let has_apikey = parsed
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if has_apikey {
        return Ok("apikey");
    }

    Ok("none")
}

/// Probe the codex CLI quota by running a tiny `codex exec` invocation.
/// Returns `{ status, failure_reason, message }` mirroring `session_status()`
/// so the JS layer can reuse the same decision logic.
///
/// Uses a fresh subprocess (no shared app-server pool) and a 30s timeout.
pub fn probe_quota() -> Result<Value> {
    let bin = match resolve_codex_binary() {
        Ok(p) => p,
        Err(e) => {
            return Ok(json!({
                "status": "no_binary",
                "failure_reason": serde_json::Value::Null,
                "message": format!("Codex CLI not found: {e}"),
            }));
        }
    };

    crate::debug_log("codex: probe_quota launching 'codex exec' (1-token probe)");

    let mut cmd = Command::new(bin.as_os_str());
    // `codex exec <prompt>` is a one-shot non-interactive invocation that
    // exercises the same auth/quota path used by the app-server.
    cmd.args(["exec", "--skip-git-repo-check", "OK"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    prepare_codex_cmd(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("codex_probe_spawn_failed:{e}"))?;

    // Manual 30s wait — std::process::Child has no built-in timeout.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!("codex_probe_wait_failed:{e}"));
            }
        }
    };

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut out, &mut stdout_buf);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut err, &mut stderr_buf);
    }

    match exit_status {
        None => Ok(json!({
            "status": "error",
            "failure_reason": "network",
            "message": "codex exec probe timed out after 30s",
        })),
        Some(status) if status.success() => Ok(json!({
            "status": "ok",
            "failure_reason": serde_json::Value::Null,
            "message": "quota_ok",
        })),
        Some(_) => {
            let combined = format!("{stdout_buf}\n{stderr_buf}");
            let failure_reason = classify_session_failure(&combined);
            let status = match failure_reason {
                Some("rate_limited") => "rate_limited",
                Some("auth") => "auth",
                Some(_) => "error",
                None => "unknown",
            };
            let message = match failure_reason {
                Some("rate_limited") => "OAuth quota exhausted.".to_string(),
                Some("auth") => "codex login required.".to_string(),
                _ => truncate(combined.trim(), 400),
            };
            Ok(json!({
                "status": status,
                "failure_reason": failure_reason,
                "message": message,
            }))
        }
    }
}

/// Swap the codex CLI auth from chatgpt OAuth to a user-provided API key.
///
/// Workflow:
/// 1. If current auth_mode is `chatgpt`, copy `auth.json` to
///    `auth.json.oauth.bak` (idempotent — we never overwrite an existing
///    backup with apikey state, so it always points at the OAuth snapshot).
/// 2. Pipe the API key into `codex login --with-api-key`, which rewrites
///    `auth.json` with `{ OPENAI_API_KEY: "…" }`.
/// 3. Stop the app-server pool so subsequent calls re-spawn with new auth.
pub fn swap_to_apikey(api_key: &str) -> Result<()> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err(anyhow!("codex_swap_empty_api_key"));
    }

    let auth_path = codex_auth_path()?;
    let backup_path = codex_auth_backup_path()?;
    let current_mode = auth_mode().unwrap_or("none");

    // Idempotent backup: only preserve when we currently have OAuth state
    // AND no backup yet exists. This prevents clobbering the OAuth snapshot
    // with a stale apikey one if swap_to_apikey is called twice.
    if current_mode == "chatgpt" && !backup_path.exists() && auth_path.exists() {
        fs::copy(&auth_path, &backup_path)
            .map_err(|e| anyhow!("codex_auth_backup_failed:{e}"))?;
        crate::debug_log(&format!(
            "codex: backed up OAuth auth -> {}",
            backup_path.display()
        ));
    }

    let bin = resolve_codex_binary()?;
    crate::debug_log("codex: invoking 'codex login --with-api-key'");

    let mut cmd = Command::new(bin.as_os_str());
    cmd.args(["login", "--with-api-key"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    prepare_codex_cmd(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("codex_login_apikey_spawn_failed:{e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(key.as_bytes())
            .map_err(|e| anyhow!("codex_login_apikey_write_failed:{e}"))?;
        // Drop closes stdin, signaling EOF to the codex CLI.
    }

    let output = child
        .wait_with_output()
        .map_err(|e| anyhow!("codex_login_apikey_wait_failed:{e}"))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "codex_login_apikey_failed:exit_code={:?}:stdout={}:stderr={}",
            output.status.code(),
            truncate(stdout.trim(), 200),
            truncate(stderr.trim(), 200)
        ));
    }

    // Ensure subsequent JSON-RPC calls pick up the new auth.
    stop_app_server();
    crate::debug_log("codex: swap_to_apikey completed, app-server pool reset");
    Ok(())
}

/// Restore codex CLI auth from the OAuth backup written by `swap_to_apikey`.
/// Returns `Ok(true)` when the restore succeeded AND the codex CLI accepts
/// the restored auth (re-init handshake passes); returns `Ok(false)` when
/// no backup exists. Caller is expected to clear the
/// `codex_auth_auto_fallback` flag only when this returns `true`.
pub fn swap_to_oauth() -> Result<bool> {
    let auth_path = codex_auth_path()?;
    let backup_path = codex_auth_backup_path()?;

    if !backup_path.exists() {
        return Ok(false);
    }

    // Stash the current (apikey) auth so we can roll back on failure.
    let rollback_payload = if auth_path.exists() {
        Some(
            fs::read(&auth_path)
                .map_err(|e| anyhow!("codex_auth_read_failed:{e}"))?,
        )
    } else {
        None
    };

    fs::copy(&backup_path, &auth_path)
        .map_err(|e| anyhow!("codex_auth_restore_failed:{e}"))?;
    stop_app_server();
    crate::debug_log("codex: restored OAuth auth from backup, app-server pool reset");

    // Verify the restored OAuth credentials are still accepted by spawning
    // a fresh app-server and running an `initialize` + `model/list` handshake.
    // This is a local-auth check (no remote call), so it's fast and cannot
    // be tripped by an unrelated quota refresh.
    let ok = match get_or_start_app_server() {
        Ok(()) => {
            let probe = with_app_server(|client| {
                let id = client.send_request("model/list", json!({}))?;
                client.recv_response(id, |_, _| {})
            });
            probe.is_ok()
        }
        Err(e) => {
            crate::debug_log(&format!(
                "codex: restored OAuth auth failed handshake: {e}"
            ));
            false
        }
    };

    if ok {
        // Successful restore — remove the backup so a future fallback
        // re-creates it from a fresh OAuth snapshot.
        let _ = fs::remove_file(&backup_path);
        Ok(true)
    } else {
        // Rollback to apikey to keep the user functional.
        crate::debug_log("codex: OAuth restore failed, rolling back to apikey");
        if let Some(bytes) = rollback_payload {
            let _ = fs::write(&auth_path, bytes);
        } else {
            let _ = fs::remove_file(&auth_path);
        }
        stop_app_server();
        Ok(false)
    }
}

/// Check if the user has a valid Codex/OpenAI session.
/// Tries to start the app-server and run `model/list` — if auth fails, returns
/// a status indicating login is required.
///
/// On failure, the returned payload includes a structured `failure_reason`
/// field (`"rate_limited" | "auth" | "network" | null`) so the JS splash
/// bootstrap can react (e.g. auto-fallback to native API key on rate-limit).
pub fn session_status() -> Result<Value> {
    // First check: can we even find the binary?
    let binary_ok = resolve_codex_binary().is_ok();
    if !binary_ok {
        return Ok(json!({
            "status": "no_binary",
            "logged_in": false,
            "failure_reason": serde_json::Value::Null,
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
                        "failure_reason": serde_json::Value::Null,
                        "models_available": model_count
                    }))
                }
                Err(e) => {
                    let err_str = e.to_string();
                    let failure_reason = classify_session_failure(&err_str);
                    let status = match failure_reason {
                        Some("rate_limited") => "rate_limited",
                        Some("auth") => "requires_login",
                        _ => "error",
                    };
                    let message = match failure_reason {
                        Some("rate_limited") => {
                            "OAuth quota exhausted. Switch to API key or wait for reset.".to_string()
                        }
                        Some("auth") => {
                            "OpenAI session expired or not found. Please log in.".to_string()
                        }
                        _ => err_str,
                    };
                    Ok(json!({
                        "status": status,
                        "logged_in": false,
                        "failure_reason": failure_reason,
                        "message": message
                    }))
                }
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            let failure_reason = classify_session_failure(&err_str);
            Ok(json!({
                "status": "error",
                "logged_in": false,
                "failure_reason": failure_reason,
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

    #[test]
    fn classify_failure_rate_limited() {
        assert_eq!(
            classify_session_failure("codex_rate_limited:quota"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("codex_rate_limited:http_429:too many"),
            Some("rate_limited")
        );
    }

    #[test]
    fn classify_failure_auth() {
        assert_eq!(
            classify_session_failure("codex_unauthorized:invalid token"),
            Some("auth")
        );
    }

    #[test]
    fn classify_failure_network() {
        assert_eq!(
            classify_session_failure("codex_http_failed:status=502:bad gateway"),
            Some("network")
        );
        assert_eq!(
            classify_session_failure("codex_network:dns_failure"),
            Some("network")
        );
        assert_eq!(
            classify_session_failure("HttpConnectionFailed: peer reset"),
            Some("network")
        );
    }

    #[test]
    fn classify_failure_unknown() {
        // Loose "unauthorized" / "connection" / "network" tokens must NOT match —
        // they appear in unrelated user-facing strings and would mis-classify
        // generic errors as transient transport failures.
        assert_eq!(classify_session_failure("codex_rpc_error:code=-1:??"), None);
        assert_eq!(classify_session_failure("unauthorized: token expired"), None);
        assert_eq!(classify_session_failure("network unreachable"), None);
        assert_eq!(
            classify_session_failure("Account 'Connection 401' has no balance"),
            None
        );
    }

    #[test]
    fn classify_failure_codex_cli_rate_limit_stderr() {
        // Exact stderr phrases printed by `codex exec` on quota exhaustion.
        assert_eq!(
            classify_session_failure("You've hit your usage limit for the day"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("error: usage limit reached, try again later"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("Upgrade to Pro for more usage"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("Please try again at 2026-05-14T18:00:00Z"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("Rate limit exceeded; back off"),
            Some("rate_limited")
        );
        assert_eq!(
            classify_session_failure("HTTP 429 Too Many Requests"),
            Some("rate_limited")
        );
    }

    #[test]
    fn classify_failure_codex_cli_auth_stderr() {
        assert_eq!(
            classify_session_failure("Please run `codex login` to authenticate"),
            Some("auth")
        );
        assert_eq!(
            classify_session_failure("error: not logged in"),
            Some("auth")
        );
        assert_eq!(
            classify_session_failure("authentication required to call this endpoint"),
            Some("auth")
        );
        assert_eq!(
            classify_session_failure("response: 401 Unauthorized"),
            Some("auth")
        );
    }

    // ── auth_mode + swap round-trip tests ────────────────────────────
    //
    // We exercise the codex auth helpers against a temporary HOME so the
    // real ~/.codex/auth.json is never touched. Each test reads/restores
    // the HOME env var around its body and serializes via a mutex because
    // HOME is process-wide.
    use std::sync::Mutex;
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    struct TempHome {
        _dir: tempfile::TempDir,
        prev_home: Option<String>,
        prev_userprofile: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let prev_home = env::var("HOME").ok();
            let prev_userprofile = env::var("USERPROFILE").ok();
            // SAFETY: HOME is process-global; tests using TempHome must hold HOME_LOCK.
            std::env::set_var("HOME", dir.path());
            std::env::set_var("USERPROFILE", dir.path());
            std::fs::create_dir_all(dir.path().join(".codex")).expect("mkdir .codex");
            TempHome { _dir: dir, prev_home, prev_userprofile }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.prev_userprofile {
                Some(v) => std::env::set_var("USERPROFILE", v),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    fn auth_mode_reports_none_when_file_missing() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        assert_eq!(auth_mode().unwrap(), "none");
    }

    #[test]
    fn auth_mode_reports_chatgpt_when_tokens_present() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        let payload = json!({
            "tokens": {
                "access_token": "oauth-access-token",
                "id_token": "oauth-id-token",
                "refresh_token": "oauth-refresh"
            }
        });
        fs::write(codex_auth_path().unwrap(), payload.to_string()).unwrap();
        assert_eq!(auth_mode().unwrap(), "chatgpt");
    }

    #[test]
    fn auth_mode_reports_apikey_when_only_api_key_present() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        let payload = json!({ "OPENAI_API_KEY": "sk-test-1234" });
        fs::write(codex_auth_path().unwrap(), payload.to_string()).unwrap();
        assert_eq!(auth_mode().unwrap(), "apikey");
    }

    #[test]
    fn auth_mode_reports_none_for_empty_payload() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        // Empty tokens object + no api key → no usable credential.
        let payload = json!({ "tokens": { "access_token": "" } });
        fs::write(codex_auth_path().unwrap(), payload.to_string()).unwrap();
        assert_eq!(auth_mode().unwrap(), "none");
    }

    #[test]
    fn swap_to_oauth_returns_false_when_no_backup() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        // Even with an existing auth.json (apikey), no backup means we
        // can't restore — must report false and leave the file alone.
        let payload = json!({ "OPENAI_API_KEY": "sk-test" });
        fs::write(codex_auth_path().unwrap(), payload.to_string()).unwrap();
        assert_eq!(swap_to_oauth().unwrap(), false);
        // auth.json still apikey
        assert_eq!(auth_mode().unwrap(), "apikey");
    }

    #[test]
    fn has_oauth_backup_reports_false_when_missing() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        assert_eq!(has_oauth_backup(), false);
    }

    #[test]
    fn has_oauth_backup_reports_true_when_present() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        let backup_path = codex_auth_backup_path().unwrap();
        let payload = json!({ "tokens": { "access_token": "oauth-access" } });
        fs::write(&backup_path, payload.to_string()).unwrap();
        assert!(has_oauth_backup());
    }

    #[test]
    fn backup_is_created_from_chatgpt_state() {
        // Pure file-IO portion of swap_to_apikey: simulate by calling the
        // backup logic manually. (swap_to_apikey itself shells out to the
        // codex binary which is unavailable in the unit-test env.)
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _home = TempHome::new();
        let oauth_payload = json!({
            "tokens": { "access_token": "oauth-access" }
        });
        let auth_path = codex_auth_path().unwrap();
        let backup_path = codex_auth_backup_path().unwrap();
        fs::write(&auth_path, oauth_payload.to_string()).unwrap();
        assert_eq!(auth_mode().unwrap(), "chatgpt");

        // Manually mirror the backup step of swap_to_apikey.
        assert!(!backup_path.exists());
        fs::copy(&auth_path, &backup_path).unwrap();
        assert!(backup_path.exists());

        // Now simulate codex login --with-api-key by writing apikey state.
        let apikey_payload = json!({ "OPENAI_API_KEY": "sk-from-cli" });
        fs::write(&auth_path, apikey_payload.to_string()).unwrap();
        assert_eq!(auth_mode().unwrap(), "apikey");

        // swap_to_oauth (file-IO portion): copies backup back. We can't
        // run the handshake step in unit tests (no codex binary), but we
        // can verify the file is restored correctly when only the IO runs.
        fs::copy(&backup_path, &auth_path).unwrap();
        assert_eq!(auth_mode().unwrap(), "chatgpt");
    }
}
