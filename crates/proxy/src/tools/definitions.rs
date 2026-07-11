use codeseex_core::catalog::APPLY_PATCH_TOOL_PARAMETER_DESCRIPTION;
use serde_json::{json, Value};
use std::collections::HashSet;

const CODEX_NATIVE_TOOL_IDS: &[&str] = &["apply_patch"];
const CODESEEX_SYSTEM_HOSTED_TOOL_IDS: &[&str] = &["web_search"];
const CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS: &[&str] = &[
    "list_directory",
    "read_file_range",
    "workspace_search",
    "vision_analyze",
    "image_gen",
];
const DEFAULT_CONFIGURABLE_HOSTED_TOOL_IDS: &[&str] = &[
    "list_directory",
    "read_file_range",
    "workspace_search",
    "vision_analyze",
];

pub fn upstream_tool_definitions(enabled_ids: &[String]) -> Vec<Value> {
    let enabled = enabled_set(enabled_ids);
    CODEX_NATIVE_TOOL_IDS
        .iter()
        .chain(CODESEEX_SYSTEM_HOSTED_TOOL_IDS.iter())
        .chain(
            CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS
                .iter()
                .filter(|id| configurable_tool_enabled(id, &enabled)),
        )
        .filter_map(|id| tool_definition(id))
        .collect()
}

pub fn is_executable_tool_enabled(name: &str, enabled_ids: &[String]) -> bool {
    let name = canonical_tool_id(name);
    CODESEEX_SYSTEM_HOSTED_TOOL_IDS.contains(&name)
        || (CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS.contains(&name)
            && enabled_ids.iter().any(|id| {
                let enabled_id = canonical_tool_id(id.as_str());
                enabled_id == name || (name == "image_gen" && enabled_id == "vision_analyze")
            }))
}

pub fn is_known_code_tool(name: &str) -> bool {
    let name = canonical_tool_id(name);
    CODESEEX_SYSTEM_HOSTED_TOOL_IDS.contains(&name)
        || CODESEEX_CONFIGURABLE_HOSTED_TOOL_IDS.contains(&name)
}

pub fn default_enabled_tool_ids() -> Vec<String> {
    DEFAULT_CONFIGURABLE_HOSTED_TOOL_IDS
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
                "description": "Apply one native Codex patch. The patch must be one complete raw apply_patch document, not markdown or prose. It must start with *** Begin Patch and end with *** End Patch. It can add, update, delete, or move files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": APPLY_PATCH_TOOL_PARAMETER_DESCRIPTION
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
                "description": "Search the web for public information or open selected public HTTP/HTTPS pages. mode=\"search\" returns candidates and automatically opens top candidates into compact evidence; answer from evidence when sufficient instead of repeating search. Use mode=\"open\" with open_urls or open_ids only when a specific page needs more content. Local/private network targets and binary resources are blocked by default.",
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
                "description": "List files and folders in a compact, paginated page. Results use paths relative to the requested root. When has_more=true, call again with next_cursor. Metadata is omitted by default because names and paths are usually sufficient.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "depth": { "type": "integer" },
                        "include_files": { "type": "boolean" },
                        "include_dirs": { "type": "boolean" },
                        "max_entries": { "type": "integer", "description": "Page size. Defaults to 60 and is capped for stable output." },
                        "cursor": { "type": "integer", "description": "Use next_cursor from the preceding page. Omit for the first page." },
                        "include_metadata": { "type": "boolean", "description": "Include size, modified time, and readonly state only when needed." }
                    },
                    "additionalProperties": false
                }
            }
        })),
        "read_file_range" => Some(json!({
            "type": "function",
            "function": {
                "name": "read_file_range",
                "description": "Read a bounded UTF-8 text page from a file. Use start/end/count for a range or tail_lines for the end. whole_file=true is paginated rather than unbounded: follow next_start/next_column until truncated=false. start_column is a zero-based character offset for continuing one very long line. Returned credential values are redacted. Do not use this tool for images, media, archives, PDFs, executables, or other binary files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Text file path to read." },
                        "whole_file": { "type": "boolean", "description": "Start reading from the beginning and continue with next_start/next_column when the page is truncated." },
                        "start": { "type": "integer", "description": "1-based start line. Negative values count backward from EOF, so -50 starts 50 lines from the end. Negative indexes require a bounded full-file line count." },
                        "end": { "type": "integer", "description": "1-based inclusive end line. Negative values count backward from EOF. Prefer tail_lines when only the end of a file is needed." },
                        "count": { "type": "integer", "description": "Maximum number of lines to read from start." },
                        "tail_lines": { "type": "integer", "description": "Read the last N lines of the file." },
                        "start_column": { "type": "integer", "description": "Zero-based character offset for continuing a long line from next_column." }
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
                "description": "Search UTF-8 workspace text and return compact relative-path snippets. Literal search is the default; set regex=true only for Rust regex syntax. Broad searches are source-first, then include hidden or large directories by default while result/file/byte budgets remain. Inspect truncation_reasons and narrow path/include/query when a budget is reached. Returned credential values are redacted.",
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
                        "regex": { "type": "boolean", "description": "Treat query as a Rust regex pattern. Supports common regex syntax and inline flags like (?i), but not JavaScript-style lookaround or backreferences." },
                        "include_deferred_dirs": { "type": "boolean", "description": "When true (default), source-first search continues into low-priority directories such as .git, target, or very large directories until result/file limits are reached. Set false only when broad search speed matters more than exhaustive coverage." }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })),
        "vision_analyze" => Some(json!({
            "type": "function",
            "function": {
                "name": "vision_analyze",
                "description": "Use the configured Vision module endpoint to inspect one or more images. Pass the user's image reference directly in image/images when needed: HTTP(S) URL, data:image URL, file:// URL, workspace path, or local absolute path when full file access is active. If the current user message already contains an input_image, the tool can use it without extra file work. Do not convert local files to base64, copy files into the workspace, or use shell as an image transport. The tool reads local files, checks permissions, sends the image to the configured Vision endpoint, and returns prompt_sent plus text. The text field is the visual model's extracted answer; when the user directly asks about the image, answer from that text without inventing details or summarizing away specifics.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "Exact question or instruction to send to the visual model. If omitted, the tool uses the latest user request text when available." },
                        "image": { "type": "string", "description": "Single image reference. Use the user-provided HTTP(S), data:image, file://, workspace path, or allowed local absolute path unchanged." },
                        "images": { "type": "array", "items": { "type": "string" }, "description": "Multiple image references. Pass user-provided references unchanged." }
                    },
                    "additionalProperties": false
                }
            }
        })),
        "image_gen" => Some(json!({
            "type": "function",
            "function": {
                "name": "image_gen",
                "description": "Use the configured Vision module endpoint to generate images directly from text. Use this tool for image generation; do not read image files, skill files, or generated images first. If the Vision generation endpoint, generation model, or API key is not configured correctly, this tool returns a structured unavailable error. Returns prompt_sent exactly as sent, image URLs or local file paths, and images_markdown for direct display to the user. Return images_markdown instead of calling view_image for generated files; generated base64 is saved to disk and never returned inline.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "Exact image generation prompt to send." },
                        "size": { "type": "string", "description": "Image size such as 1024x1024. Defaults to 1024x1024." },
                        "n": { "type": "integer", "description": "Number of images to request. Defaults to 1." },
                        "quality": { "type": "string", "description": "Optional provider-specific quality value." },
                        "background": { "type": "string", "description": "Optional provider-specific background value." },
                        "output_format": { "type": "string", "description": "Optional output format such as png, jpeg, or webp." },
                        "response_format": { "type": "string", "description": "Optional provider-specific response format such as url or b64_json." },
                        "style": { "type": "string", "description": "Optional provider-specific style value." }
                    },
                    "required": ["prompt"],
                    "additionalProperties": false
                }
            }
        })),
        _ => None,
    }
}

fn configurable_tool_enabled(id: &str, enabled: &HashSet<&str>) -> bool {
    enabled.contains(id) || (id == "image_gen" && enabled.contains("vision_analyze"))
}

fn enabled_set(enabled_ids: &[String]) -> HashSet<&str> {
    enabled_ids
        .iter()
        .map(|id| canonical_tool_id(id.as_str()))
        .collect()
}

fn canonical_tool_id(id: &str) -> &str {
    match id {
        "vision_generate" | "imagegen" | "image_generation" | "generate_image"
        | "image_generate" | "create_image" => "image_gen",
        _ => id,
    }
}
