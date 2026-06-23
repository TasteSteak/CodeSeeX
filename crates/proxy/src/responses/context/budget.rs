use codeseex_core::context::redact_inline_data_urls;
use codeseex_core::models::{DEFAULT_CONTEXT_WINDOW, DEFAULT_EFFECTIVE_CONTEXT_PERCENT};
use codeseex_core::protocol::ChatMessage;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
const RESERVED_OUTPUT_TOKENS: u64 = 64_000;
const RESERVED_TOOL_DEFINITION_TOKENS: u64 = 32_000;
const BYTES_PER_TOKEN_ESTIMATE: u64 = 4;
const BUDGET_TOOL_CONTENT_CHARS: usize = 512 * 1024;
const BUDGET_MESSAGE_CONTENT_CHARS: usize = 192 * 1024;
const BUDGET_REASONING_CHARS: usize = 64 * 1024;
const CODEX_FULL_CONTEXT_REPLAY_BUDGET_TOKENS: u64 = 96_000;
const CODEX_FULL_CONTEXT_PROTECTED_USER_BYTES: u64 = 8 * 1024;
const CODEX_FULL_CONTEXT_PROTECTED_USER_TOTAL_BYTES: u64 = 64 * 1024;

pub(crate) struct BudgetedContextMessages {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) diagnostic: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetMode {
    Standard,
    CodexFullContextReplay,
}

impl BudgetMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::CodexFullContextReplay => "codex_full_context_replay",
        }
    }
}

pub(crate) fn budget_messages_for_upstream(
    messages: Vec<ChatMessage>,
    protected_start_index: usize,
    mode: BudgetMode,
) -> BudgetedContextMessages {
    let max_bytes = upstream_context_budget_bytes(mode);
    let initial_bytes = messages_json_bytes(&messages);
    if initial_bytes <= max_bytes {
        return BudgetedContextMessages {
            messages,
            diagnostic: json!({
                "triggered": false,
                "mode": mode.label(),
                "max_bytes": max_bytes,
                "initial_bytes": initial_bytes,
                "final_bytes": initial_bytes,
                "dropped_blocks": 0,
                "compacted_messages": 0
            }),
        };
    }

    let compacted = messages
        .iter()
        .map(compact_message_for_budget)
        .collect::<Vec<_>>();
    let compacted_bytes = messages_json_bytes(&compacted);
    if compacted_bytes <= max_bytes {
        let compacted_messages = count_changed_messages(&messages, &compacted);
        return BudgetedContextMessages {
            messages: compacted,
            diagnostic: json!({
                "triggered": true,
                "mode": mode.label(),
                "max_bytes": max_bytes,
                "initial_bytes": initial_bytes,
                "final_bytes": compacted_bytes,
                "dropped_blocks": 0,
                "compacted_messages": compacted_messages
            }),
        };
    }

    let selected =
        select_budgeted_message_blocks(compacted, protected_start_index, max_bytes, mode);
    let final_bytes = messages_json_bytes(&selected.messages);
    let compacted_messages = count_changed_messages(&messages, &selected.messages);
    BudgetedContextMessages {
        messages: selected.messages,
        diagnostic: json!({
            "triggered": true,
            "mode": mode.label(),
            "max_bytes": max_bytes,
            "initial_bytes": initial_bytes,
            "compacted_bytes": compacted_bytes,
            "final_bytes": final_bytes,
            "dropped_blocks": selected.dropped_blocks,
            "compacted_messages": compacted_messages
        }),
    }
}

struct SelectedMessages {
    pub(crate) messages: Vec<ChatMessage>,
    dropped_blocks: usize,
}

fn select_budgeted_message_blocks(
    messages: Vec<ChatMessage>,
    protected_start_index: usize,
    max_bytes: u64,
    mode: BudgetMode,
) -> SelectedMessages {
    let mut selected_indexes = BTreeSet::new();
    let protected_user_indexes =
        protected_codex_full_context_user_indexes(&messages, mode, max_bytes);
    for (index, message) in messages.iter().enumerate() {
        if message.role == "system"
            || index >= protected_start_index
            || is_protected_context_message(message)
            || protected_user_indexes.contains(&index)
        {
            selected_indexes.insert(index);
        }
    }
    protect_parent_tool_calls_for_selected_results(&messages, &mut selected_indexes);
    let mut total_bytes = selected_indexes
        .iter()
        .filter_map(|index| messages.get(*index))
        .map(chat_message_json_bytes)
        .sum::<u64>();
    let mut dropped_blocks = 0;
    let blocks = message_blocks(&messages);

    for block in blocks.iter().rev() {
        if block
            .indexes
            .iter()
            .any(|index| selected_indexes.contains(index))
        {
            for index in &block.indexes {
                selected_indexes.insert(*index);
            }
            continue;
        }
        let block_bytes = block
            .indexes
            .iter()
            .filter_map(|index| messages.get(*index))
            .map(chat_message_json_bytes)
            .sum::<u64>();
        if total_bytes + block_bytes <= max_bytes || selected_indexes.is_empty() {
            for index in &block.indexes {
                selected_indexes.insert(*index);
            }
            total_bytes += block_bytes;
        } else {
            dropped_blocks += 1;
        }
    }

    SelectedMessages {
        messages: messages
            .into_iter()
            .enumerate()
            .filter_map(|(index, message)| selected_indexes.contains(&index).then_some(message))
            .collect(),
        dropped_blocks,
    }
}

fn protect_parent_tool_calls_for_selected_results(
    messages: &[ChatMessage],
    selected_indexes: &mut BTreeSet<usize>,
) {
    let selected_tool_result_ids = selected_indexes
        .iter()
        .filter_map(|index| messages.get(*index))
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.as_deref())
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    if selected_tool_result_ids.is_empty() {
        return;
    }

    for (index, message) in messages.iter().enumerate() {
        let Some(calls) = &message.tool_calls else {
            continue;
        };
        if calls.iter().any(|call| {
            call.get("id")
                .and_then(Value::as_str)
                .map(|id| selected_tool_result_ids.contains(id))
                .unwrap_or(false)
        }) {
            selected_indexes.insert(index);
        }
    }
}

struct MessageBlock {
    indexes: Vec<usize>,
}

fn message_blocks(messages: &[ChatMessage]) -> Vec<MessageBlock> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == "assistant" {
            if let Some(calls) = &message.tool_calls {
                let expected = calls
                    .iter()
                    .filter_map(|call| call.get("id").and_then(Value::as_str))
                    .collect::<HashSet<_>>();
                if !expected.is_empty() {
                    let mut indexes = vec![index];
                    while index + 1 < messages.len()
                        && messages[index + 1].role == "tool"
                        && messages[index + 1]
                            .tool_call_id
                            .as_deref()
                            .map(|id| expected.contains(id))
                            .unwrap_or(false)
                    {
                        index += 1;
                        indexes.push(index);
                    }
                    blocks.push(MessageBlock { indexes });
                    index += 1;
                    continue;
                }
            }
        }
        blocks.push(MessageBlock {
            indexes: vec![index],
        });
        index += 1;
    }
    blocks
}

fn is_protected_context_message(message: &ChatMessage) -> bool {
    message
        .content
        .starts_with("Verified CodeSeeX tool execution facts")
        || message
            .content
            .starts_with("Recovered CodeSeeX compaction summary")
        || message.content.starts_with("CodeSeeX compacted")
}

fn protected_codex_full_context_user_indexes(
    messages: &[ChatMessage],
    mode: BudgetMode,
    max_bytes: u64,
) -> HashSet<usize> {
    let mut indexes = HashSet::new();
    if mode != BudgetMode::CodexFullContextReplay {
        return indexes;
    }

    let total_cap = CODEX_FULL_CONTEXT_PROTECTED_USER_TOTAL_BYTES.min(max_bytes / 2);
    let mut total_bytes = 0_u64;
    for (index, message) in messages.iter().enumerate().rev() {
        if message.role != "user" {
            continue;
        }
        let bytes = chat_message_json_bytes(message);
        if bytes > CODEX_FULL_CONTEXT_PROTECTED_USER_BYTES {
            continue;
        }
        if total_bytes.saturating_add(bytes) > total_cap {
            continue;
        }
        indexes.insert(index);
        total_bytes = total_bytes.saturating_add(bytes);
    }
    indexes
}

fn compact_message_for_budget(message: &ChatMessage) -> ChatMessage {
    let mut next = message.clone();
    let content_limit = if next.role == "tool" {
        BUDGET_TOOL_CONTENT_CHARS
    } else {
        BUDGET_MESSAGE_CONTENT_CHARS
    };
    next.content = truncate_for_budget(&redact_inline_data_urls(&next.content), content_limit);
    if next.tool_calls.is_none() {
        next.reasoning_content = None;
    } else if let Some(reasoning) = &next.reasoning_content {
        next.reasoning_content = Some(truncate_for_budget(reasoning, BUDGET_REASONING_CHARS));
    }
    next
}

fn truncate_for_budget(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!(
        "{prefix}...[truncated chars={} bytes={}]",
        text.chars().count(),
        text.len()
    )
}

pub(crate) fn upstream_context_budget_bytes(mode: BudgetMode) -> u64 {
    match mode {
        BudgetMode::Standard => {
            let effective_tokens = DEFAULT_CONTEXT_WINDOW
                .saturating_mul(u64::from(DEFAULT_EFFECTIVE_CONTEXT_PERCENT))
                / 100;
            effective_tokens
                .saturating_sub(RESERVED_OUTPUT_TOKENS)
                .saturating_sub(RESERVED_TOOL_DEFINITION_TOKENS)
                .saturating_mul(BYTES_PER_TOKEN_ESTIMATE)
                .max(64 * 1024)
        }
        BudgetMode::CodexFullContextReplay => CODEX_FULL_CONTEXT_REPLAY_BUDGET_TOKENS
            .saturating_mul(BYTES_PER_TOKEN_ESTIMATE)
            .max(64 * 1024),
    }
}

pub(crate) fn messages_json_bytes(messages: &[ChatMessage]) -> u64 {
    serde_json::to_vec(messages)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}

fn chat_message_json_bytes(message: &ChatMessage) -> u64 {
    serde_json::to_vec(message)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}

fn count_changed_messages(left: &[ChatMessage], right: &[ChatMessage]) -> usize {
    let len = left.len().min(right.len());
    let changed = (0..len)
        .filter(|index| {
            serde_json::to_value(&left[*index]).ok() != serde_json::to_value(&right[*index]).ok()
        })
        .count();
    changed + left.len().max(right.len()) - len
}

pub(crate) fn estimate_tokens_from_messages(messages: &[ChatMessage]) -> u64 {
    messages
        .iter()
        .map(|message| estimate_tokens_from_text(&message.content))
        .sum()
}

pub(crate) fn estimate_tokens_from_text(text: &str) -> u64 {
    let chars = text.chars().count();
    u64::try_from(chars.max(1).div_ceil(4)).unwrap_or(1)
}
