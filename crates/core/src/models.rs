use serde::{Deserialize, Serialize};

pub const MODEL_FLASH: &str = "deepseek-v4-flash";
pub const MODEL_PRO: &str = "deepseek-v4-pro";
pub const DEFAULT_CONTEXT_WINDOW: u64 = 1_000_000;
pub const DEFAULT_EFFECTIVE_CONTEXT_PERCENT: u8 = 95;

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
    let normalized = requested.to_ascii_lowercase();
    match normalized.as_str() {
        "" => MODEL_PRO.to_owned(),
        MODEL_FLASH => MODEL_FLASH.to_owned(),
        MODEL_PRO => MODEL_PRO.to_owned(),
        value if is_codex_native_gpt_model(value) => MODEL_PRO.to_owned(),
        _ => requested.to_owned(),
    }
}

fn is_codex_native_gpt_model(value: &str) -> bool {
    value == "gpt-5" || value.starts_with("gpt-5.") || value.starts_with("gpt-5-")
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

/// Models advertised by the currently embedded catalog document.
pub fn available_models() -> Vec<ModelInfo> {
    available_models_from_document(&crate::catalog::embedded_catalog_document())
}

pub fn available_models_from_document(
    document: &crate::catalog::CatalogDocument,
) -> Vec<ModelInfo> {
    document
        .models
        .iter()
        .map(|model| ModelInfo {
            slug: model.slug.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            context_window: model.context_window,
            effective_context_window_percent: model.effective_context_window_percent,
        })
        .collect()
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
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.6-mini"),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("unknown-model"),
            "unknown-model"
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.4"),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.5"),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-5.3-codex"),
            MODEL_PRO
        );
        assert_eq!(
            UpstreamModelOverride::Default.upstream_slug("gpt-4o"),
            "gpt-4o"
        );
        assert_eq!(UpstreamModelOverride::Default.upstream_slug(""), MODEL_PRO);
        assert_eq!(UpstreamModelOverride::Flash.upstream_slug("x"), MODEL_FLASH);
        assert_eq!(UpstreamModelOverride::Pro.upstream_slug("x"), MODEL_PRO);
    }
}
