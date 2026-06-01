use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 96;
const MAX_DESCRIPTION_LEN: usize = 600;
const MAX_FIELD_COUNT: usize = 32;
const MAX_OPTION_COUNT: usize = 32;
const MAX_VALUE_LEN: usize = 2048;
const MAX_EXECUTION_OUTPUT_BYTES: usize = 256 * 1024;
const DEFAULT_EXECUTION_TIMEOUT_MS: u64 = 20_000;
const MAX_EXECUTION_TIMEOUT_MS: u64 = 120_000;
const TOOL_ASSET_PREFIX: &str = "/tool-assets";

const RESERVED_CONFIG_KEYS: &[&str] = &[
    "AUTO_START",
    "BILLING_FLASH_CACHED_INPUT_CNY",
    "BILLING_FLASH_CACHE_MISS_INPUT_CNY",
    "BILLING_FLASH_OUTPUT_CNY",
    "BILLING_PRO_CACHED_INPUT_CNY",
    "BILLING_PRO_CACHE_MISS_INPUT_CNY",
    "BILLING_PRO_OUTPUT_CNY",
    "CATALOG_MODE",
    "DEEPSEEK_BASE_URL",
    "DEEPSEEK_OFFICIAL_V1_COMPAT",
    "DEEPSEEK_TEMPERATURE_PRESET",
    "DEEPSEEK_THINKING",
    "ENABLED_TOOLS",
    "LOG_RETENTION_DAYS",
    "PROXY_PORT",
    "SHOW_THINKING",
    "UI_CLOSE_BEHAVIOR",
    "UI_LANGUAGE",
    "UI_THEME",
    "UPSTREAM_MODEL_OVERRIDE",
];

#[derive(Debug, Clone)]
struct CommunityToolManifest {
    id: String,
    tool_dir: PathBuf,
    manifest: Value,
}

#[derive(Debug, Clone, Default)]
pub struct CommunityToolSet {
    known_ids: HashSet<String>,
    executable: BTreeMap<String, CommunityExecutableTool>,
}

#[derive(Debug, Clone)]
struct CommunityExecutableTool {
    id: String,
    tool_dir: PathBuf,
    definition: Value,
    execution: CommandExecutionSpec,
    settings: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct CommandExecutionSpec {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    timeout_ms: u64,
}

impl CommunityToolSet {
    pub fn load(
        data_dir: &Path,
        enabled_ids: &[String],
        settings: &BTreeMap<String, String>,
    ) -> Self {
        let enabled = enabled_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut known_ids = HashSet::new();
        let mut executable = BTreeMap::new();
        for manifest in discover_manifests(data_dir) {
            known_ids.insert(manifest.id.clone());
            if !enabled.contains(manifest.id.as_str()) {
                continue;
            }
            if let Some(tool) = manifest.to_executable(settings) {
                executable.insert(tool.id.clone(), tool);
            }
        }
        Self {
            known_ids,
            executable,
        }
    }

    pub fn definitions(&self) -> Vec<Value> {
        self.executable
            .values()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    pub fn is_known_tool(&self, name: &str) -> bool {
        self.known_ids.contains(name)
    }

    pub fn is_executable_tool(&self, name: &str) -> bool {
        self.executable.contains_key(name)
    }

    pub async fn execute(&self, name: &str, arguments: &str) -> Option<Value> {
        let tool = self.executable.get(name)?;
        Some(tool.execute(arguments).await)
    }
}

pub fn list_community_tools(
    data_dir: &Path,
    enabled_ids: &[String],
    settings: &BTreeMap<String, String>,
) -> Vec<Value> {
    let enabled = enabled_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    discover_manifests(data_dir)
        .into_iter()
        .filter_map(|tool| tool.to_api_value(&enabled, settings))
        .collect()
}

pub fn community_tool_config_keys(data_dir: &Path) -> HashSet<String> {
    discover_manifests(data_dir)
        .into_iter()
        .filter_map(|tool| normalize_config_fields(tool.manifest.get("config")).ok())
        .flat_map(|fields| {
            fields
                .into_iter()
                .filter_map(|field| field.get("key").and_then(Value::as_str).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub fn tool_asset_path(
    data_dir: &Path,
    requested_id: &str,
    requested_file: &str,
) -> Option<PathBuf> {
    let id = normalize_tool_id(requested_id)?;
    let file = normalize_asset_file(requested_file)?;
    discover_manifests(data_dir)
        .into_iter()
        .find(|tool| tool.id == id)
        .and_then(|tool| fixed_icon_file(&tool.tool_dir, &file))
}

fn discover_manifests(data_dir: &Path) -> Vec<CommunityToolManifest> {
    let tools_dir = data_dir.join("extension").join("tools");
    let Ok(entries) = fs::read_dir(tools_dir) else {
        return Vec::new();
    };
    let mut dirs = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_dir())
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    dirs.sort_by_key(|path| path.file_name().map(|name| name.to_os_string()));

    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for tool_dir in dirs {
        let manifest_path = tool_dir.join("manifest.json");
        let Ok(text) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if !manifest.is_object() {
            continue;
        }
        let fallback_id = tool_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let Some(id) = manifest
            .get("id")
            .and_then(Value::as_str)
            .and_then(normalize_tool_id)
            .or_else(|| normalize_tool_id(fallback_id))
        else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        output.push(CommunityToolManifest {
            id,
            tool_dir,
            manifest,
        });
    }
    output
}

impl CommunityToolManifest {
    fn to_api_value(
        &self,
        enabled_ids: &HashSet<&str>,
        settings: &BTreeMap<String, String>,
    ) -> Option<Value> {
        let manifest_enabled = self
            .manifest
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let name = self
            .manifest
            .get("name")
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value.trim(), MAX_NAME_LEN))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.id.clone());
        let description = self
            .manifest
            .get("description")
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value.trim(), MAX_DESCRIPTION_LEN))
            .unwrap_or_default();
        let executable = command_execution_spec(self.manifest.get("execution")).is_some();
        let mut object = Map::new();
        object.insert("id".to_owned(), Value::String(self.id.clone()));
        object.insert("name".to_owned(), Value::String(name));
        object.insert("description".to_owned(), Value::String(description));
        object.insert("source".to_owned(), Value::String("community".to_owned()));
        object.insert("system".to_owned(), Value::Bool(false));
        object.insert(
            "configurable".to_owned(),
            Value::Bool(manifest_enabled && executable),
        );
        object.insert("executable".to_owned(), Value::Bool(executable));
        object.insert(
            "enabled".to_owned(),
            Value::Bool(manifest_enabled && executable && enabled_ids.contains(self.id.as_str())),
        );
        object.insert(
            "kind".to_owned(),
            Value::String(
                self.manifest
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(|value| truncate_chars(value.trim(), 48))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "tool".to_owned()),
            ),
        );
        object.insert(
            "version".to_owned(),
            Value::String(
                self.manifest
                    .get("version")
                    .and_then(Value::as_str)
                    .map(|value| truncate_chars(value.trim(), 48))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "1".to_owned()),
            ),
        );
        if let Some(icon_path) = fixed_icon_path(&self.tool_dir, &self.id) {
            object.insert("iconPath".to_owned(), Value::String(icon_path));
        } else if let Some(icon) = self.manifest.get("icon").and_then(Value::as_str) {
            object.insert(
                "icon".to_owned(),
                Value::String(truncate_chars(icon.trim(), 8)),
            );
        }
        object.insert(
            "labels".to_owned(),
            Value::Array(vec![
                json!({ "id": "community", "label": "Community" }),
                if executable {
                    json!({ "id": "executable", "label": "Executable" })
                } else {
                    json!({ "id": "manifest", "label": "Manifest only" })
                },
            ]),
        );
        object.insert(
            "config".to_owned(),
            Value::Array(normalize_config_fields(self.manifest.get("config")).ok()?),
        );
        apply_settings_to_config_fields(&mut object, settings);
        Some(Value::Object(object))
    }

    fn to_executable(
        &self,
        settings: &BTreeMap<String, String>,
    ) -> Option<CommunityExecutableTool> {
        if self
            .manifest
            .get("enabled")
            .and_then(Value::as_bool)
            .is_some_and(|enabled| !enabled)
        {
            return None;
        }
        let execution = command_execution_spec(self.manifest.get("execution"))?;
        let definition = self.model_tool_definition()?;
        let config_keys = normalize_config_fields(self.manifest.get("config"))
            .ok()?
            .into_iter()
            .filter_map(|field| field.get("key").and_then(Value::as_str).map(str::to_owned))
            .collect::<HashSet<_>>();
        let scoped_settings = settings
            .iter()
            .filter(|(key, _)| config_keys.contains(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Some(CommunityExecutableTool {
            id: self.id.clone(),
            tool_dir: self.tool_dir.clone(),
            definition,
            execution,
            settings: scoped_settings,
        })
    }

    fn model_tool_definition(&self) -> Option<Value> {
        let model = self.manifest.get("model").filter(|value| value.is_object());
        let description = first_model_string(
            &[
                model.and_then(|value| value.get("description")),
                self.manifest.get("modelDescription"),
                self.manifest.get("description"),
            ],
            "Community tool executed by CodeSeeX in an isolated child process.",
        );
        let parameters = model
            .and_then(|value| value.get("parameters"))
            .cloned()
            .or_else(|| self.manifest.get("parameters").cloned())
            .or_else(|| self.manifest.get("input_schema").cloned())
            .or_else(|| self.manifest.get("inputSchema").cloned())
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
        Some(json!({
            "type": "function",
            "function": {
                "name": self.id,
                "description": truncate_chars(description.trim(), MAX_DESCRIPTION_LEN),
                "parameters": normalize_schema(parameters)
            }
        }))
    }
}

impl CommunityExecutableTool {
    async fn execute(&self, arguments: &str) -> Value {
        let payload = json!({
            "tool": self.id,
            "arguments": parse_arguments(arguments),
            "raw_arguments": arguments,
            "settings": self.settings,
            "workspace_root": std::env::var("CODESEEX_WORKSPACE_ROOT").ok(),
        });
        let stdin = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_owned());
        let result = self.run_command(&stdin).await;
        match result {
            Ok(output) => output,
            Err(message) => json!({
                "ok": false,
                "tool": self.id,
                "error": "community_tool_failed",
                "message": message
            }),
        }
    }

    async fn run_command(&self, stdin_payload: &str) -> Result<Value, String> {
        let mut command = Command::new(&self.execution.command);
        command
            .args(&self.execution.args)
            .current_dir(&self.tool_dir)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_minimal_process_env(&mut command, &self.id, &self.tool_dir);
        for (key, value) in &self.execution.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to spawn community tool: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_payload.as_bytes())
                .await
                .map_err(|error| format!("failed to write community tool input: {error}"))?;
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "community tool stdout was unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "community tool stderr was unavailable".to_owned())?;
        let stdout_task = tokio::spawn(read_limited_output(stdout));
        let stderr_task = tokio::spawn(read_limited_output(stderr));
        let status = match tokio::time::timeout(
            Duration::from_millis(self.execution.timeout_ms),
            child.wait(),
        )
        .await
        {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                return Err(format!("failed to wait for community tool: {error}"));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(format!(
                    "community tool timed out after {} ms",
                    self.execution.timeout_ms
                ));
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| format!("failed to join stdout reader: {error}"))?;
        let stderr = stderr_task
            .await
            .map_err(|error| format!("failed to join stderr reader: {error}"))?;
        if !status.success() {
            return Err(format!(
                "community tool exited with status {}: {}",
                status,
                truncate_chars(&stderr.text, MAX_DESCRIPTION_LEN)
            ));
        }
        let mut body = stdout.text.trim().to_owned();
        if body.is_empty() {
            body = "{}".to_owned();
        }
        let mut output = serde_json::from_str::<Value>(&body)
            .unwrap_or_else(|_| json!({ "ok": true, "tool": self.id, "text": body }));
        if let Some(object) = output.as_object_mut() {
            object.entry("ok".to_owned()).or_insert(Value::Bool(true));
            object
                .entry("tool".to_owned())
                .or_insert_with(|| Value::String(self.id.clone()));
            if stdout.truncated {
                object.insert("stdout_truncated".to_owned(), Value::Bool(true));
            }
            if stderr.truncated {
                object.insert("stderr_truncated".to_owned(), Value::Bool(true));
            }
            if !stderr.text.trim().is_empty() {
                object.insert(
                    "stderr".to_owned(),
                    Value::String(truncate_chars(stderr.text.trim(), MAX_DESCRIPTION_LEN)),
                );
            }
        }
        Ok(output)
    }
}

#[derive(Debug)]
struct LimitedOutput {
    text: String,
    truncated: bool,
}

fn command_execution_spec(value: Option<&Value>) -> Option<CommandExecutionSpec> {
    let object = value?.as_object()?;
    if object
        .get("type")
        .and_then(Value::as_str)
        .map(|value| value.trim().eq_ignore_ascii_case("command"))
        != Some(true)
    {
        return None;
    }
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let args = object
        .get("args")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(64)
        .filter_map(Value::as_str)
        .map(|value| truncate_chars(value, MAX_VALUE_LEN))
        .collect();
    let env = object
        .get("env")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .iter()
                .filter_map(|(key, value)| {
                    normalize_env_key(key)
                        .and_then(|key| value_to_setting_string(value).map(|value| (key, value)))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let timeout_ms = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_EXECUTION_TIMEOUT_MS)
        .clamp(1_000, MAX_EXECUTION_TIMEOUT_MS);
    Some(CommandExecutionSpec {
        command,
        args,
        env,
        timeout_ms,
    })
}

fn normalize_schema(schema: Value) -> Value {
    if !schema.is_object() {
        return json!({ "type": "object", "properties": {} });
    }
    let mut schema = schema;
    if schema.get("type").is_none() {
        schema["type"] = Value::String("object".to_owned());
    }
    if schema.get("properties").is_none() {
        schema["properties"] = json!({});
    }
    schema
}

fn first_model_string(candidates: &[Option<&Value>], fallback: &str) -> String {
    candidates
        .iter()
        .filter_map(|value| value.and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()))
}

fn apply_minimal_process_env(command: &mut Command, tool_id: &str, tool_dir: &Path) {
    for key in ["PATH", "Path", "HOME", "USERPROFILE", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    for key in ["SystemRoot", "ComSpec", "PATHEXT", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("CODESEEX_TOOL_ID", tool_id);
    command.env("CODESEEX_TOOL_DIR", tool_dir);
    if let Ok(workspace_root) = std::env::var("CODESEEX_WORKSPACE_ROOT") {
        command.env("CODESEEX_WORKSPACE_ROOT", workspace_root);
    }
}

async fn read_limited_output<R>(mut reader: R) -> LimitedOutput
where
    R: AsyncRead + Unpin,
{
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    while let Ok(count) = reader.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let remaining = MAX_EXECUTION_OUTPUT_BYTES.saturating_sub(kept.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let keep = count.min(remaining);
        kept.extend_from_slice(&buffer[..keep]);
        if keep < count {
            truncated = true;
        }
    }
    LimitedOutput {
        text: String::from_utf8_lossy(&kept).to_string(),
        truncated,
    }
}

fn normalize_env_key(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .chars()
        .take(96)
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_config_fields(value: Option<&Value>) -> Result<Vec<Value>, ()> {
    let fields = value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for field in fields.iter().take(MAX_FIELD_COUNT) {
        let Some(source) = field.as_object() else {
            continue;
        };
        let Some(key) = source
            .get("key")
            .and_then(Value::as_str)
            .and_then(normalize_config_key)
        else {
            continue;
        };
        if !seen.insert(key.clone()) {
            continue;
        }
        let field_type = source
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .filter(|value| {
                matches!(
                    value.as_str(),
                    "text"
                        | "textarea"
                        | "password"
                        | "number"
                        | "boolean"
                        | "select"
                        | "segmented"
                )
            })
            .unwrap_or_else(|| "text".to_owned());
        let mut normalized = Map::new();
        normalized.insert("key".to_owned(), Value::String(key));
        normalized.insert("type".to_owned(), Value::String(field_type.clone()));
        copy_short_string(source, &mut normalized, "label", MAX_NAME_LEN);
        copy_short_string(source, &mut normalized, "labelKey", MAX_NAME_LEN);
        copy_short_string(source, &mut normalized, "description", MAX_DESCRIPTION_LEN);
        copy_short_string(source, &mut normalized, "descriptionKey", MAX_NAME_LEN);
        copy_short_string(source, &mut normalized, "placeholder", MAX_NAME_LEN);
        copy_short_string(source, &mut normalized, "placeholderKey", MAX_NAME_LEN);
        if let Some(default_value) = source.get("defaultValue").and_then(value_to_setting_string) {
            normalized.insert("defaultValue".to_owned(), Value::String(default_value));
        }
        if matches!(field_type.as_str(), "select" | "segmented") {
            normalized.insert(
                "options".to_owned(),
                Value::Array(normalize_options(source.get("options"))),
            );
        }
        output.push(Value::Object(normalized));
    }
    Ok(output)
}

fn normalize_options(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .take(MAX_OPTION_COUNT)
        .filter_map(|option| {
            let source = option.as_object()?;
            let value = source
                .get("value")
                .and_then(value_to_setting_string)
                .map(|value| truncate_chars(value.trim(), MAX_NAME_LEN))
                .filter(|value| !value.is_empty())?;
            let mut normalized = Map::new();
            normalized.insert("value".to_owned(), Value::String(value));
            copy_short_string(source, &mut normalized, "label", MAX_NAME_LEN);
            copy_short_string(source, &mut normalized, "labelKey", MAX_NAME_LEN);
            Some(Value::Object(normalized))
        })
        .collect()
}

fn apply_settings_to_config_fields(
    object: &mut Map<String, Value>,
    settings: &BTreeMap<String, String>,
) {
    let Some(fields) = object.get_mut("config").and_then(Value::as_array_mut) else {
        return;
    };
    for field in fields {
        let Some(key) = field.get("key").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = settings.get(key) else {
            continue;
        };
        if let Some(field_object) = field.as_object_mut() {
            field_object.insert("value".to_owned(), Value::String(value.clone()));
        }
    }
}

fn copy_short_string(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    key: &str,
    max: usize,
) {
    if let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .map(|value| truncate_chars(value.trim(), max))
        .filter(|value| !value.is_empty())
    {
        target.insert(key.to_owned(), Value::String(value));
    }
}

pub fn value_to_setting_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(truncate_chars(text, MAX_VALUE_LEN)),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

pub fn normalize_config_key(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .chars()
        .take(96)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    if normalized.is_empty() || RESERVED_CONFIG_KEYS.contains(&normalized.as_str()) {
        None
    } else {
        Some(normalized)
    }
}

fn normalize_tool_id(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .take(MAX_ID_LEN)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_owned();
    (!normalized.is_empty()).then_some(normalized)
}

fn fixed_icon_path(tool_dir: &Path, tool_id: &str) -> Option<String> {
    if fixed_icon_file(tool_dir, "icon.svg").is_some() {
        return Some(format!("{TOOL_ASSET_PREFIX}/{tool_id}/icon.svg"));
    }
    if fixed_icon_file(tool_dir, "icon.png").is_some() {
        return Some(format!("{TOOL_ASSET_PREFIX}/{tool_id}/icon.png"));
    }
    None
}

fn fixed_icon_file(tool_dir: &Path, file: &str) -> Option<PathBuf> {
    let asset_dir = tool_dir.join("assets");
    let candidate = asset_dir.join(file);
    if !candidate.exists() {
        return None;
    }
    let asset_dir = asset_dir.canonicalize().ok()?;
    let candidate = candidate.canonicalize().ok()?;
    candidate.starts_with(&asset_dir).then_some(candidate)
}

fn normalize_asset_file(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "icon.svg" => Some("icon.svg".to_owned()),
        "icon.png" => Some("icon.png".to_owned()),
        _ => None,
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discovers_manifest_without_enabling_execution() {
        let root = temp_dir("community-discovery");
        let tool_dir = root.join("extension").join("tools").join("Fancy Tool");
        fs::create_dir_all(tool_dir.join("assets")).expect("create tool dir");
        fs::write(tool_dir.join("assets").join("icon.svg"), "<svg/>").expect("write icon");
        fs::write(
            tool_dir.join("manifest.json"),
            serde_json::to_string(&json!({
                "id": "Fancy Tool!",
                "name": "Fancy Tool",
                "description": "A community tool.",
                "config": [
                    { "key": "FANCY_MODE", "type": "select", "defaultValue": "safe", "options": [{ "value": "safe", "label": "Safe" }] },
                    { "key": "PROXY_PORT", "type": "text" }
                ]
            }))
            .expect("manifest json"),
        )
        .expect("write manifest");
        let mut settings = BTreeMap::new();
        settings.insert("FANCY_MODE".to_owned(), "safe".to_owned());
        let tools = list_community_tools(&root, &[], &settings);
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("id").and_then(Value::as_str),
            Some("fancy_tool")
        );
        assert_eq!(
            tools[0].get("source").and_then(Value::as_str),
            Some("community")
        );
        assert_eq!(
            tools[0].get("enabled").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            tools[0].get("configurable").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            tools[0].get("executable").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            tools[0].get("iconPath").and_then(Value::as_str),
            Some("/tool-assets/fancy_tool/icon.svg")
        );
        let config = tools[0].get("config").and_then(Value::as_array).unwrap();
        assert_eq!(config.len(), 1);
        assert_eq!(
            config[0].get("key").and_then(Value::as_str),
            Some("FANCY_MODE")
        );
        assert_eq!(config[0].get("value").and_then(Value::as_str), Some("safe"));
        let asset_path = tool_asset_path(&root, "fancy_tool", "icon.svg").expect("asset path");
        assert!(asset_path.exists());
        assert_eq!(
            asset_path.file_name().and_then(|value| value.to_str()),
            Some("icon.svg")
        );
        assert_eq!(
            asset_path
                .parent()
                .and_then(|value| value.file_name())
                .and_then(|value| value.to_str()),
            Some("assets")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_malformed_or_duplicate_manifests() {
        let root = temp_dir("community-malformed");
        let first = root.join("extension").join("tools").join("a");
        let second = root.join("extension").join("tools").join("b");
        let broken = root.join("extension").join("tools").join("broken");
        fs::create_dir_all(&first).expect("create first");
        fs::create_dir_all(&second).expect("create second");
        fs::create_dir_all(&broken).expect("create broken");
        fs::write(
            first.join("manifest.json"),
            r#"{ "id": "same", "name": "A" }"#,
        )
        .expect("write first");
        fs::write(
            second.join("manifest.json"),
            r#"{ "id": "same", "name": "B" }"#,
        )
        .expect("write second");
        fs::write(broken.join("manifest.json"), "{").expect("write broken");
        let tools = list_community_tools(&root, &["same".to_owned()], &BTreeMap::new());
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].get("name").and_then(Value::as_str), Some("A"));
        assert_eq!(
            tools[0].get("enabled").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            tools[0].get("configurable").and_then(Value::as_bool),
            Some(false)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn enabled_command_manifest_becomes_executable_definition() {
        let root = temp_dir("community-executable");
        let tool_dir = root.join("extension").join("tools").join("adder");
        fs::create_dir_all(&tool_dir).expect("create tool");
        fs::write(
            tool_dir.join("manifest.json"),
            serde_json::to_string(&json!({
                "id": "adder",
                "name": "Adder",
                "description": "Add numbers.",
                "model": {
                    "description": "Add two integers.",
                    "parameters": {
                        "type": "object",
                        "properties": { "a": { "type": "integer" }, "b": { "type": "integer" } },
                        "required": ["a", "b"]
                    }
                },
                "execution": {
                    "type": "command",
                    "command": "node",
                    "args": ["tool.js"]
                }
            }))
            .expect("manifest json"),
        )
        .expect("write manifest");
        let set = CommunityToolSet::load(&root, &["adder".to_owned()], &BTreeMap::new());
        assert!(set.is_known_tool("adder"));
        assert!(set.is_executable_tool("adder"));
        let definitions = set.definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0]
                .pointer("/function/name")
                .and_then(Value::as_str),
            Some("adder")
        );
        let tools = list_community_tools(&root, &["adder".to_owned()], &BTreeMap::new());
        assert_eq!(tools[0].get("enabled").and_then(Value::as_bool), Some(true));
        assert_eq!(
            tools[0].get("configurable").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            tools[0].get("executable").and_then(Value::as_bool),
            Some(true)
        );
        let _ = fs::remove_dir_all(root);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("codeseex-next-{label}-{nanos}"))
    }
}
