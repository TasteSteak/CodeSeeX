use url::Url;

pub fn normalize_base_url(value: &str) -> String {
    let raw = if value.trim().is_empty() {
        "https://api.deepseek.com/"
    } else {
        value.trim()
    };
    match Url::parse(raw) {
        Ok(mut url) => {
            if is_official_deepseek_url(&url) {
                url.set_path("/");
                url.set_query(None);
                url.set_fragment(None);
            }
            url.to_string().trim_end_matches('/').to_owned()
        }
        Err(_) => raw.trim_end_matches('/').to_owned(),
    }
}

pub fn chat_completions_url(base_url: &str, official_v1_compat: bool) -> String {
    let normalized = normalize_base_url(base_url);
    if let Ok(url) = Url::parse(&normalized) {
        if is_official_deepseek_url(&url) && official_v1_compat {
            return "https://api.deepseek.com/v1/chat/completions".to_owned();
        }
    }
    format!("{}/chat/completions", normalized.trim_end_matches('/'))
}

pub fn balance_url(base_url: &str) -> Result<String, url::ParseError> {
    let normalized = normalize_base_url(base_url);
    let mut url = Url::parse(&normalized)?;
    let mut path = url.path().trim_end_matches('/').to_owned();
    if path.to_ascii_lowercase().ends_with("/v1") {
        path.truncate(path.len().saturating_sub(3));
    }
    let balance_path = if path.is_empty() {
        "/user/balance".to_owned()
    } else {
        format!("{}/user/balance", path.trim_end_matches('/'))
    };
    url.set_path(&balance_path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub fn is_official_deepseek_url(url: &Url) -> bool {
    if url.scheme() != "https"
        || url
            .host_str()
            .map(|v| v.eq_ignore_ascii_case("api.deepseek.com"))
            != Some(true)
    {
        return false;
    }
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    path.is_empty() || path == "/" || path == "/v1"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_url_is_normalized_to_root() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn official_compat_uses_v1_chat_completions() {
        assert_eq!(
            chat_completions_url("https://api.deepseek.com/", true),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn custom_url_keeps_custom_prefix() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:9000/v1", true),
            "http://127.0.0.1:9000/v1/chat/completions"
        );
    }

    #[test]
    fn balance_url_uses_official_root_without_v1() {
        assert_eq!(
            balance_url("https://api.deepseek.com/v1/").unwrap(),
            "https://api.deepseek.com/user/balance"
        );
    }

    #[test]
    fn balance_url_uses_custom_base_and_strips_trailing_v1() {
        assert_eq!(
            balance_url("http://127.0.0.1:9000/v1").unwrap(),
            "http://127.0.0.1:9000/user/balance"
        );
        assert_eq!(
            balance_url("http://127.0.0.1:9000/openai/v1").unwrap(),
            "http://127.0.0.1:9000/openai/user/balance"
        );
    }
}
