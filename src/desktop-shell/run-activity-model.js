import {
  formatProgressCounter,
  formatStageLabel,
  humanizeRunStage,
  runStatusTone
} from "../shared/run-progress-format.js";

function normalizeProgress(progress = null) {
  if (
    !progress ||
    !Number.isFinite(Number(progress.completed)) ||
    !Number.isFinite(Number(progress.total))
  ) {
    return null;
  }
  return {
    completed: Number(progress.completed),
    total: Number(progress.total)
  };
}

function terminalStateFromEvent(state, event) {
  return {
    ...state,
    active: false,
    status: String(event?.status || state.status || "idle"),
    stage: String(event?.stage || state.stage || "").trim() || null,
    pollingDegraded: false,
    statusText: String(event?.message || state.statusText || ""),
    updatedAt: String(event?.ts || new Date().toISOString())
  };
}

export function createRunActivityState() {
  return {
    active: false,
    status: "idle",
    operationId: null,
    stage: null,
    collectionProgress: null,
    lineProgress: null,
    pollingDegraded: false,
    statusText: "",
    updatedAt: null
  };
}

export function reduceRunActivityState(state = createRunActivityState(), event = null) {
  if (!event || typeof event !== "object") {
    return state;
  }
  const type = String(event.type || "").trim();
  if (type === "run.started") {
    return {
      ...createRunActivityState(),
      active: true,
      status: "starting",
      statusText: "Running analysis...",
      updatedAt: String(event?.ts || new Date().toISOString())
    };
  }
  if (type === "run.accepted") {
    return {
      ...state,
      active: true,
      status: "running",
      operationId: String(event?.operation_id || state.operationId || "").trim() || null,
      statusText: "Running analysis...",
      updatedAt: String(event?.ts || new Date().toISOString())
    };
  }
  if (type === "run.progress") {
    const stage = String(event?.stage || state.stage || "running").trim();
    const collectionProgress = normalizeProgress(event?.collection_progress);
    const lineProgress = normalizeProgress(event?.line_progress);
    const stageLabel = formatStageLabel(stage, {
      collectionProgress,
      lineProgress
    });
    return {
      ...state,
      active: true,
      status: String(event?.status || "running"),
      stage,
      collectionProgress,
      lineProgress,
      statusText: `Running analysis... (${stageLabel})`,
      updatedAt: String(event?.ts || new Date().toISOString())
    };
  }
  if (type === "run.polling_degraded") {
    return {
      ...state,
      active: true,
      pollingDegraded: true,
      statusText:
        state.stage || state.collectionProgress || state.lineProgress
          ? `${state.statusText || "Running analysis..."} · status polling degraded`
          : "Running analysis... (status polling degraded)",
      updatedAt: String(event?.ts || new Date().toISOString())
    };
  }
  if (type === "run.polling_recovered") {
    return {
      ...state,
      active: true,
      pollingDegraded: false,
      statusText: state.stage
        ? `Running analysis... (${formatStageLabel(state.stage, {
            collectionProgress: state.collectionProgress,
            lineProgress: state.lineProgress
          })})`
        : "Running analysis...",
      updatedAt: String(event?.ts || new Date().toISOString())
    };
  }
  if (type === "run.completed" || type === "run.failed") {
    return terminalStateFromEvent(state, event);
  }
  return state;
}

export function buildRunActivityDisplay({
  runActivity = null,
  latestRunSummary = null
} = {}) {
  const activeState = runActivity && runActivity.active === true ? runActivity : null;
  if (activeState) {
    const parts = [
      formatProgressCounter("collected", activeState.collectionProgress),
      formatProgressCounter("analyzed", activeState.lineProgress),
      activeState.pollingDegraded ? "polling degraded" : null
    ].filter(Boolean);
    const suffix = parts.length > 0 ? ` · ${parts.join(" · ")}` : "";
    return {
      statusText: activeState.statusText || "Running analysis...",
      statusClass: "status-loading",
      kpiText: `Run: ${activeState.status || "running"}${suffix}`,
      kpiTone: runStatusTone(activeState.status || "running")
    };
  }

  const latest = latestRunSummary && typeof latestRunSummary === "object" ? latestRunSummary : null;
  const latestCollectionProgress = formatProgressCounter("collected", latest?.collection_progress);
  const latestLineProgress = formatProgressCounter("analyzed", latest?.line_progress);
  const latestStage = String(latest?.stage || "").trim();
  const latestStageLabel = latestStage ? humanizeRunStage(latestStage) : null;
  const latestArtifacts =
    latest?.partial_artifacts_available === true
      ? `${Number(latest?.pending_recommendations_count || 0)} line artifacts`
      : null;

  return {
    statusText: "",
    statusClass: "status-idle",
    kpiText: latest
      ? `Run: ${latest.status || "unknown"} (${latest.run_id || "run"})${
          [latestStageLabel, latestCollectionProgress, latestLineProgress, latestArtifacts].filter(Boolean).length > 0
            ? ` · ${[latestStageLabel, latestCollectionProgress, latestLineProgress, latestArtifacts]
                .filter(Boolean)
                .join(" · ")}`
            : ""
        }`
      : "Run: pending",
    kpiTone: runStatusTone(latest?.status || "idle")
  };
}
