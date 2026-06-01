use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{sqlite::SqliteConnectOptions, Row, SqlitePool};
use std::collections::HashSet;
use std::path::Path;

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
pub struct StoredResponse {
    pub id: String,
    pub previous_response_id: Option<String>,
    pub status: RequestStatus,
    pub input: Value,
    pub response: Value,
    pub turn_messages: Vec<Value>,
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
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
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
        .bind(serde_json::to_string(input)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
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
                input: serde_json::from_str(&input_json).unwrap_or(Value::Null),
                response: response_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or(Value::Null),
                turn_messages: turn_messages_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_default(),
                tool_facts: tool_facts_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
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

        let mut facts = row
            .and_then(|row| {
                row.try_get::<Option<String>, _>("tool_facts_json")
                    .ok()
                    .flatten()
            })
            .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
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
        .bind(serde_json::to_string(&facts)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            bail!("request '{id}' was not found while appending tool facts");
        }
        Ok(())
    }

    pub async fn replace_request_turn_messages(&self, id: &str, messages: &[Value]) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET turn_messages_json = ?2,
                updated_at = ?3
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(serde_json::to_string(messages)?)
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

        let mut existing = row
            .and_then(|row| {
                row.try_get::<Option<String>, _>("turn_messages_json")
                    .ok()
                    .flatten()
            })
            .and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
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
        sqlx::query(
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
        .bind(response.map(serde_json::to_string).transpose()?)
        .bind(diagnostic.map(serde_json::to_string).transpose()?)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_request_diagnostic(&self, id: &str, diagnostic: &Value) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE requests
            SET diagnostic_json = ?2,
                updated_at = ?3
            WHERE id = ?1;
            "#,
        )
        .bind(id)
        .bind(serde_json::to_string(diagnostic)?)
        .bind(now)
        .execute(&self.pool)
        .await?;
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
        .bind(detail.map(serde_json::to_string).transpose()?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

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

        let _ = std::fs::remove_file(path);
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

        let _ = std::fs::remove_file(path);
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

        let _ = std::fs::remove_file(path);
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

        let _ = std::fs::remove_file(path);
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

        let _ = std::fs::remove_file(path);
    }

    fn temp_db_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("codeseex-next-{label}-{nanos}.db"))
    }
}
