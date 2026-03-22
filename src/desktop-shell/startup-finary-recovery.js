function safeStep(step, status, onStep) {
  if (typeof onStep !== "function") {
    return;
  }
  onStep({ step, status, ts: new Date().toISOString() });
}

async function withTimeout(promise, timeoutMs, code) {
  const ms = Number.isFinite(Number(timeoutMs)) && Number(timeoutMs) > 0 ? Number(timeoutMs) : 10_000;
  let timeoutId = null;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeoutId = setTimeout(() => {
          const error = new Error(`${code}:${ms}`);
          error.code = code;
          reject(error);
        }, ms);
      })
    ]);
  } finally {
    if (timeoutId) {
      clearTimeout(timeoutId);
    }
  }
}

export async function runStartupFinaryRecovery({
  refreshStatus,
  autoFinalize,
  isRunnable,
  onStep = null,
  statusTimeoutMs = 10_000,
  recoveryTimeoutMs = 30_000
}) {
  safeStep("checking_session_status", "running", onStep);
  let status = await withTimeout(
    Promise.resolve().then(() => refreshStatus()),
    statusTimeoutMs,
    "startup_status_timeout"
  );
  if (isRunnable(status)) {
    safeStep("checking_session_status", "completed", onStep);
    return { ok: true, session: status, requiresReconnect: false };
  }
  safeStep("checking_session_status", "completed", onStep);

  safeStep("attempting_auto_recovery", "running", onStep);
  try {
    const finalized = await withTimeout(
      Promise.resolve().then(() => autoFinalize()),
      recoveryTimeoutMs,
      "startup_auto_recovery_timeout"
    );
    if (finalized && isRunnable(finalized)) {
      safeStep("attempting_auto_recovery", "completed", onStep);
      return { ok: true, session: finalized, requiresReconnect: false };
    } else {
      safeStep("attempting_auto_recovery", "failed", onStep);
    }
  } catch (error) {
    safeStep("attempting_auto_recovery", "failed", onStep);
    return { ok: false, session: status, requiresReconnect: true, error };
  }

  return { ok: true, session: status, requiresReconnect: true };
}
