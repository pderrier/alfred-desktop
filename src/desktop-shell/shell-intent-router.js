function normalizeIntent(intent) {
  return String(intent || "").trim().toLowerCase();
}

export function resolveShellIntentRoute(intent) {
  const safeIntent = normalizeIntent(intent);
  switch (safeIntent) {
    case "start_analysis":
      return {
        type: "open_wizard",
        tab: "run",
        preferredMode: null,
        statusText: "Choose how Alfred should get your portfolio data.",
        statusClass: "status-idle"
      };
    case "reconnect_finary":
      return {
        type: "open_wizard",
        tab: "run",
        preferredMode: null,
        statusText: "Reconnect Finary or choose another source to continue.",
        statusClass: "status-idle"
      };
    case "use_csv":
      return {
        type: "open_wizard",
        tab: "run",
        preferredMode: "csv",
        statusText: "Provide CSV input, then click Start Analysis.",
        statusClass: "status-idle"
      };
    case "use_latest_snapshot":
      return {
        type: "open_wizard",
        tab: "run",
        preferredMode: "finary_cached",
        statusText: "Use the latest saved Finary snapshot, then click Start Analysis.",
        statusClass: "status-idle"
      };
    case "inspect_partial_lines":
      return {
        type: "switch_tab",
        tab: "lines"
      };
    case "retry_global_synthesis":
      return {
        type: "run_action",
        action: "retry_global_synthesis",
        tab: "overview",
        statusText: "Retrying global synthesis from the saved line results...",
        statusClass: "status-loading"
      };
    case "open_diagnostics":
      return {
        type: "switch_tab",
        tab: "diagnostics"
      };
    case "open_settings":
      return {
        type: "switch_tab",
        tab: "settings"
      };
    default:
      return {
        type: "switch_tab",
        tab: "overview"
      };
  }
}
