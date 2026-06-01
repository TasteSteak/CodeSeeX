use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<Value>, content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            reasoning_content: Some(String::new()),
        }
    }

    pub fn assistant_tool_calls_with_reasoning(
        tool_calls: Vec<Value>,
        content: impl Into<String>,
        reasoning_content: impl Into<String>,
    ) -> Self {
        Self {
            role: "assistant".to_owned(),
            content: content.into(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            reasoning_content: Some(reasoning_content.into()),
        }
    }

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_owned(),
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
            reasoning_content: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub input: Value,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}
