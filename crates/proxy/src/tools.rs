use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(crate) mod chat_protocol;
pub(crate) mod coordinator;
pub(crate) mod definitions;
pub(crate) mod diagnostics;
pub(crate) mod hosted;
pub(crate) mod ownership;
pub(crate) mod permissions;
pub(crate) mod registry;
pub(crate) mod response_items;
pub(crate) mod vision;
pub(crate) mod web;

pub use definitions::{
    default_enabled_tool_ids, is_executable_tool_enabled, is_known_code_tool,
    upstream_tool_definitions,
};
pub(crate) use permissions::ToolPermissionContext as ToolExecutionContext;
use permissions::{ResolvedToolPath, ToolPermissionError};

const MAX_DEPTH: usize = 4;
const DEFAULT_DIRECTORY_PAGE_SIZE: usize = 60;
const MAX_DIRECTORY_PAGE_SIZE: usize = 100;
const MAX_DIRECTORY_SCAN_ENTRIES: usize = 4_096;
const MAX_READ_LINES: usize = 220;
const MAX_READ_SCAN_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_READ_TEXT_CHARS: usize = 7_000;
const MAX_SEARCH_FILES: usize = 800;
const MAX_SEARCH_RESULTS: usize = 80;
const MAX_SEARCH_FILE_BYTES: u64 = 1_048_576;
const MAX_SEARCH_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_SEARCH_SNIPPET_CHARS: usize = 1_000;
const MAX_TOOL_RESULT_JSON_CHARS: usize = 9_000;
const SEARCH_LARGE_DIR_ENTRY_THRESHOLD: usize = 120;
const MAX_DEFERRED_DIRS_REPORTED: usize = 8;

#[cfg(test)]
pub fn execute_tool(name: &str, arguments: &str) -> Value {
    execute_tool_in_context(&ToolExecutionContext::default(), name, arguments)
}

pub(crate) fn execute_tool_in_context(
    context: &ToolExecutionContext,
    name: &str,
    arguments: &str,
) -> Value {
    let args = match parse_arguments(arguments) {
        Ok(value) => value,
        Err(error) => return error,
    };
    match name {
        "list_directory" => list_directory(context, &args),
        "read_file_range" => read_file_range(context, &args),
        "workspace_search" => workspace_search(context, &args),
        _ => json!({
            "ok": false,
            "error": "unsupported_tool",
            "message": format!("CodeSeeX does not execute tool '{name}'.")
        }),
    }
}

pub async fn execute_tool_with_client(
    client: &reqwest::Client,
    config: &codeseex_core::AppConfig,
    context: &ToolExecutionContext,
    messages: &[Value],
    current_image_refs: &[String],
    name: &str,
    arguments: &str,
) -> Value {
    if name == "web_search" {
        let args = match parse_arguments(arguments) {
            Ok(value) => value,
            Err(error) => return error,
        };
        return web::execute(client, config.network_proxy, &args, messages).await;
    }
    if name == vision::ANALYZE_TOOL_NAME {
        let args = match parse_arguments(arguments) {
            Ok(value) => value,
            Err(error) => return error,
        };
        return vision::execute(client, config, context, messages, current_image_refs, &args).await;
    }
    if name == vision::GENERATE_TOOL_NAME || name == vision::GENERATE_ALIAS_TOOL_NAME {
        let args = match parse_arguments(arguments) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let tool_name = if name == vision::GENERATE_ALIAS_TOOL_NAME {
            vision::GENERATE_ALIAS_TOOL_NAME
        } else {
            vision::GENERATE_TOOL_NAME
        };
        return vision::execute_generate(client, config, tool_name, &args).await;
    }
    execute_tool_in_context(context, name, arguments)
}

fn parse_arguments(arguments: &str) -> Result<Value, Value> {
    serde_json::from_str(arguments).map_err(|error| {
        json!({
            "ok": false,
            "error": "invalid_arguments",
            "message": format!("Tool arguments must be valid JSON: {error}")
        })
    })
}

fn resolve_inside_workspace(
    context: &ToolExecutionContext,
    value: &Value,
) -> Result<ResolvedToolPath, Value> {
    let raw = value
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    context.resolve_path(raw).map_err(permission_error_to_value)
}

fn permission_error_to_value(error: ToolPermissionError) -> Value {
    match error {
        ToolPermissionError::WorkspaceRootNotConfigured => json!({
            "ok": false,
            "error": "workspace_root_not_configured",
            "message": "No Codex workspace root was available for this request. CodeSeeX will not guess from the proxy process directory.",
        }),
        ToolPermissionError::PathOutsideWorkspace { path } => path_outside_workspace(&path),
    }
}

fn path_outside_workspace(raw: &str) -> Value {
    json!({
        "ok": false,
        "error": "path_outside_workspace",
        "message": "Path must stay inside the authorized workspace unless full file access is active.",
        "path": raw
    })
}

fn list_directory(context: &ToolExecutionContext, args: &Value) -> Value {
    let resolved = match resolve_inside_workspace(context, args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let path = resolved.path;
    let relative = tool_display_path(&path, &resolved.display_root);
    let Ok(metadata) = fs::metadata(&path) else {
        return json!({ "ok": false, "error": "not_found", "path": relative });
    };
    if !metadata.is_dir() {
        return json!({
            "ok": false,
            "error": "not_directory",
            "message": "Use read_file_range for files.",
            "path": relative
        });
    }
    let depth = usize_arg(args, "depth", 0, 0, MAX_DEPTH);
    let include_files = bool_arg(args, "include_files", true);
    let include_dirs = bool_arg(args, "include_dirs", true);
    let include_metadata = bool_arg(args, "include_metadata", false);
    let cursor = usize_arg(args, "cursor", 0, 0, usize::MAX);
    let page_size = usize_arg(
        args,
        "max_entries",
        DEFAULT_DIRECTORY_PAGE_SIZE,
        1,
        MAX_DIRECTORY_PAGE_SIZE,
    );
    let mut state = DirectoryPageState::new(cursor, page_size, include_metadata);
    let root = path.canonicalize().unwrap_or_else(|_| path.clone());
    walk_directory_page(
        &path,
        &relative,
        0,
        depth,
        include_files,
        include_dirs,
        &root,
        &mut state,
    );
    // A numeric cursor is only valid while the same bounded traversal can
    // advance it. At the scan ceiling, returning one would repeat this page.
    let has_more = state.has_more
        && !state.scan_limit_reached
        && !state.output_budget_reached
        && !state.entries.is_empty();
    let next_cursor = has_more.then(|| cursor.saturating_add(state.entries.len()));
    fit_tool_result(json!({
        "ok": true,
        "path": relative,
        "depth": depth,
        "cursor": cursor,
        "next_cursor": next_cursor,
        "has_more": has_more,
        "scan_limit_reached": state.scan_limit_reached,
        "output_budget_reached": state.output_budget_reached,
        "entries": state.entries,
        "summary": {
            "returned": state.entries.len(),
            "scanned": state.scanned,
            "metadata_included": include_metadata
        },
        "truncated": has_more || state.scan_limit_reached || state.output_budget_reached,
        "truncation_reason": if state.output_budget_reached {
            Some("output_budget")
        } else if state.scan_limit_reached {
            Some("scan_limit")
        } else if has_more {
            Some("page_limit")
        } else {
            None
        }
    }))
}

fn read_file_range(context: &ToolExecutionContext, args: &Value) -> Value {
    let resolved = match resolve_inside_workspace(context, args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let path = resolved.path;
    let relative = tool_display_path(&path, &resolved.display_root);
    let Ok(metadata) = fs::metadata(&path) else {
        return json!({ "ok": false, "error": "not_found", "path": relative });
    };
    if !metadata.is_file() {
        return json!({ "ok": false, "error": "not_file", "path": relative });
    }
    if is_known_binary_file(&path) {
        return json!({
            "ok": false,
            "error": "binary_file_not_supported",
            "message": "read_file_range only reads UTF-8 text files. Use the appropriate image, media, archive, or document tool for this file.",
            "path": relative
        });
    }
    match file_has_binary_markers(&path) {
        Ok(true) => {
            return json!({
                "ok": false,
                "error": "binary_file_not_supported",
                "message": "read_file_range only reads UTF-8 text files. Binary-looking content was detected.",
                "path": relative
            });
        }
        Ok(false) => {}
        Err(_) => {
            return json!({ "ok": false, "error": "read_failed", "path": relative });
        }
    }
    let whole_file = bool_arg(args, "whole_file", false);
    let start_column = usize_arg(args, "start_column", 0, 0, usize::MAX);
    let request = match resolve_read_line_request(&path, args, whole_file) {
        Ok(request) => request,
        Err(ReadPageError::ScanLimitReached) => {
            return json!({
                "ok": false,
                "error": "file_scan_limit_exceeded",
                "path": relative,
                "bytes": metadata.len(),
                "max_scanned_bytes": MAX_READ_SCAN_BYTES,
                "message": "The requested negative line range requires scanning more than the safe file-read budget. Use tail_lines on a smaller file or a nearer positive line range."
            });
        }
        Err(ReadPageError::ReadFailed) => {
            return json!({ "ok": false, "error": "read_failed", "path": relative });
        }
        Err(ReadPageError::NotUtf8Text) => {
            return json!({ "ok": false, "error": "not_utf8_text", "path": relative });
        }
    };
    let page = if request.line_limit == 0 {
        Ok(ReadPage {
            start: request.start,
            end: request.start.saturating_sub(1),
            next_start: None,
            next_column: None,
            has_bom: false,
            text: String::new(),
            truncated: false,
            truncation_reason: None,
        })
    } else {
        read_forward_page(&path, request.start, request.line_limit, start_column)
    };
    let page = match page {
        Ok(page) => page,
        Err(ReadPageError::ScanLimitReached) => {
            return json!({
                "ok": false,
                "error": "file_scan_limit_exceeded",
                "path": relative,
                "bytes": metadata.len(),
                "max_scanned_bytes": MAX_READ_SCAN_BYTES,
                "message": "The requested line window requires scanning more than the safe file-read budget. Use a nearer positive start line or a smaller file."
            });
        }
        Err(ReadPageError::ReadFailed) => {
            return json!({ "ok": false, "error": "read_failed", "path": relative });
        }
        Err(ReadPageError::NotUtf8Text) => {
            return json!({ "ok": false, "error": "not_utf8_text", "path": relative });
        }
    };
    let (text, redacted_sensitive_values) = redact_sensitive_text(&page.text);
    fit_tool_result(json!({
        "ok": true,
        "path": relative,
        "start": page.start,
        "end": page.end,
        "start_column": start_column,
        "next_start": page.next_start,
        "next_column": page.next_column,
        "whole_file_requested": whole_file,
        "total_lines": Value::Null,
        "total_lines_known": false,
        "has_bom": page.has_bom,
        "text": text,
        "redacted_sensitive_values": redacted_sensitive_values,
        "truncated": page.truncated,
        "truncation_reason": page.truncation_reason
    }))
}

#[derive(Debug, Default)]
struct DirectoryPageState {
    skip: usize,
    limit: usize,
    include_metadata: bool,
    scanned: usize,
    entries: Vec<Value>,
    has_more: bool,
    scan_limit_reached: bool,
    output_budget_reached: bool,
}

impl DirectoryPageState {
    fn new(skip: usize, limit: usize, include_metadata: bool) -> Self {
        Self {
            skip,
            limit,
            include_metadata,
            ..Self::default()
        }
    }

    fn push(&mut self, value: Value) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return true;
        }
        if self.entries.len() >= self.limit {
            self.has_more = true;
            return false;
        }
        self.entries.push(value);
        true
    }
}

#[derive(Debug)]
struct ReadPage {
    start: usize,
    end: usize,
    next_start: Option<usize>,
    next_column: Option<usize>,
    has_bom: bool,
    text: String,
    truncated: bool,
    truncation_reason: Option<&'static str>,
}

#[derive(Debug)]
enum ReadPageError {
    ScanLimitReached,
    ReadFailed,
    NotUtf8Text,
}

struct ReadLineRequest {
    start: usize,
    line_limit: usize,
}

fn resolve_read_line_request(
    path: &Path,
    args: &Value,
    whole_file: bool,
) -> Result<ReadLineRequest, ReadPageError> {
    let explicit_start = args.get("start").and_then(Value::as_i64);
    if whole_file && explicit_start.is_none() {
        return Ok(ReadLineRequest {
            start: 1,
            line_limit: MAX_READ_LINES,
        });
    }

    let start_value = explicit_start.unwrap_or(1);
    let end_value = args.get("end").and_then(Value::as_i64);
    let tail_lines = args
        .get("tail_lines")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map(|value| value.clamp(1, MAX_READ_LINES));
    let requires_total_lines =
        tail_lines.is_some() || start_value < 0 || end_value.is_some_and(|value| value < 0);
    let total_lines = requires_total_lines
        .then(|| count_file_lines(path))
        .transpose()?
        .unwrap_or(0);

    let (start, requested_end) = if let Some(tail_lines) = tail_lines {
        (
            total_lines.saturating_sub(tail_lines).saturating_add(1),
            total_lines,
        )
    } else {
        let start = if requires_total_lines {
            normalize_line_index(start_value, total_lines)
        } else {
            usize::try_from(start_value).unwrap_or(1).max(1)
        };
        let end = if let Some(end_value) = end_value {
            if requires_total_lines {
                normalize_line_index(end_value, total_lines)
            } else {
                usize::try_from(end_value).unwrap_or(usize::MAX).max(1)
            }
        } else if let Some(count) = args
            .get("count")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        {
            start
                .saturating_add(count.clamp(1, MAX_READ_LINES))
                .saturating_sub(1)
        } else if requires_total_lines {
            total_lines
        } else {
            start.saturating_add(MAX_READ_LINES).saturating_sub(1)
        };
        (start, end)
    };

    let line_limit = requested_end
        .checked_sub(start)
        .and_then(|value| value.checked_add(1))
        .unwrap_or(0)
        .min(MAX_READ_LINES);
    Ok(ReadLineRequest { start, line_limit })
}

fn count_file_lines(path: &Path) -> Result<usize, ReadPageError> {
    let file = fs::File::open(path).map_err(|_| ReadPageError::ReadFailed)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut total_lines = 0_usize;
    let mut scanned_bytes = 0_u64;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::InvalidData => ReadPageError::NotUtf8Text,
                _ => ReadPageError::ReadFailed,
            })?;
        if read == 0 {
            return Ok(total_lines);
        }
        scanned_bytes = scanned_bytes.saturating_add(read as u64);
        if scanned_bytes > MAX_READ_SCAN_BYTES {
            return Err(ReadPageError::ScanLimitReached);
        }
        total_lines = total_lines.saturating_add(1);
    }
}

fn normalize_line_index(value: i64, total_lines: usize) -> usize {
    let max_line = total_lines.max(1);
    if value < 0 {
        let offset = usize::try_from(value.unsigned_abs()).unwrap_or(usize::MAX);
        return total_lines
            .saturating_sub(offset)
            .saturating_add(1)
            .clamp(1, max_line);
    }
    usize::try_from(value)
        .unwrap_or(max_line)
        .clamp(1, max_line)
}

fn file_has_binary_markers(path: &Path) -> Result<bool, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut sample = vec![0_u8; 4_096];
    let read = file.read(&mut sample)?;
    sample.truncate(read);
    Ok(bytes_have_binary_markers(&sample))
}

fn read_forward_page(
    path: &Path,
    start: usize,
    line_limit: usize,
    start_column: usize,
) -> Result<ReadPage, ReadPageError> {
    let file = fs::File::open(path).map_err(|_| ReadPageError::ReadFailed)?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut line_number = 0_usize;
    let mut scanned_bytes = 0_u64;
    let mut text = String::new();
    let mut end = start.saturating_sub(1);
    let mut has_bom = false;
    let mut included = 0_usize;
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::InvalidData => ReadPageError::NotUtf8Text,
                _ => ReadPageError::ReadFailed,
            })?;
        if read == 0 {
            return Ok(ReadPage {
                start,
                end,
                next_start: None,
                next_column: None,
                has_bom,
                text,
                truncated: false,
                truncation_reason: None,
            });
        }
        scanned_bytes = scanned_bytes.saturating_add(read as u64);
        if scanned_bytes > MAX_READ_SCAN_BYTES {
            return Err(ReadPageError::ScanLimitReached);
        }
        line_number = line_number.saturating_add(1);
        if line_number == 1 {
            has_bom = line.starts_with('\u{feff}');
        }
        if line_number < start {
            continue;
        }
        if included >= line_limit {
            return Ok(ReadPage {
                start,
                end,
                next_start: Some(line_number),
                next_column: Some(0),
                has_bom,
                text,
                truncated: true,
                truncation_reason: Some("line_limit"),
            });
        }
        let visible = if line_number == 1 {
            strip_utf8_bom(trim_line_ending(&line)).0
        } else {
            trim_line_ending(&line)
        };
        let column = if line_number == start {
            start_column
        } else {
            0
        };
        let appended = append_read_text(&mut text, visible, column);
        end = line_number;
        included = included.saturating_add(1);
        match appended {
            ReadAppend::Complete => {}
            ReadAppend::Partial { next_column } => {
                return Ok(ReadPage {
                    start,
                    end,
                    next_start: Some(line_number),
                    next_column: Some(next_column),
                    has_bom,
                    text,
                    truncated: true,
                    truncation_reason: Some("output_limit"),
                });
            }
            ReadAppend::NoCapacity => {
                return Ok(ReadPage {
                    start,
                    end: end.saturating_sub(1),
                    next_start: Some(line_number),
                    next_column: Some(column),
                    has_bom,
                    text,
                    truncated: true,
                    truncation_reason: Some("output_limit"),
                });
            }
        }
    }
}

enum ReadAppend {
    Complete,
    Partial { next_column: usize },
    NoCapacity,
}

fn append_read_text(output: &mut String, line: &str, start_column: usize) -> ReadAppend {
    let line = char_slice_from(line, start_column);
    let separator = (!output.is_empty()).then_some('\n');
    let capacity = MAX_READ_TEXT_CHARS.saturating_sub(output.chars().count());
    let separator_chars = usize::from(separator.is_some());
    if capacity <= separator_chars {
        return ReadAppend::NoCapacity;
    }
    let available = capacity - separator_chars;
    let line_chars = line.chars().count();
    if line_chars <= available {
        if let Some(separator) = separator {
            output.push(separator);
        }
        output.push_str(line);
        return ReadAppend::Complete;
    }
    if let Some(separator) = separator {
        output.push(separator);
    }
    output.push_str(&line.chars().take(available).collect::<String>());
    ReadAppend::Partial {
        next_column: start_column.saturating_add(available),
    }
}

fn char_slice_from(value: &str, start_column: usize) -> &str {
    if start_column == 0 {
        return value;
    }
    let Some((byte_index, _)) = value.char_indices().nth(start_column) else {
        return "";
    };
    &value[byte_index..]
}

fn trim_line_ending(value: &str) -> &str {
    value.trim_end_matches(['\r', '\n'])
}

fn is_known_binary_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "webp"
            | "gif"
            | "bmp"
            | "ico"
            | "heic"
            | "heif"
            | "avif"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "mp4"
            | "mov"
            | "avi"
            | "mkv"
            | "webm"
            | "pdf"
            | "zip"
            | "7z"
            | "rar"
            | "gz"
            | "tar"
            | "exe"
            | "dll"
            | "dmg"
            | "appimage"
            | "msi"
    )
}

fn bytes_have_binary_markers(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"RIFF")
        || bytes.starts_with(b"%PDF-")
        || bytes.starts_with(b"PK\x03\x04")
    {
        return true;
    }
    let sample_len = bytes.len().min(4096);
    let sample = &bytes[..sample_len];
    let nul_count = sample.iter().filter(|byte| **byte == 0).count();
    nul_count > 0
}

fn strip_utf8_bom(text: &str) -> (&str, bool) {
    match text.strip_prefix('\u{feff}') {
        Some(stripped) => (stripped, true),
        None => (text, false),
    }
}

enum SearchMatcher {
    Literal { query: String, case_sensitive: bool },
    Regex(regex::Regex),
}

impl SearchMatcher {
    fn new(query: &str, case_sensitive: bool, regex: bool) -> Result<Self, String> {
        if regex {
            return RegexBuilder::new(query)
                .case_insensitive(!case_sensitive)
                .build()
                .map(Self::Regex)
                .map_err(|error| error.to_string());
        }
        let query = if case_sensitive {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        Ok(Self::Literal {
            query,
            case_sensitive,
        })
    }

    fn is_match(&self, line: &str) -> bool {
        match self {
            Self::Literal {
                query,
                case_sensitive,
            } => {
                if *case_sensitive {
                    line.contains(query)
                } else {
                    line.to_lowercase().contains(query)
                }
            }
            Self::Regex(regex) => regex.is_match(line),
        }
    }
}

fn search_boundary(path: &Path, display_root: &Path) -> PathBuf {
    if path.starts_with(display_root) {
        display_root.to_path_buf()
    } else {
        path.to_path_buf()
    }
}

fn workspace_search(context: &ToolExecutionContext, args: &Value) -> Value {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty());
    let Some(query) = query else {
        return json!({ "ok": false, "error": "missing_query" });
    };
    let resolved = match resolve_inside_workspace(context, args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let root = resolved.path;
    let relative_root = tool_display_path(&root, &resolved.display_root);
    let max_results = usize_arg(args, "max_results", 30, 1, MAX_SEARCH_RESULTS);
    let context_lines = usize_arg(args, "context_lines", 1, 0, 4);
    let case_sensitive = bool_arg(args, "case_sensitive", false);
    let regex = bool_arg(args, "regex", false);
    let include = string_list_arg(args, "include");
    let exclude = string_list_arg(args, "exclude");
    let include_deferred_dirs = bool_arg(args, "include_deferred_dirs", true);
    let matcher = match SearchMatcher::new(query, case_sensitive, regex) {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "ok": false,
                "error": "invalid_regex",
                "message": error
            });
        }
    };
    let boundary = search_boundary(&root, &resolved.display_root);
    let mut state = WorkspaceSearchState::default();
    search_path(
        &root,
        &relative_root,
        query,
        &matcher,
        context_lines,
        max_results,
        &include,
        &exclude,
        &boundary,
        true,
        &mut state,
    );
    let deferred_dirs_discovered = state.deferred_dirs.len();
    let mut deferred_dirs_searched = 0_usize;
    while include_deferred_dirs
        && state.results.len() < max_results
        && state.visited_files < MAX_SEARCH_FILES
        && !state.deferred_dirs.is_empty()
        && !state.output_budget_reached
        && !state.scan_byte_limit_reached
    {
        let Some((deferred_path, deferred_relative)) = state.deferred_dirs.pop_front() else {
            break;
        };
        deferred_dirs_searched += 1;
        search_path(
            &deferred_path,
            &deferred_relative,
            query,
            &matcher,
            context_lines,
            max_results,
            &include,
            &exclude,
            &boundary,
            false,
            &mut state,
        );
    }
    let deferred_dirs_remaining = state.deferred_dirs.len();
    let deferred_dirs_sample = state
        .deferred_dirs
        .iter()
        .take(MAX_DEFERRED_DIRS_REPORTED)
        .map(|(_, relative)| relative.clone())
        .collect::<Vec<_>>();
    let mut truncation_reasons = Vec::new();
    if state.results.len() >= max_results {
        truncation_reasons.push("result_limit");
    }
    if state.visited_files >= MAX_SEARCH_FILES {
        truncation_reasons.push("file_limit");
    }
    if state.scan_byte_limit_reached {
        truncation_reasons.push("scan_byte_limit");
    }
    if state.skipped_large_files > 0 {
        truncation_reasons.push("large_files_skipped");
    }
    if state.unreadable_files > 0 {
        truncation_reasons.push("unreadable_files");
    }
    if state.output_budget_reached {
        truncation_reasons.push("output_budget");
    }
    if deferred_dirs_remaining > 0 {
        truncation_reasons.push("deferred_dirs_remaining");
    }
    fit_tool_result(json!({
        "ok": true,
        "path": relative_root,
        "include": include,
        "exclude": exclude,
        "include_deferred_dirs": include_deferred_dirs,
        "regex": regex,
        "case_sensitive": case_sensitive,
        "files_scanned": state.visited_files,
        "bytes_scanned": state.scanned_bytes,
        "max_scan_bytes": MAX_SEARCH_BYTES,
        "files_skipped_large": state.skipped_large_files,
        "files_unreadable": state.unreadable_files,
        "redacted_sensitive_values": state.redacted_sensitive_values,
        "search_mode": "source_first",
        "deferred_dirs_discovered": deferred_dirs_discovered,
        "deferred_dirs_searched": deferred_dirs_searched,
        "deferred_dirs_remaining": deferred_dirs_remaining,
        "deferred_dirs": deferred_dirs_sample,
        "matches": state.results,
        "truncated": !truncation_reasons.is_empty(),
        "truncation_reasons": truncation_reasons
    }))
}

#[derive(Debug, Default)]
struct WorkspaceSearchState {
    results: Vec<Value>,
    visited_files: usize,
    scanned_bytes: u64,
    skipped_large_files: usize,
    unreadable_files: usize,
    output_budget_reached: bool,
    scan_byte_limit_reached: bool,
    redacted_sensitive_values: usize,
    deferred_dirs: VecDeque<(PathBuf, String)>,
}

#[allow(clippy::too_many_arguments)]
fn walk_directory_page(
    dir: &Path,
    relative: &str,
    current_depth: usize,
    max_depth: usize,
    include_files: bool,
    include_dirs: bool,
    boundary: &Path,
    state: &mut DirectoryPageState,
) {
    if state.has_more
        || state.scan_limit_reached
        || state.output_budget_reached
        || current_depth > max_depth
    {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if state.has_more || state.scan_limit_reached || state.output_budget_reached {
            return;
        }
        if state.scanned >= MAX_DIRECTORY_SCAN_ENTRIES {
            state.scan_limit_reached = true;
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let entry_relative = if relative == "." {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        state.scanned = state.scanned.saturating_add(1);
        let entry_type = if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "dir"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        let should_emit = (entry_type == "dir" && include_dirs)
            || (entry_type == "file" && include_files)
            || entry_type == "symlink";
        if should_emit {
            let mut value = json!({
                "type": entry_type,
                "name": name,
                "path": entry_relative
            });
            if state.include_metadata {
                let metadata = entry.metadata().ok();
                value["size_bytes"] = metadata.as_ref().map(fs::Metadata::len).into();
                value["modified_unix"] = metadata.as_ref().and_then(metadata_modified_unix).into();
                value["readonly"] = metadata
                    .as_ref()
                    .map(|metadata| metadata.permissions().readonly())
                    .into();
            }
            if !directory_page_entry_fits(state, &value) {
                state.output_budget_reached = true;
                return;
            }
            if !state.push(value) {
                return;
            }
        }
        if file_type.is_dir() && current_depth < max_depth {
            let entry_path = entry.path();
            if entry_path
                .canonicalize()
                .ok()
                .is_some_and(|resolved| resolved.starts_with(boundary))
            {
                walk_directory_page(
                    &entry_path,
                    &entry_relative,
                    current_depth + 1,
                    max_depth,
                    include_files,
                    include_dirs,
                    boundary,
                    state,
                );
            }
        }
    }
}

fn directory_page_entry_fits(state: &DirectoryPageState, entry: &Value) -> bool {
    let existing_chars = state
        .entries
        .iter()
        .map(serialized_json_chars)
        .sum::<usize>();
    existing_chars
        .saturating_add(serialized_json_chars(entry))
        .saturating_add(1_200)
        <= MAX_TOOL_RESULT_JSON_CHARS
}

fn metadata_modified_unix(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn absolute_display_path(path: &Path) -> String {
    clean_display_path(
        &path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy(),
    )
}

fn tool_display_path(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root)
        .ok()
        .map(|relative| {
            let text = clean_display_path(&relative.to_string_lossy());
            if text.is_empty() {
                ".".to_owned()
            } else {
                text
            }
        })
        .unwrap_or_else(|| absolute_display_path(path))
}

fn serialized_json_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.chars().count())
        .unwrap_or(usize::MAX)
}

fn fit_tool_result(value: Value) -> Value {
    let chars = serialized_json_chars(&value);
    if chars <= MAX_TOOL_RESULT_JSON_CHARS {
        return value;
    }
    json!({
        "ok": false,
        "error": "tool_result_budget_exceeded",
        "message": "The requested result exceeded the safe tool-output budget. Narrow the path, range, or result count and retry.",
        "result_json_chars": chars,
        "max_result_json_chars": MAX_TOOL_RESULT_JSON_CHARS
    })
}

fn redact_sensitive_text(value: &str) -> (String, usize) {
    static ENV_ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static JSON_SECRET: OnceLock<Regex> = OnceLock::new();
    static AUTH_HEADER: OnceLock<Regex> = OnceLock::new();
    static KNOWN_TOKEN: OnceLock<Regex> = OnceLock::new();
    static PRIVATE_KEY: OnceLock<Regex> = OnceLock::new();

    let mut output = value.to_owned();
    let mut redacted = 0_usize;
    let assignments = ENV_ASSIGNMENT.get_or_init(|| {
        Regex::new(r"(?im)^(\s*(?:export\s+)?[A-Za-z_][A-Za-z0-9_]*(?:API[_-]?KEY|TOKEN|SECRET|PASSWORD|PASSWD|PRIVATE[_-]?KEY|ACCESS[_-]?KEY|CLIENT[_-]?SECRET|AUTHORIZATION|COOKIE)[A-Za-z0-9_]*\s*=\s*)([^\r\n#]+)")
            .expect("valid sensitive assignment regex")
    });
    redacted += assignments.captures_iter(&output).count();
    output = assignments
        .replace_all(&output, "${1}[REDACTED]")
        .into_owned();

    let json_secret = JSON_SECRET.get_or_init(|| {
        Regex::new(r#"(?i)(\"(?:api[_-]?key|token|secret|password|passwd|client[_-]?secret|access[_-]?key)\"\s*:\s*\")(?:[^\"]*)(\")"#)
            .expect("valid JSON secret regex")
    });
    redacted += json_secret.captures_iter(&output).count();
    output = json_secret
        .replace_all(&output, "${1}[REDACTED]${2}")
        .into_owned();

    let auth_header = AUTH_HEADER.get_or_init(|| {
        Regex::new(r#"(?im)(authorization\s*:\s*(?:bearer|basic)\s+)([^\s\"']+)"#)
            .expect("valid authorization regex")
    });
    redacted += auth_header.captures_iter(&output).count();
    output = auth_header
        .replace_all(&output, "${1}[REDACTED]")
        .into_owned();

    let private_key = PRIVATE_KEY.get_or_init(|| {
        Regex::new(r"(?s)(-----BEGIN(?: [A-Z]+)* PRIVATE KEY-----).*?(-----END(?: [A-Z]+)* PRIVATE KEY-----)")
            .expect("valid private key regex")
    });
    redacted += private_key.captures_iter(&output).count();
    output = private_key
        .replace_all(&output, "${1}\n[REDACTED]\n${2}")
        .into_owned();

    let known_token = KNOWN_TOKEN.get_or_init(|| {
        Regex::new(r"(?i)\b(?:sk-[a-z0-9_-]{12,}|gh[pousr]_[a-z0-9_]{12,}|github_pat_[a-z0-9_]{12,}|AKIA[0-9A-Z]{16})\b")
            .expect("valid known token regex")
    });
    redacted += known_token.find_iter(&output).count();
    output = known_token.replace_all(&output, "[REDACTED]").into_owned();
    (output, redacted)
}

fn clean_display_path(path: &str) -> String {
    let text = path.replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = text.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        text
    }
}

#[allow(clippy::only_used_in_recursion, clippy::too_many_arguments)]
fn search_path(
    path: &Path,
    relative: &str,
    query: &str,
    matcher: &SearchMatcher,
    context_lines: usize,
    max_results: usize,
    include: &[String],
    exclude: &[String],
    workspace_root: &Path,
    defer_low_priority_dirs: bool,
    state: &mut WorkspaceSearchState,
) {
    if state.results.len() >= max_results
        || state.visited_files >= MAX_SEARCH_FILES
        || state.output_budget_reached
        || state.scan_byte_limit_reached
    {
        return;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    let Ok(resolved) = path.canonicalize() else {
        return;
    };
    if !resolved.starts_with(workspace_root) {
        return;
    }
    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let name_text = entry.file_name().to_string_lossy().to_string();
            let entry_relative = if relative == "." {
                name_text.clone()
            } else {
                format!("{relative}/{name_text}")
            };
            if defer_low_priority_dirs
                && file_type.is_dir()
                && should_defer_search_dir(&entry.path(), &entry_relative, &name_text, include)
            {
                state
                    .deferred_dirs
                    .push_back((entry.path(), entry_relative));
                continue;
            }
            if !path_passes_globs(&entry_relative, &name_text, &[], exclude) {
                continue;
            }
            search_path(
                &entry.path(),
                &entry_relative,
                query,
                matcher,
                context_lines,
                max_results,
                include,
                exclude,
                workspace_root,
                defer_low_priority_dirs,
                state,
            );
            if state.results.len() >= max_results
                || state.visited_files >= MAX_SEARCH_FILES
                || state.output_budget_reached
                || state.scan_byte_limit_reached
            {
                return;
            }
        }
        return;
    }
    if !metadata.is_file() {
        return;
    }
    let file_name = resolved
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    if !path_passes_globs(relative, &file_name, include, exclude) {
        return;
    }
    if metadata.len() > MAX_SEARCH_FILE_BYTES {
        state.skipped_large_files = state.skipped_large_files.saturating_add(1);
        return;
    }
    if state.scanned_bytes.saturating_add(metadata.len()) > MAX_SEARCH_BYTES {
        state.scan_byte_limit_reached = true;
        return;
    }
    state.visited_files = state.visited_files.saturating_add(1);
    state.scanned_bytes = state.scanned_bytes.saturating_add(metadata.len());
    let Ok(text) = fs::read_to_string(&resolved) else {
        state.unreadable_files = state.unreadable_files.saturating_add(1);
        return;
    };
    let (text, _) = strip_utf8_bom(&text);
    let lines = text.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if !matcher.is_match(line) {
            continue;
        }
        let start = index.saturating_sub(context_lines);
        let end = (index + context_lines + 1).min(lines.len());
        let (snippet, snippet_truncated) =
            truncate_chars(&lines[start..end].join("\n"), MAX_SEARCH_SNIPPET_CHARS);
        let (snippet, redacted) = redact_sensitive_text(&snippet);
        let result = json!({
            "path": relative,
            "line": index + 1,
            "snippet": snippet,
            "snippet_truncated": snippet_truncated
        });
        if !workspace_search_result_fits(state, &result) {
            state.output_budget_reached = true;
            return;
        }
        state.redacted_sensitive_values = state.redacted_sensitive_values.saturating_add(redacted);
        state.results.push(result);
        if state.results.len() >= max_results {
            return;
        }
    }
}

fn workspace_search_result_fits(state: &WorkspaceSearchState, result: &Value) -> bool {
    let used = state
        .results
        .iter()
        .map(serialized_json_chars)
        .sum::<usize>();
    used.saturating_add(serialized_json_chars(result))
        .saturating_add(1_600)
        <= MAX_TOOL_RESULT_JSON_CHARS
}

fn truncate_chars(value: &str, max_chars: usize) -> (String, bool) {
    if value.chars().count() <= max_chars {
        return (value.to_owned(), false);
    }
    (
        format!(
            "{}...[truncated chars={}]",
            value.chars().take(max_chars).collect::<String>(),
            value.chars().count()
        ),
        true,
    )
}

fn should_defer_search_dir(path: &Path, relative: &str, name: &str, include: &[String]) -> bool {
    if include
        .iter()
        .any(|pattern| pattern_references_path_segment(pattern, relative, name))
    {
        return false;
    }
    name.starts_with('.') || direct_entry_count_exceeds(path, SEARCH_LARGE_DIR_ENTRY_THRESHOLD)
}

fn pattern_references_path_segment(pattern: &str, relative: &str, name: &str) -> bool {
    let pattern = pattern.trim().replace('\\', "/");
    if pattern.is_empty() {
        return false;
    }
    pattern == name
        || pattern.starts_with(&format!("{name}/"))
        || pattern.contains(&format!("/{name}/"))
        || relative == pattern
        || relative.starts_with(&format!("{pattern}/"))
}

fn direct_entry_count_exceeds(path: &Path, threshold: usize) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.take(threshold + 1).count() > threshold
}

fn usize_arg(args: &Value, key: &str, fallback: usize, min: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(fallback)
        .clamp(min, max)
}

fn bool_arg(args: &Value, key: &str, fallback: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

fn push_string_or_array(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => {
            for line in text.split(['\n', '\r']) {
                output.push(line.to_owned());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    output.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

fn string_list_arg(args: &Value, key: &str) -> Vec<String> {
    let mut values = Vec::new();
    push_string_or_array(args.get(key), &mut values);
    values
        .into_iter()
        .map(|value| value.trim().replace('\\', "/"))
        .filter(|value| !value.is_empty())
        .collect()
}

fn path_passes_globs(path: &str, file_name: &str, include: &[String], exclude: &[String]) -> bool {
    let path = path.replace('\\', "/");
    let file_name = file_name.replace('\\', "/");
    if exclude
        .iter()
        .any(|pattern| path_matches_pattern(&path, &file_name, pattern))
    {
        return false;
    }
    include.is_empty()
        || include
            .iter()
            .any(|pattern| path_matches_pattern(&path, &file_name, pattern))
}

fn path_matches_pattern(path: &str, file_name: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().replace('\\', "/");
    if pattern.is_empty() {
        return false;
    }
    if pattern.contains(['*', '?']) {
        return wildcard_match(&pattern, path)
            || wildcard_match(&pattern, file_name)
            || wildcard_match(&format!("*{pattern}"), path);
    }
    file_name == pattern
        || path == pattern
        || path.ends_with(&format!("/{pattern}"))
        || path.contains(&format!("/{pattern}/"))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v) = (0_usize, 0_usize);
    let mut star = None;
    let mut star_value = 0_usize;
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_value = v;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            star_value += 1;
            v = star_value;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests;
