use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use codeseex_core::context::{content_to_text, request_looks_like_codex_full_context};
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
const IN_PROGRESS_TTL_SECONDS: i64 = 6 * 60 * 60;
const LOG_TAIL_CHUNK_BYTES: u64 = 64 * 1024;
#[cfg(any(debug_assertions, test))]
const USAGE_TEMPLATE_SEED_PREFIX: &str = "codeseex_usage_template_";

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
    pub active_requests: u64,
    pub request_count: u64,
    pub billable_request_count: u64,
    pub failed_request_count: u64,
    pub last_request_at: Option<String>,
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

    pub async fn seed_usage_template_preview(&self) -> Result<usize> {
        #[cfg(not(any(debug_assertions, test)))]
        {
            bail!("usage template preview seed is only available in debug builds");
        }
        #[cfg(any(debug_assertions, test))]
        seed_usage_template_preview_inner(self)
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
        self.push_runtime_event(event.clone())?;
        append_log_event(&self.logs_dir, &event).await
    }

    fn push_runtime_event(&self, event: EventRecord) -> Result<()> {
        let mut inner = self.lock_inner()?;
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

#[cfg(any(debug_assertions, test))]
fn seed_usage_template_preview_inner(store: &Store) -> Result<usize> {
    let now = Utc::now();
    let mut inner = store.lock_inner()?;
    remove_usage_template_preview_inner(&mut inner);

    let mut inserted = 0_usize;
    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_weather_handoff",
            previous: None,
            model: "deepseek-v4-pro",
            input: "查询今日中山天气并写入 txt，必要时使用网络搜索。",
            effort: "high",
            lifecycle: "client_tool_handoff",
            created_offset_ms: 0,
            duration_ms: 3000,
            cached: 97,
            miss: 13_337,
            output: 180,
            events: vec![SeedEvent::upstream(
                "streaming_iteration",
                0,
                97,
                13_337,
                180,
            )],
        },
        now - Duration::minutes(36),
    );
    inserted += 1;
    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_weather_final",
            previous: Some("codeseex_usage_template_weather_handoff"),
            model: "deepseek-v4-pro",
            input: "查询今日中山天气并写入 txt，必要时使用网络搜索。",
            effort: "high",
            lifecycle: "final_turn",
            created_offset_ms: 4300,
            duration_ms: 5900,
            cached: 17_721,
            miss: 2_812,
            output: 476,
            events: vec![
                SeedEvent::tool(
                    "web_search",
                    1,
                    "candidates=4, sources=[bing_html, duckduckgo, brave]",
                ),
                SeedEvent::upstream("streaming_iteration", 1, 14_852, 1_720, 144),
                SeedEvent::tool("web_search", 2, "opened=3, failure_count=0"),
            ],
        },
        now - Duration::minutes(36),
    );
    inserted += 1;

    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_ds_flash",
            previous: None,
            model: "deepseek-v4-flash",
            input: "DS Flash 单条对话请求",
            effort: "low",
            lifecycle: "final_turn",
            created_offset_ms: 0,
            duration_ms: 1200,
            cached: 0,
            miss: 2_167,
            output: 16,
            events: Vec::new(),
        },
        now - Duration::hours(2),
    );
    inserted += 1;

    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_service",
            previous: None,
            model: "deepseek-v4-flash",
            input: "ambient_suggestions service request",
            effort: "none",
            lifecycle: "service_ephemeral",
            created_offset_ms: 0,
            duration_ms: 4400,
            cached: 5_760,
            miss: 3_465,
            output: 335,
            events: Vec::new(),
        },
        now - Duration::days(1) + Duration::hours(1),
    );
    inserted += 1;

    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_analysis_handoff",
            previous: None,
            model: "deepseek-v4-flash",
            input: "检查仓库工具暴露链路，只分析不改代码。",
            effort: "low",
            lifecycle: "client_tool_handoff",
            created_offset_ms: 0,
            duration_ms: 2200,
            cached: 6_890,
            miss: 1_230,
            output: 204,
            events: vec![SeedEvent::upstream(
                "streaming_iteration",
                0,
                6_890,
                1_230,
                204,
            )],
        },
        now - Duration::days(1) - Duration::hours(1),
    );
    inserted += 1;
    insert_seed_request(
        &mut inner,
        SeedRequest {
            id: "codeseex_usage_template_analysis_final",
            previous: Some("codeseex_usage_template_analysis_handoff"),
            model: "deepseek-v4-flash",
            input: "检查仓库工具暴露链路，只分析不改代码。",
            effort: "low",
            lifecycle: "final_turn",
            created_offset_ms: 2400,
            duration_ms: 5600,
            cached: 10_520,
            miss: 1_048,
            output: 488,
            events: Vec::new(),
        },
        now - Duration::days(1) - Duration::hours(1),
    );
    inserted += 1;

    push_seed_event(
        &mut inner,
        now,
        "usage_template_preview_seeded",
        json!({
            "id": "codeseex_usage_template_seed",
            "prefix": USAGE_TEMPLATE_SEED_PREFIX,
            "records": inserted
        }),
    );
    Ok(inserted)
}

#[cfg(any(debug_assertions, test))]
fn remove_usage_template_preview_inner(inner: &mut StoreInner) {
    inner
        .requests
        .retain(|id, _| !id.starts_with(USAGE_TEMPLATE_SEED_PREFIX));
    inner
        .request_order
        .retain(|id| !id.starts_with(USAGE_TEMPLATE_SEED_PREFIX));
    inner.events.retain(|event| {
        let Some(id) = event_detail_id(event) else {
            return event.event_type != "usage_template_preview_seeded";
        };
        !id.starts_with(USAGE_TEMPLATE_SEED_PREFIX)
    });
}

#[derive(Debug)]
#[cfg(any(debug_assertions, test))]
struct SeedRequest {
    id: &'static str,
    previous: Option<&'static str>,
    model: &'static str,
    input: &'static str,
    effort: &'static str,
    lifecycle: &'static str,
    created_offset_ms: i64,
    duration_ms: i64,
    cached: u64,
    miss: u64,
    output: u64,
    events: Vec<SeedEvent>,
}

#[derive(Debug)]
#[cfg(any(debug_assertions, test))]
enum SeedEvent {
    Upstream {
        phase: &'static str,
        iteration: u32,
        cached: u64,
        miss: u64,
        output: u64,
    },
    Tool {
        name: &'static str,
        iteration: u32,
        summary: &'static str,
    },
}

#[cfg(any(debug_assertions, test))]
impl SeedEvent {
    fn upstream(phase: &'static str, iteration: u32, cached: u64, miss: u64, output: u64) -> Self {
        Self::Upstream {
            phase,
            iteration,
            cached,
            miss,
            output,
        }
    }

    fn tool(name: &'static str, iteration: u32, summary: &'static str) -> Self {
        Self::Tool {
            name,
            iteration,
            summary,
        }
    }
}

#[cfg(any(debug_assertions, test))]
fn insert_seed_request(inner: &mut StoreInner, seed: SeedRequest, base: DateTime<Utc>) {
    let created_at = base + Duration::milliseconds(seed.created_offset_ms);
    let updated_at = created_at + Duration::milliseconds(seed.duration_ms.max(0));
    let input = json!({
        "model": seed.model,
        "input": seed.input,
        "reasoning": { "effort": seed.effort }
    });
    let total = seed
        .cached
        .saturating_add(seed.miss)
        .saturating_add(seed.output);
    let response = json!({
        "id": seed.id,
        "model": seed.model,
        "usage": {
            "input_tokens": seed.cached.saturating_add(seed.miss),
            "cached_input_tokens": seed.cached,
            "cache_miss_input_tokens": seed.miss,
            "output_tokens": seed.output,
            "total_tokens": total
        }
    });
    inner.requests.insert(
        seed.id.to_owned(),
        StoredRequest {
            id: seed.id.to_owned(),
            previous_response_id: seed.previous.map(str::to_owned),
            status: RequestStatus::Completed,
            model: Some(seed.model.to_owned()),
            input: request_input_for_runtime(seed.previous, &input),
            response: memory_json_value(&response),
            turn_messages: vec![json!({ "role": "user", "content": seed.input })],
            tool_facts: Vec::new(),
            diagnostic: Some(json!({ "codeseex_lifecycle": seed.lifecycle })),
            created_at,
            updated_at,
        },
    );
    push_request_order(&mut inner.request_order, seed.id);
    for (index, event) in seed.events.into_iter().enumerate() {
        let ts = created_at + Duration::milliseconds(600 + i64::try_from(index).unwrap_or(0) * 900);
        match event {
            SeedEvent::Upstream {
                phase,
                iteration,
                cached,
                miss,
                output,
            } => {
                push_seed_event(
                    inner,
                    ts,
                    "upstream_call_usage_breakdown",
                    json!({
                        "id": seed.id,
                        "phase": phase,
                        "iteration": iteration,
                        "final_handoff": false,
                        "usage": {
                            "input_tokens": cached.saturating_add(miss),
                            "cached_input_tokens": cached,
                            "cache_miss_input_tokens": miss,
                            "output_tokens": output,
                            "total_tokens": cached.saturating_add(miss).saturating_add(output)
                        }
                    }),
                );
            }
            SeedEvent::Tool {
                name,
                iteration,
                summary,
            } => {
                push_seed_event(
                    inner,
                    ts,
                    "tool_result",
                    json!({
                        "id": seed.id,
                        "call_id": format!("{}_call_{}", seed.id, iteration),
                        "name": name,
                        "iteration": iteration,
                        "ok": true,
                        "summary": summary
                    }),
                );
            }
        }
    }
    prune_runtime_requests(inner);
}

#[cfg(any(debug_assertions, test))]
fn push_seed_event(inner: &mut StoreInner, ts: DateTime<Utc>, event_type: &str, detail: Value) {
    inner.next_event_id = inner.next_event_id.saturating_add(1);
    inner.events.push_back(EventRecord {
        id: inner.next_event_id,
        level: "info".to_owned(),
        event_type: event_type.to_owned(),
        audience: Some(event_audience_for_type(event_type).to_owned()),
        message: "CodeSeeX usage template preview event.".to_owned(),
        detail: compact_event_detail(event_type, &detail),
        ts: ts.to_rfc3339(),
    });
    while inner.events.len() > MAX_RUNTIME_EVENTS {
        inner.events.pop_front();
    }
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

fn is_safe_diagnostic_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "client_tool_handoff_diagnostic"
            | "context_compile_diagnostic"
            | "retry_cache_diagnostic"
            | "tool_exposure_diagnostic"
            | "usage_template_preview_seeded"
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
                &["id", "phase", "iteration", "final_handoff"],
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
                &[(
                    "result_size",
                    &[
                        "result_json_chars",
                        "result_hash",
                        "ok",
                        "diagnostic_bytes",
                        "diagnostic_opened_count",
                        "diagnostic_failure_count",
                    ][..],
                )],
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
        active_requests,
        request_count,
        billable_request_count,
        failed_request_count,
        last_request_at,
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

    for final_turn in turn_history {
        let chain = usage_chain_for_final_turn(inner, &final_turn.id);
        let mut rows = Vec::new();
        for id in chain {
            if let Some(turn) = billable_by_id.remove(&id) {
                rows.push(usage_session_row(&turn, turn.id == final_turn.id));
            }
        }
        if rows.is_empty() {
            rows.push(usage_session_row(final_turn, true));
        }
        sessions.push(usage_session_from_rows(
            final_turn,
            inner.requests.get(&final_turn.id),
            rows,
            true,
            inner,
        ));
    }

    for turn in billable_history {
        if let Some(turn) = billable_by_id.remove(&turn.id) {
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
            session_ids.insert(session.id.clone());
            sessions.push(session);
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
    let (title, title_source) = usage_session_title(&anchor, Some(request));
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

    segments.push(usage_segment_from_row(row));
    segments
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
        request_ms: 0,
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
            "{}:tool:{}:{}",
            row.id,
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
            "{}:tool-call:{}:{}",
            row.id,
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
        _ => "usage_record".to_owned(),
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
    let mut text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    text = text.trim().to_owned();
    if text.is_empty() {
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
    request.status == RequestStatus::Completed && request_lifecycle(request) == "final_turn"
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
    request
        .diagnostic
        .as_ref()
        .and_then(|diagnostic| diagnostic.get("codeseex_lifecycle"))
        .and_then(Value::as_str)
        .unwrap_or("final_turn")
        .to_owned()
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
                && segment.reasoning_effort == "high"));
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
        assert!(final_segments[0].total_tokens > 0);
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
    async fn usage_template_preview_seed_creates_segmented_sessions() {
        let dir = temp_dir("usage-template-preview");
        let store = Store::open(&dir).await.expect("open store");

        let records = store
            .seed_usage_template_preview()
            .await
            .expect("seed template preview");
        assert_eq!(records, 6);
        let summary = store.runtime_summary(20).await.expect("summary");

        assert_eq!(summary.billable_history.len(), 6);
        assert_eq!(summary.usage_sessions.len(), 4);
        let weather = summary
            .usage_sessions
            .iter()
            .find(|session| session.id == "codeseex_usage_template_weather_final")
            .expect("weather session");
        assert_eq!(weather.rows.len(), 2);
        assert!(weather.segments.iter().any(|segment| {
            segment.kind == "tool_result" && segment.tool_name.as_deref() == Some("web_search")
        }));
        assert!(weather.segments.iter().any(|segment| {
            segment.kind == "model_iteration" && segment.reasoning_effort == "high"
        }));
        assert_eq!(
            summary
                .usage_sessions
                .iter()
                .find(|session| session.id == "codeseex_usage_template_service")
                .map(|session| session.title.as_str()),
            Some("service_request")
        );

        store
            .seed_usage_template_preview()
            .await
            .expect("seed template preview again");
        let reseeded = store.runtime_summary(20).await.expect("reseeded summary");
        assert_eq!(reseeded.billable_history.len(), 6);
        assert_eq!(reseeded.usage_sessions.len(), 4);
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
