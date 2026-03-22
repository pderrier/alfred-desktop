export async function resolveRunnableFinarySession({
  refreshStatus,
  rematerializeFromArtifacts = async () => null,
  autoFinalize = async () => null,
  isRunnable
} = {}) {
  let current = await refreshStatus();
  if (isRunnable(current)) {
    return current;
  }

  const rematerialized = await rematerializeFromArtifacts();
  if (isRunnable(rematerialized)) {
    return rematerialized;
  }
  if (rematerialized) {
    current = rematerialized;
  }

  const recovered = await autoFinalize();
  if (isRunnable(recovered)) {
    return recovered;
  }
  if (recovered) {
    current = recovered;
  }

  return current;
}
