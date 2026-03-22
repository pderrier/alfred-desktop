export const SHELL_REFRESH_POLICY = {
  idleIntervalMs: 15_000,
  activeRunIntervalMs: 8_000
};

const ACTIVE_RUN_STATUSES = new Set(["running"]);

export function isLatestRunSummaryActive(latestRunSummary = null) {
  const status = String(latestRunSummary?.status || "").trim().toLowerCase();
  return ACTIVE_RUN_STATUSES.has(status);
}

export function resolveShellRefreshPlan({
  activeRun = false,
  latestRunSummary = null,
  idleIntervalMs = SHELL_REFRESH_POLICY.idleIntervalMs,
  activeRunIntervalMs = SHELL_REFRESH_POLICY.activeRunIntervalMs
} = {}) {
  const runActive = activeRun === true || isLatestRunSummaryActive(latestRunSummary);
  return {
    runActive,
    intervalMs: runActive ? activeRunIntervalMs : idleIntervalMs,
    refreshDashboard: true,
    refreshFinarySession: !runActive
  };
}
