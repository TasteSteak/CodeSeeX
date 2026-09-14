//! Search engine adapters and their health.
//!
//! Every engine is region-neutral: requests carry only the query, and no
//! per-query market, language, or locale parameter is ever injected.

use codeseex_core::NetworkProxyMode;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::net::{user_agent, WEB_REQUEST_TIMEOUT_SECS};
use super::parsers::{
    parse_bing_results, parse_brave_results, parse_duckduckgo_instant_answer,
    parse_duckduckgo_lite_results,
};

/// How long a probe verdict is trusted before the source is probed again.
pub(super) const HEALTH_TTL: Duration = Duration::from_secs(600);
/// Per-source probe budget.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
/// First cooldown after a source starts failing, doubled on each repeat.
const COOLDOWN_BASE: Duration = Duration::from_secs(30);
const COOLDOWN_MAX: Duration = Duration::from_secs(300);
/// A response shorter than this is a challenge/error page, not a result page.
const MIN_HEALTHY_BODY_BYTES: usize = 512;
/// Minimum spacing between two requests to the same engine. Search engines
/// answer a burst with 429s, and one turn can easily fan out three queries.
const MIN_SOURCE_INTERVAL: Duration = Duration::from_millis(1_200);
const PROBE_QUERY: &str = "codeseex web search probe";

/// One un-normalised search result as an engine reported it.
#[derive(Clone, Debug)]
pub(super) struct RawHit {
    pub(super) title: String,
    pub(super) url: String,
    pub(super) snippet: String,
    pub(super) source: &'static str,
    /// Zero-based rank inside this source's own result list.
    pub(super) rank: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SearchSource {
    BingHtml,
    BraveHtml,
    DuckDuckGoLite,
    DuckDuckGoInstantAnswer,
}

impl SearchSource {
    pub(super) const ALL: [SearchSource; 4] = [
        SearchSource::BingHtml,
        SearchSource::BraveHtml,
        SearchSource::DuckDuckGoLite,
        SearchSource::DuckDuckGoInstantAnswer,
    ];

    pub(super) fn name(self) -> &'static str {
        match self {
            SearchSource::BingHtml => "bing_html",
            SearchSource::BraveHtml => "brave_html",
            SearchSource::DuckDuckGoLite => "duckduckgo_lite",
            SearchSource::DuckDuckGoInstantAnswer => "duckduckgo_instant_answer",
        }
    }

    /// Region-neutral request URL: the query and nothing else.
    fn endpoint(self, query: &str) -> Option<reqwest::Url> {
        match self {
            SearchSource::BingHtml => {
                reqwest::Url::parse_with_params("https://www.bing.com/search", &[("q", query)])
            }
            SearchSource::BraveHtml => reqwest::Url::parse_with_params(
                "https://search.brave.com/search",
                &[("q", query), ("source", "web")],
            ),
            SearchSource::DuckDuckGoLite => reqwest::Url::parse_with_params(
                "https://lite.duckduckgo.com/lite/",
                &[("q", query)],
            ),
            SearchSource::DuckDuckGoInstantAnswer => reqwest::Url::parse_with_params(
                "https://api.duckduckgo.com/",
                &[
                    ("q", query),
                    ("format", "json"),
                    ("no_html", "1"),
                    ("skip_disambig", "1"),
                ],
            ),
        }
        .ok()
    }

    fn accepts(self, content_type: &str, body: &str, max_results: usize) -> Vec<RawHit> {
        match self {
            SearchSource::BingHtml => parse_bing_results(body, max_results),
            SearchSource::BraveHtml => parse_brave_results(body, max_results),
            SearchSource::DuckDuckGoLite => parse_duckduckgo_lite_results(body, max_results),
            SearchSource::DuckDuckGoInstantAnswer => {
                parse_duckduckgo_instant_answer(body, max_results)
            }
        }
        .into_iter()
        .filter(|_| {
            !content_type.contains("javascript")
                && !content_type.contains("image/")
                && !content_type.contains("octet-stream")
        })
        .collect()
    }
}

#[derive(Clone, Debug)]
pub(super) struct SourceOutcome {
    pub(super) source: SearchSource,
    pub(super) hits: Vec<RawHit>,
    pub(super) status: Option<u16>,
    pub(super) error: Option<String>,
    pub(super) latency_ms: u64,
    pub(super) body_bytes: usize,
}

impl SourceOutcome {
    /// The engine answered with a plausible result page.
    pub(super) fn reached(&self) -> bool {
        self.error.is_none()
            && self
                .status
                .is_some_and(|status| (200..300).contains(&status))
            && self.body_bytes >= MIN_HEALTHY_BODY_BYTES
    }

    /// The engine answered *and* produced at least one parsed result.
    pub(super) fn ok(&self) -> bool {
        self.reached() && !self.hits.is_empty()
    }

    fn detail(&self) -> Value {
        json!({
            "source": self.source.name(),
            "reachable": self.reached(),
            "ok": self.ok(),
            "status": self.status,
            "error": self.error,
            "result_count": self.hits.len(),
            "latency_ms": self.latency_ms,
            "body_bytes": self.body_bytes
        })
    }
}

/// Runs one engine for one query, bounded by the web request timeout.
pub(super) async fn search_source(
    client: &reqwest::Client,
    source: SearchSource,
    query: &str,
    max_results: usize,
) -> SourceOutcome {
    let started = Instant::now();
    let Some(url) = source.endpoint(query) else {
        return SourceOutcome {
            source,
            hits: Vec::new(),
            status: None,
            error: Some("invalid_endpoint".to_owned()),
            latency_ms: started.elapsed().as_millis() as u64,
            body_bytes: 0,
        };
    };
    pace(source).await;
    let response = tokio::time::timeout(
        Duration::from_secs(WEB_REQUEST_TIMEOUT_SECS),
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
    let response = match response {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            return SourceOutcome {
                source,
                hits: Vec::new(),
                status: None,
                error: Some(super::net::request_error_message(&error)),
                latency_ms: started.elapsed().as_millis() as u64,
                body_bytes: 0,
            }
        }
        Err(_) => {
            return SourceOutcome {
                source,
                hits: Vec::new(),
                status: None,
                error: Some("request_timeout".to_owned()),
                latency_ms: started.elapsed().as_millis() as u64,
                body_bytes: 0,
            }
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = response.text().await.unwrap_or_default();
    let body_bytes = body.len();
    let latency_ms = started.elapsed().as_millis() as u64;
    if !(200..300).contains(&status) {
        return SourceOutcome {
            source,
            hits: Vec::new(),
            status: Some(status),
            error: Some(format!("http_status_{status}")),
            latency_ms,
            body_bytes,
        };
    }
    let hits = source.accepts(&content_type, &body, max_results);
    SourceOutcome {
        source,
        hits,
        status: Some(status),
        error: None,
        latency_ms,
        body_bytes,
    }
}

#[derive(Clone, Debug)]
struct SourceHealth {
    reachable: bool,
    latency_ms: Option<u64>,
    status: Option<u16>,
    error: Option<String>,
    checked_at: Instant,
    consecutive_failures: u32,
    cooldown_until: Option<Instant>,
}

static HEALTH: OnceLock<Mutex<BTreeMap<String, BTreeMap<String, SourceHealth>>>> = OnceLock::new();
static LAST_REQUEST: OnceLock<Mutex<BTreeMap<&'static str, Instant>>> = OnceLock::new();

fn health_map() -> &'static Mutex<BTreeMap<String, BTreeMap<String, SourceHealth>>> {
    HEALTH.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn last_request_map() -> &'static Mutex<BTreeMap<&'static str, Instant>> {
    LAST_REQUEST.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Spaces out requests to one engine so a fan-out cannot trip its rate limit.
async fn pace(source: SearchSource) {
    let map = last_request_map();
    let wait = {
        let guard = map.lock().await;
        guard
            .get(source.name())
            .map(|last| MIN_SOURCE_INTERVAL.saturating_sub(last.elapsed()))
    };
    if let Some(wait) = wait {
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
    let mut guard = map.lock().await;
    guard.insert(source.name(), Instant::now());
}

#[derive(Clone, Debug)]
pub(super) struct SearchPlan {
    /// Sources to try, best first.
    pub(super) ordered: Vec<SearchSource>,
    /// Sources currently cooling down after repeated failures.
    pub(super) skipped: Vec<SearchSource>,
}

/// Orders the sources for one proxy mode, probing only what is stale.
pub(super) async fn plan(client: &reqwest::Client, proxy_key: &str) -> SearchPlan {
    let now = Instant::now();
    {
        let map = health_map().lock().await;
        if let Some(entries) = map.get(proxy_key) {
            let fresh = SearchSource::ALL.iter().all(|source| {
                entries
                    .get(source.name())
                    .is_some_and(|health| now.duration_since(health.checked_at) < HEALTH_TTL)
            });
            if fresh {
                let entries = entries.clone();
                drop(map);
                return build_plan(&entries, now);
            }
        }
    }
    probe_sources(client, proxy_key).await;
    let entries = {
        let map = health_map().lock().await;
        map.get(proxy_key).cloned().unwrap_or_default()
    };
    build_plan(&entries, now)
}

/// Forces a probe of every source and returns the resulting snapshot.
pub(super) async fn warm_sources(client: &reqwest::Client, proxy_key: &str) -> Value {
    let entries = probe_sources(client, proxy_key).await;
    let plan = build_plan(&entries, Instant::now());
    json!({
        "ok": true,
        "stage": "search_source_probe",
        "proxy_key": proxy_key,
        "source_order": plan.ordered.iter().map(|source| source.name()).collect::<Vec<_>>(),
        "sources_skipped": plan.skipped.iter().map(|source| source.name()).collect::<Vec<_>>(),
        "source_health": health_diagnostics(&entries)
    })
}

async fn probe_sources(
    client: &reqwest::Client,
    proxy_key: &str,
) -> BTreeMap<String, SourceHealth> {
    let outcomes =
        futures_util::future::join_all(SearchSource::ALL.iter().copied().map(|source| {
            let client = client.clone();
            async move {
                tokio::time::timeout(
                    PROBE_TIMEOUT,
                    search_source(&client, source, PROBE_QUERY, 5),
                )
                .await
                .unwrap_or(SourceOutcome {
                    source,
                    hits: Vec::new(),
                    status: None,
                    error: Some("probe_timeout".to_owned()),
                    latency_ms: PROBE_TIMEOUT.as_millis() as u64,
                    body_bytes: 0,
                })
            }
        }))
        .await;
    let mut map = health_map().lock().await;
    let entries = map.entry(proxy_key.to_owned()).or_default();
    for outcome in outcomes {
        // "Unreachable" means the engine did not answer with a real result
        // page: a timeout, a 4xx/5xx, or a tiny challenge body. An engine that
        // answers normally but happens to have no hit for this query is still
        // reachable and must not be cooled down.
        let reachable = outcome.reached();
        let previous = entries.get(outcome.source.name());
        let consecutive_failures = if reachable {
            0
        } else {
            previous.map_or(1, |health| health.consecutive_failures + 1)
        };
        let cooldown_until = cooldown_for(reachable, consecutive_failures);
        entries.insert(
            outcome.source.name().to_owned(),
            SourceHealth {
                reachable,
                latency_ms: Some(outcome.latency_ms),
                status: outcome.status,
                error: outcome.error,
                checked_at: Instant::now(),
                consecutive_failures,
                cooldown_until,
            },
        );
    }
    entries.clone()
}

/// Records the outcome of a real search so cooldowns track live behaviour.
pub(super) async fn record_outcome(proxy_key: &str, outcome: &SourceOutcome) {
    let mut map = health_map().lock().await;
    let entries = map.entry(proxy_key.to_owned()).or_default();
    let previous = entries.get(outcome.source.name());
    let reachable = outcome.reached();
    let consecutive_failures = if reachable {
        0
    } else {
        previous.map_or(1, |health| health.consecutive_failures + 1)
    };
    let cooldown_until = cooldown_for(reachable, consecutive_failures);
    entries.insert(
        outcome.source.name().to_owned(),
        SourceHealth {
            reachable,
            latency_ms: Some(outcome.latency_ms),
            status: outcome.status,
            error: outcome.error.clone(),
            checked_at: Instant::now(),
            consecutive_failures,
            cooldown_until,
        },
    );
}

/// One transient failure is not enough to bench a source: engines blip. Only a
/// repeated run of failures starts a cooldown, and it grows from there.
fn cooldown_for(reachable: bool, consecutive_failures: u32) -> Option<Instant> {
    if reachable || consecutive_failures < 2 {
        return None;
    }
    let excess = consecutive_failures.saturating_sub(2);
    let backoff = COOLDOWN_BASE
        .saturating_mul(1u32 << excess.min(5))
        .min(COOLDOWN_MAX);
    Some(Instant::now() + backoff)
}

fn build_plan(entries: &BTreeMap<String, SourceHealth>, now: Instant) -> SearchPlan {
    let mut ordered = Vec::new();
    let mut skipped = Vec::new();
    for source in SearchSource::ALL {
        let health = entries.get(source.name());
        let cooling = health
            .and_then(|health| health.cooldown_until)
            .is_some_and(|until| until > now);
        if cooling {
            skipped.push(source);
        } else {
            ordered.push(source);
        }
    }
    ordered.sort_by_key(|source| {
        entries
            .get(source.name())
            .and_then(|health| health.latency_ms)
            .unwrap_or(u64::MAX)
    });
    SearchPlan { ordered, skipped }
}

fn health_diagnostics(entries: &BTreeMap<String, SourceHealth>) -> Vec<Value> {
    health_diagnostics_with(entries, &SearchSource::ALL)
}

fn health_diagnostics_with(
    entries: &BTreeMap<String, SourceHealth>,
    ordered: &[SearchSource],
) -> Vec<Value> {
    ordered
        .iter()
        .map(|source| {
            let health = entries.get(source.name());
            json!({
                "source": source.name(),
                "reachable": health.map(|health| health.reachable).unwrap_or(true),
                "latency_ms": health.and_then(|health| health.latency_ms),
                "status": health.and_then(|health| health.status),
                "error": health.and_then(|health| health.error.clone()),
                "consecutive_failures": health.map(|health| health.consecutive_failures).unwrap_or(0),
                "age_ms": health.map(|health| health.checked_at.elapsed().as_millis() as u64).unwrap_or(0)
            })
        })
        .collect()
}

pub(super) fn source_outcome_detail(outcome: &SourceOutcome) -> Value {
    outcome.detail()
}

pub(super) fn proxy_cache_key(proxy_mode: NetworkProxyMode) -> String {
    crate::network::proxy_cache_key(proxy_mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_endpoints_carry_no_region_parameters() {
        for source in SearchSource::ALL {
            let url = source.endpoint("rust release notes").expect("endpoint");
            let query = url.query().unwrap_or_default().to_ascii_lowercase();
            for banned in [
                "mkt=",
                "cc=",
                "setlang=",
                "ensearch=",
                "kl=",
                "market=",
                "locale=",
            ] {
                assert!(
                    !query.contains(banned),
                    "{} leaked {banned} in {query}",
                    source.name()
                );
            }
        }
    }

    #[test]
    fn engine_set_is_region_neutral() {
        for source in SearchSource::ALL {
            let name = source.name();
            for banned in ["baidu", "sogou", "so360", "yandex", "naver"] {
                assert!(!name.contains(banned));
            }
        }
    }

    #[test]
    fn cooldown_grows_with_repeated_failures() {
        let entries = {
            let mut map = BTreeMap::new();
            map.insert(
                SearchSource::BingHtml.name().to_owned(),
                SourceHealth {
                    reachable: false,
                    latency_ms: Some(50),
                    status: Some(429),
                    error: Some("http_status_429".to_owned()),
                    checked_at: Instant::now(),
                    consecutive_failures: 2,
                    cooldown_until: Some(Instant::now() + Duration::from_secs(60)),
                },
            );
            map
        };
        let plan = build_plan(&entries, Instant::now());

        assert!(plan.skipped.contains(&SearchSource::BingHtml));
        assert!(!plan.ordered.contains(&SearchSource::BingHtml));
    }
}
