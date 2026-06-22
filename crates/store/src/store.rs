use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use codeseex_core::context::{content_to_text, request_looks_like_codex_full_context};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

mod handoff_guard;
use handoff_guard::{
    client_handoff_guard_stop, client_handoff_output_items, client_handoff_pending_request_keys,
    client_handoff_turn_key, ensure_client_handoff_turn, mark_request_guard_stopped,
    prune_client_handoff_guard, unique_client_handoff_pending_key_for_turn,
};

const MAX_RUNTIME_REQUESTS: usize = 2_048;
const MAX_RUNTIME_TURNS: usize = 500;
const MAX_RUNTIME_EVENTS: usize = 2_048;
const MAX_TURN_MESSAGES_PER_REQUEST: usize = 256;
const MAX_TOOL_FACTS_PER_REQUEST: usize = 100;
const MAX_LOG_STRING_CHARS: usize = 1_024;
const MAX_LOG_SUMMARY_CHARS: usize = 360;
const MAX_LOG_ARRAY_ITEMS: usize = 16;
const MAX_MEMORY_STRING_CHARS: usize = 64 * 1024;
const MAX_MEMORY_ARRAY_ITEMS: usize = 256;
const MAX_USAGE_SESSION_TITLE_CHARS: usize = 80;
const MAX_USAGE_SEGMENT_SUMMARY_CHARS: usize = 180;
const MAX_CLIENT_HANDOFF_GUARD_TURNS: usize = 128;
const MAX_CLIENT_HANDOFF_PENDING_CALLS: usize = 512;
const MAX_CLIENT_HANDOFF_FAILURES: u32 = 3;
const MAX_CLIENT_HANDOFF_SIGNATURE_REPEATS: u32 = 3;
const CLIENT_HANDOFF_VOLUME_DIAGNOSTIC_THRESHOLD: u32 = 48;
const IN_PROGRESS_TTL_SECONDS: i64 = 6 * 60 * 60;
const LOG_TAIL_CHUNK_BYTES: u64 = 64 * 1024;

static STORE_REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<StoreInner>>>>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct Store {
    data_dir: Arc<PathBuf>,
    logs_dir: Arc<PathBuf>,
    legacy_database_path: Arc<PathBuf>,
    inner: Arc<Mutex<StoreInner>>,
}

#[derive(Debug, Default)]
struct StoreInner {
    requests: HashMap<String, StoredRequest>,
    request_order: VecDeque<String>,
    events: VecDeque<EventRecord>,
    next_event_id: i64,
    usage_revision: u64,
    event_revision: u64,
    usage_cache: Option<UsageRuntimeCache>,
    client_handoff_guard: ClientHandoffGuardState,
}

#[derive(Debug, Clone)]
struct UsageRuntimeCache {
    revision: u64,
    summary: RuntimeSummary,
}

#[derive(Debug, Default)]
struct ClientHandoffGuardState {
    turns: HashMap<String, ClientHandoffTurnState>,
    pending_by_key: HashMap<ClientHandoffPendingKey, ClientHandoffPendingCall>,
    turn_order: VecDeque<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClientHandoffPendingKey {
    turn_key: String,
    request_key: String,
    call_id: String,
}

#[derive(Debug, Default)]
struct ClientHandoffTurnState {
    handoff_requests: u32,
    tool_calls: u32,
    signature_counts: HashMap<String, u32>,
    consecutive_failures_by_tool: HashMap<String, u32>,
    cumulative_input_tokens: u64,
    cumulative_total_tokens: u64,
    updated_at: Option<DateTime<Utc>>,
    stopped: Option<ClientToolHandoffGuardStop>,
}

#[derive(Debug, Clone)]
struct ClientHandoffPendingCall {
    turn_key: String,
    name: String,
    arguments_hash: String,
}

#[derive(Debug, Clone)]
pub struct ClientToolHandoffCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientToolHandoffGuardStop {
    pub code: String,
    pub message: String,
    pub reason: String,
    pub turn_key: String,
    pub tool_name: Option<String>,
    pub arguments_hash: Option<String>,
    pub failure_summary: Option<String>,
    pub failure_summary_hash: Option<String>,
    pub handoff_requests: u32,
    pub tool_calls: u32,
    pub repeated_signature_count: u32,
    pub consecutive_failure_count: u32,
    pub cumulative_input_tokens: u64,
    pub cumulative_total_tokens: u64,
}

impl ClientToolHandoffGuardStop {
    pub fn diagnostic(&self) -> Value {
        json!(self)
    }
}

#[derive(Debug, Clone)]
struct StoredRequest {
    id: String,
    previous_response_id: Option<String>,
    status: RequestStatus,
    model: Option<String>,
    input: Value,
    response: Value,
    turn_messages: Vec<Value>,
    tool_facts: Vec<String>,
    diagnostic: Option<Value>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: i64,
    pub level: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub message: String,
    pub detail: Option<Value>,
    pub ts: String,
}

#[derive(Debug, Clone, Default)]
pub struct EventViewQuery {
    pub limit: u32,
    pub cursor: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub audience: Option<String>,
    pub category: Option<String>,
    pub level: Option<String>,
    pub request_id: Option<String>,
    pub q: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventViewRecord {
    pub id: i64,
    pub ts: String,
    pub level: String,
    pub severity: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub audience: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub request_id: Option<String>,
    pub session_hint: Option<String>,
    pub risk_flags: Vec<String>,
    pub metrics: Value,
    pub safe_detail: Option<Value>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestTurn {
    pub id: String,
    pub model: String,
    pub requested_model: String,
    pub reasoning_effort: String,
    pub lifecycle: String,
    pub conversation_turn: bool,
    pub billable: bool,
    pub completed_at: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSession {
    pub id: String,
    pub title: String,
    pub title_source: String,
    pub completed_at: String,
    pub conversation_turn: bool,
    pub status: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
    pub segments: Vec<UsageSegment>,
    pub rows: Vec<UsageSessionRow>,
    pub technical_details: Vec<UsageTechnicalDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSessionSummary {
    pub id: String,
    pub title: String,
    pub title_source: String,
    pub completed_at: String,
    pub conversation_turn: bool,
    pub status: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
    pub billing_buckets: Vec<UsageBillingBucket>,
    pub row_count: usize,
    pub segment_count: usize,
    pub session_revision: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageBillingBucket {
    pub model: String,
    pub requested_model: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagePage {
    pub usage_revision: u64,
    pub event_revision: u64,
    pub unchanged: bool,
    pub active_requests: u64,
    pub request_count: u64,
    pub billable_request_count: u64,
    pub failed_request_count: u64,
    pub last_request_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub total_cached_input_tokens: u64,
    pub total_cache_miss_input_tokens: u64,
    pub total_output_tokens: u64,
    pub average_ms: u64,
    pub billing_buckets: Vec<UsageBillingBucket>,
    pub usage_sessions: Vec<UsageSessionSummary>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSessionDetail {
    pub usage_revision: u64,
    pub session: Option<UsageSession>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSegment {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub hint: String,
    pub model: String,
    pub requested_model: String,
    pub reasoning_effort: String,
    pub lifecycle: String,
    pub status: String,
    pub tool_name: Option<String>,
    pub iteration: Option<u32>,
    pub summary: Option<String>,
    pub completed_at: Option<String>,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
    pub rows: Vec<UsageSessionRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageSessionRow {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub hint: String,
    pub model: String,
    pub requested_model: String,
    pub reasoning_effort: String,
    pub lifecycle: String,
    pub status: String,
    pub billable: bool,
    pub completed_at: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageTechnicalDetail {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSummary {
    pub usage_revision: u64,
    pub event_revision: u64,
    pub active_requests: u64,
    pub request_count: u64,
    pub billable_request_count: u64,
    pub failed_request_count: u64,
    pub last_request_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub last_turn: Option<RequestTurn>,
    pub last_billable_request: Option<RequestTurn>,
    pub turn_history: Vec<RequestTurn>,
    pub billable_history: Vec<RequestTurn>,
    pub usage_sessions: Vec<UsageSession>,
    pub total_cached_input_tokens: u64,
    pub total_cache_miss_input_tokens: u64,
    pub total_output_tokens: u64,
    pub average_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaintenanceReport {
    pub log_retention_days: u16,
    pub deleted_events: u64,
    pub sanitized_requests: u64,
    pub request_sanitize_batches: u64,
    pub request_sanitize_limit_reached: bool,
    pub vacuumed_storage: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredResponse {
    pub id: String,
    pub previous_response_id: Option<String>,
    pub status: RequestStatus,
    pub input: Value,
    pub response: Value,
    pub turn_messages: Vec<Value>,
    pub tool_facts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RecentToolFactRecord {
    pub response: Value,
    pub tool_facts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

impl Store {
    fn lock_inner(&self) -> Result<std::sync::MutexGuard<'_, StoreInner>> {
        self.inner
            .lock()
            .map_err(|_| anyhow::anyhow!("store runtime lock was poisoned"))
    }

    pub async fn open(path: &Path) -> Result<Self> {
        let data_dir = data_dir_from_store_path(path);
        ensure_data_dir_layout(&data_dir)
            .await
            .with_context(|| format!("initialize CodeSeeX data dir {}", data_dir.display()))?;
        let data_dir = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);
        let inner = shared_runtime_inner(&data_dir)?;
        Ok(Self {
            logs_dir: Arc::new(data_dir.join("logs")),
            legacy_database_path: Arc::new(data_dir.join("codeseex.db")),
            data_dir: Arc::new(data_dir),
            inner,
        })
    }

    pub async fn close(&self) {}

    pub async fn run_maintenance(&self, log_retention_days: u16) -> Result<MaintenanceReport> {
        ensure_data_dir_layout(&self.data_dir).await?;
        let log_retention_days = log_retention_days.clamp(1, 365);
        let deleted_events = prune_old_logs(&self.logs_dir, log_retention_days).await?;
        if self.legacy_database_path.exists() {
            let _ = self
                .record_event(
                    "warn",
                    "legacy_database_ignored",
                    "Legacy codeseex.db was ignored; CodeSeeX runtime state now uses memory and logs files.",
                    Some(&json!({
                        "path": self.legacy_database_path.to_string_lossy(),
                        "note": "The file is not deleted automatically."
                    })),
                )
                .await;
        }
        Ok(MaintenanceReport {
            log_retention_days,
            deleted_events,
            sanitized_requests: 0,
            request_sanitize_batches: 0,
            request_sanitize_limit_reached: false,
            vacuumed_storage: false,
        })
    }

    pub async fn checkpoint_request(
        &self,
        id: &str,
        previous_response_id: Option<&str>,
        model: Option<&str>,
        input: &Value,
    ) -> Result<()> {
        let now = Utc::now();
        let mut inner = self.lock_inner()?;
        if inner.requests.contains_key(id) {
            bail!("request id '{id}' already exists in this CodeSeeX process");
        }
        let request = StoredRequest {
            id: id.to_owned(),
            previous_response_id: previous_response_id.map(str::to_owned),
            status: RequestStatus::InProgress,
            model: model.map(str::to_owned),
            input: request_input_for_runtime(previous_response_id, input),
            response: Value::Null,
            turn_messages: Vec::new(),
            tool_facts: Vec::new(),
            diagnostic: None,
            created_at: now,
            updated_at: now,
        };
        inner.requests.insert(id.to_owned(), request);
        push_request_order(&mut inner.request_order, id);
        prune_runtime_requests(&mut inner);
        bump_usage_revision(&mut inner);
        Ok(())
    }

    pub async fn runtime_summary(&self, turn_limit: u32) -> Result<RuntimeSummary> {
        let inner = self.lock_inner()?;
        let mut turns = completed_conversation_turns(&inner);
        let mut billable = completed_billable_requests(&inner);
        let limit = usize::try_from(turn_limit.clamp(1, 500)).unwrap_or(120);
        if turns.len() > limit {
            turns.drain(0..turns.len() - limit);
        }
        if billable.len() > limit {
            billable.drain(0..billable.len() - limit);
        }
        Ok(runtime_summary_from_inner(&inner, turns, billable))
    }

    pub async fn runtime_overview(&self) -> Result<RuntimeSummary> {
        let inner = self.lock_inner()?;
        let mut turns = completed_conversation_turns(&inner);
        let mut billable = completed_billable_requests(&inner);
        if turns.len() > 1 {
            turns.drain(0..turns.len() - 1);
        }
        if billable.len() > 1 {
            billable.drain(0..billable.len() - 1);
        }
        Ok(runtime_summary_from_inner(&inner, turns, billable))
    }

    pub async fn revisions(&self) -> Result<(u64, u64)> {
        let inner = self.lock_inner()?;
        Ok((inner.usage_revision, inner.event_revision))
    }

    pub async fn usage_page(
        &self,
        limit: u32,
        cursor: Option<&str>,
        since_revision: Option<u64>,
    ) -> Result<UsagePage> {
        let mut inner = self.lock_inner()?;
        let usage_revision = inner.usage_revision;
        let event_revision = inner.event_revision;
        if since_revision == Some(usage_revision) {
            return Ok(UsagePage {
                usage_revision,
                event_revision,
                unchanged: true,
                active_requests: 0,
                request_count: 0,
                billable_request_count: 0,
                failed_request_count: 0,
                last_request_at: None,
                last_activity_at: None,
                total_cached_input_tokens: 0,
                total_cache_miss_input_tokens: 0,
                total_output_tokens: 0,
                average_ms: 0,
                billing_buckets: Vec::new(),
                usage_sessions: Vec::new(),
                has_more: false,
                next_cursor: None,
            });
        }
        let summary = cached_usage_summary(&mut inner);
        Ok(usage_page_from_summary(
            &summary,
            limit,
            cursor,
            usage_billing_buckets(&inner),
        ))
    }

    pub async fn usage_session_detail(&self, id: &str) -> Result<UsageSessionDetail> {
        let mut inner = self.lock_inner()?;
        let summary = cached_usage_summary(&mut inner);
        let session = summary
            .usage_sessions
            .iter()
            .find(|session| session.id == id)
            .cloned();
        Ok(UsageSessionDetail {
            usage_revision: summary.usage_revision,
            session,
        })
    }

    pub async fn recent_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        if let Some(result) = self.runtime_events(limit, before, false)? {
            return Ok(result);
        }
        read_log_events(&self.logs_dir, limit, before, false).await
    }

    pub async fn recent_visible_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        if let Some(result) = self.runtime_events(limit, before, true)? {
            return Ok(result);
        }
        read_log_events(&self.logs_dir, limit, before, true).await
    }

    pub async fn recent_event_views(
        &self,
        query: EventViewQuery,
    ) -> Result<(Vec<EventViewRecord>, bool, Option<String>)> {
        if let Some(result) = self.runtime_event_views(&query)? {
            return Ok(result);
        }
        read_log_event_views(&self.logs_dir, query).await
    }

    pub async fn response_context_chain(
        &self,
        previous_response_id: &str,
        max_depth: u32,
    ) -> Result<Vec<StoredResponse>> {
        let inner = self.lock_inner()?;
        let Some(root) = inner.requests.get(previous_response_id) else {
            bail!("previous_response_id '{previous_response_id}' is not available in this CodeSeeX process; send full context instead");
        };
        if root.status != RequestStatus::Completed {
            bail!(
                "previous_response_id '{previous_response_id}' is {:?}, not completed; send full context instead",
                root.status
            );
        }
        let mut cursor = Some(previous_response_id.to_owned());
        let mut newest_first = Vec::new();
        let mut visited = HashSet::new();
        let max_depth = max_depth.clamp(1, 10_000);
        for _ in 0..max_depth {
            let Some(id) = cursor.take() else {
                break;
            };
            if !visited.insert(id.clone()) {
                bail!("previous_response_id chain contains a cycle at '{id}'");
            }
            let Some(request) = inner.requests.get(&id) else {
                bail!(
                    "previous_response_id chain is incomplete at '{id}'; send full context instead"
                );
            };
            if request.status != RequestStatus::Completed {
                bail!(
                    "previous_response_id chain contains {:?} request '{id}'; send full context instead",
                    request.status
                );
            }
            newest_first.push(StoredResponse {
                id: request.id.clone(),
                previous_response_id: request.previous_response_id.clone(),
                status: request.status,
                input: request.input.clone(),
                response: request.response.clone(),
                turn_messages: request.turn_messages.clone(),
                tool_facts: request.tool_facts.clone(),
            });
            cursor = request.previous_response_id.clone();
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    pub async fn contains_response(&self, id: &str) -> Result<bool> {
        let inner = self.lock_inner()?;
        Ok(inner.requests.contains_key(id))
    }

    pub async fn response_status(&self, id: &str) -> Result<Option<RequestStatus>> {
        let inner = self.lock_inner()?;
        Ok(inner.requests.get(id).map(|request| request.status))
    }

    pub async fn append_request_tool_fact(&self, id: &str, fact: &str) -> Result<()> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while appending tool facts");
        };
        request.tool_facts.push(sanitize_log_string(fact));
        if request.tool_facts.len() > MAX_TOOL_FACTS_PER_REQUEST {
            let omitted = request.tool_facts.len() - MAX_TOOL_FACTS_PER_REQUEST;
            request.tool_facts.drain(0..omitted);
            request.tool_facts.insert(
                0,
                format!("[CodeSeeX runtime omitted {omitted} older tool fact(s)]"),
            );
        }
        request.updated_at = Utc::now();
        Ok(())
    }

    pub async fn recent_tool_facts(&self, _limit: u32) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    pub async fn recent_tool_fact_records(&self, _limit: u32) -> Result<Vec<RecentToolFactRecord>> {
        Ok(Vec::new())
    }

    pub async fn replace_request_turn_messages(&self, id: &str, messages: &[Value]) -> Result<()> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while replacing turn messages");
        };
        let mut values = messages.iter().map(memory_json_value).collect::<Vec<_>>();
        if values.len() > MAX_TURN_MESSAGES_PER_REQUEST {
            values.drain(0..values.len() - MAX_TURN_MESSAGES_PER_REQUEST);
        }
        request.turn_messages = values;
        request.updated_at = Utc::now();
        Ok(())
    }

    pub async fn append_request_turn_messages(&self, id: &str, messages: &[Value]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while appending turn messages");
        };
        request
            .turn_messages
            .extend(messages.iter().map(memory_json_value));
        if request.turn_messages.len() > MAX_TURN_MESSAGES_PER_REQUEST {
            let drop_count = request.turn_messages.len() - MAX_TURN_MESSAGES_PER_REQUEST;
            request.turn_messages.drain(0..drop_count);
        }
        request.updated_at = Utc::now();
        Ok(())
    }

    pub async fn finish_request(
        &self,
        id: &str,
        status: RequestStatus,
        response: Option<&Value>,
        diagnostic: Option<&Value>,
    ) -> Result<()> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while finishing request");
        };
        if request.status == RequestStatus::Interrupted {
            return Ok(());
        }
        request.status = status;
        if let Some(response) = response {
            request.response = memory_json_value(response);
        }
        if let Some(diagnostic) = diagnostic {
            request.diagnostic = Some(merge_request_diagnostic(
                request.diagnostic.as_ref(),
                diagnostic,
            ));
        }
        request.updated_at = Utc::now();
        bump_usage_revision(&mut inner);
        Ok(())
    }

    pub async fn interrupt_request_if_in_progress(&self, id: &str, reason: &str) -> Result<bool> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            return Ok(false);
        };
        if request.status != RequestStatus::InProgress {
            return Ok(false);
        }
        request.status = RequestStatus::Interrupted;
        request.diagnostic = Some(json!({ "reason": reason }));
        request.updated_at = Utc::now();
        bump_usage_revision(&mut inner);
        Ok(true)
    }

    pub async fn update_request_diagnostic(&self, id: &str, diagnostic: &Value) -> Result<()> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while updating diagnostics");
        };
        request.diagnostic = Some(merge_request_diagnostic(
            request.diagnostic.as_ref(),
            diagnostic,
        ));
        request.updated_at = Utc::now();
        bump_usage_revision(&mut inner);
        Ok(())
    }

    pub async fn settle_client_tool_handoff_outputs(
        &self,
        request_id: &str,
        input: &Value,
    ) -> Result<Option<ClientToolHandoffGuardStop>> {
        let mut inner = self.lock_inner()?;
        let turn_key = client_handoff_turn_key(input);
        let output_items = client_handoff_output_items(input);
        if output_items.is_empty() {
            return Ok(None);
        }
        prune_client_handoff_guard(&mut inner.client_handoff_guard);
        let mut stop = None;
        let mut unmatched_outputs = 0_u32;
        for output in output_items {
            let call_id = output.call_id.trim().to_owned();
            let request_keys = client_handoff_pending_request_keys(request_id, input);
            let pending_key = request_keys
                .iter()
                .find_map(|request_key| {
                    let key = ClientHandoffPendingKey {
                        turn_key: turn_key.clone(),
                        request_key: request_key.clone(),
                        call_id: call_id.clone(),
                    };
                    inner
                        .client_handoff_guard
                        .pending_by_key
                        .contains_key(&key)
                        .then_some(key)
                })
                .or_else(|| {
                    unique_client_handoff_pending_key_for_turn(
                        &inner.client_handoff_guard,
                        &turn_key,
                        &call_id,
                    )
                });
            let Some(pending_key) = pending_key else {
                unmatched_outputs = unmatched_outputs.saturating_add(1);
                continue;
            };
            let Some(pending) = inner
                .client_handoff_guard
                .pending_by_key
                .remove(&pending_key)
            else {
                continue;
            };
            let effective_turn_key = if pending.turn_key.is_empty() {
                turn_key.clone()
            } else {
                pending.turn_key.clone()
            };
            ensure_client_handoff_turn(&mut inner.client_handoff_guard, &effective_turn_key);
            let state = inner
                .client_handoff_guard
                .turns
                .get_mut(&effective_turn_key)
                .expect("guard turn state should exist");
            state.updated_at = Some(Utc::now());
            if output.failed {
                let failure_count = {
                    let count = state
                        .consecutive_failures_by_tool
                        .entry(pending.name.clone())
                        .or_insert(0);
                    *count = count.saturating_add(1);
                    *count
                };
                if failure_count >= MAX_CLIENT_HANDOFF_FAILURES && stop.is_none() {
                    stop = Some(client_handoff_guard_stop(
                        "consecutive_failures",
                        &effective_turn_key,
                        Some(&pending.name),
                        Some(&pending.arguments_hash),
                        output.failure_summary.as_deref(),
                        state,
                        0,
                        failure_count,
                    ));
                }
            } else {
                state
                    .consecutive_failures_by_tool
                    .insert(pending.name.clone(), 0);
            }
        }
        if unmatched_outputs > 0 {
            if let Some(request) = inner.requests.get_mut(request_id) {
                request.diagnostic = Some(merge_request_diagnostic(
                    request.diagnostic.as_ref(),
                    &json!({
                        "client_tool_handoff_unmatched_outputs": unmatched_outputs,
                        "client_tool_handoff_pending_match": "turn_and_request"
                    }),
                ));
                request.updated_at = Utc::now();
                bump_usage_revision(&mut inner);
            }
        }
        if let Some(stop) = stop.clone() {
            if let Some(state) = inner.client_handoff_guard.turns.get_mut(&stop.turn_key) {
                state.stopped = Some(stop.clone());
            }
        }
        if let Some(stop) = stop.as_ref() {
            mark_request_guard_stopped(&mut inner, request_id, stop);
            bump_usage_revision(&mut inner);
        }
        Ok(stop)
    }

    pub async fn record_client_tool_handoff_calls(
        &self,
        request_id: &str,
        input: &Value,
        calls: &[ClientToolHandoffCall],
        usage: Option<&Value>,
    ) -> Result<Option<ClientToolHandoffGuardStop>> {
        if calls.is_empty() {
            return Ok(None);
        }
        let mut inner = self.lock_inner()?;
        prune_client_handoff_guard(&mut inner.client_handoff_guard);
        let turn_key = client_handoff_turn_key(input);
        ensure_client_handoff_turn(&mut inner.client_handoff_guard, &turn_key);
        if let Some(stop) = inner
            .client_handoff_guard
            .turns
            .get(&turn_key)
            .and_then(|state| state.stopped.clone())
        {
            mark_request_guard_stopped(&mut inner, request_id, &stop);
            return Ok(Some(stop));
        }
        let usage_input = usage_input_tokens(usage.unwrap_or(&Value::Null));
        let usage_total = usage_total_tokens(usage.unwrap_or(&Value::Null), usage_input);
        let state = inner
            .client_handoff_guard
            .turns
            .get_mut(&turn_key)
            .expect("guard turn state should exist");
        state.handoff_requests = state.handoff_requests.saturating_add(1);
        state.tool_calls = state
            .tool_calls
            .saturating_add(u32::try_from(calls.len()).unwrap_or(u32::MAX));
        state.cumulative_input_tokens = state.cumulative_input_tokens.saturating_add(usage_input);
        state.cumulative_total_tokens = state.cumulative_total_tokens.saturating_add(usage_total);
        state.updated_at = Some(Utc::now());

        let mut stop = None;
        let mut pending_calls = Vec::new();
        let request_key = request_id.to_owned();
        for call in calls {
            let arguments_hash = stable_hash_hex(call.arguments.as_bytes());
            let signature = format!("{}\n{}", call.name, arguments_hash);
            let repeat_count = {
                let count = state.signature_counts.entry(signature.clone()).or_insert(0);
                *count = count.saturating_add(1);
                *count
            };
            pending_calls.push((
                ClientHandoffPendingKey {
                    turn_key: turn_key.clone(),
                    request_key: request_key.clone(),
                    call_id: call.call_id.clone(),
                },
                ClientHandoffPendingCall {
                    turn_key: turn_key.clone(),
                    name: call.name.clone(),
                    arguments_hash: arguments_hash.clone(),
                },
            ));
            if repeat_count >= MAX_CLIENT_HANDOFF_SIGNATURE_REPEATS && stop.is_none() {
                stop = Some(client_handoff_guard_stop(
                    "repeated_signature",
                    &turn_key,
                    Some(&call.name),
                    Some(&arguments_hash),
                    None,
                    state,
                    repeat_count,
                    0,
                ));
            }
        }
        let volume_diagnostic =
            (state.handoff_requests > CLIENT_HANDOFF_VOLUME_DIAGNOSTIC_THRESHOLD).then(|| {
                json!({
                    "client_tool_handoff_high_volume": true,
                    "client_tool_handoff_volume": {
                        "handoff_requests": state.handoff_requests,
                        "tool_calls": state.tool_calls,
                        "cumulative_input_tokens": state.cumulative_input_tokens,
                        "cumulative_total_tokens": state.cumulative_total_tokens,
                        "diagnostic_threshold": CLIENT_HANDOFF_VOLUME_DIAGNOSTIC_THRESHOLD
                    }
                })
            });
        if let Some(stop) = stop.clone() {
            state.stopped = Some(stop);
        }
        let _ = state;
        if let Some(diagnostic) = volume_diagnostic {
            if let Some(request) = inner.requests.get_mut(request_id) {
                request.diagnostic = Some(merge_request_diagnostic(
                    request.diagnostic.as_ref(),
                    &diagnostic,
                ));
                request.updated_at = Utc::now();
                bump_usage_revision(&mut inner);
            }
        }
        if stop.is_none() {
            for (key, pending) in pending_calls {
                inner
                    .client_handoff_guard
                    .pending_by_key
                    .insert(key, pending);
            }
        }
        if let Some(stop) = stop.as_ref() {
            mark_request_guard_stopped(&mut inner, request_id, stop);
        }
        bump_usage_revision(&mut inner);
        Ok(stop)
    }

    pub async fn client_tool_handoff_guard_preflight(
        &self,
        request_id: &str,
        input: &Value,
    ) -> Result<Option<ClientToolHandoffGuardStop>> {
        let mut inner = self.lock_inner()?;
        prune_client_handoff_guard(&mut inner.client_handoff_guard);
        let turn_key = client_handoff_turn_key(input);
        let stop = inner
            .client_handoff_guard
            .turns
            .get_mut(&turn_key)
            .and_then(|state| {
                state.updated_at = Some(Utc::now());
                state.stopped.clone()
            });
        if let Some(stop) = stop.as_ref() {
            mark_request_guard_stopped(&mut inner, request_id, stop);
            bump_usage_revision(&mut inner);
        }
        Ok(stop)
    }

    pub async fn recover_interrupted_requests(&self, reason: &str) -> Result<Vec<String>> {
        let mut inner = self.lock_inner()?;
        let now = Utc::now();
        let mut interrupted = Vec::new();
        for request in inner.requests.values_mut() {
            if request.status != RequestStatus::InProgress {
                continue;
            }
            request.status = RequestStatus::Interrupted;
            request.diagnostic = Some(json!({ "reason": reason }));
            request.updated_at = now;
            interrupted.push(request.id.clone());
        }
        if !interrupted.is_empty() {
            bump_usage_revision(&mut inner);
        }
        Ok(interrupted)
    }

    pub async fn record_event(
        &self,
        level: &str,
        event_type: &str,
        message: &str,
        detail: Option<&Value>,
    ) -> Result<()> {
        let level = level.trim().to_ascii_lowercase();
        let event_type = event_type.trim().to_owned();
        let audience = event_audience_for_type(&event_type).to_owned();
        if audience == "diagnostic" && !diagnostic_logs_enabled() {
            return Ok(());
        }
        let id = {
            let mut inner = self.lock_inner()?;
            inner.next_event_id = inner.next_event_id.saturating_add(1);
            inner.next_event_id
        };
        let event = EventRecord {
            id,
            level,
            event_type: event_type.clone(),
            audience: Some(audience),
            message: message.trim().to_owned(),
            detail: detail.and_then(|value| compact_event_detail(&event_type, value)),
            ts: Utc::now().to_rfc3339(),
        };
        self.push_runtime_event(event.clone())?;
        append_log_event(&self.logs_dir, &event).await
    }

    fn push_runtime_event(&self, event: EventRecord) -> Result<()> {
        let mut inner = self.lock_inner()?;
        bump_event_revision(&mut inner);
        if event_affects_usage(&event.event_type) {
            bump_usage_revision(&mut inner);
        }
        inner.events.push_back(event);
        while inner.events.len() > MAX_RUNTIME_EVENTS {
            inner.events.pop_front();
        }
        Ok(())
    }

    fn runtime_events(
        &self,
        limit: u32,
        before: Option<&str>,
        visible_only: bool,
    ) -> Result<Option<(Vec<EventRecord>, bool)>> {
        let limit = usize::try_from(limit.clamp(1, 500)).unwrap_or(30);
        let inner = self.lock_inner()?;
        if inner.events.is_empty() {
            return Ok(None);
        }
        let mut matching = inner
            .events
            .iter()
            .filter(|event| !visible_only || event_is_user_visible(event))
            .filter(|event| {
                before
                    .map(|before| event.ts.as_str() < before)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        matching.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
        let has_more = matching.len() > limit;
        if has_more {
            matching.drain(0..matching.len() - limit);
        }
        Ok(Some((matching, has_more)))
    }

    fn runtime_event_views(
        &self,
        query: &EventViewQuery,
    ) -> Result<Option<(Vec<EventViewRecord>, bool, Option<String>)>> {
        let limit = event_view_limit(query.limit);
        let inner = self.lock_inner()?;
        if inner.events.is_empty() {
            return Ok(None);
        }
        if !runtime_events_cover_query(&inner.events, query) {
            return Ok(None);
        }
        let mut matching = inner
            .events
            .iter()
            .filter(|event| event_matches_event_view_query(event, query))
            .map(event_view_record)
            .collect::<Vec<_>>();
        matching.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
        let has_more = matching.len() > limit;
        if has_more
            && query
                .after
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        {
            matching.truncate(limit);
        } else if has_more {
            matching.drain(0..matching.len() - limit);
        }
        let next_cursor = matching.first().and_then(|event| event.cursor.clone());
        Ok(Some((matching, has_more, next_cursor)))
    }
}

fn merge_request_diagnostic(existing: Option<&Value>, next: &Value) -> Value {
    let existing = existing.map(memory_json_value);
    let next = memory_json_value(next);
    match (existing, next) {
        (Some(Value::Object(mut existing)), Value::Object(next)) => {
            for (key, value) in next {
                existing.insert(key, value);
            }
            Value::Object(existing)
        }
        (_, next) => next,
    }
}

fn data_dir_from_store_path(path: &Path) -> PathBuf {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("codeseex.db") || name.ends_with(".sqlite"))
        .unwrap_or(false)
    {
        return path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
    }
    path.to_path_buf()
}

fn shared_runtime_inner(data_dir: &Path) -> Result<Arc<Mutex<StoreInner>>> {
    let registry = STORE_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| anyhow::anyhow!("store registry lock was poisoned"))?;
    if let Some(existing) = registry.get(data_dir).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    let inner = Arc::new(Mutex::new(StoreInner::default()));
    registry.insert(data_dir.to_path_buf(), Arc::downgrade(&inner));
    Ok(inner)
}

async fn ensure_data_dir_layout(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)?;
    for name in [
        "extension",
        "extension/tools",
        "lang",
        "logs",
        "cache",
        "runtime",
        "secrets",
    ] {
        std::fs::create_dir_all(data_dir.join(name))?;
    }
    Ok(())
}

async fn prune_old_logs(logs_dir: &Path, retention_days: u16) -> Result<u64> {
    let cutoff = Utc::now()
        .checked_sub_signed(Duration::days(i64::from(retention_days)))
        .unwrap_or_else(Utc::now);
    let mut deleted = 0_u64;
    let entries = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let modified: DateTime<Utc> = modified.into();
        if modified >= cutoff {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            deleted = deleted.saturating_add(1);
        }
    }
    Ok(deleted)
}

async fn append_log_event(logs_dir: &Path, event: &EventRecord) -> Result<()> {
    let logs_dir = logs_dir.to_path_buf();
    let event = event.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        std::fs::create_dir_all(&logs_dir)?;
        let path = logs_dir.join(format!("{}.jsonl", Utc::now().format("%Y-%m-%d")));
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    })
    .await
    .map_err(|error| anyhow::anyhow!("log writer task failed: {error}"))?
}

async fn read_log_events(
    logs_dir: &Path,
    limit: u32,
    before: Option<&str>,
    visible_only: bool,
) -> Result<(Vec<EventRecord>, bool)> {
    let logs_dir = logs_dir.to_path_buf();
    let before = before.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        read_log_events_sync(&logs_dir, limit, before.as_deref(), visible_only)
    })
    .await
    .map_err(|error| anyhow::anyhow!("log reader task failed: {error}"))?
}

async fn read_log_event_views(
    logs_dir: &Path,
    query: EventViewQuery,
) -> Result<(Vec<EventViewRecord>, bool, Option<String>)> {
    let logs_dir = logs_dir.to_path_buf();
    tokio::task::spawn_blocking(move || read_log_event_views_sync(&logs_dir, &query))
        .await
        .map_err(|error| anyhow::anyhow!("log reader task failed: {error}"))?
}

fn read_log_events_sync(
    logs_dir: &Path,
    limit: u32,
    before: Option<&str>,
    visible_only: bool,
) -> Result<(Vec<EventRecord>, bool)> {
    let limit = usize::try_from(limit.clamp(1, 1_000)).unwrap_or(120);
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files.sort();
    files.reverse();
    let mut events = Vec::new();
    for path in files {
        collect_recent_events_from_file(&path, limit + 1, before, visible_only, &mut events)?;
        if events.len() > limit {
            break;
        }
    }
    events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
    let has_more = events.len() > limit;
    if has_more {
        events.drain(0..events.len() - limit);
    }
    Ok((events, has_more))
}

fn read_log_event_views_sync(
    logs_dir: &Path,
    query: &EventViewQuery,
) -> Result<(Vec<EventViewRecord>, bool, Option<String>)> {
    let limit = event_view_limit(query.limit);
    let mut files = Vec::new();
    let entries = match std::fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false, None));
        }
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    let mut events = Vec::new();
    if query
        .after
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        files.sort();
        for path in files {
            collect_after_event_views_from_file(&path, limit + 1, query, &mut events)?;
            if events.len() > limit {
                break;
            }
        }
        events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
        let has_more = events.len() > limit;
        if has_more {
            events.truncate(limit);
        }
        let next_cursor = events.first().and_then(|event| event.cursor.clone());
        return Ok((events, has_more, next_cursor));
    }
    files.sort();
    files.reverse();
    for path in files {
        collect_recent_event_views_from_file(&path, limit + 1, query, &mut events)?;
        if events.len() > limit {
            break;
        }
    }
    events.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.id.cmp(&b.id)));
    let has_more = events.len() > limit;
    if has_more {
        events.drain(0..events.len() - limit);
    }
    let next_cursor = events.first().and_then(|event| event.cursor.clone());
    Ok((events, has_more, next_cursor))
}

fn collect_recent_events_from_file(
    path: &Path,
    target: usize,
    before: Option<&str>,
    visible_only: bool,
    events: &mut Vec<EventRecord>,
) -> Result<()> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut offset = file.metadata()?.len();
    let mut carry = Vec::new();
    while offset > 0 && events.len() <= target {
        let read_len = LOG_TAIL_CHUNK_BYTES.min(offset);
        offset -= read_len;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = vec![0_u8; usize::try_from(read_len).unwrap_or(0)];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&carry);
        let parts = chunk.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        let start = if offset > 0 {
            carry = parts.first().copied().unwrap_or_default().to_vec();
            1
        } else {
            carry.clear();
            0
        };
        for line in parts[start..].iter().rev() {
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<EventRecord>(line) else {
                continue;
            };
            if visible_only && !event_is_user_visible(&event) {
                continue;
            }
            if let Some(before) = before {
                if event.ts.as_str() >= before {
                    continue;
                }
            }
            events.push(event);
            if events.len() > target {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn collect_recent_event_views_from_file(
    path: &Path,
    target: usize,
    query: &EventViewQuery,
    events: &mut Vec<EventViewRecord>,
) -> Result<()> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut offset = file.metadata()?.len();
    let mut carry = Vec::new();
    while offset > 0 && events.len() <= target {
        let read_len = LOG_TAIL_CHUNK_BYTES.min(offset);
        offset -= read_len;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = vec![0_u8; usize::try_from(read_len).unwrap_or(0)];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&carry);
        let parts = chunk.split(|byte| *byte == b'\n').collect::<Vec<_>>();
        let start = if offset > 0 {
            carry = parts.first().copied().unwrap_or_default().to_vec();
            1
        } else {
            carry.clear();
            0
        };
        for line in parts[start..].iter().rev() {
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<EventRecord>(line) else {
                continue;
            };
            if !event_matches_event_view_query(&event, query) {
                continue;
            }
            events.push(event_view_record(&event));
            if events.len() > target {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn collect_after_event_views_from_file(
    path: &Path,
    target: usize,
    query: &EventViewQuery,
    events: &mut Vec<EventViewRecord>,
) -> Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<EventRecord>(&line) else {
            continue;
        };
        if !event_matches_event_view_query(&event, query) {
            continue;
        }
        events.push(event_view_record(&event));
        if events.len() > target {
            return Ok(());
        }
    }
    Ok(())
}

fn event_is_user_visible(event: &EventRecord) -> bool {
    event.level != "debug" && event_audience(event) == "user"
}

fn event_audience(event: &EventRecord) -> &'static str {
    match event.audience.as_deref() {
        Some("diagnostic") => "diagnostic",
        Some("safe_diagnostic") => "safe_diagnostic",
        Some("user") => "user",
        _ => event_audience_for_type(&event.event_type),
    }
}

fn event_audience_for_type(event_type: &str) -> &'static str {
    let event_type = event_type.trim();
    if is_safe_diagnostic_event_type(event_type) {
        "safe_diagnostic"
    } else if is_diagnostic_event_type(event_type) {
        "diagnostic"
    } else {
        "user"
    }
}

fn event_view_limit(limit: u32) -> usize {
    usize::try_from(limit.clamp(1, 500)).unwrap_or(60)
}

fn event_cursor(event: &EventRecord) -> String {
    format!("{}|{}", event.ts, event.id)
}

fn event_before_query_cursor(event: &EventRecord, query: &EventViewQuery) -> bool {
    let cursor = query.cursor.as_deref().or(query.before.as_deref());
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if let Some((ts, id)) = cursor.rsplit_once('|') {
        if event.ts.as_str() < ts {
            return true;
        }
        if event.ts.as_str() > ts {
            return false;
        }
        return id
            .parse::<i64>()
            .map(|cursor_id| event.id < cursor_id)
            .unwrap_or(false);
    }
    event.ts.as_str() < cursor
}

fn event_after_query_cursor(event: &EventRecord, query: &EventViewQuery) -> bool {
    let Some(cursor) = query
        .after
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return true;
    };
    if let Some((ts, id)) = cursor.rsplit_once('|') {
        if event.ts.as_str() > ts {
            return true;
        }
        if event.ts.as_str() < ts {
            return false;
        }
        return id
            .parse::<i64>()
            .map(|cursor_id| event.id > cursor_id)
            .unwrap_or(false);
    }
    event.ts.as_str() > cursor
}

fn runtime_events_cover_query(events: &VecDeque<EventRecord>, query: &EventViewQuery) -> bool {
    let Some(first) = events.front() else {
        return false;
    };
    if let Some(cursor) = query
        .after
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if event_cursor_order(first, cursor).is_gt() {
            return false;
        }
    }
    if let Some(cursor) = query
        .cursor
        .as_deref()
        .or(query.before.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !event_cursor_order(first, cursor).is_lt() {
            return false;
        }
    }
    true
}

fn event_cursor_order(event: &EventRecord, cursor: &str) -> std::cmp::Ordering {
    let cursor = cursor.trim();
    if let Some((ts, id)) = cursor.rsplit_once('|') {
        let ts_order = event.ts.as_str().cmp(ts);
        if !ts_order.is_eq() {
            return ts_order;
        }
        return id
            .parse::<i64>()
            .map(|cursor_id| event.id.cmp(&cursor_id))
            .unwrap_or(std::cmp::Ordering::Equal);
    }
    event.ts.as_str().cmp(cursor)
}

fn query_csv_contains(filter: Option<&str>, value: &str) -> bool {
    let Some(filter) = filter.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    filter
        .split(',')
        .map(|part| part.trim())
        .any(|part| part.eq_ignore_ascii_case("all") || part.eq_ignore_ascii_case(value))
}

fn query_level_includes_debug(query: &EventViewQuery) -> bool {
    query
        .level
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("debug"))
}

fn event_view_allows_audience(event: &EventRecord, query: &EventViewQuery) -> bool {
    if event.level == "debug" && !query_level_includes_debug(query) {
        return false;
    }
    let audience = event_audience(event);
    let mode = query
        .audience
        .as_deref()
        .unwrap_or("safe")
        .trim()
        .to_ascii_lowercase();
    match mode.as_str() {
        "user" => audience == "user",
        "safe" | "all_safe" | "" => audience == "user" || audience == "safe_diagnostic",
        _ => audience == "user" || audience == "safe_diagnostic",
    }
}

fn event_matches_event_view_query(event: &EventRecord, query: &EventViewQuery) -> bool {
    if !event_before_query_cursor(event, query)
        || !event_after_query_cursor(event, query)
        || !event_view_allows_audience(event, query)
    {
        return false;
    }
    let view = event_view_record(event);
    if !query_csv_contains(query.level.as_deref(), &view.level) {
        return false;
    }
    if !query_csv_contains(query.category.as_deref(), &view.category) {
        return false;
    }
    if let Some(request_id) = query
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if view.request_id.as_deref() != Some(request_id) {
            return false;
        }
    }
    if let Some(q) = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let needle = q.to_ascii_lowercase();
        let detail = view
            .safe_detail
            .as_ref()
            .map(|value| value.to_string())
            .unwrap_or_default();
        let haystack = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            view.event_type,
            view.message,
            view.title,
            view.summary,
            view.request_id.clone().unwrap_or_default(),
            detail
        )
        .to_ascii_lowercase();
        if !haystack.contains(&needle) {
            return false;
        }
    }
    true
}

fn event_view_record(event: &EventRecord) -> EventViewRecord {
    let detail = event.detail.clone();
    let object = detail.as_ref().and_then(Value::as_object);
    let category = event_view_category(event, object).to_owned();
    let request_id = event_request_id(object);
    let session_hint = event_session_hint(object);
    let risk_flags = event_risk_flags(event, object);
    let metrics = event_metrics(object);
    EventViewRecord {
        id: event.id,
        ts: event.ts.clone(),
        level: event.level.clone(),
        severity: event.level.clone(),
        event_type: event.event_type.clone(),
        audience: event_audience(event).to_owned(),
        category,
        title: event_title(event, object),
        summary: event_summary(event, object),
        request_id,
        session_hint,
        risk_flags,
        metrics,
        safe_detail: detail,
        message: event.message.clone(),
        cursor: Some(event_cursor(event)),
    }
}

fn event_view_category(event: &EventRecord, object: Option<&Map<String, Value>>) -> &'static str {
    let event_type = event.event_type.as_str();
    if event.level == "error" {
        return "error";
    }
    if event_type == "web_search_source_probe"
        || event_type.contains("web_search")
        || detail_string(object, "name").as_deref() == Some("web_search")
    {
        return "web";
    }
    if event_type.starts_with("deepseek_tool_protocol") {
        return "protocol";
    }
    if event_type.contains("tool")
        || event_type.contains("handoff")
        || event_type == "apply_patch_input_micro_repair_diagnostic"
    {
        return "tool";
    }
    if event_type.contains("context")
        || event_type.contains("usage")
        || event_type.contains("cache")
        || event_type == "cost_risk_diagnostic"
        || event_type == "request_shape_diagnostic"
    {
        return "context";
    }
    if event_type.starts_with("request_") || event_type == "service_request_diagnostic" {
        return "request";
    }
    if event_type.contains("config") || event_type.contains("security") {
        return "security";
    }
    "system"
}

fn event_request_id(object: Option<&Map<String, Value>>) -> Option<String> {
    detail_string(object, "id")
        .or_else(|| detail_string(object, "request_id"))
        .or_else(|| detail_string(object, "response_id"))
}

fn event_session_hint(object: Option<&Map<String, Value>>) -> Option<String> {
    detail_string(object, "lifecycle")
        .or_else(|| detail_string(object, "phase"))
        .or_else(|| detail_string(object, "name"))
}

fn event_title(event: &EventRecord, object: Option<&Map<String, Value>>) -> String {
    match event.event_type.as_str() {
        "request_started" => "Request started".to_owned(),
        "request_completed" => "Request completed".to_owned(),
        "request_failed" => "Request failed".to_owned(),
        "service_request_diagnostic" => "Service request classified".to_owned(),
        "tool_call" => format!(
            "Tool call{}",
            detail_string(object, "name")
                .map(|name| format!(": {name}"))
                .unwrap_or_default()
        ),
        "tool_result" => format!(
            "Tool result{}",
            detail_string(object, "name")
                .map(|name| format!(": {name}"))
                .unwrap_or_default()
        ),
        "tool_exposure_diagnostic" => "Tool exposure".to_owned(),
        "context_compile_diagnostic" => "Context compiled".to_owned(),
        "upstream_call_usage_breakdown" => "Upstream usage".to_owned(),
        "upstream_usage_chunk_diagnostic" => "Usage chunks merged".to_owned(),
        "client_tool_handoff_diagnostic" => "Client tool handoff".to_owned(),
        "client_tool_handoff_guard_diagnostic" => "Handoff guard".to_owned(),
        "retry_cache_diagnostic" => "Retry cache diagnostic".to_owned(),
        "cost_risk_diagnostic" => "Cost risk warning".to_owned(),
        "apply_patch_input_micro_repair_diagnostic" => "Patch input micro repair".to_owned(),
        "deepseek_tool_protocol_adapted" => "DeepSeek protocol adapted".to_owned(),
        "deepseek_tool_protocol_blocked" => "DeepSeek protocol blocked".to_owned(),
        "deepseek_tool_protocol_parse_failed" => "DeepSeek protocol parse failed".to_owned(),
        "web_search_source_probe" => "Search source probe".to_owned(),
        _ => {
            let message = event.message.trim();
            if message.is_empty() {
                event.event_type.replace('_', " ")
            } else {
                compact_log_string(message, 96)
            }
        }
    }
}

fn event_summary(event: &EventRecord, object: Option<&Map<String, Value>>) -> String {
    match event.event_type.as_str() {
        "request_completed" => compact_log_string(
            &[
                model_summary(object),
                metric_summary(object, "duration_ms", "ms"),
                token_triplet_summary(object),
                detail_string(object, "lifecycle"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "request_started" => compact_log_string(
            &[
                detail_string(object, "endpoint"),
                model_summary(object),
                detail_string(object, "previous_response_id")
                    .map(|value| format!("previous={}", compact_log_string(&value, 48))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "request_failed" => compact_log_string(
            &[
                model_summary(object),
                detail_string(object, "message")
                    .or_else(|| detail_string(object, "error"))
                    .map(|value| format!("error={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "tool_result" => compact_log_string(
            &[
                detail_string(object, "name").map(|value| format!("tool={value}")),
                detail_bool(object, "ok").map(|ok| format!("ok={ok}")),
                detail_string(object, "summary"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "tool_call" => compact_log_string(
            &[
                detail_string(object, "name").map(|value| format!("tool={value}")),
                detail_string(object, "scope").map(|value| format!("scope={value}")),
                detail_u64(object, "iteration").map(|value| format!("iteration={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "context_compile_diagnostic" => context_summary(object),
        "client_tool_handoff_diagnostic" => context_summary(object),
        "upstream_call_usage_breakdown" => usage_breakdown_summary(object),
        "retry_cache_diagnostic" => compact_log_string(
            &[
                model_summary(object),
                detail_string(object, "error_kind").map(|value| format!("error={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "deepseek_tool_protocol_adapted"
        | "deepseek_tool_protocol_blocked"
        | "deepseek_tool_protocol_parse_failed" => compact_log_string(
            &[
                detail_string(object, "channel").map(|value| format!("channel={value}")),
                detail_u64(object, "tool_count").map(|value| format!("tools={value}")),
                detail_string(object, "tool_names").map(|value| format!("names={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        "web_search_source_probe" => web_source_summary(object),
        "cost_risk_diagnostic" => compact_log_string(
            &[
                detail_array_summary(object, "warnings").map(|value| format!("warnings={value}")),
                detail_u64(object, "estimated_text_chars").map(|value| format!("chars={value}")),
                detail_u64(object, "input_items").map(|value| format!("items={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" - "),
            MAX_LOG_SUMMARY_CHARS,
        ),
        _ => compact_log_string(&event.message, MAX_LOG_SUMMARY_CHARS),
    }
}

fn event_risk_flags(event: &EventRecord, object: Option<&Map<String, Value>>) -> Vec<String> {
    let mut flags = Vec::new();
    if event.level == "error" {
        push_unique_flag(&mut flags, "error");
    } else if event.level == "warn" {
        push_unique_flag(&mut flags, "warning");
    }
    match event.event_type.as_str() {
        "request_failed" => push_unique_flag(&mut flags, "request_failed"),
        "cost_risk_diagnostic" => {
            if let Some(warnings) = detail_array_strings(object, "warnings") {
                for warning in warnings {
                    push_unique_flag(&mut flags, &warning);
                }
            } else {
                push_unique_flag(&mut flags, "cost_risk");
            }
        }
        "deepseek_tool_protocol_blocked" => push_unique_flag(&mut flags, "protocol_blocked"),
        "deepseek_tool_protocol_parse_failed" => {
            push_unique_flag(&mut flags, "protocol_parse_failed")
        }
        "retry_cache_diagnostic" => push_unique_flag(&mut flags, "retry_cache"),
        "client_tool_handoff_guard_diagnostic" => push_unique_flag(&mut flags, "handoff_guard"),
        "apply_patch_input_micro_repair_diagnostic" => {
            push_unique_flag(&mut flags, "patch_micro_repair")
        }
        event_type if event_type.ends_with("_stopped") => {
            push_unique_flag(&mut flags, "loop_stopped")
        }
        _ => {}
    }
    if detail_bool(object, "ok") == Some(false) {
        push_unique_flag(&mut flags, "tool_failed");
    }
    if nested_u64(object, "context", "truncated_tool_output_items").unwrap_or(0) > 0 {
        push_unique_flag(&mut flags, "tool_output_truncated");
    }
    if nested_u64(object, "context", "display_only_thinking_items").unwrap_or(0) > 0 {
        push_unique_flag(&mut flags, "display_thinking_dropped");
    }
    if nested_u64(object, "usage", "cache_miss_input_tokens").unwrap_or(0) >= 20_000 {
        push_unique_flag(&mut flags, "high_cache_miss");
    }
    if nested_u64(object, "usage", "input_tokens").unwrap_or(0) >= 100_000 {
        push_unique_flag(&mut flags, "high_input");
    }
    if web_source_unreachable(object) {
        push_unique_flag(&mut flags, "web_source_unreachable");
    }
    flags
}

fn event_metrics(object: Option<&Map<String, Value>>) -> Value {
    let mut metrics = Map::new();
    for key in [
        "duration_ms",
        "cost_cny",
        "input_tokens",
        "cached_input_tokens",
        "cache_miss_input_tokens",
        "output_tokens",
        "total_tokens",
        "iteration",
        "input_items",
        "estimated_text_chars",
        "estimated_upstream_message_tokens",
        "tool_count",
    ] {
        copy_metric(object, &mut metrics, key);
    }
    for (object_key, keys) in [
        (
            "usage",
            &[
                "input_tokens",
                "cached_input_tokens",
                "cache_miss_input_tokens",
                "output_tokens",
                "total_tokens",
            ][..],
        ),
        (
            "context",
            &[
                "input_items",
                "message_items",
                "tool_result_items",
                "display_only_thinking_items",
                "tool_output_chars",
                "estimated_chars",
            ][..],
        ),
        ("payload", &["message_count", "tools_count"][..]),
        (
            "result_size",
            &[
                "result_json_chars",
                "diagnostic_bytes",
                "diagnostic_opened_count",
                "diagnostic_failure_count",
            ][..],
        ),
    ] {
        if let Some(nested) = object
            .and_then(|object| object.get(object_key))
            .and_then(Value::as_object)
        {
            for key in keys {
                if let Some(value) = nested.get(*key).filter(|value| value.is_number()) {
                    metrics.insert((*key).to_owned(), value.clone());
                }
            }
        }
    }
    Value::Object(metrics)
}

fn compact_log_string(value: &str, limit: usize) -> String {
    let cleaned = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= limit {
        cleaned
    } else {
        let keep = limit.saturating_sub(3);
        format!("{}...", cleaned.chars().take(keep).collect::<String>())
    }
}

fn push_unique_flag(flags: &mut Vec<String>, flag: &str) {
    if !flags.iter().any(|existing| existing == flag) {
        flags.push(flag.to_owned());
    }
}

fn copy_metric(object: Option<&Map<String, Value>>, output: &mut Map<String, Value>, key: &str) {
    if let Some(value) = object
        .and_then(|object| object.get(key))
        .filter(|value| value.is_number())
    {
        output.insert(key.to_owned(), value.clone());
    }
}

fn detail_string(object: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    object
        .and_then(|object| object.get(key))
        .and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            Value::Array(values) => Some(
                values
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => None,
        })
}

fn detail_u64(object: Option<&Map<String, Value>>, key: &str) -> Option<u64> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_u64)
}

fn detail_bool(object: Option<&Map<String, Value>>, key: &str) -> Option<bool> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
}

fn nested_u64(object: Option<&Map<String, Value>>, object_key: &str, key: &str) -> Option<u64> {
    object
        .and_then(|object| object.get(object_key))
        .and_then(Value::as_object)
        .and_then(|nested| nested.get(key))
        .and_then(Value::as_u64)
}

fn detail_array_strings(object: Option<&Map<String, Value>>, key: &str) -> Option<Vec<String>> {
    let values = object
        .and_then(|object| object.get(key))
        .and_then(Value::as_array)?;
    Some(
        values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect::<Vec<_>>(),
    )
}

fn detail_array_summary(object: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    detail_array_strings(object, key).map(|values| values.join(", "))
}

fn model_summary(object: Option<&Map<String, Value>>) -> Option<String> {
    let requested = detail_string(object, "requested_model");
    let model = detail_string(object, "model");
    match (requested, model) {
        (Some(requested), Some(model)) if requested != model => {
            Some(format!("model={requested}->{model}"))
        }
        (Some(model), _) | (_, Some(model)) => Some(format!("model={model}")),
        _ => None,
    }
}

fn metric_summary(object: Option<&Map<String, Value>>, key: &str, suffix: &str) -> Option<String> {
    detail_u64(object, key).map(|value| format!("{key}={value}{suffix}"))
}

fn token_triplet_summary(object: Option<&Map<String, Value>>) -> Option<String> {
    let input = detail_u64(object, "input_tokens")
        .or_else(|| nested_u64(object, "usage", "input_tokens"))?;
    let miss = detail_u64(object, "cache_miss_input_tokens")
        .or_else(|| nested_u64(object, "usage", "cache_miss_input_tokens"))
        .unwrap_or(0);
    let output = detail_u64(object, "output_tokens")
        .or_else(|| nested_u64(object, "usage", "output_tokens"))
        .unwrap_or(0);
    Some(format!("input={input}, miss={miss}, output={output}"))
}

fn context_summary(object: Option<&Map<String, Value>>) -> String {
    compact_log_string(
        &[
            nested_u64(object, "context", "input_items").map(|value| format!("items={value}")),
            nested_u64(object, "context", "message_items").map(|value| format!("messages={value}")),
            nested_u64(object, "context", "tool_result_items")
                .map(|value| format!("tool_results={value}")),
            nested_u64(object, "context", "display_only_thinking_items")
                .map(|value| format!("thinking_dropped={value}")),
            nested_u64(object, "context", "estimated_chars").map(|value| format!("chars={value}")),
            token_triplet_summary(object),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" - "),
        MAX_LOG_SUMMARY_CHARS,
    )
}

fn usage_breakdown_summary(object: Option<&Map<String, Value>>) -> String {
    compact_log_string(
        &[
            detail_u64(object, "iteration").map(|value| format!("iteration={value}")),
            detail_string(object, "phase").map(|value| format!("phase={value}")),
            token_triplet_summary(object),
            nested_u64(object, "payload", "tools_count").map(|value| format!("tools={value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" - "),
        MAX_LOG_SUMMARY_CHARS,
    )
}

fn web_source_summary(object: Option<&Map<String, Value>>) -> String {
    compact_log_string(
        &[
            detail_string(object, "stage").map(|value| format!("stage={value}")),
            detail_string(object, "trigger").map(|value| format!("trigger={value}")),
            detail_array_summary(object, "source_order").map(|value| format!("order={value}")),
            web_unreachable_count(object).map(|value| format!("unreachable={value}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" - "),
        MAX_LOG_SUMMARY_CHARS,
    )
}

fn web_source_unreachable(object: Option<&Map<String, Value>>) -> bool {
    web_unreachable_count(object).unwrap_or(0) > 0
}

fn web_unreachable_count(object: Option<&Map<String, Value>>) -> Option<usize> {
    let health = object
        .and_then(|object| {
            object.get("source_health").or_else(|| {
                object
                    .get("web_search")
                    .and_then(|value| value.get("source_health"))
            })
        })
        .and_then(Value::as_array)?;
    Some(
        health
            .iter()
            .filter(|item| item.get("reachable").and_then(Value::as_bool) == Some(false))
            .count(),
    )
}

fn is_safe_diagnostic_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "apply_patch_input_micro_repair_diagnostic"
            | "client_tool_handoff_diagnostic"
            | "context_compile_diagnostic"
            | "client_tool_handoff_guard_diagnostic"
            | "cost_risk_diagnostic"
            | "deepseek_tool_protocol_adapted"
            | "deepseek_tool_protocol_blocked"
            | "deepseek_tool_protocol_parse_failed"
            | "retry_cache_diagnostic"
            | "tool_exposure_diagnostic"
            | "upstream_usage_chunk_diagnostic"
            | "upstream_call_usage_breakdown"
    )
}

fn is_diagnostic_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "chat_stream_started"
            | "context_diagnostic"
            | "context_response_diagnostic"
            | "log_maintenance_completed"
            | "manager_action"
            | "manager_config_saved"
            | "mixed_tool_turn_split"
            | "models_requested"
            | "process_stderr"
            | "process_stdout"
            | "request_shape_diagnostic"
            | "previous_response_resolution_warning"
            | "runtime_context_storage"
            | "runtime_context_storage_warning"
            | "tool_lifecycle"
            | "tool_loop_iteration"
    )
}

fn diagnostic_logs_enabled() -> bool {
    std::env::var("CODESEEX_DIAGNOSTIC_LOGS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn compact_event_detail(event_type: &str, detail: &Value) -> Option<Value> {
    let Some(object) = detail.as_object() else {
        return Some(redact_log_value(&compact_log_value(
            detail,
            MAX_LOG_STRING_CHARS,
        )));
    };
    let mut output = Map::new();
    match event_type {
        "request_started" => {
            copy_log_fields(
                object,
                &mut output,
                &[
                    "id",
                    "endpoint",
                    "requested_model",
                    "model",
                    "previous_response_id",
                    "resolved_previous_response_id",
                    "history_messages",
                ],
            );
            copy_structured_log_fields(
                object,
                &mut output,
                &["previous_response_resolution", "runtime_context_storage"],
            );
        }
        "previous_response_resolution_warning" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "requires_full_context_for_lossless_replay"],
            );
            copy_structured_log_fields(object, &mut output, &["previous_response_resolution"]);
        }
        "runtime_context_storage" | "runtime_context_storage_warning" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "requires_full_context_for_lossless_replay"],
            );
            copy_structured_log_fields(object, &mut output, &["runtime_context_storage"]);
        }
        "request_completed" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "status",
                "requested_model",
                "model",
                "lifecycle",
                "duration_ms",
                "cost_cny",
                "input_tokens",
                "cached_input_tokens",
                "cache_miss_input_tokens",
                "output_tokens",
                "total_tokens",
            ],
        ),
        "service_request_diagnostic" => {
            copy_log_fields(
                object,
                &mut output,
                &[
                    "id",
                    "endpoint",
                    "kind",
                    "tools_suppressed",
                    "thinking_disabled",
                    "lifecycle",
                    "estimated_text_chars",
                    "input_items",
                    "max_output_tokens",
                ],
            );
            copy_structured_log_fields(object, &mut output, &["route", "signals"]);
        }
        "context_compile_diagnostic" => {
            copy_log_fields(object, &mut output, &["id"]);
            copy_safe_diagnostic_fields(
                object,
                &mut output,
                &[
                    (
                        "request",
                        &[
                            "input_items",
                            "has_prompt_cache_key",
                            "has_client_metadata",
                            "has_previous_response_id",
                            "request_hash",
                            "input_hash",
                        ][..],
                    ),
                    ("runtime_context_storage", &[][..]),
                    (
                        "context",
                        &[
                            "input_items",
                            "message_items",
                            "tool_result_items",
                            "verified_fact_items",
                            "display_only_items",
                            "display_only_thinking_items",
                            "display_only_chars",
                            "tool_output_chars",
                            "truncated_tool_output_items",
                            "unsupported_items",
                            "truncated_items",
                            "estimated_chars",
                            "history_messages",
                            "current_messages",
                            "tool_facts",
                            "recovered_tool_facts",
                            "current_input",
                            "budget_mode",
                            "protected_start_index",
                            "budget",
                        ][..],
                    ),
                ],
            );
        }
        "upstream_call_usage_breakdown" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "phase", "iteration", "duration_ms", "final_handoff"],
            );
            copy_safe_diagnostic_fields(
                object,
                &mut output,
                &[
                    (
                        "request",
                        &[
                            "input_items",
                            "has_prompt_cache_key",
                            "has_client_metadata",
                            "has_previous_response_id",
                            "request_hash",
                            "input_hash",
                        ][..],
                    ),
                    (
                        "payload",
                        &["message_count", "tools_count", "payload_hash"][..],
                    ),
                    (
                        "usage",
                        &[
                            "input_tokens",
                            "cached_input_tokens",
                            "cache_miss_input_tokens",
                            "output_tokens",
                            "total_tokens",
                        ][..],
                    ),
                ],
            );
        }
        "upstream_usage_chunk_diagnostic" => copy_log_fields(
            object,
            &mut output,
            &["id", "iteration", "usage_chunks", "merge_mode"],
        ),
        "cost_risk_diagnostic" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "endpoint",
                "mode",
                "warnings",
                "estimated_text_chars",
                "input_items",
                "estimated_upstream_message_tokens",
                "codex_full_context",
            ],
        ),
        "client_tool_handoff_diagnostic" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "phase", "iteration", "lifecycle"],
            );
            copy_safe_diagnostic_fields(
                object,
                &mut output,
                &[
                    (
                        "request",
                        &[
                            "input_items",
                            "has_prompt_cache_key",
                            "has_client_metadata",
                            "has_previous_response_id",
                            "request_hash",
                            "input_hash",
                        ][..],
                    ),
                    ("runtime_context_storage", &[][..]),
                    (
                        "context",
                        &[
                            "input_items",
                            "message_items",
                            "tool_result_items",
                            "verified_fact_items",
                            "display_only_items",
                            "display_only_thinking_items",
                            "display_only_chars",
                            "tool_output_chars",
                            "truncated_tool_output_items",
                            "unsupported_items",
                            "truncated_items",
                            "estimated_chars",
                            "history_messages",
                            "current_messages",
                            "tool_facts",
                            "recovered_tool_facts",
                            "current_input",
                            "budget_mode",
                            "protected_start_index",
                            "budget",
                        ][..],
                    ),
                    ("tools", &[][..]),
                    (
                        "usage",
                        &[
                            "input_tokens",
                            "cached_input_tokens",
                            "cache_miss_input_tokens",
                            "output_tokens",
                            "total_tokens",
                        ][..],
                    ),
                ],
            );
        }
        "deepseek_tool_protocol_adapted"
        | "deepseek_tool_protocol_blocked"
        | "deepseek_tool_protocol_parse_failed" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "iteration",
                "channel",
                "tool_count",
                "tool_names",
                "tools_available",
            ],
        ),
        "client_tool_handoff_guard_diagnostic" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "code",
                "reason",
                "turn_key",
                "tool_name",
                "arguments_hash",
                "failure_summary",
                "failure_summary_hash",
                "handoff_requests",
                "tool_calls",
                "repeated_signature_count",
                "consecutive_failure_count",
                "cumulative_input_tokens",
                "cumulative_total_tokens",
            ],
        ),
        "apply_patch_input_micro_repair_diagnostic" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "call_id",
                "tool_name",
                "repair_kind",
                "blank_context_lines_repaired",
                "input_chars",
            ],
        ),
        "retry_cache_diagnostic" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "requested_model", "model", "error_kind"],
            );
            copy_safe_diagnostic_fields(
                object,
                &mut output,
                &[
                    (
                        "request",
                        &[
                            "input_items",
                            "has_prompt_cache_key",
                            "has_client_metadata",
                            "has_previous_response_id",
                            "request_hash",
                            "input_hash",
                        ][..],
                    ),
                    (
                        "payload",
                        &["message_count", "tools_count", "payload_hash"][..],
                    ),
                ],
            );
        }
        "request_failed" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "status",
                "requested_model",
                "model",
                "error",
                "message",
                "upstream_error",
            ],
        ),
        "tool_loop_repeated_failure_stopped" | "tool_loop_web_search_budget_stopped" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "iteration", "error", "recover_with_final_response"],
            )
        }
        "tool_loop_iteration_limit_stopped" => {
            copy_log_fields(object, &mut output, &["id", "iteration", "limit", "error"])
        }
        "tool_loop_repeated_call" => {
            copy_log_fields(object, &mut output, &["id", "call_id", "name", "iteration"])
        }
        "tool_call" => copy_log_fields(object, &mut output, &["id", "name", "scope", "iteration"]),
        "tool_result" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "name", "scope", "iteration", "ok"],
            );
            copy_safe_diagnostic_fields(
                object,
                &mut output,
                &[
                    (
                        "result_size",
                        &[
                            "result_json_chars",
                            "result_hash",
                            "ok",
                            "diagnostic_bytes",
                            "diagnostic_opened_count",
                            "diagnostic_failure_count",
                        ][..],
                    ),
                    (
                        "web_search",
                        &[
                            "source_plan",
                            "source_order",
                            "source_health",
                            "sources_attempted",
                            "sources_deprioritized",
                            "source_diagnostics",
                            "fallback_errors",
                            "browser_fallback",
                        ][..],
                    ),
                ],
            );
            if let Some(summary) = object.get("summary") {
                output.insert(
                    "summary".to_owned(),
                    compact_log_value(summary, MAX_LOG_SUMMARY_CHARS),
                );
            }
        }
        "web_search_source_probe" => {
            copy_log_fields(
                object,
                &mut output,
                &[
                    "ok",
                    "stage",
                    "proxy_key",
                    "trigger",
                    "debounce_ms",
                    "network_proxy_signature",
                ],
            );
            copy_structured_log_fields(object, &mut output, &["source_order", "source_health"]);
        }
        "context_compaction_completed" | "context_compacted" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "compaction_id",
                "message_count",
                "tool_fact_count",
                "summary_chars",
                "mode",
                "estimated_tokens",
                "threshold_tokens",
            ],
        ),
        "request_shape_diagnostic" => {
            copy_log_fields(
                object,
                &mut output,
                &[
                    "id",
                    "endpoint",
                    "requested_model",
                    "model",
                    "service_routing",
                    "codex_service_request",
                    "codex_service_kind",
                    "service_classification_source",
                    "thinking_policy",
                    "has_previous_response_id",
                    "has_instructions",
                    "has_context_management",
                    "input_items",
                    "input_kind",
                    "estimated_text_chars",
                    "tools_count",
                    "max_output_tokens",
                    "reasoning_effort",
                    "text_format",
                    "store",
                    "metadata_keys",
                    "client_metadata",
                    "prompt_cache_key",
                    "has_title_task_signal",
                    "has_suggestion_task_signal",
                ],
            );
            copy_structured_log_fields(object, &mut output, &["service_signals"]);
        }
        "tool_exposure_diagnostic" => {
            copy_log_fields(
                object,
                &mut output,
                &[
                    "id",
                    "incoming_tool_items",
                    "discovered_tool_items",
                    "codeseex_base_tools_injected",
                    "configurable_tools_disabled_by_config",
                    "warning",
                ],
            );
            copy_structured_log_fields(
                object,
                &mut output,
                &[
                    "codeseex_enabled_tools",
                    "codeseex_expected_upstream_tools",
                    "missing_expected_codeseex_tools",
                    "external_callable_tools",
                    "external_upstream_tools",
                    "external_tool_budget",
                    "final_upstream_tools",
                    "codex_request_markers",
                    "tool_search_bridge",
                    "interesting_tools",
                ],
            );
        }
        _ => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "endpoint",
                "status",
                "model",
                "requested_model",
                "action",
                "mode",
                "path",
                "base_url",
                "host",
                "port",
                "error",
                "message",
            ],
        ),
    }
    if output.is_empty() {
        None
    } else {
        Some(redact_log_value(&Value::Object(output)))
    }
}

fn copy_log_fields(object: &Map<String, Value>, output: &mut Map<String, Value>, keys: &[&str]) {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        output.insert((*key).to_owned(), compact_log_field_value(key, value));
    }
}

fn copy_structured_log_fields(
    object: &Map<String, Value>,
    output: &mut Map<String, Value>,
    keys: &[&str],
) {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        output.insert((*key).to_owned(), compact_structured_log_value(value));
    }
}

fn copy_safe_diagnostic_fields(
    object: &Map<String, Value>,
    output: &mut Map<String, Value>,
    fields: &[(&str, &[&str])],
) {
    for (key, allowed_fields) in fields {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let compacted = if allowed_fields.is_empty() {
            compact_safe_diagnostic_value(value)
        } else {
            compact_safe_diagnostic_object(value, allowed_fields)
        };
        if !compacted.is_null() {
            output.insert((*key).to_owned(), compacted);
        }
    }
}

fn compact_safe_diagnostic_object(value: &Value, allowed_fields: &[&str]) -> Value {
    let Some(object) = value.as_object() else {
        return Value::Null;
    };
    let mut output = Map::new();
    for key in allowed_fields {
        let Some(value) = object.get(*key) else {
            continue;
        };
        let compacted = compact_safe_diagnostic_value(value);
        if compacted.is_null() {
            continue;
        }
        output.insert((*key).to_owned(), compacted);
    }
    Value::Object(output)
}

fn compact_safe_diagnostic_value(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(value) => {
            if safe_diagnostic_string(value) {
                Value::String(value.clone())
            } else {
                Value::Null
            }
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .take(MAX_LOG_ARRAY_ITEMS)
                .filter_map(|value| {
                    let compacted = compact_safe_diagnostic_value(value);
                    (!compacted.is_null()).then_some(compacted)
                })
                .collect(),
        ),
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, value) in object.iter().take(MAX_LOG_ARRAY_ITEMS) {
                let compacted = compact_safe_diagnostic_value(value);
                if !compacted.is_null() {
                    output.insert(key.clone(), compacted);
                }
            }
            Value::Object(output)
        }
    }
}

fn safe_diagnostic_string(value: &str) -> bool {
    value.chars().count() <= 128
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '/' | '.'))
}

fn compact_log_field_value(key: &str, value: &Value) -> Value {
    match key {
        "summary" => compact_log_value(value, MAX_LOG_SUMMARY_CHARS),
        "upstream_error" => compact_upstream_error(value),
        _ => compact_log_value(value, MAX_LOG_STRING_CHARS),
    }
}

fn compact_structured_log_value(value: &Value) -> Value {
    match value {
        Value::String(value) => {
            Value::String(truncate_chars_with_hash(value, MAX_LOG_STRING_CHARS))
        }
        Value::Array(values) => {
            let mut output = values
                .iter()
                .take(MAX_LOG_ARRAY_ITEMS)
                .map(compact_structured_log_value)
                .collect::<Vec<_>>();
            if values.len() > MAX_LOG_ARRAY_ITEMS {
                output.push(json!({
                    "_codeseex_log_notice": "array tail omitted from diagnostic log detail",
                    "omitted_items": values.len().saturating_sub(MAX_LOG_ARRAY_ITEMS)
                }));
            }
            Value::Array(output)
        }
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, value) in object.iter().take(MAX_LOG_ARRAY_ITEMS) {
                output.insert(key.clone(), compact_structured_log_value(value));
            }
            if object.len() > MAX_LOG_ARRAY_ITEMS {
                output.insert(
                    "_codeseex_log_notice".to_owned(),
                    json!("object tail omitted from diagnostic log detail"),
                );
                output.insert(
                    "omitted_fields".to_owned(),
                    json!(object.len().saturating_sub(MAX_LOG_ARRAY_ITEMS)),
                );
            }
            Value::Object(output)
        }
        _ => value.clone(),
    }
}

fn compact_upstream_error(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return compact_log_value(value, MAX_LOG_SUMMARY_CHARS);
    };
    let mut output = Map::new();
    copy_log_fields(
        object,
        &mut output,
        &["status", "code", "type", "error", "message"],
    );
    if output.is_empty() {
        compact_log_value(value, MAX_LOG_SUMMARY_CHARS)
    } else {
        Value::Object(output)
    }
}

fn compact_log_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(value) => Value::String(truncate_chars_with_hash(value, max_string_chars)),
        Value::Array(values) => {
            let mut output = values
                .iter()
                .take(MAX_LOG_ARRAY_ITEMS)
                .map(|value| compact_log_value(value, max_string_chars))
                .collect::<Vec<_>>();
            if values.len() > MAX_LOG_ARRAY_ITEMS {
                output.push(json!({
                    "_codeseex_log_notice": "array tail omitted from user log detail",
                    "omitted_items": values.len().saturating_sub(MAX_LOG_ARRAY_ITEMS)
                }));
            }
            Value::Array(output)
        }
        Value::Object(object) => Value::String(truncate_chars_with_hash(
            &serde_json::to_string(object).unwrap_or_default(),
            max_string_chars,
        )),
        _ => value.clone(),
    }
}

fn push_request_order(order: &mut VecDeque<String>, id: &str) {
    if let Some(index) = order.iter().position(|existing| existing == id) {
        order.remove(index);
    }
    order.push_back(id.to_owned());
}

fn bump_usage_revision(inner: &mut StoreInner) {
    inner.usage_revision = inner.usage_revision.saturating_add(1);
    inner.usage_cache = None;
}

fn bump_event_revision(inner: &mut StoreInner) {
    inner.event_revision = inner.event_revision.saturating_add(1);
}

fn event_affects_usage(event_type: &str) -> bool {
    matches!(
        event_type,
        "tool_call"
            | "tool_result"
            | "upstream_call_usage_breakdown"
            | "upstream_usage_chunk_diagnostic"
            | "client_tool_handoff_guard_diagnostic"
    ) || event_type.contains("usage")
}

fn prune_runtime_requests(inner: &mut StoreInner) {
    if mark_stale_in_progress_requests(inner) {
        bump_usage_revision(inner);
    }
    let mut scanned = 0_usize;
    while inner.requests.len() > MAX_RUNTIME_REQUESTS {
        let Some(id) = inner.request_order.pop_front() else {
            break;
        };
        if matches!(
            inner.requests.get(&id).map(|request| request.status),
            Some(RequestStatus::InProgress)
        ) {
            inner.request_order.push_back(id);
            scanned = scanned.saturating_add(1);
            if scanned >= inner.request_order.len() {
                break;
            }
            continue;
        }
        inner.requests.remove(&id);
        scanned = 0;
    }
}

fn mark_stale_in_progress_requests(inner: &mut StoreInner) -> bool {
    let now = Utc::now();
    let mut changed = false;
    for request in inner.requests.values_mut() {
        if request.status != RequestStatus::InProgress {
            continue;
        }
        if now.signed_duration_since(request.created_at).num_seconds() < IN_PROGRESS_TTL_SECONDS {
            continue;
        }
        request.status = RequestStatus::Interrupted;
        request.diagnostic = Some(json!({
            "reason": "in_progress request exceeded runtime TTL",
            "ttl_seconds": IN_PROGRESS_TTL_SECONDS
        }));
        request.updated_at = now;
        changed = true;
    }
    changed
}

fn completed_conversation_turns(inner: &StoreInner) -> Vec<RequestTurn> {
    let mut turns = inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request_is_completed_final_turn(request))
        .filter_map(turn_from_request)
        .collect::<Vec<_>>();
    if turns.len() > MAX_RUNTIME_TURNS {
        turns.drain(0..turns.len() - MAX_RUNTIME_TURNS);
    }
    turns
}

fn completed_billable_requests(inner: &StoreInner) -> Vec<RequestTurn> {
    let mut requests = inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request_is_completed_billable_request(request))
        .filter_map(turn_from_request)
        .collect::<Vec<_>>();
    if requests.len() > MAX_RUNTIME_TURNS {
        requests.drain(0..requests.len() - MAX_RUNTIME_TURNS);
    }
    requests
}

fn runtime_summary_from_inner(
    inner: &StoreInner,
    turn_history: Vec<RequestTurn>,
    billable_history: Vec<RequestTurn>,
) -> RuntimeSummary {
    let active_requests = inner
        .requests
        .values()
        .filter(|request| request.status == RequestStatus::InProgress)
        .count() as u64;
    let request_count = inner
        .requests
        .values()
        .filter(|request| request_is_completed_final_turn(request))
        .count() as u64;
    let billable_request_count = inner
        .requests
        .values()
        .filter(|request| request_is_completed_billable_request(request))
        .count() as u64;
    let failed_request_count = inner
        .requests
        .values()
        .filter(|request| {
            matches!(
                request.status,
                RequestStatus::Failed | RequestStatus::Interrupted
            )
        })
        .count() as u64;
    let last_turn = turn_history.last().cloned();
    let last_billable_request = billable_history.last().cloned();
    let last_request_at = last_billable_request
        .as_ref()
        .or(last_turn.as_ref())
        .map(|turn| turn.completed_at.clone());
    let usage_sessions = usage_sessions_from_inner(inner, &turn_history, &billable_history);
    let last_activity_at = usage_sessions
        .iter()
        .map(|session| session.completed_at.clone())
        .chain(inner.events.iter().map(|event| event.ts.clone()))
        .max()
        .or_else(|| last_request_at.clone());
    let billable_totals = inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request_is_completed_billable_request(request))
        .filter_map(turn_from_request)
        .collect::<Vec<_>>();
    let total_cached_input_tokens = billable_totals
        .iter()
        .map(|turn| turn.cached_input_tokens)
        .sum();
    let total_cache_miss_input_tokens = billable_totals
        .iter()
        .map(|turn| turn.cache_miss_input_tokens)
        .sum();
    let total_output_tokens = billable_totals.iter().map(|turn| turn.output_tokens).sum();
    let average_ms = if billable_totals.is_empty() {
        0
    } else {
        billable_totals
            .iter()
            .map(|turn| turn.request_ms)
            .sum::<u64>()
            / u64::try_from(billable_totals.len()).unwrap_or(1)
    };
    RuntimeSummary {
        usage_revision: inner.usage_revision,
        event_revision: inner.event_revision,
        active_requests,
        request_count,
        billable_request_count,
        failed_request_count,
        last_request_at,
        last_activity_at,
        last_turn,
        last_billable_request,
        turn_history,
        billable_history,
        usage_sessions,
        total_cached_input_tokens,
        total_cache_miss_input_tokens,
        total_output_tokens,
        average_ms,
    }
}

fn cached_usage_summary(inner: &mut StoreInner) -> RuntimeSummary {
    if let Some(cache) = inner.usage_cache.as_ref() {
        if cache.revision == inner.usage_revision {
            return cache.summary.clone();
        }
    }
    let turns = completed_conversation_turns(inner);
    let billable = completed_billable_requests(inner);
    let summary = runtime_summary_from_inner(inner, turns, billable);
    inner.usage_cache = Some(UsageRuntimeCache {
        revision: inner.usage_revision,
        summary: summary.clone(),
    });
    summary
}

fn usage_page_from_summary(
    summary: &RuntimeSummary,
    limit: u32,
    cursor: Option<&str>,
    billing_buckets: Vec<UsageBillingBucket>,
) -> UsagePage {
    let limit = usize::try_from(limit.clamp(1, 500)).unwrap_or(60);
    let mut sessions = summary
        .usage_sessions
        .iter()
        .rev()
        .filter(|session| usage_session_is_before_cursor(session, cursor))
        .map(usage_session_summary)
        .take(limit + 1)
        .collect::<Vec<_>>();
    let has_more = sessions.len() > limit;
    if has_more {
        sessions.truncate(limit);
    }
    let next_cursor = has_more
        .then(|| sessions.last().map(usage_session_summary_cursor))
        .flatten();
    UsagePage {
        usage_revision: summary.usage_revision,
        event_revision: summary.event_revision,
        unchanged: false,
        active_requests: summary.active_requests,
        request_count: summary.request_count,
        billable_request_count: summary.billable_request_count,
        failed_request_count: summary.failed_request_count,
        last_request_at: summary.last_request_at.clone(),
        last_activity_at: summary.last_activity_at.clone(),
        total_cached_input_tokens: summary.total_cached_input_tokens,
        total_cache_miss_input_tokens: summary.total_cache_miss_input_tokens,
        total_output_tokens: summary.total_output_tokens,
        average_ms: summary.average_ms,
        billing_buckets,
        usage_sessions: sessions,
        has_more,
        next_cursor,
    }
}

fn usage_session_summary(session: &UsageSession) -> UsageSessionSummary {
    UsageSessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        title_source: session.title_source.clone(),
        completed_at: session.completed_at.clone(),
        conversation_turn: session.conversation_turn,
        status: session.status.clone(),
        cached_input_tokens: session.cached_input_tokens,
        cache_miss_input_tokens: session.cache_miss_input_tokens,
        output_tokens: session.output_tokens,
        total_tokens: session.total_tokens,
        request_ms: session.request_ms,
        billing_buckets: usage_billing_buckets_from_rows(&session.rows),
        row_count: session.rows.len(),
        segment_count: session.segments.len(),
        session_revision: stable_hash_hex(
            serde_json::to_string(session)
                .unwrap_or_default()
                .as_bytes(),
        ),
    }
}

fn usage_session_summary_cursor(session: &UsageSessionSummary) -> String {
    format!("{}|{}", session.completed_at, session.id)
}

fn usage_session_cursor(session: &UsageSession) -> String {
    format!("{}|{}", session.completed_at, session.id)
}

fn usage_session_is_before_cursor(session: &UsageSession, cursor: Option<&str>) -> bool {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    usage_session_cursor(session).as_str() < cursor
}

fn usage_billing_buckets(inner: &StoreInner) -> Vec<UsageBillingBucket> {
    let mut buckets = HashMap::<(String, String), UsageBillingBucket>::new();
    for turn in inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request_is_completed_billable_request(request))
        .filter_map(turn_from_request)
    {
        let key = (turn.model.clone(), turn.requested_model.clone());
        let bucket = buckets.entry(key).or_insert_with(|| UsageBillingBucket {
            model: turn.model.clone(),
            requested_model: turn.requested_model.clone(),
            cached_input_tokens: 0,
            cache_miss_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        });
        bucket.cached_input_tokens = bucket
            .cached_input_tokens
            .saturating_add(turn.cached_input_tokens);
        bucket.cache_miss_input_tokens = bucket
            .cache_miss_input_tokens
            .saturating_add(turn.cache_miss_input_tokens);
        bucket.output_tokens = bucket.output_tokens.saturating_add(turn.output_tokens);
        bucket.total_tokens = bucket.total_tokens.saturating_add(turn.total_tokens);
    }
    let mut buckets = buckets.into_values().collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        left.model
            .cmp(&right.model)
            .then(left.requested_model.cmp(&right.requested_model))
    });
    buckets
}

fn usage_billing_buckets_from_rows(rows: &[UsageSessionRow]) -> Vec<UsageBillingBucket> {
    let mut buckets = HashMap::<(String, String), UsageBillingBucket>::new();
    for row in rows {
        let key = (row.model.clone(), row.requested_model.clone());
        let bucket = buckets.entry(key).or_insert_with(|| UsageBillingBucket {
            model: row.model.clone(),
            requested_model: row.requested_model.clone(),
            cached_input_tokens: 0,
            cache_miss_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        });
        bucket.cached_input_tokens = bucket
            .cached_input_tokens
            .saturating_add(row.cached_input_tokens);
        bucket.cache_miss_input_tokens = bucket
            .cache_miss_input_tokens
            .saturating_add(row.cache_miss_input_tokens);
        bucket.output_tokens = bucket.output_tokens.saturating_add(row.output_tokens);
        bucket.total_tokens = bucket.total_tokens.saturating_add(row.total_tokens);
    }
    let mut buckets = buckets.into_values().collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        left.model
            .cmp(&right.model)
            .then(left.requested_model.cmp(&right.requested_model))
    });
    buckets
}

fn usage_sessions_from_inner(
    inner: &StoreInner,
    turn_history: &[RequestTurn],
    billable_history: &[RequestTurn],
) -> Vec<UsageSession> {
    let mut billable_by_id = billable_history
        .iter()
        .cloned()
        .map(|turn| (turn.id.clone(), turn))
        .collect::<HashMap<_, _>>();
    let mut sessions = Vec::new();
    let final_turn_ids = turn_history
        .iter()
        .map(|turn| turn.id.as_str())
        .collect::<HashSet<_>>();
    let mut handoff_ids_by_final_id = orphan_handoffs_by_final_turn(inner, turn_history);
    let mut guard_stop_ids_by_final_id = guard_stopped_requests_by_final_turn(inner, turn_history);

    for final_turn in turn_history {
        let chain = usage_chain_for_final_turn(inner, &final_turn.id);
        let mut rows = Vec::new();
        for id in chain {
            if let Some(turn) = billable_by_id.remove(&id) {
                rows.push(usage_session_row(&turn, turn.id == final_turn.id));
            }
        }
        if let Some(handoff_ids) = handoff_ids_by_final_id.remove(&final_turn.id) {
            let existing = rows
                .iter()
                .map(|row| row.id.clone())
                .collect::<HashSet<_>>();
            for id in handoff_ids {
                if existing.contains(&id) {
                    continue;
                }
                if let Some(turn) = billable_by_id.remove(&id) {
                    rows.push(usage_session_row(&turn, false));
                }
            }
        }
        if let Some(guard_stop_ids) = guard_stop_ids_by_final_id.remove(&final_turn.id) {
            let existing = rows
                .iter()
                .map(|row| row.id.clone())
                .collect::<HashSet<_>>();
            for id in guard_stop_ids {
                if existing.contains(&id) {
                    continue;
                }
                if let Some(turn) = billable_by_id.remove(&id) {
                    rows.push(usage_session_row(&turn, false));
                }
            }
        }
        if rows.is_empty() {
            rows.push(usage_session_row(final_turn, true));
        }
        rows.sort_by(|left, right| {
            row_sort_ts(inner, &left.id)
                .cmp(&row_sort_ts(inner, &right.id))
                .then(left.id.cmp(&right.id))
        });
        sessions.push(usage_session_from_rows(
            final_turn,
            inner.requests.get(&final_turn.id),
            rows,
            true,
            inner,
        ));
    }

    let mut non_final_rows = billable_by_id
        .values()
        .filter(|turn| {
            turn.lifecycle == "client_tool_handoff"
                || request_has_guard_stopped_by_id(inner, &turn.id)
        })
        .filter_map(|turn| {
            let request = inner.requests.get(&turn.id)?;
            Some((
                usage_non_final_session_key(request),
                request.created_at,
                turn.id.clone(),
            ))
        })
        .collect::<Vec<_>>();
    non_final_rows.sort_by(|left, right| left.1.cmp(&right.1).then(left.2.cmp(&right.2)));
    let mut non_final_session_ids = HashSet::new();
    for (session_key, _, _id) in non_final_rows {
        if !non_final_session_ids.insert(session_key.clone()) {
            continue;
        }
        let ids = billable_by_id
            .values()
            .filter_map(|turn| {
                let request = inner.requests.get(&turn.id)?;
                (usage_non_final_session_key(request) == session_key).then_some(turn.id.clone())
            })
            .collect::<Vec<_>>();
        let mut rows = Vec::new();
        for id in ids {
            if let Some(turn) = billable_by_id.remove(&id) {
                rows.push(usage_session_row(&turn, false));
            }
        }
        rows.sort_by(|left, right| {
            row_sort_ts(inner, &left.id)
                .cmp(&row_sort_ts(inner, &right.id))
                .then(left.id.cmp(&right.id))
        });
        if let Some(anchor) = rows.last().cloned() {
            sessions.push(usage_non_final_session_from_rows(inner, anchor, rows));
        }
    }

    for turn in billable_history {
        if let Some(turn) = billable_by_id.remove(&turn.id) {
            if (turn.lifecycle == "client_tool_handoff"
                || request_has_guard_stopped_by_id(inner, &turn.id))
                && !final_turn_ids.contains(turn.id.as_str())
            {
                continue;
            }
            let rows = vec![usage_session_row(&turn, false)];
            sessions.push(usage_session_from_rows(
                &turn,
                inner.requests.get(&turn.id),
                rows,
                false,
                inner,
            ));
        }
    }

    let mut session_ids = sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<HashSet<_>>();
    for request in inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request.status == RequestStatus::InProgress)
    {
        if session_ids.contains(&request.id)
            || request_has_service_diagnostic_event(inner, &request.id)
        {
            continue;
        }
        if let Some(session) = usage_session_from_active_request(inner, request) {
            if let Some(anchor_id) = usage_active_session_anchor(request, &sessions, inner) {
                if let Some(existing) = sessions.iter_mut().find(|session| session.id == anchor_id)
                {
                    merge_active_usage_session(existing, session);
                }
            } else {
                session_ids.insert(session.id.clone());
                sessions.push(session);
            }
        }
    }

    sessions.sort_by(|left, right| left.completed_at.cmp(&right.completed_at));
    sessions
}

fn usage_chain_for_final_turn(inner: &StoreInner, final_id: &str) -> Vec<String> {
    let mut newest_first = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = Some(final_id.to_owned());
    while let Some(id) = cursor.take() {
        if !visited.insert(id.clone()) {
            break;
        }
        let Some(request) = inner.requests.get(&id) else {
            break;
        };
        newest_first.push(id);
        cursor = request.previous_response_id.clone();
    }
    newest_first.reverse();
    newest_first
}

fn orphan_handoffs_by_final_turn(
    inner: &StoreInner,
    turn_history: &[RequestTurn],
) -> HashMap<String, Vec<String>> {
    let chain_ids = turn_history
        .iter()
        .flat_map(|turn| usage_chain_for_final_turn(inner, &turn.id))
        .collect::<HashSet<_>>();
    let mut finals = turn_history
        .iter()
        .filter_map(|turn| {
            let request = inner.requests.get(&turn.id)?;
            Some(UsageFinalAnchor {
                id: turn.id.clone(),
                created_at: request.created_at,
                summary_key: usage_summary_key(request),
                full_context_key: usage_full_context_key(request),
                prompt_cache_key: usage_prompt_cache_key(request),
            })
        })
        .collect::<Vec<_>>();
    finals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut output: HashMap<String, Vec<String>> = HashMap::new();
    for request in inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
    {
        if chain_ids.contains(&request.id) || request_lifecycle(request) != "client_tool_handoff" {
            continue;
        }
        let Some(anchor_id) = usage_final_anchor_for_handoff(request, &finals) else {
            continue;
        };
        output
            .entry(anchor_id)
            .or_default()
            .push(request.id.clone());
    }
    output
}

fn guard_stopped_requests_by_final_turn(
    inner: &StoreInner,
    turn_history: &[RequestTurn],
) -> HashMap<String, Vec<String>> {
    let mut finals = turn_history
        .iter()
        .filter_map(|turn| {
            let request = inner.requests.get(&turn.id)?;
            Some(UsageFinalAnchor {
                id: turn.id.clone(),
                created_at: request.created_at,
                summary_key: usage_summary_key(request),
                full_context_key: usage_full_context_key(request),
                prompt_cache_key: usage_prompt_cache_key(request),
            })
        })
        .collect::<Vec<_>>();
    finals.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.id.cmp(&right.id))
    });
    let mut output: HashMap<String, Vec<String>> = HashMap::new();
    for request in inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
    {
        if !request_has_guard_stopped(request) {
            continue;
        }
        let Some(anchor_id) = usage_final_anchor_for_handoff(request, &finals) else {
            continue;
        };
        output
            .entry(anchor_id)
            .or_default()
            .push(request.id.clone());
    }
    output
}

#[derive(Debug)]
struct UsageFinalAnchor {
    id: String,
    created_at: DateTime<Utc>,
    summary_key: Option<String>,
    full_context_key: Option<String>,
    prompt_cache_key: Option<String>,
}

fn usage_final_anchor_for_handoff(
    handoff: &StoredRequest,
    finals: &[UsageFinalAnchor],
) -> Option<String> {
    let summary_key = usage_summary_key(handoff);
    let full_context_key = usage_full_context_key(handoff);
    let prompt_cache_key = usage_prompt_cache_key(handoff);
    let mut fallback = None;
    for final_turn in finals {
        if final_turn.created_at < handoff.created_at {
            continue;
        }
        let delay = final_turn
            .created_at
            .signed_duration_since(handoff.created_at);
        if delay > Duration::minutes(20) {
            break;
        }
        if summary_key.is_some() && summary_key == final_turn.summary_key {
            return Some(final_turn.id.clone());
        }
        if full_context_key.is_some() && full_context_key == final_turn.full_context_key {
            return Some(final_turn.id.clone());
        }
        if prompt_cache_key.is_some() && prompt_cache_key == final_turn.prompt_cache_key {
            fallback = Some(final_turn.id.clone());
        } else if fallback.is_none() && delay <= Duration::minutes(3) {
            fallback = Some(final_turn.id.clone());
        }
    }
    fallback
}

fn row_sort_ts(inner: &StoreInner, request_id: &str) -> DateTime<Utc> {
    inner
        .requests
        .get(request_id)
        .map(|request| request.created_at)
        .unwrap_or_else(Utc::now)
}

fn usage_summary_key(request: &StoredRequest) -> Option<String> {
    latest_user_summary(request).map(|summary| stable_hash_hex(summary.as_bytes()))
}

fn usage_full_context_key(request: &StoredRequest) -> Option<String> {
    request
        .input
        .pointer("/_codeseex_runtime/original_input_hash")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn usage_prompt_cache_key(request: &StoredRequest) -> Option<String> {
    request
        .input
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| stable_hash_hex(value.as_bytes()))
}

fn usage_non_final_session_key(request: &StoredRequest) -> String {
    usage_summary_key(request)
        .map(|key| format!("summary:{key}"))
        .or_else(|| usage_full_context_key(request).map(|key| format!("full_context:{key}")))
        .or_else(|| usage_prompt_cache_key(request).map(|key| format!("prompt_cache:{key}")))
        .unwrap_or_else(|| format!("request:{}", request.id))
}

fn request_has_guard_stopped(request: &StoredRequest) -> bool {
    request
        .diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.get("client_tool_handoff_guard_stopped"))
        .and_then(Value::as_bool)
        == Some(true)
}

fn request_has_guard_stopped_by_id(inner: &StoreInner, id: &str) -> bool {
    inner
        .requests
        .get(id)
        .map(request_has_guard_stopped)
        .unwrap_or(false)
}

fn usage_session_from_rows(
    anchor: &RequestTurn,
    anchor_request: Option<&StoredRequest>,
    rows: Vec<UsageSessionRow>,
    conversation_turn: bool,
    inner: &StoreInner,
) -> UsageSession {
    let cached_input_tokens = rows.iter().map(|row| row.cached_input_tokens).sum();
    let cache_miss_input_tokens = rows.iter().map(|row| row.cache_miss_input_tokens).sum();
    let output_tokens = rows.iter().map(|row| row.output_tokens).sum();
    let total_tokens = rows.iter().map(|row| row.total_tokens).sum();
    let request_ms = rows.iter().map(|row| row.request_ms).sum();
    let status = if rows.iter().any(|row| row.status == "failed") {
        "failed"
    } else {
        "completed"
    }
    .to_owned();
    let (title, title_source) = usage_session_title(anchor, anchor_request);
    let segments = usage_session_segments(inner, &rows);
    UsageSession {
        id: anchor.id.clone(),
        title,
        title_source,
        completed_at: anchor.completed_at.clone(),
        conversation_turn,
        status,
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms,
        segments,
        technical_details: usage_session_technical_details(anchor, &rows),
        rows,
    }
}

fn usage_session_from_active_request(
    inner: &StoreInner,
    request: &StoredRequest,
) -> Option<UsageSession> {
    let anchor = turn_from_request(request)?;
    let completed_at =
        latest_request_event_ts(inner, &request.id).unwrap_or_else(|| anchor.completed_at.clone());
    let row = UsageSessionRow {
        id: request.id.clone(),
        kind: "in_progress_reply".to_owned(),
        label: "usage_in_progress_reply".to_owned(),
        hint: "usage_in_progress_reply_hint".to_owned(),
        model: anchor.model.clone(),
        requested_model: anchor.requested_model.clone(),
        reasoning_effort: anchor.reasoning_effort.clone(),
        lifecycle: anchor.lifecycle.clone(),
        status: "running".to_owned(),
        billable: false,
        completed_at: completed_at.clone(),
        cached_input_tokens: 0,
        cache_miss_input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        request_ms: request_ms(request.created_at, Utc::now()),
    };
    let segments = usage_request_event_segments(inner, &row);
    let cached_input_tokens = segments
        .iter()
        .map(|segment| segment.cached_input_tokens)
        .sum();
    let cache_miss_input_tokens = segments
        .iter()
        .map(|segment| segment.cache_miss_input_tokens)
        .sum();
    let output_tokens = segments.iter().map(|segment| segment.output_tokens).sum();
    let total_tokens = segments.iter().map(|segment| segment.total_tokens).sum();
    let (mut title, mut title_source) = latest_user_summary(request)
        .filter(|summary| !summary.trim().is_empty())
        .map(|summary| (summary, "user_summary".to_owned()))
        .unwrap_or_else(|| usage_session_title(&anchor, Some(request)));
    if title == "usage_record" || title == "intermediate_reply" {
        title = "in_progress_reply".to_owned();
        title_source = "semantic".to_owned();
    }
    Some(UsageSession {
        id: request.id.clone(),
        title,
        title_source,
        completed_at,
        conversation_turn: true,
        status: "running".to_owned(),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: request_ms(request.created_at, Utc::now()),
        segments,
        rows: Vec::new(),
        technical_details: usage_session_technical_details(&anchor, &[]),
    })
}

fn usage_non_final_session_from_rows(
    inner: &StoreInner,
    anchor_row: UsageSessionRow,
    rows: Vec<UsageSessionRow>,
) -> UsageSession {
    let anchor_request = inner.requests.get(&anchor_row.id);
    let title_turn = RequestTurn {
        id: anchor_row.id.clone(),
        model: anchor_row.model.clone(),
        requested_model: anchor_row.requested_model.clone(),
        reasoning_effort: anchor_row.reasoning_effort.clone(),
        lifecycle: anchor_row.lifecycle.clone(),
        conversation_turn: true,
        billable: anchor_row.billable,
        completed_at: anchor_row.completed_at.clone(),
        cached_input_tokens: anchor_row.cached_input_tokens,
        cache_miss_input_tokens: anchor_row.cache_miss_input_tokens,
        output_tokens: anchor_row.output_tokens,
        total_tokens: anchor_row.total_tokens,
        request_ms: anchor_row.request_ms,
    };
    let cached_input_tokens = rows.iter().map(|row| row.cached_input_tokens).sum();
    let cache_miss_input_tokens = rows.iter().map(|row| row.cache_miss_input_tokens).sum();
    let output_tokens = rows.iter().map(|row| row.output_tokens).sum();
    let total_tokens = rows.iter().map(|row| row.total_tokens).sum();
    let request_ms = rows.iter().map(|row| row.request_ms).sum();
    let status = if rows.iter().any(|row| row.status == "failed") {
        "failed"
    } else {
        "completed"
    }
    .to_owned();
    let (title, title_source) = usage_session_title(&title_turn, anchor_request);
    let segments = usage_session_segments(inner, &rows);
    UsageSession {
        id: anchor_row.id.clone(),
        title,
        title_source,
        completed_at: anchor_row.completed_at.clone(),
        conversation_turn: false,
        status,
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms,
        segments,
        rows,
        technical_details: usage_session_technical_details(&title_turn, &[]),
    }
}

fn usage_active_session_anchor(
    request: &StoredRequest,
    sessions: &[UsageSession],
    inner: &StoreInner,
) -> Option<String> {
    let summary_key = usage_summary_key(request);
    let full_context_key = usage_full_context_key(request);
    let prompt_cache_key = usage_prompt_cache_key(request);
    let mut fallback = None;
    for session in sessions.iter().rev() {
        let Some(anchor_request) = inner.requests.get(&session.id) else {
            continue;
        };
        let anchor_ts = row_sort_ts(inner, &session.id);
        if anchor_ts > request.created_at {
            continue;
        }
        let delay = request.created_at.signed_duration_since(anchor_ts);
        if delay > Duration::minutes(20) {
            break;
        }
        let can_merge_into_session = session.status == "running" || !session.conversation_turn;
        if !can_merge_into_session {
            continue;
        }
        if summary_key.is_some() && summary_key == usage_summary_key(anchor_request) {
            return Some(session.id.clone());
        }
        if full_context_key.is_some() && full_context_key == usage_full_context_key(anchor_request)
        {
            return Some(session.id.clone());
        }
        if prompt_cache_key.is_some()
            && prompt_cache_key == usage_prompt_cache_key(anchor_request)
            && session.status == "running"
        {
            fallback = Some(session.id.clone());
        } else if fallback.is_none() && delay <= Duration::minutes(3) && session.status == "running"
        {
            fallback = Some(session.id.clone());
        }
    }
    fallback
}

fn merge_active_usage_session(existing: &mut UsageSession, active: UsageSession) {
    existing.status = if existing.status == "failed" {
        "failed".to_owned()
    } else {
        "running".to_owned()
    };
    existing.completed_at = active.completed_at;
    existing.request_ms = existing.request_ms.saturating_add(active.request_ms);
    existing.cached_input_tokens = existing
        .cached_input_tokens
        .saturating_add(active.cached_input_tokens);
    existing.cache_miss_input_tokens = existing
        .cache_miss_input_tokens
        .saturating_add(active.cache_miss_input_tokens);
    existing.output_tokens = existing.output_tokens.saturating_add(active.output_tokens);
    existing.total_tokens = existing.total_tokens.saturating_add(active.total_tokens);
    existing.segments.extend(active.segments);
    existing.technical_details.extend(active.technical_details);
}

fn usage_session_row(turn: &RequestTurn, is_final: bool) -> UsageSessionRow {
    let kind = if is_final {
        "final_reply"
    } else if turn.lifecycle == "service_ephemeral" {
        "service"
    } else if turn.lifecycle == "client_tool_handoff" {
        "intermediate_reply"
    } else if turn.lifecycle == "failed_billable" {
        "failed_reply"
    } else {
        "intermediate_reply"
    };
    UsageSessionRow {
        id: turn.id.clone(),
        kind: kind.to_owned(),
        label: usage_row_label_key(turn, kind),
        hint: usage_row_hint_key(turn, kind),
        model: turn.model.clone(),
        requested_model: turn.requested_model.clone(),
        reasoning_effort: turn.reasoning_effort.clone(),
        lifecycle: turn.lifecycle.clone(),
        status: if turn.lifecycle == "failed_billable" {
            "failed"
        } else {
            "completed"
        }
        .to_owned(),
        billable: turn.billable,
        completed_at: turn.completed_at.clone(),
        cached_input_tokens: turn.cached_input_tokens,
        cache_miss_input_tokens: turn.cache_miss_input_tokens,
        output_tokens: turn.output_tokens,
        total_tokens: turn.total_tokens,
        request_ms: turn.request_ms,
    }
}

fn usage_session_segments(inner: &StoreInner, rows: &[UsageSessionRow]) -> Vec<UsageSegment> {
    let mut segments = Vec::new();
    for row in rows {
        segments.extend(usage_request_event_segments(inner, row));
        if !segments.iter().any(|segment| {
            segment
                .rows
                .iter()
                .any(|existing_row| existing_row.id == row.id)
        }) {
            segments.push(usage_segment_from_row(row));
        }
    }
    if segments.is_empty() {
        rows.iter().map(usage_segment_from_row).collect()
    } else {
        segments
    }
}

fn usage_request_event_segments(inner: &StoreInner, row: &UsageSessionRow) -> Vec<UsageSegment> {
    let mut events = inner
        .events
        .iter()
        .filter(|event| event_detail_id(event) == Some(row.id.as_str()))
        .collect::<Vec<_>>();
    events.sort_by(|left, right| left.ts.cmp(&right.ts).then(left.id.cmp(&right.id)));

    let mut segments = Vec::new();
    let completed_tool_keys = events
        .iter()
        .filter(|event| event.event_type == "tool_result")
        .filter_map(|event| usage_tool_event_key(event))
        .collect::<HashSet<_>>();
    let has_tool_events = events
        .iter()
        .any(|event| matches!(event.event_type.as_str(), "tool_call" | "tool_result"));
    for event in events {
        match event.event_type.as_str() {
            "upstream_call_usage_breakdown" => {
                if has_tool_events {
                    if let Some(segment) = usage_model_segment_from_event(event, row) {
                        segments.push(segment);
                    }
                }
            }
            "tool_call" => {
                if let Some(key) = usage_tool_event_key(event) {
                    if !completed_tool_keys.contains(&key) {
                        if let Some(segment) = usage_tool_call_segment_from_event(event, row) {
                            segments.push(segment);
                        }
                    }
                }
            }
            "tool_result" => {
                if let Some(segment) = usage_tool_segment_from_event(event, row) {
                    segments.push(segment);
                }
            }
            _ => {}
        }
    }

    if usage_segments_have_billable_tokens(&segments) {
        if row.kind == "final_reply" {
            mark_last_billable_model_segment_as_final(&mut segments, row);
        } else {
            attach_row_to_last_billable_model_segment(&mut segments, row);
        }
        return segments;
    }

    segments.push(usage_segment_from_row(row));
    segments
}

fn usage_segments_have_billable_tokens(segments: &[UsageSegment]) -> bool {
    segments.iter().any(|segment| {
        segment.cached_input_tokens > 0
            || segment.cache_miss_input_tokens > 0
            || segment.output_tokens > 0
            || segment.total_tokens > 0
    })
}

fn mark_last_billable_model_segment_as_final(segments: &mut [UsageSegment], row: &UsageSessionRow) {
    let Some(segment) = segments.iter_mut().rev().find(|segment| {
        segment.tool_name.is_none()
            && (segment.cached_input_tokens > 0
                || segment.cache_miss_input_tokens > 0
                || segment.output_tokens > 0
                || segment.total_tokens > 0)
    }) else {
        return;
    };
    segment.kind = row.kind.clone();
    segment.label = row.label.clone();
    segment.hint = row.hint.clone();
    segment.status = row.status.clone();
    if segment.request_ms == 0 {
        segment.request_ms = row.request_ms;
    }
    attach_row_to_segment(segment, row);
}

fn attach_row_to_last_billable_model_segment(segments: &mut [UsageSegment], row: &UsageSessionRow) {
    let Some(segment) = segments.iter_mut().rev().find(|segment| {
        segment.tool_name.is_none()
            && (segment.cached_input_tokens > 0
                || segment.cache_miss_input_tokens > 0
                || segment.output_tokens > 0
                || segment.total_tokens > 0)
    }) else {
        return;
    };
    attach_row_to_segment(segment, row);
}

fn attach_row_to_segment(segment: &mut UsageSegment, row: &UsageSessionRow) {
    if !segment.rows.iter().any(|existing| existing.id == row.id) {
        segment.rows.push(row.clone());
    }
}

fn usage_segment_from_row(row: &UsageSessionRow) -> UsageSegment {
    UsageSegment {
        id: row.id.clone(),
        kind: row.kind.clone(),
        label: row.label.clone(),
        hint: row.hint.clone(),
        model: row.model.clone(),
        requested_model: row.requested_model.clone(),
        reasoning_effort: row.reasoning_effort.clone(),
        lifecycle: row.lifecycle.clone(),
        status: row.status.clone(),
        tool_name: None,
        iteration: None,
        summary: None,
        completed_at: Some(row.completed_at.clone()),
        cached_input_tokens: row.cached_input_tokens,
        cache_miss_input_tokens: row.cache_miss_input_tokens,
        output_tokens: row.output_tokens,
        total_tokens: row.total_tokens,
        request_ms: row.request_ms,
        rows: vec![row.clone()],
    }
}

fn usage_model_segment_from_event(
    event: &EventRecord,
    row: &UsageSessionRow,
) -> Option<UsageSegment> {
    let detail = event.detail.as_ref()?;
    let phase = detail
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let iteration = detail
        .get("iteration")
        .and_then(value_to_u64)
        .map(|value| value as u32);
    let usage = detail.get("usage").unwrap_or(&Value::Null);
    let cached_input_tokens = first_u64(
        usage,
        &[
            "cached_input_tokens",
            "input_cached_tokens",
            "prompt_cache_hit_tokens",
            "cache_hit_input_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .unwrap_or(0);
    let input_tokens = first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
    let cache_miss_input_tokens = first_u64(
        usage,
        &[
            "cache_miss_input_tokens",
            "input_cache_miss_tokens",
            "prompt_cache_miss_tokens",
            "cache_miss_tokens",
        ],
    )
    .unwrap_or_else(|| input_tokens.saturating_sub(cached_input_tokens));
    let output_tokens = first_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
    let total_tokens = first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        cached_input_tokens
            .saturating_add(cache_miss_input_tokens)
            .saturating_add(output_tokens)
    });
    if total_tokens == 0 && output_tokens == 0 && input_tokens == 0 {
        return None;
    }
    let is_final_handoff = detail
        .get("final_handoff")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let kind = if is_final_handoff {
        "client_handoff_model"
    } else if phase.contains("iteration") || phase.contains("continuation") {
        "model_iteration"
    } else {
        "model_request"
    };
    Some(UsageSegment {
        id: format!(
            "{}:model:{}",
            row.id,
            iteration
                .map(|value| value.to_string())
                .unwrap_or_else(|| event.id.to_string())
        ),
        kind: kind.to_owned(),
        label: usage_model_segment_label_key(kind).to_owned(),
        hint: usage_model_segment_hint_key(kind).to_owned(),
        model: row.model.clone(),
        requested_model: row.requested_model.clone(),
        reasoning_effort: row.reasoning_effort.clone(),
        lifecycle: row.lifecycle.clone(),
        status: row.status.clone(),
        tool_name: None,
        iteration,
        summary: Some(phase.to_owned()).filter(|value| !value.trim().is_empty()),
        completed_at: Some(event.ts.clone()),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: detail
            .get("duration_ms")
            .and_then(value_to_u64)
            .unwrap_or(0),
        rows: Vec::new(),
    })
}

fn usage_tool_segment_from_event(
    event: &EventRecord,
    row: &UsageSessionRow,
) -> Option<UsageSegment> {
    let detail = event.detail.as_ref()?;
    let tool_name = detail.get("name").and_then(Value::as_str)?.to_owned();
    let iteration = detail
        .get("iteration")
        .and_then(value_to_u64)
        .map(|value| value as u32);
    let ok = detail.get("ok").and_then(Value::as_bool);
    let status = if ok == Some(false) {
        "failed"
    } else {
        "completed"
    };
    Some(UsageSegment {
        id: format!(
            "{}:tool:{}:{}:{}",
            row.id,
            event.id,
            iteration
                .map(|value| value.to_string())
                .unwrap_or_else(|| event.id.to_string()),
            sanitize_segment_id(&tool_name)
        ),
        kind: "tool_result".to_owned(),
        label: usage_tool_segment_label_key(&tool_name).to_owned(),
        hint: if ok == Some(false) {
            "usage_tool_failed"
        } else {
            "usage_tool_completed"
        }
        .to_owned(),
        model: tool_name.clone(),
        requested_model: String::new(),
        reasoning_effort: String::new(),
        lifecycle: row.lifecycle.clone(),
        status: status.to_owned(),
        tool_name: Some(tool_name),
        iteration,
        summary: event
            .detail
            .as_ref()
            .and_then(|detail| detail.get("summary"))
            .and_then(compact_usage_segment_summary),
        completed_at: Some(event.ts.clone()),
        cached_input_tokens: 0,
        cache_miss_input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        request_ms: 0,
        rows: Vec::new(),
    })
}

fn usage_tool_call_segment_from_event(
    event: &EventRecord,
    row: &UsageSessionRow,
) -> Option<UsageSegment> {
    let detail = event.detail.as_ref()?;
    let tool_name = detail.get("name").and_then(Value::as_str)?.to_owned();
    let iteration = detail
        .get("iteration")
        .and_then(value_to_u64)
        .map(|value| value as u32);
    Some(UsageSegment {
        id: format!(
            "{}:tool-call:{}:{}:{}",
            row.id,
            event.id,
            iteration
                .map(|value| value.to_string())
                .unwrap_or_else(|| event.id.to_string()),
            sanitize_segment_id(&tool_name)
        ),
        kind: "tool_call".to_owned(),
        label: usage_tool_segment_label_key(&tool_name).to_owned(),
        hint: "usage_tool_requested".to_owned(),
        model: tool_name.clone(),
        requested_model: String::new(),
        reasoning_effort: String::new(),
        lifecycle: row.lifecycle.clone(),
        status: "running".to_owned(),
        tool_name: Some(tool_name),
        iteration,
        summary: None,
        completed_at: Some(event.ts.clone()),
        cached_input_tokens: 0,
        cache_miss_input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        request_ms: 0,
        rows: Vec::new(),
    })
}

fn usage_tool_event_key(event: &EventRecord) -> Option<String> {
    let detail = event.detail.as_ref()?;
    let name = detail.get("name").and_then(Value::as_str)?;
    let iteration = detail
        .get("iteration")
        .and_then(value_to_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| event.id.to_string());
    Some(format!("{iteration}:{name}"))
}

fn usage_model_segment_label_key(kind: &str) -> &str {
    match kind {
        "client_handoff_model" => "usage_client_handoff_model_stage",
        "model_iteration" => "usage_model_iteration",
        _ => "usage_model_request",
    }
}

fn usage_model_segment_hint_key(kind: &str) -> &str {
    match kind {
        "client_handoff_model" => "usage_client_handoff_model_stage_hint",
        "model_iteration" => "usage_model_iteration_hint",
        _ => "usage_model_request_hint",
    }
}

fn usage_tool_segment_label_key(tool_name: &str) -> &str {
    if tool_name == "web_search" {
        "usage_web_search_stage"
    } else {
        "usage_tool_stage"
    }
}

fn compact_usage_segment_summary(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Null => return None,
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_USAGE_SEGMENT_SUMMARY_CHARS {
        return Some(text.to_owned());
    }
    let prefix = text
        .chars()
        .take(MAX_USAGE_SEGMENT_SUMMARY_CHARS.saturating_sub(1))
        .collect::<String>();
    Some(format!("{prefix}..."))
}

fn event_detail_id(event: &EventRecord) -> Option<&str> {
    event.detail.as_ref()?.get("id").and_then(Value::as_str)
}

fn latest_request_event_ts(inner: &StoreInner, request_id: &str) -> Option<String> {
    inner
        .events
        .iter()
        .filter(|event| event_detail_id(event) == Some(request_id))
        .map(|event| event.ts.clone())
        .max()
}

fn request_has_service_diagnostic_event(inner: &StoreInner, request_id: &str) -> bool {
    inner.events.iter().any(|event| {
        event.event_type == "service_request_diagnostic"
            && event_detail_id(event) == Some(request_id)
    })
}

fn usage_input_tokens(usage: &Value) -> u64 {
    first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0)
}

fn usage_total_tokens(usage: &Value, input_tokens: u64) -> u64 {
    first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        input_tokens
            .saturating_add(first_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0))
    })
}

fn sanitize_segment_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn usage_session_title_key(turn: &RequestTurn) -> String {
    if turn.conversation_turn {
        return "conversation".to_owned();
    }
    match turn.lifecycle.as_str() {
        "client_tool_handoff" => "intermediate_reply".to_owned(),
        "service_ephemeral" => "service_request".to_owned(),
        "failed_billable" => "failed_billable".to_owned(),
        _ => "intermediate_reply".to_owned(),
    }
}

fn usage_session_title(anchor: &RequestTurn, request: Option<&StoredRequest>) -> (String, String) {
    if !anchor.conversation_turn {
        return (usage_session_title_key(anchor), "semantic".to_owned());
    }
    if let Some(summary) = request
        .and_then(latest_user_summary)
        .filter(|summary| !summary.trim().is_empty())
    {
        return (summary, "user_summary".to_owned());
    }
    (usage_session_title_key(anchor), "semantic".to_owned())
}

fn latest_user_summary(request: &StoredRequest) -> Option<String> {
    runtime_latest_user_summary(&request.input)
        .or_else(|| latest_user_summary_from_messages(&request.turn_messages))
        .or_else(|| latest_user_summary_from_input(&request.input))
}

fn runtime_latest_user_summary(value: &Value) -> Option<String> {
    value
        .pointer("/_codeseex_runtime/latest_user_summary")
        .and_then(Value::as_str)
        .and_then(compact_usage_title)
}

fn latest_user_summary_from_messages(messages: &[Value]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        message
            .get("content")
            .and_then(Value::as_str)
            .and_then(compact_usage_title)
    })
}

fn latest_user_summary_from_input(input: &Value) -> Option<String> {
    if let Some(text) = input.get("input").and_then(Value::as_str) {
        return compact_usage_title(text);
    }
    if let Some(messages) = input.get("messages").and_then(Value::as_array) {
        if let Some(summary) = latest_user_summary_from_messages(messages) {
            return Some(summary);
        }
    }
    let items = input.get("input").and_then(Value::as_array)?;
    items.iter().rev().find_map(|item| {
        if item.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        item.get("content")
            .map(content_to_text)
            .and_then(|text| compact_usage_title(&text))
    })
}

fn compact_usage_title(value: &str) -> Option<String> {
    let mut text = codex_user_request_text(value)
        .unwrap_or(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    if text.starts_with("# Selected text:") || text.starts_with("# Files mentioned by the user:") {
        return None;
    }
    let count = text.chars().count();
    if count > MAX_USAGE_SESSION_TITLE_CHARS {
        let prefix = text
            .chars()
            .take(MAX_USAGE_SESSION_TITLE_CHARS.saturating_sub(1))
            .collect::<String>();
        text = format!("{prefix}...");
    }
    Some(text)
}

fn codex_user_request_text(value: &str) -> Option<&str> {
    for marker in ["## My request for Codex:", "## My request:"] {
        if let Some((_, rest)) = value.split_once(marker) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

fn usage_row_label_key(turn: &RequestTurn, kind: &str) -> String {
    match kind {
        "final_reply" => "final_reply".to_owned(),
        "service" => "service_request".to_owned(),
        "failed_reply" => "failed_billable".to_owned(),
        _ if turn.lifecycle == "client_tool_handoff" => "intermediate_reply".to_owned(),
        _ => "intermediate".to_owned(),
    }
}

fn usage_row_hint_key(turn: &RequestTurn, kind: &str) -> String {
    match kind {
        "final_reply" => "completed_final_response".to_owned(),
        "service" => "background_service_request".to_owned(),
        "failed_reply" => "billable_failed_request".to_owned(),
        _ if turn.lifecycle == "client_tool_handoff" => "client_tool_handoff".to_owned(),
        _ => "billable_model_request".to_owned(),
    }
}

fn usage_session_technical_details(
    anchor: &RequestTurn,
    rows: &[UsageSessionRow],
) -> Vec<UsageTechnicalDetail> {
    let mut details = vec![
        UsageTechnicalDetail {
            label: "session id".to_owned(),
            value: anchor.id.clone(),
        },
        UsageTechnicalDetail {
            label: "lifecycle".to_owned(),
            value: anchor.lifecycle.clone(),
        },
        UsageTechnicalDetail {
            label: "billable rows".to_owned(),
            value: rows.len().to_string(),
        },
    ];
    if let Some(row) = rows.first() {
        details.push(UsageTechnicalDetail {
            label: "first request".to_owned(),
            value: row.id.clone(),
        });
    }
    if let Some(row) = rows.last() {
        details.push(UsageTechnicalDetail {
            label: "last request".to_owned(),
            value: row.id.clone(),
        });
    }
    details
}

fn request_is_completed_final_turn(request: &StoredRequest) -> bool {
    request.status == RequestStatus::Completed
        && request_lifecycle(request) == "final_turn"
        && request
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.get("client_tool_handoff_guard_stopped"))
            .and_then(Value::as_bool)
            != Some(true)
}

fn request_is_completed_billable_request(request: &StoredRequest) -> bool {
    matches!(
        request.status,
        RequestStatus::Completed | RequestStatus::Failed
    ) && request_has_billable_usage(request)
}

fn request_has_billable_usage(request: &StoredRequest) -> bool {
    usage_value(&request.response)
        .map(usage_has_tokens)
        .unwrap_or(false)
}

fn request_lifecycle(request: &StoredRequest) -> String {
    if let Some(lifecycle) = request
        .diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.get("codeseex_lifecycle"))
        .and_then(Value::as_str)
    {
        return lifecycle.to_owned();
    }
    if matches!(
        request.status,
        RequestStatus::Failed | RequestStatus::Interrupted
    ) {
        return "failed_billable".to_owned();
    }
    "final_turn".to_owned()
}

fn turn_from_request(request: &StoredRequest) -> Option<RequestTurn> {
    let usage = usage_value(&request.response).unwrap_or(&Value::Null);
    let cached_input_tokens = first_u64(
        usage,
        &[
            "cached_input_tokens",
            "input_cached_tokens",
            "prompt_cache_hit_tokens",
            "cache_hit_input_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .unwrap_or(0);
    let input_tokens = first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
    let cache_miss_input_tokens = first_u64(
        usage,
        &[
            "cache_miss_input_tokens",
            "input_cache_miss_tokens",
            "prompt_cache_miss_tokens",
            "cache_miss_tokens",
        ],
    )
    .unwrap_or_else(|| input_tokens.saturating_sub(cached_input_tokens));
    let output_tokens = first_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
    let total_tokens = first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        cached_input_tokens
            .saturating_add(cache_miss_input_tokens)
            .saturating_add(output_tokens)
    });
    Some(RequestTurn {
        id: request.id.clone(),
        model: request
            .response
            .get("model")
            .and_then(Value::as_str)
            .or(request.model.as_deref())
            .unwrap_or_default()
            .to_owned(),
        requested_model: request
            .input
            .get("model")
            .and_then(Value::as_str)
            .or(request.model.as_deref())
            .unwrap_or_default()
            .to_owned(),
        reasoning_effort: request_reasoning_effort(request),
        lifecycle: request_lifecycle(request),
        conversation_turn: request_is_completed_final_turn(request),
        billable: request_has_billable_usage(request),
        completed_at: request.updated_at.to_rfc3339(),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: request_ms(request.created_at, request.updated_at),
    })
}

fn request_reasoning_effort(request: &StoredRequest) -> String {
    let service_thinking_disabled = request_lifecycle(request) == "service_ephemeral"
        || request
            .diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.get("thinking_disabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if service_thinking_disabled {
        return "none".to_owned();
    }
    let effort = request
        .input
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .or_else(|| {
            request
                .input
                .pointer("/_codeseex_runtime/reasoning_effort")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            request
                .diagnostic
                .as_ref()
                .and_then(|diagnostic| diagnostic.get("reasoning_effort"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if !effort.is_empty() {
        return effort;
    }
    String::new()
}

fn usage_value(response: &Value) -> Option<&Value> {
    response
        .get("usage")
        .or_else(|| response.pointer("/response/usage"))
        .or_else(|| response.pointer("/choices/0/usage"))
}

fn usage_has_tokens(usage: &Value) -> bool {
    let input_tokens = first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
    let cached_input_tokens = first_u64(
        usage,
        &[
            "cached_input_tokens",
            "input_cached_tokens",
            "prompt_cache_hit_tokens",
            "cache_hit_input_tokens",
            "cached_tokens",
        ],
    )
    .or_else(|| {
        usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(value_to_u64)
    })
    .unwrap_or(0);
    let cache_miss_input_tokens = first_u64(
        usage,
        &[
            "cache_miss_input_tokens",
            "input_cache_miss_tokens",
            "prompt_cache_miss_tokens",
            "cache_miss_tokens",
        ],
    )
    .unwrap_or_else(|| input_tokens.saturating_sub(cached_input_tokens));
    let output_tokens = first_u64(usage, &["output_tokens", "completion_tokens"]).unwrap_or(0);
    let total_tokens = first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        cached_input_tokens
            .saturating_add(cache_miss_input_tokens)
            .saturating_add(output_tokens)
    });
    total_tokens > 0
        || input_tokens > 0
        || cached_input_tokens > 0
        || cache_miss_input_tokens > 0
        || output_tokens > 0
}

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(value_to_u64)
}

fn value_to_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value as u64)
    })
}

fn request_ms(created_at: DateTime<Utc>, updated_at: DateTime<Utc>) -> u64 {
    let millis = updated_at
        .signed_duration_since(created_at)
        .num_milliseconds();
    u64::try_from(millis).unwrap_or(0)
}

fn request_input_for_runtime(_previous_response_id: Option<&str>, value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return memory_json_value(value);
    };
    let mut stored = Map::new();
    for key in ["model", "prompt_cache_key"] {
        if let Some(value) = object.get(key) {
            stored.insert(key.to_owned(), memory_json_value(value));
        }
    }
    let reasoning_effort = value
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    if let Some(effort) = reasoning_effort.as_ref() {
        stored.insert(
            "_codeseex_runtime".to_owned(),
            json!({ "reasoning_effort": effort }),
        );
    }
    if let Some(input) = object.get("input") {
        if request_looks_like_codex_full_context(value) {
            let item_count = input.as_array().map(Vec::len).unwrap_or(0);
            let latest_user_summary = latest_user_summary_from_input(value);
            stored.insert("input".to_owned(), Value::Array(Vec::new()));
            let mut runtime = json!({
                "mode": "codex_full_context_not_stored",
                "reason": "Codex owns and resends full conversation context; CodeSeeX keeps no duplicate transcript.",
                "original_input_items": item_count,
                "original_input_hash": stable_hash_hex(&serde_json::to_vec(input).unwrap_or_default())
            });
            if let (Some(object), Some(summary)) = (runtime.as_object_mut(), latest_user_summary) {
                object.insert("latest_user_summary".to_owned(), Value::String(summary));
            }
            if let (Some(object), Some(effort)) =
                (runtime.as_object_mut(), reasoning_effort.as_ref())
            {
                object.insert("reasoning_effort".to_owned(), Value::String(effort.clone()));
            }
            stored.insert("_codeseex_runtime".to_owned(), runtime);
        } else {
            stored.insert("input".to_owned(), memory_json_value(input));
        }
        return Value::Object(stored);
    }
    if let Some(messages) = object.get("messages") {
        stored.insert("messages".to_owned(), memory_json_value(messages));
        return Value::Object(stored);
    }
    memory_json_value(value)
}

fn memory_json_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(sanitize_memory_string(value)),
        Value::Array(values) => {
            if values.len() <= MAX_MEMORY_ARRAY_ITEMS {
                return Value::Array(values.iter().map(memory_json_value).collect());
            }
            let mut output = values
                .iter()
                .take(MAX_MEMORY_ARRAY_ITEMS / 2)
                .map(memory_json_value)
                .collect::<Vec<_>>();
            output.push(json!({
                "_codeseex_runtime_notice": "array middle omitted from in-memory adapter state",
                "omitted_items": values.len().saturating_sub(MAX_MEMORY_ARRAY_ITEMS)
            }));
            output.extend(
                values
                    .iter()
                    .skip(values.len().saturating_sub(MAX_MEMORY_ARRAY_ITEMS / 2))
                    .map(memory_json_value),
            );
            Value::Array(output)
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), memory_json_value(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn redact_log_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(sanitize_log_string(value)),
        Value::Array(values) => Value::Array(values.iter().map(redact_log_value).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    if is_sensitive_key(key) {
                        (key.clone(), Value::String("[redacted]".to_owned()))
                    } else {
                        (key.clone(), redact_log_value(value))
                    }
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("authorization")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("access_token")
        || key.contains("refresh_token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("cookie")
}

fn sanitize_memory_string(value: &str) -> String {
    if looks_like_inline_data_url(value) {
        return format!(
            "[redacted inline data url; original_chars={}; hash={}]",
            value.chars().count(),
            stable_hash_hex(value.as_bytes())
        );
    }
    truncate_chars_with_hash(value, MAX_MEMORY_STRING_CHARS)
}

fn sanitize_log_string(value: &str) -> String {
    if looks_like_inline_data_url(value) {
        return format!(
            "[redacted inline data url; original_chars={}; hash={}]",
            value.chars().count(),
            stable_hash_hex(value.as_bytes())
        );
    }
    truncate_chars_with_hash(&redact_secret_text(value), MAX_LOG_STRING_CHARS)
}

fn looks_like_inline_data_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("data:") && lower.contains(";base64,")
}

fn truncate_chars_with_hash(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let prefix = value.chars().take(max_chars).collect::<String>();
    format!(
        "{}\n[CodeSeeX omitted {} chars; hash={}]",
        prefix,
        value.chars().count().saturating_sub(max_chars),
        stable_hash_hex(value.as_bytes())
    )
}

fn redact_secret_text(value: &str) -> String {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r#"(?i)(authorization\s*[:=]\s*)(bearer\s+)?[A-Za-z0-9._~+/=-]{4,}"#,
            r#"(?i)((api[_-]?key|apikey|access[_-]?token|refresh[_-]?token|secret|password|cookie)\s*["']?\s*[:=]\s*["']?)[^"',\s}]{4,}"#,
        ]
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect()
    });
    patterns.iter().fold(value.to_owned(), |text, pattern| {
        pattern.replace_all(&text, "${1}[redacted]").to_string()
    })
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("codeseex-store-{label}-{nanos}"))
    }

    #[tokio::test]
    async fn open_creates_data_dir_layout_without_database() {
        let dir = temp_dir("layout");
        let store = Store::open(&dir.join("codeseex.db"))
            .await
            .expect("open store");
        drop(store);

        for name in ["extension", "lang", "logs", "cache", "runtime", "secrets"] {
            assert!(dir.join(name).is_dir(), "{name} should exist");
        }
        assert!(!dir.join("codeseex.db").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_state_is_not_persisted_across_open() {
        let dir = temp_dir("runtime");
        {
            let store = Store::open(&dir).await.expect("open store");
            store
                .checkpoint_request(
                    "resp_1",
                    None,
                    Some("deepseek-v4-flash"),
                    &json!({ "model": "deepseek-v4-flash", "input": "hello" }),
                )
                .await
                .expect("checkpoint");
            store
                .finish_request(
                    "resp_1",
                    RequestStatus::Completed,
                    Some(&json!({
                        "model": "deepseek-v4-flash",
                        "usage": { "prompt_cache_hit_tokens": 2, "prompt_cache_miss_tokens": 3, "completion_tokens": 4, "total_tokens": 9 }
                    })),
                    None,
                )
                .await
                .expect("finish");
            assert_eq!(
                store
                    .runtime_summary(10)
                    .await
                    .expect("summary")
                    .request_count,
                1
            );
        }

        let reopened = Store::open(&dir).await.expect("reopen");
        assert_eq!(
            reopened
                .runtime_summary(10)
                .await
                .expect("summary")
                .request_count,
            0
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn same_process_handles_share_runtime_state() {
        let dir = temp_dir("shared-runtime");
        {
            let proxy_store = Store::open(&dir).await.expect("open proxy store");
            let manager_store = Store::open(&dir.join("codeseex.db"))
                .await
                .expect("open manager store");

            proxy_store
                .checkpoint_request(
                    "resp_shared",
                    None,
                    Some("deepseek-v4-pro"),
                    &json!({ "model": "deepseek-v4-pro", "input": "hello" }),
                )
                .await
                .expect("checkpoint");
            proxy_store
                .finish_request(
                    "resp_shared",
                    RequestStatus::Completed,
                    Some(&json!({
                        "model": "deepseek-v4-pro",
                        "usage": { "input_tokens": 7, "completion_tokens": 3, "total_tokens": 10 }
                    })),
                    None,
                )
                .await
                .expect("finish");

            let summary = manager_store
                .runtime_summary(10)
                .await
                .expect("manager summary");
            assert_eq!(summary.request_count, 1);
            assert_eq!(
                summary.last_turn.as_ref().map(|turn| turn.id.as_str()),
                Some("resp_shared")
            );
        }

        let reopened = Store::open(&dir).await.expect("reopen store");
        assert_eq!(
            reopened
                .runtime_summary(10)
                .await
                .expect("reopened summary")
                .request_count,
            0
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_keeps_client_tool_handoff_billable_usage() {
        let dir = temp_dir("client-tool-handoff-runtime");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_handoff",
                None,
                Some("deepseek-v4-pro"),
                &json!({ "model": "deepseek-v4-pro", "input": "use a client tool" }),
            )
            .await
            .expect("checkpoint handoff");
        store
            .finish_request(
                "resp_handoff",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-pro",
                    "usage": { "input_tokens": 5, "output_tokens": 1, "total_tokens": 6 }
                })),
                Some(&json!({ "codeseex_lifecycle": "client_tool_handoff" })),
            )
            .await
            .expect("finish handoff");

        let chain = store
            .response_context_chain("resp_handoff", 1)
            .await
            .expect("handoff response remains usable as Codex context");
        assert_eq!(chain.len(), 1);

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.request_count, 0);
        assert_eq!(summary.billable_request_count, 1);
        assert!(summary.last_turn.is_none());
        assert_eq!(
            summary
                .last_billable_request
                .as_ref()
                .map(|turn| turn.id.as_str()),
            Some("resp_handoff")
        );
        assert!(summary.turn_history.is_empty());
        assert_eq!(summary.billable_history.len(), 1);
        assert_eq!(summary.billable_history[0].lifecycle, "client_tool_handoff");
        assert!(!summary.billable_history[0].conversation_turn);
        assert!(summary.billable_history[0].billable);
        assert_eq!(summary.total_cache_miss_input_tokens, 5);
        assert_eq!(summary.total_output_tokens, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn client_handoff_guard_stops_repeated_failures_for_same_turn() {
        let dir = temp_dir("client-handoff-guard-failures");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-a",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "edit the file" }]
            }]
        });

        for index in 0..3 {
            let request_id = format!("resp_handoff_{index}");
            store
                .checkpoint_request(&request_id, None, Some("deepseek-v4-pro"), &input)
                .await
                .expect("checkpoint");
            let stop = store
                .record_client_tool_handoff_calls(
                    &request_id,
                    &input,
                    &[ClientToolHandoffCall {
                        call_id: format!("call_patch_{index}"),
                        name: "apply_patch".to_owned(),
                        arguments: format!("patch {index}"),
                    }],
                    Some(&json!({ "input_tokens": 10, "total_tokens": 12 })),
                )
                .await
                .expect("record handoff");
            assert!(stop.is_none());
            let followup_id = format!("resp_followup_{index}");
            let followup_input = json!({
                "model": "deepseek-v4-pro",
                "prompt_cache_key": "thread-a",
                "input": [{
                    "type": "custom_tool_call_output",
                    "call_id": format!("call_patch_{index}"),
                    "output": "apply_patch verification failed: invalid patch"
                }]
            });
            store
                .checkpoint_request(&followup_id, None, Some("deepseek-v4-pro"), &followup_input)
                .await
                .expect("checkpoint followup");
            let stop = store
                .settle_client_tool_handoff_outputs(&followup_id, &followup_input)
                .await
                .expect("settle output");
            if index < 2 {
                assert!(stop.is_none());
            } else {
                let stop = stop.expect("third consecutive failure should stop");
                assert_eq!(stop.code, "client_tool_handoff_guard_stopped");
                assert_eq!(stop.reason, "consecutive_failures");
                assert_eq!(stop.tool_name.as_deref(), Some("apply_patch"));
                assert_eq!(stop.consecutive_failure_count, 3);
                assert_eq!(stop.cumulative_total_tokens, 36);
            }
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn client_handoff_guard_stops_repeated_signatures() {
        let dir = temp_dir("client-handoff-guard-signatures");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-b",
            "input": "run the same client tool"
        });
        let mut stop = None;
        for index in 0..3 {
            let request_id = format!("resp_repeat_{index}");
            store
                .checkpoint_request(&request_id, None, Some("deepseek-v4-pro"), &input)
                .await
                .expect("checkpoint");
            stop = store
                .record_client_tool_handoff_calls(
                    &request_id,
                    &input,
                    &[ClientToolHandoffCall {
                        call_id: format!("call_repeat_{index}"),
                        name: "shell_command".to_owned(),
                        arguments: "{\"command\":\"date\"}".to_owned(),
                    }],
                    Some(&json!({ "input_tokens": 20, "total_tokens": 22 })),
                )
                .await
                .expect("record handoff");
        }
        let stop = stop.expect("third repeated signature should stop");
        assert_eq!(stop.reason, "repeated_signature");
        assert_eq!(stop.tool_name.as_deref(), Some("shell_command"));
        assert_eq!(stop.repeated_signature_count, 3);
        assert_eq!(stop.cumulative_total_tokens, 66);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn client_handoff_guard_allows_typical_distinct_handoff_depth() {
        let dir = temp_dir("client-handoff-guard-typical-depth");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "deep-agent-turn",
            "input": [{ "type": "message", "role": "user", "content": "inspect and edit several files" }]
        });

        for index in 0..13 {
            let request_id = format!("resp_distinct_{index}");
            let stop = store
                .record_client_tool_handoff_calls(
                    &request_id,
                    &input,
                    &[ClientToolHandoffCall {
                        call_id: format!("call_distinct_{index}"),
                        name: "read_file_range".to_owned(),
                        arguments: format!(
                            "{{\"path\":\"file_{index}.rs\",\"start\":1,\"end\":20}}"
                        ),
                    }],
                    Some(&json!({ "input_tokens": 20, "output_tokens": 1, "total_tokens": 21 })),
                )
                .await
                .expect("record handoff");
            assert!(
                stop.is_none(),
                "distinct handoff {index} should remain allowed"
            );
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn client_handoff_guard_allows_high_distinct_handoff_volume_with_diagnostic() {
        let dir = temp_dir("client-handoff-guard-high-volume");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "runaway-agent-turn",
            "input": [{ "type": "message", "role": "user", "content": "perform a very large client workflow" }]
        });
        for index in 0..=CLIENT_HANDOFF_VOLUME_DIAGNOSTIC_THRESHOLD {
            let request_id = format!("resp_budget_{index}");
            store
                .checkpoint_request(&request_id, None, Some("deepseek-v4-pro"), &input)
                .await
                .expect("checkpoint");
            let stop = store
                .record_client_tool_handoff_calls(
                    &request_id,
                    &input,
                    &[ClientToolHandoffCall {
                        call_id: format!("call_budget_{index}"),
                        name: "read_file_range".to_owned(),
                        arguments: format!(
                            "{{\"path\":\"file_{index}.rs\",\"start\":1,\"end\":20}}"
                        ),
                    }],
                    Some(&json!({ "input_tokens": 10, "output_tokens": 1, "total_tokens": 11 })),
                )
                .await
                .expect("record handoff");
            assert!(
                stop.is_none(),
                "distinct handoff volume should remain allowed at request {index}"
            );
        }
        let inner = store.lock_inner().expect("lock store");
        let request = inner
            .requests
            .get("resp_budget_48")
            .expect("high volume request should be tracked");
        let diagnostic = request
            .diagnostic
            .as_ref()
            .expect("high volume request should keep a diagnostic");
        assert_eq!(
            diagnostic["client_tool_handoff_high_volume"],
            Value::Bool(true)
        );
        assert_eq!(
            diagnostic["client_tool_handoff_volume"]["handoff_requests"],
            json!(CLIENT_HANDOFF_VOLUME_DIAGNOSTIC_THRESHOLD + 1)
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn client_handoff_guard_preflight_stops_after_turn_is_stopped() {
        let dir = temp_dir("client-handoff-guard-preflight");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "guard-thread",
            "input": [{ "type": "message", "role": "user", "content": "run a client command" }]
        });
        for index in 0..3 {
            let request_id = format!("resp_handoff_{index}");
            let stop = store
                .record_client_tool_handoff_calls(
                    &request_id,
                    &input,
                    &[ClientToolHandoffCall {
                        call_id: format!("call_shell_{index}"),
                        name: "shell_command".to_owned(),
                        arguments: "{\"command\":\"date\"}".to_owned(),
                    }],
                    Some(&json!({ "input_tokens": 10, "output_tokens": 1, "total_tokens": 11 })),
                )
                .await
                .expect("record handoff");
            if index < 2 {
                assert!(stop.is_none());
            } else {
                assert_eq!(
                    stop.as_ref().map(|stop| stop.reason.as_str()),
                    Some("repeated_signature")
                );
            }
        }

        let stop = store
            .client_tool_handoff_guard_preflight("resp_followup", &input)
            .await
            .expect("preflight")
            .expect("stopped turn should be remembered");
        assert_eq!(stop.reason, "repeated_signature");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_groups_usage_sessions_by_conversation_chain() {
        let dir = temp_dir("usage-session-chain");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_handoff",
                None,
                Some("deepseek-v4-flash"),
                &json!({
                    "model": "deepseek-v4-flash",
                    "input": "use tool",
                    "reasoning": { "effort": "high" }
                }),
            )
            .await
            .expect("checkpoint handoff");
        store
            .finish_request(
                "resp_handoff",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 10,
                        "cache_miss_input_tokens": 90,
                        "output_tokens": 5,
                        "total_tokens": 105
                    }
                })),
                Some(&json!({ "codeseex_lifecycle": "client_tool_handoff" })),
            )
            .await
            .expect("finish handoff");
        store
            .checkpoint_request(
                "resp_final",
                Some("resp_handoff"),
                Some("deepseek-v4-flash"),
                &json!({
                    "model": "deepseek-v4-flash",
                    "input": "请查询今日天气写入 txt",
                    "reasoning": { "effort": "high" }
                }),
            )
            .await
            .expect("checkpoint final");
        store
            .record_event(
                "info",
                "tool_call",
                "CodeSeeX tool requested.",
                Some(&json!({
                    "id": "resp_final",
                    "call_id": "call_search_1",
                    "name": "web_search",
                    "iteration": 1
                })),
            )
            .await
            .expect("record tool call");
        store
            .record_event(
                "info",
                "tool_result",
                "CodeSeeX tool result returned.",
                Some(&json!({
                    "id": "resp_final",
                    "call_id": "call_search_1",
                    "name": "web_search",
                    "iteration": 1,
                    "ok": true,
                    "summary": "web_search search ok=true candidates=8"
                })),
            )
            .await
            .expect("record tool result");
        store
            .record_event(
                "info",
                "upstream_call_usage_breakdown",
                "CodeSeeX upstream call usage breakdown.",
                Some(&json!({
                    "id": "resp_final",
                    "phase": "streaming_iteration",
                    "iteration": 1,
                    "duration_ms": 2500,
                    "final_handoff": false,
                    "usage": {
                        "cached_input_tokens": 25,
                        "cache_miss_input_tokens": 5,
                        "output_tokens": 2,
                        "total_tokens": 32
                    }
                })),
            )
            .await
            .expect("record usage breakdown");
        store
            .record_event(
                "info",
                "tool_result",
                "CodeSeeX tool result returned.",
                Some(&json!({
                    "id": "resp_final",
                    "call_id": "call_search_2",
                    "name": "web_search",
                    "iteration": 2,
                    "ok": true,
                    "summary": "opened weather page"
                })),
            )
            .await
            .expect("record second tool result");
        store
            .record_event(
                "info",
                "upstream_call_usage_breakdown",
                "CodeSeeX upstream call usage breakdown.",
                Some(&json!({
                    "id": "resp_final",
                    "phase": "streaming_iteration",
                    "iteration": 2,
                    "duration_ms": 7500,
                    "final_handoff": false,
                    "usage": {
                        "cached_input_tokens": 75,
                        "cache_miss_input_tokens": 15,
                        "output_tokens": 10,
                        "total_tokens": 100
                    }
                })),
            )
            .await
            .expect("record final usage breakdown");
        store
            .finish_request(
                "resp_final",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 100,
                        "cache_miss_input_tokens": 20,
                        "output_tokens": 12,
                        "total_tokens": 132
                    }
                })),
                None,
            )
            .await
            .expect("finish final");

        let summary = store.runtime_summary(10).await.expect("summary");

        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.billable_request_count, 2);
        assert_eq!(summary.usage_sessions.len(), 1);
        let session = &summary.usage_sessions[0];
        assert_eq!(session.id, "resp_final");
        assert_eq!(session.title, "请查询今日天气写入 txt");
        assert_eq!(session.title_source, "user_summary");
        assert_eq!(session.rows.len(), 2);
        assert_eq!(session.rows[0].id, "resp_handoff");
        assert_eq!(session.rows[0].kind, "intermediate_reply");
        assert_eq!(session.rows[0].reasoning_effort, "high");
        assert_eq!(session.rows[1].id, "resp_final");
        assert_eq!(session.rows[1].kind, "final_reply");
        assert!(session.segments.len() >= 5);
        assert!(session
            .segments
            .iter()
            .any(|segment| segment.kind == "tool_result"
                && segment.tool_name.as_deref() == Some("web_search")
                && segment.iteration == Some(1)));
        assert!(session
            .segments
            .iter()
            .any(|segment| segment.kind == "model_iteration"
                && segment.cache_miss_input_tokens == 5
                && segment.output_tokens == 2
                && segment.reasoning_effort == "high"
                && segment.request_ms == 2500));
        assert_eq!(
            session.segments.last().map(|segment| segment.kind.as_str()),
            Some("final_reply")
        );
        let final_segments = session
            .segments
            .iter()
            .filter(|segment| segment.kind == "final_reply")
            .collect::<Vec<_>>();
        assert_eq!(final_segments.len(), 1);
        assert_eq!(final_segments[0].rows.len(), 1);
        assert_eq!(final_segments[0].rows[0].id, "resp_final");
        assert_eq!(final_segments[0].total_tokens, 100);
        assert_eq!(final_segments[0].request_ms, 7500);
        assert!(!session
            .segments
            .iter()
            .any(|segment| segment.id == "resp_final"));
        assert_eq!(session.cached_input_tokens, 110);
        assert_eq!(session.cache_miss_input_tokens, 110);
        assert_eq!(session.output_tokens, 17);
        assert_eq!(session.total_tokens, 237);
        assert!(
            !session.rows.iter().any(|row| row.label.contains("缓存检查")
                || row.label.contains("写入用量")
                || row.label == "合计")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_groups_orphan_handoffs_into_nearest_user_turn() {
        let dir = temp_dir("usage-session-orphan-handoffs");
        let store = Store::open(&dir).await.expect("open store");
        for index in 0..2 {
            let id = format!("resp_orphan_handoff_{index}");
            store
                .checkpoint_request(
                    &id,
                    None,
                    Some("deepseek-v4-pro"),
                    &json!({
                        "model": "deepseek-v4-pro",
                        "prompt_cache_key": "thread-usage",
                        "input": [{
                            "type": "message",
                            "role": "user",
                            "content": [{ "type": "input_text", "text": "请检查工具暴露链路，只分析不改代码" }]
                        }],
                        "reasoning": { "effort": "high" }
                    }),
                )
                .await
                .expect("checkpoint handoff");
            store
                .finish_request(
                    &id,
                    RequestStatus::Completed,
                    Some(&json!({
                        "model": "deepseek-v4-pro",
                        "usage": {
                            "cached_input_tokens": 100 * (index + 1),
                            "cache_miss_input_tokens": 10,
                            "output_tokens": 2,
                            "total_tokens": 112 * (index + 1)
                        }
                    })),
                    Some(&json!({ "codeseex_lifecycle": "client_tool_handoff" })),
                )
                .await
                .expect("finish handoff");
        }
        store
            .checkpoint_request(
                "resp_final_orphan_group",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "thread-usage",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "请检查工具暴露链路，只分析不改代码" }]
                    }],
                    "reasoning": { "effort": "high" }
                }),
            )
            .await
            .expect("checkpoint final");
        store
            .finish_request(
                "resp_final_orphan_group",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-pro",
                    "usage": {
                        "cached_input_tokens": 500,
                        "cache_miss_input_tokens": 20,
                        "output_tokens": 6,
                        "total_tokens": 526
                    }
                })),
                None,
            )
            .await
            .expect("finish final");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.request_count, 1);
        assert_eq!(summary.billable_request_count, 3);
        assert_eq!(summary.usage_sessions.len(), 1);
        let session = &summary.usage_sessions[0];
        assert_eq!(session.id, "resp_final_orphan_group");
        assert_eq!(session.rows.len(), 3);
        assert_eq!(session.rows[0].kind, "intermediate_reply");
        assert_eq!(session.rows[1].kind, "intermediate_reply");
        assert_eq!(session.rows[2].kind, "final_reply");
        assert_eq!(session.cache_miss_input_tokens, 40);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_groups_non_final_handoff_guard_chain_into_one_session() {
        let dir = temp_dir("usage-session-handoff-guard-chain");
        let store = Store::open(&dir).await.expect("open store");
        for index in 0..2 {
            let id = format!("resp_handoff_guard_chain_{index}");
            store
                .checkpoint_request(
                    &id,
                    None,
                    Some("deepseek-v4-pro"),
                    &json!({
                        "model": "deepseek-v4-pro",
                        "prompt_cache_key": "guard-session",
                        "input": [{
                            "type": "message",
                            "role": "user",
                            "content": [{ "type": "input_text", "text": "update a file with apply_patch" }]
                        }]
                    }),
                )
                .await
                .expect("checkpoint handoff");
            store
                .finish_request(
                    &id,
                    RequestStatus::Completed,
                    Some(&json!({
                        "model": "deepseek-v4-pro",
                        "usage": {
                            "cached_input_tokens": 100,
                            "cache_miss_input_tokens": 10,
                            "output_tokens": 2,
                            "total_tokens": 112
                        }
                    })),
                    Some(&json!({ "codeseex_lifecycle": "client_tool_handoff" })),
                )
                .await
                .expect("finish handoff");
        }
        store
            .checkpoint_request(
                "resp_guard_stopped",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "guard-session",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "update a file with apply_patch" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint guard stop");
        store
            .finish_request(
                "resp_guard_stopped",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-pro",
                    "status": "completed",
                    "usage": {
                        "cached_input_tokens": 120,
                        "cache_miss_input_tokens": 8,
                        "output_tokens": 1,
                        "total_tokens": 129
                    }
                })),
                Some(&json!({
                    "codeseex_lifecycle": "failed_billable",
                    "client_tool_handoff_guard_stopped": true
                })),
            )
            .await
            .expect("finish guard stop");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.request_count, 0);
        assert_eq!(summary.billable_request_count, 3);
        assert_eq!(summary.usage_sessions.len(), 1);
        let session = &summary.usage_sessions[0];
        assert!(!session.conversation_turn);
        assert_eq!(session.rows.len(), 3);
        assert_eq!(session.rows[0].kind, "intermediate_reply");
        assert_eq!(session.rows[1].kind, "intermediate_reply");
        assert_eq!(session.rows[2].kind, "failed_reply");
        assert_eq!(session.cache_miss_input_tokens, 28);
        assert_eq!(session.total_tokens, 353);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_merges_active_handoff_into_running_usage_session() {
        let dir = temp_dir("usage-session-active-running-handoff");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_active_anchor",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "active-thread",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "inspect active usage grouping" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint active anchor");
        store
            .checkpoint_request(
                "resp_active_handoff",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "active-thread",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "inspect active usage grouping" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint active");
        store
            .update_request_diagnostic(
                "resp_active_handoff",
                &json!({ "codeseex_lifecycle": "client_tool_handoff" }),
            )
            .await
            .expect("active diagnostic");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions.len(), 1);
        let session = &summary.usage_sessions[0];
        assert_eq!(session.id, "resp_active_anchor");
        assert_eq!(session.status, "running");
        assert!(session
            .segments
            .iter()
            .any(|segment| segment.kind == "in_progress_reply"
                && segment.id == "resp_active_handoff"));
        assert!(!summary
            .usage_sessions
            .iter()
            .any(|session| session.title == "usage_record"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_does_not_merge_new_active_turn_into_completed_prompt_cache_session() {
        let dir = temp_dir("usage-session-active-new-turn");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_completed_turn",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "same-codex-thread",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "first user request" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint completed");
        store
            .finish_request(
                "resp_completed_turn",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-pro",
                    "usage": {
                        "cached_input_tokens": 10,
                        "cache_miss_input_tokens": 5,
                        "output_tokens": 2,
                        "total_tokens": 17
                    }
                })),
                None,
            )
            .await
            .expect("finish completed");
        store
            .checkpoint_request(
                "resp_new_active_turn",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "same-codex-thread",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "second user request" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint active");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions.len(), 2);
        let completed = summary
            .usage_sessions
            .iter()
            .find(|session| session.id == "resp_completed_turn")
            .expect("completed session");
        let active = summary
            .usage_sessions
            .iter()
            .find(|session| session.id == "resp_new_active_turn")
            .expect("active session");
        assert_eq!(completed.status, "completed");
        assert_eq!(active.status, "running");
        assert_eq!(completed.title, "first user request");
        assert_eq!(active.title, "second user request");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_failed_billable_without_lifecycle_does_not_emit_usage_record_title() {
        let dir = temp_dir("usage-session-failed-title");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_failed_without_lifecycle",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": "request that fails after billing" }]
                    }]
                }),
            )
            .await
            .expect("checkpoint failed");
        store
            .finish_request(
                "resp_failed_without_lifecycle",
                RequestStatus::Failed,
                Some(&json!({
                    "model": "deepseek-v4-pro",
                    "status": "failed",
                    "usage": {
                        "cached_input_tokens": 5,
                        "cache_miss_input_tokens": 7,
                        "output_tokens": 1,
                        "total_tokens": 13
                    }
                })),
                None,
            )
            .await
            .expect("finish failed");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions.len(), 1);
        assert_eq!(summary.usage_sessions[0].title, "failed_billable");
        assert_eq!(summary.usage_sessions[0].status, "failed");
        assert!(!summary
            .usage_sessions
            .iter()
            .any(|session| session.title == "usage_record"));

        let _ = std::fs::remove_dir_all(dir);
    }
    #[tokio::test]
    async fn runtime_summary_keeps_short_title_for_codex_full_context_without_prompt_body() {
        let dir = temp_dir("usage-session-full-context-title");
        let store = Store::open(&dir).await.expect("open store");
        let mut input_items = Vec::new();
        input_items.push(json!({
            "type": "message",
            "role": "system",
            "content": [{ "type": "input_text", "text": "instructions" }]
        }));
        for index in 0..82 {
            input_items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": format!("historical item {index}") }]
            }));
        }
        input_items.push(json!({
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": "请严格检查 usage 会话卡片是否紧凑" }]
        }));
        let input = json!({
            "model": "deepseek-v4-flash",
            "instructions": "system",
            "prompt_cache_key": "thread-full-context-title",
            "tools": [],
            "input": input_items
        });
        store
            .checkpoint_request("resp_full", None, Some("deepseek-v4-flash"), &input)
            .await
            .expect("checkpoint full context");
        store
            .finish_request(
                "resp_full",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 1,
                        "cache_miss_input_tokens": 2,
                        "output_tokens": 3,
                        "total_tokens": 6
                    }
                })),
                None,
            )
            .await
            .expect("finish full context");

        let stored = store
            .response_context_chain("resp_full", 1)
            .await
            .expect("stored chain")
            .pop()
            .expect("stored response");
        assert_eq!(
            stored
                .input
                .pointer("/input")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
        assert_eq!(
            stored
                .input
                .pointer("/_codeseex_runtime/latest_user_summary")
                .and_then(Value::as_str),
            Some("请严格检查 usage 会话卡片是否紧凑")
        );

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions.len(), 1);
        assert_eq!(
            summary.usage_sessions[0].title,
            "请严格检查 usage 会话卡片是否紧凑"
        );
        assert_eq!(summary.usage_sessions[0].title_source, "user_summary");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_marks_semantic_titles_separately_from_user_text() {
        let dir = temp_dir("usage-session-title-source");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_user_word",
                None,
                Some("deepseek-v4-flash"),
                &json!({ "model": "deepseek-v4-flash", "input": "conversation" }),
            )
            .await
            .expect("checkpoint user word");
        store
            .finish_request(
                "resp_user_word",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 0,
                        "cache_miss_input_tokens": 1,
                        "output_tokens": 1,
                        "total_tokens": 2
                    }
                })),
                None,
            )
            .await
            .expect("finish user word");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions[0].title, "conversation");
        assert_eq!(summary.usage_sessions[0].title_source, "user_summary");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_title_uses_codex_request_body_not_selected_text_context() {
        let dir = temp_dir("usage-session-codex-selected-title");
        let store = Store::open(&dir).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-flash",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "# Selected text:\n\n## Selection 1\ninternal context\n\n## My request for Codex:\nActual request title"
                }]
            }]
        });
        store
            .checkpoint_request(
                "resp_selected_title",
                None,
                Some("deepseek-v4-flash"),
                &input,
            )
            .await
            .expect("checkpoint selected title");
        store
            .finish_request(
                "resp_selected_title",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 0,
                        "cache_miss_input_tokens": 1,
                        "output_tokens": 1,
                        "total_tokens": 2
                    }
                })),
                None,
            )
            .await
            .expect("finish selected title");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.usage_sessions.len(), 1);
        assert_eq!(summary.usage_sessions[0].title, "Actual request title");
        assert_eq!(summary.usage_sessions[0].title_source, "user_summary");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn usage_tool_segments_use_unique_event_ids_for_same_iteration_tool_results() {
        let dir = temp_dir("usage-session-duplicate-tool-segments");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_duplicate_tools",
                None,
                Some("deepseek-v4-flash"),
                &json!({ "model": "deepseek-v4-flash", "input": "run two searches" }),
            )
            .await
            .expect("checkpoint duplicate tools");
        for summary in [
            "workspace_search ok: 0 matches for missing",
            "workspace_search ok: 6 matches for fn log",
        ] {
            store
                .record_event(
                    "info",
                    "tool_result",
                    "CodeSeeX tool result returned.",
                    Some(&json!({
                        "id": "resp_duplicate_tools",
                        "name": "workspace_search",
                        "iteration": 3,
                        "ok": true,
                        "summary": summary
                    })),
                )
                .await
                .expect("record duplicate tool result");
        }
        store
            .finish_request(
                "resp_duplicate_tools",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "cached_input_tokens": 2,
                        "cache_miss_input_tokens": 3,
                        "output_tokens": 1,
                        "total_tokens": 6
                    }
                })),
                None,
            )
            .await
            .expect("finish duplicate tools");

        let summary = store.runtime_summary(10).await.expect("summary");
        let session = summary
            .usage_sessions
            .iter()
            .find(|session| session.id == "resp_duplicate_tools")
            .expect("duplicate tool session");
        let tool_segments = session
            .segments
            .iter()
            .filter(|segment| segment.kind == "tool_result")
            .collect::<Vec<_>>();
        assert_eq!(tool_segments.len(), 2);
        assert_ne!(tool_segments[0].id, tool_segments[1].id);
        assert!(tool_segments
            .iter()
            .all(|segment| segment.iteration == Some(3)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn usage_page_uses_revision_gate_and_lazy_session_detail() {
        let dir = temp_dir("usage-page-lazy-detail");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_usage_page",
                None,
                Some("deepseek-v4-flash"),
                &json!({ "model": "deepseek-v4-flash", "input": "summarize usage page" }),
            )
            .await
            .expect("checkpoint request");
        store
            .finish_request(
                "resp_usage_page",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "input_tokens": 20,
                        "cached_input_tokens": 6,
                        "output_tokens": 4,
                        "total_tokens": 24
                    }
                })),
                None,
            )
            .await
            .expect("finish request");

        let page = store.usage_page(60, None, None).await.expect("usage page");
        assert!(!page.unchanged);
        assert!(page.usage_revision > 0);
        assert_eq!(page.usage_sessions.len(), 1);
        assert_eq!(page.usage_sessions[0].id, "resp_usage_page");
        assert_eq!(page.usage_sessions[0].row_count, 1);
        assert_eq!(page.usage_sessions[0].segment_count, 1);

        let unchanged = store
            .usage_page(60, None, Some(page.usage_revision))
            .await
            .expect("unchanged usage page");
        assert!(unchanged.unchanged);
        assert!(unchanged.usage_sessions.is_empty());
        assert!(unchanged.billing_buckets.is_empty());

        let detail = store
            .usage_session_detail(&page.usage_sessions[0].id)
            .await
            .expect("usage detail");
        let session = detail.session.expect("session detail");
        assert_eq!(session.rows.len(), 1);
        assert_eq!(session.segments.len(), 1);
        assert!(session
            .technical_details
            .iter()
            .any(|detail| detail.value == "resp_usage_page"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_keeps_service_ephemeral_billable_but_not_a_turn() {
        let dir = temp_dir("service-ephemeral-runtime");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_service",
                None,
                Some("deepseek-v4-flash"),
                &json!({
                    "model": "gpt-5.4",
                    "input": "service title",
                    "reasoning": { "effort": "high" }
                }),
            )
            .await
            .expect("checkpoint service");
        store
            .update_request_diagnostic("resp_service", &json!({ "context": "kept" }))
            .await
            .expect("initial diagnostic");
        store
            .finish_request(
                "resp_service",
                RequestStatus::Completed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "usage": {
                        "input_tokens": 9,
                        "cached_input_tokens": 4,
                        "output_tokens": 2,
                        "total_tokens": 11
                    }
                })),
                Some(&json!({
                    "codeseex_lifecycle": "service_ephemeral",
                    "codeseex_service_kind": "thread_title"
                })),
            )
            .await
            .expect("finish service");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.request_count, 0);
        assert_eq!(summary.billable_request_count, 1);
        assert!(summary.last_turn.is_none());
        assert!(summary.turn_history.is_empty());
        assert_eq!(summary.billable_history.len(), 1);
        assert_eq!(summary.billable_history[0].lifecycle, "service_ephemeral");
        assert!(!summary.billable_history[0].conversation_turn);
        assert!(summary.billable_history[0].billable);
        assert_eq!(summary.total_cached_input_tokens, 4);
        assert_eq!(summary.total_cache_miss_input_tokens, 5);
        assert_eq!(summary.total_output_tokens, 2);
        assert_eq!(summary.usage_sessions.len(), 1);
        assert_eq!(summary.usage_sessions[0].title, "service_request");
        assert_eq!(summary.usage_sessions[0].title_source, "semantic");
        assert_eq!(summary.usage_sessions[0].rows[0].reasoning_effort, "none");
        assert_eq!(
            summary.usage_sessions[0].segments[0].reasoning_effort,
            "none"
        );

        let chain = store
            .response_context_chain("resp_service", 1)
            .await
            .expect("service response remains addressable");
        assert_eq!(chain.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_summary_keeps_failed_billable_usage_but_not_a_turn() {
        let dir = temp_dir("failed-billable-runtime");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_failed_billable",
                None,
                Some("deepseek-v4-flash"),
                &json!({ "model": "deepseek-v4-flash", "input": "tool loop fails after billed upstream calls" }),
            )
            .await
            .expect("checkpoint failed billable");
        store
            .finish_request(
                "resp_failed_billable",
                RequestStatus::Failed,
                Some(&json!({
                    "model": "deepseek-v4-flash",
                    "status": "failed",
                    "usage": {
                        "input_tokens": 11,
                        "cached_input_tokens": 7,
                        "output_tokens": 3,
                        "total_tokens": 14
                    }
                })),
                Some(&json!({
                    "codeseex_lifecycle": "failed_billable"
                })),
            )
            .await
            .expect("finish failed billable");

        let summary = store.runtime_summary(10).await.expect("summary");
        assert_eq!(summary.request_count, 0);
        assert_eq!(summary.failed_request_count, 1);
        assert_eq!(summary.billable_request_count, 1);
        assert!(summary.last_turn.is_none());
        assert!(summary.turn_history.is_empty());
        assert_eq!(summary.billable_history.len(), 1);
        assert_eq!(summary.billable_history[0].lifecycle, "failed_billable");
        assert!(!summary.billable_history[0].conversation_turn);
        assert!(summary.billable_history[0].billable);
        assert_eq!(summary.total_cached_input_tokens, 7);
        assert_eq!(summary.total_cache_miss_input_tokens, 4);
        assert_eq!(summary.total_output_tokens, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn finish_request_does_not_overwrite_interrupted_status() {
        let dir = temp_dir("finish-interrupted");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request("resp_cancelled", None, Some("deepseek-v4-pro"), &json!({}))
            .await
            .expect("checkpoint");
        store
            .interrupt_request_if_in_progress("resp_cancelled", "client cancelled")
            .await
            .expect("interrupt");

        store
            .finish_request(
                "resp_cancelled",
                RequestStatus::Completed,
                Some(&json!({ "status": "completed" })),
                None,
            )
            .await
            .expect("finish ignored");

        assert_eq!(
            store
                .response_status("resp_cancelled")
                .await
                .expect("status"),
            Some(RequestStatus::Interrupted)
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn events_are_written_to_logs_and_redacted() {
        let dir = temp_dir("logs");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "error",
                "request_failed",
                "Request failed.",
                Some(&json!({
                    "status": 400,
                    "error": "Authorization: Bearer secret",
                    "message": "data:image/png;base64,abcdef"
                })),
            )
            .await
            .expect("record event");

        let (events, has_more) = store.recent_visible_events(10, None).await.expect("events");
        assert!(!has_more);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].audience.as_deref(), Some("user"));
        let detail = serde_json::to_string(&events[0].detail).expect("detail json");
        assert!(!detail.contains("Bearer secret"));
        assert!(!detail.contains("abcdef"));
        assert!(detail.contains("redacted"));
        assert!(dir
            .join("logs")
            .read_dir()
            .expect("read logs")
            .next()
            .is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn runtime_event_ledger_is_shared_across_store_handles() {
        let dir = temp_dir("runtime-event-ledger");
        let writer = Store::open(&dir).await.expect("open writer");
        let reader = Store::open(&dir.join("codeseex.db"))
            .await
            .expect("open reader");

        writer
            .record_event(
                "info",
                "request_started",
                "Ledger event",
                Some(&json!({ "id": "resp_ledger", "endpoint": "/v1/responses" })),
            )
            .await
            .expect("record event");

        let (events, has_more) = reader
            .recent_visible_events(10, None)
            .await
            .expect("visible events");
        assert!(!has_more);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "request_started");
        assert_eq!(events[0].message, "Ledger event");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn diagnostic_events_are_not_persisted_by_default() {
        let dir = temp_dir("visible-events");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "mixed_tool_turn_split",
                "Internal tool routing detail.",
                Some(&json!({ "id": "resp_1", "native_tools": ["apply_patch"] })),
            )
            .await
            .expect("record diagnostic event");
        store
            .record_event(
                "info",
                "manager_config_saved",
                "Configuration saved.",
                Some(&json!({ "path": "/tmp/config.toml" })),
            )
            .await
            .expect("record config diagnostic event");
        store
            .record_event(
                "info",
                "request_started",
                "Responses request started.",
                Some(&json!({ "id": "resp_1", "model": "deepseek-v4-pro" })),
            )
            .await
            .expect("record user event");

        let (visible, _) = store
            .recent_visible_events(10, None)
            .await
            .expect("visible events");
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].event_type, "request_started");
        assert_eq!(visible[0].audience.as_deref(), Some("user"));

        let (all, _) = store.recent_events(10, None).await.expect("all events");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].audience.as_deref(), Some("user"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn safe_diagnostic_events_are_persisted_but_not_user_visible() {
        let dir = temp_dir("safe-diagnostic-events");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "context_compile_diagnostic",
                "Context compile diagnostic.",
                Some(&json!({
                    "id": "resp_safe",
                    "request": {
                        "input_items": 120,
                        "request_hash": "req_hash",
                        "input_hash": "input_hash",
                        "unsafe_text": "secret body".repeat(1_000)
                    },
                    "runtime_context_storage": {
                        "current": {
                            "mode": "codex_full_context_not_stored",
                            "original_input_items": 120
                        }
                    },
                    "context": {
                        "message_items": 8,
                        "tool_result_items": 2,
                        "display_only_thinking_items": 1,
                        "tool_output_chars": 524288,
                        "unsafe_prompt": "do not keep me".repeat(1_000)
                    },
                    "top_level_body": "also omitted"
                })),
            )
            .await
            .expect("record safe diagnostic");

        let (visible, _) = store
            .recent_visible_events(10, None)
            .await
            .expect("visible events");
        assert!(visible.is_empty());

        let (all, _) = store.recent_events(10, None).await.expect("all events");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].audience.as_deref(), Some("safe_diagnostic"));
        let detail = all[0].detail.as_ref().expect("detail");
        assert_eq!(detail["id"], "resp_safe");
        assert_eq!(detail["request"]["input_items"], 120);
        assert_eq!(detail["context"]["tool_output_chars"], 524288);
        assert!(detail.get("top_level_body").is_none());
        let detail_text = serde_json::to_string(detail).expect("detail json");
        assert!(!detail_text.contains("secret body"));
        assert!(!detail_text.contains("do not keep me"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn event_views_include_safe_diagnostics_by_default() {
        let dir = temp_dir("event-views-safe-default");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "request_started",
                "Responses request started.",
                Some(&json!({ "id": "resp_view", "model": "deepseek-v4-pro" })),
            )
            .await
            .expect("record user event");
        store
            .record_event(
                "warn",
                "cost_risk_diagnostic",
                "High token risk.",
                Some(&json!({
                    "id": "resp_view",
                    "endpoint": "/v1/responses",
                    "mode": "warn_only",
                    "warnings": ["high_input_items", "codex_full_context"],
                    "input_items": 128,
                    "estimated_text_chars": 240000,
                    "unsafe_prompt": "do not expose"
                })),
            )
            .await
            .expect("record cost diagnostic");
        store
            .record_event(
                "info",
                "context_compile_diagnostic",
                "Context compile diagnostic.",
                Some(&json!({
                    "id": "resp_view",
                    "context": {
                        "input_items": 128,
                        "message_items": 10,
                        "tool_result_items": 2,
                        "display_only_thinking_items": 1,
                        "estimated_chars": 240000,
                        "unsafe_prompt": "do not expose"
                    }
                })),
            )
            .await
            .expect("record context diagnostic");

        let (views, has_more, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                ..EventViewQuery::default()
            })
            .await
            .expect("event views");
        assert!(!has_more);
        assert_eq!(views.len(), 3);
        assert!(views
            .iter()
            .any(|event| event.audience == "safe_diagnostic"));
        assert!(views.iter().any(|event| event.category == "context"));
        let risk = views
            .iter()
            .find(|event| event.event_type == "cost_risk_diagnostic")
            .expect("cost risk view");
        assert_eq!(risk.request_id.as_deref(), Some("resp_view"));
        assert!(risk
            .risk_flags
            .iter()
            .any(|flag| flag == "high_input_items"));
        let detail_text = serde_json::to_string(&risk.safe_detail).expect("detail json");
        assert!(!detail_text.contains("unsafe_prompt"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn event_views_filter_and_page_with_cursor() {
        let dir = temp_dir("event-views-filter-cursor");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "request_started",
                "First request.",
                Some(&json!({ "id": "resp_a", "model": "deepseek-v4-flash" })),
            )
            .await
            .expect("record first");
        store
            .record_event(
                "info",
                "tool_result",
                "Tool result returned.",
                Some(&json!({
                    "id": "resp_b",
                    "name": "web_search",
                    "ok": false,
                    "summary": "web_search search ok=false candidates=0",
                    "web_search": {
                        "source_order": ["bing_html", "duckduckgo_lite"],
                        "source_health": [{
                            "source": "duckduckgo_lite",
                            "reachable": false,
                            "error": "timeout"
                        }]
                    }
                })),
            )
            .await
            .expect("record web tool");
        store
            .record_event(
                "info",
                "request_completed",
                "Second request completed.",
                Some(&json!({
                    "id": "resp_b",
                    "model": "deepseek-v4-flash",
                    "duration_ms": 1200,
                    "input_tokens": 200,
                    "cache_miss_input_tokens": 30,
                    "output_tokens": 10,
                    "total_tokens": 240
                })),
            )
            .await
            .expect("record completed");

        let (web_views, _, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                category: Some("web".to_owned()),
                ..EventViewQuery::default()
            })
            .await
            .expect("web views");
        assert_eq!(web_views.len(), 1);
        assert_eq!(web_views[0].request_id.as_deref(), Some("resp_b"));
        assert!(web_views[0]
            .risk_flags
            .iter()
            .any(|flag| flag == "web_source_unreachable"));

        let (request_views, _, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                request_id: Some("resp_b".to_owned()),
                q: Some("completed".to_owned()),
                ..EventViewQuery::default()
            })
            .await
            .expect("request filtered views");
        assert_eq!(request_views.len(), 1);
        assert_eq!(request_views[0].event_type, "request_completed");

        let (latest, has_more, cursor) = store
            .recent_event_views(EventViewQuery {
                limit: 1,
                ..EventViewQuery::default()
            })
            .await
            .expect("latest view");
        assert!(has_more);
        assert_eq!(latest.len(), 1);
        let cursor = cursor.expect("cursor");
        let (older, _, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                cursor: Some(cursor),
                ..EventViewQuery::default()
            })
            .await
            .expect("older views");
        assert_eq!(older.len(), 2);
        assert!(older
            .iter()
            .all(|event| event.event_type != latest[0].event_type));

        let (all, _, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                ..EventViewQuery::default()
            })
            .await
            .expect("all views");
        let after_cursor = all[0].cursor.clone().expect("after cursor");
        let (newer, _, latest_cursor) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                after: Some(after_cursor),
                ..EventViewQuery::default()
            })
            .await
            .expect("newer views");
        assert_eq!(newer.len(), 2);
        assert_eq!(newer[0].event_type, "tool_result");
        assert_eq!(
            latest_cursor,
            newer.first().and_then(|event| event.cursor.clone())
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn event_views_after_cursor_falls_back_to_log_file() {
        let dir = temp_dir("event-views-after-file");
        let store = Store::open(&dir).await.expect("open store");
        for id in ["resp_a", "resp_b", "resp_c"] {
            store
                .record_event(
                    "info",
                    "request_started",
                    "Request started.",
                    Some(&json!({ "id": id, "model": "deepseek-v4-flash" })),
                )
                .await
                .expect("record event");
        }
        let (all, _, _) = store
            .recent_event_views(EventViewQuery {
                limit: 10,
                ..EventViewQuery::default()
            })
            .await
            .expect("all views");
        let after = all[0].cursor.clone().expect("cursor");
        drop(store);

        let reopened = Store::open(&dir).await.expect("reopen store");
        let (newer, _, _) = reopened
            .recent_event_views(EventViewQuery {
                limit: 10,
                after: Some(after),
                ..EventViewQuery::default()
            })
            .await
            .expect("file newer views");
        assert_eq!(newer.len(), 2);
        assert_eq!(newer[0].request_id.as_deref(), Some("resp_b"));
        assert_eq!(newer[1].request_id.as_deref(), Some("resp_c"));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn web_search_source_probe_keeps_safe_source_health() {
        let dir = temp_dir("web-search-source-probe-log");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "web_search_source_probe",
                "Probe completed.",
                Some(&json!({
                    "ok": true,
                    "stage": "search_source_probe",
                    "proxy_key": "system:http://redacted:redacted@127.0.0.1:7890/",
                    "source_order": ["bing_html", "duckduckgo_lite"],
                    "source_health": [{
                        "source": "bing_html",
                        "reachable": true,
                        "latency_ms": 123,
                        "status": 200,
                        "error": null,
                        "age_ms": 0
                    }],
                    "trigger": "manager_save",
                    "debounce_ms": 5000,
                    "network_proxy_signature": "system:http://redacted:redacted@127.0.0.1:7890/",
                    "unsafe_prompt": "do not keep me"
                })),
            )
            .await
            .expect("record probe event");

        let (events, _) = store.recent_events(10, None).await.expect("events");
        let detail = events[0].detail.as_ref().expect("detail");
        assert_eq!(
            detail["proxy_key"],
            "system:http://redacted:redacted@127.0.0.1:7890/"
        );
        assert_eq!(detail["source_order"][0], "bing_html");
        assert_eq!(detail["source_health"][0]["source"], "bing_html");
        assert_eq!(detail["trigger"], "manager_save");
        assert_eq!(detail["debounce_ms"], 5000);
        assert_eq!(
            detail["network_proxy_signature"],
            "system:http://redacted:redacted@127.0.0.1:7890/"
        );
        assert!(detail.get("unsafe_prompt").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_result_log_detail_is_compact() {
        let dir = temp_dir("compact-tool-result-log");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "tool_result",
                "Tool result returned.",
                Some(&json!({
                    "id": "resp_1",
                    "call_id": "call_should_not_be_written",
                    "name": "workspace_search",
                    "iteration": 2,
                    "ok": true,
                    "summary": "x".repeat(5_000),
                    "result_size": {
                        "result_json_chars": 524288,
                        "result_hash": "abc123",
                        "diagnostic_bytes": 524288,
                        "unsafe_text": "secret output".repeat(100)
                    }
                })),
            )
            .await
            .expect("record event");

        let (events, _) = store.recent_events(10, None).await.expect("events");
        assert_eq!(events.len(), 1);
        let detail = events[0].detail.as_ref().expect("detail");
        assert!(detail.get("call_id").is_none());
        assert_eq!(detail["result_size"]["result_json_chars"], 524288);
        assert_eq!(detail["result_size"]["diagnostic_bytes"], 524288);
        assert!(detail["result_size"].get("unsafe_text").is_none());
        let summary = detail
            .get("summary")
            .and_then(Value::as_str)
            .expect("summary");
        assert!(summary.chars().count() < 520);
        let detail_text = serde_json::to_string(detail).expect("detail json");
        assert!(!detail_text.contains("secret output"));
        let log_file = dir
            .join("logs")
            .read_dir()
            .expect("read logs")
            .next()
            .expect("log file")
            .expect("log entry")
            .path();
        let bytes = std::fs::metadata(log_file).expect("log metadata").len();
        assert!(bytes < 1_000);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn web_search_tool_result_keeps_safe_source_diagnostics() {
        let dir = temp_dir("web-search-tool-result-log");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "info",
                "tool_result",
                "Tool result returned.",
                Some(&json!({
                    "id": "resp_web",
                    "name": "web_search",
                    "iteration": 1,
                    "ok": false,
                    "summary": "web_search search ok=false candidates=0",
                    "web_search": {
                        "source_plan": "on_demand_probe",
                        "source_order": ["bing_html", "duckduckgo_lite"],
                        "source_health": [{
                            "source": "bing_html",
                            "reachable": true,
                            "latency_ms": 120,
                            "status": 200,
                            "error": null,
                            "age_ms": 18,
                            "message": "do not keep source health prose"
                        }],
                        "sources_attempted": ["bing_html"],
                        "source_diagnostics": [{
                            "source": "bing_html",
                            "error": "filtered_low_confidence",
                            "result_count": 2,
                            "usable_result_count": 0,
                            "unsafe_body": "do not keep me"
                        }],
                        "fallback_errors": [{
                            "source": "duckduckgo_lite",
                            "error": "request_failed"
                        }],
                        "unsafe_html": "<html>secret</html>"
                    }
                })),
            )
            .await
            .expect("record event");

        let (events, _) = store.recent_events(10, None).await.expect("events");
        let detail = events[0].detail.as_ref().expect("detail");
        assert_eq!(detail["web_search"]["source_plan"], "on_demand_probe");
        assert_eq!(detail["web_search"]["source_order"][0], "bing_html");
        assert_eq!(
            detail["web_search"]["source_health"][0]["source"],
            "bing_html"
        );
        assert_eq!(detail["web_search"]["source_health"][0]["reachable"], true);
        assert!(detail["web_search"]["source_health"][0]
            .get("message")
            .is_none());
        assert_eq!(detail["web_search"]["sources_attempted"][0], "bing_html");
        assert_eq!(
            detail["web_search"]["source_diagnostics"][0]["error"],
            "filtered_low_confidence"
        );
        assert!(detail["web_search"].get("unsafe_html").is_none());
        assert!(detail["web_search"]["source_diagnostics"][0]
            .get("unsafe_body")
            .is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tool_exposure_diagnostic_keeps_structured_summary_by_default() {
        let dir = temp_dir("tool-exposure-diagnostic-log");
        let store = Store::open(&dir).await.expect("open store");
        store
            .record_event(
                "debug",
                "tool_exposure_diagnostic",
                "CodeSeeX tool exposure diagnostic.",
                Some(&json!({
                    "id": "resp_1",
                    "incoming_tool_items": 1,
                    "discovered_tool_items": 1,
                    "external_callable_tools": { "count": 1, "names": ["tool_search_tool"], "omitted": 0 },
                    "external_tool_budget": {
                        "attempted_declarations": 130,
                        "accepted_declarations": 128,
                        "dropped": { "count_limit": 2, "sample_names": ["tool_search_tool"] }
                    },
                    "final_upstream_tools": { "count": 2, "names": ["tool_search_tool", "spawn_agent"], "omitted": 0 },
                    "codex_request_markers": { "client_metadata": true },
                    "tool_search_bridge": { "injected": false, "reason": "already_present" },
                    "interesting_tools": ["tool_search_tool", "spawn_agent"],
                    "large_unused_field": "x".repeat(5_000)
                })),
            )
            .await
            .expect("record diagnostic");

        let (events, _) = store.recent_events(10, None).await.expect("events");
        assert_eq!(events.len(), 1);
        let detail = events[0].detail.as_ref().expect("detail");
        assert_eq!(detail["id"], "resp_1");
        assert_eq!(detail["tool_search_bridge"]["reason"], "already_present");
        assert_eq!(detail["discovered_tool_items"], 1);
        assert_eq!(detail["external_tool_budget"]["dropped"]["count_limit"], 2);
        assert_eq!(
            detail["external_tool_budget"]["dropped"]["sample_names"][0],
            "tool_search_tool"
        );
        assert!(detail.get("large_unused_field").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn codex_full_context_input_is_not_kept_in_runtime() {
        let dir = temp_dir("full-context");
        let store = Store::open(&dir).await.expect("open store");
        let input_items = (0..100)
            .map(|index| {
                if index == 0 {
                    json!({
                        "type": "function_call",
                        "call_id": "call_secret",
                        "name": "read_file_range",
                        "arguments": "{\"path\":\"secret.txt\"}"
                    })
                } else if index == 1 {
                    json!({
                        "type": "function_call_output",
                        "call_id": "call_secret",
                        "output": "secret tool output"
                    })
                } else {
                    json!({ "role": "user", "content": format!("secret item {index}") })
                }
            })
            .collect::<Vec<_>>();
        store
            .checkpoint_request(
                "resp_full",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "prompt_cache_key": "thread",
                    "instructions": "system",
                    "tools": [],
                    "input": input_items
                }),
            )
            .await
            .expect("checkpoint");
        store
            .finish_request(
                "resp_full",
                RequestStatus::Completed,
                Some(&json!({ "model": "deepseek-v4-pro" })),
                None,
            )
            .await
            .expect("finish");

        let chain = store
            .response_context_chain("resp_full", 1)
            .await
            .expect("chain");
        assert_eq!(chain[0].input["input"].as_array().unwrap().len(), 0);
        assert_eq!(
            chain[0].input["_codeseex_runtime"]["mode"],
            "codex_full_context_not_stored"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn codex_full_context_without_prompt_cache_key_is_not_kept_in_runtime() {
        let dir = temp_dir("full-context-no-cache-key");
        let store = Store::open(&dir).await.expect("open store");
        let input_items = (0..100)
            .map(|index| {
                if index == 0 {
                    json!({
                        "type": "function_call",
                        "call_id": "call_secret",
                        "name": "read_file_range",
                        "arguments": "{\"path\":\"secret.txt\"}"
                    })
                } else if index == 1 {
                    json!({
                        "type": "function_call_output",
                        "call_id": "call_secret",
                        "output": "secret tool output"
                    })
                } else {
                    json!({ "role": "user", "content": format!("secret item {index}") })
                }
            })
            .collect::<Vec<_>>();
        store
            .checkpoint_request(
                "resp_full_no_cache_key",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "instructions": "system",
                    "tools": [],
                    "input": input_items
                }),
            )
            .await
            .expect("checkpoint");

        let chain = store
            .response_context_chain("resp_full_no_cache_key", 1)
            .await
            .expect_err("in-progress previous should fail");
        assert!(chain.to_string().contains("not completed"));
        store
            .finish_request(
                "resp_full_no_cache_key",
                RequestStatus::Completed,
                Some(&json!({ "model": "deepseek-v4-pro" })),
                None,
            )
            .await
            .expect("finish");
        let chain = store
            .response_context_chain("resp_full_no_cache_key", 1)
            .await
            .expect("chain");
        assert_eq!(chain[0].input["input"].as_array().unwrap().len(), 0);
        assert_eq!(
            chain[0].input["_codeseex_runtime"]["mode"],
            "codex_full_context_not_stored"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn input_image_data_urls_are_redacted_in_runtime_context() {
        let dir = temp_dir("input-image-redaction");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_image",
                None,
                Some("deepseek-v4-pro"),
                &json!({
                    "model": "deepseek-v4-pro",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [
                            { "type": "input_text", "text": "Describe this image." },
                            { "type": "input_image", "image_url": "data:image/png;base64,AAAASECRETBBBB" }
                        ]
                    }]
                }),
            )
            .await
            .expect("checkpoint");
        store
            .finish_request(
                "resp_image",
                RequestStatus::Completed,
                Some(&json!({ "model": "deepseek-v4-pro" })),
                None,
            )
            .await
            .expect("finish");

        let chain = store
            .response_context_chain("resp_image", 1)
            .await
            .expect("chain");
        let serialized = serde_json::to_string(&chain[0].input).expect("runtime input json");
        assert!(!serialized.contains("AAAASECRETBBBB"));
        assert!(!serialized.contains("data:image/png;base64"));
        assert!(serialized.contains("redacted inline data url"));
        assert!(serialized.contains("Describe this image."));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn duplicate_request_ids_are_rejected() {
        let dir = temp_dir("duplicate-id");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_dup",
                None,
                Some("deepseek-v4-pro"),
                &json!({ "input": [] }),
            )
            .await
            .expect("first checkpoint");
        let error = store
            .checkpoint_request(
                "resp_dup",
                None,
                Some("deepseek-v4-pro"),
                &json!({ "input": [] }),
            )
            .await
            .expect_err("duplicate id should fail");
        assert!(error.to_string().contains("already exists"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn previous_context_requires_completed_status() {
        let dir = temp_dir("previous-status");
        let store = Store::open(&dir).await.expect("open store");
        store
            .checkpoint_request(
                "resp_parent",
                None,
                Some("deepseek-v4-pro"),
                &json!({ "input": [] }),
            )
            .await
            .expect("checkpoint");
        let error = store
            .response_context_chain("resp_parent", 1)
            .await
            .expect_err("in-progress previous should fail");
        assert!(error.to_string().contains("not completed"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn missing_previous_response_id_is_explicit() {
        let dir = temp_dir("missing-previous");
        let store = Store::open(&dir).await.expect("open store");
        let error = store
            .response_context_chain("resp_missing", 1)
            .await
            .expect_err("missing previous should fail");
        assert!(error.to_string().contains("send full context"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
