export const SHELL_TABS = [
  { id: "overview", label: "Overview" },
  { id: "run", label: "Run Analysis" },
  { id: "lines", label: "Lines" },
  { id: "history", label: "History" },
  { id: "diagnostics", label: "Diagnostics" },
  { id: "settings", label: "Settings" }
];

const DEFAULT_TAB_ID = "overview";

export function resolveShellTabState(activeTab = DEFAULT_TAB_ID) {
  const safeActiveTab = SHELL_TABS.some((tab) => tab.id === activeTab) ? activeTab : DEFAULT_TAB_ID;
  return {
    activeTab: safeActiveTab,
    tabs: SHELL_TABS.map((tab) => ({
      ...tab,
      selected: tab.id === safeActiveTab
    }))
  };
}
