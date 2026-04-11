use std::{env, path::PathBuf};

/// Portable data directory that works both in dev and installed builds.
///
/// Resolution order:
/// 1. `ALFRED_DATA_DIR` env var (explicit override)
/// 2. Dev mode (`CARGO_MANIFEST_DIR` exists at runtime) → `<manifest>/../data`
/// 3. Production → `<exe_dir>/data` (next to the installed binary)
pub fn default_data_dir() -> PathBuf {
    if let Ok(raw) = env::var("ALFRED_DATA_DIR") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    // Dev mode: CARGO_MANIFEST_DIR is baked at compile time. Only use it if
    // the current exe actually lives inside the build tree (not an installed copy).
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest_dir.exists() {
        if let Ok(exe) = env::current_exe() {
            let exe_str = exe.to_string_lossy();
            let manifest_str = manifest_dir.to_string_lossy();
            // exe is inside src-tauri/target/ → we're in dev mode
            if exe_str.contains(&*manifest_str) || exe_str.contains("src-tauri") {
                return manifest_dir.join("../data");
            }
        }
    }
    // Production: platform-conventional app data directory.
    // Avoids permission issues in Program Files / .app bundles.
    #[cfg(target_os = "windows")]
    if let Ok(appdata) = env::var("APPDATA") {
        return PathBuf::from(appdata).join("alfred-desktop");
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join("Library/Application Support/alfred-desktop");
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".alfred-desktop");
    }
    // Fallback: next to the executable
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("data")))
        .unwrap_or_else(|| PathBuf::from("data"))
}

pub fn default_db_path() -> PathBuf {
    default_data_dir().join("desktop-state/alfred.db")
}

fn default_runtime_state_dir() -> PathBuf {
    default_data_dir().join("runtime-state")
}

pub fn resolve_runtime_state_dir() -> PathBuf {
    match env::var("ALFRED_STATE_DIR") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_runtime_state_dir(),
    }
}

fn default_reports_dir() -> PathBuf {
    default_data_dir().join("reports")
}

pub fn resolve_reports_dir() -> PathBuf {
    match env::var("ALFRED_REPORTS_DIR") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_reports_dir(),
    }
}

pub fn resolve_latest_report_path() -> PathBuf {
    match env::var("ALFRED_LATEST_REPORT_PATH") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => resolve_reports_dir().join("latest.json"),
    }
}

pub fn resolve_report_history_dir() -> PathBuf {
    match env::var("ALFRED_REPORT_HISTORY_DIR") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => resolve_reports_dir().join("history"),
    }
}

pub fn resolve_runtime_settings_path() -> PathBuf {
    match env::var("ALFRED_RUNTIME_SETTINGS_PATH") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_data_dir().join("runtime-settings.json"),
    }
}

pub fn resolve_source_snapshot_store_path() -> PathBuf {
    match env::var("ALFRED_SOURCE_SNAPSHOTS_PATH") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_data_dir().join("source-sync/source-snapshots.json"),
    }
}

pub fn resolve_audit_log_path() -> PathBuf {
    match env::var("ALFRED_AUDIT_LOG_PATH") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_data_dir().join("audit/events.jsonl"),
    }
}

pub fn resolve_debug_log_path() -> PathBuf {
    match env::var("ALFRED_DEBUG_LOG_PATH") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => default_data_dir().join("debug.log"),
    }
}

pub fn resolve_control_plane_base_url() -> String {
    env::var("ALFRED_CONTROL_PLANE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:4300".to_string())
        .trim()
        .trim_end_matches('/')
        .to_string()
}
