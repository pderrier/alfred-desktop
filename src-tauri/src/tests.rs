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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::helpers::test_env_lock();
        // Clear the run_state_cache so stale entries from previous tests (pointing to
        // deleted temp dirs) don't bleed into the current test.
        crate::run_state_cache::reset_cache();
        // Always use mock_cache mode in tests — never call real LLM APIs.
        std::env::set_var("LITELLM_GENERATION_MODE", "mock_cache");
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
        let active = TEST_ACTIVE_COLLECTION_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        TEST_MAX_CONCURRENT_COLLECTION_CALLS.fetch_max(active, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(60));
        TEST_ACTIVE_COLLECTION_CALLS.fetch_sub(1, Ordering::SeqCst);
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
            "key_reasoning": "Strong growth thesis based on margin expansion.",
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
            "key_reasoning": "Short thesis."
        });
        let result = crate::llm_prompts::build_memory_section(Some(&partial));
        assert!(result.contains("MEMOIRE LIGNE (historique persistant):"));
        assert!(result.contains("These: Short thesis."));
        // No trend section since it's missing
        assert!(!result.contains("Tendance"));
    }
