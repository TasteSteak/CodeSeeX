use crate::upstream::codex_request_markers;
use codeseex_core::models::MODEL_FLASH;
use codeseex_core::{AppConfig, UserConfig};
use serde_json::{json, Value};

const TEXT_SCAN_CHAR_LIMIT: usize = 32_000;

pub(crate) fn normalize_chat_payload(config: &AppConfig, request: &Value, payload: &mut Value) {
    let service_kind = codex_service_request_kind(request);
    if let Some(model) = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        payload["model"] = Value::String(resolve_upstream_model(config, request, &model));
    }
    if let Some(temperature) = config.temperature.value() {
        payload["temperature"] = json!(temperature);
    } else if let Some(temperature) = request.get("temperature").and_then(Value::as_f64) {
        payload["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.get("top_p").and_then(Value::as_f64) {
        payload["top_p"] = json!(top_p);
    }
    if let Some(max_tokens) = request
        .get("max_output_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .and_then(value_to_u64)
    {
        payload["max_tokens"] = json!(max_tokens);
    }
    if let Some(response_format) = response_format_from_request(request) {
        payload["response_format"] = response_format;
        if response_format_needs_json_prompt(request) {
            ensure_json_instruction(payload);
        }
    }
    if let Some(thinking) = thinking_from_request(config, request) {
        payload["thinking"] = thinking;
    }
    if service_kind.is_service() {
        if let Some(object) = payload.as_object_mut() {
            object.remove("tools");
            object.remove("tool_choice");
            object.remove("parallel_tool_calls");
        }
    }
    if payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        payload["stream_options"] = json!({ "include_usage": true });
    }
}

pub(crate) fn resolve_upstream_model(
    config: &AppConfig,
    request: &Value,
    requested: &str,
) -> String {
    let service_kind = codex_service_request_kind(request);
    if service_kind.is_service() {
        return MODEL_FLASH.to_owned();
    }
    config.model_override.upstream_slug(requested)
}

pub(crate) fn request_is_codex_service(request: &Value) -> bool {
    codex_service_request_kind(request).is_service()
}

pub(crate) fn codex_service_request_kind(request: &Value) -> CodexServiceRequestKind {
    CodexServiceRequestKind::from_request(request)
}

pub(crate) fn request_shape_diagnostic(request: &Value) -> Value {
    let shape = RequestShape::from_request(request);
    let service_kind = codex_service_request_kind(request);
    json!({
        "service_routing": if service_kind.is_service() { "flash" } else { "default" },
        "codex_service_request": request_is_codex_service(request),
        "codex_service_kind": service_kind.label(),
        "service_classification_source": service_classification_source(&shape, service_kind),
        "service_signals": shape.service_signals(),
        "thinking_policy": if service_kind.is_service() { "disabled_for_service" } else { "request_or_user_config" },
        "has_previous_response_id": shape.has_previous_response_id,
        "has_instructions": shape.has_instructions,
        "has_context_management": shape.has_context_management,
        "input_items": shape.input_items,
        "input_kind": shape.input_kind,
        "estimated_text_chars": shape.estimated_text_chars,
        "tools_count": shape.tools_count,
        "max_output_tokens": shape.max_output_tokens,
        "reasoning_effort": shape.reasoning_effort,
        "text_format": shape.text_format,
        "store": shape.store,
        "metadata_keys": shape.metadata_keys,
        "client_metadata": shape.client_metadata,
        "prompt_cache_key": shape.prompt_cache_key,
        "has_title_task_signal": shape.has_title_task_signal,
        "has_suggestion_task_signal": shape.has_suggestion_task_signal
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexServiceRequestKind {
    ThreadTitle,
    AmbientSuggestions,
    AmbientSuggestionSafety,
    UnknownService,
    None,
}

impl CodexServiceRequestKind {
    fn from_request(request: &Value) -> Self {
        let shape = RequestShape::from_request(request);
        if let Some(kind) = shape.explicit_service_kind {
            if shape.explicit_service_kind_is_trusted() {
                return kind;
            }
        }
        if !shape.is_service_eligible() {
            return Self::None;
        }
        if shape.has_thread_title_schema_name
            || (shape.has_thread_title_schema_shape && shape.has_direct_service_semantic_signal())
        {
            return Self::ThreadTitle;
        }
        if shape.has_ambient_suggestions_schema_name
            || (shape.has_ambient_suggestions_schema_shape
                && shape.has_direct_service_semantic_signal())
        {
            return Self::AmbientSuggestions;
        }
        if shape.has_ambient_suggestion_safety_schema_name
            || (shape.has_ambient_suggestion_safety_schema_shape
                && shape.has_direct_service_semantic_signal())
        {
            return Self::AmbientSuggestionSafety;
        }
        if shape.looks_structured_service_like() {
            return Self::UnknownService;
        }
        Self::None
    }

    pub(crate) fn is_service(self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ThreadTitle => "thread_title",
            Self::AmbientSuggestions => "ambient_suggestions",
            Self::AmbientSuggestionSafety => "ambient_suggestion_safety",
            Self::UnknownService => "unknown_service",
            Self::None => "none",
        }
    }

}

fn service_classification_source(
    shape: &RequestShape,
    kind: CodexServiceRequestKind,
) -> &'static str {
    if kind == CodexServiceRequestKind::None {
        return "none";
    }
    if shape.explicit_service_kind == Some(kind) && shape.explicit_service_kind_is_trusted() {
        return if shape.codex_request_marker {
            "explicit_codex_feature"
        } else {
            "explicit_service_shape"
        };
    }
    match kind {
        CodexServiceRequestKind::ThreadTitle => "thread_title_schema",
        CodexServiceRequestKind::AmbientSuggestions => "ambient_suggestions_schema",
        CodexServiceRequestKind::AmbientSuggestionSafety => "ambient_suggestion_safety_schema",
        CodexServiceRequestKind::UnknownService => "structured_service_like",
        CodexServiceRequestKind::None => "none",
    }
}

fn explicit_service_kind(request: &Value) -> Option<CodexServiceRequestKind> {
    [
        request.get("feature"),
        request.pointer("/metadata/codex_feature"),
        request.pointer("/metadata/codex_service_feature"),
        request.pointer("/metadata/feature"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find_map(service_kind_from_label)
}

fn service_kind_from_label(value: &str) -> Option<CodexServiceRequestKind> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "thread_title" | "thread-title" | "title" => Some(CodexServiceRequestKind::ThreadTitle),
        "ambient_suggestions" | "ambient-suggestions" => {
            Some(CodexServiceRequestKind::AmbientSuggestions)
        }
        "ambient_suggestion_safety" | "ambient-suggestion-safety" => {
            Some(CodexServiceRequestKind::AmbientSuggestionSafety)
        }
        "codex_service" | "service_ephemeral" | "ephemeral_generation" => {
            Some(CodexServiceRequestKind::UnknownService)
        }
        _ => None,
    }
}

fn service_kind_from_schema_name(value: &str) -> Option<CodexServiceRequestKind> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "thread_title" | "thread-title" => Some(CodexServiceRequestKind::ThreadTitle),
        "ambient_suggestions" | "ambient-suggestions" => {
            Some(CodexServiceRequestKind::AmbientSuggestions)
        }
        "ambient_suggestion_safety" | "ambient-suggestion-safety" => {
            Some(CodexServiceRequestKind::AmbientSuggestionSafety)
        }
        _ => None,
    }
}

#[derive(Debug)]
struct RequestShape {
    has_previous_response_id: bool,
    has_instructions: bool,
    has_context_management: bool,
    input_items: usize,
    input_kind: &'static str,
    estimated_text_chars: usize,
    tools_count: usize,
    max_output_tokens: Option<u64>,
    reasoning_effort: Option<String>,
    text_format: Option<String>,
    store: Option<bool>,
    metadata_keys: Vec<String>,
    client_metadata: bool,
    prompt_cache_key: bool,
    codex_request_marker: bool,
    ephemeral: bool,
    thread_source: Option<String>,
    approval_policy: Option<String>,
    permissions: Option<String>,
    has_title_task_signal: bool,
    has_suggestion_task_signal: bool,
    has_structured_output: bool,
    has_thread_title_schema_name: bool,
    has_thread_title_schema_shape: bool,
    has_ambient_suggestions_schema_name: bool,
    has_ambient_suggestions_schema_shape: bool,
    has_ambient_suggestion_safety_schema_name: bool,
    has_ambient_suggestion_safety_schema_shape: bool,
    has_service_lifecycle_hint: bool,
    explicit_service_kind: Option<CodexServiceRequestKind>,
    schema_name: Option<String>,
}

impl RequestShape {
    fn from_request(request: &Value) -> Self {
        let input = request.get("input").unwrap_or(&Value::Null);
        let instructions = request.get("instructions").unwrap_or(&Value::Null);
        let metadata = request.get("metadata").unwrap_or(&Value::Null);
        let output_schema = output_schema_value(request);
        let schema_name = output_schema_name(request);
        Self {
            has_previous_response_id: request
                .get("previous_response_id")
                .and_then(Value::as_str)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false),
            has_instructions: !instructions.is_null(),
            has_context_management: request.get("context_management").is_some(),
            input_items: input_item_count(input),
            input_kind: value_kind(input),
            estimated_text_chars: estimate_text_chars(input)
                .saturating_add(estimate_text_chars(instructions)),
            tools_count: tools_count(request.get("tools")),
            max_output_tokens: request
                .get("max_output_tokens")
                .or_else(|| request.get("max_completion_tokens"))
                .and_then(value_to_u64),
            reasoning_effort: request
                .pointer("/reasoning/effort")
                .and_then(Value::as_str)
                .map(str::to_owned),
            text_format: request
                .pointer("/text/format/type")
                .and_then(Value::as_str)
                .map(str::to_owned),
            store: request.get("store").and_then(Value::as_bool),
            metadata_keys: object_keys(metadata),
            client_metadata: request.get("client_metadata").is_some(),
            prompt_cache_key: request.get("prompt_cache_key").is_some(),
            codex_request_marker: codex_request_markers(request).has_any(),
            ephemeral: request
                .get("ephemeral")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            thread_source: request
                .get("threadSource")
                .or_else(|| request.get("thread_source"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            approval_policy: request
                .get("approvalPolicy")
                .or_else(|| request.get("approval_policy"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            permissions: request
                .get("permissions")
                .and_then(Value::as_str)
                .map(str::to_owned),
            has_title_task_signal: value_has_title_task_signal(instructions)
                || value_has_title_task_signal(input)
                || value_has_title_task_signal(metadata),
            has_suggestion_task_signal: value_has_suggestion_task_signal(instructions)
                || value_has_suggestion_task_signal(input)
                || value_has_suggestion_task_signal(metadata),
            has_structured_output: output_schema.is_some(),
            has_thread_title_schema_name: schema_name.as_deref().is_some_and(|name| {
                service_kind_from_schema_name(name) == Some(CodexServiceRequestKind::ThreadTitle)
            }),
            has_thread_title_schema_shape: output_schema.is_some_and(schema_has_thread_title_shape),
            has_ambient_suggestions_schema_name: schema_name.as_deref().is_some_and(|name| {
                service_kind_from_schema_name(name)
                    == Some(CodexServiceRequestKind::AmbientSuggestions)
            }),
            has_ambient_suggestions_schema_shape: output_schema
                .is_some_and(schema_has_ambient_suggestions_shape),
            has_ambient_suggestion_safety_schema_name: schema_name.as_deref().is_some_and(|name| {
                service_kind_from_schema_name(name)
                    == Some(CodexServiceRequestKind::AmbientSuggestionSafety)
            }),
            has_ambient_suggestion_safety_schema_shape: output_schema
                .is_some_and(schema_has_ambient_suggestion_safety_shape),
            has_service_lifecycle_hint: value_has_service_lifecycle_hint(metadata)
                || value_has_service_lifecycle_hint(request.get("config").unwrap_or(&Value::Null)),
            explicit_service_kind: explicit_service_kind(request),
            schema_name,
        }
    }

    fn is_service_eligible(&self) -> bool {
        if self.has_previous_response_id || self.has_context_management {
            return false;
        }
        if !self.has_structured_output {
            return false;
        }
        true
    }

    fn has_direct_service_semantic_signal(&self) -> bool {
        self.has_service_lifecycle_signal()
            || self.store == Some(false)
            || self.codex_request_marker
                && (self.has_title_task_signal || self.has_suggestion_task_signal)
    }

    fn explicit_service_kind_is_trusted(&self) -> bool {
        self.codex_request_marker
            || self.is_service_eligible()
                && (self.has_service_lifecycle_signal() || self.store == Some(false))
    }

    fn has_service_lifecycle_signal(&self) -> bool {
        self.has_service_lifecycle_hint
            || self.ephemeral
            || self
                .thread_source
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("system"))
            || self
                .approval_policy
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("never"))
            || self.permissions.as_deref().is_some_and(|value| {
                let normalized = value.trim().to_ascii_lowercase();
                normalized == ":read-only" || normalized == "read-only"
            })
    }

    fn looks_structured_service_like(&self) -> bool {
        self.has_service_lifecycle_signal()
            || self.store == Some(false)
                && (self.has_title_task_signal || self.has_suggestion_task_signal)
    }

    fn service_signals(&self) -> Value {
        json!({
            "structured_output": self.has_structured_output,
            "schema_name": self.schema_name,
            "thread_title_schema_name": self.has_thread_title_schema_name,
            "thread_title_schema_shape": self.has_thread_title_schema_shape,
            "ambient_suggestions_schema_name": self.has_ambient_suggestions_schema_name,
            "ambient_suggestions_schema_shape": self.has_ambient_suggestions_schema_shape,
            "ambient_suggestion_safety_schema_name": self.has_ambient_suggestion_safety_schema_name,
            "ambient_suggestion_safety_schema_shape": self.has_ambient_suggestion_safety_schema_shape,
            "explicit_service_kind": self.explicit_service_kind.map(|kind| kind.label()),
            "explicit_service_kind_trusted": self.explicit_service_kind
                .is_some_and(|_| self.explicit_service_kind_is_trusted()),
            "service_lifecycle_hint": self.has_service_lifecycle_hint,
            "store_false": self.store == Some(false),
            "codex_request_marker": self.codex_request_marker,
            "ephemeral": self.ephemeral,
            "thread_source": self.thread_source,
            "approval_policy": self.approval_policy,
            "permissions": self.permissions,
            "title_text_signal": self.has_title_task_signal,
            "suggestion_text_signal": self.has_suggestion_task_signal
        })
    }
}

fn input_item_count(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Array(items) => items.len(),
        _ => 1,
    }
}

fn output_schema_value(request: &Value) -> Option<&Value> {
    request
        .pointer("/text/format/schema")
        .or_else(|| request.pointer("/text/format/json_schema/schema"))
        .or_else(|| request.pointer("/response_format/json_schema/schema"))
        .or_else(|| request.pointer("/output_schema"))
        .or_else(|| request.pointer("/outputSchema"))
}

fn output_schema_name(request: &Value) -> Option<String> {
    request
        .pointer("/text/format/name")
        .or_else(|| request.pointer("/text/format/json_schema/name"))
        .or_else(|| request.pointer("/response_format/json_schema/name"))
        .or_else(|| request.pointer("/output_schema/name"))
        .or_else(|| request.pointer("/outputSchema/name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn schema_has_thread_title_shape(schema: &Value) -> bool {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return false;
    };
    properties.contains_key("title")
        && schema_required_contains(schema, "title")
        && properties.len() <= 2
}

fn schema_has_ambient_suggestions_shape(schema: &Value) -> bool {
    let Some(suggestions) = schema.pointer("/properties/suggestions") else {
        return false;
    };
    if !schema_required_contains(schema, "suggestions") {
        return false;
    }
    schema_array_items_have_props(suggestions, &["title", "description", "prompt", "appId"])
}

fn schema_has_ambient_suggestion_safety_shape(schema: &Value) -> bool {
    let Some(exclude) = schema.pointer("/properties/exclude") else {
        return false;
    };
    if !schema_required_contains(schema, "exclude") {
        return false;
    }
    schema_array_items_have_props(exclude, &["id", "reason"])
}

fn schema_required_contains(schema: &Value, field: &str) -> bool {
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(field)))
}

fn schema_array_items_have_props(value: &Value, fields: &[&str]) -> bool {
    if value.get("type").and_then(Value::as_str) != Some("array") {
        return false;
    }
    let Some(properties) = value
        .pointer("/items/properties")
        .and_then(Value::as_object)
    else {
        return false;
    };
    fields.iter().all(|field| properties.contains_key(*field))
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn tools_count(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(object)) if !object.is_empty() => 1,
        Some(Value::String(value)) if !value.trim().is_empty() => 1,
        _ => 0,
    }
}

fn object_keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .map(|object| object.keys().take(24).cloned().collect())
        .unwrap_or_default()
}

fn estimate_text_chars(value: &Value) -> usize {
    estimate_text_chars_inner(value, 0, &mut 0)
}

fn estimate_text_chars_inner(value: &Value, depth: usize, scanned: &mut usize) -> usize {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return 0;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let count = text.chars().take(remaining).count();
            *scanned = (*scanned).saturating_add(count);
            count
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .map(|item| estimate_text_chars_inner(item, depth + 1, scanned))
            .sum(),
        Value::Object(object) => object
            .values()
            .take(128)
            .map(|item| estimate_text_chars_inner(item, depth + 1, scanned))
            .sum(),
        _ => 0,
    }
}

fn value_has_title_task_signal(value: &Value) -> bool {
    value_has_title_task_signal_inner(value, 0, &mut 0)
}

fn value_has_title_task_signal_inner(value: &Value, depth: usize, scanned: &mut usize) -> bool {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return false;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let snippet = text.chars().take(remaining).collect::<String>();
            *scanned = (*scanned).saturating_add(snippet.chars().count());
            text_has_title_task_signal(&snippet)
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .any(|item| value_has_title_task_signal_inner(item, depth + 1, scanned)),
        Value::Object(object) => object.iter().take(128).any(|(key, item)| {
            text_has_title_task_signal(key)
                || value_has_title_task_signal_inner(item, depth + 1, scanned)
        }),
        _ => false,
    }
}

fn text_has_title_task_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let title_word = lower.contains("title")
        || lower.contains("headline")
        || lower.contains("rename")
        || text.contains("标题")
        || text.contains("话题")
        || text.contains("命名");
    if !title_word {
        return false;
    }
    lower.contains("conversation")
        || lower.contains("chat")
        || lower.contains("thread")
        || lower.contains("session")
        || lower.contains("transcript")
        || lower.contains("summarize")
        || lower.contains("short")
        || text.contains("对话")
        || text.contains("会话")
        || text.contains("聊天")
        || text.contains("线程")
}

fn value_has_suggestion_task_signal(value: &Value) -> bool {
    value_has_suggestion_task_signal_inner(value, 0, &mut 0)
}

fn value_has_suggestion_task_signal_inner(
    value: &Value,
    depth: usize,
    scanned: &mut usize,
) -> bool {
    if depth > 8 || *scanned >= TEXT_SCAN_CHAR_LIMIT {
        return false;
    }
    match value {
        Value::String(text) => {
            let remaining = TEXT_SCAN_CHAR_LIMIT.saturating_sub(*scanned);
            let snippet = text.chars().take(remaining).collect::<String>();
            *scanned = (*scanned).saturating_add(snippet.chars().count());
            text_has_suggestion_task_signal(&snippet)
        }
        Value::Array(items) => items
            .iter()
            .take(128)
            .any(|item| value_has_suggestion_task_signal_inner(item, depth + 1, scanned)),
        Value::Object(object) => object.iter().take(128).any(|(key, item)| {
            text_has_suggestion_task_signal(key)
                || value_has_suggestion_task_signal_inner(item, depth + 1, scanned)
        }),
        _ => false,
    }
}

fn text_has_suggestion_task_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let suggestion_word = lower.contains("suggest")
        || lower.contains("recommend")
        || lower.contains("suggestion")
        || lower.contains("recommendation")
        || lower.contains("starter")
        || text.contains("建议")
        || text.contains("推荐")
        || text.contains("起始");
    if !suggestion_word {
        return false;
    }
    let prompt_context = lower.contains("prompt")
        || lower.contains("starter")
        || lower.contains("conversation starter")
        || text.contains("提示")
        || text.contains("开场");
    let conversation_context = lower.contains("conversation")
        || lower.contains("chat")
        || lower.contains("thread")
        || lower.contains("topic")
        || text.contains("对话")
        || text.contains("会话")
        || text.contains("聊天")
        || text.contains("话题");
    let workspace_context = lower.contains("workspace")
        || lower.contains("project")
        || text.contains("工作区")
        || text.contains("项目");
    let new_topic_context = lower.contains("new conversation")
        || lower.contains("new chat")
        || lower.contains("new topic")
        || lower.contains("create conversation")
        || lower.contains("start conversation")
        || lower.contains("start a chat")
        || text.contains("新对话")
        || text.contains("新话题")
        || text.contains("新建")
        || text.contains("创建");

    prompt_context
        || (conversation_context && (workspace_context || new_topic_context))
        || (workspace_context && new_topic_context)
}

fn value_has_service_lifecycle_hint(value: &Value) -> bool {
    value_has_service_lifecycle_hint_inner(value, 0)
}

fn value_has_service_lifecycle_hint_inner(value: &Value, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    match value {
        Value::String(text) => text_has_service_lifecycle_hint(text),
        Value::Array(items) => items
            .iter()
            .take(32)
            .any(|item| value_has_service_lifecycle_hint_inner(item, depth + 1)),
        Value::Object(object) => object.iter().take(32).any(|(key, item)| {
            text_has_service_lifecycle_hint(key)
                || value_has_service_lifecycle_hint_inner(item, depth + 1)
        }),
        _ => false,
    }
}

fn text_has_service_lifecycle_hint(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "ephemeral"
            | "service_ephemeral"
            | "codex_service"
            | "ephemeral_generation"
            | "thread_title"
            | "ambient_suggestions"
            | "ambient_suggestion_safety"
            | "system"
            | ":read-only"
            | "read-only"
            | "never"
    )
}

fn response_format_from_request(request: &Value) -> Option<Value> {
    let format_type = request
        .pointer("/text/format/type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match format_type {
        "json_object" | "json_schema" => Some(json!({ "type": "json_object" })),
        _ => None,
    }
}

fn response_format_needs_json_prompt(request: &Value) -> bool {
    request
        .pointer("/text/format/type")
        .or_else(|| request.pointer("/response_format/type"))
        .and_then(Value::as_str)
        .is_some_and(|value| matches!(value, "json_object" | "json_schema"))
}

fn ensure_json_instruction(payload: &mut Value) {
    let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let already_present = messages.iter().any(|message| {
        message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == "system")
            && message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.to_ascii_lowercase().contains("json"))
    });
    if already_present {
        return;
    }
    if let Some(system_message) = messages.iter_mut().find(|message| {
        message
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == "system")
    }) {
        if let Some(content) = system_message.get("content").and_then(Value::as_str) {
            let mut next = content.to_owned();
            if !next.is_empty() {
                next.push('\n');
            }
            next.push_str("Return valid JSON only.");
            system_message["content"] = Value::String(next);
            return;
        }
    }
    messages.insert(
        0,
        json!({
            "role": "system",
            "content": "Return valid JSON only."
        }),
    );
}

fn thinking_from_request(config: &AppConfig, request: &Value) -> Option<Value> {
    if codex_service_request_kind(request).is_service() {
        return Some(json!({ "type": "disabled" }));
    }
    let forced = UserConfig::read_from(&config.config_path())
        .ok()
        .and_then(|user_config| user_config.model.and_then(|model| model.thinking))
        .unwrap_or_else(|| "auto".to_owned())
        .trim()
        .to_ascii_lowercase();
    if forced == "enabled" || forced == "on" {
        return Some(json!({ "type": "enabled" }));
    }
    if forced == "disabled" || forced == "off" {
        return Some(json!({ "type": "disabled" }));
    }
    let effort = request
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if effort == "none" {
        Some(json!({ "type": "disabled" }))
    } else if !effort.is_empty() {
        Some(json!({ "type": "enabled" }))
    } else {
        None
    }
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.max(0.0).floor() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codeseex_core::models::{MODEL_FLASH, MODEL_PRO};

    #[test]
    fn thread_title_schema_request_routes_to_flash() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": [{ "type": "text", "text": "Generate a short conversation title." }],
            "store": false,
            "max_output_tokens": 32,
            "reasoning": { "effort": "low" },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "minLength": 1, "maxLength": 36 }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::ThreadTitle
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
        assert_eq!(
            request_shape_diagnostic(&request)["service_routing"],
            "flash"
        );
        let mut payload = json!({ "model": "gpt-5.4", "stream": false });
        normalize_chat_payload(&config, &request, &mut payload);
        assert_eq!(payload["thinking"], json!({ "type": "disabled" }));
    }

    #[test]
    fn ambient_suggestions_schema_request_routes_to_flash_without_text_limit() {
        let config = AppConfig::default();
        let large_context = "recent project signal ".repeat(1_000);
        let request = json!({
            "model": "gpt-5.4",
            "input": large_context,
            "store": false,
            "max_output_tokens": 4_096,
            "reasoning": { "effort": "medium" },
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "ambient_suggestions",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "suggestions": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "title": { "type": "string" },
                                        "description": { "type": "string" },
                                        "prompt": { "type": "string" },
                                        "appId": { "type": "string" }
                                    },
                                    "required": ["title", "description", "prompt", "appId"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["suggestions"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::AmbientSuggestions
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
        assert_eq!(
            request_shape_diagnostic(&request)["codex_service_request"],
            true
        );
        let mut payload = json!({ "model": "gpt-5.4", "stream": false });
        normalize_chat_payload(&config, &request, &mut payload);
        assert_eq!(payload["thinking"], json!({ "type": "disabled" }));
    }

    #[test]
    fn ambient_suggestion_safety_schema_request_routes_to_flash() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4-mini",
            "input": "Classify ambient suggestion candidates.",
            "store": false,
            "max_output_tokens": 256,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "ambient_suggestion_safety",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "exclude": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "reason": { "type": "string" }
                                    },
                                    "required": ["id", "reason"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["exclude"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::AmbientSuggestionSafety
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4-mini"),
            MODEL_FLASH
        );
    }

    #[test]
    fn service_schema_request_remains_service_even_with_tools_present() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Generate a short conversation title.",
            "store": false,
            "tools": [{
                "type": "function",
                "function": { "name": "should_not_block_service" }
            }],
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::ThreadTitle
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn structured_output_payload_mentions_json_for_upstream_compatibility() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Generate a short conversation title.",
            "store": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });
        let mut payload = json!({
            "model": "gpt-5.4",
            "messages": [{ "role": "user", "content": "Generate a short conversation title." }],
            "stream": false
        });

        normalize_chat_payload(&config, &request, &mut payload);

        let messages = payload["messages"].as_array().expect("messages");
        assert_eq!(messages[0]["role"], "system");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("json"),
            "{payload}"
        );
    }

    #[test]
    fn structured_output_payload_does_not_duplicate_existing_json_instruction() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Return json.",
            "store": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });
        let mut payload = json!({
            "model": "gpt-5.4",
            "messages": [
                { "role": "system", "content": "Return JSON matching the schema." },
                { "role": "user", "content": "Return json." }
            ],
            "stream": false
        });

        normalize_chat_payload(&config, &request, &mut payload);

        let messages = payload["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2, "{payload}");
        assert_eq!(messages[0]["content"], "Return JSON matching the schema.");
    }

    #[test]
    fn ordinary_project_recommendation_is_not_a_service_request() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Recommend improvements for this Rust project.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
        assert_eq!(
            request_shape_diagnostic(&request)["codex_service_request"],
            false
        );
    }

    #[test]
    fn ordinary_structured_title_shape_without_service_semantics_is_not_service() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Return a title for this article as JSON.",
            "max_output_tokens": 128,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::None
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn plain_external_feature_label_does_not_force_service_behavior() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "feature": "thread_title",
            "input": "Update the project title using available tools.",
            "tools": [{
                "type": "function",
                "function": { "name": "apply_patch" }
            }],
            "max_output_tokens": 512
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::None
        );
        assert_eq!(
            request_shape_diagnostic(&request)["service_signals"]["explicit_service_kind"],
            "thread_title"
        );
        assert_eq!(
            request_shape_diagnostic(&request)["service_signals"]["explicit_service_kind_trusted"],
            false
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn codex_marker_trusts_explicit_service_feature() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "feature": "thread_title",
            "client_metadata": { "x-codex-installation-id": "codex-test" },
            "input": "Generate a short conversation title.",
            "max_output_tokens": 32
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::ThreadTitle
        );
        assert_eq!(
            request_shape_diagnostic(&request)["service_classification_source"],
            "explicit_codex_feature"
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn structured_service_shape_trusts_explicit_service_feature() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "feature": "thread_title",
            "input": "Generate a short conversation title.",
            "store": false,
            "max_output_tokens": 32,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::ThreadTitle
        );
        assert_eq!(
            request_shape_diagnostic(&request)["service_classification_source"],
            "explicit_service_shape"
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn service_request_forces_flash_even_with_pro_override() {
        let config = AppConfig {
            model_override: codeseex_core::models::UpstreamModelOverride::Pro,
            ..AppConfig::default()
        };
        let request = json!({
            "model": "gpt-5.4",
            "input": "Generate a short conversation title.",
            "store": false,
            "max_output_tokens": 32,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "thread_title",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" }
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                }
            }
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::ThreadTitle
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn prompt_only_title_request_uses_default_pro_fallback() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Generate a short conversation title for: user asked to test visual tools.",
            "max_output_tokens": 32
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::None
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn non_title_codex_gpt_request_respects_configured_flash_override() {
        let config = AppConfig {
            model_override: codeseex_core::models::UpstreamModelOverride::Flash,
            ..AppConfig::default()
        };
        let request = json!({
            "model": "gpt-5.4",
            "input": "Write a small HTML file.",
            "max_output_tokens": 512
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_FLASH
        );
    }

    #[test]
    fn tool_requests_do_not_downshift_to_flash_even_with_title_text() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4",
            "input": "Update the page title.",
            "max_output_tokens": 32,
            "tools": [{
                "type": "function",
                "function": { "name": "apply_patch" }
            }]
        });

        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4"),
            MODEL_PRO
        );
    }

    #[test]
    fn arbitrary_mini_suffix_is_not_the_primary_classifier() {
        let config = AppConfig::default();
        let request = json!({
            "model": "gpt-5.4-mini",
            "input": "Write a small HTML file.",
            "max_output_tokens": 512
        });

        assert_eq!(
            codex_service_request_kind(&request),
            CodexServiceRequestKind::None
        );
        assert_eq!(
            resolve_upstream_model(&config, &request, "gpt-5.4-mini"),
            MODEL_PRO
        );
    }
}
