// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Layered configuration with provenance.
//!
//! Every value remembers which layer set it, so `marelle config explain <key>`
//! can show where a setting comes from and what it overrode.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub mod file;

/// A configuration layer. Later variants override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    /// Built-in defaults.
    Default,
    /// User-wide configuration file.
    Global,
    /// Per-project configuration file.
    Project,
    /// Environment variables.
    Env,
    /// Command-line flags.
    Flag,
}

impl Layer {
    /// Stable lowercase name, used in output and documentation.
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Default => "default",
            Layer::Global => "global",
            Layer::Project => "project",
            Layer::Env => "env",
            Layer::Flag => "flag",
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry in the history of a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The layer that set the value.
    pub layer: Layer,
    /// The value that layer set.
    pub value: String,
}

/// What a setting is expected to hold.
///
/// Error messages describe the expectation and never repeat the value: it
/// could be a secret that was typed into the wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expected {
    /// `true` or `false`.
    Bool,
    /// A whole number, zero or more.
    WholeNumber,
    /// A whole number, one or more.
    PositiveWholeNumber,
    /// A decimal number.
    Number,
    /// The name of an environment variable, not its value.
    EnvName,
    /// An `http` or `https` URL.
    Url,
    /// `system`, `none`, or the URL of an `http` or `https` proxy.
    Proxy,
    /// A JSON object.
    JsonObject,
    /// A list of words, written as a TOML array or comma-separated.
    List,
    /// A colour written as `#RGB` or `#RRGGBB`.
    HexColor,
    /// One of the listed words.
    OneOf(&'static [&'static str]),
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expected::Bool => f.write_str("true or false"),
            Expected::WholeNumber => f.write_str("a whole number"),
            Expected::PositiveWholeNumber => f.write_str("a whole number above zero"),
            Expected::Number => f.write_str("a number"),
            Expected::EnvName => f.write_str("the name of an environment variable"),
            Expected::Url => f.write_str("an http or https URL"),
            Expected::Proxy => f.write_str("system, none, or an http or https proxy URL"),
            Expected::JsonObject => f.write_str("a JSON object"),
            Expected::List => f.write_str("a list of words"),
            Expected::HexColor => f.write_str("a HEX colour, such as #B57EDC"),
            Expected::OneOf(words) => write!(f, "one of: {}", words.join(", ")),
        }
    }
}

/// A setting whose value cannot be used.
///
/// Carries the key and what was expected, never the value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingError {
    /// The setting's key.
    pub key: String,
    /// What the setting should hold.
    pub expected: Expected,
}

impl SettingError {
    /// Creates an error for `key`.
    pub fn new(key: impl Into<String>, expected: Expected) -> Self {
        Self {
            key: key.into(),
            expected,
        }
    }
}

impl fmt::Display for SettingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "setting {} should be {}", self.key, self.expected)
    }
}

impl Error for SettingError {}

/// Settings from every layer, kept apart so provenance is never lost.
#[derive(Debug, Clone, Default)]
pub struct Config {
    layers: BTreeMap<Layer, BTreeMap<String, String>>,
}

impl Config {
    /// Creates an empty configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `key` in `layer`, replacing any earlier value from that layer.
    pub fn set(&mut self, layer: Layer, key: impl Into<String>, value: impl Into<String>) {
        self.layers
            .entry(layer)
            .or_default()
            .insert(key.into(), value.into());
    }

    /// Loads variables that start with `prefix` into the [`Layer::Env`] layer.
    ///
    /// Other variables are ignored. See [`env_key`] for how names map to keys.
    pub fn load_env<I, K, V>(&mut self, prefix: &str, vars: I)
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        for (name, value) in vars {
            if let Some(key) = env_key(prefix, name.as_ref()) {
                self.set(Layer::Env, key, value);
            }
        }
    }

    /// The effective value: the one from the highest-priority layer.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.layers
            .values()
            .rev()
            .find_map(|values| values.get(key))
            .map(String::as_str)
    }

    /// Every layer that sets `key`, lowest priority first.
    ///
    /// The last entry is the effective value.
    pub fn explain(&self, key: &str) -> Vec<Origin> {
        self.layers
            .iter()
            .filter_map(|(layer, values)| {
                values.get(key).map(|value| Origin {
                    layer: *layer,
                    value: value.clone(),
                })
            })
            .collect()
    }

    /// All known keys in sorted order, without duplicates.
    pub fn keys(&self) -> Vec<&str> {
        self.layers
            .values()
            .flat_map(|values| values.keys().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Reads a yes-or-no setting: `true`/`false`, `yes`/`no`, `on`/`off` or `1`/`0`.
    pub fn get_bool(&self, key: &str) -> Result<Option<bool>, SettingError> {
        let Some(value) = self.get(key) else {
            return Ok(None);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(Some(true)),
            "false" | "no" | "off" | "0" => Ok(Some(false)),
            _ => Err(SettingError::new(key, Expected::Bool)),
        }
    }

    /// Reads a whole-number setting.
    pub fn get_u64(&self, key: &str) -> Result<Option<u64>, SettingError> {
        self.get(key)
            .map(|value| {
                value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| SettingError::new(key, Expected::WholeNumber))
            })
            .transpose()
    }

    /// Reads a decimal setting. Infinite and not-a-number values are refused.
    pub fn get_f64(&self, key: &str) -> Result<Option<f64>, SettingError> {
        self.get(key)
            .map(|value| match value.trim().parse::<f64>() {
                Ok(number) if number.is_finite() => Ok(number),
                _ => Err(SettingError::new(key, Expected::Number)),
            })
            .transpose()
    }

    /// Reads a list. A TOML array arrives as JSON; a person may also write
    /// a comma-separated list.
    pub fn get_list(&self, key: &str) -> Result<Option<Vec<String>>, SettingError> {
        let Some(value) = self.get(key) else {
            return Ok(None);
        };
        let value = value.trim();
        if value.starts_with('[') {
            let items: Vec<String> =
                serde_json::from_str(value).map_err(|_| SettingError::new(key, Expected::List))?;
            return Ok(Some(items));
        }
        Ok(Some(
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        ))
    }
}

/// Maps an environment variable name to a configuration key.
///
/// The prefix is stripped, the rest is lowercased, and a double underscore
/// becomes a dot: `MARELLE_AGENT__MAX_TURNS` becomes `agent.max_turns`.
/// Returns `None` when the name lacks the prefix or nothing follows it.
pub fn env_key(prefix: &str, name: &str) -> Option<String> {
    let rest = name.strip_prefix(prefix)?;
    if rest.is_empty() {
        return None;
    }
    Some(rest.to_ascii_lowercase().replace("__", "."))
}

/// Whether `value` plausibly names an environment variable.
///
/// Settings such as `api_key_env` must hold a variable's name, never the
/// secret itself. People mix the two up, so values that look like tokens
/// (long, with both lowercase letters and digits) are refused. That keeps a
/// pasted key out of error messages and `config explain` output.
pub fn looks_like_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    let starts_well = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    if !starts_well || value.len() > 64 || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    let token_like = value.len() >= 20
        && value.chars().any(|c| c.is_ascii_lowercase())
        && value.chars().any(|c| c.is_ascii_digit());
    !token_like
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highest_layer_wins() {
        let mut config = Config::new();
        config.set(Layer::Default, "agent.max_turns", "20");
        config.set(Layer::Env, "agent.max_turns", "5");
        config.set(Layer::Project, "agent.max_turns", "10");
        assert_eq!(config.get("agent.max_turns"), Some("5"));
    }

    #[test]
    fn explain_lists_layers_lowest_first() {
        let mut config = Config::new();
        config.set(Layer::Flag, "agent.max_turns", "1");
        config.set(Layer::Default, "agent.max_turns", "20");
        config.set(Layer::Global, "agent.max_turns", "15");

        let layers: Vec<Layer> = config
            .explain("agent.max_turns")
            .iter()
            .map(|origin| origin.layer)
            .collect();
        assert_eq!(layers, [Layer::Default, Layer::Global, Layer::Flag]);
    }

    #[test]
    fn missing_key_has_no_value_and_no_history() {
        let config = Config::new();
        assert_eq!(config.get("nope"), None);
        assert!(config.explain("nope").is_empty());
    }

    #[test]
    fn env_names_map_to_keys() {
        assert_eq!(
            env_key("MARELLE_", "MARELLE_AGENT__MAX_TURNS").as_deref(),
            Some("agent.max_turns")
        );
        assert_eq!(env_key("MARELLE_", "HOME"), None);
        assert_eq!(env_key("MARELLE_", "MARELLE_"), None);
    }

    #[test]
    fn load_env_ignores_foreign_variables() {
        let mut config = Config::new();
        config.load_env("MARELLE_", [("MARELLE_LANG", "ru"), ("PATH", "/bin")]);
        assert_eq!(config.get("lang"), Some("ru"));
        assert_eq!(config.keys(), ["lang"]);
    }

    #[test]
    fn keys_are_sorted_and_unique() {
        let mut config = Config::new();
        config.set(Layer::Default, "b", "1");
        config.set(Layer::Env, "b", "2");
        config.set(Layer::Default, "a", "3");
        assert_eq!(config.keys(), ["a", "b"]);
    }

    #[test]
    fn booleans_accept_common_spellings() {
        let mut config = Config::new();
        for (text, expected) in [
            ("true", true),
            ("YES", true),
            ("on", true),
            ("1", true),
            ("false", false),
            ("No", false),
            ("off", false),
            ("0", false),
        ] {
            config.set(Layer::Env, "flag", text);
            assert_eq!(config.get_bool("flag"), Ok(Some(expected)), "{text}");
        }
        assert_eq!(config.get_bool("unset"), Ok(None));
    }

    #[test]
    fn typed_getters_report_the_key_but_not_the_value() {
        let mut config = Config::new();
        config.set(Layer::Env, "a", "sk-secret-value");
        let error = config.get_bool("a").unwrap_err();
        assert_eq!(error, SettingError::new("a", Expected::Bool));
        assert!(!error.to_string().contains("sk-secret-value"));
        assert!(config.get_u64("a").is_err());
        assert!(config.get_f64("a").is_err());
    }

    #[test]
    fn numbers_are_parsed_strictly() {
        let mut config = Config::new();
        config.set(Layer::Env, "n", " 42 ");
        config.set(Layer::Env, "x", "0.25");
        config.set(Layer::Env, "neg", "-1");
        config.set(Layer::Env, "inf", "inf");
        assert_eq!(config.get_u64("n"), Ok(Some(42)));
        assert_eq!(config.get_f64("x"), Ok(Some(0.25)));
        assert!(config.get_u64("neg").is_err());
        assert!(config.get_f64("inf").is_err());
        assert_eq!(config.get_u64("missing"), Ok(None));
    }

    #[test]
    fn lists_come_from_json_or_commas() {
        let mut config = Config::new();
        config.set(Layer::Env, "json", r#"["a","b"]"#);
        config.set(Layer::Env, "commas", "a, b ,,c");
        config.set(Layer::Env, "broken", "[1,2]");
        assert_eq!(
            config.get_list("json"),
            Ok(Some(vec!["a".to_owned(), "b".to_owned()]))
        );
        assert_eq!(
            config.get_list("commas"),
            Ok(Some(vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]))
        );
        assert!(config.get_list("broken").is_err());
        assert_eq!(config.get_list("missing"), Ok(None));
    }

    #[test]
    fn env_names_are_told_apart_from_tokens() {
        assert!(looks_like_env_name("ZAI_API_KEY"));
        assert!(looks_like_env_name("my_key"));
        assert!(looks_like_env_name("_X1"));
        assert!(!looks_like_env_name(""));
        assert!(!looks_like_env_name("1KEY"));
        assert!(!looks_like_env_name("sk-ant-api03-abc"));
        assert!(!looks_like_env_name("id.secret"));
        assert!(!looks_like_env_name("0123456789abcdef0123456789abcdef"));
        assert!(!looks_like_env_name(&"A".repeat(65)));
    }
}
