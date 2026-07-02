use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use codeseex_core::{AppConfig, UserConfig};
use reqwest::header;
use serde_json::{json, Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::permissions::ToolPermissionError;
use super::ToolExecutionContext;

pub(crate) const ANALYZE_TOOL_NAME: &str = "vision_analyze";
pub(crate) const GENERATE_TOOL_NAME: &str = "vision_generate";
pub(crate) const GENERATE_ALIAS_TOOL_NAME: &str = "image_gen";
pub(crate) const ANALYZE_URL_KEY: &str = "VISION_ANALYZE_URL";
pub(crate) const ANALYZE_MODEL_KEY: &str = "VISION_ANALYZE_MODEL";
pub(crate) const GENERATE_URL_KEY: &str = "VISION_GENERATE_URL";
pub(crate) const GENERATE_MODEL_KEY: &str = "VISION_GENERATE_MODEL";
pub(crate) const API_KEY_KEY: &str = "VISION_API_KEY";

const MAX_IMAGE_REFERENCES: usize = 4;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMAGE_URL_CHARS: usize = 12 * 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 4_000;
const MAX_ERROR_CHARS: usize = 1_200;
const DEFAULT_GENERATE_SIZE: &str = "1024x1024";
const MAX_GENERATE_PROMPT_CHARS: usize = 4_000;
const MAX_GENERATED_IMAGE_SCAN_DEPTH: usize = 8;

#[derive(Debug, Clone)]
struct VisionAnalyzeConfig {
    request_url: String,
    model: String,
    api_key: String,
}

#[derive(Debug, Clone)]
struct VisionGenerateConfig {
    request_url: String,
    model: String,
    api_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualEndpointKind {
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerateEndpointKind {
    ImagesGenerations,
    Responses,
}

pub(crate) fn config_keys() -> [&'static str; 5] {
    [
        ANALYZE_URL_KEY,
        ANALYZE_MODEL_KEY,
        GENERATE_URL_KEY,
        GENERATE_MODEL_KEY,
        API_KEY_KEY,
    ]
}

pub(crate) fn registry_config_fields(settings: &BTreeMap<String, String>) -> Vec<Value> {
    vec![
        json!({
            "key": ANALYZE_URL_KEY,
            "type": "text",
            "labelKey": "visionAnalyzeRequestUrl",
            "label": "Request URL",
            "descriptionKey": "visionAnalyzeRequestUrlHint",
            "description": "Complete OpenAI-compatible image understanding endpoint. Local image pixels are sent to this endpoint.",
            "placeholderKey": "visionAnalyzeRequestUrlPlaceholder",
            "placeholder": "https://api.example.com/v1/responses",
            "width": "wide",
            "value": setting_value(settings, ANALYZE_URL_KEY)
        }),
        json!({
            "key": ANALYZE_MODEL_KEY,
            "type": "text",
            "labelKey": "visionAnalyzeModel",
            "label": "Vision model",
            "descriptionKey": "visionAnalyzeModelHint",
            "description": "Model name sent to the visual endpoint.",
            "placeholderKey": "visionAnalyzeModelPlaceholder",
            "placeholder": "gpt-4o-mini",
            "width": "compact",
            "value": setting_value(settings, ANALYZE_MODEL_KEY)
        }),
        json!({
            "key": GENERATE_URL_KEY,
            "type": "text",
            "labelKey": "visionGenerateRequestUrl",
            "label": "Generate request URL",
            "descriptionKey": "visionGenerateRequestUrlHint",
            "description": "Complete OpenAI-compatible image generation endpoint. Prefer /responses; use /images/generations only for the official image-model API.",
            "placeholderKey": "visionGenerateRequestUrlPlaceholder",
            "placeholder": "https://api.example.com/v1/responses",
            "width": "wide",
            "value": settings.get(GENERATE_URL_KEY).cloned().unwrap_or_default()
        }),
        json!({
            "key": GENERATE_MODEL_KEY,
            "type": "text",
            "labelKey": "visionGenerateModel",
            "label": "Generate model",
            "descriptionKey": "visionGenerateModelHint",
            "description": "Model name sent to the Vision image generation endpoint.",
            "placeholderKey": "visionGenerateModelPlaceholder",
            "placeholder": "gpt-image-1",
            "width": "compact",
            "value": settings.get(GENERATE_MODEL_KEY).cloned().unwrap_or_default()
        }),
        json!({
            "key": API_KEY_KEY,
            "type": "password",
            "labelKey": "visionApiKey",
            "label": "API key",
            "descriptionKey": "visionApiKeyHint",
            "description": "Bearer token used only by the Vision module.",
            "placeholderKey": "visionApiKeyPlaceholder",
            "placeholder": "sk-...",
            "width": "wide",
            "value": "",
            "configured": settings
                .get(API_KEY_KEY)
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false)
        }),
    ]
}

fn setting_value(settings: &BTreeMap<String, String>, key: &'static str) -> String {
    settings.get(key).cloned().unwrap_or_default()
}

pub(crate) async fn execute(
    client: &reqwest::Client,
    app_config: &AppConfig,
    context: &ToolExecutionContext,
    messages: &[Value],
    current_image_refs: &[String],
    arguments: &Value,
) -> Value {
    let config = match VisionAnalyzeConfig::load(app_config) {
        Ok(config) => config,
        Err(error) => return error,
    };
    let (endpoint, endpoint_kind) = match visual_endpoint(&config.request_url) {
        Ok(value) => value,
        Err(message) => {
            return unavailable(
                ANALYZE_TOOL_NAME,
                vec![ANALYZE_URL_KEY],
                Some(format!("Invalid Vision request URL: {message}")),
            );
        }
    };
    let (prompt, prompt_source) = match visual_prompt(arguments, messages) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let images = match visual_images(arguments, context, messages, current_image_refs) {
        Ok(images) => images,
        Err(error) => return error,
    };
    if images.is_empty() {
        return json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "missing_image",
            "message": "vision_analyze requires image_url/image_urls/url/urls/image/images or workspace path/paths. No image references were found in the current request."
        });
    }
    let payload = match endpoint_kind {
        VisualEndpointKind::ChatCompletions => {
            chat_completions_payload(&config.model, &prompt, &images)
        }
        VisualEndpointKind::Responses => responses_payload(&config.model, &prompt, &images),
    };
    let response = match client
        .post(endpoint.clone())
        .bearer_auth(&config.api_key)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "CodeSeeX Vision")
        .json(&payload)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({
                "ok": false,
                "tool": ANALYZE_TOOL_NAME,
                "error": "request_failed",
                "message": format!("Vision request failed: {}", request_error_message(&error)),
                "prompt_sent": prompt,
                "prompt_source": prompt_source
            });
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let message = upstream_http_error_message(status, body.trim(), "Vision analyze endpoint");
        return json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "upstream_error",
            "status": status.as_u16(),
            "message": message,
            "prompt_sent": prompt,
            "prompt_source": prompt_source
        });
    }

    let body = match response.json::<Value>().await {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "ok": false,
                "tool": ANALYZE_TOOL_NAME,
                "error": "invalid_response",
                "message": format!("Vision endpoint returned invalid JSON: {error}"),
                "prompt_sent": prompt,
                "prompt_source": prompt_source
            });
        }
    };
    let text = extract_visual_text(&body);
    if text.trim().is_empty() {
        return json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "empty_response",
            "message": "Vision endpoint returned JSON but no readable text content.",
            "status": status.as_u16(),
            "model": config.model,
            "image_count": images.len(),
            "prompt_sent": prompt,
            "prompt_source": prompt_source,
            "response_shape": response_shape_summary(&body)
        });
    }

    json!({
        "ok": true,
        "tool": ANALYZE_TOOL_NAME,
        "model": config.model,
        "endpoint_kind": endpoint_kind_name(endpoint_kind),
        "image_count": images.len(),
        "prompt_sent": prompt,
        "prompt_source": prompt_source,
        "text": text,
        "usage": body.get("usage").cloned().unwrap_or(Value::Null)
    })
}

pub(crate) async fn execute_generate(
    client: &reqwest::Client,
    app_config: &AppConfig,
    tool_name: &'static str,
    arguments: &Value,
) -> Value {
    let config = match VisionGenerateConfig::load(app_config, tool_name) {
        Ok(config) => config,
        Err(error) => return error,
    };
    let (endpoint, endpoint_kind) = match generation_endpoint(&config.request_url) {
        Ok(value) => value,
        Err(message) => {
            return unavailable(
                tool_name,
                vec![GENERATE_URL_KEY],
                Some(format!("Invalid Vision generation request URL: {message}")),
            );
        }
    };
    let prompt = match generate_prompt(tool_name, arguments) {
        Ok(prompt) => prompt,
        Err(error) => return error,
    };
    let payload = match endpoint_kind {
        GenerateEndpointKind::ImagesGenerations => {
            image_generations_payload(&config.model, &prompt, arguments)
        }
        GenerateEndpointKind::Responses => {
            responses_generation_payload(&config.model, &prompt, arguments)
        }
    };
    let response = match client
        .post(endpoint.clone())
        .bearer_auth(&config.api_key)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, "CodeSeeX Vision")
        .json(&payload)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({
                "ok": false,
                "tool": tool_name,
                "error": "request_failed",
                "message": format!("Vision generation request failed: {}", request_error_message(&error)),
                "prompt_sent": prompt
            });
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let message =
            upstream_http_error_message(status, body.trim(), "Vision generation endpoint");
        return json!({
            "ok": false,
            "tool": tool_name,
            "error": "upstream_error",
            "status": status.as_u16(),
            "message": message,
            "prompt_sent": prompt
        });
    }

    let body = match response.json::<Value>().await {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "ok": false,
                "tool": tool_name,
                "error": "invalid_response",
                "message": format!("Vision generation endpoint returned invalid JSON: {error}"),
                "prompt_sent": prompt
            });
        }
    };
    let images = match extract_generated_images(app_config, &body, arguments, tool_name) {
        Ok(images) => images,
        Err(error) => return error,
    };
    if images.is_empty() {
        return json!({
            "ok": false,
            "tool": tool_name,
            "error": "empty_response",
            "message": "Vision generation endpoint returned JSON but no readable image URL or base64 image.",
            "status": status.as_u16(),
            "model": config.model,
            "prompt_sent": prompt,
            "response_shape": response_shape_summary(&body)
        });
    }

    json!({
        "ok": true,
        "tool": tool_name,
        "model": config.model,
        "endpoint_kind": generation_endpoint_kind_name(endpoint_kind),
        "image_count": images.len(),
        "prompt_sent": prompt,
        "images_markdown": generated_images_markdown(&images),
        "images": images,
        "usage": body.get("usage").cloned().unwrap_or(Value::Null)
    })
}

impl VisionAnalyzeConfig {
    fn load(app_config: &AppConfig) -> Result<Self, Value> {
        let settings = read_tool_settings(app_config);
        let request_url = setting_value_opt(&settings, ANALYZE_URL_KEY);
        let model = setting_value_opt(&settings, ANALYZE_MODEL_KEY);
        let api_key = setting_value_opt(&settings, API_KEY_KEY);
        let mut missing = Vec::new();
        if request_url.is_none() {
            missing.push(ANALYZE_URL_KEY);
        }
        if model.is_none() {
            missing.push(ANALYZE_MODEL_KEY);
        }
        if api_key.is_none() {
            missing.push(API_KEY_KEY);
        }
        if !missing.is_empty() {
            return Err(unavailable(ANALYZE_TOOL_NAME, missing, None));
        }
        Ok(Self {
            request_url: request_url.unwrap_or_default(),
            model: model.unwrap_or_default(),
            api_key: api_key.unwrap_or_default(),
        })
    }
}

impl VisionGenerateConfig {
    fn load(app_config: &AppConfig, tool_name: &'static str) -> Result<Self, Value> {
        let settings = read_tool_settings(app_config);
        let request_url = settings
            .get(GENERATE_URL_KEY)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let model = settings
            .get(GENERATE_MODEL_KEY)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let api_key = setting_value_opt(&settings, API_KEY_KEY);
        let mut missing = Vec::new();
        if request_url.is_none() {
            missing.push(GENERATE_URL_KEY);
        }
        if model.is_none() {
            missing.push(GENERATE_MODEL_KEY);
        }
        if api_key.is_none() {
            missing.push(API_KEY_KEY);
        }
        if !missing.is_empty() {
            return Err(unavailable(tool_name, missing, None));
        }
        Ok(Self {
            request_url: request_url.unwrap_or_default(),
            model: model.unwrap_or_default(),
            api_key: api_key.unwrap_or_default(),
        })
    }
}

fn read_tool_settings(app_config: &AppConfig) -> BTreeMap<String, String> {
    let mut settings = UserConfig::read_from(&app_config.config_path())
        .ok()
        .map(|config| crate::config_payload::tool_settings_from_user_config(&config))
        .unwrap_or_default();
    if let Some(api_key) = crate::secrets::vision_api_key(app_config) {
        settings.insert(API_KEY_KEY.to_owned(), api_key);
    }
    settings
}

fn setting_value_opt(settings: &BTreeMap<String, String>, key: &'static str) -> Option<String> {
    settings
        .get(key)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn unavailable(
    tool: &'static str,
    missing_or_invalid: Vec<&'static str>,
    detail: Option<String>,
) -> Value {
    let message = match tool {
        GENERATE_TOOL_NAME | GENERATE_ALIAS_TOOL_NAME => {
            "Vision is unavailable because its generation request URL, generation model, or API key is not configured."
        }
        _ => {
            "Vision is unavailable because its analyze request URL, model, or API key is not configured."
        }
    };
    json!({
        "ok": false,
        "tool": tool,
        "error": "vision_unavailable",
        "message": detail.unwrap_or_else(|| message.to_owned()),
        "missing_or_invalid": missing_or_invalid
    })
}

fn visual_endpoint(raw: &str) -> Result<(reqwest::Url, VisualEndpointKind), String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|error| error.to_string())?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("only http and https endpoints are supported".to_owned()),
    }
    let path = url.path().trim_end_matches('/').to_owned();
    if path.ends_with("/responses") {
        return Ok((url, VisualEndpointKind::Responses));
    }
    if path.ends_with("/chat/completions") {
        return Ok((url, VisualEndpointKind::ChatCompletions));
    }
    Err(
        "analyze URL must be a complete endpoint ending with /responses or /chat/completions"
            .to_owned(),
    )
}

fn generation_endpoint(raw: &str) -> Result<(reqwest::Url, GenerateEndpointKind), String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|error| error.to_string())?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("only http and https endpoints are supported".to_owned()),
    }
    let path = url.path().trim_end_matches('/').to_owned();
    if path.ends_with("/responses") {
        return Ok((url, GenerateEndpointKind::Responses));
    }
    if path.ends_with("/images/generations") {
        return Ok((url, GenerateEndpointKind::ImagesGenerations));
    }
    Err(
        "generation URL must be a complete endpoint ending with /responses or /images/generations"
            .to_owned(),
    )
}

fn chat_completions_payload(model: &str, prompt: &str, images: &[String]) -> Value {
    let mut content = vec![json!({ "type": "text", "text": prompt })];
    content.extend(images.iter().map(|url| {
        json!({
            "type": "image_url",
            "image_url": { "url": url }
        })
    }));
    json!({
        "model": model,
        "messages": [{ "role": "user", "content": content }],
        "stream": false
    })
}

fn responses_payload(model: &str, prompt: &str, images: &[String]) -> Value {
    let mut content = vec![json!({ "type": "input_text", "text": prompt })];
    content.extend(images.iter().map(|url| {
        json!({
            "type": "input_image",
            "image_url": url
        })
    }));
    json!({
        "model": model,
        "input": [{ "role": "user", "content": content }]
    })
}

fn endpoint_kind_name(kind: VisualEndpointKind) -> &'static str {
    match kind {
        VisualEndpointKind::ChatCompletions => "chat_completions",
        VisualEndpointKind::Responses => "responses",
    }
}

fn generation_endpoint_kind_name(kind: GenerateEndpointKind) -> &'static str {
    match kind {
        GenerateEndpointKind::ImagesGenerations => "images_generations",
        GenerateEndpointKind::Responses => "responses",
    }
}

fn generate_prompt(tool_name: &'static str, arguments: &Value) -> Result<String, Value> {
    let Some(value) = string_arg_exact(arguments, &["prompt", "description", "input"]) else {
        return Err(json!({
            "ok": false,
            "tool": tool_name,
            "error": "missing_prompt",
            "message": "image_gen requires prompt."
        }));
    };
    checked_prompt(tool_name, value, MAX_GENERATE_PROMPT_CHARS)
}

fn image_generations_payload(model: &str, prompt: &str, arguments: &Value) -> Value {
    let mut payload = Map::new();
    payload.insert("model".to_owned(), Value::String(model.to_owned()));
    payload.insert("prompt".to_owned(), Value::String(prompt.to_owned()));
    payload.insert(
        "size".to_owned(),
        Value::String(
            string_arg(arguments, &["size"])
                .unwrap_or(DEFAULT_GENERATE_SIZE)
                .to_owned(),
        ),
    );
    if let Some(n) = integer_arg(arguments, "n").filter(|value| *value > 0) {
        payload.insert("n".to_owned(), Value::Number(n.into()));
    } else {
        payload.insert("n".to_owned(), Value::Number(1_u64.into()));
    }
    for key in [
        "quality",
        "background",
        "output_format",
        "response_format",
        "style",
        "moderation",
        "user",
    ] {
        if let Some(value) = string_arg(arguments, &[key]) {
            payload.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
    Value::Object(payload)
}

fn responses_generation_payload(model: &str, prompt: &str, arguments: &Value) -> Value {
    let mut tool = Map::new();
    tool.insert(
        "type".to_owned(),
        Value::String("image_generation".to_owned()),
    );
    for key in ["size", "quality", "background", "output_format", "style"] {
        if let Some(value) = string_arg(arguments, &[key]) {
            tool.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
    json!({
        "model": model,
        "input": prompt,
        "tools": [Value::Object(tool)]
    })
}

fn integer_arg(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str()?.trim().parse().ok())
    })
}

fn visual_prompt(arguments: &Value, messages: &[Value]) -> Result<(String, &'static str), Value> {
    if let Some(value) = string_arg_exact(arguments, &["prompt", "query", "question"]) {
        return checked_prompt(ANALYZE_TOOL_NAME, value, MAX_PROMPT_CHARS)
            .map(|prompt| (prompt, "argument"));
    }
    if let Some(value) = latest_user_request_text(messages) {
        return checked_prompt(ANALYZE_TOOL_NAME, &value, MAX_PROMPT_CHARS)
            .map(|prompt| (prompt, "latest_user_message"));
    }
    Ok((
        "Describe the image and extract visible text, objects, UI state, and any details relevant to the user's request.".to_owned(),
        "default",
    ))
}

fn checked_prompt(tool: &'static str, value: &str, max_chars: usize) -> Result<String, Value> {
    let chars = value.chars().count();
    if chars > max_chars {
        return Err(json!({
            "ok": false,
            "tool": tool,
            "error": "prompt_too_long",
            "message": "Vision prompt is too long to send without truncation.",
            "chars": chars,
            "max_chars": max_chars
        }));
    }
    Ok(value.to_owned())
}

fn string_arg_exact<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

fn latest_user_request_text(messages: &[Value]) -> Option<String> {
    for message in messages.iter().rev() {
        if message.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let text = collect_text_content(message.get("content")?);
        let request = extract_codex_request_text(&text).unwrap_or(text);
        if !request.trim().is_empty() {
            return Some(request);
        }
    }
    None
}

fn collect_text_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.to_owned(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn extract_codex_request_text(text: &str) -> Option<String> {
    let marker = "## My request for Codex:";
    let (_, request) = text.split_once(marker)?;
    Some(request.trim_matches(['\r', '\n']).to_owned())
}

fn visual_images(
    arguments: &Value,
    context: &ToolExecutionContext,
    messages: &[Value],
    current_image_refs: &[String],
) -> Result<Vec<String>, Value> {
    let mut images = Vec::new();
    let mut seen = HashSet::new();
    let mut explicit_error = None;
    for value in string_args(
        arguments,
        &[
            "image_url",
            "image_urls",
            "url",
            "urls",
            "image",
            "images",
            "file_url",
            "file_urls",
            "source",
            "sources",
        ],
    ) {
        if let Err(error) = push_image_reference(value, context, &mut images, &mut seen) {
            explicit_error.get_or_insert(error);
        }
    }
    for raw_path in string_args(arguments, &["path", "paths"]) {
        match image_path_to_data_url(context, raw_path) {
            Ok(data_url) => {
                if let Err(error) = push_image_reference(&data_url, context, &mut images, &mut seen)
                {
                    explicit_error.get_or_insert(error);
                }
            }
            Err(error) => {
                explicit_error.get_or_insert(error);
            }
        }
    }
    if images.is_empty() {
        match collect_current_image_references(current_image_refs, context, &mut images, &mut seen)
        {
            Ok(()) if !images.is_empty() => {}
            Ok(()) => {}
            Err(error) => {
                if explicit_error.is_none() {
                    explicit_error = Some(error);
                }
            }
        }
    }
    if images.is_empty() {
        match collect_message_image_references(messages, context, &mut images, &mut seen) {
            Ok(()) if !images.is_empty() => {}
            Ok(()) => {
                if let Some(error) = explicit_error {
                    return Err(error);
                }
            }
            Err(error) => {
                return Err(explicit_error.unwrap_or(error));
            }
        }
    }
    images.truncate(MAX_IMAGE_REFERENCES);
    Ok(images)
}

fn collect_current_image_references(
    current_image_refs: &[String],
    context: &ToolExecutionContext,
    output: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), Value> {
    for reference in current_image_refs {
        push_image_reference(reference, context, output, seen)?;
        if output.len() >= MAX_IMAGE_REFERENCES {
            break;
        }
    }
    Ok(())
}

fn string_arg<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn string_args<'a>(value: &'a Value, keys: &[&str]) -> Vec<&'a str> {
    let mut output = Vec::new();
    for key in keys {
        match value.get(*key) {
            Some(Value::String(text)) => output.push(text.as_str()),
            Some(Value::Array(items)) => {
                output.extend(items.iter().filter_map(Value::as_str));
            }
            _ => {}
        }
    }
    output
        .into_iter()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect()
}

fn collect_message_image_references(
    messages: &[Value],
    context: &ToolExecutionContext,
    output: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), Value> {
    for message in messages.iter().rev() {
        collect_image_references_from_value(message, context, output, seen, 0)?;
        if output.len() >= MAX_IMAGE_REFERENCES {
            break;
        }
    }
    Ok(())
}

fn collect_image_references_from_value(
    value: &Value,
    context: &ToolExecutionContext,
    output: &mut Vec<String>,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Result<(), Value> {
    if depth > 8 || output.len() >= MAX_IMAGE_REFERENCES {
        return Ok(());
    }
    match value {
        Value::Object(object) => {
            if let Some(image_url) = object.get("image_url") {
                match image_url {
                    Value::String(text) => push_image_reference(text, context, output, seen)?,
                    Value::Object(inner) => {
                        if let Some(url) = inner.get("url").and_then(Value::as_str) {
                            push_image_reference(url, context, output, seen)?;
                        }
                    }
                    _ => {}
                }
            }
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "input_image" | "image"))
            {
                if let Some(url) = object
                    .get("url")
                    .or_else(|| object.get("image"))
                    .or_else(|| object.get("data_url"))
                    .and_then(Value::as_str)
                {
                    push_image_reference(url, context, output, seen)?;
                }
            }
            for child in object.values() {
                collect_image_references_from_value(child, context, output, seen, depth + 1)?;
                if output.len() >= MAX_IMAGE_REFERENCES {
                    break;
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_image_references_from_value(child, context, output, seen, depth + 1)?;
                if output.len() >= MAX_IMAGE_REFERENCES {
                    break;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn push_image_reference(
    raw: &str,
    context: &ToolExecutionContext,
    output: &mut Vec<String>,
    seen: &mut HashSet<String>,
) -> Result<(), Value> {
    if output.len() >= MAX_IMAGE_REFERENCES {
        return Ok(());
    }
    let reference = resolve_image_reference(raw, context)?;
    if seen.insert(reference.clone()) {
        output.push(reference);
    }
    Ok(())
}

fn resolve_image_reference(raw: &str, context: &ToolExecutionContext) -> Result<String, Value> {
    let value = raw.trim();
    if value.len() > MAX_IMAGE_URL_CHARS {
        return Err(json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "image_reference_too_large",
            "message": "Image reference is too large for vision_analyze."
        }));
    }
    if value.starts_with("data:image/") {
        return Ok(value.to_owned());
    }
    if looks_like_host_path(value) {
        return image_path_to_data_url(context, value);
    }
    if let Ok(parsed) = reqwest::Url::parse(value) {
        return match parsed.scheme() {
            "http" | "https" => Ok(value.to_owned()),
            "file" => {
                let path = file_url_to_path(&parsed)?;
                image_path_to_data_url(context, &path.to_string_lossy())
            }
            _ => Err(json!({
                "ok": false,
                "tool": ANALYZE_TOOL_NAME,
                "error": "unsupported_image_reference",
                "message": "Image references must be HTTP(S), data:image URLs, file:// URLs, or local/workspace paths."
            })),
        };
    }
    image_path_to_data_url(context, value)
}

fn looks_like_host_path(value: &str) -> bool {
    let value = value.trim();
    has_windows_drive_prefix(value)
        || value.starts_with("\\\\")
        || value.starts_with("//")
        || Path::new(value).is_absolute()
        || value.starts_with("./")
        || value.starts_with(".\\")
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

fn file_url_to_path(url: &reqwest::Url) -> Result<PathBuf, Value> {
    url.to_file_path().map_err(|_| {
        json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "unsupported_file_url",
            "message": "file:// image URLs must point to a local file path."
        })
    })
}

fn image_path_to_data_url(context: &ToolExecutionContext, raw_path: &str) -> Result<String, Value> {
    let resolved = context
        .resolve_path(raw_path)
        .map_err(permission_error_to_value)?;
    let metadata = fs::metadata(&resolved.path).map_err(|_| {
        json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "image_not_found",
            "path": resolved.display_path
        })
    })?;
    if !metadata.is_file() {
        return Err(json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "not_file",
            "path": resolved.display_path
        }));
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Err(json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "image_too_large",
            "path": resolved.display_path,
            "bytes": metadata.len(),
            "max_bytes": MAX_IMAGE_BYTES
        }));
    }
    let mime = mime_from_path(&resolved.path).ok_or_else(|| {
        json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "unsupported_image_type",
            "path": resolved.display_path,
            "message": "Supported local image types are png, jpeg, webp, and gif."
        })
    })?;
    let bytes = fs::read(&resolved.path).map_err(|error| {
        json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "image_read_failed",
            "path": resolved.display_path,
            "message": error.to_string()
        })
    })?;
    Ok(format!(
        "data:{mime};base64,{}",
        BASE64_STANDARD.encode(bytes)
    ))
}

fn permission_error_to_value(error: ToolPermissionError) -> Value {
    match error {
        ToolPermissionError::WorkspaceRootNotConfigured => json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "workspace_root_not_configured",
            "message": "No Codex workspace root was available for resolving the vision_analyze image path."
        }),
        ToolPermissionError::PathOutsideWorkspace { path } => json!({
            "ok": false,
            "tool": ANALYZE_TOOL_NAME,
            "error": "path_outside_workspace",
            "message": "Image path must stay inside the authorized workspace unless full file access is active.",
            "path": path
        }),
    }
}

fn mime_from_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

fn extract_visual_text(value: &Value) -> String {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return text.to_owned();
    }
    for pointer in [
        "/choices/0/message/content",
        "/choices/0/delta/content",
        "/output/0/content/0/text",
    ] {
        if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
            return text.to_owned();
        }
    }
    if let Some(content) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_array)
    {
        let text = content
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        let mut parts = Vec::new();
        for item in output {
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for part in content {
                    if let Some(text) = part
                        .get("text")
                        .or_else(|| part.get("output_text"))
                        .and_then(Value::as_str)
                    {
                        parts.push(text);
                    }
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

fn response_shape_summary(value: &Value) -> Value {
    summarize_json_shape(value, 0)
}

fn summarize_json_shape(value: &Value, depth: usize) -> Value {
    const MAX_DEPTH: usize = 5;
    const MAX_OBJECT_KEYS: usize = 16;
    const MAX_ARRAY_ITEMS: usize = 4;
    if depth >= MAX_DEPTH {
        return json!({ "type": value_kind(value) });
    }
    match value {
        Value::Object(object) => {
            let mut fields = Map::new();
            let omitted = object.len().saturating_sub(MAX_OBJECT_KEYS);
            for (key, child) in object.iter().take(MAX_OBJECT_KEYS) {
                fields.insert(key.clone(), summarize_json_shape(child, depth + 1));
            }
            json!({
                "type": "object",
                "keys": object.keys().take(MAX_OBJECT_KEYS).cloned().collect::<Vec<_>>(),
                "omitted_keys": omitted,
                "fields": fields
            })
        }
        Value::Array(items) => json!({
            "type": "array",
            "len": items.len(),
            "items": items
                .iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|item| summarize_json_shape(item, depth + 1))
                .collect::<Vec<_>>(),
            "omitted_items": items.len().saturating_sub(MAX_ARRAY_ITEMS)
        }),
        _ => json!({ "type": value_kind(value) }),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn extract_generated_images(
    app_config: &AppConfig,
    value: &Value,
    arguments: &Value,
    tool_name: &'static str,
) -> Result<Vec<Value>, Value> {
    let mut images = Vec::new();
    let mut seen = HashSet::new();
    if let Some(items) = value.get("data").and_then(Value::as_array) {
        for item in items {
            collect_generated_images(
                app_config,
                item,
                arguments,
                tool_name,
                &mut images,
                &mut seen,
                true,
                None,
                0,
            )?;
        }
    }
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            collect_generated_images(
                app_config,
                item,
                arguments,
                tool_name,
                &mut images,
                &mut seen,
                generated_image_object_context(item),
                None,
                0,
            )?;
        }
    }
    Ok(images)
}

#[allow(clippy::too_many_arguments)]
fn collect_generated_images(
    app_config: &AppConfig,
    item: &Value,
    arguments: &Value,
    tool_name: &'static str,
    images: &mut Vec<Value>,
    seen: &mut HashSet<u64>,
    image_context: bool,
    revised_prompt: Option<&Value>,
    depth: usize,
) -> Result<(), Value> {
    if depth > MAX_GENERATED_IMAGE_SCAN_DEPTH {
        return Ok(());
    }
    match item {
        Value::Object(object) => {
            let current_revised_prompt = item.get("revised_prompt").or(revised_prompt);
            let current_context = image_context || object_suggests_generated_image(object);
            if current_context {
                push_generated_image(
                    app_config,
                    item,
                    arguments,
                    tool_name,
                    images,
                    seen,
                    current_revised_prompt,
                )?;
            }
            for (key, child) in object {
                if key == "revised_prompt" {
                    continue;
                }
                let child_context = current_context
                    || generated_image_field_key(key)
                    || generated_image_object_context(child);
                collect_generated_images(
                    app_config,
                    child,
                    arguments,
                    tool_name,
                    images,
                    seen,
                    child_context,
                    current_revised_prompt,
                    depth + 1,
                )?;
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_generated_images(
                    app_config,
                    child,
                    arguments,
                    tool_name,
                    images,
                    seen,
                    image_context || generated_image_object_context(child),
                    revised_prompt,
                    depth + 1,
                )?;
            }
        }
        Value::String(raw) if image_context => {
            push_generated_image_string(
                app_config,
                raw,
                arguments,
                tool_name,
                images,
                seen,
                revised_prompt,
                false,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn push_generated_image(
    app_config: &AppConfig,
    item: &Value,
    arguments: &Value,
    tool_name: &'static str,
    images: &mut Vec<Value>,
    seen: &mut HashSet<u64>,
    revised_prompt: Option<&Value>,
) -> Result<(), Value> {
    if let Some(url) = item
        .get("url")
        .or_else(|| item.get("image_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_generated_url(
            app_config,
            url,
            arguments,
            tool_name,
            images,
            seen,
            revised_prompt,
        )?;
    }
    if let Some(url) = item
        .get("image_url")
        .and_then(Value::as_object)
        .and_then(|object| object.get("url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        push_generated_url(
            app_config,
            url,
            arguments,
            tool_name,
            images,
            seen,
            revised_prompt,
        )?;
    }
    for key in ["b64_json", "base64", "b64", "image_base64", "result"] {
        if let Some(raw) = item.get(key).and_then(Value::as_str) {
            push_generated_image_string(
                app_config,
                raw,
                arguments,
                tool_name,
                images,
                seen,
                revised_prompt,
                true,
            )?;
        }
    }
    if let Some(raw) = item.get("data").and_then(Value::as_str) {
        let strict = object_image_mime(item);
        push_generated_image_string(
            app_config,
            raw,
            arguments,
            tool_name,
            images,
            seen,
            revised_prompt,
            strict,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_generated_url(
    app_config: &AppConfig,
    url: &str,
    arguments: &Value,
    tool_name: &'static str,
    images: &mut Vec<Value>,
    seen: &mut HashSet<u64>,
    revised_prompt: Option<&Value>,
) -> Result<(), Value> {
    if !seen.insert(generated_image_fingerprint("url", url)) {
        return Ok(());
    }
    if url.starts_with("data:image/") {
        let saved = attach_revised_prompt(
            save_generated_data_url(app_config, url, arguments, tool_name)?,
            revised_prompt,
        );
        images.push(saved);
    } else {
        images.push(json!({
            "type": "url",
            "url": url,
            "revised_prompt": revised_prompt.cloned().unwrap_or(Value::Null)
        }));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn push_generated_image_string(
    app_config: &AppConfig,
    raw: &str,
    arguments: &Value,
    tool_name: &'static str,
    images: &mut Vec<Value>,
    seen: &mut HashSet<u64>,
    revised_prompt: Option<&Value>,
    strict_base64: bool,
) -> Result<(), Value> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(());
    }
    if raw.starts_with("data:image/") {
        if !seen.insert(generated_image_fingerprint("data_url", raw)) {
            return Ok(());
        }
        let saved = attach_revised_prompt(
            save_generated_data_url(app_config, raw, arguments, tool_name)?,
            revised_prompt,
        );
        images.push(saved);
        return Ok(());
    }
    if !strict_base64 && !looks_like_base64_image(raw) {
        return Ok(());
    }
    if !seen.insert(generated_image_fingerprint("base64", raw)) {
        return Ok(());
    }
    let saved = attach_revised_prompt(
        save_generated_base64(app_config, raw, arguments, tool_name)?,
        revised_prompt,
    );
    images.push(saved);
    Ok(())
}

fn generated_image_object_context(value: &Value) -> bool {
    value
        .as_object()
        .map(object_suggests_generated_image)
        .unwrap_or(false)
}

fn object_suggests_generated_image(object: &Map<String, Value>) -> bool {
    object_image_mime(&Value::Object(object.clone()))
        || object
            .get("type")
            .or_else(|| object.get("kind"))
            .or_else(|| object.get("name"))
            .and_then(Value::as_str)
            .map(|value| value.to_ascii_lowercase().contains("image"))
            .unwrap_or(false)
        || [
            "b64_json",
            "base64",
            "image_base64",
            "image_url",
            "image",
            "file_id",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
}

fn object_image_mime(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|object| {
            object
                .get("mime_type")
                .or_else(|| object.get("media_type"))
                .or_else(|| object.get("mime"))
        })
        .and_then(Value::as_str)
        .map(|value| value.starts_with("image/"))
        .unwrap_or(false)
}

fn generated_image_field_key(key: &str) -> bool {
    matches!(
        key,
        "image"
            | "images"
            | "image_url"
            | "output_image"
            | "generated_image"
            | "source"
            | "result"
            | "data"
            | "b64_json"
            | "base64"
            | "image_base64"
    )
}

fn looks_like_base64_image(raw: &str) -> bool {
    let Ok(bytes) = BASE64_STANDARD.decode(raw.trim()) else {
        return false;
    };
    image_mime_from_bytes(&bytes).is_some()
}

fn generated_image_fingerprint(kind: &str, raw: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    raw.hash(&mut hasher);
    hasher.finish()
}

fn generated_images_markdown(images: &[Value]) -> String {
    images
        .iter()
        .filter_map(generated_image_markdown)
        .collect::<Vec<_>>()
        .join("\n")
}

fn generated_image_markdown(image: &Value) -> Option<String> {
    if let Some(markdown) = image.get("markdown").and_then(Value::as_str) {
        return Some(markdown.to_owned());
    }
    if let Some(url) = image.get("url").and_then(Value::as_str) {
        return Some(format!("![generated image]({url})"));
    }
    image
        .get("path")
        .and_then(Value::as_str)
        .map(|path| format!("![generated image]({})", path.replace('\\', "/")))
}

fn attach_revised_prompt(mut value: Value, revised_prompt: Option<&Value>) -> Value {
    if let Some(revised_prompt) = revised_prompt {
        if let Some(object) = value.as_object_mut() {
            object.insert("revised_prompt".to_owned(), revised_prompt.clone());
        }
    }
    value
}

fn save_generated_data_url(
    app_config: &AppConfig,
    data_url: &str,
    arguments: &Value,
    tool_name: &'static str,
) -> Result<Value, Value> {
    let Some((metadata, encoded)) = data_url.split_once(',') else {
        return Err(generation_save_error(
            tool_name,
            "Generated image data URL is malformed.",
        ));
    };
    let mime = metadata
        .strip_prefix("data:")
        .and_then(|value| value.split_once(';').map(|(mime, _)| mime))
        .unwrap_or("image/png");
    save_generated_bytes(
        app_config,
        BASE64_STANDARD.decode(encoded.trim()).map_err(|_| {
            generation_save_error(tool_name, "Generated image base64 could not be decoded.")
        })?,
        mime,
        arguments,
        tool_name,
    )
}

fn save_generated_base64(
    app_config: &AppConfig,
    raw: &str,
    arguments: &Value,
    tool_name: &'static str,
) -> Result<Value, Value> {
    if raw.trim_start().starts_with("data:image/") {
        return save_generated_data_url(app_config, raw, arguments, tool_name);
    }
    let bytes = BASE64_STANDARD.decode(raw.trim()).map_err(|_| {
        generation_save_error(tool_name, "Generated image base64 could not be decoded.")
    })?;
    let mime = image_mime_from_bytes(&bytes)
        .or_else(|| requested_output_mime(arguments))
        .unwrap_or("image/png");
    save_generated_bytes(app_config, bytes, mime, arguments, tool_name)
}

fn save_generated_bytes(
    app_config: &AppConfig,
    bytes: Vec<u8>,
    mime: &str,
    arguments: &Value,
    tool_name: &'static str,
) -> Result<Value, Value> {
    let dir = app_config.data_dir.join("generated-images");
    fs::create_dir_all(&dir).map_err(|error| {
        generation_save_error(
            tool_name,
            &format!("Could not create generated image directory: {error}"),
        )
    })?;
    let extension = image_extension_for_mime(mime)
        .or_else(|| requested_output_extension(arguments))
        .unwrap_or("png");
    let path = unique_generated_image_path(&dir, extension);
    fs::write(&path, bytes).map_err(|error| {
        generation_save_error(
            tool_name,
            &format!("Could not write generated image: {error}"),
        )
    })?;
    let path_text = path.to_string_lossy().to_string();
    let markdown_path = path_text.replace('\\', "/");
    let markdown = format!("![generated image]({markdown_path})");
    Ok(json!({
        "type": "file",
        "path": path_text,
        "markdown_path": markdown_path,
        "markdown": markdown,
        "mime": mime
    }))
}

fn unique_generated_image_path(dir: &Path, extension: &str) -> PathBuf {
    dir.join(format!(
        "vision-{}.{}",
        Uuid::new_v4().simple(),
        extension.trim_start_matches('.')
    ))
}

fn image_mime_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12).is_some_and(|value| value == b"WEBP") {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    None
}

fn requested_output_mime(arguments: &Value) -> Option<&'static str> {
    requested_output_extension(arguments).and_then(|extension| match extension {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    })
}

fn requested_output_extension(arguments: &Value) -> Option<&'static str> {
    match string_arg(arguments, &["output_format", "format"])
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        _ => None,
    }
}

fn image_extension_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn generation_save_error(tool_name: &'static str, message: &str) -> Value {
    json!({
        "ok": false,
        "tool": tool_name,
        "error": "image_save_failed",
        "message": message
    })
}

fn request_error_message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = std::error::Error::source(error);
    }
    message
}

fn upstream_http_error_message(
    status: reqwest::StatusCode,
    body: &str,
    endpoint_label: &str,
) -> String {
    if body.trim().is_empty() {
        return format!("{endpoint_label} returned HTTP {}", status.as_u16());
    }
    truncate_chars(body.trim(), MAX_ERROR_CHARS)
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let mut output = value.chars().take(max).collect::<String>();
    output.push_str("...");
    output
}

#[cfg(test)]
mod tests {
    use axum::{extract::State, routing::post, Json, Router};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeVisionState {
        requests: Arc<Mutex<Vec<Value>>>,
    }

    async fn fake_image_generations(
        State(state): State<FakeVisionState>,
        Json(payload): Json<Value>,
    ) -> Json<Value> {
        state
            .requests
            .lock()
            .expect("fake vision lock poisoned")
            .push(payload);
        Json(json!({
            "data": [{
                "b64_json": BASE64_STANDARD.encode(b"\x89PNG\r\n\x1a\nfake"),
                "revised_prompt": "Draw a tiny red cube."
            }],
            "usage": { "total_tokens": 42 }
        }))
    }

    #[test]
    fn chat_completions_endpoint_is_detected_without_rewrite() {
        let (url, kind) =
            visual_endpoint("https://api.example.com/v1/chat/completions").expect("endpoint");
        assert_eq!(kind, VisualEndpointKind::ChatCompletions);
        assert_eq!(url.as_str(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn responses_endpoint_is_detected_without_rewrite() {
        let (url, kind) =
            visual_endpoint("https://api.example.com/v1/responses").expect("endpoint");
        assert_eq!(kind, VisualEndpointKind::Responses);
        assert_eq!(url.as_str(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn analyze_base_url_is_rejected_instead_of_rewritten() {
        let error = visual_endpoint("https://api.example.com/v1").expect_err("base url rejected");
        assert!(error.contains("/responses"));
        assert!(error.contains("/chat/completions"));
    }

    #[test]
    fn image_generations_endpoint_is_detected_without_rewrite() {
        let (url, kind) =
            generation_endpoint("https://api.example.com/v1/images/generations").expect("endpoint");
        assert_eq!(kind, GenerateEndpointKind::ImagesGenerations);
        assert_eq!(
            url.as_str(),
            "https://api.example.com/v1/images/generations"
        );
    }

    #[test]
    fn generation_responses_endpoint_is_detected_without_rewrite() {
        let (url, kind) = generation_endpoint("https://api.example.com/v1/responses")
            .expect("responses endpoint");
        assert_eq!(kind, GenerateEndpointKind::Responses);
        assert_eq!(url.as_str(), "https://api.example.com/v1/responses");
    }

    #[test]
    fn generation_base_url_is_rejected_instead_of_rewritten() {
        let error =
            generation_endpoint("https://api.example.com/codex").expect_err("base url rejected");
        assert!(error.contains("/responses"));
        assert!(error.contains("/images/generations"));
    }

    #[test]
    fn upstream_http_error_message_falls_back_when_body_is_empty() {
        let message = upstream_http_error_message(
            reqwest::StatusCode::BAD_GATEWAY,
            "   ",
            "Vision generation endpoint",
        );

        assert_eq!(message, "Vision generation endpoint returned HTTP 502");
    }

    #[test]
    fn extracts_chat_completions_text() {
        let text = extract_visual_text(&json!({
            "choices": [{
                "message": { "content": "  A chart with three bars.\n" }
            }]
        }));
        assert_eq!(text, "  A chart with three bars.\n");
    }

    #[test]
    fn extracts_responses_text() {
        let text = extract_visual_text(&json!({
            "output": [{
                "content": [{ "type": "output_text", "text": "\nA screenshot of settings.  " }]
            }]
        }));
        assert_eq!(text, "\nA screenshot of settings.  ");
    }

    #[test]
    fn visual_prompt_preserves_argument_text() {
        let (prompt, source) = visual_prompt(&json!({ "prompt": "  请逐字识别图片文字。\n" }), &[])
            .expect("visual prompt");
        assert_eq!(prompt, "  请逐字识别图片文字。\n");
        assert_eq!(source, "argument");
    }

    #[test]
    fn visual_prompt_uses_latest_user_request_text() {
        let messages = vec![json!({
            "role": "user",
            "content": "\n# Files mentioned by the user:\n\n## a.png: D:/a.png\n\n## My request for Codex:\n分析这张图\n"
        })];
        let (prompt, source) = visual_prompt(&json!({}), &messages).expect("visual prompt");
        assert_eq!(prompt, "分析这张图");
        assert_eq!(source, "latest_user_message");
    }

    #[test]
    fn generate_prompt_rejects_instead_of_truncating() {
        let prompt = "x".repeat(MAX_GENERATE_PROMPT_CHARS + 1);
        let error = generate_prompt(GENERATE_ALIAS_TOOL_NAME, &json!({ "prompt": prompt }))
            .expect_err("too long");
        assert_eq!(
            error.get("error").and_then(Value::as_str),
            Some("prompt_too_long")
        );
        assert_eq!(
            error.get("tool").and_then(Value::as_str),
            Some(GENERATE_ALIAS_TOOL_NAME)
        );
    }

    #[test]
    fn generate_prompt_preserves_argument_text() {
        let prompt = "  Draw exactly this label:\nCodeSeeX  ";
        assert_eq!(
            generate_prompt(GENERATE_ALIAS_TOOL_NAME, &json!({ "prompt": prompt }))
                .expect("generate prompt"),
            prompt
        );
    }

    #[test]
    fn response_shape_summary_does_not_include_text_values() {
        let summary = response_shape_summary(&json!({
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": "secret visible text" }]
            }]
        }));
        let serialized = serde_json::to_string(&summary).expect("shape json");
        assert!(serialized.contains("\"text\""));
        assert!(!serialized.contains("secret visible text"));
    }

    #[test]
    fn builds_responses_generation_payload() {
        let payload = responses_generation_payload(
            "gpt-5.5",
            "Draw a small red cube.",
            &json!({ "size": "1024x1024", "output_format": "png" }),
        );
        assert_eq!(
            payload.get("model").and_then(Value::as_str),
            Some("gpt-5.5")
        );
        assert_eq!(
            payload.pointer("/tools/0/type").and_then(Value::as_str),
            Some("image_generation")
        );
        assert_eq!(
            payload.pointer("/tools/0/size").and_then(Value::as_str),
            Some("1024x1024")
        );
    }

    #[test]
    fn image_generation_payload_does_not_send_base64_inputs() {
        let payload = image_generations_payload(
            "gpt-image-1",
            "Draw a small red cube.",
            &json!({ "image": "data:image/png;base64,AAAA", "path": "input.png" }),
        );
        let serialized = serde_json::to_string(&payload).expect("payload json");
        assert!(!serialized.contains("base64"));
        assert!(!serialized.contains("input.png"));
        assert_eq!(
            payload.get("prompt").and_then(Value::as_str),
            Some("Draw a small red cube.")
        );
    }

    #[test]
    fn file_url_image_reference_is_read_by_tool() {
        let root = temp_dir("vision-file-url");
        fs::create_dir_all(&root).expect("create root");
        let image = root.join("sample.png");
        fs::write(&image, b"\x89PNG\r\n\x1a\nfake").expect("write image");
        let file_url = reqwest::Url::from_file_path(&image)
            .expect("file url")
            .to_string();
        let context = ToolExecutionContext::new(vec![root.clone()], false);
        let images = visual_images(&json!({ "image": file_url }), &context, &[], &[])
            .expect("resolve file url image");

        assert_eq!(images.len(), 1);
        assert!(images[0].starts_with("data:image/png;base64,"));
        assert!(!images[0].contains("sample.png"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_explicit_image_reference_falls_back_to_message_input_image() {
        let root = temp_dir("vision-message-image-fallback");
        fs::create_dir_all(&root).expect("create root");
        let context = ToolExecutionContext::new(vec![root.clone()], false);
        let messages = vec![json!({
            "role": "user",
            "content": [
                { "type": "input_text", "text": "Please analyze the attached image." },
                { "type": "input_image", "image_url": "data:image/png;base64,AAAA" }
            ]
        })];

        let images = visual_images(
            &json!({ "image": "missing-file-from-model.png" }),
            &context,
            &messages,
            &[],
        )
        .expect("message image fallback");

        assert_eq!(images, vec!["data:image/png;base64,AAAA"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn absolute_local_image_path_respects_full_access() {
        let workspace = temp_dir("vision-workspace");
        let outside = temp_dir("vision-outside");
        fs::create_dir_all(&workspace).expect("create workspace");
        fs::create_dir_all(&outside).expect("create outside");
        let image = outside.join("outside.png");
        fs::write(&image, b"\x89PNG\r\n\x1a\nfake").expect("write image");
        let raw = image.to_string_lossy().to_string();

        let restricted = ToolExecutionContext::new(vec![workspace.clone()], false);
        let rejected = visual_images(&json!({ "image": raw }), &restricted, &[], &[])
            .expect_err("outside path should be rejected");
        assert_eq!(
            rejected.get("error").and_then(Value::as_str),
            Some("path_outside_workspace")
        );

        let full_access = ToolExecutionContext::new(vec![workspace.clone()], true);
        let images = visual_images(
            &json!({ "image": image.to_string_lossy() }),
            &full_access,
            &[],
            &[],
        )
        .expect("outside path allowed with full access");
        assert_eq!(images.len(), 1);
        assert!(images[0].starts_with("data:image/png;base64,"));

        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn current_input_image_reference_is_used_without_message_pollution() {
        let root = temp_dir("vision-current-input-image");
        fs::create_dir_all(&root).expect("create root");
        let context = ToolExecutionContext::new(vec![root.clone()], false);
        let current_image_refs = vec!["data:image/png;base64,AAAA".to_owned()];

        let images = visual_images(&json!({}), &context, &[], &current_image_refs)
            .expect("current image ref");

        assert_eq!(images, vec!["data:image/png;base64,AAAA"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn saves_generated_base64_images_without_returning_base64() {
        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-vision-generated-{}",
            Uuid::new_v4().simple()
        ));
        let mut config = AppConfig::default();
        config.data_dir = data_dir.clone();
        let images = extract_generated_images(
            &config,
            &json!({
                "data": [{
                    "b64_json": BASE64_STANDARD.encode(b"\x89PNG\r\n\x1a\nfake"),
                    "revised_prompt": "Draw a tiny cube."
                }]
            }),
            &json!({}),
            GENERATE_ALIAS_TOOL_NAME,
        )
        .expect("generated images");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].get("type").and_then(Value::as_str), Some("file"));
        let path = images[0]
            .get("path")
            .and_then(Value::as_str)
            .expect("generated image path");
        assert!(Path::new(path).exists());
        assert!(images[0].get("b64_json").is_none());
        assert!(images[0]
            .get("markdown_path")
            .and_then(Value::as_str)
            .is_some_and(|value| value.contains("generated-images/vision-")));
        assert!(images[0]
            .get("markdown")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("![generated image](")));
        assert_eq!(
            images[0].get("revised_prompt").and_then(Value::as_str),
            Some("Draw a tiny cube.")
        );
        let serialized = serde_json::to_string(&images).expect("images json");
        assert!(!serialized.contains("iVBOR"));
        assert!(!serialized.contains("base64"));
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn extracts_nested_responses_generated_image_shapes() {
        let data_dir = std::env::temp_dir().join(format!(
            "codeseex-vision-nested-generated-{}",
            Uuid::new_v4().simple()
        ));
        let mut config = AppConfig::default();
        config.data_dir = data_dir.clone();
        let image_a = BASE64_STANDARD.encode(b"\x89PNG\r\n\x1a\nnested-a");
        let image_b = BASE64_STANDARD.encode(b"\x89PNG\r\n\x1a\nnested-b");

        let images = extract_generated_images(
            &config,
            &json!({
                "output": [
                    {
                        "type": "message",
                        "content": [{
                            "type": "output_image",
                            "image": {
                                "b64_json": image_a,
                                "revised_prompt": "Nested prompt A"
                            }
                        }]
                    },
                    {
                        "type": "image_generation_call",
                        "revised_prompt": "Nested prompt B",
                        "result": {
                            "media_type": "image/png",
                            "data": image_b
                        }
                    }
                ]
            }),
            &json!({}),
            GENERATE_ALIAS_TOOL_NAME,
        )
        .expect("nested generated images");

        assert_eq!(images.len(), 2);
        for image in &images {
            let path = image
                .get("path")
                .and_then(Value::as_str)
                .expect("generated image path");
            assert!(Path::new(path).exists());
            assert_eq!(image.get("type").and_then(Value::as_str), Some("file"));
        }
        assert_eq!(
            images[0].get("revised_prompt").and_then(Value::as_str),
            Some("Nested prompt A")
        );
        assert_eq!(
            images[1].get("revised_prompt").and_then(Value::as_str),
            Some("Nested prompt B")
        );
        let serialized = serde_json::to_string(&images).expect("images json");
        assert!(!serialized.contains("nested-a"));
        assert!(!serialized.contains("base64"));
        let _ = fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn execute_image_gen_saves_fake_upstream_base64_without_returning_it() {
        let fake_state = FakeVisionState::default();
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake vision");
        let addr = listener.local_addr().expect("fake vision addr");
        let app = Router::new()
            .route("/v1/images/generations", post(fake_image_generations))
            .with_state(fake_state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fake vision server");
        });

        let data_dir = temp_dir("vision-execute-image-gen");
        fs::create_dir_all(&data_dir).expect("create data dir");
        let mut config = AppConfig::default();
        config.data_dir = data_dir.clone();
        let mut settings = BTreeMap::new();
        settings.insert(
            GENERATE_URL_KEY.to_owned(),
            format!("http://{addr}/v1/images/generations"),
        );
        settings.insert(GENERATE_MODEL_KEY.to_owned(), "gpt-image-1".to_owned());
        settings.insert(API_KEY_KEY.to_owned(), "test-key".to_owned());
        UserConfig {
            tools: Some(codeseex_core::UserToolsConfig {
                settings: Some(settings),
                ..Default::default()
            }),
            ..Default::default()
        }
        .write_atomic(&config.config_path())
        .expect("write config");

        let prompt = "  Draw a tiny red cube.\n";
        let result = execute_generate(
            &reqwest::Client::new(),
            &config,
            GENERATE_ALIAS_TOOL_NAME,
            &json!({ "prompt": prompt, "size": "1024x1024" }),
        )
        .await;

        assert_eq!(result.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            result.get("prompt_sent").and_then(Value::as_str),
            Some(prompt)
        );
        let serialized = serde_json::to_string(&result).expect("result json");
        assert!(!serialized.contains("model_instruction"));
        assert!(!serialized.contains("iVBOR"));
        assert!(!serialized.contains("b64_json"));
        assert!(serialized.contains("generated-images"));
        let images_markdown = result
            .get("images_markdown")
            .and_then(Value::as_str)
            .expect("images markdown");
        assert!(images_markdown.starts_with("![generated image]("));
        assert!(!images_markdown.contains("base64"));
        assert!(!images_markdown.contains("iVBOR"));
        let path = result
            .pointer("/images/0/path")
            .and_then(Value::as_str)
            .expect("generated image path");
        assert!(Path::new(path).exists());

        let requests = fake_state
            .requests
            .lock()
            .expect("fake vision lock poisoned")
            .clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].get("prompt").and_then(Value::as_str),
            Some(prompt)
        );
        assert!(!serde_json::to_string(&requests[0])
            .expect("request json")
            .contains("base64"));

        let _ = fs::remove_dir_all(data_dir);
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("codeseex-{label}-{}", Uuid::new_v4().simple()))
    }
}
