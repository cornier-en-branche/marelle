// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The command-line front end.
//!
//! [`run`] takes its inputs and outputs as parameters instead of touching the
//! process directly, which keeps it deterministic and easy to test.

mod args;
mod chat;
mod inspect;
mod messages;
mod schema;
mod settings;
mod theme;
mod tools;
mod tui;

use std::io::{self, BufRead, Write};

use marelle_i18n::{Locale, tr, tr_with};

use crate::args::{ArgError, Command};
use crate::messages::USAGE_ERROR;

/// The version of the `marelle` command.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const OK: u8 = 0;

/// The streams of a run, and what they are connected to.
pub struct Io<'a> {
    /// Where a prompt is read from.
    pub input: &'a mut dyn BufRead,
    /// Where answers go. Nothing else is written here.
    pub out: &'a mut dyn Write,
    /// Where prompts, notes, reasoning and errors go.
    pub err: &'a mut dyn Write,
    /// Whether `input` is a person at a terminal. Without a prompt on the
    /// command line, that starts a session; a pipe is read as the prompt.
    pub interactive: bool,
    /// Whether the process owns a real terminal that the full-screen
    /// interface may take over: standard input, output and error are all
    /// terminals. Only `main` sets this. Tests and anything that embeds the
    /// library leave it off, so that a run never reaches for the terminal of
    /// whoever started it.
    pub screen: bool,
    /// Whether `err` understands colour.
    pub color: bool,
}

/// Runs the command line and returns the process exit code.
///
/// `env_vars` is a snapshot of the process environment.
pub fn run(args: &[String], env_vars: &[(String, String)], io: &mut Io<'_>) -> io::Result<u8> {
    let global = args::split_global(args);
    let lookup = |name: &str| settings::env_lookup(env_vars, name);

    let locale = match &global.lang {
        Some(code) => match Locale::parse(code) {
            Some(locale) => locale,
            None => {
                let message = tr(Locale::from_env(lookup), "error.unknown_lang");
                writeln!(io.err, "{message}: '{code}'")?;
                return Ok(USAGE_ERROR);
            }
        },
        None => Locale::from_env(lookup),
    };

    let mut words = global.words.clone();
    if words.is_empty() && io.interactive {
        words.push("chat".to_owned());
    } else if words
        .first()
        .is_some_and(|word| matches!(word.as_str(), "--tui" | "--continue" | "-c"))
    {
        let first = words.remove(0);
        let mut chat = vec!["chat".to_owned()];
        if first != "--tui" {
            chat.push(first);
        }
        chat.extend(words);
        words = chat;
    } else if words.first().is_some_and(|word| word == "--tui") {
        words[0] = "chat".to_owned();
    }
    let command = match args::parse(&words) {
        Ok(command) => command,
        Err(error) => {
            let (key, name) = match &error {
                ArgError::UnknownOption(name) => ("error.unknown_option", name),
                ArgError::NeedsValue(name) => ("error.needs_value", name),
            };
            writeln!(
                io.err,
                "marelle: {}",
                tr_with(locale, key, &[("name", name)])
            )?;
            return Ok(USAGE_ERROR);
        }
    };

    match command {
        Command::Help => help(locale, io.out),
        Command::Version => {
            writeln!(io.out, "marelle {VERSION}")?;
            Ok(OK)
        }
        Command::Providers => inspect::providers(locale, env_vars, io),
        Command::ConfigShow { prefix } => inspect::show(locale, prefix.as_deref(), env_vars, io),
        Command::ConfigExplain { key } => inspect::explain(locale, key.as_deref(), env_vars, io),
        Command::ConfigGet { key } => inspect::get(locale, &key, env_vars, io),
        Command::ConfigSet { key, value, file } => {
            inspect::set(locale, &key, &value, &file, env_vars, io)
        }
        Command::ConfigUnset { key, file } => inspect::unset(locale, &key, &file, env_vars, io),
        Command::ConfigSchema { prefix } => inspect::schema(locale, prefix.as_deref(), io),
        Command::ConfigPath => inspect::paths(locale, env_vars, io),
        Command::Chat(chat_args) if chat_args.help => help(locale, io.out),
        Command::Chat(chat_args) => chat::run(chat_args, locale, env_vars, io),
        Command::Unknown(name) => {
            writeln!(io.err, "{}: {name}", tr(locale, "error.unknown_command"))?;
            Ok(USAGE_ERROR)
        }
    }
}

fn help(locale: Locale, out: &mut dyn Write) -> io::Result<u8> {
    let say = |key: &str| tr(locale, key);

    writeln!(out, "Marelle {VERSION} - {}", say("app.tagline"))?;
    writeln!(out)?;
    writeln!(out, "{}:", say("help.usage"))?;
    writeln!(out, "  {}", say("help.usage.line"))?;

    writeln!(out)?;
    writeln!(out, "{}:", say("help.commands"))?;
    let commands = [
        ("chat", "help.cmd.chat"),
        ("providers", "help.cmd.providers"),
        ("config", "help.cmd.config"),
        ("help", "help.cmd.help"),
        ("version", "help.cmd.version"),
    ];
    for (name, key) in commands {
        writeln!(out, "  {name:<10} {}", say(key))?;
    }

    writeln!(out)?;
    writeln!(out, "{}:", say("help.chat_options"))?;
    let options = [
        ("-p, --provider <id>", "help.opt.provider"),
        ("-m, --model <name>", "help.opt.model"),
        ("-q, --query <text>", "help.opt.query"),
        ("-c, --continue", "help.opt.continue"),
        ("    --system <text>", "help.opt.system"),
        ("    --max-tokens <n>", "help.opt.max_tokens"),
        ("    --temperature <x>", "help.opt.temperature"),
        ("    --no-stream", "help.opt.no_stream"),
        ("    --show-reasoning", "help.opt.show_reasoning"),
        ("    --show-usage", "help.opt.show_usage"),
    ];
    for (flag, key) in options {
        writeln!(out, "  {flag:<22} {}", say(key))?;
    }

    writeln!(out)?;
    writeln!(out, "{}:", say("help.options"))?;
    writeln!(
        out,
        "  {:<22} {}",
        "    --lang <code>",
        say("help.opt.lang")
    )?;

    writeln!(out)?;
    writeln!(out, "{}:", say("help.examples"))?;
    writeln!(out, "  # {}", say("help.example.ask"))?;
    writeln!(
        out,
        "  marelle chat -m zai/MODEL \"Explain lifetimes in two sentences\""
    )?;
    writeln!(out, "  # {}", say("help.example.pipe"))?;
    writeln!(
        out,
        "  git diff | marelle chat -p anthropic -m MODEL \"Review this diff:\" -"
    )?;
    writeln!(out, "  # {}", say("help.example.repl"))?;
    writeln!(out, "  marelle --tui")?;
    writeln!(out, "  marelle --continue")?;

    writeln!(out)?;
    writeln!(out, "{}", say("help.env"))?;
    writeln!(out, "{}", say("help.exit_codes"))?;
    Ok(OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn run_cli(args: &[&str], env: &[(&str, &str)]) -> (u8, String, String) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut input = Cursor::new(Vec::new());
        let mut io = Io {
            input: &mut input,
            out: &mut out,
            err: &mut err,
            interactive: false,
            screen: false,
            color: false,
        };
        let code = run(&args, &env, &mut io).expect("writing to memory cannot fail");
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
        assert!(out.contains("chat"));
        assert!(out.contains("--max-tokens"));
    }

    #[test]
    fn help_follows_the_environment_language() {
        let (_, out, _) = run_cli(&["help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Использование:"));
        let (_, out, _) = run_cli(&["help"], &[("MARELLE_LANG", "uk")]);
        assert!(out.contains("Використання:"));
    }

    #[test]
    fn lang_flag_beats_the_environment() {
        let (_, out, _) = run_cli(&["--lang", "fr", "help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Utilisation:"));

        let (_, out, _) = run_cli(&["--lang=fr", "help"], &[("LANG", "ru_RU.UTF-8")]);
        assert!(out.contains("Utilisation:"));
    }

    #[test]
    fn every_language_has_a_complete_help_page() {
        for locale in Locale::ALL {
            let (code, out, _) = run_cli(&["--lang", locale.code(), "help"], &[]);
            assert_eq!(code, 0);
            assert!(
                !out.contains("help."),
                "a missing translation shows its key ({locale:?})"
            );
            assert!(out.contains("--show-reasoning"));
            assert!(out.contains("MARELLE_CHAT__MODEL"));
        }
    }

    #[test]
    fn help_descriptions_line_up_in_every_language() {
        // An option row is two spaces, a 22-wide flag column, a space, then
        // the description: so column 24 is blank and column 25 is not.
        for locale in Locale::ALL {
            let (_, out, _) = run_cli(&["--lang", locale.code(), "help"], &[]);
            let rows: Vec<&str> = out
                .lines()
                .filter(|line| line.starts_with("  -") || line.starts_with("      --"))
                .collect();
            // Four short chat flags, six long chat flags and one global flag.
            assert_eq!(rows.len(), 11, "{locale:?}: option rows in:\n{out}");
            for row in rows {
                let cells: Vec<char> = row.chars().collect();
                assert_eq!(cells[24], ' ', "{locale:?}: {row}");
                assert_ne!(cells[25], ' ', "{locale:?}: {row}");
            }
        }
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
    fn option_mistakes_are_usage_errors() {
        let (code, out, err) = run_cli(&["chat", "--wat"], &[]);
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.contains("Unknown option: --wat"));

        let (code, _, err) = run_cli(&["chat", "--model"], &[]);
        assert_eq!(code, 2);
        assert!(err.contains("The option --model needs a value."));
    }

    #[test]
    fn chat_help_is_the_general_help() {
        let (code, out, _) = run_cli(&["chat", "--help"], &[]);
        assert_eq!(code, 0);
        assert!(out.contains("Chat options:"));
    }

    #[test]
    fn providers_come_as_a_table_with_key_status() {
        let (code, out, _) = run_cli(&["providers"], &[("ZAI_API_KEY", "set-in-the-test")]);
        assert_eq!(code, 0);
        let line = |name: &str| {
            out.lines()
                .find(|line| line.starts_with(name))
                .unwrap_or_else(|| panic!("no row for {name} in:\n{out}"))
        };
        assert!(out.lines().next().unwrap().starts_with("Provider"));
        assert!(line("zai ").contains("openai-compatible"));
        assert!(line("zai ").contains("https://api.z.ai/api/paas/v4"));
        assert!(line("zai ").contains("ZAI_API_KEY (set)"));
        assert!(
            !out.contains("set-in-the-test"),
            "the key itself is never shown"
        );
        assert!(line("anthropic ").contains("ANTHROPIC_API_KEY (not set)"));
        assert!(line("ollama ").contains("no key needed"));
        assert!(out.contains("marelle chat --provider"));
    }

    #[test]
    fn custom_providers_join_the_table_and_broken_ones_say_why() {
        let (_, out, _) = run_cli(
            &["providers"],
            &[
                (
                    "MARELLE_PROVIDERS__WORK__BASE_URL",
                    "https://u:p@gateway.example/v1",
                ),
                (
                    "MARELLE_PROVIDERS__WORK__API_KEY_FILE",
                    "/definitely/not/here",
                ),
                ("MARELLE_PROVIDERS__BROKEN__KIND", "openai"),
            ],
        );
        let work = out
            .lines()
            .find(|line| line.starts_with("work"))
            .expect("a row for work");
        assert!(work.contains("https://***@gateway.example/v1"), "{work}");
        assert!(!work.contains("u:p@"));
        assert!(
            work.contains("file /definitely/not/here (missing)"),
            "{work}"
        );
        let broken = out
            .lines()
            .find(|line| line.starts_with("broken"))
            .expect("a row for broken");
        assert!(broken.contains("unusable"), "{broken}");
        assert!(broken.contains("providers.broken.base_url"), "{broken}");
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

    #[test]
    fn config_show_lists_settings_with_their_layer() {
        let (code, out, _) = run_cli(
            &["config", "show", "chat."],
            &[
                ("MARELLE_CHAT__MODEL", "glm"),
                ("MARELLE_CHAT__SYSTEM", "Be brief."),
            ],
        );
        assert_eq!(code, 0);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().all(|line| line.starts_with("chat.")), "{out}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("chat.model") && l.contains("= glm") && l.ends_with("[env]"))
        );
        assert!(lines.iter().any(|l| l.contains("chat.stream")
            && l.contains("= true")
            && l.ends_with("[default]")));
        let sorted = {
            let mut copy = lines.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(lines, sorted, "keys come sorted");
    }

    #[test]
    fn config_show_hides_secrets_and_shows_where_keys_live() {
        let (_, out, _) = run_cli(
            &["config", "show", "providers.zai."],
            &[("MARELLE_PROVIDERS__ZAI__API_KEY_FILE", "~/.keys/zai")],
        );
        assert!(out.contains("providers.zai.api_key_env"));
        assert!(out.contains("= ZAI_API_KEY"), "{out}");
        assert!(out.contains("= ~/.keys/zai"), "{out}");
    }

    #[test]
    fn config_show_with_no_match_says_so() {
        let (code, out, _) = run_cli(&["config", "show", "zzz."], &[]);
        assert_eq!(code, 0);
        assert_eq!(out, "No settings match.\n");
    }

    #[test]
    fn config_set_writes_a_checked_value_and_unset_removes_it() {
        let dir = std::env::temp_dir().join(format!("marelle-cli-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let file = path.display().to_string();

        let (code, out, err) = run_cli(
            &["config", "set", "theme.primary", "#112233", "--file", &file],
            &[],
        );
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("theme.primary"), "{out}");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[theme]"), "{written}");
        assert!(written.contains("primary = \"#112233\""), "{written}");

        let (code, _, err) = run_cli(
            &["config", "set", "theme.primary", "purple", "--file", &file],
            &[],
        );
        assert_eq!(code, 2);
        assert!(err.contains("theme.primary"), "{err}");
        assert!(err.contains("HEX colour"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            written,
            "a refused value leaves the file alone"
        );

        let (code, _, err) = run_cli(
            &[
                "config",
                "set",
                "theme.nonsense",
                "#112233",
                "--file",
                &file,
            ],
            &[],
        );
        assert_eq!(code, 2);
        assert!(err.contains("Unknown setting"), "{err}");

        let (code, _, _) = run_cli(&["config", "unset", "theme.primary", "--file", &file], &[]);
        assert_eq!(code, 0);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("primary"), "{after}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_schema_shows_types_and_dynamic_families() {
        let (code, out, _) = run_cli(&["config", "schema", "theme."], &[]);
        assert_eq!(code, 0);
        assert!(out.contains("theme.primary"), "{out}");
        assert!(out.contains("HEX colour"), "{out}");
        assert!(out.contains("#B57EDC"), "{out}");

        let (_, out, _) = run_cli(&["config", "schema", "providers."], &[]);
        assert!(out.contains("providers.<id>.base_url"), "{out}");
        assert!(out.contains("URL"), "{out}");
    }

    #[test]
    fn config_path_names_both_files() {
        let (code, out, _) = run_cli(&["config", "path"], &[("HOME", "/home/someone")]);
        assert_eq!(code, 0);
        assert!(
            out.contains("/home/someone/.config/marelle/config.toml"),
            "{out}"
        );
        assert!(out.contains(".marelle/config.toml"), "{out}");
        assert!(out.contains("missing"), "{out}");
    }
}
