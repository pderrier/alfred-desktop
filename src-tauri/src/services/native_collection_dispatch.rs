use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::native_collection_helpers::{
    as_text, assess_ticker_quality, fetch_ticker_enrichment, hydrate_row_with_line_memory,
    normalize_ticker, resolve_source_current_price, HttpRequestFn,
};

#[derive(Clone)]
pub(crate) struct CollectionResult {
    pub(crate) index: usize,
    pub(crate) ticker: String,
    pub(crate) name: String,
    pub(crate) hydrated_row: Value,
    pub(crate) market_row: Value,
    pub(crate) news_row: Value,
    pub(crate) issues: Vec<Value>,
    pub(crate) quality: Value,
    pub(crate) hydration_diag: Value,
}

struct CollectionTask {
    index: usize,
    row: Value,
}

struct CollectionWorkerConfig {
    line_memory_store: Arc<Value>,
    news_quality_threshold: i64,
    max_missing_market_fields: usize,
    request_fn: HttpRequestFn,
}

pub(crate) struct NativeCollectionDispatchQueue {
    task_tx: mpsc::Sender<CollectionTask>,
    result_rx: mpsc::Receiver<CollectionResult>,
    throttle_ms: u64,
}

impl NativeCollectionDispatchQueue {
    pub(crate) fn new(
        concurrency: usize,
        throttle_ms: u64,
        line_memory_store: Arc<Value>,
        news_quality_threshold: i64,
        max_missing_market_fields: usize,
        request_fn: HttpRequestFn,
    ) -> Self {
        let (task_tx, task_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let shared_rx = Arc::new(Mutex::new(task_rx));
        let config = CollectionWorkerConfig {
            line_memory_store,
            news_quality_threshold,
            max_missing_market_fields,
            request_fn,
        };

        for _ in 0..std::cmp::max(1, concurrency) {
            let worker_rx = Arc::clone(&shared_rx);
            let worker_tx = result_tx.clone();
            let worker_config = config.clone();
            thread::spawn(move || loop {
                let task = {
                    let guard = worker_rx.lock().expect("collection worker rx lock");
                    guard.recv()
                };
                let Ok(task) = task else {
                    break;
                };
                let result = process_collection_task(&worker_config, task);
                if worker_tx.send(result).is_err() {
                    break;
                }
            });
        }

        Self {
            task_tx,
            result_rx,
            throttle_ms,
        }
    }

    pub(crate) fn push(&mut self, index: usize, row: Value) -> Result<()> {
        self.task_tx
            .send(CollectionTask { index, row })
            .map_err(|_| anyhow!("collection_dispatch_failed:send_failed"))?;
        if self.throttle_ms > 0 {
            thread::sleep(Duration::from_millis(self.throttle_ms));
        }
        Ok(())
    }

    pub(crate) fn drain_ready(&self) -> Vec<CollectionResult> {
        let mut drained = Vec::new();
        while let Ok(result) = self.result_rx.try_recv() {
            drained.push(result);
        }
        drained
    }

    pub(crate) fn recv_blocking(&self) -> Result<CollectionResult> {
        self.result_rx
            .recv()
            .map_err(|_| anyhow!("collection_dispatch_failed:recv_failed"))
    }
}

fn process_collection_task(config: &CollectionWorkerConfig, task: CollectionTask) -> CollectionResult {
    let ticker = normalize_ticker(task.row.get("ticker"));
    let name = as_text(task.row.get("nom"));
    let isin = as_text(task.row.get("isin"));
    let (mut market_row, news_row, issues) = if ticker.is_empty() {
        (Value::Object(Default::default()), json!({ "articles": [], "sources": [] }), Vec::new())
    } else {
        fetch_ticker_enrichment(
            &ticker,
            if name.is_empty() { None } else { Some(name.as_str()) },
            if isin.is_empty() { None } else { Some(isin.as_str()) },
            config.request_fn,
        )
    };
    if market_row.get("prix_actuel").map(|value| value.is_null()).unwrap_or(true) {
        if let Some(price) = resolve_source_current_price(&task.row) {
            if let Some(object) = market_row.as_object_mut() {
                object.insert("prix_actuel".to_string(), json!(price));
            }
        }
    }
    let (hydrated_row, filtered_news_row, hydration_diag) =
        hydrate_row_with_line_memory(&config.line_memory_store, &task.row, &news_row);
    let quality = assess_ticker_quality(
        &ticker,
        &name,
        &market_row,
        &filtered_news_row,
        config.news_quality_threshold,
        config.max_missing_market_fields,
    );
    // Shared insights are fetched on-demand by the MCP server's get_line_data tool
    // (direct API call), not stored in run_state. Deep news enrichment is done
    // server-side in the /api/news handler.
    CollectionResult {
        index: task.index,
        ticker,
        name,
        hydrated_row,
        market_row,
        news_row: filtered_news_row,
        issues,
        quality,
        hydration_diag,
    }
}

impl Clone for CollectionWorkerConfig {
    fn clone(&self) -> Self {
        Self {
            line_memory_store: Arc::clone(&self.line_memory_store),
            news_quality_threshold: self.news_quality_threshold,
            max_missing_market_fields: self.max_missing_market_fields,
            request_fn: self.request_fn,
        }
    }
}
