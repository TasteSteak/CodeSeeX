//! Remote model/pricing catalog service.
//!
//! Layers (highest wins): user overrides > remote manifest > disk cache >
//! built-in document. The upstream `GET /v1/models` endpoint is deliberately
//! *only* an availability probe: it returns ids and nothing else, so it never
//! contributes windows, capabilities or prices and never removes a model.

use crate::app_state::ProxyState;
use crate::runtime_config::{RuntimeConfigChange, RuntimeConfigService};
use codeseex_core::catalog::{
    build_codeseex_catalog_from_document, write_cached_catalog_document, write_catalog_atomic,
    CatalogDocument,
};
use codeseex_core::config::{AppConfig, UpstreamConfig};
use codeseex_core::urls::models_url;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CODESEEX_CATALOG_RAW_BASE_URL: &str =
    "https://raw.githubusercontent.com/TasteSteak/CodeSeeX";
const CODESEEX_CATALOG_DEFAULT_PATH: &str = "main/catalog/model-catalog.json";
const CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const CATALOG_FETCH_THROTTLE: Duration = Duration::from_secs(60);
/// CodeSeeX is a tray program that can stay open for days, so the manifest is
/// re-checked periodically instead of only once at startup.
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const UPSTREAM_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_PROBE_TTL: Duration = Duration::from_secs(10 * 60);
const UPSTREAM_PROBE_MAX_MODELS: usize = 500;
const ERROR_SUMMARY_LIMIT: usize = 240;

#[derive(Clone, Default)]
pub(crate) struct CatalogService {
    inner: Arc<Mutex<CatalogServiceState>>,
}

#[derive(Default)]
struct CatalogServiceState {
    etag: Option<String>,
    last_attempt: Option<Instant>,
    last_success: Option<Instant>,
    last_failure: Option<String>,
    last_failure_at: Option<Instant>,
    probe: Option<UpstreamProbeEntry>,
}

#[derive(Clone)]
struct UpstreamProbeEntry {
    key: String,
    at: Instant,
    result: UpstreamModelProbe,
}

#[derive(Debug, Clone)]
pub(crate) enum UpstreamModelProbe {
    /// The endpoint answered with a recognizable model list.
    Listed(Vec<String>),
    /// The endpoint answered, but the body was not a recognizable list.
    Unrecognized { status: Option<u16> },
    /// The endpoint could not be used at all (network, timeout, 401/404/...).
    Unavailable { status: Option<u16>, error: String },
}

impl UpstreamModelProbe {

    fn to_value(&self) -> Value {
        match self {
            Self::Listed(models) => json!({
                "status": "listed",
                "models": models,
                "count": models.len()
            }),
            Self::Unrecognized { status } => json!({
                "status": "unknown",
                "reason": "unrecognized_response",
                "upstream_status": status
            }),
            Self::Unavailable { status, error } => json!({
                "status": "unknown",
                "reason": "unavailable",
                "upstream_status": status,
                "error": error
            }),
        }
    }

    fn contains(&self, slug: &str) -> bool {
        match self {
            Self::Listed(models) => models.iter().any(|model| model == slug),
            _ => false,
        }
    }
}

impl CatalogService {
    /// URL of the remote manifest. `CODESEEX_CATALOG_URL` / `[catalog]
    /// source_url` override it; the special values `off`, `none` and
    /// `disabled` turn remote refresh off.
    pub(crate) fn remote_url(config: &AppConfig) -> Option<String> {
        match config
            .catalog_source_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => match value.to_ascii_lowercase().as_str() {
                "off" | "none" | "disabled" | "false" => None,
                _ => Some(value.to_owned()),
            },
            None => Some(format!(
                "{CODESEEX_CATALOG_RAW_BASE_URL}/{CODESEEX_CATALOG_DEFAULT_PATH}"
            )),
        }
    }

    pub(crate) fn status(&self, config: &AppConfig) -> Value {
        let state = self.inner.lock().ok();
        let document = config.catalog_document();
        json!({
            "ok": true,
            "enabled": config.catalog_remote_enabled,
            "source": config.catalog_source_label(),
            "source_label": catalog_source_label(config),
            "revision": document.revision,
            "builtin_revision": codeseex_core::catalog::embedded_catalog_document().revision,
            "provider_name": document.provider_name,
            "model_count": document.models.len(),
            "remote_url": Self::remote_url(config),
            "last_success_ms_ago": state.as_ref().and_then(|state| state.last_success).map(|at| at.elapsed().as_millis() as u64),
            "last_failure": state.as_ref().and_then(|state| state.last_failure.clone()),
            "cache_path": config.catalog_cache_path().to_string_lossy()
        })
    }

    /// Full model + pricing rows for the settings UI.
    pub(crate) fn document_payload(&self, config: &AppConfig) -> Value {
        let document = config.catalog_document();
        let probe = self.cached_probe(config);
        let models = document
            .models
            .iter()
            .map(|model| {
                let rate = document
                    .pricing
                    .rate_for(&model.slug, model.pricing_group().as_deref());
                json!({
                    "slug": model.slug,
                    "display_name": model.display_name,
                    "short_display_name": model.extra.get("short_display_name").cloned().unwrap_or(Value::Null),
                    "description": model.description,
                    "context_window": model.context_window,
                    "effective_context_window_percent": model.effective_context_window_percent,
                    "aliases": model.aliases().collect::<Vec<_>>(),
                    "alias_patterns": model.alias_patterns().collect::<Vec<_>>(),
                    "upstream_slug": model.upstream_slug_or_slug(),
                    "pricing_group": model.pricing_group(),
                    "is_default": model.slug == document.default_slug(),
                    "pricing": rate.as_ref().map(|rate| json!({
                        "cached_input": rate.rates.cached_input,
                        "cache_miss_input": rate.rates.cache_miss_input,
                        "output": rate.rates.output,
                        "source": rate.source.label()
                    })),
                    "upstream_status": probe
                        .as_ref()
                        .map(|probe| if probe.contains(&model.slug) { "listed" } else { "not_listed" })
                        .unwrap_or("unknown")
                })
            })
            .collect::<Vec<_>>();
        json!({
            "revision": document.revision,
            "issued_at": document.issued_at,
            "provider_name": document.provider_name,
            "default_model": document.default_slug(),
            "pricing": document.pricing.to_value(),
            "models": models
        })
    }

    /// Fetches the remote manifest, validates it, caches it and activates it.
    pub(crate) async fn refresh(
        &self,
        runtime: &RuntimeConfigService,
        client: &reqwest::Client,
    ) -> Value {
        let config = runtime.active_config();
        if !config.catalog_remote_enabled {
            return json!({ "ok": false, "error": "catalog_remote_disabled" });
        }
        let Some(url) = Self::remote_url(&config) else {
            return json!({ "ok": false, "error": "catalog_remote_disabled" });
        };

        let (etag, throttled) = {
            let Ok(state) = self.inner.lock() else {
                return json!({ "ok": false, "error": "catalog_state_poisoned" });
            };
            let throttled = state
                .last_attempt
                .is_some_and(|at| at.elapsed() < CATALOG_FETCH_THROTTLE);
            (state.etag.clone(), throttled)
        };
        if throttled {
            return json!({
                "ok": true,
                "revision": config.catalog_document().revision,
                "source": config.catalog_source_label(),
                "throttled": true
            });
        }
        if let Ok(mut state) = self.inner.lock() {
            state.last_attempt = Some(Instant::now());
        }

        let mut request = client
            .get(&url)
            .header(reqwest::header::USER_AGENT, "CodeSeeX")
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(etag) = etag.as_deref() {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }

        let response = match tokio::time::timeout(CATALOG_REQUEST_TIMEOUT, request.send()).await {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return self.record_refresh_failure("catalog_remote_unreachable", &error.to_string(), &config),
            Err(_) => return self.record_refresh_failure("catalog_remote_timeout", "request timed out", &config),
        };

        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return json!({
                "ok": true,
                "revision": config.catalog_document().revision,
                "source": config.catalog_source_label(),
                "not_modified": true
            });
        }
        if !response.status().is_success() {
            return self.record_refresh_failure(
                "catalog_remote_http_error",
                &response.status().to_string(),
                &config,
            );
        }

        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = match response.text().await {
            Ok(body) => body,
            Err(error) => {
                return self.record_refresh_failure(
                    "catalog_remote_read_failed",
                    &error.to_string(),
                    &config,
                )
            }
        };
        let document = match CatalogDocument::from_json(&body) {
            Ok(document) => document,
            Err(error) => {
                return self.record_refresh_failure("catalog_remote_invalid", &error, &config)
            }
        };
        if document.revision == config.catalog_document().revision
            && config.catalog_source_label() == "remote"
        {
            // Remember the validator even when the payload did not change,
            // otherwise every later refresh re-downloads the whole manifest.
            if let Ok(mut state) = self.inner.lock() {
                state.last_success = Some(Instant::now());
                if etag.is_some() {
                    state.etag = etag;
                }
            }
            return json!({
                "ok": true,
                "revision": document.revision,
                "source": "remote",
                "unchanged": true
            });
        }

        self.activate(runtime, &document, etag.as_deref());
        json!({
            "ok": true,
            "revision": document.revision,
            "source": "remote",
            "model_count": document.models.len()
        })
    }

    /// Persists and activates a validated remote document.
    pub(crate) fn activate(
        &self,
        runtime: &RuntimeConfigService,
        document: &CatalogDocument,
        etag: Option<&str>,
    ) {
        let config = runtime.active_config();
        if let Err(error) = write_cached_catalog_document(&config.catalog_cache_path(), document) {
            tracing::warn!(error = %error, "failed to write the catalog cache file");
        }
        runtime.set_catalog_document(Some(document.clone()));
        // The Codex-facing file contract is unchanged; only its generator moves.
        let catalog = build_codeseex_catalog_from_document(&runtime.active_config().catalog_document());
        if let Err(error) = write_catalog_atomic(&config.catalog_path(), &catalog) {
            tracing::warn!(error = %error, "failed to rewrite model-catalog.json");
        }
        if let Ok(mut state) = self.inner.lock() {
            state.last_success = Some(Instant::now());
            state.last_failure = None;
            state.last_failure_at = None;
            if let Some(etag) = etag {
                state.etag = Some(etag.to_owned());
            }
        }
    }

    fn record_refresh_failure(&self, code: &str, detail: &str, config: &AppConfig) -> Value {
        let summary = summarize_error(detail, &[]);
        if let Ok(mut state) = self.inner.lock() {
            state.last_failure = Some(format!("{code}: {summary}"));
            state.last_failure_at = Some(Instant::now());
        }
        json!({
            "ok": false,
            "error": code,
            "message": summary,
            "revision": config.catalog_document().revision,
            "source": config.catalog_source_label()
        })
    }

    /// Upstream availability probe. Never mutates the catalog.
    pub(crate) async fn probe(
        &self,
        config: &AppConfig,
        client: &reqwest::Client,
        force: bool,
    ) -> Value {
        let result = self.probe_result(config, client, force).await;
        result.to_value()
    }

    pub(crate) fn cached_probe(&self, config: &AppConfig) -> Option<UpstreamModelProbe> {
        let key = probe_cache_key(&config.upstream);
        let state = self.inner.lock().ok()?;
        state
            .probe
            .as_ref()
            .filter(|entry| entry.key == key && entry.at.elapsed() < UPSTREAM_PROBE_TTL)
            .map(|entry| entry.result.clone())
    }

    async fn probe_result(
        &self,
        config: &AppConfig,
        client: &reqwest::Client,
        force: bool,
    ) -> UpstreamModelProbe {
        let key = probe_cache_key(&config.upstream);
        if !force {
            if let Some(result) = self.cached_probe(config) {
                return result;
            }
        }
        let result = fetch_upstream_models(config, client).await;
        if let Ok(mut state) = self.inner.lock() {
            state.probe = Some(UpstreamProbeEntry {
                key,
                at: Instant::now(),
                result: result.clone(),
            });
        }
        result
    }

    /// `POST /manager/upstream/test`: explains which credential reaches the
    /// upstream, which URL is used, and what the upstream answered. The key
    /// itself is never included.
    pub(crate) async fn upstream_test(&self, config: &AppConfig, client: &reqwest::Client) -> Value {
        let upstream = &config.upstream;
        let managed_key = crate::secrets::upstream_api_key(config);
        let resolved = crate::upstream::resolve_upstream_authorization(
            upstream,
            crate::upstream::UpstreamAuthRequest {
                inbound: None,
                local_access_token: None,
                managed_key: managed_key.as_deref(),
                passthrough: Default::default(),
            },
            &json!({}),
            &|| codeseex_core::codex_auth::read_codex_auth_api_key(false),
        );
        let url = models_url(&upstream.base_url).ok();
        let mut request = client
            .get(url.clone().unwrap_or_default())
            .header(reqwest::header::USER_AGENT, "CodeSeeX")
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(header) = resolved.header.as_deref() {
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }

        let (status, summary, models) = match url.clone() {
            None => (None, "invalid upstream base URL".to_owned(), None),
            Some(_) => {
                match tokio::time::timeout(UPSTREAM_PROBE_TIMEOUT, request.send()).await {
                    Ok(Ok(response)) => {
                        let status = response.status().as_u16();
                        let body = response.text().await.unwrap_or_default();
                        let models = parse_model_ids(&serde_json::from_str::<Value>(&body).unwrap_or(Value::Null));
                        let needles = secret_needles(upstream, managed_key.as_deref());
                        (
                            Some(status),
                            summarize_error(&body, &needles),
                            models,
                        )
                    }
                    Ok(Err(error)) => (None, summarize_error(&error.to_string(), &[]), None),
                    Err(_) => (None, "request timed out".to_owned(), None),
                }
            }
        };

        json!({
            "ok": status.is_some_and(|status| (200..300).contains(&status)),
            "base_url": upstream.base_url,
            "transport": upstream.transport,
            "official_endpoint": crate::upstream::upstream_is_official(upstream),
            "credential_source": resolved.source,
            "credential_configured": resolved.header.is_some(),
            "managed_key_configured": managed_key.is_some(),
            "url": url,
            "upstream_status": status,
            "upstream_error": summary,
            "models_listed": models.as_ref().map(|models| models.len()),
            "diagnostics": probe_diagnostics(
                upstream,
                resolved.source,
                resolved.header.is_some(),
                status,
                &summary
            )
        })
    }
}

/// The probe has no client request to forward, so it cannot show a relay the
/// `originator` / `user-agent` that real Codex traffic carries. Say so instead
/// of leaving the user with a bare `401 unauthorized client detected`.
fn probe_diagnostics(
    upstream: &UpstreamConfig,
    source: &str,
    has_header: bool,
    status: Option<u16>,
    summary: &str,
) -> String {
    let mut diagnostics = upstream_credential_hint(upstream, source, has_header);
    let client_gate = matches!(status, Some(401 | 403))
        && {
            let summary = summary.to_ascii_lowercase();
            summary.contains("unauthorized client") || summary.contains("unrecognized client")
        };
    if client_gate {
        diagnostics.push_str(
            " This upstream appears to require a Codex client request; the probe only identifies itself as CodeSeeX, while real traffic forwards the client's originator and User-Agent.",
        );
    }
    diagnostics
}

/// Background remote refresh. It is delayed and never blocks the window or the
/// proxy listener, and any failure is recorded as a diagnostic instead of
/// surfacing as an error.
pub(crate) fn spawn_remote_refresh(
    state: ProxyState,
    store: codeseex_store::Store,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Delayed so the window and the proxy come up first.
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let config = state.active_config();
            if config.catalog_remote_enabled && CatalogService::remote_url(&config).is_some() {
                let client = state.client();
                let result = state.catalog.refresh(&state.runtime_config, &client).await;
                let flag = |key: &str| {
                    result.get(key).and_then(Value::as_bool).unwrap_or(false)
                };
                if !flag("throttled") {
                    let _ = store
                        .record_event(
                            if flag("ok") { "info" } else { "warn" },
                            "catalog_remote_refresh",
                            "Remote model catalog refresh finished.",
                            Some(&result),
                        )
                        .await;
                }
            }
            tokio::time::sleep(CATALOG_REFRESH_INTERVAL).await;
        }
    })
}

/// Keeps the store's billing rules in sync with the active pricing document.
/// The store holds no windows or multipliers of its own, so this is the only
/// place that decides which price list the usage history is priced with.
pub(crate) fn spawn_pricing_sync(
    runtime_config: RuntimeConfigService,
    mut changes: tokio::sync::broadcast::Receiver<RuntimeConfigChange>,
    store: codeseex_store::Store,
) -> tokio::task::JoinHandle<()> {
    let _ = store.set_pricing_table(runtime_config.active_config().pricing_table());
    tokio::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(_) => {
                    let pricing = runtime_config.active_config().pricing_table();
                    let _ = store.set_pricing_table(pricing);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn probe_cache_key(upstream: &UpstreamConfig) -> String {
    format!(
        "{}|{}|{}",
        upstream.base_url,
        upstream.credential.label(),
        upstream.api_key.is_some()
    )
}

/// `None` disables every other ambient credential so the probe cannot pick up
/// the Codex sign-in by accident.
pub(crate) fn upstream_credential_hint(
    upstream: &UpstreamConfig,
    source: &str,
    has_header: bool,
) -> String {
    if !has_header {
        return format!(
            "No credential was resolved for {} (source: {source}). Set the upstream API key in CodeSeeX settings, DEEPSEEK_API_KEY, or a Codex sign-in.",
            upstream.base_url
        );
    }
    match source {
        "request" => "The client Authorization is forwarded to the upstream.".to_owned(),
        "env" => "DEEPSEEK_API_KEY (process environment) is used.".to_owned(),
        "secret" => "The key stored in CodeSeeX settings is used.".to_owned(),
        "codex_auth" => "The Codex sign-in credential is used.".to_owned(),
        _ => format!("Credential source: {source}."),
    }
}

fn secret_needles(upstream: &UpstreamConfig, managed_key: Option<&str>) -> Vec<String> {
    let mut needles = Vec::new();
    if let Some(key) = upstream.api_key.as_deref() {
        needles.push(key.to_owned());
    }
    if let Some(key) = managed_key {
        needles.push(key.to_owned());
    }
    needles
}

pub(crate) fn summarize_error(text: &str, secrets: &[String]) -> String {
    let mut summary = text.trim().to_owned();
    for secret in secrets {
        let secret = secret.trim();
        if secret.len() >= 8 {
            summary = summary.replace(secret, "<redacted>");
        }
    }
    summary = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if summary.len() > ERROR_SUMMARY_LIMIT {
        summary.truncate(ERROR_SUMMARY_LIMIT);
        summary.push('…');
    }
    summary
}

/// Tolerant parsing of the OpenAI-compatible model listing shapes.
pub(crate) fn parse_model_ids(value: &Value) -> Option<Vec<String>> {
    let items = match value {
        Value::Array(items) => items.as_slice(),
        Value::Object(object) => object
            .get("data")
            .or_else(|| object.get("models"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)?,
        _ => return None,
    };
    let mut models = Vec::new();
    for item in items.iter().take(UPSTREAM_PROBE_MAX_MODELS) {
        let id = match item {
            Value::String(id) => Some(id.clone()),
            Value::Object(object) => object
                .get("id")
                .or_else(|| object.get("model"))
                .or_else(|| object.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        };
        if let Some(id) = id.map(|id| id.trim().to_owned()).filter(|id| !id.is_empty()) {
            models.push(id);
        }
    }
    Some(models)
}

async fn fetch_upstream_models(
    config: &AppConfig,
    client: &reqwest::Client,
) -> UpstreamModelProbe {
    let upstream = &config.upstream;
    let Ok(url) = models_url(&upstream.base_url) else {
        return UpstreamModelProbe::Unavailable {
            status: None,
            error: "invalid upstream base URL".to_owned(),
        };
    };
    let managed_key = crate::secrets::upstream_api_key(config);
    let resolved = crate::upstream::resolve_upstream_authorization(
        upstream,
        crate::upstream::UpstreamAuthRequest {
            inbound: None,
            local_access_token: None,
            managed_key: managed_key.as_deref(),
            passthrough: Default::default(),
        },
        &json!({}),
        &|| codeseex_core::codex_auth::read_codex_auth_api_key(false),
    );
    let mut request = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "CodeSeeX")
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(header) = resolved.header.as_deref() {
        request = request.header(reqwest::header::AUTHORIZATION, header);
    }

    match tokio::time::timeout(UPSTREAM_PROBE_TIMEOUT, request.send()).await {
        Ok(Ok(response)) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                let needles = secret_needles(upstream, managed_key.as_deref());
                return UpstreamModelProbe::Unavailable {
                    status: Some(status.as_u16()),
                    error: summarize_error(&body, &needles),
                };
            }
            match parse_model_ids(&serde_json::from_str::<Value>(&body).unwrap_or(Value::Null)) {
                Some(models) => UpstreamModelProbe::Listed(models),
                None => UpstreamModelProbe::Unrecognized {
                    status: Some(status.as_u16()),
                },
            }
        }
        Ok(Err(error)) => UpstreamModelProbe::Unavailable {
            status: None,
            error: summarize_error(&error.to_string(), &[]),
        },
        Err(_) => UpstreamModelProbe::Unavailable {
            status: None,
            error: "request timed out".to_owned(),
        },
    }
}

fn catalog_source_label(config: &AppConfig) -> &'static str {
    if !config.catalog_remote_enabled {
        return "builtin";
    }
    match config.catalog_source_label() {
        "remote" => {
            if !config.catalog_overrides.is_empty() {
                "remote+user"
            } else {
                "remote"
            }
        }
        _ => {
            if !config.catalog_overrides.is_empty() {
                "builtin+user"
            } else {
                "builtin"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_diagnostics_explains_a_client_identity_gate() {
        let upstream = UpstreamConfig {
            base_url: "https://relay.example.com/v1".to_owned(),
            ..UpstreamConfig::default()
        };
        let gated = probe_diagnostics(
            &upstream,
            "codex_auth",
            true,
            Some(401),
            "{\"error\":{\"message\":\"unauthorized client detected\"}}",
        );
        assert!(gated.contains("Codex client request"));

        let plain = probe_diagnostics(
            &upstream,
            "codex_auth",
            true,
            Some(500),
            "internal error",
        );
        assert!(!plain.contains("Codex client request"));
    }

    #[test]
    fn parses_the_known_model_list_shapes() {
        assert_eq!(
            parse_model_ids(&json!({ "data": [{ "id": "deepseek-v4-pro" }] })),
            Some(vec!["deepseek-v4-pro".to_owned()])
        );
        assert_eq!(
            parse_model_ids(&json!({ "models": [{ "model": "deepseek-v4-flash" }] })),
            Some(vec!["deepseek-v4-flash".to_owned()])
        );
        assert_eq!(
            parse_model_ids(&json!(["a", "b"])),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(parse_model_ids(&json!({ "ok": true })), None);
        assert_eq!(parse_model_ids(&json!("nope")), None);
    }

    #[test]
    fn error_summaries_redact_and_truncate() {
        let summary = summarize_error(
            "unauthorized client detected: sk-abcdefghijklmnop",
            &["sk-abcdefghijklmnop".to_owned()],
        );
        assert!(!summary.contains("sk-abcdefghijklmnop"));
        assert!(summary.contains("<redacted>"));

        let long = "x".repeat(600);
        assert!(summarize_error(&long, &[]).chars().count() <= ERROR_SUMMARY_LIMIT + 1);
    }

    #[test]
    fn probe_cache_key_tracks_base_url_and_credential_source() {
        let mut upstream = UpstreamConfig::default();
        let key = probe_cache_key(&upstream);
        upstream.base_url = "https://relay.example.com/v1".to_owned();
        assert_ne!(key, probe_cache_key(&upstream));
    }
}
