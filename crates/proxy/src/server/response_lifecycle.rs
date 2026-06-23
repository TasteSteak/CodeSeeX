use crate::app_state::ProxyState;
use crate::http_response::json_error;
use crate::responses::compaction::build_compaction_item;
use crate::responses::context::estimate_tokens_from_messages;
use crate::upstream::payload::{request_shape_diagnostic, resolve_upstream_model};
use axum::body::Body;
use axum::http::{Response, StatusCode};
use codeseex_core::context::request_looks_like_codex_full_context;
use codeseex_core::AppConfig;
use codeseex_store::{ClientToolHandoffCall, RequestStatus, Store};
use serde_json::{json, Value};
use uuid::Uuid;
pub(super) async fn ensure_new_response_id(
    state: &ProxyState,
    request_id: &str,
    previous: Option<&str>,
) -> Result<(), Response<Body>> {
    if previous
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|previous| previous == request_id)
    {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "invalid_response_id",
            "response id must not equal previous_response_id".to_owned(),
        ));
    }
    match state.store.response_status(request_id).await {
        Ok(Some(status)) => Err(json_error(
            StatusCode::CONFLICT,
            "duplicate_response_id",
            format!("response id '{request_id}' already exists with status {status:?}"),
        )),
        Ok(None) => Ok(()),
        Err(error) => Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_response_id_check_failed",
            error.to_string(),
        )),
    }
}

#[derive(Debug, Clone)]
pub(super) struct PreviousResponseResolution {
    requested: Option<String>,
    pub(super) resolved: Option<String>,
    kind: &'static str,
    status: Option<&'static str>,
    warning: Option<&'static str>,
}

impl PreviousResponseResolution {
    fn none() -> Self {
        Self {
            requested: None,
            resolved: None,
            kind: "none",
            status: None,
            warning: None,
        }
    }

    fn resolved(previous: &str) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: Some(previous.to_owned()),
            kind: "resolved",
            status: Some("completed"),
            warning: None,
        }
    }

    pub(super) fn inferred_prompt_cache_anchor(previous: &str) -> Self {
        Self {
            requested: None,
            resolved: Some(previous.to_owned()),
            kind: "inferred_prompt_cache_anchor",
            status: Some("completed"),
            warning: None,
        }
    }

    fn missing(previous: &str) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: None,
            kind: "missing",
            status: None,
            warning: Some("previous_response_id was not found in this CodeSeeX process"),
        }
    }

    fn non_completed(previous: &str, status: RequestStatus) -> Self {
        Self {
            requested: Some(previous.to_owned()),
            resolved: None,
            kind: "non_completed",
            status: Some(request_status_name(status)),
            warning: Some(
                "previous_response_id is not completed; local history replay was skipped",
            ),
        }
    }

    pub(super) fn suppressed_service(previous: Option<&str>) -> Self {
        Self {
            requested: previous.map(str::to_owned),
            resolved: None,
            kind: "suppressed_service",
            status: None,
            warning: None,
        }
    }

    fn should_warn(&self, request: &Value) -> bool {
        self.warning.is_some() && !request_looks_like_codex_full_context(request)
    }

    pub(super) fn diagnostic(&self) -> Value {
        json!({
            "requested": self.requested.as_deref(),
            "resolved": self.resolved.as_deref(),
            "kind": self.kind,
            "status": self.status,
            "warning": self.warning
        })
    }
}

fn request_status_name(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::InProgress => "in_progress",
        RequestStatus::Completed => "completed",
        RequestStatus::Failed => "failed",
        RequestStatus::Interrupted => "interrupted",
    }
}

pub(super) async fn resolve_previous_response_id(
    state: &ProxyState,
    previous: Option<&str>,
) -> Result<PreviousResponseResolution, Response<Body>> {
    let Some(previous) = previous.filter(|value| !value.trim().is_empty()) else {
        return Ok(PreviousResponseResolution::none());
    };
    match state.store.response_status(previous).await {
        Ok(Some(RequestStatus::Completed)) => Ok(PreviousResponseResolution::resolved(previous)),
        Ok(Some(status)) => Ok(PreviousResponseResolution::non_completed(previous, status)),
        Ok(None) => Ok(PreviousResponseResolution::missing(previous)),
        Err(error) => Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "state_previous_response_check_failed",
            error.to_string(),
        )),
    }
}

pub(super) async fn resolve_prompt_cache_session_anchor(
    state: &ProxyState,
    input: &Value,
) -> Option<PreviousResponseResolution> {
    if !request_looks_like_codex_full_context(input) {
        return None;
    }
    if input
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return None;
    }
    let prompt_cache_key = input
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let anchor = state
        .store
        .latest_completed_final_response_for_prompt_cache_key(prompt_cache_key)
        .await
        .ok()
        .flatten()?;
    Some(PreviousResponseResolution::inferred_prompt_cache_anchor(
        &anchor,
    ))
}

pub(super) async fn record_previous_response_resolution_warning(
    state: &ProxyState,
    id: &str,
    input: &Value,
    resolution: &PreviousResponseResolution,
) {
    if !resolution.should_warn(input) {
        return;
    }
    let _ = state
        .store
        .record_event(
            "warn",
            "previous_response_resolution_warning",
            "previous_response_id could not be used for local history replay.",
            Some(&json!({
                "id": id,
                "previous_response_resolution": resolution.diagnostic(),
                "requires_full_context_for_lossless_replay": true
            })),
        )
        .await;
}

#[derive(Debug, Clone, Default)]
pub(super) struct RuntimeContextStorageDiagnostic {
    current_mode: &'static str,
    current_full_context_not_stored: bool,
    current_original_input_items: Option<usize>,
    current_original_input_hash: Option<String>,
    previous_full_context_not_stored: bool,
    previous_full_context_response_id: Option<String>,
    continuation_warning: bool,
}

impl RuntimeContextStorageDiagnostic {
    pub(super) fn diagnostic(&self) -> Value {
        json!({
            "current": {
                "mode": self.current_mode,
                "full_context_not_stored": self.current_full_context_not_stored,
                "original_input_items": self.current_original_input_items,
                "original_input_hash": self.current_original_input_hash.as_deref()
            },
            "previous": {
                "full_context_not_stored": self.previous_full_context_not_stored,
                "response_id": self.previous_full_context_response_id.as_deref()
            },
            "continuation_warning": self.continuation_warning
        })
    }
}

pub(super) async fn runtime_context_storage_diagnostic(
    state: &ProxyState,
    input: &Value,
    previous_for_context: Option<&str>,
) -> RuntimeContextStorageDiagnostic {
    let input_items = input.get("input").and_then(Value::as_array).map(Vec::len);
    let current_full_context_not_stored = request_looks_like_codex_full_context(input);
    let current_original_input_hash = current_full_context_not_stored.then(|| {
        stable_log_hash_hex(
            &serde_json::to_vec(input.get("input").unwrap_or(&Value::Null)).unwrap_or_default(),
        )
    });
    let mut diagnostic = RuntimeContextStorageDiagnostic {
        current_mode: if current_full_context_not_stored {
            "codex_full_context_not_stored"
        } else {
            "stored_runtime_context"
        },
        current_full_context_not_stored,
        current_original_input_items: input_items.filter(|_| current_full_context_not_stored),
        current_original_input_hash,
        previous_full_context_not_stored: false,
        previous_full_context_response_id: None,
        continuation_warning: false,
    };

    if let Some(previous) = previous_for_context {
        if let Some(response_id) = first_full_context_not_stored_response_id(state, previous).await
        {
            diagnostic.previous_full_context_not_stored = true;
            diagnostic.previous_full_context_response_id = Some(response_id);
            diagnostic.continuation_warning = !current_full_context_not_stored;
        }
    }
    diagnostic
}

async fn first_full_context_not_stored_response_id(
    state: &ProxyState,
    previous: &str,
) -> Option<String> {
    let chain = state
        .store
        .response_context_chain(previous, 10_000)
        .await
        .ok()?;
    chain
        .into_iter()
        .find(|record| response_has_full_context_not_stored_marker(&record.input))
        .map(|record| record.id)
}

fn response_has_full_context_not_stored_marker(input: &Value) -> bool {
    input
        .pointer("/_codeseex_runtime/mode")
        .and_then(Value::as_str)
        == Some("codex_full_context_not_stored")
}

fn stable_log_hash_hex(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(super) async fn record_runtime_context_storage_events(
    state: &ProxyState,
    id: &str,
    diagnostic: &RuntimeContextStorageDiagnostic,
) {
    if diagnostic.current_full_context_not_stored {
        let _ = state
            .store
            .record_event(
                "debug",
                "runtime_context_storage",
                "CodeSeeX did not duplicate Codex full-context input in runtime storage.",
                Some(&json!({
                    "id": id,
                    "runtime_context_storage": diagnostic.diagnostic()
                })),
            )
            .await;
    }
    if diagnostic.continuation_warning {
        let _ = state
            .store
            .record_event(
                "warn",
                "runtime_context_storage_warning",
                "Continuation references a prior full-context request whose original input was not duplicated in CodeSeeX runtime storage.",
                Some(&json!({
                    "id": id,
                    "runtime_context_storage": diagnostic.diagnostic(),
                    "requires_full_context_for_lossless_replay": true
                })),
            )
            .await;
    }
}

pub(super) async fn record_request_shape_diagnostic(
    store: &Store,
    id: &str,
    endpoint: &str,
    requested_model: Option<&str>,
    model: &str,
    request: &Value,
) {
    let mut detail = request_shape_diagnostic(request);
    if let Some(object) = detail.as_object_mut() {
        object.insert("id".to_owned(), json!(id));
        object.insert("endpoint".to_owned(), json!(endpoint));
        if let Some(requested_model) = requested_model {
            object.insert("requested_model".to_owned(), json!(requested_model));
        }
        object.insert("model".to_owned(), json!(model));
    }
    let _ = store
        .record_event(
            "debug",
            "request_shape_diagnostic",
            "CodeSeeX request shape diagnostic.",
            Some(&detail),
        )
        .await;
}

pub(super) fn response_model_from_input(config: &AppConfig, input: &Value) -> String {
    let requested = input
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    resolve_upstream_model(config, input, requested)
}

pub(super) fn build_automatic_compaction(
    config: &AppConfig,
    request: &Value,
    model: &str,
    context: &crate::responses::context::BuiltResponseContext,
) -> anyhow::Result<Option<Value>> {
    let Some(threshold) = resolve_compact_threshold(request.get("context_management")) else {
        return Ok(None);
    };
    let estimated_tokens = estimate_tokens_from_messages(&context.messages);
    if estimated_tokens < threshold {
        return Ok(None);
    }
    let compaction_id = format!("cmp_{}", Uuid::new_v4().simple());
    let compact = build_compaction_item(
        config,
        &compaction_id,
        model,
        &context.messages,
        &context.tool_facts,
    )?;
    Ok(Some(compact.item))
}

pub(crate) fn resolve_compact_threshold(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Number(number) => number.as_u64().filter(|threshold| *threshold > 0),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| resolve_compact_threshold(Some(item)))
            .find(|threshold| *threshold > 0),
        Value::Object(object) => {
            for key in [
                "compact_threshold",
                "threshold",
                "token_threshold",
                "max_tokens",
            ] {
                if let Some(threshold) = value_to_positive_u64(object.get(key)) {
                    return Some(threshold);
                }
            }
            object
                .get("compaction")
                .and_then(|value| resolve_compact_threshold(Some(value)))
        }
        _ => None,
    }
}

fn value_to_positive_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(number) => number.as_u64().filter(|value| *value > 0),
        Value::String(text) => text.trim().parse::<u64>().ok().filter(|value| *value > 0),
        _ => None,
    }
}

pub(super) fn append_auto_compaction_if_safe(response: &mut Value, item: Option<&Value>) -> bool {
    let Some(item) = item else {
        return false;
    };
    let Some(output) = response.get_mut("output").and_then(Value::as_array_mut) else {
        return false;
    };
    if output
        .iter()
        .any(response_item_requires_client_tool_execution)
    {
        return false;
    }
    output.push(item.clone());
    true
}

fn response_item_requires_client_tool_execution(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("function_call") | Some("custom_tool_call")
    )
}

pub(super) fn client_handoff_guard_calls(
    partition: &crate::tools::ownership::ToolCallPartition,
) -> Vec<ClientToolHandoffCall> {
    partition
        .native
        .iter()
        .chain(partition.external.iter())
        .map(|call| ClientToolHandoffCall {
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        })
        .collect()
}
