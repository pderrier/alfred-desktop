import {
  RESILIENCE_POLICY,
  isAnalysisStatusPollingTransientError,
  normalizeResilienceDuration
} from "./resilience-policy.js";
import { formatStageLabel } from "./run-progress-format.js";

const ERROR_HINTS = {
  service_unreachable: "start local stack with `npm run dev:stack`",
  service_unhealthy: "check local services health via /api/stack/health",
  desktop_preflight_failed: "run desktop preflight dependencies before analysis",
  csv_input_missing: "set CSV export path or provide CSV upload payload",
  finary_credentials_missing: "enter Finary email/password in the wizard auth form",
  finary_mfa_required: "enter current MFA code in the wizard auth form and retry",
  finary_signin_failed: "verify Finary credentials and retry",
  finary_uapi_command_failed: "check Finary credentials/MFA and retry authentication",
  browser_session_not_materialized:
    "complete browser login, then materialize finary session artifacts (or configure FINARY_BROWSER_SESSION_MATERIALIZER_CMD)",
  // Alfred API errors
  alfred_api_auth_required: "API authentication expired — restart Alfred to refresh the token",
  alfred_api_rate_limited: "API rate limit reached — wait a few minutes and retry",
  alfred_api_unavailable: "Alfred API server is unreachable — check VPS status or network connection",
  alfred_api_timeout: "API request timed out — the server may be overloaded, retry shortly",
  alfred_api_server_error: "Alfred API returned an internal error — check server logs",
  // Codex/LLM errors
  codex_session_expired: "OpenAI session expired — click 'Connect' to sign in again",
  codex_binary_missing: "Codex CLI not found — it will be installed automatically on next restart",
  codex_process_failed: "LLM process crashed — retry the analysis",
  // Analysis errors
  analysis_run_start_local_failed: "analysis could not start — check the error details above",
  finary_snapshot_empty: "Finary returned no positions — session may be stale, reconnect Finary",
  no_positions_for_account: "no positions found for the selected account — verify account name"
};

const CRITICAL_ERRORS = new Set([
  "alfred_api_auth_required",
  "codex_session_expired",
  "finary_snapshot_empty",
  "finary_credentials_missing"
]);

export function formatBridgeError(error) {
  if (error && typeof error === "object") {
    const code = error.code || "bridge_error";
    const message = error.message || code;
    const hint = ERROR_HINTS[code] ? ` (hint: ${ERROR_HINTS[code]})` : "";
    return `${code}: ${message}${hint}`;
  }
  return String(error);
}

export function extractErrorCode(error) {
  if (error && typeof error === "object") return error.code || "bridge_error";
  const s = String(error);
  const match = s.match(/^([a-z_]+):/);
  return match ? match[1] : "unknown_error";
}

export function isErrorCritical(error) {
  return CRITICAL_ERRORS.has(extractErrorCode(error));
}

export function createRunOperationsController({
  bridge,
  setStatus,
  setBusy,
  renderAnalysisResult,
  refreshAfterRun,
  onError = () => {},
  emitEvent = () => {},
  analysisPollIntervalMs = RESILIENCE_POLICY.analysisPollIntervalMs,
  analysisPollTimeoutMs = RESILIENCE_POLICY.analysisPollTimeoutMs,
  degradedPollingRefreshCooldownMs = RESILIENCE_POLICY.activeRunDegradedRefreshCooldownMs,
  progressRefreshThrottleMs = RESILIENCE_POLICY.activeRunProgressRefreshThrottleMs,
  analysisStatusInvokeTimeoutMs = RESILIENCE_POLICY.analysisStatusInvokeTimeoutMs,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  now = () => Date.now()
}) {
  if (!bridge) {
    throw new Error("bridge_required");
  }

  async function refreshAfterRunSafe(context = "refresh") {
    try {
      await refreshAfterRun();
    } catch (error) {
      emitEvent({
        ts: new Date().toISOString(),
        type: "run.refresh_warning",
        status: "degraded",
        message: `${context}: ${formatBridgeError(error)}`,
        error_code: error?.code || null
      });
    }
  }

  let abortRequested = false;
  let activeOperationId = null;

  return {
    requestAbort() {
      abortRequested = true;
      // Send stop command to Rust backend to kill workers immediately
      if (activeOperationId && bridge && typeof bridge.stopAnalysisRun === "function") {
        bridge.stopAnalysisRun(activeOperationId).catch(() => {});
      }
    },
    async runAnalysis(options = {}) {
      abortRequested = false;
      setBusy(true);
      setStatus("Running analysis...", "status-loading");
      emitEvent({
        ts: new Date().toISOString(),
        type: "run.started",
        status: "running",
        message: "Analysis started"
      });
      try {
        let payload = null;
        let lastProgressSignature = null;
        let pollingDegraded = false;
        let lastStatusSignature = null;
        let lastDegradedRefreshAt = Number.NaN;
        let lastProgressRefreshAt = Number.NaN;
        const supportsAsyncRun =
          bridge &&
          typeof bridge.startAnalysisRun === "function" &&
          typeof bridge.getAnalysisRunStatus === "function";

        function applyStatus(text, style) {
          const signature = `${String(style || "")}:${String(text || "")}`;
          if (signature === lastStatusSignature) {
            return;
          }
          lastStatusSignature = signature;
          setStatus(text, style);
        }

        if (!supportsAsyncRun) {
          payload = await bridge.runAnalysis(options);
        } else {
          const started = await bridge.startAnalysisRun(options);
          const operationId = String(started?.operation_id || "").trim();
          if (!operationId) {
            throw { code: "analysis_operation_id_missing", message: "analysis_operation_id_missing" };
          }
          activeOperationId = operationId;
          emitEvent({
            ts: new Date().toISOString(),
            type: "run.accepted",
            status: "running",
            operation_id: operationId,
            message: `Analysis accepted (${operationId})`
          });
          const startedAt = Date.now();
          const pollInterval = Math.max(250, normalizeResilienceDuration(analysisPollIntervalMs, 2000));
          const pollTimeout = Math.max(0, normalizeResilienceDuration(analysisPollTimeoutMs, 0));
          while (true) {
            if (abortRequested) {
              emitEvent({
                ts: new Date().toISOString(),
                type: "run.failed",
                status: "failed",
                message: "Analysis stopped by user"
              });
              setBusy(false);
              setStatus("Analysis stopped.", "status-idle");
              return;
            }
            if (pollTimeout > 0 && Date.now() - startedAt > pollTimeout) {
              throw {
                code: "analysis_run_poll_timeout",
                message: `analysis_run_poll_timeout:${operationId}`
              };
            }
            await sleep(pollInterval);
            let statusPayload;
            try {
              statusPayload = await bridge.getAnalysisRunStatus(operationId, {
                timeoutMs: analysisStatusInvokeTimeoutMs
              });
            } catch (error) {
              if (!isAnalysisStatusPollingTransientError(error)) {
                throw error;
              }
              applyStatus("Running analysis... (status polling degraded)", "status-loading");
              if (!pollingDegraded) {
                emitEvent({
                  ts: new Date().toISOString(),
                  type: "run.polling_degraded",
                  status: "degraded",
                  message: "Analysis status polling degraded; keeping last known run state visible.",
                  error_code: error?.code || null
                });
              }
              pollingDegraded = true;
              const nowMs = Number(now());
              if (
                !Number.isFinite(lastDegradedRefreshAt) ||
                nowMs - lastDegradedRefreshAt >= Math.max(0, Number(degradedPollingRefreshCooldownMs) || 0)
              ) {
                lastDegradedRefreshAt = nowMs;
                await refreshAfterRunSafe("status_timeout_refresh");
              }
              continue;
            }
            if (pollingDegraded) {
              pollingDegraded = false;
              emitEvent({
                ts: new Date().toISOString(),
                type: "run.polling_recovered",
                status: "running",
                message: "Analysis status polling recovered."
              });
            }
            const status = String(statusPayload?.status || "running");
            const stage = String(statusPayload?.stage || status || "running");
            const collectionProgress = statusPayload?.collection_progress || null;
            const lineProgress = statusPayload?.line_progress || null;
            const lineStatus = statusPayload?.line_status || null;
            const runId = String(statusPayload?.run_id || "").trim();
            const stageLabel = formatStageLabel(stage, { collectionProgress, lineProgress });
            applyStatus(`Running analysis... (${stageLabel})`, "status-loading");
            const progressSignature = JSON.stringify({
              status,
              stage,
              collectionProgress,
              lineProgress,
              lineStatus
            });
            if (progressSignature !== lastProgressSignature) {
              lastProgressSignature = progressSignature;
              emitEvent({
                ts: new Date().toISOString(),
                type: "run.progress",
                operation_id: operationId,
                run_id: runId || null,
                status,
                message: `Analysis ${status} (${stageLabel})`,
                stage,
                collection_progress: collectionProgress,
                line_progress: lineProgress,
                line_status: lineStatus
              });
              if (status === "running") {
                const nowMs = Number(now());
                if (
                  !Number.isFinite(lastProgressRefreshAt) ||
                  nowMs - lastProgressRefreshAt >= Math.max(0, Number(progressRefreshThrottleMs) || 0)
                ) {
                  lastProgressRefreshAt = nowMs;
                  await refreshAfterRunSafe("progress_refresh");
                }
              }
            }
            if (status === "completed") {
              payload = statusPayload?.result || {};
              break;
            }
            if (status === "failed") {
              const err = statusPayload?.error || {};
              throw {
                code: String(err?.code || "analysis_run_failed"),
                message: String(err?.message || err?.code || "analysis_run_failed")
              };
            }
          }
        }

        renderAnalysisResult(payload);
        const degradedFinarySnapshot =
          payload?.ingestion_status === "degraded" && payload?.source_mode === "finary";
        const degradedOrchestration =
          payload?.degraded === true || String(payload?.orchestration_status || "").trim() === "completed_degraded";
        const degradedReason = String(payload?.degradation_reason || "").trim();
        const degradedMessage = degradedFinarySnapshot
          ? "Analysis completed in degraded mode on latest Finary snapshot"
          : degradedOrchestration
            ? `Analysis completed in degraded mode${degradedReason ? ` (${degradedReason})` : ""}`
            : "Analysis completed";
        setStatus(
          degradedMessage,
          degradedFinarySnapshot || degradedOrchestration ? "status-idle" : "status-success"
        );
        emitEvent({
          ts: new Date().toISOString(),
          type: "run.completed",
          status: degradedFinarySnapshot || degradedOrchestration ? "degraded" : "success",
          message: degradedMessage
        });
        await refreshAfterRunSafe("completion_refresh");
      } catch (error) {
        onError(error);
        setStatus(formatBridgeError(error), "status-error");
        emitEvent({
          ts: new Date().toISOString(),
          type: "run.failed",
          status: "failed",
          message: formatBridgeError(error),
          error_code: error?.code || null
        });
        // Do NOT refresh after failure — keep failure state visible to the user.
      } finally {
        activeOperationId = null;
        setBusy(false);
      }
    }
  };
}
