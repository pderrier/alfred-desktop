import {
  RESILIENCE_POLICY,
  isAnalysisStatusPollingTransientError,
  normalizeResilienceTimeout
} from "./resilience-policy.js";

function createBridgeError(code, message) {
  return {
    code,
    message
  };
}

function normalizeAnalysisStatusInvokeError(error) {
  const normalized = normalizeInvokeError(error);
  if (isAnalysisStatusPollingTransientError(normalized)) {
    return createBridgeError(
      "analysis_status_polling_unavailable",
      `analysis_status_polling_unavailable:${normalized.message || normalized.code || "analysis_run_status_local"}`
    );
  }
  return normalized;
}

const DEFAULT_INVOKE_TIMEOUT_MS = RESILIENCE_POLICY.invokeTimeoutMs;
const DEFAULT_DASHBOARD_SNAPSHOT_INVOKE_TIMEOUT_MS = RESILIENCE_POLICY.dashboardSnapshotInvokeTimeoutMs;
const DEFAULT_FINARY_BROWSER_FLOW_INVOKE_TIMEOUT_MS = RESILIENCE_POLICY.finaryBrowserFlowInvokeTimeoutMs;
const DEFAULT_ANALYSIS_RUN_INVOKE_TIMEOUT_MS = RESILIENCE_POLICY.analysisRunInvokeTimeoutMs;
const DEFAULT_ANALYSIS_STATUS_INVOKE_TIMEOUT_MS = RESILIENCE_POLICY.analysisStatusInvokeTimeoutMs;

function isWrapperCode(code) {
  const normalized = String(code || "").trim().toLowerCase();
  return normalized.endsWith("_local_failed") || normalized === "bridge_error";
}

function inferCodedErrorFromText(raw) {
  const text = String(raw || "").trim();
  if (!text) {
    return null;
  }
  const loweredText = text.toLowerCase();
  const priorityCodes = [
    "reauth_required",
    "browser_session_not_materialized",
    "finary_connector_unavailable",
    "finary_rust_bridge_unavailable"
  ];
  for (const code of priorityCodes) {
    if (loweredText.includes(code)) {
      return code;
    }
  }
  const duplicatedTail = text.match(/:([a-z][a-z0-9_]{2,}):\1(?:\s|$)/i);
  if (duplicatedTail?.[1]) {
    return duplicatedTail[1].toLowerCase();
  }
  const candidates = text.match(/\b([a-z]+(?:_[a-z0-9]+)+)\b/gi);
  if (!Array.isArray(candidates) || candidates.length === 0) {
    return null;
  }
  const ignore = new Set([
    "node_bin",
    "script_path",
    "invalid_json",
    "bridge_error",
    "token_success",
    "client_status",
    "auth1_cookie1",
    "auth1_cookie0",
    "auth0_cookie1"
  ]);
  const filtered = candidates
    .map((value) => String(value).toLowerCase())
    .filter((value) => !ignore.has(value));
  if (filtered.length === 0) {
    return null;
  }
  const nonWrapper = filtered.filter((value) => !isWrapperCode(value));
  const selected = nonWrapper.length > 0 ? nonWrapper : filtered;
  return String(selected[selected.length - 1]).toLowerCase();
}

function normalizeInvokeError(error) {
  const rawMessage = String(error?.message || error || "bridge_invoke_failed");
  if (error && typeof error === "object" && typeof error.code === "string" && error.code.trim()) {
    const explicitCode = String(error.code).trim();
    const inferredFromMessage = inferCodedErrorFromText(rawMessage);
    const normalizedCode =
      isWrapperCode(explicitCode) && inferredFromMessage ? inferredFromMessage : explicitCode;
    return createBridgeError(normalizedCode, rawMessage || normalizedCode);
  }
  const inferredCode = inferCodedErrorFromText(rawMessage) || "bridge_invoke_failed";
  return createBridgeError(inferredCode, rawMessage || inferredCode);
}

function scoreBridgeError(error) {
  const code = String(error?.code || "").trim().toLowerCase();
  if (!code) return 0;
  if (code !== "bridge_invoke_failed" && code !== "bridge_payload_invalid") return 3;
  if (code === "bridge_payload_invalid") return 2;
  return 1;
}

function pickMoreInformativeError(current, candidate) {
  if (!current) {
    return candidate;
  }
  if (!candidate) {
    return current;
  }
  const currentScore = scoreBridgeError(current);
  const candidateScore = scoreBridgeError(candidate);
  if (candidateScore > currentScore) {
    return candidate;
  }
  if (candidateScore === currentScore && String(candidate.message || "").length > String(current.message || "").length) {
    return candidate;
  }
  return current;
}

function resolveInvoke(globalObject) {
  return globalObject?.__TAURI__?.core?.invoke || null;
}

function resolveFetch(globalObject) {
  if (!globalObject?.fetch) {
    return null;
  }
  return globalObject.fetch.bind(globalObject);
}

function normalizeTauriPayload(payload, acceptedActions) {
  const action = payload?.action;
  const actionOk =
    Array.isArray(acceptedActions) &&
    acceptedActions.length > 0 &&
    acceptedActions.includes(action);
  if (!payload || payload.ok !== true || !actionOk || !payload.result) {
    throw createBridgeError("bridge_payload_invalid", "bridge_payload_invalid");
  }
  return payload.result;
}

async function invokeWithFallback(
  invoke,
  commandNames,
  acceptedActions,
  invokeArgs = undefined,
  { timeoutMs = DEFAULT_INVOKE_TIMEOUT_MS } = {}
) {
  const effectiveTimeoutMs =
    Number.isFinite(Number(timeoutMs)) && Number(timeoutMs) > 0
      ? Number(timeoutMs)
      : DEFAULT_INVOKE_TIMEOUT_MS;
  let lastError = null;
  for (const command of commandNames) {
    try {
      const payload = await new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          reject(createBridgeError("bridge_invoke_timeout", `bridge_invoke_timeout:${command}`));
        }, effectiveTimeoutMs);
        Promise.resolve(invokeArgs === undefined ? invoke(command) : invoke(command, invokeArgs)).then(
          (result) => {
            clearTimeout(timer);
            resolve(result);
          },
          (error) => {
            clearTimeout(timer);
            reject(error);
          }
        );
      });
      return normalizeTauriPayload(payload, acceptedActions);
    } catch (error) {
      lastError = pickMoreInformativeError(lastError, normalizeInvokeError(error));
    }
  }
  throw lastError || createBridgeError("bridge_invoke_failed", "bridge_invoke_failed");
}

async function parseHttpResponse(response, fallbackCode) {
  const raw =
    typeof response?.text === "function"
      ? await response.text()
      : typeof response?.json === "function"
        ? JSON.stringify(await response.json())
        : "";
  if (!raw) {
    throw createBridgeError("bridge_response_invalid_json", `${fallbackCode}:empty_body`);
  }
  let payload = null;
  try {
    payload = raw ? JSON.parse(raw) : null;
  } catch (error) {
    throw createBridgeError("bridge_response_invalid_json", `${fallbackCode}:invalid_json`);
  }
  if (!response.ok || payload?.ok !== true) {
    const code = payload?.error_code || fallbackCode;
    throw createBridgeError(code, payload?.message || code);
  }
  return payload;
}

async function parseConnectorResponse(response, fallbackCode) {
  const raw =
    typeof response?.text === "function"
      ? await response.text()
      : typeof response?.json === "function"
        ? JSON.stringify(await response.json())
        : "";
  if (!raw) {
    throw createBridgeError("bridge_response_invalid_json", `${fallbackCode}:empty_body`);
  }
  let payload = null;
  try {
    payload = raw ? JSON.parse(raw) : null;
  } catch (error) {
    throw createBridgeError("bridge_response_invalid_json", `${fallbackCode}:invalid_json`);
  }
  if (!response.ok) {
    const code = payload?.error_code || fallbackCode;
    throw createBridgeError(code, payload?.message || code);
  }
  return payload;
}

function normalizeSessionPayload(payload, fallbackCode) {
  if (!payload || typeof payload !== "object") {
    throw createBridgeError(fallbackCode, fallbackCode);
  }
  const session = payload.session && typeof payload.session === "object" ? payload.session : payload;
  if (!session || typeof session !== "object") {
    throw createBridgeError(fallbackCode, fallbackCode);
  }
  return session;
}

function normalizeBrowserAuthPayload(payload, fallbackCode) {
  if (!payload || typeof payload !== "object") {
    throw createBridgeError(fallbackCode, fallbackCode);
  }
  const browserAuth =
    payload.browser_auth && typeof payload.browser_auth === "object" ? payload.browser_auth : payload;
  if (!browserAuth || typeof browserAuth !== "object") {
    throw createBridgeError(fallbackCode, fallbackCode);
  }
  const loginUrl = String(browserAuth.login_url || "").trim();
  if (!loginUrl) {
    throw createBridgeError(fallbackCode, fallbackCode);
  }
  return browserAuth;
}

function normalizeExternalUrl(raw) {
  const value = String(raw || "").trim();
  if (!value) {
    throw createBridgeError("external_url_invalid", "external_url_invalid");
  }
  try {
    const parsed = new URL(value);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new Error("invalid_scheme");
    }
    return parsed.toString();
  } catch {
    throw createBridgeError("external_url_invalid", "external_url_invalid");
  }
}

export function createDesktopBridgeClient({
  globalObject = globalThis,
  invoke = resolveInvoke(globalObject),
  fetchFn = resolveFetch(globalObject),
  invokeTimeoutMs = DEFAULT_INVOKE_TIMEOUT_MS,
  dashboardSnapshotInvokeTimeoutMs = DEFAULT_DASHBOARD_SNAPSHOT_INVOKE_TIMEOUT_MS,
  finaryBrowserFlowInvokeTimeoutMs = DEFAULT_FINARY_BROWSER_FLOW_INVOKE_TIMEOUT_MS,
  analysisRunInvokeTimeoutMs = DEFAULT_ANALYSIS_RUN_INVOKE_TIMEOUT_MS,
  analysisStatusInvokeTimeoutMs = DEFAULT_ANALYSIS_STATUS_INVOKE_TIMEOUT_MS
} = {}) {
  const resolvedDashboardSnapshotInvokeTimeoutMs = normalizeResilienceTimeout(
    dashboardSnapshotInvokeTimeoutMs,
    DEFAULT_DASHBOARD_SNAPSHOT_INVOKE_TIMEOUT_MS
  );
  const resolvedFinaryBrowserFlowInvokeTimeoutMs = normalizeResilienceTimeout(
    finaryBrowserFlowInvokeTimeoutMs,
    DEFAULT_FINARY_BROWSER_FLOW_INVOKE_TIMEOUT_MS
  );
  const resolvedAnalysisRunInvokeTimeoutMs = normalizeResilienceTimeout(
    analysisRunInvokeTimeoutMs,
    DEFAULT_ANALYSIS_RUN_INVOKE_TIMEOUT_MS
  );
  const resolvedAnalysisStatusInvokeTimeoutMs = normalizeResilienceTimeout(
    analysisStatusInvokeTimeoutMs,
    DEFAULT_ANALYSIS_STATUS_INVOKE_TIMEOUT_MS
  );
  async function runAnalysisToCompletion(options = {}) {
    const started = await bridge.startAnalysisRun(options);
    const operationId = String(started?.operation_id || "").trim();
    if (!operationId) {
      throw createBridgeError("analysis_operation_id_missing", "analysis_operation_id_missing");
    }
    for (;;) {
      const statusPayload = await bridge.getAnalysisRunStatus(operationId, {
        timeoutMs: resolvedAnalysisStatusInvokeTimeoutMs
      });
      const status = String(statusPayload?.status || "running").trim().toLowerCase();
      if (status === "completed") {
        return statusPayload?.result || {};
      }
      if (status === "failed") {
        const error = statusPayload?.error || {};
        throw createBridgeError(
          String(error.code || "analysis_run_failed").trim() || "analysis_run_failed",
          String(error.message || error.code || "analysis_run_failed")
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }
  const bridge = {
    mode: invoke ? "tauri" : "http",
    async getDashboardSnapshot() {
      const commandNames = ["dashboard_snapshot_local", "dashboard:snapshot-local"];
      if (invoke) {
        return invokeWithFallback(invoke, commandNames, commandNames, undefined, {
          timeoutMs: resolvedDashboardSnapshotInvokeTimeoutMs
        });
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/dashboard/snapshot");
      return parseHttpResponse(response, "bridge_dashboard_http_failed");
    },
    async getDashboardOverview() {
      const commandNames = ["dashboard_overview_local", "dashboard:overview-local"];
      if (invoke) {
        return invokeWithFallback(invoke, commandNames, commandNames, undefined, {
          timeoutMs: resolvedDashboardSnapshotInvokeTimeoutMs
        });
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/dashboard/overview");
      return parseHttpResponse(response, "bridge_dashboard_overview_http_failed");
    },
    async getDashboardDetails() {
      const commandNames = ["dashboard_details_local", "dashboard:details-local"];
      if (invoke) {
        return invokeWithFallback(invoke, commandNames, commandNames, undefined, {
          timeoutMs: resolvedDashboardSnapshotInvokeTimeoutMs
        });
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/dashboard/details");
      return parseHttpResponse(response, "bridge_dashboard_details_http_failed");
    },
    async getStackHealth() {
      const commandNames = ["stack_health_local", "stack:health-local"];
      if (invoke) {
        return invokeWithFallback(invoke, commandNames, commandNames, undefined, {
          timeoutMs: invokeTimeoutMs
        });
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/stack/health");
      return parseHttpResponse(response, "bridge_stack_health_http_failed");
    },
    async getRunById(runId) {
      const safeRunId = String(runId || "").trim();
      if (!safeRunId) {
        throw createBridgeError("run_id_required", "run_id_required");
      }
      const commandNames = ["run_by_id_local", "run:by-id-local"];
      if (invoke) {
        const payload = await invokeWithFallback(
          invoke,
          commandNames,
          ["run_by_id_local", "run:by-id-local"],
          { runId: safeRunId },
          { timeoutMs: invokeTimeoutMs }
        );
        return payload.run || null;
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn(`/api/runs/${encodeURIComponent(safeRunId)}`);
      const payload = await parseHttpResponse(response, "bridge_run_detail_http_failed");
      return payload.run || null;
    },
    async retryGlobalSynthesis(runId) {
      const safeRunId = String(runId || "").trim();
      if (!safeRunId) {
        throw createBridgeError("run_id_required", "run_id_required");
      }
      const commandNames = ["retry_global_synthesis_local", "analysis:retry-global-synthesis-local"];
      if (invoke) {
        return invokeWithFallback(
          invoke,
          commandNames,
          ["retry_global_synthesis_local", "analysis:retry-global-synthesis-local"],
          { runId: safeRunId },
          { timeoutMs: resolvedAnalysisRunInvokeTimeoutMs }
        );
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn(`/api/runs/${encodeURIComponent(safeRunId)}/retry-global-synthesis`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({})
      });
      return parseHttpResponse(response, "bridge_retry_global_synthesis_http_failed");
    },
    async getRuntimeSettings() {
      const commandNames = ["runtime_settings_local", "runtime:settings-local"];
      if (invoke) {
        const payload = await invokeWithFallback(
          invoke,
          commandNames,
          ["runtime:settings-local"],
          undefined,
          { timeoutMs: resolvedAnalysisRunInvokeTimeoutMs }
        );
        return payload.settings || null;
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/settings/runtime");
      const payload = await parseHttpResponse(response, "bridge_runtime_settings_http_failed");
      return payload.settings || null;
    },
    async updateRuntimeSettings(settings = {}) {
      const commandNames = ["runtime_settings_update_local", "runtime:settings-update-local"];
      if (invoke) {
        const payload = await invokeWithFallback(
          invoke,
          commandNames,
          ["runtime:settings-update-local"],
          { settings: settings || {} },
          { timeoutMs: resolvedAnalysisRunInvokeTimeoutMs }
        );
        return payload.settings || null;
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/settings/runtime", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ settings: settings || {} })
      });
      const payload = await parseHttpResponse(response, "bridge_runtime_settings_update_failed");
      return payload.settings || null;
    },
    async resetRuntimeSettings() {
      const commandNames = ["runtime_settings_reset_local", "runtime:settings-reset-local"];
      if (invoke) {
        const payload = await invokeWithFallback(
          invoke,
          commandNames,
          ["runtime:settings-reset-local"],
          undefined,
          { timeoutMs: resolvedAnalysisRunInvokeTimeoutMs }
        );
        return payload.settings || null;
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/settings/runtime/reset", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({})
      });
      const payload = await parseHttpResponse(response, "bridge_runtime_settings_reset_failed");
      return payload.settings || null;
    },
    async runAnalysis(options = {}) {
      return runAnalysisToCompletion(options);
    },
    async startAnalysisRun(options = {}) {
      const commandNames = ["analysis_run_start_local", "analysis:run-start-local"];
      if (invoke) {
        return invokeWithFallback(
          invoke,
          commandNames,
          ["analysis_run_start_local", "analysis:run-start-local"],
          { options: options || {} },
          { timeoutMs: resolvedAnalysisRunInvokeTimeoutMs }
        );
      }
      throw createBridgeError("analysis_run_requires_tauri", "analysis_run_requires_tauri");
    },
    async stopAnalysisRun(operationId) {
      const safeOperationId = String(operationId || "").trim();
      if (!safeOperationId) {
        throw createBridgeError("analysis_operation_id_required", "analysis_operation_id_required");
      }
      if (invoke) {
        return invokeWithFallback(
          invoke,
          ["analysis_stop_local"],
          ["analysis_stop_local"],
          { operationId: safeOperationId },
          { timeoutMs: 5000 }
        );
      }
      throw createBridgeError("analysis_stop_requires_tauri", "analysis_stop_requires_tauri");
    },
    async getAnalysisRunStatus(operationId, { timeoutMs = resolvedAnalysisStatusInvokeTimeoutMs } = {}) {
      const safeOperationId = String(operationId || "").trim();
      if (!safeOperationId) {
        throw createBridgeError("analysis_operation_id_required", "analysis_operation_id_required");
      }
      const commandNames = ["analysis_run_status_local", "analysis:run-status-local"];
      if (invoke) {
        try {
          return await invokeWithFallback(
            invoke,
            commandNames,
            ["analysis_run_status_local", "analysis:run-status-local"],
            { operationId: safeOperationId },
            { timeoutMs: normalizeResilienceTimeout(timeoutMs, resolvedAnalysisStatusInvokeTimeoutMs) }
          );
        } catch (error) {
          throw normalizeAnalysisStatusInvokeError(error);
        }
      }
      throw createBridgeError("analysis_status_requires_tauri", "analysis_status_requires_tauri");
    },
    async getFinarySessionStatus() {
      const commandNames = ["finary_session_status_local"];
      if (invoke) {
        const payload = await invokeWithFallback(
          invoke,
          commandNames,
          ["finary_session_status_local", "finary:session-status-local"],
          undefined,
          { timeoutMs: invokeTimeoutMs }
        );
        return normalizeSessionPayload(payload, "finary_session_status_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/source/finary/session");
      const payload = await parseConnectorResponse(response, "finary_session_status_failed");
      return normalizeSessionPayload(payload, "finary_session_status_invalid_payload");
    },
    async connectFinarySession(payload = {}) {
      const commandNames = ["finary_session_connect_local"];
      if (invoke) {
        const result = await invokeWithFallback(
          invoke,
          commandNames,
          ["finary_session_connect_local", "finary:session-connect-local"],
          {
          payload: payload || {}
          },
          { timeoutMs: invokeTimeoutMs }
        );
        return normalizeSessionPayload(result, "finary_session_connect_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/source/finary/session/connect", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(payload || {})
      });
      const result = await parseConnectorResponse(response, "finary_session_connect_failed");
      return normalizeSessionPayload(result, "finary_session_connect_invalid_payload");
    },
    async refreshFinarySession() {
      const commandNames = ["finary_session_refresh_local"];
      if (invoke) {
        const payload = await invokeWithFallback(invoke, commandNames, [
          "finary_session_refresh_local",
          "finary:session-refresh-local"
        ], undefined, { timeoutMs: invokeTimeoutMs });
        return normalizeSessionPayload(payload, "finary_session_refresh_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/source/finary/session/refresh", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}"
      });
      const payload = await parseConnectorResponse(response, "finary_session_refresh_failed");
      return normalizeSessionPayload(payload, "finary_session_refresh_invalid_payload");
    },
    async startFinaryBrowserSession() {
      const commandNames = ["finary_session_browser_start_local"];
      if (invoke) {
        const payload = await invokeWithFallback(invoke, commandNames, [
          "finary_session_browser_start_local",
          "finary:session-browser-start-local"
        ], undefined, { timeoutMs: invokeTimeoutMs });
        return normalizeBrowserAuthPayload(payload, "finary_session_browser_start_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/source/finary/session/browser/start");
      const payload = await parseConnectorResponse(response, "finary_session_browser_start_failed");
      return normalizeBrowserAuthPayload(payload, "finary_session_browser_start_invalid_payload");
    },
    async completeFinaryBrowserSession() {
      const commandNames = ["finary_session_browser_complete_local"];
      if (invoke) {
        const payload = await invokeWithFallback(invoke, commandNames, [
          "finary_session_browser_complete_local",
          "finary:session-browser-complete-local"
        ], undefined, { timeoutMs: invokeTimeoutMs });
        return normalizeSessionPayload(payload, "finary_session_browser_complete_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const response = await fetchFn("/api/source/finary/session/browser/complete", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}"
      });
      const payload = await parseConnectorResponse(response, "finary_session_browser_complete_failed");
      return normalizeSessionPayload(payload, "finary_session_browser_complete_invalid_payload");
    },
    async runFinaryPlaywrightBrowserSession() {
      const commandNames = ["finary_session_browser_playwright_local"];
      if (invoke) {
        const payload = await invokeWithFallback(invoke, commandNames, [
          "finary_session_browser_playwright_local",
          "finary:session-browser-playwright-local"
        ], undefined, { timeoutMs: resolvedFinaryBrowserFlowInvokeTimeoutMs });
        return normalizeSessionPayload(payload, "finary_session_browser_playwright_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      const browserAuth = await this.startFinaryBrowserSession();
      if (globalObject?.open && browserAuth?.login_url) {
        globalObject.open(String(browserAuth.login_url), "_blank", "noopener,noreferrer");
      }
      const payload = await this.completeFinaryBrowserSession();
      return normalizeSessionPayload(payload, "finary_session_browser_playwright_invalid_payload");
    },
    async reuseFinaryBrowserSession() {
      const commandNames = ["finary_session_browser_reuse_local"];
      if (invoke) {
        const payload = await invokeWithFallback(invoke, commandNames, [
          "finary_session_browser_reuse_local",
          "finary:session-browser-reuse-local"
        ], undefined, { timeoutMs: resolvedFinaryBrowserFlowInvokeTimeoutMs });
        return normalizeSessionPayload(payload, "finary_session_browser_reuse_invalid_payload");
      }
      if (!fetchFn) {
        throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
      }
      // HTTP fallback cannot run local materialization directly; complete endpoint still provides
      // connector-side artifact validation/materialization fallback when configured.
      const payload = await this.completeFinaryBrowserSession();
      return normalizeSessionPayload(payload, "finary_session_browser_reuse_invalid_payload");
    },
    async getCodexSessionStatus() {
      if (!invoke) {
        throw createBridgeError("codex_session_requires_tauri", "codex_session_requires_tauri");
      }
      try {
        const payload = await invoke("codex_session_status_local");
        const result = payload?.result || payload || {};
        return result;
      } catch (error) {
        console.error("[bridge] codex_session_status_local: error:", error);
        throw error;
      }
    },
    async codexSessionLogin() {
      if (!invoke) {
        throw createBridgeError("codex_session_requires_tauri", "codex_session_requires_tauri");
      }
      try {
        const payload = await invoke("codex_session_login_local");
        return payload?.result || payload || {};
      } catch (error) {
        console.error("[bridge] codex_session_login_local: error:", error);
        throw error;
      }
    },
    async codexSessionLogout() {
      if (!invoke) {
        throw createBridgeError("codex_session_requires_tauri", "codex_session_requires_tauri");
      }
      try {
        const payload = await invoke("codex_session_logout_local");
        return payload?.result || payload || {};
      } catch (error) {
        console.error("[bridge] codex_session_logout_local: error:", error);
        throw error;
      }
    },
    async openExternalUrl(url) {
      const normalizedUrl = normalizeExternalUrl(url);
      if (invoke) {
        let lastError = null;
        for (const command of ["desktop:open-external-url", "desktop_open_external_url"]) {
          try {
            const payload = await invoke(command, { url: normalizedUrl });
            return normalizeTauriPayload(payload, ["desktop:open-external-url", "desktop_open_external_url"]);
          } catch (error) {
            lastError = error;
          }
        }
        throw lastError || createBridgeError("bridge_invoke_failed", "bridge_invoke_failed");
      }
      if (globalObject?.open) {
        globalObject.open(normalizedUrl, "_blank", "noopener,noreferrer");
        return { ok: true, opened: true, url: normalizedUrl };
      }
      throw createBridgeError("bridge_transport_unavailable", "bridge_transport_unavailable");
    },
    async getStorageUsage() {
      const payload = await invoke("storage_usage_local");
      return normalizeTauriPayload(payload, ["storage_usage_local"]);
    },
    async pruneStorage(keep = 10) {
      const payload = await invoke("storage_prune_local", { keep });
      return normalizeTauriPayload(payload, ["storage_prune_local"]);
    },
    async clearDebugLog() {
      const payload = await invoke("storage_clear_log_local");
      return normalizeTauriPayload(payload, ["storage_clear_log_local"]);
    }
  };
  return bridge;
}
