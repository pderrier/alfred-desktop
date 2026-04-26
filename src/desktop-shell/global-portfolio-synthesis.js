function asNumber(value, fallback = 0) {
  const num = Number(value);
  return Number.isFinite(num) ? num : fallback;
}

function normalizeLabel(position) {
  return [
    position?.type,
    position?.asset_class,
    position?.category,
    position?.nom,
    position?.name,
    position?.ticker,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

function inferSupportBucket(position) {
  const label = normalizeLabel(position);
  if (!label) return "Other";
  if (/(cash|liquid|esp[eè]ce|money\s*market)/.test(label)) return "Cash";
  if (/(crypto|bitcoin|ethereum|btc|eth)/.test(label)) return "Crypto";
  if (/(bond|oblig|fixed\s*income)/.test(label)) return "Bonds";
  if (/(etf|tracker|index\s*fund|fund|ucits)/.test(label)) return "ETFs/Funds";
  if (/(stock|equity|action)/.test(label)) return "Stocks";
  return "Stocks";
}

export function buildGlobalPortfolioSynthesis(snapshot) {
  const finaryMeta = snapshot?.latest_finary_snapshot || {};
  const accountsMeta = Array.isArray(finaryMeta.accounts) ? finaryMeta.accounts : [];
  const latestRunPositions = Array.isArray(snapshot?.latest_run?.portfolio?.positions)
    ? snapshot.latest_run.portfolio.positions
    : [];

  const accountTotals = new Map();
  for (const account of accountsMeta) {
    const name = account?.name;
    if (!name) continue;
    accountTotals.set(name, {
      value: asNumber(account.total_value, 0),
      cash: asNumber(account.cash, 0),
      gain: asNumber(account.total_gain, 0),
    });
  }

  for (const pos of latestRunPositions) {
    const accountName = pos?.compte || pos?.account || "Unknown account";
    if (accountTotals.has(accountName)) continue;
    const row = accountTotals.get(accountName) || { value: 0, cash: 0, gain: 0 };
    row.value += asNumber(pos?.montant, 0);
    row.gain += asNumber(pos?.plus_moins_value, 0);
    accountTotals.set(accountName, row);
  }

  const supportBuckets = new Map();
  let investedValue = 0;
  for (const pos of latestRunPositions) {
    const value = asNumber(pos?.montant, 0);
    if (value <= 0) continue;
    investedValue += value;
    const bucket = inferSupportBucket(pos);
    supportBuckets.set(bucket, (supportBuckets.get(bucket) || 0) + value);
  }

  let totalValue = 0;
  let totalCash = 0;
  let totalGain = 0;
  for (const row of accountTotals.values()) {
    totalValue += asNumber(row.value, 0);
    totalCash += asNumber(row.cash, 0);
    totalGain += asNumber(row.gain, 0);
  }

  const accountBreakdown = Array.from(accountTotals.entries()).map(([name, values]) => ({
    name,
    value: asNumber(values.value, 0),
    cash: asNumber(values.cash, 0),
    gain: asNumber(values.gain, 0),
  }));
  accountBreakdown.sort((a, b) => b.value - a.value);

  const supportBreakdown = Array.from(supportBuckets.entries()).map(([name, value]) => ({
    name,
    value,
    weightPct: investedValue > 0 ? (value * 100) / investedValue : 0,
  }));
  supportBreakdown.sort((a, b) => b.value - a.value);

  const topAccount = accountBreakdown[0] || null;
  const topSupport = supportBreakdown[0] || null;
  const topAccountWeightPct = topAccount && totalValue > 0 ? (topAccount.value * 100) / totalValue : 0;
  const cashWeightPct = totalValue > 0 ? (totalCash * 100) / totalValue : 0;

  const suggestions = [];
  if (topAccountWeightPct >= 65 && topAccount) {
    suggestions.push(`Your largest account (${topAccount.name}) is ${topAccountWeightPct.toFixed(0)}% of total assets — consider balancing flows across accounts.`);
  }
  if (cashWeightPct >= 25) {
    suggestions.push(`Cash is ${cashWeightPct.toFixed(0)}% of your portfolio. If this is not intentional, consider redeploying progressively.`);
  } else if (cashWeightPct > 0 && cashWeightPct <= 3) {
    suggestions.push(`Cash buffer is only ${cashWeightPct.toFixed(1)}%. Keep enough liquidity for near-term needs and volatility.`);
  }
  if (topSupport && topSupport.weightPct >= 70) {
    suggestions.push(`${topSupport.name} represent ${topSupport.weightPct.toFixed(0)}% of invested assets. Consider diversifying support types.`);
  }

  let verdict = "Balanced allocation";
  if (topAccountWeightPct >= 65 || (topSupport && topSupport.weightPct >= 70)) {
    verdict = "High concentration risk";
  } else if (topAccountWeightPct >= 50 || (topSupport && topSupport.weightPct >= 55)) {
    verdict = "Moderate concentration";
  }

  return {
    generatedAt: new Date().toISOString(),
    verdict,
    totalValue,
    totalCash,
    totalGain,
    cashWeightPct,
    accountCount: accountBreakdown.length,
    accountBreakdown,
    supportBreakdown,
    suggestions,
  };
}
