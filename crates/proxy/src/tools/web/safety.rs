use base64::{engine::general_purpose, Engine as _};
use std::net::{IpAddr, Ipv6Addr};

pub(super) fn normalize_candidate_url(value: &str) -> Option<String> {
    let raw = value
        .trim()
        .trim_matches(['"', '\'', ' ', '\n', '\r', '\t']);
    if raw.is_empty() {
        return None;
    }
    let raw = raw.trim_end_matches([',', '.', ';', ')']);
    let parsed = if raw.starts_with("http://") || raw.starts_with("https://") {
        reqwest::Url::parse(raw).ok()?
    } else if raw
        .split('/')
        .next()
        .map(|host| host.contains('.'))
        .unwrap_or(false)
    {
        reqwest::Url::parse(&format!("https://{raw}")).ok()?
    } else {
        return None;
    };
    let parsed = decode_bing_redirect_url(&parsed).unwrap_or(parsed);
    match parsed.scheme() {
        "http" | "https" => Some(parsed.to_string()),
        _ => None,
    }
}

fn decode_bing_redirect_url(url: &reqwest::Url) -> Option<reqwest::Url> {
    let host = url.host_str()?.to_ascii_lowercase();
    if !host.ends_with("bing.com") || !url.path().starts_with("/ck/") {
        return None;
    }
    let encoded = url.query_pairs().find_map(|(key, value)| {
        (key == "u").then(|| value.trim().trim_start_matches("a1").to_owned())
    })?;
    let bytes = general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let parsed = reqwest::Url::parse(&decoded).ok()?;
    matches!(parsed.scheme(), "http" | "https").then_some(parsed)
}

pub(super) fn url_path_looks_blocked_resource(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".avif", ".svg", ".ico", ".bmp", ".mp4", ".webm",
        ".mp3", ".wav", ".flac", ".woff", ".woff2", ".ttf", ".otf", ".eot", ".pdf", ".zip", ".7z",
        ".rar", ".tar", ".gz", ".exe", ".dll", ".dmg", ".iso",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

pub(super) fn validate_public_web_url(url: &reqwest::Url) -> Result<(), String> {
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

/// Resolves one URL's host and returns only the addresses that pass the public
/// target check.
///
/// Callers must pin these addresses on the client they actually connect with:
/// validating a name and then letting the transport resolve it again leaves a
/// window where the second lookup returns a private address.
pub(super) async fn resolve_public_web_host(url: &reqwest::Url) -> Result<Vec<IpAddr>, String> {
    validate_public_web_url(url)?;
    if allow_private_web_targets() {
        return Ok(Vec::new());
    }
    let Some(host) = url.host_str() else {
        return Err("URL must include a host.".to_owned());
    };
    let host = host.trim_matches(['[', ']']).to_owned();
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![ip]);
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
    let mut resolved = Vec::new();
    for address in addresses {
        if !resolved.contains(&address.ip()) {
            resolved.push(address.ip());
        }
    }
    Ok(resolved)
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

fn ip_is_blocked(ip: IpAddr) -> bool {
    let ip = normalize_mapped_ip(ip);
    ip.is_loopback()
        || ip.is_unspecified()
        || is_private_ip(ip)
        || is_link_local_ip(ip)
        || is_documentation_ip(ip)
        || is_reserved_special_ip(ip)
}

/// Ranges that are not RFC1918 private space but are never a public web target:
/// carrier-grade NAT, IETF protocol assignments, benchmarking space, reserved
/// space, and the IPv6 transition prefixes that tunnel to an embedded IPv4
/// address.
fn is_reserved_special_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 198 && matches!(octets[1], 18 | 19))
                || octets[0] >= 240
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            segments[0] == 0x2002 || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_targets_by_default() {
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
    fn blocks_transition_and_reserved_ranges() {
        std::env::remove_var("CODESEEX_WEB_SEARCH_ALLOW_PRIVATE");
        for host in [
            "100.64.1.5",        // carrier-grade NAT
            "0.10.1.5",          // "this network"
            "192.0.0.10",        // IETF protocol assignments
            "198.19.20.30",      // benchmarking
            "240.1.2.3",         // reserved
            "169.254.169.254",   // link-local metadata service
            "[2002:7f00:1::1]",  // 6to4 wrapping a loopback address
            "[64:ff9b::7f00:1]", // NAT64 wrapping a loopback address
        ] {
            let url = reqwest::Url::parse(&format!("http://{host}/")).expect("parse url");
            assert!(
                validate_public_web_url(&url).is_err(),
                "{host} must not be treated as a public web target"
            );
        }
        let public = reqwest::Url::parse("https://1.1.1.1/").expect("parse url");
        assert!(validate_public_web_url(&public).is_ok());
    }

    #[test]
    fn blocks_binary_resource_paths() {
        assert!(url_path_looks_blocked_resource("/file.pdf"));
        assert!(url_path_looks_blocked_resource("/font.woff2"));
        assert!(url_path_looks_blocked_resource("/image.png"));
        assert!(!url_path_looks_blocked_resource("/article"));
    }

    #[test]
    fn normalizes_bing_redirect_urls() {
        let normalized = normalize_candidate_url(
            "https://www.bing.com/ck/a?!&&u=a1aHR0cHM6Ly9wZXBzLnB5dGhvbi5vcmcvcGVwLTA3NDUv&ntb=1",
        );

        assert_eq!(
            normalized.as_deref(),
            Some("https://peps.python.org/pep-0745/")
        );
    }
}
