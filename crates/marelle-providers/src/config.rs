// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Provider settings: which API a provider speaks, where it lives and where
//! its key comes from.
//!
//! A provider is described by settings named `providers.<id>.<field>`. The
//! built-in presets register defaults for well-known providers; anything can
//! be overridden or added through environment variables, flags or, later,
//! configuration files. A key is never stored in a setting. A setting names
//! the environment variable (or the file) that holds it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use marelle_config::{Config, Expected, SettingError, looks_like_env_name};
use serde_json::{Map, Value};

use crate::error::ProviderError;
use crate::secret::Secret;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// The chat-completions API that OpenAI introduced and many others copy:
    /// OpenAI, OpenRouter, Ollama, Z.ai and more.
    OpenAiCompatible,
    /// The Anthropic Messages API.
    Anthropic,
}

const KIND_IDS: [&str; 2] = ["openai-compatible", "anthropic"];
const AUTH_IDS: [&str; 4] = ["bearer", "x-api-key", "api-key", "none"];
const MAX_TOKENS_IDS: [&str; 2] = ["max_tokens", "max_completion_tokens"];

impl ProviderKind {
    /// Every kind.
    pub const ALL: [ProviderKind; 2] = [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic];

    /// The name used in settings and in output.
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => KIND_IDS[0],
            ProviderKind::Anthropic => KIND_IDS[1],
        }
    }

    /// Reads a kind from a setting. `openai` is accepted for
    /// `openai-compatible`.
    pub fn parse(text: &str) -> Option<ProviderKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "openai-compatible" | "openai" => Some(ProviderKind::OpenAiCompatible),
            "anthropic" => Some(ProviderKind::Anthropic),
            _ => None,
        }
    }
}

/// How the API key is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`, as Anthropic does.
    XApiKey,
    /// `api-key: <key>`, as Azure OpenAI does.
    ApiKey,
    /// No credentials are sent.
    None,
}

impl AuthStyle {
    /// The name used in settings and in output.
    pub fn id(self) -> &'static str {
        match self {
            AuthStyle::Bearer => AUTH_IDS[0],
            AuthStyle::XApiKey => AUTH_IDS[1],
            AuthStyle::ApiKey => AUTH_IDS[2],
            AuthStyle::None => AUTH_IDS[3],
        }
    }

    /// Reads a style from a setting.
    pub fn parse(text: &str) -> Option<AuthStyle> {
        match text.trim().to_ascii_lowercase().as_str() {
            "bearer" => Some(AuthStyle::Bearer),
            "x-api-key" => Some(AuthStyle::XApiKey),
            "api-key" => Some(AuthStyle::ApiKey),
            "none" => Some(AuthStyle::None),
            _ => None,
        }
    }

    fn default_for(kind: ProviderKind) -> AuthStyle {
        match kind {
            ProviderKind::OpenAiCompatible => AuthStyle::Bearer,
            ProviderKind::Anthropic => AuthStyle::XApiKey,
        }
    }
}

/// Which request field limits the answer length.
///
/// OpenAI's newer models insist on `max_completion_tokens`. Most compatible
/// providers still expect `max_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensField {
    /// `max_tokens`.
    MaxTokens,
    /// `max_completion_tokens`.
    MaxCompletionTokens,
}

impl MaxTokensField {
    /// The request field name, also used in settings.
    pub fn id(self) -> &'static str {
        match self {
            MaxTokensField::MaxTokens => MAX_TOKENS_IDS[0],
            MaxTokensField::MaxCompletionTokens => MAX_TOKENS_IDS[1],
        }
    }

    /// Reads a field name from a setting.
    pub fn parse(text: &str) -> Option<MaxTokensField> {
        match text.trim().to_ascii_lowercase().as_str() {
            "max_tokens" => Some(MaxTokensField::MaxTokens),
            "max_completion_tokens" => Some(MaxTokensField::MaxCompletionTokens),
            _ => None,
        }
    }
}

/// Everything needed to reach one provider, except the key itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    /// The provider's id, such as `zai`.
    pub name: String,
    /// The protocol it speaks.
    pub kind: ProviderKind,
    /// The base URL, without a trailing slash.
    pub base_url: String,
    /// The environment variable that holds the API key.
    pub api_key_env: Option<String>,
    /// A file that holds the API key, used when the variable is unset.
    pub api_key_file: Option<String>,
    /// How the key is sent.
    pub auth: AuthStyle,
    /// The request field that limits the answer length.
    pub max_tokens_field: MaxTokensField,
    /// Whether to ask for token counts in streamed answers.
    pub stream_usage: bool,
    /// Extra fields merged into every request body.
    pub extra_body: Map<String, Value>,
}

/// Lowercases a provider name and turns dashes into underscores, so that
/// `zai-coding` and `ZAI_CODING` name the same provider. Environment
/// variables cannot contain dashes, which is why ids use underscores.
pub fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

/// The ids of all providers the settings describe, sorted.
///
/// A provider exists when it has a `kind` or a `base_url`.
pub fn provider_names(config: &Config) -> Vec<String> {
    let mut names = BTreeSet::new();
    for key in config.keys() {
        let Some(rest) = key.strip_prefix("providers.") else {
            continue;
        };
        let Some((id, field)) = rest.split_once('.') else {
            continue;
        };
        if field == "kind" || field == "base_url" {
            names.insert(id.to_owned());
        }
    }
    names.into_iter().collect()
}

/// Provider fields that would hold a secret. They are refused, not ignored.
const SECRET_FIELDS: [&str; 5] = ["api_key", "apikey", "key", "token", "secret"];

pub(crate) fn text_setting(config: &Config, key: &str) -> Option<String> {
    config
        .get(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Checks a base URL and returns it without trailing slashes.
fn clean_base_url(text: &str) -> Option<String> {
    let url = reqwest::Url::parse(text.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(text.trim().trim_end_matches('/').to_owned())
}

impl ProviderConfig {
    /// Builds the configuration of the provider called `name` from settings.
    pub fn from_config(config: &Config, name: &str) -> Result<ProviderConfig, ProviderError> {
        let id = normalize_name(name);
        if !provider_names(config).contains(&id) {
            return Err(ProviderError::UnknownProvider {
                name: name.to_owned(),
            });
        }
        let key = |field: &str| format!("providers.{id}.{field}");

        // Settings only say where a key lives. A key typed into a setting would
        // end up in shell history, in dotfiles and in commits.
        for field in SECRET_FIELDS {
            if text_setting(config, &key(field)).is_some() {
                return Err(ProviderError::SecretInSettings { key: key(field) });
            }
        }

        let kind = match text_setting(config, &key("kind")) {
            None => ProviderKind::OpenAiCompatible,
            Some(text) => ProviderKind::parse(&text)
                .ok_or_else(|| SettingError::new(key("kind"), Expected::OneOf(&KIND_IDS)))?,
        };

        let base_url_text = text_setting(config, &key("base_url")).ok_or_else(|| {
            ProviderError::MissingSetting {
                key: key("base_url"),
            }
        })?;
        let base_url = clean_base_url(&base_url_text)
            .ok_or_else(|| SettingError::new(key("base_url"), Expected::Url))?;

        let api_key_env = text_setting(config, &key("api_key_env"));
        if let Some(env) = &api_key_env {
            if !looks_like_env_name(env) {
                return Err(SettingError::new(key("api_key_env"), Expected::EnvName).into());
            }
        }
        let api_key_file = text_setting(config, &key("api_key_file"));

        let auth = match text_setting(config, &key("auth")) {
            None => AuthStyle::default_for(kind),
            Some(text) => AuthStyle::parse(&text)
                .ok_or_else(|| SettingError::new(key("auth"), Expected::OneOf(&AUTH_IDS)))?,
        };

        let max_tokens_field = match text_setting(config, &key("max_tokens_field")) {
            None => MaxTokensField::MaxTokens,
            Some(text) => MaxTokensField::parse(&text).ok_or_else(|| {
                SettingError::new(key("max_tokens_field"), Expected::OneOf(&MAX_TOKENS_IDS))
            })?,
        };

        let stream_usage = config.get_bool(&key("stream_usage"))?.unwrap_or(true);

        let extra_body = match text_setting(config, &key("extra_body")) {
            None => Map::new(),
            Some(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(map)) => map,
                _ => return Err(SettingError::new(key("extra_body"), Expected::JsonObject).into()),
            },
        };

        Ok(ProviderConfig {
            name: id,
            kind,
            base_url,
            api_key_env,
            api_key_file,
            auth,
            max_tokens_field,
            stream_usage,
            extra_body,
        })
    }

    /// Finds the API key with `lookup` for environment variables.
    ///
    /// The variable named by `api_key_env` wins. When it is unset or blank the
    /// file named by `api_key_file` is read. `Ok(None)` means the provider
    /// needs no key, as with a local Ollama.
    pub fn api_key_with(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Secret>, ProviderError> {
        if let Some(env) = &self.api_key_env {
            let value = lookup(env).unwrap_or_default();
            let value = value.trim();
            if !value.is_empty() {
                return Ok(Some(Secret::new(value)));
            }
        }
        if let Some(file) = &self.api_key_file {
            let path = expand_home(file, &lookup);
            let shown = path.display().to_string();
            let text =
                std::fs::read_to_string(&path).map_err(|error| ProviderError::ApiKeyFile {
                    path: shown.clone(),
                    reason: error.to_string(),
                })?;
            let key = text.trim();
            if key.is_empty() {
                return Err(ProviderError::ApiKeyFile {
                    path: shown,
                    reason: "the file is empty".to_owned(),
                });
            }
            return Ok(Some(Secret::new(key)));
        }
        match &self.api_key_env {
            Some(env) => Err(ProviderError::MissingApiKey { env: env.clone() }),
            None => Ok(None),
        }
    }

    /// Finds the API key in the process environment.
    pub fn api_key(&self) -> Result<Option<Secret>, ProviderError> {
        self.api_key_with(|name| std::env::var(name).ok())
    }

    /// Where `api_key_file` points, with `~/` expanded through `lookup`.
    pub fn api_key_file_path(&self, lookup: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
        self.api_key_file
            .as_deref()
            .map(|file| expand_home(file, &lookup))
    }
}

pub(crate) fn expand_home(path: &str, lookup: &impl Fn(&str) -> Option<String>) -> PathBuf {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = lookup("HOME").filter(|home| !home.is_empty()) {
            return PathBuf::from(home).join(path.trim_start_matches('~').trim_start_matches('/'));
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::register_presets;
    use marelle_config::Layer;

    fn with_presets() -> Config {
        let mut config = Config::new();
        register_presets(&mut config);
        config
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn a_preset_resolves_to_a_full_configuration() {
        let provider = ProviderConfig::from_config(&with_presets(), "zai").unwrap();
        assert_eq!(provider.name, "zai");
        assert_eq!(provider.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(provider.base_url, "https://api.z.ai/api/paas/v4");
        assert_eq!(provider.api_key_env.as_deref(), Some("ZAI_API_KEY"));
        assert_eq!(provider.auth, AuthStyle::Bearer);
        assert_eq!(provider.max_tokens_field, MaxTokensField::MaxTokens);
        assert!(provider.stream_usage);
        assert!(provider.extra_body.is_empty());
    }

    #[test]
    fn presets_differ_where_the_providers_differ() {
        let config = with_presets();
        let openai = ProviderConfig::from_config(&config, "openai").unwrap();
        assert_eq!(openai.max_tokens_field, MaxTokensField::MaxCompletionTokens);
        let anthropic = ProviderConfig::from_config(&config, "anthropic").unwrap();
        assert_eq!(anthropic.kind, ProviderKind::Anthropic);
        assert_eq!(anthropic.auth, AuthStyle::XApiKey);
        let ollama = ProviderConfig::from_config(&config, "ollama").unwrap();
        assert_eq!(ollama.api_key_env, None);
        let zai_anthropic = ProviderConfig::from_config(&config, "zai_anthropic").unwrap();
        assert_eq!(zai_anthropic.kind, ProviderKind::Anthropic);
        assert_eq!(zai_anthropic.auth, AuthStyle::Bearer);
    }

    #[test]
    fn a_key_typed_into_a_setting_is_refused_and_never_echoed() {
        for field in ["api_key", "apikey", "key", "token", "secret"] {
            let mut config = with_presets();
            let setting = format!("providers.zai.{field}");
            config.set(
                Layer::Env,
                setting.clone(),
                "sk-should-never-be-shown-0123456789",
            );
            let error = ProviderConfig::from_config(&config, "zai").unwrap_err();
            assert_eq!(error, ProviderError::SecretInSettings { key: setting });
            assert!(!error.to_string().contains("sk-should"), "{field}");
        }
        // A blank value is not a secret.
        let mut config = with_presets();
        config.set(Layer::Env, "providers.zai.api_key", "  ");
        assert!(ProviderConfig::from_config(&config, "zai").is_ok());
    }

    #[test]
    fn the_key_file_path_expands_the_home_directory() {
        let mut config = with_presets();
        config.set(Layer::Env, "providers.zai.api_key_file", "~/keys/zai");
        let provider = ProviderConfig::from_config(&config, "zai").unwrap();
        let home = |name: &str| (name == "HOME").then(|| "/home/someone".to_owned());
        assert_eq!(
            provider.api_key_file_path(home),
            Some(PathBuf::from("/home/someone/keys/zai"))
        );
        let plain = ProviderConfig::from_config(&with_presets(), "zai").unwrap();
        assert_eq!(plain.api_key_file_path(|_| None), None);
    }

    #[test]
    fn names_are_normalised() {
        let config = with_presets();
        let provider = ProviderConfig::from_config(&config, " ZAI-Coding ").unwrap();
        assert_eq!(provider.name, "zai_coding");
        assert_eq!(normalize_name("A-b_C"), "a_b_c");
    }

    #[test]
    fn unknown_providers_are_reported_by_the_name_given() {
        let error = ProviderConfig::from_config(&with_presets(), "Nope").unwrap_err();
        assert_eq!(
            error,
            ProviderError::UnknownProvider {
                name: "Nope".to_owned()
            }
        );
    }

    #[test]
    fn a_custom_provider_needs_only_a_base_url() {
        let mut config = with_presets();
        config.set(
            Layer::Env,
            "providers.mine.base_url",
            "http://127.0.0.1:8080/v1/",
        );
        let provider = ProviderConfig::from_config(&config, "mine").unwrap();
        assert_eq!(provider.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(provider.base_url, "http://127.0.0.1:8080/v1");
        assert_eq!(provider.api_key_env, None);
        assert!(provider_names(&config).contains(&"mine".to_owned()));
    }

    #[test]
    fn settings_override_presets() {
        let mut config = with_presets();
        config.set(
            Layer::Env,
            "providers.zai.base_url",
            "https://open.bigmodel.cn/api/paas/v4",
        );
        config.set(Layer::Flag, "providers.zai.stream_usage", "false");
        config.set(
            Layer::Env,
            "providers.zai.extra_body",
            r#"{"thinking":{"type":"disabled"}}"#,
        );
        let provider = ProviderConfig::from_config(&config, "zai").unwrap();
        assert_eq!(provider.base_url, "https://open.bigmodel.cn/api/paas/v4");
        assert!(!provider.stream_usage);
        assert_eq!(provider.extra_body["thinking"]["type"], "disabled");
    }

    #[test]
    fn bad_values_name_the_setting_and_never_echo_the_value() {
        let secret = "sk-this-is-a-real-looking-key-123";
        for (field, value, expected) in [
            ("kind", "grpc", Expected::OneOf(&KIND_IDS)),
            ("base_url", "ftp://example.com", Expected::Url),
            ("base_url", "not a url", Expected::Url),
            ("api_key_env", secret, Expected::EnvName),
            ("auth", "magic", Expected::OneOf(&AUTH_IDS)),
            (
                "max_tokens_field",
                "limit",
                Expected::OneOf(&MAX_TOKENS_IDS),
            ),
            ("stream_usage", "maybe", Expected::Bool),
            ("extra_body", "[1, 2]", Expected::JsonObject),
            ("extra_body", "{broken", Expected::JsonObject),
        ] {
            let mut config = with_presets();
            config.set(Layer::Env, format!("providers.zai.{field}"), value);
            let error = ProviderConfig::from_config(&config, "zai").unwrap_err();
            assert_eq!(
                error,
                ProviderError::Setting(SettingError::new(
                    format!("providers.zai.{field}"),
                    expected
                )),
                "{field}={value}"
            );
            assert!(!error.to_string().contains(value), "{field}: {error}");
        }
    }

    #[test]
    fn a_custom_provider_without_a_url_is_incomplete() {
        let mut config = with_presets();
        config.set(Layer::Env, "providers.mine.kind", "anthropic");
        assert_eq!(
            ProviderConfig::from_config(&config, "mine").unwrap_err(),
            ProviderError::MissingSetting {
                key: "providers.mine.base_url".to_owned()
            }
        );
    }

    #[test]
    fn the_key_comes_from_the_named_variable() {
        let provider = ProviderConfig::from_config(&with_presets(), "zai").unwrap();
        let key = provider
            .api_key_with(env(&[("ZAI_API_KEY", "  abc.def  ")]))
            .unwrap()
            .unwrap();
        assert_eq!(key.expose(), "abc.def");
    }

    #[test]
    fn a_missing_or_blank_variable_is_an_error_that_names_it() {
        let provider = ProviderConfig::from_config(&with_presets(), "zai").unwrap();
        for lookup in [env(&[]), env(&[("ZAI_API_KEY", "   ")])] {
            assert_eq!(
                provider.api_key_with(lookup).unwrap_err(),
                ProviderError::MissingApiKey {
                    env: "ZAI_API_KEY".to_owned()
                }
            );
        }
    }

    #[test]
    fn providers_without_a_key_setting_need_no_key() {
        let provider = ProviderConfig::from_config(&with_presets(), "ollama").unwrap();
        assert_eq!(provider.api_key_with(env(&[])), Ok(None));
    }

    #[test]
    fn the_key_file_is_the_fallback_and_is_trimmed() {
        let dir = std::env::temp_dir().join(format!("marelle-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("key.txt");
        std::fs::write(&file, "  file-key-1234567890\n").unwrap();

        let mut config = with_presets();
        config.set(
            Layer::Env,
            "providers.zai.api_key_file",
            file.display().to_string(),
        );
        let provider = ProviderConfig::from_config(&config, "zai").unwrap();

        let from_file = provider.api_key_with(env(&[])).unwrap().unwrap();
        assert_eq!(from_file.expose(), "file-key-1234567890");

        let from_env = provider
            .api_key_with(env(&[("ZAI_API_KEY", "env-key-0987654321")]))
            .unwrap()
            .unwrap();
        assert_eq!(from_env.expose(), "env-key-0987654321", "the variable wins");

        std::fs::write(&file, "\n").unwrap();
        assert!(matches!(
            provider.api_key_with(env(&[])).unwrap_err(),
            ProviderError::ApiKeyFile { .. }
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_key_file_names_the_path() {
        let mut config = with_presets();
        config.set(
            Layer::Env,
            "providers.ollama.api_key_file",
            "~/definitely/not/here",
        );
        let provider = ProviderConfig::from_config(&config, "ollama").unwrap();
        let error = provider
            .api_key_with(env(&[("HOME", "/nonexistent-home")]))
            .unwrap_err();
        match error {
            ProviderError::ApiKeyFile { path, .. } => {
                assert_eq!(path, "/nonexistent-home/definitely/not/here");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}
