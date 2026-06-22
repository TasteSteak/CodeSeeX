use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use codeseex_core::NetworkProxyMode;

use super::super::net::{request_error_message, user_agent};

const SEARCH_SOURCE_ON_DEMAND_PROBE_TIMEOUT_SECS: u64 = 3;
static SEARCH_SOURCE_HEALTH: OnceLock<Mutex<BTreeMap<String, SearchHealthSnapshot>>> =
    OnceLock::new();
static SEARCH_SOURCE_REFRESH_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(super) async fn search_plan(
    client: &reqwest::Client,
    proxy_mode: NetworkProxyMode,
) -> SearchPlan {
    let cache_key = crate::network::proxy_cache_key(proxy_mode);
    if let Some(snapshot) = cached_search_source_health(&cache_key).await {
        return SearchPlan::from_snapshot(snapshot, "cached_probe");
    }
    let refresh_lock = SEARCH_SOURCE_REFRESH_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = refresh_lock.lock().await;
    if let Some(snapshot) = cached_search_source_health(&cache_key).await {
        return SearchPlan::from_snapshot(snapshot, "cached_probe");
    }
    let snapshot = refresh_search_source_health(
        client,
        &cache_key,
        SEARCH_SOURCE_ON_DEMAND_PROBE_TIMEOUT_SECS,
    )
    .await;
    SearchPlan::from_snapshot(snapshot, "on_demand_probe")
}

async fn cached_search_source_health(cache_key: &str) -> Option<SearchHealthSnapshot> {
    let cache = SEARCH_SOURCE_HEALTH.get_or_init(|| Mutex::new(BTreeMap::new()));
    let guard = cache.lock().await;
    guard.get(cache_key).cloned()
}

pub(super) async fn refresh_search_source_health(
    client: &reqwest::Client,
    cache_key: &str,
    timeout_secs: u64,
) -> SearchHealthSnapshot {
    let snapshot = SearchHealthSnapshot {
        cache_key: cache_key.to_owned(),
        checked_at: Instant::now(),
        sources: futures_util::future::join_all(
            SearchSource::ALL
                .iter()
                .copied()
                .map(|source| probe_search_source(client, source, timeout_secs)),
        )
        .await,
    };
    let cache = SEARCH_SOURCE_HEALTH.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = cache.lock().await;
    guard.insert(cache_key.to_owned(), snapshot.clone());
    snapshot
}

async fn probe_search_source(
    client: &reqwest::Client,
    source: SearchSource,
    timeout_secs: u64,
) -> SearchSourceHealth {
    let Some(url) = source.probe_url() else {
        return SearchSourceHealth {
            source,
            reachable: false,
            latency_ms: None,
            status: None,
            error: Some("invalid_probe_url".to_owned()),
        };
    };
    let started = Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        client
            .get(url)
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,application/json;q=0.9,*/*;q=0.8",
            )
            .send(),
    )
    .await;
    let latency_ms = Some(started.elapsed().as_millis() as u64);
    match response {
        Ok(Ok(response)) => SearchSourceHealth {
            source,
            reachable: true,
            latency_ms,
            status: Some(response.status().as_u16()),
            error: None,
        },
        Ok(Err(error)) => SearchSourceHealth {
            source,
            reachable: false,
            latency_ms,
            status: None,
            error: Some(request_error_message(&error)),
        },
        Err(_) => SearchSourceHealth {
            source,
            reachable: false,
            latency_ms,
            status: None,
            error: Some("probe_timeout".to_owned()),
        },
    }
}

pub(super) fn ranked_sources_from_health(health: &[SearchSourceHealth]) -> Vec<SearchSource> {
    let mut health = health.to_vec();
    health.sort_by(|left, right| {
        right
            .reachable
            .cmp(&left.reachable)
            .then_with(|| {
                left.latency_ms
                    .unwrap_or(u64::MAX)
                    .cmp(&right.latency_ms.unwrap_or(u64::MAX))
            })
            .then_with(|| {
                left.source
                    .preferred_rank()
                    .cmp(&right.source.preferred_rank())
            })
    });
    let mut sources = health
        .into_iter()
        .map(|health| health.source)
        .collect::<Vec<_>>();
    for source in SearchSource::ALL {
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SearchSource {
    BingHtml,
    BraveHtml,
    DuckDuckGoLite,
    DuckDuckGoHtml,
    DuckDuckGoInstantAnswer,
}

impl SearchSource {
    pub(super) const ALL: [SearchSource; 5] = [
        SearchSource::BingHtml,
        SearchSource::BraveHtml,
        SearchSource::DuckDuckGoLite,
        SearchSource::DuckDuckGoHtml,
        SearchSource::DuckDuckGoInstantAnswer,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            SearchSource::BingHtml => "bing_html",
            SearchSource::BraveHtml => "brave_html",
            SearchSource::DuckDuckGoLite => "duckduckgo_lite",
            SearchSource::DuckDuckGoHtml => "duckduckgo_html",
            SearchSource::DuckDuckGoInstantAnswer => "duckduckgo_instant_answer",
        }
    }

    pub(super) fn preferred_rank(self) -> usize {
        match self {
            SearchSource::BingHtml => 0,
            SearchSource::BraveHtml => 1,
            SearchSource::DuckDuckGoLite => 2,
            SearchSource::DuckDuckGoHtml => 3,
            SearchSource::DuckDuckGoInstantAnswer => 4,
        }
    }

    pub(super) fn probe_url(self) -> Option<reqwest::Url> {
        match self {
            SearchSource::BingHtml => reqwest::Url::parse_with_params(
                "https://www.bing.com/search",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::BraveHtml => reqwest::Url::parse_with_params(
                "https://search.brave.com/search",
                &[("q", "codeseex web search probe"), ("source", "web")],
            ),
            SearchSource::DuckDuckGoLite => reqwest::Url::parse_with_params(
                "https://lite.duckduckgo.com/lite/",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::DuckDuckGoHtml => reqwest::Url::parse_with_params(
                "https://html.duckduckgo.com/html/",
                &[("q", "codeseex web search probe")],
            ),
            SearchSource::DuckDuckGoInstantAnswer => reqwest::Url::parse_with_params(
                "https://api.duckduckgo.com/",
                &[
                    ("q", "codeseex web search probe"),
                    ("format", "json"),
                    ("no_html", "1"),
                    ("skip_disambig", "1"),
                ],
            ),
        }
        .ok()
    }
}

#[derive(Clone, Debug)]
pub(super) struct SearchSourceHealth {
    pub(super) source: SearchSource,
    pub(super) reachable: bool,
    pub(super) latency_ms: Option<u64>,
    pub(super) status: Option<u16>,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct SearchHealthSnapshot {
    pub(super) cache_key: String,
    pub(super) checked_at: Instant,
    pub(super) sources: Vec<SearchSourceHealth>,
}

#[derive(Clone, Debug)]
pub(super) struct SearchPlan {
    pub(super) cache_key: String,
    pub(super) plan_source: &'static str,
    pub(super) checked_at_age_ms: u64,
    pub(super) ordered_sources: Vec<SearchSource>,
    pub(super) health: Vec<SearchSourceHealth>,
}

impl SearchPlan {
    pub(super) fn from_snapshot(snapshot: SearchHealthSnapshot, plan_source: &'static str) -> Self {
        Self {
            cache_key: snapshot.cache_key,
            plan_source,
            checked_at_age_ms: snapshot.checked_at.elapsed().as_millis() as u64,
            ordered_sources: ranked_sources_from_health(&snapshot.sources),
            health: snapshot.sources,
        }
    }

    #[cfg(test)]
    pub(super) fn unprobed(cache_key: String) -> Self {
        Self {
            cache_key,
            plan_source: "unprobed",
            checked_at_age_ms: 0,
            ordered_sources: SearchSource::ALL.to_vec(),
            health: Vec::new(),
        }
    }

    pub(super) fn primary_sources(&self) -> Vec<SearchSource> {
        self.ordered_sources
            .iter()
            .copied()
            .filter(|source| self.source_reachable(*source) != Some(false))
            .collect()
    }

    pub(super) fn deprioritized_sources(&self) -> Vec<SearchSource> {
        self.ordered_sources
            .iter()
            .copied()
            .filter(|source| self.source_reachable(*source) == Some(false))
            .collect()
    }

    pub(super) fn source_order_names(&self) -> Vec<&'static str> {
        self.ordered_sources
            .iter()
            .map(|source| source.name())
            .collect()
    }

    pub(super) fn deprioritized_source_names(&self) -> Vec<&'static str> {
        self.deprioritized_sources()
            .iter()
            .map(|source| source.name())
            .collect()
    }

    pub(super) fn health_diagnostic(&self) -> Vec<Value> {
        let mut diagnostic = source_health_diagnostic(&self.health, self.checked_at_age_ms);
        for item in &mut diagnostic {
            if let Some(object) = item.as_object_mut() {
                object.insert(
                    "proxy_key".to_owned(),
                    Value::String(self.cache_key.clone()),
                );
            }
        }
        diagnostic
    }

    pub(super) fn source_reachable(&self, source: SearchSource) -> Option<bool> {
        self.health
            .iter()
            .find(|health| health.source == source)
            .map(|health| health.reachable)
    }
}

pub(super) fn source_health_diagnostic(health: &[SearchSourceHealth], age_ms: u64) -> Vec<Value> {
    health
        .iter()
        .map(|health| {
            json!({
                "source": health.source.name(),
                "reachable": health.reachable,
                "latency_ms": health.latency_ms,
                "status": health.status,
                "error": health.error,
                "age_ms": age_ms
            })
        })
        .collect()
}
