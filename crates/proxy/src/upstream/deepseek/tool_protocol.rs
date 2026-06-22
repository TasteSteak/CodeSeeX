use crate::tools::ownership::ChatToolCall;
use serde_json::{Map, Value};
use uuid::Uuid;

const DSML_START: &str = concat!(
    "<",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "tool_calls>"
);
const DSML_END: &str = concat!(
    "</",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "tool_calls>"
);
const INVOKE_OPEN: &str = concat!(
    "<",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "invoke"
);
const INVOKE_CLOSE: &str = concat!(
    "</",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "invoke>"
);
const PARAM_OPEN: &str = concat!(
    "<",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "parameter"
);
const PARAM_CLOSE: &str = concat!(
    "</",
    "\u{ff5c}\u{ff5c}",
    "DSML",
    "\u{ff5c}\u{ff5c}",
    "parameter>"
);

#[derive(Debug, Default)]
pub(crate) struct DeepSeekStreamToolAdapter {
    buffer: String,
}

#[derive(Debug, Default)]
pub(crate) struct DeepSeekToolContent {
    pub(crate) visible_text: String,
    pub(crate) tool_calls: Vec<ChatToolCall>,
    pub(crate) blocked: bool,
    pub(crate) parse_failed: bool,
}

#[derive(Debug, Default)]
pub(crate) struct DeepSeekChatToolProtocolAdaptation {
    pub(crate) adapted_tool_names: Vec<String>,
    pub(crate) blocked_channels: Vec<&'static str>,
    pub(crate) parse_failed_channels: Vec<&'static str>,
}

impl DeepSeekChatToolProtocolAdaptation {
    pub(crate) fn changed(&self) -> bool {
        !self.adapted_tool_names.is_empty()
            || !self.blocked_channels.is_empty()
            || !self.parse_failed_channels.is_empty()
    }
}

pub(crate) fn adapt_chat_tool_protocol(
    chat: &mut Value,
    allow_tool_calls: bool,
) -> DeepSeekChatToolProtocolAdaptation {
    let Some(message) = chat.pointer_mut("/choices/0/message") else {
        return DeepSeekChatToolProtocolAdaptation::default();
    };
    let mut adaptation = DeepSeekChatToolProtocolAdaptation::default();
    let mut calls = Vec::new();

    adapt_message_channel(
        message,
        "content",
        "content",
        allow_tool_calls,
        &mut calls,
        &mut adaptation,
    );
    adapt_message_channel(
        message,
        "reasoning_content",
        "reasoning",
        allow_tool_calls,
        &mut calls,
        &mut adaptation,
    );

    if !calls.is_empty() {
        let tool_calls = message
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut)
            .map(std::mem::take)
            .unwrap_or_default();
        let mut output = tool_calls;
        for call in calls {
            adaptation.adapted_tool_names.push(call.name.clone());
            output.push(chat_tool_call_to_value(call));
        }
        message["tool_calls"] = Value::Array(output);
    }

    adaptation
}

fn adapt_message_channel(
    message: &mut Value,
    field: &'static str,
    channel: &'static str,
    allow_tool_calls: bool,
    calls: &mut Vec<ChatToolCall>,
    adaptation: &mut DeepSeekChatToolProtocolAdaptation,
) {
    let Some(text) = message
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    if !looks_like_dsml_protocol(&text) {
        return;
    }

    let mut adapter = DeepSeekStreamToolAdapter::default();
    let mut output = adapter.push(&text, allow_tool_calls);
    let finish = adapter.finish(allow_tool_calls);
    output.visible_text.push_str(&finish.visible_text);
    output.tool_calls.extend(finish.tool_calls);
    output.blocked |= finish.blocked;
    output.parse_failed |= finish.parse_failed;

    message[field] = Value::String(output.visible_text);
    if output.blocked {
        adaptation.blocked_channels.push(channel);
    }
    if output.parse_failed {
        adaptation.parse_failed_channels.push(channel);
    }
    calls.extend(output.tool_calls);
}

fn chat_tool_call_to_value(call: ChatToolCall) -> Value {
    serde_json::json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.arguments
        }
    })
}

impl DeepSeekStreamToolAdapter {
    pub(crate) fn push(&mut self, content: &str, allow_tool_calls: bool) -> DeepSeekToolContent {
        self.buffer.push_str(content);
        let mut output = DeepSeekToolContent::default();

        loop {
            let Some(start) = self.buffer.find(DSML_START) else {
                let keep_from = dsml_prefix_start(&self.buffer).unwrap_or(self.buffer.len());
                output.visible_text.push_str(&self.buffer[..keep_from]);
                self.buffer.drain(..keep_from);
                return output;
            };
            output.visible_text.push_str(&self.buffer[..start]);
            self.buffer.drain(..start);

            let Some(end_start) = self.buffer.find(DSML_END) else {
                return output;
            };
            let block_end = end_start + DSML_END.len();
            let block = self.buffer[..block_end].to_owned();
            self.buffer.drain(..block_end);

            if allow_tool_calls {
                let parsed = parse_dsml_tool_calls(&block);
                if parsed.is_empty() {
                    output.parse_failed = true;
                } else {
                    output.tool_calls.extend(parsed);
                }
            } else {
                output.blocked = true;
            }
        }
    }

    pub(crate) fn finish(&mut self, allow_tool_calls: bool) -> DeepSeekToolContent {
        let mut output = DeepSeekToolContent::default();
        if self.buffer.is_empty() {
            return output;
        }
        let pending = std::mem::take(&mut self.buffer);
        if looks_like_dsml_protocol(&pending) {
            if allow_tool_calls {
                let parsed = parse_dsml_tool_calls(&pending);
                if parsed.is_empty() {
                    output.parse_failed = true;
                } else {
                    output.tool_calls.extend(parsed);
                }
            } else {
                output.blocked = true;
            }
        } else {
            output.visible_text.push_str(&pending);
        }
        output
    }
}

fn blocked_dsml_prefix_start(buffer: &str) -> Option<usize> {
    buffer
        .find("<\u{ff5c}\u{ff5c}DSML")
        .or_else(|| buffer.find("</\u{ff5c}\u{ff5c}DSML"))
}

fn looks_like_dsml_protocol(value: &str) -> bool {
    blocked_dsml_prefix_start(value).is_some()
}

fn parse_dsml_tool_calls(block: &str) -> Vec<ChatToolCall> {
    let mut calls = Vec::new();
    let mut cursor = 0_usize;
    while let Some(relative_start) = block[cursor..].find(INVOKE_OPEN) {
        let start = cursor + relative_start;
        let Some(header_end_relative) = block[start..].find('>') else {
            break;
        };
        let header_end = start + header_end_relative;
        let header = &block[start..=header_end];
        let Some(name) = attribute_value(header, "name") else {
            cursor = header_end + 1;
            continue;
        };
        let body_start = header_end + 1;
        let Some(close_relative) = block[body_start..].find(INVOKE_CLOSE) else {
            break;
        };
        let body_end = body_start + close_relative;
        let body = &block[body_start..body_end];
        let arguments = parse_dsml_arguments(body);
        calls.push(ChatToolCall {
            id: format!("call_{}", Uuid::new_v4().simple()),
            name,
            arguments: arguments.to_string(),
        });
        cursor = body_end + INVOKE_CLOSE.len();
    }
    calls
}

fn parse_dsml_arguments(body: &str) -> Value {
    let mut object = Map::new();
    let mut cursor = 0_usize;
    while let Some(relative_start) = body[cursor..].find(PARAM_OPEN) {
        let start = cursor + relative_start;
        let Some(header_end_relative) = body[start..].find('>') else {
            break;
        };
        let header_end = start + header_end_relative;
        let header = &body[start..=header_end];
        let body_start = header_end + 1;
        let Some(close_relative) = body[body_start..].find(PARAM_CLOSE) else {
            break;
        };
        let body_end = body_start + close_relative;
        if let Some(name) = attribute_value(header, "name") {
            let raw_value = decode_dsml_text(&body[body_start..body_end]);
            object.insert(name, parse_parameter_value(header, raw_value.trim()));
        }
        cursor = body_end + PARAM_CLOSE.len();
    }
    Value::Object(object)
}

fn parse_parameter_value(header: &str, raw_value: &str) -> Value {
    if attribute_value(header, "string").as_deref() == Some("true") {
        return Value::String(raw_value.to_owned());
    }
    serde_json::from_str::<Value>(raw_value).unwrap_or_else(|_| Value::String(raw_value.to_owned()))
}

fn attribute_value(header: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = header.find(&needle)? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(decode_dsml_text(&header[start..end]))
}

fn decode_dsml_text(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn dsml_prefix_start(buffer: &str) -> Option<usize> {
    buffer
        .char_indices()
        .skip(1)
        .map(|(index, _)| index)
        .chain(std::iter::once(buffer.len()))
        .find(|&index| DSML_START.starts_with(&buffer[index..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dsml_tag(tag: &str) -> String {
        format!("<\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}{tag}>")
    }

    fn dsml_close(tag: &str) -> String {
        format!("</\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}{tag}>")
    }

    #[test]
    fn parses_dsml_web_search_block_from_content() {
        let mut adapter = DeepSeekStreamToolAdapter::default();
        let content = format!(
            "before {}\n{} name=\"web_search\">\n{} name=\"mode\" string=\"true\">open{}\n{} name=\"queries\" string=\"false\">[\"DeepSeek API documentation vision image support 2025 2026\"]{}\n{}\n{} after",
            dsml_tag("tool_calls"),
            dsml_tag("invoke").trim_end_matches('>'),
            dsml_tag("parameter").trim_end_matches('>'),
            dsml_close("parameter"),
            dsml_tag("parameter").trim_end_matches('>'),
            dsml_close("parameter"),
            dsml_close("invoke"),
            dsml_close("tool_calls")
        );
        let chunk = adapter.push(&content, true);

        assert_eq!(chunk.visible_text, "before  after");
        assert_eq!(chunk.tool_calls.len(), 1);
        assert_eq!(chunk.tool_calls[0].name, "web_search");
        let args = serde_json::from_str::<Value>(&chunk.tool_calls[0].arguments).unwrap();
        assert_eq!(args["mode"], Value::String("open".to_owned()));
        assert_eq!(
            args["queries"][0],
            Value::String("DeepSeek API documentation vision image support 2025 2026".to_owned())
        );

        let finish = adapter.finish(true);
        assert!(finish.visible_text.is_empty());
    }

    #[test]
    fn buffers_split_dsml_prefix_until_complete() {
        let mut adapter = DeepSeekStreamToolAdapter::default();
        let first = adapter.push("hello <\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}tool", true);
        assert_eq!(first.visible_text, "hello ");
        assert!(first.tool_calls.is_empty());

        let content = format!(
            "_calls>{} name=\"read_file_range\">{} name=\"path\" string=\"true\">README.md{}{}{}",
            dsml_tag("invoke").trim_end_matches('>'),
            dsml_tag("parameter").trim_end_matches('>'),
            dsml_close("parameter"),
            dsml_close("invoke"),
            dsml_close("tool_calls")
        );
        let second = adapter.push(&content, true);
        assert!(second.visible_text.is_empty());
        assert_eq!(second.tool_calls.len(), 1);
        assert_eq!(second.tool_calls[0].name, "read_file_range");
    }

    #[test]
    fn blocks_dsml_when_tool_calls_are_not_allowed() {
        let mut adapter = DeepSeekStreamToolAdapter::default();
        let content = format!(
            "{}{} name=\"web_search\">{}{}",
            dsml_tag("tool_calls"),
            dsml_tag("invoke").trim_end_matches('>'),
            dsml_close("invoke"),
            dsml_close("tool_calls")
        );
        let chunk = adapter.push(&content, false);
        assert!(chunk.visible_text.is_empty());
        assert!(chunk.blocked);
        assert!(chunk.tool_calls.is_empty());
    }

    #[test]
    fn blocks_truncated_dsml_protocol_on_finish() {
        let mut adapter = DeepSeekStreamToolAdapter::default();
        let chunk = adapter.push("visible <\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}tool", false);
        assert_eq!(chunk.visible_text, "visible ");

        let finish = adapter.finish(false);
        assert!(finish.visible_text.is_empty());
        assert!(finish.blocked);
    }

    #[test]
    fn adapts_non_streaming_content_dsml_to_standard_tool_call() {
        let mut chat = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": format!(
                        "{}{} name=\"web_search\">{} name=\"mode\" string=\"true\">search{}{}{}",
                        dsml_tag("tool_calls"),
                        dsml_tag("invoke").trim_end_matches('>'),
                        dsml_tag("parameter").trim_end_matches('>'),
                        dsml_close("parameter"),
                        dsml_close("invoke"),
                        dsml_close("tool_calls")
                    )
                }
            }]
        });

        let adaptation = adapt_chat_tool_protocol(&mut chat, true);

        assert_eq!(adaptation.adapted_tool_names, vec!["web_search"]);
        assert_eq!(
            chat.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("")
        );
        assert_eq!(
            chat.pointer("/choices/0/message/tool_calls/0/function/name")
                .and_then(Value::as_str),
            Some("web_search")
        );
    }

    #[test]
    fn blocks_non_streaming_reasoning_dsml_when_tools_are_not_allowed() {
        let mut chat = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "final text",
                    "reasoning_content": format!(
                        "{}{} name=\"web_search\">{}{}",
                        dsml_tag("tool_calls"),
                        dsml_tag("invoke").trim_end_matches('>'),
                        dsml_close("invoke"),
                        dsml_close("tool_calls")
                    )
                }
            }]
        });

        let adaptation = adapt_chat_tool_protocol(&mut chat, false);

        assert_eq!(adaptation.blocked_channels, vec!["reasoning"]);
        assert_eq!(
            chat.pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some("final text")
        );
        assert_eq!(
            chat.pointer("/choices/0/message/reasoning_content")
                .and_then(Value::as_str),
            Some("")
        );
        assert!(chat.pointer("/choices/0/message/tool_calls").is_none());
    }
}
