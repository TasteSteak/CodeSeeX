use crate::native_responses::{NativeToolCall, NativeToolCallKind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_PENDING_NATIVE_GROUPS: usize = 64;
const PENDING_NATIVE_GROUP_TTL: Duration = Duration::from_secs(30 * 60);

/// RAM-only state for a native provider group that still needs Codex-owned
/// tool outputs. It intentionally contains no persisted transcript: after a
/// restart, a complete client replay can continue directly, while a replay
/// that depends on hidden local outputs is rejected instead of reconstructed
/// from disk or a tail cache.
#[derive(Clone, Default)]
pub(crate) struct NativePendingToolGroups {
    groups: Arc<Mutex<BTreeMap<String, PendingNativeToolGroup>>>,
}

#[derive(Clone)]
pub(crate) struct PendingNativeToolGroup {
    pub(crate) response_id: String,
    pub(crate) request_anchor: Option<String>,
    pub(crate) authoritative_input: Vec<Value>,
    pub(crate) provider_output: Vec<Value>,
    pub(crate) visible_provider_output: Vec<Value>,
    pub(crate) local_output_items: Vec<Value>,
    pub(crate) client_calls: Vec<NativeToolCall>,
    created_at: Instant,
}

#[derive(Debug, Clone)]
pub(crate) struct NativePendingContinuation {
    pub(crate) pending_response_id: String,
    /// The only field reconstructed from the retained protocol group. The
    /// caller must apply it to its freshly normalized current request, so the
    /// current authoritative fields (model, instructions, tools, stream, ...)
    /// are never replaced by a cached earlier payload.
    pub(crate) merged_input: Vec<Value>,
    pub(crate) client_output_count: usize,
    pub(crate) local_output_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativePendingError {
    InvalidPendingGroup(&'static str),
    AmbiguousPendingGroup,
    AnchorMismatch,
    PreviousResponseMismatch,
    InputMissing,
    AuthoritativePrefixMismatch,
    VisibleProviderOutputMismatch,
    MissingClientToolOutput {
        call_id: String,
    },
    DuplicateClientToolOutput {
        call_id: String,
    },
    ClientToolOutputOrderMismatch {
        expected_call_id: String,
        actual_call_id: String,
    },
    UnexpectedToolOutput {
        call_id: String,
    },
    InvalidClientToolOutput {
        call_id: String,
    },
}

impl NativePendingError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::AnchorMismatch
            | Self::PreviousResponseMismatch
            | Self::InputMissing
            | Self::AuthoritativePrefixMismatch
            | Self::VisibleProviderOutputMismatch => "context_required",
            Self::InvalidPendingGroup(_)
            | Self::AmbiguousPendingGroup
            | Self::MissingClientToolOutput { .. }
            | Self::DuplicateClientToolOutput { .. }
            | Self::ClientToolOutputOrderMismatch { .. }
            | Self::UnexpectedToolOutput { .. }
            | Self::InvalidClientToolOutput { .. } => "tool_output_required",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::InvalidPendingGroup(reason) => {
                format!("CodeSeeX could not safely retain a native tool group: {reason}.")
            }
            Self::AmbiguousPendingGroup => {
                "CodeSeeX found more than one native tool group for this replay. Start from the authoritative Codex replay and retry.".to_owned()
            }
            Self::AnchorMismatch => {
                "The native tool replay did not match the original Codex session anchor. CodeSeeX did not merge contexts.".to_owned()
            }
            Self::PreviousResponseMismatch => {
                "The native tool replay referenced a different previous response while this Codex session still has a pending tool group. CodeSeeX did not bypass tool-output validation.".to_owned()
            }
            Self::InputMissing => {
                "The native tool continuation did not include an input item array. CodeSeeX requires the authoritative Codex replay.".to_owned()
            }
            Self::AuthoritativePrefixMismatch => {
                "The native tool continuation no longer begins with the original Codex replay. CodeSeeX did not use a tail-only continuation.".to_owned()
            }
            Self::VisibleProviderOutputMismatch => {
                "The native tool continuation did not retain the provider tool group visible to Codex.".to_owned()
            }
            Self::MissingClientToolOutput { call_id } => {
                format!("The native tool group is incomplete: output for call '{call_id}' is missing.")
            }
            Self::DuplicateClientToolOutput { call_id } => {
                format!("The native tool group is ambiguous: output for call '{call_id}' appeared more than once.")
            }
            Self::ClientToolOutputOrderMismatch {
                expected_call_id,
                actual_call_id,
            } => {
                format!(
                    "The native tool group changed output order: expected call '{expected_call_id}', received '{actual_call_id}'."
                )
            }
            Self::UnexpectedToolOutput { call_id } => {
                format!("The native tool continuation contained an unexpected tool output for call '{call_id}'.")
            }
            Self::InvalidClientToolOutput { call_id } => {
                format!("The native tool output for call '{call_id}' did not match its original provider call.")
            }
        }
    }
}

impl NativePendingToolGroups {
    pub(crate) fn register(&self, group: PendingNativeToolGroup) -> Result<(), NativePendingError> {
        validate_pending_group(&group)?;
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut groups);
        groups.insert(group.response_id.clone(), group);
        trim_to_capacity(&mut groups);
        Ok(())
    }

    /// Builds a provider continuation only after the Codex replay proves that
    /// every client-owned call in the provider group has an exact output.
    /// The pending group stays available until the caller confirms that the
    /// next upstream request was accepted, so a pre-dispatch network failure
    /// can be retried without inventing or losing an output.
    pub(crate) fn continuation_for(
        &self,
        request: &Value,
    ) -> Result<Option<NativePendingContinuation>, NativePendingError> {
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut groups);
        let Some(group) = matching_group(&groups, request)? else {
            return Ok(None);
        };
        let input = request
            .get("input")
            .and_then(Value::as_array)
            .ok_or(NativePendingError::InputMissing)?;
        validate_request_anchor(group, request)?;
        let after_authoritative = strip_prefix(input, &group.authoritative_input)
            .ok_or(NativePendingError::AuthoritativePrefixMismatch)?;
        let after_visible = strip_prefix(after_authoritative, &group.visible_provider_output)
            .ok_or(NativePendingError::VisibleProviderOutputMismatch)?;
        let (client_outputs, suffix) = collect_client_outputs(after_visible, &group.client_calls)?;

        let mut merged_input = Vec::with_capacity(
            group.authoritative_input.len()
                + group.provider_output.len()
                + group.local_output_items.len()
                + client_outputs.len()
                + suffix.len(),
        );
        merged_input.extend(group.authoritative_input.iter().cloned());
        merged_input.extend(group.provider_output.iter().cloned());
        merged_input.extend(group.local_output_items.iter().cloned());
        merged_input.extend(client_outputs);
        merged_input.extend(suffix);

        Ok(Some(NativePendingContinuation {
            pending_response_id: group.response_id.clone(),
            merged_input,
            client_output_count: group.client_calls.len(),
            local_output_count: group.local_output_items.len(),
        }))
    }

    pub(crate) fn settle(&self, response_id: &str) {
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        groups.remove(response_id);
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.groups
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl PendingNativeToolGroup {
    pub(crate) fn new(
        response_id: impl Into<String>,
        request: &Value,
        authoritative_input: Vec<Value>,
        provider_output: Vec<Value>,
        visible_provider_output: Vec<Value>,
        local_output_items: Vec<Value>,
        client_calls: Vec<NativeToolCall>,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            request_anchor: request_anchor(request),
            authoritative_input,
            provider_output,
            visible_provider_output,
            local_output_items,
            client_calls,
            created_at: Instant::now(),
        }
    }

    pub(crate) fn diagnostic(&self) -> Value {
        json!({
            "pending": true,
            "response_id_hash": short_hash(&self.response_id),
            "session_anchor": self.request_anchor.as_ref().map(|value| short_hash(value)),
            "authoritative_input_items": self.authoritative_input.len(),
            "provider_output_items": self.provider_output.len(),
            "visible_provider_output_items": self.visible_provider_output.len(),
            "local_output_items": self.local_output_items.len(),
            "client_calls": self.client_calls.iter().map(|call| json!({
                "call_id_hash": short_hash(&call.call_id),
                "name": call.name,
                "kind": native_kind_label(call.kind)
            })).collect::<Vec<_>>()
        })
    }
}

fn matching_group<'a>(
    groups: &'a BTreeMap<String, PendingNativeToolGroup>,
    request: &Value,
) -> Result<Option<&'a PendingNativeToolGroup>, NativePendingError> {
    let previous = request
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(previous) = previous {
        if let Some(group) = groups.get(previous) {
            return Ok(Some(group));
        }
        let anchor = request_anchor(request);
        if anchor.as_deref().is_some_and(|anchor| {
            groups
                .values()
                .any(|group| group.request_anchor.as_deref() == Some(anchor))
        }) {
            return Err(NativePendingError::PreviousResponseMismatch);
        }
        return Ok(None);
    }

    let anchor = request_anchor(request);
    let Some(anchor) = anchor.as_deref() else {
        return Ok(None);
    };
    let mut matches = groups
        .values()
        .filter(|group| group.request_anchor.as_deref() == Some(anchor));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(NativePendingError::AmbiguousPendingGroup);
    }
    Ok(first)
}

fn validate_request_anchor(
    group: &PendingNativeToolGroup,
    request: &Value,
) -> Result<(), NativePendingError> {
    let incoming = request_anchor(request);
    match (group.request_anchor.as_deref(), incoming.as_deref()) {
        (Some(expected), Some(actual)) if expected != actual => {
            Err(NativePendingError::AnchorMismatch)
        }
        _ => Ok(()),
    }
}

fn collect_client_outputs(
    input: &[Value],
    calls: &[NativeToolCall],
) -> Result<(Vec<Value>, Vec<Value>), NativePendingError> {
    // A provider tool group and its outputs are an ordered protocol unit.
    // Do not use a map to "fix" an out-of-order client replay: that would
    // turn a malformed or divergent replay into a different request.
    let mut outputs = Vec::with_capacity(calls.len());
    for (index, call) in calls.iter().enumerate() {
        let Some(item) = input.get(index) else {
            return Err(NativePendingError::MissingClientToolOutput {
                call_id: call.call_id.clone(),
            });
        };
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let is_tool_output = matches!(
            item_type,
            "function_call_output" | "custom_tool_call_output"
        );
        if !is_tool_output {
            return Err(NativePendingError::ClientToolOutputOrderMismatch {
                expected_call_id: call.call_id.clone(),
                actual_call_id: "non_tool_item".to_owned(),
            });
        }
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| NativePendingError::UnexpectedToolOutput {
                call_id: "missing".to_owned(),
            })?;
        if call_id != call.call_id {
            if calls[..index]
                .iter()
                .any(|previous| previous.call_id == call_id)
            {
                return Err(NativePendingError::DuplicateClientToolOutput {
                    call_id: call_id.to_owned(),
                });
            }
            if calls.iter().any(|expected| expected.call_id == call_id) {
                return Err(NativePendingError::ClientToolOutputOrderMismatch {
                    expected_call_id: call.call_id.clone(),
                    actual_call_id: call_id.to_owned(),
                });
            }
            return Err(NativePendingError::UnexpectedToolOutput {
                call_id: call_id.to_owned(),
            });
        }
        let expected_type = output_type_for(call.kind);
        if item_type != expected_type || item.get("output").and_then(Value::as_str).is_none() {
            return Err(NativePendingError::InvalidClientToolOutput {
                call_id: call_id.to_owned(),
            });
        }
        outputs.push(item.clone());
    }
    let suffix = input[calls.len()..].to_vec();
    for item in &suffix {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        if !matches!(
            item_type,
            "function_call_output" | "custom_tool_call_output"
        ) {
            continue;
        }
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("missing");
        if calls.iter().any(|call| call.call_id == call_id) {
            return Err(NativePendingError::DuplicateClientToolOutput {
                call_id: call_id.to_owned(),
            });
        }
        return Err(NativePendingError::UnexpectedToolOutput {
            call_id: call_id.to_owned(),
        });
    }
    Ok((outputs, suffix))
}

fn validate_pending_group(group: &PendingNativeToolGroup) -> Result<(), NativePendingError> {
    if group.response_id.trim().is_empty() {
        return Err(NativePendingError::InvalidPendingGroup(
            "response id was empty",
        ));
    }
    if group.client_calls.is_empty() {
        return Err(NativePendingError::InvalidPendingGroup(
            "client tool group was empty",
        ));
    }
    let mut ids = HashSet::new();
    if group
        .client_calls
        .iter()
        .any(|call| call.call_id.trim().is_empty() || !ids.insert(call.call_id.as_str()))
    {
        return Err(NativePendingError::InvalidPendingGroup(
            "client tool call ids were missing or duplicated",
        ));
    }
    Ok(())
}

fn strip_prefix<'a>(input: &'a [Value], prefix: &[Value]) -> Option<&'a [Value]> {
    input
        .get(..prefix.len())
        .filter(|candidate| *candidate == prefix)
        .map(|_| &input[prefix.len()..])
}

fn prune_expired(groups: &mut BTreeMap<String, PendingNativeToolGroup>) {
    let now = Instant::now();
    groups.retain(|_, group| now.duration_since(group.created_at) <= PENDING_NATIVE_GROUP_TTL);
}

fn trim_to_capacity(groups: &mut BTreeMap<String, PendingNativeToolGroup>) {
    while groups.len() > MAX_PENDING_NATIVE_GROUPS {
        let Some(oldest) = groups
            .iter()
            .min_by_key(|(_, group)| group.created_at)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        groups.remove(&oldest);
    }
}

fn request_anchor(request: &Value) -> Option<String> {
    let prompt_cache_key = request
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let installation_id = request
        .pointer("/client_metadata/x-codex-installation-id")
        .or_else(|| request.pointer("/metadata/x-codex-installation-id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (prompt_cache_key, installation_id) {
        (Some(cache), Some(installation)) => {
            Some(format!("cache:{cache}\u{0}installation:{installation}"))
        }
        (Some(cache), None) => Some(format!("cache:{cache}")),
        (None, Some(installation)) => Some(format!("installation:{installation}")),
        (None, None) => None,
    }
}

fn output_type_for(kind: NativeToolCallKind) -> &'static str {
    match kind {
        NativeToolCallKind::Function => "function_call_output",
        NativeToolCallKind::Custom => "custom_tool_call_output",
    }
}

fn native_kind_label(kind: NativeToolCallKind) -> &'static str {
    match kind {
        NativeToolCallKind::Function => "function",
        NativeToolCallKind::Custom => "custom",
    }
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(input: Vec<Value>) -> Value {
        json!({
            "previous_response_id": "resp_local_1",
            "prompt_cache_key": "thread-a",
            "client_metadata": { "x-codex-installation-id": "install-a" },
            "input": input
        })
    }

    fn function_call(call_id: &str, name: &str) -> NativeToolCall {
        NativeToolCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            input: "{}".to_owned(),
            kind: NativeToolCallKind::Function,
        }
    }

    fn group(
        authoritative_input: Vec<Value>,
        provider_output: Vec<Value>,
        visible_provider_output: Vec<Value>,
        local_output_items: Vec<Value>,
        client_calls: Vec<NativeToolCall>,
    ) -> PendingNativeToolGroup {
        PendingNativeToolGroup::new(
            "resp_local_1",
            &request(Vec::new()),
            authoritative_input,
            provider_output,
            visible_provider_output,
            local_output_items,
            client_calls,
        )
    }

    #[test]
    fn full_replay_continuation_preserves_authoritative_prefix_and_call_order() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                provider.clone(),
                provider.clone(),
                Vec::new(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let continuation = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[0].clone(),
                json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
            ]))
            .unwrap()
            .unwrap();
        assert_eq!(continuation.client_output_count, 1);
        assert_eq!(continuation.local_output_count, 0);
        assert_eq!(
            Value::Array(continuation.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                { "type": "function_call", "call_id": "call_shell", "name": "shell_command", "arguments": "{}", "status": "completed" },
                { "type": "function_call_output", "call_id": "call_shell", "output": "ok" }
            ])
        );
        assert_eq!(groups.pending_count(), 1);
        groups.settle("resp_local_1");
        assert_eq!(groups.pending_count(), 0);
    }

    #[test]
    fn out_of_order_client_outputs_are_rejected_without_rewriting_the_replay() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![
            json!({
                "type": "function_call",
                "call_id": "call_shell",
                "name": "shell_command",
                "arguments": "{}",
                "status": "completed"
            }),
            json!({
                "type": "custom_tool_call",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** End Patch",
                "status": "completed"
            }),
        ];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                provider.clone(),
                provider.clone(),
                Vec::new(),
                vec![
                    function_call("call_shell", "shell_command"),
                    NativeToolCall {
                        call_id: "call_patch".to_owned(),
                        name: "apply_patch".to_owned(),
                        input: "*** Begin Patch\n*** End Patch".to_owned(),
                        kind: NativeToolCallKind::Custom,
                    },
                ],
            ))
            .unwrap();

        let error = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[0].clone(),
                provider[1].clone(),
                json!({ "type": "custom_tool_call_output", "call_id": "call_patch", "output": "done" }),
                json!({ "type": "function_call_output", "call_id": "call_shell", "output": "done" }),
            ]))
            .unwrap_err();

        assert_eq!(
            error,
            NativePendingError::ClientToolOutputOrderMismatch {
                expected_call_id: "call_shell".to_owned(),
                actual_call_id: "call_patch".to_owned(),
            }
        );
        assert_eq!(groups.pending_count(), 1);
    }

    #[test]
    fn mixed_group_reconstructs_only_the_verified_local_output() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let local = json!({
            "type": "function_call",
            "call_id": "call_local",
            "name": "workspace_search",
            "arguments": "{}",
            "status": "completed"
        });
        let client = json!({
            "type": "custom_tool_call",
            "call_id": "call_patch",
            "name": "apply_patch",
            "input": "*** Begin Patch\n*** End Patch",
            "status": "completed"
        });
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                vec![local.clone(), client.clone()],
                vec![client.clone()],
                vec![json!({ "type": "function_call_output", "call_id": "call_local", "output": "local result" })],
                vec![NativeToolCall {
                    call_id: "call_patch".to_owned(),
                    name: "apply_patch".to_owned(),
                    input: "*** Begin Patch\n*** End Patch".to_owned(),
                    kind: NativeToolCallKind::Custom,
                }],
            ))
            .unwrap();
        let continuation = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                client.clone(),
                json!({ "type": "custom_tool_call_output", "call_id": "call_patch", "output": "Done" }),
            ]))
            .unwrap()
            .unwrap();
        assert_eq!(
            Value::Array(continuation.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                { "type": "function_call", "call_id": "call_local", "name": "workspace_search", "arguments": "{}", "status": "completed" },
                { "type": "custom_tool_call", "call_id": "call_patch", "name": "apply_patch", "input": "*** Begin Patch\n*** End Patch", "status": "completed" },
                { "type": "function_call_output", "call_id": "call_local", "output": "local result" },
                { "type": "custom_tool_call_output", "call_id": "call_patch", "output": "Done" }
            ])
        );
    }

    #[test]
    fn partial_or_wrong_outputs_fail_closed_without_settling_pending_group() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                provider.clone(),
                provider.clone(),
                Vec::new(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let error = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[0].clone(),
            ]))
            .unwrap_err();
        assert_eq!(
            error,
            NativePendingError::MissingClientToolOutput {
                call_id: "call_shell".to_owned()
            }
        );
        assert_eq!(groups.pending_count(), 1);
    }

    #[test]
    fn anchor_mismatch_is_rejected_even_when_previous_response_matches() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                provider.clone(),
                provider.clone(),
                Vec::new(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let mut replay = request(vec![
            authoritative[0].clone(),
            provider[0].clone(),
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]);
        replay["prompt_cache_key"] = json!("thread-b");
        assert_eq!(
            groups.continuation_for(&replay).unwrap_err(),
            NativePendingError::AnchorMismatch
        );
    }

    #[test]
    fn unknown_previous_response_cannot_bypass_pending_group_with_same_anchor() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative,
                provider,
                Vec::new(),
                Vec::new(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let mut replay = request(Vec::new());
        replay["previous_response_id"] = json!("resp_unrelated");

        assert_eq!(
            groups.continuation_for(&replay).unwrap_err(),
            NativePendingError::PreviousResponseMismatch
        );
        assert_eq!(groups.pending_count(), 1);
    }
}
