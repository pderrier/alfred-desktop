use crate::analysis_ops::{ops_store as analysis_ops_store, AnalysisOperationRecord};
use crate::cli::dispatch as run_command;
use crate::command_handlers::invoke_command as run_invoke_command;
use crate::command_handlers::run_analysis_status as run_local_analysis_status;
use crate::helpers::{now_epoch_ms, now_iso_string};
use crate::report::persist_retry_global_synthesis as persist_retry_global_synthesis_report;
use crate::run_state::initialize_with_control_plane_with as initialize_analysis_run_state_with_control_plane_with;
use crate::storage::read_json_file;
    
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::helpers::test_env_lock();
        // Clear the run_state_cache so stale entries from previous tests (pointing to
        // deleted temp dirs) don't bleed into the current test.
        crate::run_state_cache::reset_cache();
        // Always use mock_cache mode in tests — never call real LLM APIs.
        std::env::set_var("LITELLM_GENERATION_MODE", "mock_cache");
        // Disable the alfred-api HTTP client. Otherwise the ISIN resolver
        // (`enrichment::fetch_resolved_symbol`) hits the production
        // `/api/resolve` endpoint over real DNS/HTTPS, and intermittent
        // success/failure of that call changes downstream enrichment URLs
        // (`&canonical=` is appended or not), bypassing the in-test
        // `request_fn` mock and producing wall-clock-dependent flakes.
        // Every test using `env_lock()` is already hermetic w.r.t. LLM and
        // Codex; the API client must be hermetic too.
        std::env::set_var("ALFRED_API_ENABLED", "0");
        // Install Codex mock so MCP batch/synthesis turns don't hit real Codex.
        crate::codex::set_codex_mock(Some(codex_test_mock));
        guard
    }

    /// Mock for `run_codex_prompt_with_progress`. Simulates what Codex+MCP would do:
    /// - For batch prompts: reads run_state, writes mock recommendations via MCP tools
    /// - For synthesis prompts: calls persist_retry_global_synthesis with a mock draft
    fn codex_test_mock(prompt: &str) -> anyhow::Result<serde_json::Value> {
        // Extract run_id from prompt (appears as run_id="xxx" or run "{xxx}")
        let run_id = prompt
            .find("run_id=\"").or_else(|| prompt.find("run \""))
            .and_then(|start| {
                let rest = &prompt[start..];
                let quote_start = rest.find('"')? + 1;
                let rest2 = &rest[quote_start..];
                let quote_end = rest2.find('"')?;
                Some(rest2[..quote_end].to_string())
            })
            .unwrap_or_default();

        if run_id.is_empty() {
            return Ok(json!({"ok": true, "mock": true}));
        }

        let is_synthesis = prompt.contains("synthese globale") || prompt.contains("check_coverage");

        if is_synthesis {
            // Synthesis mock: call persist_retry_global_synthesis directly
            let run_state = crate::load_run_by_id_direct(&run_id)?;
            let reco_count = run_state.get("pending_recommandations")
                .and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
            if reco_count == 0 {
                return Ok(json!({"ok": true, "mock": true, "skipped": "no_recommendations"}));
            }
            let draft = json!({
                "synthese_marche": "Synthese mock: portefeuille equilibre avec des fondamentaux solides et une diversification adequate.",
                "actions_immediates": [],
                "llm_utilise": "codex-mock",
            });
            let _ = crate::report::persist_retry_global_synthesis(&run_id, &draft)?;
            Ok(json!({"ok": true, "mock": true, "orchestration_status": "completed"}))
        } else {
            // Batch analysis mock: read run_state, write mock recommendations
            let run_state = crate::load_run_by_id_direct(&run_id)?;
            let positions = run_state.get("portfolio")
                .and_then(|p| p.get("positions"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            for pos in &positions {
                let ticker = pos.get("ticker").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
                let nom = pos.get("nom").and_then(|v| v.as_str()).unwrap_or("");
                let line_type = pos.get("type").and_then(|v| v.as_str()).unwrap_or("position");
                if ticker.is_empty() { continue; }
                let line_id = format!("{line_type}:{ticker}");
                // Check if already has a recommendation (retry_failed mode)
                let already_has = run_state.get("pending_recommandations")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().any(|r| r.get("line_id").and_then(|v| v.as_str()) == Some(&line_id)))
                    .unwrap_or(false);
                if already_has { continue; }

                let rec = json!({
                    "line_id": line_id,
                    "ticker": ticker,
                    "type": line_type,
                    "nom": nom,
                    "signal": "CONSERVER",
                    "conviction": "moderee",
                    "synthese": format!("{ticker}: conserver la position avec discipline et suivi du risque structurel."),
                    "analyse_technique": "Tendance stable, pas de signal technique majeur.",
                    "analyse_fondamentale": "Fondamentaux robustes, croissance reguliere.",
                    "analyse_sentiment": "Sentiment neutre a legerement positif.",
                    "raisons_principales": ["Fondamentaux solides", "Position equilibree"],
                    "risques": ["Volatilite sectorielle"],
                    "catalyseurs": ["Resultats trimestriels"],
                    "badges_keywords": ["stable", "fondamentaux"],
                    "action_recommandee": "Conserver, pas d'action immediate",
                    "deep_news_summary": "Pas d'actualite majeure recente.",
                    "reanalyse_after": "2026-04-22",
                    "reanalyse_reason": "prochain trimestre"
                });
                // Write directly to run_state (same as MCP validate_recommendation tool)
                let _ = crate::patch_run_state_direct_with(&run_id, |rs| {
                    let obj = rs.as_object_mut().expect("run_state object");
                    let mut pending = obj.get("pending_recommandations")
                        .and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    pending.retain(|r| {
                        r.get("line_id").and_then(|v| v.as_str()) != Some(&line_id)
                    });
                    pending.push(rec.clone());
                    obj.insert("pending_recommandations".to_string(), json!(pending));
                });
            }
            Ok(json!({"ok": true, "mock": true}))
        }
    }

    /// RAII guard that removes env vars on drop (even on panic).
    struct EnvCleanup(&'static [&'static str]);
    impl Drop for EnvCleanup {
        fn drop(&mut self) {
            for key in self.0 {
                std::env::remove_var(key);
            }
        }
    }
    const TEST_ENV_KEYS: &[&str] = &[
        "ALFRED_STATE_DIR",
        "ALFRED_REPORTS_DIR",
        "ALFRED_RUNTIME_SETTINGS_PATH",
        "LITELLM_GENERATION_MODE",
        "ALFRED_LLM_TOKEN",
        "ALFRED_API_ENABLED",
    ];

    static TEST_ACTIVE_LINE_ANALYZE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_MAX_CONCURRENT_LINE_ANALYZE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_ACTIVE_COLLECTION_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_MAX_CONCURRENT_COLLECTION_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn reset_parallelism_counters() {
        TEST_ACTIVE_LINE_ANALYZE_CALLS.store(0, Ordering::SeqCst);
        TEST_MAX_CONCURRENT_LINE_ANALYZE_CALLS.store(0, Ordering::SeqCst);
        TEST_ACTIVE_COLLECTION_CALLS.store(0, Ordering::SeqCst);
        TEST_MAX_CONCURRENT_COLLECTION_CALLS.store(0, Ordering::SeqCst);
    }

    fn record_collection_parallelism() {
        // Deterministic concurrency rendezvous (opt-in via CollectionSyncPoint).
        // When enabled by a test that needs to prove the dispatcher fans out
        // N workers concurrently, every worker waits here until all parties
        // arrive — guaranteeing the max-concurrent counter reaches `parties`
        // regardless of OS scheduling. When disabled (default), this is a
        // cheap atomic read and other tests using the same mock harness
        // are unaffected. See `CollectionSyncPoint` below for the protocol.
        CollectionSyncPoint::arrive_if_enabled();
        let active = TEST_ACTIVE_COLLECTION_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        TEST_MAX_CONCURRENT_COLLECTION_CALLS.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(60));
        TEST_ACTIVE_COLLECTION_CALLS.fetch_sub(1, Ordering::SeqCst);
    }

    /// Deterministic concurrency rendezvous used by the collection-parallelism
    /// test. The semantic intent of that test is "the dispatcher actually runs
    /// N collection workers concurrently when configured for N-way parallelism".
    /// The previous design relied on OS scheduling to overlap two 60ms sleeps
    /// inside `record_collection_parallelism`; under CPU contention (WSL2,
    /// busy CI) workers serialised and the max-concurrent counter never
    /// reached 2 — a ~40% flake rate.
    ///
    /// Protocol:
    /// - Test calls `CollectionSyncPoint::enable(parties)` before launching the
    ///   workflow, and `disable()` (or drops the guard) after.
    /// - Each worker hitting `record_collection_parallelism()` calls
    ///   `arrive_if_enabled()`. The first `parties - 1` arrivers block on the
    ///   condvar; the `parties`-th arriver releases everyone. All workers
    ///   then proceed into the existing counter-increment block.
    /// - `arrive_if_enabled()` carries a 5s timeout. A serial-dispatch
    ///   regression manifests as: only one worker reaches the rendezvous,
    ///   times out after 5s, proceeds, counter stays at 1, the test assertion
    ///   `>= parties` fails fast and loudly. The barrier never deadlocks the
    ///   whole suite.
    /// - When disabled, `arrive_if_enabled()` is a single atomic load and
    ///   returns immediately — other tests sharing the mock harness are
    ///   unaffected.
    struct CollectionSyncPoint;

    static SYNC_PARTIES: AtomicUsize = AtomicUsize::new(0);
    static SYNC_STATE: Mutex<SyncState> = Mutex::new(SyncState {
        arrived: 0,
        released: false,
    });
    static SYNC_COND: Condvar = Condvar::new();

    struct SyncState {
        arrived: usize,
        released: bool,
    }

    /// RAII guard returned by `CollectionSyncPoint::enable`. Disables the
    /// rendezvous on drop so a panicking test still leaves the sync point
    /// clean for subsequent tests.
    struct SyncPointGuard;

    impl Drop for SyncPointGuard {
        fn drop(&mut self) {
            CollectionSyncPoint::disable();
        }
    }

    impl CollectionSyncPoint {
        const TIMEOUT: Duration = Duration::from_secs(5);

        fn enable(parties: usize) -> SyncPointGuard {
            assert!(parties >= 2, "sync point needs >= 2 parties to be meaningful");
            {
                let mut state = SYNC_STATE.lock().expect("sync state lock");
                state.arrived = 0;
                state.released = false;
            }
            SYNC_PARTIES.store(parties, Ordering::SeqCst);
            SyncPointGuard
        }

        fn disable() {
            SYNC_PARTIES.store(0, Ordering::SeqCst);
            let mut state = SYNC_STATE.lock().expect("sync state lock");
            state.arrived = 0;
            state.released = true;
            SYNC_COND.notify_all();
        }

        fn arrive_if_enabled() {
            let parties = SYNC_PARTIES.load(Ordering::SeqCst);
            if parties == 0 {
                return;
            }
            let mut state = SYNC_STATE.lock().expect("sync state lock");
            state.arrived += 1;
            if state.arrived >= parties {
                state.released = true;
                SYNC_COND.notify_all();
                return;
            }
            // Wait until released or timeout — timeout means the dispatcher
            // failed to fan out enough workers concurrently. We proceed
            // anyway so the test's assertion can fail with a meaningful
            // counter value instead of the whole suite deadlocking.
            let deadline = std::time::Instant::now() + Self::TIMEOUT;
            while !state.released {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next_state, result) = SYNC_COND
                    .wait_timeout(state, remaining)
                    .expect("sync condvar wait");
                state = next_state;
                if result.timed_out() {
                    break;
                }
            }
        }
    }

    fn native_parallelism_test_request(
        _method: &str,
        _host: &str,
        _port: u16,
        path: &str,
        body: Option<&str>,
        _timeout_ms: Option<u64>,
    ) -> anyhow::Result<serde_json::Value> {
        if path == "/market/spot?ticker=MC&name=LVMH&isin=FR0000121014" {
            record_collection_parallelism();
            return Ok(json!({
                "ok": true,
                "market": {
                    "price": 800.0,
                    "pe_ratio": 22.0,
                    "revenue_growth": 0.12,
                    "profit_margin": 0.18,
                    "debt_to_equity": 0.3,
                    "source": "alphavantage:spot"
                }
            }));
        }
        if path == "/market/spot?ticker=SU&name=Schneider%20Electric&isin=FR0000121972" {
            record_collection_parallelism();
            return Ok(json!({
                "ok": true,
                "market": {
                    "price": 210.0,
                    "pe_ratio": 20.0,
                    "revenue_growth": 0.10,
                    "profit_margin": 0.16,
                    "debt_to_equity": 0.2,
                    "source": "alphavantage:spot"
                }
            }));
        }
        if path == "/news?ticker=MC&name=LVMH&isin=FR0000121014" {
            record_collection_parallelism();
            return Ok(json!({
                "ok": true,
                "news": {
                    "items": [{
                        "title": "LVMH live update",
                        "source": "Reuters",
                        "url": "https://example.test/mc"
                    }]
                }
            }));
        }
        if path == "/news?ticker=SU&name=Schneider%20Electric&isin=FR0000121972" {
            record_collection_parallelism();
            return Ok(json!({
                "ok": true,
                "news": {
                    "items": [{
                        "title": "Schneider live update",
                        "source": "Reuters",
                        "url": "https://example.test/su"
                    }]
                }
            }));
        }
        if path == "/v1/line/analyze" {
            let parsed: serde_json::Value =
                serde_json::from_str(body.expect("line analyze body should exist"))
                    .expect("line analyze body should parse");
            let ticker = parsed["line_context"]["ticker"]
                .as_str()
                .expect("ticker should exist");
            // NOTE: increments the max-concurrent counter but no test currently
            // asserts on it — `native_analysis_workflow_runs_line_analysis_with_configured_parallelism`
            // checks reco_count / orchestration_status / report file, never the
            // line-analyze max-concurrent counter. So this site is NOT prone to
            // the same OS-scheduling flake as `record_collection_parallelism`.
            // If a future test does assert on it, route it through
            // `CollectionSyncPoint`-style rendezvous to stay deterministic.
            let active = TEST_ACTIVE_LINE_ANALYZE_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
            TEST_MAX_CONCURRENT_LINE_ANALYZE_CALLS.fetch_max(active, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(80));
            TEST_ACTIVE_LINE_ANALYZE_CALLS.fetch_sub(1, Ordering::SeqCst);
            return Ok(json!({
                "ok": true,
                "recommendation": {
                    "line_id": format!("position:{ticker}"),
                    "ticker": ticker,
                    "type": "position",
                    "nom": if ticker == "MC" { "LVMH" } else { "Schneider Electric" },
                    "signal": "CONSERVER",
                    "conviction": "moderee",
                    "synthese": format!("{ticker}: conserver la ligne avec une discipline de portefeuille explicite et un suivi du risque structurel."),
                    "analyse_technique": "Momentum stable.",
                    "analyse_fondamentale": "Qualite robuste.",
                    "analyse_sentiment": "Sentiment neutre-positif.",
                    "raisons_principales": ["Qualite", "Execution"],
                    "risques": ["Valorisation"],
                    "catalyseurs": ["Resultats"],
                    "badges_keywords": ["qualite"],
                    "action_recommandee": "Conserver",
                    "deep_news_summary": format!("{ticker}: live update"),
                    "deep_news_selected_url": format!("https://example.test/{}", ticker.to_lowercase()),
                    "deep_news_seen_urls": [format!("https://example.test/{}", ticker.to_lowercase())]
                },
                "model": "codex"
            }));
        }
        Err(anyhow::anyhow!("unexpected_test_path:{path}"))
    }

    #[test]
    fn health_command_returns_ok_payload() {
        let args = vec!["backend".to_string(), "health".to_string()];
        let payload = run_command(&args).expect("health command should succeed");
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["service"], "alfred-desktop-backend");
    }

    #[test]
    fn unknown_command_fails() {
        let args = vec!["backend".to_string(), "nope".to_string()];
        let err = run_command(&args).expect_err("unknown command should fail");
        assert!(err.to_string().contains("unknown_command"));
    }

    #[test]
    fn help_command_lists_native_analysis_async_commands() {
        let args = vec!["backend".to_string(), "help".to_string()];
        let payload = run_command(&args).expect("help command should succeed");
        let commands = payload["commands"]
            .as_array()
            .expect("commands should be array");
        assert!(commands
            .iter()
            .any(|value| value == "analysis:run-start-local"));
        assert!(commands
            .iter()
            .any(|value| value == "analysis:run-status-local <operation_id>"));
        assert!(commands
            .iter()
            .any(|value| value == "dashboard:snapshot-local"));
        assert!(commands
            .iter()
            .any(|value| value == "finary:session-status-local"));
        assert!(commands
            .iter()
            .any(|value| value == "finary:session-browser-start-local"));
    }

    #[test]
    fn analysis_run_start_and_status_commands_complete_lifecycle() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-native-analysis-lifecycle-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::create_dir_all(reports_dir.join("history")).expect("reports history should exist");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST",
                "agent_guidelines": "Focus on downside risk."
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_local_001" },
                        "device": { "id": "dev_local_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");
        let run_id = initialized["run_id"].as_str().expect("run id should exist").to_string();

        let payload = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "TEST",
                "uploaded_snapshot": {
                    "positions": [{
                        "ticker": "MC",
                        "nom": "LVMH",
                        "isin": "FR0000121014",
                        "quantite": 1,
                        "prix_actuel": 800,
                        "valeur_actuelle": 800,
                        "prix_revient": 700,
                            "compte": "TEST"
                    }],
                    "transactions": [],
                    "orders": [],
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35
                }
            })),
            None,
            |_method, _host, _port, path, body, _timeout_ms| {
                if path == "/market/spot?ticker=MC&name=LVMH&isin=FR0000121014" {
                    return Ok(json!({
                        "ok": true,
                        "market": {
                            "price": 800.0,
                            "pe_ratio": 22.0,
                            "revenue_growth": 0.12,
                            "profit_margin": 0.18,
                            "debt_to_equity": 0.3,
                            "source": "alphavantage:spot"
                        }
                    }));
                }
                if path == "/news?ticker=MC&name=LVMH&isin=FR0000121014" {
                    return Ok(json!({
                        "ok": true,
                        "news": {
                            "items": [{
                                "title": "LVMH live update",
                                "source": "Reuters",
                                "url": "https://example.test/mc"
                            }]
                        }
                    }));
                }
                if path == "/v1/line/analyze" {
                    let _parsed: serde_json::Value =
                        serde_json::from_str(body.expect("line analyze body should exist"))
                            .expect("line analyze body should parse");
                    return Ok(json!({
                        "ok": true,
                        "recommendation": {
                            "line_id": "position:MC",
                            "ticker": "MC",
                            "type": "position",
                            "nom": "LVMH",
                            "signal": "CONSERVER",
                            "conviction": "moderee",
                            "synthese": "Conserver la ligne avec discipline active et suivi du risque.",
                            "analyse_technique": "Momentum stable.",
                            "analyse_fondamentale": "Qualite robuste.",
                            "analyse_sentiment": "Sentiment neutre-positif.",
                            "raisons_principales": ["Qualite", "Execution"],
                            "risques": ["Valorisation"],
                            "catalyseurs": ["Resultats"],
                            "badges_keywords": ["qualite"],
                            "action_recommandee": "Conserver",
                            "deep_news_summary": "LVMH: live update",
                            "deep_news_selected_url": "https://example.test/mc",
                            "deep_news_seen_urls": ["https://example.test/mc"]
                        },
                        "model": "codex"
                    }));
                }
                if path == "/v1/report/generate" {
                    return Ok(json!({
                        "ok": true,
                        "draft": {
                            "synthese_marche": "Synthese globale finalisee nativement avec une lecture portefeuille complete et une priorisation exploitable.",
                            "actions_immediates": [{
                                "ticker": "MC",
                                "action": "CONSERVER",
                                "order_type": "MARKET",
                                "quantity": 1,
                                "estimated_amount_eur": 800,
                                "priority": 1,
                                "rationale": "Pas de desequilibre tactique immediat."
                            }],
                            "recommandations": [{ "ticker": "MC" }]
                        }
                    }));
                }
                Err(anyhow::anyhow!("unexpected_test_path:{path}"))
            },
        )
        .expect("native analysis workflow should succeed");

        // Simulate what the MCP finalize_report tool does: call persist_retry_global_synthesis
        // with a synthetic draft (same as what Codex produces via validate_synthesis)
        let draft = json!({
            "ok": true,
            "synthese_marche": "Synthese globale finalisee avec une lecture portefeuille complete et une priorisation exploitable.",
            "actions_immediates": [{
                "ticker": "MC",
                "action": "CONSERVER",
                "order_type": "MARKET",
                "quantity": 1,
                "estimated_amount_eur": 800,
                "priority": 1,
                "rationale": "Pas de desequilibre tactique immediat."
            }],
            "llm_utilise": "codex-mcp",
        });
        let finalized = persist_retry_global_synthesis_report(&run_id, &draft)
            .expect("report finalization should succeed");

        let persisted = read_json_file(&state_dir.join(format!("{run_id}.json")))
            .expect("persisted run should be readable");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["result"]["run_id"], run_id);
        assert_eq!(payload["result"]["collection"]["positions_count"], 1);
        assert_eq!(finalized["ok"], true);
        assert!(matches!(
            persisted["orchestration"]["status"].as_str(),
            Some("completed") | Some("completed_degraded")
        ));
        assert_eq!(persisted["pending_recommandations"].as_array().map(|rows| rows.len()), Some(1));
        assert_eq!(persisted["portfolio"]["positions"].as_array().map(|rows| rows.len()), Some(1));
    }

    #[test]
    fn native_analysis_workflow_runs_line_analysis_with_configured_parallelism() {
        let _guard = env_lock();
        reset_parallelism_counters();

        let base_dir = std::env::temp_dir().join(format!(
            "alfred-native-analysis-parallel-{}-{}",
            std::process::id(),
            now_epoch_ms()
        ));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        let runtime_settings_path = base_dir.join("runtime-settings.json");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::create_dir_all(reports_dir.join("history")).expect("reports history should exist");
        fs::write(
            &runtime_settings_path,
            serde_json::to_string_pretty(&json!({
                "line_analysis_concurrency": 2,
                "line_analysis_throttle_ms": 0
            }))
            .expect("runtime settings should serialize"),
        )
        .expect("runtime settings should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        std::env::set_var("ALFRED_RUNTIME_SETTINGS_PATH", runtime_settings_path.as_os_str());
        // RAII cleanup — env vars removed even on panic
        let _env_cleanup = EnvCleanup(TEST_ENV_KEYS);

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST",
                "agent_guidelines": "Keep concurrent throughput healthy."
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_local_parallel" },
                        "device": { "id": "dev_local_parallel", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-15T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");
        let run_id = initialized["run_id"].as_str().expect("run id should exist").to_string();

        let payload = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "TEST",
                "uploaded_snapshot": {
                    "positions": [
                        {
                            "ticker": "MC",
                            "nom": "LVMH",
                            "isin": "FR0000121014",
                            "quantite": 1,
                            "prix_actuel": 800,
                            "valeur_actuelle": 800,
                            "prix_revient": 700,
                            "compte": "TEST"
                        },
                        {
                            "ticker": "SU",
                            "nom": "Schneider Electric",
                            "isin": "FR0000121972",
                            "quantite": 1,
                            "prix_actuel": 210,
                            "valeur_actuelle": 210,
                            "prix_revient": 180,
                            "compte": "TEST"
                        }
                    ],
                    "transactions": [],
                    "orders": [],
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35
                }
            })),
            None,
            native_parallelism_test_request,
        )
        .expect("native analysis workflow should succeed");

        // Recommendations written by Codex mock (during workflow)
        let persisted_pre: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(state_dir.join(format!("{run_id}.json")))
                .expect("run state should be readable"),
        )
        .expect("run state should parse");
        let reco_count = persisted_pre["pending_recommandations"].as_array().map(|rows| rows.len()).unwrap_or(0);
        assert!(reco_count >= 2, "should have at least 2 recommendations (got {reco_count})");

        // Finalize report (simulates what analysis_ops worker does after workflow returns)
        let draft = json!({
            "synthese_marche": "Synthese mock: portefeuille equilibre, fondamentaux solides, diversification adequate.",
            "actions_immediates": [],
            "llm_utilise": "codex-mock",
        });
        let _ = persist_retry_global_synthesis_report(&run_id, &draft)
            .expect("report finalization should succeed");

        // Re-read final state
        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(state_dir.join(format!("{run_id}.json")))
                .expect("run state should be readable"),
        )
        .expect("run state should parse");

        // End-to-end checks
        assert_eq!(payload["result"]["collection"]["positions_count"], 2);

        // Orchestration completed
        let orch_status = persisted["orchestration"]["status"].as_str().unwrap_or("unknown");
        assert!(
            orch_status == "completed" || orch_status == "completed_degraded",
            "orchestration status should be completed (got {orch_status})"
        );

        // composed_payload written (UI needs this for rendering)
        assert!(persisted.get("composed_payload").is_some(), "composed_payload should exist");
        assert!(!persisted["composed_payload"]["synthese_marche"].as_str().unwrap_or("").is_empty(),
            "synthese_marche should be non-empty");

        // Report file written
        let latest_report = reports_dir.join("latest.json");
        assert!(latest_report.exists(), "reports/latest.json should exist");

        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn native_collection_runs_enrichment_with_configured_parallelism() {
        let _guard = env_lock();
        reset_parallelism_counters();
        // Force the two dispatcher workers to rendezvous inside the mock
        // before either proceeds, so the max-concurrent counter is
        // guaranteed to reach 2 regardless of OS scheduling. See
        // `CollectionSyncPoint` doc-comment for the rationale.
        let _sync_guard = CollectionSyncPoint::enable(2);

        let base_dir = std::env::temp_dir().join(format!(
            "alfred-native-collection-parallel-{}-{}",
            std::process::id(),
            now_epoch_ms()
        ));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        let runtime_settings_path = base_dir.join("runtime-settings.json");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::create_dir_all(reports_dir.join("history")).expect("reports history should exist");
        fs::write(
            &runtime_settings_path,
            serde_json::to_string_pretty(&json!({
                "collection_concurrency": 2,
                "collection_throttle_ms": 0,
                "line_analysis_concurrency": 1,
                "line_analysis_throttle_ms": 0
            }))
            .expect("runtime settings should serialize"),
        )
        .expect("runtime settings should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        std::env::set_var("ALFRED_RUNTIME_SETTINGS_PATH", runtime_settings_path.as_os_str());
        let _env_cleanup = EnvCleanup(TEST_ENV_KEYS);

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST",
                "agent_guidelines": "Keep collection throughput healthy."
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_collection_parallel" },
                        "device": { "id": "dev_collection_parallel", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-15T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");
        let run_id = initialized["run_id"].as_str().expect("run id should exist").to_string();

        let payload = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "TEST",
                "uploaded_snapshot": {
                    "positions": [
                        {
                            "ticker": "MC",
                            "nom": "LVMH",
                            "isin": "FR0000121014",
                            "quantite": 1,
                            "prix_actuel": 800,
                            "valeur_actuelle": 800,
                            "prix_revient": 700,
                            "compte": "TEST"
                        },
                        {
                            "ticker": "SU",
                            "nom": "Schneider Electric",
                            "isin": "FR0000121972",
                            "quantite": 1,
                            "prix_actuel": 210,
                            "valeur_actuelle": 210,
                            "prix_revient": 180,
                            "compte": "TEST"
                        }
                    ],
                    "transactions": [],
                    "orders": [],
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35
                }
            })),
            None,
            native_parallelism_test_request,
        )
        .expect("native analysis workflow should succeed");

        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["result"]["collection"]["positions_count"], 2);
        assert!(
            TEST_MAX_CONCURRENT_COLLECTION_CALLS.load(Ordering::SeqCst) >= 2,
            "collection should execute enrichment with bounded parallelism (got {})",
            TEST_MAX_CONCURRENT_COLLECTION_CALLS.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn native_collection_filters_positions_by_account_when_scoped() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-native-account-filter-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::create_dir_all(reports_dir.join("history")).expect("reports history should exist");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST",
                "account": "PEA Bourse"
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_acct_filter" },
                        "device": { "id": "dev_acct_filter", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-15T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");
        let run_id = initialized["run_id"].as_str().expect("run id").to_string();

        let payload = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "TEST",
                "uploaded_snapshot": {
                    "positions": [
                        {
                            "ticker": "MC", "nom": "LVMH", "isin": "FR0000121014",
                            "quantite": 1, "prix_actuel": 800, "valeur_actuelle": 800,
                            "prix_revient": 700, "compte": "PEA Bourse"
                        },
                        {
                            "ticker": "SU", "nom": "Schneider Electric", "isin": "FR0000121972",
                            "quantite": 1, "prix_actuel": 210, "valeur_actuelle": 210,
                            "prix_revient": 180, "compte": "CTO Degiro"
                        }
                    ],
                    "transactions": [],
                    "orders": [],
                    "valeur_totale": 1010,
                    "plus_value_totale": 130,
                    "liquidites": 500
                }
            })),
            None,
            native_parallelism_test_request,
        )
        .expect("account-scoped workflow should succeed");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(
            payload["result"]["collection"]["positions_count"], 1,
            "only PEA Bourse position (MC) should be collected"
        );
    }

    #[test]
    fn native_analysis_does_not_persist_llm_token() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-native-secret-guard-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        fs::create_dir_all(reports_dir.join("history")).expect("reports history should exist");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        std::env::set_var("ALFRED_LLM_TOKEN", "token-super-secret-123");

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST"
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_secret_guard" },
                        "device": { "id": "dev_secret_guard", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-15T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");
        let run_id = initialized["run_id"].as_str().expect("run id should exist").to_string();

        let _ = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "TEST",
                "uploaded_snapshot": {
                    "positions": [
                        {
                            "ticker": "MC",
                            "nom": "LVMH",
                            "isin": "FR0000121014",
                            "quantite": 1,
                            "prix_actuel": 800,
                            "valeur_actuelle": 800,
                            "prix_revient": 700,
                            "compte": "TEST"
                        }
                    ],
                    "transactions": [],
                    "orders": [],
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35
                }
            })),
            None,
            native_parallelism_test_request,
        )
        .expect("native analysis workflow should succeed");

        let raw_state =
            fs::read_to_string(state_dir.join(format!("{run_id}.json"))).expect("run state readable");
        assert!(
            !raw_state.contains("token-super-secret-123"),
            "raw llm token should not be persisted"
        );

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        std::env::remove_var("ALFRED_LLM_TOKEN");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn patch_run_state_serializes_parallel_updates() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-run-state-lock-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir should exist");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let run_id = "run_lock_test";
        let run_path = state_dir.join(format!("{run_id}.json"));
        crate::storage::write_json_file(
            &run_path,
            &json!({
                "run_id": run_id,
                "created_at": now_iso_string(),
                "updated_at": now_iso_string(),
                "pending_recommandations": []
            }),
        )
        .expect("seed run state should write");

        let run_id = run_id.to_string();
        let mut handles = Vec::new();
        for idx in 0..12 {
            let run_id = run_id.clone();
            handles.push(std::thread::spawn(move || {
                crate::run_state::patch_run_state_with(&run_id, |run_state| {
                    let list = run_state
                        .as_object_mut()
                        .and_then(|obj| obj.get_mut("pending_recommandations"))
                        .and_then(|value| value.as_array_mut())
                        .expect("pending_recommandations array");
                    list.push(json!({ "line_id": format!("line_{idx}") }));
                })
                .expect("patch run state should succeed");
            }));
        }
        for handle in handles {
            handle.join().expect("thread should finish");
        }

        // Flush cache to disk before reading via load_run_by_id (which reads from disk).
        crate::run_state_cache::flush_now(&run_id);
        let final_state =
            crate::run_state::load_run_by_id(&run_id).expect("final state should load");
        let count = final_state
            .get("pending_recommandations")
            .and_then(|v| v.as_array())
            .map(|rows| rows.len())
            .unwrap_or(0);
        assert_eq!(count, 12);

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn initialize_analysis_run_state_with_control_plane_persists_native_run_bootstrap() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-native-run-init-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let audit_path = base_dir.join("audit/events.jsonl");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_AUDIT_LOG_PATH", audit_path.as_os_str());

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "csv",
                "account": "TEST",
                "latest_export": "/tmp/p.csv",
                "agent_guidelines": "Focus on downside risk."
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_local_001" },
                        "device": { "id": "dev_local_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4401",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization should succeed");

        let run_id = initialized["run_id"].as_str().expect("run_id should be present");
        let persisted = read_json_file(&state_dir.join(format!("{run_id}.json")))
            .expect("persisted run should be readable");
        let audit_lines = fs::read_to_string(&audit_path).expect("audit log should be readable");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_AUDIT_LOG_PATH");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(persisted["portfolio_source"], "csv");
        assert_eq!(persisted["control_plane"]["user_id"], "usr_local_001");
        assert_eq!(persisted["runtime_llm"]["provider_base_url"], "http://127.0.0.1:4401");
        assert!(audit_lines.contains("\"action\":\"created\""));
    }

    #[test]
    fn initialize_run_state_with_account_persists_account_field() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-account-init-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let audit_path = base_dir.join("audit/events.jsonl");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_AUDIT_LOG_PATH", audit_path.as_os_str());

        let with_account = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({
                "portfolio_source": "finary",
                "account": "PEA Bourse"
            })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_001" },
                        "device": { "id": "dev_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4401",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization with account should succeed");

        let run_id = with_account["run_id"].as_str().expect("run_id");
        let persisted = read_json_file(&state_dir.join(format!("{run_id}.json")))
            .expect("persisted run should be readable");
        assert_eq!(persisted["account"], "PEA Bourse");

        let without_account = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({ "portfolio_source": "csv" })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_001" },
                        "device": { "id": "dev_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4401",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        )
        .expect("run initialization without account should succeed");

        let run_id2 = without_account["run_id"].as_str().expect("run_id");
        let persisted2 = read_json_file(&state_dir.join(format!("{run_id2}.json")))
            .expect("persisted run should be readable");
        assert!(persisted2["account"].is_null(), "account should be null when not provided");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_AUDIT_LOG_PATH");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn runtime_settings_direct_roundtrip_reads_updates_and_resets() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-runtime-settings-direct-{}", now_epoch_ms()));
        fs::create_dir_all(&base_dir).expect("temp dir should exist");
        let settings_path = base_dir.join("runtime-settings.json");
        std::env::set_var("ALFRED_RUNTIME_SETTINGS_PATH", settings_path.as_os_str());

        let initial = crate::runtime_settings::get_payload().expect("initial settings should load");
        assert_eq!(initial["values"]["default_run_mode"], "finary_resync");
        assert_eq!(initial["values"]["agent_guidelines"], "");

        let updated = crate::runtime_settings::patch(&json!({
            "default_run_mode": "csv",
            "agent_guidelines": "Protect downside first."
        }))
        .expect("settings update should succeed");
        assert_eq!(updated["values"]["default_run_mode"], "csv");
        assert_eq!(updated["values"]["agent_guidelines"], "Protect downside first.");

        let reset = crate::runtime_settings::reset().expect("settings reset should succeed");
        assert_eq!(reset["values"]["default_run_mode"], "finary_resync");
        assert_eq!(reset["values"]["agent_guidelines"], "");

        std::env::remove_var("ALFRED_RUNTIME_SETTINGS_PATH");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn runtime_settings_oauth_proposal_normalizes_and_persists() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir()
            .join(format!("alfred-runtime-settings-oauth-{}", now_epoch_ms()));
        fs::create_dir_all(&base_dir).expect("temp dir should exist");
        let settings_path = base_dir.join("runtime-settings.json");
        std::env::set_var("ALFRED_RUNTIME_SETTINGS_PATH", settings_path.as_os_str());

        // Defaults
        let initial =
            crate::runtime_settings::get_payload().expect("initial settings should load");
        assert_eq!(initial["values"]["oauth_proposal_dismissed_until_ms"], 0);
        assert_eq!(initial["values"]["oauth_proposal_permanently_dismissed"], 0);

        // Patch valid values
        let updated = crate::runtime_settings::patch(&json!({
            "oauth_proposal_dismissed_until_ms": 1_700_000_000_000_i64,
            "oauth_proposal_permanently_dismissed": 1,
        }))
        .expect("settings update should succeed");
        assert_eq!(
            updated["values"]["oauth_proposal_dismissed_until_ms"],
            1_700_000_000_000_i64
        );
        assert_eq!(updated["values"]["oauth_proposal_permanently_dismissed"], 1);

        // Range check on the dismissed-until ms (must reject negative)
        let err = crate::runtime_settings::patch(&json!({
            "oauth_proposal_dismissed_until_ms": -1
        }))
        .err()
        .expect("negative epoch-ms should be rejected");
        assert!(err.to_string().contains("oauth_proposal_dismissed_until_ms"));

        // Range check on the permanent-dismiss flag (must reject 2)
        let err = crate::runtime_settings::patch(&json!({
            "oauth_proposal_permanently_dismissed": 2
        }))
        .err()
        .expect("value out of 0..=1 range should be rejected");
        assert!(err.to_string().contains("oauth_proposal_permanently_dismissed"));

        std::env::remove_var("ALFRED_RUNTIME_SETTINGS_PATH");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn health_payload_ready_matches_service_health_contract() {
        assert_eq!(
            crate::health::health_payload_ready(&json!({ "ok": true }), false).expect("healthy payload"),
            true
        );
        assert_eq!(
            crate::health::health_payload_ready(
                &json!({ "ok": false, "live": true, "ready": false, "status": "warming_up" }),
                false
            )
            .expect_err("warming payload should not be ready")
            .to_string(),
            "service_unhealthy"
        );
        assert_eq!(
            crate::health::health_payload_ready(&json!({ "ok": false, "status": "degraded" }), true)
                .expect("accepted degraded payload"),
            true
        );
        assert_eq!(
            crate::health::health_payload_ready(&json!({ "ok": false }), false)
                .expect_err("invalid payload should fail")
                .to_string(),
            "service_health_payload_invalid"
        );
    }

    #[test]
    fn analysis_run_status_reads_real_stage_and_progress_from_run_state() {
        let _guard = env_lock();
        let mut state_dir = std::env::temp_dir();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        state_dir.push(format!("alfred-tauri-state-{nonce}"));
        fs::create_dir_all(&state_dir).expect("state dir should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let operation_id = "analysis_op_test_progress".to_string();
        let run_id = "run_progress_1".to_string();
        fs::write(
            state_dir.join(format!("{run_id}.json")),
            serde_json::to_string(&json!({
                "run_id": run_id,
                "orchestration": {
                    "status": "running",
                    "stage": "analyzing_lines",
                    "collection_progress": { "completed": 5, "total": 12 },
                    "line_progress": { "completed": 3, "total": 12 }
                }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");

        {
            let mut store = analysis_ops_store().lock().expect("state lock should succeed");
            store.insert(
                operation_id.clone(),
                AnalysisOperationRecord {
                    operation_id: operation_id.clone(),
                    status: "running".to_string(),
                    stage: "starting".to_string(),
                    run_id: Some(run_id.clone()),
                    started_at_ms: now_epoch_ms(),
                    finished_at_ms: None,
                    result: None,
                    error_code: None,
                    error_message: None,
                    collection_progress: None,
                    line_progress: None,
                    line_status: None,
                },
            );
        }

        let payload = run_local_analysis_status(operation_id.clone()).expect("status should load");
        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&state_dir);
        if let Ok(mut store) = analysis_ops_store().lock() {
            store.remove(&operation_id);
        }

        assert_eq!(payload["result"]["status"], "running");
        assert_eq!(payload["result"]["stage"], "analyzing_lines");
        assert_eq!(payload["result"]["collection_progress"]["completed"], 5);
        assert_eq!(payload["result"]["line_progress"]["completed"], 3);
    }

    #[test]
    fn run_by_id_local_reads_persisted_run_directly_without_node_helper() {
        let _guard = env_lock();
        let state_dir = std::env::temp_dir().join(format!("alfred-run-by-id-{}", now_epoch_ms()));
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        let run_path = state_dir.join("run_123.json");
        fs::write(
            &run_path,
            serde_json::to_string(&json!({
                "run_id": "run_123",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "portfolio": { "positions": [{ "ticker": "MC" }] },
                "pending_recommandations": [{ "line_id": "position:MC", "ticker": "MC", "type": "position", "signal": "CONSERVER" }],
                "composed_payload": { "synthese_marche": "Hydrated directly from rust." }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let payload =
            crate::command_handlers::run_by_id("run_123".to_string()).expect("run_by_id should succeed");

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&state_dir);

        assert_eq!(payload["action"], "run:by-id-local");
        assert_eq!(payload["result"]["run"]["run_id"], "run_123");
        assert_eq!(
            payload["result"]["run"]["composed_payload"]["synthese_marche"],
            "Hydrated directly from rust."
        );
    }

    #[test]
    fn dashboard_details_local_reads_persisted_state_directly_without_node_helper() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-dashboard-details-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        let history_dir = reports_dir.join("history");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::create_dir_all(&history_dir).expect("history dir should be created");
        fs::write(
            state_dir.join("run_123.json"),
            serde_json::to_string(&json!({
                "run_id": "run_123",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "portfolio_source": "finary",
                "portfolio": { "positions": [{ "ticker": "MC" }] },
                "pending_recommandations": [{ "line_id": "position:MC", "ticker": "MC", "type": "position", "signal": "CONSERVER" }],
                "composed_payload": {
                    "synthese_marche": "Direct rust dashboard details payload.",
                    "recommandations": [{ "line_id": "position:MC", "ticker": "MC", "type": "position", "signal": "CONSERVER" }]
                }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        fs::write(
            reports_dir.join("latest.json"),
            serde_json::to_string(&json!({
                "run_id": "run_123",
                "saved_at": "2026-03-14T14:40:55.000Z",
                "payload": {
                    "synthese_marche": "Latest report from rust.",
                    "recommandations": [{ "ticker": "MC" }]
                }
            }))
            .expect("json should serialize"),
        )
        .expect("latest report should be writable");
        fs::write(
            history_dir.join("20260314_144055_run_123.json"),
            serde_json::to_string(&json!({
                "run_id": "run_123",
                "saved_at": "2026-03-14T14:40:55.000Z",
                "payload": {
                    "synthese_marche": "History report from rust.",
                    "recommandations": [{ "ticker": "MC" }]
                }
            }))
            .expect("json should serialize"),
        )
        .expect("history report should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());

        let payload = crate::command_handlers::run_dashboard_details().expect("dashboard details should succeed");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["action"], "dashboard:details-local");
        assert_eq!(payload["result"]["snapshot"]["runs"][0]["run_id"], "run_123");
        assert_eq!(payload["result"]["snapshot"]["latest_run_summary"]["run_id"], "run_123");
        assert_eq!(payload["result"]["snapshot"]["latest_run"]["run_id"], "run_123");
        assert_eq!(
            payload["result"]["snapshot"]["latest_run"]["composed_payload"]["synthese_marche"],
            "Direct rust dashboard details payload."
        );
        assert_eq!(payload["result"]["snapshot"]["latest_report"]["run_id"], "run_123");
        assert_eq!(payload["result"]["snapshot"]["report_history"][0]["run_id"], "run_123");
    }

    #[test]
    fn dashboard_overview_local_reads_persisted_state_and_local_health_directly_without_node_helper() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-dashboard-overview-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        let source_sync_dir = base_dir.join("source-sync");
        let audit_dir = base_dir.join("audit");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::create_dir_all(reports_dir.join("history")).expect("history dir should be created");
        fs::create_dir_all(&source_sync_dir).expect("source sync dir should be created");
        fs::create_dir_all(&audit_dir).expect("audit dir should be created");
        fs::write(
            state_dir.join("run_ov_1.json"),
            serde_json::to_string(&json!({
                "run_id": "run_ov_1",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "portfolio_source": "finary",
                "portfolio": { "positions": [{ "ticker": "MC" }] },
                "pending_recommandations": [{ "line_id": "position:MC", "ticker": "MC", "type": "position", "signal": "CONSERVER" }]
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        fs::write(
            reports_dir.join("latest.json"),
            serde_json::to_string(&json!({
                "run_id": "run_ov_1",
                "saved_at": "2026-03-14T14:40:55.000Z",
                "payload": {
                    "valeur_portefeuille": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35,
                    "recommandations": [{ "ticker": "MC" }]
                }
            }))
            .expect("json should serialize"),
        )
        .expect("latest report should be writable");
        fs::write(
            source_sync_dir.join("source-snapshots.json"),
            serde_json::to_string(&json!({
                "latest_by_source": {
                    "finary_local_default": {
                        "saved_at": "2026-03-14T14:30:03.944Z",
                        "snapshot": { "positions": [{ "ticker": "MC" }] }
                    }
                }
            }))
            .expect("json should serialize"),
        )
        .expect("source snapshot store should be writable");
        fs::write(
            audit_dir.join("events.jsonl"),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&json!({
                    "ts": "2026-03-14T14:40:00.000Z",
                    "category": "run",
                    "action": "started",
                    "run_id": "run_ov_1",
                    "status": "running"
                }))
                .expect("audit json should serialize"),
                serde_json::to_string(&json!({
                    "ts": "2026-03-14T14:41:00.000Z",
                    "type": "run.completed",
                    "run_id": "run_ov_1",
                    "status": "completed"
                }))
                .expect("audit json should serialize")
            ),
        )
        .expect("audit log should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        std::env::set_var(
            "ALFRED_SOURCE_SNAPSHOTS_PATH",
            source_sync_dir.join("source-snapshots.json").as_os_str(),
        );
        std::env::set_var(
            "ALFRED_AUDIT_LOG_PATH",
            audit_dir.join("events.jsonl").as_os_str(),
        );
        std::env::set_var("ALFRED_STACK_HEALTH_TIMEOUT_MS", "500");

        let payload = crate::command_handlers::run_dashboard_overview().expect("dashboard overview should succeed");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        std::env::remove_var("ALFRED_SOURCE_SNAPSHOTS_PATH");
        std::env::remove_var("ALFRED_AUDIT_LOG_PATH");
        std::env::remove_var("ALFRED_STACK_HEALTH_TIMEOUT_MS");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["action"], "dashboard:overview-local");
        assert_eq!(payload["result"]["snapshot"]["runs"][0]["run_id"], "run_ov_1");
        assert_eq!(payload["result"]["snapshot"]["latest_run_summary"]["run_id"], "run_ov_1");
        assert_eq!(payload["result"]["snapshot"]["latest_report_summary"]["recommandations_count"], 1);
        assert_eq!(payload["result"]["snapshot"]["latest_finary_snapshot"]["available"], true);
        assert!(payload["result"]["snapshot"]["stack_health"]["services"].is_array());
        assert_eq!(payload["result"]["snapshot"]["audit_events"][0]["type"], "run.completed");
    }

    #[test]
    fn dashboard_snapshot_local_reads_direct_overview_and_details_without_node_helper() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-dashboard-snapshot-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::create_dir_all(reports_dir.join("history")).expect("history dir should be created");
        fs::write(
            state_dir.join("run_snap_1.json"),
            serde_json::to_string(&json!({
                "run_id": "run_snap_1",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "portfolio_source": "finary",
                "portfolio": { "positions": [{ "ticker": "MC" }] },
                "pending_recommandations": [{ "line_id": "position:MC", "ticker": "MC", "type": "position", "signal": "CONSERVER" }],
                "composed_payload": { "synthese_marche": "Synthese snapshot rust." }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        fs::write(
            reports_dir.join("latest.json"),
            serde_json::to_string(&json!({
                "run_id": "run_snap_1",
                "saved_at": "2026-03-14T14:40:55.000Z",
                "payload": { "synthese_marche": "Latest snapshot report.", "recommandations": [{ "ticker": "MC" }] }
            }))
            .expect("json should serialize"),
        )
        .expect("latest report should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        std::env::set_var("ALFRED_STACK_HEALTH_TIMEOUT_MS", "500");

        let payload = crate::command_handlers::run_dashboard_snapshot().expect("dashboard snapshot should succeed");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        std::env::remove_var("ALFRED_STACK_HEALTH_TIMEOUT_MS");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["action"], "dashboard:snapshot-local");
        assert_eq!(payload["result"]["snapshot"]["latest_run"]["run_id"], "run_snap_1");
        assert_eq!(payload["result"]["snapshot"]["latest_report"]["run_id"], "run_snap_1");
    }

    #[test]
    fn finary_native_session_status_returns_missing_when_no_session_file() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-finary-native-{}", now_epoch_ms()));
        let session_dir = base_dir.join("finary-session");
        std::env::set_var("FINARY_SESSION_DIR", session_dir.as_os_str());

        let payload = crate::finary::session_status().expect("status should succeed");

        std::env::remove_var("FINARY_SESSION_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(payload["session_state"], "missing");
        assert_eq!(payload["requires_reauth"], true);
    }

    #[test]
    fn finary_native_session_connect_requires_reauth_without_credentials() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-finary-connect-{}", now_epoch_ms()));
        let session_dir = base_dir.join("finary-session");
        std::env::set_var("FINARY_SESSION_DIR", session_dir.as_os_str());

        let error = crate::finary::session_connect(None)
            .expect_err("connect without credentials should fail");

        std::env::remove_var("FINARY_SESSION_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert!(error.to_string().contains("reauth_required"));
    }

    #[test]
    fn streamed_native_collection_packet_persists_partial_collection_state_directly() {
        let _guard = env_lock();
        let base_dir =
            std::env::temp_dir().join(format!("alfred-native-collection-packet-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::write(
            state_dir.join("run_native_collection_1.json"),
            serde_json::to_string(&json!({
                "run_id": "run_native_collection_1",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "pending_recommandations": [],
                "orchestration": {
                    "status": "running",
                    "stage": "collecting_data"
                }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        crate::native_line_analysis::persist_native_collection_state(
            "run_native_collection_1",
            &json!({
                "portfolio": {
                    "positions": [{ "ticker": "MC", "nom": "LVMH" }],
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35
                },
                "transactions": [],
                "orders": [],
                "market": {
                    "MC": { "prix_actuel": 800 }
                },
                "news": {
                    "MC": { "articles": [{ "title": "MC live news" }] }
                },
                "quality": {
                    "weak_tickers": []
                },
                "collection_issues": {
                    "count": 0,
                    "items": []
                },
                "enrichment": {
                    "status": "success",
                    "failures": []
                },
                "source_ingestion": {
                    "mode": "finary",
                    "status": "success",
                    "connector": "finary-connector",
                    "updated_at": "2026-03-14T14:40:56.140Z"
                },
                "normalization": null,
                "line_memory_hydration": null
            }),
        )
        .expect("native collection state should be persisted");

        // Flush cache to disk before reading the file directly.
        crate::run_state_cache::flush_now("run_native_collection_1");
        let persisted_run: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(state_dir.join("run_native_collection_1.json"))
                .expect("run state should remain readable"),
        )
        .expect("persisted run should parse");

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(persisted_run["portfolio"]["positions"].as_array().map(|rows| rows.len()), Some(1));
        assert_eq!(persisted_run["market"]["MC"]["prix_actuel"], 800);
        assert_eq!(persisted_run["news"]["MC"]["articles"][0]["title"], "MC live news");
        assert_eq!(persisted_run["source_ingestion"]["status"], "success");
    }

    #[test]
    fn persist_retry_global_synthesis_report_updates_run_state_and_report_artifacts_directly() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-retry-global-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir should be created");
        fs::create_dir_all(reports_dir.join("history")).expect("history dir should be created");
        fs::write(
            state_dir.join("run_retry_1.json"),
            serde_json::to_string(&json!({
                "run_id": "run_retry_1",
                "updated_at": "2026-03-14T14:40:56.140Z",
                "portfolio": {
                    "valeur_totale": 21024.87,
                    "plus_value_totale": -8181.63,
                    "liquidites": 990.35,
                    "positions": [{ "ticker": "MC" }]
                },
                "pending_recommandations": [{
                    "line_id": "position:MC",
                    "ticker": "MC",
                    "type": "position",
                    "signal": "CONSERVER",
                    "synthese": "Synthese ligne suffisamment detaillee pour satisfaire la validation locale avec un contexte exploitable et une action coherente."
                }],
                "orchestration": {
                    "status": "completed_degraded",
                    "stage": "completed_degraded",
                    "degraded": true,
                    "degradation_reason": "litellm_generation_timeout"
                }
            }))
            .expect("json should serialize"),
        )
        .expect("run state should be writable");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());

        let result = persist_retry_global_synthesis_report(
            "run_retry_1",
            &json!({
                "llm_utilise": "litellm",
                "synthese_marche": "Synthese globale retried avec une lecture portefeuille complete, des priorites nettes et un plan operatoire concret a court terme. Ecart a la strategie: la concentration sur quelques convictions reste elevee et impose une execution disciplinee plutot qu'un mouvement opportuniste. Les lignes deja solides doivent etre gerees avec selectivite, tandis que les ajustements immediats doivent rester coherents avec la liquidite disponible et l'historique d'execution recent.",
                "actions_immediates": [{
                    "ticker": "MC",
                    "action": "RENFORCER",
                    "order_type": "MARKET",
                    "quantity": 1,
                    "estimated_amount_eur": 500,
                    "priority": 1,
                    "rationale": "Execution faisable aujourd'hui."
                }]
            }),
        )
        .expect("retry persistence should succeed");

        let persisted_run: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(state_dir.join("run_retry_1.json")).expect("run state should remain readable"),
        )
        .expect("persisted run should parse");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(&base_dir);

        assert_eq!(result["ok"], true);
        assert_eq!(persisted_run["orchestration"]["status"], "completed");
        assert_eq!(persisted_run["composed_payload"]["synthese_marche"].is_string(), true);
        assert_eq!(persisted_run["report_artifacts"]["latest_path"].is_string(), true);
    }

    #[test]
    fn invoke_dispatch_supports_snake_case_aliases() {
        let dashboard_payload = run_invoke_command("dashboard_snapshot_local")
            .expect("dashboard invoke alias should succeed");
        assert_eq!(dashboard_payload["action"], "dashboard:snapshot-local");
    }

    #[test]
    fn invoke_dispatch_rejects_unknown_commands() {
        let err = run_invoke_command("unknown_cmd").expect_err("unknown invoke should fail");
        assert!(err.to_string().contains("unknown_invoke_command"));
    }

    #[test]
    fn run_mode_detection_uses_cli_when_command_argument_is_present() {
        let cli_args = vec!["backend".to_string(), "health".to_string()];
        assert!(crate::cli::should_run_cli(&cli_args));
    }

    #[test]
    fn run_mode_detection_uses_tauri_when_no_command_argument_is_present() {
        let tauri_args = vec!["backend".to_string()];
        assert!(!crate::cli::should_run_cli(&tauri_args));
    }

    #[test]
    fn validate_external_url_accepts_http_and_https_only() {
        assert!(crate::command_handlers::validate_external_url("https://app.finary.com/login").is_ok());
        assert!(crate::command_handlers::validate_external_url("http://localhost:4310").is_ok());
        assert!(crate::command_handlers::validate_external_url("javascript:alert(1)").is_err());
        assert!(crate::command_handlers::validate_external_url("file:///tmp/x").is_err());
    }

    #[test]
    fn decode_http_response_body_parses_chunked_json_payload() {
        let head = concat!(
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "Transfer-Encoding: chunked\r\n",
            "Connection: close"
        );
        let body = concat!("1E\r\n", "{\"ok\":true,\"status\":\"healthy\"}\r\n", "0\r\n", "\r\n");

        let decoded = crate::local_http::decode_http_response_body(head, body)
            .expect("chunked body should decode");
        let payload: serde_json::Value =
            serde_json::from_str(decoded.trim()).expect("decoded payload should parse");

        assert_eq!(payload["ok"], true);
        assert_eq!(payload["status"], "healthy");
    }

    #[test]
    fn parse_http_json_body_returns_coded_error_on_empty_json_body() {
        let error = crate::local_http::parse_http_json_body("")
            .expect_err("empty JSON body should fail with coded error");
        assert_eq!(error.to_string(), "http_invalid_response:empty_body");
    }

    #[test]
    fn resolve_socket_addr_accepts_localhost_hostnames() {
        let address = crate::local_http::resolve_socket_addr("localhost", 4401)
            .expect("localhost should resolve");
        assert_eq!(address.port(), 4401);
    }

    #[test]
    fn normalize_finary_snapshot_preserves_accounts_and_all_positions() {
        let snapshot = json!({
            "total_value": 2850,
            "total_gain": 150,
            "cash": 1800,
            "positions": [
                {
                    "ticker": "MC", "nom": "LVMH", "isin": "FR0000121014",
                    "quantite": 2, "prix_actuel": 800, "valeur_actuelle": 1600,
                    "prix_revient": 700, "plus_moins_value": 200, "plus_moins_value_pct": 14.3,
                    "compte": "PEA Bourse"
                },
                {
                    "ticker": "AI", "nom": "Air Liquide", "isin": "FR0000120073",
                    "quantite": 5, "prix_actuel": 150, "valeur_actuelle": 750,
                    "prix_revient": 140, "plus_moins_value": 50, "plus_moins_value_pct": 7.1,
                    "compte": "CTO Bourso"
                }
            ],
            "accounts": [
                { "name": "PEA Bourse", "cash": 1500, "total_value": 1600, "total_gain": 200 },
                { "name": "CTO Bourso", "cash": 300, "total_value": 750, "total_gain": 50 }
            ],
            "transactions": [],
            "orders": []
        });

        let normalized = crate::native_collection_helpers::normalize_finary_snapshot(&snapshot);

        let positions = normalized["positions"].as_array().expect("positions should be array");
        assert_eq!(positions.len(), 2, "all positions should be preserved");
        assert_eq!(positions[0]["ticker"], "MC");
        assert_eq!(positions[0]["compte"], "PEA Bourse");
        assert_eq!(positions[1]["ticker"], "AI");
        assert_eq!(positions[1]["compte"], "CTO Bourso");

        let accounts = normalized["accounts"].as_array().expect("accounts should be array");
        assert_eq!(accounts.len(), 2, "accounts array should be preserved");
        assert_eq!(accounts[0]["name"], "PEA Bourse");
        assert_eq!(accounts[1]["name"], "CTO Bourso");

        assert_eq!(normalized["valeur_totale"], 2850.0);
        assert_eq!(normalized["liquidites"], 1800.0);
    }

    // ── Real data integration tests ──────────────────────────────────

    #[test]
    fn load_run_by_id_reads_real_fixture_with_account_and_positions() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-real-fixture-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        // Copy real fixture into state dir
        let fixture = include_str!("../test-fixtures/pea-run.json");
        let parsed: serde_json::Value = serde_json::from_str(fixture).expect("fixture should parse");
        let run_id = parsed["run_id"].as_str().expect("run_id");
        fs::write(state_dir.join(format!("{run_id}.json")), fixture).expect("write fixture");

        // Test load_run_by_id
        let loaded = crate::run_state::load_run_by_id(run_id).expect("should load run");
        assert_eq!(loaded["run_id"], run_id);
        assert_eq!(loaded["account"], "Plan Epargne en Action");
        assert_eq!(loaded["orchestration"]["status"], "completed");
        let positions = loaded["portfolio"]["positions"].as_array().expect("positions");
        assert!(positions.len() > 0, "should have positions");
        assert_eq!(positions[0]["compte"], "Plan Epargne en Action");

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn load_run_history_returns_account_field_from_real_fixture() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-history-fixture-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let fixture = include_str!("../test-fixtures/pea-run.json");
        let parsed: serde_json::Value = serde_json::from_str(fixture).expect("fixture should parse");
        let run_id = parsed["run_id"].as_str().expect("run_id");
        fs::write(state_dir.join(format!("{run_id}.json")), fixture).expect("write fixture");

        let history = crate::run_state::load_run_history(10).expect("should load history");
        assert!(history.len() > 0, "should have at least one run");
        let run = &history[0];
        assert_eq!(run["run_id"], run_id);
        assert_eq!(run["account"], "Plan Epargne en Action", "account should be in history summary");
        assert_eq!(run["status"], "completed");

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn run_by_id_command_returns_run_inside_result() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-cmd-fixture-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        let fixture = include_str!("../test-fixtures/pea-run.json");
        let parsed: serde_json::Value = serde_json::from_str(fixture).expect("fixture should parse");
        let run_id = parsed["run_id"].as_str().expect("run_id").to_string();
        fs::write(state_dir.join(format!("{run_id}.json")), fixture).expect("write fixture");

        // Test the command handler (same path as Tauri invoke)
        let response = crate::command_handlers::run_by_id(run_id.clone())
            .expect("run_by_id command should succeed");

        // Verify the response structure matches what the bridge expects
        assert_eq!(response["ok"], true);
        assert!(response["result"]["ok"] == true, "result.ok should be true");
        let run = &response["result"]["run"];
        assert_eq!(run["run_id"], run_id, "result.run.run_id should match");
        assert_eq!(run["account"], "Plan Epargne en Action", "result.run.account should be set");
        assert!(run["portfolio"]["positions"].as_array().expect("positions").len() > 0);

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn account_positions_command_returns_positions_from_snapshot_store() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-acct-pos-{}", now_epoch_ms()));
        let sync_dir = base_dir.join("source-sync");
        fs::create_dir_all(&sync_dir).expect("sync dir");
        let snapshots_path = sync_dir.join("source-snapshots.json");
        std::env::set_var("ALFRED_SOURCE_SNAPSHOTS_PATH", snapshots_path.as_os_str());

        // Create a minimal snapshot store
        let store = json!({
            "latest_by_source": {
                "finary_local_default": {
                    "saved_at": "2026-03-15T10:00:00Z",
                    "snapshot": {
                        "positions": [
                            { "ticker": "MC", "nom": "LVMH", "compte": "PEA Bourse", "quantite": 2, "prix_actuel": 800, "valeur_actuelle": 1600, "prix_revient": 700, "plus_moins_value": 200, "plus_moins_value_pct": 14.3 },
                            { "ticker": "AI", "nom": "Air Liquide", "compte": "CTO", "quantite": 5, "prix_actuel": 150, "valeur_actuelle": 750, "prix_revient": 140, "plus_moins_value": 50, "plus_moins_value_pct": 7.1 },
                            { "ticker": "BN", "nom": "Danone", "compte": "PEA Bourse", "quantite": 10, "prix_actuel": 55, "valeur_actuelle": 550, "prix_revient": 50, "plus_moins_value": 50, "plus_moins_value_pct": 10.0 }
                        ]
                    }
                }
            }
        });
        crate::storage::write_json_file(&snapshots_path, &store).expect("write store");

        let result = crate::command_handlers::run_account_positions("PEA Bourse".to_string())
            .expect("should return positions");
        let positions = result["positions"].as_array().expect("positions array");
        assert_eq!(positions.len(), 2, "should return only PEA Bourse positions");
        assert_eq!(positions[0]["ticker"], "MC");
        assert_eq!(positions[1]["ticker"], "BN");
        assert_eq!(result["source"], "finary_snapshot");

        // Test non-existent account
        let empty = crate::command_handlers::run_account_positions("NonExistent".to_string())
            .expect("should return empty");
        assert_eq!(empty["positions"].as_array().expect("array").len(), 0);

        std::env::remove_var("ALFRED_SOURCE_SNAPSHOTS_PATH");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn cleanup_orphaned_runs_marks_running_as_aborted() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-orphan-cleanup-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        fs::create_dir_all(&state_dir).expect("state dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());

        // Create a "stuck" running run
        let run = json!({
            "run_id": "orphan_test_1",
            "account": "TEST",
            "orchestration": { "status": "running", "stage": "analyzing_lines" },
            "line_status": { "MC": "analyzing", "BN": "waiting" }
        });
        crate::storage::write_json_file(
            &state_dir.join("orphan_test_1.json"),
            &run,
        ).expect("write run");

        // Also create a completed run (should not be touched)
        let completed = json!({
            "run_id": "completed_test_1",
            "account": "TEST",
            "orchestration": { "status": "completed", "stage": "completed" }
        });
        crate::storage::write_json_file(
            &state_dir.join("completed_test_1.json"),
            &completed,
        ).expect("write completed");

        // Populate run index so cleanup_orphaned_runs can find the orphan
        crate::run_index::upsert("orphan_test_1", &crate::run_index::summary_from_run_state(&run));
        crate::run_index::upsert("completed_test_1", &crate::run_index::summary_from_run_state(&completed));

        // Run cleanup
        crate::run_state::cleanup_orphaned_runs();

        // Verify orphan was marked aborted
        let patched = read_json_file(&state_dir.join("orphan_test_1.json")).expect("read patched");
        assert_eq!(patched["orchestration"]["status"], "aborted");
        assert_eq!(patched["orchestration"]["error_code"], "run_aborted");
        assert_eq!(patched["line_status"]["MC"], "aborted");
        assert_eq!(patched["line_status"]["BN"], "aborted");

        // Verify completed run was not touched
        let still_completed = read_json_file(&state_dir.join("completed_test_1.json")).expect("read completed");
        assert_eq!(still_completed["orchestration"]["status"], "completed");

        std::env::remove_var("ALFRED_STATE_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    #[test]
    fn account_required_rejects_null_account() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-acct-required-{}", now_epoch_ms()));
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::create_dir_all(reports_dir.join("history")).expect("reports dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());

        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({ "portfolio_source": "csv" })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_001" },
                        "device": { "id": "dev_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4401",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        ).expect("init should succeed");
        let run_id = initialized["run_id"].as_str().expect("run_id").to_string();

        let result = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "uploaded_snapshot": {
                    "positions": [{ "ticker": "MC", "nom": "LVMH", "quantite": 1, "prix_actuel": 800, "valeur_actuelle": 800, "prix_revient": 700 }],
                    "transactions": [], "orders": [], "valeur_totale": 800, "plus_value_totale": 100, "liquidites": 0
                }
            })),
            None,
            native_parallelism_test_request,
        );

        assert!(result.is_err(), "should reject run with no account");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("account_required"), "error should mention account_required, got: {err}");

        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(&base_dir);
    }

    fn init_csv_run_with_account(base_dir: &std::path::Path, account: &str) -> String {
        let state_dir = base_dir.join("runtime-state");
        let reports_dir = base_dir.join("reports");
        fs::create_dir_all(&state_dir).expect("state dir");
        fs::create_dir_all(reports_dir.join("history")).expect("reports dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        std::env::set_var("ALFRED_REPORTS_DIR", reports_dir.as_os_str());
        let initialized = initialize_analysis_run_state_with_control_plane_with(
            Some(&json!({ "portfolio_source": "csv", "account": account })),
            |method, _host, _port, path, _body, _timeout_ms| {
                if method == "GET" && path == "/bootstrap" {
                    return Ok(json!({
                        "user": { "id": "usr_local_001" },
                        "device": { "id": "dev_local_001", "platform": "linux" },
                        "entitlements": { "plan": "dev" }
                    }));
                }
                Ok(json!({
                    "provider_base_url": "http://127.0.0.1:4504",
                    "allowed_models": ["gpt-5-mini"],
                    "expires_at": "2026-03-14T12:00:00.000Z"
                }))
            },
        ).expect("run init");
        initialized["run_id"].as_str().expect("run_id").to_string()
    }

    fn cleanup_csv_run(base_dir: &std::path::Path) {
        std::env::remove_var("ALFRED_STATE_DIR");
        std::env::remove_var("ALFRED_REPORTS_DIR");
        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn csv_upload_text_parses_positions_from_file_picker_format() {
        let csv_text = "--- FILE: POSITIONS_20260308.csv ---\nMeta;ignored\nValo total = 12 500,50 \u{20ac} ; +/- value latente = 500,40 \u{20ac} ; Solde esp\u{00e8}ces = 1 200,10 \u{20ac}\nNom;Code Isin;Quantit\u{00e9};Cours actuel;Valorisation;Poids%;PRU;Perf. jour; +/- value latente;Perf. latente;Perf. latente %\nAIRBUS;NL0000235190;10;170,50;1705,00;13,64;160,20;0,00;103,00;0,00;6,43%\nTOTALENERGIES;FR0000120271;20;62,10;1242,00;9,94;58,00;0,00;82,00;0,00;7,07%";

        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-csv-upload-{}", now_epoch_ms()));
        let run_id = init_csv_run_with_account(&base_dir, "EXPORT_CSV");

        let result = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "EXPORT_CSV",
                "csv_upload": { "csv_text": csv_text }
            })),
            None,
            |_url, _method, _timeout, _body, _auth, _retries| Ok(json!({"error": "mock"})),
        );

        assert!(result.is_ok(), "csv_upload should succeed, got: {:?}", result.err());
        let payload = result.unwrap();
        assert_eq!(payload["result"]["collection"]["positions_count"], 2,
            "should have 2 positions (AIRBUS + TOTALENERGIES)");

        cleanup_csv_run(&base_dir);
    }

    #[test]
    fn csv_upload_text_parses_single_file_without_separator() {
        let csv_text = "Meta;ignored\nValo total = 5 000,00 \u{20ac} ; +/- value latente = 100,00 \u{20ac} ; Solde esp\u{00e8}ces = 500,00 \u{20ac}\nNom;Code Isin;Quantit\u{00e9};Cours actuel;Valorisation;Poids%;PRU;Perf. jour; +/- value latente;Perf. latente;Perf. latente %\nLVMH;FR0000121014;5;800,00;4000,00;80,00;750,00;0,00;250,00;0,00;6,67%";

        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-csv-single-{}", now_epoch_ms()));
        let run_id = init_csv_run_with_account(&base_dir, "EXPORT_CSV");

        let result = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "EXPORT_CSV",
                "csv_upload": { "csv_text": csv_text }
            })),
            None,
            |_url, _method, _timeout, _body, _auth, _retries| Ok(json!({"error": "mock"})),
        );

        assert!(result.is_ok(), "single csv_upload should succeed, got: {:?}", result.err());
        let payload = result.unwrap();
        assert_eq!(payload["result"]["collection"]["positions_count"], 1,
            "should have 1 position (LVMH)");

        cleanup_csv_run(&base_dir);
    }

    #[test]
    fn csv_upload_with_cached_spec_parses_english_headers() {
        // English CSV with comma delimiter — pre-cache a spec so the LLM is not needed
        let csv_text = "Symbol,Company,Qty,Last Price,Market Value,PnL\nAAPL,Apple Inc,50,185.50,9275.00,1250.00\nMSFT,Microsoft,30,380.20,11406.00,2100.00";

        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-csv-heuristic-{}", now_epoch_ms()));
        let run_id = init_csv_run_with_account(&base_dir, "CSV_heuristic");

        // Pre-cache a CsvParsingSpec for these headers so the 2-tier flow hits cache
        let headers = vec!["Symbol", "Company", "Qty", "Last Price", "Market Value", "PnL"]
            .into_iter().map(String::from).collect::<Vec<_>>();
        let fingerprint = crate::native_collection::compute_header_fingerprint_for_test(&headers);
        let spec = CsvParsingSpec {
            format_type: "position_snapshot".to_string(),
            delimiter: ",".to_string(),
            header_row_index: 0,
            skip_rows_before_header: 0,
            number_format: "english".to_string(),
            columns: {
                let mut m = std::collections::HashMap::new();
                m.insert("ticker".to_string(), col(0));
                m.insert("name".to_string(), col(1));
                m.insert("quantity".to_string(), col(2));
                m.insert("current_price".to_string(), col(3));
                m.insert("market_value".to_string(), col(4));
                m.insert("pnl".to_string(), col(5));
                m
            },
            action_map: None,
            infer_action_from_quantity_sign: false,
            confidence: "high".to_string(),
        };
        // Write the spec to user-preferences via the same path used by cache_spec
        let mut prefs = crate::runtime_settings::get_user_preferences();
        if let Some(obj) = prefs.as_object_mut() {
            let specs = obj.entry("csv_parsing_specs").or_insert_with(|| json!({}));
            if let Some(specs_obj) = specs.as_object_mut() {
                specs_obj.insert(fingerprint.clone(), json!({
                    "spec": serde_json::to_value(&spec).unwrap(),
                    "cached_at": "2026-04-17",
                    "hit_count": 0,
                    "original_headers": headers,
                }));
            }
        }
        let _ = crate::runtime_settings::save_user_preferences(&prefs);

        let result = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "CSV_heuristic",
                "csv_upload": { "csv_text": csv_text }
            })),
            None,
            |_url, _method, _timeout, _body, _auth, _retries| Ok(json!({"error": "mock"})),
        );

        assert!(result.is_ok(), "cached spec csv should succeed, got: {:?}", result.err());
        let payload = result.unwrap();
        assert_eq!(payload["result"]["collection"]["positions_count"], 2,
            "should have 2 positions (AAPL + MSFT)");

        cleanup_csv_run(&base_dir);
    }

    #[test]
    fn csv_upload_empty_text_returns_csv_input_missing() {
        let _guard = env_lock();
        let base_dir = std::env::temp_dir().join(format!("alfred-csv-empty-{}", now_epoch_ms()));
        let run_id = init_csv_run_with_account(&base_dir, "EXPORT_CSV");

        let result = crate::native_collection::execute_native_local_analysis_workflow_with(
            Some(json!({
                "run_id": run_id,
                "portfolio_source": "csv",
                "account": "EXPORT_CSV",
                "csv_upload": { "csv_text": "" }
            })),
            None,
            |_url, _method, _timeout, _body, _auth, _retries| Ok(json!({"error": "mock"})),
        );

        assert!(result.is_err(), "empty csv_upload should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("csv_input_missing"), "error should be csv_input_missing, got: {err}");

        cleanup_csv_run(&base_dir);
    }

    // ── Universal CSV parser tests ─────────────────────────────────

    use crate::native_collection::{CsvParsingSpec, ColumnSpec};

    fn make_spec(format_type: &str, number_format: &str, columns: Vec<(&str, Option<ColumnSpec>)>) -> CsvParsingSpec {
        let mut col_map = std::collections::HashMap::new();
        for (k, v) in columns {
            col_map.insert(k.to_string(), v);
        }
        CsvParsingSpec {
            format_type: format_type.to_string(),
            delimiter: ",".to_string(),
            header_row_index: 0,
            skip_rows_before_header: 0,
            number_format: number_format.to_string(),
            columns: col_map,
            action_map: None,
            infer_action_from_quantity_sign: false,
            confidence: "high".to_string(),
        }
    }

    fn col(index: i32) -> Option<ColumnSpec> {
        Some(ColumnSpec { index, parse_pattern: None })
    }

    fn col_with_pattern(index: i32, pattern: &str) -> Option<ColumnSpec> {
        Some(ColumnSpec { index, parse_pattern: Some(pattern.to_string()) })
    }

    #[test]
    fn test_header_fingerprint_stability() {
        let _guard = env_lock();
        let h1 = vec!["Date".to_string(), "Ticker".to_string(), "Type".to_string()];
        let h2 = vec!["Type".to_string(), "Date".to_string(), "Ticker".to_string()];
        let fp1 = crate::native_collection::compute_header_fingerprint_for_test(&h1);
        let fp2 = crate::native_collection::compute_header_fingerprint_for_test(&h2);
        assert_eq!(fp1, fp2, "same headers in different order should produce same fingerprint");
        assert_eq!(fp1.len(), 16, "fingerprint should be 16 hex chars");

        let h3 = vec!["Date".to_string(), "Symbol".to_string(), "Type".to_string()];
        let fp3 = crate::native_collection::compute_header_fingerprint_for_test(&h3);
        assert_ne!(fp1, fp3, "different headers should produce different fingerprint");
    }

    #[test]
    fn test_number_format_english_vs_french() {
        let _guard = env_lock();
        use crate::native_collection_helpers::parse_number_with_format;

        assert!((parse_number_with_format("1,234.56", "english") - 1234.56).abs() < 0.01);
        assert!((parse_number_with_format("1 234,56", "french") - 1234.56).abs() < 0.01);
        assert!((parse_number_with_format("235.56", "english") - 235.56).abs() < 0.01);
        assert!((parse_number_with_format("235,56", "french") - 235.56).abs() < 0.01);
    }

    #[test]
    fn test_parse_pattern_currency_prefix() {
        let _guard = env_lock();
        use crate::native_collection_helpers::{extract_with_pattern, parse_number_with_format};
        use regex::Regex;

        let re = Regex::new(r"^[A-Z]{3}\s*([\d.,]+)$").unwrap();
        let extracted = extract_with_pattern("USD 235.56", Some(&re));
        assert_eq!(extracted, "235.56");
        assert!((parse_number_with_format(&extracted, "english") - 235.56).abs() < 0.01);
    }

    #[test]
    fn test_parse_pattern_euro_suffix() {
        let _guard = env_lock();
        use crate::native_collection_helpers::{extract_with_pattern, parse_number_with_format};
        use regex::Regex;

        let re = Regex::new(r"([\d\s.,]+)").unwrap();
        let extracted = extract_with_pattern("1 234,56 €", Some(&re));
        assert!((parse_number_with_format(&extracted, "french") - 1234.56).abs() < 0.01);
    }

    #[test]
    fn test_parse_pattern_dollar_prefix() {
        let _guard = env_lock();
        use crate::native_collection_helpers::{extract_with_pattern, parse_number_with_format};
        use regex::Regex;

        let re = Regex::new(r"[\$€£]?\s*([\d.,]+)").unwrap();
        let extracted = extract_with_pattern("$1,234.56", Some(&re));
        assert!((parse_number_with_format(&extracted, "english") - 1234.56).abs() < 0.01);
    }

    #[test]
    fn test_parse_pattern_fallback_on_invalid_regex() {
        let _guard = env_lock();
        use crate::native_collection_helpers::extract_with_pattern;

        // Invalid regex should not compile — function should gracefully return raw
        // (The compile happens in compile_spec_patterns, which skips invalid regexes,
        //  so extract_with_pattern gets None for those fields)
        let result = extract_with_pattern("235.56", None);
        assert_eq!(result, "235.56");
    }

    #[test]
    fn test_execute_spec_revolut_transactions() {
        let _guard = env_lock();
        let mut spec = make_spec("transaction_history", "english", vec![
            ("date", col(0)),
            ("ticker", col(1)),
            ("action", col(2)),
            ("quantity", col(3)),
            ("price", col_with_pattern(4, r"^[A-Z]{3}\s*([\d.,]+)$")),
            ("amount", col_with_pattern(5, r"^[A-Z]{3}\s*([\d.,]+)$")),
            ("currency", col(6)),
            ("fx_rate", col(7)),
        ]);
        let mut action_map = std::collections::HashMap::new();
        action_map.insert("BUY".to_string(), vec!["BUY".to_string(), "Market buy".to_string(), "Limit buy".to_string()]);
        action_map.insert("SELL".to_string(), vec!["SELL".to_string(), "Market sell".to_string(), "Limit sell".to_string()]);
        spec.action_map = Some(action_map);

        let headers = vec!["Date", "Ticker", "Type", "Quantity", "Price per share", "Total Amount", "Currency", "FX Rate"]
            .into_iter().map(String::from).collect::<Vec<_>>();
        let rows = vec![
            vec!["2024-01-15", "AAPL", "BUY", "10", "USD 185.50", "USD 1855.00", "USD", "0.92"],
            vec!["2024-02-20", "AAPL", "BUY", "5", "USD 190.00", "USD 950.00", "USD", "0.93"],
            vec!["2024-03-10", "MSFT", "Market buy", "3", "USD 415.00", "USD 1245.00", "USD", "0.91"],
        ].into_iter().map(|r| r.into_iter().map(String::from).collect()).collect::<Vec<Vec<String>>>();

        let result = crate::native_collection::execute_spec_for_test(&spec, &rows, &headers, "Revolut");
        assert!(result.is_ok(), "execute_spec should succeed: {:?}", result.err());
        let snapshot = result.unwrap();
        let positions = snapshot.get("positions").unwrap().as_array().unwrap();
        assert_eq!(positions.len(), 2, "should have AAPL and MSFT");

        // Find AAPL position
        let aapl = positions.iter().find(|p| p.get("ticker").unwrap().as_str() == Some("AAPL")).unwrap();
        assert!((aapl.get("quantite").unwrap().as_f64().unwrap() - 15.0).abs() < 0.01);
        // prix_revient = weighted avg cost: (1855/0.92 + 950/0.93) / 15
        let pr = aapl.get("prix_revient").unwrap().as_f64().unwrap();
        assert!(pr > 195.0 && pr < 210.0, "AAPL prix_revient should be around 202, got {pr}");
    }

    #[test]
    fn test_execute_spec_degiro_infer_action_from_sign() {
        let _guard = env_lock();
        let spec = CsvParsingSpec {
            format_type: "transaction_history".to_string(),
            delimiter: ",".to_string(),
            header_row_index: 0,
            skip_rows_before_header: 0,
            number_format: "english".to_string(),
            columns: {
                let mut m = std::collections::HashMap::new();
                m.insert("date".to_string(), col(0));
                m.insert("name".to_string(), col(1));
                m.insert("isin".to_string(), col(2));
                m.insert("quantity".to_string(), col(3));
                m.insert("price".to_string(), col(4));
                m.insert("amount".to_string(), col(5));
                m.insert("fees".to_string(), col(6));
                m
            },
            action_map: None,
            infer_action_from_quantity_sign: true,
            confidence: "high".to_string(),
        };

        let headers = vec!["Date", "Product", "ISIN", "Quantity", "Price", "Total", "Fees"]
            .into_iter().map(String::from).collect::<Vec<_>>();
        let rows = vec![
            vec!["2024-01-15", "Apple Inc", "US0378331005", "10", "185.50", "1855.00", "2.50"],
            vec!["2024-02-20", "Apple Inc", "US0378331005", "-3", "190.00", "570.00", "2.50"],
            vec!["2024-03-10", "Microsoft Corp", "US5949181045", "5", "415.00", "2075.00", "3.00"],
        ].into_iter().map(|r| r.into_iter().map(String::from).collect()).collect::<Vec<Vec<String>>>();

        let result = crate::native_collection::execute_spec_for_test(&spec, &rows, &headers, "DEGIRO");
        assert!(result.is_ok(), "execute_spec should succeed: {:?}", result.err());
        let snapshot = result.unwrap();
        let positions = snapshot.get("positions").unwrap().as_array().unwrap();
        assert_eq!(positions.len(), 2, "should have APPLE and MSFT");

        let apple = positions.iter().find(|p| {
            let t = p.get("ticker").unwrap().as_str().unwrap_or("");
            t == "US0378331005" || t.contains("APPLE")
        }).unwrap();
        assert!((apple.get("quantite").unwrap().as_f64().unwrap() - 7.0).abs() < 0.01,
            "APPLE should have 10-3=7 shares");
    }

    #[test]
    fn test_execute_spec_position_snapshot() {
        let _guard = env_lock();
        let spec = make_spec("position_snapshot", "french", vec![
            ("ticker", col(1)),
            ("name", col(0)),
            ("quantity", col(2)),
            ("current_price", col(3)),
            ("market_value", col(4)),
            ("cost_basis", col(5)),
            ("pnl", col(6)),
        ]);

        let headers = vec!["Nom", "Ticker", "Quantite", "Cours", "Valo", "PRU", "+/-MV"]
            .into_iter().map(String::from).collect::<Vec<_>>();
        let rows = vec![
            vec!["Total Energies", "TTE", "50", "58,30", "2 915,00", "55,20", "155,00"],
            vec!["Air Liquide", "AI", "10", "175,40", "1 754,00", "160,00", "154,00"],
        ].into_iter().map(|r| r.into_iter().map(String::from).collect()).collect::<Vec<Vec<String>>>();

        let result = crate::native_collection::execute_spec_for_test(&spec, &rows, &headers, "Test");
        assert!(result.is_ok(), "execute_spec should succeed: {:?}", result.err());
        let snapshot = result.unwrap();
        let positions = snapshot.get("positions").unwrap().as_array().unwrap();
        assert_eq!(positions.len(), 2);

        let tte = positions.iter().find(|p| p.get("ticker").unwrap().as_str() == Some("TTE")).unwrap();
        assert!((tte.get("quantite").unwrap().as_f64().unwrap() - 50.0).abs() < 0.01);
        assert!((tte.get("prix_actuel").unwrap().as_f64().unwrap() - 58.30).abs() < 0.1);
        assert!((tte.get("valeur_actuelle").unwrap().as_f64().unwrap() - 2915.0).abs() < 1.0);
    }

    #[test]
    fn test_csv_parsing_spec_serde_roundtrip() {
        let _guard = env_lock();
        let spec = make_spec("transaction_history", "english", vec![
            ("ticker", col(1)),
            ("quantity", col_with_pattern(3, r"^[\d.,]+$")),
            ("isin", None),
        ]);
        let json_val = serde_json::to_value(&spec).unwrap();
        let roundtrip: CsvParsingSpec = serde_json::from_value(json_val).unwrap();
        assert_eq!(roundtrip.format_type, "transaction_history");
        assert_eq!(roundtrip.number_format, "english");
        assert!(roundtrip.columns.get("isin").unwrap().is_none());
        assert!(roundtrip.columns.get("ticker").unwrap().is_some());
    }

    // ── build_memory_section tests ──────────────────────────────────

    #[test]
    fn test_build_memory_section_v2_full() {
        let memory = json!({
            "schema_version": 2,
            "signal_history": [
                { "date": "2026-04-01", "signal": "ACHAT", "conviction": "forte", "price_at_signal": 142.5 },
                { "date": "2026-03-15", "signal": "CONSERVER", "conviction": "moderee", "price_at_signal": 138.0 }
            ],
            "memory_narrative": "Strong growth thesis based on margin expansion.",
            "price_tracking": {
                "last_signal": "ACHAT",
                "last_signal_date": "2026-04-01",
                "price_at_signal": 142.5,
                "current_price": 151.2,
                "return_since_signal_pct": 6.1,
                "signal_accuracy": "correct"
            },
            "news_themes": ["tariffs", "margin_expansion"],
            "trend": "upgrading",
            "user_action": {
                "followed": true,
                "date": "2026-04-02",
                "note": "Bought 10 shares"
            }
        });
        let result = crate::llm_prompts::build_memory_section(Some(&memory));
        assert!(result.contains("MEMOIRE LIGNE (historique persistant):"));
        assert!(result.contains("Signal: ACHAT"));
        assert!(result.contains("rendement: +6.1%"));
        assert!(result.contains("correct"));
        assert!(result.contains("Tendance 3 analyses: upgrading"));
        assert!(result.contains("Themes: tariffs, margin_expansion"));
        assert!(result.contains("These: Strong growth thesis"));
        assert!(result.contains("Action utilisateur: suivi"));
        assert!(result.contains("Bought 10 shares"));
    }

    #[test]
    fn test_build_memory_section_empty_returns_first_analysis() {
        let result = crate::llm_prompts::build_memory_section(None);
        assert!(result.contains("premiere analyse"));

        let empty_obj = json!({});
        let result2 = crate::llm_prompts::build_memory_section(Some(&empty_obj));
        assert!(result2.contains("premiere analyse"));
    }

    #[test]
    fn test_build_memory_section_v1_data_treated_as_first_analysis() {
        // V1 data (no schema_version, no signal_history) should be treated as first analysis
        let v1 = json!({
            "llm_memory_summary": "Old V1 summary",
            "llm_strong_signals": ["signal1"]
        });
        let result = crate::llm_prompts::build_memory_section(Some(&v1));
        assert!(result.contains("premiere analyse"));
    }

    #[test]
    fn test_build_memory_section_partial_v2() {
        // V2 with only signal_history (no price_tracking, no trend)
        let partial = json!({
            "signal_history": [
                { "date": "2026-04-01", "signal": "ACHAT", "conviction": "forte", "price_at_signal": 100.0 }
            ],
            "memory_narrative": "Short thesis."
        });
        let result = crate::llm_prompts::build_memory_section(Some(&partial));
        assert!(result.contains("MEMOIRE LIGNE (historique persistant):"));
        assert!(result.contains("These: Short thesis."));
        // No trend section since it's missing
        assert!(!result.contains("Tendance"));
    }

    #[test]
    fn test_synthetic_portfolio_key_filtered_from_by_ticker_iterators() {
        // _PORTFOLIO is a synthetic key for portfolio-level insights.
        // It must be excluded from any by_ticker iteration that treats keys as real tickers.
        let store = json!({
            "by_ticker": {
                "AAPL": { "news_themes": ["tech"], "reanalyse_after": "2020-01-01" },
                "_PORTFOLIO": { "news_themes": ["macro"], "reanalyse_after": "2020-01-01" },
                "_CUSTOM": { "news_themes": ["test"] }
            }
        });
        let by_ticker = store.get("by_ticker").unwrap().as_object().unwrap();

        // Simulate the filter pattern used in get_stale_positions, run_get_run_diff, compute_theme_concentration
        let real_tickers: Vec<&String> = by_ticker.keys()
            .filter(|t| !t.starts_with('_'))
            .collect();

        assert_eq!(real_tickers.len(), 1);
        assert_eq!(real_tickers[0], "AAPL");
    }

    // ── Phase 1: cross-account context — holdings_accounts enrichment ──
    //
    // The Finary API returns ~29 holdings_accounts for a typical user
    // (PEA, CTO, livrets, comptes courants, real estate, loans, crypto…).
    // Until Phase 1 the snapshot only kept the ~7 investment-only accounts.
    // These tests pin the new `holdings_accounts` / `portfolio_summary`
    // shape and confirm `snapshot.accounts` (UI contract) is unchanged.

    /// Builds a Finary holdings_account JSON the way the API returns it.
    /// Real-world reference fields observed in
    /// `~/AppData/Roaming/alfred-desktop/debug.log` (`finary_cash_raw:` lines).
    fn fixture_holdings_account(
        name: &str,
        slug: &str,
        institution: &str,
        total_value: f64,
        securities: &[serde_json::Value],
        fiats: &[(&str, f64)],
        provider_categories: &[&str],
    ) -> serde_json::Value {
        let secs: Vec<serde_json::Value> = securities.to_vec();
        let fiats_json: Vec<serde_json::Value> = fiats
            .iter()
            .map(|(code, amount)| json!({
                "current_value": amount,
                "currency": { "code": code }
            }))
            .collect();
        let account_types: Vec<serde_json::Value> = provider_categories
            .iter()
            .map(|c| json!({ "name": c }))
            .collect();
        json!({
            "name": name,
            "slug": slug,
            "institution": { "name": institution },
            "total_value": total_value,
            "total_gain": 0.0,
            "securities": secs,
            "fiats": fiats_json,
            "institution_connection": {
                "institution_provider": {
                    "account_types": account_types
                }
            }
        })
    }

    #[test]
    fn test_kind_classification_structural() {
        use crate::native_collection_helpers::classify_holding_kind;
        // Table-driven — kind is structural, independent of names/institutions.
        // (securities_count, fiats_sum_eur, total_value) → expected kind
        let cases = [
            (5_usize, 0.0_f64, 12000.0_f64, "investment"),  // PEA with securities
            (0, 5000.0, 5000.0, "cash_only"),                // Livret A
            (0, 0.0, -50000.0, "liability"),                  // Loan
            (3, 1500.0, -200.0, "liability"),                 // Margin underwater
            (0, 0.0, 250000.0, "other"),                      // Real estate manual
            (0, 0.0, 0.0, "other"),                           // Empty / dormant
            (10, 500.0, 25000.0, "investment"),               // CTO with cash
            // Non-EUR-only cash: fiats_sum_eur is 0 so classifies as `other`
            // (multi-currency cash IS preserved in cash_by_currency though).
            (0, 0.0, 1200.0, "other"),
        ];
        for (sec, fiat, total, expected) in cases {
            let actual = classify_holding_kind(sec, fiat, total);
            assert_eq!(
                actual, expected,
                "classify_holding_kind({sec}, {fiat}, {total}) = {actual}, want {expected}"
            );
        }
    }

    #[test]
    fn test_institution_provider_categories_extraction() {
        use crate::native_collection_helpers::build_holdings_metadata;
        let holdings = vec![fixture_holdings_account(
            "Compte Cheque", "cc-1", "Banque X",
            850.0, &[], &[("EUR", 850.0)],
            &["checkings", "savings"],
        )];
        let meta = build_holdings_metadata(&holdings);
        assert_eq!(meta.len(), 1);
        let cats = meta[0]["institution_provider_categories"].as_array().unwrap();
        let cat_strs: Vec<&str> = cats.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(cat_strs, vec!["checkings", "savings"],
            "raw provider categories must be propagated in order");
    }

    #[test]
    fn test_cash_by_currency_multi() {
        use crate::native_collection_helpers::{aggregate_cash_by_currency, build_holdings_metadata};
        let holdings = vec![
            fixture_holdings_account(
                "Revolut EUR", "revolut-eur", "Revolut",
                2500.0, &[], &[("EUR", 2500.0)],
                &["checkings"],
            ),
            fixture_holdings_account(
                "Revolut USD", "revolut-usd", "Revolut",
                1000.0, &[], &[("USD", 1000.0)],
                &["checkings"],
            ),
            fixture_holdings_account(
                "Revolut JPY", "revolut-jpy", "Revolut",
                50000.0, &[], &[("JPY", 50000.0)],
                &["checkings"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        // Per-account: each entry only carries its own currency.
        assert_eq!(meta[0]["cash_by_currency"]["EUR"], 2500.0);
        assert!(meta[1]["cash_by_currency"].get("EUR").is_none());
        assert_eq!(meta[1]["cash_by_currency"]["USD"], 1000.0);
        assert_eq!(meta[2]["cash_by_currency"]["JPY"], 50000.0);
        // Cash field (EUR only) is 0 for non-EUR accounts.
        assert_eq!(meta[0]["cash"], 2500.0);
        assert_eq!(meta[1]["cash"], 0.0);
        assert_eq!(meta[2]["cash"], 0.0);
        // kind for non-EUR-only cash falls into `other` (cash field is EUR-scoped).
        assert_eq!(meta[1]["kind"], "other");
        // Aggregate across the snapshot keeps every currency.
        let agg = aggregate_cash_by_currency(&meta);
        assert_eq!(agg["EUR"], 2500.0);
        assert_eq!(agg["USD"], 1000.0);
        assert_eq!(agg["JPY"], 50000.0);
    }

    #[test]
    fn test_snapshot_portfolio_aggregates() {
        use crate::native_collection_helpers::{build_holdings_metadata, build_portfolio_summary};
        let holdings = vec![
            // Investment account — PEA with securities, no cash
            fixture_holdings_account(
                "PEA", "pea-1", "Bourse Direct",
                15000.0,
                &[json!({"ticker": "MC"}), json!({"ticker": "AI"})],
                &[],
                &["stocks"],
            ),
            // Cash-only — Livret A
            fixture_holdings_account(
                "Livret A", "livret-a", "Banque X",
                12000.0, &[], &[("EUR", 12000.0)],
                &["savings"],
            ),
            // Liability — loan
            fixture_holdings_account(
                "Loan", "loan-1", "Banque X",
                -85000.0, &[], &[],
                &["loans"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        let summary = build_portfolio_summary(&meta);
        // 15000 + 12000 - 85000 = -58000
        assert_eq!(summary["total_value"], -58000.0);
        assert_eq!(summary["total_cash_eur"], 12000.0);
        assert_eq!(summary["account_count"], 3);
        assert_eq!(summary["value_by_kind"]["investment"], 15000.0);
        assert_eq!(summary["value_by_kind"]["cash_only"], 12000.0);
        assert_eq!(summary["value_by_kind"]["liability"], -85000.0);
        assert_eq!(summary["value_by_institution_provider_category"]["stocks"], 15000.0);
        assert_eq!(summary["value_by_institution_provider_category"]["savings"], 12000.0);
        assert_eq!(summary["value_by_institution_provider_category"]["loans"], -85000.0);
        assert_eq!(summary["cash_by_currency"]["EUR"], 12000.0);
    }

    #[test]
    fn test_snapshot_holdings_accounts_includes_all_kinds() {
        // Five-holding mix mirrors what the user actually has — investment,
        // cash-only, liability, real-estate manual, multi-currency.
        // `build_holdings_metadata` must surface all of them with `kind`.
        use crate::native_collection_helpers::build_holdings_metadata;
        let holdings = vec![
            fixture_holdings_account(
                "PEA", "pea-1", "Bourse Direct",
                10000.0,
                &[json!({"ticker": "MC"})],
                &[("EUR", 200.0)],
                &["stocks"],
            ),
            fixture_holdings_account(
                "Livret", "livret-1", "Banque X",
                3000.0, &[], &[("EUR", 3000.0)],
                &["savings"],
            ),
            fixture_holdings_account(
                "Pret", "pret-1", "Banque X",
                -50000.0, &[], &[],
                &["loans"],
            ),
            fixture_holdings_account(
                "Maison", "maison-1", "Manual",
                250000.0, &[], &[],
                &["real_estate"],
            ),
            fixture_holdings_account(
                "Revolut JPY", "rev-jpy", "Revolut",
                4000.0, &[], &[("JPY", 4000.0)],
                &["checkings"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        assert_eq!(meta.len(), 5, "all five holdings must be surfaced");
        let kinds: Vec<&str> = meta.iter()
            .map(|e| e["kind"].as_str().unwrap_or("?"))
            .collect();
        assert_eq!(kinds, vec!["investment", "cash_only", "liability", "other", "other"]);
        // PEA stock has both securities AND cash — kind=investment, cash preserved.
        assert_eq!(meta[0]["cash"], 200.0);
        assert_eq!(meta[0]["securities_count"], 1);
    }

    #[test]
    fn test_cross_account_context_excludes_target() {
        use crate::native_collection_helpers::{
            build_cross_account_context, build_holdings_metadata, build_portfolio_summary,
        };
        let holdings = vec![
            fixture_holdings_account(
                "PEA", "pea-1", "Bourse Direct",
                10000.0, &[json!({"ticker":"MC"})], &[], &["stocks"],
            ),
            fixture_holdings_account(
                "Livret", "livret-1", "Banque X",
                3000.0, &[], &[("EUR", 3000.0)], &["savings"],
            ),
            fixture_holdings_account(
                "CTO", "cto-1", "Bourse Direct",
                5000.0, &[json!({"ticker":"AAPL"})], &[], &["stocks"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        let summary = build_portfolio_summary(&meta);
        let snapshot = json!({
            "holdings_accounts": meta,
            "portfolio_summary": summary,
            "positions": [],
        });
        let ctx = build_cross_account_context(&snapshot, "PEA", json!([]));
        // The target itself must NOT appear in other_accounts.
        let others = ctx["other_accounts"].as_array().unwrap();
        assert_eq!(others.len(), 2, "PEA excluded → 2 other accounts");
        let names: Vec<&str> = others.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(!names.contains(&"PEA"));
        assert!(names.contains(&"Livret"));
        assert!(names.contains(&"CTO"));
        // portfolio_totals must be the full-portfolio summary
        assert_eq!(ctx["portfolio_totals"]["account_count"], 3);
        assert_eq!(ctx["target_account"], "PEA");
    }

    #[test]
    fn test_cross_account_context_top_positions() {
        use crate::native_collection_helpers::{
            build_cross_account_context, build_holdings_metadata,
        };
        // CTO has 5 positions; cross_account_context must keep only the 3
        // largest in `top_positions`, sorted by `valeur_actuelle` desc.
        let holdings = vec![
            fixture_holdings_account(
                "PEA", "pea-1", "Bourse Direct",
                10000.0, &[json!({"ticker":"MC"})], &[], &["stocks"],
            ),
            fixture_holdings_account(
                "CTO", "cto-1", "Bourse Direct",
                15000.0,
                &[
                    json!({"ticker":"A"}), json!({"ticker":"B"}),
                    json!({"ticker":"C"}), json!({"ticker":"D"}),
                    json!({"ticker":"E"}),
                ],
                &[],
                &["stocks"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        // All positions used to compute top_positions (filtered by compte field)
        let positions = json!([
            {"ticker":"A","nom":"AlphaCo","compte":"CTO","valeur_actuelle":500.0},
            {"ticker":"B","nom":"BetaCo","compte":"CTO","valeur_actuelle":3000.0},
            {"ticker":"C","nom":"CharlieCo","compte":"CTO","valeur_actuelle":1000.0},
            {"ticker":"D","nom":"DeltaCo","compte":"CTO","valeur_actuelle":7000.0},
            {"ticker":"E","nom":"EchoCo","compte":"CTO","valeur_actuelle":2500.0},
            {"ticker":"MC","nom":"LVMH","compte":"PEA","valeur_actuelle":10000.0},
        ]);
        let snapshot = json!({
            "holdings_accounts": meta,
            "portfolio_summary": {},
            "positions": positions,
        });
        let ctx = build_cross_account_context(&snapshot, "PEA", json!([]));
        let others = ctx["other_accounts"].as_array().unwrap();
        let cto = others.iter().find(|e| e["name"] == "CTO").unwrap();
        let tops = cto["top_positions"].as_array().expect("top_positions present");
        assert_eq!(tops.len(), 3, "max 3 top positions");
        // Sorted by value desc: D(7000), B(3000), E(2500)
        let tickers: Vec<&str> = tops.iter().filter_map(|t| t["ticker"].as_str()).collect();
        assert_eq!(tickers, vec!["D", "B", "E"]);
        // Weights sum to total (7000+3000+2500+1000+500 = 14000 total in CTO):
        // D = 7000/14000 = 50.0%
        assert!((tops[0]["weight_pct"].as_f64().unwrap() - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_cross_account_context_non_investment_omits_top_positions() {
        use crate::native_collection_helpers::{
            build_cross_account_context, build_holdings_metadata,
        };
        let holdings = vec![
            fixture_holdings_account(
                "PEA", "pea-1", "Bourse Direct",
                10000.0, &[json!({"ticker":"MC"})], &[], &["stocks"],
            ),
            fixture_holdings_account(
                "Livret", "livret-1", "Banque X",
                3000.0, &[], &[("EUR", 3000.0)], &["savings"],
            ),
        ];
        let meta = build_holdings_metadata(&holdings);
        let snapshot = json!({
            "holdings_accounts": meta,
            "portfolio_summary": {},
            "positions": [],
        });
        let ctx = build_cross_account_context(&snapshot, "PEA", json!([]));
        let others = ctx["other_accounts"].as_array().unwrap();
        let livret = others.iter().find(|e| e["name"] == "Livret").unwrap();
        assert!(livret.get("top_positions").is_none(),
            "non-investment accounts must omit top_positions");
    }

    #[test]
    fn test_synthesis_prompt_contains_cross_account_section() {
        // Both `build_synthesis_prompt` and `build_report_prompt` must render
        // the cross-account block when run_state holds a cross_account_context.
        // We test through `build_cross_account_prompt_section` directly since
        // the prompt builders are private but render this helper verbatim.
        use crate::native_collection_helpers::build_cross_account_prompt_section;
        let context = json!({
            "target_account": "PEA",
            "other_accounts": [
                {
                    "slug": "livret-1",
                    "name": "Livret A",
                    "institution_name": "Banque X",
                    "kind": "cash_only",
                    "institution_provider_categories": ["savings"],
                    "total_value_eur": 12000.0,
                    "cash_by_currency": {"EUR": 12000.0},
                },
                {
                    "slug": "cto-1",
                    "name": "CTO US",
                    "institution_name": "Broker Y",
                    "kind": "investment",
                    "institution_provider_categories": ["stocks"],
                    "total_value_eur": 25000.0,
                    "cash_by_currency": {"USD": 800.0},
                    "top_positions": [
                        {"ticker": "AAPL", "nom": "Apple", "weight_pct": 35.0},
                        {"ticker": "MSFT", "nom": "Microsoft", "weight_pct": 25.0}
                    ]
                }
            ],
            "portfolio_totals": {
                "total_value": 47000.0,
                "total_cash_eur": 12000.0,
                "cash_by_currency": {"EUR": 12000.0, "USD": 800.0},
                "value_by_kind": {"investment": 35000.0, "cash_only": 12000.0},
                "value_by_institution_provider_category": {"stocks": 35000.0, "savings": 12000.0},
                "account_count": 3
            },
            "cross_account_themes": [
                {"theme": "ai semiconductors", "tickers": ["NVDA", "AAPL"], "accounts": ["PEA", "CTO US"]}
            ]
        });
        let section = build_cross_account_prompt_section(&context);
        // Key markers — prove the section renders the right blocks
        assert!(section.contains("Contexte cross-account"),
            "section header present");
        assert!(section.contains("PEA"), "target account named");
        assert!(section.contains("Livret A"), "other account listed");
        assert!(section.contains("CTO US"), "other account listed");
        assert!(section.contains("kind=cash_only"), "kind exposed");
        assert!(section.contains("kind=investment"), "kind exposed");
        assert!(section.contains("AAPL(35%)"), "top position with weight");
        assert!(section.contains("ai semiconductors"), "cross-account theme rendered");
        assert!(section.contains("la synthese reste centree sur \"PEA\""),
            "instruction line names target account");
        // Multi-currency
        assert!(section.contains("USD=800"), "USD cash exposed");
        assert!(section.contains("EUR=12000"), "EUR cash exposed");
    }

    #[test]
    fn test_cross_account_prompt_section_empty_when_no_others() {
        // Single-account user: no other_accounts and no themes → skip section
        // entirely to avoid prompt noise.
        use crate::native_collection_helpers::build_cross_account_prompt_section;
        let context = json!({
            "target_account": "PEA",
            "other_accounts": [],
            "portfolio_totals": {"account_count": 1},
            "cross_account_themes": []
        });
        let section = build_cross_account_prompt_section(&context);
        assert!(section.is_empty(),
            "single-account user → empty section, prompt unaffected");
    }

    #[test]
    fn test_snapshot_accounts_ui_contract_unchanged() {
        // CRITICAL byte-equality regression test: `snapshot.accounts` is the UI
        // contract. shell-layout.js and global-portfolio-synthesis.js iterate
        // over it expecting `{ name, total_value, total_gain, cash }`. The
        // Phase 1 enrichment must NOT mutate this field — it adds parallel
        // `holdings_accounts` / `portfolio_summary` fields instead.
        //
        // We exercise the snapshot via `normalize_finary_snapshot`, which is the
        // last touch-point before the snapshot is persisted/consumed. The
        // reference shape mirrors what `fetch_finary_snapshot` produces today
        // (and what the UI has been reading for months).
        let snapshot_with_phase1_fields = json!({
            "total_value": 10000.0,
            "total_gain": 500.0,
            "cash": 1500.0,
            "positions": [
                {
                    "ticker": "MC", "nom": "LVMH", "isin": "FR0000121014",
                    "quantite": 2, "prix_actuel": 800, "valeur_actuelle": 1600,
                    "prix_revient": 700, "plus_moins_value": 200, "plus_moins_value_pct": 14.3,
                    "compte": "PEA"
                }
            ],
            "accounts": [
                { "name": "PEA", "cash": 1500.0, "total_value": 1600.0, "total_gain": 200.0 }
            ],
            "transactions": [],
            "orders": [],
            // New Phase 1 fields:
            "holdings_accounts": [{"name":"PEA","slug":"pea","kind":"investment"}],
            "portfolio_summary": {"total_value": 10000.0, "account_count": 1},
            "cash_by_currency": {"EUR": 1500.0}
        });

        let normalized = crate::native_collection_helpers::normalize_finary_snapshot(
            &snapshot_with_phase1_fields,
        );

        // `accounts` MUST be byte-identical to the input (no reordering, no field added).
        let actual_accounts = serde_json::to_string(&normalized["accounts"]).unwrap();
        let expected_accounts = serde_json::to_string(&snapshot_with_phase1_fields["accounts"]).unwrap();
        assert_eq!(actual_accounts, expected_accounts,
            "snapshot.accounts UI contract must be byte-identical");

        // The new fields must survive normalization (consumed by synthesis prompt).
        assert!(normalized.get("holdings_accounts").is_some());
        assert!(normalized.get("portfolio_summary").is_some());
        assert!(normalized.get("cash_by_currency").is_some());
    }

    // ── Fix 1 — sync_position_from_market (Bug A primary fix) ───────────

    #[test]
    fn sync_position_from_market_applies_real_provider_price() {
        // Revolut-style position whose inline `fetch_market_spot` failed
        // during reconciliation (prix_actuel == 0). The later async
        // enrichment returned a valid `boursorama:spot` price — that price
        // must overwrite the position fields so the UI shows real values.
        let mut position = json!({
            "ticker": "AMD",
            "quantite": 10.0,
            "prix_actuel": 0.0,
            "valeur_actuelle": 0.0,
            "prix_revient": 100.0,
            "plus_moins_value": 0.0,
            "plus_moins_value_pct": 0.0,
        });
        let market = json!({
            "prix_actuel": 150.0,
            "source": "boursorama:spot",
        });
        let updated = crate::native_collection_helpers::sync_position_from_market(
            &mut position,
            &market,
        );
        assert!(updated, "real provider price must be applied");
        assert_eq!(position["prix_actuel"].as_f64().unwrap(), 150.0);
        assert_eq!(position["valeur_actuelle"].as_f64().unwrap(), 1500.0);
        assert_eq!(position["plus_moins_value"].as_f64().unwrap(), 500.0);
        assert_eq!(position["plus_moins_value_pct"].as_f64().unwrap(), 50.0);
    }

    #[test]
    fn sync_position_from_market_rejects_pru_fallback_source() {
        // CRITICAL guard: `resolve_source_current_price` writes the PRU
        // into `market[ticker].prix_actuel` with `source = "none"` when every
        // provider fails. We MUST NOT propagate that into the position fields
        // — doing so would silently overwrite the user's holdings with a
        // zero P&L (prix_actuel == prix_revient).
        let mut position = json!({
            "ticker": "TX",
            "quantite": 5.0,
            "prix_actuel": 0.0,
            "valeur_actuelle": 0.0,
            "prix_revient": 80.0,
            "plus_moins_value": 0.0,
            "plus_moins_value_pct": 0.0,
        });
        let market_with_pru_fallback = json!({
            "prix_actuel": 80.0,
            "source": "none",
        });
        let updated = crate::native_collection_helpers::sync_position_from_market(
            &mut position,
            &market_with_pru_fallback,
        );
        assert!(!updated, "source=none must be rejected");
        assert_eq!(position["prix_actuel"].as_f64().unwrap(), 0.0);
        assert_eq!(position["valeur_actuelle"].as_f64().unwrap(), 0.0);

        // Sanity check on `is_real_market_source` directly.
        assert!(!crate::native_collection_helpers::is_real_market_source("none"));
        assert!(!crate::native_collection_helpers::is_real_market_source(""));
        assert!(crate::native_collection_helpers::is_real_market_source("boursorama:spot"));
        assert!(crate::native_collection_helpers::is_real_market_source("google_finance:spot"));
    }

    #[test]
    fn sync_position_from_market_handles_missing_prix_revient_safely() {
        // Watchlist-style row with no PRU. `plus_moins_value_pct` must fall
        // back to 0 instead of dividing by zero; the other fields still get
        // populated from the market price.
        let mut position = json!({
            "ticker": "RBT",
            "quantite": 3.0,
            "prix_actuel": 0.0,
            "valeur_actuelle": 0.0,
            "prix_revient": 0.0,
        });
        let market = json!({
            "prix_actuel": 800.0,
            "source": "boursorama:spot",
        });
        let updated = crate::native_collection_helpers::sync_position_from_market(
            &mut position,
            &market,
        );
        assert!(updated);
        assert_eq!(position["prix_actuel"].as_f64().unwrap(), 800.0);
        assert_eq!(position["valeur_actuelle"].as_f64().unwrap(), 2400.0);
        assert_eq!(position["plus_moins_value"].as_f64().unwrap(), 2400.0);
        assert_eq!(
            position["plus_moins_value_pct"].as_f64().unwrap(),
            0.0,
            "no PRU must yield 0% pct, never NaN/Inf"
        );

        // Negative path: no usable market price must leave the position alone.
        let mut position2 = json!({
            "ticker": "RBT", "quantite": 3.0, "prix_actuel": 0.0,
            "prix_revient": 0.0, "valeur_actuelle": 0.0,
        });
        let market_missing = json!({ "source": "boursorama:spot" });
        let updated2 = crate::native_collection_helpers::sync_position_from_market(
            &mut position2,
            &market_missing,
        );
        assert!(!updated2);
        assert_eq!(position2["prix_actuel"].as_f64().unwrap(), 0.0);
    }

    // ── Fix 3 — watchlist persist-before-LLM (Bug B) ────────────────────
    //
    // The actual dispatch logic is wired to a thread queue; the surface we
    // can test deterministically is the contract: `build_collection_state`
    // produces a state where `market[ticker]` is visible to a hypothetical
    // `tool_get_line_data` caller BEFORE the LLM packet is emitted. Mirrors
    // the call sequence in `apply_watchlist_collection_result`.

    #[test]
    fn watchlist_market_data_visible_in_partial_state_before_mcp_dispatch() {
        use serde_json::Map;

        // Simulate the in-flight state at the moment a watchlist drain loop
        // is about to call mcp_dispatch.push: positions are done, market_by_ticker
        // has just received the watchlist ticker's fresh row.
        let snapshot = json!({
            "accounts": [{"name": "PEA PME", "cash": 100.0}],
            "valeur_totale": 1000.0,
            "plus_value_totale": 50.0,
            "liquidites": 100.0,
            "transactions": [],
            "orders": [],
        });
        let incremental_positions: Vec<serde_json::Value> = vec![json!({
            "ticker": "MC", "nom": "LVMH", "quantite": 2.0,
            "prix_actuel": 800.0, "valeur_actuelle": 1600.0,
            "prix_revient": 700.0, "compte": "PEA PME",
        })];
        let mut market_by_ticker: Map<String, serde_json::Value> = Map::new();
        market_by_ticker.insert("MC".into(), json!({"prix_actuel": 800.0, "source": "boursorama:spot"}));
        // Watchlist ticker — was just drained by the dispatch queue.
        market_by_ticker.insert("RBT".into(), json!({"prix_actuel": 800.0, "source": "boursorama:spot"}));
        let mut news_by_ticker: Map<String, serde_json::Value> = Map::new();
        news_by_ticker.insert("RBT".into(), json!({"articles": [], "sources": []}));

        let technicals_by_ticker: Map<String, serde_json::Value> = Map::new();
        let partial_state = crate::native_collection_helpers::build_collection_state(
            &snapshot,
            &incremental_positions,
            &market_by_ticker,
            &news_by_ticker,
            &technicals_by_ticker,
            &serde_json::Value::Null,
            &[],
            &[],
            "finary",
            "ok",
            &serde_json::Value::Null,
            &serde_json::Value::Null,
            None,
        );

        // Contract: tool_get_line_data(watchlist:RBT) reads state.market.RBT.
        // The partial state we hand to `persist_native_collection_state`
        // must expose this — without the persist call before mcp_dispatch.push,
        // it stays invisible until the final flush at end of run.
        let market_rbt = partial_state.get("market").and_then(|m| m.get("RBT"));
        assert!(market_rbt.is_some(), "RBT market data must be present in partial state");
        assert_eq!(
            market_rbt.unwrap().get("prix_actuel").and_then(|v| v.as_f64()).unwrap(),
            800.0,
            "RBT prix_actuel must match the freshly drained provider value"
        );
        assert_eq!(
            market_rbt.unwrap().get("source").and_then(|v| v.as_str()).unwrap(),
            "boursorama:spot",
            "RBT source must be the real provider, not 'none'"
        );

        // Watchlist row must NOT be in positions[] (quantite=0 would inflate totals).
        let positions = partial_state.get("portfolio")
            .and_then(|p| p.get("positions"))
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(positions.len(), 1, "watchlist must not appear as a position");
        assert_eq!(positions[0].get("ticker").and_then(|v| v.as_str()).unwrap(), "MC");
    }

    // ── Fix 4 — repair_line_memory_zero_prices (Bug B follow-up) ────────

    #[test]
    fn repair_line_memory_flags_only_all_zero_price_history() {
        // Build a contaminated store that mirrors the user's actual
        // line-memory.json content for RBT (10 entries all at price 0).
        let mut store = json!({
            "by_ticker": {
                "RBT": {
                    "schema_version": 2,
                    "ticker": "RBT",
                    "signal": "SURVEILLANCE",
                    "signal_history": [
                        { "date": "2026-05-14", "signal": "SURVEILLANCE", "price_at_signal": 0.0 },
                        { "date": "2026-05-07", "signal": "CONSERVER", "price_at_signal": 0.0 },
                        { "date": "2026-04-30", "signal": "CONSERVER", "price_at_signal": 0.0 },
                    ],
                    "price_tracking": {
                        "current_price": 0.0,
                        "price_at_signal": 0.0,
                    }
                },
                "MC": {
                    // Healthy ticker — must be left alone.
                    "schema_version": 2,
                    "ticker": "MC",
                    "signal": "CONSERVER",
                    "signal_history": [
                        { "date": "2026-05-14", "signal": "CONSERVER", "price_at_signal": 800.0 }
                    ],
                    "price_tracking": { "current_price": 805.0, "price_at_signal": 800.0 }
                },
                "PARTIAL": {
                    // Mixed history — one real price means the chain WAS healthy
                    // at some point, so we don't flag this case.
                    "schema_version": 2,
                    "ticker": "PARTIAL",
                    "signal_history": [
                        { "date": "2026-05-14", "price_at_signal": 0.0 },
                        { "date": "2026-05-01", "price_at_signal": 42.0 }
                    ],
                    "price_tracking": { "current_price": 0.0 }
                },
                "FRESH": {
                    // Empty history — first analysis, no contamination possible.
                    "schema_version": 2,
                    "ticker": "FRESH",
                    "signal_history": [],
                    "price_tracking": { "current_price": 0.0 }
                }
            }
        });

        let outcome = crate::native_mcp_analysis::apply_zero_price_repair(&mut store);
        assert_eq!(outcome.flagged, 1, "only RBT should be flagged");
        assert_eq!(outcome.cleared, 0, "no entries had a stale flag to clear");

        let rbt = store.get("by_ticker").and_then(|b| b.get("RBT")).unwrap();
        assert_eq!(
            rbt.get("price_data_unavailable").and_then(|v| v.as_bool()).unwrap(),
            true,
            "RBT must carry the price_data_unavailable=true flag"
        );

        // Signal history MUST be preserved — repair flags, never deletes.
        let history = rbt.get("signal_history").and_then(|v| v.as_array()).unwrap();
        assert_eq!(history.len(), 3, "signal history must be preserved");

        // Healthy tickers must not be touched.
        let mc = store.get("by_ticker").and_then(|b| b.get("MC")).unwrap();
        assert!(
            mc.get("price_data_unavailable").is_none(),
            "MC must not be flagged"
        );
        let partial = store.get("by_ticker").and_then(|b| b.get("PARTIAL")).unwrap();
        assert!(
            partial.get("price_data_unavailable").is_none(),
            "partial-zero history must not be flagged (it has at least one real price)"
        );
        let fresh = store.get("by_ticker").and_then(|b| b.get("FRESH")).unwrap();
        assert!(
            fresh.get("price_data_unavailable").is_none(),
            "empty-history first analysis must not be flagged"
        );

        // Idempotent — running again must not double-count.
        let outcome2 = crate::native_mcp_analysis::apply_zero_price_repair(&mut store);
        assert_eq!(outcome2.flagged, 0, "second pass must not re-flag");
        assert_eq!(outcome2.cleared, 0, "second pass must not clear anything");
    }

    #[test]
    fn build_memory_section_skips_zero_price_when_flagged() {
        // When `price_data_unavailable=true` propagates into the memory
        // payload, `build_memory_section` must NOT render `prix: 0.00€` —
        // that's the bogus price the LLM was reading as a "no quote
        // available" signal.
        let memory_unavailable = json!({
            "schema_version": 2,
            "signal_history": [{"date": "2026-05-14", "signal": "SURVEILLANCE"}],
            "price_tracking": {
                "last_signal": "SURVEILLANCE",
                "last_signal_date": "2026-05-14",
                "price_at_signal": 0.0,
                "return_since_signal_pct": 0.0,
                "current_price": 0.0,
                "signal_accuracy": "unknown",
            },
            "conviction": "moyenne",
            "price_data_unavailable": true,
        });
        let rendered = crate::llm_prompts::build_memory_section(Some(&memory_unavailable));
        assert!(
            !rendered.contains("0.00\u{20ac}"),
            "must not render bogus 0.00€ price when price_data_unavailable=true\nGot: {rendered}"
        );
        assert!(
            rendered.contains("indisponible"),
            "must surface the indisponible marker so the LLM understands the gap\nGot: {rendered}"
        );

        // Sanity — healthy memory still renders the price.
        let memory_healthy = json!({
            "schema_version": 2,
            "signal_history": [{"date": "2026-05-14", "signal": "CONSERVER"}],
            "price_tracking": {
                "last_signal": "CONSERVER",
                "last_signal_date": "2026-05-14",
                "price_at_signal": 800.0,
                "return_since_signal_pct": 0.6,
                "current_price": 805.0,
                "signal_accuracy": "correct",
            },
            "conviction": "forte",
            "price_data_unavailable": false,
        });
        let rendered_healthy = crate::llm_prompts::build_memory_section(Some(&memory_healthy));
        assert!(rendered_healthy.contains("800.00\u{20ac}"));
    }

    #[test]
    fn build_memory_for_prompt_propagates_price_data_unavailable() {
        // The flag must survive the hop from `line-memory.json` →
        // `build_memory_for_prompt` → prompt builder. This is the contract
        // that makes Fix 4 end-to-end.
        let entry = json!({
            "schema_version": 2,
            "signal": "SURVEILLANCE",
            "signal_history": [{"date": "2026-05-14", "price_at_signal": 0.0}],
            "price_data_unavailable": true,
        });
        let prompt_view = crate::native_collection_helpers::build_memory_for_prompt(
            Some(&entry),
            None,
        ).expect("prompt view must be produced");
        assert_eq!(
            prompt_view.get("price_data_unavailable").and_then(|v| v.as_bool()),
            Some(true),
            "price_data_unavailable must round-trip through build_memory_for_prompt"
        );

        // Negative case — entries without the flag must default to false.
        let entry_no_flag = json!({
            "schema_version": 2,
            "signal": "CONSERVER",
            "signal_history": [{"date": "2026-05-14", "price_at_signal": 800.0}],
        });
        let prompt_view2 = crate::native_collection_helpers::build_memory_for_prompt(
            Some(&entry_no_flag),
            None,
        ).unwrap();
        assert_eq!(
            prompt_view2.get("price_data_unavailable").and_then(|v| v.as_bool()),
            Some(false),
        );
    }

    // ── Fix 5: signal_history dedup + distinct-date counting ─────────────
    //
    // Bugs the user reported on ticker ERA: three identical same-day
    // ALLEGEMENT rows in the "Signal Accuracy" widget, scored_count counting
    // duplicates (2/10) and trend "↘ declining" because the duplicates
    // dominated the recent-3 window.

    #[test]
    fn sync_line_memory_dedupes_same_day_same_signal() {
        // Three runs on the same day with the same signal must collapse into
        // ONE signal_history entry. The freshest (date, signal, price_at_signal,
        // run_id) wins — old entries are not promoted deeper into history.
        let today = "2026-05-14";

        let run_a = json!({
            "date": today,
            "signal": "ALLEGEMENT",
            "conviction": "moyenne",
            "price_at_signal": 59.8,
            "run_id": "run-A",
        });
        let run_b = json!({
            "date": today,
            "signal": "ALLEGEMENT",
            "conviction": "moyenne",
            "price_at_signal": 59.9,
            "run_id": "run-B",
        });
        let run_c = json!({
            "date": today,
            "signal": "ALLEGEMENT",
            "conviction": "moyenne",
            "price_at_signal": 60.1,
            "run_id": "run-C",
        });

        // First run starts from no prior history.
        let after_a = crate::native_mcp_analysis::build_signal_history(&run_a, None);
        assert_eq!(after_a.len(), 1, "first same-day run produces one entry");

        // Second same-day same-signal run replaces head, doesn't push.
        let after_b = crate::native_mcp_analysis::build_signal_history(&run_b, Some(&after_a));
        assert_eq!(after_b.len(), 1, "second same-day same-signal run must dedup");
        assert_eq!(
            after_b[0].get("run_id").and_then(|v| v.as_str()),
            Some("run-B"),
            "head must carry the fresher run_id (run-B replaces run-A)"
        );
        assert_eq!(
            after_b[0].get("price_at_signal").and_then(|v| v.as_f64()),
            Some(59.9),
            "head must carry the fresher price"
        );

        // Third same-day same-signal run also dedups against the new head.
        let after_c = crate::native_mcp_analysis::build_signal_history(&run_c, Some(&after_b));
        assert_eq!(after_c.len(), 1, "third same-day same-signal run must still dedup");
        assert_eq!(
            after_c[0].get("run_id").and_then(|v| v.as_str()),
            Some("run-C"),
        );
        assert_eq!(
            after_c[0].get("price_at_signal").and_then(|v| v.as_f64()),
            Some(60.1),
        );
    }

    #[test]
    fn sync_line_memory_prepends_when_signal_changes_same_day() {
        // Same date, different signals → BOTH preserved. This is the rare
        // mid-day strategy/news shift case — it's a legitimate new decision
        // point, not noise.
        let today = "2026-05-14";

        let first = json!({
            "date": today,
            "signal": "ALLEGEMENT",
            "conviction": "moyenne",
            "price_at_signal": 59.9,
            "run_id": "run-1",
        });
        let second = json!({
            "date": today,
            "signal": "ACHAT",
            "conviction": "forte",
            "price_at_signal": 60.5,
            "run_id": "run-2",
        });

        let after_first = crate::native_mcp_analysis::build_signal_history(&first, None);
        let after_second =
            crate::native_mcp_analysis::build_signal_history(&second, Some(&after_first));

        assert_eq!(
            after_second.len(),
            2,
            "same-day signal change must prepend, not dedup"
        );
        assert_eq!(
            after_second[0].get("signal").and_then(|v| v.as_str()),
            Some("ACHAT"),
            "newest entry must be at head"
        );
        assert_eq!(
            after_second[1].get("signal").and_then(|v| v.as_str()),
            Some("ALLEGEMENT"),
            "older entry must still be present at index 1"
        );

        // And the cap at 10 still holds when a long prior history exists.
        let mut long_history: Vec<serde_json::Value> = (0..10).map(|i| {
            json!({
                "date": format!("2026-05-{:02}", 4 + i),
                "signal": "CONSERVER",
                "price_at_signal": 50.0,
                "run_id": format!("old-{i}"),
            })
        }).collect();
        long_history.reverse(); // newest-first
        let capped = crate::native_mcp_analysis::build_signal_history(&first, Some(&long_history));
        assert_eq!(capped.len(), 10, "prepend with cap=10 must trim the tail");
        assert_eq!(
            capped[0].get("run_id").and_then(|v| v.as_str()),
            Some("run-1"),
        );
    }

    #[test]
    fn scored_count_uses_distinct_dates() {
        // Synthetic line-memory store mirroring the user's contaminated ERA
        // history: three same-day ALLEGEMENT duplicates plus older varied
        // signals. After dedup-by-date, scored_count must count distinct
        // dates only.
        let _guard = env_lock();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let state_dir = tempdir.path().join("runtime-state");
        std::fs::create_dir_all(&state_dir).expect("mkdir state_dir");
        std::env::set_var("ALFRED_STATE_DIR", state_dir.as_os_str());
        crate::native_mcp_analysis::line_memory_reset_for_tests();

        // Seed a store on disk so run_get_signal_scorecard can read it.
        let store = json!({
            "by_ticker": {
                "ERA": {
                    "schema_version": 2,
                    "ticker": "ERA",
                    "signal_history": [
                        // 3 same-day ALLEGEMENT duplicates (contamination)
                        { "date": "2026-05-14", "signal": "ALLEGEMENT", "price_at_signal": 59.9 },
                        { "date": "2026-05-14", "signal": "ALLEGEMENT", "price_at_signal": 59.9 },
                        { "date": "2026-05-14", "signal": "ALLEGEMENT", "price_at_signal": 59.9 },
                        // Older distinct dates — one correct sell, one incorrect sell
                        { "date": "2026-05-11", "signal": "ALLEGEMENT", "price_at_signal": 60.0 },
                        { "date": "2026-05-07", "signal": "ALLEGEMENT", "price_at_signal": 58.3 },
                    ],
                    "price_tracking": {
                        "current_price": 59.9,
                        "price_at_signal": 59.9,
                    }
                }
            }
        });
        let lm_path = state_dir.join("line-memory.json");
        std::fs::write(&lm_path, serde_json::to_string(&store).unwrap()).unwrap();

        let card = crate::command_handlers::run_get_signal_scorecard("ERA".to_string())
            .expect("scorecard");

        // Three same-day duplicates collapse to ONE distinct-date bucket.
        // 2026-05-14 ALLEGEMENT @59.9 (current=59.9) → 0% return, sell
        //   expected <0 → incorrect.
        // 2026-05-11 ALLEGEMENT @60.0 (current=59.9) → -0.17% return, sell
        //   expected <0 → correct.
        // 2026-05-07 ALLEGEMENT @58.3 (current=59.9) → +2.7% return, sell
        //   expected <0 → incorrect.
        // So 3 distinct dates scored, 1 correct.
        assert_eq!(
            card.get("scored_count").and_then(|v| v.as_u64()),
            Some(3),
            "scored_count must count distinct dates, not duplicates. Got: {card}"
        );
        assert_eq!(
            card.get("correct_count").and_then(|v| v.as_u64()),
            Some(1),
            "correct_count must count distinct correct dates. Got: {card}"
        );

        std::env::remove_var("ALFRED_STATE_DIR");
        crate::native_mcp_analysis::line_memory_reset_for_tests();
    }

    #[test]
    fn compute_trend_buckets_by_distinct_date() {
        // Three same-day ALLEGEMENT duplicates at the head, followed by a
        // CONSERVER from yesterday and an ACHAT from two days ago. Without
        // dedup, the recent-3 window is all ALLEGEMENT and the "older"
        // window is CONSERVER/ACHAT, which would read as a downgrading
        // trend. After bucketing by distinct (date, signal), the recent-3
        // window is ALLEGEMENT / CONSERVER / ACHAT (3 distinct days) and the
        // older window is empty → trend collapses to "stable".
        let history = vec![
            json!({ "date": "2026-05-14", "signal": "ALLEGEMENT" }),
            json!({ "date": "2026-05-14", "signal": "ALLEGEMENT" }),
            json!({ "date": "2026-05-14", "signal": "ALLEGEMENT" }),
            json!({ "date": "2026-05-13", "signal": "CONSERVER" }),
            json!({ "date": "2026-05-12", "signal": "ACHAT" }),
        ];

        // Verify the dedup helper first.
        let bucketed =
            crate::native_mcp_analysis::dedupe_signal_history_by_date_signal(&history);
        assert_eq!(
            bucketed.len(),
            3,
            "three same-day same-signal entries must collapse to one bucket"
        );
        assert_eq!(
            bucketed[0].get("date").and_then(|v| v.as_str()),
            Some("2026-05-14"),
        );
        assert_eq!(
            bucketed[1].get("date").and_then(|v| v.as_str()),
            Some("2026-05-13"),
        );
        assert_eq!(
            bucketed[2].get("date").and_then(|v| v.as_str()),
            Some("2026-05-12"),
        );

        // The downstream contract: with only 5 entries and 3 distinct
        // (date, signal) buckets, compute_trend operates on the buckets, not
        // the raw history. A history of ALLEGEMENT(sell, weak) → CONSERVER
        // (neutral) → ACHAT(buy) reads as upgrading (weakest-buy index first,
        // strongest last in newest→older order means newest is weaker).
        // Either way the test guarantees: WITHOUT bucketing the result would
        // be "stable" (all 3 recent ALLEGEMENT identical, no signal change
        // in window). WITH bucketing, the recent-3 window contains 3 varied
        // signals → trend reflects real movement.
        //
        // Concretely: newest is ALLEGEMENT(strength=2), older are
        // CONSERVER(4) and ACHAT(6). Walking pairs newest→older gives diffs
        // 2-4=-2 (down) then 4-6=-2 (down), so two downs, zero ups → trend
        // = "downgrading". The point is it MUST NOT be "stable" (which it
        // would be without bucketing, since 3 identical ALLEGEMENT in the
        // window produces all-zero diffs).
        let bucketed_trend = crate::native_mcp_analysis::compute_trend_for_test(&history);
        assert_ne!(
            bucketed_trend, "stable",
            "without bucketing this would be stable (3 identical ALLEGEMENT). \
             After bucketing the recent-3 window varies → trend must not be stable. Got: {bucketed_trend}"
        );
        assert_eq!(
            bucketed_trend, "downgrading",
            "ALLEGEMENT(newest) < CONSERVER < ACHAT (oldest) by signal strength → downgrading. Got: {bucketed_trend}"
        );
    }

    // ── P1 source-bypass chain (sprint follow-up) ────────────────────────
    //
    // The four tests below pin the contract that closes the cascade:
    //   1. Dispatch overwrites `source = "none"` whenever it falls back to PRU.
    //   2. `extract_market_price_from_run_state` refuses to expose a price
    //      from a `source == "none"` row, returning 0.0 so `sync_line_memory`
    //      preserves `price_data_unavailable` instead of clearing it.
    //   3. `build_signal_history` keeps the existing head's non-zero price
    //      on a same-day same-signal replace where the new entry's price is
    //      zero — protects accuracy/trend widgets from PRU-fallback contamination.
    //   4. `apply_zero_price_repair` clears a stale `price_data_unavailable`
    //      flag once the ticker's history regains a real `price_at_signal`.

    #[test]
    fn dispatch_fallback_marks_source_as_none_when_pru_used() {
        // Simulates a partial enrichment response: provider tag came back as
        // `boursorama:spot` but the `prix_actuel` field is null (the typical
        // mid-day Revolut/Boursorama hiccup). The dispatch must fall back to
        // the row's PRU AND rewrite `source` to `"none"` so every downstream
        // guard (`is_real_market_source`, `sync_position_from_market`,
        // `extract_market_price_from_run_state`) refuses to treat the value
        // as authentic provider data.
        let mut market_row = json!({
            "source": "boursorama:spot",
            "prix_actuel": null,
        });
        let source_row = json!({
            "ticker": "TX",
            "prix_revient": 80.0,
            "quantite": 5.0,
        });

        crate::native_collection_dispatch::apply_pru_fallback_to_market_row(
            &mut market_row,
            &source_row,
        );

        assert_eq!(
            market_row.get("prix_actuel").and_then(|v| v.as_f64()),
            Some(80.0),
            "PRU must be applied as the fallback price"
        );
        assert_eq!(
            market_row.get("source").and_then(|v| v.as_str()),
            Some("none"),
            "source MUST be rewritten to 'none' — leaving 'boursorama:spot' \
             would let the PRU bleed into position P&L and signal history"
        );

        // Negative path: when the enrichment already returned a real price,
        // the helper is a no-op (does NOT clobber the legitimate source).
        let mut market_row_real = json!({
            "source": "boursorama:spot",
            "prix_actuel": 152.5,
        });
        crate::native_collection_dispatch::apply_pru_fallback_to_market_row(
            &mut market_row_real,
            &source_row,
        );
        assert_eq!(
            market_row_real.get("source").and_then(|v| v.as_str()),
            Some("boursorama:spot"),
            "real provider source must be preserved when prix_actuel is present"
        );
        assert_eq!(
            market_row_real.get("prix_actuel").and_then(|v| v.as_f64()),
            Some(152.5),
            "real provider price must be preserved"
        );
    }

    #[test]
    fn extract_market_price_returns_zero_when_source_none() {
        // Direct contract test on the pure-function core. The codex path
        // reads market[ticker].prix_actuel out of the cached run state to
        // feed `sync_line_memory(current_price = ...)`. If the market row
        // is a PRU fallback (`source == "none"`), the extractor MUST return
        // 0.0 so `sync_line_memory` carries `price_data_unavailable: true`
        // forward instead of clearing it on a phantom price recovery.
        let state = json!({
            "market": {
                "TX": {
                    "source": "none",
                    "prix_actuel": 80.0,
                }
            }
        });
        let price = crate::native_mcp_analysis::extract_market_price_from_state_value(
            &state, "TX",
        );
        assert_eq!(
            price, 0.0,
            "source=none must yield 0.0 — PRU fallback is not a real price"
        );

        // Sanity — a real provider row returns its price normally.
        let state_real = json!({
            "market": {
                "TX": {
                    "source": "boursorama:spot",
                    "prix_actuel": 152.5,
                }
            }
        });
        let price_real = crate::native_mcp_analysis::extract_market_price_from_state_value(
            &state_real, "TX",
        );
        assert_eq!(price_real, 152.5);

        // Sanity — empty/missing source is also rejected.
        let state_empty = json!({
            "market": {
                "TX": {
                    "source": "",
                    "prix_actuel": 152.5,
                }
            }
        });
        let price_empty = crate::native_mcp_analysis::extract_market_price_from_state_value(
            &state_empty, "TX",
        );
        assert_eq!(
            price_empty, 0.0,
            "empty source must be rejected (treated as no real provider)"
        );

        // Sanity — ticker missing from market returns 0.0.
        let state_missing = json!({ "market": {} });
        let price_missing = crate::native_mcp_analysis::extract_market_price_from_state_value(
            &state_missing, "TX",
        );
        assert_eq!(price_missing, 0.0);
    }

    #[test]
    fn build_signal_history_prefers_priced_head_over_zero_replacement() {
        // Pre-existing head carries a real `price_at_signal`. A same-day
        // same-signal re-run fires with `price_at_signal == 0.0` (the PRU
        // fallback chain produced no real price this run). The replacement
        // must adopt the new run_id/conviction (freshness) but keep the
        // existing head's price (truth).
        let today = "2026-05-14";

        let prior_head = json!({
            "date": today,
            "signal": "SELL",
            "conviction": "moyenne",
            "price_at_signal": 100.0,
            "run_id": "run-prior",
        });
        let prior_history = vec![prior_head];

        let new_entry = json!({
            "date": today,
            "signal": "SELL",
            "conviction": "forte",
            "price_at_signal": 0.0,
            "run_id": "run-new",
        });

        let after = crate::native_mcp_analysis::build_signal_history(
            &new_entry, Some(&prior_history),
        );

        assert_eq!(after.len(), 1, "same-day same-signal must still dedup to one entry");
        let head = &after[0];
        assert_eq!(
            head.get("price_at_signal").and_then(|v| v.as_f64()),
            Some(100.0),
            "existing real price MUST be preserved over a zero replacement"
        );
        assert_eq!(
            head.get("run_id").and_then(|v| v.as_str()),
            Some("run-new"),
            "fresh run_id is adopted (the entry is still 'this run's' decision)"
        );
        assert_eq!(
            head.get("conviction").and_then(|v| v.as_str()),
            Some("forte"),
            "fresh conviction is adopted"
        );
        assert_eq!(
            head.get("signal").and_then(|v| v.as_str()),
            Some("SELL"),
        );

        // Negative path: when the new entry has its own real price, that
        // price MUST win (the existing rule — fresher data is better) so we
        // don't accidentally freeze prices forever.
        let new_with_price = json!({
            "date": today,
            "signal": "SELL",
            "conviction": "forte",
            "price_at_signal": 110.0,
            "run_id": "run-newer",
        });
        let after_real = crate::native_mcp_analysis::build_signal_history(
            &new_with_price, Some(&prior_history),
        );
        assert_eq!(
            after_real[0].get("price_at_signal").and_then(|v| v.as_f64()),
            Some(110.0),
            "non-zero new price must overwrite — preservation only fires on zero",
        );

        // Negative path: when both prior and new have zero, the entry stays
        // zero (nothing to preserve).
        let prior_zero = vec![json!({
            "date": today, "signal": "SELL", "price_at_signal": 0.0, "run_id": "run-a",
        })];
        let after_both_zero = crate::native_mcp_analysis::build_signal_history(
            &new_entry, Some(&prior_zero),
        );
        assert_eq!(
            after_both_zero[0].get("price_at_signal").and_then(|v| v.as_f64()),
            Some(0.0),
        );
    }

    #[test]
    fn repair_line_memory_clears_flag_when_prices_recover() {
        // RBT was previously flagged as `price_data_unavailable: true` (all
        // signal_history[].price_at_signal == 0 and current_price == 0). A
        // subsequent run brought a real price back into the history. An
        // operator-triggered `repair_line_memory_local` must un-flag the
        // ticker — the auto-clear in `sync_line_memory` only fires on a
        // fresh analysis, so the explicit repair has to handle the reverse
        // direction or the flag is sticky forever.
        let mut store = json!({
            "by_ticker": {
                "RBT": {
                    "schema_version": 2,
                    "ticker": "RBT",
                    "price_data_unavailable": true,
                    "signal_history": [
                        // Recovered: latest run picked up a real provider price.
                        { "date": "2026-05-14", "signal": "CONSERVER", "price_at_signal": 815.0 },
                        { "date": "2026-05-07", "signal": "CONSERVER", "price_at_signal": 0.0 },
                    ],
                    "price_tracking": {
                        "current_price": 815.0,
                        "price_at_signal": 815.0,
                    }
                },
                "STILL_BROKEN": {
                    // Untouched contamination — flag must stay (idempotent).
                    "schema_version": 2,
                    "ticker": "STILL_BROKEN",
                    "price_data_unavailable": true,
                    "signal_history": [
                        { "date": "2026-05-14", "signal": "SURVEILLANCE", "price_at_signal": 0.0 },
                    ],
                    "price_tracking": { "current_price": 0.0 }
                }
            }
        });

        let outcome = crate::native_mcp_analysis::apply_zero_price_repair(&mut store);
        assert_eq!(outcome.flagged, 0, "no fresh contamination to flag");
        assert_eq!(outcome.cleared, 1, "RBT must be cleared exactly once");

        let rbt = store.get("by_ticker").and_then(|b| b.get("RBT")).unwrap();
        // Either the flag is gone, or it's been written to `false`. Both
        // are acceptable downstream; we standardise on `false` so a partial
        // read never sees the property absent and infers an unset state.
        let flag_value = rbt
            .get("price_data_unavailable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(
            !flag_value,
            "RBT must no longer carry price_data_unavailable=true"
        );

        // Sanity — STILL_BROKEN was correctly left flagged.
        let broken = store
            .get("by_ticker")
            .and_then(|b| b.get("STILL_BROKEN"))
            .unwrap();
        assert_eq!(
            broken
                .get("price_data_unavailable")
                .and_then(|v| v.as_bool()),
            Some(true),
            "still-contaminated ticker must keep the flag (idempotent)"
        );

        // Running again is a no-op now that RBT has a clean `false` flag.
        let outcome2 = crate::native_mcp_analysis::apply_zero_price_repair(&mut store);
        assert_eq!(outcome2.flagged, 0);
        assert_eq!(outcome2.cleared, 0, "second pass must not re-clear");
    }

    // ── v0.3 #22: cross-account ISIN dedup via canonical key ─────────

    #[test]
    fn canonical_line_memory_key_prefers_resolved_symbol() {
        // Two brokers carrying the same security must collapse onto a
        // single line-memory bucket keyed by the resolved Yahoo symbol.
        let key_pea = crate::native_mcp_analysis::canonical_line_memory_key(
            "STMPA", Some("STMPA.PA"),
        );
        let key_cto = crate::native_mcp_analysis::canonical_line_memory_key(
            "STM", Some("STMPA.PA"),
        );
        assert_eq!(key_pea, "STMPA.PA");
        assert_eq!(key_cto, "STMPA.PA");
        assert_eq!(key_pea, key_cto, "PEA + CTO must share canonical key");
    }

    #[test]
    fn canonical_line_memory_key_falls_back_to_ticker_when_unresolved() {
        // Watchlist entries without ISIN never hit /api/resolve — they must
        // still produce a stable key (uppercased ticker) so we don't lose
        // pre-v0.3 history nor confuse repeated writes.
        let key_none = crate::native_mcp_analysis::canonical_line_memory_key("aapl", None);
        let key_empty = crate::native_mcp_analysis::canonical_line_memory_key("aapl", Some(""));
        let key_blank = crate::native_mcp_analysis::canonical_line_memory_key("aapl", Some("   "));
        assert_eq!(key_none, "AAPL");
        assert_eq!(key_empty, "AAPL");
        assert_eq!(key_blank, "AAPL");
    }

    #[test]
    fn read_line_memory_entry_canonical_first_then_legacy_ticker() {
        // Hybrid store: one entry under raw ticker (pre-v0.3), one under
        // canonical key (v0.3+). The canonical key must win when both
        // exist, and the raw key serves as fallback when only it is
        // present.
        let store = json!({
            "by_ticker": {
                "AAPL":     { "schema_version": 2, "signal": "legacy" },
                "STMPA.PA": { "schema_version": 2, "signal": "canonical" }
            }
        });

        let canonical_hit = crate::native_mcp_analysis::read_line_memory_entry(
            &store, "STMPA", Some("STMPA.PA"),
        );
        assert_eq!(
            canonical_hit.get("signal").and_then(|v| v.as_str()),
            Some("canonical"),
            "canonical-key match must take precedence",
        );

        let legacy_hit = crate::native_mcp_analysis::read_line_memory_entry(
            &store, "AAPL", None,
        );
        assert_eq!(
            legacy_hit.get("signal").and_then(|v| v.as_str()),
            Some("legacy"),
            "raw-ticker key must still be readable for back-compat",
        );

        // Missing entry returns Value::Null.
        let miss = crate::native_mcp_analysis::read_line_memory_entry(
            &store, "TSLA", Some("TSLA"),
        );
        assert!(miss.is_null());
    }

    #[test]
    fn read_line_memory_entry_falls_back_when_canonical_missing() {
        // v0.3 first-run-after-upgrade: history was written under the raw
        // ticker by an older binary; the new resolver returns a canonical
        // symbol but no entry exists under that key yet. The reader must
        // still surface the legacy entry so memory continuity is preserved.
        let store = json!({
            "by_ticker": {
                "STMPA": { "schema_version": 2, "signal": "achat" }
            }
        });
        let hit = crate::native_mcp_analysis::read_line_memory_entry(
            &store, "STMPA", Some("STMPA.PA"),
        );
        assert_eq!(
            hit.get("signal").and_then(|v| v.as_str()),
            Some("achat"),
            "legacy raw-ticker entry must surface when canonical key has no data",
        );
    }

    #[test]
    fn lookup_resolved_symbol_from_state_returns_canonical_when_set() {
        // The codex batch path has only `ticker` at merge time and must
        // pull `resolved_symbol` from run_state.portfolio.positions[] so
        // sync_line_memory can route the write under the canonical key.
        let state = json!({
            "portfolio": {
                "positions": [
                    { "ticker": "STMPA", "resolved_symbol": "STMPA.PA" },
                    { "ticker": "RBT" },  // no resolved_symbol
                ]
            }
        });

        let stmpa = crate::native_mcp_analysis::lookup_resolved_symbol_from_state_value(
            &state, "stmpa",  // ticker matching is case-insensitive
        );
        assert_eq!(stmpa.as_deref(), Some("STMPA.PA"));

        let rbt = crate::native_mcp_analysis::lookup_resolved_symbol_from_state_value(
            &state, "RBT",
        );
        assert_eq!(rbt, None, "row without resolved_symbol must yield None");

        let unknown = crate::native_mcp_analysis::lookup_resolved_symbol_from_state_value(
            &state, "TSLA",
        );
        assert_eq!(unknown, None, "ticker not in portfolio yields None");
    }

    #[test]
    fn lookup_resolved_symbol_filters_blank_strings() {
        // Defensive: a stale snapshot might carry an empty string under
        // `resolved_symbol`. Treat it as absent so we don't route writes
        // under an empty canonical_key (which collides with falsy keys).
        let state = json!({
            "portfolio": {
                "positions": [
                    { "ticker": "X", "resolved_symbol": "" },
                    { "ticker": "Y", "resolved_symbol": "   " },
                ]
            }
        });
        assert_eq!(
            crate::native_mcp_analysis::lookup_resolved_symbol_from_state_value(&state, "X"),
            None,
        );
        assert_eq!(
            crate::native_mcp_analysis::lookup_resolved_symbol_from_state_value(&state, "Y"),
            None,
        );
    }

    #[test]
    fn sync_line_memory_writes_single_entry_for_cross_account_dup() {
        // The whole point of v0.3 #22: same ISIN held in two accounts under
        // different broker tickers (PEA STMPA / CTO STM) must collapse into
        // ONE entry on disk, keyed by the canonical Yahoo symbol resolved
        // upstream.
        let _guard = env_lock();
        crate::native_mcp_analysis::line_memory_reset_for_tests();

        let pea_rec = json!({
            "ticker": "STMPA", "signal": "ACHAT", "conviction": "haute",
            "synthese": "from PEA", "memory_narrative": "PEA narrative",
        });
        crate::native_mcp_analysis::sync_line_memory_for_test(
            "run-pea", "STMPA", Some("STMPA.PA"), &pea_rec, 42.0,
        );

        let cto_rec = json!({
            "ticker": "STM", "signal": "ACHAT", "conviction": "haute",
            "synthese": "from CTO", "memory_narrative": "CTO narrative",
        });
        crate::native_mcp_analysis::sync_line_memory_for_test(
            "run-cto", "STM", Some("STMPA.PA"), &cto_rec, 41.5,
        );

        let store = crate::native_mcp_analysis::line_memory_read_for_test();
        let by_ticker = store
            .get("by_ticker")
            .and_then(|v| v.as_object())
            .expect("by_ticker map must exist after sync");

        // Both writes must land under STMPA.PA — never under STMPA or STM.
        assert!(
            by_ticker.contains_key("STMPA.PA"),
            "canonical key STMPA.PA missing from line memory:\n{by_ticker:#?}",
        );
        assert!(
            !by_ticker.contains_key("STMPA"),
            "broker key STMPA must NOT exist (cross-account dedup):\n{by_ticker:#?}",
        );
        assert!(
            !by_ticker.contains_key("STM"),
            "broker key STM must NOT exist (cross-account dedup):\n{by_ticker:#?}",
        );

        // And the canonical entry must reflect the LAST write (CTO came
        // second). The merge preserves signal_history across runs — both
        // entries should appear there.
        let canonical_entry = by_ticker.get("STMPA.PA").expect("canonical entry");
        let history = canonical_entry
            .get("signal_history")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            history.len() >= 1,
            "signal_history must record at least the latest run on the canonical entry",
        );
    }

    // ── Contract test: persist must propagate every field build emits ─────
    //
    // Production bug surfaced 2026-05-15: a v0.3.0 run had `technicals: {}`
    // ABSENT from the run JSON despite `build_collection_state` writing it.
    // Root cause: `persist_native_collection_state` uses a hardcoded whitelist
    // of keys from `collection_state` to copy into `run_state`. When new
    // top-level fields were added to `build_collection_state` (`technicals`
    // in v0.2.17, `collection_quality` later), the whitelist was never
    // updated and the data was silently dropped.
    //
    // This is a CONTRACT test: it computes the diff of top-level keys
    // between what `build_collection_state` emits and what survives the
    // persist round-trip. The set must be empty (modulo control fields the
    // persist adds, like `updated_at`). Any future field added to
    // `build_collection_state` will fail this test if the whitelist isn't
    // updated — agents can't silently drop data anymore.
    #[test]
    fn persist_whitelist_covers_all_build_collection_state_fields() {
        use std::collections::HashSet;
        let _guard = env_lock();
        let tmpdir = tempfile::tempdir().expect("tempdir");
        // ALFRED_STATE_DIR is the runtime-state directory itself (per
        // paths::resolve_runtime_state_dir). persist_native_collection_state
        // takes its parent as data_dir, so the actual file lives at
        // ALFRED_STATE_DIR/{run_id}.json (= data_dir/runtime-state/{run_id}.json).
        let runtime_dir = tmpdir.path().join("runtime-state");
        std::fs::create_dir_all(&runtime_dir).expect("mkdir runtime-state");
        std::env::set_var("ALFRED_STATE_DIR", runtime_dir.as_os_str());
        crate::run_state_cache::reset_cache();

        // Seed a minimal run state file so `persist_native_collection_state`
        // can patch it (otherwise it errors run_not_found).
        let run_id = "test_persist_contract_run";
        std::fs::write(
            runtime_dir.join(format!("{run_id}.json")),
            "{}",
        ).expect("seed empty run state");
        let state_dir = tmpdir.path();

        // Build a fully-populated collection_state. Every top-level field
        // produced by build_collection_state must come through persist.
        let mut market = serde_json::Map::new();
        market.insert("STMPA".into(), json!({ "prix_actuel": 51.91, "source": "boursorama:spot" }));
        let news = serde_json::Map::new();
        let mut technicals = serde_json::Map::new();
        technicals.insert("STMPA".into(), json!({
            "as_of": "2026-05-15",
            "source": "yahoo:chart:resolved:STMPA.PA",
            "samples": 250,
            "indicators": { "sma_200": 26.51, "rsi_14": 64.68, "trend_signal": "up" }
        }));
        let snapshot = json!({ "valeur_totale": 1000.0, "plus_value_totale": 0.0, "liquidites": 0.0 });
        let positions = vec![json!({
            "ticker": "STMPA", "nom": "STMicroelectronics",
            "isin": "NL0000226223", "resolved_symbol": "STMPA.PA",
            "quantite": 18.0, "prix_actuel": 51.91, "prix_revient": 26.95,
            "valeur_actuelle": 934.38, "plus_moins_value": 449.28,
            "plus_moins_value_pct": 92.51, "compte": "PEA"
        })];

        let collection_state = crate::native_collection_helpers::build_collection_state(
            &snapshot, &positions, &market, &news, &technicals,
            &serde_json::Value::Null, &[], &[],
            "finary", "ok",
            &serde_json::Value::Null, &serde_json::Value::Null, None,
        );

        // Persist through the actual production pipeline.
        crate::native_line_analysis::persist_native_collection_state(run_id, &collection_state)
            .expect("persist must succeed");
        crate::run_state_cache::flush_to_disk();

        let persisted = crate::run_state_cache::load(state_dir, run_id)
            .expect("must read back the persisted state");

        // Diff: every top-level key emitted by build_collection_state should
        // appear in the persisted run state.
        let emitted_keys: HashSet<String> = collection_state.as_object().unwrap()
            .keys().cloned().collect();
        let persisted_keys: HashSet<String> = persisted.as_object().unwrap()
            .keys().cloned().collect();
        let missing: Vec<&String> = emitted_keys.difference(&persisted_keys).collect();
        assert!(
            missing.is_empty(),
            "CONTRACT VIOLATION: persist_native_collection_state's hardcoded whitelist \
             dropped these fields emitted by build_collection_state: {:?}.\n\
             Fix: add them to the `for key in [...]` array in \
             native_line_analysis.rs::persist_native_collection_state.\n\
             Emitted (build): {:?}\n\
             Persisted: {:?}",
            missing, emitted_keys, persisted_keys
        );

        // Spot check: the technicals payload must round-trip intact when
        // the whitelist propagates it.
        let stmpa_after = persisted.get("technicals").and_then(|t| t.get("STMPA"));
        assert!(stmpa_after.is_some(),
            "STMPA technical must survive persist round-trip");
        assert_eq!(
            stmpa_after.unwrap().get("source").and_then(|s| s.as_str()),
            Some("yahoo:chart:resolved:STMPA.PA"),
            "source audit tag must be preserved verbatim"
        );

        // Clean up env vars — env_lock serializes but doesn't reset on drop.
        // Leaving ALFRED_STATE_DIR pointing at the just-deleted tempdir would
        // break any subsequent test that resolves the runtime state dir from
        // env (parallelism + isolation hygiene).
        std::env::remove_var("ALFRED_STATE_DIR");
    }
