// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Command-line parsing.
//!
//! Small on purpose. Every option is a setting in disguise: the parser turns
//! flags into values for the top settings layer, and the rest of the program
//! sees one configuration, whichever way a value arrived.

/// What the user asked for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Version,
    Providers,
    ConfigShow { prefix: Option<String> },
    ConfigExplain { key: Option<String> },
    Chat(ChatArgs),
    Unknown(String),
}

/// The arguments of `marelle chat`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ChatArgs {
    /// Settings given as flags, as (key, value), in the order written.
    pub flags: Vec<(&'static str, String)>,
    /// The words of the prompt. A lone `-` stands for standard input.
    pub words: Vec<String>,
    /// `--help` was given.
    pub help: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArgError {
    UnknownOption(String),
    NeedsValue(String),
}

/// The global options, pulled out of the arguments wherever they stand.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Global {
    /// The value of `--lang`, if given.
    pub lang: Option<String>,
    /// Everything else, in order.
    pub words: Vec<String>,
}

/// Separates `--lang` from the rest. After a bare `--` nothing is touched, so
/// a prompt can say anything.
pub(crate) fn split_global(args: &[String]) -> Global {
    let mut lang = None;
    let mut words = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            words.push(arg.clone());
            words.extend(iter.by_ref().cloned());
            break;
        }
        if arg == "--lang" {
            lang = Some(iter.next().cloned().unwrap_or_default());
        } else if let Some(code) = arg.strip_prefix("--lang=") {
            lang = Some(code.to_owned());
        } else {
            words.push(arg.clone());
        }
    }
    Global { lang, words }
}

pub(crate) fn parse(words: &[String]) -> Result<Command, ArgError> {
    let mut iter = words.iter().map(String::as_str);
    let command = match iter.next() {
        None | Some("help" | "--help" | "-h") => Command::Help,
        Some("version" | "--version" | "-V") => Command::Version,
        Some("providers") => Command::Providers,
        Some("config") => match iter.next() {
            None => Command::ConfigShow { prefix: None },
            Some("show") => Command::ConfigShow {
                prefix: iter.next().map(str::to_owned),
            },
            Some("explain") => Command::ConfigExplain {
                key: iter.next().map(str::to_owned),
            },
            Some(other) => Command::Unknown(format!("config {other}")),
        },
        Some("chat") => Command::Chat(parse_chat(iter)?),
        Some(other) => Command::Unknown(other.to_owned()),
    };
    Ok(command)
}

fn parse_chat<'a>(mut words: impl Iterator<Item = &'a str>) -> Result<ChatArgs, ArgError> {
    let mut chat = ChatArgs::default();
    while let Some(word) = words.next() {
        if word == "--" {
            chat.words.extend(words.by_ref().map(str::to_owned));
            break;
        }
        if word == "-" || !word.starts_with('-') {
            chat.words.push(word.to_owned());
            continue;
        }

        // `--name=value` carries its value along; `--name value` does not.
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (word, None),
        };
        let mut value = || -> Result<String, ArgError> {
            inline
                .clone()
                .or_else(|| words.next().map(str::to_owned))
                .ok_or_else(|| ArgError::NeedsValue(name.to_owned()))
        };
        match name {
            "-h" | "--help" => chat.help = true,
            "-p" | "--provider" => chat.flags.push(("chat.provider", value()?)),
            "-m" | "--model" => chat.flags.push(("chat.model", value()?)),
            "--system" => chat.flags.push(("chat.system", value()?)),
            "--max-tokens" => chat.flags.push(("chat.max_tokens", value()?)),
            "--temperature" => chat.flags.push(("chat.temperature", value()?)),
            "--stream" => chat.flags.push(("chat.stream", "true".to_owned())),
            "--no-stream" => chat.flags.push(("chat.stream", "false".to_owned())),
            "--show-reasoning" => chat.flags.push(("chat.show_reasoning", "true".to_owned())),
            "--show-usage" => chat.flags.push(("chat.show_usage", "true".to_owned())),
            _ => return Err(ArgError::UnknownOption(name.to_owned())),
        }
    }
    Ok(chat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    fn chat(list: &[&str]) -> ChatArgs {
        let mut all = vec!["chat"];
        all.extend_from_slice(list);
        match parse(&words(&all)).expect("parses") {
            Command::Chat(chat) => chat,
            other => panic!("not a chat command: {other:?}"),
        }
    }

    #[test]
    fn language_is_found_anywhere() {
        let global = split_global(&words(&["--lang", "fr", "help"]));
        assert_eq!(global.lang.as_deref(), Some("fr"));
        assert_eq!(global.words, ["help"]);

        let global = split_global(&words(&["chat", "--lang=uk", "hi"]));
        assert_eq!(global.lang.as_deref(), Some("uk"));
        assert_eq!(global.words, ["chat", "hi"]);

        let global = split_global(&words(&["help", "--lang"]));
        assert_eq!(
            global.lang.as_deref(),
            Some(""),
            "a missing value is an unknown language"
        );
    }

    #[test]
    fn nothing_after_a_double_dash_is_an_option() {
        let global = split_global(&words(&["chat", "--", "--lang", "fr"]));
        assert_eq!(global.lang, None);
        assert_eq!(global.words, ["chat", "--", "--lang", "fr"]);
    }

    #[test]
    fn commands_are_recognised() {
        assert_eq!(parse(&[]), Ok(Command::Help));
        assert_eq!(parse(&words(&["--help"])), Ok(Command::Help));
        assert_eq!(parse(&words(&["-V"])), Ok(Command::Version));
        assert_eq!(parse(&words(&["version"])), Ok(Command::Version));
        assert_eq!(parse(&words(&["providers"])), Ok(Command::Providers));
        assert_eq!(
            parse(&words(&["config"])),
            Ok(Command::ConfigShow { prefix: None })
        );
        assert_eq!(
            parse(&words(&["config", "show", "chat."])),
            Ok(Command::ConfigShow {
                prefix: Some("chat.".into())
            })
        );
        assert_eq!(
            parse(&words(&["config", "explain"])),
            Ok(Command::ConfigExplain { key: None })
        );
        assert_eq!(
            parse(&words(&["config", "explain", "a.b"])),
            Ok(Command::ConfigExplain {
                key: Some("a.b".into())
            })
        );
        assert_eq!(
            parse(&words(&["config", "nope"])),
            Ok(Command::Unknown("config nope".into()))
        );
        assert_eq!(
            parse(&words(&["frobnicate"])),
            Ok(Command::Unknown("frobnicate".into()))
        );
    }

    #[test]
    fn chat_flags_become_settings() {
        let args = chat(&[
            "-p",
            "zai",
            "--model",
            "glm",
            "--system=Be brief.",
            "--max-tokens",
            "50",
            "--temperature=0.2",
            "--no-stream",
            "--show-reasoning",
            "--show-usage",
        ]);
        assert_eq!(
            args.flags,
            [
                ("chat.provider", "zai".to_owned()),
                ("chat.model", "glm".to_owned()),
                ("chat.system", "Be brief.".to_owned()),
                ("chat.max_tokens", "50".to_owned()),
                ("chat.temperature", "0.2".to_owned()),
                ("chat.stream", "false".to_owned()),
                ("chat.show_reasoning", "true".to_owned()),
                ("chat.show_usage", "true".to_owned()),
            ]
        );
        assert!(args.words.is_empty());
    }

    #[test]
    fn the_prompt_is_whatever_is_not_an_option() {
        let args = chat(&["tell", "me", "-m", "x", "a", "joke"]);
        assert_eq!(args.words, ["tell", "me", "a", "joke"]);
        assert_eq!(args.flags, [("chat.model", "x".to_owned())]);
    }

    #[test]
    fn a_lone_dash_is_stdin_and_a_double_dash_ends_the_options() {
        let args = chat(&["Summarize:", "-"]);
        assert_eq!(args.words, ["Summarize:", "-"]);
        let args = chat(&["-m", "x", "--", "-5", "is", "negative", "--model"]);
        assert_eq!(args.words, ["-5", "is", "negative", "--model"]);
        assert_eq!(args.flags.len(), 1);
    }

    #[test]
    fn a_later_stream_flag_wins_through_the_layer_order() {
        let args = chat(&["--no-stream", "--stream"]);
        assert_eq!(
            args.flags,
            [
                ("chat.stream", "false".to_owned()),
                ("chat.stream", "true".to_owned())
            ]
        );
    }

    #[test]
    fn help_and_mistakes() {
        assert!(chat(&["--help"]).help);
        assert!(chat(&["-h"]).help);
        assert_eq!(
            parse(&words(&["chat", "--wat"])),
            Err(ArgError::UnknownOption("--wat".into()))
        );
        assert_eq!(
            parse(&words(&["chat", "-x"])),
            Err(ArgError::UnknownOption("-x".into()))
        );
        assert_eq!(
            parse(&words(&["chat", "--wat=1"])),
            Err(ArgError::UnknownOption("--wat".into()))
        );
        assert_eq!(
            parse(&words(&["chat", "--model"])),
            Err(ArgError::NeedsValue("--model".into()))
        );
        assert_eq!(
            parse(&words(&["chat", "-p"])),
            Err(ArgError::NeedsValue("-p".into()))
        );
    }

    #[test]
    fn an_empty_inline_value_is_still_a_value() {
        let args = chat(&["--system="]);
        assert_eq!(args.flags, [("chat.system", String::new())]);
    }
}
