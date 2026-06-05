use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
    Row, SqlitePool,
};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration as StdDuration;

const MAX_STORAGE_STRING_CHARS: usize = 64 * 1024;
const MAX_STORAGE_JSON_BYTES: usize = 512 * 1024;
const COMPACT_STORAGE_STRING_CHARS: usize = 4 * 1024;
const COMPACT_STORAGE_ARRAY_HEAD_ITEMS: usize = 80;
const COMPACT_STORAGE_ARRAY_TAIL_ITEMS: usize = 20;
const COMPACT_STORAGE_OBJECT_KEYS: usize = 128;
const MAX_STORAGE_TOOL_FACTS_PER_REQUEST: usize = 100;
const CODEX_FULL_CONTEXT_INPUT_ITEMS_THRESHOLD: usize = 80;
const CODEX_FULL_CONTEXT_TURN_MESSAGES_THRESHOLD: usize = 80;
const CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS: usize = 32;
const RECENT_TOOL_FACT_RETENTION_HOURS: i64 = 24;
const TOOL_FACT_OMITTED_PREFIX: &str = "[CodeSeeX storage omitted ";
const TOOL_FACT_OMITTED_SUFFIX: &str = " older tool fact(s) after exceeding durable state budget]";
const MAINTENANCE_LARGE_FIELD_BYTES: i64 = 256 * 1024;
#[cfg(not(test))]
const MAINTENANCE_REQUEST_BATCH: i64 = 200;
#[cfg(test)]
const MAINTENANCE_REQUEST_BATCH: i64 = 8;
const MAINTENANCE_REQUEST_MAX_BATCHES: usize = 20;

#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventRecord {
    pub id: i64,
    pub level: String,
    #[serde(rename = "type")]
    pub event_type: String,
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

struct RequestSanitizeReport {
    sanitized_requests: u64,
    batches: u64,
    limit_reached: bool,
}

struct RequestSanitizeBatch {
    fetched: u64,
    changed: u64,
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

impl RequestStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "interrupted" => Self::Interrupted,
            _ => Self::InProgress,
        }
    }
}

fn parse_json_field<T: DeserializeOwned>(label: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).with_context(|| format!("parse {label}"))
}

fn parse_optional_json_field<T: DeserializeOwned>(
    label: &str,
    text: Option<&str>,
) -> Result<Option<T>> {
    text.map(|value| parse_json_field(label, value)).transpose()
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create data directory {}", parent.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(StdDuration::from_secs(5));
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS requests (
              id TEXT PRIMARY KEY,
              previous_response_id TEXT,
              status TEXT NOT NULL,
              model TEXT,
              input_json TEXT NOT NULL,
              response_json TEXT,
              turn_messages_json TEXT,
              tool_facts_json TEXT,
              diagnostic_json TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        ensure_column(&self.pool, "requests", "tool_facts_json", "TEXT").await?;
        ensure_column(&self.pool, "requests", "turn_messages_json", "TEXT").await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS events (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              level TEXT NOT NULL,
              event_type TEXT NOT NULL,
              message TEXT NOT NULL,
              detail_json TEXT,
              created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_requests_status_updated_at ON requests(status, updated_at DESC);",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_requests_previous_response_id ON requests(previous_response_id);",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_events_level_created_at ON events(level, created_at DESC, id DESC);",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn checkpoint_request(
        &self,
        id: &str,
        previous_response_id: Option<&str>,
        model: Option<&str>,
        input: &Value,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO requests (id, previous_response_id, status, model, input_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(id) DO UPDATE SET
              previous_response_id = excluded.previous_response_id,
              status = excluded.status,
              model = excluded.model,
              input_json = excluded.input_json,
              response_json = NULL,
              turn_messages_json = NULL,
              tool_facts_json = NULL,
              diagnostic_json = NULL,
              updated_at = excluded.updated_at;
            "#,
        )
        .bind(id)
        .bind(previous_response_id)
        .bind(RequestStatus::InProgress.as_str())
        .bind(model)
        .bind(storage_request_input_json_string(
            previous_response_id,
            input,
        )?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn run_maintenance(&self, log_retention_days: u16) -> Result<MaintenanceReport> {
        let log_retention_days = log_retention_days.clamp(1, 365);
        let cutoff = Utc::now()
            .checked_sub_signed(Duration::days(i64::from(log_retention_days)))
            .unwrap_or_else(Utc::now)
            .to_rfc3339();
        let deleted_events = sqlx::query(
            r#"
            DELETE FROM events
            WHERE created_at < ?1;
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let request_sanitize = self.sanitize_large_request_payloads().await?;
        let vacuumed_storage = if deleted_events > 0 || request_sanitize.sanitized_requests > 0 {
            self.vacuum_storage().await?;
            true
        } else {
            false
        };
        Ok(MaintenanceReport {
            log_retention_days,
            deleted_events,
            sanitized_requests: request_sanitize.sanitized_requests,
            request_sanitize_batches: request_sanitize.batches,
            request_sanitize_limit_reached: request_sanitize.limit_reached,
            vacuumed_storage,
        })
    }

    async fn vacuum_storage(&self) -> Result<()> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
            .execute(&self.pool)
            .await?;
        sqlx::query("VACUUM;").execute(&self.pool).await?;
        Ok(())
    }

    async fn sanitize_large_request_payloads(&self) -> Result<RequestSanitizeReport> {
        let mut sanitized_requests = 0_u64;
        let mut batches = 0_u64;
        for _ in 0..MAINTENANCE_REQUEST_MAX_BATCHES {
            let batch = self.sanitize_large_request_payload_batch().await?;
            if batch.fetched == 0 {
                return Ok(RequestSanitizeReport {
                    sanitized_requests,
                    batches,
                    limit_reached: false,
                });
            }
            batches = batches.saturating_add(1);
            sanitized_requests = sanitized_requests.saturating_add(batch.changed);
            if batch.fetched < MAINTENANCE_REQUEST_BATCH as u64 || batch.changed == 0 {
                return Ok(RequestSanitizeReport {
                    sanitized_requests,
                    batches,
                    limit_reached: false,
                });
            }
        }
        Ok(RequestSanitizeReport {
            sanitized_requests,
            batches,
            limit_reached: true,
        })
    }

    async fn sanitize_large_request_payload_batch(&self) -> Result<RequestSanitizeBatch> {
        let rows = sqlx::query(
            r#"
            SELECT id, previous_response_id, input_json, response_json, turn_messages_json, tool_facts_json, diagnostic_json
            FROM requests
            WHERE length(input_json) > ?1
               OR COALESCE(length(response_json), 0) > ?1
               OR COALESCE(length(turn_messages_json), 0) > ?1
               OR COALESCE(length(tool_facts_json), 0) > ?1
               OR COALESCE(length(diagnostic_json), 0) > ?1
               OR (
                    previous_response_id IS NULL
                    AND input_json LIKE '%"prompt_cache_key"%'
                    AND input_json LIKE '%"instructions"%'
                    AND input_json LIKE '%"tools"%'
               )
            ORDER BY updated_at ASC
            LIMIT ?2;
            "#,
        )
        .bind(MAINTENANCE_LARGE_FIELD_BYTES)
        .bind(MAINTENANCE_REQUEST_BATCH)
        .fetch_all(&self.pool)
        .await?;

        let fetched = rows.len() as u64;
        let mut changed = 0_u64;
        for row in rows {
            let id: String = row.try_get("id")?;
            let previous_response_id: Option<String> = row.try_get("previous_response_id")?;
            let input_json: String = row.try_get("input_json")?;
            let response_json: Option<String> = row.try_get("response_json")?;
            let turn_messages_json: Option<String> = row.try_get("turn_messages_json")?;
            let tool_facts_json: Option<String> = row.try_get("tool_facts_json")?;
            let diagnostic_json: Option<String> = row.try_get("diagnostic_json")?;

            let input_value: Value = parse_json_field("requests.input_json", &input_json)?;
            let next_input_json =
                storage_request_input_json_string(previous_response_id.as_deref(), &input_value)?;
            let next_response_json =
                sanitize_json_text("requests.response_json", response_json.as_deref())?;
            let next_turn_messages_json = sanitize_value_array_json_text(
                "requests.turn_messages_json",
                stored_input_is_codex_full_context_slice(&next_input_json),
                turn_messages_json.as_deref(),
            )?;
            let next_tool_facts_json = sanitize_tool_facts_json_text(
                "requests.tool_facts_json",
                tool_facts_json.as_deref(),
            )?;
            let next_diagnostic_json =
                sanitize_json_text("requests.diagnostic_json", diagnostic_json.as_deref())?;

            if next_input_json == input_json
                && next_response_json == response_json
                && next_turn_messages_json == turn_messages_json
                && next_tool_facts_json == tool_facts_json
                && next_diagnostic_json == diagnostic_json
            {
                continue;
            }

            sqlx::query(
                r#"
                UPDATE requests
                SET input_json = ?2,
                    response_json = ?3,
                    turn_messages_json = ?4,
                    tool_facts_json = ?5,
                    diagnostic_json = ?6
                WHERE id = ?1;
                "#,
            )
            .bind(&id)
            .bind(next_input_json)
            .bind(next_response_json)
            .bind(next_turn_messages_json)
            .bind(next_tool_facts_json)
            .bind(next_diagnostic_json)
            .execute(&self.pool)
            .await?;
            changed = changed.saturating_add(1);
        }
        Ok(RequestSanitizeBatch { fetched, changed })
    }

    pub async fn runtime_summary(&self, turn_limit: u32) -> Result<RuntimeSummary> {
        let active_requests = count_by_status(&self.pool, "in_progress").await?;
        let request_count = count_by_status(&self.pool, "completed").await?;
        let failed_request_count = count_failed(&self.pool).await?;
        let limit = i64::from(turn_limit.clamp(1, 500));
        let rows = sqlx::query(
            r#"
            SELECT id, status, model, input_json, response_json, created_at, updated_at
            FROM requests
            WHERE status = 'completed'
            ORDER BY updated_at DESC
            LIMIT ?1;
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut newest_first = Vec::with_capacity(rows.len());
        for row in rows {
            newest_first.push(turn_from_row(&row)?);
        }
        let last_turn = newest_first.first().cloned();
        let mut turn_history = newest_first;
        turn_history.reverse();
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
        let last_request_at = last_turn.as_ref().map(|turn| turn.completed_at.clone());
        Ok(RuntimeSummary {
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
        })
    }

    pub async fn runtime_overview(&self) -> Result<RuntimeSummary> {
        let active_requests = count_by_status(&self.pool, "in_progress").await?;
        let request_count = count_by_status(&self.pool, "completed").await?;
        let failed_request_count = count_failed(&self.pool).await?;
        let row = sqlx::query(
            r#"
            SELECT id, status, model, response_json, created_at, updated_at
            FROM requests
            WHERE status = 'completed'
            ORDER BY updated_at DESC
            LIMIT 1;
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;
        let last_turn = row.as_ref().map(turn_from_overview_row).transpose()?;
        let last_request_at = last_turn.as_ref().map(|turn| turn.completed_at.clone());
        Ok(RuntimeSummary {
            active_requests,
            request_count,
            failed_request_count,
            last_request_at,
            last_turn,
            turn_history: Vec::new(),
            total_cached_input_tokens: 0,
            total_cache_miss_input_tokens: 0,
            total_output_tokens: 0,
            average_ms: 0,
        })
    }

    pub async fn recent_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        let limit = i64::from(limit.clamp(1, 200));
        let fetch_limit = limit + 1;
        let rows = if let Some(before) = before.filter(|value| !value.trim().is_empty()) {
            sqlx::query(
                r#"
                SELECT id, level, event_type, message, detail_json, created_at
                FROM events
                WHERE created_at < ?1
                ORDER BY created_at DESC, id DESC
                LIMIT ?2;
                "#,
            )
            .bind(before)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, level, event_type, message, detail_json, created_at
                FROM events
                ORDER BY created_at DESC, id DESC
                LIMIT ?1;
                "#,
            )
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        };

        let has_more = rows.len() > usize::try_from(limit).unwrap_or(200);
        let mut events = rows
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(200))
            .map(|row| event_from_row(&row))
            .collect::<Result<Vec<_>>>()?;
        events.reverse();
        Ok((events, has_more))
    }

    pub async fn recent_visible_events(
        &self,
        limit: u32,
        before: Option<&str>,
    ) -> Result<(Vec<EventRecord>, bool)> {
        let limit = i64::from(limit.clamp(1, 200));
        let fetch_limit = limit + 1;
        let rows = if let Some(before) = before.filter(|value| !value.trim().is_empty()) {
            sqlx::query(
                r#"
                SELECT id, level, event_type, message, detail_json, created_at
                FROM events
                WHERE level <> 'debug' AND created_at < ?1
                ORDER BY created_at DESC, id DESC
                LIMIT ?2;
                "#,
            )
            .bind(before)
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, level, event_type, message, detail_json, created_at
                FROM events
                WHERE level <> 'debug'
                ORDER BY created_at DESC, id DESC
                LIMIT ?1;
                "#,
            )
            .bind(fetch_limit)
            .fetch_all(&self.pool)
            .await?
        };

        let has_more = rows.len() > usize::try_from(limit).unwrap_or(200);
        let mut events = rows
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(200))
            .map(|row| event_from_row(&row))
            .collect::<Result<Vec<_>>>()?;
        events.reverse();
        Ok((events, has_more))
    }

    pub async fn response_context_chain(
        &self,
        previous_response_id: &str,
        max_depth: u32,
    ) -> Result<Vec<StoredResponse>> {
        let mut cursor = Some(previous_response_id.to_owned());
        let mut seen = HashSet::new();
        let mut newest_first = Vec::new();
        let max_depth = max_depth.clamp(1, 10_000);

        for _ in 0..max_depth {
            let Some(id) = cursor.take() else {
                break;
            };
            if !seen.insert(id.clone()) {
                break;
            }
            let row = sqlx::query(
                r#"
                SELECT id, previous_response_id, status, input_json, response_json, turn_messages_json, tool_facts_json
                FROM requests
                WHERE id = ?1
                LIMIT 1;
                "#,
            )
            .bind(&id)
            .fetch_optional(&self.pool)
            .await?;
            let Some(row) = row else {
                break;
            };
            let input_json: String = row.try_get("input_json")?;
            let response_json: Option<String> = row.try_get("response_json")?;
            let turn_messages_json: Option<String> = row.try_get("turn_messages_json")?;
            let tool_facts_json: Option<String> = row.try_get("tool_facts_json")?;
            let previous_response_id: Option<String> = row.try_get("previous_response_id")?;
            let status: String = row.try_get("status")?;
            newest_first.push(StoredResponse {
                id: row.try_get("id")?,
                previous_response_id: previous_response_id.clone(),
                status: RequestStatus::from_str(&status),
                input: parse_json_field("requests.input_json", &input_json)?,
                response: parse_optional_json_field(
                    "requests.response_json",
                    response_json.as_deref(),
                )?
                .unwrap_or(Value::Null),
                turn_messages: parse_optional_json_field(
                    "requests.turn_messages_json",
                    turn_messages_json.as_deref(),
                )?
                .unwrap_or_default(),
                tool_facts: parse_optional_json_field(
                    "requests.tool_facts_json",
                    tool_facts_json.as_deref(),
                )?
                .unwrap_or_default(),
            });
            cursor = previous_response_id;
        }

        newest_first.reverse();
        Ok(newest_first)
    }

    pub async fn append_request_tool_fact(&self, id: &str, fact: &str) -> Result<()> {
        let row = sqlx::query(
            r#"
            SELECT tool_facts_json
            FROM requests
            WHERE id = ?1
            LIMIT 1;
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let existing_facts = row.and_then(|row| {
            row.try_get::<Option<String>, _>("tool_facts_json")
                .ok()
                .flatten()
        });
        let mut facts: Vec<String> =
            parse_optional_json_field("requests.tool_facts_json", existing_facts.as_deref())?
                .unwrap_or_default();
        facts.push(fact.to_owned());
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET tool_facts_json = ?2,
                updated_at = ?3
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(storage_tool_facts_json_string(&facts)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            bail!("request '{id}' was not found while appending tool facts");
        }
        Ok(())
    }

    pub async fn recent_tool_facts(&self, limit: u32) -> Result<Vec<String>> {
        let records = self.recent_tool_fact_records(limit).await?;
        Ok(records
            .into_iter()
            .flat_map(|record| record.tool_facts)
            .collect())
    }

    pub async fn recent_tool_fact_records(&self, limit: u32) -> Result<Vec<RecentToolFactRecord>> {
        let limit = i64::from(limit.clamp(1, 1_000));
        let cutoff = Utc::now()
            .checked_sub_signed(Duration::hours(RECENT_TOOL_FACT_RETENTION_HOURS))
            .unwrap_or_else(Utc::now)
            .to_rfc3339();
        let rows = sqlx::query(
            r#"
            SELECT response_json, tool_facts_json
            FROM requests
            WHERE tool_facts_json IS NOT NULL
              AND updated_at >= ?1
            ORDER BY updated_at DESC
            LIMIT ?2;
            "#,
        )
        .bind(cutoff)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut records = Vec::new();
        for row in rows {
            let response_json = row.try_get::<Option<String>, _>("response_json")?;
            let Some(json) = row.try_get::<Option<String>, _>("tool_facts_json")? else {
                continue;
            };
            let facts: Vec<String> = parse_json_field("requests.tool_facts_json", &json)?;
            records.push(RecentToolFactRecord {
                response: parse_optional_json_field(
                    "requests.response_json",
                    response_json.as_deref(),
                )?
                .unwrap_or(Value::Null),
                tool_facts: facts,
            });
        }
        Ok(records)
    }

    pub async fn replace_request_turn_messages(&self, id: &str, messages: &[Value]) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query("SELECT input_json FROM requests WHERE id = ?1 LIMIT 1;")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let slice_codex_full_context = row
            .as_ref()
            .and_then(|row| row.try_get::<String, _>("input_json").ok())
            .map(|text| stored_input_is_codex_full_context_slice(&text))
            .unwrap_or(false);
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET turn_messages_json = ?2,
                updated_at = ?3
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(storage_turn_messages_json_string(
            slice_codex_full_context,
            messages,
        )?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            bail!("request '{id}' was not found while replacing turn messages");
        }
        Ok(())
    }

    pub async fn append_request_turn_messages(&self, id: &str, messages: &[Value]) -> Result<()> {
        if messages.is_empty() {
            return Ok(());
        }
        let row = sqlx::query(
            r#"
            SELECT turn_messages_json
            FROM requests
            WHERE id = ?1
            LIMIT 1;
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let existing_json = row.and_then(|row| {
            row.try_get::<Option<String>, _>("turn_messages_json")
                .ok()
                .flatten()
        });
        let mut existing: Vec<Value> =
            parse_optional_json_field("requests.turn_messages_json", existing_json.as_deref())?
                .unwrap_or_default();
        existing.extend(messages.iter().cloned());
        self.replace_request_turn_messages(id, &existing).await
    }

    pub async fn finish_request(
        &self,
        id: &str,
        status: RequestStatus,
        response: Option<&Value>,
        diagnostic: Option<&Value>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET status = ?2,
                response_json = ?3,
                diagnostic_json = COALESCE(?4, diagnostic_json),
                updated_at = ?5
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(status.as_str())
        .bind(response.map(storage_json_string).transpose()?)
        .bind(diagnostic.map(storage_json_string).transpose()?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            bail!("request '{id}' was not found while finishing request");
        }
        Ok(())
    }

    pub async fn recover_interrupted_requests(&self, reason: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            r#"
            SELECT id, diagnostic_json
            FROM requests
            WHERE status = 'in_progress'
            ORDER BY updated_at ASC;
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let interrupted_at = Utc::now().to_rfc3339();
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get("id")?;
            let diagnostic_json: Option<String> = row.try_get("diagnostic_json")?;
            let mut diagnostic = diagnostic_json
                .as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            if let Some(object) = diagnostic.as_object_mut() {
                object.insert(
                    "lifecycle".to_owned(),
                    json!({
                        "status": "interrupted",
                        "interrupted_at": interrupted_at,
                        "interruption_reason": reason
                    }),
                );
            }
            sqlx::query(
                r#"
                UPDATE requests
                SET status = ?2,
                    diagnostic_json = ?3,
                    updated_at = ?4
                WHERE id = ?1 AND status = 'in_progress';
                "#,
            )
            .bind(&id)
            .bind(RequestStatus::Interrupted.as_str())
            .bind(serde_json::to_string(&diagnostic)?)
            .bind(&interrupted_at)
            .execute(&self.pool)
            .await?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub async fn update_request_diagnostic(&self, id: &str, diagnostic: &Value) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET diagnostic_json = ?2,
                updated_at = ?3
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(storage_json_string(diagnostic)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            bail!("request '{id}' was not found while updating diagnostics");
        }
        Ok(())
    }

    pub async fn record_event(
        &self,
        level: &str,
        event_type: &str,
        message: &str,
        detail: Option<&Value>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO events (level, event_type, message, detail_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5);
            "#,
        )
        .bind(level)
        .bind(event_type)
        .bind(message)
        .bind(detail.map(storage_json_string).transpose()?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn count_by_status(pool: &SqlitePool, status: &str) -> Result<u64> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE status = ?1;")
        .bind(status)
        .fetch_one(pool)
        .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table});");
    let rows = sqlx::query(&pragma).fetch_all(pool).await?;
    let exists = rows.iter().any(|row| {
        row.try_get::<String, _>("name")
            .map(|name| name == column)
            .unwrap_or(false)
    });
    if !exists {
        let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {definition};");
        sqlx::query(&alter).execute(pool).await?;
    }
    Ok(())
}

async fn count_failed(pool: &SqlitePool) -> Result<u64> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM requests WHERE status IN ('failed', 'interrupted');",
    )
    .fetch_one(pool)
    .await?;
    Ok(u64::try_from(count).unwrap_or(0))
}

fn turn_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<RequestTurn> {
    let id: String = row.try_get("id")?;
    let model: Option<String> = row.try_get("model")?;
    let input_json: String = row.try_get("input_json")?;
    let response_json: Option<String> = row.try_get("response_json")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let input = serde_json::from_str::<Value>(&input_json).unwrap_or(Value::Null);
    let response = response_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null);
    let empty_usage = Value::Null;
    let usage = usage_value(&response).unwrap_or(&empty_usage);
    let requested_model = input
        .get("model")
        .and_then(Value::as_str)
        .or_else(|| {
            input
                .pointer("/metadata/requested_model")
                .and_then(Value::as_str)
        })
        .unwrap_or_else(|| model.as_deref().unwrap_or(""))
        .to_owned();
    let model = model.unwrap_or_else(|| requested_model.clone());
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
            .and_then(Value::as_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
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
    .unwrap_or_else(|| {
        let input_tokens = first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
        input_tokens.saturating_sub(cached_input_tokens)
    });
    let output_tokens = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "reasoning_output_tokens",
        ],
    )
    .unwrap_or(0);
    let total_tokens = first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        cached_input_tokens
            .saturating_add(cache_miss_input_tokens)
            .saturating_add(output_tokens)
    });
    Ok(RequestTurn {
        id,
        model,
        requested_model,
        completed_at: updated_at.clone(),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: duration_ms(&created_at, &updated_at),
    })
}

fn turn_from_overview_row(row: &sqlx::sqlite::SqliteRow) -> Result<RequestTurn> {
    let id: String = row.try_get("id")?;
    let model = row
        .try_get::<Option<String>, _>("model")?
        .unwrap_or_default();
    let response_json: Option<String> = row.try_get("response_json")?;
    let created_at: String = row.try_get("created_at")?;
    let updated_at: String = row.try_get("updated_at")?;
    let response = response_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null);
    let empty_usage = Value::Null;
    let usage = usage_value(&response).unwrap_or(&empty_usage);
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
            .and_then(Value::as_u64)
    })
    .or_else(|| {
        usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
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
    .unwrap_or_else(|| {
        let input_tokens = first_u64(usage, &["input_tokens", "prompt_tokens"]).unwrap_or(0);
        input_tokens.saturating_sub(cached_input_tokens)
    });
    let output_tokens = first_u64(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "reasoning_output_tokens",
        ],
    )
    .unwrap_or(0);
    let total_tokens = first_u64(usage, &["total_tokens"]).unwrap_or_else(|| {
        cached_input_tokens
            .saturating_add(cache_miss_input_tokens)
            .saturating_add(output_tokens)
    });
    Ok(RequestTurn {
        id,
        model: model.clone(),
        requested_model: model,
        completed_at: updated_at.clone(),
        cached_input_tokens,
        cache_miss_input_tokens,
        output_tokens,
        total_tokens,
        request_ms: duration_ms(&created_at, &updated_at),
    })
}

fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<EventRecord> {
    let detail_json: Option<String> = row.try_get("detail_json")?;
    Ok(EventRecord {
        id: row.try_get("id")?,
        level: row.try_get("level")?,
        event_type: row.try_get("event_type")?,
        message: row.try_get("message")?,
        detail: detail_json.and_then(|text| serde_json::from_str(&text).ok()),
        ts: row.try_get("created_at")?,
    })
}

fn usage_value(response: &Value) -> Option<&Value> {
    response
        .get("usage")
        .or_else(|| response.pointer("/response/usage"))
}

fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(number_like_u64))
}

fn number_like_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn duration_ms(start: &str, end: &str) -> u64 {
    let Ok(start) = DateTime::parse_from_rfc3339(start) else {
        return 0;
    };
    let Ok(end) = DateTime::parse_from_rfc3339(end) else {
        return 0;
    };
    let millis = end
        .with_timezone(&Utc)
        .signed_duration_since(start.with_timezone(&Utc))
        .num_milliseconds();
    u64::try_from(millis).unwrap_or(0)
}

fn storage_request_input_json_string(
    previous_response_id: Option<&str>,
    value: &Value,
) -> Result<String> {
    let value = request_input_for_storage(previous_response_id, value);
    storage_json_string(&value)
}

fn request_input_for_storage(previous_response_id: Option<&str>, value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut stored = serde_json::Map::new();
    for key in ["model", "prompt_cache_key"] {
        if let Some(value) = object.get(key) {
            stored.insert(key.to_owned(), value.clone());
        }
    }

    if let Some(input) = object.get("input") {
        let full_context = request_looks_like_codex_full_context(previous_response_id, value);
        stored.insert(
            "input".to_owned(),
            if full_context {
                tail_slice_value(input, CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS)
            } else {
                input.clone()
            },
        );
        if full_context {
            let (original_input_bytes, original_input_hash) = json_value_storage_stats(input);
            stored.insert(
                "_codeseex_storage".to_owned(),
                json!({
                    "mode": "codex_full_context_slice",
                    "reason": "Codex already owns and resends full conversation context; CodeSeeX stores only bounded adapter replay state.",
                    "original_input_items": input.as_array().map(Vec::len).unwrap_or(0),
                    "retained_tail_items": input.as_array().map(|items| items.len().min(CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS)).unwrap_or(0),
                    "original_input_bytes": original_input_bytes,
                    "original_input_hash": original_input_hash
                }),
            );
        }
        return Value::Object(stored);
    }

    if let Some(messages) = object.get("messages") {
        stored.insert("messages".to_owned(), messages.clone());
        return Value::Object(stored);
    }

    value.clone()
}

fn request_looks_like_codex_full_context(
    previous_response_id: Option<&str>,
    value: &Value,
) -> bool {
    if previous_response_id.is_some() {
        return false;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("prompt_cache_key").is_none()
        || object.get("instructions").is_none()
        || object.get("tools").is_none()
    {
        return false;
    }
    object
        .get("input")
        .and_then(Value::as_array)
        .map(|items| items.len() > CODEX_FULL_CONTEXT_INPUT_ITEMS_THRESHOLD)
        .unwrap_or(false)
}

fn storage_turn_messages_json_string(
    slice_codex_full_context: bool,
    values: &[Value],
) -> Result<String> {
    let values = turn_messages_for_storage(slice_codex_full_context, values);
    storage_json_array_string(&values)
}

fn turn_messages_for_storage(slice_codex_full_context: bool, values: &[Value]) -> Vec<Value> {
    if !slice_codex_full_context || values.len() <= CODEX_FULL_CONTEXT_TURN_MESSAGES_THRESHOLD {
        return values.to_vec();
    }
    tail_slice(values, CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS)
}

fn stored_input_is_codex_full_context_slice(text: &str) -> bool {
    text.contains("\"mode\":\"codex_full_context_slice\"")
        || text.contains("\"mode\": \"codex_full_context_slice\"")
}

fn tail_slice_value(value: &Value, limit: usize) -> Value {
    value
        .as_array()
        .map(|items| Value::Array(tail_slice(items, limit)))
        .unwrap_or_else(|| value.clone())
}

fn tail_slice<T: Clone>(items: &[T], limit: usize) -> Vec<T> {
    let start = items.len().saturating_sub(limit);
    items[start..].to_vec()
}

fn json_value_storage_stats(value: &Value) -> (usize, String) {
    let serialized = serde_json::to_string(value).unwrap_or_default();
    (serialized.len(), stable_hash_hex(serialized.as_bytes()))
}

fn storage_json_string(value: &Value) -> Result<String> {
    let sanitized = sanitize_json_for_storage(value);
    let serialized = serde_json::to_string(&sanitized)?;
    if serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(serialized);
    }

    let compacted = compact_json_for_storage(&sanitized);
    let compacted_serialized = serde_json::to_string(&compacted)?;
    if compacted_serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(compacted_serialized);
    }

    Ok(serde_json::to_string(&json!({
        "_codeseex_storage_notice": "json payload omitted after exceeding durable state budget",
        "original_bytes": serialized.len(),
        "compacted_bytes": compacted_serialized.len(),
        "hash": stable_hash_hex(serialized.as_bytes())
    }))?)
}

fn storage_json_array_string(values: &[Value]) -> Result<String> {
    let value = Value::Array(values.iter().map(sanitize_json_for_storage).collect());
    let serialized = serde_json::to_string(&value)?;
    if serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(serialized);
    }

    let compacted = compact_json_for_storage(&value);
    let compacted_serialized = serde_json::to_string(&compacted)?;
    if compacted_serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(compacted_serialized);
    }

    Ok(serde_json::to_string(&vec![json!({
        "_codeseex_storage_notice": "json array payload omitted after exceeding durable state budget",
        "original_bytes": serialized.len(),
        "compacted_bytes": compacted_serialized.len(),
        "hash": stable_hash_hex(serialized.as_bytes())
    })])?)
}

fn storage_tool_facts_json_string(facts: &[String]) -> Result<String> {
    let mut prior_omitted = 0_usize;
    let mut sanitized = facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| {
            if index == 0 {
                if let Some(count) = omitted_tool_fact_count(fact) {
                    prior_omitted = prior_omitted.saturating_add(count);
                    return None;
                }
            }
            Some(fact)
        })
        .map(|fact| sanitize_string_for_storage(fact))
        .collect::<Vec<_>>();
    if sanitized.len() > MAX_STORAGE_TOOL_FACTS_PER_REQUEST {
        let omitted = prior_omitted + sanitized.len() - (MAX_STORAGE_TOOL_FACTS_PER_REQUEST - 1);
        let tail_start = sanitized.len() - (MAX_STORAGE_TOOL_FACTS_PER_REQUEST - 1);
        let mut compacted = Vec::with_capacity(MAX_STORAGE_TOOL_FACTS_PER_REQUEST);
        compacted.push(tool_fact_omitted_marker(omitted));
        compacted.extend(sanitized.drain(tail_start..));
        sanitized = compacted;
    } else if prior_omitted > 0 {
        let available = MAX_STORAGE_TOOL_FACTS_PER_REQUEST - 1;
        if sanitized.len() > available {
            let newly_omitted = sanitized.len() - available;
            prior_omitted = prior_omitted.saturating_add(newly_omitted);
            sanitized.drain(0..newly_omitted);
        }
        sanitized.insert(0, tool_fact_omitted_marker(prior_omitted));
    }

    let serialized = serde_json::to_string(&sanitized)?;
    if serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(serialized);
    }

    let compacted = sanitized
        .into_iter()
        .map(|fact| truncate_storage_string(&fact, COMPACT_STORAGE_STRING_CHARS))
        .collect::<Vec<_>>();
    let compacted_serialized = serde_json::to_string(&compacted)?;
    if compacted_serialized.len() <= MAX_STORAGE_JSON_BYTES {
        return Ok(compacted_serialized);
    }

    Ok(serde_json::to_string(&vec![format!(
        "[CodeSeeX storage omitted tool facts after exceeding durable state budget; original_bytes={}, compacted_bytes={}, hash={}]",
        serialized.len(),
        compacted_serialized.len(),
        stable_hash_hex(serialized.as_bytes())
    )])?)
}

fn tool_fact_omitted_marker(count: usize) -> String {
    format!("{TOOL_FACT_OMITTED_PREFIX}{count}{TOOL_FACT_OMITTED_SUFFIX}")
}

fn omitted_tool_fact_count(text: &str) -> Option<usize> {
    text.strip_prefix(TOOL_FACT_OMITTED_PREFIX)?
        .strip_suffix(TOOL_FACT_OMITTED_SUFFIX)?
        .parse()
        .ok()
}

fn sanitize_json_text(label: &str, text: Option<&str>) -> Result<Option<String>> {
    text.map(|text| {
        let value = parse_json_field::<Value>(label, text)?;
        storage_json_string(&value)
    })
    .transpose()
}

fn sanitize_value_array_json_text(
    label: &str,
    slice_codex_full_context: bool,
    text: Option<&str>,
) -> Result<Option<String>> {
    text.map(|text| {
        let values = parse_json_field::<Vec<Value>>(label, text)?;
        storage_turn_messages_json_string(slice_codex_full_context, &values)
    })
    .transpose()
}

fn sanitize_tool_facts_json_text(label: &str, text: Option<&str>) -> Result<Option<String>> {
    text.map(|text| {
        let facts = parse_json_field::<Vec<String>>(label, text)?;
        storage_tool_facts_json_string(&facts)
    })
    .transpose()
}

fn sanitize_json_for_storage(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(sanitize_string_for_storage(text)),
        Value::Array(items) => Value::Array(items.iter().map(sanitize_json_for_storage).collect()),
        Value::Object(object) => {
            let mut sanitized = serde_json::Map::with_capacity(object.len());
            for (key, value) in object {
                let next = if is_sensitive_storage_key(key) {
                    Value::String("[redacted sensitive value]".to_owned())
                } else {
                    sanitize_json_for_storage(value)
                };
                sanitized.insert(key.clone(), next);
            }
            Value::Object(sanitized)
        }
        other => other.clone(),
    }
}

fn sanitize_string_for_storage(text: &str) -> String {
    let redacted = redact_inline_data_urls_for_storage(text);
    truncate_storage_string(&redacted, MAX_STORAGE_STRING_CHARS)
}

fn is_sensitive_storage_key(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase();
    matches!(
        key.as_str(),
        "api_key"
            | "apikey"
            | "authorization"
            | "auth"
            | "bearer"
            | "token"
            | "access_token"
            | "refresh_token"
            | "password"
            | "secret"
            | "client_secret"
            | "proxy_password"
    ) || key.ends_with("_api_key")
        || key.ends_with("_token")
        || key.ends_with("_secret")
}

fn redact_inline_data_urls_for_storage(text: &str) -> String {
    let Some(mut cursor) = text.find("data:") else {
        return text.to_owned();
    };
    let mut output = String::with_capacity(text.len().min(MAX_STORAGE_STRING_CHARS));
    let mut copied_until = 0;
    while cursor < text.len() {
        output.push_str(&text[copied_until..cursor]);
        let mut end = text.len();
        for (offset, ch) in text[cursor..].char_indices() {
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')' | ']' | '}' | '`') {
                end = cursor + offset;
                break;
            }
        }
        let segment = &text[cursor..end];
        if segment.len() > 1024 || segment.contains(";base64,") {
            output.push_str(&format!(
                "[inline-data-url omitted chars={} bytes={} hash={}]",
                segment.chars().count(),
                segment.len(),
                stable_hash_hex(segment.as_bytes())
            ));
        } else {
            output.push_str(segment);
        }
        copied_until = end;
        let Some(next) = text[copied_until..].find("data:") else {
            break;
        };
        cursor = copied_until + next;
    }
    output.push_str(&text[copied_until..]);
    output
}

fn truncate_storage_string(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(96);
    let prefix = text.chars().take(keep).collect::<String>();
    format!(
        "{prefix}...[truncated chars={} bytes={} hash={}]",
        count,
        text.len(),
        stable_hash_hex(text.as_bytes())
    )
}

fn compact_json_for_storage(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            Value::String(truncate_storage_string(text, COMPACT_STORAGE_STRING_CHARS))
        }
        Value::Array(items) => compact_json_array_for_storage(items),
        Value::Object(object) => {
            let mut compacted = serde_json::Map::with_capacity(object.len());
            for (index, (key, value)) in object.iter().enumerate() {
                if index >= COMPACT_STORAGE_OBJECT_KEYS {
                    compacted.insert(
                        "_codeseex_storage_omitted_keys".to_owned(),
                        json!(object.len() - COMPACT_STORAGE_OBJECT_KEYS),
                    );
                    break;
                }
                compacted.insert(key.clone(), compact_json_for_storage(value));
            }
            Value::Object(compacted)
        }
        other => other.clone(),
    }
}

fn compact_json_array_for_storage(items: &[Value]) -> Value {
    let keep_all = COMPACT_STORAGE_ARRAY_HEAD_ITEMS + COMPACT_STORAGE_ARRAY_TAIL_ITEMS;
    if items.len() <= keep_all {
        return Value::Array(items.iter().map(compact_json_for_storage).collect());
    }

    let tail_start = items.len().saturating_sub(COMPACT_STORAGE_ARRAY_TAIL_ITEMS);
    let mut compacted = Vec::with_capacity(keep_all + 1);
    compacted.extend(
        items
            .iter()
            .take(COMPACT_STORAGE_ARRAY_HEAD_ITEMS)
            .map(compact_json_for_storage),
    );
    compacted.push(json!({
        "_codeseex_storage_notice": "array items omitted after exceeding durable state budget",
        "omitted_items": tail_start.saturating_sub(COMPACT_STORAGE_ARRAY_HEAD_ITEMS),
        "total_items": items.len()
    }));
    compacted.extend(items.iter().skip(tail_start).map(compact_json_for_storage));
    Value::Array(compacted)
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[tokio::test]
    async fn open_configures_sqlite_for_local_proxy_state() {
        let path = temp_db_path("sqlite-options");
        let store = Store::open(&path).await.expect("open store");

        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode;")
            .fetch_one(&store.pool)
            .await
            .expect("journal mode");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");

        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous;")
            .fetch_one(&store.pool)
            .await
            .expect("synchronous pragma");
        assert_eq!(synchronous, 1);

        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout;")
            .fetch_one(&store.pool)
            .await
            .expect("busy timeout");
        assert!(busy_timeout >= 5_000);

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn response_context_chain_includes_failed_parent_without_response() {
        let path = temp_db_path("failed-parent");
        let store = Store::open(&path).await.expect("open store");

        let parent_input = json!({
            "input": [
                {"type":"function_call_output","call_id":"call_failed","output":"FAILED_FACT_42"},
                {"role":"user","content":[{"type":"input_text","text":"force upstream failure"}]}
            ]
        });
        store
            .checkpoint_request(
                "resp_failed_parent",
                None,
                Some("deepseek-v4-pro"),
                &parent_input,
            )
            .await
            .expect("checkpoint parent");
        store
            .finish_request("resp_failed_parent", RequestStatus::Failed, None, None)
            .await
            .expect("finish failed parent");

        let child_input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"recover"}]}]
        });
        store
            .checkpoint_request(
                "resp_child",
                Some("resp_failed_parent"),
                Some("deepseek-v4-pro"),
                &child_input,
            )
            .await
            .expect("checkpoint child");
        store
            .finish_request(
                "resp_child",
                RequestStatus::Completed,
                Some(&json!({"output":[]})),
                None,
            )
            .await
            .expect("finish child");

        let chain = store
            .response_context_chain("resp_child", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].status, RequestStatus::Failed);
        assert_eq!(chain[0].response, Value::Null);
        assert!(chain[0].tool_facts.is_empty());
        assert_eq!(chain[1].status, RequestStatus::Completed);

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn response_context_chain_includes_persisted_tool_facts() {
        let path = temp_db_path("tool-facts");
        let store = Store::open(&path).await.expect("open store");

        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"list files"}]}]
        });
        store
            .checkpoint_request("resp_tool", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");
        store
            .append_request_tool_fact(
                "resp_tool",
                "tool=list_directory call_id=call_1 ok=true result=Cargo.toml",
            )
            .await
            .expect("append tool fact");
        store
            .finish_request(
                "resp_tool",
                RequestStatus::Completed,
                Some(&json!({"output":[]})),
                None,
            )
            .await
            .expect("finish request");

        let chain = store
            .response_context_chain("resp_tool", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].tool_facts.len(), 1);
        assert!(chain[0].tool_facts[0].contains("list_directory"));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn recent_tool_facts_returns_persisted_facts() {
        let path = temp_db_path("recent-tool-facts");
        let store = Store::open(&path).await.expect("open store");

        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"search"}]}]
        });
        store
            .checkpoint_request("resp_search", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");
        store
            .append_request_tool_fact(
                "resp_search",
                "tool=web_search call_id=call_web arguments={\"mode\":\"search\",\"query\":\"Shanghai weather\"} ok=true result={\"summary\":\"rain\"}",
            )
            .await
            .expect("append tool fact");

        let facts = store
            .recent_tool_facts(10)
            .await
            .expect("recent tool facts");
        assert_eq!(facts.len(), 1);
        assert!(facts[0].contains("Shanghai weather"));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn recent_tool_facts_ignores_stale_adapter_facts() {
        let path = temp_db_path("recent-tool-facts-stale");
        let store = Store::open(&path).await.expect("open store");

        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"search"}]}]
        });
        store
            .checkpoint_request("resp_old_search", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");
        store
            .append_request_tool_fact(
                "resp_old_search",
                "tool=web_search call_id=call_old ok=true result=stale",
            )
            .await
            .expect("append tool fact");
        let old_ts = Utc::now()
            .checked_sub_signed(Duration::hours(RECENT_TOOL_FACT_RETENTION_HOURS + 1))
            .expect("old timestamp")
            .to_rfc3339();
        sqlx::query("UPDATE requests SET updated_at = ?2 WHERE id = ?1;")
            .bind("resp_old_search")
            .bind(old_ts)
            .execute(&store.pool)
            .await
            .expect("backdate request");

        let facts = store
            .recent_tool_facts(10)
            .await
            .expect("recent tool facts");
        assert!(facts.is_empty(), "{facts:?}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn response_context_chain_includes_persisted_turn_messages() {
        let path = temp_db_path("turn-messages");
        let store = Store::open(&path).await.expect("open store");

        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"list files"}]}]
        });
        store
            .checkpoint_request("resp_turn", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");
        store
            .replace_request_turn_messages(
                "resp_turn",
                &[
                    json!({"role":"user","content":"list files"}),
                    json!({
                        "role":"assistant",
                        "content":"",
                        "reasoning_content":"need directory first",
                        "tool_calls":[{
                            "id":"call_1",
                            "type":"function",
                            "function":{"name":"list_directory","arguments":"{\"path\":\".\"}"}
                        }]
                    }),
                ],
            )
            .await
            .expect("replace turn messages");
        store
            .append_request_turn_messages(
                "resp_turn",
                &[json!({"role":"tool","tool_call_id":"call_1","content":"Cargo.toml"})],
            )
            .await
            .expect("append turn message");
        store
            .finish_request(
                "resp_turn",
                RequestStatus::Completed,
                Some(&json!({"output":[]})),
                None,
            )
            .await
            .expect("finish request");

        let chain = store
            .response_context_chain("resp_turn", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].turn_messages.len(), 3);
        assert_eq!(
            chain[0].turn_messages[1]
                .get("reasoning_content")
                .and_then(Value::as_str),
            Some("need directory first")
        );
        assert_eq!(
            chain[0].turn_messages[2]
                .get("tool_call_id")
                .and_then(Value::as_str),
            Some("call_1")
        );

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn turn_messages_keep_array_shape_when_structurally_compacted() {
        let path = temp_db_path("turn-messages-compact");
        let store = Store::open(&path).await.expect("open store");

        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"many messages"}]}]
        });
        store
            .checkpoint_request("resp_many_turns", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        let messages = (0..4_000)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": format!("stored turn message {index:04} {}", "m".repeat(256))
                })
            })
            .collect::<Vec<_>>();
        store
            .replace_request_turn_messages("resp_many_turns", &messages)
            .await
            .expect("replace turn messages");

        let stored_json: String =
            sqlx::query_scalar("SELECT turn_messages_json FROM requests WHERE id = ?1;")
                .bind("resp_many_turns")
                .fetch_one(&store.pool)
                .await
                .expect("stored turn messages");
        assert!(stored_json.len() <= MAX_STORAGE_JSON_BYTES, "{stored_json}");
        let stored: Vec<Value> =
            serde_json::from_str(&stored_json).expect("turn messages stay array-shaped");
        assert_eq!(
            stored.len(),
            COMPACT_STORAGE_ARRAY_HEAD_ITEMS + COMPACT_STORAGE_ARRAY_TAIL_ITEMS + 1
        );
        assert!(stored_json.contains("array items omitted"), "{stored_json}");
        assert!(
            stored_json.contains("stored turn message 0000"),
            "{stored_json}"
        );
        assert!(
            stored_json.contains("stored turn message 3999"),
            "{stored_json}"
        );

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn checkpoint_reuse_clears_stale_response_and_tool_facts() {
        let path = temp_db_path("checkpoint-reuse");
        let store = Store::open(&path).await.expect("open store");

        let first_input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"first"}]}]
        });
        store
            .checkpoint_request("resp_reused", None, Some("deepseek-v4-pro"), &first_input)
            .await
            .expect("checkpoint first");
        store
            .append_request_tool_fact("resp_reused", "tool=list_directory result=Cargo.toml")
            .await
            .expect("append tool fact");
        store
            .append_request_turn_messages(
                "resp_reused",
                &[json!({"role":"assistant","content":"old turn"})],
            )
            .await
            .expect("append turn message");
        store
            .finish_request(
                "resp_reused",
                RequestStatus::Completed,
                Some(&json!({"output":[{"type":"message","content":[{"text":"old"}]}]})),
                Some(&json!({"old": true})),
            )
            .await
            .expect("finish first");

        let second_input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"second"}]}]
        });
        store
            .checkpoint_request("resp_reused", None, Some("deepseek-v4-pro"), &second_input)
            .await
            .expect("checkpoint second");

        let chain = store
            .response_context_chain("resp_reused", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].status, RequestStatus::InProgress);
        assert_eq!(chain[0].response, Value::Null);
        assert!(chain[0].tool_facts.is_empty());
        assert!(chain[0].turn_messages.is_empty());
        assert_eq!(
            chain[0]
                .input
                .pointer("/input/0/content/0/text")
                .and_then(Value::as_str),
            Some("second")
        );

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn recover_interrupted_requests_marks_stale_in_progress() {
        let path = temp_db_path("recover-interrupted");
        let store = Store::open(&path).await.expect("open store");
        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"hello"}]}]
        });
        store
            .checkpoint_request("resp_stale", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint stale");

        let recovered = store
            .recover_interrupted_requests("proxy_started_with_in_progress_checkpoint")
            .await
            .expect("recover interrupted");
        assert_eq!(recovered, vec!["resp_stale".to_owned()]);

        let chain = store
            .response_context_chain("resp_stale", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].status, RequestStatus::Interrupted);

        let overview = store.runtime_overview().await.expect("runtime overview");
        assert_eq!(overview.active_requests, 0);
        assert_eq!(overview.failed_request_count, 1);

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn append_tool_fact_fails_for_missing_request() {
        let path = temp_db_path("missing-tool-fact-request");
        let store = Store::open(&path).await.expect("open store");
        let error = store
            .append_request_tool_fact("missing", "tool=list_directory")
            .await
            .expect_err("missing request should fail");
        assert!(error.to_string().contains("was not found"));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn appended_tool_facts_keep_string_array_shape_under_storage_budget() {
        let path = temp_db_path("tool-facts-budget");
        let store = Store::open(&path).await.expect("open store");
        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"many facts"}]}]
        });
        store
            .checkpoint_request("resp_many_facts", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        for index in 0..120 {
            store
                .append_request_tool_fact(
                    "resp_many_facts",
                    &format!("tool_fact {index:04} {}", "f".repeat(8_000)),
                )
                .await
                .expect("append tool fact");
        }

        let stored_json: String =
            sqlx::query_scalar("SELECT tool_facts_json FROM requests WHERE id = ?1;")
                .bind("resp_many_facts")
                .fetch_one(&store.pool)
                .await
                .expect("stored tool facts");
        assert!(stored_json.len() <= MAX_STORAGE_JSON_BYTES, "{stored_json}");
        let facts: Vec<String> =
            serde_json::from_str(&stored_json).expect("tool facts stay string array-shaped");
        assert_eq!(facts.len(), MAX_STORAGE_TOOL_FACTS_PER_REQUEST);
        assert!(facts[0].contains("omitted 21 older tool fact"), "{facts:?}");
        assert!(
            facts.last().unwrap().contains("tool_fact 0119"),
            "{facts:?}"
        );
        assert!(facts
            .iter()
            .all(|fact| fact.chars().count() <= MAX_STORAGE_STRING_CHARS));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn recent_visible_events_hides_debug_diagnostics() {
        let path = temp_db_path("visible-events");
        let store = Store::open(&path).await.expect("open store");

        store
            .record_event(
                "debug",
                "tool_loop_iteration",
                "Internal diagnostic.",
                Some(&json!({"call_id": "call_secret"})),
            )
            .await
            .expect("record debug event");
        store
            .record_event("info", "request_received", "Visible event.", None)
            .await
            .expect("record info event");

        let (all_events, _) = store.recent_events(10, None).await.expect("all events");
        assert_eq!(all_events.len(), 2);

        let (visible_events, _) = store
            .recent_visible_events(10, None)
            .await
            .expect("visible events");
        assert_eq!(visible_events.len(), 1);
        assert_eq!(visible_events[0].level, "info");
        assert_eq!(visible_events[0].event_type, "request_received");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn checkpoint_request_sanitizes_large_inline_data_and_secrets() {
        let path = temp_db_path("sanitize-checkpoint");
        let store = Store::open(&path).await.expect("open store");
        let screenshot = format!("before data:image/png;base64,{} after", "A".repeat(80_000));
        let input = json!({
            "api_key": "sk-should-not-be-stored",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": screenshot}]
            }]
        });

        store
            .checkpoint_request("resp_sanitize", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        let chain = store
            .response_context_chain("resp_sanitize", 10)
            .await
            .expect("response context chain");
        let stored = serde_json::to_string(&chain[0].input).expect("stored input json");
        assert!(stored.contains("[inline-data-url omitted"), "{stored}");
        assert!(!stored.contains("sk-should-not-be-stored"), "{stored}");
        assert!(!stored.contains(&"A".repeat(2048)), "{stored}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn checkpoint_request_compacts_many_small_items_before_storage() {
        let path = temp_db_path("compact-many-small-items");
        let store = Store::open(&path).await.expect("open store");
        let items = (0..4_000)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("small item {index:04} {}", "z".repeat(256))
                    }]
                })
            })
            .collect::<Vec<_>>();
        let input = json!({ "input": items });

        store
            .checkpoint_request("resp_structural", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        let stored_json: String =
            sqlx::query_scalar("SELECT input_json FROM requests WHERE id = ?1;")
                .bind("resp_structural")
                .fetch_one(&store.pool)
                .await
                .expect("stored input");
        assert!(stored_json.len() <= MAX_STORAGE_JSON_BYTES, "{stored_json}");
        assert!(stored_json.contains("array items omitted"), "{stored_json}");

        let chain = store
            .response_context_chain("resp_structural", 10)
            .await
            .expect("response context chain");
        let stored_items = chain[0]
            .input
            .pointer("/input")
            .and_then(Value::as_array)
            .expect("stored input array");
        assert_eq!(
            stored_items.len(),
            COMPACT_STORAGE_ARRAY_HEAD_ITEMS + COMPACT_STORAGE_ARRAY_TAIL_ITEMS + 1
        );
        let joined = serde_json::to_string(&chain[0].input).expect("stored input json");
        assert!(joined.contains("small item 0000"), "{joined}");
        assert!(joined.contains("small item 3999"), "{joined}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn checkpoint_request_slices_codex_full_context_payload() {
        let path = temp_db_path("codex-full-context-slice");
        let store = Store::open(&path).await.expect("open store");
        let items = (0..150)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": format!("history item {index:03}") }]
                })
            })
            .collect::<Vec<_>>();
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-full-context",
            "instructions": "system instructions should not be durable state",
            "tools": [{"type": "function", "function": {"name": "expensive_tool"}}],
            "input": items
        });
        let raw_len = serde_json::to_string(&input).expect("raw input").len();

        store
            .checkpoint_request("resp_full_context", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        let stored_json: String =
            sqlx::query_scalar("SELECT input_json FROM requests WHERE id = ?1;")
                .bind("resp_full_context")
                .fetch_one(&store.pool)
                .await
                .expect("stored input");
        assert!(stored_json.len() < raw_len / 2, "{stored_json}");
        let stored: Value = serde_json::from_str(&stored_json).expect("stored json");
        assert!(stored.get("instructions").is_none(), "{stored}");
        assert!(stored.get("tools").is_none(), "{stored}");
        assert_eq!(
            stored
                .pointer("/_codeseex_storage/mode")
                .and_then(Value::as_str),
            Some("codex_full_context_slice")
        );
        let stored_items = stored
            .get("input")
            .and_then(Value::as_array)
            .expect("stored input items");
        assert_eq!(stored_items.len(), CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS);
        assert!(serde_json::to_string(&stored_items[0])
            .expect("first retained item")
            .contains("history item 118"));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn checkpoint_request_keeps_delta_input_when_previous_response_id_is_used() {
        let path = temp_db_path("previous-response-delta");
        let store = Store::open(&path).await.expect("open store");
        let items = (0..120)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": [{"type": "input_text", "text": format!("delta item {index:03}") }]
                })
            })
            .collect::<Vec<_>>();
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-with-previous",
            "instructions": "not replayed from adapter state",
            "tools": [{"type": "function", "function": {"name": "tool"}}],
            "input": items
        });

        store
            .checkpoint_request(
                "resp_delta",
                Some("resp_parent"),
                Some("deepseek-v4-pro"),
                &input,
            )
            .await
            .expect("checkpoint request");

        let stored_json: String =
            sqlx::query_scalar("SELECT input_json FROM requests WHERE id = ?1;")
                .bind("resp_delta")
                .fetch_one(&store.pool)
                .await
                .expect("stored input");
        let stored: Value = serde_json::from_str(&stored_json).expect("stored json");
        assert!(stored.get("_codeseex_storage").is_none(), "{stored}");
        assert_eq!(
            stored.get("input").and_then(Value::as_array).map(Vec::len),
            Some(120)
        );
        assert!(stored.get("instructions").is_none(), "{stored}");
        assert!(stored.get("tools").is_none(), "{stored}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn turn_messages_are_sliced_for_codex_full_context_without_parent() {
        let path = temp_db_path("turn-messages-full-context-slice");
        let store = Store::open(&path).await.expect("open store");
        let input = json!({
            "model": "deepseek-v4-pro",
            "prompt_cache_key": "thread-turn-slice",
            "instructions": "instructions",
            "tools": [{"type": "function", "function": {"name": "tool"}}],
            "input": (0..120).map(|index| json!({
                "role": "user",
                "content": [{"type": "input_text", "text": format!("history {index}") }]
            })).collect::<Vec<_>>()
        });
        store
            .checkpoint_request("resp_turn_slice", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");
        let messages = (0..120)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": format!("compiled history message {index:03}")
                })
            })
            .collect::<Vec<_>>();

        store
            .replace_request_turn_messages("resp_turn_slice", &messages)
            .await
            .expect("replace turn messages");

        let stored_json: String =
            sqlx::query_scalar("SELECT turn_messages_json FROM requests WHERE id = ?1;")
                .bind("resp_turn_slice")
                .fetch_one(&store.pool)
                .await
                .expect("stored turn messages");
        let stored: Vec<Value> = serde_json::from_str(&stored_json).expect("stored messages");
        assert_eq!(stored.len(), CODEX_FULL_CONTEXT_STORAGE_TAIL_ITEMS);
        assert!(serde_json::to_string(&stored[0])
            .expect("first retained message")
            .contains("compiled history message 088"));

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn finish_request_sanitizes_large_response_strings() {
        let path = temp_db_path("sanitize-response");
        let store = Store::open(&path).await.expect("open store");
        let input = json!({
            "input": [{"role":"user","content":[{"type":"input_text","text":"hello"}]}]
        });
        store
            .checkpoint_request("resp_large", None, Some("deepseek-v4-pro"), &input)
            .await
            .expect("checkpoint request");

        let response = json!({
            "authorization": "Bearer should-not-be-stored",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "x".repeat(MAX_STORAGE_STRING_CHARS + 1024)}]
            }]
        });
        store
            .finish_request(
                "resp_large",
                RequestStatus::Completed,
                Some(&response),
                None,
            )
            .await
            .expect("finish request");

        let chain = store
            .response_context_chain("resp_large", 10)
            .await
            .expect("response context chain");
        let stored = serde_json::to_string(&chain[0].response).expect("stored response json");
        assert!(stored.contains("[truncated chars="), "{stored}");
        assert!(stored.contains("[redacted sensitive value]"), "{stored}");
        assert!(!stored.contains("should-not-be-stored"), "{stored}");
        assert!(stored.len() < MAX_STORAGE_STRING_CHARS + 2048, "{stored}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn maintenance_prunes_events_older_than_retention() {
        let path = temp_db_path("maintenance-events");
        let store = Store::open(&path).await.expect("open store");

        store
            .record_event("info", "old_event", "Old event.", None)
            .await
            .expect("record old event");
        store
            .record_event(
                "info",
                "new_event",
                "New event.",
                Some(&json!({"token": "secret-token-value"})),
            )
            .await
            .expect("record new event");

        let old_ts = Utc::now()
            .checked_sub_signed(Duration::days(10))
            .expect("old timestamp")
            .to_rfc3339();
        sqlx::query("UPDATE events SET created_at = ?1 WHERE event_type = 'old_event';")
            .bind(old_ts)
            .execute(&store.pool)
            .await
            .expect("backdate old event");

        let report = store.run_maintenance(7).await.expect("run maintenance");
        assert_eq!(report.log_retention_days, 7);
        assert_eq!(report.deleted_events, 1);

        let (events, _) = store.recent_events(10, None).await.expect("recent events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "new_event");
        let detail = serde_json::to_string(&events[0].detail).expect("event detail");
        assert!(detail.contains("[redacted sensitive value]"), "{detail}");
        assert!(!detail.contains("secret-token-value"), "{detail}");

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn maintenance_compacts_existing_structurally_large_request_payloads() {
        let path = temp_db_path("maintenance-structural-large");
        let store = Store::open(&path).await.expect("open store");
        let items = (0..4_000)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("legacy small item {index:04} {}", "q".repeat(256))
                    }]
                })
            })
            .collect::<Vec<_>>();
        let raw_input = json!({ "input": items });
        let raw_input_json = serde_json::to_string(&raw_input).expect("raw input json");
        assert!(raw_input_json.len() > MAX_STORAGE_JSON_BYTES);
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO requests
              (id, previous_response_id, status, model, input_json, created_at, updated_at)
            VALUES (?1, NULL, 'completed', 'deepseek-v4-pro', ?2, ?3, ?3);
            "#,
        )
        .bind("resp_legacy_structural")
        .bind(&raw_input_json)
        .bind(now)
        .execute(&store.pool)
        .await
        .expect("insert legacy structural request");

        let report = store.run_maintenance(7).await.expect("run maintenance");
        assert_eq!(report.sanitized_requests, 1);

        let stored_json: String =
            sqlx::query_scalar("SELECT input_json FROM requests WHERE id = ?1;")
                .bind("resp_legacy_structural")
                .fetch_one(&store.pool)
                .await
                .expect("stored input");
        assert!(stored_json.len() <= MAX_STORAGE_JSON_BYTES, "{stored_json}");
        assert!(stored_json.contains("array items omitted"), "{stored_json}");
        assert!(
            stored_json.contains("legacy small item 0000"),
            "{stored_json}"
        );
        assert!(
            stored_json.contains("legacy small item 3999"),
            "{stored_json}"
        );

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn maintenance_sanitizes_multiple_large_request_batches() {
        let path = temp_db_path("maintenance-multiple-batches");
        let store = Store::open(&path).await.expect("open store");
        let row_count = usize::try_from(MAINTENANCE_REQUEST_BATCH).unwrap() + 2;
        let large_text = "r".repeat(MAINTENANCE_LARGE_FIELD_BYTES as usize + 1024);
        let now = Utc::now().to_rfc3339();

        for index in 0..row_count {
            let raw_input = json!({
                "input": [{
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("legacy batch row {index:04} {large_text}")
                    }]
                }]
            });
            sqlx::query(
                r#"
                INSERT INTO requests
                  (id, previous_response_id, status, model, input_json, created_at, updated_at)
                VALUES (?1, NULL, 'completed', 'deepseek-v4-pro', ?2, ?3, ?3);
                "#,
            )
            .bind(format!("resp_legacy_batch_{index:04}"))
            .bind(serde_json::to_string(&raw_input).expect("raw input json"))
            .bind(&now)
            .execute(&store.pool)
            .await
            .expect("insert legacy batch request");
        }

        let report = store.run_maintenance(7).await.expect("run maintenance");
        assert_eq!(report.sanitized_requests, row_count as u64);
        assert!(report.request_sanitize_batches >= 2);
        assert!(!report.request_sanitize_limit_reached);

        let remaining_large: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM requests
            WHERE length(input_json) > ?1;
            "#,
        )
        .bind(MAINTENANCE_LARGE_FIELD_BYTES)
        .fetch_one(&store.pool)
        .await
        .expect("remaining large count");
        assert_eq!(remaining_large, 0);

        remove_temp_db(store, path).await;
    }

    #[tokio::test]
    async fn maintenance_sanitizes_existing_large_request_payloads() {
        let path = temp_db_path("maintenance-large-requests");
        let store = Store::open(&path).await.expect("open store");
        let raw_input = json!({
            "api_key": "old-secret-key",
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("legacy screenshot data:image/jpeg;base64,{}", "B".repeat(300_000))
                }]
            }]
        });
        let raw_response = json!({
            "secret": "old-response-secret",
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "y".repeat(300_000)}]
            }]
        });
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO requests
              (id, previous_response_id, status, model, input_json, response_json, created_at, updated_at)
            VALUES (?1, NULL, 'completed', 'deepseek-v4-pro', ?2, ?3, ?4, ?4);
            "#,
        )
        .bind("resp_legacy_large")
        .bind(serde_json::to_string(&raw_input).expect("raw input json"))
        .bind(serde_json::to_string(&raw_response).expect("raw response json"))
        .bind(now)
        .execute(&store.pool)
        .await
        .expect("insert legacy large request");

        let report = store.run_maintenance(7).await.expect("run maintenance");
        assert_eq!(report.sanitized_requests, 1);

        let chain = store
            .response_context_chain("resp_legacy_large", 10)
            .await
            .expect("response context chain");
        assert_eq!(chain.len(), 1);
        let stored = serde_json::to_string(&chain[0]).expect("stored response json");
        assert!(stored.contains("[inline-data-url omitted"), "{stored}");
        assert!(stored.contains("[truncated chars="), "{stored}");
        assert!(stored.contains("[redacted sensitive value]"), "{stored}");
        assert!(!stored.contains("old-secret-key"), "{stored}");
        assert!(!stored.contains("old-response-secret"), "{stored}");
        assert!(!stored.contains(&"B".repeat(2048)), "{stored}");
        assert!(
            stored.len()
                < serde_json::to_string(&raw_input).unwrap().len()
                    + serde_json::to_string(&raw_response).unwrap().len(),
            "{stored}"
        );

        remove_temp_db(store, path).await;
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("codeseex-next-{label}-{nanos}.db"))
    }

    async fn remove_temp_db(store: Store, path: std::path::PathBuf) {
        store.close().await;
        drop(store);
        let wal = temp_db_sidecar_path(&path, "-wal");
        let shm = temp_db_sidecar_path(&path, "-shm");
        remove_temp_file(path).await;
        remove_temp_file(wal).await;
        remove_temp_file(shm).await;
    }

    async fn remove_temp_file(path: std::path::PathBuf) {
        for _ in 0..5 {
            match std::fs::remove_file(&path) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => tokio::time::sleep(StdDuration::from_millis(20)).await,
            }
        }
        let _ = std::fs::remove_file(path);
    }

    fn temp_db_sidecar_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        std::path::PathBuf::from(value)
    }
}
