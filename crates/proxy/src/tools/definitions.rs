use serde_json::{json, Value};
use std::collections::HashSet;

const CODEX_NATIVE_TOOL_IDS: &[&str] = &["apply_patch"];
const CODESEEX_SYSTEM_HOSTED_TOOL_IDS: &[&str] = &["web_search"];
const CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS: &[&str] =
    &["list_directory", "read_file_range", "workspace_search"];

pub fn upstream_tool_definitions(enabled_ids: &[String]) -> Vec<Value> {
    let enabled = enabled_set(enabled_ids);
    CODEX_NATIVE_TOOL_IDS
        .iter()
        .chain(CODESEEX_SYSTEM_HOSTED_TOOL_IDS.iter())
        .chain(
            CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS
                .iter()
                .filter(|id| enabled.contains(**id)),
        )
        .filter_map(|id| tool_definition(id))
        .collect()
}

pub fn is_executable_tool_enabled(name: &str, enabled_ids: &[String]) -> bool {
    CODESEEX_SYSTEM_HOSTED_TOOL_IDS.contains(&name)
        || (CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS.contains(&name)
            && enabled_ids.iter().any(|id| id == name))
}

pub fn is_known_code_tool(name: &str) -> bool {
    CODESEEX_SYSTEM_HOSTED_TOOL_IDS.contains(&name)
        || CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS.contains(&name)
}

pub fn default_enabled_tool_ids() -> Vec<String> {
    CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS
        .iter()
        .map(|id| (*id).to_owned())
        .collect()
}

fn tool_definition(id: &str) -> Option<Value> {
    codex_native_tool_definition(id)
        .or_else(|| codeseex_system_hosted_tool_definition(id))
        .or_else(|| codeseex_configurable_hosted_tool_definition(id))
}

fn codex_native_tool_definition(id: &str) -> Option<Value> {
    match id {
        "apply_patch" => Some(json!({
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply one native Codex patch. The patch can add, update, delete, or move files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": "Raw apply_patch text. Use exactly one patch document: a single *** Begin Patch, then one or more *** Add File / *** Update File / *** Delete File operations for all files, then a single *** End Patch. For move/rename, use *** Update File: old path followed immediately by *** Move to: new path before hunks. Standard unified hunk headers like @@ -10,2 +10,3 @@ are accepted and normalized to native Codex @@ headers."
                        }
                    },
                    "required": ["patch"],
                    "additionalProperties": false
                }
            }
        })),
        _ => None,
    }
}

fn codeseex_system_hosted_tool_definition(id: &str) -> Option<Value> {
    match id {
        "web_search" => Some(json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for public information or open selected public HTTP/HTTPS pages. Use mode=\"search\" with query/queries, then mode=\"open\" with open_urls or open_ids when page content is needed. Returns compact text-only evidence; local/private network targets and binary resources are blocked by default.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["search", "open"] },
                        "type": { "type": "string", "enum": ["search", "open"] },
                        "query": { "type": "string" },
                        "q": { "type": "string" },
                        "queries": { "type": "array", "items": { "type": "string" } },
                        "search_query": {
                            "oneOf": [
                                { "type": "string" },
                                { "type": "array", "items": { "type": "string" } }
                            ]
                        },
                        "url": { "type": "string" },
                        "urls": { "type": "array", "items": { "type": "string" } },
                        "open_urls": { "type": "array", "items": { "type": "string" } },
                        "id": { "type": "string" },
                        "ids": { "type": "array", "items": { "type": "string" } },
                        "open_ids": { "type": "array", "items": { "type": "string" } },
                        "max_results": { "type": "integer" }
                    },
                    "additionalProperties": true
                }
            }
        })),
        _ => None,
    }
}

fn codeseex_configurable_hosted_tool_definition(id: &str) -> Option<Value> {
    match id {
        "list_directory" => Some(json!({
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "List files and folders.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "depth": { "type": "integer" },
                        "include_files": { "type": "boolean" },
                        "include_dirs": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }
            }
        })),
        "read_file_range" => Some(json!({
            "type": "function",
            "function": {
                "name": "read_file_range",
                "description": "Read text from a file. Use whole_file=true for the full file, tail_lines for the last N lines, or start/end/count for a range.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path to read." },
                        "whole_file": { "type": "boolean", "description": "Read the entire file when true. Still limited by the maximum file byte size." },
                        "start": { "type": "integer", "description": "1-based start line. Negative values count backward from EOF, so -50 starts 50 lines from the end." },
                        "end": { "type": "integer", "description": "1-based end line. Negative values count backward from EOF." },
                        "count": { "type": "integer", "description": "Maximum number of lines to read from start." },
                        "tail_lines": { "type": "integer", "description": "Read the last N lines of the file." }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })),
        "workspace_search" => Some(json!({
            "type": "function",
            "function": {
                "name": "workspace_search",
                "description": "Search text files and return compact path, line, and snippet matches. Literal search is the default; set regex=true only when a Rust regex pattern is needed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Literal text by default. When regex=true, this must be a Rust regex pattern, not JavaScript/PCRE-only syntax." },
                        "path": { "type": "string", "description": "Directory or file path to search." },
                        "include": {
                            "description": "Glob-style file filters such as \"src/*.rs\" or [\"*.ts\", \"*.tsx\"].",
                            "oneOf": [
                                { "type": "string" },
                                { "type": "array", "items": { "type": "string" } }
                            ]
                        },
                        "exclude": {
                            "description": "Glob-style path filters to skip, such as \"target\" or [\"node_modules\", \"*.lock\"].",
                            "oneOf": [
                                { "type": "string" },
                                { "type": "array", "items": { "type": "string" } }
                            ]
                        },
                        "max_results": { "type": "integer" },
                        "context_lines": { "type": "integer" },
                        "case_sensitive": { "type": "boolean" },
                        "regex": { "type": "boolean", "description": "Treat query as a Rust regex pattern. Supports common regex syntax and inline flags like (?i), but not JavaScript-style lookaround or backreferences." }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })),
        _ => None,
    }
}

fn enabled_set(enabled_ids: &[String]) -> HashSet<&str> {
    enabled_ids.iter().map(String::as_str).collect()
}
