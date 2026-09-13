use codeseex_core::catalog::{
    app_server_model_list_from_catalog, build_codeseex_catalog_from_document,
    AppServerModelListParams,
};
use codeseex_core::AppConfig;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, Command as TokioCommand};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_CODEX_DEBUG_PORT: u16 = 9222;
const CDP_HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(8);
const CODEX_RUNTIME_RPC_TIMEOUT: Duration = Duration::from_secs(8);
const CODEX_RUNTIME_HTTP_TIMEOUT: Duration = Duration::from_millis(700);
const CODEX_LAUNCH_INJECT_ATTEMPTS: usize = 60;
const CODEX_LAUNCH_INJECT_INTERVAL: Duration = Duration::from_millis(500);
const CODEX_PACKAGE_IDENTITIES: &[&str] = &["OpenAI.Codex", "OpenAI.CodexBeta"];
#[cfg(target_os = "windows")]
const WINDOWS_CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Deserialize)]
struct CdpTarget {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "type")]
    target_type: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CodexProcess {
    pid: u32,
    name: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexProcessJson {
    pid: Option<u32>,
    name: Option<String>,
    path: Option<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone)]
struct WindowsPackagedCodexApp {
    app_user_model_id: String,
    package_full_name: Option<String>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Deserialize)]
struct WindowsPackagedCodexAppJson {
    #[serde(default, rename = "appUserModelId")]
    app_user_model_id: Option<String>,
    #[serde(default, rename = "packageFullName")]
    package_full_name: Option<String>,
}

pub(crate) fn default_debug_port() -> u16 {
    std::env::var("CODESEEX_CODEX_DEBUG_PORT")
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_CODEX_DEBUG_PORT)
}

pub(crate) fn debug_port_from_values(query: Option<&Value>, body: Option<&Value>) -> Option<u16> {
    body.and_then(debug_port_from_value)
        .or_else(|| query.and_then(debug_port_from_value))
}

fn debug_port_from_value(value: &Value) -> Option<u16> {
    value
        .get("debugPort")
        .or_else(|| value.get("debug_port"))
        .and_then(|value| {
            value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .or_else(|| value.as_str().and_then(|text| text.parse::<u16>().ok()))
        })
}

pub(crate) fn codex_model_catalog_value(config: &AppConfig) -> Value {
    let document = config.catalog_document();
    let catalog = build_codeseex_catalog_from_document(&document);
    let app_server = app_server_model_list_from_catalog(
        &catalog,
        AppServerModelListParams {
            cursor: None,
            limit: Some(500),
            include_hidden: Some(true),
        },
    );
    let models = app_server
        .data
        .iter()
        .map(|model| model.model.clone())
        .collect::<Vec<_>>();
    let default_model = app_server
        .data
        .iter()
        .find(|model| model.is_default)
        .or_else(|| {
            app_server
                .data
                .iter()
                .find(|model| model.model == document.default_slug())
        })
        .or_else(|| app_server.data.first())
        .map(|model| model.model.clone())
        .unwrap_or_else(|| document.default_slug().to_owned());

    json!({
        "status": if models.is_empty() { "not_configured" } else { "ok" },
        "path": config.catalog_path().to_string_lossy(),
        "model": default_model.clone(),
        "model_provider": "custom",
        "provider_name": document.provider_name,
        "catalog_revision": document.revision,
        "catalog_source": config.catalog_source_label(),
        "default_model": default_model,
        "models": models,
        "sources": [{
            "id": "codeseex:builtin-catalog",
            "type": "model_catalog_json",
            "name": "CodeSeeX built-in catalog",
            "path": config.catalog_path().to_string_lossy(),
            "status": "ok",
            "models": app_server.data.len()
        }],
        "responses_api": {
            "status": "supported",
            "endpoint": format!("{}/responses", config.proxy_base_url()),
            "message": ""
        },
        "appServer": app_server
    })
}

pub(crate) async fn verify_runtime_catalog(config: &AppConfig) -> Value {
    let started = Instant::now();
    let catalog = codex_model_catalog_value(config);
    let expected_models = catalog
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected_path = config.catalog_path();

    match run_codex_runtime_probe(&expected_path, &expected_models).await {
        Ok(probe) => runtime_catalog_diagnostic_from_probe(
            &expected_path,
            &expected_models,
            probe,
            started.elapsed(),
        ),
        Err(error) => json!({
            "status": "unavailable",
            "issue": "app_server_unavailable",
            "method": "codex_app_server",
            "message": compact_message(&error.to_string(), 500),
            "expected_catalog_path": expected_path.to_string_lossy(),
            "expected_models": expected_models,
            "duration_ms": started.elapsed().as_millis() as u64,
            "startup_only_catalog": true,
            "proves_current_open_window": false
        }),
    }
}

#[derive(Debug)]
struct CodexRuntimeProbe {
    cli_path: String,
    transport: String,
    port: Option<u16>,
    config_model: Option<String>,
    config_model_provider: Option<String>,
    config_catalog_path: Option<String>,
    layers: Vec<Value>,
    runtime_models: Vec<String>,
    config_warnings: Vec<Value>,
}

async fn run_codex_runtime_probe(
    expected_path: &std::path::Path,
    expected_models: &[String],
) -> anyhow::Result<CodexRuntimeProbe> {
    let cli = find_codex_cli()
        .ok_or_else(|| anyhow::anyhow!("Codex CLI was not found on PATH or in npm globals"))?;
    match probe_codex_app_server_stdio(&cli, expected_path, expected_models).await {
        Ok(probe) => Ok(probe),
        Err(stdio_error) => {
            let port = free_loopback_port()?;
            let mut child = spawn_codex_app_server_ws(&cli, port).await?;
            let result = async {
                wait_codex_app_server_ready(port, &mut child).await?;
                probe_codex_app_server_ws(port, &cli, expected_path, expected_models).await
            }
            .await;
            terminate_codex_app_server(&mut child).await;
            result.map_err(|ws_error| {
                anyhow::anyhow!(
                    "Codex app-server verification failed over stdio and websocket. stdio: {stdio_error}; websocket: {ws_error}"
                )
            })
        }
    }
}

async fn probe_codex_app_server_stdio(
    cli: &std::path::Path,
    expected_path: &std::path::Path,
    _expected_models: &[String],
) -> anyhow::Result<CodexRuntimeProbe> {
    let mut child = spawn_codex_app_server_stdio(cli).await?;
    let result = async {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Codex app-server stdio stdin was unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Codex app-server stdio stdout was unavailable"))?;
        let mut lines = BufReader::new(stdout).lines();
        let mut warnings = Vec::new();

        app_server_stdio_request(
            &mut stdin,
            &mut lines,
            1,
            "initialize",
            json!({
                "clientInfo": {
                    "name": "codeseex-catalog-probe",
                    "title": "CodeSeeX Catalog Probe",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {
                    "experimentalApi": true
                }
            }),
            &mut warnings,
        )
        .await?;
        app_server_stdio_notification(&mut stdin, "initialized").await?;
        let config_result = app_server_stdio_request(
            &mut stdin,
            &mut lines,
            2,
            "config/read",
            json!({
                "cwd": std::env::current_dir()
                    .unwrap_or_else(|_| expected_path.parent().unwrap_or(expected_path).to_path_buf())
                    .to_string_lossy(),
                "includeLayers": true
            }),
            &mut warnings,
        )
        .await?;
        let model_list_result = app_server_stdio_request(
            &mut stdin,
            &mut lines,
            3,
            "model/list",
            json!({
                "includeHidden": true,
                "limit": 500
            }),
            &mut warnings,
        )
        .await?;

        Ok(CodexRuntimeProbe {
            cli_path: cli.to_string_lossy().into_owned(),
            transport: "stdio".to_owned(),
            port: None,
            config_model: nested_string(&config_result, &["config", "model"]),
            config_model_provider: nested_string(&config_result, &["config", "model_provider"]),
            config_catalog_path: nested_string(&config_result, &["config", "model_catalog_json"]),
            layers: runtime_config_layer_summaries(&config_result),
            runtime_models: runtime_model_names(&model_list_result),
            config_warnings: warnings,
        })
    }
    .await;
    terminate_codex_app_server(&mut child).await;
    result
}

async fn app_server_stdio_notification(
    stdin: &mut tokio::process::ChildStdin,
    method: &str,
) -> anyhow::Result<()> {
    stdin
        .write_all(format!("{}\n", json!({ "method": method })).as_bytes())
        .await?;
    stdin.flush().await?;
    Ok(())
}

async fn app_server_stdio_request(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    method: &str,
    params: Value,
    warnings: &mut Vec<Value>,
) -> anyhow::Result<Value> {
    stdin
        .write_all(
            format!(
                "{}\n",
                json!({
                    "id": id,
                    "method": method,
                    "params": params
                })
            )
            .as_bytes(),
        )
        .await?;
    stdin.flush().await?;
    tokio::time::timeout(
        CODEX_RUNTIME_RPC_TIMEOUT,
        wait_app_server_stdio_response(lines, id, method, warnings),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Codex app-server stdio {method} timed out"))?
}

async fn wait_app_server_stdio_response(
    lines: &mut Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    method: &str,
    warnings: &mut Vec<Value>,
) -> anyhow::Result<Value> {
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)?;
        collect_config_warning(&value, warnings);
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server stdio {method} failed: {error}");
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
    anyhow::bail!("Codex app-server stdio ended before {method} response")
}

async fn spawn_codex_app_server_stdio(cli: &std::path::Path) -> anyhow::Result<Child> {
    let mut command = codex_app_server_command(cli, "stdio://");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    command.spawn().map_err(Into::into)
}

async fn spawn_codex_app_server_ws(cli: &std::path::Path, port: u16) -> anyhow::Result<Child> {
    let listen = format!("ws://127.0.0.1:{port}");
    let mut command = codex_app_server_command(cli, &listen);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    command.spawn().map_err(Into::into)
}

#[allow(clippy::needless_return)]
fn codex_app_server_command(cli: &std::path::Path, listen: &str) -> TokioCommand {
    if cli
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("js"))
        .unwrap_or(false)
    {
        let mut command = TokioCommand::new(node_executable_for_codex_script(cli));
        command
            .arg(cli)
            .arg("app-server")
            .arg("--listen")
            .arg(listen);
        return command;
    }

    #[cfg(target_os = "windows")]
    {
        if cli
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("ps1"))
            .unwrap_or(false)
        {
            let mut command = TokioCommand::new("powershell");
            command
                .arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(cli)
                .arg("app-server")
                .arg("--listen")
                .arg(listen);
            return command;
        }
        let script = windows_command_line_arguments(&[
            cli.to_string_lossy().into_owned(),
            "app-server".to_owned(),
            "--listen".to_owned(),
            listen.to_owned(),
        ]);
        let mut command = TokioCommand::new("cmd");
        command.args(["/D", "/C", &format!("call {script}")]);
        return command;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = TokioCommand::new(cli);
        command.args(["app-server", "--listen", listen]);
        command
    }
}

fn node_executable_for_codex_script(script: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(npm_dir) = script.ancestors().find(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(|name| name.eq_ignore_ascii_case("npm"))
                .unwrap_or(false)
        }) {
            let node = npm_dir.join("node.exe");
            if node.is_file() {
                return node;
            }
        }
    }
    PathBuf::from("node")
}

async fn probe_codex_app_server_ws(
    port: u16,
    cli: &std::path::Path,
    expected_path: &std::path::Path,
    _expected_models: &[String],
) -> anyhow::Result<CodexRuntimeProbe> {
    let url = format!("ws://127.0.0.1:{port}");
    let (mut socket, _) = tokio::time::timeout(
        CODEX_RUNTIME_RPC_TIMEOUT,
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Codex app-server websocket connect timed out"))??;
    let mut warnings = Vec::new();

    app_server_request(
        &mut socket,
        1,
        "initialize",
        json!({
            "clientInfo": {
                "name": "codeseex-catalog-probe",
                "title": "CodeSeeX Catalog Probe",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {
                "experimentalApi": true
            }
        }),
        &mut warnings,
    )
    .await?;
    app_server_notification(&mut socket, "initialized").await?;
    let config_result = app_server_request(
        &mut socket,
        2,
        "config/read",
        json!({
            "cwd": std::env::current_dir()
                .unwrap_or_else(|_| expected_path.parent().unwrap_or(expected_path).to_path_buf())
                .to_string_lossy(),
            "includeLayers": true
        }),
        &mut warnings,
    )
    .await?;
    let model_list_result = app_server_request(
        &mut socket,
        3,
        "model/list",
        json!({
            "includeHidden": true,
            "limit": 500
        }),
        &mut warnings,
    )
    .await?;
    let _ = socket.close(None).await;

    Ok(CodexRuntimeProbe {
        cli_path: cli.to_string_lossy().into_owned(),
        transport: "websocket".to_owned(),
        port: Some(port),
        config_model: nested_string(&config_result, &["config", "model"]),
        config_model_provider: nested_string(&config_result, &["config", "model_provider"]),
        config_catalog_path: nested_string(&config_result, &["config", "model_catalog_json"]),
        layers: runtime_config_layer_summaries(&config_result),
        runtime_models: runtime_model_names(&model_list_result),
        config_warnings: warnings,
    })
}

fn runtime_catalog_diagnostic_from_probe(
    expected_path: &std::path::Path,
    expected_models: &[String],
    probe: CodexRuntimeProbe,
    elapsed: Duration,
) -> Value {
    let catalog_path_matches = probe
        .config_catalog_path
        .as_deref()
        .map(|path| paths_match(path, expected_path))
        .unwrap_or(false);
    let runtime_model_set = probe
        .runtime_models
        .iter()
        .map(|model| model.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let missing_models = expected_models
        .iter()
        .filter(|model| !runtime_model_set.contains(model.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let model_list_verified = missing_models.is_empty();
    let (status, issue) = if !model_list_verified {
        ("error", "runtime_model_list_missing_catalog_models")
    } else {
        ("ok", "none")
    };
    let path_note =
        if model_list_verified && !catalog_path_matches && probe.config_catalog_path.is_some() {
            Some("runtime_catalog_path_differs")
        } else {
            None
        };

    json!({
        "status": status,
        "issue": issue,
        "path_note": path_note,
        "method": "codex_app_server",
        "transport": probe.transport,
        "cli_path": probe.cli_path,
        "port": probe.port,
        "config": {
            "model": probe.config_model,
            "model_provider": probe.config_model_provider,
            "model_catalog_json": probe.config_catalog_path,
            "catalog_path_matches": catalog_path_matches,
            "layers": probe.layers
        },
        "model_list": {
            "verified": model_list_verified,
            "model_count": probe.runtime_models.len(),
            "models": probe.runtime_models,
            "expected_models": expected_models,
            "missing_expected_models": missing_models
        },
        "config_warnings": probe.config_warnings,
        "duration_ms": elapsed.as_millis() as u64,
        "startup_only_catalog": true,
        "proves_current_open_window": false
    })
}

async fn app_server_notification(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    method: &str,
) -> anyhow::Result<()> {
    socket
        .send(Message::Text(json!({ "method": method }).to_string()))
        .await?;
    Ok(())
}

async fn app_server_request(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    params: Value,
    warnings: &mut Vec<Value>,
) -> anyhow::Result<Value> {
    socket
        .send(Message::Text(
            json!({
                "id": id,
                "method": method,
                "params": params
            })
            .to_string(),
        ))
        .await?;

    tokio::time::timeout(
        CODEX_RUNTIME_RPC_TIMEOUT,
        wait_app_server_response(socket, id, method, warnings),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Codex app-server {method} timed out"))?
}

async fn wait_app_server_response(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    warnings: &mut Vec<Value>,
) -> anyhow::Result<Value> {
    while let Some(message) = socket.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => {
                anyhow::bail!("Codex app-server websocket closed before {method} response")
            }
            Message::Frame(_) => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        collect_config_warning(&value, warnings);
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("Codex app-server {method} failed: {error}");
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
    anyhow::bail!("Codex app-server websocket ended before {method} response")
}

fn collect_config_warning(value: &Value, warnings: &mut Vec<Value>) {
    if value.get("method").and_then(Value::as_str) != Some("configWarning") {
        return;
    }
    let params = value.get("params").unwrap_or(&Value::Null);
    warnings.push(json!({
        "summary": nested_string(params, &["summary"]).map(|value| compact_message(&value, 240)),
        "details": nested_string(params, &["details"]).map(|value| compact_message(&value, 360)),
        "path": nested_string(params, &["path"]).map(|value| compact_message(&value, 260))
    }));
}

fn runtime_config_layer_summaries(config_result: &Value) -> Vec<Value> {
    config_result
        .get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|layer| {
            json!({
                "name": layer.get("name").cloned().unwrap_or(Value::Null),
                "version": layer.get("version").cloned().unwrap_or(Value::Null),
                "has_model_catalog_json": layer
                    .get("config")
                    .and_then(|config| config.get("model_catalog_json"))
                    .is_some(),
                "model_catalog_json": nested_string(layer, &["config", "model_catalog_json"])
            })
        })
        .collect()
}

fn runtime_model_names(model_list_result: &Value) -> Vec<String> {
    let mut names = Vec::<String>::new();
    if let Some(items) = model_list_result.get("data").and_then(Value::as_array) {
        for item in items {
            let Some(name) = item
                .get("model")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !names.iter().any(|existing| existing == name) {
                names.push(name.to_owned());
            }
        }
    }
    names
}

fn nested_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn compact_message(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    compact
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>()
        + "..."
}

fn paths_match(candidate: &str, expected: &std::path::Path) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let candidate_path = std::path::PathBuf::from(candidate);
    if candidate_path == expected {
        return true;
    }
    let candidate_text = normalize_path_for_compare(&candidate_path);
    let expected_text = normalize_path_for_compare(expected);
    !candidate_text.is_empty() && candidate_text == expected_text
}

fn normalize_path_for_compare(path: &std::path::Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut text = canonical.to_string_lossy().replace('\\', "/");
    while text.contains("//") {
        text = text.replace("//", "/");
    }
    #[cfg(target_os = "windows")]
    {
        text = text.to_ascii_lowercase();
    }
    text.trim_end_matches('/').to_owned()
}

fn free_loopback_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

async fn wait_codex_app_server_ready(port: u16, child: &mut Child) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(CODEX_RUNTIME_HTTP_TIMEOUT)
        .build()?;
    let deadline = Instant::now() + CODEX_RUNTIME_READY_TIMEOUT;
    let url = format!("http://127.0.0.1:{port}/readyz");
    let mut last_error = "not ready yet".to_owned();
    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("Codex app-server did not become ready on {url}: {last_error}");
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("Codex app-server exited before ready: {status}");
        }
        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => last_error = format!("readyz returned {}", response.status()),
            Err(error) => last_error = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn terminate_codex_app_server(child: &mut Child) {
    #[cfg(target_os = "windows")]
    if let Some(pid) = child.id() {
        let _ = TokioCommand::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

fn find_codex_cli() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    for key in ["CODESEEX_CODEX_CLI", "CODEX_CLI", "CODEX_BIN"] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
            candidates.push(path);
        }
    }
    append_codex_cli_platform_candidates(&mut candidates);
    dedup_paths_preserve_order(&mut candidates);
    candidates.into_iter().find(|path| path.is_file())
}

fn append_codex_cli_platform_candidates(candidates: &mut Vec<PathBuf>) {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            candidates.push(
                appdata
                    .join("npm")
                    .join("node_modules")
                    .join("@openai")
                    .join("codex")
                    .join("bin")
                    .join("codex.js"),
            );
            candidates.push(appdata.join("npm").join("codex.cmd"));
            candidates.push(appdata.join("npm").join("codex.exe"));
            candidates.push(appdata.join("npm").join("codex.ps1"));
        }
        for dir in std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            candidates.push(dir.join("codex.cmd"));
            candidates.push(dir.join("codex.exe"));
            candidates.push(dir.join("codex.bat"));
            candidates.push(dir.join("codex.ps1"));
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(home) = dirs_next::home_dir() {
            candidates.push(home.join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
        candidates.push(PathBuf::from("/usr/bin/codex"));
        for dir in std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .unwrap_or_default()
        {
            candidates.push(dir.join("codex"));
        }
    }
}

pub(crate) fn renderer_inject_script(catalog: &Value) -> String {
    const TEMPLATE: &str = r#"
(async () => {
  const catalog = __CODESEEX_MODEL_CATALOG_JSON__;
  const VERSION = "codeseex-model-catalog-unlock-v3:" + String((catalog && catalog.catalog_revision) || "builtin");
  const LEGACY_VERSION_PREFIX = "codeseex-model-catalog-unlock-v2";
  const state = window.__codeseexModelCatalogUnlock = window.__codeseexModelCatalogUnlock || {};
  state.version = VERSION;
  state.catalog = catalog;
  state.failures = state.failures || [];

  function rememberFailure(error) {
    try {
      state.failures.push(String(error && (error.stack || error.message) || error));
      if (state.failures.length > 20) state.failures.splice(0, state.failures.length - 20);
    } catch {}
  }

  /// Everything this profile installs is remembered here so the whole injection
  /// can be torn down again. Codex must never keep a hook we added.
  const hooks = {
    fetch: null,
    statsigClient: null,
    statsigOriginal: null,
    statsigValue: undefined,
    hadStatsig: false,
    interval: 0,
    installed: false
  };

  /// True once the renderer has actually received the widened model list, either
  /// through the statsig gate or through a rewritten model response.
  function injectionDelivered() {
    return Boolean(
      (state.dynamicConfigPatch && state.dynamicConfigPatch.applied)
      || (state.fetchPatch && state.fetchPatch.patched)
    );
  }

  /// Removes every hook this profile installed and leaves nothing behind. The
  /// injected status object stays for diagnostics, but it holds no behaviour.
  function uninstallInjection(reason) {
    if (!hooks.installed) {
      state.uninstallReason = state.uninstallReason || reason;
      return;
    }
    try {
      if (hooks.fetch) window.fetch = hooks.fetch;
    } catch (error) {
      rememberFailure(error);
    }
    try {
      if (hooks.statsigClient && hooks.statsigOriginal) {
        hooks.statsigClient.getDynamicConfig = hooks.statsigOriginal;
      }
      if (hooks.statsigClient) delete hooks.statsigClient.__codeseexDynamicConfigPatch;
    } catch (error) {
      rememberFailure(error);
    }
    try {
      delete window.__STATSIG__;
      if (hooks.hadStatsig) {
        Object.defineProperty(window, "__STATSIG__", {
          value: hooks.statsigValue,
          writable: true,
          configurable: true,
          enumerable: true
        });
      }
    } catch (error) {
      rememberFailure(error);
    }
    try {
      if (hooks.interval) window.clearInterval(hooks.interval);
    } catch (error) {
      rememberFailure(error);
    }
    // These two markers are the "already patched" sentinels for the fetch hook
    // and the statsig trap. They must leave together with the hooks: if they
    // survive, a later injection at the same catalog revision early-returns and
    // reports success without installing anything.
    try {
      delete window.__codeseexModelFetchPatchVersion;
      delete window.__codeseexStatsigTrapVersion;
    } catch (error) {
      rememberFailure(error);
    }
    hooks.fetch = null;
    hooks.statsigClient = null;
    hooks.statsigOriginal = null;
    hooks.interval = 0;
    hooks.installed = false;
    state.installed = false;
    state.uninstalled = true;
    state.uninstallReason = reason;
  }

  /// The v2 profile is no longer shipped, but a renderer it injected earlier can
  /// still be alive after an app update: it left a statsig wrapper (which built a
  /// clone and rebound `get`), a DOM observer, a click listener and an interval
  /// behind. Undo everything that can be undone before installing v3.
  function cleanupPreviousInjection() {
    const removed = [];
    try {
      if (window.__codeseexModelCatalogObserver) {
        window.__codeseexModelCatalogObserver.disconnect();
        removed.push("dom-observer");
      }
      delete window.__codeseexModelCatalogObserver;
      if (window.__codeseexModelCatalogClickHandler) {
        document.removeEventListener("click", window.__codeseexModelCatalogClickHandler, true);
        removed.push("click-listener");
      }
      delete window.__codeseexModelCatalogClickHandler;
      if (window.__codeseexModelCatalogInterval) {
        window.clearInterval(window.__codeseexModelCatalogInterval);
        removed.push("refresh-interval");
      }
      delete window.__codeseexModelCatalogInterval;
    } catch (error) {
      rememberFailure(error);
    }
    try {
      for (const client of statsigClients()) {
        if (typeof client.__codeseexOriginalGetDynamicConfig === "function") {
          client.getDynamicConfig = client.__codeseexOriginalGetDynamicConfig;
          removed.push("statsig-clone-wrapper");
        }
        delete client.__codeseexOriginalGetDynamicConfig;
        const marker = client.__codeseexDynamicConfigPatch;
        if (typeof marker === "string" && marker.startsWith(LEGACY_VERSION_PREFIX)) {
          delete client.__codeseexDynamicConfigPatch;
        }
      }
    } catch (error) {
      rememberFailure(error);
    }
    if (window.__codeseexModelMessagePatch) {
      // v2 added an anonymous capture-phase `message` listener and never kept a
      // reference, so it cannot be removed from a live renderer; it goes away the
      // next time the renderer reloads.
      removed.push("stale-window-message-listener");
      delete window.__codeseexModelMessagePatch;
    }
    for (const key of ["assetProbe", "hostConfig", "modules", "appServerPatch", "messagePatch", "shortLabelPatch"]) {
      if (key in state) {
        delete state[key];
        removed.push(key);
      }
    }
    state.cleanup = removed;
    return removed;
  }

  function unique(values) {
    const out = [];
    for (const value of values) {
      const text = String(value || "").trim();
      if (text && !out.includes(text)) out.push(text);
    }
    return out;
  }

  function appServerModels() {
    const data = catalog && catalog.appServer && Array.isArray(catalog.appServer.data) ? catalog.appServer.data : [];
    return data.filter((model) => model && typeof model === "object");
  }

  function modelNames() {
    return unique([
      catalog && catalog.default_model,
      catalog && catalog.model,
      ...(Array.isArray(catalog && catalog.models) ? catalog.models : []),
      ...appServerModels().map((model) => model.model || model.id || model.slug)
    ]);
  }

  function sourceForModel(name) {
    return appServerModels().find((model) => (model.model || model.id || model.slug) === name) || null;
  }

  function normalizeModelDisplayName(value) {
    return String(value || "").trim().replace(/^DeepSeek-V4\b/, "DeepSeek V4");
  }

  function displayNameForModel(name) {
    const source = sourceForModel(name);
    return normalizeModelDisplayName(source && (source.displayName || source.name || source.display_name)) || name;
  }

  function shortDisplayNameForModel(name, displayName) {
    const source = sourceForModel(name);
    const explicit = source && (source.shortDisplayName || source.shortName || source.short_display_name || source.compactDisplayName);
    if (explicit && String(explicit).trim()) return String(explicit).trim();
    const full = normalizeModelDisplayName(displayName || "");
    if (full.startsWith("DeepSeek V4 ")) return full.slice("DeepSeek V4 ".length).trim() || full;
    return full || name;
  }

  function reasoningEfforts() {
    return ["minimal", "low", "medium", "high", "xhigh"].map((reasoningEffort) => ({
      reasoningEffort,
      description: `${reasoningEffort} effort`
    }));
  }

  const shortDisplayDescriptorKeys = [
    "shortDisplayName",
    "short_display_name",
    "shortName",
    "compactDisplayName",
    "selectedDisplayName",
    "selectedLabel",
    "compactLabel"
  ];

  function setDescriptorField(descriptor, key, value) {
    if (descriptor[key] === value) return false;
    descriptor[key] = value;
    return true;
  }

  function setDescriptorFields(descriptor, keys, value) {
    let changed = false;
    for (const key of keys) {
      if (setDescriptorField(descriptor, key, value)) changed = true;
    }
    return changed;
  }

  function baseDescriptor(name) {
    const displayName = displayNameForModel(name);
    const shortDisplayName = shortDisplayNameForModel(name, displayName);
    return {
      id: name,
      model: name,
      slug: name,
      name,
      displayName,
      display_name: displayName,
      shortDisplayName,
      short_display_name: shortDisplayName,
      compactDisplayName: shortDisplayName,
      selectedDisplayName: shortDisplayName,
      selectedLabel: shortDisplayName,
      compactLabel: shortDisplayName,
      description: (catalog && (catalog.provider_name || catalog.model_provider)) || "CodeSeeX model",
      hidden: false,
      isDefault: ((catalog && (catalog.default_model || catalog.model)) || "") === name,
      defaultReasoningEffort: "medium",
      supportedReasoningEfforts: reasoningEfforts(),
      inputModalities: ["text", "image"],
      supportsPersonality: false
    };
  }

  function patchModelDescriptor(descriptor, name) {
    if (!descriptor || typeof descriptor !== "object") return false;
    const modelName = name || itemModelName(descriptor);
    if (!modelName) return false;
    let changed = false;
    const displayName = normalizeModelDisplayName(descriptor.displayName || descriptor.name || displayNameForModel(modelName));
    const shortDisplayName = shortDisplayNameForModel(modelName, descriptor.shortDisplayName || displayName);
    if (setDescriptorField(descriptor, "displayName", displayName)) changed = true;
    if (setDescriptorField(descriptor, "display_name", displayName)) changed = true;
    if (!descriptor.name) {
      descriptor.name = displayName;
      changed = true;
    }
    if (setDescriptorFields(descriptor, shortDisplayDescriptorKeys, shortDisplayName)) changed = true;
    return changed;
  }

  function descriptorFor(name) {
    const source = sourceForModel(name);
    const descriptor = { ...baseDescriptor(name), ...(source || {}) };
    descriptor.id = descriptor.id || name;
    descriptor.model = descriptor.model || name;
    descriptor.slug = descriptor.slug || name;
    descriptor.name = descriptor.name || descriptor.displayName || name;
    descriptor.displayName = normalizeModelDisplayName(descriptor.displayName || descriptor.name || name);
    patchModelDescriptor(descriptor, name);
    descriptor.hidden = false;
    descriptor.isDefault = !!descriptor.isDefault || ((catalog && (catalog.default_model || catalog.model)) || "") === name;
    descriptor.defaultReasoningEffort = descriptor.defaultReasoningEffort || "medium";
    if (!Array.isArray(descriptor.supportedReasoningEfforts) || descriptor.supportedReasoningEfforts.length === 0) {
      descriptor.supportedReasoningEfforts = reasoningEfforts();
    }
    if (!Array.isArray(descriptor.inputModalities) || descriptor.inputModalities.length === 0) {
      descriptor.inputModalities = ["text", "image"];
    }
    return descriptor;
  }

  function itemModelName(item) {
    if (!item || typeof item !== "object") return "";
    return String(item.model || item.id || item.slug || "").trim();
  }

  function modelArrayLooksPatchable(value, allowEmpty) {
    return Array.isArray(value)
      && (allowEmpty || value.length > 0)
      && value.every((item) => item && typeof item === "object" && (
        typeof item.model === "string"
        || typeof item.defaultReasoningEffort === "string"
        || Array.isArray(item.supportedReasoningEfforts)
        || Array.isArray(item.inputModalities)
      ));
  }

  function stringArrayLooksPatchable(value) {
    return Array.isArray(value) && value.every((item) => typeof item === "string");
  }

  function patchModelNameArray(models) {
    if (!stringArrayLooksPatchable(models)) return false;
    let changed = false;
    for (const name of modelNames()) {
      if (!models.includes(name)) {
        models.push(name);
        changed = true;
      }
    }
    return changed;
  }

  function patchModelArray(models, allowEmpty) {
    if (!modelArrayLooksPatchable(models, !!allowEmpty)) return false;
    const names = modelNames();
    if (!names.length) return false;
    let changed = false;
    const existing = new Map();
    for (const item of models) {
      const name = itemModelName(item);
      if (name) existing.set(name, item);
      if (names.includes(name) && item.hidden !== false) {
        item.hidden = false;
        changed = true;
      }
      if (names.includes(name) && patchModelDescriptor(item, name)) changed = true;
    }
    for (const name of names) {
      if (!existing.has(name)) {
        models.push(descriptorFor(name));
        changed = true;
      }
    }
    return changed;
  }

  function patchModelContainer(value, allowEmpty) {
    if (!value || typeof value !== "object") return false;
    let changed = false;
    const patchEmpty = !!allowEmpty;
    if (patchModelArray(value.models, patchEmpty || "defaultModel" in value)) changed = true;
    if (patchModelNameArray(value.models)) changed = true;
    if (patchModelArray(value.data, patchEmpty)) changed = true;
    if (patchModelArray(value.result, patchEmpty)) changed = true;
    if (patchModelArray(value.result && value.result.data, patchEmpty)) changed = true;
    if (patchModelArray(value.result && value.result.models, patchEmpty)) changed = true;
    if (patchModelArray(value.message && value.message.result && value.message.result.data, patchEmpty)) changed = true;
    const names = modelNames();
    if (value.defaultModel == null && names.length > 0 && ("models" in value || "data" in value || "result" in value)) {
      value.defaultModel = descriptorFor(names[0]);
      changed = true;
    }
    if (typeof value.defaultModel === "string" && names.includes(value.defaultModel) && value.model == null) {
      value.model = value.defaultModel;
      changed = true;
    }
    return changed;
  }


  const STATSIG_MODEL_CONFIG_KEY = "107580212";

  function statsigClients() {
    const global = window.__STATSIG__;
    const clients = [];
    const push = (value) => {
      if (value && typeof value === "object" && typeof value.getDynamicConfig === "function" && !clients.includes(value)) {
        clients.push(value);
      }
    };
    push(global);
    push(global && global.firstInstance);
    if (global && typeof global.instance === "function") {
      try { push(global.instance()); } catch {}
    }
    if (global && global.instances && typeof global.instances === "object") {
      for (const value of Object.values(global.instances)) push(value);
    }
    return clients;
  }

  /// Widens the model gate in place. The provider's DynamicConfig object is
  /// handed back untouched: only the values inside `value` are merged, so no
  /// prototype or receiver is replaced and nothing can throw on a private field.
  function patchDynamicConfigValue(config) {
    if (!config || typeof config !== "object") return false;
    const value = config.value;
    if (!value || typeof value !== "object") return false;
    const names = modelNames();
    if (!names.length) return false;
    let changed = false;
    const available = Array.isArray(value.available_models) ? value.available_models : [];
    const merged = unique([...available, ...names]);
    if (merged.length !== available.length) {
      try {
        value.available_models = merged;
        changed = true;
      } catch (error) {
        rememberFailure(error);
      }
    }
    const defaultName = (catalog && (catalog.default_model || catalog.model)) || names[0] || "";
    if (defaultName && (!value.default_model || !names.includes(String(value.default_model)))) {
      try {
        value.default_model = defaultName;
        changed = true;
      } catch (error) {
        rememberFailure(error);
      }
    }
    if (changed) {
      state.dynamicConfigPatch = {
        installed: true,
        applied: true,
        configKey: STATSIG_MODEL_CONFIG_KEY,
        models: names,
        patchVersion: VERSION
      };
    }
    return changed;
  }

  function patchStatsigClient(client) {
    if (!client || typeof client.getDynamicConfig !== "function") return false;
    if (client.__codeseexDynamicConfigPatch === VERSION) return true;
    const original = client.getDynamicConfig;
    client.getDynamicConfig = function codeseexGetDynamicConfig(name, options) {
      const result = original.call(this, name, options);
      if (String(name || "") !== STATSIG_MODEL_CONFIG_KEY) return result;
      if (result && typeof result.then === "function") {
        return result.then((value) => {
          patchDynamicConfigValue(value);
          return value;
        });
      }
      patchDynamicConfigValue(result);
      return result;
    };
    client.__codeseexDynamicConfigPatch = VERSION;
    hooks.statsigClient = client;
    hooks.statsigOriginal = original;
    return true;
  }

  function patchStatsigDynamicConfig() {
    let patched = 0;
    for (const client of statsigClients()) {
      if (patchStatsigClient(client)) patched += 1;
    }
    if (patched > 0) {
      state.dynamicConfigPatch = {
        installed: true,
        clients: patched,
        configKey: STATSIG_MODEL_CONFIG_KEY,
        models: modelNames(),
        patchVersion: VERSION
      };
    } else if (!(state.dynamicConfigPatch && state.dynamicConfigPatch.installed)) {
      state.dynamicConfigPatch = {
        installed: false,
        clients: 0,
        configKey: STATSIG_MODEL_CONFIG_KEY,
        patchVersion: VERSION
      };
    }
    return patched > 0;
  }

  /// Statsig may be created after this script runs, so the global is trapped
  /// instead of polled into existence.
  function installStatsigTrap() {
    if (window.__codeseexStatsigTrapVersion === VERSION) return;
    let current = window.__STATSIG__;
    hooks.hadStatsig = current !== undefined;
    hooks.statsigValue = current;
    try {
      Object.defineProperty(window, "__STATSIG__", {
        configurable: true,
        get() {
          return current;
        },
        set(value) {
          current = value;
          hooks.statsigValue = value;
          try { patchStatsigDynamicConfig(); } catch (error) { rememberFailure(error); }
        }
      });
      window.__codeseexStatsigTrapVersion = VERSION;
    } catch (error) {
      rememberFailure(error);
    }
    try { patchStatsigDynamicConfig(); } catch (error) { rememberFailure(error); }
  }

  /// True only for a payload that is unambiguously a model list, so the network
  /// hook can never rewrite unrelated JSON.
  function isModelListPayload(payload) {
    if (!payload || typeof payload !== "object") return false;
    const candidates = [payload.data, payload.models, payload.result && payload.result.data];
    return candidates.some((value) => Array.isArray(value)
      && value.length > 0
      && value.every((item) => item && typeof item === "object" && typeof item.model === "string"));
  }

  /// Narrow safety net for the model list: only an OK JSON response whose body is
  /// unambiguously a model list is rewritten. Everything else is returned as the
  /// provider sent it, so the hook cannot disturb other renderer traffic.
  function installFetchPatch() {
    if (window.__codeseexModelFetchPatchVersion === VERSION) return;
    const original = window.fetch;
    if (typeof original !== "function") return;
    window.fetch = async function codeseexFetch(input, init) {
      const response = await original.call(this, input, init);
      try {
        const url = String((input && input.url) || input || "");
        if (!/model/i.test(url) || !response || response.ok !== true || typeof response.clone !== "function") {
          return response;
        }
        const contentType = String(response.headers && response.headers.get && response.headers.get("content-type") || "");
        if (!contentType.includes("json")) return response;
        const payload = await response.clone().json();
        if (!isModelListPayload(payload)) return response;
        if (patchModelContainer(payload, true)) {
          state.fetchPatch = { patched: true, url: url.slice(0, 120), patchVersion: VERSION };
          return new Response(JSON.stringify(payload), {
            status: response.status,
            statusText: response.statusText,
            headers: response.headers
          });
        }
      } catch (error) {
        rememberFailure(error);
      }
      return response;
    };
    window.__codeseexModelFetchPatchVersion = VERSION;
    hooks.fetch = original;
    state.fetchPatch = state.fetchPatch || { patched: false, patchVersion: VERSION };
  }

  function rendererDiagnostic(reason) {
    return {
      version: VERSION,
      reason,
      models: modelNames(),
      cleanup: state.cleanup || [],
      installed: state.installed === true,
      uninstalled: state.uninstalled === true,
      uninstallReason: state.uninstallReason || "",
      delivered: injectionDelivered(),
      dynamicConfigPatch: state.dynamicConfigPatch || { installed: false },
      fetchPatch: state.fetchPatch || { patched: false },
      failures: (state.failures || []).slice(-8)
    };
  }

  function refresh(reason) {
    try {
      patchStatsigDynamicConfig();
    } catch (error) {
      rememberFailure(error);
    }
    return rendererDiagnostic(reason || "refresh");
  }

  cleanupPreviousInjection();
  hooks.installed = true;
  installStatsigTrap();
  installFetchPatch();
  state.installed = true;
  state.uninstalled = false;
  state.uninstallReason = "";

  // The hooks stay installed so the widened model list keeps holding, but they
  // stay controllable: `uninstall()` removes every one of them and restores the
  // renderer to its original state.
  state.uninstall = (reason) => uninstallInjection(reason || "manual");
  let remaining = 120;
  hooks.interval = window.setInterval(() => {
    try { patchStatsigDynamicConfig(); } catch (error) { rememberFailure(error); }
    remaining -= 1;
    if (remaining <= 0) window.clearInterval(hooks.interval);
  }, 500);

  const initialDiagnostic = refresh("initial");
  return initialDiagnostic;
})();
"#;

    let catalog_json = serde_json::to_string(catalog).unwrap_or_else(|_| "{}".to_owned());
    TEMPLATE.replace("__CODESEEX_MODEL_CATALOG_JSON__", &catalog_json)
}

pub(crate) async fn inject_model_catalog(debug_port: u16, catalog: Value) -> anyhow::Result<Value> {
    let target = pick_codex_target(&list_targets(debug_port).await?)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("selected Codex CDP target has no websocket URL"))?;
    let script = renderer_inject_script(&catalog);
    let renderer_state = inject_script(websocket_url, &script).await?;
    // The injection now widens the renderer's model gate in place instead of
    // importing an internal renderer module, so "effective" means one of the
    // model-list hooks is live rather than "the removed appServer patch worked".
    let injection_effective = ["/dynamicConfigPatch/installed", "/fetchPatch/patched"]
        .iter()
        .any(|pointer| {
            renderer_state
                .pointer(pointer)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
    Ok(json!({
        "ok": injection_effective,
        "status": if injection_effective { "injected" } else { "injected_without_statsig" },
        "debug_port": debug_port,
        "target": {
            "id": target.id,
            "title": target.title,
            "url": target.url
        },
        "renderer_state": renderer_state,
        "models": catalog.get("models").cloned().unwrap_or_else(|| json!([]))
    }))
}

/// Removes a previously installed renderer injection from a live Codex App.
/// The injection is controllable by design: this restores the renderer to its
/// original state without restarting Codex.
pub(crate) async fn remove_model_catalog_injection(debug_port: u16) -> anyhow::Result<Value> {
    let target = pick_codex_target(&list_targets(debug_port).await?)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("selected Codex CDP target has no websocket URL"))?;
    let outcome = inject_script(
        websocket_url,
        r#"(() => {
  const state = window.__codeseexModelCatalogUnlock;
  if (!state) return { removed: false, reason: "not-injected" };
  if (typeof state.uninstall !== "function") return { removed: false, reason: "no-uninstall-hook" };
  state.uninstall("requested");
  return { removed: true, reason: "uninstalled" };
})()"#,
    )
    .await?;
    Ok(json!({
        "ok": outcome.get("removed").and_then(Value::as_bool).unwrap_or(false),
        "debug_port": debug_port,
        "result": outcome
    }))
}

pub(crate) fn launch_app(debug_port: u16) -> anyhow::Result<Value> {
    let launch = launch_codex_app(debug_port)?;
    Ok(json!({
        "ok": true,
        "status": "launched",
        "debug_port": debug_port,
        "launch": launch,
        "injection": {
            "enabled": false,
            "ok": false,
            "status": "disabled"
        }
    }))
}

pub(crate) async fn launch_with_model_catalog_injection(
    debug_port: u16,
    catalog: Value,
) -> anyhow::Result<Value> {
    let mut launch = None;
    let mut first_error = None;
    let mut last_message = None;
    let mut last_status = None;
    let mut last_target = None;
    let mut last_models = None;
    let mut last_renderer_probe = None;
    let mut running_processes = None;

    for attempt in 0..CODEX_LAUNCH_INJECT_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(CODEX_LAUNCH_INJECT_INTERVAL).await;
        }

        match inject_model_catalog(debug_port, catalog.clone()).await {
            Ok(injected) => {
                let effective = model_catalog_injection_effective(&injected);
                last_status = injected
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                last_target = injected.get("target").cloned();
                last_models = injected.get("models").cloned();
                last_renderer_probe = renderer_probe_from_injected(&injected);
                last_message = injection_message_from_probe(last_renderer_probe.as_ref())
                    .or(last_status.clone());
                if effective {
                    return Ok(codex_launch_injection_response(
                        debug_port,
                        launch,
                        "model_list_injected",
                        true,
                        attempt + 1,
                        last_status,
                        last_target,
                        last_models,
                        last_renderer_probe,
                        first_error,
                        running_processes,
                        None,
                    ));
                }
            }
            Err(error) => {
                let message = error.to_string();
                if attempt == 0 && launch.is_none() {
                    first_error = Some(message.clone());
                    let running = running_codex_processes();
                    if !running.is_empty() {
                        running_processes = Some(codex_process_summary(&running));
                    }
                    let launch_value = launch_codex_app(debug_port).map_err(|launch_error| {
                        anyhow::anyhow!(
                            "Codex injection preflight failed and Codex launch failed: {launch_error}. Initial CDP check: {message}"
                        )
                    })?;
                    launch = Some(launch_value);
                    last_message = Some(message);
                    continue;
                }
                last_message = Some(message);
            }
        }
    }

    let status = if launch.is_some() {
        "launched_injection_incomplete"
    } else {
        "existing_injection_incomplete"
    };
    Ok(codex_launch_injection_response(
        debug_port,
        launch,
        status,
        false,
        CODEX_LAUNCH_INJECT_ATTEMPTS,
        last_status,
        last_target,
        last_models,
        last_renderer_probe,
        first_error,
        running_processes,
        last_message,
    ))
}

#[allow(clippy::too_many_arguments)]
fn codex_launch_injection_response(
    debug_port: u16,
    launch: Option<Value>,
    status: &str,
    injection_ok: bool,
    attempts: usize,
    injection_status: Option<String>,
    target: Option<Value>,
    models: Option<Value>,
    renderer_probe: Option<Value>,
    preflight_error: Option<String>,
    running_processes: Option<String>,
    message: Option<String>,
) -> Value {
    let mut response = json!({
        "ok": true,
        "status": status,
        "debug_port": debug_port,
        "launch": launch.unwrap_or_else(|| json!({ "mode": "existing_debug_port" })),
        "injection": {
            "enabled": true,
            "ok": injection_ok,
            "attempts": attempts,
            "status": injection_status.unwrap_or_else(|| (if injection_ok { "injected" } else { "incomplete" }).to_owned())
        }
    });
    if let Some(object) = response
        .pointer_mut("/injection")
        .and_then(Value::as_object_mut)
    {
        if let Some(target) = target {
            object.insert("target".to_owned(), target);
        }
        if let Some(models) = models {
            object.insert("models".to_owned(), models);
        }
        if let Some(renderer_probe) = renderer_probe {
            object.insert("renderer_probe".to_owned(), renderer_probe);
        }
        if let Some(preflight_error) = preflight_error {
            object.insert("preflight_error".to_owned(), json!(preflight_error));
        }
        if let Some(running_processes) = running_processes {
            object.insert("running_processes".to_owned(), json!(running_processes));
        }
        if let Some(message) = message {
            object.insert("message".to_owned(), json!(message));
        }
    }
    response
}

fn model_catalog_injection_effective(value: &Value) -> bool {
    value.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn renderer_probe_from_injected(value: &Value) -> Option<Value> {
    let renderer_state = value.get("renderer_state")?.as_object()?;
    let mut output = Map::new();
    for key in [
        "version",
        "reason",
        "models",
        "cleanup",
        "installed",
        "uninstalled",
        "uninstallReason",
        "delivered",
        "dynamicConfigPatch",
        "fetchPatch",
        "failures",
    ] {
        if let Some(value) = renderer_state.get(key) {
            output.insert(key.to_owned(), value.clone());
        }
    }
    Some(Value::Object(output))
}

fn injection_message_from_probe(probe: Option<&Value>) -> Option<String> {
    let probe = probe?;
    for pointer in [
        "/dynamicConfigPatch/error",
        "/fetchPatch/error",
    ] {
        if let Some(message) = probe.pointer(pointer).and_then(Value::as_str) {
            if !message.trim().is_empty() {
                return Some(message.trim().to_owned());
            }
        }
    }
    if let Some(last) = probe
        .get("failures")
        .and_then(Value::as_array)
        .and_then(|failures| failures.last())
        .and_then(Value::as_str)
    {
        if !last.trim().is_empty() {
            return Some(last.trim().to_owned());
        }
    }
    None
}

fn codex_process_summary(processes: &[CodexProcess]) -> String {
    processes
        .iter()
        .take(8)
        .map(|process| {
            if process.path.is_empty() {
                format!("{} pid {}", process.name, process.pid)
            } else {
                format!("{} pid {} ({})", process.name, process.pid, process.path)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn launch_codex_app(debug_port: u16) -> anyhow::Result<Value> {
    #[cfg(target_os = "windows")]
    {
        if let Some(executable) = find_codex_executable() {
            if let Some(app_user_model_id) = packaged_app_user_model_id_from_path(&executable) {
                return activate_packaged_codex_app(
                    &app_user_model_id,
                    debug_port,
                    Some(&executable),
                    None,
                );
            }
            return launch_codex_process(&executable, debug_port);
        }

        if let Some(app) = find_windows_packaged_codex_app() {
            return activate_packaged_codex_app(
                &app.app_user_model_id,
                debug_port,
                None,
                app.package_full_name.as_deref(),
            );
        }

        anyhow::bail!("Codex executable or packaged app entry was not found");
    }

    #[cfg(not(target_os = "windows"))]
    {
        let executable = find_codex_executable()
            .ok_or_else(|| anyhow::anyhow!("Codex executable was not found"))?;
        #[cfg(target_os = "macos")]
        if let Some(app_bundle) = macos_app_bundle_from_executable(&executable) {
            return launch_macos_codex_app_bundle(&app_bundle, debug_port, &executable);
        }

        launch_codex_process(&executable, debug_port)
    }
}

fn launch_codex_process(executable: &std::path::Path, debug_port: u16) -> anyhow::Result<Value> {
    #[cfg(target_os = "windows")]
    if let Some(app_user_model_id) = packaged_app_user_model_id_from_path(executable) {
        return activate_packaged_codex_app(&app_user_model_id, debug_port, Some(executable), None);
    }

    let arguments = codex_launch_arguments(debug_port);
    let mut command = Command::new(executable);
    command.args(&arguments);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let child = command.spawn()?;
    Ok(json!({
        "mode": "process",
        "path": executable.to_string_lossy(),
        "arguments": arguments,
        "pid": child.id(),
        "debug_port": debug_port
    }))
}

#[cfg(target_os = "macos")]
fn launch_macos_codex_app_bundle(
    app_bundle: &std::path::Path,
    debug_port: u16,
    executable: &std::path::Path,
) -> anyhow::Result<Value> {
    let arguments = codex_launch_arguments(debug_port);
    let mut command = Command::new("open");
    command
        .arg("-n")
        .arg(app_bundle)
        .arg("--args")
        .args(&arguments);
    let child = command.spawn()?;
    Ok(json!({
        "mode": "macos_open",
        "app_bundle": app_bundle.to_string_lossy(),
        "path": executable.to_string_lossy(),
        "arguments": arguments,
        "pid": child.id(),
        "debug_port": debug_port
    }))
}

#[cfg(target_os = "windows")]
fn activate_packaged_codex_app(
    app_user_model_id: &str,
    debug_port: u16,
    executable: Option<&std::path::Path>,
    package_full_name: Option<&str>,
) -> anyhow::Result<Value> {
    let arguments = codex_launch_arguments(debug_port);
    let argument_line = windows_command_line_arguments(&arguments);
    let script = r#"
$ErrorActionPreference = 'Stop'
$aumid = [string]$env:CODESEEX_CODEX_AUMID
$arguments = [string]$env:CODESEEX_CODEX_ARGS
$source = @'
using System;
using System.Runtime.InteropServices;

public enum ActivateOptions
{
    None = 0
}

[ComImport]
[Guid("2e941141-7f97-4756-ba1d-9decde894a3d")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IApplicationActivationManager
{
    [PreserveSig]
    int ActivateApplication(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        [MarshalAs(UnmanagedType.LPWStr)] string arguments,
        ActivateOptions options,
        out uint processId);

    [PreserveSig]
    int ActivateForFile(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        IntPtr itemArray,
        [MarshalAs(UnmanagedType.LPWStr)] string verb,
        out uint processId);

    [PreserveSig]
    int ActivateForProtocol(
        [MarshalAs(UnmanagedType.LPWStr)] string appUserModelId,
        IntPtr itemArray,
        out uint processId);
}

[ComImport]
[Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
class ApplicationActivationManager
{
}

public static class CodeSeeXPackagedAppActivator
{
    public static uint Activate(string appUserModelId, string arguments)
    {
        var manager = (IApplicationActivationManager)new ApplicationActivationManager();
        uint processId;
        int hr = manager.ActivateApplication(appUserModelId, arguments ?? "", ActivateOptions.None, out processId);
        if (hr < 0)
        {
            Marshal.ThrowExceptionForHR(hr);
        }
        return processId;
    }
}
'@
Add-Type -TypeDefinition $source -Language CSharp
[CodeSeeXPackagedAppActivator]::Activate($aumid, $arguments)
"#;
    let mut command = Command::new("powershell");
    command
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("CODESEEX_CODEX_AUMID", app_user_model_id)
        .env("CODESEEX_CODEX_ARGS", &argument_line);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        anyhow::bail!(
            "Codex packaged app activation failed for {app_user_model_id}: {}",
            if stderr.is_empty() { stdout } else { stderr }
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid = stdout
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<u32>().ok());
    let path = executable.map(|path| path.to_string_lossy().into_owned());
    Ok(json!({
        "mode": "packaged_activation",
        "app_user_model_id": app_user_model_id,
        "package_full_name": package_full_name,
        "arguments": argument_line,
        "path": path,
        "pid": pid,
        "debug_port": debug_port
    }))
}

fn codex_launch_arguments(debug_port: u16) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={debug_port}"),
        format!("--remote-allow-origins=http://127.0.0.1:{debug_port}"),
    ]
}

#[cfg(target_os = "windows")]
fn windows_command_line_arguments(args: &[String]) -> String {
    args.iter()
        .map(|arg| windows_quote_command_line_argument(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_os = "windows")]
fn windows_quote_command_line_argument(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\\'))
    {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(ch);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn packaged_app_user_model_id_from_path(path: &std::path::Path) -> Option<String> {
    path.components()
        .rev()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(packaged_app_user_model_id_from_component)
        .or_else(|| {
            path.to_string_lossy()
                .split(['\\', '/'])
                .rev()
                .find_map(packaged_app_user_model_id_from_component)
        })
}

fn packaged_app_user_model_id_from_component(name: &str) -> Option<String> {
    codex_package_parts(name)
        .map(|(identity, _version, publisher_id)| format!("{identity}_{publisher_id}!App"))
}

fn codex_package_parts(package_name: &str) -> Option<(&str, &str, &str)> {
    for identity in CODEX_PACKAGE_IDENTITIES {
        let Some(rest) = package_name.strip_prefix(identity) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('_') else {
            continue;
        };
        let Some((version, rest)) = rest.split_once('_') else {
            continue;
        };
        let Some((_, publisher_id)) = rest.rsplit_once("__") else {
            continue;
        };
        if publisher_id.is_empty() {
            continue;
        }
        return Some((*identity, version, publisher_id));
    }
    None
}

#[cfg(target_os = "windows")]
fn find_windows_packaged_codex_app() -> Option<WindowsPackagedCodexApp> {
    for key in [
        "CODESEEX_CODEX_APP_AUMID",
        "CODEX_APP_AUMID",
        "CODESEEX_CODEX_AUMID",
    ] {
        let Some(value) = std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return Some(WindowsPackagedCodexApp {
            app_user_model_id: value,
            package_full_name: None,
        });
    }

    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$items = New-Object System.Collections.Generic.List[object]
Get-StartApps | Where-Object { $_.AppID -like 'OpenAI.Codex*!*' } | ForEach-Object {
  $items.Add([pscustomobject]@{
    appUserModelId = [string]$_.AppID
    packageFullName = $null
  })
}
Get-AppxPackage | Where-Object { $_.Name -eq 'OpenAI.Codex' -or $_.Name -eq 'OpenAI.CodexBeta' } | ForEach-Object {
  $items.Add([pscustomobject]@{
    appUserModelId = "$($_.PackageFamilyName)!App"
    packageFullName = [string]$_.PackageFullName
  })
}
$items | Sort-Object appUserModelId -Unique | ConvertTo-Json -Compress
"#;
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return None;
    };
    if !output.status.success() {
        return None;
    }
    parse_windows_packaged_codex_apps_json(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .next()
}

#[cfg(any(target_os = "windows", test))]
fn parse_windows_packaged_codex_apps_json(text: &str) -> Vec<WindowsPackagedCodexApp> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(items) = serde_json::from_str::<Vec<WindowsPackagedCodexAppJson>>(trimmed) {
        return items
            .into_iter()
            .filter_map(windows_packaged_codex_app_from_json)
            .collect();
    }
    serde_json::from_str::<WindowsPackagedCodexAppJson>(trimmed)
        .ok()
        .and_then(windows_packaged_codex_app_from_json)
        .into_iter()
        .collect()
}

#[cfg(any(target_os = "windows", test))]
fn windows_packaged_codex_app_from_json(
    value: WindowsPackagedCodexAppJson,
) -> Option<WindowsPackagedCodexApp> {
    let app_user_model_id = value
        .app_user_model_id
        .map(|value| value.trim().to_owned())
        .filter(|value| is_codex_app_user_model_id(value))?;
    Some(WindowsPackagedCodexApp {
        app_user_model_id,
        package_full_name: value.package_full_name.filter(|value| !value.is_empty()),
    })
}

#[cfg(any(target_os = "windows", test))]
fn is_codex_app_user_model_id(value: &str) -> bool {
    CODEX_PACKAGE_IDENTITIES
        .iter()
        .any(|identity| value.starts_with(&format!("{identity}_")) && value.contains('!'))
}

#[cfg(target_os = "windows")]
fn running_codex_processes() -> Vec<CodexProcess> {
    let script = r#"
Get-Process -ErrorAction SilentlyContinue |
  Where-Object { $_.ProcessName -ieq 'Codex' } |
  Select-Object @{Name='pid';Expression={$_.Id}},@{Name='name';Expression={[string]$_.ProcessName}},@{Name='path';Expression={if ($_.Path) {[string]$_.Path} else {''}}} |
  ConvertTo-Json -Compress
"#;
    let mut command = Command::new("powershell");
    command.args([
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_windows_codex_processes_json(&String::from_utf8_lossy(&output.stdout))
}

fn parse_windows_codex_processes_json(text: &str) -> Vec<CodexProcess> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Ok(items) = serde_json::from_str::<Vec<CodexProcessJson>>(trimmed) {
        return items
            .into_iter()
            .filter_map(codex_process_from_json)
            .collect();
    }
    serde_json::from_str::<CodexProcessJson>(trimmed)
        .ok()
        .and_then(codex_process_from_json)
        .into_iter()
        .collect()
}

fn codex_process_from_json(value: CodexProcessJson) -> Option<CodexProcess> {
    Some(CodexProcess {
        pid: value.pid.filter(|pid| *pid > 0)?,
        name: value.name.unwrap_or_else(|| "Codex".to_owned()),
        path: value.path.unwrap_or_default(),
    })
}

#[cfg(unix)]
fn running_codex_processes_from_ps() -> Vec<CodexProcess> {
    let Ok(output) = Command::new("ps")
        .args(["-eo", "pid=,comm=,args="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_unix_codex_processes(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn running_codex_processes() -> Vec<CodexProcess> {
    running_codex_processes_from_ps()
}

#[cfg(all(
    not(target_os = "windows"),
    not(target_os = "macos"),
    not(target_os = "linux")
))]
fn running_codex_processes() -> Vec<CodexProcess> {
    Vec::new()
}

#[cfg(any(unix, test))]
fn parse_unix_codex_processes(text: &str) -> Vec<CodexProcess> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (pid, rest) = trimmed.split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (name, args) = rest
                .split_once(char::is_whitespace)
                .map(|(name, args)| (name, args.trim()))
                .unwrap_or((rest, ""));
            let base_name = std::path::Path::new(name)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(name);
            if !base_name.eq_ignore_ascii_case("codex") {
                return None;
            }
            Some(CodexProcess {
                pid: pid.parse::<u32>().ok()?,
                name: base_name.to_owned(),
                path: args.to_owned(),
            })
        })
        .collect()
}

fn find_codex_executable() -> Option<PathBuf> {
    for key in ["CODESEEX_CODEX_APP_EXE", "CODEX_APP_EXE", "CODEX_APP_PATH"] {
        if let Some(path) = std::env::var_os(key).map(PathBuf::from) {
            if path.is_file() {
                return Some(path);
            }
            let exe = codex_executable_in_dir(&path);
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    platform_codex_executable_candidates()
        .into_iter()
        .find(|path| path.is_file())
}

#[allow(clippy::needless_return)]
fn codex_executable_in_dir(path: &std::path::Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        return path.join("Contents").join("MacOS").join("Codex");
    }
    #[cfg(target_os = "windows")]
    {
        return path.join("Codex.exe");
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let lowercase = path.join("codex");
        if lowercase.is_file() {
            lowercase
        } else {
            path.join("Codex")
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_app_bundle_from_executable(path: &std::path::Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.extension().and_then(|value| value.to_str()) == Some("app"))
        .map(std::path::Path::to_path_buf)
}

fn platform_codex_executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "windows")]
    {
        append_windows_codex_candidates(&mut candidates);
    }
    #[cfg(target_os = "macos")]
    {
        append_macos_codex_candidates(&mut candidates, &PathBuf::from("/Applications"));
        if let Some(home) = dirs_next::home_dir() {
            append_macos_codex_candidates(&mut candidates, &home.join("Applications"));
            candidates.push(home.join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs_next::home_dir() {
            candidates.push(home.join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/usr/bin/codex"));
        candidates.push(PathBuf::from("/opt/Codex/codex"));
        candidates.push(PathBuf::from("/opt/codex/codex"));
    }
    dedup_paths_preserve_order(&mut candidates);
    candidates
}

fn dedup_paths_preserve_order(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::<PathBuf>::new();
    paths.retain(|path| {
        if seen.iter().any(|existing| existing == path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
}

#[cfg(target_os = "macos")]
fn append_macos_codex_candidates(candidates: &mut Vec<PathBuf>, root: &std::path::Path) {
    for name in ["Codex.app", "OpenAI Codex.app", "OpenAI.Codex.app"] {
        candidates.push(root.join(name).join("Contents").join("MacOS").join("Codex"));
    }
}

#[cfg(target_os = "windows")]
fn append_windows_codex_candidates(candidates: &mut Vec<PathBuf>) {
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        candidates.push(
            local
                .join("OpenAI")
                .join("Codex")
                .join("app")
                .join("Codex.exe"),
        );
        candidates.push(local.join("OpenAI").join("Codex").join("Codex.exe"));
        candidates.push(
            local
                .join("Programs")
                .join("OpenAI")
                .join("Codex")
                .join("Codex.exe"),
        );
    }
    append_windows_appx_candidates(candidates);
    dedup_paths_preserve_order(candidates);
}

#[cfg(target_os = "windows")]
fn append_windows_appx_candidates(candidates: &mut Vec<PathBuf>) {
    let mut roots = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles").map(PathBuf::from) {
        roots.push(program_files.join("WindowsApps"));
    }
    if let Some(program_w6432) = std::env::var_os("ProgramW6432").map(PathBuf::from) {
        roots.push(program_w6432.join("WindowsApps"));
    }
    roots.push(PathBuf::from(r"C:\Program Files\WindowsApps"));
    roots.sort();
    roots.dedup();

    let mut appx = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if !(name.starts_with("OpenAI.Codex_") || name.starts_with("OpenAI.CodexBeta_")) {
                continue;
            }
            let exe = path.join("app").join("Codex.exe");
            if exe.is_file() {
                appx.push(exe);
            }
        }
    }
    appx.sort();
    appx.reverse();
    candidates.extend(appx);
}

async fn list_targets(debug_port: u16) -> anyhow::Result<Vec<CdpTarget>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(CDP_HTTP_TIMEOUT)
        .build()?;
    let urls = [
        format!("http://127.0.0.1:{debug_port}/json"),
        format!("http://[::1]:{debug_port}/json"),
    ];
    let mut errors = Vec::new();
    for url in urls {
        match client.get(&url).send().await {
            Ok(response) => match response.error_for_status() {
                Ok(response) => match response.json::<Vec<CdpTarget>>().await {
                    Ok(targets) => return Ok(targets),
                    Err(error) => errors.push(format!("{url}: {error}")),
                },
                Err(error) => errors.push(format!("{url}: {error}")),
            },
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    anyhow::bail!(
        "failed to query Codex CDP targets on debug port {debug_port}: {}",
        errors.join("; ")
    )
}

fn pick_codex_target(targets: &[CdpTarget]) -> anyhow::Result<CdpTarget> {
    let mut first_page = None;
    for target in targets {
        if target.target_type != "page"
            || target
                .web_socket_debugger_url
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        {
            continue;
        }
        first_page.get_or_insert_with(|| target.clone());
        let haystack = format!("{} {}", target.title, target.url).to_ascii_lowercase();
        if haystack.contains("codex") {
            return Ok(target.clone());
        }
    }
    first_page.ok_or_else(|| anyhow::anyhow!("no injectable Codex page target found"))
}

async fn inject_script(websocket_url: &str, script: &str) -> anyhow::Result<Value> {
    let (mut socket, _) = tokio_tungstenite::connect_async(websocket_url).await?;
    send_cdp_command(&mut socket, 1, "Runtime.enable", json!({})).await?;
    let response = send_cdp_command(
        &mut socket,
        2,
        "Runtime.evaluate",
        json!({
            "expression": script,
            "awaitPromise": true,
            "returnByValue": true
        }),
    )
    .await?;
    let _ = socket.close(None).await;
    if let Some(exception) = response.pointer("/result/exceptionDetails") {
        anyhow::bail!("Codex renderer injection threw an exception: {exception}");
    }
    Ok(response
        .pointer("/result/result/value")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "appServerPatch": {
                    "attempted": false,
                    "installed": false
                },
                "error": "Runtime.evaluate returned no by-value renderer diagnostic"
            })
        }))
}

async fn send_cdp_command(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    socket
        .send(Message::Text(
            json!({
                "id": id,
                "method": method,
                "params": params
            })
            .to_string(),
        ))
        .await?;

    while let Some(message) = socket.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => anyhow::bail!("CDP websocket closed before {method} response"),
            Message::Frame(_) => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            anyhow::bail!("CDP {method} failed: {error}");
        }
        return Ok(value);
    }
    anyhow::bail!("CDP websocket ended before {method} response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_script_embeds_catalog_and_app_server_shortcut() {
        let catalog = json!({
            "model": "deepseek-v4-pro",
            "default_model": "deepseek-v4-pro",
            "models": ["deepseek-v4-flash", "deepseek-v4-pro"],
            "appServer": {
                "data": [
                    {
                        "id": "deepseek-v4-flash",
                        "model": "deepseek-v4-flash",
                        "displayName": "DeepSeek V4 Flash",
                        "shortDisplayName": "Flash",
                        "hidden": false,
                        "isDefault": false
                    },
                    {
                        "id": "deepseek-v4-pro",
                        "model": "deepseek-v4-pro",
                        "displayName": "DeepSeek V4 Pro",
                        "shortDisplayName": "Pro",
                        "hidden": false,
                        "isDefault": true
                    }
                ]
            }
        });
        let script = renderer_inject_script(&catalog);
        assert!(script.contains("deepseek-v4-flash"));
        assert!(script.contains("deepseek-v4-pro"));
        assert!(script.contains("isModelListPayload"));
        assert!(script.contains("codeseex-model-catalog-unlock-v3"));
        assert!(script.contains("cleanupPreviousInjection"));
        assert!(script.contains("stale-window-message-listener"));
        assert!(script.contains("uninstallInjection"));
        assert!(script.contains("injectionDelivered"));
        assert!(script.contains("uninstallReason"));
        assert!(script.contains("response.ok !== true"));
        assert!(!script.contains("addEventListener(\"message\""));
        assert!(!script.contains("patchMcpModelResponseData"));
        assert!(script.contains("const initialDiagnostic = refresh(\"initial\");"));
        assert!(script.contains("return initialDiagnostic;"));
        assert!(script.contains("if (patchModelArray(value.data, patchEmpty))"));
        assert!(script.contains("patchStatsigDynamicConfig"));
        assert!(script.contains("patchDynamicConfigValue"));
        assert!(script.contains("__codeseexStatsigTrapVersion"));
        assert!(script.contains("installFetchPatch"));
        // Teardown clears those sentinels, so a repeat injection really
        // re-installs instead of silently reporting success.
        assert!(script.contains("delete window.__codeseexModelFetchPatchVersion"));
        assert!(script.contains("delete window.__codeseexStatsigTrapVersion"));
        assert!(script.contains("fetchPatch"));
        assert!(script.contains("available_models"));
        assert!(script.contains("107580212"));
        assert!(script.contains("dynamicConfigPatch"));
        // The boundary-crossing mechanisms must be gone entirely.
        assert!(!script.contains("model-queries-"));
        assert!(!script.contains("use-host-config"));
        assert!(!script.contains("installAppServerPatch"));
        assert!(!script.contains("patchAppServerClient"));
        assert!(!script.contains("loadCodexAppModule"));
        assert!(!script.contains("codexAppAssetUrl"));
        assert!(!script.contains("shortenSelectedModelButtonLabels"));
        assert!(!script.contains("Object.create"));
        assert!(!script.contains("MutationObserver"));
        assert!(!script.contains("createTreeWalker"));
        assert!(!script.contains("Response.prototype.json"));
        assert!(!script.contains("window.dispatchEvent ="));
        assert!(!script.contains("MODEL_DYNAMIC_CONFIG_NAMES"));
        assert!(!script.contains("patchReactModelState"));
        assert!(!script.contains("patchObjectGraph"));
        assert!(!script.contains("Object.values(module)"));
        assert!(!script.contains("app-server-manager-signals-"));
        assert!(!script.contains("value.pages"));
    }

    #[test]
    fn renderer_probe_keeps_actionable_injection_failure_summary() {
        let injected = json!({
            "renderer_state": {
                "version": "codeseex-test",
                "dynamicConfigPatch": {
                    "installed": false,
                    "error": "Codex statsig client not found"
                },
                "fetchPatch": {
                    "installed": true,
                    "patchVersion": "codeseex-test"
                },
                "failures": ["statsig missing"]
            }
        });

        let probe = renderer_probe_from_injected(&injected).expect("renderer probe");
        assert_eq!(
            probe
                .pointer("/dynamicConfigPatch/error")
                .and_then(Value::as_str),
            Some("Codex statsig client not found")
        );
        assert_eq!(
            injection_message_from_probe(Some(&probe)).as_deref(),
            Some("Codex statsig client not found")
        );
        assert!(probe.get("ignored").is_none());
    }

    #[test]
    fn parses_windows_codex_process_json_shapes() {
        let single = parse_windows_codex_processes_json(
            r#"{"pid":2760,"name":"Codex","path":"C:\\Program Files\\WindowsApps\\OpenAI.Codex\\app\\Codex.exe"}"#,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].pid, 2760);
        assert_eq!(single[0].name, "Codex");

        let many = parse_windows_codex_processes_json(
            r#"[{"pid":2760,"name":"Codex","path":"a"},{"pid":16212,"name":"codex","path":"b"}]"#,
        );
        assert_eq!(many.len(), 2);
        assert_eq!(
            codex_process_summary(&many),
            "Codex pid 2760 (a); codex pid 16212 (b)"
        );
        assert!(parse_windows_codex_processes_json("").is_empty());
    }

    #[test]
    fn parses_unix_codex_process_list() {
        let processes = parse_unix_codex_processes(
            "123 Codex /Applications/Codex.app/Contents/MacOS/Codex --remote-debugging-port=9222\n456 bash bash -lc codex\n789 codex /usr/local/bin/codex\n",
        );
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid, 123);
        assert_eq!(processes[0].name, "Codex");
        assert_eq!(processes[1].pid, 789);
        assert_eq!(processes[1].path, "/usr/local/bin/codex");
    }

    #[test]
    fn derives_macos_app_bundle_from_executable() {
        let path = std::path::Path::new("/Applications/Codex.app/Contents/MacOS/Codex");
        assert_eq!(
            macos_app_bundle_from_executable(path)
                .as_deref()
                .and_then(std::path::Path::to_str),
            Some("/Applications/Codex.app")
        );
        assert!(
            macos_app_bundle_from_executable(std::path::Path::new("/usr/local/bin/codex"))
                .is_none()
        );
    }

    #[test]
    fn dedups_candidate_paths_without_reordering() {
        let mut paths = vec![
            PathBuf::from("local"),
            PathBuf::from("appx-new"),
            PathBuf::from("local"),
            PathBuf::from("appx-old"),
        ];
        dedup_paths_preserve_order(&mut paths);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("local"),
                PathBuf::from("appx-new"),
                PathBuf::from("appx-old")
            ]
        );
    }

    #[test]
    fn derives_packaged_codex_app_user_model_id() {
        let path = std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0\app\Codex.exe",
        );
        assert_eq!(
            packaged_app_user_model_id_from_path(path).as_deref(),
            Some("OpenAI.Codex_2p2nqsd0c76g0!App")
        );

        let beta = std::path::Path::new(
            r"C:\Program Files\WindowsApps\OpenAI.CodexBeta_26.1.2.3_x64__publisher\app\Codex.exe",
        );
        assert_eq!(
            packaged_app_user_model_id_from_path(beta).as_deref(),
            Some("OpenAI.CodexBeta_publisher!App")
        );
    }

    #[test]
    fn parses_windows_packaged_codex_app_json_shapes() {
        let single = parse_windows_packaged_codex_apps_json(
            r#"{"appUserModelId":"OpenAI.Codex_2p2nqsd0c76g0!App","packageFullName":"OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0"}"#,
        );
        assert_eq!(single.len(), 1);
        assert_eq!(
            single[0].app_user_model_id,
            "OpenAI.Codex_2p2nqsd0c76g0!App"
        );
        assert_eq!(
            single[0].package_full_name.as_deref(),
            Some("OpenAI.Codex_26.616.6631.0_x64__2p2nqsd0c76g0")
        );

        let many = parse_windows_packaged_codex_apps_json(
            r#"[{"appUserModelId":"OpenAI.CodexBeta_publisher!App"},{"appUserModelId":"Other.App_abc!App"}]"#,
        );
        assert_eq!(many.len(), 1);
        assert_eq!(many[0].app_user_model_id, "OpenAI.CodexBeta_publisher!App");
        assert!(parse_windows_packaged_codex_apps_json("").is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn quotes_packaged_activation_arguments() {
        assert_eq!(
            windows_command_line_arguments(&codex_launch_arguments(9222)),
            "--remote-debugging-port=9222 --remote-allow-origins=http://127.0.0.1:9222"
        );
        assert_eq!(
            windows_quote_command_line_argument(r#"C:\Path With Spaces\Codex"#),
            r#""C:\Path With Spaces\Codex""#
        );
    }
}
