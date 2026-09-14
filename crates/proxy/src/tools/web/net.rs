use codeseex_core::NetworkProxyMode;
use futures_util::StreamExt;
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr};

use super::MAX_BYTES;

pub(super) const WEB_REQUEST_TIMEOUT_SECS: u64 = 12;

pub(super) fn user_agent() -> &'static str {
    "Mozilla/5.0 (compatible; CodeSeeX/1.0; +https://localhost)"
}

pub(super) fn web_client(proxy_mode: NetworkProxyMode) -> reqwest::Client {
    crate::network::apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .timeout(std::time::Duration::from_secs(WEB_REQUEST_TIMEOUT_SECS))
        .build()
        .expect("build web client")
}

/// A no-redirect client that may only reach the addresses already validated for
/// `host`, so the address that was checked is the address that is used.
pub(super) fn pinned_no_redirect_client(
    proxy_mode: NetworkProxyMode,
    host: &str,
    addrs: &[IpAddr],
) -> reqwest::Client {
    let builder = crate::network::apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .http1_only()
        .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(WEB_REQUEST_TIMEOUT_SECS));
    let pinned = addrs
        .iter()
        .map(|ip| std::net::SocketAddr::new(*ip, 0))
        .collect::<Vec<_>>();
    let builder = if host.is_empty() || pinned.is_empty() {
        builder
    } else {
        builder.resolve_to_addrs(host, &pinned)
    };
    builder.build().expect("build pinned web client")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_request_timeout_stays_short_enough_for_agent_loops() {
        const { assert!(WEB_REQUEST_TIMEOUT_SECS <= 12) }
    }
}
