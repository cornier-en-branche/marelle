// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Providers that work without any setup beyond an API key.
//!
//! Presets are only defaults. Every field can be overridden, and a preset's
//! id can be reused for a different endpoint.

use marelle_config::{Config, Layer};

use crate::config::{AuthStyle, MaxTokensField, ProviderKind};

/// A well-known provider.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    /// The id used in `--provider` and in settings.
    pub id: &'static str,
    /// The protocol it speaks.
    pub kind: ProviderKind,
    /// The base URL.
    pub base_url: &'static str,
    /// The conventional environment variable for its key, when it needs one.
    pub api_key_env: Option<&'static str>,
    /// How the key is sent, when that differs from the protocol's default.
    pub auth: Option<AuthStyle>,
    /// The request field that limits the answer length.
    pub max_tokens_field: MaxTokensField,
    /// A short description for `marelle providers`.
    pub note: &'static str,
}

/// The built-in presets.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "openai",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.openai.com/v1",
        api_key_env: Some("OPENAI_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        note: "OpenAI",
    },
    Preset {
        id: "anthropic",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.anthropic.com",
        api_key_env: Some("ANTHROPIC_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Anthropic Claude",
    },
    Preset {
        id: "zai",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.z.ai/api/paas/v4",
        api_key_env: Some("ZAI_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Z.ai (Zhipu GLM), international",
    },
    Preset {
        id: "zai_coding",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://api.z.ai/api/coding/paas/v4",
        api_key_env: Some("ZAI_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Z.ai GLM Coding Plan",
    },
    Preset {
        id: "zai_anthropic",
        kind: ProviderKind::Anthropic,
        base_url: "https://api.z.ai/api/anthropic",
        api_key_env: Some("ZAI_API_KEY"),
        auth: Some(AuthStyle::Bearer),
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Z.ai, Anthropic-compatible endpoint",
    },
    Preset {
        id: "zhipu",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        api_key_env: Some("ZHIPUAI_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Zhipu BigModel, China",
    },
    Preset {
        id: "zhipu_coding",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        api_key_env: Some("ZHIPUAI_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Zhipu BigModel Coding Plan, China",
    },
    Preset {
        id: "openrouter",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "https://openrouter.ai/api/v1",
        api_key_env: Some("OPENROUTER_API_KEY"),
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "OpenRouter, many models behind one key",
    },
    Preset {
        id: "ollama",
        kind: ProviderKind::OpenAiCompatible,
        base_url: "http://localhost:11434/v1",
        api_key_env: None,
        auth: None,
        max_tokens_field: MaxTokensField::MaxTokens,
        note: "Ollama on this machine, no key",
    },
];

/// Registers every preset as defaults, the lowest layer, so that any other
/// layer can override a field.
pub fn register_presets(config: &mut Config) {
    for preset in PRESETS {
        let key = |field: &str| format!("providers.{}.{field}", preset.id);
        config.set(Layer::Default, key("kind"), preset.kind.id());
        config.set(Layer::Default, key("base_url"), preset.base_url);
        if let Some(env) = preset.api_key_env {
            config.set(Layer::Default, key("api_key_env"), env);
        }
        if let Some(auth) = preset.auth {
            config.set(Layer::Default, key("auth"), auth.id());
        }
        if preset.max_tokens_field != MaxTokensField::MaxTokens {
            config.set(
                Layer::Default,
                key("max_tokens_field"),
                preset.max_tokens_field.id(),
            );
        }
    }
}

/// The preset with this id, if any.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderConfig, provider_names};

    #[test]
    fn ids_are_unique_and_env_friendly() {
        let mut seen = std::collections::BTreeSet::new();
        for preset in PRESETS {
            assert!(seen.insert(preset.id), "duplicate id {}", preset.id);
            assert!(
                preset
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{} cannot be spelled in an environment variable",
                preset.id
            );
        }
    }

    #[test]
    fn every_preset_resolves() {
        let mut config = Config::new();
        register_presets(&mut config);
        assert_eq!(provider_names(&config).len(), PRESETS.len());
        for preset in PRESETS {
            let provider = ProviderConfig::from_config(&config, preset.id)
                .unwrap_or_else(|error| panic!("{}: {error}", preset.id));
            assert_eq!(provider.base_url, preset.base_url);
            assert_eq!(provider.kind, preset.kind);
        }
    }

    #[test]
    fn only_local_presets_use_plain_http() {
        for preset in PRESETS {
            if preset.base_url.starts_with("http://") {
                assert!(
                    preset.api_key_env.is_none(),
                    "{} sends a key in the clear",
                    preset.id
                );
            }
        }
    }

    #[test]
    fn lookup_by_id() {
        assert_eq!(
            preset("zai").map(|p| p.api_key_env),
            Some(Some("ZAI_API_KEY"))
        );
        assert!(preset("nope").is_none());
    }
}
