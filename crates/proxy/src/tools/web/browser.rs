use codeseex_core::context::redact_inline_data_urls;
use codeseex_core::NetworkProxyMode;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use uuid::Uuid;

use super::candidates::candidate_id_for;
use super::extract::{
    clean_visible_text, decode_text_bytes, extract_html_title, html_to_text, truncate_chars,
};
use super::safety::{validate_public_web_url, validate_web_url_network};
use super::{MAX_BYTES, MAX_TEXT_CHARS};

const BROWSER_RENDER_TIMEOUT_SECS: u64 = 10;
const MAX_BROWSER_STDERR_BYTES: usize = 16 * 1024;
const BROWSER_RENDER_FLAG: &str = "CODESEEX_WEB_BROWSER_RENDER";
const BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG: &str =
    "CODESEEX_WEB_BROWSER_RENDER_UNSAFE_NO_INTERCEPTION";

pub(super) async fn render_public_page(
    proxy_mode: NetworkProxyMode,
    url: &reqwest::Url,
    requested_url: &str,
) -> Value {
    if !render_enabled() {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": browser_renderer_disabled_code(),
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": browser_renderer_disabled_message()
        });
    }
    if let Err(message) = validate_public_web_url(url) {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": "blocked_url",
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": message
        });
    }
    if let Err(message) = validate_web_url_network(url).await {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": "blocked_url",
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": message
        });
    }

    let Some(executable) = find_browser_executable() else {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": "browser_renderer_unavailable",
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": "No supported isolated browser executable was found."
        });
    };

    let profile_dir = isolated_profile_dir();
    if let Err(error) = std::fs::create_dir_all(&profile_dir) {
        return json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": "browser_profile_unavailable",
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": error.to_string()
        });
    }

    let args = browser_args(url.as_str(), &profile_dir, proxy_mode);
    let output = run_browser(&executable, &args).await;
    let _ = std::fs::remove_dir_all(&profile_dir);
    match output {
        Ok(output) => rendered_output_to_result(url, requested_url, &executable, output),
        Err(error) => json!({
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": error.code,
            "url": url.as_str(),
            "requested_url": requested_url,
            "message": error.message,
            "browser": browser_label(&executable)
        }),
    }
}

pub(super) fn should_try_after_http_result(result: &Value) -> bool {
    matches!(
        result.get("error").and_then(Value::as_str),
        Some("request_failed" | "empty_text_content")
    )
}

fn rendered_output_to_result(
    url: &reqwest::Url,
    requested_url: &str,
    executable: &Path,
    output: BrowserOutput,
) -> Value {
    let (html, encoding, had_decode_errors) = decode_text_bytes(&output.stdout, "text/html");
    let title = extract_html_title(&html).unwrap_or_else(|| url.as_str().to_owned());
    let text = html_to_text(&html);
    let full_text_chars = text.chars().count();
    let text = truncate_chars(&redact_inline_data_urls(&text), MAX_TEXT_CHARS);
    let stderr_preview = truncate_chars(
        &redact_inline_data_urls(&clean_visible_text(&String::from_utf8_lossy(
            &output.stderr,
        ))),
        500,
    );
    if text.trim().is_empty() {
        return json!({
            "_diagnostics": browser_diagnostics(executable, &output, encoding, had_decode_errors, false, stderr_preview),
            "ok": false,
            "stage": "open",
            "mode": "open",
            "renderer": "isolated_browser",
            "error": "empty_rendered_text",
            "url": url.as_str(),
            "requested_url": requested_url,
            "title": title,
            "opened": false,
            "bytes": output.stdout.len(),
            "truncated": output.stdout_truncated
        });
    }
    json!({
        "_diagnostics": browser_diagnostics(executable, &output, encoding, had_decode_errors, true, stderr_preview),
        "ok": true,
        "stage": "open",
        "mode": "open",
        "renderer": "isolated_browser",
        "id": candidate_id_for(url.as_str(), &title, requested_url, "isolated_browser"),
        "url": url.as_str(),
        "requested_url": requested_url,
        "status": Value::Null,
        "content_type": "text/html; renderer=isolated_browser",
        "encoding": encoding,
        "decode_errors": had_decode_errors,
        "title": title,
        "snippet": truncate_chars(&text, 500),
        "content": text.clone(),
        "text": text,
        "opened": true,
        "bytes": output.stdout.len(),
        "truncated": output.stdout_truncated || full_text_chars > MAX_TEXT_CHARS
    })
}

fn browser_diagnostics(
    executable: &Path,
    output: &BrowserOutput,
    encoding: &'static str,
    decode_errors: bool,
    readable_text: bool,
    stderr_preview: String,
) -> Value {
    json!({
        "renderer": "isolated_browser",
        "browser": browser_label(executable),
        "profile_isolated": true,
        "shared_cookies": false,
        "extensions_disabled": true,
        "private_network_guard": "chromium_host_resolver_rules",
        "headless": true,
        "exit_status": output.status_code,
        "stdout_bytes": output.stdout.len(),
        "stdout_truncated": output.stdout_truncated,
        "stderr_bytes": output.stderr.len(),
        "stderr_truncated": output.stderr_truncated,
        "stderr_preview": stderr_preview,
        "encoding": encoding,
        "decode_errors": decode_errors,
        "readable_text": readable_text
    })
}

#[derive(Debug)]
struct BrowserOutput {
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stdout_truncated: bool,
    stderr: Vec<u8>,
    stderr_truncated: bool,
}

#[derive(Debug)]
struct BrowserRunError {
    code: &'static str,
    message: String,
}

async fn run_browser(executable: &Path, args: &[String]) -> Result<BrowserOutput, BrowserRunError> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| BrowserRunError {
            code: "browser_spawn_failed",
            message: error.to_string(),
        })?;

    let stdout = child.stdout.take().ok_or_else(|| BrowserRunError {
        code: "browser_stdout_unavailable",
        message: "Browser stdout was unavailable.".to_owned(),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| BrowserRunError {
        code: "browser_stderr_unavailable",
        message: "Browser stderr was unavailable.".to_owned(),
    })?;
    let stdout_task = tokio::spawn(read_limited(stdout, MAX_BYTES as usize));
    let stderr_task = tokio::spawn(read_limited(stderr, MAX_BROWSER_STDERR_BYTES));

    let status = match tokio::time::timeout(
        Duration::from_secs(BROWSER_RENDER_TIMEOUT_SECS),
        child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            return Err(BrowserRunError {
                code: "browser_wait_failed",
                message: error.to_string(),
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(BrowserRunError {
                code: "browser_render_timeout",
                message: format!(
                    "Isolated browser rendering exceeded {BROWSER_RENDER_TIMEOUT_SECS} seconds."
                ),
            });
        }
    };

    let (stdout, stdout_truncated) = stdout_task.await.unwrap_or_default();
    let (stderr, stderr_truncated) = stderr_task.await.unwrap_or_default();
    if !status.success() && stdout.is_empty() {
        return Err(BrowserRunError {
            code: "browser_render_failed",
            message: truncate_chars(&clean_visible_text(&String::from_utf8_lossy(&stderr)), 500),
        });
    }
    Ok(BrowserOutput {
        status_code: status.code(),
        stdout,
        stdout_truncated,
        stderr,
        stderr_truncated,
    })
}

async fn read_limited<R>(mut reader: R, limit: usize) -> (Vec<u8>, bool)
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let Ok(count) = reader.read(&mut buffer).await else {
            break;
        };
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let take = count.min(remaining);
        output.extend_from_slice(&buffer[..take]);
        if take < count {
            truncated = true;
        }
    }
    (output, truncated)
}

fn browser_args(url: &str, profile_dir: &Path, proxy_mode: NetworkProxyMode) -> Vec<String> {
    let mut args = vec![
        "--headless=new".to_owned(),
        "--disable-gpu".to_owned(),
        "--disable-gpu-compositing".to_owned(),
        "--disable-software-rasterizer".to_owned(),
        "--disable-dev-shm-usage".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-default-apps".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-crash-reporter".to_owned(),
        "--disable-sync".to_owned(),
        "--disable-notifications".to_owned(),
        "--deny-permission-prompts".to_owned(),
        "--disable-local-file-accesses".to_owned(),
        "--disable-features=Translate,MediaRouter,AutofillServerCommunication,OptimizationHints,InterestFeedContentSuggestions".to_owned(),
        "--autoplay-policy=user-gesture-required".to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--hide-scrollbars".to_owned(),
        "--mute-audio".to_owned(),
        "--blink-settings=imagesEnabled=false".to_owned(),
        private_network_host_resolver_rules(),
        "--window-size=1280,900".to_owned(),
        format!("--user-data-dir={}", profile_dir.display()),
    ];
    if matches!(proxy_mode, NetworkProxyMode::None) {
        args.push("--no-proxy-server".to_owned());
    }
    args.push("--dump-dom".to_owned());
    args.push(url.to_owned());
    args
}

fn private_network_host_resolver_rules() -> String {
    let rules = [
        "MAP localhost ~NOTFOUND",
        "MAP *.localhost ~NOTFOUND",
        "MAP localhost.localdomain ~NOTFOUND",
        "MAP 127.* ~NOTFOUND",
        "MAP 0.* ~NOTFOUND",
        "MAP 10.* ~NOTFOUND",
        "MAP 172.16.* ~NOTFOUND",
        "MAP 172.17.* ~NOTFOUND",
        "MAP 172.18.* ~NOTFOUND",
        "MAP 172.19.* ~NOTFOUND",
        "MAP 172.20.* ~NOTFOUND",
        "MAP 172.21.* ~NOTFOUND",
        "MAP 172.22.* ~NOTFOUND",
        "MAP 172.23.* ~NOTFOUND",
        "MAP 172.24.* ~NOTFOUND",
        "MAP 172.25.* ~NOTFOUND",
        "MAP 172.26.* ~NOTFOUND",
        "MAP 172.27.* ~NOTFOUND",
        "MAP 172.28.* ~NOTFOUND",
        "MAP 172.29.* ~NOTFOUND",
        "MAP 172.30.* ~NOTFOUND",
        "MAP 172.31.* ~NOTFOUND",
        "MAP 192.168.* ~NOTFOUND",
        "MAP 169.254.* ~NOTFOUND",
        "MAP ::1 ~NOTFOUND",
        "MAP fc00::* ~NOTFOUND",
        "MAP fd00::* ~NOTFOUND",
        "MAP fe80::* ~NOTFOUND",
    ]
    .join(",");
    format!("--host-resolver-rules={rules}")
}

pub(super) fn render_enabled() -> bool {
    if !env_flag(BROWSER_RENDER_FLAG) {
        return false;
    }
    #[cfg(test)]
    {
        env_flag("CODESEEX_WEB_BROWSER_RENDER_TEST")
            && (browser_request_interception_available()
                || env_flag(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG))
    }
    #[cfg(not(test))]
    {
        browser_request_interception_available()
            || env_flag(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG)
    }
}

fn browser_request_interception_available() -> bool {
    false
}

fn browser_renderer_disabled_code() -> &'static str {
    if env_flag(BROWSER_RENDER_FLAG)
        && !browser_request_interception_available()
        && !env_flag(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG)
    {
        "browser_renderer_requires_request_interception"
    } else {
        "browser_renderer_disabled"
    }
}

fn browser_renderer_disabled_message() -> &'static str {
    if browser_renderer_disabled_code() == "browser_renderer_requires_request_interception" {
        "The isolated browser renderer is disabled because the current dump-dom backend cannot revalidate redirects and subresource requests. Use the HTTP text fetcher, or enable only in development with CODESEEX_WEB_BROWSER_RENDER_UNSAFE_NO_INTERCEPTION=1."
    } else {
        "Browser rendering is disabled."
    }
}

fn env_flag(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn isolated_profile_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "codeseex-browser-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ))
}

fn find_browser_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CODESEEX_WEB_BROWSER_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    browser_candidates().into_iter().find(|path| path.is_file())
}

fn browser_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    {
        for env_key in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(root) = std::env::var(env_key) {
                let root = PathBuf::from(root);
                candidates.push(root.join("Microsoft\\Edge\\Application\\msedge.exe"));
                candidates.push(root.join("Google\\Chrome\\Application\\chrome.exe"));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let names = [
            "microsoft-edge",
            "microsoft-edge-stable",
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ];
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                for name in names {
                    candidates.push(dir.join(name));
                }
            }
        }
    }
    candidates
}

fn browser_label(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("browser")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_args_use_isolated_profile_and_no_user_state() {
        let profile = PathBuf::from(r"C:\Temp\codeseex-browser-test");
        let args = browser_args("https://example.com", &profile, NetworkProxyMode::System);

        assert!(args.iter().any(|arg| arg == "--headless=new"));
        assert!(args.iter().any(|arg| arg == "--disable-extensions"));
        assert!(args
            .iter()
            .any(|arg| arg == "--disable-local-file-accesses"));
        assert!(args.iter().any(|arg| arg == "--deny-permission-prompts"));
        assert!(args.iter().any(|arg| arg.starts_with("--user-data-dir=")));
        assert!(args.iter().any(|arg| {
            arg.starts_with("--host-resolver-rules=")
                && arg.contains("MAP localhost ~NOTFOUND")
                && arg.contains("MAP 192.168.* ~NOTFOUND")
                && arg.contains("MAP 10.* ~NOTFOUND")
        }));
        assert!(args.iter().any(|arg| arg == "--dump-dom"));
        assert_eq!(args.last().map(String::as_str), Some("https://example.com"));
        assert!(!args.iter().any(|arg| arg == "--no-proxy-server"));
    }

    #[test]
    fn browser_args_can_disable_proxy_for_direct_mode() {
        let profile = PathBuf::from("/tmp/codeseex-browser-test");
        let args = browser_args("https://example.com", &profile, NetworkProxyMode::None);

        assert!(args.iter().any(|arg| arg == "--no-proxy-server"));
    }

    #[test]
    fn browser_fallback_only_handles_network_or_empty_text_failures() {
        assert!(should_try_after_http_result(
            &json!({ "error": "request_failed" })
        ));
        assert!(should_try_after_http_result(
            &json!({ "error": "empty_text_content" })
        ));
        assert!(!should_try_after_http_result(
            &json!({ "error": "blocked_url" })
        ));
        assert!(!should_try_after_http_result(
            &json!({ "error": "non_text_response" })
        ));
    }

    #[test]
    fn browser_render_requires_explicit_enable_flag() {
        std::env::remove_var("CODESEEX_WEB_BROWSER_RENDER_TEST");
        std::env::remove_var("CODESEEX_WEB_BROWSER_RENDER");
        std::env::remove_var(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG);
        assert!(!render_enabled());

        std::env::set_var("CODESEEX_WEB_BROWSER_RENDER", "1");
        #[cfg(test)]
        assert!(!render_enabled());

        std::env::set_var("CODESEEX_WEB_BROWSER_RENDER", "true");
        std::env::set_var("CODESEEX_WEB_BROWSER_RENDER_TEST", "1");
        assert!(!render_enabled());

        std::env::set_var(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG, "1");
        assert!(render_enabled());

        std::env::set_var("CODESEEX_WEB_BROWSER_RENDER", "false");
        assert!(!render_enabled());
        std::env::remove_var("CODESEEX_WEB_BROWSER_RENDER_TEST");
        std::env::remove_var("CODESEEX_WEB_BROWSER_RENDER");
        std::env::remove_var(BROWSER_RENDER_UNSAFE_NO_INTERCEPTION_FLAG);
    }
}
