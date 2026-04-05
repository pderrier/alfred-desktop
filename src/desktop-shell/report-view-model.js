function createUiCodedError(code, message) {
  const error = new Error(message || code);
  error.code = code;
  return error;
}

function asNumber(value, fallback = null) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  return fallback;
}

function asText(value, fallback = "") {
  const normalized = String(value || "").trim();
  return normalized || fallback;
}

function normalizeKeywords(raw) {
  if (!Array.isArray(raw)) {
    return [];
  }
  return raw
    .map((item) => asText(item))
    .filter(Boolean)
    .slice(0, 8);
}

function normalizeNewsItem(item) {
  return {
    title: asText(item?.titre || item?.title, "Untitled"),
    url: asText(item?.url || item?.link, ""),
    source: asText(item?.source, "unknown"),
    date: asText(item?.date || item?.published_at, ""),
    summary: asText(item?.resume || item?.summary, "")
  };
}

function normalizeLineMemory(raw = {}, fallback = {}) {
  const nested = raw && typeof raw === "object" ? raw : {};
  const base = fallback && typeof fallback === "object" ? fallback : {};
  return {
    llm_memory_summary: asText(
      nested?.llm_memory_summary || base?.llm_memory_summary || base?.summary || base?.synthese
    ),
    llm_strong_signals: Array.isArray(nested?.llm_strong_signals)
      ? nested.llm_strong_signals
      : Array.isArray(base?.llm_strong_signals)
        ? base.llm_strong_signals
        : [],
    llm_key_history: Array.isArray(nested?.llm_key_history)
      ? nested.llm_key_history
      : Array.isArray(base?.llm_key_history)
        ? base.llm_key_history
        : [],
    deep_news_memory_summary: asText(
      nested?.deep_news_memory_summary || base?.deep_news_memory_summary || base?.deep_news_summary
    ),
    deep_news_selected_url: asText(nested?.deep_news_selected_url || base?.deep_news_selected_url),
    deep_news_seen_urls: Array.isArray(nested?.deep_news_seen_urls)
      ? nested.deep_news_seen_urls
      : Array.isArray(base?.deep_news_seen_urls)
        ? base.deep_news_seen_urls
        : [],
    deep_news_banned_urls: Array.isArray(nested?.deep_news_banned_urls)
      ? nested.deep_news_banned_urls
      : Array.isArray(base?.deep_news_banned_urls)
        ? base.deep_news_banned_urls
        : []
  };
}

function normalizeAnalysisDetails(rec, latestRun) {
  const ticker = asText(rec?.ticker).toUpperCase();
  const lineId = asText(rec?.line_id || rec?.id);
  const runMatches =
    latestRun &&
    typeof latestRun === "object" &&
    (!rec?.run_id || asText(rec?.run_id) === asText(latestRun?.run_id));
  const positions = Array.isArray(latestRun?.portfolio?.positions) ? latestRun.portfolio.positions : [];
  const row =
    runMatches && ticker
      ? positions.find((entry) => asText(entry?.ticker).toUpperCase() === ticker) || null
      : null;
  const market =
    runMatches && latestRun?.market && typeof latestRun.market === "object" && ticker
      ? latestRun.market[ticker] || {}
      : {};
  const newsRows =
    runMatches && latestRun?.news && typeof latestRun.news === "object" && ticker
      ? Array.isArray(latestRun.news[ticker]?.articles)
        ? latestRun.news[ticker].articles
        : []
      : [];
  const quality =
    runMatches && latestRun?.quality?.by_ticker && typeof latestRun.quality.by_ticker === "object" && ticker
      ? latestRun.quality.by_ticker[ticker] || null
      : null;
  const enrichmentFailures =
    runMatches && Array.isArray(latestRun?.enrichment?.failures)
      ? latestRun.enrichment.failures.filter((f) => asText(f?.ticker).toUpperCase() === ticker)
      : [];
  return {
    line_id: lineId,
    position: row
      ? {
          nom: asText(row?.nom),
          quantite: row?.quantite ?? null,
          poids_pct: row?.poids_pct ?? null,
          prix_actuel: row?.prix_actuel ?? null,
          plus_moins_value_pct: row?.plus_moins_value_pct ?? null
        }
      : null,
    market:
      market && typeof market === "object"
        ? {
            prix_actuel: market?.prix_actuel ?? null,
            pe_ratio: market?.pe_ratio ?? null,
            revenue_growth: market?.revenue_growth ?? null,
            profit_margin: market?.profit_margin ?? null,
            debt_to_equity: market?.debt_to_equity ?? null
          }
        : {},
    news: newsRows.map(normalizeNewsItem),
    quality: quality
      ? {
          missing_market_fundamentals: Array.isArray(quality.missing_market_fundamentals)
            ? quality.missing_market_fundamentals
            : [],
          news_quality_score: quality.news_quality_score ?? null,
          needs_enrichment: quality.needs_enrichment === true,
          reasons: Array.isArray(quality.reasons) ? quality.reasons : []
        }
      : null,
    enrichmentIssues: enrichmentFailures.map((f) => ({
      scope: asText(f?.scope),
      error_code: asText(f?.error_code),
      message: asText(f?.message),
      provider: asText(f?.provider),
      upstream_status: f?.upstream_status ?? null
    })),
    marketSource: market?.source || null,
    analysis: {
      analyse_technique: asText(rec?.analyse_technique),
      analyse_fondamentale: asText(rec?.analyse_fondamentale),
      analyse_sentiment: asText(rec?.analyse_sentiment),
      raisons_principales: Array.isArray(rec?.raisons_principales) ? rec.raisons_principales : [],
      risques: Array.isArray(rec?.risques) ? rec.risques : [],
      catalyseurs: Array.isArray(rec?.catalyseurs) ? rec.catalyseurs : [],
      deep_news_summary: asText(rec?.deep_news_summary),
      deep_news_selected_url: asText(rec?.deep_news_selected_url)
    }
  };
}

function normalizeRecommendation(rec, index, latestRun) {
  const ticker = asText(rec?.ticker, "N/A");
  const name = asText(rec?.nom || rec?.name);
  const signal = asText(rec?.signal, "N/A");
  const conviction = asText(rec?.conviction, "N/A");
  const summary = asText(rec?.summary || rec?.synthese || rec?.analyse, "No summary.");
  const action = asText(rec?.action_recommandee || rec?.action || rec?.decision, "N/A");
  const type = asText(rec?.type, "position");
  const id = asText(rec?.id, `${ticker}_${index + 1}`);
  const lineMemory = normalizeLineMemory(rec?.memoire_ligne || rec?.line_memory, rec);
  return {
    id,
    lineId: asText(rec?.line_id || id, id),
    ticker,
    name,
    signal,
    conviction,
    summary,
    action,
    type,
    reanalyseAfter: asText(rec?.reanalyse_after),
    reanalyseReason: asText(rec?.reanalyse_reason),
    keywords: normalizeKeywords(rec?.badges_keywords),
    provenance: buildRecommendationProvenance(rec, latestRun, { source: "recommendation" }),
    lineMemory,
    details: normalizeAnalysisDetails(rec, latestRun)
  };
}

function buildCollectedOnlyRecommendation(row, index, latestRun) {
  const ticker = asText(row?.ticker, "N/A").toUpperCase();
  const type = asText(row?.type, "position");
  const lineId = asText(row?.line_id, `${type}:${ticker}`);
  const lineMemory = normalizeLineMemory(row?.memoire_ligne || row?.line_memory, row);
  return {
    id: asText(row?.id, lineId || `collected_${index + 1}`),
    lineId,
    ticker,
    name: asText(row?.nom || row?.name),
    signal: "COLLECTED",
    conviction: asText(row?.analysis_status || "pending"),
    summary: "Collected data is available for this line, but no LLM recommendation was persisted.",
    action: "Inspect collected data",
    type,
    keywords: [],
    provenance: buildRecommendationProvenance(row, latestRun, { source: "collected_only" }),
    lineMemory,
    details: normalizeAnalysisDetails(
      {
        line_id: lineId,
        ticker,
        nom: row?.nom || row?.name,
        type,
        analyse_technique: "",
        analyse_fondamentale: "",
        analyse_sentiment: "",
        raisons_principales: [],
        risques: [],
        catalyseurs: [],
        deep_news_summary: asText(row?.deep_news_summary),
        deep_news_selected_url: asText(row?.deep_news_selected_url)
      },
      latestRun
    )
  };
}

function buildRecommendationProvenance(rec, latestRun, { source = "recommendation" } = {}) {
  const labels = [];
  if (latestRun?.source_ingestion?.source_mode === "finary" || latestRun?.portfolio) {
    labels.push("Finary snapshot");
  }
  if (latestRun?.market || latestRun?.news) {
    labels.push("Provider enrichment");
  }
  if (source === "collected_only") {
    labels.push("Collected without LLM recommendation");
  } else {
    labels.push("LLM recommendation");
  }
  if (rec?.memoire_ligne || rec?.line_memory || rec?.llm_memory_summary) {
    labels.push("Memory reuse");
  }
  if (rec?.line_validation?.repaired === true || rec?.validation_context?.repaired === true) {
    labels.push("Validation repair");
  }
  return labels;
}

function buildCollectedOnlyRecommendations(latestRun, existingRecommendations = []) {
  const positions = Array.isArray(latestRun?.portfolio?.positions) ? latestRun.portfolio.positions : [];
  if (positions.length === 0) {
    return [];
  }
  const seenLineIds = new Set(
    (Array.isArray(existingRecommendations) ? existingRecommendations : [])
      .map((row) => asText(row?.line_id || row?.lineId || row?.id))
      .filter(Boolean)
  );
  const seenTickers = new Set(
    (Array.isArray(existingRecommendations) ? existingRecommendations : [])
      .map((row) => asText(row?.ticker).toUpperCase())
      .filter(Boolean)
  );
  return positions
    .filter((row) => {
      const ticker = asText(row?.ticker).toUpperCase();
      const type = asText(row?.type, "position");
      const lineId = asText(row?.line_id, `${type}:${ticker}`);
      if (!ticker) {
        return false;
      }
      return !seenLineIds.has(lineId) && !seenTickers.has(ticker);
    })
    .map((row, index) => buildCollectedOnlyRecommendation(row, index, latestRun));
}

function normalizeActionItem(item, index) {
  const quantity = asNumber(item?.quantity, null);
  const priceLimit = asNumber(item?.limit_price ?? item?.price_limit ?? item?.priceLimit ?? item?.limitPrice, null);
  const estimatedAmountEur = asNumber(item?.estimated_amount_eur ?? item?.estimatedAmountEur, null);
  return {
    id: asText(item?.id, `action_${index + 1}`),
    priority: asText(item?.priority, String(index + 1)),
    action: asText(item?.action || item?.signal, "ACTION"),
    ticker: asText(item?.ticker, ""),
    nom: asText(item?.nom || item?.name, ""),
    orderType: asText(item?.order_type || item?.orderType, "MARKET"),
    quantity,
    priceLimit,
    estimatedAmountEur,
    rationale: asText(item?.rationale || item?.reason, "No rationale provided.")
  };
}

function hasSubstantiveReportPayload(payload) {
  if (!payload || typeof payload !== "object") {
    return false;
  }
  return Boolean(
    asText(payload.synthese_marche) ||
      (Array.isArray(payload.actions_immediates) && payload.actions_immediates.length > 0) ||
      (Array.isArray(payload.recommandations) && payload.recommandations.length > 0)
  );
}

function hasSubstantiveRunPayload(run) {
  if (!run || typeof run !== "object") {
    return false;
  }
  return Boolean(
    hasSubstantiveReportPayload(run?.composed_payload) ||
      (Array.isArray(run?.pending_recommandations) && run.pending_recommandations.length > 0)
  );
}

function parseTimestamp(value) {
  const parsed = Date.parse(String(value || ""));
  return Number.isFinite(parsed) ? parsed : null;
}

export function buildReportViewModel(dashboardPayload) {
  const snapshot = dashboardPayload?.snapshot || {};
  const latestRun = snapshot.latest_run || null;
  const latestRunSummary =
    snapshot.latest_run_summary && typeof snapshot.latest_run_summary === "object"
      ? snapshot.latest_run_summary
      : {};
  const latestReport = snapshot.latest_report || null;
  const reportHistory = Array.isArray(snapshot.report_history) ? snapshot.report_history : [];
  const latestReportPayload = latestReport?.payload || {};
  const latestHistoricalReport =
    reportHistory.find((report) => hasSubstantiveReportPayload(report?.payload || {})) ||
    reportHistory[0] ||
    null;
  const latestSummary = snapshot.latest_report_summary || {};
  const currentRunPayload =
    latestRun && typeof latestRun === "object" && latestRun.composed_payload && typeof latestRun.composed_payload === "object"
      ? latestRun.composed_payload
      : {};
  const runStatus = asText(latestRunSummary?.status || latestRun?.status).toLowerCase();
  const latestRunIsAuthoritative = hasSubstantiveRunPayload(latestRun);
  const effectiveArtifactReport = latestRunIsAuthoritative
    ? null
    : hasSubstantiveReportPayload(latestReportPayload)
      ? latestReport
      : latestHistoricalReport || latestReport;
  const effectiveArtifactPayload = effectiveArtifactReport?.payload || {};
  const fallbackRecommendations = Array.isArray(currentRunPayload.recommandations)
    ? currentRunPayload.recommandations
    : Array.isArray(latestRun?.pending_recommandations)
      ? latestRun.pending_recommandations
      : [];
  const effectiveRecommendationsSource = latestRunIsAuthoritative
    ? fallbackRecommendations
    : Array.isArray(effectiveArtifactPayload.recommandations)
      ? effectiveArtifactPayload.recommandations
      : fallbackRecommendations;
  const effectiveRunId = latestRun?.run_id || effectiveArtifactReport?.run_id || null;

  const recommendations = Array.isArray(effectiveRecommendationsSource)
    ? effectiveRecommendationsSource.map((row, index) =>
        normalizeRecommendation(
          {
            ...row,
            run_id: effectiveRunId
          },
          index,
          latestRun
        )
      )
    : [];
  const collectedOnlyRecommendations = buildCollectedOnlyRecommendations(latestRun, recommendations);
  const effectiveRecommendations = recommendations.concat(collectedOnlyRecommendations);
  const effectiveActions = latestRunIsAuthoritative
    ? Array.isArray(currentRunPayload.actions_immediates)
      ? currentRunPayload.actions_immediates
      : []
    : Array.isArray(effectiveArtifactPayload.actions_immediates)
      ? effectiveArtifactPayload.actions_immediates
      : Array.isArray(currentRunPayload.actions_immediates)
        ? currentRunPayload.actions_immediates
        : [];
  const NON_ACTIONABLE = new Set(["CONSERVER", "SURVEILLANCE", "HOLD", "WATCH", "MONITOR"]);
  const actionsNow = Array.isArray(effectiveActions)
    ? effectiveActions.map(normalizeActionItem).filter((a) => !NON_ACTIONABLE.has((a.action || "").toUpperCase()))
    : [];
  // Enrich action names from recommendations or positions
  const positions = latestRun?.portfolio?.positions || [];
  for (const action of actionsNow) {
    if (action.nom) continue;
    const upper = (action.ticker || "").toUpperCase();
    const rec = effectiveRecommendations.find((r) => (r.ticker || "").toUpperCase() === upper);
    if (rec?.nom) { action.nom = rec.nom; continue; }
    const pos = positions.find((p) => (p.ticker || "").toUpperCase() === upper);
    if (pos?.nom) { action.nom = pos.nom; }
  }
  const hasPartialArtifacts =
    !effectiveArtifactReport &&
    (effectiveRecommendations.length > 0 ||
      actionsNow.length > 0 ||
      Boolean(asText(currentRunPayload.synthese_marche)) ||
      (Array.isArray(latestRun?.portfolio?.positions) && latestRun.portfolio.positions.length > 0));
  const isRunCompleted = runStatus === "completed" || runStatus === "completed_degraded";
  const artifactState = latestRunIsAuthoritative && isRunCompleted
    ? "final"
    : hasPartialArtifacts
      ? runStatus === "completed_degraded"
        ? "degraded"
        : "partial"
      : effectiveArtifactReport
        ? "final"
        : "empty";
  const fallbackSynthesis =
    asText(currentRunPayload.synthese_marche) ||
    (effectiveRecommendations.length > 0
      ? "Collected line data is available for the latest incomplete or failed run."
      : "");
  const provenanceSummary = [];
  if (effectiveArtifactReport) {
    provenanceSummary.push("Final report artifact");
  } else if (artifactState === "degraded") {
    provenanceSummary.push("Degraded latest-run artifact");
  } else if (artifactState === "partial") {
    provenanceSummary.push("Partial latest-run artifact");
  }
  if (latestRun?.portfolio) {
    provenanceSummary.push("Finary collection");
  }
  if (latestRun?.market || latestRun?.news) {
    provenanceSummary.push("Provider enrichment");
  }
  if (effectiveRecommendations.some((item) => item.provenance.includes("Memory reuse"))) {
    provenanceSummary.push("Memory reuse");
  }
  if (effectiveRecommendations.some((item) => item.provenance.includes("LLM recommendation"))) {
    provenanceSummary.push("LLM analysis");
  }

  return {
    value: asNumber(
      latestRunIsAuthoritative ? currentRunPayload.valeur_portefeuille : effectiveArtifactPayload.valeur_portefeuille,
      asNumber(currentRunPayload.valeur_portefeuille, asNumber(latestSummary.valeur_portefeuille, null))
    ),
    gain: asNumber(
      latestRunIsAuthoritative
        ? currentRunPayload.plus_value_totale
        : effectiveArtifactPayload.plus_value_totale,
      asNumber(currentRunPayload.plus_value_totale, asNumber(latestSummary.plus_value_totale, null))
    ),
    cash: asNumber(
      latestRunIsAuthoritative ? currentRunPayload.liquidites : effectiveArtifactPayload.liquidites,
      asNumber(currentRunPayload.liquidites, asNumber(latestSummary.liquidites, null))
    ),
    recommendationCount:
      effectiveRecommendations.length > 0
        ? effectiveRecommendations.length
        : asNumber(latestSummary.recommandations_count, 0),
    synthesis: asText(
      (latestRunIsAuthoritative ? currentRunPayload.synthese_marche : effectiveArtifactPayload.synthese_marche) ||
        fallbackSynthesis,
      "No synthesis yet."
    ),
    watchlistSummary: asText(
      (latestRunIsAuthoritative ? currentRunPayload.opportunites_watchlist : effectiveArtifactPayload.opportunites_watchlist) ||
        currentRunPayload.opportunites_watchlist,
      ""
    ),
    nextAnalysis: asText(
      (latestRunIsAuthoritative ? currentRunPayload.prochaine_analyse : effectiveArtifactPayload.prochaine_analyse) ||
        currentRunPayload.prochaine_analyse,
      ""
    ),
    lastUpdate: asText(
      (latestRunIsAuthoritative
        ? currentRunPayload.date
        : effectiveArtifactReport?.saved_at || effectiveArtifactPayload.date) ||
        currentRunPayload.date ||
        latestRun?.updated_at ||
        latestRunSummary?.updated_at,
      "n/a"
    ),
    artifactState,
    artifactLabel:
      artifactState === "partial"
        ? "Latest run (partial)"
        : artifactState === "degraded"
          ? "Latest run (degraded)"
          : artifactState === "final"
            ? "Report"
            : "No report",
    account: asText(latestRun?.account || latestRunSummary?.account, null),
    accounts: Array.isArray(snapshot.latest_finary_snapshot?.accounts) ? snapshot.latest_finary_snapshot.accounts : [],
    provenanceSummary,
    recommendations: effectiveRecommendations,
    allocation: buildAllocation(latestRun, effectiveRecommendations),
    previousRun: buildPreviousRunDelta(reportHistory, {
      value: asNumber(
        latestRunIsAuthoritative
          ? currentRunPayload.valeur_portefeuille
          : effectiveArtifactPayload.valeur_portefeuille,
        null
      ),
      gain: asNumber(
        latestRunIsAuthoritative
          ? currentRunPayload.plus_value_totale
          : effectiveArtifactPayload.plus_value_totale,
        null
      ),
      recoCount: effectiveRecommendations.length
    }),
    actionsNow
  };
}

function buildPreviousRunDelta(reportHistory, current) {
  if (!Array.isArray(reportHistory) || reportHistory.length < 2) return null;
  // Find the previous report (second entry — first is current)
  const prev = reportHistory[1]?.payload || reportHistory[1] || {};
  const prevValue = asNumber(prev.valeur_portefeuille, null);
  const prevGain = asNumber(prev.plus_value_totale, null);
  const prevRecoCount = Array.isArray(prev.recommandations) ? prev.recommandations.length : 0;
  if (prevValue === null && prevGain === null) return null;
  return {
    valueDelta: current.value != null && prevValue != null ? current.value - prevValue : null,
    gainDelta: current.gain != null && prevGain != null ? current.gain - prevGain : null,
    recoCountDelta: current.recoCount - prevRecoCount,
    prevDate: reportHistory[1]?.saved_at || null
  };
}

function buildAllocation(latestRun, recommendations) {
  const positions = latestRun?.portfolio?.positions || [];
  const totalValue = asNumber(latestRun?.portfolio?.valeur_totale, 0);
  const cash = asNumber(latestRun?.portfolio?.liquidites, 0);
  if (totalValue <= 0 || positions.length === 0) return [];

  const recByTicker = new Map();
  for (const rec of recommendations) {
    recByTicker.set((rec.ticker || "").toUpperCase(), rec);
  }

  const items = positions
    .filter((p) => p.type !== "watchlist")
    .map((p) => {
      const ticker = asText(p.ticker).toUpperCase();
      const value = asNumber(p.valeur_actuelle, 0);
      const weight = totalValue > 0 ? (value / totalValue) * 100 : 0;
      const rec = recByTicker.get(ticker);
      const signal = rec?.signal || "";
      const tone = signal.includes("ACHAT") || signal.includes("RENFORC") ? "buy"
        : signal.includes("VENTE") || signal.includes("ALLEG") ? "sell"
        : "neutral";
      return { ticker, nom: asText(p.nom), value, weight, signal, tone };
    })
    .sort((a, b) => b.weight - a.weight);

  if (cash > 0) {
    items.push({ ticker: "CASH", nom: "Cash", value: cash, weight: (cash / totalValue) * 100, signal: "", tone: "cash" });
  }
  return items;
}

export function buildRunAnalysisOptions({
  source,
  account = "",
  csvText = "",
  csvExportPath = "",
  agentGuidelines = "",
  runMode = "full_run"
} = {}) {
  const normalizedSource = asText(source).toLowerCase();
  const guidance = asText(agentGuidelines);
  const accountName = asText(account) || null;
  const mode = asText(runMode, "full_run");
  if (normalizedSource === "finary") {
    return {
      portfolio_source: "finary",
      account: accountName,
      agent_guidelines: guidance || null,
      run_mode: mode
    };
  }
  if (normalizedSource === "finary_cached") {
    return {
      portfolio_source: "finary",
      account: accountName,
      run_on_latest_finary_snapshot: true,
      agent_guidelines: guidance || null,
      run_mode: mode
    };
  }
  if (normalizedSource !== "csv") {
    throw createUiCodedError("source_selection_invalid", "source_selection_invalid");
  }

  const trimmedCsv = asText(csvText);
  if (trimmedCsv) {
    return {
      portfolio_source: "csv",
      account: accountName,
      agent_guidelines: guidance || null,
      run_mode: mode,
      csv_upload: {
        csv_text: trimmedCsv
      }
    };
  }

  const exportPath = asText(csvExportPath);
  if (exportPath) {
    return {
      portfolio_source: "csv",
      account: accountName,
      agent_guidelines: guidance || null,
      run_mode: mode,
      latest_export: exportPath
    };
  }

  throw createUiCodedError("csv_input_missing", "csv_input_missing");
}
