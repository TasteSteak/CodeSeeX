use crate::response_sse::{
    custom_tool_call_sse_added, custom_tool_call_sse_done, function_call_sse_added,
    function_call_sse_done, next_sequence, sse_bytes,
};
use crate::tool_passthrough::ToolContext;
use crate::tools::ownership::{is_native_apply_patch_tool, ChatToolCall};
use crate::tools::response_items::{
    native_apply_patch_response_item_from_chat_call_with_id, normalize_patch_line,
};
use axum::body::Bytes;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Default)]
pub(crate) struct StreamingVisibleToolBridge {
    states: BTreeMap<u64, VisibleToolState>,
    last_tool_index: u64,
}

#[derive(Debug, Default)]
struct VisibleToolState {
    id: String,
    name: String,
    arguments: String,
    consumed_arguments: usize,
    item_id: Option<String>,
    output_index: Option<u64>,
    kind: Option<VisibleToolKind>,
    patch_extractor: JsonStringFieldExtractor,
    patch_normalizer: PatchStreamNormalizer,
    emitted_input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisibleToolKind {
    NativeApplyPatch,
    ExternalFunction,
}

impl StreamingVisibleToolBridge {
    pub(crate) fn process_delta(
        &mut self,
        response_id: &str,
        delta: &Value,
        external_tool_context: &ToolContext,
        output_index: &mut u64,
        sequence: &mut u64,
    ) -> Vec<Bytes> {
        let mut events = Vec::new();
        let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            return events;
        };
        for call in calls {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(self.last_tool_index);
            self.last_tool_index = index;
            let state = self.states.entry(index).or_default();
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                state.id = id.to_owned();
            }
            let mut saw_arguments = false;
            if let Some(function) = call.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    state.name.push_str(name);
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    state.arguments.push_str(arguments);
                    saw_arguments = true;
                }
            }
            if state.kind.is_none() {
                if let Some(event) = state.start_if_visible(
                    response_id,
                    external_tool_context,
                    output_index,
                    sequence,
                ) {
                    events.push(event);
                }
            }
            if saw_arguments || state.consumed_arguments < state.arguments.len() {
                if let Some(event) = state.emit_argument_delta(response_id, sequence) {
                    events.push(event);
                }
            }
        }
        events
    }

    pub(crate) fn finish_native_apply_patch(
        &mut self,
        response_id: &str,
        call: &ChatToolCall,
        sequence: &mut u64,
    ) -> Option<FinishedVisibleTool> {
        let state = self.find_state_mut(call, VisibleToolKind::NativeApplyPatch)?;
        let item_id = state.item_id.clone()?;
        let output_index = state.output_index?;
        let item = native_apply_patch_response_item_from_chat_call_with_id(call, &item_id);
        let mut bytes = Vec::new();
        let final_input = item
            .get("input")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let flushed = state.patch_normalizer.finish();
        if !flushed.is_empty() {
            let mut expected_prefix = state.emitted_input.clone();
            expected_prefix.push_str(&flushed);
            if final_input.starts_with(&expected_prefix) {
                state.emitted_input.push_str(&flushed);
                bytes.extend_from_slice(&custom_tool_input_delta_bytes(
                    response_id,
                    &item_id,
                    output_index,
                    &flushed,
                    sequence,
                ));
            }
        }
        if let Some(delta) = final_input.strip_prefix(&state.emitted_input) {
            if !delta.is_empty() {
                state.emitted_input.push_str(delta);
                bytes.extend_from_slice(&custom_tool_input_delta_bytes(
                    response_id,
                    &item_id,
                    output_index,
                    delta,
                    sequence,
                ));
            }
        }
        bytes.extend_from_slice(&custom_tool_call_sse_done(
            response_id,
            output_index,
            &item,
            sequence,
        ));
        Some(FinishedVisibleTool {
            item,
            bytes: Bytes::from(bytes),
        })
    }

    pub(crate) fn finish_external_function(
        &mut self,
        response_id: &str,
        call: &ChatToolCall,
        external_tool_context: &ToolContext,
        sequence: &mut u64,
    ) -> Option<FinishedVisibleTool> {
        let state = self.find_state_mut(call, VisibleToolKind::ExternalFunction)?;
        let item_id = state.item_id.clone()?;
        let output_index = state.output_index?;
        let item = external_tool_context.response_item_from_chat_call_with_id(call, &item_id);
        let mut bytes = Vec::new();
        if let Some(delta) = call.arguments.strip_prefix(&state.emitted_input) {
            if !delta.is_empty() {
                state.emitted_input.push_str(delta);
                bytes.extend_from_slice(&function_arguments_delta_bytes(
                    response_id,
                    &item_id,
                    output_index,
                    delta,
                    sequence,
                ));
            }
        }
        bytes.extend_from_slice(&function_call_sse_done(
            response_id,
            output_index,
            &item,
            sequence,
        ));
        Some(FinishedVisibleTool {
            item,
            bytes: Bytes::from(bytes),
        })
    }

    fn find_state_mut(
        &mut self,
        call: &ChatToolCall,
        kind: VisibleToolKind,
    ) -> Option<&mut VisibleToolState> {
        self.states.values_mut().find(|state| {
            state.id == call.id && state.name == call.name && state.kind == Some(kind)
        })
    }
}

pub(crate) struct FinishedVisibleTool {
    pub(crate) item: Value,
    pub(crate) bytes: Bytes,
}

impl VisibleToolState {
    fn start_if_visible(
        &mut self,
        response_id: &str,
        external_tool_context: &ToolContext,
        output_index: &mut u64,
        sequence: &mut u64,
    ) -> Option<Bytes> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return None;
        }
        if is_native_apply_patch_tool(&self.name) {
            let item_id = format!("ctc_{}", Uuid::new_v4().simple());
            let call_output_index = *output_index;
            *output_index += 1;
            self.item_id = Some(item_id.clone());
            self.output_index = Some(call_output_index);
            self.kind = Some(VisibleToolKind::NativeApplyPatch);
            let item = json!({
                "id": item_id,
                "type": "custom_tool_call",
                "status": "completed",
                "call_id": self.id,
                "name": "apply_patch",
                "input": ""
            });
            return Some(custom_tool_call_sse_added(
                response_id,
                call_output_index,
                &item,
                sequence,
            ));
        }
        if external_tool_context.has_external_tool(&self.name) {
            let item_id = format!("fc_{}", Uuid::new_v4().simple());
            let call_output_index = *output_index;
            *output_index += 1;
            let call = ChatToolCall {
                id: self.id.clone(),
                name: self.name.clone(),
                arguments: String::new(),
            };
            let item = external_tool_context.response_item_from_chat_call_with_id(&call, &item_id);
            self.item_id = Some(item_id);
            self.output_index = Some(call_output_index);
            self.kind = Some(VisibleToolKind::ExternalFunction);
            return Some(function_call_sse_added(
                response_id,
                call_output_index,
                &item,
                sequence,
            ));
        }
        None
    }

    fn emit_argument_delta(&mut self, response_id: &str, sequence: &mut u64) -> Option<Bytes> {
        if self.consumed_arguments >= self.arguments.len() {
            return None;
        }
        let kind = self.kind?;
        let item_id = self.item_id.as_deref()?;
        let output_index = self.output_index?;
        let chunk = &self.arguments[self.consumed_arguments..];
        self.consumed_arguments = self.arguments.len();
        match kind {
            VisibleToolKind::NativeApplyPatch => {
                let decoded = self.patch_extractor.feed(chunk);
                let normalized = self.patch_normalizer.feed(&decoded);
                if !normalized.is_empty() {
                    self.emitted_input.push_str(&normalized);
                    return Some(custom_tool_input_delta_bytes(
                        response_id,
                        item_id,
                        output_index,
                        &normalized,
                        sequence,
                    ));
                }
            }
            VisibleToolKind::ExternalFunction => {
                if !chunk.is_empty() {
                    self.emitted_input.push_str(chunk);
                    return Some(function_arguments_delta_bytes(
                        response_id,
                        item_id,
                        output_index,
                        chunk,
                        sequence,
                    ));
                }
            }
        }
        None
    }
}

fn custom_tool_input_delta_bytes(
    response_id: &str,
    item_id: &str,
    output_index: u64,
    delta: &str,
    sequence: &mut u64,
) -> Bytes {
    sse_bytes(
        "response.custom_tool_call_input.delta",
        json!({
            "type": "response.custom_tool_call_input.delta",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "delta": delta,
            "sequence_number": next_sequence(sequence)
        }),
    )
}

fn function_arguments_delta_bytes(
    response_id: &str,
    item_id: &str,
    output_index: u64,
    delta: &str,
    sequence: &mut u64,
) -> Bytes {
    sse_bytes(
        "response.function_call_arguments.delta",
        json!({
            "type": "response.function_call_arguments.delta",
            "response_id": response_id,
            "item_id": item_id,
            "output_index": output_index,
            "delta": delta,
            "sequence_number": next_sequence(sequence)
        }),
    )
}

#[derive(Debug, Default)]
struct JsonStringFieldExtractor {
    mode: JsonFieldMode,
    decoder: JsonStringDecoder,
    current_key: String,
    pending_key: String,
}

#[derive(Debug, Default)]
enum JsonFieldMode {
    #[default]
    SeekingKey,
    InKey,
    AfterKey,
    BeforeValue,
    InTargetValue,
    InIgnoredString,
}

impl JsonStringFieldExtractor {
    fn feed(&mut self, chunk: &str) -> String {
        let mut output = String::new();
        for ch in chunk.chars() {
            match self.mode {
                JsonFieldMode::SeekingKey => {
                    if ch == '"' {
                        self.current_key.clear();
                        self.decoder.reset();
                        self.mode = JsonFieldMode::InKey;
                    }
                }
                JsonFieldMode::InKey => {
                    let mut decoded = String::new();
                    if self.decoder.feed(ch, &mut decoded) == JsonStringStatus::Ended {
                        self.pending_key.clone_from(&self.current_key);
                        self.mode = JsonFieldMode::AfterKey;
                    } else {
                        self.current_key.push_str(&decoded);
                    }
                }
                JsonFieldMode::AfterKey => {
                    if ch.is_whitespace() {
                        continue;
                    }
                    if ch == ':' {
                        self.mode = JsonFieldMode::BeforeValue;
                    } else {
                        self.mode = JsonFieldMode::SeekingKey;
                    }
                }
                JsonFieldMode::BeforeValue => {
                    if ch.is_whitespace() {
                        continue;
                    }
                    if ch == '"' {
                        self.decoder.reset();
                        if matches!(self.pending_key.as_str(), "patch" | "input") {
                            self.mode = JsonFieldMode::InTargetValue;
                        } else {
                            self.mode = JsonFieldMode::InIgnoredString;
                        }
                    } else {
                        self.mode = JsonFieldMode::SeekingKey;
                    }
                }
                JsonFieldMode::InTargetValue => {
                    let mut decoded = String::new();
                    if self.decoder.feed(ch, &mut decoded) == JsonStringStatus::Ended {
                        self.mode = JsonFieldMode::SeekingKey;
                    } else {
                        output.push_str(&decoded);
                    }
                }
                JsonFieldMode::InIgnoredString => {
                    let mut ignored = String::new();
                    if self.decoder.feed(ch, &mut ignored) == JsonStringStatus::Ended {
                        self.mode = JsonFieldMode::SeekingKey;
                    }
                }
            }
        }
        output
    }
}

#[derive(Debug, Default)]
struct JsonStringDecoder {
    escaped: bool,
    unicode_escape: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonStringStatus {
    Continuing,
    Ended,
}

impl JsonStringDecoder {
    fn reset(&mut self) {
        self.escaped = false;
        self.unicode_escape = None;
    }

    fn feed(&mut self, ch: char, output: &mut String) -> JsonStringStatus {
        if let Some(hex) = self.unicode_escape.as_mut() {
            if ch.is_ascii_hexdigit() {
                hex.push(ch);
                if hex.len() == 4 {
                    if let Ok(value) = u32::from_str_radix(hex, 16) {
                        if let Some(decoded) = char::from_u32(value) {
                            output.push(decoded);
                        }
                    }
                    self.unicode_escape = None;
                }
            } else {
                self.unicode_escape = None;
                output.push(ch);
            }
            return JsonStringStatus::Continuing;
        }
        if self.escaped {
            self.escaped = false;
            match ch {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                '/' => output.push('/'),
                'b' => output.push('\u{0008}'),
                'f' => output.push('\u{000c}'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                'u' => self.unicode_escape = Some(String::new()),
                other => output.push(other),
            }
            return JsonStringStatus::Continuing;
        }
        match ch {
            '"' => JsonStringStatus::Ended,
            '\\' => {
                self.escaped = true;
                JsonStringStatus::Continuing
            }
            other => {
                output.push(other);
                JsonStringStatus::Continuing
            }
        }
    }
}

#[derive(Debug, Default)]
struct PatchStreamNormalizer {
    line: String,
    pending_cr: bool,
}

impl PatchStreamNormalizer {
    fn feed(&mut self, chunk: &str) -> String {
        let mut output = String::new();
        for ch in chunk.chars() {
            if self.pending_cr {
                self.pending_cr = false;
                if ch == '\n' {
                    continue;
                }
            }
            match ch {
                '\r' => {
                    self.flush_line(&mut output);
                    self.pending_cr = true;
                }
                '\n' => self.flush_line(&mut output),
                other => self.line.push(other),
            }
        }
        output
    }

    fn finish(&mut self) -> String {
        if self.line.is_empty() {
            return String::new();
        }
        let output = normalize_patch_line(&self.line);
        self.line.clear();
        output
    }

    fn flush_line(&mut self, output: &mut String) {
        output.push_str(&normalize_patch_line(&self.line));
        output.push('\n');
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_patch_field_from_streamed_json_chunks() {
        let mut extractor = JsonStringFieldExtractor::default();
        let mut normalizer = PatchStreamNormalizer::default();
        let mut output = String::new();
        for chunk in [
            r#"{"patch":"*** Begin"#,
            r#" Patch\n*** Update File: a.txt\n@@ -1 +1,2 @@"#,
            r#"\n-old\n+new\n*** End Patch"}"#,
        ] {
            output.push_str(&normalizer.feed(&extractor.feed(chunk)));
        }
        output.push_str(&normalizer.finish());
        assert_eq!(
            output,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End Patch"
        );
    }

    #[test]
    fn visible_bridge_streams_apply_patch_input_before_finish() {
        let mut bridge = StreamingVisibleToolBridge::default();
        let external = ToolContext::default();
        let mut output_index = 0;
        let mut sequence = 0;
        let delta = json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_patch",
                "type": "function",
                "function": {
                    "name": "apply_patch",
                    "arguments": "{\"patch\":\"*** Begin Patch\\n*** Add File: a.txt\\n+hello"
                }
            }]
        });
        let events = bridge.process_delta(
            "resp_1",
            &delta,
            &external,
            &mut output_index,
            &mut sequence,
        );
        let body = events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        assert!(body.contains("response.output_item.added"), "{body}");
        assert!(
            body.contains("response.custom_tool_call_input.delta"),
            "{body}"
        );
        assert!(body.contains("*** Begin Patch"), "{body}");
        assert!(!body.contains("response.output_item.done"), "{body}");
    }

    #[test]
    fn visible_bridge_finishes_apply_patch_with_same_item_id() {
        let mut bridge = StreamingVisibleToolBridge::default();
        let external = ToolContext::default();
        let mut output_index = 0;
        let mut sequence = 0;
        let first = json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_patch",
                "function": {
                    "name": "apply_patch",
                    "arguments": "{\"patch\":\"*** Begin Patch\\n"
                }
            }]
        });
        let second = json!({
            "tool_calls": [{
                "index": 0,
                "function": {
                    "arguments": "*** End Patch\"}"
                }
            }]
        });
        let first_events = bridge.process_delta(
            "resp_1",
            &first,
            &external,
            &mut output_index,
            &mut sequence,
        );
        let _ = bridge.process_delta(
            "resp_1",
            &second,
            &external,
            &mut output_index,
            &mut sequence,
        );
        let added_body = first_events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        let call = ChatToolCall {
            id: "call_patch".to_owned(),
            name: "apply_patch".to_owned(),
            arguments: r#"{"patch":"*** Begin Patch\n*** End Patch"}"#.to_owned(),
        };
        let finished = bridge
            .finish_native_apply_patch("resp_1", &call, &mut sequence)
            .expect("streamed tool should finish");
        let done_body = String::from_utf8_lossy(&finished.bytes).into_owned();
        let item_id = finished.item["id"].as_str().unwrap();
        assert!(added_body.contains(item_id), "{added_body}\n{done_body}");
        assert!(
            done_body.contains("response.custom_tool_call_input.done"),
            "{done_body}"
        );
        assert!(
            done_body.contains("\"input\":\"*** Begin Patch\\n*** End Patch\""),
            "{done_body}"
        );
    }

    #[test]
    fn visible_bridge_streams_external_function_arguments() {
        let mut bridge = StreamingVisibleToolBridge::default();
        let external = ToolContext::from_request_tools(Some(&json!([{
            "type": "function",
            "function": {
                "name": "client_tool",
                "description": "client owned",
                "parameters": { "type": "object", "properties": {} }
            }
        }])));
        let mut output_index = 0;
        let mut sequence = 0;
        let first = json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_client",
                "function": {
                    "name": "client_tool",
                    "arguments": "{\"path\":"
                }
            }]
        });
        let second = json!({
            "tool_calls": [{
                "index": 0,
                "function": {
                    "arguments": "\"README.md\"}"
                }
            }]
        });
        let first_events = bridge.process_delta(
            "resp_1",
            &first,
            &external,
            &mut output_index,
            &mut sequence,
        );
        let _ = bridge.process_delta(
            "resp_1",
            &second,
            &external,
            &mut output_index,
            &mut sequence,
        );
        let added_body = first_events
            .iter()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .collect::<String>();
        assert!(
            added_body.contains("response.output_item.added"),
            "{added_body}"
        );
        assert!(
            added_body.contains("response.function_call_arguments.delta"),
            "{added_body}"
        );
        let call = ChatToolCall {
            id: "call_client".to_owned(),
            name: "client_tool".to_owned(),
            arguments: r#"{"path":"README.md"}"#.to_owned(),
        };
        let finished = bridge
            .finish_external_function("resp_1", &call, &external, &mut sequence)
            .expect("streamed external function should finish");
        let done_body = String::from_utf8_lossy(&finished.bytes).into_owned();
        let item_id = finished.item["id"].as_str().unwrap();
        assert!(added_body.contains(item_id), "{added_body}\n{done_body}");
        assert!(
            done_body.contains("response.function_call_arguments.done"),
            "{done_body}"
        );
        assert!(
            done_body.contains("\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\""),
            "{done_body}"
        );
    }
}
