use crate::native_responses::{NativeToolCall, NativeToolCallKind};
use crate::response_sse::thinking_display_prefix;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_PENDING_NATIVE_GROUPS: usize = 64;
const PENDING_NATIVE_GROUP_TTL: Duration = Duration::from_secs(30 * 60);
/// Upper bound on item identities kept in a rejection diagnostic. A Codex
/// replay can be very long, and an event log entry only needs enough to show
/// which item diverged.
const DIAGNOSTIC_IDENTITY_LIMIT: usize = 12;

/// RAM-only state for a native provider group that still needs Codex-owned
/// tool outputs. It intentionally contains no persisted transcript: after a
/// restart, a complete client replay can continue directly, while a replay
/// that depends on hidden local outputs is rejected instead of reconstructed
/// from disk or a tail cache.
#[derive(Clone, Default)]
pub(crate) struct NativePendingToolGroups {
    groups: Arc<Mutex<BTreeMap<String, PendingNativeToolGroup>>>,
}

/// One block of upstream input that CodeSeeX itself injected: a hosted tool
/// round it executed inside the same turn. The provider already saw these
/// items, but Codex never did, so they are not part of the client-visible
/// anchor. They are replayed at the offset in that anchor where CodeSeeX
/// injected them, which keeps a nested continuation in provider order.
#[derive(Debug, Clone)]
pub(crate) struct NativeInjectedItems {
    /// Index inside `PendingNativeToolGroup::authoritative_input` this block
    /// was injected after.
    pub(crate) offset: usize,
    pub(crate) items: Vec<Value>,
}

#[derive(Clone)]
pub(crate) struct PendingNativeToolGroup {
    pub(crate) response_id: String,
    pub(crate) request_anchor: Option<String>,
    pub(crate) authoritative_input: Vec<Value>,
    pub(crate) injected_items: Vec<NativeInjectedItems>,
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
    /// Hosted rounds CodeSeeX executed itself. The next continuation must keep
    /// replaying them, because the provider produced the retained group in a
    /// context that already contained them.
    pub(crate) injected_items: Vec<NativeInjectedItems>,
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
    AuthoritativePrefixMismatch {
        stored: Vec<String>,
        replayed: Vec<String>,
    },
    VisibleProviderOutputMismatch {
        stored: Vec<String>,
        replayed: Vec<String>,
    },
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
            | Self::AuthoritativePrefixMismatch { .. }
            | Self::VisibleProviderOutputMismatch { .. } => "context_required",
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
            Self::AuthoritativePrefixMismatch { stored, replayed } => {
                let mut message = "The native tool continuation no longer begins with the original Codex replay. CodeSeeX did not use a tail-only continuation.".to_owned();
                if !stored.is_empty() || !replayed.is_empty() {
                    message.push_str(&format!(
                        " Stored: [{}]. Replayed: [{}].",
                        stored.join(", "),
                        replayed.join(", ")
                    ));
                }
                message
            }
            Self::VisibleProviderOutputMismatch { stored, replayed } => {
                let mut message = "The native tool continuation did not retain the provider tool group visible to Codex.".to_owned();
                if !stored.is_empty() || !replayed.is_empty() {
                    message.push_str(&format!(
                        " Stored: [{}]. Replayed: [{}].",
                        stored.join(", "),
                        replayed.join(", ")
                    ));
                }
                message
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

    /// Structured detail for the event log. Only a replay mismatch needs it:
    /// the message alone cannot show which item diverged.
    pub(crate) fn diagnostic(&self) -> Option<Value> {
        match self {
            Self::VisibleProviderOutputMismatch { stored, replayed } => Some(json!({
                "stored_provider_items": stored,
                "replayed_items": replayed
            })),
            Self::AuthoritativePrefixMismatch { stored, replayed } => Some(json!({
                "stored_authoritative_items": stored,
                "replayed_items": replayed
            })),
            Self::AmbiguousPendingGroup => Some(json!({
                "reason": "more_than_one_pending_group_for_this_anchor_or_replay"
            })),
            _ => None,
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
        let after_authoritative = match strip_prefix(input, &group.authoritative_input) {
            Some(after) => after,
            None => {
                return Err(NativePendingError::AuthoritativePrefixMismatch {
                    stored: compact_identities(
                        &group.authoritative_input,
                        DIAGNOSTIC_IDENTITY_LIMIT,
                    ),
                    replayed: compact_identities(input, DIAGNOSTIC_IDENTITY_LIMIT),
                })
            }
        };
        let after_visible =
            after_visible_group(after_authoritative, &group.visible_provider_output).ok_or_else(
                || NativePendingError::VisibleProviderOutputMismatch {
                    stored: group
                        .visible_provider_output
                        .iter()
                        .map(compact_item_identity)
                        .collect(),
                    replayed: after_authoritative
                        .iter()
                        .map(compact_item_identity)
                        .collect(),
                },
            )?;
        let untracked_call_ids = group.untracked_client_call_ids();
        let (client_outputs, suffix) =
            collect_client_outputs(after_visible, &group.client_calls, &untracked_call_ids)?;

        let injected_item_count = group
            .injected_items
            .iter()
            .map(|segment| segment.items.len())
            .sum::<usize>();
        let mut merged_input = Vec::with_capacity(
            group.authoritative_input.len()
                + injected_item_count
                + group.provider_output.len()
                + group.local_output_items.len()
                + client_outputs.len()
                + suffix.len(),
        );
        // CodeSeeX-injected hosted rounds sit at their original offset inside
        // the client-visible anchor, so a nested continuation keeps the exact
        // order the provider already produced them in.
        let mut anchor_replayed = 0_usize;
        for segment in &group.injected_items {
            merged_input.extend(
                group.authoritative_input[anchor_replayed..segment.offset]
                    .iter()
                    .cloned(),
            );
            merged_input.extend(segment.items.iter().cloned());
            anchor_replayed = segment.offset;
        }
        merged_input.extend(group.authoritative_input[anchor_replayed..].iter().cloned());
        merged_input.extend(group.provider_output.iter().cloned());
        merged_input.extend(group.local_output_items.iter().cloned());
        merged_input.extend(client_outputs);
        merged_input.extend(suffix);

        Ok(Some(NativePendingContinuation {
            pending_response_id: group.response_id.clone(),
            merged_input,
            injected_items: group.injected_items.clone(),
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
    // The retained group is one protocol unit: identity, anchor, provider
    // output, CodeSeeX-injected rounds and the calls Codex owns.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        response_id: impl Into<String>,
        request: &Value,
        authoritative_input: Vec<Value>,
        injected_items: Vec<NativeInjectedItems>,
        provider_output: Vec<Value>,
        visible_provider_output: Vec<Value>,
        local_output_items: Vec<Value>,
        client_calls: Vec<NativeToolCall>,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            request_anchor: request_anchor(request),
            authoritative_input,
            injected_items,
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
            "injected_items": self
                .injected_items
                .iter()
                .map(|segment| segment.items.len())
                .sum::<usize>(),
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

    /// Call ids inside the retained group that the client answers itself.
    ///
    /// The provider can call Codex's own `tool_search`. CodeSeeX forwards that
    /// call like any other provider item, but Codex executes it and replays the
    /// `tool_search_output` on its own, so the call never becomes one of the
    /// verified `client_calls`. Its output still sits in the client's replay,
    /// and can sit between the outputs CodeSeeX does verify, so the coordinator
    /// has to recognise it instead of reading it as a shifted tool output.
    fn untracked_client_call_ids(&self) -> BTreeSet<String> {
        self.visible_provider_output
            .iter()
            .filter(|item| item_type(item) == "tool_search_call")
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
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
    let candidates = groups
        .values()
        .filter(|group| group.request_anchor.as_deref() == Some(anchor))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let Some(input) = request.get("input").and_then(Value::as_array) else {
        // Without an input array nothing can answer a group. Keep the previous
        // single-candidate behaviour so the caller still reports the missing
        // authoritative replay instead of silently ignoring the session.
        return match candidates.len() {
            1 => Ok(Some(candidates[0])),
            _ => Err(NativePendingError::AmbiguousPendingGroup),
        };
    };
    // A retained group only belongs to this request when the client is actually
    // answering the tool calls it owns. Codex hands the same session anchor to
    // sub-agent threads, so a different conversation under one anchor must read
    // as a fresh request instead of a broken continuation.
    let mut answering = candidates
        .into_iter()
        .filter(|group| group_is_answered_by_replay(group, input));
    let Some(first) = answering.next() else {
        return Ok(None);
    };
    if answering.next().is_some() {
        return Err(NativePendingError::AmbiguousPendingGroup);
    }
    Ok(Some(first))
}

/// Whether the incoming replay carries at least one tool call this retained
/// group owns. Every continuation answers those calls, so a replay that
/// mentions none of them belongs to a different conversation.
fn group_is_answered_by_replay(group: &PendingNativeToolGroup, input: &[Value]) -> bool {
    group.client_calls.iter().any(|call| {
        input.iter().any(|item| {
            item.get("call_id").and_then(Value::as_str) == Some(call.call_id.as_str())
        })
    })
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

/// Whether an item is the client's own answer to a provider call CodeSeeX does
/// not verify, such as the `tool_search_output` Codex replays for a provider
/// `tool_search_call`. Only an answer to a call the retained group actually
/// carries is recognised; every other stray item still fails closed.
fn is_untracked_client_answer(item: &Value, untracked_call_ids: &BTreeSet<String>) -> bool {
    item_type(item) == "tool_search_output"
        && item
            .get("call_id")
            .and_then(Value::as_str)
            .is_some_and(|call_id| untracked_call_ids.contains(call_id))
}

/// Whether a replayed tool output actually carries a result payload.
///
/// Codex answers most client tools with a text `output`, but a tool that returns
/// media answers with an `output` array of content parts: `view_image` replays
/// `[{"type":"input_image",...}]`. The upstream compiler also reads `content`
/// and `result`, so accepting every shape it understands keeps a valid answer
/// from failing the turn, while an item with no payload at all still fails
/// closed.
fn client_tool_output_has_payload(item: &Value) -> bool {
    ["output", "content", "result"]
        .iter()
        .any(|field| item.get(*field).is_some_and(|value| !value.is_null()))
}

fn collect_client_outputs(
    input: &[Value],
    calls: &[NativeToolCall],
    untracked_call_ids: &BTreeSet<String>,
) -> Result<(Vec<Value>, Vec<Value>), NativePendingError> {
    // A provider tool group and its outputs are an ordered protocol unit.
    // Do not use a map to "fix" an out-of-order client replay: that would
    // turn a malformed or divergent replay into a different request.
    //
    // Display-only items CodeSeeX added for the conversation view are dropped
    // first: they are never sent upstream, so they cannot own a tool output.
    let input = input
        .iter()
        .filter(|item| !is_display_only_item(item))
        .cloned()
        .collect::<Vec<_>>();
    let input = input.as_slice();
    // The client's own answers stay exactly where it replayed them, so the
    // continuation keeps the order the client sent instead of moving a
    // `tool_search_output` behind the verified outputs.
    let mut answers = Vec::with_capacity(calls.len());
    let mut next = 0_usize;
    for (index, call) in calls.iter().enumerate() {
        loop {
            let Some(item) = input.get(next) else {
                return Err(NativePendingError::MissingClientToolOutput {
                    call_id: call.call_id.clone(),
                });
            };
            if is_untracked_client_answer(item, untracked_call_ids) {
                answers.push(item.clone());
                next += 1;
                continue;
            }
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
            if item_type != expected_type || !client_tool_output_has_payload(item) {
                return Err(NativePendingError::InvalidClientToolOutput {
                    call_id: call_id.to_owned(),
                });
            }
            answers.push(item.clone());
            next += 1;
            break;
        }
    }
    let suffix = input[next..].to_vec();
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
    Ok((answers, suffix))
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
    let mut injected_through = 0_usize;
    for segment in &group.injected_items {
        if segment.offset < injected_through || segment.offset > group.authoritative_input.len() {
            return Err(NativePendingError::InvalidPendingGroup(
                "injected hosted rounds did not map onto the client-visible anchor",
            ));
        }
        injected_through = segment.offset;
    }
    Ok(())
}

/// Whether an item is provider display-only thinking. Codex may drop its own
/// prior `reasoning` items when it replays history, or re-emit them reshaped.
fn is_display_only_reasoning(item: &Value) -> bool {
    matches!(item.get("type").and_then(Value::as_str), Some("reasoning"))
}

/// Plain text of a message item, whether the client sent the content as a
/// string or as an array of parts.
fn message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let mut text = String::new();
    for part in content.as_array()? {
        let Some(part) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(part);
    }
    Some(text)
}

/// Whether an item is the readable thinking message CodeSeeX adds for the
/// conversation view, as it comes back from the client.
///
/// CodeSeeX marks that artifact with `codeseex_display_only`, but Codex
/// re-serializes history through its own item types before a request and a
/// `message` item cannot carry the marker, so only the text survives. The Chat
/// transport has always recognized the artifact by that text prefix; the native
/// path has to use the same signal, because the marker is gone by the time the
/// item is replayed.
fn is_thinking_display_message(item: &Value) -> bool {
    item_type(item) == "message"
        && item.get("role").and_then(Value::as_str) == Some("assistant")
        && message_text(item)
            .map(|text| {
                text.trim_start()
                    .starts_with(thinking_display_prefix().trim_end())
            })
            .unwrap_or(false)
}

/// Compact identity used only for diagnostics: the same provider item must keep
/// the same type and id, so a mismatch can be read straight off the log.
fn compact_item_identity(item: &Value) -> String {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| item.get("call_id").and_then(Value::as_str))
        .unwrap_or_default();
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    match (id.is_empty(), name.is_empty()) {
        (false, false) => format!("{kind}:{id}:{name}"),
        (false, true) => format!("{kind}:{id}"),
        (true, false) => format!("{kind}:{name}"),
        (true, true) => kind.to_owned(),
    }
}

fn item_type(item: &Value) -> &str {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn compact_identities(items: &[Value], limit: usize) -> Vec<String> {
    let mut identities = items
        .iter()
        .take(limit)
        .map(compact_item_identity)
        .collect::<Vec<_>>();
    if items.len() > limit {
        identities.push(format!("...{} more", items.len() - limit));
    }
    identities
}

/// Whether a provider tool call CodeSeeX forwarded still carries the same
/// identity in the client's replay. The call id and the tool payload are the
/// protocol unit the upstream provider matches outputs against, so they must be
/// equal; CodeSeeX's own stored copy is what reaches the model either way.
fn replayed_visible_call_matches(stored: &Value, replayed: &Value, input_field: &str) -> bool {
    ["call_id", "name", input_field].iter().all(|field| {
        stored.get(*field).and_then(Value::as_str) == replayed.get(*field).and_then(Value::as_str)
    })
}

/// Whether the client's replay of one provider item is the same protocol unit
/// CodeSeeX forwarded.
///
/// Codex re-serializes history through its own item types before sending a
/// request, so the copy that comes back is not byte-identical to the provider
/// group CodeSeeX observed:
/// - an item id that is not prefix-qualified (a bare provider UUID) is dropped,
///   because `ResponseItemId::is_prefixed` only keeps ids like `msg_...`; and
/// - anything Codex's own item shape cannot carry (such as `status` on a
///   `function_call`) is dropped as an unknown field.
///
/// The continuation sent upstream is rebuilt from the coordinator's stored copy,
/// so this comparison only has to prove that the client replayed the same
/// protocol unit: the same item kinds in order, the same message role, and the
/// same tool call identity. Injected, reordered, or edited calls still fail
/// closed.
fn replayed_visible_item_matches(stored: &Value, replayed: &Value) -> bool {
    let kind = item_type(stored);
    if kind != item_type(replayed) {
        return false;
    }
    match kind {
        "function_call" => replayed_visible_call_matches(stored, replayed, "arguments"),
        "custom_tool_call" => replayed_visible_call_matches(stored, replayed, "input"),
        "message" => {
            stored.get("role").and_then(Value::as_str)
                == replayed.get("role").and_then(Value::as_str)
        }
        _ => true,
    }
}

/// Walks the provider output group Codex observed against the items Codex
/// actually replayed, returning everything after that group.
///
/// Codex may omit its own prior `reasoning` items, or re-emit them in a
/// different shape, because thinking is display-only for the client. Tolerating
/// that here is safe: the continuation sent upstream is rebuilt from the
/// coordinator's own stored copy of the group, so dropping client-side thinking cannot change
/// what the model sees. Every other item must still line up in order and
/// identity, so injected, reordered, or edited history still fails closed.
fn after_visible_group<'a>(input: &'a [Value], stored: &[Value]) -> Option<&'a [Value]> {
    let mut stored_index = 0;
    let mut input_index = 0;
    while stored_index < stored.len() && input_index < input.len() {
        if is_group_invisible(&stored[stored_index]) {
            stored_index += 1;
            continue;
        }
        if is_group_invisible(&input[input_index]) {
            input_index += 1;
            continue;
        }
        if !replayed_visible_item_matches(&stored[stored_index], &input[input_index]) {
            return None;
        }
        stored_index += 1;
        input_index += 1;
    }
    stored[stored_index..]
        .iter()
        .all(is_group_invisible)
        .then(|| &input[input_index..])
}

/// Items that never belong to the provider's own conversation: the thinking
/// Codex replays in its own shape, and the display-only artifacts CodeSeeX
/// adds for the conversation view. Both are dropped before anything reaches
/// upstream, so neither can answer a tool call.
fn is_group_invisible(item: &Value) -> bool {
    is_display_only_reasoning(item) || is_display_only_item(item)
}

/// Whether an item exists only for the client's conversation view.
///
/// CodeSeeX marks the artifacts it adds itself; they are removed again by the
/// upstream boundary, so they must not shift the tool group a continuation
/// answers. The marker only survives until Codex rewrites its own history, so
/// the thinking message is also recognized by the text it was given.
pub(crate) fn is_display_only_item(item: &Value) -> bool {
    item.get("codeseex_display_only").is_some()
        || item
            .get("metadata")
            .and_then(|metadata| metadata.get("codeseex_display_only"))
            .and_then(Value::as_bool)
            == Some(true)
        || is_thinking_display_message(item)
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
        group_with_injected(
            authoritative_input,
            provider_output,
            visible_provider_output,
            local_output_items,
            Vec::new(),
            client_calls,
        )
    }

    fn group_with_injected(
        authoritative_input: Vec<Value>,
        provider_output: Vec<Value>,
        visible_provider_output: Vec<Value>,
        local_output_items: Vec<Value>,
        injected_items: Vec<NativeInjectedItems>,
        client_calls: Vec<NativeToolCall>,
    ) -> PendingNativeToolGroup {
        PendingNativeToolGroup::new(
            "resp_local_1",
            &request(Vec::new()),
            authoritative_input,
            injected_items,
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
    fn codex_replay_omitting_its_own_reasoning_still_continues() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let reasoning = json!({ "type": "reasoning", "id": "rs_1", "summary": "thinking" });
        let provider = vec![
            reasoning.clone(),
            json!({
                "type": "function_call",
                "call_id": "call_shell",
                "name": "shell_command",
                "arguments": "{}",
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
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        // A real client can drop its own prior thinking from the replay; the
        // upstream continuation is rebuilt from the stored copy, so this must
        // not be treated as a tampered history.
        let continuation = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[1].clone(),
                json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
            ]))
            .unwrap()
            .unwrap();
        assert_eq!(continuation.client_output_count, 1);
        assert_eq!(
            Value::Array(continuation.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                { "type": "reasoning", "id": "rs_1", "summary": "thinking" },
                { "type": "function_call", "call_id": "call_shell", "name": "shell_command", "arguments": "{}", "status": "completed" },
                { "type": "function_call_output", "call_id": "call_shell", "output": "ok" }
            ])
        );
        groups.settle("resp_local_1");
    }

    #[test]
    fn injected_or_edited_history_still_fails_closed() {
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

        let injected = groups.continuation_for(&request(vec![
            authoritative[0].clone(),
            json!({ "type": "message", "role": "assistant", "content": "forged" }),
            provider[0].clone(),
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]));
        assert!(matches!(
            injected,
            Err(NativePendingError::VisibleProviderOutputMismatch { .. })
        ));

        let mut edited = provider[0].clone();
        edited["name"] = json!("forged_command");
        let edited_replay = groups.continuation_for(&request(vec![
            authoritative[0].clone(),
            edited,
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]));
        let Err(NativePendingError::VisibleProviderOutputMismatch { stored, replayed }) =
            edited_replay
        else {
            panic!("edited history item must fail closed");
        };
        assert_eq!(stored, vec!["function_call:call_shell:shell_command"]);
        assert_eq!(
            replayed,
            vec![
                "function_call:call_shell:forged_command",
                "function_call_output:call_shell"
            ]
        );
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

    #[test]
    fn codex_normalized_replay_of_provider_items_still_continues() {
        // Before a request Codex drops item ids that are not prefix-qualified
        // and any provider field its own item shape cannot carry. The client
        // still replayed the same protocol unit, so the continuation must be
        // accepted and rebuilt from CodeSeeX's stored copy of the group.
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![
            json!({
                "type": "message",
                "id": "c53e120a-544c-49fc-8314-6ebf26c2a122",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "working" }]
            }),
            json!({
                "type": "function_call",
                "id": "2997568c-4abb-4010-999b-7a914321067a",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwsh\"}",
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
                vec![function_call(
                    "call_00_mDEXZuYtEFUSlPgouz1p8384",
                    "exec_command",
                )],
            ))
            .unwrap();

        let continuation = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "working" }]
                }),
                json!({
                    "type": "function_call",
                    "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"pwsh\"}"
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                    "output": "ok"
                }),
            ]))
            .unwrap()
            .unwrap();

        // Upstream keeps the stored provider copy, including the provider id and
        // `status` the client dropped, so the model sees exactly what CodeSeeX
        // forwarded.
        assert_eq!(
            Value::Array(continuation.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                provider[0].clone(),
                provider[1].clone(),
                {
                    "type": "function_call_output",
                    "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                    "output": "ok"
                }
            ])
        );
    }

    #[test]
    fn replayed_group_still_has_to_keep_the_forwarded_item_identity() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![
            json!({
                "type": "message",
                "id": "c53e120a-544c-49fc-8314-6ebf26c2a122",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "working" }]
            }),
            json!({
                "type": "function_call",
                "id": "2997568c-4abb-4010-999b-7a914321067a",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwsh\"}",
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
                vec![function_call(
                    "call_00_mDEXZuYtEFUSlPgouz1p8384",
                    "exec_command",
                )],
            ))
            .unwrap();

        // Dropping a forwarded item, or editing the call payload, would shift or
        // forge the group, so the tolerant replay check must still fail closed.
        let dropped_message = groups.continuation_for(&request(vec![
            authoritative[0].clone(),
            json!({
                "type": "function_call",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"pwsh\"}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "output": "ok"
            }),
        ]));
        assert!(matches!(
            dropped_message,
            Err(NativePendingError::VisibleProviderOutputMismatch { .. })
        ));

        let edited_arguments = groups.continuation_for(&request(vec![
            authoritative[0].clone(),
            json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "working" }]
            }),
            json!({
                "type": "function_call",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "name": "exec_command",
                "arguments": "{\"cmd\":\"rm -rf /\"}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_00_mDEXZuYtEFUSlPgouz1p8384",
                "output": "ok"
            }),
        ]));
        assert!(matches!(
            edited_arguments,
            Err(NativePendingError::VisibleProviderOutputMismatch { .. })
        ));
    }

    #[test]
    fn executed_local_rounds_are_replayed_before_the_retained_group() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let search_call = json!({
            "type": "function_call",
            "id": "fc_search_1",
            "call_id": "call_search",
            "name": "web_search",
            "arguments": "{\"query\":\"codeseex\"}",
            "status": "completed"
        });
        let search_output = json!({
            "type": "function_call_output",
            "call_id": "call_search",
            "output": "search result"
        });
        let client = json!({
            "type": "function_call",
            "id": "fc_shell_1",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        });
        let groups = NativePendingToolGroups::default();
        groups
            .register(group_with_injected(
                authoritative.clone(),
                vec![client.clone()],
                vec![client.clone()],
                Vec::new(),
                vec![NativeInjectedItems {
                    offset: authoritative.len(),
                    items: vec![search_call.clone(), search_output.clone()],
                }],
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let continuation = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                client.clone(),
                json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
            ]))
            .unwrap()
            .unwrap();
        // The client never saw the hosted round, so only CodeSeeX can replay it,
        // and it has to come before the group that it was executed for.
        assert_eq!(
            Value::Array(continuation.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                search_call,
                search_output,
                client,
                { "type": "function_call_output", "call_id": "call_shell", "output": "ok" }
            ])
        );
        assert_eq!(continuation.injected_items.len(), 1);
        assert_eq!(continuation.injected_items[0].offset, 1);
    }

    #[test]
    fn injected_local_rounds_keep_their_offset_across_a_nested_continuation() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let search_call = json!({
            "type": "function_call",
            "id": "fc_search_1",
            "call_id": "call_search",
            "name": "web_search",
            "arguments": "{}",
            "status": "completed"
        });
        let search_output = json!({
            "type": "function_call_output",
            "call_id": "call_search",
            "output": "search result"
        });
        let shell_call = json!({
            "type": "function_call",
            "id": "fc_shell_1",
            "call_id": "call_shell",
            "name": "shell_command",
            "arguments": "{}",
            "status": "completed"
        });
        let shell_output =
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" });
        let patch_call = json!({
            "type": "function_call",
            "id": "fc_patch_1",
            "call_id": "call_patch",
            "name": "apply_patch",
            "arguments": "{}",
            "status": "completed"
        });
        let patch_output =
            json!({ "type": "function_call_output", "call_id": "call_patch", "output": "Done" });
        let injected = vec![NativeInjectedItems {
            offset: authoritative.len(),
            items: vec![search_call.clone(), search_output.clone()],
        }];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group_with_injected(
                authoritative.clone(),
                vec![shell_call.clone()],
                vec![shell_call.clone()],
                Vec::new(),
                injected.clone(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();
        let first = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                shell_call.clone(),
                shell_output.clone(),
            ]))
            .unwrap()
            .unwrap();
        // The second group is retained while the hosted round is still pending;
        // its offset stays relative to the anchor that predates it.
        groups
            .register(group_with_injected(
                vec![
                    authoritative[0].clone(),
                    shell_call.clone(),
                    shell_output.clone(),
                ],
                vec![patch_call.clone()],
                vec![patch_call.clone()],
                Vec::new(),
                first.injected_items,
                vec![function_call("call_patch", "apply_patch")],
            ))
            .unwrap();
        let second = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                shell_call.clone(),
                shell_output.clone(),
                patch_call.clone(),
                patch_output.clone(),
            ]))
            .unwrap()
            .unwrap();
        assert_eq!(
            Value::Array(second.merged_input),
            json!([
                { "type": "message", "role": "user", "content": "start" },
                search_call,
                search_output,
                shell_call,
                shell_output,
                patch_call,
                patch_output
            ])
        );
    }

    #[test]
    fn unrelated_conversation_under_one_anchor_is_not_a_continuation() {
        // Codex hands the parent's prompt_cache_key to sub-agent threads, so a
        // child turn arrives under the parent's anchor while the parent's
        // `spawn_agent` call is still pending. It answers none of that group's
        // calls, so it is a fresh conversation and not a broken continuation.
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "parent turn" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_spawn",
            "name": "spawn_agent",
            "arguments": "{}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative,
                provider.clone(),
                provider,
                Vec::new(),
                vec![function_call("call_spawn", "spawn_agent")],
            ))
            .unwrap();

        let mut child = request(vec![json!({
            "type": "message",
            "role": "user",
            "content": "child prompt"
        })]);
        child["previous_response_id"] = Value::Null;

        assert!(groups.continuation_for(&child).unwrap().is_none());
        assert_eq!(groups.pending_count(), 1);
    }

    #[test]
    fn display_only_thinking_never_shifts_the_tool_group_it_precedes() {
        // The shape Codex actually replays: CodeSeeX's readable thinking message
        // arrives before the provider's own reasoning item, and Codex has
        // dropped `codeseex_display_only` because its `message` item cannot
        // carry it. The message is still stripped before anything reaches
        // upstream, so it must not shift the group a continuation answers.
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![
            json!({
                "type": "reasoning",
                "id": "rs_provider",
                "summary": [{ "type": "summary_text", "text": "step" }],
                "content": null,
                "encrypted_content": "blob"
            }),
            json!({
                "type": "function_call",
                "call_id": "call_shell",
                "name": "shell_command",
                "arguments": "{}",
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
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();

        let mut replay = request(vec![
            authoritative[0].clone(),
            json!({
                "type": "message",
                "id": "msg_display",
                "role": "assistant",
                "phase": "commentary",
                "content": [{ "type": "output_text", "text": "**DeepSeek Thinking**\n> step" }]
            }),
            provider[0].clone(),
            provider[1].clone(),
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]);
        replay["previous_response_id"] = Value::Null;

        let continuation = groups
            .continuation_for(&replay)
            .unwrap()
            .expect("the group is still answered");
        assert_eq!(continuation.client_output_count, 1);
        assert!(
            !continuation
                .merged_input
                .iter()
                .any(is_display_only_item),
            "display-only thinking must not be forwarded"
        );
        assert_eq!(
            continuation.merged_input.len(),
            authoritative.len() + provider.len() + 1,
            "the replayed group is rebuilt from the stored copy, not the client's"
        );
    }

    #[test]
    fn a_display_message_codex_edited_away_from_the_prefix_still_fails_closed() {
        // Only the artifact CodeSeeX actually wrote is tolerated. Any other
        // assistant message the client adds inside the group is a divergence.
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
            json!({
                "type": "message",
                "id": "msg_edited",
                "role": "assistant",
                "phase": "commentary",
                "content": [{ "type": "output_text", "text": "an unrelated aside" }]
            }),
            provider[0].clone(),
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]);
        replay["previous_response_id"] = Value::Null;

        assert!(matches!(
            groups.continuation_for(&replay),
            Err(NativePendingError::VisibleProviderOutputMismatch { .. })
        ));
    }

    #[test]
    fn client_owned_tool_search_output_keeps_its_place_between_verified_outputs() {
        // Live shape: the provider called Codex's own `tool_search` twice and
        // then `exec_command` inside one group. CodeSeeX forwards the two
        // `tool_search_call` items but only verifies the `exec_command` output,
        // and Codex replays both `tool_search_output` items before it. Reading
        // those as a shifted tool output rejected the whole turn.
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![
            json!({
                "type": "tool_search_call",
                "id": "43f35b3b-7cd7-421f-a8a3-e4f915d5e79a",
                "call_id": "call_00_search",
                "status": "completed",
                "execution": "client",
                "arguments": { "limit": 10, "query": "search the internet" }
            }),
            json!({
                "type": "tool_search_call",
                "id": "5a60265f-eb49-40d8-87f3-7c7afb691fe8",
                "call_id": "call_01_search",
                "status": "completed",
                "execution": "client",
                "arguments": { "limit": 10, "query": "browser tool" }
            }),
            json!({
                "type": "function_call",
                "call_id": "call_02_shell",
                "name": "exec_command",
                "arguments": "{}",
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
                vec![function_call("call_02_shell", "exec_command")],
            ))
            .unwrap();

        let replay = vec![
            authoritative[0].clone(),
            provider[0].clone(),
            provider[1].clone(),
            provider[2].clone(),
            json!({
                "type": "tool_search_output",
                "call_id": "call_00_search",
                "status": "completed",
                "execution": "client",
                "tools": []
            }),
            json!({
                "type": "tool_search_output",
                "call_id": "call_01_search",
                "status": "completed",
                "execution": "client",
                "tools": []
            }),
            json!({ "type": "function_call_output", "call_id": "call_02_shell", "output": "ok" }),
        ];
        let continuation = groups
            .continuation_for(&request(replay.clone()))
            .unwrap()
            .expect("the group is still answered");

        assert_eq!(continuation.client_output_count, 1);
        assert_eq!(
            continuation.merged_input, replay,
            "the client's own answers keep the order it replayed them in"
        );
    }

    #[test]
    fn a_tool_search_output_the_group_never_called_still_fails_closed() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_shell",
            "name": "exec_command",
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
                vec![function_call("call_shell", "exec_command")],
            ))
            .unwrap();

        let error = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[0].clone(),
                json!({
                    "type": "tool_search_output",
                    "call_id": "call_09_invented",
                    "status": "completed",
                    "execution": "client",
                    "tools": []
                }),
                json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
            ]))
            .unwrap_err();

        assert_eq!(
            error,
            NativePendingError::ClientToolOutputOrderMismatch {
                expected_call_id: "call_shell".to_owned(),
                actual_call_id: "non_tool_item".to_owned(),
            }
        );
    }

    #[test]
    fn client_image_output_replays_content_parts_instead_of_text() {
        // Live shape: Codex answered `view_image` with a `function_call_output`
        // whose `output` is an array of content parts. Requiring a text output
        // rejected the whole turn with `tool_output_required`.
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_view_image",
            "name": "view_image",
            "arguments": "{\"path\":\"probe.png\",\"detail\":\"high\"}",
            "status": "completed"
        })];
        let groups = NativePendingToolGroups::default();
        groups
            .register(group(
                authoritative.clone(),
                provider.clone(),
                provider.clone(),
                Vec::new(),
                vec![function_call("call_view_image", "view_image")],
            ))
            .unwrap();

        let replay = vec![
            authoritative[0].clone(),
            provider[0].clone(),
            json!({
                "type": "function_call_output",
                "call_id": "call_view_image",
                "output": [{
                    "type": "input_image",
                    "image_url": "data:image/png;base64,AAAA",
                    "detail": "high"
                }]
            }),
        ];
        let continuation = groups
            .continuation_for(&request(replay.clone()))
            .unwrap()
            .expect("the image answer completes the retained group");

        assert_eq!(continuation.client_output_count, 1);
        assert_eq!(continuation.merged_input, replay);
    }

    #[test]
    fn a_client_output_without_any_payload_still_fails_closed() {
        let authoritative = vec![json!({ "type": "message", "role": "user", "content": "start" })];
        let provider = vec![json!({
            "type": "function_call",
            "call_id": "call_view_image",
            "name": "view_image",
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
                vec![function_call("call_view_image", "view_image")],
            ))
            .unwrap();

        let error = groups
            .continuation_for(&request(vec![
                authoritative[0].clone(),
                provider[0].clone(),
                json!({ "type": "function_call_output", "call_id": "call_view_image" }),
            ]))
            .unwrap_err();

        assert_eq!(
            error,
            NativePendingError::InvalidClientToolOutput {
                call_id: "call_view_image".to_owned(),
            }
        );
    }

    #[test]
    fn display_only_detection_covers_marked_and_markerless_thinking_messages() {
        assert!(is_display_only_item(&json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "**DeepSeek Thinking**\n> step" }],
            "codeseex_display_only": "thinking_markdown"
        })));
        assert!(is_display_only_item(&json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "**DeepSeek Thinking**\n> step" }]
        })));
        // A user could write the same words, and a normal answer is not an
        // artifact: neither may be dropped from the provider's conversation.
        assert!(!is_display_only_item(&json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "**DeepSeek Thinking**\n> step" }]
        })));
        assert!(!is_display_only_item(&json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": "the answer" }]
        })));
    }

    #[test]
    fn diverged_replay_that_still_answers_the_group_fails_closed() {
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
                provider.clone(),
                provider,
                Vec::new(),
                vec![function_call("call_shell", "shell_command")],
            ))
            .unwrap();

        let mut replay = request(vec![
            json!({ "type": "message", "role": "user", "content": "rewritten start" }),
            json!({
                "type": "function_call",
                "call_id": "call_shell",
                "name": "shell_command",
                "arguments": "{}"
            }),
            json!({ "type": "function_call_output", "call_id": "call_shell", "output": "ok" }),
        ]);
        replay["previous_response_id"] = Value::Null;

        let error = groups.continuation_for(&replay).unwrap_err();
        assert!(
            matches!(error, NativePendingError::AuthoritativePrefixMismatch { .. }),
            "{error:?}"
        );
        assert!(error.diagnostic().is_some());
        assert!(error.message().contains("Stored: ["));
    }
}
