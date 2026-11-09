// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The configuration the commands see, and how its values are shown.

use marelle_config::{Config, Layer, looks_like_env_name};
use marelle_providers::{NetworkSettings, register_presets};

pub(crate) const ENV_PREFIX: &str = "MARELLE_";

/// Words in a setting's name that mark its value as secret.
const SECRET_HINTS: [&str; 4] = ["key", "token", "secret", "password"];

/// Looks a variable up in the environment snapshot.
pub(crate) fn env_lookup(env_vars: &[(String, String)], name: &str) -> Option<String> {
    env_vars
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// Builds the configuration: defaults, then the environment, then flags.
///
/// Files will slot in between the defaults and the environment.
pub(crate) fn build_config(env_vars: &[(String, String)], flags: &[(&str, String)]) -> Config {
    let mut config = Config::new();
    // Placeholders for settings that later milestones will start to use.
    config.set(Layer::Default, "agent.max_turns", "20");
    config.set(Layer::Default, "permissions.shell", "ask");

    config.set(Layer::Default, "chat.stream", "true");
    config.set(Layer::Default, "chat.show_reasoning", "false");
    config.set(Layer::Default, "chat.show_usage", "false");
    NetworkSettings::register_defaults(&mut config);
    register_presets(&mut config);

    config.load_env(
        ENV_PREFIX,
        env_vars
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    for (key, value) in flags {
        config.set(Layer::Flag, *key, value.clone());
    }
    config
}

/// The text to show for a setting's value.
///
/// Secrets never appear. A setting that only names where a secret lives
/// (`api_key_env`, `api_key_file`) is shown, because the name is what a
/// person needs to see. Credentials inside URLs are hidden.
pub(crate) fn display_value(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    let last = key.rsplit('.').next().unwrap_or(&key);

    if last.ends_with("_env") && looks_like_env_name(value) {
        return value.to_owned();
    }
    if last.ends_with("_file") {
        return value.to_owned();
    }
    if SECRET_HINTS.iter().any(|hint| key.contains(hint)) {
        return "***".to_owned();
    }
    if last == "proxy" || last == "url" || last.ends_with("_url") {
        return redact_url(value);
    }
    value.to_owned()
}

/// Replaces the user name and password of a URL with `***`.
pub(crate) fn redact_url(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.to_owned();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}{tail}"),
        None => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn layers_stack_in_order() {
        let config = build_config(
            &env(&[
                ("MARELLE_CHAT__MODEL", "from-env"),
                ("MARELLE_CHAT__SYSTEM", "env"),
            ]),
            &[("chat.model", "from-flag".to_owned())],
        );
        assert_eq!(config.get("chat.model"), Some("from-flag"));
        assert_eq!(config.get("chat.system"), Some("env"));
        assert_eq!(config.get("chat.stream"), Some("true"));
        assert_eq!(config.get("network.idle_timeout_secs"), Some("300"));
        assert_eq!(
            config.get("providers.zai.base_url"),
            Some("https://api.z.ai/api/paas/v4")
        );
    }

    #[test]
    fn secrets_are_hidden_but_where_to_find_them_is_not() {
        assert_eq!(display_value("provider.api_key", "sk-secret"), "***");
        assert_eq!(display_value("auth.token", "abc"), "***");
        assert_eq!(display_value("db.PASSWORD", "hunter2"), "***");
        assert_eq!(
            display_value("providers.zai.api_key_env", "ZAI_API_KEY"),
            "ZAI_API_KEY"
        );
        assert_eq!(
            display_value("providers.zai.api_key_file", "~/.config/marelle/zai.key"),
            "~/.config/marelle/zai.key"
        );
        // A real key typed where a name belongs is still hidden.
        assert_eq!(
            display_value("providers.zai.api_key_env", "sk-0123456789abcdef0123456789"),
            "***"
        );
        assert_eq!(display_value("chat.model", "glm"), "glm");
    }

    #[test]
    fn credentials_in_urls_are_hidden() {
        assert_eq!(
            display_value("network.proxy", "http://user:hunter2@proxy.example:3128"),
            "http://***@proxy.example:3128"
        );
        assert_eq!(
            display_value(
                "providers.x.base_url",
                "https://u:p@gateway.example/v1?x=a@b"
            ),
            "https://***@gateway.example/v1?x=a@b"
        );
        assert_eq!(display_value("network.proxy", "system"), "system");
        assert_eq!(
            display_value("providers.zai.base_url", "https://api.z.ai/api/paas/v4"),
            "https://api.z.ai/api/paas/v4"
        );
        assert_eq!(
            redact_url("https://host/path@not-userinfo"),
            "https://host/path@not-userinfo"
        );
    }
}
