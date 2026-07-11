use codeseex_core::protocol::ChatMessage;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ACTIVE_SESSIONS: usize = 64;
const MAX_TRACKED_MESSAGE_FINGERPRINTS: usize = 16_384;
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Default)]
pub(crate) struct CanonicalSessionCore {
    sessions: Arc<Mutex<HashMap<String, CanonicalSession>>>,
}

struct CanonicalSession {
    message_fingerprints: Vec<[u8; 32]>,
    root_fingerprint: [u8; 32],
    touched_at: Instant,
}

pub(crate) struct CanonicalReplayOutcome {
    diagnostic: Value,
}

impl CanonicalReplayOutcome {
    pub(crate) fn diagnostic(&self) -> Value {
        self.diagnostic.clone()
    }
}

impl CanonicalSessionCore {
    pub(crate) fn reconcile(
        &self,
        request: &Value,
        incoming: &[ChatMessage],
    ) -> CanonicalReplayOutcome {
        let Some(key) = session_key(request) else {
            return CanonicalReplayOutcome {
                diagnostic: json!({
                    "tracked": false,
                    "alignment": "untracked_missing_session_anchor",
                    "incoming_messages": incoming.len()
                }),
            };
        };
        if incoming.len() > MAX_TRACKED_MESSAGE_FINGERPRINTS {
            return CanonicalReplayOutcome {
                diagnostic: json!({
                    "tracked": false,
                    "alignment": "untracked_replay_too_many_items",
                    "incoming_messages": incoming.len(),
                    "max_tracked_messages": MAX_TRACKED_MESSAGE_FINGERPRINTS,
                    "session_hash": short_hash(&key)
                }),
            };
        }

        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        sessions.retain(|_, session| now.duration_since(session.touched_at) <= SESSION_TTL);

        let message_fingerprints = incoming.iter().map(message_fingerprint).collect::<Vec<_>>();
        let root_fingerprint = fingerprints_hash(message_fingerprints.iter().take(3));
        let session_hash = short_hash(&key);
        let outcome = match sessions.get_mut(&key) {
            None => {
                sessions.insert(
                    key,
                    CanonicalSession {
                        message_fingerprints: message_fingerprints.clone(),
                        root_fingerprint,
                        touched_at: now,
                    },
                );
                json!({
                    "tracked": true,
                    "alignment": "rebuilt_no_active_session",
                    "incoming_messages": incoming.len(),
                    "canonical_messages": incoming.len(),
                    "root_changed": false
                })
            }
            Some(session) => {
                let previous_len = session.message_fingerprints.len();
                let alignment =
                    if has_fingerprint_prefix(&message_fingerprints, &session.message_fingerprints)
                    {
                        "appended_authoritative_replay"
                    } else if has_fingerprint_prefix(
                        &session.message_fingerprints,
                        &message_fingerprints,
                    ) {
                        "authoritative_checkpoint_compacted"
                    } else {
                        "authoritative_checkpoint_rebuilt"
                    };
                let root_changed = session.root_fingerprint != root_fingerprint;
                session.message_fingerprints = message_fingerprints;
                session.root_fingerprint = root_fingerprint;
                session.touched_at = now;
                json!({
                    "tracked": true,
                    "alignment": alignment,
                    "previous_messages": previous_len,
                    "incoming_messages": incoming.len(),
                    "canonical_messages": incoming.len(),
                    "root_changed": root_changed
                })
            }
        };

        trim_to_capacity(&mut sessions);
        let mut object = outcome.as_object().cloned().unwrap_or_default();
        object.insert("session_hash".to_owned(), Value::String(session_hash));
        CanonicalReplayOutcome {
            diagnostic: Value::Object(object),
        }
    }
}

fn has_fingerprint_prefix(messages: &[[u8; 32]], prefix: &[[u8; 32]]) -> bool {
    messages
        .get(..prefix.len())
        .map(|candidate| candidate == prefix)
        .unwrap_or(false)
}

fn trim_to_capacity(sessions: &mut HashMap<String, CanonicalSession>) {
    while sessions.len() > MAX_ACTIVE_SESSIONS {
        let Some(oldest_key) = sessions
            .iter()
            .min_by_key(|(_, session)| session.touched_at)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        sessions.remove(&oldest_key);
    }
}

fn session_key(request: &Value) -> Option<String> {
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

    let raw = match (prompt_cache_key, installation_id) {
        (Some(cache_key), Some(installation_id)) => {
            format!("cache:{cache_key}\u{0}installation:{installation_id}")
        }
        (Some(cache_key), None) => format!("cache:{cache_key}"),
        (None, Some(installation_id)) => format!("installation:{installation_id}"),
        (None, None) => return None,
    };
    Some(full_hash(raw.as_bytes()))
}

fn message_fingerprint(message: &ChatMessage) -> [u8; 32] {
    let payload = serde_json::to_vec(message).unwrap_or_default();
    digest_bytes(&payload)
}

fn fingerprints_hash<'a>(fingerprints: impl Iterator<Item = &'a [u8; 32]>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for fingerprint in fingerprints {
        hasher.update(fingerprint);
        hasher.update([0]);
    }
    hasher.finalize().into()
}

fn digest_bytes(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn full_hash(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_append_and_authoritative_checkpoint_without_exposing_anchor() {
        let core = CanonicalSessionCore::default();
        let request = json!({
            "prompt_cache_key": "private-thread-key",
            "client_metadata": { "x-codex-installation-id": "install-a" }
        });
        let first = vec![ChatMessage::text("user", "first")];
        let second = vec![
            ChatMessage::text("user", "first"),
            ChatMessage::text("assistant", "answer"),
        ];
        let compacted = vec![ChatMessage::text("user", "summary checkpoint")];

        assert_eq!(
            core.reconcile(&request, &first).diagnostic()["alignment"],
            "rebuilt_no_active_session"
        );
        assert_eq!(
            core.reconcile(&request, &second).diagnostic()["alignment"],
            "appended_authoritative_replay"
        );
        let diagnostic = core.reconcile(&request, &compacted).diagnostic();
        assert_eq!(diagnostic["alignment"], "authoritative_checkpoint_rebuilt");
        assert!(!diagnostic.to_string().contains("private-thread-key"));
    }
}
