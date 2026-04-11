//! Auto-update mechanism — check for new versions, download, and install.
//!
//! On startup the frontend calls `check_for_update()`. If an update is available
//! the UI either shows a dismissable banner (optional) or blocks the splash
//! screen (mandatory). The download streams progress events to the frontend
//! via `emit_event("update-download-progress", …)`.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

const CHECK_TIMEOUT_SECS: u64 = 5;
const DOWNLOAD_TIMEOUT_SECS: u64 = 600;

fn platform_manifest_url() -> &'static str {
    if cfg!(target_os = "macos") {
        "https://vps-c5793aab.vps.ovh.net/alfred/release/macos/update-manifest.json"
    } else {
        "https://vps-c5793aab.vps.ovh.net/alfred/release/windows/update-manifest.json"
    }
}

fn manifest_url() -> String {
    env::var("ALFRED_UPDATE_URL")
        .unwrap_or_else(|_| platform_manifest_url().to_string())
}

// ── Types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub mandatory: bool,
    pub min_version: Option<String>,
    pub release_notes: Option<String>,
    pub installer_url: String,
    pub installer_sha256: Option<String>,
    pub published_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdateCheckResult {
    pub update_available: bool,
    pub mandatory: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_notes: Option<String>,
    pub installer_url: Option<String>,
}

// ── Version comparison ────────────────────────────────────────────

fn parse_version(s: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> = s
        .trim_start_matches('v')
        .splitn(3, '.')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    )
}

fn version_less_than(a: &str, b: &str) -> bool {
    parse_version(a) < parse_version(b)
}

// ── Public API ────────────────────────────────────────────────────

pub fn check_for_update() -> Result<serde_json::Value> {
    let current = env!("CARGO_PKG_VERSION");
    crate::debug_log(&format!("updater: checking for update (current={current})"));

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(CHECK_TIMEOUT_SECS))
        .build();

    let resp = agent.get(&manifest_url()).call()
        .map_err(|e| anyhow!("updater_manifest_fetch_failed:{e}"))?;

    let manifest: UpdateManifest = resp.into_json()
        .map_err(|e| anyhow!("updater_manifest_parse_failed:{e}"))?;

    let update_available = version_less_than(current, &manifest.version);
    let mandatory = update_available
        && (manifest.mandatory
            || manifest
                .min_version
                .as_ref()
                .map(|mv| version_less_than(current, mv))
                .unwrap_or(false));

    crate::debug_log(&format!(
        "updater: latest={} available={update_available} mandatory={mandatory}",
        manifest.version
    ));

    let result = UpdateCheckResult {
        update_available,
        mandatory,
        current_version: current.to_string(),
        latest_version: manifest.version,
        release_notes: manifest.release_notes,
        installer_url: if update_available {
            Some(manifest.installer_url)
        } else {
            None
        },
    };

    Ok(serde_json::to_value(result)?)
}

pub fn download_update(url: &str, expected_sha256: Option<&str>) -> Result<serde_json::Value> {
    crate::debug_log(&format!("updater: downloading {url}"));

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build();

    let resp = agent.get(url).call()
        .map_err(|e| anyhow!("updater_download_failed:{e}"))?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|h| h.parse().ok())
        .unwrap_or(0);

    let tmp_dir = env::temp_dir();
    let partial_path = tmp_dir.join("alfred-update.partial");
    let final_name = if cfg!(target_os = "macos") {
        "alfred-update.dmg"
    } else {
        "alfred-update-setup.exe"
    };
    let final_path = tmp_dir.join(final_name);

    let mut reader = resp.into_reader();
    let mut file = fs::File::create(&partial_path)?;
    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 65536];
    let mut last_emit = std::time::Instant::now();

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;

        // Emit progress at most every 200ms
        if last_emit.elapsed().as_millis() >= 200 {
            crate::emit_event(
                "update-download-progress",
                json!({ "downloaded": downloaded, "total": total }),
            );
            last_emit = std::time::Instant::now();
        }
    }
    file.flush()?;
    drop(file);

    // Final progress event
    crate::emit_event(
        "update-download-progress",
        json!({ "downloaded": downloaded, "total": total }),
    );

    // SHA-256 verification
    if let Some(expected) = expected_sha256 {
        let hash = sha256_file(&partial_path)?;
        if !hash.eq_ignore_ascii_case(expected) {
            let _ = fs::remove_file(&partial_path);
            return Err(anyhow!(
                "updater_sha256_mismatch:expected={expected} got={hash}"
            ));
        }
        crate::debug_log("updater: SHA-256 verified");
    }

    fs::rename(&partial_path, &final_path)?;
    crate::debug_log(&format!("updater: saved to {}", final_path.display()));

    Ok(json!({ "path": final_path.display().to_string() }))
}

pub fn install_update(installer_path: &str) -> Result<serde_json::Value> {
    let path = Path::new(installer_path);
    if !path.exists() {
        return Err(anyhow!("updater_installer_not_found:{installer_path}"));
    }

    crate::debug_log(&format!("updater: launching installer {installer_path}"));

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| anyhow!("updater_installer_launch_failed:{e}"))?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        Command::new(path)
            .spawn()
            .map_err(|e| anyhow!("updater_installer_launch_failed:{e}"))?;
    }

    // Exit so the installer can replace our binary
    std::process::exit(0);
}

// ── SHA-256 (manual, no external crate) ───────────────────────────

fn sha256_file(path: &Path) -> Result<String> {
    // Minimal SHA-256 using the system — Windows has certutil, Unix has sha256sum.
    // We avoid pulling in the sha2 crate to keep deps minimal.
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("certutil")
            .args(["-hashfile", &path.to_string_lossy(), "SHA256"])
            .output()
            .map_err(|e| anyhow!("sha256_certutil_failed:{e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // certutil output: first line is header, second is the hash
        let hash = stdout
            .lines()
            .nth(1)
            .map(|l| l.trim().replace(' ', ""))
            .unwrap_or_default();
        if hash.len() == 64 {
            Ok(hash)
        } else {
            Err(anyhow!("sha256_parse_failed:{stdout}"))
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
            .map_err(|e| anyhow!("shasum_failed:{e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let hash = stdout.split_whitespace().next().unwrap_or_default().to_string();
        if hash.len() == 64 {
            Ok(hash)
        } else {
            Err(anyhow!("sha256_parse_failed:{stdout}"))
        }
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let out = Command::new("sha256sum")
            .arg(path)
            .output()
            .map_err(|e| anyhow!("sha256sum_failed:{e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let hash = stdout.split_whitespace().next().unwrap_or_default().to_string();
        if hash.len() == 64 {
            Ok(hash)
        } else {
            Err(anyhow!("sha256_parse_failed:{stdout}"))
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("0.1.0"), (0, 1, 0));
        assert_eq!(parse_version("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_version("10.0.1"), (10, 0, 1));
        assert_eq!(parse_version("1"), (1, 0, 0));
    }

    #[test]
    fn test_version_less_than() {
        assert!(version_less_than("0.1.0", "0.2.0"));
        assert!(version_less_than("0.1.0", "1.0.0"));
        assert!(version_less_than("0.9.9", "1.0.0"));
        assert!(!version_less_than("1.0.0", "0.9.9"));
        assert!(!version_less_than("1.0.0", "1.0.0"));
    }
}
