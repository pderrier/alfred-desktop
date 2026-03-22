export function formatProgressCounter(label, progress) {
  if (
    !progress ||
    !Number.isFinite(Number(progress.completed)) ||
    !Number.isFinite(Number(progress.total)) ||
    Number(progress.total) <= 0
  ) {
    return null;
  }
  return `${label} ${Number(progress.completed)}/${Number(progress.total)}`;
}

export function humanizeRunStage(stage) {
  const normalized = String(stage || "").trim().toLowerCase();
  if (!normalized) {
    return "running";
  }
  const LABELS = {
    starting: "starting",
    collecting_data: "collecting portfolio data",
    collecting_market_data: "collecting market data",
    analyzing_lines: "analyzing lines",
    llm_generating: "generating global synthesis",
    composing_report: "finalizing global synthesis",
    syncing_line_memory: "saving line memory",
    completed: "completed",
    completed_degraded: "completed in degraded mode",
    failed: "failed"
  };
  return LABELS[normalized] || normalized.replaceAll("_", " ");
}

export function formatStageLabel(stage, { collectionProgress = null, lineProgress = null } = {}) {
  const base = humanizeRunStage(stage);
  const parts = [];
  const collectionPart = formatProgressCounter("collecting", collectionProgress);
  const linePart = formatProgressCounter("analyzing", lineProgress);
  if (collectionPart) {
    parts.push(collectionPart);
  }
  if (linePart) {
    parts.push(linePart);
  }
  if (parts.length > 0) {
    return `${base} (${parts.join(" · ")})`;
  }
  return base;
}

export function runStatusTone(status) {
  const normalized = String(status || "").trim().toLowerCase();
  if (normalized === "completed" || normalized === "success") {
    return "success";
  }
  if (normalized === "running" || normalized === "accepted" || normalized === "starting") {
    return "running";
  }
  if (normalized === "completed_degraded" || normalized === "degraded") {
    return "warning";
  }
  if (normalized === "failed" || normalized === "failed_with_partial") {
    return "error";
  }
  return "idle";
}
