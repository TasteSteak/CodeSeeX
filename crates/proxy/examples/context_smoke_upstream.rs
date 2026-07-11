use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

#[derive(Clone)]
struct AppState {
    payload_file: PathBuf,
    balance_file: PathBuf,
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let payload_file = env::var("PAYLOAD_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".private/context-smoke-upstream-payload.json"));
    let balance_file = env::var("BALANCE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".private/context-smoke-balance-request.json"));
    let port = env::var("FAKE_UPSTREAM_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8892);
    let state = Arc::new(AppState {
        payload_file,
        balance_file,
        port,
    });

    let app = Router::new()
        .route("/web-fixture", get(web_fixture))
        .route("/user/balance", get(balance))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;
    println!("fake-upstream-ready:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn web_fixture() -> Html<&'static str> {
    Html("<!doctype html><html><head><title>CodeSeeX Web Fixture</title><script>window.noise = true;</script></head><body><main><h1>CodeSeeX Web Fixture</h1><p>WEB_SEARCH_FIXTURE_OK text evidence.</p></main></body></html>")
}

async fn balance(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let request = json!({
        "path": "/user/balance",
        "authorization": redact_secret_text(&authorization)
    });
    if let Err(error) = write_json(&state.balance_file, &request) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "balance_write_failed",
            error.to_string(),
        );
    }
    json_response(json!({
        "is_available": true,
        "balance_infos": [
            {
                "currency": "CNY",
                "total_balance": "8.8",
                "granted_balance": "1.2",
                "topped_up_balance": "7.6"
            }
        ]
    }))
}

async fn chat_completions(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    body: Bytes,
) -> Response {
    let parsed: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid_json", error.to_string());
        }
    };
    if let Err(error) = write_json(
        &state.payload_file,
        &json!({
            "path": uri.path(),
            "trace": payload_trace(&parsed)
        }),
    ) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "payload_write_failed",
            error.to_string(),
        );
    }

    let model = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(
        model,
        "deepseek-v4-pro" | "deepseek-v4-flash" | "unknown-codex-model"
    ) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "unsupported_model",
            format!("unsupported model {model}"),
        );
    }

    if has_duplicate_tool_names(&parsed) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "duplicate_tools",
            "Tool names must be unique.",
        );
    }

    let messages = parsed
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_user_text = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content"))
        .map(content_to_text)
        .unwrap_or_default();

    if last_user_text.contains("force upstream failure") {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "forced_failure",
            "forced smoke failure",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_external_smoke_add") {
        let ok = content.contains("42");
        return assistant_response(
            &parsed,
            if ok {
                "external-tool-result-ok"
            } else {
                "external-tool-result-missing"
            },
            "chatcmpl_context_smoke_external_tool_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_community_smoke") {
        let ok = content.contains("community_smoke")
            && content.contains("\"sum\":42")
            && content.contains("\"mode\":\"fast\"");
        return assistant_response(
            &parsed,
            if ok {
                "community-tool-ok"
            } else {
                "community-tool-missing-result"
            },
            "chatcmpl_context_smoke_community_tool_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_list_directory") {
        let ok = content.contains("Cargo.toml");
        return assistant_response(
            &parsed,
            if ok {
                "tool-loop-ok"
            } else {
                "tool-loop-missing-cargo"
            },
            "chatcmpl_context_smoke_list_directory_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_read_file_range") {
        let ok = content.contains("[workspace]") || content.contains("members");
        return assistant_response(
            &parsed,
            if ok {
                "read-tool-ok"
            } else {
                "read-tool-missing-content"
            },
            "chatcmpl_context_smoke_read_file_range_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_data_url_read") {
        let ok = content.contains("[inline-data-url omitted")
            && !content.contains("AAAAAAAAAABBBBBBBBBB");
        return assistant_response(
            &parsed,
            if ok {
                "data-url-redacted-ok"
            } else {
                "data-url-leaked"
            },
            "chatcmpl_context_smoke_data_url_read_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_workspace_search") {
        let ok = content.contains("CodeSeeX");
        return assistant_response(
            &parsed,
            if ok {
                "search-tool-ok"
            } else {
                "search-tool-missing-content"
            },
            "chatcmpl_context_smoke_workspace_search_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_web_search") {
        let ok = content.contains("WEB_SEARCH_FIXTURE_OK") && !content.contains("window.noise");
        return assistant_response(
            &parsed,
            if ok {
                "web-search-tool-ok"
            } else {
                "web-search-tool-missing-content"
            },
            "chatcmpl_context_smoke_web_search_final",
        );
    }

    if let Some(content) = tool_message_content(&messages, "call_apply_patch") {
        let ok = (content.contains("\"ok\":true") || content.contains("Success."))
            && content.contains("apply-patch-smoke.txt");
        return assistant_response(
            &parsed,
            if ok {
                "apply-patch-tool-ok"
            } else {
                "apply-patch-tool-failed"
            },
            "chatcmpl_context_smoke_apply_patch_final",
        );
    }

    if last_user_text.contains("call list_directory tool") {
        return tool_call_response(
            &parsed,
            "call_list_directory",
            "list_directory",
            json!({
                "path": ".",
                "depth": 0
            }),
        );
    }
    if last_user_text.contains("call disabled read tool") {
        return tool_call_response(
            &parsed,
            "call_disabled_read_file_range",
            "read_file_range",
            json!({
                "path": "Cargo.toml",
                "start": 1,
                "count": 2
            }),
        );
    }
    if last_user_text.contains("call read_file_range tool") {
        return tool_call_response(
            &parsed,
            "call_read_file_range",
            "read_file_range",
            json!({
                "path": "Cargo.toml",
                "start": 1,
                "count": 8
            }),
        );
    }
    if last_user_text.contains("call data_url read_file_range tool") {
        return tool_call_response(
            &parsed,
            "call_data_url_read",
            "read_file_range",
            json!({
                "path": "fixtures/data-url-smoke.txt",
                "start": 1,
                "count": 2
            }),
        );
    }
    if last_user_text.contains("call workspace_search tool") {
        return tool_call_response(
            &parsed,
            "call_workspace_search",
            "workspace_search",
            json!({
                "query": "CodeSeeX",
                "path": "README.md",
                "max_results": 5
            }),
        );
    }
    if last_user_text.contains("call web_search tool") {
        return tool_call_response(
            &parsed,
            "call_web_search",
            "web_search",
            json!({
                "mode": "open",
                "url": format!("http://127.0.0.1:{}/web-fixture", state.port)
            }),
        );
    }
    if last_user_text.contains("call apply_patch tool") {
        let patch = [
            "*** Begin Patch",
            "*** Update File: fixtures/apply-patch-smoke.txt",
            "@@",
            "-before",
            "+after",
            "*** End Patch",
        ]
        .join("\n");
        return tool_call_response(
            &parsed,
            "call_apply_patch",
            "apply_patch",
            json!({
                "patch": patch
            }),
        );
    }
    if last_user_text.contains("call external mcp smoke tool") {
        if !has_tool(&parsed, "smoke_add") {
            return assistant_response(
                &parsed,
                "external-tool-not-forwarded",
                "chatcmpl_context_smoke_external_tool_missing",
            );
        }
        return tool_call_response(
            &parsed,
            "call_external_smoke_add",
            "smoke_add",
            json!({
                "a": 21,
                "b": 21
            }),
        );
    }
    if last_user_text.contains("call community smoke tool") {
        if !has_tool(&parsed, "community_smoke") {
            return assistant_response(
                &parsed,
                "community-tool-not-forwarded",
                "chatcmpl_context_smoke_community_tool_missing",
            );
        }
        return tool_call_response(
            &parsed,
            "call_community_smoke",
            "community_smoke",
            json!({
                "a": 21,
                "b": 21
            }),
        );
    }

    if parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return sse_response(format!(
            "data: {}\r\n\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({ "choices": [{ "delta": { "content": "stream-" } }] }),
            json!({
                "choices": [{ "delta": { "content": "ok" } }],
                "usage": { "prompt_tokens": 11, "completion_tokens": 2, "total_tokens": 13 }
            })
        ));
    }

    json_response(json!({
        "id": "chatcmpl_context_smoke",
        "object": "chat.completion",
        "created": now_seconds(),
        "model": "deepseek-v4-pro",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": "smoke-ok" },
                "finish_reason": "stop"
            }
        ],
        "usage": { "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12 }
    }))
}

fn has_duplicate_tool_names(parsed: &Value) -> bool {
    let mut names = std::collections::BTreeSet::new();
    for tool in parsed
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = tool.pointer("/function/name").and_then(Value::as_str) else {
            continue;
        };
        if !names.insert(name.to_owned()) {
            return true;
        }
    }
    false
}

fn has_tool(parsed: &Value, expected: &str) -> bool {
    parsed
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|tool| tool.pointer("/function/name").and_then(Value::as_str) == Some(expected))
}

fn tool_message_content(messages: &[Value], call_id: &str) -> Option<String> {
    messages
        .iter()
        .find(|message| {
            message.get("role").and_then(Value::as_str) == Some("tool")
                && message.get("tool_call_id").and_then(Value::as_str) == Some(call_id)
        })
        .and_then(|message| message.get("content"))
        .map(content_to_text)
}

fn content_to_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn assistant_response(parsed: &Value, content: &str, id: &str) -> Response {
    if parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return sse_response(format!(
            "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({
                "id": id,
                "choices": [{ "delta": { "content": content } }],
                "usage": { "prompt_tokens": 15, "completion_tokens": 3, "total_tokens": 18 }
            })
        ));
    }
    json_response(json!({
        "id": id,
        "object": "chat.completion",
        "created": now_seconds(),
        "model": "deepseek-v4-pro",
        "choices": [
            {
                "index": 0,
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }
        ],
        "usage": { "prompt_tokens": 15, "completion_tokens": 3, "total_tokens": 18 }
    }))
}

fn tool_call_response(parsed: &Value, call_id: &str, name: &str, args: Value) -> Response {
    if parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return sse_response(format!(
            "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            json!({
                "id": format!("chatcmpl_context_smoke_{name}_call"),
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": call_id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": args.to_string()
                                    }
                                }
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }
                ],
                "usage": { "prompt_tokens": 12, "completion_tokens": 1, "total_tokens": 13 }
            })
        ));
    }
    json_response(json!({
        "id": format!("chatcmpl_context_smoke_{name}_call"),
        "object": "chat.completion",
        "created": now_seconds(),
        "model": "deepseek-v4-pro",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [
                        {
                            "id": call_id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": args.to_string()
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }
        ],
        "usage": { "prompt_tokens": 12, "completion_tokens": 1, "total_tokens": 13 }
    }))
}

fn json_response(value: Value) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        value.to_string(),
    )
        .into_response()
}

fn sse_response(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

fn error_response(
    status: StatusCode,
    code: impl Into<String>,
    message: impl Into<String>,
) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        json!({
            "error": {
                "code": code.into(),
                "message": message.into()
            }
        })
        .to_string(),
    )
        .into_response()
}

fn write_json(path: &PathBuf, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, value.to_string())
}

fn payload_trace(payload: &Value) -> Value {
    let serialized = serde_json::to_vec(payload).unwrap_or_default();
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let roles = messages
        .iter()
        .filter_map(|message| message.get("role").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let tool_names = payload
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let has_tool_result = messages
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"));
    let payload_hash = Sha256::digest(&serialized)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    json!({
        "model": payload.get("model").and_then(Value::as_str),
        "stream": payload.get("stream").and_then(Value::as_bool).unwrap_or(false),
        "message_count": messages.len(),
        "message_roles": roles,
        "tool_names": tool_names,
        "has_tool_result": has_tool_result,
        "payload_bytes": serialized.len(),
        "estimated_input_tokens": serialized.len() / 4,
        "payload_hash": payload_hash
    })
}

fn redact_secret_text(value: &str) -> String {
    if value.to_ascii_lowercase().contains("data:") && value.contains(";base64,") {
        return "[redacted inline data url]".to_owned();
    }
    if value.to_ascii_lowercase().starts_with("bearer ") {
        return "Bearer [redacted]".to_owned();
    }
    value.to_owned()
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
