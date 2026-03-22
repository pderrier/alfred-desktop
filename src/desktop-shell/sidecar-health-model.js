function getAlphaVantageCredentialStatus(runtimeSettings = null) {
  const credentials = Array.isArray(runtimeSettings?.advanced?.credentials)
    ? runtimeSettings.advanced.credentials
    : [];
  const entry = credentials.find((item) => String(item?.id || "").trim() === "alphavantage_api_key");
  return String(entry?.value || "").trim().toLowerCase() || null;
}

export function describeSidecarHealthLine(service = {}, { runtimeSettings = null } = {}) {
  const serviceName = String(service?.name || "unknown");
  const port = Number.isFinite(Number(service?.port)) ? Number(service.port) : null;
  const live = service?.live !== false;
  const ready = service?.ready === true || service?.ok === true || service?.accepted === true;
  const status = service?.ok === true
    ? "healthy"
    : service?.accepted === true
      ? "degraded (accepted)"
      : !live
        ? "unreachable"
        : !ready
          ? "alive, not ready"
          : "degraded";
  const details = [];

  if (serviceName === "enrichment-api") {
    const alphaVantage = service?.diagnostics?.market_data?.alphavantage;
    if (alphaVantage && typeof alphaVantage === "object") {
      const configured = alphaVantage.configured === true;
      const credentialStatus = getAlphaVantageCredentialStatus(runtimeSettings);
      details.push(configured ? "AlphaVantage loaded" : "AlphaVantage not loaded");
      if (!configured && credentialStatus === "configured") {
        details.push("restart enrichment-api to load copied key");
      }
    }
  }

  return `${serviceName}${port ? `:${port}` : ""} - ${status}${details.length > 0 ? ` · ${details.join(" · ")}` : ""}`;
}
