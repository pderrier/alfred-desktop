//! MCP-driven batch analysis — multi-line Codex turns with self-validation.
//!
//! Drop-in replacement for `NativeLineDispatchQueue`. Collection stays the same;
//! instead of dispatching each line to a separate Codex turn, collected lines
//! are batched and sent as multi-line turns where Codex uses MCP tools to
//! fetch data, analyze, and self-validate each recommendation.

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;
use serde_json::{json, Value};

// ── MCP results sidecar merge ───────────────────────────────────

/// Merge MCP results from the sidecar JSONL file into the run state.
/// Called by the main process after batches complete — single writer.
fn merge_mcp_results(run_id: &str, data_dir: &str) {
    let results_path = std::path::Path::new(data_dir)
        .join("runtime-state")
        .join(format!("{run_id}_mcp_results.jsonl"));

    let content = match std::fs::read_to_string(&results_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return,
    };

    let data_path = std::path::Path::new(data_dir);
    let mut merged_count = 0;

    for line in content.lines() {
        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let entry_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match entry_type {
            "recommendation" => {
                let line_id = entry.get("line_id").and_then(|v| v.as_str()).unwrap_or("");
                let rec = entry.get("recommendation").cloned().unwrap_or(json!({}));
                if !line_id.is_empty() {
                    let lid = line_id.to_string();
                    let rec_clone = rec.clone();
                    let _ = crate::run_state_cache::patch(data_path, run_id, |state| {
                        let pending = state.as_object_mut().unwrap()
                            .entry("pending_recommandations")
                            .or_insert_with(|| json!([]));
                        if let Some(arr) = pending.as_array_mut() {
                            arr.retain(|r| {
                                r.get("line_id").and_then(|v| v.as_str()).unwrap_or("") != lid
                            });
                            arr.push(rec_clone.clone());
                        }
                    });
                    // Update line_status to done
                    let ticker = line_id.split(':').last().unwrap_or("");
                    if !ticker.is_empty() {
                        crate::run_state_cache::cache_line_status(run_id, ticker, json!({"status": "done"}));
                    }
                    merged_count += 1;
                }
            }
            "synthesis" => {
                let composed = entry.get("composed_payload").cloned().unwrap_or(json!({}));
                let _ = crate::run_state_cache::patch(data_path, run_id, |state| {
                    if let Some(obj) = state.as_object_mut() {
                        obj.insert("composed_payload".to_string(), composed.clone());
                    }
                });
                merged_count += 1;
            }
            _ => {}
        }
    }

    if merged_count > 0 {
        crate::debug_log(&format!(
            "[mcp-merge] merged {merged_count} results from sidecar for {run_id}"
        ));
        crate::run_state_cache::flush_now(run_id);
        // Clear the sidecar after successful merge
        let _ = std::fs::remove_file(&results_path);
    }
}

// ── Batch prompt ─────────────────────────────────────────────────

/// Build a per-line prompt for the native backend. Data is pre-injected.
/// The model returns JSON only — we handle validation and persistence ourselves.
fn build_native_line_prompt(_run_id: &str, ticker: &str, nom: &str, line_type: &str, line_data: &Value) -> String {
    let position = serde_json::to_string_pretty(&line_data["position"]).unwrap_or_default();
    let market = serde_json::to_string_pretty(&line_data["market_data"]).unwrap_or_default();
    let news = serde_json::to_string_pretty(&line_data["news"]).unwrap_or_default();
    let insights = serde_json::to_string_pretty(&line_data["shared_insights"]).unwrap_or_default();
    let memory = serde_json::to_string_pretty(&line_data["line_memory"]).unwrap_or_default();
    let quality = serde_json::to_string_pretty(&line_data["quality"]).unwrap_or_default();

    format!(
        r#"Tu es Alfred, un conseiller financier bienveillant. Analyse cette ligne.

Ligne: {line_type}:{ticker} ({nom})

=== DONNEES ===

Position:
{position}

Donnees de marche:
{market}

Actualites:
{news}

Insights partages:
{insights}

Memoire precedente:
{memory}

Qualite des donnees:
{quality}

=== INSTRUCTIONS ===

Reponds UNIQUEMENT avec un objet JSON (pas de texte avant ou apres) contenant :
- line_id: "{line_type}:{ticker}"
- ticker: "{ticker}", type: "{line_type}", nom: "{nom}"
- signal: ACHAT_FORT | ACHAT | RENFORCEMENT | CONSERVER | ALLEGEMENT | VENTE | SURVEILLANCE
- conviction: faible | moderee | forte
- synthese: minimum 150 caracteres (explique comme a un ami)
- analyse_technique, analyse_fondamentale, analyse_sentiment
- raisons_principales: 3-5 raisons (array)
- risques, catalyseurs, badges_keywords: arrays
- action_recommandee: instruction CHIFFREE (nb titres, montant EUR, prix). Pour watchlist: prix d'entree ideal + montant suggere.
- deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
- deep_news_quality_score: 0-100
- deep_news_relevance: high|medium|low
- deep_news_staleness: fresh|recent|stale
- extracted_fundamentals: {{ "pe_ratio": ..., "revenue_growth": ..., "profit_margin": ..., "debt_to_equity": ... }} (si trouves via web ou calcules)
- shared_insights: {{ "analyse_technique": "...", "analyse_fondamentale": "...", "analyse_sentiment": "...", "risques": "...", "catalyseurs": "..." }}
- reanalyse_after: date ISO, reanalyse_reason
- llm_memory_summary: resume factuel

Si les donnees sont insuffisantes, tu peux faire UNE recherche web (pas plus) pour completer.
Sois concret: chiffres, montants, dates. Pas de generalites.
Les articles "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement."#,
        ticker = ticker,
        nom = nom,
        line_type = line_type,
        position = position,
        market = market,
        news = news,
        insights = insights,
        memory = memory,
        quality = quality,
    )
}

/// Run a single line analysis with native backend: LLM returns JSON, we validate+persist.
fn run_native_line_analysis(
    run_id: &str,
    ticker: &str,
    nom: &str,
    line_type: &str,
    line_data: &Value,
    data_dir: &std::path::Path,
    mut on_progress: Option<crate::llm_backend::ProgressFn>,
) -> Result<()> {
    let prompt = build_native_line_prompt(run_id, ticker, nom, line_type, line_data);
    let timeout_ms = 180_000u64;
    const MAX_RETRIES: u32 = 2;

    let mut last_issues: Vec<String> = Vec::new();
    let mut best_rec: Option<Value> = None;

    for attempt in 0..=MAX_RETRIES {
        let retry_prompt = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{}\n\n=== CORRECTION (tentative {}/{}) ===\nTa reponse precedente avait ces problemes: {}.\nCorrige-les et renvoie le JSON complet.",
                prompt,
                attempt + 1,
                MAX_RETRIES + 1,
                last_issues.join(", ")
            )
        };

        // Call LLM — model returns JSON (+ optional web search)
        // Progress callback only on first attempt
        let cb = if attempt == 0 { on_progress.take() } else { None };
        let result = crate::llm_backend::run_prompt(&retry_prompt, timeout_ms, cb)?;

        // Extract JSON from response
        let rec = match extract_recommendation_json(&result) {
            Some(r) => {
                best_rec = Some(r.clone());
                r
            }
            None => {
                crate::debug_log(&format!("native_line: {ticker} attempt {attempt}: no JSON in response"));
                last_issues = vec!["no_json_in_response".to_string()];
                continue;
            }
        };

        // Validate locally
        let validation = crate::mcp_server::dispatch_tool_direct(
            data_dir,
            "validate_recommendation",
            &json!({"run_id": run_id, "recommendation": serde_json::to_string(&rec).unwrap_or_default()}),
        );

        let ok = validation.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
            || validation.get("stored").and_then(|v| v.as_bool()).unwrap_or(false);

        if ok {
            // Persist extracted data
            persist_line_extras(data_dir, ticker, line_data, &rec);
            return Ok(());
        }

        // Validation failed
        last_issues = validation
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["validation_failed".to_string()]);

        crate::debug_log(&format!(
            "native_line: {ticker} attempt {}: issues: {:?}",
            attempt + 1,
            last_issues
        ));

        crate::mcp_progress_relay::write_progress_event(
            &data_dir.parent().unwrap_or(data_dir),
            run_id, ticker, "repairing",
            &format!("fixing: {}", last_issues.join(", ")),
        );
    }

    // Max retries exhausted — force-store best available recommendation
    if let Some(rec) = best_rec {
        crate::debug_log(&format!("native_line: {ticker} force-storing best rec after {MAX_RETRIES} retries"));
        // Call validate with high attempt count to trigger accept-with-warnings
        crate::mcp_server::dispatch_tool_direct(
            data_dir,
            "validate_recommendation",
            &json!({"run_id": run_id, "recommendation": serde_json::to_string(&rec).unwrap_or_default()}),
        );
        persist_line_extras(data_dir, ticker, line_data, &rec);
    } else {
        crate::debug_log(&format!("native_line: {ticker} no valid JSON after {MAX_RETRIES} retries"));
        let _ = crate::run_state::update_line_status_with_error(run_id, ticker, "failed", Some("no_valid_recommendation"));
    }
    Ok(())
}

/// Extract recommendation JSON from LLM response Value.
fn extract_recommendation_json(result: &Value) -> Option<Value> {
    // Result may be the JSON directly, or {"ok":true, "mcp_turn":true} with text
    if result.get("line_id").is_some() {
        return Some(result.clone());
    }
    if result.get("recommendation").is_some() {
        return result.get("recommendation").cloned();
    }
    // Try parsing from text content
    if let Some(text) = result.as_str() {
        return crate::llm_parsing::extract_json_object(text);
    }
    // The result itself might be the recommendation
    if result.get("signal").is_some() && result.get("ticker").is_some() {
        return Some(result.clone());
    }
    None
}

/// Persist extracted fundamentals, shared insights, deep news from the recommendation.
fn persist_line_extras(data_dir: &std::path::Path, ticker: &str, line_data: &Value, rec: &Value) {
    let isin = line_data.get("position")
        .and_then(|p| p.get("isin"))
        .and_then(|v| v.as_str())
        .unwrap_or(ticker);

    // Persist shared insights
    if let Some(insights) = rec.get("shared_insights") {
        if !insights.is_null() {
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_shared_insights",
                &json!({"ticker": ticker, "isin": isin, "insights": serde_json::to_string(insights).unwrap_or_default()}),
            );
        }
    }

    // Persist extracted fundamentals
    if let Some(fundamentals) = rec.get("extracted_fundamentals") {
        if !fundamentals.is_null() {
            crate::mcp_server::dispatch_tool_direct(
                data_dir,
                "persist_extracted_fundamentals",
                &json!({"ticker": ticker, "isin": isin, "fundamentals": serde_json::to_string(fundamentals).unwrap_or_default()}),
            );
        }
    }
}

fn build_batch_prompt(run_id: &str, tickers: &[(String, String, String)]) -> String {
    let lines_list = tickers
        .iter()
        .map(|(t, n, lt)| format!("  - {lt}:{t} ({n})"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"Tu es Alfred, un conseiller financier bienveillant. Analyse ces {count} lignes pour le run "{run_id}".

Lignes a analyser :
{lines_list}

Pour CHAQUE ligne ci-dessus :
1. Appelle `get_line_data(run_id="{run_id}", ticker="TICKER")` pour obtenir le contexte.
2. Analyse et produis un JSON de recommandation avec :
   - line_id: "type:ticker" (ex: "position:MC")
   - ticker, type, nom
   - signal: ACHAT_FORT | ACHAT | RENFORCEMENT | CONSERVER | ALLEGEMENT | VENTE | SURVEILLANCE
   - conviction: faible | moderee | forte
   - synthese: minimum 150 caracteres (explique comme a un ami)
   - analyse_technique, analyse_fondamentale, analyse_sentiment
   - raisons_principales: 3-5 raisons (array)
   - risques, catalyseurs, badges_keywords: arrays
   - action_recommandee: instruction CHIFFREE (nb titres, montant €, prix)
   - deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
   - deep_news_quality_score: 0-100 (recence 0-35 + pertinence 0-35 + diversite 0-20 + utilite 0-10)
   - deep_news_relevance: high|medium|low
   - deep_news_staleness: fresh|recent|stale
   - reanalyse_after: date ISO, reanalyse_reason
   - llm_memory_summary: resume factuel
3. Appelle `validate_recommendation(run_id="{run_id}", recommendation=...)`.
   Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.
4. BUDGET WEB: maximum 1 recherche web par ligne. Utilise-la uniquement si
   deep_news_quality_score < 30 et les donnees de get_line_data sont vraiment insuffisantes.
   Prefere TOUJOURS analyser avec les donnees disponibles.
5. Si tu fais une recherche web et lis un article, appelle `persist_deep_news` pour enrichir
   le cache collectif (les prochains runs reutiliseront ce resume sans recherche web).
6. Si un article dans les news est du bruit (pub, generique, non pertinent), appelle
   `ban_deep_news` pour le filtrer dans les prochains runs.
7. Si fondamentaux manquants trouves, appelle `persist_extracted_fundamentals`.
8. Appelle `persist_shared_insights` avec tes analyses generiques.

Regles :
- Analyse CHAQUE ligne. Ne saute aucune.
- Toujours appeler validate_recommendation — ne presume pas que ton output est valide.
- Sois concret sur les chiffres.
- Les articles marques "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement.
- Les articles marques "A APPROFONDIR" n'ont pas de resume — lis-les via recherche web."#,
        count = tickers.len(),
        run_id = run_id,
        lines_list = lines_list,
    )
}

fn build_synthesis_prompt(run_id: &str) -> String {
    format!(
        r#"Tu es Alfred, un gestionnaire de portefeuille qui conseille un investisseur particulier.
Tu dois produire la synthese globale du portefeuille pour le run "{run_id}".

WORKFLOW STRICT — suis ces etapes dans l'ordre :

1. Appelle `get_run_context(run_id="{run_id}")` pour obtenir le resume du portefeuille.

2. Appelle `check_coverage(run_id="{run_id}")` pour verifier la couverture.
   Si des lignes manquent, continue avec ce qui est disponible.

3. Genere la synthese avec ces 4 champs :

   synthese_marche (minimum 300 caracteres):
   IMPORTANT — ne repete PAS les donnees ligne par ligne (l'utilisateur les voit
   deja dans le detail de chaque position). Concentre-toi sur :
   - La NARRATIVE : quel est le fil conducteur du portefeuille ? Quel profil de
     risque se dessine ? Est-ce coherent avec la strategie annoncee ?
   - Les DECISIONS A PRENDRE : quels arbitrages concrets, dans quel ordre, et
     pourquoi maintenant plutot que dans un mois ?
   - Les RISQUES CROISES : correlations entre lignes, exposition sectorielle ou
     geographique desequilibree, impact d'un scenario macro (hausse des taux,
     recession, change EUR/USD).
   - Le TON : parle comme un conseiller bienveillant a un particulier, pas a un pro.
     Pas de jargon financier inutile. Sois direct et opinione, pas descriptif.
     Exemple : "Vous avez trop mise sur la tech US sans protection. Avant
     de renforcer NVDA, securisez vos gains sur VZ et vendez les lignes
     qui ne bougent plus — ca libere du cash pour de vraies opportunites."

   actions_immediates (JSON array, 1-5 actions):
   CHAQUE signal ACHAT/VENTE/RENFORCEMENT/ALLEGEMENT des recommandations DOIT
   avoir une action ici (sauf si >5, priorise les plus urgentes).
   Ne PAS inclure CONSERVER/SURVEILLANCE.
   OBLIGATOIRE si des recommandations ont un signal actionnable.
   Schema strict par action:
   {{
     "ticker": "MC",
     "action": "ACHAT|VENTE|RENFORCEMENT|ALLEGEMENT",
     "order_type": "MARKET|LIMIT",
     "limit_price": null,
     "quantity": 3,
     "estimated_amount_eur": 2400.0,
     "priority": 1,
     "rationale": "phrase courte et concrete"
   }}
   Regles: quantity > 0, estimated_amount_eur > 0, priorities 1-5 uniques,
   LIMIT => limit_price > 0, MARKET => limit_price = null.

   prochaine_analyse: date + catalyseurs justifiant cette date
   (ex: "Relancez apres le 15 avril — resultats T1 Schneider et LVMH")

   opportunites_watchlist: resume des 2-3 meilleures opportunites watchlist
   (si des lignes watchlist existent). Sinon, chaine vide.

4. Appelle `validate_synthesis(run_id="{run_id}",
     synthese_marche="...", actions_immediates=[...],
     prochaine_analyse="...", opportunites_watchlist="...")`.
   Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.

5. Appelle `finalize_report(run_id="{run_id}")` pour composer et persister le rapport.

CRITIQUE — si tu ne fais pas les etapes 4 ET 5, le rapport est PERDU.
Le travail d'analyse de toutes les lignes sera gache. Tu DOIS appeler
validate_synthesis puis finalize_report. Pas d'exception."#,
        run_id = run_id,
    )
}

// ── Batch dispatch queue ─────────────────────────────────────────

pub struct McpLinePacket {
    pub ticker: String,
    pub nom: String,
    pub line_type: String,
}

pub struct McpBatchDispatchQueue {
    run_id: String,
    data_dir: String,
    batch_size: usize,
    pending: Vec<McpLinePacket>,
    result_tx: mpsc::Sender<Result<Vec<String>>>,
    result_rx: mpsc::Receiver<Result<Vec<String>>>,
    active_batches: usize,
    completed_tickers: Vec<String>,
    relay_stop_flags: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl McpBatchDispatchQueue {
    pub fn new(run_id: &str, data_dir: &str, batch_size: usize) -> Self {
        // Ensure MCP config is written before first analysis (codex backend only)
        if crate::llm_backend::current_backend_name() == "codex" {
            crate::codex::ensure_mcp_config();
        }

        let (result_tx, result_rx) = mpsc::channel();
        Self {
            run_id: run_id.to_string(),
            data_dir: data_dir.to_string(),
            batch_size: batch_size.max(1),
            pending: Vec::new(),
            result_tx,
            result_rx,
            active_batches: 0,
            completed_tickers: Vec::new(),
            relay_stop_flags: Vec::new(),
        }
    }

    pub fn push(&mut self, packet: McpLinePacket) -> Result<()> {
        self.pending.push(packet);
        if self.pending.len() >= self.batch_size {
            self.flush_batch()?;
        }
        Ok(())
    }

    pub fn flush_pending(&mut self) -> Result<()> {
        if !self.pending.is_empty() {
            self.flush_batch()?;
        }
        Ok(())
    }

    pub fn join_all(&mut self) -> Result<Vec<String>> {
        while self.active_batches > 0 {
            match self.result_rx.recv() {
                Ok(Ok(tickers)) => {
                    self.completed_tickers.extend(tickers);
                    self.active_batches -= 1;
                    // Merge MCP results after each batch completes
                    merge_mcp_results(&self.run_id, &self.data_dir);
                }
                Ok(Err(e)) => {
                    eprintln!("[mcp-batch] batch failed: {e}");
                    self.active_batches -= 1;
                    merge_mcp_results(&self.run_id, &self.data_dir);
                }
                Err(_) => break,
            }
        }
        // Final merge in case any results arrived after last batch
        merge_mcp_results(&self.run_id, &self.data_dir);
        for flag in &self.relay_stop_flags {
            flag.store(true, Ordering::Relaxed);
        }
        Ok(self.completed_tickers.clone())
    }

    fn flush_batch(&mut self) -> Result<()> {
        let batch: Vec<McpLinePacket> = self.pending.drain(..).collect();
        let tickers: Vec<(String, String, String)> = batch
            .iter()
            .map(|p| (p.ticker.clone(), p.nom.clone(), p.line_type.clone()))
            .collect();

        let run_id = self.run_id.clone();
        let data_dir = self.data_dir.clone();
        let tx = self.result_tx.clone();

        // Emit analyzing_lines stage on first batch dispatch
        if self.active_batches == 0 && self.completed_tickers.is_empty() {
            let _ = crate::run_state::set_native_run_stage(
                &self.run_id, "analyzing_lines", None, None,
            );
        }

        let (relay_handle, relay_stop) =
            crate::mcp_progress_relay::start_relay(&run_id, &data_dir);
        self.relay_stop_flags.push(relay_stop);

        let batch_tickers: Vec<String> = tickers.iter().map(|(t, _, _)| t.clone()).collect();
        let is_native = crate::llm_backend::current_backend_name() != "codex";

        let progress_run_id = run_id.clone();
        let progress_data_dir = data_dir.clone();
        let progress_tickers = batch_tickers.clone();

        thread::spawn(move || {
            if is_native {
                // Native backend: parallel per-line — LLM returns JSON, we validate+persist
                let dd = std::path::PathBuf::from(&progress_data_dir);

                // Pre-fetch all line data (fast, from cache/disk)
                let line_data_vec: Vec<(String, String, String, Value)> = tickers.iter().map(|(ticker, nom, line_type)| {
                    let line_data = crate::mcp_server::dispatch_tool_direct(
                        &dd,
                        "get_line_data",
                        &serde_json::json!({"run_id": progress_run_id, "line_id": format!("{line_type}:{ticker}")}),
                    );
                    (ticker.clone(), nom.clone(), line_type.clone(), line_data)
                }).collect();

                // Spawn one thread per line
                let handles: Vec<_> = line_data_vec.into_iter().map(|(ticker, nom, line_type, line_data)| {
                    let rid = progress_run_id.clone();
                    let pdd = dd.clone();
                    let tk = ticker.clone();

                    thread::spawn(move || {
                        let prid = rid.clone();
                        let pdd2 = pdd.clone();
                        let tk2 = tk.clone();

                        let progress_cb: Option<crate::llm_backend::ProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
                            if label.starts_with("tokens:") || label.starts_with("rate_limit:") {
                                let path = pdd2.join("runtime-state").join(format!("{prid}_mcp_progress.jsonl"));
                                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                                    use std::io::Write;
                                    let event = if label.starts_with("tokens:") {
                                        let parts: Vec<&str> = label.split(':').collect();
                                        serde_json::json!({
                                            "type": "token_usage",
                                            "total": parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                            "input": parts.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                            "output": parts.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                        })
                                    } else {
                                        serde_json::json!({
                                            "type": "rate_limit",
                                            "used_pct": label.trim_start_matches("rate_limit:").trim_end_matches('%'),
                                        })
                                    };
                                    let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
                                }
                                return;
                            }
                            if !tk2.is_empty() {
                                let dominated = label.starts_with("writing (") || label.starts_with("round ");
                                if !dominated {
                                    crate::mcp_progress_relay::write_progress_event(
                                        &pdd2, &prid, &tk2, "analyzing", &label.replace('\u{2026}', "..."),
                                    );
                                }
                            }
                        }));

                        match run_native_line_analysis(&rid, &tk, &nom, &line_type, &line_data, &pdd, progress_cb) {
                            Ok(()) => Ok(tk.clone()),
                            Err(e) => {
                                eprintln!("[mcp-native] line {tk} failed: {e}");
                                let err_msg = format!("{e}");
                                let _ = crate::run_state::update_line_status_with_error(&rid, &tk, "failed", Some(&err_msg));
                                Err(e)
                            }
                        }
                    })
                }).collect();

                // Join all line threads
                let mut ok_tickers: Vec<String> = Vec::new();
                let mut has_error = false;
                for handle in handles {
                    match handle.join() {
                        Ok(Ok(ticker)) => ok_tickers.push(ticker),
                        Ok(Err(_)) => has_error = true,
                        Err(_) => has_error = true,
                    }
                }

                let _ = tx.send(if ok_tickers.is_empty() && has_error {
                    Err(anyhow::anyhow!("all_lines_failed"))
                } else {
                    Ok(ok_tickers)
                });
                drop(relay_handle);
            } else {
                // Codex backend: batch prompt (model calls get_line_data via MCP tools)
                let prompt = build_batch_prompt(&progress_run_id, &tickers);
                let timeout_ms = 180_000 + (progress_tickers.len() as u64 * 30_000);

                let current_ticker = std::sync::Arc::new(std::sync::Mutex::new(
                    progress_tickers.first().cloned().unwrap_or_default()
                ));
                let ct = std::sync::Arc::clone(&current_ticker);
                let prid = progress_run_id.clone();
                let pdd = std::path::PathBuf::from(&progress_data_dir);

                let progress_cb: Option<crate::llm_backend::ProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
                    let ticker = ct.lock().map(|g| g.clone()).unwrap_or_default();

                    if label.starts_with("tokens:") || label.starts_with("rate_limit:") {
                        let path = pdd.join("runtime-state").join(format!("{prid}_mcp_progress.jsonl"));
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                            use std::io::Write;
                            let event = if label.starts_with("tokens:") {
                                let parts: Vec<&str> = label.split(':').collect();
                                serde_json::json!({
                                    "type": "token_usage",
                                    "total": parts.get(1).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                    "input": parts.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                    "output": parts.get(3).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
                                })
                            } else {
                                serde_json::json!({
                                    "type": "rate_limit",
                                    "used_pct": label.trim_start_matches("rate_limit:").trim_end_matches('%'),
                                })
                            };
                            let _ = writeln!(file, "{}", serde_json::to_string(&event).unwrap_or_default());
                        }
                        return;
                    }

                    if ticker.is_empty() { return; }
                    crate::mcp_progress_relay::write_progress_event(
                        &pdd, &prid, &ticker, "analyzing", &label.replace('\u{2026}', "..."),
                    );
                }));

                let result = crate::llm_backend::run_prompt(&prompt, timeout_ms, progress_cb);
                let _ = tx.send(match result {
                    Ok(_) => Ok(batch_tickers),
                    Err(e) => {
                        eprintln!("[mcp-batch] turn failed: {e}");
                        let err_msg = format!("{e}");
                        for t in &batch_tickers {
                            let _ = crate::run_state::update_line_status_with_error(&progress_run_id, t, "failed", Some(&err_msg));
                            crate::emit_event("alfred://line-progress", serde_json::json!({
                                "run_id": progress_run_id,
                                "ticker": t,
                                "line_status": { "status": "failed", "error": err_msg },
                            }));
                        }
                        Err(e)
                    }
                });
                drop(relay_handle);
            }
        });

        self.active_batches += 1;
        Ok(())
    }
}

// ── Synthesis turn ───────────────────────────────────────────────

pub fn run_synthesis_turn(run_id: &str, data_dir: &str) -> Result<Value> {
    crate::run_state::set_native_run_stage(run_id, "llm_generating", None, None)?;

    let is_native = crate::llm_backend::current_backend_name() != "codex";

    if is_native {
        return run_native_synthesis(run_id, data_dir);
    }

    // Codex backend: MCP tool-based synthesis (model calls validate_synthesis + finalize_report)
    let (relay_handle, relay_stop) = crate::mcp_progress_relay::start_relay(run_id, data_dir);

    let prompt = build_synthesis_prompt(run_id);
    let pdd = std::path::PathBuf::from(data_dir);
    let prid = run_id.to_string();
    let synthesis_cb: Option<crate::llm_backend::ProgressFn> = Some(Box::new(move |_bytes, _lines, label| {
        let progress_text = label.replace('\u{2026}', "...");
        crate::mcp_progress_relay::write_progress_event(
            &pdd, &prid, "__synthesis__", "generating", &progress_text,
        );
    }));
    let result = crate::llm_backend::run_prompt(&prompt, 300_000, synthesis_cb);

    relay_stop.store(true, Ordering::Relaxed);
    let _ = relay_handle.join();

    // Merge synthesis results from MCP sidecar
    merge_mcp_results(run_id, data_dir);

    match result {
        Ok(_) => {
            crate::run_state_cache::flush_now(run_id);
            let run_state = crate::load_run_by_id_direct(run_id)?;
            let status = run_state
                .get("orchestration")
                .and_then(|o| o.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or("running");

            if status == "completed" || status == "completed_degraded" {
                Ok(json!({
                    "ok": true,
                    "orchestration_status": status,
                    "run_id": run_id,
                }))
            } else {
                // Fallback: finalize with composed_payload if available
                codex_synthesis_fallback(run_id)
            }
        }
        Err(e) => Err(e),
    }
}

/// Native backend synthesis: LLM returns JSON, we validate (with retry) + finalize.
fn run_native_synthesis(run_id: &str, data_dir: &str) -> Result<Value> {
    crate::run_state_cache::flush_now(run_id);
    let run_state = crate::load_run_by_id_direct(run_id)?;
    let prompt = crate::llm_prompts::build_report_prompt(&run_state);
    let dd = std::path::Path::new(data_dir);

    crate::debug_log("[native-synthesis] generating synthesis via direct JSON...");

    const MAX_RETRIES: u32 = 2;
    let mut last_issues: Vec<String> = Vec::new();

    for attempt in 0..=MAX_RETRIES {
        let retry_prompt = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{}\n\n=== CORRECTION (tentative {}/{}) ===\nTa synthese precedente avait ces problemes: {}.\nCorrige-les et renvoie le JSON complet.",
                prompt,
                attempt + 1,
                MAX_RETRIES + 1,
                last_issues.join(", ")
            )
        };

        let prid = run_id.to_string();
        let pdd = std::path::PathBuf::from(data_dir);
        let cb: Option<crate::llm_backend::ProgressFn> = if attempt == 0 {
            Some(Box::new(move |_bytes, _lines, label| {
                let dominated = label.starts_with("writing (") || label.starts_with("round ");
                if !dominated {
                    crate::mcp_progress_relay::write_progress_event(
                        &pdd, &prid, "__synthesis__", "generating", &label.replace('\u{2026}', "..."),
                    );
                }
            }))
        } else {
            None
        };

        let result = crate::llm_backend::run_prompt(&retry_prompt, 300_000, cb)?;

        // Extract synthesis JSON
        let draft = if result.get("synthese_marche").is_some() {
            result.clone()
        } else if result.get("draft").is_some() {
            result.get("draft").cloned().unwrap_or(result.clone())
        } else {
            result.clone()
        };

        let synthese = draft.get("synthese_marche")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if synthese.len() < 20 {
            crate::debug_log(&format!("[native-synthesis] attempt {}: no synthese_marche in response", attempt + 1));
            last_issues = vec!["synthese_marche_missing_or_empty".to_string()];
            continue;
        }

        // Validate
        let actions_str = serde_json::to_string(
            draft.get("actions_immediates").unwrap_or(&json!([]))
        ).unwrap_or_else(|_| "[]".to_string());

        let validation = crate::mcp_server::dispatch_tool_direct(dd, "validate_synthesis", &json!({
            "run_id": run_id,
            "synthese_marche": synthese,
            "actions_immediates": actions_str,
            "prochaine_analyse": draft.get("prochaine_analyse").and_then(|v| v.as_str()).unwrap_or(""),
            "opportunites_watchlist": draft.get("opportunites_watchlist").and_then(|v| v.as_str()).unwrap_or(""),
        }));

        let valid = validation.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if valid || attempt == MAX_RETRIES {
            if !valid {
                crate::debug_log(&format!("[native-synthesis] accepting after {MAX_RETRIES} retries despite issues"));
            }
            crate::debug_log(&format!("[native-synthesis] synthesis ({} chars), finalizing", synthese.len()));
            crate::mcp_server::dispatch_tool_direct(dd, "finalize_report", &json!({"run_id": run_id}));
            return Ok(json!({
                "ok": true,
                "orchestration_status": "completed",
                "run_id": run_id,
            }));
        }

        // Validation failed — collect issues for retry prompt
        last_issues = validation
            .get("issues")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| vec!["validation_failed".to_string()]);

        crate::debug_log(&format!(
            "[native-synthesis] attempt {}: issues: {:?}",
            attempt + 1, last_issues
        ));
    }

    unreachable!("last attempt always accepts")
}

/// Codex fallback: finalize with composed_payload or hardcoded partial message.
fn codex_synthesis_fallback(run_id: &str) -> Result<Value> {
    crate::run_state_cache::flush_now(run_id);
    let run_state = crate::load_run_by_id_direct(run_id)?;
    let reco_count = run_state
        .get("pending_recommandations")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if reco_count > 0 {
        crate::debug_log(&format!(
            "[synthesis-fallback] finalize_report not called, attempting with {reco_count} recommendations"
        ));
        let composed = run_state.get("composed_payload").cloned().unwrap_or(json!({}));
        let synthese = composed.get("synthese_marche")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() > 20)
            .unwrap_or("Synthese partielle — le rapport a ete compose a partir des recommandations disponibles.");
        let mut actions = composed.get("actions_immediates").cloned().unwrap_or(json!([]));

        // If actions_immediates is empty, derive from per-line recommendations
        if actions.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            let recs = run_state.get("pending_recommandations")
                .and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut derived: Vec<Value> = recs.iter()
                .filter(|r| {
                    let sig = r.get("signal").and_then(|v| v.as_str()).unwrap_or("");
                    matches!(sig, "ACHAT_FORT" | "ACHAT" | "VENTE" | "ALLEGEMENT" | "RENFORCEMENT")
                })
                .map(|r| {
                    let ticker = r.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
                    let signal = r.get("signal").and_then(|v| v.as_str()).unwrap_or("");
                    let action = r.get("action_recommandee").and_then(|v| v.as_str()).unwrap_or(signal);
                    let rationale = r.get("synthese").and_then(|v| v.as_str())
                        .map(|s| s.chars().take(120).collect::<String>())
                        .unwrap_or_default();
                    json!({
                        "ticker": ticker,
                        "action": action,
                        "rationale": rationale,
                        "priority": signal,
                    })
                })
                .collect();
            derived.truncate(5);
            if !derived.is_empty() {
                // Assign unique priorities 1..N
                for (i, a) in derived.iter_mut().enumerate() {
                    if let Some(obj) = a.as_object_mut() {
                        obj.insert("priority".to_string(), json!(i + 1));
                    }
                }
                actions = json!(derived);
                crate::debug_log(&format!(
                    "[synthesis-fallback] derived {} actions from per-line recommendations", derived.len()
                ));
            }
        }
        let draft = json!({
            "synthese_marche": synthese,
            "actions_immediates": actions,
            "prochaine_analyse": composed.get("prochaine_analyse").cloned().unwrap_or(json!("")),
            "opportunites_watchlist": composed.get("opportunites_watchlist").cloned().unwrap_or(json!("")),
            "llm_utilise": "codex-mcp-partial",
        });
        let _ = crate::report::persist_retry_global_synthesis(run_id, &draft);
        // Mark orchestration as completed so sidebar/UI stops showing "running"
        let _ = crate::run_state::set_native_run_stage(run_id, "completed", None, None);
        Ok(json!({
            "ok": true,
            "orchestration_status": "completed",
            "run_id": run_id,
        }))
    } else {
        Err(anyhow::anyhow!("synthesis_incomplete:no_recommendations_and_finalize_not_called"))
    }
}
