// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Layered configuration with provenance.
//!
//! Every value remembers which layer set it, so `marelle config explain <key>`
//! can show where a setting comes from and what it overrode.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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
}
