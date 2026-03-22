function emitStep(onStep, step, status) {
  if (typeof onStep !== "function") {
    return;
  }
  onStep({ step, status, ts: new Date().toISOString() });
}

function createTimeoutError(code, timeoutMs) {
  const error = new Error(`${code}:${timeoutMs}`);
  error.code = code;
  return error;
}

function sleep(ms) {
  const delay = Number(ms);
  if (!Number.isFinite(delay) || delay <= 0) {
    return Promise.resolve();
  }
  return new Promise((resolve) => setTimeout(resolve, delay));
}

async function runWithTimeout(task, timeoutMs, code) {
  const effectiveTimeoutMs =
    Number.isFinite(Number(timeoutMs)) && Number(timeoutMs) > 0 ? Number(timeoutMs) : 10_000;
  let timeoutHandle = null;
  try {
    return await Promise.race([
      Promise.resolve().then(() => task()),
      new Promise((_, reject) => {
        timeoutHandle = setTimeout(() => {
          reject(createTimeoutError(code, effectiveTimeoutMs));
        }, effectiveTimeoutMs);
      })
    ]);
  } finally {
    if (timeoutHandle) {
      clearTimeout(timeoutHandle);
    }
  }
}

export async function orchestrateStartupSession({
  refreshStatus,
  rematerializeFromArtifacts = async () => null,
  autoFinalize,
  isRunnable,
  onStep = null,
  statusTimeoutMs = 12_000,
  recoveryTimeoutMs = 30_000,
  statusRetries = 1,
  statusRetryDelayMs = 350
}) {
  let latestStatus = null;

  const runStep = async (step, timeoutCode, timeoutMs, task, { retries = 0, retryDelayMs = 0 } = {}) => {
    emitStep(onStep, step, "running");
    const safeRetries = Math.max(0, Number.parseInt(String(retries || 0), 10) || 0);
    const safeRetryDelayMs = Math.max(0, Number.parseInt(String(retryDelayMs || 0), 10) || 0);
    let attempt = 0;
    while (attempt <= safeRetries) {
      try {
        const result = await runWithTimeout(task, timeoutMs, timeoutCode);
        emitStep(onStep, step, "completed");
        return result;
      } catch (error) {
        if (attempt < safeRetries) {
          emitStep(onStep, step, "retrying");
          // eslint-disable-next-line no-await-in-loop
          await sleep(safeRetryDelayMs * (attempt + 1));
          attempt += 1;
          continue;
        }
        emitStep(onStep, step, "failed");
        throw error;
      }
    }
  };

  try {
    latestStatus = await runStep(
      "checking_session_status",
      "startup_status_timeout",
      statusTimeoutMs,
      () => refreshStatus(),
      { retries: statusRetries, retryDelayMs: statusRetryDelayMs }
    );

    if (isRunnable(latestStatus)) {
      return {
        ok: true,
        state: "ready",
        session: latestStatus,
        requiresReconnect: false,
        error: null
      };
    }

    latestStatus = await runStep(
      "attempting_artifact_rematerialization",
      "startup_artifact_rematerialization_timeout",
      recoveryTimeoutMs,
      () => rematerializeFromArtifacts()
    );

    if (isRunnable(latestStatus)) {
      return {
        ok: true,
        state: "ready",
        session: latestStatus,
        requiresReconnect: false,
        error: null
      };
    }

    latestStatus = await runStep(
      "attempting_auto_recovery",
      "startup_auto_recovery_timeout",
      recoveryTimeoutMs,
      () => autoFinalize()
    );

    if (isRunnable(latestStatus)) {
      return {
        ok: true,
        state: "ready",
        session: latestStatus,
        requiresReconnect: false,
        error: null
      };
    }

    latestStatus = await runStep(
      "validating_recovered_session",
      "startup_status_timeout",
      statusTimeoutMs,
      () => refreshStatus(),
      { retries: statusRetries, retryDelayMs: statusRetryDelayMs }
    );

    if (isRunnable(latestStatus)) {
      return {
        ok: true,
        state: "ready",
        session: latestStatus,
        requiresReconnect: false,
        error: null
      };
    }

    return {
      ok: true,
      state: "reconnect",
      session: latestStatus,
      requiresReconnect: true,
      error: null
    };
  } catch (error) {
    return {
      ok: false,
      state: "error",
      session: latestStatus,
      requiresReconnect: true,
      error
    };
  }
}
