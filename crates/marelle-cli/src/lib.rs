// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The command-line front end.
//!
//! [`run`] takes its inputs and outputs as parameters instead of touching the
//! process directly, which keeps it deterministic and easy to test.

use std::io::{self, Write};

use marelle_config::{Config, Layer};
use marelle_i18n::{Locale, tr};
use marelle_providers::ProviderKind;

/// The version of the `marelle` command.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const ENV_PREFIX: &str = "MARELLE_";
const OK: u8 = 0;
const USAGE_ERROR: u8 = 2;

/// Words in a setting's name that mark its value as secret.
const SECRET_HINTS: [&str; 4] = ["key", "token", "secret", "password"];

struct Invocation {
    lang: Option<String>,
    words: Vec<String>,
}

fn split_args(args: &[String]) -> Invocation {
    let mut lang = None;
    let mut words = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--lang" {
            lang = Some(iter.next().cloned().unwrap_or_default());
        } else if let Some(code) = arg.strip_prefix("--lang=") {
            lang = Some(code.to_owned());
        } else {
            words.push(arg.clone());
        }
    }
    Invocation { lang, words }
}

/// Runs the command line and returns the process exit code.
///
/// `env_vars` is a snapshot of the process environment.
pub fn run(
    args: &[String],
    env_vars: &[(String, String)],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<u8> {
    let invocation = split_args(args);
    let lookup = |name: &str| {
        env_vars
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };

    let locale = match &invocation.lang {
        Some(code) => match Locale::parse(code) {
            Some(locale) => locale,
            None => {
                let message = tr(Locale::from_env(lookup), "error.unknown_lang");
                writeln!(err, "{message}: '{code}'")?;
                return Ok(USAGE_ERROR);
            }
        },
        None => Locale::from_env(lookup),
    };

    let words: Vec<&str> = invocation.words.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["help" | "--help" | "-h"] => help(locale, out),
        ["version" | "--version" | "-V"] => {
            writeln!(out, "marelle {VERSION}")?;
            Ok(OK)
        }
        ["providers"] => {
            for kind in ProviderKind::ALL {
                writeln!(out, "{}", kind.id())?;
            }
            Ok(OK)
        }
        ["config", "explain"] => {
            writeln!(err, "{}", tr(locale, "error.missing_key"))?;
            Ok(USAGE_ERROR)
        }
        ["config", "explain", key] => explain(locale, key, env_vars, out),
        [other, ..] => {
            writeln!(err, "{}: {other}", tr(locale, "error.unknown_command"))?;
            Ok(USAGE_ERROR)
        }
    }
}

fn help(locale: Locale, out: &mut dyn Write) -> io::Result<u8> {
    writeln!(out, "Marelle {VERSION} - {}", tr(locale, "app.tagline"))?;
    writeln!(out)?;
    writeln!(out, "{}:", tr(locale, "help.usage"))?;
    writeln!(out, "  {}", tr(locale, "help.usage.line"))?;
    writeln!(out)?;
    writeln!(out, "{}:", tr(locale, "help.commands"))?;
    let commands = [
        ("help", "help.cmd.help"),
        ("version", "help.cmd.version"),
        ("providers", "help.cmd.providers"),
        ("config", "help.cmd.config"),
    ];
    for (name, key) in commands {
        writeln!(out, "  {name:<10} {}", tr(locale, key))?;
    }
    writeln!(out)?;
    writeln!(out, "{}:", tr(locale, "help.options"))?;
    writeln!(out, "  --lang     {}", tr(locale, "help.opt.lang"))?;
    Ok(OK)
}

/// Built-in defaults. These keys are placeholders for settings that later
/// milestones will start to use.
fn defaults() -> Config {
    let mut config = Config::new();
    config.set(Layer::Default, "agent.max_turns", "20");
    config.set(Layer::Default, "permissions.shell", "ask");
    config
}

fn is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|hint| key.contains(hint))
}

fn display_value<'a>(key: &str, value: &'a str) -> &'a str {
    if is_secret(key) { "***" } else { value }
}

fn explain(
    locale: Locale,
    key: &str,
    env_vars: &[(String, String)],
    out: &mut dyn Write,
) -> io::Result<u8> {
    let mut config = defaults();
    config.load_env(
        ENV_PREFIX,
        env_vars
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );

    writeln!(out, "{key}")?;
    match config.get(key) {
        None => writeln!(out, "  {}", tr(locale, "config.not_set"))?,
        Some(value) => {
            let value = display_value(key, value);
            writeln!(out, "  {}: {value}", tr(locale, "config.effective"))?;
            writeln!(out, "  {}:", tr(locale, "config.history"))?;
            for origin in config.explain(key) {
                let value = display_value(key, &origin.value);
                writeln!(out, "    {}: {value}", origin.layer)?;
            }
        }
    }
    Ok(OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_cli(args: &[&str], env: &[(&str, &str)]) -> (u8, String, String) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&args, &env, &mut out, &mut err).expect("writing to memory cannot fail");
        (
            code,
            String::from_utf8(out).expect("output is UTF-8"),
            String::from_utf8(err).expect("error output is UTF-8"),
        )
    }

    #[test]
    fn version_prints_the_package_version() {
        let (code, out, _) = run_cli(&["version"], &[]);
        assert_eq!(code, 0);
        assert_eq!(out, format!("marelle {VERSION}\n"));
    }

    #[test]
    fn no_arguments_shows_help() {
        let (code, out, _) = run_cli(&[], &[]);
        assert_eq!(code, 0);
        assert!(out.contains("Usage:"));
    }

    #[test]
    fn help_follows_the_environment_language() {
        let (_, out, _) = run_cli(&["help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Использование:"));
    }

    #[test]
    fn lang_flag_beats_the_environment() {
        let (_, out, _) = run_cli(&["--lang", "fr", "help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Utilisation:"));

        let (_, out, _) = run_cli(&["--lang=fr", "help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Utilisation:"));
    }

    #[test]
    fn unknown_language_is_a_usage_error() {
        let (code, out, err) = run_cli(&["--lang", "xx", "help"], &[]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("Unknown language: 'xx'"));
    }

    #[test]
    fn unknown_command_is_a_usage_error() {
        let (code, out, err) = run_cli(&["frobnicate"], &[]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("Unknown command: frobnicate"));
    }

    #[test]
    fn providers_lists_every_kind() {
        let (code, out, _) = run_cli(&["providers"], &[]);
        assert_eq!(code, 0);
        assert_eq!(out, "openai-compatible\nanthropic\n");
    }

    #[test]
    fn config_explain_shows_every_layer_in_order() {
        let (code, out, _) = run_cli(
            &["config", "explain", "agent.max_turns"],
            &[("MARELLE_AGENT__MAX_TURNS", "5")],
        );
        assert_eq!(code, 0);
        assert!(out.contains("effective value: 5"));
        let default = out.find("default: 20").expect("default is listed");
        let env = out.find("env: 5").expect("env is listed");
        assert!(default < env);
    }

    #[test]
    fn config_explain_reports_unset_keys() {
        let (code, out, _) = run_cli(&["config", "explain", "nope"], &[]);
        assert_eq!(code, 0);
        assert!(out.contains("not set"));
    }

    #[test]
    fn config_explain_needs_a_key() {
        let (code, _, err) = run_cli(&["config", "explain"], &[]);
        assert_eq!(code, 2);
        assert!(err.contains("Missing setting key"));
    }

    #[test]
    fn secret_values_are_redacted() {
        let (code, out, _) = run_cli(
            &["config", "explain", "provider.api_key"],
            &[("MARELLE_PROVIDER__API_KEY", "sk-do-not-print")],
        );
        assert_eq!(code, 0);
        assert!(out.contains("***"));
        assert!(!out.contains("sk-do-not-print"));
    }
}
