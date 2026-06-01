use serde::{Deserialize, Serialize};

pub const MODEL_FLASH: &str = "deepseek-v4-flash";
pub const MODEL_PRO: &str = "deepseek-v4-pro";
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
pub const DEFAULT_EFFECTIVE_CONTEXT_PERCENT: u8 = 90;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub context_window: u64,
    pub effective_context_window_percent: u8,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamModelOverride {
    #[default]
    Default,
    Flash,
    Pro,
}

impl UpstreamModelOverride {
    pub fn upstream_slug(self, requested: &str) -> String {
        match self {
            Self::Default => default_upstream_slug(requested),
            Self::Flash => MODEL_FLASH.to_owned(),
            Self::Pro => MODEL_PRO.to_owned(),
        }
    }
}

fn default_upstream_slug(requested: &str) -> String {
    let requested = requested.trim();
    match requested.to_ascii_lowercase().as_str() {
        MODEL_FLASH => MODEL_FLASH.to_owned(),
        MODEL_PRO => MODEL_PRO.to_owned(),
        "gpt-5.4-mini" | "gpt-5.5-mini" => MODEL_FLASH.to_owned(),
        "gpt-5.4" | "gpt-5.5" => MODEL_PRO.to_owned(),
        _ => MODEL_PRO.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TemperaturePreset {
    #[default]
    Default,
    Strict,
    Balanced,
    General,
    Creative,
}

impl TemperaturePreset {
    pub fn value(self) -> Option<f32> {
        match self {
            Self::Default => None,
            Self::Strict => Some(0.0),
            Self::Balanced => Some(1.0),
            Self::General => Some(1.3),
            Self::Creative => Some(1.5),
        }
    }
}

pub fn available_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            slug: MODEL_FLASH.to_owned(),
            display_name: "DeepSeek-V4 Flash".to_owned(),
            description: "DeepSeek-V4 Flash served through CodeSeeX Next.".to_owned(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_PERCENT,
        },
        ModelInfo {
            slug: MODEL_PRO.to_owned(),
            display_name: "DeepSeek-V4 Pro".to_owned(),
            description: "DeepSeek-V4 Pro served through CodeSeeX Next.".to_owned(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            effective_context_window_percent: DEFAULT_EFFECTIVE_CONTEXT_PERCENT,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_maps_requested_model_only_when_enabled() {
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug(MODEL_FLASH),
            MODEL_FLASH
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug(MODEL_PRO),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.4-mini"),
            MODEL_FLASH
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.5-mini"),
            MODEL_FLASH
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.4"),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.5"),
            MODEL_PRO
        );
        assert_eq!(UpstreamModelOverride::Flash.upstream_slug("x"), MODEL_FLASH);
        assert_eq!(UpstreamModelOverride::Pro.upstream_slug("x"), MODEL_PRO);
    }
}
