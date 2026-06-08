use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use codeseex_core::context::request_looks_like_codex_full_context;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_RUNTIME_REQUESTS: usize = 2_048;
const MAX_RUNTIME_TURNS: usize = 500;
const MAX_TURN_MESSAGES_PER_REQUEST: usize = 256;
const MAX_TOOL_FACTS_PER_REQUEST: usize = 100;
const MAX_LOG_STRING_CHARS: usize = 1_024;
const MAX_LOG_SUMMARY_CHARS: usize = 360;
const MAX_LOG_ARRAY_ITEMS: usize = 16;
const MAX_MEMORY_STRING_CHARS: usize = 64 * 1024;
const MAX_MEMORY_ARRAY_ITEMS: usize = 256;
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
    next_event_id: i64,
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

#[derive(Debug, Clone, Serialize)]
pub struct RequestTurn {
    pub id: String,
    pub model: String,
    pub requested_model: String,
    pub completed_at: String,
    pub cached_input_tokens: u64,
    pub cache_miss_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub request_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSummary {
    pub active_requests: u64,
    pub request_count: u64,
    pub failed_request_count: u64,
    pub last_request_at: Option<String>,
    pub last_turn: Option<RequestTurn>,
    pub turn_history: Vec<RequestTurn>,
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
        Ok(())
    }

    pub async fn runtime_summary(&self, turn_limit: u32) -> Result<RuntimeSummary> {
        let inner = self.lock_inner()?;
        let mut turns = completed_turns(&inner);
        let limit = usize::try_from(turn_limit.clamp(1, 500)).unwrap_or(120);
        if turns.len() > limit {
            turns.drain(0..turns.len() - limit);
        }
        Ok(runtime_summary_from_inner(&inner, turns))
    }

    pub async fn runtime_overview(&self) -> Result<RuntimeSummary> {
        let inner = self.lock_inner()?;
        let mut turns = completed_turns(&inner);
        if turns.len() > 1 {
            turns.drain(0..turns.len() - 1);
        }
        Ok(runtime_summary_from_inner(&inner, turns))
    }

    pub async fn recent_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        read_log_events(&self.logs_dir, limit, before, false).await
    }

    pub async fn recent_visible_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        read_log_events(&self.logs_dir, limit, before, true).await
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
        request.status = status;
        if let Some(response) = response {
            request.response = memory_json_value(response);
        }
        if let Some(diagnostic) = diagnostic {
            request.diagnostic = Some(memory_json_value(diagnostic));
        }
        request.updated_at = Utc::now();
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
        Ok(true)
    }

    pub async fn update_request_diagnostic(&self, id: &str, diagnostic: &Value) -> Result<()> {
        let mut inner = self.lock_inner()?;
        let Some(request) = inner.requests.get_mut(id) else {
            bail!("request '{id}' was not found while updating diagnostics");
        };
        request.diagnostic = Some(memory_json_value(diagnostic));
        request.updated_at = Utc::now();
        Ok(())
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
        append_log_event(&self.logs_dir, &event).await
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

fn event_is_user_visible(event: &EventRecord) -> bool {
    event.level != "debug" && event_audience(event) == "user"
}

fn event_audience(event: &EventRecord) -> &'static str {
    match event.audience.as_deref() {
        Some("diagnostic") => "diagnostic",
        Some("user") => "user",
        _ => event_audience_for_type(&event.event_type),
    }
}

fn event_audience_for_type(event_type: &str) -> &'static str {
    if is_diagnostic_event_type(event_type.trim()) {
        "diagnostic"
    } else {
        "user"
    }
}

fn is_diagnostic_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "chat_stream_started"
            | "context_diagnostic"
            | "context_response_diagnostic"
            | "log_maintenance_completed"
            | "manager_action"
            | "mixed_tool_turn_split"
            | "models_requested"
            | "process_stderr"
            | "process_stdout"
            | "tool_lifecycle"
            | "tool_exposure_diagnostic"
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
        "request_started" => copy_log_fields(
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
        ),
        "request_completed" => copy_log_fields(
            object,
            &mut output,
            &[
                "id",
                "status",
                "requested_model",
                "model",
                "duration_ms",
                "cost_cny",
            ],
        ),
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
        "tool_call" => copy_log_fields(object, &mut output, &["id", "name", "scope", "iteration"]),
        "tool_result" => {
            copy_log_fields(
                object,
                &mut output,
                &["id", "name", "scope", "iteration", "ok"],
            );
            if let Some(summary) = object.get("summary") {
                output.insert(
                    "summary".to_owned(),
                    compact_log_value(summary, MAX_LOG_SUMMARY_CHARS),
                );
            }
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

fn compact_log_field_value(key: &str, value: &Value) -> Value {
    match key {
        "summary" => compact_log_value(value, MAX_LOG_SUMMARY_CHARS),
        "upstream_error" => compact_upstream_error(value),
        _ => compact_log_value(value, MAX_LOG_STRING_CHARS),
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

fn prune_runtime_requests(inner: &mut StoreInner) {
    mark_stale_in_progress_requests(inner);
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

fn mark_stale_in_progress_requests(inner: &mut StoreInner) {
    let now = Utc::now();
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
    }
}

fn completed_turns(inner: &StoreInner) -> Vec<RequestTurn> {
    let mut turns = inner
        .request_order
        .iter()
        .filter_map(|id| inner.requests.get(id))
        .filter(|request| request.status == RequestStatus::Completed)
        .filter_map(turn_from_request)
        .collect::<Vec<_>>();
    if turns.len() > MAX_RUNTIME_TURNS {
        turns.drain(0..turns.len() - MAX_RUNTIME_TURNS);
    }
    turns
}

fn runtime_summary_from_inner(
    inner: &StoreInner,
    turn_history: Vec<RequestTurn>,
) -> RuntimeSummary {
    let active_requests = inner
        .requests
        .values()
        .filter(|request| request.status == RequestStatus::InProgress)
        .count() as u64;
    let request_count = inner
        .requests
        .values()
        .filter(|request| request.status == RequestStatus::Completed)
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
    let last_request_at = last_turn.as_ref().map(|turn| turn.completed_at.clone());
    let total_cached_input_tokens = turn_history
        .iter()
        .map(|turn| turn.cached_input_tokens)
        .sum();
    let total_cache_miss_input_tokens = turn_history
        .iter()
        .map(|turn| turn.cache_miss_input_tokens)
        .sum();
    let total_output_tokens = turn_history.iter().map(|turn| turn.output_tokens).sum();
    let average_ms = if turn_history.is_empty() {
        0
    } else {
        turn_history.iter().map(|turn| turn.request_ms).sum::<u64>()
            / u64::try_from(turn_history.len()).unwrap_or(1)
    };
    RuntimeSummary {
        active_requests,
        request_count,
        failed_request_count,
        last_request_at,
        last_turn,
        turn_history,
        total_cached_input_tokens,
        total_cache_miss_input_tokens,
        total_output_tokens,
        average_ms,
    }
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
        completed_at: request.updated_at.to_rfc3339(),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: request_ms(request.created_at, request.updated_at),
    })
}

fn usage_value(response: &Value) -> Option<&Value> {
    response
        .get("usage")
        .or_else(|| response.pointer("/response/usage"))
        .or_else(|| response.pointer("/choices/0/usage"))
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
    if let Some(input) = object.get("input") {
        if request_looks_like_codex_full_context(value) {
            let item_count = input.as_array().map(Vec::len).unwrap_or(0);
            stored.insert("input".to_owned(), Value::Array(Vec::new()));
            stored.insert(
                "_codeseex_runtime".to_owned(),
                json!({
                    "mode": "codex_full_context_not_stored",
                    "reason": "Codex owns and resends full conversation context; CodeSeeX keeps no duplicate transcript.",
                    "original_input_items": item_count,
                    "original_input_hash": stable_hash_hex(&serde_json::to_vec(input).unwrap_or_default())
                }),
            );
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
                    "summary": "x".repeat(5_000)
                })),
            )
            .await
            .expect("record event");

        let (events, _) = store.recent_events(10, None).await.expect("events");
        assert_eq!(events.len(), 1);
        let detail = events[0].detail.as_ref().expect("detail");
        assert!(detail.get("call_id").is_none());
        let summary = detail
            .get("summary")
            .and_then(Value::as_str)
            .expect("summary");
        assert!(summary.chars().count() < 520);
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
    async fn codex_full_context_input_is_not_kept_in_runtime() {
        let dir = temp_dir("full-context");
        let store = Store::open(&dir).await.expect("open store");
        let input_items = (0..100)
            .map(|index| json!({ "role": "user", "content": format!("secret item {index}") }))
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
            .map(|index| json!({ "role": "user", "content": format!("secret item {index}") }))
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
