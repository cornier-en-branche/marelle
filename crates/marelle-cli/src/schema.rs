// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The settings Marelle knows, with a type and a description for each.
//!
//! `config schema` prints this table, and `config set` uses it to refuse a
//! value of the wrong shape before it reaches a file. Dynamic families
//! (`providers.<id>.*`, `models.<alias>.*`, `theme.<role>`) are described by
//! patterns; the key itself supplies the name.

use marelle_config::file::ValueKind;

/// The kind of value a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Any text.
    String,
    /// `true` or `false`.
    Bool,
    /// A whole number, possibly zero.
    Integer,
    /// A whole number above zero.
    PositiveInteger,
    /// A decimal number.
    Number,
    /// An `http` or `https` URL.
    Url,
    /// The name of an environment variable, never a key.
    EnvName,
    /// `system`, `none`, or an `http`/`https` proxy URL.
    Proxy,
    /// A JSON object.
    JsonObject,
    /// A list of words.
    List,
    /// `#RGB` or `#RRGGBB`.
    HexColor,
    /// One of the listed words.
    OneOf(&'static [&'static str]),
}

impl Kind {
    /// The type name shown by `config schema`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::String => "text",
            Kind::Bool => "true/false",
            Kind::Integer => "whole number",
            Kind::PositiveInteger => "positive number",
            Kind::Number => "number",
            Kind::Url => "URL",
            Kind::EnvName => "variable name",
            Kind::Proxy => "system/none/URL",
            Kind::JsonObject => "JSON object",
            Kind::List => "list",
            Kind::HexColor => "HEX colour",
            Kind::OneOf(words) => words.first().map(|_| "one of").unwrap_or("choice"),
        }
    }

    /// The value kind used when writing TOML.
    pub(crate) fn value_kind(self) -> ValueKind {
        match self {
            Kind::Bool => ValueKind::Bool,
            Kind::Integer | Kind::PositiveInteger => ValueKind::Integer,
            Kind::Number => ValueKind::Float,
            Kind::JsonObject => ValueKind::Object,
            Kind::List => ValueKind::Array,
            _ => ValueKind::String,
        }
    }

    /// Checks a value without echoing it in an error.
    pub(crate) fn check(self, value: &str) -> Result<(), String> {
        let value = value.trim();
        let ok = match self {
            Kind::String => true,
            Kind::Bool => matches!(
                value.to_ascii_lowercase().as_str(),
                "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
            ),
            Kind::Integer => value.parse::<i64>().is_ok(),
            Kind::PositiveInteger => value.parse::<u64>().is_ok_and(|number| number > 0),
            Kind::Number => value.parse::<f64>().is_ok_and(f64::is_finite),
            Kind::Url => value
                .parse::<reqwest::Url>()
                .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host().is_some()),
            Kind::EnvName => marelle_config::looks_like_env_name(value),
            Kind::Proxy => {
                value == "system"
                    || value == "none"
                    || value.parse::<reqwest::Url>().is_ok_and(|url| {
                        matches!(url.scheme(), "http" | "https") && url.host().is_some()
                    })
            }
            Kind::JsonObject => value
                .parse::<serde_json::Value>()
                .is_ok_and(|json| json.is_object()),
            Kind::List => {
                if value.starts_with('[') {
                    value.parse::<serde_json::Value>().is_ok_and(|json| {
                        json.as_array()
                            .is_some_and(|items| items.iter().all(serde_json::Value::is_string))
                    })
                } else {
                    !value.is_empty()
                }
            }
            Kind::HexColor => super::theme::parse_hex(value).is_some(),
            Kind::OneOf(words) => words.contains(&value),
        };
        if ok {
            Ok(())
        } else {
            Err(format!("expected {}", self.name()))
        }
    }
}

/// One row of the schema.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SettingSpec {
    /// The key, or a pattern with `<id>`, `<alias>` or `<role>`.
    pub(crate) key: &'static str,
    /// What it holds.
    pub(crate) kind: Kind,
    /// The built-in default, when there is one.
    pub(crate) default: Option<&'static str>,
    /// What it does.
    pub(crate) description: &'static str,
}

const PERMISSIONS: &[&str] = &["allow", "ask", "deny"];
const AUTH_STYLES: &[&str] = &["bearer", "x-api-key", "api-key", "none"];
const PROVIDER_KINDS: &[&str] = &["openai-compatible", "anthropic"];
const MAX_TOKENS_FIELDS: &[&str] = &["max_tokens", "max_completion_tokens"];

/// The static part of the schema.
pub(crate) const SETTINGS: &[SettingSpec] = &[
    SettingSpec {
        key: "chat.provider",
        kind: Kind::String,
        default: None,
        description: "Provider id, as shown by `marelle providers`.",
    },
    SettingSpec {
        key: "chat.model",
        kind: Kind::String,
        default: None,
        description: "Model name. `provider/model` works when chat.provider is unset.",
    },
    SettingSpec {
        key: "chat.system",
        kind: Kind::String,
        default: None,
        description: "System prompt sent ahead of the conversation.",
    },
    SettingSpec {
        key: "chat.max_tokens",
        kind: Kind::PositiveInteger,
        default: None,
        description: "Longest answer, in tokens. Unset leaves it to the provider.",
    },
    SettingSpec {
        key: "chat.temperature",
        kind: Kind::Number,
        default: None,
        description: "Sampling temperature. Unset leaves it to the provider.",
    },
    SettingSpec {
        key: "chat.stream",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Print the answer as it arrives.",
    },
    SettingSpec {
        key: "chat.show_reasoning",
        kind: Kind::Bool,
        default: Some("false"),
        description: "Show the model's reasoning on stderr, when it sends any.",
    },
    SettingSpec {
        key: "chat.show_usage",
        kind: Kind::Bool,
        default: Some("false"),
        description: "Show token counts on stderr.",
    },
    SettingSpec {
        key: "chat.compare",
        kind: Kind::List,
        default: None,
        description: "Ask these models at once and label every answer.",
    },
    SettingSpec {
        key: "chat.fallback",
        kind: Kind::List,
        default: None,
        description: "Try these models in order when the chosen one fails.",
    },
    SettingSpec {
        key: "chat.judge",
        kind: Kind::String,
        default: None,
        description: "Model that merges compared answers into one.",
    },
    SettingSpec {
        key: "agent.max_turns",
        kind: Kind::PositiveInteger,
        default: Some("20"),
        description: "How many tool round-trips one question may take.",
    },
    SettingSpec {
        key: "agent.persist_sessions",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Save local conversations between runs. Stored on this device only.",
    },
    SettingSpec {
        key: "permissions.shell",
        kind: Kind::OneOf(PERMISSIONS),
        default: Some("ask"),
        description: "Run shell commands: allow, ask or deny.",
    },
    SettingSpec {
        key: "permissions.files_read",
        kind: Kind::OneOf(PERMISSIONS),
        default: Some("allow"),
        description: "Read files: allow, ask or deny.",
    },
    SettingSpec {
        key: "permissions.files_write",
        kind: Kind::OneOf(PERMISSIONS),
        default: Some("ask"),
        description: "Write files: allow, ask or deny.",
    },
    SettingSpec {
        key: "permissions.network",
        kind: Kind::OneOf(PERMISSIONS),
        default: Some("ask"),
        description: "Fetch URLs for a tool: allow, ask or deny.",
    },
    SettingSpec {
        key: "permissions.allowed_paths",
        kind: Kind::List,
        default: Some(r#"["."]"#),
        description: "Paths tools may touch. Relative paths start at the project.",
    },
    SettingSpec {
        key: "permissions.denied_paths",
        kind: Kind::List,
        default: None,
        description: "Paths tools must never touch, even inside an allowed path.",
    },
    SettingSpec {
        key: "network.connect_timeout_secs",
        kind: Kind::PositiveInteger,
        default: Some("15"),
        description: "Seconds allowed to open a connection.",
    },
    SettingSpec {
        key: "network.idle_timeout_secs",
        kind: Kind::PositiveInteger,
        default: Some("300"),
        description: "Seconds of silence allowed while waiting for data.",
    },
    SettingSpec {
        key: "network.proxy",
        kind: Kind::Proxy,
        default: Some("system"),
        description: "Proxy: system, none, or an http/https URL.",
    },
    SettingSpec {
        key: "network.ca_bundle",
        kind: Kind::String,
        default: None,
        description: "Extra certificate authorities, as a PEM file.",
    },
    SettingSpec {
        key: "network.allow_insecure_http",
        kind: Kind::Bool,
        default: Some("false"),
        description: "Allow a key over plain http to a non-local host.",
    },
    SettingSpec {
        key: "theme.primary",
        kind: Kind::HexColor,
        default: Some("#B57EDC"),
        description: "Wisteria. The main accent: banner, prompt and borders.",
    },
    SettingSpec {
        key: "theme.secondary",
        kind: Kind::HexColor,
        default: Some("#9B7EBD"),
        description: "A quieter purple for secondary labels.",
    },
    SettingSpec {
        key: "theme.accent",
        kind: Kind::HexColor,
        default: Some("#D8BFD8"),
        description: "Thistle. Highlights and selected values.",
    },
    SettingSpec {
        key: "theme.text",
        kind: Kind::HexColor,
        default: None,
        description: "Answer text. Unset keeps the terminal's own colour.",
    },
    SettingSpec {
        key: "theme.dim",
        kind: Kind::HexColor,
        default: Some("#8A7F95"),
        description: "Reasoning and less important notes.",
    },
    SettingSpec {
        key: "theme.error",
        kind: Kind::HexColor,
        default: Some("#E06C75"),
        description: "Errors.",
    },
    SettingSpec {
        key: "theme.success",
        kind: Kind::HexColor,
        default: Some("#98C379"),
        description: "Confirmations and success notes.",
    },
    SettingSpec {
        key: "theme.warning",
        kind: Kind::HexColor,
        default: Some("#E5C07B"),
        description: "Warnings and cut-off answers.",
    },
    SettingSpec {
        key: "theme.border",
        kind: Kind::HexColor,
        default: Some("#B57EDC"),
        description: "The frame around an answer. Defaults to theme.primary.",
    },
    SettingSpec {
        key: "theme.prompt",
        kind: Kind::HexColor,
        default: Some("#B57EDC"),
        description: "The input prompt. Defaults to theme.primary.",
    },
    SettingSpec {
        key: "theme.banner",
        kind: Kind::HexColor,
        default: Some("#B57EDC"),
        description: "The banner. Defaults to theme.primary.",
    },
    SettingSpec {
        key: "theme.reasoning",
        kind: Kind::HexColor,
        default: Some("#8A7F95"),
        description: "Reasoning text. Defaults to theme.dim.",
    },
    SettingSpec {
        key: "ui.banner",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Show the banner when an interactive session starts.",
    },
    SettingSpec {
        key: "ui.panel",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Show the provider, model and system prompt at the start.",
    },
    SettingSpec {
        key: "ui.frame",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Draw a rule above and below answers in an interactive session.",
    },
    SettingSpec {
        key: "ui.spinner",
        kind: Kind::Bool,
        default: Some("true"),
        description: "Say that Marelle is waiting for the first token.",
    },
    SettingSpec {
        key: "ui.width",
        kind: Kind::Integer,
        default: Some("0"),
        description: "Frame width in columns. 0 means the terminal's width.",
    },
];

/// The dynamic provider fields.
const PROVIDER_FIELDS: &[SettingSpec] = &[
    SettingSpec {
        key: "providers.<id>.kind",
        kind: Kind::OneOf(PROVIDER_KINDS),
        default: None,
        description: "Wire protocol: openai-compatible or anthropic.",
    },
    SettingSpec {
        key: "providers.<id>.base_url",
        kind: Kind::Url,
        default: None,
        description: "The API's base address, without a trailing slash.",
    },
    SettingSpec {
        key: "providers.<id>.api_key_env",
        kind: Kind::EnvName,
        default: None,
        description: "Environment variable that holds the key; never the key itself.",
    },
    SettingSpec {
        key: "providers.<id>.api_key_file",
        kind: Kind::String,
        default: None,
        description: "File that holds the key, used when the variable is unset.",
    },
    SettingSpec {
        key: "providers.<id>.auth",
        kind: Kind::OneOf(AUTH_STYLES),
        default: None,
        description: "How the key travels: bearer, x-api-key, api-key or none.",
    },
    SettingSpec {
        key: "providers.<id>.max_tokens_field",
        kind: Kind::OneOf(MAX_TOKENS_FIELDS),
        default: None,
        description: "The request field that limits the answer length.",
    },
    SettingSpec {
        key: "providers.<id>.stream_usage",
        kind: Kind::Bool,
        default: None,
        description: "Ask for token counts in streamed answers.",
    },
    SettingSpec {
        key: "providers.<id>.extra_body",
        kind: Kind::JsonObject,
        default: None,
        description: "Extra fields merged into every request body.",
    },
];

/// The dynamic model alias fields.
const MODEL_FIELDS: &[SettingSpec] = &[
    SettingSpec {
        key: "models.<alias>.provider",
        kind: Kind::String,
        default: None,
        description: "Provider id for this alias.",
    },
    SettingSpec {
        key: "models.<alias>.model",
        kind: Kind::String,
        default: None,
        description: "Model name for this alias.",
    },
    SettingSpec {
        key: "models.<alias>.system",
        kind: Kind::String,
        default: None,
        description: "System prompt for this alias, when it differs.",
    },
    SettingSpec {
        key: "models.<alias>.max_tokens",
        kind: Kind::PositiveInteger,
        default: None,
        description: "Longest answer for this alias.",
    },
    SettingSpec {
        key: "models.<alias>.temperature",
        kind: Kind::Number,
        default: None,
        description: "Sampling temperature for this alias.",
    },
];

/// The theme roles.
const THEME_ROLES: &[&str] = &[
    "primary",
    "secondary",
    "accent",
    "text",
    "dim",
    "error",
    "success",
    "warning",
    "border",
    "prompt",
    "banner",
    "reasoning",
];

/// Finds the specification for a key, including the dynamic families.
pub(crate) fn lookup(key: &str) -> Option<&'static SettingSpec> {
    if let Some(spec) = SETTINGS.iter().find(|spec| spec.key == key) {
        return Some(spec);
    }
    if let Some(spec) = PROVIDER_FIELDS
        .iter()
        .find(|spec| matches_pattern(spec.key, key, "providers", "<id>"))
    {
        return Some(spec);
    }
    if let Some(spec) = MODEL_FIELDS
        .iter()
        .find(|spec| matches_pattern(spec.key, key, "models", "<alias>"))
    {
        return Some(spec);
    }
    if let Some(role) = key.strip_prefix("theme.") {
        if THEME_ROLES.contains(&role) {
            return SETTINGS
                .iter()
                .find(|spec| spec.key.strip_prefix("theme.") == Some(role));
        }
    }
    None
}

/// Whether `key` fits `pattern`, such as `providers.<id>.base_url`.
fn matches_pattern(pattern: &str, key: &str, prefix: &str, placeholder: &str) -> bool {
    let Some(rest) = key
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return false;
    };
    let Some((name, field)) = rest.split_once('.') else {
        return false;
    };
    if name.is_empty() || field.is_empty() {
        return false;
    }
    let expected = pattern
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(pattern);
    expected.replacen(placeholder, name, 1) == rest
}

/// Every default that belongs to the schema.
pub(crate) fn defaults() -> impl Iterator<Item = (&'static str, &'static str)> {
    SETTINGS
        .iter()
        .filter_map(|spec| spec.default.map(|default| (spec.key, default)))
}

/// Every row for `config schema`, with dynamic families included once.
pub(crate) fn rows() -> impl Iterator<Item = &'static SettingSpec> {
    SETTINGS
        .iter()
        .chain(PROVIDER_FIELDS.iter())
        .chain(MODEL_FIELDS.iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_static_key_has_a_spec() {
        for spec in SETTINGS {
            assert_eq!(lookup(spec.key).map(|found| found.key), Some(spec.key));
        }
    }

    #[test]
    fn dynamic_keys_use_the_right_field() {
        assert_eq!(
            lookup("providers.zai.base_url").map(|spec| spec.kind),
            Some(Kind::Url)
        );
        assert_eq!(
            lookup("providers.my-gateway.stream_usage").map(|spec| spec.kind),
            Some(Kind::Bool)
        );
        assert_eq!(
            lookup("models.fast.temperature").map(|spec| spec.kind),
            Some(Kind::Number)
        );
        assert_eq!(
            lookup("theme.primary").map(|spec| spec.kind),
            Some(Kind::HexColor)
        );
        assert!(lookup("providers.zai.nonsense").is_none());
        assert!(lookup("models..model").is_none());
    }

    #[test]
    fn kinds_check_values_without_repeating_them() {
        assert!(Kind::Bool.check("yes").is_ok());
        assert!(Kind::Bool.check("maybe").is_err());
        assert!(Kind::PositiveInteger.check("0").is_err());
        assert!(Kind::PositiveInteger.check("20").is_ok());
        assert!(Kind::Url.check("https://example.com/v1").is_ok());
        assert!(Kind::Url.check("ftp://example.com").is_err());
        assert!(Kind::HexColor.check("#B57EDC").is_ok());
        assert!(Kind::HexColor.check("#b57").is_ok());
        assert!(Kind::HexColor.check("purple").is_err());
        let error = Kind::OneOf(PERMISSIONS).check("sometimes").unwrap_err();
        assert!(!error.contains("sometimes"));
    }
}
