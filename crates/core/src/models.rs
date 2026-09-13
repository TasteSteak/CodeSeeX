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

/// Which upstream model the proxy should send, whatever the client asked for.
///
/// `Default` follows the client through the catalog. `Flash` and `Pro` are the
/// legacy presets kept for existing `config.toml` files and the tray menu;
/// `Custom` pins any slug the active catalog knows, so the model list can offer
/// a lock for every model it shows instead of only the built-in two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpstreamModelOverride {
    #[default]
    Default,
    Flash,
    Pro,
    Custom(String),
}

impl UpstreamModelOverride {
    /// The slug the user pinned; `None` means "follow the client".
    pub fn pinned_slug(&self) -> Option<&str> {
        match self {
            Self::Default => None,
            Self::Flash => Some(MODEL_FLASH),
            Self::Pro => Some(MODEL_PRO),
            Self::Custom(slug) => Some(slug.as_str()),
        }
    }

    /// Upstream name to use when the catalog cannot resolve the pin.
    pub fn upstream_slug(&self, requested: &str) -> String {
        match self.pinned_slug() {
            Some(slug) => slug.to_owned(),
            None => default_upstream_slug(requested),
        }
    }

    /// Labels the settings UI, the environment and `config.toml` accept.
    pub fn from_label(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "default" => Self::Default,
            "flash" | MODEL_FLASH => Self::Flash,
            "pro" | MODEL_PRO => Self::Pro,
            _ => Self::Custom(value.trim().to_owned()),
        }
    }

    fn label(&self) -> &str {
        match self {
            Self::Default => "default",
            Self::Flash => "flash",
            Self::Pro => "pro",
            Self::Custom(slug) => slug.as_str(),
        }
    }
}

impl Serialize for UpstreamModelOverride {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for UpstreamModelOverride {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_label(&String::deserialize(deserializer)?))
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

/// Whether the proxy forces the upstream `thinking` flag on or off.
///
/// `Auto` keeps the historical behaviour: a Codex service request (thread
/// title, ambient suggestions, ...) disables thinking and every other request
/// follows the client's `reasoning.effort`. The other two variants override
/// that decision. The value lives on `AppConfig` so the hot upstream path never
/// has to re-read `config.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelThinking {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

impl ModelThinking {
    /// Canonical label shared by `config.toml`, the settings payload and the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

pub fn parse_model_thinking(value: &str) -> Option<ModelThinking> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" | "default" => Some(ModelThinking::Auto),
        "enabled" | "on" | "enable" => Some(ModelThinking::Enabled),
        "disabled" | "off" | "disable" => Some(ModelThinking::Disabled),
        _ => None,
    }
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

    /// Any catalog slug can be pinned, not just the two built-in presets.
    #[test]
    fn a_pinned_slug_survives_its_label_and_serde() {
        let custom = UpstreamModelOverride::from_label("test-placeholder-1");
        assert_eq!(
            custom,
            UpstreamModelOverride::Custom("test-placeholder-1".to_owned())
        );
        assert_eq!(custom.pinned_slug(), Some("test-placeholder-1"));
        assert_eq!(custom.upstream_slug(MODEL_PRO), "test-placeholder-1");

        // The labels the manager already writes keep their legacy meaning.
        assert_eq!(
            UpstreamModelOverride::from_label(""),
            UpstreamModelOverride::Default
        );
        assert_eq!(
            UpstreamModelOverride::from_label("default"),
            UpstreamModelOverride::Default
        );
        assert_eq!(
            UpstreamModelOverride::from_label("flash"),
            UpstreamModelOverride::Flash
        );
        assert_eq!(
            UpstreamModelOverride::from_label(MODEL_PRO),
            UpstreamModelOverride::Pro
        );
        assert_eq!(UpstreamModelOverride::Default.pinned_slug(), None);

        for value in [
            UpstreamModelOverride::Default,
            UpstreamModelOverride::Flash,
            UpstreamModelOverride::Pro,
            UpstreamModelOverride::Custom("test-placeholder-1".to_owned()),
        ] {
            let text = serde_json::to_string(&value).expect("serialize");
            let parsed: UpstreamModelOverride = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(parsed, value, "{text}");
        }
        assert_eq!(
            serde_json::to_string(&UpstreamModelOverride::Flash).expect("serialize"),
            "\"flash\""
        );
    }
}
