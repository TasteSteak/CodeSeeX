use codeseex_core::NetworkProxyMode;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr};

use super::extract::{bytes_have_binary_markers, decode_text_bytes};
use super::MAX_BYTES;

pub(super) fn user_agent() -> &'static str {
    "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)"
}

pub(super) fn web_client(proxy_mode: NetworkProxyMode) -> reqwest::Client {
    crate::network::apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build web client")
}

pub(super) fn no_redirect_client(proxy_mode: NetworkProxyMode) -> reqwest::Client {
    crate::network::apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build no-redirect web client")
}

pub(super) async fn fetch_text(client: &reqwest::Client, url: reqwest::Url, accept: &str) -> Value {
    let response = match client
        .get(url.clone())
        .header(reqwest::header::USER_AGENT, user_agent())
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return json!({
                "ok": false,
                "error": "request_failed",
                "url": url.as_str(),
                "message": request_error_message(&error)
            });
        }
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let (bytes, byte_truncated) = read_limited_response_bytes(response).await;
    if bytes_have_binary_markers(&bytes) {
        return json!({
            "ok": false,
            "error": "binary_response",
            "url": url.as_str(),
            "status": status,
            "content_type": content_type,
            "bytes": bytes.len()
        });
    }
    let (text, encoding, had_decode_errors) = decode_text_bytes(&bytes, &content_type);
    json!({
        "ok": (200..400).contains(&status),
        "url": url.as_str(),
        "status": status,
        "content_type": content_type,
        "encoding": encoding,
        "decode_errors": had_decode_errors,
        "text": text,
        "bytes": bytes.len(),
        "truncated": byte_truncated
    })
}

pub(super) fn request_error_message(error: &reqwest::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        message.push_str(": ");
        message.push_str(&error.to_string());
        source = error.source();
    }
    message
}

pub(super) async fn read_limited_response_bytes(response: reqwest::Response) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = usize::try_from(MAX_BYTES)
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
