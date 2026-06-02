use codeseex_core::context::redact_inline_data_urls;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Component, Path, PathBuf};

pub(crate) mod chat_protocol;
pub(crate) mod coordinator;
pub(crate) mod diagnostics;
pub(crate) mod hosted;
pub(crate) mod ownership;
pub(crate) mod registry;
pub(crate) mod response_items;

const MAX_DEPTH: usize = 4;
const MAX_ENTRIES: usize = 200;
const MAX_READ_LINES: usize = 220;
const MAX_SEARCH_FILES: usize = 800;
const MAX_SEARCH_RESULTS: usize = 80;
const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_WEB_BYTES: u64 = 524_288;
const MAX_WEB_TEXT_CHARS: usize = 12_000;
const MAX_WEB_RESULTS: usize = 8;

const NATIVE_TOOL_IDS: &[&str] = &["apply_patch"];
const SYSTEM_EXECUTABLE_TOOL_IDS: &[&str] = &["web_search"];
const EXECUTABLE_TOOL_IDS: &[&str] = &["list_directory", "read_file_range", "workspace_search"];
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "dist",
    "build",
    "coverage",
    "debug",
    "logs",
    ".next",
    ".cache",
    "tmp",
    "temp",
    "target",
];

pub fn executable_tool_definitions(enabled_ids: &[String]) -> Vec<Value> {
    let enabled = enabled_set(enabled_ids);
    NATIVE_TOOL_IDS
        .iter()
        .chain(SYSTEM_EXECUTABLE_TOOL_IDS.iter())
        .chain(
            EXECUTABLE_TOOL_IDS
                .iter()
                .filter(|id| enabled.contains(**id)),
        )
        .copied()
        .collect::<Vec<_>>()
        .iter()
        .filter_map(|id| tool_definition(id))
        .collect()
}

pub fn is_executable_tool_enabled(name: &str, enabled_ids: &[String]) -> bool {
    SYSTEM_EXECUTABLE_TOOL_IDS.contains(&name)
        || (EXECUTABLE_TOOL_IDS.contains(&name) && enabled_ids.iter().any(|id| id == name))
}

pub fn is_known_code_tool(name: &str) -> bool {
    SYSTEM_EXECUTABLE_TOOL_IDS.contains(&name) || EXECUTABLE_TOOL_IDS.contains(&name)
}

pub fn execute_tool(name: &str, arguments: &str) -> Value {
    let args = parse_arguments(arguments);
    match name {
        "apply_patch" => apply_patch(arguments, &args),
        "list_directory" => list_directory(&args),
        "read_file_range" => read_file_range(&args),
        "workspace_search" => workspace_search(&args),
        _ => json!({
            "ok": false,
            "error": "unsupported_tool",
            "message": format!("CodeSeeX Next does not execute tool '{name}'.")
        }),
    }
}

pub async fn execute_tool_with_client(
    client: &reqwest::Client,
    name: &str,
    arguments: &str,
) -> Value {
    if name == "web_search" {
        return web_search(client, &parse_arguments(arguments)).await;
    }
    execute_tool(name, arguments)
}

pub fn default_enabled_tool_ids() -> Vec<String> {
    vec![
        "list_directory".to_owned(),
        "read_file_range".to_owned(),
        "workspace_search".to_owned(),
    ]
}

fn tool_definition(id: &str) -> Option<Value> {
    match id {
        "apply_patch" => Some(json!({
            "type": "function",
            "function": {
                "name": "apply_patch",
                "description": "Apply a Codex-style patch to files under the configured workspace root. The patch string must use the native apply_patch grammar: begin with *** Begin Patch, end with *** End Patch, and use *** Add File, *** Update File, or *** Delete File as top-level file operations. To rename or move a file, use *** Update File: <old path> followed immediately by *** Move to: <new path> before any @@, context, removed, or added lines. Prefer small patches and re-read the target file before retrying after a context mismatch.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "string",
                            "description": "Raw patch text. First line must be *** Begin Patch and last line must be *** End Patch. Top-level headers are only *** Add File, *** Update File, and *** Delete File. For rename/move, put *** Move to: <new path> immediately after the *** Update File: <old path> line, before any context or hunk lines. Do not use unified diff headers such as --- or +++."
                        }
                    },
                    "required": ["patch"],
                    "additionalProperties": false
                }
            }
        })),
        "list_directory" => Some(json!({
            "type": "function",
            "function": {
                "name": "list_directory",
                "description": "List files and folders under the configured CodeSeeX workspace root. Paths must stay inside the workspace.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "depth": { "type": "integer" },
                        "include_files": { "type": "boolean" },
                        "include_dirs": { "type": "boolean" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }
            }
        })),
        "read_file_range" => Some(json!({
            "type": "function",
            "function": {
                "name": "read_file_range",
                "description": "Read a limited line range from a text file under the configured CodeSeeX workspace root.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "start": { "type": "integer" },
                        "end": { "type": "integer" },
                        "count": { "type": "integer" }
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
                "description": "Search text files under the configured CodeSeeX workspace root and return compact path, line, and snippet matches.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "path": { "type": "string" },
                        "max_results": { "type": "integer" },
                        "context_lines": { "type": "integer" },
                        "case_sensitive": { "type": "boolean" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })),
        "web_search" => Some(json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web or open a public HTTP/HTTPS URL. Returns compact text-only evidence; local/private network targets are blocked by default.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "mode": { "type": "string", "enum": ["search", "open"] },
                        "query": { "type": "string" },
                        "search_query": {
                            "oneOf": [
                                { "type": "string" },
                                { "type": "array", "items": { "type": "string" } }
                            ]
                        },
                        "url": { "type": "string" },
                        "urls": { "type": "array", "items": { "type": "string" } },
                        "max_results": { "type": "integer" }
                    },
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

fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

fn workspace_root() -> PathBuf {
    std::env::var("CODESEEX_WORKSPACE_ROOT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn canonical_workspace_root() -> PathBuf {
    let root = workspace_root();
    fs::canonicalize(&root).unwrap_or(root)
}

fn resolve_inside_workspace(value: &Value) -> Result<(PathBuf, String), Value> {
    let raw = value
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    let root = canonical_workspace_root();
    let joined = root.join(raw);
    let resolved = match joined.canonicalize() {
        Ok(path) => path,
        Err(_) if path_contains_parent_dir(raw) => return Err(path_outside_workspace(raw)),
        Err(_) => joined,
    };
    let relative = match resolved.strip_prefix(&root) {
        Ok(path) => path.to_string_lossy().replace('\\', "/"),
        Err(_) => return Err(path_outside_workspace(raw)),
    };
    Ok((
        resolved,
        if relative.is_empty() {
            ".".to_owned()
        } else {
            relative
        },
    ))
}

fn path_contains_parent_dir(raw: &str) -> bool {
    Path::new(raw)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn path_outside_workspace(raw: &str) -> Value {
    json!({
        "ok": false,
        "error": "path_outside_workspace",
        "message": "Path must stay inside the configured workspace root.",
        "path": raw
    })
}

fn apply_patch(raw_arguments: &str, args: &Value) -> Value {
    let patch = normalize_apply_patch_input(raw_arguments, args);
    let result = parse_patch(&patch).and_then(apply_patch_operations);
    match result {
        Ok(files) => json!({
            "ok": true,
            "tool": "apply_patch",
            "message": "Success. Updated the requested files.",
            "files": files
        }),
        Err(message) => json!({
            "ok": false,
            "tool": "apply_patch",
            "error": "apply_patch_failed",
            "message": message,
            "recovery": "CodeSeeX note: apply_patch failed. Re-read the target file before retrying, then submit a smaller patch with exact current context lines. Do not reuse remembered context."
        }),
    }
}

fn normalize_apply_patch_input(raw_arguments: &str, args: &Value) -> String {
    if let Some(patch) = args.get("patch").and_then(Value::as_str) {
        return normalize_patch_newlines(patch);
    }
    if let Some(input) = args.get("input").and_then(Value::as_str) {
        return normalize_patch_newlines(input);
    }
    if let Some(command) = args.get("command").and_then(Value::as_array) {
        if command.first().and_then(Value::as_str) == Some("apply_patch") {
            if let Some(patch) = command.get(1).and_then(Value::as_str) {
                return normalize_patch_newlines(patch);
            }
        }
    }
    normalize_patch_newlines(raw_arguments)
}

fn normalize_patch_newlines(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

#[derive(Debug)]
enum PatchOperation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Update {
        path: String,
        hunks: Vec<PatchHunk>,
        move_to: Option<String>,
    },
    Delete {
        path: String,
    },
}

#[derive(Debug, Default)]
struct PatchHunk {
    lines: Vec<PatchLine>,
}

#[derive(Debug)]
enum PatchLine {
    Context(String),
    Remove(String),
    Add(String),
}

fn parse_patch(patch: &str) -> Result<Vec<PatchOperation>, String> {
    let lines = patch.lines().collect::<Vec<_>>();
    if lines.first().copied() != Some("*** Begin Patch") {
        return Err("Patch must start with *** Begin Patch.".to_owned());
    }
    if lines.last().copied() != Some("*** End Patch") {
        return Err("Patch must end with *** End Patch.".to_owned());
    }
    let mut index = 1_usize;
    let mut operations = Vec::new();
    while index + 1 < lines.len() {
        let line = lines[index];
        if line == "*** End Patch" {
            break;
        }
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut content = Vec::new();
            while index < lines.len() && !is_patch_header(lines[index]) {
                let Some(added) = lines[index].strip_prefix('+') else {
                    return Err(format!(
                        "Add File content lines must start with '+': {}",
                        lines[index]
                    ));
                };
                content.push(added.to_owned());
                index += 1;
            }
            operations.push(PatchOperation::Add {
                path: path.trim().to_owned(),
                lines: content,
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut hunks = Vec::new();
            let mut current = PatchHunk::default();
            let mut move_to = None;
            let mut seen_hunk_or_change = false;
            while index < lines.len() && !is_patch_header(lines[index]) {
                let hunk_line = lines[index];
                if hunk_line.starts_with("@@") {
                    seen_hunk_or_change = true;
                    if !current.lines.is_empty() {
                        hunks.push(current);
                        current = PatchHunk::default();
                    }
                    index += 1;
                    continue;
                }
                if let Some(target) = hunk_line.strip_prefix("*** Move to: ") {
                    if seen_hunk_or_change || !current.lines.is_empty() {
                        return Err(
                            "*** Move to must immediately follow *** Update File before any hunk or context lines."
                                .to_owned(),
                        );
                    }
                    if move_to.is_some() {
                        return Err(
                            "Patch update can contain only one *** Move to line.".to_owned()
                        );
                    }
                    move_to = Some(target.trim().to_owned());
                    index += 1;
                    continue;
                }
                let Some((prefix, text)) = split_patch_line(hunk_line) else {
                    return Err(format!("Invalid update line prefix: {hunk_line}"));
                };
                seen_hunk_or_change = true;
                current.lines.push(match prefix {
                    ' ' => PatchLine::Context(text.to_owned()),
                    '-' => PatchLine::Remove(text.to_owned()),
                    '+' => PatchLine::Add(text.to_owned()),
                    _ => unreachable!(),
                });
                index += 1;
            }
            if !current.lines.is_empty() {
                hunks.push(current);
            }
            operations.push(PatchOperation::Update {
                path: path.trim().to_owned(),
                hunks,
                move_to,
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOperation::Delete {
                path: path.trim().to_owned(),
            });
            index += 1;
            continue;
        }
        return Err(format!("Unsupported patch header: {line}"));
    }
    if operations.is_empty() {
        return Err("Patch did not contain any file operations.".to_owned());
    }
    Ok(operations)
}

fn is_patch_header(line: &str) -> bool {
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Update File: ")
        || line.starts_with("*** Delete File: ")
}

fn split_patch_line(line: &str) -> Option<(char, &str)> {
    let prefix = line.chars().next()?;
    if matches!(prefix, ' ' | '-' | '+') {
        Some((prefix, &line[prefix.len_utf8()..]))
    } else {
        None
    }
}

fn apply_patch_operations(operations: Vec<PatchOperation>) -> Result<Vec<Value>, String> {
    let mut changed = Vec::new();
    for operation in operations {
        match operation {
            PatchOperation::Add { path, lines } => {
                let (target, relative) = resolve_patch_path(&path)?;
                if target.exists() {
                    return Err(format!("Add File target already exists: {relative}"));
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| format!("Failed to create parent directory: {error}"))?;
                }
                fs::write(&target, patch_lines_to_text(&lines))
                    .map_err(|error| format!("Failed to write {relative}: {error}"))?;
                changed.push(json!({ "path": relative, "operation": "add" }));
            }
            PatchOperation::Update {
                path,
                hunks,
                move_to,
            } => {
                let (target, relative) = resolve_patch_path(&path)?;
                let text = fs::read_to_string(&target)
                    .map_err(|error| format!("Failed to read {relative}: {error}"))?;
                let updated = apply_hunks_to_text(&text, &hunks)
                    .map_err(|error| format!("{error} in {relative}"))?;
                fs::write(&target, updated)
                    .map_err(|error| format!("Failed to write {relative}: {error}"))?;
                if let Some(move_to) = move_to {
                    let (destination, destination_relative) = resolve_patch_path(&move_to)?;
                    if let Some(parent) = destination.parent() {
                        fs::create_dir_all(parent).map_err(|error| {
                            format!("Failed to create move destination parent: {error}")
                        })?;
                    }
                    fs::rename(&target, &destination).map_err(|error| {
                        format!("Failed to move {relative} to {destination_relative}: {error}")
                    })?;
                    changed.push(json!({
                        "path": relative,
                        "operation": "update_move",
                        "move_to": destination_relative
                    }));
                } else {
                    changed.push(json!({ "path": relative, "operation": "update" }));
                }
            }
            PatchOperation::Delete { path } => {
                let (target, relative) = resolve_patch_path(&path)?;
                fs::remove_file(&target)
                    .map_err(|error| format!("Failed to delete {relative}: {error}"))?;
                changed.push(json!({ "path": relative, "operation": "delete" }));
            }
        }
    }
    Ok(changed)
}

fn resolve_patch_path(raw: &str) -> Result<(PathBuf, String), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("Patch path must not be empty.".to_owned());
    }
    let raw_path = Path::new(raw);
    if !raw_path.is_absolute() && path_contains_parent_dir(raw) {
        return Err("Patch paths must be relative and stay inside the workspace root.".to_owned());
    }
    let root = canonical_workspace_root();
    let joined = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        root.join(raw_path)
    };
    let nearest_parent = nearest_existing_parent(&joined)
        .ok_or_else(|| "Workspace root does not exist.".to_owned())?;
    let parent_real = nearest_parent
        .canonicalize()
        .map_err(|error| format!("Failed to verify patch path parent: {error}"))?;
    if !parent_real.starts_with(&root) {
        return Err("Patch path parent resolves outside the workspace root.".to_owned());
    }
    let target = if joined.exists() {
        let real = joined
            .canonicalize()
            .map_err(|error| format!("Failed to verify patch path: {error}"))?;
        if !real.starts_with(&root) {
            return Err("Patch path resolves outside the workspace root.".to_owned());
        }
        real
    } else {
        joined
    };
    let relative = target
        .strip_prefix(&root)
        .map_err(|_| "Patch path must stay inside the workspace root.".to_owned())?
        .to_string_lossy()
        .replace('\\', "/");
    Ok((target, relative))
}

fn nearest_existing_parent(path: &Path) -> Option<PathBuf> {
    let mut cursor = path.parent()?;
    loop {
        if cursor.exists() {
            return Some(cursor.to_path_buf());
        }
        cursor = cursor.parent()?;
    }
}

fn patch_lines_to_text(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn apply_hunks_to_text(text: &str, hunks: &[PatchHunk]) -> Result<String, String> {
    let trailing_newline = text.ends_with('\n');
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut cursor = 0_usize;
    for hunk in hunks {
        let old = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Remove(text) => Some(text.clone()),
                PatchLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let new = hunk
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Context(text) | PatchLine::Add(text) => Some(text.clone()),
                PatchLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();
        let Some(position) = find_line_block(&lines, &old, cursor) else {
            return Err("Failed to find expected patch context".to_owned());
        };
        lines.splice(position..position + old.len(), new.clone());
        cursor = position + new.len();
    }
    let mut output = lines.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    Ok(output)
}

fn find_line_block(lines: &[String], needle: &[String], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(lines.len()));
    }
    if needle.len() > lines.len() {
        return None;
    }
    (start..=lines.len().saturating_sub(needle.len()))
        .find(|position| lines[*position..*position + needle.len()] == *needle)
}

fn list_directory(args: &Value) -> Value {
    let Ok((path, relative)) = resolve_inside_workspace(args) else {
        return resolve_inside_workspace(args)
            .err()
            .unwrap_or_else(|| json!({"ok": false}));
    };
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
    let depth = usize_arg(args, "depth", 1, 0, MAX_DEPTH);
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
    json!({
        "ok": true,
        "path": relative,
        "depth": depth,
        "entries": entries,
        "truncated": truncated
    })
}

fn read_file_range(args: &Value) -> Value {
    let Ok((path, relative)) = resolve_inside_workspace(args) else {
        return resolve_inside_workspace(args)
            .err()
            .unwrap_or_else(|| json!({"ok": false}));
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return json!({ "ok": false, "error": "not_found", "path": relative });
    };
    if !metadata.is_file() {
        return json!({ "ok": false, "error": "not_file", "path": relative });
    }
    if metadata.len() > MAX_FILE_BYTES {
        return json!({ "ok": false, "error": "file_too_large", "path": relative, "bytes": metadata.len() });
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return json!({ "ok": false, "error": "not_text", "path": relative });
    };
    let lines = text.lines().collect::<Vec<_>>();
    let start = usize_arg(args, "start", 1, 1, lines.len().max(1));
    let count = usize_arg(args, "count", 80, 1, MAX_READ_LINES);
    let requested_end = args
        .get("end")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(start.saturating_add(count).saturating_sub(1));
    let end = requested_end
        .min(lines.len())
        .min(start + MAX_READ_LINES - 1);
    let selected = if start <= end && start <= lines.len() {
        lines[start - 1..end]
            .iter()
            .enumerate()
            .map(|(index, line)| json!({ "line": start + index, "text": *line }))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    json!({
        "ok": true,
        "path": relative,
        "start": start,
        "end": end,
        "total_lines": lines.len(),
        "lines": selected,
        "truncated": requested_end > end
    })
}

fn workspace_search(args: &Value) -> Value {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty());
    let Some(query) = query else {
        return json!({ "ok": false, "error": "missing_query" });
    };
    let Ok((root, relative_root)) = resolve_inside_workspace(args) else {
        return resolve_inside_workspace(args)
            .err()
            .unwrap_or_else(|| json!({"ok": false}));
    };
    let workspace_root = canonical_workspace_root();
    let max_results = usize_arg(args, "max_results", 30, 1, MAX_SEARCH_RESULTS);
    let context_lines = usize_arg(args, "context_lines", 1, 0, 4);
    let case_sensitive = bool_arg(args, "case_sensitive", false);
    let query_cmp = if case_sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    let mut results = Vec::new();
    let mut visited_files = 0_usize;
    search_path(
        &root,
        &relative_root,
        query,
        &query_cmp,
        case_sensitive,
        context_lines,
        max_results,
        &workspace_root,
        &mut visited_files,
        &mut results,
    );
    json!({
        "ok": true,
        "query": query,
        "path": relative_root,
        "files_scanned": visited_files,
        "matches": results,
        "truncated": results.len() >= max_results || visited_files >= MAX_SEARCH_FILES
    })
}

async fn web_search(client: &reqwest::Client, args: &Value) -> Value {
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "search".to_owned());
    let urls = normalize_web_open_targets(args);
    if mode == "open" || !urls.is_empty() {
        return web_open_many(client, &urls).await;
    }

    let queries = normalize_web_queries(args);
    if queries.is_empty() {
        return json!({
            "ok": false,
            "error": "missing_query",
            "message": "web_search requires query/search_query for mode=search or url/urls for mode=open."
        });
    }

    let max_results = usize_arg(args, "max_results", 5, 1, MAX_WEB_RESULTS);
    let mut per_query = Vec::new();
    let mut all_results = Vec::new();
    for query in queries.iter().take(3) {
        let result = web_search_duckduckgo(client, query, max_results).await;
        if let Some(results) = result.get("results").and_then(Value::as_array) {
            all_results.extend(results.iter().cloned());
        }
        per_query.push(result);
    }
    all_results.truncate(max_results);

    json!({
        "ok": true,
        "mode": "search",
        "queries": queries,
        "source": "duckduckgo_instant_answer",
        "results": all_results,
        "per_query": per_query,
        "truncated": per_query.len() > 3
    })
}

async fn web_open_many(client: &reqwest::Client, urls: &[String]) -> Value {
    if urls.is_empty() {
        return json!({
            "ok": false,
            "error": "missing_url",
            "message": "web_search mode=open requires url or urls."
        });
    }

    let mut opened = Vec::new();
    for url in urls.iter().take(3) {
        opened.push(web_open_url(client, url).await);
    }
    json!({
        "ok": opened.iter().any(|item| item.get("ok").and_then(Value::as_bool) == Some(true)),
        "mode": "open",
        "opened": opened,
        "truncated": urls.len() > 3
    })
}

async fn web_open_url(_client: &reqwest::Client, raw_url: &str) -> Value {
    let Ok(url) = reqwest::Url::parse(raw_url.trim()) else {
        return json!({ "ok": false, "error": "invalid_url", "url": raw_url });
    };
    if let Err(message) = validate_public_web_url(&url) {
        return json!({ "ok": false, "error": "blocked_url", "url": raw_url, "message": message });
    }

    if let Err(message) = validate_web_url_network(&url).await {
        return json!({ "ok": false, "error": "blocked_url", "url": raw_url, "message": message });
    }

    let web_client = safe_web_client();
    let response = match web_client
        .get(url.clone())
        .header(reqwest::header::USER_AGENT, "CodeSeeX-Next/0.1")
        .header(
            reqwest::header::ACCEPT,
            "text/html,text/plain,application/json,application/xml;q=0.9,*/*;q=0.5",
        )
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({ "ok": false, "error": "request_failed", "url": raw_url, "message": error.to_string() });
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let redirect_location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if (300..400).contains(&status) {
        return json!({
            "ok": false,
            "error": "redirect_not_followed",
            "url": raw_url,
            "status": status,
            "location": redirect_location
        });
    }
    if !content_type.is_empty() && !is_textual_content_type(&content_type) {
        return json!({
            "ok": false,
            "error": "non_text_response",
            "url": raw_url,
            "status": status,
            "content_type": content_type
        });
    }

    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    let raw_text = String::from_utf8_lossy(&bytes).to_string();
    let title = extract_html_title(&raw_text);
    let text = if response_looks_like_html(&content_type, &raw_text) {
        html_to_text(&raw_text)
    } else {
        compact_whitespace(&raw_text)
    };
    json!({
        "ok": (200..400).contains(&status),
        "mode": "open",
        "url": url.as_str(),
        "status": status,
        "content_type": content_type,
        "title": title,
        "text": truncate_chars(&redact_inline_data_urls(&text), MAX_WEB_TEXT_CHARS),
        "bytes": bytes.len(),
        "truncated": byte_truncated || text.chars().count() > MAX_WEB_TEXT_CHARS
    })
}

async fn web_search_duckduckgo(client: &reqwest::Client, query: &str, max_results: usize) -> Value {
    let Ok(url) = reqwest::Url::parse_with_params(
        "https://api.duckduckgo.com/",
        &[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    ) else {
        return json!({ "ok": false, "query": query, "error": "invalid_search_url" });
    };
    let response = match client
        .get(url)
        .header(reqwest::header::USER_AGENT, "CodeSeeX-Next/0.1")
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({ "ok": false, "query": query, "error": "request_failed", "message": error.to_string() });
        }
    };
    let status = response.status().as_u16();
    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if byte_truncated {
        return json!({
            "ok": false,
            "query": query,
            "status": status,
            "error": "search_response_too_large",
            "bytes": bytes.len()
        });
    }
    let payload = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|_| json!({}));
    let mut results = Vec::new();

    let abstract_text = payload
        .get("AbstractText")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if let Some(text) = abstract_text {
        results.push(json!({
            "title": payload.get("Heading").and_then(Value::as_str).unwrap_or(query),
            "url": payload.get("AbstractURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(text, 1200),
            "source": "abstract"
        }));
    }
    if let Some(answer) = payload
        .get("Answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        results.push(json!({
            "title": "Answer",
            "url": payload.get("AnswerType").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(answer, 1200),
            "source": "answer"
        }));
    }
    collect_duckduckgo_related(payload.get("RelatedTopics"), &mut results, max_results);
    results.truncate(max_results);

    json!({
        "ok": (200..400).contains(&status),
        "query": query,
        "status": status,
        "source": "duckduckgo_instant_answer",
        "results": results,
        "truncated": false,
        "bytes": bytes.len()
    })
}

fn collect_duckduckgo_related(value: Option<&Value>, output: &mut Vec<Value>, max_results: usize) {
    if output.len() >= max_results {
        return;
    }
    let Some(items) = value.and_then(Value::as_array) else {
        return;
    };
    for item in items {
        if output.len() >= max_results {
            return;
        }
        if let Some(topics) = item.get("Topics") {
            collect_duckduckgo_related(Some(topics), output, max_results);
            continue;
        }
        let text = item
            .get("Text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            continue;
        };
        output.push(json!({
            "title": truncate_chars(text, 120),
            "url": item.get("FirstURL").and_then(Value::as_str).unwrap_or(""),
            "snippet": truncate_chars(text, 1200),
            "source": "related_topic"
        }));
    }
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
        if file_type.is_dir() {
            if include_dirs {
                output.push(json!({ "type": "dir", "path": entry_relative }));
            }
            if current_depth < max_depth && !SKIP_DIRS.contains(&name.as_str()) {
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
            output.push(json!({ "type": "file", "path": entry_relative }));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn search_path(
    path: &Path,
    relative: &str,
    query: &str,
    query_cmp: &str,
    case_sensitive: bool,
    context_lines: usize,
    max_results: usize,
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
            let name = entry.file_name().to_string_lossy().to_string();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let entry_relative = if relative == "." {
                name
            } else {
                format!("{relative}/{}", entry.file_name().to_string_lossy())
            };
            search_path(
                &entry.path(),
                &entry_relative,
                query,
                query_cmp,
                case_sensitive,
                context_lines,
                max_results,
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
    *visited_files += 1;
    let Ok(text) = fs::read_to_string(resolved) else {
        return;
    };
    let lines = text.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let haystack = if case_sensitive {
            (*line).to_owned()
        } else {
            line.to_lowercase()
        };
        if !haystack.contains(query_cmp) {
            continue;
        }
        let start = index.saturating_sub(context_lines);
        let end = (index + context_lines + 1).min(lines.len());
        let snippet = lines[start..end].join("\n");
        results.push(json!({
            "path": relative,
            "line": index + 1,
            "snippet": snippet,
            "query": query
        }));
        if results.len() >= max_results {
            return;
        }
    }
}

fn normalize_web_queries(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_string_or_array(args.get("search_query"), &mut values);
    push_string_or_array(args.get("query"), &mut values);
    push_string_or_array(args.get("q"), &mut values);
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_web_open_targets(args: &Value) -> Vec<String> {
    let mut values = Vec::new();
    push_string_or_array(args.get("urls"), &mut values);
    push_string_or_array(args.get("url"), &mut values);
    if values.is_empty() {
        for query in normalize_web_queries(args) {
            if looks_like_url(&query) {
                values.push(query);
            }
        }
    }
    values
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

fn push_string_or_array(value: Option<&Value>, output: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => output.push(text.to_owned()),
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

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn validate_public_web_url(url: &reqwest::Url) -> Result<(), String> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("Only http:// and https:// URLs are supported.".to_owned()),
    }
    if allow_private_web_targets() {
        return Ok(());
    }
    let Some(host) = url.host_str() else {
        return Err("URL must include a host.".to_owned());
    };
    let host_lower = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if matches!(host_lower.as_str(), "localhost" | "localhost.localdomain") {
        return Err("Localhost targets are blocked for web_search.".to_owned());
    }
    if let Ok(ip) = host_lower.parse::<IpAddr>() {
        if ip_is_blocked(ip) {
            return Err("Private or local network targets are blocked for web_search.".to_owned());
        }
    }
    Ok(())
}

async fn validate_web_url_network(url: &reqwest::Url) -> Result<(), String> {
    validate_public_web_url(url)?;
    if allow_private_web_targets() {
        return Ok(());
    }
    let Some(host) = url.host_str() else {
        return Err("URL must include a host.".to_owned());
    };
    if host.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("DNS lookup failed: {error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("DNS lookup returned no addresses.".to_owned());
    }
    if addresses.iter().any(|address| ip_is_blocked(address.ip())) {
        return Err("DNS resolved to a private or local network target.".to_owned());
    }
    Ok(())
}

fn safe_web_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build no-redirect web client")
}

fn ip_is_blocked(ip: IpAddr) -> bool {
    let ip = normalize_mapped_ip(ip);
    ip.is_loopback()
        || ip.is_unspecified()
        || is_private_ip(ip)
        || is_link_local_ip(ip)
        || is_documentation_ip(ip)
}

fn normalize_mapped_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ipv4_mapped(ip).map(IpAddr::V4).unwrap_or(IpAddr::V6(ip)),
        other => other,
    }
}

fn ipv4_mapped(ip: Ipv6Addr) -> Option<std::net::Ipv4Addr> {
    let segments = ip.segments();
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        let octets = ip.octets();
        Some(std::net::Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else {
        None
    }
}

fn allow_private_web_targets() -> bool {
    std::env::var("CODESEEX_WEB_SEARCH_ALLOW_PRIVATE")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private(),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            (segments[0] & 0xfe00) == 0xfc00
        }
    }
}

fn is_link_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

fn is_documentation_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            matches!(
                octets,
                [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
            )
        }
        IpAddr::V6(ip) => (ip.segments()[0] == 0x2001) && (ip.segments()[1] == 0x0db8),
    }
}

async fn read_limited_response_bytes(response: reqwest::Response) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = usize::try_from(MAX_WEB_BYTES)
            .unwrap_or(usize::MAX)
            .saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    (bytes, truncated)
}

fn is_textual_content_type(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("html")
        || content_type.contains("javascript")
}

fn response_looks_like_html(content_type: &str, text: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("html") {
        return true;
    }
    let sample = text.trim_start().chars().take(4096).collect::<String>();
    let sample = sample.to_ascii_lowercase();
    sample.starts_with("<!doctype html")
        || sample.starts_with("<html")
        || sample.contains("<body")
        || sample.contains("<script")
        || sample.contains("<style")
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let after_open = lower[start..].find('>')? + start + 1;
    let end = lower[after_open..].find("</title>")? + after_open;
    let title = compact_whitespace(&html[after_open..end]);
    (!title.is_empty()).then(|| truncate_chars(&decode_basic_html_entities(&title), 240))
}

fn html_to_text(html: &str) -> String {
    let without_scripts = remove_html_block(html, "script");
    let without_styles = remove_html_block(&without_scripts, "style");
    let mut text = String::new();
    let mut in_tag = false;
    for ch in without_styles.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    compact_whitespace(&decode_basic_html_entities(&text))
}

fn remove_html_block(html: &str, tag: &str) -> String {
    let mut output = html.to_owned();
    loop {
        let lower = output.to_ascii_lowercase();
        let Some(start) = lower.find(&format!("<{tag}")) else {
            break;
        };
        let Some(relative_end) = lower[start..].find(&format!("</{tag}>")) else {
            output.truncate(start);
            break;
        };
        let end = start + relative_end + tag.len() + 3;
        output.replace_range(start..end, " ");
    }
    output
}

fn decode_basic_html_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let prefix = text.chars().take(max_chars).collect::<String>();
    format!("{prefix}...[truncated chars={count}]")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn definitions_follow_enabled_ids() {
        let definitions = executable_tool_definitions(&["list_directory".to_owned()]);
        let names = definitions
            .iter()
            .filter_map(|definition| definition.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["apply_patch", "web_search", "list_directory"]);
    }

    #[test]
    fn executable_tool_checks_enabled_allowlist() {
        let enabled = vec!["list_directory".to_owned()];
        assert!(!is_executable_tool_enabled("apply_patch", &[]));
        assert!(is_executable_tool_enabled("web_search", &[]));
        assert!(is_executable_tool_enabled("list_directory", &enabled));
        assert!(!is_executable_tool_enabled("read_file_range", &enabled));
    }

    #[test]
    fn apply_patch_move_to_must_immediately_follow_update_file() {
        let good = parse_patch(
            &[
                "*** Begin Patch",
                "*** Update File: old.txt",
                "*** Move to: new.txt",
                " unchanged",
                "*** End Patch",
            ]
            .join("\n"),
        )
        .expect("move immediately after update should parse");
        match &good[0] {
            PatchOperation::Update { move_to, .. } => {
                assert_eq!(move_to.as_deref(), Some("new.txt"));
            }
            _ => panic!("expected update operation"),
        }

        let bad = parse_patch(
            &[
                "*** Begin Patch",
                "*** Update File: old.txt",
                " unchanged",
                "*** Move to: new.txt",
                "*** End Patch",
            ]
            .join("\n"),
        )
        .expect_err("late move should match native apply_patch rejection");
        assert!(bad.contains("immediately follow"), "{bad}");
    }

    #[test]
    fn web_search_blocks_private_targets_by_default() {
        std::env::remove_var("CODESEEX_WEB_SEARCH_ALLOW_PRIVATE");
        let local = reqwest::Url::parse("http://127.0.0.1:8787/").expect("parse url");
        let private = reqwest::Url::parse("http://192.168.1.20/").expect("parse url");
        let mapped = reqwest::Url::parse("http://[::ffff:127.0.0.1]/").expect("parse url");
        let public = reqwest::Url::parse("https://example.com/").expect("parse url");
        assert!(validate_public_web_url(&local).is_err());
        assert!(validate_public_web_url(&private).is_err());
        assert!(validate_public_web_url(&mapped).is_err());
        assert!(validate_public_web_url(&public).is_ok());
    }

    #[test]
    fn web_search_detects_html_even_without_html_content_type() {
        let html = "<html><head><script>window.noise = true;</script></head><body>VISIBLE_TEXT</body></html>";
        assert!(response_looks_like_html("text/plain", html));
        let text = html_to_text(html);
        assert!(text.contains("VISIBLE_TEXT"));
        assert!(!text.contains("window.noise"));
    }

    #[test]
    fn read_only_tools_execute_inside_workspace_and_reject_escape() {
        let root = temp_workspace("read-only-tools");
        let outside = root.parent().expect("temp workspace parent").join(format!(
            "{}-outside.txt",
            root.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir_all(&root).expect("create temp workspace");
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/codeseex-core\"]\n",
        )
        .expect("write Cargo.toml");
        fs::write(root.join("README.md"), "CodeSeeX Next smoke file\n").expect("write README.md");
        fs::write(&outside, "outside").expect("write outside file");

        std::env::set_var("CODESEEX_WORKSPACE_ROOT", &root);

        let listed = execute_tool("list_directory", r#"{"path":".","depth":0}"#);
        assert_eq!(listed.get("ok").and_then(Value::as_bool), Some(true));
        assert!(listed.to_string().contains("Cargo.toml"));

        let read = execute_tool("read_file_range", r#"{"path":"Cargo.toml","count":2}"#);
        assert_eq!(read.get("ok").and_then(Value::as_bool), Some(true));
        assert!(read.to_string().contains("[workspace]"));

        let searched = execute_tool(
            "workspace_search",
            r#"{"query":"CodeSeeX Next","path":"README.md"}"#,
        );
        assert_eq!(searched.get("ok").and_then(Value::as_bool), Some(true));
        assert!(searched.to_string().contains("CodeSeeX Next"));

        let patch = [
            "*** Begin Patch",
            "*** Add File: generated.txt",
            "+alpha",
            "+beta",
            "*** Update File: README.md",
            "@@",
            "-CodeSeeX Next smoke file",
            "+CodeSeeX Next patched smoke file",
            "*** Delete File: Cargo.toml",
            "*** End Patch",
        ]
        .join("\n");
        let patched = execute_tool("apply_patch", &json!({ "patch": patch }).to_string());
        assert_eq!(patched.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            fs::read_to_string(root.join("generated.txt")).expect("read generated"),
            "alpha\nbeta\n"
        );
        assert!(fs::read_to_string(root.join("README.md"))
            .expect("read patched README")
            .contains("patched smoke file"));
        assert!(!root.join("Cargo.toml").exists());

        let escape_args = json!({
            "path": format!("../{}", outside.file_name().unwrap().to_string_lossy())
        })
        .to_string();
        let escaped = execute_tool("read_file_range", &escape_args);
        assert_eq!(
            escaped.get("error").and_then(Value::as_str),
            Some("path_outside_workspace")
        );

        std::env::remove_var("CODESEEX_WORKSPACE_ROOT");
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("codeseex-next-{label}-{nanos}"))
    }
}
