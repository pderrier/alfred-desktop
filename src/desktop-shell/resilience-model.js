function asText(value) {
  return String(value || "").trim();
}

function asTimestamp(value) {
  const parsed = Date.parse(asText(value));
  return Number.isFinite(parsed) ? parsed : 0;
}

function hasPartialArtifacts(run) {
  return Boolean(
    (Array.isArray(run?.pending_recommandations) && run.pending_recommandations.length > 0) ||
      (Array.isArray(run?.portfolio?.positions) && run.portfolio.positions.length > 0) ||
      (run?.composed_payload && typeof run.composed_payload === "object")
  );
}

function asRunStatus(run) {
  return asText(run?.orchestration?.status || run?.status);
}

function hasSavedLineRecommendations(run) {
  return Boolean(
    (Array.isArray(run?.pending_recommandations) && run.pending_recommandations.length > 0) ||
      (Array.isArray(run?.composed_payload?.recommandations) && run.composed_payload.recommandations.length > 0)
  );
}

function canRetryGlobalSynthesis(run) {
  const status = asRunStatus(run);
  const degradationReason = asText(run?.orchestration?.degradation_reason || run?.composed_payload?.degradation_reason);
  const synthesis = asText(run?.composed_payload?.synthese_marche).toLowerCase();
  return (
    (status === "completed_degraded" || status === "completed") &&
    hasSavedLineRecommendations(run) &&
    (degradationReason === "litellm_generation_timeout" || synthesis.includes("synthese globale degradee"))
  );
}

function scoreRunSnapshot(run) {
  if (!run || typeof run !== "object") {
    return 0;
  }
  let score = 0;
  if (hasPartialArtifacts(run)) {
    score += 4;
  }
  if (Array.isArray(run?.pending_recommandations) && run.pending_recommandations.length > 0) {
    score += 2;
  }
  if (Array.isArray(run?.portfolio?.positions) && run.portfolio.positions.length > 0) {
    score += 1;
  }
  return score;
}

function pickBetterRunSnapshot(previousRun, nextRun) {
  if (!previousRun) {
    return nextRun || null;
  }
  if (!nextRun) {
    return previousRun;
  }
  const previousUpdatedAt = asTimestamp(previousRun?.updated_at || previousRun?.orchestration?.finished_at);
  const nextUpdatedAt = asTimestamp(nextRun?.updated_at || nextRun?.orchestration?.finished_at);
  const previousScore = scoreRunSnapshot(previousRun);
  const nextScore = scoreRunSnapshot(nextRun);
  if (nextUpdatedAt > previousUpdatedAt && nextScore >= previousScore) {
    return nextRun;
  }
  if (previousScore > nextScore && previousUpdatedAt >= nextUpdatedAt) {
    return previousRun;
  }
  if (nextScore > previousScore) {
    return nextRun;
  }
  return nextUpdatedAt >= previousUpdatedAt ? nextRun : previousRun;
}

function mergeReportHistory(previousHistory = [], nextHistory = []) {
  return Array.isArray(nextHistory) && nextHistory.length > 0 ? nextHistory : previousHistory;
}

function enrichNotification(event = {}) {
  const type = asText(event.type || event.event_type || "event");
  const normalized = {
    id: asText(event.id || `${type}:${event.ts || event.message || ""}`),
    ts: event.ts || new Date().toISOString(),
    type,
    tone: asText(event.status || "idle"),
    message: asText(event.message || type),
    intent: null,
    actionLabel: null
  };
  if (type === "run.polling_degraded") {
    normalized.tone = "warning";
    normalized.intent = "inspect_partial_lines";
    normalized.actionLabel = "Inspect partial output";
  } else if (type === "startup.warning" || type === "run.refresh_warning") {
    normalized.tone = "warning";
    normalized.intent = "open_diagnostics";
    normalized.actionLabel = "Open diagnostics";
  } else if (type === "run.failed") {
    normalized.tone = "error";
    normalized.intent = "start_analysis";
    normalized.actionLabel = "Retry analysis";
  } else if (type === "run.completed" && asText(event.status) === "degraded") {
    normalized.tone = "warning";
    normalized.intent = "inspect_partial_lines";
    normalized.actionLabel = "Review degraded output";
  }
  return normalized;
}

export function mergeDashboardPayloads(previousPayload = null, nextPayload = null) {
  if (!previousPayload) {
    return nextPayload;
  }
  if (!nextPayload) {
    return previousPayload;
  }
  const previousSnapshot = previousPayload?.snapshot || {};
  const nextSnapshot = nextPayload?.snapshot || {};
  return {
    ...previousPayload,
    ...nextPayload,
    snapshot: {
      ...previousSnapshot,
      ...nextSnapshot,
      latest_run: pickBetterRunSnapshot(previousSnapshot.latest_run, nextSnapshot.latest_run),
      latest_report: nextSnapshot.latest_report || previousSnapshot.latest_report || null,
      report_history: mergeReportHistory(previousSnapshot.report_history, nextSnapshot.report_history),
      audit_events:
        Array.isArray(nextSnapshot.audit_events) && nextSnapshot.audit_events.length > 0
          ? nextSnapshot.audit_events
          : previousSnapshot.audit_events || []
    }
  };
}

export function buildRecoveryActions({
  stackHealth = null,
  finarySession = null,
  latestRunSummary = null,
  latestFinarySnapshot = null,
  latestRun = null
} = {}) {
  const actions = [];
  if (finarySession?.requires_reauth === true) {
    actions.push({
      id: "reconnect_finary",
      label: "Reconnect Finary",
      description: "Refresh your session before launching another live sync.",
      intent: "reconnect_finary",
      tone: "warning"
    });
  }
  if (stackHealth?.status === "degraded") {
    actions.push({
      id: "open_diagnostics",
      label: "Open diagnostics",
      description: "Review which sidecar is degraded before retrying collection.",
      intent: "open_diagnostics",
      tone: "warning"
    });
  }
  if (canRetryGlobalSynthesis(latestRun)) {
    actions.push({
      id: "retry_global_synthesis",
      label: "Retry global synthesis",
      description: "Regenerate only the portfolio-wide synthesis and immediate actions from the saved line results.",
      intent: "retry_global_synthesis",
      tone: "success",
      runId: String(latestRun?.run_id || "").trim() || null
    });
  }
  if (latestRunSummary?.partial_artifacts_available === true) {
    actions.push({
      id: "inspect_partial_lines",
      label: "Inspect partial output",
      description: "Browse collected data and saved line artifacts from the latest run.",
      intent: "inspect_partial_lines",
      tone: "running"
    });
  }
  if (latestFinarySnapshot?.available === true) {
    actions.push({
      id: "use_latest_snapshot",
      label: "Use latest snapshot",
      description: "Run Alfred in degraded mode using the freshest saved Finary snapshot.",
      intent: "use_latest_snapshot",
      tone: "idle"
    });
  }
  actions.push({
    id: "retry_collection",
    label: "Retry analysis",
    description: "Restart collection and analysis with the current settings.",
    intent: "start_analysis",
    tone: "success"
  });
  return actions.slice(0, 4);
}

export function appendNotification(queue = [], event = {}) {
  const next = [enrichNotification(event), ...(Array.isArray(queue) ? queue : [])];
  const deduped = [];
  const seen = new Set();
  for (const item of next) {
    if (!item?.message) {
      continue;
    }
    const key = `${item.type}:${item.message}`;
    if (seen.has(key)) {
      continue;
    }
    seen.add(key);
    deduped.push(item);
    if (deduped.length >= 6) {
      break;
    }
  }
  return deduped;
}
