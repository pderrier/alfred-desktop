use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

use crate::{
    load_run_by_id_direct, native_collection_dispatch::NativeCollectionDispatchQueue,
    now_epoch_ms, now_iso_string, patch_run_state_direct_with, request_http_json,
    resolve_source_snapshot_store_path, set_native_run_stage, write_json_file,
};

use crate::native_collection_helpers::{
    as_array, as_text, build_collection_state, diagnose_run_quality, infer_issue_code, normalize_csv_snapshot,
    normalize_finary_snapshot, parse_fr_number,
    HttpRequestFn,
};

const LOCAL_FINARY_SOURCE_ID: &str = "finary_local_default";

fn strip_csv_quotes(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('\u{FEFF}').trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
        return trimmed[1..trimmed.len() - 1].replace("\"\"", "\"").trim().to_string();
    }
    trimmed.to_string()
}

fn split_semicolon(line: &str) -> Vec<String> {
    line.split(';').map(strip_csv_quotes).collect()
}

fn normalize_header(raw: &str) -> String {
    strip_csv_quotes(raw)
        .to_lowercase()
        .replace(['à', 'â', 'ä'], "a")
        .replace(['é', 'è', 'ê', 'ë'], "e")
        .replace(['î', 'ï'], "i")
        .replace(['ô', 'ö'], "o")
        .replace(['ù', 'û', 'ü'], "u")
        .replace('ç', "c")
        .replace(['’', '\''], "'")
        .trim()
        .to_string()
}

fn resolve_latest_by_prefix(base_dir: &PathBuf, prefix: &str) -> Result<Option<PathBuf>> {
    let mut entries = fs::read_dir(base_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with(prefix) && name.ends_with(".csv"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        let left_mtime = fs::metadata(left)
            .and_then(|metadata| metadata.modified())
            .ok();
        let right_mtime = fs::metadata(right)
            .and_then(|metadata| metadata.modified())
            .ok();
        right_mtime.cmp(&left_mtime)
    });
    Ok(entries.into_iter().next())
}

fn resolve_export_files(export_path: &str) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>)> {
    let candidate = PathBuf::from(export_path);
    let metadata = fs::metadata(&candidate)
        .map_err(|error| anyhow!("export_path_not_found:{export_path}:{error}"))?;
    let base_dir = if metadata.is_dir() {
        candidate
    } else {
        candidate
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("export_path_not_found:{export_path}"))?
    };
    let positions = resolve_latest_by_prefix(&base_dir, "POSITIONS_")?
        .ok_or_else(|| anyhow!("positions_export_not_found:{export_path}"))?;
    let history = resolve_latest_by_prefix(&base_dir, "HISTORY_")?;
    let orders = resolve_latest_by_prefix(&base_dir, "ORDERS_")?;
    Ok((positions, history, orders))
}

fn parse_positions_file(path: &PathBuf, account_name: &str) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    parse_positions_text(&text, account_name)
}

/// Parse CSV positions text (Boursorama semicolon format) into a portfolio snapshot.
fn parse_positions_text(text: &str, account_name: &str) -> Result<Value> {
    let lines = text.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>();
    let totals_line = lines
        .iter()
        .find(|line| line.contains("Valo total") && line.contains("Solde espèces"))
        .copied()
        .unwrap_or_default();
    let valeur_totale = totals_line
        .split("Valo total")
        .nth(1)
        .and_then(|part| part.split('=').nth(1))
        .and_then(|part| part.split(';').next())
        .map(parse_fr_number)
        .unwrap_or(0.0);
    let plus_value_totale = totals_line
        .split("+/- value latente")
        .nth(1)
        .and_then(|part| part.split('=').nth(1))
        .and_then(|part| part.split(';').next())
        .map(parse_fr_number)
        .unwrap_or(0.0);
    let liquidites = totals_line
        .split("Solde espèces")
        .nth(1)
        .and_then(|part| part.split('=').nth(1))
        .and_then(|part| part.split(';').next())
        .map(parse_fr_number)
        .unwrap_or(0.0);
    let header_index = lines.iter().position(|line| {
        let cols = split_semicolon(line);
        cols.first().map(|value| normalize_header(value)).unwrap_or_default() == "nom"
            && cols.get(1)
                .map(|value| {
                    let normalized = normalize_header(value);
                    normalized == "code isin" || normalized == "isin"
                })
                .unwrap_or(false)
    });
    let mut positions = Vec::new();
    if let Some(index) = header_index {
        for line in lines.iter().skip(index + 1) {
            let cols = split_semicolon(line);
            if cols.len() < 11 {
                continue;
            }
            let nom = cols[0].clone();
            let isin = cols[1].clone();
            let ticker = derive_ticker_from_name(&nom, &isin);
            positions.push(serde_json::to_value(crate::models::Position {
                ticker,
                nom,
                isin: if isin.trim().is_empty() { None } else { Some(isin) },
                quantite: parse_fr_number(&cols[2]),
                prix_actuel: parse_fr_number(&cols[3]),
                valeur_actuelle: parse_fr_number(&cols[4]),
                prix_revient: parse_fr_number(&cols[6]),
                plus_moins_value: parse_fr_number(&cols[8]),
                plus_moins_value_pct: parse_fr_number(&cols[10]),
                compte: account_name.to_string(),
                position_type: None,
            }).unwrap_or_default());
        }
    }
    Ok(json!({
        "positions": positions,
        "valeur_totale": valeur_totale,
        "plus_value_totale": plus_value_totale,
        "liquidites": liquidites
    }))
}

fn parse_history_file(path: Option<PathBuf>) -> Result<Value> {
    let Some(path) = path else { return Ok(json!([])); };
    let text = fs::read_to_string(path)?;
    let lines = text.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>();
    let header_index = lines
        .iter()
        .position(|line| normalize_header(split_semicolon(line).first().map(String::as_str).unwrap_or_default()) == "operation");
    let Some(index) = header_index else { return Ok(json!([])); };
    let mut out = Vec::new();
    for line in lines.iter().skip(index + 1) {
        let cols = split_semicolon(line);
        if cols.len() < 7 {
            continue;
        }
        out.push(json!({
            "operation": cols[0],
            "value": cols[1],
            "isin": cols[2],
            "quantity": parse_fr_number(&cols[3]),
            "amount": parse_fr_number(&cols[4]),
            "fees": parse_fr_number(&cols[5]),
            "date": cols[6]
        }));
    }
    Ok(Value::Array(out))
}

fn parse_orders_file(path: Option<PathBuf>) -> Result<Value> {
    let Some(path) = path else { return Ok(json!([])); };
    let text = fs::read_to_string(path)?;
    let lines = text.lines().filter(|line| !line.trim().is_empty()).collect::<Vec<_>>();
    let header_index = lines.iter().position(|line| {
        let first = normalize_header(split_semicolon(line).first().map(String::as_str).unwrap_or_default());
        first == "date de mise a jour" || first == "date"
    });
    let Some(index) = header_index else { return Ok(json!([])); };
    if lines.len() < index + 2 {
        return Ok(json!([]));
    }
    let headers = split_semicolon(lines[index]);
    let mut out = Vec::new();
    for line in lines.iter().skip(index + 1) {
        let cols = split_semicolon(line);
        if cols.len() < 2 {
            continue;
        }
        let mut row = Map::new();
        for (idx, header) in headers.iter().enumerate() {
            row.insert(header.clone(), Value::String(cols.get(idx).cloned().unwrap_or_default()));
        }
        out.push(Value::Object(row));
    }
    Ok(Value::Array(out))
}

/// Parse raw CSV text from the file picker (possibly multiple files concatenated
/// with `--- FILE: name.csv ---` separators). Returns a portfolio snapshot.
///
/// Three-tier parsing strategy:
/// 1. **Boursorama format** — semicolon-delimited with "Nom;Code Isin;..." header
/// 2. **Heuristic mapping** — detect delimiter, match common column names
/// 3. **LLM fallback** — ask the LLM to map unknown columns
fn parse_csv_upload_text(raw: &str, account: &str) -> Result<Value> {
    // Strip "--- FILE: xxx ---" separators from the file picker format.
    let text: String = raw
        .lines()
        .filter(|line| !line.starts_with("--- FILE: "))
        .collect::<Vec<_>>()
        .join("\n");

    let acct = if account.is_empty() { "CSV_import" } else { account };

    // ── Tier 1: Boursorama format ───────────────────────────────────
    if let Ok(snap) = parse_positions_text(&text, acct) {
        let pos = snap.get("positions").and_then(|v| v.as_array());
        if pos.map(|a| !a.is_empty()).unwrap_or(false) {
            crate::debug_log("[csv-parse] tier 1: Boursorama format detected");
            return wrap_snapshot(snap);
        }
    }

    // ── Tier 2 & 3: heuristic + LLM fallback ───────────────────────
    let delimiter = detect_csv_delimiter(&text);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return Err(anyhow!("csv_upload_no_positions_found"));
    }
    let headers: Vec<String> = split_csv_line(lines[0], delimiter);
    let data_rows: Vec<Vec<String>> = lines[1..]
        .iter()
        .map(|line| split_csv_line(line, delimiter))
        .collect();

    // Tier 2: heuristic column mapping
    let mapping = heuristic_column_map(&headers);
    if mapping.ticker >= 0 && mapping.quantite >= 0 {
        crate::debug_log(&format!("[csv-parse] tier 2: heuristic mapping — ticker={} quantite={}", mapping.ticker, mapping.quantite));
        let positions = apply_column_mapping(&mapping, &data_rows, acct);
        if !positions.is_empty() {
            return wrap_snapshot(json!({
                "positions": positions,
                "valeur_totale": positions.iter().filter_map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64())).sum::<f64>(),
                "plus_value_totale": positions.iter().filter_map(|p| p.get("plus_moins_value").and_then(|v| v.as_f64())).sum::<f64>(),
                "liquidites": 0.0
            }));
        }
    }

    // ── Tier 3: LLM adaptation ──────────────────────────────────────
    crate::debug_log("[csv-parse] tier 3: requesting LLM column mapping");
    let sample: Vec<Vec<String>> = data_rows.iter().take(5).cloned().collect();
    let llm_result = crate::llm::adapt_csv_columns(&headers, &sample, delimiter)?;
    let llm_mapping = parse_llm_column_mapping(&llm_result, &headers);
    if llm_mapping.ticker < 0 || llm_mapping.quantite < 0 {
        return Err(anyhow!("csv_upload_llm_mapping_failed:could not identify ticker and quantity columns"));
    }
    let _number_format = llm_result
        .get("number_format")
        .and_then(|v| v.as_str())
        .unwrap_or("french");
    let positions = apply_column_mapping(&llm_mapping, &data_rows, acct);
    if positions.is_empty() {
        return Err(anyhow!("csv_upload_no_positions_found"));
    }
    wrap_snapshot(json!({
        "positions": positions,
        "valeur_totale": positions.iter().filter_map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64())).sum::<f64>(),
        "plus_value_totale": positions.iter().filter_map(|p| p.get("plus_moins_value").and_then(|v| v.as_f64())).sum::<f64>(),
        "liquidites": 0.0
    }))
}

fn wrap_snapshot(portfolio: Value) -> Result<Value> {
    if portfolio.get("positions").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true) {
        return Err(anyhow!("csv_upload_no_positions_found"));
    }
    Ok(json!({
        "portfolio_source": "csv",
        "positions": portfolio.get("positions").cloned().unwrap_or_else(|| json!([])),
        "transactions": [],
        "orders": [],
        "valeur_totale": portfolio.get("valeur_totale").cloned().unwrap_or_else(|| json!(0.0)),
        "plus_value_totale": portfolio.get("plus_value_totale").cloned().unwrap_or_else(|| json!(0.0)),
        "liquidites": portfolio.get("liquidites").cloned().unwrap_or_else(|| json!(0.0))
    }))
}

// ── CSV parsing utilities ───────────────────────────────────────

fn detect_csv_delimiter(text: &str) -> char {
    let first_line = text.lines().next().unwrap_or_default();
    let candidates = [';', ',', '\t', '|'];
    candidates
        .iter()
        .copied()
        .max_by_key(|&d| first_line.matches(d).count())
        .unwrap_or(',')
}

fn split_csv_line(line: &str, delimiter: char) -> Vec<String> {
    // Handle quoted fields (e.g. "some, value")
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch == delimiter && !in_quotes {
            fields.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    fields.push(current.trim().to_string());
    fields
}

struct ColumnMapping {
    ticker: i32,
    nom: i32,
    isin: i32,
    quantite: i32,
    prix_actuel: i32,
    valeur_actuelle: i32,
    prix_revient: i32,
    plus_moins_value: i32,
    compte: i32,
}

fn heuristic_column_map(headers: &[String]) -> ColumnMapping {
    let normalized: Vec<String> = headers.iter().map(|h| normalize_header(h)).collect();

    let find = |patterns: &[&str]| -> i32 {
        normalized
            .iter()
            .position(|h| patterns.iter().any(|p| h.contains(p)))
            .map(|i| i as i32)
            .unwrap_or(-1)
    };

    ColumnMapping {
        ticker: find(&["ticker", "symbol", "code", "mnemo"]),
        nom: find(&["name", "company", "asset", "instrument", "nom", "libelle", "valeur"]),
        isin: find(&["isin"]),
        quantite: find(&["qty", "quantity", "shares", "units", "quantite", "qte", "nb"]),
        prix_actuel: find(&["price", "last_price", "cours", "dernier", "last"]),
        valeur_actuelle: find(&["market_value", "valorisation", "valo", "montant"]),
        prix_revient: find(&["pru", "avg_cost", "prix_revient", "cost", "revient"]),
        plus_moins_value: find(&["gain", "pnl", "plus_value", "plus_moins", "p&l", "+/- value"]),
        compte: find(&["account", "compte", "portfolio"]),
    }
}

fn parse_llm_column_mapping(llm_response: &Value, _headers: &[String]) -> ColumnMapping {
    let mapping = llm_response
        .get("column_mapping")
        .unwrap_or(llm_response);

    let get_idx = |field: &str| -> i32 {
        mapping
            .get(field)
            .and_then(|v| v.as_i64())
            .map(|v| v as i32)
            .unwrap_or(-1)
    };

    ColumnMapping {
        ticker: get_idx("ticker"),
        nom: get_idx("nom"),
        isin: get_idx("isin"),
        quantite: get_idx("quantite"),
        prix_actuel: get_idx("prix_actuel"),
        valeur_actuelle: get_idx("valeur_actuelle"),
        prix_revient: get_idx("prix_revient"),
        plus_moins_value: get_idx("plus_moins_value"),
        compte: get_idx("compte"),
    }
}

fn apply_column_mapping(mapping: &ColumnMapping, rows: &[Vec<String>], account_name: &str) -> Vec<Value> {
    let col = |row: &Vec<String>, idx: i32| -> String {
        if idx < 0 { return String::new(); }
        row.get(idx as usize).cloned().unwrap_or_default()
    };
    let num = |row: &Vec<String>, idx: i32| -> f64 {
        if idx < 0 { return 0.0; }
        parse_fr_number(row.get(idx as usize).map(String::as_str).unwrap_or_default())
    };

    let mut positions = Vec::new();
    for row in rows {
        let ticker_raw = col(row, mapping.ticker);
        let ticker = ticker_raw.trim().to_uppercase();
        if ticker.is_empty() { continue; }
        let quantite = num(row, mapping.quantite);
        if !quantite.is_finite() || quantite == 0.0 { continue; }

        let nom = col(row, mapping.nom);
        let isin_raw = col(row, mapping.isin);
        let compte = col(row, mapping.compte);

        positions.push(
            serde_json::to_value(crate::models::Position {
                ticker: ticker.clone(),
                nom: if nom.is_empty() { ticker.clone() } else { nom },
                isin: if isin_raw.trim().is_empty() { None } else { Some(isin_raw) },
                quantite,
                prix_actuel: num(row, mapping.prix_actuel),
                valeur_actuelle: num(row, mapping.valeur_actuelle),
                prix_revient: num(row, mapping.prix_revient),
                plus_moins_value: num(row, mapping.plus_moins_value),
                plus_moins_value_pct: 0.0,
                compte: if compte.is_empty() { account_name.to_string() } else { compte },
                position_type: None,
            })
            .unwrap_or_default(),
        );
    }
    positions
}

fn load_csv_export_snapshot(export_path: &str, account_name: &str) -> Result<Value> {
    let (positions_file, history_file, orders_file) = resolve_export_files(export_path)?;
    let portfolio = parse_positions_file(&positions_file, account_name)?;
    Ok(json!({
        "portfolio_source": "csv",
        "latest_export": export_path,
        "positions": portfolio.get("positions").cloned().unwrap_or_else(|| json!([])),
        "transactions": parse_history_file(history_file)?,
        "orders": parse_orders_file(orders_file)?,
        "valeur_totale": portfolio.get("valeur_totale").cloned().unwrap_or_else(|| json!(0.0)),
        "plus_value_totale": portfolio.get("plus_value_totale").cloned().unwrap_or_else(|| json!(0.0)),
        "liquidites": portfolio.get("liquidites").cloned().unwrap_or_else(|| json!(0.0))
    }))
}

fn read_source_snapshot_store() -> Result<Value> {
    let path = resolve_source_snapshot_store_path();
    if !path.exists() {
        return Ok(json!({ "latest_by_source": {}, "snapshots_by_source": {} }));
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn write_source_snapshot_store(store: &Value) -> Result<()> {
    let path = resolve_source_snapshot_store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_json_file(&path, store)
}

fn persist_local_finary_snapshot(snapshot: &Value) -> Result<()> {
    let positions = snapshot.get("positions").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    if positions == 0 {
        return Err(anyhow!(
            "finary_snapshot_empty:refusing to persist empty snapshot (0 positions) — \
             the Finary API may have returned an error or the session token may be stale"
        ));
    }
    let mut store = read_source_snapshot_store()?;
    let entry = json!({
        "snapshot_id": format!("snap_{:012x}", now_epoch_ms()),
        "source_id": LOCAL_FINARY_SOURCE_ID,
        "portfolio_source": "finary",
        "saved_at": now_iso_string(),
        "snapshot": snapshot
    });
    if let Some(object) = store.as_object_mut() {
        let latest = object
            .entry("latest_by_source".to_string())
            .or_insert_with(|| json!({}));
        if let Some(latest_obj) = latest.as_object_mut() {
            latest_obj.insert(LOCAL_FINARY_SOURCE_ID.to_string(), entry.clone());
        }
        let snapshots = object
            .entry("snapshots_by_source".to_string())
            .or_insert_with(|| json!({}));
        if let Some(snapshots_obj) = snapshots.as_object_mut() {
            let list = snapshots_obj
                .entry(LOCAL_FINARY_SOURCE_ID.to_string())
                .or_insert_with(|| json!([]));
            if let Some(array) = list.as_array_mut() {
                array.push(entry);
            }
        }
    }
    write_source_snapshot_store(&store)
}

fn get_latest_local_finary_snapshot() -> Result<Option<Value>> {
    let store = read_source_snapshot_store()?;
    Ok(store
        .get("latest_by_source")
        .and_then(|value| value.get(LOCAL_FINARY_SOURCE_ID))
        .cloned())
}

fn local_day_key(raw: &str) -> String {
    raw.get(0..10).unwrap_or_default().to_string()
}

fn same_local_day(saved_at: &str) -> bool {
    local_day_key(saved_at) == local_day_key(&now_iso_string())
}




pub(crate) fn read_line_memory_store() -> Result<Value> {
    let path = crate::resolve_runtime_state_dir().join("line-memory.json");
    if !path.exists() {
        return Ok(json!({
            "by_ticker": {},
            "global_deep_news_banned_urls": [],
            "deep_news_rotation_cache": { "by_ticker": {} }
        }));
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Fetch a Finary snapshot if no cached one exists. Persists to disk.
/// Returns the snapshot (cached or fresh).
pub fn fetch_finary_snapshot_standalone() -> Result<Value> {
    // Check if we already have a cached snapshot with positions
    if let Ok(Some(existing)) = get_latest_local_finary_snapshot() {
        let count = existing.get("snapshot")
            .and_then(|s| s.get("positions"))
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if count > 0 {
            crate::debug_log(&format!("finary_sync: using cached snapshot ({count} positions)"));
            return Ok(existing.get("snapshot").cloned().unwrap_or(existing));
        }
    }

    crate::debug_log("finary_sync: no cached snapshot, fetching from Finary API...");
    let snapshot = fetch_finary_snapshot("__startup__", crate::request_http_json)?;
    if let Err(e) = persist_local_finary_snapshot(&snapshot) {
        crate::debug_log(&format!("finary_sync: persist failed (may be empty): {e}"));
    } else {
        let count = snapshot.get("positions").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        crate::debug_log(&format!("finary_sync: persisted snapshot with {count} positions"));
    }
    Ok(snapshot)
}

fn fetch_finary_snapshot(run_id: &str, _request_fn: HttpRequestFn) -> Result<Value> {
    let raw = crate::finary::fetch_snapshot()
        .map_err(|e| anyhow!("finary_snapshot_failed:{e}"))?;
    // The native fetch_snapshot returns raw API payloads — map them
    let investments = raw.get("investmentsPayload").cloned().unwrap_or_else(|| json!({}));
    let holdings = raw.get("accountsPayload").cloned().unwrap_or_else(|| json!({}));
    let transactions = raw.get("transactionsPayload").cloned().unwrap_or_else(|| json!({}));
    let orders = raw.get("ordersPayload").cloned().unwrap_or_else(|| json!({}));
    // Build snapshot using the same field extraction as the JS mapper
    let securities = extract_securities(&investments);
    let positions: Vec<Value> = securities.iter().map(|row| map_security_to_position(row)).collect();
    let total_value: f64 = positions.iter().map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum();
    let total_gain: f64 = positions.iter().map(|p| p.get("plus_moins_value").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum();
    // Build accounts summary from positions
    let mut account_map: std::collections::HashMap<String, (f64, f64)> = std::collections::HashMap::new();
    for pos in &positions {
        let compte = pos.get("compte").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let entry = account_map.entry(compte.to_string()).or_insert((0.0, 0.0));
        entry.0 += pos.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0);
        entry.1 += pos.get("plus_moins_value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    }
    // Compute cash per investment account by matching via connection_id
    let holdings_accounts = extract_result_list(&holdings);
    let cash_result = build_cash_mapping(&holdings_accounts);
    let investment_name_to_cash = cash_result.mapping;
    // Global cash = ALL fiats across ALL holdings accounts (not just matched ones)
    let cash: f64 = holdings_accounts.iter().map(|acct| {
        acct.get("fiats")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|fiat| {
                fiat.get("current_value").or_else(|| fiat.get("amount")).or_else(|| fiat.get("quantity"))
                    .and_then(|v| v.as_f64()).unwrap_or(0.0)
            })
            .sum::<f64>()
    }).sum();

    let snapshot_accounts: Vec<Value> = account_map.iter().map(|(name, (value, gain))| {
        let acct_cash = investment_name_to_cash.get(name).copied().unwrap_or(0.0);
        json!({ "name": name, "total_value": value, "total_gain": gain, "cash": acct_cash })
    }).collect();

    let mut snapshot = json!({
        "run_id": run_id,
        "positions": positions,
        "accounts": snapshot_accounts,
        "transactions": extract_result_list(&transactions),
        "orders": extract_result_list(&orders),
        "total_value": total_value,
        "total_gain": total_gain,
        "cash": cash
    });
    // Include ambiguous cash groups that need user confirmation
    if !cash_result.ambiguous_groups.is_empty() {
        snapshot.as_object_mut().unwrap().insert(
            "ambiguous_cash_groups".to_string(),
            json!(cash_result.ambiguous_groups),
        );
    }
    Ok(snapshot)
}

fn extract_securities(payload: &Value) -> Vec<Value> {
    if let Some(arr) = payload.get("result").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = payload.get("result").and_then(|v| v.get("securities")).and_then(|v| v.as_array()) {
        return arr.clone();
    }
    Vec::new()
}

fn extract_result_list(payload: &Value) -> Vec<Value> {
    if let Some(arr) = payload.get("result").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = payload.get("result").and_then(|v| v.get("items")).and_then(|v| v.as_array()) {
        return arr.clone();
    }
    Vec::new()
}

/// Result of cash mapping — includes the amount mapping and any ambiguous groups
/// that need user confirmation.
struct CashMappingResult {
    /// Investment account name → cash balance amount
    mapping: std::collections::HashMap<String, f64>,
    /// Ambiguous groups where heuristic matching was used and needs confirmation
    ambiguous_groups: Vec<Value>,
}

/// Build a mapping from investment account name → associated cash balance.
///
/// Holdings accounts with securities are "investment" accounts; those with only
/// fiats are "cash" accounts. We pair them via `connection_id` so that e.g.
/// "Plan Epargne en Action" (securities) gets matched with "Compte espèce PEA"
/// (fiats) when they share the same connection.
///
/// Before heuristic matching, checks user-preferences.json for persisted
/// `cash_account_links` (investment_name → cash_name). Ambiguous groups
/// (N:M within same connection) that aren't covered by saved preferences
/// are flagged for user confirmation in the snapshot.
fn build_cash_mapping(holdings_accounts: &[Value]) -> CashMappingResult {
    use std::collections::HashMap;

    struct HoldingsEntry {
        name: String,
        connection_id: Option<i64>,
        correlation_id: Option<i64>,
        institution_name: String,
        securities_count: usize,
        fiats_sum: f64,
    }

    // Load persisted cash account links from user preferences
    let prefs = crate::runtime_settings::get_user_preferences();
    let saved_links: HashMap<String, String> = prefs
        .get("cash_account_links")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    if !saved_links.is_empty() {
        crate::debug_log(&format!(
            "finary_cash_mapping: loaded {} persisted cash_account_links from preferences",
            saved_links.len()
        ));
    }

    // Parse all holdings accounts
    let mut entries: Vec<HoldingsEntry> = Vec::new();
    for acct in holdings_accounts {
        let name = acct.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let connection_id = acct.get("connection_id").and_then(|v| v.as_i64());
        let correlation_id = acct.get("correlation_id").and_then(|v| v.as_i64());
        let institution_name = acct.get("institution")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let securities_count = acct.get("securities")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        let fiats_sum: f64 = acct.get("fiats")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|fiat| {
                fiat.get("current_value")
                    .or_else(|| fiat.get("amount"))
                    .or_else(|| fiat.get("quantity"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
            })
            .sum();
        entries.push(HoldingsEntry { name, connection_id, correlation_id, institution_name, securities_count, fiats_sum });
    }

    // Classify into investment vs cash accounts
    let investment_entries: Vec<&HoldingsEntry> = entries.iter().filter(|e| e.securities_count > 0).collect();
    let cash_entries: Vec<&HoldingsEntry> = entries.iter().filter(|e| e.fiats_sum > 0.0 && e.securities_count == 0).collect();

    // Build name → entry lookup for saved links resolution
    let cash_by_name: HashMap<&str, &HoldingsEntry> = cash_entries.iter()
        .map(|e| (e.name.as_str(), *e))
        .collect();

    crate::debug_log(&format!(
        "finary_cash_mapping: {} investment accounts, {} cash accounts out of {} total",
        investment_entries.len(), cash_entries.len(), entries.len()
    ));

    let mut result: HashMap<String, f64> = HashMap::new();
    let mut matched_cash_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut ambiguous_groups: Vec<Value> = Vec::new();

    // Strategy 0: Apply persisted cash_account_links from user preferences
    for inv in &investment_entries {
        if let Some(cash_name) = saved_links.get(&inv.name) {
            if let Some(cash_entry) = cash_by_name.get(cash_name.as_str()) {
                result.insert(inv.name.clone(), cash_entry.fiats_sum);
                // Mark the cash entry as matched
                if let Some(idx) = cash_entries.iter().position(|e| e.name == cash_entry.name) {
                    matched_cash_indices.insert(idx);
                }
                crate::debug_log(&format!(
                    "finary_cash_mapping: matched '{}' → cash {:.2} (user preference → '{}')",
                    inv.name, cash_entry.fiats_sum, cash_name
                ));
            }
        }
    }

    // Strategy 1: Group by connection_id and match within each group
    let mut inv_by_conn: HashMap<i64, Vec<&HoldingsEntry>> = HashMap::new();
    let mut cash_by_conn: HashMap<i64, Vec<(usize, &HoldingsEntry)>> = HashMap::new();
    for inv in &investment_entries {
        if result.contains_key(&inv.name) {
            continue; // Already matched via saved preferences
        }
        if let Some(cid) = inv.connection_id {
            inv_by_conn.entry(cid).or_default().push(inv);
        }
    }
    for (idx, ce) in cash_entries.iter().enumerate() {
        if matched_cash_indices.contains(&idx) {
            continue; // Already matched via saved preferences
        }
        if let Some(cid) = ce.connection_id {
            cash_by_conn.entry(cid).or_default().push((idx, ce));
        }
    }

    for (cid, invs) in &inv_by_conn {
        if let Some(cashes) = cash_by_conn.get(cid) {
            if invs.len() == 1 && cashes.len() == 1 {
                // Exactly 1 investment + 1 cash in this connection → direct match
                let cash_amount = cashes[0].1.fiats_sum;
                result.insert(invs[0].name.clone(), cash_amount);
                matched_cash_indices.insert(cashes[0].0);
                crate::debug_log(&format!(
                    "finary_cash_mapping: matched '{}' → cash {:.2} (connection_id={}, 1:1)",
                    invs[0].name, cash_amount, cid
                ));
            } else {
                // N investments + M cash accounts — ambiguous case
                // Use correlation_id heuristic as temporary fallback
                let mut sorted_invs: Vec<&&HoldingsEntry> = invs.iter().collect();
                let mut sorted_cashes: Vec<&(usize, &HoldingsEntry)> = cashes.iter().collect();
                sorted_invs.sort_by_key(|e| e.correlation_id.unwrap_or(i64::MAX));
                sorted_cashes.sort_by_key(|e| e.1.correlation_id.unwrap_or(i64::MAX));
                for (i, inv) in sorted_invs.iter().enumerate() {
                    if let Some(cash_pair) = sorted_cashes.get(i) {
                        result.insert(inv.name.clone(), cash_pair.1.fiats_sum);
                        matched_cash_indices.insert(cash_pair.0);
                        crate::debug_log(&format!(
                            "finary_cash_mapping: matched '{}' → cash {:.2} (connection_id={}, correlation pair, needs_confirmation)",
                            inv.name, cash_pair.1.fiats_sum, cid
                        ));
                    }
                }
                // Record this as an ambiguous group for the frontend
                let inv_json: Vec<Value> = invs.iter().map(|e| json!({
                    "name": e.name,
                    "connection_id": e.connection_id,
                    "correlation_id": e.correlation_id,
                    "securities_count": e.securities_count,
                })).collect();
                let cash_json: Vec<Value> = cashes.iter().map(|(_, e)| json!({
                    "name": e.name,
                    "connection_id": e.connection_id,
                    "correlation_id": e.correlation_id,
                    "fiats_sum": e.fiats_sum,
                })).collect();
                ambiguous_groups.push(json!({
                    "connection_id": cid,
                    "investment_accounts": inv_json,
                    "cash_accounts": cash_json,
                    "needs_confirmation": true,
                }));
            }
        }
    }

    // Strategy 2: For investment accounts without connection_id, try institution name prefix
    for inv in &investment_entries {
        if result.contains_key(&inv.name) {
            continue; // Already matched
        }
        if inv.connection_id.is_some() {
            continue; // Had a connection_id but no cash match — skip prefix fallback
        }
        if inv.institution_name.is_empty() {
            continue;
        }
        // Find unmatched cash accounts whose institution name starts with same prefix
        let inv_prefix = inv.institution_name.split_whitespace().next().unwrap_or_default().to_lowercase();
        let mut prefix_cash_sum = 0.0;
        for (idx, ce) in cash_entries.iter().enumerate() {
            if matched_cash_indices.contains(&idx) {
                continue;
            }
            let ce_prefix = ce.institution_name.split_whitespace().next().unwrap_or_default().to_lowercase();
            if !inv_prefix.is_empty() && inv_prefix == ce_prefix {
                prefix_cash_sum += ce.fiats_sum;
                matched_cash_indices.insert(idx);
            }
        }
        if prefix_cash_sum > 0.0 {
            result.insert(inv.name.clone(), prefix_cash_sum);
            crate::debug_log(&format!(
                "finary_cash_mapping: matched '{}' → cash {:.2} (institution prefix '{}')",
                inv.name, prefix_cash_sum, inv_prefix
            ));
        }
    }

    // Log unmatched cash accounts
    for (idx, ce) in cash_entries.iter().enumerate() {
        if !matched_cash_indices.contains(&idx) {
            crate::debug_log(&format!(
                "finary_cash_mapping: unmatched cash account '{}' ({:.2}€, connection_id={:?})",
                ce.name, ce.fiats_sum, ce.connection_id
            ));
        }
    }

    // Also add any accounts that are BOTH investment + cash (fiats on the investment account itself)
    for inv in &investment_entries {
        if inv.fiats_sum > 0.0 {
            *result.entry(inv.name.clone()).or_insert(0.0) += inv.fiats_sum;
            crate::debug_log(&format!(
                "finary_cash_mapping: '{}' has direct fiats {:.2} (investment account with cash)",
                inv.name, inv.fiats_sum
            ));
        }
    }

    CashMappingResult { mapping: result, ambiguous_groups }
}

/// Derive a short, human-readable ticker from the security name when Finary
/// doesn't provide a symbol. Falls back to ISIN if name is empty.
fn derive_ticker_from_name(name: &str, isin: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return isin.to_uppercase();
    }
    let skip: &[&str] = &["LA", "LE", "LES", "DE", "DU", "DES", "ET", "SA", "SE", "NV"];
    let upper = name.to_uppercase();
    let words: Vec<&str> = upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let ticker = words
        .iter()
        .find(|w| w.len() > 1 && !skip.contains(w))
        .or(words.first())
        .copied()
        .unwrap_or("UNKNOWN");
    if ticker.len() > 12 {
        ticker[..12].to_string()
    } else {
        ticker.to_string()
    }
}

fn map_security_to_position(row: &Value) -> Value {
    let security = row.get("security").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let account = row.get("account").or_else(|| row.get("bank_account"))
        .and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let quantity = row.get("quantity").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let buying_price = row.get("buying_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let current_value = row.get("current_value").and_then(|v| v.as_f64())
        .unwrap_or_else(|| row.get("current_price").and_then(|v| v.as_f64()).unwrap_or(0.0) * quantity);
    let current_price = row.get("current_price").and_then(|v| v.as_f64())
        .unwrap_or_else(|| if quantity > 0.0 { current_value / quantity } else { 0.0 });
    let gain = row.get("current_upnl").and_then(|v| v.as_f64())
        .unwrap_or_else(|| current_value - buying_price * quantity);
    let gain_pct = row.get("current_upnl_percent").and_then(|v| v.as_f64())
        .unwrap_or_else(|| if buying_price > 0.0 { ((current_price / buying_price) - 1.0) * 100.0 } else { 0.0 });
    let symbol = security.get("symbol").and_then(|v| v.as_str()).unwrap_or_default().trim();
    let isin = security.get("isin").and_then(|v| v.as_str()).unwrap_or_default();
    let nom = security.get("name").and_then(|v| v.as_str()).unwrap_or(symbol);
    let compte = account.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let ticker = if !symbol.is_empty() {
        symbol.to_string()
    } else {
        // Derive a short ticker from the security name instead of using raw ISIN
        derive_ticker_from_name(nom, isin)
    };
    serde_json::to_value(crate::models::Position {
        ticker,
        nom: nom.to_string(),
        isin: if isin.is_empty() { None } else { Some(isin.to_string()) },
        quantite: quantity,
        prix_actuel: current_price,
        prix_revient: buying_price,
        valeur_actuelle: current_value,
        plus_moins_value: gain,
        plus_moins_value_pct: gain_pct,
        compte: if compte.is_empty() { "Unknown".to_string() } else { compte.to_string() },
        position_type: None,
    }).unwrap_or_default()
}

fn patch_source_ingestion_failed(run_id: &str, portfolio_source: &str, error: &anyhow::Error) -> Result<()> {
    patch_run_state_direct_with(run_id, |run_state| {
        if let Some(object) = run_state.as_object_mut() {
            object.insert(
                "source_ingestion".to_string(),
                json!({
                    "mode": portfolio_source,
                    "status": "failed",
                    "error_code": infer_issue_code(error),
                    "error_message": error.to_string(),
                    "updated_at": now_iso_string()
                }),
            );
        }
    })?;
    Ok(())
}

fn resolve_native_snapshot(
    run_id: &str,
    run_state: &Value,
    options: Option<&Value>,
    request_fn: HttpRequestFn,
) -> Result<(Value, String, Value)> {
    let portfolio_source = as_text(run_state.get("portfolio_source"));
    if portfolio_source == "csv" {
        if let Some(snapshot) = options.and_then(|value| value.get("uploaded_snapshot")).filter(|value| value.is_object()) {
            return Ok((normalize_csv_snapshot(snapshot), "success".to_string(), json!({
                "used_latest_snapshot": false,
                "latest_snapshot_saved_at": null,
                "degradation_reason": null
            })));
        }
        // Try csv_upload.csv_text — raw CSV text from the file picker.
        // The file picker concatenates multiple files with "--- FILE: name ---" separators.
        let csv_text = options
            .and_then(|v| v.get("csv_upload"))
            .and_then(|v| v.get("csv_text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();
        if !csv_text.is_empty() {
            let account = as_text(run_state.get("account"));
            let snapshot = parse_csv_upload_text(csv_text, &account)?;
            return Ok((normalize_csv_snapshot(&snapshot), "success".to_string(), json!({
                "used_latest_snapshot": false,
                "latest_snapshot_saved_at": null,
                "degradation_reason": null
            })));
        }

        let latest_export = as_text(
            options
                .and_then(|value| value.get("latest_export"))
                .or_else(|| run_state.get("latest_export")),
        );
        if latest_export.is_empty() {
            return Err(anyhow!("csv_input_missing"));
        }
        let account = as_text(run_state.get("account"));
        return Ok((load_csv_export_snapshot(&latest_export, &account)?, "success".to_string(), json!({
            "used_latest_snapshot": false,
            "latest_snapshot_saved_at": null,
            "degradation_reason": null
        })));
    }

    if let Some(snapshot) = options.and_then(|value| value.get("finary_snapshot")).filter(|value| value.is_object()) {
        let normalized = normalize_finary_snapshot(snapshot);
        persist_local_finary_snapshot(&normalized)?;
        return Ok((normalized, "success".to_string(), json!({
            "used_latest_snapshot": false,
            "latest_snapshot_saved_at": null,
            "degradation_reason": null
        })));
    }

    let latest_cached = get_latest_local_finary_snapshot()?;
    let run_on_latest = options
        .and_then(|value| value.get("run_on_latest_finary_snapshot"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    if !run_on_latest {
        if let Some(entry) = latest_cached.as_ref() {
            let saved_at = as_text(entry.get("saved_at"));
            let cached_positions = entry.get("snapshot")
                .and_then(|s| s.get("positions"))
                .and_then(|p| p.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            // Only reuse same-day cache if it has positions — skip empty snapshots
            if !saved_at.is_empty() && same_local_day(&saved_at) && cached_positions > 0 {
                let snapshot = entry.get("snapshot").cloned().unwrap_or_else(|| json!({}));
                return Ok((snapshot, "success".to_string(), json!({
                    "used_latest_snapshot": true,
                    "latest_snapshot_saved_at": saved_at,
                    "degradation_reason": null
                })));
            }
        }
        match fetch_finary_snapshot(run_id, request_fn) {
            Ok(snapshot) => {
                let normalized = normalize_finary_snapshot(&snapshot);
                persist_local_finary_snapshot(&normalized)?;
                Ok((normalized, "success".to_string(), json!({
                    "used_latest_snapshot": false,
                    "latest_snapshot_saved_at": null,
                    "degradation_reason": null
                })))
            }
            Err(error) => {
                // Only fall back to cached snapshot if it has positions
                let usable_cached = latest_cached.filter(|entry| {
                    entry.get("snapshot")
                        .and_then(|s| s.get("positions"))
                        .and_then(|p| p.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false)
                });
                if let Some(entry) = usable_cached {
                    let snapshot = entry.get("snapshot").cloned().unwrap_or_else(|| json!({}));
                    let saved_at = as_text(entry.get("saved_at"));
                    Ok((snapshot, "degraded".to_string(), json!({
                        "used_latest_snapshot": true,
                        "latest_snapshot_saved_at": saved_at,
                        "degradation_reason": infer_issue_code(&error)
                    })))
                } else {
                    Err(error)
                }
            }
        }
    } else if let Some(entry) = latest_cached.filter(|e| {
        e.get("snapshot").and_then(|s| s.get("positions")).and_then(|p| p.as_array()).map(|a| !a.is_empty()).unwrap_or(false)
    }) {
        let saved_at = as_text(entry.get("saved_at"));
        let snapshot = entry.get("snapshot").cloned().unwrap_or_else(|| json!({}));
        Ok((snapshot, "degraded".to_string(), json!({
            "used_latest_snapshot": true,
            "latest_snapshot_saved_at": saved_at,
            "degradation_reason": "latest_finary_snapshot_requested"
        })))
    } else {
        Err(anyhow!("source_snapshot_not_found:finary_local_default"))
    }
}

fn apply_collection_result(
    result: crate::native_collection_dispatch::CollectionResult,
    snapshot: &Value,
    positions_by_index: &mut [Option<Value>],
    incremental_positions: &mut Vec<Value>,
    market_by_ticker: &mut Map<String, Value>,
    news_by_ticker: &mut Map<String, Value>,
    collection_issues: &mut Vec<Value>,
    failures: &mut Vec<Value>,
    hydration_totals: &mut Value,
    portfolio_source: &str,
    source_ingestion_status: &str,
    source_details: &Value,
    total_lines: i64,
    run_id: &str,
    mcp_dispatch: &mut crate::native_mcp_analysis::McpBatchDispatchQueue,
    collection_completed: usize,
) -> Result<()> {
    let ticker = result.ticker;
    let name = result.name;
    for issue in &result.issues {
        let typed = crate::models::EnrichmentIssue::from_raw(&ticker, &name, issue, true);
        failures.push(serde_json::to_value(typed.as_failure()).unwrap_or_default());
        collection_issues.push(serde_json::to_value(typed).unwrap_or_default());
    }
    for key in [
        "tickers_hydrated",
        "banned_articles_filtered",
        "seen_articles_filtered",
        "global_banned_articles_filtered",
        "total_articles_filtered",
    ] {
        let current = hydration_totals.get(key).and_then(|value| value.as_i64()).unwrap_or(0);
        let delta = result
            .hydration_diag
            .get(key)
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        if let Some(object) = hydration_totals.as_object_mut() {
            object.insert(key.to_string(), json!(current + delta));
        }
    }
    if result
        .quality
        .get("needs_enrichment")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        collection_issues.push(json!({
            "type": "data_quality_weak",
            "ticker": ticker.clone(),
            "nom": name.clone(),
            "reasons": result.quality.get("reasons").cloned().unwrap_or_else(|| json!([])),
            "missing_market_fundamentals": result
                .quality
                .get("missing_market_fundamentals")
                .cloned()
                .unwrap_or_else(|| json!([])),
            "news_quality_score": result.quality.get("news_quality_score").cloned().unwrap_or_else(|| json!(0)),
            "at": now_iso_string()
        }));
    }
    if result.index < positions_by_index.len() {
        positions_by_index[result.index] = Some(result.hydrated_row.clone());
    }
    incremental_positions.clear();
    incremental_positions.extend(positions_by_index.iter().filter_map(|row| row.clone()));
    if !ticker.is_empty() {
        market_by_ticker.insert(ticker.clone(), result.market_row.clone());
        news_by_ticker.insert(ticker.clone(), result.news_row.clone());
    }
    let partial_collection_state = build_collection_state(
        snapshot,
        incremental_positions,
        market_by_ticker,
        news_by_ticker,
        &Value::Null,
        collection_issues,
        &[],
        portfolio_source,
        source_ingestion_status,
        source_details,
        &Value::Null,
    );
    crate::native_line_analysis::persist_native_collection_state(run_id, &partial_collection_state)?;
    let collection_progress = json!({
        "completed": collection_completed,
        "total": total_lines
    });
    set_native_run_stage(
        run_id,
        "collecting_data",
        Some(collection_progress.clone()),
        Some(json!({
            "completed": load_run_by_id_direct(run_id)?
                .get("pending_recommandations")
                .and_then(|value| value.as_array())
                .map(|rows| rows.len())
                .unwrap_or(0),
            "total": total_lines
        })),
    )?;
    let _ = crate::update_line_status(run_id, &ticker, "analyzing");
    let name = result.hydrated_row.get("nom").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let line_type = result.hydrated_row.get("type").and_then(|v| v.as_str()).unwrap_or("position").to_string();
    mcp_dispatch.push(crate::native_mcp_analysis::McpLinePacket {
        ticker, nom: name, line_type,
    })?;
    Ok(())
}

pub(crate) fn execute_native_local_analysis_workflow_with(
    options: Option<Value>,
    llm_token: Option<&str>,
    request_fn: HttpRequestFn,
) -> Result<Value> {
    let payload = options.unwrap_or_else(|| json!({}));
    let run_id = as_text(payload.get("run_id"));
    if run_id.is_empty() {
        return Err(anyhow!("analysis_run_start_local_failed:missing_run_id"));
    }
    let run_state = load_run_by_id_direct(&run_id)?;
    let portfolio_source = as_text(run_state.get("portfolio_source"));
    let (snapshot, source_ingestion_status, source_details) =
        match resolve_native_snapshot(&run_id, &run_state, Some(&payload), request_fn) {
            Ok(result) => result,
            Err(error) => {
                patch_source_ingestion_failed(&run_id, &portfolio_source, &error)?;
                return Err(error);
            }
        };

    let target_account = as_text(run_state.get("account"));
    if target_account.is_empty() {
        return Err(anyhow!("analysis_run_start_local_failed:account_required:select an account before running analysis"));
    }
    // __force_account: rename all positions to the target account (user chose "use anyway")
    let force_account = payload
        .get("__force_account")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let positions: Vec<Value> = if !force_account.is_empty() {
        as_array(snapshot.get("positions"))
            .iter()
            .map(|row| {
                let mut p = row.clone();
                if let Some(obj) = p.as_object_mut() {
                    obj.insert("compte".to_string(), json!(force_account.as_str()));
                }
                p
            })
            .collect()
    } else {
        as_array(snapshot.get("positions"))
            .iter()
            .filter(|row| as_text(row.get("compte")) == target_account)
            .cloned()
            .collect()
    };
    if positions.is_empty() {
        // Collect the distinct account names found in the snapshot
        let all_positions = as_array(snapshot.get("positions"));
        let mut found_accounts: Vec<String> = all_positions
            .iter()
            .map(|p| as_text(p.get("compte")))
            .filter(|a| !a.is_empty())
            .collect();
        found_accounts.sort();
        found_accounts.dedup();
        let accounts_json = serde_json::to_string(&found_accounts).unwrap_or_default();
        return Err(anyhow!(
            "account_mismatch:{target_account}:{accounts_json}:{} positions available",
            all_positions.len()
        ));
    }
    // Scope portfolio totals to the selected account
    let account_entry = snapshot.get("accounts")
        .and_then(|v| v.as_array())
        .and_then(|accounts| accounts.iter().find(|a| as_text(a.get("name")) == target_account))
        .cloned();
    let account_cash = account_entry.as_ref()
        .and_then(|a| a.get("cash")).and_then(|v| v.as_f64())
        .unwrap_or_else(|| snapshot.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0));
    let account_value: f64 = positions.iter()
        .map(|p| p.get("valeur_actuelle").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum::<f64>() + account_cash;
    let account_gain: f64 = positions.iter()
        .map(|p| p.get("plus_moins_value").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum();
    let mut snapshot = snapshot;
    if let Some(obj) = snapshot.as_object_mut() {
        obj.insert("liquidites".to_string(), json!(account_cash));
        obj.insert("valeur_totale".to_string(), json!(account_value));
        obj.insert("plus_value_totale".to_string(), json!(account_gain));
    }
    let run_mode = as_text(run_state.get("run_mode"));
    let run_mode = if run_mode.is_empty() { "full_run".to_string() } else { run_mode };

    // ── refresh_synthesis mode: skip line analysis, inject previous recommendations ──
    if run_mode == "refresh_synthesis" {
        return crate::native_collection_modes::execute_refresh_synthesis_mode(
            &run_id, &snapshot, &positions, &portfolio_source, &source_ingestion_status, &source_details,
            &payload, llm_token, request_fn,
        );
    }

    // ── retry_failed mode: re-analyze only failed/aborted lines ──
    let positions = if run_mode == "retry_failed" {
        let (prev_recs, prev_line_status) = crate::run_state::load_previous_run_data(&run_id);
        // Inject existing good recommendations into new run state
        let _ = patch_run_state_direct_with(&run_id, |rs| {
            if let Some(obj) = rs.as_object_mut() {
                obj.insert("pending_recommandations".to_string(), json!(prev_recs));
            }
        });
        // Filter to only positions that failed/aborted in previous run
        let failed_tickers: std::collections::HashSet<String> = prev_line_status
            .as_object()
            .map(|map| {
                map.iter()
                    .filter(|(_, v)| {
                        let status = v.as_str().unwrap_or(
                            v.get("status").and_then(|s| s.as_str()).unwrap_or("")
                        );
                        status == "failed" || status == "aborted" || status == "error"
                    })
                    .map(|(k, _)| k.to_uppercase())
                    .collect()
            })
            .unwrap_or_default();
        if failed_tickers.is_empty() {
            eprintln!("[run_mode] retry_failed: no failed lines found, running full analysis");
            positions
        } else {
            eprintln!("[run_mode] retry_failed: re-analyzing {} failed lines: {:?}", failed_tickers.len(), failed_tickers);
            // Remove failed recommendations from pending
            let _ = patch_run_state_direct_with(&run_id, |rs| {
                if let Some(obj) = rs.as_object_mut() {
                    if let Some(recs) = obj.get("pending_recommandations").and_then(|v| v.as_array()).cloned() {
                        let filtered: Vec<Value> = recs.into_iter()
                            .filter(|r| {
                                let t = as_text(r.get("ticker")).to_uppercase();
                                !failed_tickers.contains(&t)
                            })
                            .collect();
                        obj.insert("pending_recommandations".to_string(), Value::Array(filtered));
                    }
                }
            });
            positions.into_iter()
                .filter(|p| {
                    let t = as_text(p.get("ticker")).to_uppercase();
                    failed_tickers.contains(&t)
                })
                .collect()
        }
    } else {
        positions
    };

    let total_lines = positions.len() as i64;
    let collection_progress_zero = json!({ "completed": 0, "total": total_lines });
    let line_progress_zero = json!({ "completed": 0, "total": total_lines });
    set_native_run_stage(
        &run_id,
        "collecting_data",
        Some(collection_progress_zero.clone()),
        Some(line_progress_zero.clone()),
    )?;

    let line_memory_store = Arc::new(read_line_memory_store()?);
    let news_quality_threshold = payload
        .get("news_quality_threshold")
        .and_then(|value| value.as_i64())
        .unwrap_or(60);
    let max_missing_market_fields = payload
        .get("max_missing_market_fields")
        .and_then(|value| value.as_u64())
        .unwrap_or(2) as usize;
    let collection_concurrency = std::cmp::max(
        1,
        crate::runtime_setting_integer_direct("collection_concurrency", 2),
    )
    .clamp(1, 12) as usize;
    let collection_throttle_ms =
        std::cmp::max(0, crate::runtime_setting_integer_direct("collection_throttle_ms", 40)) as u64;

    let mut positions_by_index = vec![None; positions.len()];
    let mut incremental_positions = Vec::new();
    let mut market_by_ticker = Map::new();
    let mut news_by_ticker = Map::new();
    let mut collection_issues = Vec::new();
    let mut failures = Vec::new();
    let mut hydration_totals = json!({
        "tickers_hydrated": 0,
        "banned_articles_filtered": 0,
        "seen_articles_filtered": 0,
        "global_banned_articles_filtered": 0,
        "total_articles_filtered": 0
    });
    let mcp_batch_size = if crate::llm_backend::current_backend_name() != "codex" {
        // Native backend: each line runs as its own API call with clean context.
        // batch_size=1 means pool_size (line_analysis_concurrency) controls total parallelism.
        1
    } else {
        crate::runtime_setting_integer_direct("mcp_batch_size", 5).max(1) as usize
    };
    let data_dir = crate::resolve_runtime_state_dir()
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let mut mcp_dispatch = crate::native_mcp_analysis::McpBatchDispatchQueue::new(
        &run_id, &data_dir, mcp_batch_size,
    );
    let mut collection_dispatch = NativeCollectionDispatchQueue::new(
        collection_concurrency,
        collection_throttle_ms,
        Arc::clone(&line_memory_store),
        news_quality_threshold,
        max_missing_market_fields,
        request_fn,
    );
    let mut collection_completed = 0usize;

    // Initialize all positions as "waiting"
    for row in &positions {
        let ticker = as_text(row.get("ticker"));
        if !ticker.is_empty() {
            let _ = crate::update_line_status(&run_id, &ticker, "waiting");
        }
    }

    // Launch watchlist suggestion in background while positions collect
    let wl_positions = positions.clone();
    let wl_snapshot = snapshot.clone();
    let wl_run_id = run_id.clone();
    let wl_guidelines = run_state.get("agent_guidelines").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let wl_account = as_text(run_state.get("account"));
    let watchlist_handle = std::thread::spawn(move || {
        let portfolio_summary = json!({
            "valeur_totale": wl_snapshot.get("valeur_totale").cloned().unwrap_or_else(|| json!(0.0)),
            "liquidites": wl_snapshot.get("liquidites").cloned().unwrap_or_else(|| json!(0.0)),
            "plus_value_totale": wl_snapshot.get("plus_value_totale").cloned().unwrap_or_else(|| json!(0.0)),
        });
        match crate::llm::generate_watchlist_suggestions(&wl_positions, &portfolio_summary, &wl_guidelines, &wl_account) {
            Ok(items) if !items.is_empty() => {
                eprintln!("[watchlist] LLM suggested {} items (background)", items.len());
                // Save to user preferences
                let mut prefs = crate::runtime_settings::get_user_preferences();
                if let Some(obj) = prefs.as_object_mut() {
                    let wl = obj.entry("watchlist_by_account".to_string()).or_insert_with(|| json!({}));
                    if let Some(wl_obj) = wl.as_object_mut() {
                        wl_obj.insert(wl_account.clone(), json!(&items));
                    }
                }
                let _ = crate::runtime_settings::save_user_preferences(&prefs);
                let _ = patch_run_state_direct_with(&wl_run_id, |rs| {
                    if let Some(obj) = rs.as_object_mut() {
                        obj.insert("watchlist".to_string(), json!({ "items": &items }));
                    }
                });
                Some((items, wl_account))
            }
            Ok(_) => { eprintln!("[watchlist] LLM returned empty watchlist"); None }
            Err(e) => { eprintln!("[watchlist] generation failed (non-blocking): {e}"); None }
        }
    });

    // Collect + analyze positions
    for (index, row) in positions.iter().enumerate() {
        if crate::analysis_ops::is_any_operation_cancelled_for_run(&run_id) {
            return Err(anyhow!("run_aborted:analysis stopped by user"));
        }
        let ticker = as_text(row.get("ticker"));
        if !ticker.is_empty() {
            let _ = crate::update_line_status(&run_id, &ticker, "collecting");
        }
        collection_dispatch.push(index, row.clone())?;
        for result in collection_dispatch.drain_ready() {
            collection_completed += 1;
            apply_collection_result(
                result,
                &snapshot,
                &mut positions_by_index,
                &mut incremental_positions,
                &mut market_by_ticker,
                &mut news_by_ticker,
                &mut collection_issues,
                &mut failures,
                &mut hydration_totals,
                &portfolio_source,
                &source_ingestion_status,
                &source_details,
                total_lines,
                &run_id,
                &mut mcp_dispatch,
                collection_completed,
            )?;
            if crate::run_state_cache::should_flush() {
                crate::run_state_cache::flush_to_disk();
            }
        }
    }
    while collection_completed < positions.len() {
        if crate::analysis_ops::is_any_operation_cancelled_for_run(&run_id) {
            return Err(anyhow!("run_aborted:analysis stopped by user"));
        }
        let result = collection_dispatch.recv_blocking()?;
        collection_completed += 1;
        apply_collection_result(
            result,
            &snapshot,
            &mut positions_by_index,
            &mut incremental_positions,
            &mut market_by_ticker,
            &mut news_by_ticker,
            &mut collection_issues,
            &mut failures,
            &mut hydration_totals,
            &portfolio_source,
            &source_ingestion_status,
            &source_details,
            total_lines,
            &run_id,
            &mut mcp_dispatch,
            collection_completed,
        )?;
        if crate::run_state_cache::should_flush() {
            crate::run_state_cache::flush_to_disk();
        }
    }

    // Positions done — join watchlist thread and inject items into same pipeline
    if let Ok(Some((watchlist_items, account))) = watchlist_handle.join() {
        let wl_count = watchlist_items.len();
        let new_total = (positions.len() + wl_count) as i64;
        // Update total in progress counters
        set_native_run_stage(
            &run_id,
            "collecting_data",
            Some(json!({ "completed": positions.len(), "total": new_total })),
            Some(json!({ "completed": collection_completed, "total": new_total })),
        )?;
        // Push watchlist items through the SAME collection + line dispatch
        for item in &watchlist_items {
            if crate::analysis_ops::is_any_operation_cancelled_for_run(&run_id) { break; }
            let ticker = as_text(item.get("ticker"));
            if ticker.is_empty() { continue; }
            let wl_row = json!({
                "ticker": ticker.to_uppercase(), "nom": as_text(item.get("nom")),
                "isin": as_text(item.get("isin")), "type": "watchlist",
                "quantite": 0, "prix_actuel": 0, "valeur_actuelle": 0,
                "prix_revient": 0, "plus_moins_value": 0, "plus_moins_value_pct": 0,
                "compte": account,
            });
            let _ = crate::update_line_status(&run_id, &ticker.to_uppercase(), "collecting");
            collection_dispatch.push(positions.len() + collection_completed - positions.len(), wl_row)?;
            for result in collection_dispatch.drain_ready() {
                collection_completed += 1;
                let wl_ticker = result.ticker.clone();
                market_by_ticker.insert(wl_ticker.clone(), result.market_row.clone());
                news_by_ticker.insert(wl_ticker.clone(), result.news_row.clone());
                let _ = crate::update_line_status(&run_id, &wl_ticker, "analyzing");
                let nom = as_text(result.hydrated_row.get("nom"));
                mcp_dispatch.push(crate::native_mcp_analysis::McpLinePacket {
                    ticker: wl_ticker, nom, line_type: "watchlist".to_string(),
                })?;
                if crate::run_state_cache::should_flush() {
                    crate::run_state_cache::flush_to_disk();
                }
            }
        }
        // Drain remaining watchlist collection results
        while collection_completed < positions.len() + wl_count {
            if crate::analysis_ops::is_any_operation_cancelled_for_run(&run_id) { break; }
            if let Ok(result) = collection_dispatch.recv_blocking() {
                collection_completed += 1;
                let wl_ticker = result.ticker.clone();
                market_by_ticker.insert(wl_ticker.clone(), result.market_row.clone());
                news_by_ticker.insert(wl_ticker.clone(), result.news_row.clone());
                let _ = crate::update_line_status(&run_id, &wl_ticker, "analyzing");
                let nom = as_text(result.hydrated_row.get("nom"));
                mcp_dispatch.push(crate::native_mcp_analysis::McpLinePacket {
                    ticker: wl_ticker, nom, line_type: "watchlist".to_string(),
                })?;
                if crate::run_state_cache::should_flush() {
                    crate::run_state_cache::flush_to_disk();
                }
            }
        }
    }

    // Flush remaining batched lines and wait for all MCP turns to complete
    mcp_dispatch.flush_pending()?;
    mcp_dispatch.join_all()?;
    crate::run_state_cache::flush_to_disk();
    crate::run_state_cache::clear_run(&run_id);

    let quality = diagnose_run_quality(
        &market_by_ticker,
        &news_by_ticker,
        &incremental_positions,
        news_quality_threshold,
        max_missing_market_fields,
    );
    let final_collection_state = build_collection_state(
        &snapshot,
        &incremental_positions,
        &market_by_ticker,
        &news_by_ticker,
        &quality,
        &collection_issues,
        &failures,
        &portfolio_source,
        &source_ingestion_status,
        &source_details,
        &hydration_totals,
    );
    crate::native_line_analysis::persist_native_collection_state(&run_id, &final_collection_state)?;
    Ok(json!({
        "ok": true,
        "action": "analysis:run-start-local",
        "result": {
            "ok": true,
            "run_id": run_id,
            "collection": {
                "ok": true,
                "run_id": run_id,
                "positions_count": positions.len(),
                "transactions_count": as_array(snapshot.get("transactions")).len(),
                "orders_count": as_array(snapshot.get("orders")).len(),
                "weak_tickers": quality.get("weak_tickers").cloned().unwrap_or_else(|| json!([])),
                "source_mode": portfolio_source,
                "ingestion_status": source_ingestion_status,
                "enrichment_failures_count": failures.len(),
                "collection_issues_count": collection_issues.len(),
                "collection_state": final_collection_state
            },
            "generated_draft": null,
            "report": null,
            "line_memory_sync": null,
            "degraded": false,
            "degradation_reason": null,
            "finalization_delegated": true
        }
    }))
}

// Refresh+synthesis mode implementation moved to native_collection_modes.rs

fn native_request_fn(
    method: &str,
    host: &str,
    port: u16,
    path: &str,
    body: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<Value> {
    // Enrichment: native Rust
    if method == "GET" && path.starts_with("/market/spot?") {
        let params = parse_query_params(path);
        let ticker = params.get("ticker").map(|s| s.as_str()).unwrap_or_default();
        let name = params.get("name").map(|s| s.as_str()).unwrap_or_default();
        let isin = params.get("isin").map(|s| s.as_str()).unwrap_or_default();
        return crate::enrichment::fetch_market_spot(ticker, name, isin);
    }
    if method == "GET" && path.starts_with("/news?") {
        let params = parse_query_params(path);
        let ticker = params.get("ticker").map(|s| s.as_str()).unwrap_or_default();
        let name = params.get("name").map(|s| s.as_str()).unwrap_or_default();
        let isin = params.get("isin").map(|s| s.as_str()).unwrap_or_default();
        return crate::enrichment::fetch_news(ticker, name, isin);
    }
    if method == "GET" && path.starts_with("/insights?") {
        let params = parse_query_params(path);
        let ticker = params.get("ticker").map(|s| s.as_str()).unwrap_or_default();
        let isin = params.get("isin").map(|s| s.as_str()).unwrap_or_default();
        return crate::enrichment::fetch_shared_insights(ticker, isin);
    }
    // LLM line analysis: native Rust
    if method == "POST" && path == "/v1/line/analyze" {
        if let Some(body_str) = body {
            let parsed = serde_json::from_str::<Value>(body_str)?;
            let line_context = parsed.get("line_context").cloned().unwrap_or_else(|| json!({}));
            let run_state = json!({
                "run_id": parsed.get("run_id").cloned().unwrap_or(serde_json::Value::Null),
                "portfolio": parsed.get("report_context").and_then(|r| r.get("portfolio")).cloned().unwrap_or_else(|| json!({})),
                "agent_guidelines": parsed.get("agent_guidelines").cloned()
            });
            let guidelines = parsed.get("agent_guidelines").and_then(|v| v.as_str()).unwrap_or_default();
            let guidelines_ref = if guidelines.is_empty() { None } else { Some(guidelines) };
            let validation_context = parsed.get("validation_context").cloned();
            return crate::llm::generate_line_analysis(&line_context, &run_state, guidelines_ref, validation_context.as_ref());
        }
    }
    if method == "POST" && path == "/v1/report/generate" {
        if let Some(body_str) = body {
            let parsed = serde_json::from_str::<Value>(body_str)?;
            let run_state = json!({
                "portfolio": parsed.get("report_context").and_then(|r| r.get("portfolio")).cloned().unwrap_or_else(|| json!({})),
                "pending_recommandations": parsed.get("report_context").and_then(|r| r.get("line_recommendations")).cloned().unwrap_or_else(|| json!([]))
            });
            let run_id = parsed.get("run_id").and_then(|v| v.as_str()).unwrap_or("");
            return crate::llm::generate_report_draft(&run_state, run_id);
        }
    }
    // HTTP only for finary-connector snapshot/session calls
    request_http_json(method, host, port, path, body, timeout_ms)
}

fn parse_query_params(path: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(query) = path.split('?').nth(1) {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                map.insert(
                    key.to_string(),
                    urlencoding_decode(value),
                );
            }
        }
    }
    map
}

fn urlencoding_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h = chars.next().unwrap_or(b'0');
            let l = chars.next().unwrap_or(b'0');
            let decoded = u8::from_str_radix(&format!("{}{}", h as char, l as char), 16).unwrap_or(b'?');
            result.push(decoded as char);
        } else if b == b'+' {
            result.push(' ');
        } else {
            result.push(b as char);
        }
    }
    result
}

pub fn execute_native_local_analysis_workflow(
    options: Option<Value>,
    llm_token: Option<&str>,
) -> Result<Value> {
    execute_native_local_analysis_workflow_with(options, llm_token, native_request_fn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_account(name: &str, connection_id: Option<i64>, securities: usize, fiats_sum: f64, institution: &str) -> Value {
        let securities_arr: Vec<Value> = (0..securities).map(|_| json!({"symbol": "X"})).collect();
        let fiats_arr = if fiats_sum > 0.0 {
            vec![json!({"current_value": fiats_sum})]
        } else {
            vec![]
        };
        let mut obj = json!({
            "name": name,
            "securities": securities_arr,
            "fiats": fiats_arr,
            "institution": { "name": institution }
        });
        if let Some(cid) = connection_id {
            obj["connection_id"] = json!(cid);
        }
        obj
    }

    #[test]
    fn build_cash_mapping_empty_returns_empty() {
        let result = build_cash_mapping(&[]);
        assert!(result.mapping.is_empty(), "empty holdings should produce empty mapping");
        assert!(result.ambiguous_groups.is_empty());
    }

    #[test]
    fn build_cash_mapping_1_investment_1_cash_same_connection_direct_match() {
        let accounts = vec![
            make_account("PEA", Some(101), 3, 0.0, "Bourso"),
            make_account("Compte espèce PEA", Some(101), 0, 500.0, "Bourso"),
        ];
        let result = build_cash_mapping(&accounts);
        assert_eq!(result.mapping.get("PEA").copied(), Some(500.0),
            "1:1 connection group should auto-match");
        assert!(result.ambiguous_groups.is_empty(), "1:1 should not be flagged ambiguous");
    }

    #[test]
    fn build_cash_mapping_2_investments_2_cash_same_connection_flagged_ambiguous() {
        let accounts = vec![
            make_account("PEA", Some(200), 3, 0.0, "Bourso"),
            make_account("CTO", Some(200), 2, 0.0, "Bourso"),
            make_account("Cash PEA", Some(200), 0, 300.0, "Bourso"),
            make_account("Cash CTO", Some(200), 0, 150.0, "Bourso"),
        ];
        let result = build_cash_mapping(&accounts);
        // Ambiguous: 2 investments + 2 cash accounts in same connection — correlation heuristic used
        assert_eq!(result.ambiguous_groups.len(), 1,
            "N:M group should be recorded as ambiguous");
        let group = &result.ambiguous_groups[0];
        assert_eq!(group["connection_id"], 200);
        assert!(group["needs_confirmation"].as_bool().unwrap_or(false));
    }

    #[test]
    fn build_cash_mapping_no_connection_id_falls_through_to_institution_prefix() {
        let accounts = vec![
            make_account("PEA Boursorama", None, 3, 0.0, "Boursorama Banque"),
            make_account("Livret Boursorama", None, 0, 750.0, "Boursorama Banque"),
        ];
        let result = build_cash_mapping(&accounts);
        assert_eq!(result.mapping.get("PEA Boursorama").copied(), Some(750.0),
            "institution prefix match should assign cash to investment");
    }

    #[test]
    fn build_cash_mapping_investment_with_direct_fiats_adds_to_result() {
        // An investment account that has both securities AND fiats (cash on the investment account itself)
        let accounts = vec![
            // Account with securities and fiats (investment account with embedded cash)
            json!({
                "name": "PEA avec espèces",
                "securities": [{"symbol": "MC"}],
                "fiats": [{"current_value": 200.0}],
                "institution": {"name": "Bourso"}
            }),
        ];
        let result = build_cash_mapping(&accounts);
        // The investment account has direct fiats so it should appear in mapping
        assert_eq!(result.mapping.get("PEA avec espèces").copied(), Some(200.0),
            "investment with direct fiats should contribute cash from fiats field");
    }
}
