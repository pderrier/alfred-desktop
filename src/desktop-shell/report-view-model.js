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
      nested?.llm_memory_summary || base?.llm_memory_summary
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
        : [],
    // V2 fields — pass through from raw line memory
    signal_history: Array.isArray(nested?.signal_history)
      ? nested.signal_history
      : Array.isArray(base?.signal_history)
        ? base.signal_history
        : [],
    key_reasoning: asText(nested?.key_reasoning || base?.key_reasoning),
    price_tracking: (nested?.price_tracking && typeof nested.price_tracking === "object")
      ? nested.price_tracking
      : (base?.price_tracking && typeof base.price_tracking === "object")
        ? base.price_tracking
        : null,
    news_themes: Array.isArray(nested?.news_themes)
      ? nested.news_themes
      : Array.isArray(base?.news_themes)
        ? base.news_themes
        : [],
    trend: asText(nested?.trend || base?.trend)
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
          prix_revient: row?.prix_revient ?? null,
          poids_pct: row?.poids_pct ?? null,
          prix_actuel: row?.prix_actuel ?? null,
          plus_moins_value_pct: row?.plus_moins_value_pct ?? null,
          valorisation: row?.valorisation ?? null
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

// ── Unified data source resolution (replaces multi-path fallback cascade) ──
function hasRecos(source) {
  return source && typeof source === "object"
    && Array.isArray(source.recommandations) && source.recommandations.length > 0;
}
function sameRunId(report, run) {
  if (!report || !run) return false;
  const a = String(report.run_id || "").trim();
  const b = String(run.run_id || "").trim();
  return a !== "" && a === b;
}
function resolveDataSource(latestRun, latestReport) {
  if (hasRecos(latestRun?.composed_payload)) return "composed";
  if (Array.isArray(latestRun?.pending_recommandations) && latestRun.pending_recommandations.length > 0) return "pending";
  if (sameRunId(latestReport, latestRun) && hasRecos(latestReport?.payload)) return "artifact";
  return "empty";
}

function parseTimestamp(value) {
  const parsed = Date.parse(String(value || ""));
  return Number.isFinite(parsed) ? parsed : null;
}

function normalizeThemeLabel(value) {
  return asText(value).replace(/\s+/g, " ").trim();
}

function aggregateRisksFromRecommendations(recommendations = []) {
  const riskMap = new Map();
  for (const rec of recommendations) {
    const ticker = asText(rec?.ticker).toUpperCase();
    const risks = Array.isArray(rec?.details?.analysis?.risques)
      ? rec.details.analysis.risques
      : [];
    for (const rawRisk of risks) {
      const label = normalizeThemeLabel(rawRisk);
      if (!label) continue;
      if (!riskMap.has(label)) {
        riskMap.set(label, { risk: label, count: 0, tickers: new Set() });
      }
      const row = riskMap.get(label);
      row.count += 1;
      if (ticker) row.tickers.add(ticker);
    }
  }
  return [...riskMap.values()]
    .map((entry) => ({
      risk: entry.risk,
      count: entry.count,
      tickers: [...entry.tickers].sort(),
    }))
    .sort((a, b) => {
      if (b.count !== a.count) return b.count - a.count;
      return a.risk.localeCompare(b.risk);
    });
}

function buildCrossAccountThemeView(snapshot, currentAccount = "") {
  const runs = Array.isArray(snapshot?.runs) ? snapshot.runs : [];
  const latestByAccount = new Map();
  for (const run of runs) {
    const account = asText(run?.account);
    if (!account) continue;
    const runTs = parseTimestamp(run?.updated_at || run?.saved_at || run?.created_at || run?.date) || 0;
    const prev = latestByAccount.get(account);
    if (!prev || runTs > prev.ts) {
      latestByAccount.set(account, { run, ts: runTs });
    }
  }

  const themeMap = new Map();
  for (const [account, holder] of latestByAccount.entries()) {
    const themes = holder?.run?.composed_payload?.theme_concentration?.themes;
    if (!Array.isArray(themes)) continue;
    for (const rawTheme of themes) {
      const label = normalizeThemeLabel(rawTheme?.theme);
      if (!label) continue;
      if (!themeMap.has(label)) {
        themeMap.set(label, { theme: label, totalCount: 0, accounts: new Set() });
      }
      const entry = themeMap.get(label);
      entry.totalCount += asNumber(rawTheme?.count, 0) || 0;
      entry.accounts.add(account);
    }
  }

  const rows = [...themeMap.values()]
    .map((entry) => ({
      theme: entry.theme,
      totalCount: entry.totalCount,
      accountCount: entry.accounts.size,
      accounts: [...entry.accounts].sort(),
      inCurrentAccount: currentAccount ? entry.accounts.has(currentAccount) : false,
    }))
    .sort((a, b) => {
      if (b.accountCount !== a.accountCount) return b.accountCount - a.accountCount;
      if (b.totalCount !== a.totalCount) return b.totalCount - a.totalCount;
      return a.theme.localeCompare(b.theme);
    });
  return rows.slice(0, 8);
}

function buildThemeRiskInsight({ snapshot, latestRun, recommendations, themeConcentration }) {
  const themeRows = Array.isArray(themeConcentration?.themes)
    ? [...themeConcentration.themes]
        .map((entry) => ({
          theme: normalizeThemeLabel(entry?.theme),
          count: asNumber(entry?.count, 0) || 0,
          tickers: Array.isArray(entry?.tickers) ? entry.tickers.filter(Boolean) : [],
        }))
        .filter((entry) => entry.theme)
        .sort((a, b) => (b.count || 0) - (a.count || 0))
    : [];
  const topThemes = themeRows.slice(0, 5);
  const riskRows = aggregateRisksFromRecommendations(recommendations);
  const topRisks = riskRows.slice(0, 5);
  const currentAccount = asText(latestRun?.account);
  const globalThemes = buildCrossAccountThemeView(snapshot, currentAccount);
  const sharedThemeCount = topThemes.filter((theme) =>
    globalThemes.some((row) => row.theme === theme.theme && row.accountCount > 1)
  ).length;

  const highlights = [];
  if (topThemes.length > 0) {
    highlights.push(
      `${topThemes.length} thème${topThemes.length > 1 ? "s" : ""} dominant${topThemes.length > 1 ? "s" : ""} (max ${topThemes[0].count} positions sur ${topThemes[0].theme}).`
    );
  }
  if (topRisks.length > 0) {
    highlights.push(
      `${topRisks.length} risque${topRisks.length > 1 ? "s" : ""} récurrent${topRisks.length > 1 ? "s" : ""} identifié${topRisks.length > 1 ? "s" : ""}; ${topRisks[0].risk}.`
    );
  }
  if (globalThemes.length > 0) {
    highlights.push(
      sharedThemeCount > 0
        ? `${sharedThemeCount} thème${sharedThemeCount > 1 ? "s" : ""} est/sont aussi concentré(s) dans d'autres comptes.`
        : "Les thèmes concentrés semblent plutôt spécifiques à ce compte."
    );
  }

  return {
    topThemes,
    topRisks,
    globalThemes,
    sharedThemeCount,
    highlights,
  };
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

  // Unified data source resolution — single cascade, no multi-path fallbacks
  const dataSource = resolveDataSource(latestRun, latestReport);
  const primaryPayload =
    dataSource === "composed" ? currentRunPayload
    : dataSource === "artifact" ? (latestReport?.payload || {})
    : {};
  const effectiveRecommendationsSource =
    dataSource === "composed" ? currentRunPayload.recommandations
    : dataSource === "pending" ? latestRun.pending_recommandations
    : dataSource === "artifact" ? (latestReport?.payload?.recommandations || [])
    : [];
  // Backward compat aliases used downstream
  const latestRunIsAuthoritative = dataSource === "composed" || dataSource === "pending";
  const effectiveArtifactReport = dataSource === "artifact" ? latestReport : null;
  const effectiveArtifactPayload = effectiveArtifactReport?.payload || {};
  const effectiveRunId = latestRun?.run_id || effectiveArtifactReport?.run_id || null;
  const themeConcentration = (latestRunIsAuthoritative
    ? currentRunPayload.theme_concentration
    : effectiveArtifactPayload.theme_concentration) || null;

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
  const effectiveActions = Array.isArray(primaryPayload.actions_immediates) && primaryPayload.actions_immediates.length > 0
    ? primaryPayload.actions_immediates
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
      primaryPayload.valeur_portefeuille,
      asNumber(currentRunPayload.valeur_portefeuille, asNumber(latestSummary.valeur_portefeuille, null))
    ),
    gain: asNumber(
      primaryPayload.plus_value_totale,
      asNumber(currentRunPayload.plus_value_totale, asNumber(latestSummary.plus_value_totale, null))
    ),
    cash: asNumber(
      primaryPayload.liquidites,
      asNumber(currentRunPayload.liquidites, asNumber(latestSummary.liquidites, null))
    ),
    recommendationCount:
      effectiveRecommendations.length > 0
        ? effectiveRecommendations.length
        : asNumber(latestSummary.recommandations_count, 0),
    synthesis: asText(
      primaryPayload.synthese_marche || currentRunPayload.synthese_marche || fallbackSynthesis,
      "No synthesis yet."
    ),
    watchlistSummary: asText(
      primaryPayload.opportunites_watchlist || currentRunPayload.opportunites_watchlist,
      ""
    ),
    nextAnalysis: asText(
      primaryPayload.prochaine_analyse || currentRunPayload.prochaine_analyse,
      ""
    ),
    lastUpdate: asText(
      primaryPayload.date ||
        effectiveArtifactReport?.saved_at ||
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
    actionsNow,
    // Phase 2b: theme concentration from composed_payload
    themeConcentration,
    themeRiskInsight: buildThemeRiskInsight({
      snapshot,
      latestRun,
      recommendations: effectiveRecommendations,
      themeConcentration
    })
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
      force_finary_refresh: true,
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
