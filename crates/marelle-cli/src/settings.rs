// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The configuration the commands see, and how its values are shown.

use std::path::PathBuf;

use marelle_config::{Config, Layer, file, looks_like_env_name};
use marelle_providers::{NetworkSettings, register_presets};

use crate::schema;

pub(crate) const ENV_PREFIX: &str = "MARELLE_";

/// Where the setting files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigPaths {
    /// The user-wide file, when a home or config directory is known.
    pub(crate) global: Option<PathBuf>,
    /// The per-project file.
    pub(crate) project: PathBuf,
}

/// Works out the setting file paths from the environment snapshot.
///
/// `XDG_CONFIG_HOME` wins over `HOME`. Without either there is no global
/// file, which also keeps tests that pass no home directory deterministic.
pub(crate) fn config_paths(env_vars: &[(String, String)]) -> ConfigPaths {
    let lookup = |name: &str| env_lookup(env_vars, name).filter(|value| !value.is_empty());
    let global = lookup("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|base| base.join("marelle").join("config.toml"))
        .or_else(|| {
            lookup("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".config")
                    .join("marelle")
                    .join("config.toml")
            })
        });
    let project = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".marelle")
        .join("config.toml");
    ConfigPaths { global, project }
}

/// Words in a setting's name that mark its value as secret.
const SECRET_HINTS: [&str; 4] = ["key", "token", "secret", "password"];

/// Looks a variable up in the environment snapshot.
pub(crate) fn env_lookup(env_vars: &[(String, String)], name: &str) -> Option<String> {
    env_vars
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// Builds the configuration: defaults, the global file, the project file,
/// the environment, then flags.
pub(crate) fn build_config(
    env_vars: &[(String, String)],
    flags: &[(&str, String)],
) -> Result<Config, file::FileError> {
    build_config_at(env_vars, flags, &config_paths(env_vars))
}

/// The same, with the file paths supplied. Tests use this to stay off the
/// real home directory.
pub(crate) fn build_config_at(
    env_vars: &[(String, String)],
    flags: &[(&str, String)],
    paths: &ConfigPaths,
) -> Result<Config, file::FileError> {
    let mut config = Config::new();

    for (key, value) in schema::defaults() {
        config.set(Layer::Default, key, value);
    }
    NetworkSettings::register_defaults(&mut config);
    register_presets(&mut config);

    if let Some(path) = &paths.global {
        file::load(&mut config, Layer::Global, path)?;
    }
    file::load(&mut config, Layer::Project, &paths.project)?;

    config.load_env(
        ENV_PREFIX,
        env_vars
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    for (key, value) in flags {
        config.set(Layer::Flag, *key, value.clone());
    }
    Ok(config)
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
        )
        .unwrap();
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
    fn files_sit_between_defaults_and_the_environment() {
        let dir = std::env::temp_dir().join(format!("marelle-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join("global.toml");
        let project = dir.join("project.toml");
        std::fs::write(
            &global,
            "[chat]\nmodel = \"global-model\"\nprovider = \"zai\"\n",
        )
        .unwrap();
        std::fs::write(&project, "[chat]\nmodel = \"project-model\"\n").unwrap();

        let paths = ConfigPaths {
            global: Some(global),
            project,
        };
        let config =
            build_config_at(&env(&[("MARELLE_CHAT__MODEL", "env-model")]), &[], &paths).unwrap();
        assert_eq!(config.get("chat.provider"), Some("zai"));
        assert_eq!(
            config.get("chat.model"),
            Some("env-model"),
            "the environment wins over files"
        );

        let config = build_config_at(&env(&[]), &[], &paths).unwrap();
        assert_eq!(
            config.get("chat.model"),
            Some("project-model"),
            "the project wins over the global file"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_file_is_reported_with_its_path() {
        let dir = std::env::temp_dir().join(format!("marelle-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let global = dir.join("config.toml");
        std::fs::write(&global, "[chat\nmodel = 1").unwrap();
        let paths = ConfigPaths {
            global: Some(global.clone()),
            project: dir.join("project.toml"),
        };
        let error = build_config_at(&env(&[]), &[], &paths).unwrap_err();
        assert_eq!(error.path, global);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_home_directory_decides_the_global_path() {
        let paths = config_paths(&env(&[("HOME", "/home/someone")]));
        assert_eq!(
            paths.global,
            Some(PathBuf::from("/home/someone/.config/marelle/config.toml"))
        );
        let paths = config_paths(&env(&[
            ("HOME", "/home/someone"),
            ("XDG_CONFIG_HOME", "/cfg"),
        ]));
        assert_eq!(
            paths.global,
            Some(PathBuf::from("/cfg/marelle/config.toml"))
        );
        let paths = config_paths(&env(&[]));
        assert_eq!(paths.global, None, "no home, no file, deterministic tests");
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
