use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use anyhow::{anyhow, Result};

static FILE_WRITE_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn read_json_file(path: &PathBuf) -> Result<serde_json::Value> {
    let raw = fs::read_to_string(path)?;
    let parsed = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|error| anyhow!("invalid_json:{error}"))?;
    Ok(parsed)
}

pub fn write_json_file(path: &PathBuf, payload: &serde_json::Value) -> Result<()> {
    with_file_lock(path, || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_string_pretty(payload)?;
        let temp_path = build_temp_path(path);
        let mut result = Ok(());
        if let Err(error) = fs::write(&temp_path, serialized) {
            result = Err(anyhow!("json_storage_write_failed:{error}"));
        } else if let Err(error) = replace_file_with_retry(&temp_path, path) {
            result = Err(error);
        }
        if temp_path.exists() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    })
}

fn build_temp_path(path: &PathBuf) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("payload.json");
    let seq = FILE_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!("{file_name}.{}.{}.tmp", std::process::id(), seq);
    path.with_file_name(tmp_name)
}

fn is_transient_lock_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
    ) || matches!(error.raw_os_error(), Some(5 | 13 | 32 | 35 | 183))
}

fn with_file_lock<T, F: FnOnce() -> Result<T>>(path: &PathBuf, task: F) -> Result<T> {
    let lock_path = path.with_extension("lock");
    for attempt in 0..200 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(file) => {
                drop(file);
                let result = task();
                let _ = fs::remove_file(&lock_path);
                return result;
            }
            Err(error) => {
                if !is_transient_lock_error(&error) {
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(10 + attempt as u64));
    }
    Err(anyhow!(
        "json_storage_lock_failed:{}",
        path.file_name().and_then(|v| v.to_str()).unwrap_or("payload.json")
    ))
}

fn is_transient_rename_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AlreadyExists
    ) || matches!(error.raw_os_error(), Some(5 | 13 | 16 | 32 | 35 | 183))
}

fn replace_file_with_retry(tmp_path: &PathBuf, target_path: &PathBuf) -> Result<()> {
    for attempt in 0..6 {
        match fs::rename(tmp_path, target_path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    let _ = fs::remove_file(target_path);
                    if fs::rename(tmp_path, target_path).is_ok() {
                        return Ok(());
                    }
                }
                if !is_transient_rename_error(&error) {
                    break;
                }
            }
        }
        thread::sleep(Duration::from_millis(25 * (attempt + 1) as u64));
    }
    Err(anyhow!(
        "json_storage_write_failed:{}",
        target_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("payload.json")
    ))
}
