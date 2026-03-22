export const RESILIENCE_POLICY = {
  invokeTimeoutMs: 15_000,
  dashboardSnapshotInvokeTimeoutMs: 45_000,
  finaryBrowserFlowInvokeTimeoutMs: 420_000,
  analysisRunInvokeTimeoutMs: 900_000,
  analysisStatusInvokeTimeoutMs: 30_000,
  analysisPollIntervalMs: 2_000,
  analysisPollTimeoutMs: 0,
  activeRunProgressRefreshThrottleMs: 15_000,
  activeRunDegradedRefreshCooldownMs: 10_000
};

export function normalizeResilienceTimeout(value, fallback) {
  const numeric = Number(value);
  return Number.isFinite(numeric) && numeric > 0 ? numeric : fallback;
}

export function normalizeResilienceDuration(value, fallback) {
  const numeric = Number(value);
  return Number.isFinite(numeric) && numeric >= 0 ? numeric : fallback;
}

export function isAnalysisStatusPollingTransientError(error) {
  const code = String(error?.code || "").trim().toLowerCase();
  const message = String(error?.message || "");
  if (code === "bridge_invoke_timeout" && message.includes("analysis_run_status_local")) {
    return true;
  }
  if (code === "analysis_status_polling_unavailable") {
    return true;
  }
  return false;
}
