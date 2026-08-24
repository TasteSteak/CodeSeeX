use codeseex_core::config::UpstreamConfig;
use codeseex_core::models::{MODEL_FLASH, MODEL_PRO};
use codeseex_core::urls::normalize_base_url;

pub(crate) mod tool_protocol;

pub(crate) fn should_adapt_tool_protocol(upstream: &UpstreamConfig, model: &str) -> bool {
    if model_looks_like_deepseek(model) {
        return true;
    }
    normalize_base_url(&upstream.base_url).eq_ignore_ascii_case("https://api.deepseek.com")
}

fn model_looks_like_deepseek(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    normalized == MODEL_FLASH || normalized == MODEL_PRO || normalized.starts_with("deepseek")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(base_url: &str) -> UpstreamConfig {
        UpstreamConfig {
            base_url: base_url.to_owned(),
            official_v1_compat: true,
            transport: Default::default(),
            api_key: None,
            timeout_ms: 120_000,
        }
    }

    #[test]
    fn adapts_official_deepseek_upstream_even_for_native_codex_model_alias() {
        assert!(should_adapt_tool_protocol(
            &upstream("https://api.deepseek.com"),
            "gpt-5.4"
        ));
    }

    #[test]
    fn adapts_deepseek_named_model_on_custom_compatible_upstream() {
        assert!(should_adapt_tool_protocol(
            &upstream("http://127.0.0.1:9000/v1"),
            "deepseek-chat"
        ));
    }

    #[test]
    fn does_not_adapt_unrelated_openai_compatible_upstream() {
        assert!(!should_adapt_tool_protocol(
            &upstream("http://127.0.0.1:9000/v1"),
            "gpt-4o"
        ));
    }
}
