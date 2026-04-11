//! LLM prompt builders — report, line analysis, watchlist, repair.
//!
//! Structured prompts inspired by pea-agent POC: clear sections with headers,
//! explicit data quality hints, strict JSON schemas, and deterministic rules.

use serde_json::{json, Value};

// ── Previous syntheses loader ───────────────────────────────────

/// Load the last N global syntheses from report history for narrative continuity.
/// When `account` is non-empty, only artifacts matching that account are included.
pub(crate) fn load_previous_syntheses(limit: usize, account: &str) -> Vec<(String, String)> {
    let history_dir = crate::paths::resolve_report_history_dir();
    if !history_dir.exists() { return Vec::new(); }

    let filter_active = !account.is_empty();
    let mut entries: Vec<(String, String, String)> = Vec::new(); // (date, run_id, synthese)

    if let Ok(dir) = std::fs::read_dir(&history_dir) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
            let filename = entry.file_name().to_string_lossy().to_string();
            let date = filename.get(..8).unwrap_or("?").to_string();

            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(artifact) = serde_json::from_str::<Value>(&text) {
                    if filter_active {
                        let artifact_account = artifact.get("account").and_then(|v| v.as_str()).unwrap_or("");
                        if artifact_account != account { continue; }
                    }
                    let synthese = artifact.get("payload")
                        .and_then(|p| p.get("synthese_marche"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !synthese.is_empty() {
                        entries.push((filename.clone(), date, synthese.to_string()));
                    }
                }
            }
        }
    }

    // Sort by filename desc (most recent first), deduplicate by date prefix
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen_dates = std::collections::HashSet::new();
    entries.into_iter()
        .filter(|(_, date, _)| seen_dates.insert(date.clone()))
        .take(limit)
        .map(|(_, date, synthese)| (date, synthese))
        .collect()
}

/// Public version for use by native_mcp_analysis synthesis prompt.
pub(crate) fn build_previous_syntheses_section_public(account: &str) -> String {
    build_previous_syntheses_section(account)
}

fn build_previous_syntheses_section(account: &str) -> String {
    let prev = load_previous_syntheses(2, account);
    if prev.is_empty() { return String::new(); }

    let mut lines = vec!["\nSYNTHESES PRECEDENTES (pour continuite narrative — ne pas repeter, mais faire evoluer):".to_string()];
    for (date, synthese) in &prev {
        let formatted_date = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..8]);
        let truncated = if synthese.len() > 600 {
            let mut end = 600;
            while !synthese.is_char_boundary(end) { end -= 1; }
            format!("{}…", &synthese[..end])
        } else { synthese.clone() };
        lines.push(format!("[{formatted_date}] {truncated}"));
    }
    lines.push("\nConsigne: construis sur ces analyses precedentes. Identifie ce qui a CHANGE (nouvelles positions, evolution des signaux, performances). Evite de repeter les memes observations — fais progresser le narratif.".to_string());
    lines.join("\n")
}

// ── Report synthesis prompt ──────────────────────────────────────

pub(crate) fn build_report_prompt(run_state: &Value) -> String {
    let portfolio = run_state.get("portfolio").cloned().unwrap_or_else(|| json!({}));
    let guidelines = run_state
        .get("agent_guidelines")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let recommendations = run_state
        .get("pending_recommandations")
        .cloned()
        .unwrap_or_else(|| json!([]));

    let recs_arr = recommendations.as_array().cloned().unwrap_or_default();
    let nb_achat = recs_arr.iter().filter(|r| {
        let s = as_text_upper(r.get("signal"));
        s.contains("ACHAT") || s.contains("RENFORC")
    }).count();
    let nb_vente = recs_arr.iter().filter(|r| {
        let s = as_text_upper(r.get("signal"));
        s.contains("VENTE") || s.contains("ALLEG")
    }).count();
    let nb_conserver = recs_arr.iter().filter(|r| {
        let s = as_text_upper(r.get("signal"));
        s.contains("CONSERVER") || s.contains("SURVEILLANCE")
    }).count();

    let mut rec_lines = Vec::new();
    for r in &recs_arr {
        let rtype = as_text(r.get("type")).to_lowercase();
        let prefix = if rtype == "watchlist" { "[watchlist]" } else { "[position]" };
        rec_lines.push(format!(
            "- {prefix} {} ({}): {} | {} | {}",
            as_text(r.get("nom")),
            as_text(r.get("ticker")),
            as_text(r.get("signal")),
            as_text(r.get("conviction")),
            truncate_str(&as_text(r.get("synthese")), 120),
        ));
    }

    let guidelines_section = if guidelines.is_empty() {
        String::new()
    } else {
        format!("\nDIRECTIVES INVESTISSEUR:\n{guidelines}\n")
    };

    let account = run_state.get("account").and_then(|v| v.as_str()).unwrap_or("");
    let previous_syntheses = build_previous_syntheses_section(account);

    format!(
        r#"Tu es un conseiller financier bienveillant qui parle a un investisseur particulier.
Pas de jargon technique — explique simplement, comme a un ami.

REGLE FONDAMENTALE : les signaux par ligne ci-dessous sont des FAITS
produits par l'analyse detaillee. Ne les re-evalue PAS. Ta synthese
les resume et les met en perspective — elle ne reinvente pas l'analyse.

RESUME DU PORTEFEUILLE:
- Valeur totale: {total_value:.0}€
- Plus/moins-value: {total_gain:+.0}€
- Liquidites: {cash:.0}€
- {nb_achat} signaux achat/renforcement | {nb_conserver} a conserver/surveiller | {nb_vente} a alleger/vendre

RECOMMANDATIONS PAR LIGNE (signaux definitifs — ne pas contredire):
{rec_lines}
{guidelines_section}{previous_syntheses}
---

Produis un JSON avec exactement ces champs:

{{
  "synthese_marche": "string (minimum 300 caracteres)",
  "actions_immediates": [action objects],
  "prochaine_analyse": "string",
  "opportunites_watchlist": "string",
  "recommandations": []
}}

synthese_marche — IMPORTANT, ne repete PAS les donnees par ligne (l'utilisateur
les voit deja). Concentre-toi sur :
- La NARRATIVE : quel profil se dessine ? Est-ce coherent avec la strategie ?
- Les DECISIONS A PRENDRE : quels arbitrages, dans quel ordre, pourquoi maintenant ?
- Les RISQUES CROISES : correlations, desequilibres sectoriels/geographiques
- Le TON : direct, opinione, bienveillant. Comme un conseiller, pas un analyste.
  Exemple : "Vous avez trop mise sur la tech sans filet. Securisez vos gains
  et liberez du cash pour de vraies opportunites."

actions_immediates — schema par action:
  {{ "ticker": "MC", "nom": "LVMH",
     "action": "ACHAT|VENTE|RENFORCEMENT|ALLEGEMENT",
     "order_type": "MARKET|LIMIT", "limit_price": null,
     "quantity": 3, "estimated_amount_eur": 2400.0,
     "priority": 1, "rationale": "phrase courte" }}

Regles strictes:
- UNIQUEMENT les tickers dont le signal ci-dessus est ACHAT/ACHAT_FORT/VENTE/ALLEGEMENT/RENFORCEMENT
- Si le signal est CONSERVER ou SURVEILLANCE → PAS d'action pour ce ticker
- L'action DOIT correspondre au signal par ligne (pas de RENFORCEMENT si signal=SURVEILLANCE)
- Chaque action DOIT etre chiffree (quantity > 0, estimated_amount_eur > 0)
- Maximum 5 actions, priorites 1-5 uniques
- Pour LIMIT: limit_price > 0. Pour MARKET: limit_price = null
- Si liquidites = 0: uniquement VENTE/ALLEGEMENT (ou 0 action)
- Ne presente PAS les watchlist comme deja detenues
- Si ecart strategie, le dire clairement dans synthese_marche

Reponds uniquement en JSON valide."#,
        total_value = portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        total_gain = portfolio.get("plus_value_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        cash = portfolio.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0),
        nb_achat = nb_achat,
        nb_conserver = nb_conserver,
        nb_vente = nb_vente,
        rec_lines = rec_lines.join("\n"),
        guidelines_section = guidelines_section,
    )
}

// ── Line analysis prompt ─────────────────────────────────────────

pub(crate) fn build_line_analysis_prompt(
    line_context: &Value,
    run_state: &Value,
    agent_guidelines: Option<&str>,
) -> String {
    let portfolio = run_state.get("portfolio").cloned().unwrap_or_else(|| json!({}));
    let guidelines = agent_guidelines.unwrap_or_default();
    let ticker = line_context
        .get("ticker")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let line_type = line_context
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("position");
    let is_watchlist = line_type == "watchlist";

    // ── Sections ──
    let section_position = build_position_section(line_context.get("row"), is_watchlist);
    let section_market = build_market_section(line_context.get("market"));
    let section_news = build_news_section(line_context.get("news"));
    let section_shared = build_shared_insights_section(line_context.get("shared_insights"));
    let section_memory = build_memory_section(line_context.get("line_memory"));
    let section_sector_cot = build_sector_cot_section(line_context.get("sector_cot"));
    let section_activity = build_activity_section(line_context.get("activity"));
    let section_data_quality = build_data_quality_section(line_context.get("market"));

    let guidelines_section = if guidelines.is_empty() {
        String::new()
    } else {
        format!("\nDIRECTIVES INVESTISSEUR:\n{guidelines}\n")
    };

    let mcp_suffix = mcp_validation_suffix(run_state);

    format!(
        r#"Tu es un analyste financier qui s'adresse a un investisseur particulier non-expert. Sois factuel, concret et base sur les donnees fournies.

VALEUR ANALYSEE: {nom} ({ticker})
{section_position}
{section_market}
{section_data_quality}
{section_news}
{section_shared}
{section_sector_cot}
{section_activity}
{section_memory}

CONTEXTE PORTEFEUILLE:
- Valeur totale: {total_value:.0}€
- Liquidites: {cash:.0}€
- Plus/moins-value totale: {total_gain:+.0}€
{guidelines_section}
---

Produis ton analyse en JSON UNIQUEMENT avec cette structure exacte:

{{
  "recommendation": {{
    "line_id": "{line_type}:{ticker_lower}",
    "ticker": "{ticker}",
    "type": "{line_type}",
    "signal": "ACHAT_FORT|ACHAT|RENFORCEMENT|CONSERVER|ALLEGEMENT|VENTE|SURVEILLANCE",
    "conviction": "forte|moderee|faible",
    "synthese": "4-6 phrases: actualite cle, indicateurs determinants, plan court et moyen terme",
    "analyse_technique": "3-4 phrases sur la tendance du prix",
    "analyse_fondamentale": "3-4 phrases sur la sante de l'entreprise",
    "analyse_sentiment": "2-3 phrases sur les actualites et le sentiment",
    "raisons_principales": ["raison 1", "raison 2", "raison 3"],
    "risques": ["risque concret 1", "risque concret 2"],
    "catalyseurs": ["catalyseur 1", "catalyseur 2"],
    "badges_keywords": ["mot-cle-1", "mot-cle-2", "mot-cle-3"],
    "action_recommandee": "instruction CHIFFREE: nb titres, montant €, prix entree/sortie",
    "reanalyse_after": "YYYY-MM-DD",
    "reanalyse_reason": "prochain catalyseur",
    "deep_news_summary": "synthese 100-500 chars des actualites les plus impactantes pour cet investissement",
    "deep_news_quality_score": 75,
    "deep_news_relevance": "high|medium|low",
    "deep_news_staleness": "fresh|recent|stale",
    "sector_analysis": "2-3 phrases sur le positionnement sectoriel (COT, tendances, risques macro)"{extracted_fundamentals_field}
  }}
}}

Regles strictes:
- synthese: minimum 150 caracteres, explique comme a un ami
- action_recommandee: TOUJOURS chiffree (nb titres, montant €, prix si pertinent)
- badges_keywords: 3-8 mots courts, specifiques, orientes decision (pas de signal/conviction/secteur seul)
  - au moins 1 badge "fait d'actualite" (ex: "restructuration en cours")
  - au moins 1 badge "risque/chiffre" (ex: "PER 35x eleve")
  - au moins 1 badge "plan/trigger" (ex: "attendre resultats T1")

DEEP NEWS — process:
- Evalue la qualite des actualites avec un score 0-100 (deep_news_quality_score):
  - recence (0-35): articles des derniers jours = score eleve
  - pertinence (0-35): lien direct avec la societe, contenu actionnable
  - diversite sources (0-20): plusieurs sources credibles > source unique
  - utilite signal (0-10): catalyseurs/risques concrets > bruit
- deep_news_summary: ta PROPRE analyse des actualites (100-500 chars), pas une copie.
  Les articles avec "RESUME APPROFONDI (cache)" fournissent du contexte — synthetise-les
  avec les nouvelles infos pour produire une analyse FRAICHE et actionnelle.
  Ne copie PAS le cache tel quel — ajoute ton interpretation et les implications pour l'investisseur.
- BUDGET WEB: maximum 1 recherche web par ligne, uniquement si le score < 30 et les
  donnees fournies sont vraiment insuffisantes pour une analyse. Prefere toujours
  analyser avec les donnees disponibles plutot que chercher sur le web.
- Si tu lis un article en profondeur via web, appelle persist_deep_news pour enrichir
  le cache collectif (les prochains runs beneficieront du resume).
- deep_news_relevance: "high" si directement actionnable, "medium" si contextuel, "low" si marginal
- deep_news_staleness: "fresh" si <3 jours, "recent" si <7 jours, "stale" si >7 jours
- Si des analyses precedentes contiennent une "Synthese news precedentes",
  integre et mets a jour ces insights dans deep_news_summary (ne perds pas l'historique).

- Si donnees marche incompletes, donne plus de poids aux actualites et evenements
- Si tu changes de signal par rapport au precedent (memoire), explique pourquoi
- Si donnees insuffisantes: signal = SURVEILLANCE avec explication
{mcp_suffix}
Reponds uniquement en JSON valide."#,
        nom = as_text(line_context.get("row").and_then(|r| r.get("nom")).or(Some(&json!(ticker)))),
        ticker = ticker,
        ticker_lower = ticker.to_lowercase(),
        line_type = line_type,
        section_position = section_position,
        section_market = section_market,
        section_data_quality = section_data_quality,
        section_news = section_news,
        section_shared = section_shared,
        section_memory = section_memory,
        total_value = portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        total_gain = portfolio.get("plus_value_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        cash = portfolio.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0),
        guidelines_section = guidelines_section,
        extracted_fundamentals_field = if has_missing_market_fundamentals(line_context.get("market")) {
            ",\n    \"extracted_fundamentals\": {\"pe_ratio\": null, \"revenue_growth\": null, \"profit_margin\": null, \"debt_to_equity\": null}"
        } else {
            ""
        },
        mcp_suffix = mcp_suffix,
    )
}

// ── Structured sections ──────────────────────────────────────────

fn build_position_section(row: Option<&Value>, is_watchlist: bool) -> String {
    if is_watchlist {
        return "POSITION: NON DETENU — Analyse pour potentielle entree en position".to_string();
    }
    let r = match row {
        Some(v) if v.is_object() => v,
        _ => return "POSITION ACTUELLE: n/a".to_string(),
    };
    let name = as_text(r.get("nom").or(r.get("name")));
    let qty = r.get("quantite").or(r.get("quantity")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pru = r.get("pru").or(r.get("avg_price")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let val = r.get("valeur").or(r.get("value")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pv = r.get("plus_value").or(r.get("gain")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let pv_pct = r.get("plus_value_pct").or(r.get("gain_pct")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let compte = as_text(r.get("compte"));
    let isin = as_text(r.get("isin"));

    let mut lines = vec![format!("POSITION ACTUELLE: {name} ({isin})")];
    lines.push(format!("- Quantite: {qty:.0} titres"));
    lines.push(format!("- Prix de revient: {pru:.2}€"));
    lines.push(format!("- Valeur actuelle: {val:.0}€"));
    lines.push(format!("- Plus/moins-value: {pv:+.0}€ ({pv_pct:+.1}%)"));
    if !compte.is_empty() {
        lines.push(format!("- Compte: {compte}"));
    }
    lines.join("\n")
}

fn build_market_section(market: Option<&Value>) -> String {
    let m = match market {
        Some(v) if v.is_object() => v,
        _ => return "DONNEES DE MARCHE: aucune donnee disponible".to_string(),
    };
    let mut lines = vec!["DONNEES DE MARCHE:".to_string()];

    if let Some(p) = m.get("price").and_then(|v| v.as_f64()) {
        let currency = m.get("currency").and_then(|v| v.as_str()).unwrap_or("EUR");
        lines.push(format!("- Prix actuel: {p:.2} {currency}"));
    }
    if let Some(p) = m.get("pe_ratio").and_then(|v| v.as_f64()) { lines.push(format!("- PER: {p:.1}")); }
    if let Some(r) = m.get("revenue_growth").and_then(|v| v.as_f64()) { lines.push(format!("- Croissance CA: {r:+.1}%")); }
    if let Some(mg) = m.get("profit_margin").and_then(|v| v.as_f64()) { lines.push(format!("- Marge operationnelle: {mg:.1}%")); }
    if let Some(d) = m.get("debt_to_equity").and_then(|v| v.as_f64()) { lines.push(format!("- Dette/Capitaux propres: {d:.2}")); }
    if let Some(dy) = m.get("dividend_yield").and_then(|v| v.as_f64()) { lines.push(format!("- Rendement dividende: {dy:.1}%")); }

    if lines.len() == 1 { lines.push("- Aucune donnee disponible".to_string()); }

    // Web search context (if available)
    if let Some(ctx) = m.get("web_search_context").and_then(|v| v.as_array()) {
        if !ctx.is_empty() {
            lines.push("\nSOURCES WEB (recherche complementaire):".to_string());
            for item in ctx {
                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let snippet = item.get("snippet").and_then(|v| v.as_str()).unwrap_or("");
                if !snippet.is_empty() {
                    lines.push(format!("- {title}: {snippet}"));
                }
            }
        }
    }

    lines.join("\n")
}

fn build_data_quality_section(market: Option<&Value>) -> String {
    let m = match market {
        Some(v) if v.is_object() => v,
        _ => return "QUALITE DONNEES: aucune donnee marche".to_string(),
    };

    let mut missing = Vec::new();
    if m.get("pe_ratio").and_then(|v| v.as_f64()).is_none() { missing.push("PER"); }
    if m.get("revenue_growth").and_then(|v| v.as_f64()).is_none() { missing.push("croissance CA"); }
    if m.get("profit_margin").and_then(|v| v.as_f64()).is_none() { missing.push("marge"); }
    if m.get("debt_to_equity").and_then(|v| v.as_f64()).is_none() { missing.push("dette/equity"); }

    if missing.is_empty() {
        "QUALITE DONNEES: fondamentaux complets".to_string()
    } else {
        format!(
            "QUALITE DONNEES:\n- Fondamentaux manquants: {}\n- Consigne: analyse avec les donnees disponibles. Donne plus de poids aux actualites si les fondamentaux sont incomplets.",
            missing.join(", ")
        )
    }
}

fn build_news_section(news: Option<&Value>) -> String {
    let items = news.and_then(|v| {
        v.as_array()
            .or_else(|| v.get("items").and_then(|i| i.as_array()))
            .or_else(|| v.get("articles").and_then(|i| i.as_array()))
    });
    match items {
        Some(arr) if !arr.is_empty() => {
            let mut deep_lines = Vec::new();
            let mut fresh_lines = Vec::new();

            // Separate cached (with deep summary + scores) from uncached
            let mut cached_articles: Vec<(i64, String)> = Vec::new(); // (score, formatted)
            let mut uncached_articles: Vec<(String, String, String, String, String)> = Vec::new(); // (date, title, source, url, snippet)

            for item in arr {
                let title = item.get("title").or_else(|| item.get("titre"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                if title.is_empty() { continue; }
                let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let date = item.get("date").or_else(|| item.get("published_at"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                let url = item.get("url").or_else(|| item.get("link"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                let deep_summary = item.get("deep_summary").and_then(|v| v.as_str()).unwrap_or("");
                let is_cached = item.get("deep_summary_cached").and_then(|v| v.as_bool()).unwrap_or(false);
                let deep_score = item.get("deep_quality_score").and_then(|v| v.as_i64()).unwrap_or(0);
                let snippet = item.get("summary").or_else(|| item.get("resume"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                let date_short = if date.len() >= 10 { &date[..10] } else { date };

                if is_cached && !deep_summary.is_empty() {
                    let relevance = item.get("deep_relevance")
                        .or_else(|| item.get("relevance"))
                        .and_then(|v| v.as_str()).unwrap_or("");
                    let staleness = item.get("deep_staleness")
                        .or_else(|| item.get("staleness"))
                        .and_then(|v| v.as_str()).unwrap_or("");
                    let line = format!("- [{}] {} ({}) score={} relevance={} staleness={}\n   RESUME: {}",
                        source, title, date_short, deep_score, relevance, staleness,
                        truncate_str(deep_summary, 500));
                    cached_articles.push((deep_score, line));
                } else {
                    uncached_articles.push((
                        date.to_string(), title.to_string(), source.to_string(),
                        url.to_string(), snippet.to_string(),
                    ));
                }
            }

            // Sort cached by score (best first) — most relevant summaries at top
            cached_articles.sort_by(|a, b| b.0.cmp(&a.0));

            // Sort uncached by date (most recent first) — best candidate for deep reading
            uncached_articles.sort_by(|a, b| b.0.cmp(&a.0));

            // Build output
            let mut lines = vec!["ACTUALITES RECENTES:".to_string()];

            if !cached_articles.is_empty() {
                lines.push("  Resumes approfondis (cache, tries par pertinence):".to_string());
                for (_, line) in &cached_articles {
                    deep_lines.push(line.clone());
                }
                lines.extend(deep_lines);
            }

            // Select 1 best uncached article for deep reading (most recent with URL)
            let mut selected = false;
            for (i, (date, title, source, url, snippet)) in uncached_articles.iter().enumerate() {
                let date_short = if date.len() >= 10 { &date[..10] } else { date };
                if !selected && !url.is_empty() {
                    selected = true;
                    fresh_lines.push(format!("- [{}] {} ({}) [url: {}]", source, title, date_short, truncate_str(url, 80)));
                    if !snippet.is_empty() {
                        fresh_lines.push(format!("   {}", truncate_str(snippet, 200)));
                    }
                    fresh_lines.push("   → CANDIDAT: si budget web dispo, lis cet article et appelle persist_deep_news".to_string());
                } else {
                    fresh_lines.push(format!("- [{}] {} ({})", source, title, date_short));
                    if !snippet.is_empty() && i < 3 { // Only show snippets for top 3
                        fresh_lines.push(format!("   {}", truncate_str(snippet, 150)));
                    }
                }
            }

            if !fresh_lines.is_empty() {
                lines.push("  Autres articles (snippets, tries par date):".to_string());
                lines.extend(fresh_lines);
            }
            lines.join("\n")
        }
        _ => "ACTUALITES RECENTES: aucune actualite disponible".to_string(),
    }
}

fn build_shared_insights_section(insights: Option<&Value>) -> String {
    let m = match insights {
        Some(v) if v.is_object() && !v.as_object().map(|o| o.is_empty()).unwrap_or(true) => v,
        _ => return String::new(),
    };

    let mut lines = vec!["ANALYSES PRECEDENTES (autres investisseurs, anonymisees):".to_string()];

    if let Some(s) = m.get("analyse_fondamentale").and_then(|v| v.as_str()) {
        if !s.is_empty() { lines.push(format!("- Fondamentale: {s}")); }
    }
    if let Some(s) = m.get("analyse_technique").and_then(|v| v.as_str()) {
        if !s.is_empty() { lines.push(format!("- Technique: {s}")); }
    }
    if let Some(s) = m.get("analyse_sentiment").and_then(|v| v.as_str()) {
        if !s.is_empty() { lines.push(format!("- Sentiment: {s}")); }
    }
    if let Some(arr) = m.get("risques").and_then(|v| v.as_array()) {
        let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !items.is_empty() { lines.push(format!("- Risques: {}", items.join(", "))); }
    }
    if let Some(arr) = m.get("catalyseurs").and_then(|v| v.as_array()) {
        let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !items.is_empty() { lines.push(format!("- Catalyseurs: {}", items.join(", "))); }
    }
    if let Some(s) = m.get("deep_news_collective_summary").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            lines.push(format!("- Synthese news precedentes: {s}"));
            lines.push("  Consigne: integre dans deep_news_summary en conservant les insights precedents (max 500 chars).".to_string());
        }
    }
    if let Some(n) = m.get("contributor_count").and_then(|v| v.as_u64()) {
        let updated = m.get("updated_at").and_then(|v| v.as_str()).unwrap_or("?");
        lines.push(format!("  ({n} contributeur(s), maj: {updated})"));
    }

    if lines.len() == 1 { return String::new(); }
    lines.join("\n")
}

fn build_activity_section(activity: Option<&Value>) -> String {
    let arr = match activity {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return String::new(),
    };

    let mut lines = vec!["HISTORIQUE DES OPERATIONS RECENTES:".to_string()];
    for item in arr.iter().take(10) {
        let date = item.get("date").and_then(|v| v.as_str()).unwrap_or("?");
        let action = item.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        let amount = item.get("amount_eur").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let qty_str = item.get("quantity")
            .and_then(|v| v.as_f64())
            .map(|q| format!(" x{q:.0}"))
            .unwrap_or_default();
        lines.push(format!("- {date}: {action}{qty_str} — {amount:.0}€ ({name})"));
    }

    lines.push("\nConsigne: analyse les decisions passees (bon timing? prix moyen d'achat vs cours actuel? renforcements/allegements pertinents?). Commente les bonnes et mauvaises decisions, et utilise-les pour calibrer ta recommandation.".to_string());
    lines.join("\n")
}

fn build_sector_cot_section(sector_cot: Option<&Value>) -> String {
    let data = match sector_cot {
        Some(v) if v.is_object() => v,
        _ => return String::new(),
    };

    let sector = data.get("sector").and_then(|v| v.as_str()).unwrap_or("");
    if sector.is_empty() { return String::new(); }

    let sector_upper = sector.to_uppercase().replace('_', " ");
    let mut lines = vec![format!("POSITIONNEMENT SECTORIEL ({sector_upper}):")];

    // COT contracts
    if let Some(cot_obj) = data.get("cot") {
        let contracts = cot_obj.get("contracts").and_then(|v| v.as_array());
        if let Some(contracts) = contracts {
            for item in contracts {
                let contract = item.get("contract").and_then(|v| v.as_str()).unwrap_or("?");
                let net = item.get("noncomm_net").and_then(|v| v.as_i64()).unwrap_or(0);
                let change = item.get("change_noncomm_net").and_then(|v| v.as_i64()).unwrap_or(0);
                let sentiment = item.get("sentiment").and_then(|v| v.as_str()).unwrap_or("?");
                let sign = if change >= 0 { "+" } else { "" };
                lines.push(format!(
                    "- {contract}: net speculateurs = {net} ({sign}{change}), sentiment = {sentiment}"
                ));
            }
        }
    }

    // Sector analysis memo (from previous analyses)
    if let Some(analysis) = data.get("sector_analysis").and_then(|v| v.as_str()) {
        if !analysis.is_empty() {
            lines.push(format!("- Memo sectoriel precedent: {}", truncate_str(analysis, 400)));
        }
    }

    if lines.len() == 1 { return String::new(); }
    lines.push("\nConsigne: integre le positionnement COT dans ton analyse de sentiment. Un COT tres haussier/baissier sur les futures du secteur est un signal macro a mentionner.".to_string());
    lines.join("\n")
}

fn build_memory_section(memory: Option<&Value>) -> String {
    let m = match memory {
        Some(v) if v.is_object() && !v.as_object().map(|o| o.is_empty()).unwrap_or(true) => v,
        _ => return "MEMOIRE LIGNE: premiere analyse (pas d'historique)".to_string(),
    };

    // V2 detection: if schema_version == 2 or signal_history exists, use V2 rendering.
    // Otherwise render nothing (fresh install / pre-V2 data is treated as first analysis).
    let is_v2 = m.get("schema_version").and_then(|v| v.as_u64()).unwrap_or(0) == 2
        || m.get("signal_history").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);

    if !is_v2 {
        return "MEMOIRE LIGNE: premiere analyse (pas d'historique)".to_string();
    }

    let mut lines = vec!["MEMOIRE LIGNE (historique persistant):".to_string()];

    // Signal + price tracking
    if let Some(pt) = m.get("price_tracking") {
        let signal = pt.get("last_signal").and_then(|v| v.as_str()).unwrap_or("");
        let conviction = m.get("conviction").and_then(|v| v.as_str()).unwrap_or("");
        let date = pt.get("last_signal_date").and_then(|v| v.as_str()).unwrap_or("");
        let price = pt.get("price_at_signal").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ret = pt.get("return_since_signal_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let accuracy = pt.get("signal_accuracy").and_then(|v| v.as_str()).unwrap_or("");
        let accuracy_mark = match accuracy {
            "correct" => " \u{2713} correct",
            "incorrect" => " \u{2717} incorrect",
            _ => "",
        };
        if !signal.is_empty() {
            lines.push(format!(
                "- Signal: {signal} ({conviction}) depuis {date} | prix: {price:.2}\u{20ac} | rendement: {ret:+.1}%{accuracy_mark}"
            ));
        }
    }

    // Trend
    if let Some(trend) = m.get("trend").and_then(|v| v.as_str()) {
        if !trend.is_empty() {
            lines.push(format!("- Tendance 3 analyses: {trend}"));
        }
    }

    // News themes
    if let Some(arr) = m.get("news_themes").and_then(|v| v.as_array()) {
        let items: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).take(10).collect();
        if !items.is_empty() {
            lines.push(format!("- Themes: {}", items.join(", ")));
        }
    }

    // Key reasoning
    if let Some(reasoning) = m.get("key_reasoning").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            lines.push(format!("- These: {}", truncate_str(reasoning, 400)));
        }
    }

    // User action (optional)
    if let Some(ua) = m.get("user_action") {
        if !ua.is_null() {
            let followed = ua.get("followed").and_then(|v| v.as_bool());
            let date = ua.get("date").and_then(|v| v.as_str()).unwrap_or("");
            let note = ua.get("note").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(f) = followed {
                let action_str = if f { "suivi" } else { "non suivi" };
                let note_str = if note.is_empty() { String::new() } else { format!(" ({note})") };
                lines.push(format!("- Action utilisateur: {action_str} {date}{note_str}"));
            }
        }
    }

    lines.push("\nConsigne: construis un NARRATIF progressif. Ce qui a CHANGE depuis la derniere analyse. Si tu changes de signal, explique le declencheur. Si tu confirmes, dis pourquoi la these tient malgre l'evolution du marche.".to_string());

    if lines.len() == 1 { lines.push("- Premiere analyse".to_string()); }
    lines.join("\n")
}

// ── Watchlist generation ─────────────────────────────────────────

pub(crate) fn build_watchlist_prompt(positions: &[Value], portfolio: &Value, guidelines: &str, account: &str) -> String {
    let position_tickers: Vec<String> = positions
        .iter()
        .map(|p| {
            let ticker = p.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
            let nom = p.get("nom").and_then(|v| v.as_str()).unwrap_or("");
            let isin = p.get("isin").and_then(|v| v.as_str()).unwrap_or("");
            format!("- {ticker} ({nom}) [ISIN:{isin}]")
        })
        .collect();
    let total_value = portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cash = portfolio.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let guidelines_section = if guidelines.is_empty() {
        String::new()
    } else {
        format!("\nDIRECTIVES INVESTISSEUR:\n{guidelines}\n")
    };

    let account_lower = account.to_lowercase();
    let is_pea = account_lower.contains("pea");
    let is_us = account_lower.contains("trading") || account_lower.contains("revolut")
        || account_lower.contains("degiro") || account_lower.contains("ibkr")
        || account_lower.contains("interactive");
    let universe_constraint = if is_pea {
        "- Univers: PEA europeen (Euronext Paris, Amsterdam, Bruxelles, Francfort). Pas de valeurs US."
    } else if is_us {
        "- Univers: international — grandes valeurs US (NYSE, NASDAQ) et internationales."
    } else {
        "- Univers: large — valeurs europeennes ou internationales selon la composition actuelle."
    };
    let ticker_example = if is_pea { "AIR" } else if is_us { "AAPL" } else { "AIR" };

    format!(
        r#"Tu es un conseiller financier qui aide a diversifier un portefeuille.

COMPTE: {account}

POSITIONS ACTUELLES:
{positions}

PORTEFEUILLE: {total_value:.0}€ total, {cash:.0}€ cash
{guidelines_section}
Suggere exactement 5 tickers complementaires (non detenus) en te basant sur:
- Diversification sectorielle (quels secteurs sont sous-representes?)
{universe_constraint}
- Complementarite (pas de duplication avec les positions existantes)
- Qualite (entreprises etablies avec fondamentaux solides)

Reponds en JSON strict:
{{
  "watchlist": [
    {{
      "ticker": "{ticker_example}",
      "nom": "Nom complet",
      "isin": "FR0000000000",
      "raison": "pourquoi cette opportunite pour CE portefeuille",
      "secteur": "secteur d'activite"
    }}
  ]
}}"#,
        account = account,
        positions = position_tickers.join("\n"),
        total_value = total_value,
        cash = cash,
        guidelines_section = guidelines_section,
        universe_constraint = universe_constraint,
        ticker_example = ticker_example,
    )
}

// ── Repair prompt ────────────────────────────────────────────────

pub(crate) fn build_repair_prompt(
    line_context: &Value,
    run_state: &Value,
    agent_guidelines: Option<&str>,
    validation_context: &Value,
) -> String {
    let portfolio = run_state.get("portfolio").cloned().unwrap_or_else(|| json!({}));
    let guidelines = agent_guidelines.unwrap_or_default();
    let ticker = line_context.get("ticker").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");

    let section_position = build_position_section(line_context.get("row"), false);
    let section_market = build_market_section_no_web(line_context.get("market"));
    let section_news = build_news_section(line_context.get("news"));
    let section_memory = build_memory_section(line_context.get("line_memory"));

    let issues = validation_context
        .get("validation_issues")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();

    let rec_to_fix = validation_context
        .get("recommendation_to_fix")
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .unwrap_or_else(|| "{}".to_string());

    let guidelines_section = if guidelines.is_empty() {
        String::new()
    } else {
        format!("\nDIRECTIVES INVESTISSEUR:\n{guidelines}\n")
    };

    format!(
        r#"Repare cette recommandation pour {ticker}.

PROBLEMES A CORRIGER: {issues}

{section_position}
{section_market}
{section_news}
{section_memory}

CONTEXTE PORTEFEUILLE:
- Valeur totale: {total_value:.0}€
- Liquidites: {cash:.0}€
- Plus/moins-value: {total_gain:+.0}€
{guidelines_section}
RECOMMANDATION PRECEDENTE (a ameliorer):
{rec_to_fix}

Regles:
- Corrige les champs identifies comme defaillants
- Garde les champs corrects de la recommandation precedente
- Utilise les donnees fournies (pas de recherche web)

JSON valide uniquement, cle "recommendation"."#,
        ticker = ticker,
        issues = issues,
        section_position = section_position,
        section_market = section_market,
        section_news = section_news,
        section_memory = section_memory,
        rec_to_fix = rec_to_fix,
        total_value = portfolio.get("valeur_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        total_gain = portfolio.get("plus_value_totale").and_then(|v| v.as_f64()).unwrap_or(0.0),
        cash = portfolio.get("liquidites").and_then(|v| v.as_f64()).unwrap_or(0.0),
    )
}

/// Market section for repair — no web search instructions.
fn build_market_section_no_web(market: Option<&Value>) -> String {
    let m = match market {
        Some(v) if v.is_object() => v,
        _ => return "DONNEES DE MARCHE: aucune".to_string(),
    };
    let mut lines = vec!["DONNEES DE MARCHE:".to_string()];

    if let Some(p) = m.get("price").and_then(|v| v.as_f64()) {
        let currency = m.get("currency").and_then(|v| v.as_str()).unwrap_or("EUR");
        lines.push(format!("- Prix: {p:.2} {currency}"));
    }
    if let Some(p) = m.get("pe_ratio").and_then(|v| v.as_f64()) { lines.push(format!("- PER: {p:.1}")); }
    if let Some(r) = m.get("revenue_growth").and_then(|v| v.as_f64()) { lines.push(format!("- Croissance CA: {r:+.1}%")); }
    if let Some(mg) = m.get("profit_margin").and_then(|v| v.as_f64()) { lines.push(format!("- Marge: {mg:.1}%")); }
    if let Some(d) = m.get("debt_to_equity").and_then(|v| v.as_f64()) { lines.push(format!("- Dette/Equity: {d:.2}")); }
    if let Some(dy) = m.get("dividend_yield").and_then(|v| v.as_f64()) { lines.push(format!("- Rendement dividende: {dy:.1}%")); }

    if lines.len() == 1 { lines.push("- Aucune donnee disponible".to_string()); }
    lines.join("\n")
}

// ── MCP-aware prompt suffix ──────────────────────────────────────

/// When MCP tools are available, returns instructions for Codex to self-validate.
pub(crate) fn mcp_validation_suffix(run_state: &Value) -> String {
    let run_id = run_state.get("run_id").and_then(|v| v.as_str()).unwrap_or("");
    if run_id.is_empty() {
        return String::new();
    }
    format!(
        r#"
OUTILS MCP DISPONIBLES — appelle-les dans cet ordre apres ton analyse:

1. `validate_recommendation(run_id="{run_id}", recommendation={{...}})` — OBLIGATOIRE
   Valide ta recommandation. Si ok=false, corrige les issues et re-appelle jusqu'a ok=true.

2. `persist_deep_news(ticker, isin, url, title, summary, quality_score, relevance, staleness)`
   Pour chaque article que tu as lu en profondeur (pas ceux deja en cache),
   persiste le resume pour que les prochains runs le reutilisent sans relire.

3. `ban_deep_news(ticker, isin, url, reason)`
   Si un article est du bruit (non pertinent, pub, contenu generique), banne-le.
   Raisons: "noise", "not_relevant", "stale", "duplicate", "advertising".
   Les articles bannis seront filtres dans les prochains runs.

4. `persist_shared_insights(ticker, isin, insights={{analyse_technique, risques, catalyseurs, ...}})`
   Persiste tes analyses generiques pour enrichir les prochains runs d'autres investisseurs.

5. `persist_extracted_fundamentals(ticker, isin, fundamentals={{pe_ratio, profit_margin, ...}})`
   Si tu as trouve des fondamentaux manquants via recherche web, persiste-les."#,
        run_id = run_id,
    )
}

// ── Helpers ──────────────────────────────────────────────────────

pub(crate) fn has_missing_market_fundamentals(market: Option<&Value>) -> bool {
    let m = match market {
        Some(v) if v.is_object() => v,
        _ => return true,
    };
    m.get("pe_ratio").and_then(|v| v.as_f64()).is_none()
        || m.get("revenue_growth").and_then(|v| v.as_f64()).is_none()
        || m.get("profit_margin").and_then(|v| v.as_f64()).is_none()
        || m.get("debt_to_equity").and_then(|v| v.as_f64()).is_none()
}

fn as_text(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

fn as_text_upper(v: Option<&Value>) -> String {
    as_text(v).to_uppercase()
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..s.char_indices().nth(max).map(|(i,_)|i).unwrap_or(s.len())]) }
}

// ── CSV column adaptation prompt ────────────────────────────────

pub(crate) fn build_csv_adaptation_prompt(
    headers: &[String],
    sample_rows: &[Vec<String>],
    delimiter: char,
) -> String {
    let header_line = headers.join(" | ");
    let sample_lines: Vec<String> = sample_rows
        .iter()
        .take(5)
        .map(|row| row.join(" | "))
        .collect();

    format!(
        r#"Tu reçois un fichier CSV de portefeuille boursier avec un format inconnu.
Le délimiteur est '{delimiter}'.

HEADERS (colonnes):
{header_line}

EXEMPLE DE LIGNES (max 5):
{samples}

Ta tâche: identifier quelles colonnes correspondent à ces champs obligatoires et optionnels.

Champs OBLIGATOIRES (le CSV est invalide sans eux):
- ticker: le code ticker ou symbole de l'action (ex: MC, AAPL, NL0000235190)
- quantite: le nombre de parts/actions détenues

Champs OPTIONNELS:
- nom: le nom complet de l'entreprise
- isin: le code ISIN (ex: FR0000121014)
- prix_actuel: le cours/prix actuel
- valeur_actuelle: la valorisation totale de la ligne
- prix_revient: le prix de revient unitaire (PRU)
- plus_moins_value: la plus ou moins-value latente
- compte: le nom du compte (PEA, CTO, etc.)

Réponds UNIQUEMENT avec un JSON valide:
{{
  "column_mapping": {{
    "ticker": <index_colonne>,
    "quantite": <index_colonne>,
    "nom": <index_colonne_ou_null>,
    "isin": <index_colonne_ou_null>,
    "prix_actuel": <index_colonne_ou_null>,
    "valeur_actuelle": <index_colonne_ou_null>,
    "prix_revient": <index_colonne_ou_null>,
    "plus_moins_value": <index_colonne_ou_null>,
    "compte": <index_colonne_ou_null>
  }},
  "header_row_index": <0_si_la_premiere_ligne_est_le_header>,
  "number_format": "french" | "english",
  "confidence": "high" | "medium" | "low"
}}"#,
        samples = sample_lines.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write a mock report history artifact to a temp dir.
    fn write_history_artifact(dir: &std::path::Path, filename: &str, account: &str, synthese: &str) {
        let artifact = json!({
            "account": account,
            "payload": {
                "synthese_marche": synthese
            }
        });
        fs::write(
            dir.join(filename),
            serde_json::to_string(&artifact).unwrap(),
        ).unwrap();
    }

    fn with_history_dir<F: FnOnce(&std::path::Path)>(f: F) {
        // Serialize env-var mutation across all tests in this module.
        let _guard = crate::helpers::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "alfred-history-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ALFRED_REPORT_HISTORY_DIR", dir.as_os_str());
        f(&dir);
        std::env::remove_var("ALFRED_REPORT_HISTORY_DIR");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_previous_syntheses_filters_by_account() {
        with_history_dir(|dir| {
            // 2 PEA entries, 1 CTO entry
            write_history_artifact(dir, "20260401_run1.json", "PEA", "Synthese PEA premier");
            write_history_artifact(dir, "20260402_run2.json", "PEA", "Synthese PEA second");
            write_history_artifact(dir, "20260403_run3.json", "CTO", "Synthese CTO");

            let result = load_previous_syntheses(2, "PEA");
            assert_eq!(result.len(), 2, "should return 2 PEA entries");
            for (_, synthese) in &result {
                assert!(synthese.contains("PEA"), "synthese should be from PEA account");
            }
        });
    }

    #[test]
    fn load_previous_syntheses_empty_account_returns_most_recent() {
        with_history_dir(|dir| {
            write_history_artifact(dir, "20260401_run1.json", "PEA", "PEA synthese");
            write_history_artifact(dir, "20260402_run2.json", "CTO", "CTO synthese");
            write_history_artifact(dir, "20260403_run3.json", "PEA", "PEA synthese 2");

            let result = load_previous_syntheses(2, "");
            assert_eq!(result.len(), 2, "empty account filter should return 2 most recent");
            // Dates are sorted descending (most recent first)
            assert!(result[0].0 >= result[1].0, "should be sorted most-recent-first");
        });
    }

    #[test]
    fn load_previous_syntheses_unknown_account_returns_empty() {
        with_history_dir(|dir| {
            write_history_artifact(dir, "20260401_run1.json", "PEA", "PEA synthese");

            let result = load_previous_syntheses(2, "UNKNOWN");
            assert!(result.is_empty(), "unknown account should return empty");
        });
    }

    #[test]
    fn load_previous_syntheses_nonexistent_dir_returns_empty() {
        let _guard = crate::helpers::test_env_lock();
        std::env::set_var("ALFRED_REPORT_HISTORY_DIR", "/tmp/nonexistent-dir-alfred-test-xyz");
        let result = load_previous_syntheses(2, "PEA");
        std::env::remove_var("ALFRED_REPORT_HISTORY_DIR");
        assert!(result.is_empty(), "nonexistent dir should return empty");
    }
}
