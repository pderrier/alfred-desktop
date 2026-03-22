export function isFinarySessionRunnable(sessionPayload) {
  if (!sessionPayload || typeof sessionPayload !== "object") {
    return false;
  }
  const valid = sessionPayload.session_state === "valid" || sessionPayload.valid === true;
  const requiresReauth = sessionPayload.requires_reauth === true || sessionPayload.requiresReauth === true;
  return valid && !requiresReauth;
}

export function hasLatestFinarySnapshot(snapshotPayload) {
  return Boolean(snapshotPayload && typeof snapshotPayload === "object" && snapshotPayload.available === true);
}

export function hasCsvRunnableInput({ csvText = "", csvExportPath = "" } = {}) {
  return String(csvText || "").trim().length > 0 || String(csvExportPath || "").trim().length > 0;
}

export function isLatestSnapshotSameDay(snapshotPayload, now = new Date()) {
  if (!hasLatestFinarySnapshot(snapshotPayload)) {
    return false;
  }
  const parsed = new Date(String(snapshotPayload?.saved_at || "").trim());
  if (Number.isNaN(parsed.getTime())) {
    return false;
  }
  return (
    parsed.getFullYear() === now.getFullYear() &&
    parsed.getMonth() === now.getMonth() &&
    parsed.getDate() === now.getDate()
  );
}

export function deriveRunWizardSourcePolicy({ sessionPayload, latestFinarySnapshot = null } = {}) {
  const finaryRunnable = isFinarySessionRunnable(sessionPayload);
  const finaryCachedRunnable = !finaryRunnable && hasLatestFinarySnapshot(latestFinarySnapshot);
  return {
    showCsvSource: true,
    showFinaryRunSource: true,
    finaryRunnable,
    finaryCachedRunnable
  };
}

export function deriveRunWizardModeOptions({
  sessionPayload,
  latestFinarySnapshot = null,
  now = new Date()
} = {}) {
  const snapshotAvailable = hasLatestFinarySnapshot(latestFinarySnapshot);
  const sameDaySnapshot = isLatestSnapshotSameDay(latestFinarySnapshot, now);
  const sessionRunnable = isFinarySessionRunnable(sessionPayload);
  const options = [];

  if (snapshotAvailable && sameDaySnapshot) {
    options.push({
      value: "finary_cached_forced",
      source: "finary_cached",
      label: "Use today's Finary snapshot",
      help: "Reuse the snapshot already collected today.",
      requiresRunnableSession: false,
      csv: false,
      forced: true
    });
    options.push({
      value: "finary_resync",
      source: "finary",
      label: "Force resync from Finary",
      help: sessionRunnable
        ? "Collect a fresh snapshot from Finary even though one was already captured today."
        : "Requires a valid Finary session.",
      requiresRunnableSession: true,
      csv: false,
      forced: false
    });
  } else {
    options.push({
      value: "finary_resync",
      source: "finary",
      label: "Sync from Finary now",
      help: sessionRunnable
        ? "Collect a fresh portfolio snapshot from Finary before analysis starts."
        : "Requires a valid Finary session before Alfred can resync your portfolio.",
      requiresRunnableSession: true,
      csv: false,
      forced: false
    });
    if (snapshotAvailable) {
      options.push({
        value: "finary_cached",
        source: "finary_cached",
        label: "Use latest Finary snapshot",
        help: "Skip a new Finary poll and analyze the latest stored snapshot instead.",
        requiresRunnableSession: false,
        csv: false,
        forced: false
      });
    }
  }

  options.push({
    value: "csv",
    source: "csv",
    label: "Use CSV import",
    help: "Run from pasted CSV content or a local export folder instead of Finary.",
    requiresRunnableSession: false,
    csv: true,
    forced: false
  });

  return options;
}

