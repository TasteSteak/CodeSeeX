//! Safe page fetching: DNS-pinned requests with a manually validated redirect
//! chain, then decoding and extraction into an [`ExtractedDocument`].

use codeseex_core::NetworkProxyMode;
use serde_json::Value;

use super::extract::{
    bytes_have_binary_markers, decode_text_bytes, html_to_document, is_textual_content_type,
    markdown_to_document, plain_text_to_document, response_looks_like_html,
    response_looks_like_markdown, ExtractedDocument,
};
use super::net::{
    pinned_no_redirect_client, read_limited_response_bytes, request_error_message, user_agent,
};
use super::safety::{
    normalize_candidate_url, resolve_public_web_host, url_path_looks_blocked_resource,
    validate_public_web_url,
};

const MAX_REDIRECTS: usize = 5;
const ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,text/plain;q=0.8,*/*;q=0.5";

#[derive(Clone, Debug)]
pub(super) struct FetchedPage {
    pub(super) url: String,
    pub(super) requested_url: String,
    pub(super) status: Option<u16>,
    pub(super) content_type: String,
    pub(super) bytes: usize,
    pub(super) truncated: bool,
    pub(super) redirects: Vec<String>,
    pub(super) error: Option<String>,
    pub(super) message: String,
    pub(super) document: Option<ExtractedDocument>,
}

impl FetchedPage {
    pub(super) fn ok(&self) -> bool {
        self.error.is_none() && self.document.is_some()
    }

    fn failure(requested_url: &str, error: &str, message: impl Into<String>) -> Self {
        FetchedPage {
            url: requested_url.to_owned(),
            requested_url: requested_url.to_owned(),
            status: None,
            content_type: String::new(),
            bytes: 0,
            truncated: false,
            redirects: Vec::new(),
            error: Some(error.to_owned()),
            message: message.into(),
            document: None,
        }
    }

    pub(super) fn title(&self) -> Option<String> {
        self.document
            .as_ref()
            .and_then(|document| document.title.clone())
    }

    pub(super) fn diagnostic(&self) -> Value {
        serde_json::json!({
            "stage": "open",
            "code": self.error.clone().unwrap_or_else(|| "ok".to_owned()),
            "url": self.url,
            "requested_url": self.requested_url,
            "status": self.status,
            "content_type": self.content_type,
            "bytes": self.bytes,
            "truncated": self.truncated,
            "redirects": self.redirects,
            "error": self.error,
            "message": self.message
        })
    }
}

pub(super) async fn fetch_page(proxy_mode: NetworkProxyMode, raw_url: &str) -> FetchedPage {
    let normalized = normalize_candidate_url(raw_url).unwrap_or_else(|| raw_url.trim().to_owned());
    let Ok(mut current_url) = reqwest::Url::parse(&normalized) else {
        return FetchedPage::failure(raw_url, "invalid_url", "The URL could not be parsed.");
    };
    let requested_url = current_url.to_string();
    let mut redirects = Vec::new();

    for _ in 0..=MAX_REDIRECTS {
        if url_path_looks_blocked_resource(current_url.path()) {
            return FetchedPage::failure(
                &requested_url,
                "blocked_resource_type",
                "Binary, font, image, media, archive, and PDF resources are not opened by web search.",
            );
        }
        if let Err(message) = validate_public_web_url(&current_url) {
            return FetchedPage::failure(&requested_url, "blocked_url", message);
        }
        let pinned = match resolve_public_web_host(&current_url).await {
            Ok(addresses) => addresses,
            Err(message) => return FetchedPage::failure(&requested_url, "blocked_url", message),
        };
        let client = pinned_no_redirect_client(
            proxy_mode,
            current_url.host_str().unwrap_or_default(),
            &pinned,
        );
        let response = match client
            .get(current_url.clone())
            .header(reqwest::header::USER_AGENT, user_agent())
            .header(reqwest::header::ACCEPT, ACCEPT)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return FetchedPage::failure(
                    &requested_url,
                    "request_failed",
                    request_error_message(&error),
                )
            }
        };
        let status = response.status();
        if status.is_redirection() {
            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return FetchedPage::failure(
                    &requested_url,
                    "invalid_redirect",
                    "The server sent a redirect without a Location header.",
                );
            };
            let Ok(next) = current_url.join(location) else {
                return FetchedPage::failure(
                    &requested_url,
                    "invalid_redirect",
                    "The redirect target could not be resolved.",
                );
            };
            redirects.push(next.to_string());
            current_url = next;
            continue;
        }

        let status_code = status.as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let (bytes, truncated) = read_limited_response_bytes(response).await;
        if !status.is_success() {
            return FetchedPage {
                url: current_url.to_string(),
                requested_url,
                status: Some(status_code),
                content_type,
                bytes: bytes.len(),
                truncated,
                redirects,
                error: Some("http_status".to_owned()),
                message: format!("The page returned HTTP {status_code}."),
                document: None,
            };
        }
        if bytes_have_binary_markers(&bytes) || !is_textual_content_type(&content_type) {
            return FetchedPage {
                url: current_url.to_string(),
                requested_url,
                status: Some(status_code),
                content_type,
                bytes: bytes.len(),
                truncated,
                redirects,
                error: Some("unsupported_content_type".to_owned()),
                message: "The response is not a readable text document.".to_owned(),
                document: None,
            };
        }
        let (text, _encoding, _had_errors) = decode_text_bytes(&bytes, &content_type);
        let base_url = current_url.as_str();
        let document = if response_looks_like_html(&content_type, &text) {
            html_to_document(&text, Some(base_url))
        } else if response_looks_like_markdown(&content_type, current_url.as_str()) {
            markdown_to_document(&text, Some(base_url))
        } else {
            plain_text_to_document(&text)
        };
        if document.blocks.is_empty() {
            return FetchedPage {
                url: current_url.to_string(),
                requested_url,
                status: Some(status_code),
                content_type,
                bytes: bytes.len(),
                truncated,
                redirects,
                error: Some("empty_text_content".to_owned()),
                message: "The page yielded no readable text.".to_owned(),
                document: None,
            };
        }
        return FetchedPage {
            url: current_url.to_string(),
            requested_url,
            status: Some(status_code),
            content_type,
            bytes: bytes.len(),
            truncated,
            redirects,
            error: None,
            message: String::new(),
            document: Some(document),
        };
    }
    FetchedPage::failure(
        &requested_url,
        "too_many_redirects",
        "The page redirected more times than CodeSeeX follows.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_schemes_before_fetching() {
        let page = fetch_page(NetworkProxyMode::None, "ftp://example.com/file").await;

        assert!(!page.ok());
    }
}
