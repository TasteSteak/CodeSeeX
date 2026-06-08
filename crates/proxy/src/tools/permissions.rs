use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct ToolPermissionContext {
    workspace_roots: Vec<PathBuf>,
    allow_outside_workspace: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedToolPath {
    pub(crate) path: PathBuf,
    pub(crate) display_path: String,
    pub(crate) display_root: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) enum ToolPermissionError {
    WorkspaceRootNotConfigured,
    PathOutsideWorkspace { path: String },
}

impl Default for ToolPermissionContext {
    fn default() -> Self {
        Self::from_env()
    }
}

impl ToolPermissionContext {
    pub(crate) fn new(workspace_roots: Vec<PathBuf>, allow_outside_workspace: bool) -> Self {
        Self {
            workspace_roots: dedupe_existing_dirs(workspace_roots),
            allow_outside_workspace,
        }
    }

    pub(crate) fn from_request(request: &Value) -> Self {
        let mut roots = Vec::new();
        collect_request_workspace_roots(request, &mut roots);
        if roots.is_empty() {
            extend_workspace_roots_from_env(&mut roots);
        }
        Self::new(
            roots,
            request_indicates_full_file_access(request) || env_indicates_full_file_access(),
        )
    }

    pub(crate) fn from_env() -> Self {
        let mut roots = Vec::new();
        extend_workspace_roots_from_env(&mut roots);
        Self::new(roots, env_indicates_full_file_access())
    }

    pub(crate) fn resolve_path(&self, raw: &str) -> Result<ResolvedToolPath, ToolPermissionError> {
        let raw = raw.trim();
        let raw = if raw.is_empty() { "." } else { raw };
        let workspace_path = normalize_workspace_path(raw);
        let root = self
            .workspace_roots
            .first()
            .ok_or(ToolPermissionError::WorkspaceRootNotConfigured)?;
        let absolute_input = should_treat_as_host_absolute(raw, root);
        let joined = if absolute_input {
            PathBuf::from(raw)
        } else {
            root.join(&workspace_path)
        };
        let resolved = match joined.canonicalize() {
            Ok(path) => path,
            Err(_) if path_contains_parent_dir(&workspace_path) => {
                return Err(ToolPermissionError::PathOutsideWorkspace {
                    path: raw.to_owned(),
                });
            }
            Err(_) => joined,
        };
        let containing_root = find_containing_workspace_root(&self.workspace_roots, &resolved);
        if containing_root.is_none() && !(self.allow_outside_workspace && absolute_input) {
            return Err(ToolPermissionError::PathOutsideWorkspace {
                path: raw.to_owned(),
            });
        }
        let display_root = containing_root.unwrap_or(root.as_path());
        let display_path = display_path(&resolved);
        Ok(ResolvedToolPath {
            path: resolved,
            display_path,
            display_root: display_root.to_path_buf(),
        })
    }

    pub(crate) fn diagnostic(&self) -> Value {
        json!({
            "workspace_roots": self
                .workspace_roots
                .iter()
                .map(|path| display_path(path))
                .collect::<Vec<_>>(),
            "allow_outside_workspace": self.allow_outside_workspace
        })
    }
}

fn normalize_workspace_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ".".to_owned();
    }
    if has_windows_drive_prefix(trimmed) || is_unc_path(trimmed) {
        return trimmed.to_owned();
    }
    let relative = trimmed.trim_start_matches(['/', '\\']);
    if relative.is_empty() {
        ".".to_owned()
    } else {
        relative.to_owned()
    }
}

fn path_contains_parent_dir(raw: &str) -> bool {
    Path::new(raw)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn is_unc_path(path: &str) -> bool {
    path.starts_with("\\\\") || path.starts_with("//")
}

fn is_absolute_host_path(path: &str) -> bool {
    let trimmed = path.trim();
    has_windows_drive_prefix(trimmed)
        || is_unc_path(trimmed)
        || (!cfg!(windows) && Path::new(trimmed).is_absolute())
}

fn should_treat_as_host_absolute(raw: &str, workspace_root: &Path) -> bool {
    let trimmed = raw.trim();
    if !is_absolute_host_path(trimmed) {
        return false;
    }
    if has_windows_drive_prefix(trimmed) || is_unc_path(trimmed) || cfg!(windows) {
        return true;
    }
    if trimmed == "/" {
        return false;
    }
    let virtual_workspace_path = normalize_workspace_path(trimmed);
    !workspace_root.join(virtual_workspace_path).exists()
}

fn find_containing_workspace_root<'a>(roots: &'a [PathBuf], path: &Path) -> Option<&'a Path> {
    roots
        .iter()
        .map(PathBuf::as_path)
        .find(|root| path.starts_with(root))
}

fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = text.strip_prefix("//?/") {
        rest.to_owned()
    } else {
        text
    }
}

fn extend_workspace_roots_from_env(roots: &mut Vec<PathBuf>) {
    for key in [
        "CODESEEX_WORKSPACE_ROOT",
        "WORKSPACE_ROOT",
        "WORKSPACE_ROOTS",
    ] {
        if let Ok(value) = env::var(key) {
            for part in split_path_list(&value) {
                roots.push(PathBuf::from(part));
            }
        }
    }
}

fn split_path_list(value: &str) -> Vec<String> {
    let separator = if cfg!(windows) || value.contains(';') {
        ';'
    } else {
        ':'
    };
    value
        .split(separator)
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

fn dedupe_existing_dirs(values: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut output: Vec<PathBuf> = Vec::new();
    for value in values {
        let Some(path) = normalize_existing_directory(&value) else {
            continue;
        };
        if !output
            .iter()
            .any(|existing| same_path(existing.as_path(), path.as_path()))
        {
            output.push(path);
        }
    }
    output
}

fn normalize_existing_directory(path: &Path) -> Option<PathBuf> {
    let metadata = fs::metadata(path).ok()?;
    let dir = if metadata.is_dir() {
        path.to_path_buf()
    } else if metadata.is_file() {
        path.parent()?.to_path_buf()
    } else {
        return None;
    };
    Some(fs::canonicalize(&dir).unwrap_or(dir))
}

fn same_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn collect_request_workspace_roots(request: &Value, roots: &mut Vec<PathBuf>) {
    if let Some(items) = request.get("input").and_then(Value::as_array) {
        for item in items.iter().take(20) {
            collect_environment_context_values(Some(item), roots);
        }
    }
}

fn collect_environment_context_values(value: Option<&Value>, roots: &mut Vec<PathBuf>) {
    let Some(value) = value else {
        return;
    };
    match value {
        Value::String(text) if is_trusted_environment_context_text(text) => {
            collect_path_like_text(text, roots);
        }
        Value::String(_) => {}
        Value::Array(items) => {
            for item in items.iter().take(40) {
                collect_environment_context_values(Some(item), roots);
            }
        }
        Value::Object(object) => {
            for child in object.values() {
                collect_environment_context_values(Some(child), roots);
            }
        }
        _ => {}
    }
}

fn collect_path_like_text(text: &str, roots: &mut Vec<PathBuf>) {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(value) = extract_tag_value(trimmed, "cwd")
            .or_else(|| extract_tag_value(trimmed, "root"))
            .or_else(|| extract_key_value_path(trimmed))
        {
            add_path_candidate(value, roots);
        }
    }
}

fn extract_tag_value<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = line.find(&open)? + open.len();
    let end = line[start..].find(&close)? + start;
    Some(line[start..end].trim())
}

fn extract_key_value_path(line: &str) -> Option<&str> {
    let (key, value) = line.split_once([':', '='])?;
    if !is_likely_path_key(key) {
        return None;
    }
    Some(value.trim().trim_matches('"').trim_matches('\''))
}

fn add_path_candidate(value: &str, roots: &mut Vec<PathBuf>) {
    let value = value
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'');
    if is_absolute_host_path(value) {
        roots.push(PathBuf::from(value));
    }
}

fn is_likely_path_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "cwd",
        "workdir",
        "workspace",
        "workspace_root",
        "project_root",
        "current_dir",
        "current_directory",
        "root_dir",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn request_indicates_full_file_access(value: &Value) -> bool {
    request_input_indicates_full_file_access(value.get("input"))
}

fn request_input_indicates_full_file_access(value: Option<&Value>) -> bool {
    let Some(Value::Array(items)) = value else {
        return false;
    };
    items
        .iter()
        .take(20)
        .any(|item| tagged_environment_context_indicates_full_access(Some(item)))
}

fn tagged_environment_context_indicates_full_access(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return false;
    };
    match value {
        Value::String(text) if is_trusted_environment_context_text(text) => {
            access_text_indicates_full_access(text)
        }
        Value::String(_) => false,
        Value::Array(items) => items
            .iter()
            .take(80)
            .any(|item| tagged_environment_context_indicates_full_access(Some(item))),
        Value::Object(object) => object
            .values()
            .any(|child| tagged_environment_context_indicates_full_access(Some(child))),
        _ => false,
    }
}

fn is_trusted_environment_context_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<permissions instructions>")
}

fn access_text_indicates_full_access(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("danger-full-access")
        || text.contains("sandbox_mode\":\"danger")
        || text.contains("sandbox\":\"elevated")
        || text.contains("windows.sandbox\":\"elevated")
        || text.contains("file_system type=\"unrestricted\"")
        || text.contains("filesystem unrestricted")
}

fn env_indicates_full_file_access() -> bool {
    env::var("WORKSPACE_TOOL_FILE_ACCESS")
        .or_else(|_| env::var("CODESEEX_WORKSPACE_FILE_ACCESS"))
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "all" | "full" | "danger-full-access" | "unrestricted"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn request_workspace_root_comes_from_trusted_environment_context_user_item() {
        let root = temp_workspace("env-context-root");
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("inside.txt"), "inside").expect("write file");

        let request = json!({
            "input": [
                {
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!(
                            "<environment_context>\n  <cwd>{}</cwd>\n  <filesystem><workspace_roots><root>{}</root></workspace_roots></filesystem>\n</environment_context>",
                            root.display(),
                            root.display()
                        )
                    }]
                },
                {
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "Please inspect C:\\\\not-the-workspace." }]
                }
            ]
        });

        let context = ToolPermissionContext::from_request(&request);
        let resolved = context.resolve_path("inside.txt").expect("resolve inside");

        assert!(resolved.path.ends_with("inside.txt"));
        assert!(resolved
            .path
            .starts_with(fs::canonicalize(&root).unwrap_or(root.clone())));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_and_instructions_do_not_define_workspace_root() {
        let root = temp_workspace("metadata-spoof-root");
        fs::create_dir_all(&root).expect("create root");

        let request = json!({
            "metadata": { "workspace_root": root.display().to_string() },
            "instructions": format!("<environment_context><cwd>{}</cwd></environment_context>", root.display()),
            "input": [{
                "role": "user",
                "content": [{ "type": "input_text", "text": "list files" }]
            }]
        });

        let context = ToolPermissionContext::from_request(&request);
        let error = context
            .resolve_path(".")
            .expect_err("spoofed roots should be ignored");

        assert!(matches!(
            error,
            ToolPermissionError::WorkspaceRootNotConfigured
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ordinary_user_text_does_not_enable_full_file_access() {
        let root = temp_workspace("ordinary-text-root");
        fs::create_dir_all(&root).expect("create root");

        let request = json!({
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!("cwd: {}\nPlease explain danger-full-access and approval_policy never.", root.display())
                }]
            }]
        });

        let context = ToolPermissionContext::from_request(&request);

        assert!(!context.allow_outside_workspace);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tagged_environment_context_can_enable_full_file_access() {
        let root = temp_workspace("full-access-root");
        fs::create_dir_all(&root).expect("create root");

        let request = json!({
            "input": [{
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": format!(
                        "<environment_context>\n  <cwd>{}</cwd>\n  <filesystem><permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile></filesystem>\n</environment_context>",
                        root.display()
                    )
                }]
            }]
        });

        let context = ToolPermissionContext::from_request(&request);

        assert!(context.allow_outside_workspace);
        let _ = fs::remove_dir_all(root);
    }

    fn temp_workspace(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        env::temp_dir().join(format!("codeseex-{label}-{nanos}"))
    }
}
