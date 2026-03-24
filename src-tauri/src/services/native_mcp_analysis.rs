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

// ── Batch prompt ─────────────────────────────────────────────────

/// Build a per-line prompt for the native backend. Data is pre-injected —
/// no get_line_data tool call needed, saving a round-trip and keeping context clean.
fn build_native_line_prompt(run_id: &str, ticker: &str, nom: &str, line_type: &str, line_data: &Value) -> String {
    let position = serde_json::to_string_pretty(&line_data["position"]).unwrap_or_default();
    let market = serde_json::to_string_pretty(&line_data["market_data"]).unwrap_or_default();
    let news = serde_json::to_string_pretty(&line_data["news"]).unwrap_or_default();
    let insights = serde_json::to_string_pretty(&line_data["shared_insights"]).unwrap_or_default();
    let memory = serde_json::to_string_pretty(&line_data["line_memory"]).unwrap_or_default();
    let quality = serde_json::to_string_pretty(&line_data["quality"]).unwrap_or_default();

    format!(
        r#"Tu es Alfred, un conseiller financier bienveillant. Analyse cette ligne pour le run "{run_id}".

Ligne: {line_type}:{ticker} ({nom})

=== DONNEES (pre-chargees) ===

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

Produis un JSON de recommandation avec :
- line_id: "{line_type}:{ticker}"
- ticker: "{ticker}", type: "{line_type}", nom: "{nom}"
- signal: ACHAT_FORT | ACHAT | RENFORCEMENT | CONSERVER | ALLEGEMENT | VENTE | SURVEILLANCE
- conviction: faible | moderee | forte
- synthese: minimum 150 caracteres (explique comme a un ami)
- analyse_technique, analyse_fondamentale, analyse_sentiment
- raisons_principales: 3-5 raisons (array)
- risques, catalyseurs, badges_keywords: arrays
- action_recommandee: instruction CHIFFREE (nb titres, montant EUR, prix). Pour les lignes watchlist (non detenues), indiquer le prix d'entree ideal et le montant suggere.
- deep_news_summary: synthese 100-500 chars des actualites cles (OBLIGATOIRE si news disponibles)
- deep_news_quality_score: 0-100
- deep_news_relevance: high|medium|low
- deep_news_staleness: fresh|recent|stale
- reanalyse_after: date ISO, reanalyse_reason
- llm_memory_summary: resume factuel

Appelle `validate_recommendation(run_id="{run_id}", recommendation=...)`.
Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.

IMPORTANT: Produis UNE SEULE recommandation pour la ligne {line_type}:{ticker}. Ne cree aucune autre recommandation.

BUDGET WEB: maximum 1 recherche web, uniquement si deep_news_quality_score < 30.
Si tu fais une recherche web et lis un article, appelle `persist_deep_news`.
Si un article est du bruit, appelle `ban_deep_news`.
Si fondamentaux manquants trouves, appelle `persist_extracted_fundamentals`.
Appelle `persist_shared_insights` avec tes analyses generiques.

Les articles marques "RESUME APPROFONDI (cache)" sont deja resumes — utilise-les directement.
Les articles marques "A APPROFONDIR" n'ont pas de resume — lis-les via recherche web."#,
        run_id = run_id,
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
        r#"Tu es Alfred, un gestionnaire de portefeuille qui explique a un investisseur particulier non-expert.
Tu dois produire la synthese globale du portefeuille pour le run "{run_id}".

WORKFLOW STRICT — suis ces etapes dans l'ordre :

1. Appelle `get_run_context(run_id="{run_id}")` pour obtenir le resume du portefeuille
   et la liste des lignes analysees.

2. Appelle `check_coverage(run_id="{run_id}")` pour verifier la couverture.
   Si des lignes manquent, note-les mais continue avec les recommandations disponibles.
   Une synthese partielle est mieux que pas de synthese.

3. Genere la synthese avec ces 4 champs :

   synthese_marche (minimum 300 caracteres):
   - Raconte l'histoire du portefeuille: posture (offensive/defensive/neutre)
   - Points forts (lignes performantes, fondamentaux solides)
   - Points faibles (risques, lignes en difficulte)
   - Orientation: que faire dans les prochains jours/semaines
   - Ecarts a la strategie (si l'investisseur a des directives)
   - Utilise des chiffres concrets (valeurs, %, montants)

   actions_immediates (JSON array, 0-5 actions):
   CHAQUE signal ACHAT/VENTE/RENFORCEMENT/ALLEGEMENT des recommandations DOIT
   avoir une action ici (sauf si >5, alors prioriser les plus urgentes).
   Ne PAS inclure CONSERVER/SURVEILLANCE.
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
   Si ok=false, corrige les issues listees et re-appelle jusqu'a ok=true.

5. Appelle `finalize_report(run_id="{run_id}")` pour composer et persister le rapport.

REGLES:
- Ne saute AUCUNE etape. Chaque outil doit etre appele.
- Si check_coverage echoue, ne continue PAS — signale le probleme.
- Sois concret: chiffres, montants, dates. Pas de generalites.
- Ne presente PAS les watchlist comme deja detenues."#,
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
                }
                Ok(Err(e)) => {
                    eprintln!("[mcp-batch] batch failed: {e}");
                    self.active_batches -= 1;
                }
                Err(_) => break,
            }
        }
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
                // Native backend: parallel per-line dispatch, each with pre-injected data + clean context
                let dd = std::path::PathBuf::from(&progress_data_dir);

                // Pre-fetch all line data on the batch thread (fast, local disk reads)
                let line_prompts: Vec<(String, String)> = tickers.iter().map(|(ticker, nom, line_type)| {
                    let line_data = crate::mcp_server::dispatch_tool_direct(
                        &dd,
                        "get_line_data",
                        &serde_json::json!({"run_id": progress_run_id, "line_id": format!("{line_type}:{ticker}")}),
                    );
                    let prompt = build_native_line_prompt(&progress_run_id, ticker, nom, line_type, &line_data);
                    (ticker.clone(), prompt)
                }).collect();

                // Spawn one thread per line — all run in parallel
                let handles: Vec<_> = line_prompts.into_iter().map(|(ticker, prompt)| {
                    let rid = progress_run_id.clone();
                    let pdd = dd.clone();
                    let tk = ticker.clone();

                    thread::spawn(move || {
                        let timeout_ms = 180_000u64;
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
                                // Only show meaningful progress — skip noisy output streaming
                                let dominated = label.starts_with("writing (")
                                    || label.starts_with("round ");
                                if !dominated {
                                    crate::mcp_progress_relay::write_progress_event(
                                        &pdd2, &prid, &tk2, "analyzing", &label.replace('\u{2026}', "..."),
                                    );
                                }
                            }
                        }));

                        match crate::llm_backend::run_prompt(&prompt, timeout_ms, progress_cb) {
                            Ok(_) => Ok(tk.clone()),
                            Err(e) => {
                                eprintln!("[mcp-native] line {tk} failed: {e}");
                                let err_msg = format!("{e}");
                                let _ = crate::run_state::update_line_status_with_error(&rid, &tk, "failed", Some(&err_msg));
                                crate::emit_event("alfred://line-progress", serde_json::json!({
                                    "run_id": rid,
                                    "ticker": tk,
                                    "line_status": { "status": "failed", "error": err_msg },
                                }));
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

    match result {
        Ok(_) => {
            // Flush cache to disk before reading back (finalize_report may have evicted already)
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
                // Codex turn completed but finalize_report was never called.
                // This happens when coverage is incomplete or Codex stops early.
                // Mark as completed_degraded with partial data.
                let reco_count = run_state
                    .get("pending_recommandations")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);

                if reco_count > 0 {
                    // We have recommendations — try to finalize with what we have
                    crate::debug_log(&format!(
                        "[mcp-synthesis] finalize_report not called, attempting with {reco_count} recommendations"
                    ));
                    // Use composed_payload if validate_synthesis was called, else minimal fallback
                    let composed = run_state.get("composed_payload").cloned().unwrap_or(json!({}));
                    let synthese = composed.get("synthese_marche")
                        .and_then(|v| v.as_str())
                        .filter(|s| s.len() > 20)
                        .unwrap_or("Synthese partielle — le rapport a ete compose a partir des recommandations disponibles.");
                    let actions = composed.get("actions_immediates").cloned().unwrap_or(json!([]));
                    let draft = serde_json::json!({
                        "synthese_marche": synthese,
                        "actions_immediates": actions,
                        "prochaine_analyse": composed.get("prochaine_analyse").cloned().unwrap_or(json!("")),
                        "opportunites_watchlist": composed.get("opportunites_watchlist").cloned().unwrap_or(json!("")),
                        "llm_utilise": "codex-mcp-partial",
                    });
                    let _ = crate::report::persist_retry_global_synthesis(run_id, &draft);
                    Ok(json!({
                        "ok": true,
                        "orchestration_status": "completed_degraded",
                        "run_id": run_id,
                    }))
                } else {
                    Err(anyhow::anyhow!("synthesis_incomplete:no_recommendations_and_finalize_not_called"))
                }
            }
        }
        Err(e) => Err(e),
    }
}
