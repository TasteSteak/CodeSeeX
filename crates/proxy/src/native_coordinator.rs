//! Bookkeeping for the upstream rounds CodeSeeX injects on its own.
//!
//! Codex owns the conversation: the client's `input` array is authoritative and
//! is forwarded upstream as it arrives. Nothing here validates, rewrites, or
//! rejects that array, and nothing here models the client's tool outputs.
//!
//! The one thing CodeSeeX has to remember is the hosted tool round it executes
//! itself. Codex never sees those items, so it can never replay them, while the
//! provider already produced them in that order. The rounds are therefore
//! retained per response id and spliced back into the client's own array at the
//! offset inside the client-visible anchor where CodeSeeX injected them.
//!
//! The state is RAM-only and advisory: when nothing matches, the client's
//! request is forwarded unchanged and the caller records what happened. A
//! restart, an eviction, or an unrecognised continuation can therefore cost at
//! most the replayed hosted round, never the user's turn.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_PENDING_NATIVE_GROUPS: usize = 64;
const PENDING_NATIVE_GROUP_TTL: Duration = Duration::from_secs(30 * 60);

/// One block of upstream input that CodeSeeX itself injected: a hosted tool
/// round it executed inside the same turn. The provider already saw these
/// items, but Codex never did, so they are not part of the client-visible
/// anchor. They are replayed at the offset in that anchor where CodeSeeX
/// injected them, which keeps a nested continuation in provider order.
#[derive(Debug, Clone)]
pub(crate) struct NativeInjectedItems {
    pub(crate) offset: usize,
    pub(crate) items: Vec<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct NativePendingContinuation {
    pub(crate) pending_response_id: String,
    /// The client's own items with the retained rounds spliced back in.
    pub(crate) merged_input: Vec<Value>,
    /// Rounds CodeSeeX executed itself, so a nested continuation can keep
    /// replaying them in the order the provider already saw them.
    pub(crate) injected_items: Vec<NativeInjectedItems>,
}

/// RAM-only state for the hosted rounds CodeSeeX injected inside one turn.
#[derive(Clone, Default)]
pub(crate) struct NativePendingToolGroups {
    groups: Arc<Mutex<BTreeMap<String, PendingNativeToolGroup>>>,
    /// Response ids dropped by the TTL or the capacity bound, waiting to be
    /// reported. Eviction is normal housekeeping, but it must never be silent:
    /// a dropped round is a hosted round the provider will not see again.
    evicted: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
pub(crate) struct PendingNativeToolGroup {
    pub(crate) response_id: String,
    pub(crate) request_anchor: Option<String>,
    /// Call ids the client owns in this group. They are used only to recognise
    /// which retained round a later request continues; the client's answers are
    /// never inspected or required.
    pub(crate) client_call_ids: Vec<String>,
    pub(crate) injected_items: Vec<NativeInjectedItems>,
    created_at: Instant,
}

impl NativePendingToolGroups {
    pub(crate) fn register(&self, group: PendingNativeToolGroup) {
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut evicted = self
            .evicted
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut groups, &mut evicted);
        groups.insert(group.response_id.clone(), group);
        trim_to_capacity(&mut groups, &mut evicted);
    }

    /// The rounds this request has to replay, if any. `None` means the request
    /// is forwarded exactly as the client sent it.
    pub(crate) fn continuation_for(&self, request: &Value) -> Option<NativePendingContinuation> {
        let mut groups = self
            .groups
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut evicted = self
            .evicted
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        prune_expired(&mut groups, &mut evicted);
        let group = matching_group(&groups, request)?;
        let input = request.get("input").and_then(Value::as_array)?;
        Some(group.continuation(input))
    }

    /// Forgets a retained group once the request that consumed it was accepted
    /// upstream. A failed dispatch keeps it, so a retry still replays it.
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

    /// Response ids dropped by the TTL or the capacity bound since the last
    /// call. The caller records them so an evicted round leaves a trace.
    pub(crate) fn take_evictions(&self) -> Vec<String> {
        let mut evicted = self
            .evicted
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *evicted)
    }

    /// The hash of a retained round that belongs to this request's session
    /// anchor, if any. Used only to explain a request that arrived without its
    /// round: the client's request is never changed either way.
    pub(crate) fn has_round_for(&self, request: &Value) -> Option<String> {
        let anchor = request_anchor(request)?;
        self.groups
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .find(|group| group.request_anchor.as_deref() == Some(anchor.as_str()))
            .map(|group| short_hash(&group.response_id))
    }
}

impl PendingNativeToolGroup {
    pub(crate) fn new(
        response_id: impl Into<String>,
        request: &Value,
        injected_items: Vec<NativeInjectedItems>,
        client_call_ids: Vec<String>,
    ) -> Self {
        Self {
            response_id: response_id.into(),
            request_anchor: request_anchor(request),
            client_call_ids,
            injected_items,
            created_at: Instant::now(),
        }
    }

    pub(crate) fn diagnostic(&self) -> Value {
        json!({
            "pending": true,
            "response_id_hash": short_hash(&self.response_id),
            "session_anchor": self.request_anchor.as_ref().map(|value| short_hash(value)),
            "injected_items": self
                .injected_items
                .iter()
                .map(|segment| segment.items.len())
                .sum::<usize>(),
            "client_calls": self.client_call_ids.len(),
        })
    }

    /// The client's own items with CodeSeeX's rounds put back where they were
    /// injected. The client's items are copied through untouched.
    fn continuation(&self, input: &[Value]) -> NativePendingContinuation {
        let injected_count = self
            .injected_items
            .iter()
            .map(|segment| segment.items.len())
            .sum::<usize>();
        let mut merged_input = Vec::with_capacity(input.len() + injected_count);
        let mut replayed = 0_usize;
        for segment in &self.injected_items {
            let offset = segment.offset.min(input.len()).max(replayed);
            merged_input.extend(input[replayed..offset].iter().cloned());
            merged_input.extend(segment.items.iter().cloned());
            replayed = offset;
        }
        merged_input.extend(input[replayed..].iter().cloned());
        NativePendingContinuation {
            pending_response_id: self.response_id.clone(),
            merged_input,
            injected_items: self.injected_items.clone(),
        }
    }
}

/// The retained round that this request continues.
///
/// The client's own `previous_response_id` is the exact link. Full-replay
/// clients send none, so the session anchor plus the call ids the client owns
/// in the group decide instead: a sub-agent thread shares its parent's anchor
/// but never mentions the parent's calls, and must read as a new conversation.
/// Ambiguity resolves to the most recent group. Nothing here can reject a
/// request, and no client item is required to be well formed.
fn matching_group<'a>(
    groups: &'a BTreeMap<String, PendingNativeToolGroup>,
    request: &Value,
) -> Option<&'a PendingNativeToolGroup> {
    let previous = request
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(previous) = previous {
        if let Some(group) = groups.get(previous) {
            return Some(group);
        }
    }
    let anchor = request_anchor(request)?;
    let input = request.get("input").and_then(Value::as_array)?;
    groups
        .values()
        .filter(|group| group.request_anchor.as_deref() == Some(anchor.as_str()))
        .filter(|group| group_is_continued_by(group, input))
        .max_by_key(|group| group.created_at)
}

/// Whether the incoming replay carries at least one tool call this retained
/// group owns. Every continuation answers those calls, so a replay that
/// mentions none of them belongs to a different conversation.
fn group_is_continued_by(group: &PendingNativeToolGroup, input: &[Value]) -> bool {
    group.client_call_ids.iter().any(|call_id| {
        input
            .iter()
            .any(|item| item.get("call_id").and_then(Value::as_str) == Some(call_id.as_str()))
    })
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

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A short, loggable identity for one retained round.
pub(crate) fn round_hash(response_id: &str) -> String {
    short_hash(response_id)
}

fn prune_expired(groups: &mut BTreeMap<String, PendingNativeToolGroup>, evicted: &mut Vec<String>) {
    let now = Instant::now();
    groups.retain(|response_id, group| {
        if now.duration_since(group.created_at) < PENDING_NATIVE_GROUP_TTL {
            return true;
        }
        evicted.push(response_id.clone());
        false
    });
}

fn trim_to_capacity(
    groups: &mut BTreeMap<String, PendingNativeToolGroup>,
    evicted: &mut Vec<String>,
) {
    while groups.len() > MAX_PENDING_NATIVE_GROUPS {
        let Some(oldest) = groups
            .iter()
            .min_by_key(|(_, group)| group.created_at)
            .map(|(response_id, _)| response_id.clone())
        else {
            return;
        };
        groups.remove(&oldest);
        evicted.push(oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(input: Vec<Value>) -> Value {
        json!({
            "prompt_cache_key": "thread-a",
            "client_metadata": { "x-codex-installation-id": "install-a" },
            "input": input,
        })
    }

    fn hosted_round() -> Vec<NativeInjectedItems> {
        vec![NativeInjectedItems {
            offset: 1,
            items: vec![
                json!({
                    "type": "function_call",
                    "call_id": "call_hosted_search",
                    "name": "web_search",
                    "arguments": "{}"
                }),
                json!({
                    "type": "function_call_output",
                    "call_id": "call_hosted_search",
                    "output": "search result"
                }),
            ],
        }]
    }

    fn register_hosted_round(groups: &NativePendingToolGroups) {
        let group = PendingNativeToolGroup::new(
            "resp_local_1",
            &request(Vec::new()),
            hosted_round(),
            vec!["call_client_shell".to_owned()],
        );
        groups.register(group);
    }

    fn client_replay() -> Vec<Value> {
        vec![
            json!({ "type": "message", "role": "user", "content": "start" }),
            json!({
                "type": "function_call",
                "call_id": "call_client_shell",
                "name": "exec_command",
                "arguments": "{}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_client_shell",
                "output": "ok"
            }),
        ]
    }

    #[test]
    fn injected_rounds_are_spliced_into_the_clients_own_items() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        let replay = client_replay();

        let Some(continuation) = groups.continuation_for(&request(replay.clone())) else {
            panic!("the retained round belongs to this request");
        };

        assert_eq!(continuation.pending_response_id, "resp_local_1");
        assert_eq!(continuation.injected_items.len(), 1);
        assert_eq!(
            continuation.merged_input,
            vec![
                replay[0].clone(),
                hosted_round()[0].items[0].clone(),
                hosted_round()[0].items[1].clone(),
                replay[1].clone(),
                replay[2].clone(),
            ],
            "the injected round keeps its offset inside the client's own items"
        );
    }

    #[test]
    fn the_clients_own_items_are_never_rewritten_or_dropped() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        // Shapes the removed validator used to argue about: a client-owned
        // answer to `tool_search`, provider reasoning, and a media payload.
        let mut replay = vec![
            json!({ "type": "message", "role": "user", "content": "start" }),
            json!({ "type": "reasoning", "id": "rs_1", "summary": [] }),
            json!({ "type": "tool_search_output", "call_id": "call_00_search", "tools": [] }),
            json!({ "type": "function_call", "call_id": "call_other", "name": "exec_command" }),
            json!({ "type": "function_call_output", "call_id": "call_other", "output": "ok" }),
        ];
        replay.push(json!({
            "type": "function_call_output",
            "call_id": "call_client_shell",
            "output": [{ "type": "input_image", "image_url": "data:image/png;base64,AAAA" }]
        }));

        let Some(continuation) = groups.continuation_for(&request(replay.clone())) else {
            panic!("the retained round belongs to this request");
        };

        let mut expected = replay[..1].to_vec();
        expected.extend(hosted_round()[0].items.clone());
        expected.extend(replay[1..].iter().cloned());
        assert_eq!(continuation.merged_input, expected);
    }

    #[test]
    fn a_previous_response_id_link_wins_over_the_anchor() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        let mut linked = request(vec![
            json!({ "type": "message", "role": "user", "content": "x" }),
        ]);
        linked["previous_response_id"] = json!("resp_local_1");

        let Some(continuation) = groups.continuation_for(&linked) else {
            panic!("an exact response link always continues the retained round");
        };

        assert_eq!(continuation.pending_response_id, "resp_local_1");
    }

    #[test]
    fn an_unrelated_conversation_under_one_anchor_is_left_alone() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        let other_conversation = request(vec![
            json!({ "type": "message", "role": "user", "content": "sub-agent task" }),
            json!({
                "type": "function_call_output",
                "call_id": "call_something_else",
                "output": "ok"
            }),
        ]);

        assert!(groups.continuation_for(&other_conversation).is_none());
    }

    #[test]
    fn a_request_without_an_input_array_is_reported_not_repaired() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        let mut without_input = request(Vec::new());
        without_input.as_object_mut().unwrap().remove("input");
        without_input["previous_response_id"] = json!("resp_local_1");

        assert!(groups.continuation_for(&without_input).is_none());
        assert_eq!(
            groups.has_round_for(&without_input),
            Some(round_hash("resp_local_1")),
            "the caller can still tell which round this session had to replay"
        );
    }

    #[test]
    fn settle_forgets_a_round_that_was_already_replayed() {
        let groups = NativePendingToolGroups::default();
        register_hosted_round(&groups);
        assert_eq!(groups.pending_count(), 1);

        groups.settle("resp_local_1");

        assert_eq!(groups.pending_count(), 0);
        assert!(groups.continuation_for(&request(client_replay())).is_none());
    }

    #[test]
    fn capacity_evicts_the_oldest_retained_round() {
        let groups = NativePendingToolGroups::default();
        for index in 0..(MAX_PENDING_NATIVE_GROUPS + 2) {
            let group = PendingNativeToolGroup::new(
                format!("resp_{index}"),
                &request(Vec::new()),
                hosted_round(),
                vec![format!("call_{index}")],
            );
            groups.register(group);
        }

        assert_eq!(groups.pending_count(), MAX_PENDING_NATIVE_GROUPS);
        assert!(groups.has_round_for(&request(Vec::new())).is_some());
        let evicted = groups.take_evictions();
        assert_eq!(
            evicted.len(),
            2,
            "the two rounds dropped by the capacity bound are reported"
        );
        assert!(evicted.contains(&"resp_0".to_owned()));
        assert!(evicted.contains(&"resp_1".to_owned()));
        assert!(groups.take_evictions().is_empty(), "the report drains");
    }

    #[test]
    fn multiple_injected_rounds_keep_their_order_and_offsets() {
        let groups = NativePendingToolGroups::default();
        let segments = vec![
            NativeInjectedItems {
                offset: 1,
                items: vec![json!({
                    "type": "function_call",
                    "call_id": "call_hosted_1",
                    "name": "web_search",
                    "arguments": "{}"
                })],
            },
            NativeInjectedItems {
                offset: 3,
                items: vec![json!({
                    "type": "function_call",
                    "call_id": "call_hosted_2",
                    "name": "web_search",
                    "arguments": "{}"
                })],
            },
        ];
        groups.register(PendingNativeToolGroup::new(
            "resp_local_multi",
            &request(Vec::new()),
            segments,
            vec!["call_client_shell".to_owned()],
        ));
        let replay = vec![
            json!({ "type": "message", "role": "user", "content": "start" }),
            json!({
                "type": "function_call",
                "call_id": "call_client_shell",
                "name": "exec_command",
                "arguments": "{}"
            }),
            json!({
                "type": "function_call_output",
                "call_id": "call_client_shell",
                "output": "ok"
            }),
            json!({ "type": "message", "role": "user", "content": "after" }),
        ];

        let continuation = groups
            .continuation_for(&request(replay.clone()))
            .expect("the retained rounds belong to this request");

        let replay_order = continuation
            .merged_input
            .iter()
            .map(|item| {
                item.get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "message".to_owned())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            replay_order,
            vec![
                "message",
                "call_hosted_1",
                "call_client_shell",
                "call_client_shell",
                "call_hosted_2",
                "message",
            ],
            "each injected round stays at its own offset inside the client's items"
        );
    }
}
