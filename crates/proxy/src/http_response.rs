use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, Response, StatusCode};
use codeseex_store::{RequestStatus, Store};
use futures_util::StreamExt;
use serde_json::json;
use serde_json::Value;

pub(crate) fn response_from_stream(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    body: Body,
) -> axum::response::Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn passthrough_stream_with_completion(
    response: reqwest::Response,
    store: Store,
    request_id: String,
    completion_diagnostic: Option<Value>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::try_stream! {
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut usage = Value::Null;
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    capture_stream_usage(&bytes, &mut buffer, &mut usage);
                    yield bytes;
                }
                Err(error) => {
                    let detail = json!({ "error": error.to_string() });
                    let _ = store.finish_request(&request_id, RequestStatus::Failed, None, Some(&detail)).await;
                    let _ = store
                        .record_event(
                            "error",
                            "request_failed",
                            "Streaming chat completion failed.",
                            Some(&json!({ "id": request_id, "error": error.to_string() })),
                        )
                        .await;
                    Err(std::io::Error::other(error))?;
                }
            }
        }
        capture_remaining_stream_usage(&buffer, &mut usage);
        let response = (!usage.is_null()).then(|| json!({ "usage": usage }));
        let _ = store
            .finish_request(
                &request_id,
                RequestStatus::Completed,
                response.as_ref(),
                completion_diagnostic.as_ref(),
            )
            .await;
        let _ = store
            .record_event(
                "info",
                "request_completed",
                "Streaming chat completion completed.",
                Some(&json!({
                    "id": request_id,
                    "lifecycle": completion_diagnostic
                        .as_ref()
                        .and_then(|value| value.get("codeseex_lifecycle"))
                        .and_then(Value::as_str)
                })),
            )
            .await;
    }
}

fn capture_stream_usage(bytes: &Bytes, buffer: &mut Vec<u8>, usage: &mut Value) {
    buffer.extend_from_slice(bytes);
    while let Some((index, delimiter_len)) = find_sse_frame_delimiter(buffer.as_slice()) {
        let frame = buffer.drain(..index).collect::<Vec<_>>();
        buffer.drain(..delimiter_len);
        capture_sse_frame_usage(&frame, usage);
    }
}

fn capture_remaining_stream_usage(buffer: &[u8], usage: &mut Value) {
    if !buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
        capture_sse_frame_usage(buffer, usage);
    }
}

fn capture_sse_frame_usage(frame: &[u8], usage: &mut Value) {
    let Ok(frame) = std::str::from_utf8(frame) else {
        return;
    };
    let data = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(data) else {
        return;
    };
    if let Some(next_usage) = parsed.get("usage") {
        *usage = next_usage.clone();
    }
}

fn find_sse_frame_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .enumerate()
        .find_map(|(index, window)| (window == b"\n\n").then_some((index, 2)));
    let crlf = buffer
        .windows(4)
        .enumerate()
        .find_map(|(index, window)| (window == b"\r\n\r\n").then_some((index, 4)));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

pub(crate) fn response_from_bytes(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    bytes: Vec<u8>,
) -> axum::response::Response {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY));
    if let Some(value) = content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn json_error(
    status: StatusCode,
    code: &str,
    message: String,
) -> axum::response::Response {
    json_response_with_status(
        status,
        json!({ "error": { "code": code, "message": message, "type": "api_error" } }),
    )
}

pub(crate) fn json_response(value: Value) -> axum::response::Response {
    json_response_with_status(StatusCode::OK, value)
}

pub(crate) fn json_response_with_status(
    status: StatusCode,
    value: Value,
) -> axum::response::Response {
    let bytes = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, response_content_type_json().unwrap())
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

pub(crate) fn response_content_type_json() -> Option<HeaderValue> {
    Some(HeaderValue::from_static("application/json; charset=utf-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Bytes;

    #[test]
    fn captures_stream_usage_from_lf_and_crlf_sse_frames() {
        let mut buffer = Vec::new();
        let mut usage = Value::Null;

        capture_stream_usage(
            &Bytes::from_static(
                br#"data: {"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}

"#,
            ),
            &mut buffer,
            &mut usage,
        );
        assert_eq!(usage["total_tokens"], 4);

        capture_stream_usage(
            &Bytes::from_static(
                b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\r\n\r\n",
            ),
            &mut buffer,
            &mut usage,
        );
        assert_eq!(usage["total_tokens"], 7);
        assert!(buffer.is_empty());
    }

    #[test]
    fn mixed_sse_delimiters_are_consumed_in_wire_order() {
        let mut buffer = Vec::new();
        let mut usage = Value::Null;
        capture_stream_usage(
            &Bytes::from_static(
                b"data: {\"usage\":{\"total_tokens\":3}}\r\n\r\ndata: {\"usage\":{\"total_tokens\":7}}\n\n",
            ),
            &mut buffer,
            &mut usage,
        );

        assert_eq!(usage["total_tokens"], 7);
        assert!(buffer.is_empty());
    }
}
