function parseComparableTimestamp(value) {
  const parsed = Date.parse(String(value || ""));
  return Number.isFinite(parsed) ? parsed : 0;
}

function hasSubstantiveLatestRunPayload(run) {
  if (!run || typeof run !== "object") {
    return false;
  }
  return Boolean(
    (Array.isArray(run?.pending_recommandations) && run.pending_recommandations.length > 0) ||
      (run?.composed_payload && typeof run.composed_payload === "object") ||
      (Array.isArray(run?.portfolio?.positions) && run.portfolio.positions.length > 0) ||
      (run?.market && typeof run.market === "object" && Object.keys(run.market).length > 0) ||
      (run?.news && typeof run.news === "object" && Object.keys(run.news).length > 0)
  );
}

export function buildHistoryRuns(snapshot = {}) {
  const directRuns = Array.isArray(snapshot?.runs) ? snapshot.runs.filter(Boolean) : [];
  if (directRuns.length > 0) {
    return directRuns;
  }

  const synthesized = [];
  const seenRunIds = new Set();
  const latestRun = snapshot?.latest_run && typeof snapshot.latest_run === "object" ? snapshot.latest_run : null;
  if (latestRun?.run_id) {
    seenRunIds.add(String(latestRun.run_id));
    synthesized.push({
      run_id: latestRun.run_id,
      status: latestRun?.orchestration?.status || latestRun?.status || "unknown",
      stage: latestRun?.orchestration?.stage || null,
      portfolio_source: latestRun?.portfolio_source || "runtime_state",
      updated_at: latestRun?.updated_at || latestRun?.orchestration?.finished_at || null
    });
  }

  const reportHistory = Array.isArray(snapshot?.report_history) ? snapshot.report_history : [];
  for (const report of reportHistory) {
    const runId = String(report?.run_id || "").trim();
    if (!runId || seenRunIds.has(runId)) {
      continue;
    }
    seenRunIds.add(runId);
    synthesized.push({
      run_id: runId,
      status: "completed",
      stage: null,
      portfolio_source: "report_history",
      updated_at: report?.saved_at || null
    });
  }

  return synthesized.sort(
    (left, right) => parseComparableTimestamp(right?.updated_at) - parseComparableTimestamp(left?.updated_at)
  );
}

export async function loadDashboardPayload({
  getDashboardOverview,
  getDashboardDetails,
  getDashboardSnapshot
} = {}) {
  const overviewPromise =
    typeof getDashboardOverview === "function" ? Promise.resolve().then(() => getDashboardOverview()) : Promise.resolve(null);
  const detailsPromise =
    typeof getDashboardDetails === "function" ? Promise.resolve().then(() => getDashboardDetails()) : Promise.resolve(null);

  const [overviewResult, detailsResult] = await Promise.allSettled([overviewPromise, detailsPromise]);
  const overviewPayload = overviewResult.status === "fulfilled" ? overviewResult.value : null;
  const detailsPayload = detailsResult.status === "fulfilled" ? detailsResult.value : null;

  if (overviewPayload || detailsPayload) {
    return {
      ok: true,
      snapshot: {
        ...(overviewPayload?.snapshot || {}),
        ...(detailsPayload?.snapshot || {})
      }
    };
  }

  if (typeof getDashboardSnapshot === "function") {
    return getDashboardSnapshot();
  }

  throw (detailsResult.status === "rejected" ? detailsResult.reason : overviewResult.reason) || new Error("dashboard_refresh_failed");
}

export async function hydrateDashboardSnapshot(snapshot = {}, { getRunById } = {}) {
  const latestRun = snapshot?.latest_run && typeof snapshot.latest_run === "object" ? snapshot.latest_run : null;
  const latestRunSummary =
    snapshot?.latest_run_summary && typeof snapshot.latest_run_summary === "object" ? snapshot.latest_run_summary : null;
  const historyRuns = buildHistoryRuns(snapshot);
  const runId = String(latestRunSummary?.run_id || historyRuns[0]?.run_id || "").trim();
  if ((latestRun && hasSubstantiveLatestRunPayload(latestRun)) || !runId || typeof getRunById !== "function") {
    return snapshot;
  }

  try {
    const hydratedRun = await getRunById(runId);
    if (!hydratedRun || typeof hydratedRun !== "object") {
      return snapshot;
    }
    return {
      ...snapshot,
      latest_run: hydratedRun && typeof hydratedRun === "object"
        ? {
            ...(latestRun || {}),
            ...hydratedRun
          }
        : hydratedRun
    };
  } catch {
    return snapshot;
  }
}
