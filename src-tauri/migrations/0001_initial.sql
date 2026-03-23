PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS app_users_local (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL,
  display_name TEXT,
  auth_provider TEXT NOT NULL CHECK (auth_provider IN ('google', 'email')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS devices (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  platform TEXT NOT NULL CHECK (platform IN ('windows', 'macos')),
  hostname TEXT,
  app_version TEXT,
  last_seen_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sources (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  type TEXT NOT NULL CHECK (type IN ('finary', 'csv', 'manual', 'other')),
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('connected', 'disconnected', 'error', 'connecting', 'needs_reauth')),
  connection_mode TEXT,
  last_sync_status TEXT,
  last_synced_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS source_sessions (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('valid', 'expired', 'needs_reauth', 'error')),
  last_validated_at TEXT,
  expires_at TEXT,
  session_version INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sync_jobs (
  id TEXT PRIMARY KEY,
  source_id TEXT NOT NULL,
  job_type TEXT NOT NULL CHECK (job_type IN ('full', 'incremental', 'validate_only')),
  status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'success', 'failed', 'cancelled')),
  started_at TEXT,
  finished_at TEXT,
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS instruments (
  id TEXT PRIMARY KEY,
  provider TEXT,
  provider_ref TEXT,
  symbol TEXT,
  isin TEXT,
  name TEXT,
  asset_type TEXT,
  currency TEXT,
  exchange TEXT,
  country TEXT,
  sector TEXT,
  metadata_json TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS snapshots (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  source_id TEXT,
  snapshot_type TEXT NOT NULL CHECK (snapshot_type IN ('source_raw', 'normalized', 'merged')),
  valuation_date TEXT NOT NULL,
  base_currency TEXT NOT NULL,
  total_value REAL,
  total_cost REAL,
  total_gain REAL,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE,
  FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  snapshot_id TEXT NOT NULL,
  source_account_ref TEXT,
  account_type TEXT,
  name TEXT,
  currency TEXT,
  balance REAL,
  metadata_json TEXT,
  FOREIGN KEY (snapshot_id) REFERENCES snapshots(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS positions (
  id TEXT PRIMARY KEY,
  snapshot_id TEXT NOT NULL,
  account_id TEXT,
  source_position_ref TEXT,
  asset_type TEXT,
  instrument_id TEXT,
  symbol TEXT,
  name TEXT,
  currency TEXT,
  quantity REAL,
  price REAL,
  market_value REAL,
  cost_basis REAL,
  unrealized_gain REAL,
  weight REAL,
  metadata_json TEXT,
  FOREIGN KEY (snapshot_id) REFERENCES snapshots(id) ON DELETE CASCADE,
  FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE SET NULL,
  FOREIGN KEY (instrument_id) REFERENCES instruments(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS transactions (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  source_id TEXT,
  account_id TEXT,
  instrument_id TEXT,
  txn_type TEXT NOT NULL,
  trade_date TEXT,
  settle_date TEXT,
  quantity REAL,
  unit_price REAL,
  gross_amount REAL,
  fees REAL,
  taxes REAL,
  currency TEXT,
  description TEXT,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE,
  FOREIGN KEY (source_id) REFERENCES sources(id) ON DELETE SET NULL,
  FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE SET NULL,
  FOREIGN KEY (instrument_id) REFERENCES instruments(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS price_cache (
  id TEXT PRIMARY KEY,
  instrument_id TEXT NOT NULL,
  price_date TEXT NOT NULL,
  price REAL NOT NULL,
  currency TEXT NOT NULL,
  source TEXT NOT NULL,
  metadata_json TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (instrument_id) REFERENCES instruments(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS news_cache (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL,
  title TEXT NOT NULL,
  url TEXT,
  published_at TEXT,
  summary TEXT,
  tickers_json TEXT,
  metadata_json TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS analyses (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  snapshot_id TEXT,
  run_id TEXT,
  analysis_type TEXT NOT NULL,
  title TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('draft', 'final', 'failed')),
  summary_md TEXT,
  body_md TEXT,
  created_by TEXT NOT NULL CHECK (created_by IN ('agent', 'user', 'system')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE,
  FOREIGN KEY (snapshot_id) REFERENCES snapshots(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS reports (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  analysis_id TEXT,
  report_type TEXT NOT NULL CHECK (report_type IN ('md', 'html', 'pdf')),
  title TEXT NOT NULL,
  file_path TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE,
  FOREIGN KEY (analysis_id) REFERENCES analyses(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS memory_entries (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  scope TEXT NOT NULL CHECK (scope IN ('preferences', 'user_profile', 'constraints', 'learned', 'recent_findings')),
  key TEXT NOT NULL,
  value_json TEXT NOT NULL,
  importance REAL NOT NULL DEFAULT 0.5,
  source TEXT,
  updated_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS agent_runs (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  run_type TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
  model TEXT,
  input_summary TEXT,
  output_summary TEXT,
  started_at TEXT,
  finished_at TEXT,
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY (user_id) REFERENCES app_users_local(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id);
CREATE INDEX IF NOT EXISTS idx_sources_user_id ON sources(user_id);
CREATE INDEX IF NOT EXISTS idx_source_sessions_source_id ON source_sessions(source_id);
CREATE INDEX IF NOT EXISTS idx_sync_jobs_source_id ON sync_jobs(source_id);
CREATE INDEX IF NOT EXISTS idx_sync_jobs_status ON sync_jobs(status);
CREATE INDEX IF NOT EXISTS idx_snapshots_user_id ON snapshots(user_id);
CREATE INDEX IF NOT EXISTS idx_snapshots_source_id ON snapshots(source_id);
CREATE INDEX IF NOT EXISTS idx_snapshots_valuation_date ON snapshots(valuation_date);
CREATE INDEX IF NOT EXISTS idx_accounts_snapshot_id ON accounts(snapshot_id);
CREATE INDEX IF NOT EXISTS idx_positions_snapshot_id ON positions(snapshot_id);
CREATE INDEX IF NOT EXISTS idx_positions_account_id ON positions(account_id);
CREATE INDEX IF NOT EXISTS idx_positions_instrument_id ON positions(instrument_id);
CREATE INDEX IF NOT EXISTS idx_transactions_user_id ON transactions(user_id);
CREATE INDEX IF NOT EXISTS idx_transactions_source_id ON transactions(source_id);
CREATE INDEX IF NOT EXISTS idx_transactions_trade_date ON transactions(trade_date);
CREATE INDEX IF NOT EXISTS idx_price_cache_instrument_date ON price_cache(instrument_id, price_date);
CREATE INDEX IF NOT EXISTS idx_news_cache_published_at ON news_cache(published_at);
CREATE INDEX IF NOT EXISTS idx_analyses_user_id ON analyses(user_id);
CREATE INDEX IF NOT EXISTS idx_analyses_snapshot_id ON analyses(snapshot_id);
CREATE INDEX IF NOT EXISTS idx_reports_user_id ON reports(user_id);
CREATE INDEX IF NOT EXISTS idx_memory_entries_user_id ON memory_entries(user_id);
CREATE INDEX IF NOT EXISTS idx_memory_entries_scope ON memory_entries(scope);
CREATE INDEX IF NOT EXISTS idx_agent_runs_user_id ON agent_runs(user_id);
