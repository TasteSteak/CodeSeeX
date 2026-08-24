//! Read-only observation of a native DeepSeek Responses SSE stream.
//!
//! Native transport must not synthesize Chat-style events, renumber sequence
//! numbers, or append a `[DONE]` sentinel. This module deliberately observes
//! bytes without changing them. It provides only the terminal facts needed for
//! local lifecycle accounting; the future native route remains responsible for
//! any narrowly-scoped response-id mapping.

use codeseex_core::config::WebSearchBackend;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_INSPECTED_SSE_FRAME_BYTES: usize = 256 * 1024;
const MAX_RETAINED_NATIVE_OUTPUT_ITEMS: usize = 128;
const MAX_RETAINED_NATIVE_OUTPUT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeToolPlan {
    pub(crate) tools: Vec<Value>,
    /// True means the request needs a future native local-tool coordinator;
    /// it is not eligible for the initial direct native transport slice.
    pub(crate) requires_local_execution: bool,
    pub(crate) uses_official_web_search: bool,
}

/// A provider output group is immutable protocol data. The coordinator may
/// append one complete, ordered set of outputs to it, but must never split or
/// reorder its calls. This is the boundary proven by the live mixed-tool
/// probe: DeepSeek rejects a partial group with HTTP 400.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NativeToolCallGroup {
    pub(crate) provider_output: Vec<Value>,
    pub(crate) calls: Vec<NativeToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeToolCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) input: String,
    pub(crate) kind: NativeToolCallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeToolCallKind {
    Function,
    Custom,
}

/// Parses only the two provider tool-call item shapes CodeSeeX has verified
/// against DeepSeek Responses. Unknown output items remain provider-owned;
/// unknown/malformed *tool calls* fail closed rather than being dropped.
pub(crate) fn native_tool_call_group_from_response(
    response: &Value,
) -> Result<Option<NativeToolCallGroup>, String> {
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| "Native Responses response did not contain an output array.".to_owned())?;
    let mut call_ids = BTreeSet::new();
    let mut calls = Vec::new();
    for item in output {
        let Some(call) = native_tool_call_from_output_item(item)? else {
            continue;
        };
        if !call_ids.insert(call.call_id.clone()) {
            return Err(
                "Native Responses returned duplicate tool call identifiers in one output group."
                    .to_owned(),
            );
        }
        calls.push(call);
    }
    if calls.is_empty() {
        return Ok(None);
    }
    Ok(Some(NativeToolCallGroup {
        provider_output: output.clone(),
        calls,
    }))
}

/// Builds the exact full-replay continuation for a single provider tool group.
/// The caller supplies output items in the provider call order; every call
/// must have exactly one matching output with its verified native type.
#[allow(dead_code)] // Used by the local-tool native continuation slice.
pub(crate) fn append_complete_native_tool_group(
    authoritative_input: &[Value],
    group: &NativeToolCallGroup,
    output_items: &[Value],
) -> Result<Vec<Value>, String> {
    if output_items.len() != group.calls.len() {
        return Err("Native tool continuation requires one result for every call in the provider output group."
            .to_owned());
    }
    for (call, output) in group.calls.iter().zip(output_items) {
        let expected_type = match call.kind {
            NativeToolCallKind::Function => "function_call_output",
            NativeToolCallKind::Custom => "custom_tool_call_output",
        };
        if output.get("type").and_then(Value::as_str) != Some(expected_type)
            || output.get("call_id").and_then(Value::as_str) != Some(call.call_id.as_str())
            || output.get("output").and_then(Value::as_str).is_none()
        {
            return Err(
                "Native tool continuation output did not exactly match the provider call group."
                    .to_owned(),
            );
        }
    }
    let mut input = Vec::with_capacity(
        authoritative_input.len() + group.provider_output.len() + output_items.len(),
    );
    input.extend_from_slice(authoritative_input);
    input.extend(group.provider_output.iter().cloned());
    input.extend(output_items.iter().cloned());
    Ok(input)
}

#[allow(dead_code)] // Used by the local-tool native continuation slice.
pub(crate) fn native_tool_output_item(call: &NativeToolCall, output: impl Into<String>) -> Value {
    let item_type = match call.kind {
        NativeToolCallKind::Function => "function_call_output",
        NativeToolCallKind::Custom => "custom_tool_call_output",
    };
    json!({
        "type": item_type,
        "call_id": call.call_id,
        "output": output.into()
    })
}

fn native_tool_call_from_output_item(item: &Value) -> Result<Option<NativeToolCall>, String> {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let (kind, input_field) = match item_type {
        "function_call" => (NativeToolCallKind::Function, "arguments"),
        "custom_tool_call" => (NativeToolCallKind::Custom, "input"),
        _ => return Ok(None),
    };
    let call_id = item
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Native tool call did not contain a call identifier.".to_owned())?;
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Native tool call did not contain a tool name.".to_owned())?;
    if kind == NativeToolCallKind::Custom && name != "apply_patch" {
        return Err("Native Responses returned an unsupported custom tool call.".to_owned());
    }
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Err("Native tool call was not completed in the provider response.".to_owned());
    }
    let input = item
        .get(input_field)
        .and_then(Value::as_str)
        .ok_or_else(|| "Native tool call did not contain its verified input field.".to_owned())?;
    Ok(Some(NativeToolCall {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        input: input.to_owned(),
        kind,
    }))
}

/// Converts the existing, Chat-shaped tool definitions into native Responses
/// definitions without deciding how calls are executed. In particular, this
/// keeps CodeSeeX local search and DeepSeek official search mutually exclusive.
pub(crate) fn plan_native_tools(
    chat_tool_definitions: &[Value],
    web_search_backend: WebSearchBackend,
) -> Result<NativeToolPlan, String> {
    let mut tools = Vec::new();
    let mut names = BTreeSet::new();
    let mut requires_local_execution = false;
    let mut saw_local_web_search = false;
    let mut saw_provider_web_search = false;

    for definition in chat_tool_definitions {
        if is_provider_web_search_definition(definition) {
            saw_provider_web_search = true;
            if web_search_backend == WebSearchBackend::Official {
                // Normalize to the one provider-owned definition appended
                // below. This keeps a client-provided native declaration from
                // creating a duplicate official search capability.
                continue;
            }
            // Codex can advertise its provider-native web-search declaration
            // even when the user selected CodeSeeX local search. It is safe to
            // consume that declaration only if the local function definition
            // is also present in this request. The later validation refuses a
            // missing local function instead of silently switching backend.
            continue;
        }
        let name = tool_name(definition).ok_or_else(|| {
            "A native Responses tool definition is missing a callable name; CodeSeeX did not silently drop it."
                .to_owned()
        })?;
        if matches!(name, "web_search" | "web_search_preview") {
            saw_local_web_search = true;
            if web_search_backend == WebSearchBackend::Official {
                continue;
            }
            requires_local_execution = true;
        } else if is_codeseex_local_tool(name) {
            requires_local_execution = true;
        }
        let native = native_definition_from_chat(definition, name)?;
        if names.insert(tool_identity(&native).to_owned()) {
            tools.push(native);
        }
    }

    let uses_official_web_search = web_search_backend == WebSearchBackend::Official
        && (saw_provider_web_search || saw_local_web_search);
    if uses_official_web_search {
        // `web_search` is provider-owned in this mode. It replaces the
        // CodeSeeX function with the schema verified against DeepSeek's
        // Responses endpoint; it never coexists with the local function.
        if names.insert("web_search".to_owned()) {
            tools.push(json!({ "type": "web_search" }));
        }
    } else if saw_provider_web_search && !saw_local_web_search {
        return Err(
            "Provider-native web_search was requested while CodeSeeX local web_search is selected, but the local web_search function is unavailable. CodeSeeX will not silently switch to provider search."
                .to_owned(),
        );
    } else if saw_local_web_search {
        // The boolean documents the intended ownership in diagnostics and
        // keeps the local backend explicit even when it is the default.
        requires_local_execution = true;
    }

    Ok(NativeToolPlan {
        tools,
        requires_local_execution,
        uses_official_web_search,
    })
}

/// Rewrites only the provider response identity at the local lifecycle
/// boundary. Output-item ids and tool call ids have a different scope and must
/// remain untouched so later full replay stays valid.
pub(crate) fn rewrite_provider_response_identity(
    payload: &mut Value,
    provider_response_id: &str,
    local_response_id: &str,
) -> bool {
    if provider_response_id == local_response_id {
        return false;
    }
    let mut changed = false;
    if payload
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    if payload
        .pointer("/response/id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["response"]["id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    if payload
        .get("response_id")
        .and_then(Value::as_str)
        .is_some_and(|value| value == provider_response_id)
    {
        payload["response_id"] = Value::String(local_response_id.to_owned());
        changed = true;
    }
    changed
}

fn tool_name(definition: &Value) -> Option<&str> {
    definition
        .pointer("/function/name")
        .and_then(Value::as_str)
        .or_else(|| definition.get("name").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

fn native_definition_from_chat(definition: &Value, name: &str) -> Result<Value, String> {
    if name == "apply_patch" {
        let description = definition
            .pointer("/function/description")
            .or_else(|| definition.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("Apply one complete native apply_patch document.");
        return Ok(json!({
            "type": "custom",
            "name": "apply_patch",
            "description": description
        }));
    }

    let declared_type = definition
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(declared_type, "web_search" | "web_search_2025_08_26") {
        return Ok(json!({ "type": "web_search" }));
    }
    if declared_type != "function" {
        return Err(format!(
            "Native Responses cannot safely translate tool '{name}' with type '{declared_type}'."
        ));
    }
    let function = definition.get("function").unwrap_or(definition);
    let description = function
        .get("description")
        .or_else(|| definition.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let parameters = function
        .get("parameters")
        .or_else(|| function.get("input_schema"))
        .or_else(|| definition.get("parameters"))
        .or_else(|| definition.get("input_schema"))
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    let mut native = json!({
        "type": "function",
        "name": name,
        "description": description,
        "parameters": parameters
    });
    if let Some(strict) = function
        .get("strict")
        .or_else(|| definition.get("strict"))
        .and_then(Value::as_bool)
    {
        native["strict"] = Value::Bool(strict);
    }
    Ok(native)
}

fn is_provider_web_search_definition(definition: &Value) -> bool {
    matches!(
        definition.get("type").and_then(Value::as_str),
        Some("web_search" | "web_search_2025_08_26")
    )
}

fn tool_identity(definition: &Value) -> &str {
    definition
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| definition.pointer("/function/name").and_then(Value::as_str))
        .unwrap_or_default()
}

fn is_codeseex_local_tool(name: &str) -> bool {
    matches!(
        name,
        "web_search"
            | "list_directory"
            | "read_file_range"
            | "workspace_search"
            | "vision_analyze"
            | "image_gen"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeResponseTerminal {
    Completed,
    Failed,
    Incomplete,
}

/// Local lifecycle result selected only after the raw upstream stream has
/// closed. A provider terminal event wins over a late local cancellation; an
/// unterminated stream is never treated as completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeStreamFinalization {
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NativeResponseStreamInspection {
    pub(crate) terminal: Option<NativeResponseTerminal>,
    pub(crate) final_usage: Option<Value>,
    pub(crate) provider_response_id_hash: Option<String>,
    pub(crate) event_count: usize,
    pub(crate) sequence_count: usize,
    pub(crate) sequences_strictly_increasing: bool,
    pub(crate) saw_done_sentinel: bool,
    pub(crate) oversized_frame_ignored: bool,
    /// Raw provider output items are retained only in RAM until the stream
    /// ends. They are needed to register an exact client-tool continuation;
    /// no item content is put into diagnostics.
    pub(crate) output_items: Vec<Value>,
    pub(crate) output_items_bytes: usize,
    pub(crate) output_items_incomplete: bool,
}

pub(crate) fn native_stream_finalization(
    inspection: &NativeResponseStreamInspection,
    locally_cancelled: bool,
) -> NativeStreamFinalization {
    match inspection.terminal {
        Some(NativeResponseTerminal::Completed) => NativeStreamFinalization::Completed,
        Some(NativeResponseTerminal::Failed | NativeResponseTerminal::Incomplete) => {
            NativeStreamFinalization::Failed
        }
        None if locally_cancelled => NativeStreamFinalization::Interrupted,
        None => NativeStreamFinalization::Failed,
    }
}

/// Bounded SSE parser for side-channel accounting. `observe_bytes` never
/// returns transformed data and callers must forward their original bytes.
#[derive(Debug, Default)]
pub(crate) struct NativeResponseSseInspector {
    buffer: Vec<u8>,
    last_sequence: Option<u64>,
    inspection: NativeResponseStreamInspection,
}

impl NativeResponseSseInspector {
    pub(crate) fn observe_bytes(&mut self, bytes: &[u8]) {
        if self.inspection.oversized_frame_ignored {
            return;
        }
        self.buffer.extend_from_slice(bytes);
        while let Some((index, delimiter_len)) = find_sse_frame_delimiter(&self.buffer) {
            if index > MAX_INSPECTED_SSE_FRAME_BYTES {
                self.buffer.drain(..index + delimiter_len);
                self.inspection.oversized_frame_ignored = true;
                self.inspection.output_items_incomplete = true;
                self.buffer.clear();
                break;
            }
            let frame = self.buffer.drain(..index).collect::<Vec<_>>();
            self.buffer.drain(..delimiter_len);
            self.inspect_frame(&frame);
        }
        if self.buffer.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            self.buffer.clear();
            self.inspection.oversized_frame_ignored = true;
            self.inspection.output_items_incomplete = true;
        }
    }

    /// Call once when the upstream stream closes. A final event is permitted
    /// to omit the trailing blank line, so it is inspected if still bounded.
    pub(crate) fn finish(&mut self) {
        if !self.inspection.oversized_frame_ignored && !self.buffer.is_empty() {
            let frame = std::mem::take(&mut self.buffer);
            self.inspect_frame(&frame);
        }
    }

    pub(crate) fn inspection(&self) -> &NativeResponseStreamInspection {
        &self.inspection
    }

    fn inspect_frame(&mut self, frame: &[u8]) {
        if frame.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            self.inspection.oversized_frame_ignored = true;
            self.inspection.output_items_incomplete = true;
            return;
        }
        let frame = match std::str::from_utf8(frame) {
            Ok(frame) => frame,
            Err(_) => {
                self.inspection.output_items_incomplete = true;
                return;
            }
        };
        let event_name = frame
            .lines()
            .find_map(|line| line.strip_prefix("event:").map(str::trim));
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let data = data.trim();
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.inspection.saw_done_sentinel = true;
            return;
        }
        let payload = match serde_json::from_str::<Value>(data) {
            Ok(payload) => payload,
            Err(_) => {
                self.inspection.output_items_incomplete = true;
                return;
            }
        };
        self.inspection.event_count += 1;
        if let Some(sequence) = payload.get("sequence_number").and_then(Value::as_u64) {
            self.inspection.sequence_count += 1;
            if self
                .last_sequence
                .is_some_and(|previous| sequence <= previous)
            {
                self.inspection.sequences_strictly_increasing = false;
            } else if self.inspection.sequence_count == 1 {
                self.inspection.sequences_strictly_increasing = true;
            }
            self.last_sequence = Some(sequence);
        }
        if let Some(provider_id) = response_id_from_event(&payload) {
            self.inspection.provider_response_id_hash = Some(hash_identifier(provider_id));
        }
        if let Some(usage) = payload
            .pointer("/response/usage")
            .or_else(|| payload.get("usage"))
        {
            // Responses events contain snapshots, not deltas. Replacing is
            // therefore intentional and prevents token/cost multiplication.
            self.inspection.final_usage = Some(usage.clone());
        }
        let event_kind = event_name
            .or_else(|| payload.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        if event_kind == "response.output_item.done" {
            match payload.get("item") {
                Some(item) => self.retain_output_item(item),
                None => self.inspection.output_items_incomplete = true,
            }
        } else if event_kind == "response.completed"
            && self.inspection.output_items.is_empty()
            && !self.inspection.output_items_incomplete
        {
            // Some compatible providers include the complete output only in
            // the terminal response. Accept that equivalent form, but never
            // merge it with already observed item-done events.
            if let Some(items) = payload
                .pointer("/response/output")
                .and_then(Value::as_array)
            {
                for item in items {
                    self.retain_output_item(item);
                }
            }
        }
        self.inspection.terminal = match event_kind {
            "response.completed" => Some(NativeResponseTerminal::Completed),
            "response.failed" => Some(NativeResponseTerminal::Failed),
            "response.incomplete" => Some(NativeResponseTerminal::Incomplete),
            _ => self.inspection.terminal,
        };
    }

    fn retain_output_item(&mut self, item: &Value) {
        if self.inspection.output_items_incomplete {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(item) else {
            self.inspection.output_items_incomplete = true;
            return;
        };
        if self.inspection.output_items.len() >= MAX_RETAINED_NATIVE_OUTPUT_ITEMS
            || self
                .inspection
                .output_items_bytes
                .saturating_add(bytes.len())
                > MAX_RETAINED_NATIVE_OUTPUT_BYTES
        {
            self.inspection.output_items_incomplete = true;
            return;
        }
        self.inspection.output_items_bytes += bytes.len();
        self.inspection.output_items.push(item.clone());
    }
}

/// Streaming counterpart to the inspector. It buffers only up to a complete
/// SSE event, observes the untouched upstream event, and rewrites the narrow
/// response-id boundary when one is present. All other event bytes, including
/// sequence numbers and tool/output ids, pass through unchanged.
#[derive(Debug)]
pub(crate) struct NativeResponseSseRelay {
    buffer: Vec<u8>,
    inspector: NativeResponseSseInspector,
    provider_response_id: Option<String>,
    local_response_id: String,
}

impl NativeResponseSseRelay {
    pub(crate) fn new(local_response_id: impl Into<String>) -> Self {
        Self {
            buffer: Vec::new(),
            inspector: NativeResponseSseInspector::default(),
            provider_response_id: None,
            local_response_id: local_response_id.into(),
        }
    }

    pub(crate) fn relay_bytes(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        if self.inspector.inspection().oversized_frame_ignored {
            return Vec::new();
        }
        let mut ready = Vec::new();
        self.buffer.extend_from_slice(bytes);
        while let Some((index, delimiter_len)) = find_sse_frame_delimiter(&self.buffer) {
            if index > MAX_INSPECTED_SSE_FRAME_BYTES {
                self.buffer.drain(..index + delimiter_len);
                self.inspector.inspection.oversized_frame_ignored = true;
                self.inspector.inspection.output_items_incomplete = true;
                self.buffer.clear();
                break;
            }
            let frame = self.buffer.drain(..index).collect::<Vec<_>>();
            let delimiter = self.buffer.drain(..delimiter_len).collect::<Vec<_>>();
            self.inspector.observe_bytes(&frame);
            self.inspector.observe_bytes(&delimiter);
            ready.push(self.relay_frame(frame, delimiter));
        }
        // Do not hold an unterminated upstream event unboundedly. It cannot be
        // inspected or safely mapped. Drop it instead of forwarding provider
        // response identity or an unverified tool payload to Codex.
        if self.buffer.len() > MAX_INSPECTED_SSE_FRAME_BYTES {
            self.inspector.observe_bytes(&self.buffer);
            self.inspector.inspection.oversized_frame_ignored = true;
            self.inspector.inspection.output_items_incomplete = true;
            self.buffer.clear();
        }
        ready
    }

    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let remainder = (!self.buffer.is_empty()).then(|| std::mem::take(&mut self.buffer));
        if let Some(bytes) = remainder.as_ref() {
            self.inspector.observe_bytes(bytes);
        }
        self.inspector.finish();
        // A bounded terminal SSE event may omit its trailing blank line. It
        // can still carry a provider response id, so apply the same narrow
        // identity rewrite before forwarding it.
        remainder.map(|frame| self.relay_frame(frame, Vec::new()))
    }

    pub(crate) fn inspection(&self) -> &NativeResponseStreamInspection {
        self.inspector.inspection()
    }

    fn relay_frame(&mut self, frame: Vec<u8>, delimiter: Vec<u8>) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(&frame) else {
            return append_delimiter(frame, &delimiter);
        };
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(mut payload) = serde_json::from_str::<Value>(data.trim()) else {
            return append_delimiter(frame, &delimiter);
        };
        if self.provider_response_id.is_none() {
            self.provider_response_id = response_id_from_event(&payload).map(str::to_owned);
        }
        let Some(provider_response_id) = self.provider_response_id.as_deref() else {
            return append_delimiter(frame, &delimiter);
        };
        if !rewrite_provider_response_identity(
            &mut payload,
            provider_response_id,
            &self.local_response_id,
        ) {
            return append_delimiter(frame, &delimiter);
        }
        let serialized = serde_json::to_string(&payload).unwrap_or_else(|_| data.trim().to_owned());
        append_delimiter(rewrite_sse_data_lines(text, &serialized), &delimiter)
    }
}

/// Preserve every non-data SSE field byte-for-byte (`event`, `id`, `retry`,
/// comments, extensions and original line endings). The response-id boundary
/// only requires changing JSON carried by `data:`; rebuilding an entire event
/// would accidentally change SSE resume and reconnect behaviour.
fn rewrite_sse_data_lines(frame: &str, serialized_data: &str) -> Vec<u8> {
    let mut rendered = String::with_capacity(frame.len());
    let mut replaced = false;
    for raw_line in frame.split_inclusive('\n') {
        let (line, ending) = if let Some(line) = raw_line.strip_suffix("\r\n") {
            (line, "\r\n")
        } else if let Some(line) = raw_line.strip_suffix('\n') {
            (line, "\n")
        } else {
            (raw_line, "")
        };
        if line.strip_prefix("data:").is_some() {
            if !replaced {
                rendered.push_str("data: ");
                rendered.push_str(serialized_data);
                rendered.push_str(ending);
                replaced = true;
            }
            continue;
        }
        rendered.push_str(raw_line);
    }
    rendered.into_bytes()
}

fn append_delimiter(mut frame: Vec<u8>, delimiter: &[u8]) -> Vec<u8> {
    frame.extend_from_slice(delimiter);
    frame
}

fn response_id_from_event(payload: &Value) -> Option<&str> {
    payload
        .pointer("/response/id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("response_id").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
}

fn hash_identifier(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{:x}", digest)
}

fn find_sse_frame_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .enumerate()
        .find_map(|(index, window)| (window == b"\n\n").then_some((index, 2)));
    let crlf = buffer
        .windows(4)
        .enumerate()
        .find_map(|(index, window)| (window == b"\r\n\r\n").then_some((index, 4)));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_function(name: &str) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": format!("{name} description"),
                "parameters": { "type": "object", "properties": {} }
            }
        })
    }

    #[test]
    fn local_web_search_stays_local_and_is_never_replaced_by_provider_search() {
        let plan = plan_native_tools(
            &[
                chat_function("web_search"),
                chat_function("workspace_search"),
            ],
            WebSearchBackend::Local,
        )
        .unwrap();

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(plan.tools.len(), 2);
        assert_eq!(plan.tools[0]["name"], "web_search");
        assert!(plan.tools.iter().all(|tool| tool["type"] != "web_search"));
    }

    #[test]
    fn official_web_search_replaces_only_the_local_web_function_without_double_dispatch() {
        let plan = plan_native_tools(
            &[
                chat_function("web_search"),
                chat_function("workspace_search"),
                chat_function("external_lookup"),
            ],
            WebSearchBackend::Official,
        )
        .unwrap();

        assert!(plan.requires_local_execution);
        assert!(plan.uses_official_web_search);
        assert_eq!(
            plan.tools
                .iter()
                .filter(|tool| tool["type"] == "web_search")
                .count(),
            1
        );
        assert!(!plan.tools.iter().any(|tool| tool["name"] == "web_search"));
        assert!(plan
            .tools
            .iter()
            .any(|tool| tool["name"] == "workspace_search"));
        assert!(plan
            .tools
            .iter()
            .any(|tool| tool["name"] == "external_lookup"));
    }

    #[test]
    fn official_web_search_also_replaces_the_preview_alias() {
        let plan = plan_native_tools(
            &[chat_function("web_search_preview")],
            WebSearchBackend::Official,
        )
        .unwrap();

        assert!(!plan.requires_local_execution);
        assert!(plan.uses_official_web_search);
        assert_eq!(plan.tools, vec![json!({ "type": "web_search" })]);
    }

    #[test]
    fn official_backend_without_a_web_search_request_does_not_claim_search_ownership() {
        let plan = plan_native_tools(
            &[chat_function("workspace_search")],
            WebSearchBackend::Official,
        )
        .unwrap();

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(plan.tools[0]["name"], "workspace_search");
    }

    #[test]
    fn provider_native_web_search_maps_to_the_existing_local_function() {
        let plan = plan_native_tools(
            &[chat_function("web_search"), json!({ "type": "web_search" })],
            WebSearchBackend::Local,
        )
        .unwrap();

        assert!(plan.requires_local_execution);
        assert!(!plan.uses_official_web_search);
        assert_eq!(
            plan.tools
                .iter()
                .filter(|tool| tool["name"] == "web_search")
                .count(),
            1
        );
        assert!(plan.tools.iter().all(|tool| tool["type"] != "web_search"));
    }

    #[test]
    fn provider_native_web_search_is_rejected_when_local_function_is_unavailable() {
        let error = plan_native_tools(&[json!({ "type": "web_search" })], WebSearchBackend::Local)
            .unwrap_err();

        assert!(error.contains("local web_search function is unavailable"));
    }

    #[test]
    fn apply_patch_is_converted_to_provider_custom_schema_without_parameter_wrapper() {
        let plan =
            plan_native_tools(&[chat_function("apply_patch")], WebSearchBackend::Local).unwrap();

        assert!(!plan.requires_local_execution);
        assert_eq!(
            plan.tools,
            vec![json!({
                "type": "custom",
                "name": "apply_patch",
                "description": "apply_patch description"
            })]
        );
    }

    #[test]
    fn parses_the_verified_function_and_custom_tool_item_shapes_without_reordering() {
        let function = json!({
            "id": "fc_item",
            "type": "function_call",
            "status": "completed",
            "call_id": "call_function",
            "name": "workspace_search",
            "arguments": "{\"query\":\"needle\"}"
        });
        let custom = json!({
            "id": "ctc_item",
            "type": "custom_tool_call",
            "status": "completed",
            "call_id": "call_patch",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch"
        });
        let response = json!({
            "output": [
                { "id": "reasoning_1", "type": "reasoning", "status": "completed" },
                function,
                custom
            ]
        });

        let group = native_tool_call_group_from_response(&response)
            .unwrap()
            .expect("tool group");

        assert_eq!(
            group.provider_output,
            response["output"].as_array().expect("output array").clone()
        );
        assert_eq!(group.calls.len(), 2);
        assert_eq!(group.calls[0].call_id, "call_function");
        assert_eq!(group.calls[0].name, "workspace_search");
        assert_eq!(group.calls[0].input, r#"{"query":"needle"}"#);
        assert_eq!(group.calls[0].kind, NativeToolCallKind::Function);
        assert_eq!(group.calls[1].call_id, "call_patch");
        assert_eq!(group.calls[1].input, "*** Begin Patch\n*** End Patch");
        assert_eq!(group.calls[1].kind, NativeToolCallKind::Custom);
    }

    #[test]
    fn native_tool_continuation_requires_the_complete_provider_call_group_in_order() {
        let response = json!({
            "output": [
                {
                    "id": "fc_item",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_function",
                    "name": "workspace_search",
                    "arguments": "{}"
                },
                {
                    "id": "ctc_item",
                    "type": "custom_tool_call",
                    "status": "completed",
                    "call_id": "call_patch",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** End Patch"
                }
            ]
        });
        let group = native_tool_call_group_from_response(&response)
            .unwrap()
            .expect("tool group");
        let outputs = group
            .calls
            .iter()
            .map(|call| native_tool_output_item(call, format!("result: {}", call.name)))
            .collect::<Vec<_>>();
        let prefix = vec![json!({ "type": "message", "role": "user", "content": [] })];

        let continuation = append_complete_native_tool_group(&prefix, &group, &outputs).unwrap();
        assert_eq!(continuation[0], prefix[0]);
        assert_eq!(continuation[1], response["output"][0]);
        assert_eq!(continuation[2], response["output"][1]);
        assert_eq!(continuation[3]["type"], "function_call_output");
        assert_eq!(continuation[4]["type"], "custom_tool_call_output");

        assert!(append_complete_native_tool_group(&prefix, &group, &outputs[..1]).is_err());
        assert!(append_complete_native_tool_group(
            &prefix,
            &group,
            &[outputs[1].clone(), outputs[0].clone()]
        )
        .is_err());
        assert!(append_complete_native_tool_group(
            &prefix,
            &group,
            &[
                json!({ "type": "function_call_output", "call_id": "call_function", "output": "ok" }),
                json!({ "type": "function_call_output", "call_id": "call_patch", "output": "wrong kind" })
            ]
        )
        .is_err());
    }

    #[test]
    fn provider_owned_web_search_does_not_create_a_local_pending_tool_group() {
        let response = json!({
            "output": [{
                "id": "ws_item",
                "type": "web_search_call",
                "status": "completed"
            }]
        });

        assert_eq!(
            native_tool_call_group_from_response(&response).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_or_unsupported_native_tool_calls_fail_closed() {
        let missing_result_input = json!({
            "output": [{
                "type": "function_call",
                "status": "completed",
                "call_id": "call_function",
                "name": "workspace_search"
            }]
        });
        assert!(native_tool_call_group_from_response(&missing_result_input).is_err());

        let unsupported_custom = json!({
            "output": [{
                "type": "custom_tool_call",
                "status": "completed",
                "call_id": "call_custom",
                "name": "other_custom",
                "input": "unsafe"
            }]
        });
        assert!(native_tool_call_group_from_response(&unsupported_custom).is_err());
    }

    #[test]
    fn unsafe_or_unknown_tool_shapes_fail_closed_instead_of_being_dropped() {
        let error = plan_native_tools(
            &[json!({ "type": "computer_use", "name": "computer" })],
            WebSearchBackend::Local,
        )
        .unwrap_err();

        assert!(error.contains("cannot safely translate"));
    }

    #[test]
    fn response_identity_mapping_never_rewrites_tool_or_output_item_ids() {
        let mut payload = json!({
            "type": "response.output_item.done",
            "response_id": "resp_provider",
            "response": { "id": "resp_provider", "previous_response_id": null },
            "item": { "id": "msg_provider", "call_id": "call_provider" }
        });

        assert!(rewrite_provider_response_identity(
            &mut payload,
            "resp_provider",
            "resp_local"
        ));
        assert_eq!(payload["response_id"], "resp_local");
        assert_eq!(payload["response"]["id"], "resp_local");
        assert_eq!(payload["item"]["id"], "msg_provider");
        assert_eq!(payload["item"]["call_id"], "call_provider");
        assert!(!rewrite_provider_response_identity(
            &mut payload,
            "resp_provider",
            "resp_local"
        ));
    }

    #[test]
    fn relay_only_rewrites_response_identity_and_keeps_native_sse_protocol_facts() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let first = concat!(
            ": keepalive\r\n",
            "id: upstream-event\r\n",
            "retry: 1500\r\n",
            "event: response.created\r\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,",
            "\"response\":{\"id\":\"resp_provider\"}}\r\n\r\n"
        );
        let second = concat!(
            "event: response.output_item.done\r\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,",
            "\"response_id\":\"resp_provider\",\"item\":{\"id\":\"msg_provider\",\"call_id\":\"call_provider\"}}\r\n\r\n"
        );

        assert!(relay.relay_bytes(&first.as_bytes()[..31]).is_empty());
        let mut ready = relay.relay_bytes(&first.as_bytes()[31..]);
        ready.extend(relay.relay_bytes(second.as_bytes()));
        assert_eq!(ready.len(), 2);
        assert!(!ready
            .iter()
            .any(|frame| frame.windows(6).any(|part| part == b"[DONE]")));

        let first_body = String::from_utf8(ready.remove(0)).unwrap();
        assert!(first_body.contains("\"id\":\"resp_local\""));
        assert!(first_body.contains("\"sequence_number\":1"));
        assert!(first_body.contains(": keepalive\r\n"));
        assert!(first_body.contains("id: upstream-event\r\n"));
        assert!(first_body.contains("retry: 1500\r\n"));
        assert!(first_body.contains("event: response.created\r\n"));
        assert!(first_body.ends_with("\r\n\r\n"));
        let second_body = String::from_utf8(ready.remove(0)).unwrap();
        assert!(second_body.contains("\"response_id\":\"resp_local\""));
        assert!(second_body.contains("\"id\":\"msg_provider\""));
        assert!(second_body.contains("\"call_id\":\"call_provider\""));
        assert!(second_body.contains("\"sequence_number\":2"));
        assert_eq!(relay.inspection().sequence_count, 2);
        assert!(relay.inspection().sequences_strictly_increasing);
    }

    #[test]
    fn observes_fragmented_native_sse_without_rewriting_or_accumulating_usage() {
        let mut inspector = NativeResponseSseInspector::default();
        let first = concat!(
            "event: response.created\r\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":1,",
            "\"response\":{\"id\":\"resp_provider\",\"usage\":{\"input_tokens\":5}}}\r\n\r\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"response_id\":\"resp_provider\"}\n\n"
        );
        let terminal = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":3,",
            "\"response\":{\"id\":\"resp_provider\",\"usage\":{\"input_tokens\":9,\"output_tokens\":2}}}\n\n"
        );

        // Splitting in the middle of JSON mimics reqwest chunk boundaries.
        inspector.observe_bytes(&first.as_bytes()[..79]);
        inspector.observe_bytes(&first.as_bytes()[79..]);
        inspector.observe_bytes(terminal.as_bytes());
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.terminal, Some(NativeResponseTerminal::Completed));
        assert_eq!(observed.event_count, 3);
        assert_eq!(observed.sequence_count, 3);
        assert!(observed.sequences_strictly_increasing);
        assert_eq!(observed.final_usage.as_ref().unwrap()["input_tokens"], 9);
        assert_eq!(observed.final_usage.as_ref().unwrap()["output_tokens"], 2);
        assert_eq!(
            observed.provider_response_id_hash.as_deref(),
            Some("5241e1b55519ba7c41cabb46af6f8692b82bfd5cd2a2665c8341434994a272d9")
        );
        assert!(!observed.saw_done_sentinel);
    }

    #[test]
    fn retains_completed_output_items_in_provider_event_order_for_tool_continuation() {
        let mut inspector = NativeResponseSseInspector::default();
        let stream = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":1,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"status\":\"completed\"}}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"sequence_number\":2,\"item\":{\"type\":\"function_call\",\"call_id\":\"call_shell\",\"name\":\"shell_command\",\"arguments\":\"{}\",\"status\":\"completed\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"resp_provider\",\"status\":\"completed\"}}\n\n"
        );
        inspector.observe_bytes(stream.as_bytes());
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.output_items.len(), 2);
        assert_eq!(observed.output_items[0]["type"], "reasoning");
        assert_eq!(observed.output_items[1]["call_id"], "call_shell");
        assert!(observed.output_items_bytes > 0);
        assert!(!observed.output_items_incomplete);
    }

    #[test]
    fn detects_non_monotonic_sequence_and_terminal_failure_without_throwing() {
        let mut inspector = NativeResponseSseInspector::default();
        inspector.observe_bytes(
            br#"event: response.created
data: {"type":"response.created","sequence_number":3,"response_id":"resp_provider"}

event: response.failed
data: {"type":"response.failed","sequence_number":3,"response_id":"resp_provider"}

"#,
        );
        inspector.finish();

        let observed = inspector.inspection();
        assert_eq!(observed.terminal, Some(NativeResponseTerminal::Failed));
        assert_eq!(observed.sequence_count, 2);
        assert!(!observed.sequences_strictly_increasing);
    }

    #[test]
    fn cancelled_or_unterminated_native_stream_is_never_finalized_as_completed() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        relay.relay_bytes(
            br#"event: response.created
data: {"type":"response.created","sequence_number":1,"response":{"id":"resp_provider"}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":2,"response_id":"resp_provider","delta":"partial"}
"#,
        );

        let trailing = relay.finish().expect("unterminated final SSE event");
        assert!(trailing.starts_with(b"event: response.output_text.delta"));
        assert_eq!(relay.inspection().terminal, None);
        assert_eq!(
            native_stream_finalization(relay.inspection(), true),
            NativeStreamFinalization::Interrupted
        );
        assert_eq!(
            native_stream_finalization(relay.inspection(), false),
            NativeStreamFinalization::Failed
        );

        let completed = NativeResponseStreamInspection {
            terminal: Some(NativeResponseTerminal::Completed),
            ..NativeResponseStreamInspection::default()
        };
        assert_eq!(
            native_stream_finalization(&completed, true),
            NativeStreamFinalization::Completed
        );
        let incomplete = NativeResponseStreamInspection {
            terminal: Some(NativeResponseTerminal::Incomplete),
            ..NativeResponseStreamInspection::default()
        };
        assert_eq!(
            native_stream_finalization(&incomplete, false),
            NativeStreamFinalization::Failed
        );
    }

    #[test]
    fn unterminated_terminal_frame_still_rewrites_provider_response_identity() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        relay.relay_bytes(
            br#"event: response.created
data: {"type":"response.created","response":{"id":"resp_provider"}}

"#,
        );
        relay.relay_bytes(
            br#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_provider","status":"completed"}}"#,
        );

        let trailing =
            String::from_utf8(relay.finish().expect("unterminated terminal frame")).unwrap();
        assert!(trailing.contains("resp_local"));
        assert!(!trailing.contains("resp_provider"));
        assert_eq!(
            relay.inspection().terminal,
            Some(NativeResponseTerminal::Completed)
        );
    }

    #[test]
    fn done_sentinel_and_oversized_unframed_data_are_observation_only() {
        let mut inspector = NativeResponseSseInspector::default();
        inspector.observe_bytes(b"data: [DONE]\n\n");
        assert!(inspector.inspection().saw_done_sentinel);

        inspector.observe_bytes(&vec![b'x'; MAX_INSPECTED_SSE_FRAME_BYTES + 1]);
        inspector.finish();
        assert!(inspector.inspection().oversized_frame_ignored);
        assert!(inspector.inspection().output_items_incomplete);
    }

    #[test]
    fn relay_drops_oversized_unterminated_frame_instead_of_leaking_provider_id() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let mut oversized = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_provider\"},\"padding\":\"".to_vec();
        oversized.extend(std::iter::repeat_n(
            b'x',
            MAX_INSPECTED_SSE_FRAME_BYTES.saturating_sub(oversized.len()) + 1,
        ));

        let frames = relay.relay_bytes(&oversized);

        assert!(frames.is_empty());
        assert!(relay.inspection().oversized_frame_ignored);
        assert!(relay.inspection().output_items_incomplete);
        assert!(!relay
            .finish()
            .unwrap_or_default()
            .windows(b"resp_provider".len())
            .any(|part| part == b"resp_provider"));
    }

    #[test]
    fn relay_drops_oversized_complete_frame_before_forwarding_it() {
        let mut relay = NativeResponseSseRelay::new("resp_local");
        let prefix = b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_provider\"},\"padding\":\"";
        let mut frame = prefix.to_vec();
        frame.extend(std::iter::repeat_n(
            b'x',
            MAX_INSPECTED_SSE_FRAME_BYTES.saturating_sub(frame.len()) + 1,
        ));
        frame.extend_from_slice(b"\"}\n\n");

        let frames = relay.relay_bytes(&frame);

        assert!(frames.is_empty());
        assert!(relay.inspection().oversized_frame_ignored);
        assert!(relay.inspection().output_items_incomplete);
        assert_eq!(relay.inspection().terminal, None);
        assert!(relay
            .relay_bytes(b"event: response.completed\ndata: {}\n\n")
            .is_empty());
    }
}
