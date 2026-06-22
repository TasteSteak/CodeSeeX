use crate::runtime_config::{RuntimeConfigChange, RuntimeConfigChangeKind, RuntimeConfigService};
use codeseex_store::Store;
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub(super) const WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS: u64 = 5_000;

pub(super) type WarmSearchSourcesFuture = Pin<Box<dyn Future<Output = Value> + Send>>;
pub(super) type WarmSearchSourcesFn =
    Arc<dyn Fn(codeseex_core::NetworkProxyMode) -> WarmSearchSourcesFuture + Send + Sync + 'static>;

pub(super) fn spawn_default_web_search_source_probe_subscriber(
    runtime_config: RuntimeConfigService,
    changes: tokio::sync::broadcast::Receiver<RuntimeConfigChange>,
    store: Store,
) -> tokio::task::JoinHandle<()> {
    spawn_web_search_source_probe_subscriber(
        runtime_config,
        changes,
        store,
        std::time::Duration::from_millis(WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS),
        Arc::new(warm_search_sources_for_probe),
    )
}

pub(super) fn spawn_web_search_source_probe_subscriber(
    runtime_config: RuntimeConfigService,
    mut changes: tokio::sync::broadcast::Receiver<RuntimeConfigChange>,
    store: Store,
    debounce: std::time::Duration,
    warm_sources: WarmSearchSourcesFn,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let mut change = match changes.recv().await {
                Ok(change) => change,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    record_lagged(&store, skipped).await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if !change.has_kind(RuntimeConfigChangeKind::NetworkProxy) {
                continue;
            }
            let sleep = tokio::time::sleep(debounce);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    received = changes.recv() => {
                        let next = match received {
                            Ok(next) => next,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                record_lagged(&store, skipped).await;
                                continue;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        };
                        if next.has_kind(RuntimeConfigChangeKind::NetworkProxy) {
                            change = next;
                            sleep.as_mut().reset(tokio::time::Instant::now() + debounce);
                        }
                    }
                }
            }
            let snapshot = runtime_config.snapshot();
            if snapshot.network_proxy_signature != change.snapshot.network_proxy_signature {
                continue;
            }
            let diagnostic = (warm_sources)(snapshot.config.network_proxy).await;
            let _ = store
                .record_event(
                    "info",
                    "web_search_source_probe",
                    "CodeSeeX web_search source probe completed.",
                    Some(&web_search_source_probe_event_detail(
                        change.source.label(),
                        snapshot.network_proxy_signature.as_str(),
                        diagnostic,
                    )),
                )
                .await;
        }
    })
}

async fn record_lagged(store: &Store, skipped: u64) {
    let _ = store
        .record_event(
            "warn",
            "web_search_source_probe_lagged",
            "CodeSeeX web_search source probe skipped stale config events.",
            Some(&json!({ "skipped": skipped })),
        )
        .await;
}

fn web_search_source_probe_event_detail(
    trigger: &str,
    network_proxy_signature: &str,
    diagnostic: Value,
) -> Value {
    json!({
        "trigger": trigger,
        "debounce_ms": WEB_SEARCH_SOURCE_PROBE_DEBOUNCE_MS,
        "network_proxy_signature": network_proxy_signature,
        "stage": diagnostic.get("stage").cloned().unwrap_or(Value::Null),
        "source_order": diagnostic.get("source_order").cloned().unwrap_or(Value::Null),
        "source_health": diagnostic.get("source_health").cloned().unwrap_or(Value::Null)
    })
}

fn warm_search_sources_for_probe(
    proxy_mode: codeseex_core::NetworkProxyMode,
) -> WarmSearchSourcesFuture {
    Box::pin(crate::tools::web::warm_search_sources(proxy_mode))
}
