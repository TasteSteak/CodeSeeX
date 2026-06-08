use regex::RegexBuilder;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
const MAX_ENTRIES: usize = 200;
const MAX_READ_LINES: usize = 220;
const MAX_SEARCH_FILES: usize = 800;
const MAX_SEARCH_RESULTS: usize = 80;
const MAX_FILE_BYTES: u64 = 1_048_576;

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
    if name == vision::ANALYZE_TOOL_NAME || name == vision::LEGACY_ANALYZE_TOOL_NAME {
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
    let relative = resolved.display_path;
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
    let mut entries = Vec::new();
    walk_directory(
        &path,
        &relative,
        0,
        depth,
        include_files,
        include_dirs,
        &mut entries,
    );
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    let summary = directory_summary(&entries);
    json!({
        "ok": true,
        "path": relative,
        "depth": depth,
        "entries": entries,
        "summary": summary,
        "truncated": truncated
    })
}

fn read_file_range(context: &ToolExecutionContext, args: &Value) -> Value {
    let resolved = match resolve_inside_workspace(context, args) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let path = resolved.path;
    let relative = resolved.display_path;
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
    if metadata.len() > MAX_FILE_BYTES {
        return json!({ "ok": false, "error": "file_too_large", "path": relative, "bytes": metadata.len() });
    }
    let Ok(bytes) = fs::read(&path) else {
        return json!({ "ok": false, "error": "read_failed", "path": relative });
    };
    if bytes_have_binary_markers(&bytes) {
        return json!({
            "ok": false,
            "error": "binary_file_not_supported",
            "message": "read_file_range only reads UTF-8 text files. Binary-looking content was detected.",
            "path": relative
        });
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return json!({ "ok": false, "error": "not_utf8_text", "path": relative });
    };
    let (text, has_bom) = strip_utf8_bom(&text);
    let lines = text.lines().collect::<Vec<_>>();
    let whole_file = bool_arg(args, "whole_file", false);
    let (start, requested_end) = if whole_file {
        (1, lines.len())
    } else {
        read_line_window(args, lines.len())
    };
    let max_lines = if whole_file {
        lines.len().max(1)
    } else {
        MAX_READ_LINES
    };
    let end = requested_end
        .min(lines.len())
        .min(start.saturating_add(max_lines).saturating_sub(1));
    let selected = if start <= end && start <= lines.len() {
        lines[start - 1..end]
            .iter()
            .enumerate()
            .map(|(index, line)| json!({ "line": start + index, "text": *line }))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let selected_text = selected
        .iter()
        .filter_map(|line| line.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    json!({
        "ok": true,
        "path": relative,
        "start": start,
        "end": end,
        "total_lines": lines.len(),
        "whole_file": whole_file,
        "has_bom": has_bom,
        "text": selected_text,
        "lines": selected,
        "truncated": requested_end > end
    })
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

fn read_line_window(args: &Value, total_lines: usize) -> (usize, usize) {
    if total_lines == 0 {
        return (1, 0);
    }
    if let Some(tail_lines) = args
        .get("tail_lines")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map(|value| value.clamp(1, MAX_READ_LINES))
    {
        return (
            total_lines.saturating_sub(tail_lines).saturating_add(1),
            total_lines,
        );
    }
    let start_value = args.get("start").and_then(Value::as_i64).unwrap_or(1);
    let start = normalize_line_index(start_value, total_lines);
    if let Some(end_value) = args.get("end").and_then(Value::as_i64) {
        return (start, normalize_line_index(end_value, total_lines));
    }
    if let Some(count) = args
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .map(|value| value.clamp(1, MAX_READ_LINES))
    {
        return (start, start.saturating_add(count).saturating_sub(1));
    }
    let requested_end = if start_value < 0 {
        total_lines
    } else {
        total_lines.max(start)
    };
    (start, requested_end)
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
    let relative_root = resolved.display_path;
    let max_results = usize_arg(args, "max_results", 30, 1, MAX_SEARCH_RESULTS);
    let context_lines = usize_arg(args, "context_lines", 1, 0, 4);
    let case_sensitive = bool_arg(args, "case_sensitive", false);
    let regex = bool_arg(args, "regex", false);
    let include = string_list_arg(args, "include");
    let exclude = string_list_arg(args, "exclude");
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
    let mut results = Vec::new();
    let mut visited_files = 0_usize;
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
        &mut visited_files,
        &mut results,
    );
    json!({
        "ok": true,
        "query": query,
        "path": relative_root,
        "include": include,
        "exclude": exclude,
        "regex": regex,
        "case_sensitive": case_sensitive,
        "files_scanned": visited_files,
        "matches": results,
        "truncated": results.len() >= max_results || visited_files >= MAX_SEARCH_FILES
    })
}

fn walk_directory(
    dir: &Path,
    relative: &str,
    current_depth: usize,
    max_depth: usize,
    include_files: bool,
    include_dirs: bool,
    output: &mut Vec<Value>,
) {
    if output.len() > MAX_ENTRIES || current_depth > max_depth {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if output.len() > MAX_ENTRIES {
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
        let entry_path = absolute_display_path(&entry.path());
        let metadata = entry.metadata().ok();
        let readonly = metadata
            .as_ref()
            .map(|metadata| metadata.permissions().readonly());
        let modified_unix = metadata.as_ref().and_then(metadata_modified_unix);
        if file_type.is_dir() {
            if include_dirs {
                output.push(json!({
                    "type": "dir",
                    "name": name,
                    "path": entry_path,
                    "modified_unix": modified_unix,
                    "readonly": readonly
                }));
            }
            if current_depth < max_depth {
                walk_directory(
                    &entry.path(),
                    &entry_relative,
                    current_depth + 1,
                    max_depth,
                    include_files,
                    include_dirs,
                    output,
                );
            }
        } else if file_type.is_file() && include_files {
            let extension = Path::new(&name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            let size_bytes = metadata.as_ref().map(fs::Metadata::len);
            output.push(json!({
                "type": "file",
                "name": name,
                "path": entry_path,
                "extension": extension,
                "size_bytes": size_bytes,
                "modified_unix": modified_unix,
                "readonly": readonly
            }));
        }
    }
}

fn metadata_modified_unix(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn directory_summary(entries: &[Value]) -> Value {
    let mut files = 0_usize;
    let mut dirs = 0_usize;
    let mut extensions = BTreeMap::<String, usize>::new();
    for entry in entries {
        match entry.get("type").and_then(Value::as_str) {
            Some("file") => {
                files += 1;
                if let Some(extension) = entry
                    .get("extension")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    *extensions
                        .entry(extension.to_ascii_lowercase())
                        .or_default() += 1;
                }
            }
            Some("dir") => dirs += 1,
            _ => {}
        }
    }
    json!({
        "files": files,
        "dirs": dirs,
        "extensions": extensions
    })
}

fn absolute_display_path(path: &Path) -> String {
    clean_display_path(
        &path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy(),
    )
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

#[allow(clippy::too_many_arguments)]
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
    visited_files: &mut usize,
    results: &mut Vec<Value>,
) {
    if results.len() >= max_results || *visited_files >= MAX_SEARCH_FILES {
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
                visited_files,
                results,
            );
            if results.len() >= max_results || *visited_files >= MAX_SEARCH_FILES {
                return;
            }
        }
        return;
    }
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return;
    }
    let file_name = resolved
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    if !path_passes_globs(relative, &file_name, include, exclude) {
        return;
    }
    *visited_files += 1;
    let Ok(text) = fs::read_to_string(&resolved) else {
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
        let snippet = lines[start..end].join("\n");
        results.push(json!({
            "path": absolute_display_path(&resolved),
            "line": index + 1,
            "snippet": snippet,
            "query": query
        }));
        if results.len() >= max_results {
            return;
        }
    }
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
