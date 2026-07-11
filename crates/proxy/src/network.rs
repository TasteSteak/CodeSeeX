use codeseex_core::NetworkProxyMode;
use std::net::{IpAddr, Ipv4Addr};

pub(crate) fn client(
    proxy_mode: NetworkProxyMode,
    timeout: std::time::Duration,
) -> reqwest::Result<reqwest::Client> {
    apply_proxy_mode(reqwest::Client::builder(), proxy_mode)
        .timeout(timeout)
        .build()
}

pub(crate) fn apply_proxy_mode(
    builder: reqwest::ClientBuilder,
    proxy_mode: NetworkProxyMode,
) -> reqwest::ClientBuilder {
    match proxy_mode {
        NetworkProxyMode::System => {
            if let Some(proxy_url) = system_proxy_url() {
                if reqwest::Proxy::all(&proxy_url).is_ok() {
                    let proxy_value = proxy_url.clone();
                    return builder.proxy(reqwest::Proxy::custom(move |url| {
                        if should_bypass_proxy(url) {
                            None
                        } else {
                            Some(proxy_value.clone())
                        }
                    }));
                }
            }
            builder
        }
        NetworkProxyMode::None => builder.no_proxy(),
    }
}

pub(crate) fn proxy_cache_key(proxy_mode: NetworkProxyMode) -> String {
    match proxy_mode {
        NetworkProxyMode::System => {
            format!(
                "system:{}",
                system_proxy_url()
                    .as_deref()
                    .map(redacted_proxy_url)
                    .unwrap_or_else(|| "direct".to_owned())
            )
        }
        NetworkProxyMode::None => "none".to_owned(),
    }
}

fn redacted_proxy_url(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return value.split('@').next_back().unwrap_or(value).to_owned();
    };
    if !url.username().is_empty() {
        let _ = url.set_username("redacted");
    }
    if url.password().is_some() {
        let _ = url.set_password(Some("redacted"));
    }
    url.to_string()
}

fn system_proxy_url() -> Option<String> {
    env_proxy_url().or_else(windows_internet_settings_proxy_url)
}

fn env_proxy_url() -> Option<String> {
    ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find_map(|value| normalize_proxy_server(&value))
}

#[cfg(windows)]
fn windows_internet_settings_proxy_url() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled = settings
        .get_value::<u32, _>("ProxyEnable")
        .ok()
        .or_else(|| {
            settings
                .get_value::<u64, _>("ProxyEnable")
                .ok()
                .and_then(|value| u32::try_from(value).ok())
        })?;
    if enabled == 0 {
        return None;
    }
    let server = settings.get_value::<String, _>("ProxyServer").ok()?;
    normalize_proxy_server(&server)
}

#[cfg(not(windows))]
fn windows_internet_settings_proxy_url() -> Option<String> {
    None
}

fn normalize_proxy_server(value: &str) -> Option<String> {
    let selected = select_proxy_server(value)?;
    let selected = selected.trim();
    if selected.is_empty() {
        return None;
    }
    if selected.contains("://") {
        return Some(selected.to_owned());
    }
    Some(format!("http://{selected}"))
}

fn select_proxy_server(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('=') {
        return Some(trimmed);
    }
    let entries = trimmed
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim()))
        .collect::<Vec<_>>();
    for wanted in ["https", "http"] {
        if let Some((_, value)) = entries.iter().find(|(key, _)| key == wanted) {
            return Some(*value);
        }
    }
    entries.first().map(|(_, value)| *value)
}

fn should_bypass_proxy(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if no_proxy_matches(&host) {
        return true;
    }
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip == Ipv4Addr::UNSPECIFIED
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn no_proxy_matches(host: &str) -> bool {
    let Ok(value) = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")) else {
        return false;
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| no_proxy_entry_matches(host, entry))
}

fn no_proxy_entry_matches(host: &str, entry: &str) -> bool {
    if entry == "*" {
        return true;
    }
    let entry = entry.trim_start_matches('.').to_ascii_lowercase();
    host == entry || host.ends_with(&format!(".{entry}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_plain_system_proxy_server() {
        assert_eq!(
            normalize_proxy_server("127.0.0.1:7890").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn normalizes_protocol_mapped_system_proxy_server() {
        assert_eq!(
            normalize_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7891").as_deref(),
            Some("http://127.0.0.1:7891")
        );
    }

    #[test]
    fn bypasses_local_and_private_hosts() {
        for raw in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:11434/v1",
            "http://192.168.1.10:8000/v1",
            "http://[::1]:11434/v1",
        ] {
            let url = reqwest::Url::parse(raw).expect("url");
            assert!(should_bypass_proxy(&url), "{raw}");
        }
    }

    #[test]
    fn external_hosts_do_not_bypass_proxy() {
        let url = reqwest::Url::parse("https://api.openai.com/v1/responses").expect("url");
        assert!(!should_bypass_proxy(&url));
    }

    #[test]
    fn proxy_cache_key_redacts_proxy_credentials() {
        assert_eq!(
            redacted_proxy_url("http://user:secret@127.0.0.1:7890/"),
            "http://redacted:redacted@127.0.0.1:7890/"
        );
    }
}
