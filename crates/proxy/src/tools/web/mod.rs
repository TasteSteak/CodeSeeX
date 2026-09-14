//! CodeSeeX local web search.
//!
//! The pipeline is `request -> sources -> rank -> fetch -> extract -> outcome`,
//! with a single result contract handed to the model and to the client. Nothing
//! in here depends on the caller's transport, and no request carries a locale.

mod extract;
mod fetch;
mod ids;
mod net;
mod outcome;
mod parsers;
mod rank;
mod request;
mod safety;
mod sanitize;
mod sources;
mod text;

use codeseex_core::NetworkProxyMode;
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) use request::client_action_from_arguments;

/// Character-bounded truncation, shared with the model-facing compaction so the
/// two boundaries cannot drift apart.
pub(crate) fn truncate_to_chars(text: &str, max_chars: usize) -> String {
    text::truncate_chars(text, max_chars)
}

pub(crate) fn text_char_count(text: &str) -> usize {
    text::char_count(text)
}

/// Hard cap on how many bytes of a single response are read.
const MAX_BYTES: u64 = 524_288;
/// How many candidates a search may return.
const MAX_RESULTS: usize = 8;
/// How many queries one call may run.
const MAX_QUERIES: usize = 3;
/// How many pages one open call may fetch.
const MAX_OPEN_TARGETS: usize = 6;
/// How many top candidates are opened automatically for evidence.
const EVIDENCE_TARGETS: usize = 3;
/// Character budget for one page's rendered evidence excerpt. This is the whole
/// excerpt, including section markers and omission accounting, so what the
/// pipeline selects is exactly what the model receives.
pub(crate) const EVIDENCE_BUDGET_CHARS: usize = 2_000;
/// Budget for the automatic evidence fetch.
const AUTO_OPEN_TIMEOUT_SECS: u64 = 8;
/// Whole-call budget, including automatic evidence.
const SEARCH_TOTAL_TIMEOUT_SECS: u64 = 20;

pub(crate) async fn warm_search_sources(proxy_mode: NetworkProxyMode) -> Value {
    let client = net::web_client(proxy_mode);
    let proxy_key = sources::proxy_cache_key(proxy_mode);
    sources::warm_sources(&client, &proxy_key).await
}

pub(crate) async fn execute(
    proxy_mode: NetworkProxyMode,
    arguments: &Value,
    messages: &[Value],
) -> Value {
    let request = request::WebRequest::parse(arguments);
    let mode = request.mode.as_str();
    match tokio::time::timeout(
        Duration::from_secs(SEARCH_TOTAL_TIMEOUT_SECS),
        outcome::run(proxy_mode, request, messages),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => json!({
            "ok": false,
            "stage": mode,
            "mode": mode,
            "error": "web_search_timeout",
            "message": "web_search exceeded its bounded execution time. Narrow the query, open fewer URLs, or retry when the network is stable.",
            "timeout_seconds": SEARCH_TOTAL_TIMEOUT_SECS
        }),
    }
}
