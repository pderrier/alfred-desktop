export function normalizeAgentGuidanceValue(value) {
  return String(value || "").trim();
}

export function resolveAgentGuidanceInputValue(settings = null) {
  const values = settings?.values && typeof settings.values === "object" ? settings.values : {};
  return normalizeAgentGuidanceValue(values.agent_guidelines || "");
}

export function buildAgentGuidanceSettingsPatch(value) {
  return {
    agent_guidelines: normalizeAgentGuidanceValue(value)
  };
}

export function hasPersistedAgentGuidanceChanges(currentValue, persistedValue) {
  return normalizeAgentGuidanceValue(currentValue) !== normalizeAgentGuidanceValue(persistedValue);
}
