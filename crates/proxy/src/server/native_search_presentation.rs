//! Makes the searches CodeSeeX executes itself visible to the client.
//!
//! In the fully hosted path the search never leaves the proxy, so the client
//! used to receive only the final answer. This module folds the executed
//! searches into the terminal provider turn as ordinary `web_search_call` items,
//! which is the same shape Codex already renders for provider-hosted search.

use serde_json::{json, Value};

use crate::tools::native_web_search_call_item;

/// One search CodeSeeX ran on the client's behalf during this turn.
#[derive(Clone, Debug)]
pub(super) struct ExecutedSearch {
    pub(super) call_id: String,
    pub(super) input: String,
}

#[derive(Clone)]
struct Frame {
    event: Option<String>,
    payload: Option<Value>,
    raw: Vec<u8>,
}

impl Frame {
    fn from_bytes(bytes: &[u8]) -> Frame {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Frame {
                event: None,
                payload: None,
                raw: bytes.to_vec(),
            };
        };
        let event = text.lines().find_map(|line| {
            line.strip_prefix("event:")
                .map(|value| value.trim().to_owned())
        });
        let payload = text
            .lines()
            .find_map(|line| line.strip_prefix("data:").map(str::trim))
            .filter(|value| !value.is_empty())
            .and_then(|value| serde_json::from_str::<Value>(value).ok());
        Frame {
            event,
            payload,
            raw: bytes.to_vec(),
        }
    }

    fn synthetic(event: &str, payload: Value) -> Frame {
        Frame {
            event: Some(event.to_owned()),
            payload: Some(payload),
            raw: Vec::new(),
        }
    }

    fn render(&self, sequence: &mut u64) -> Vec<u8> {
        let Some(payload) = self.payload.as_ref() else {
            return self.raw.clone();
        };
        let mut payload = payload.clone();
        let event = self
            .event
            .clone()
            .or_else(|| {
                payload
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "message".to_owned());
        if payload.get("sequence_number").is_some() {
            *sequence += 1;
            payload["sequence_number"] = json!(*sequence);
        }
        let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
        format!("event: {event}\ndata: {data}\n\n").into_bytes()
    }
}

/// Folds `searches` into one terminal provider turn.
///
/// Returns the original body unchanged when there is nothing to present or the
/// turn shape is not one this transformation can do safely.
pub(super) fn present(
    frames: &[Vec<u8>],
    is_sse: bool,
    body: Vec<u8>,
    response_id: &str,
    searches: &[ExecutedSearch],
) -> Vec<u8> {
    if searches.is_empty() {
        return body;
    }
    if is_sse {
        present_sse(frames, response_id, searches).unwrap_or(body)
    } else {
        present_json(&body, searches).unwrap_or(body)
    }
}

fn present_sse(
    frames: &[Vec<u8>],
    response_id: &str,
    searches: &[ExecutedSearch],
) -> Option<Vec<u8>> {
    let mut parsed = frames
        .iter()
        .map(|bytes| Frame::from_bytes(bytes))
        .collect::<Vec<_>>();
    let completed = parsed.iter().position(|frame| {
        frame
            .payload
            .as_ref()
            .and_then(|payload| payload.get("type"))
            .and_then(Value::as_str)
            == Some("response.completed")
    })?;
    let items = searches
        .iter()
        .map(|search| native_web_search_call_item(&search.call_id, &search.input))
        .collect::<Vec<_>>();
    let shift = items.len() as u64;

    // Existing items move down by the number of injected search items, so every
    // event that references an output index has to move with them.
    for frame in parsed.iter_mut() {
        if let Some(payload) = frame.payload.as_mut() {
            shift_output_index(payload, shift);
        }
    }
    if let Some(payload) = parsed[completed].payload.as_mut() {
        prepend_completed_items(payload, &items);
    }

    // Announce the search before the provider's own items so the stream order
    // matches the item indices in the final response.
    let insertion = parsed
        .iter()
        .position(|frame| {
            frame
                .payload
                .as_ref()
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("response.created")
        })
        .map_or(0, |index| index + 1);
    let injected = search_event_frames(response_id, &items);
    let mut ordered = Vec::with_capacity(parsed.len() + injected.len());
    for (index, frame) in parsed.into_iter().enumerate() {
        if index == insertion {
            ordered.extend(injected.iter().cloned());
        }
        ordered.push(frame);
    }
    let mut sequence = 0_u64;
    let mut body = Vec::new();
    for frame in &ordered {
        body.extend_from_slice(&frame.render(&mut sequence));
    }
    Some(body)
}

fn present_json(body: &[u8], searches: &[ExecutedSearch]) -> Option<Vec<u8>> {
    let mut payload = serde_json::from_slice::<Value>(body).ok()?;
    let items = searches
        .iter()
        .map(|search| native_web_search_call_item(&search.call_id, &search.input))
        .collect::<Vec<_>>();
    let output = payload.get_mut("output").and_then(Value::as_array_mut)?;
    for (offset, item) in items.iter().enumerate() {
        output.insert(offset, item.clone());
    }
    serde_json::to_vec(&payload).ok()
}

fn search_event_frames(response_id: &str, items: &[Value]) -> Vec<Frame> {
    let mut frames = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let output_index = index as u64;
        let item_id = item.get("id").cloned().unwrap_or(Value::Null);
        let action = item.get("action").cloned().unwrap_or(Value::Null);
        let action_type = item.pointer("/action/type").and_then(Value::as_str);
        let progress = if matches!(action_type, Some("open") | Some("open_page")) {
            "response.web_search_call.opening"
        } else {
            "response.web_search_call.searching"
        };
        let mut added = item.clone();
        added["status"] = Value::String("in_progress".to_owned());
        frames.push(Frame::synthetic(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added",
                "response_id": response_id,
                "output_index": output_index,
                "item": added
            }),
        ));
        frames.push(Frame::synthetic(
            "response.web_search_call.in_progress",
            json!({
                "type": "response.web_search_call.in_progress",
                "response_id": response_id,
                "output_index": output_index,
                "item_id": item_id
            }),
        ));
        frames.push(Frame::synthetic(
            progress,
            json!({
                "type": progress,
                "response_id": response_id,
                "output_index": output_index,
                "item_id": item_id,
                "action": action
            }),
        ));
        frames.push(Frame::synthetic(
            "response.web_search_call.completed",
            json!({
                "type": "response.web_search_call.completed",
                "response_id": response_id,
                "output_index": output_index,
                "item_id": item_id
            }),
        ));
        frames.push(Frame::synthetic(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "response_id": response_id,
                "output_index": output_index,
                "item": item
            }),
        ));
    }
    frames
}

fn shift_output_index(payload: &mut Value, shift: u64) {
    if shift == 0 {
        return;
    }
    if let Some(index) = payload.get_mut("output_index") {
        if let Some(value) = index.as_u64() {
            *index = json!(value + shift);
        }
    }
}

fn prepend_completed_items(payload: &mut Value, items: &[Value]) {
    let Some(output) = payload
        .pointer_mut("/response/output")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for (offset, item) in items.iter().enumerate() {
        output.insert(offset, item.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sse(event: &str, payload: Value) -> Vec<u8> {
        format!("event: {event}\ndata: {payload}\n\n").into_bytes()
    }

    #[test]
    fn non_streaming_turn_gains_the_search_item() {
        let body = serde_json::to_vec(&json!({
            "id": "resp_1",
            "status": "completed",
            "output": [{ "type": "message", "id": "msg_1", "content": [] }]
        }))
        .unwrap();
        let presented = present(
            &[],
            false,
            body,
            "resp_1",
            &[ExecutedSearch {
                call_id: "call_1".to_owned(),
                input: r#"{"query":"rust"}"#.to_owned(),
            }],
        );
        let value: Value = serde_json::from_slice(&presented).unwrap();
        let output = value["output"].as_array().unwrap();

        assert_eq!(output[0]["type"], "web_search_call");
        assert_eq!(output[0]["call_id"], "call_1");
        assert_eq!(output[1]["type"], "message");
    }

    #[test]
    fn streaming_turn_emits_search_events_before_completion() {
        let frames = vec![
            sse(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": { "id": "resp_1", "status": "in_progress" },
                    "sequence_number": 1
                }),
            ),
            sse(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": { "type": "message", "id": "msg_1" },
                    "sequence_number": 3
                }),
            ),
            sse(
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": { "id": "resp_1", "output": [{ "type": "message", "id": "msg_1" }] },
                    "sequence_number": 4
                }),
            ),
        ];
        let presented = present(
            &frames,
            true,
            frames.concat(),
            "resp_1",
            &[ExecutedSearch {
                call_id: "call_1".to_owned(),
                input: r#"{"query":"rust"}"#.to_owned(),
            }],
        );
        let text = String::from_utf8(presented).unwrap();

        let created = text.find("response.created").unwrap();
        let added = text.find("response.web_search_call.in_progress").unwrap();
        let message_done = text.find("msg_1").unwrap();
        let completed = text.find("response.completed").unwrap();
        assert!(created < added, "the search follows response.created");
        assert!(
            added < message_done,
            "the search is announced before the answer it precedes"
        );
        assert!(
            added < completed,
            "search must be presented before completion"
        );
        // The original message item moved down by one and carries index 1.
        assert!(text.contains("\"output_index\":1"));
        // The completed response carries the search item first.
        let completed_line = text
            .lines()
            .find(|line| line.contains("\"response.completed\""))
            .unwrap();
        let payload: Value =
            serde_json::from_str(completed_line.trim_start_matches("data: ")).unwrap();
        assert_eq!(payload["response"]["output"][0]["type"], "web_search_call");
    }

    #[test]
    fn no_searches_leave_the_body_untouched() {
        let frames = vec![sse(
            "response.completed",
            json!({ "type": "response.completed" }),
        )];
        let body = frames.concat();
        let presented = present(&frames, true, body.clone(), "resp_1", &[]);

        assert_eq!(presented, body);
    }
}
