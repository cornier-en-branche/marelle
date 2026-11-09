// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! `marelle chat`: one question, a pipe, or an interactive session.
//!
//! The answer goes to standard output and nothing else does. The reasoning,
//! the token counts, notes and errors go to standard error, so that
//! `marelle chat ... > answer.txt` captures exactly the answer.

use std::io::{self, Write};
use std::ops::ControlFlow;

use marelle_config::{Config, Expected, SettingError};
use marelle_core::{Message, Role, Session};
use marelle_i18n::{Locale, tr, tr_with};
use marelle_providers::{
    ChatOutcome, ChatRequest, Client, FinishReason, NetworkSettings, ProviderConfig, ProviderError,
    StreamEvent, Usage, normalize_name, provider_names,
};
use tokio::runtime::Runtime;

use crate::Io;
use crate::args::ChatArgs;
use crate::messages::{Failure, provider_failure};
use crate::settings::{build_config, env_lookup};

const OK: u8 = 0;

/// The `chat.*` settings.
struct ChatSettings {
    system: Option<String>,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
    stream: bool,
    show_reasoning: bool,
    show_usage: bool,
}

impl ChatSettings {
    fn from_config(config: &Config) -> Result<Self, ProviderError> {
        let positive = |key: &str| SettingError::new(key, Expected::PositiveWholeNumber);
        let max_tokens = match config
            .get_u64("chat.max_tokens")
            .map_err(|_| positive("chat.max_tokens"))?
        {
            Some(0) => return Err(positive("chat.max_tokens").into()),
            other => other,
        };
        Ok(Self {
            system: config
                .get("chat.system")
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned),
            max_tokens,
            temperature: config.get_f64("chat.temperature")?,
            stream: config.get_bool("chat.stream")?.unwrap_or(true),
            show_reasoning: config.get_bool("chat.show_reasoning")?.unwrap_or(false),
            show_usage: config.get_bool("chat.show_usage")?.unwrap_or(false),
        })
    }
}

/// Everything decided before the first byte is sent.
struct Plan {
    provider_name: String,
    model: String,
    client: Client,
    chat: ChatSettings,
}

/// Chooses the provider and the model, checks the settings, finds the key.
///
/// A model may be written `provider/model`, but only when no provider is
/// chosen otherwise. That keeps names such as OpenRouter's
/// `anthropic/claude-...` intact for anyone who chose a provider.
fn plan(
    config: &Config,
    locale: Locale,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Plan, Failure> {
    let known = provider_names(config);
    let fail = |error: ProviderError| provider_failure(locale, &error, &known);

    let chosen = |key: &str| {
        config
            .get(key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let (provider_name, model) = match (chosen("chat.provider"), chosen("chat.model")) {
        (Some(provider), Some(model)) => (normalize_name(provider), model.to_owned()),
        (Some(_), None) => return Err(Failure::usage(tr(locale, "chat.no_model"))),
        (None, Some(model)) => match model.split_once('/') {
            Some((prefix, rest)) if !rest.is_empty() && known.contains(&normalize_name(prefix)) => {
                (normalize_name(prefix), rest.to_owned())
            }
            _ => return Err(Failure::usage(tr(locale, "chat.no_provider"))),
        },
        (None, None) => return Err(Failure::usage(tr(locale, "chat.no_provider"))),
    };

    let provider = ProviderConfig::from_config(config, &provider_name).map_err(&fail)?;
    let key = provider.api_key_with(&lookup).map_err(&fail)?;
    let network = NetworkSettings::from_config(config, &lookup).map_err(&fail)?;
    let chat = ChatSettings::from_config(config).map_err(&fail)?;
    let client = Client::new(&provider, key, &network).map_err(&fail)?;
    Ok(Plan {
        provider_name,
        model,
        client,
        chat,
    })
}

/// The conversation so far, and what an interactive session may change.
struct Conversation {
    session: Session,
    model: String,
    system: Option<String>,
}

/// Runs the command and returns the exit code.
pub(crate) fn run(
    args: ChatArgs,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let config = build_config(env_vars, &args.flags);
    let plan = match plan(&config, locale, |name| env_lookup(env_vars, name)) {
        Ok(plan) => plan,
        Err(failure) => return failure.print(io.err),
    };

    // A prompt on the command line is asked once. Without one, a terminal
    // gets a session and a pipe is read as the prompt.
    let prompt = if !args.words.is_empty() {
        let piped = if args.words.iter().any(|word| word == "-") {
            match read_stdin(io, locale) {
                Ok(text) => Some(text),
                Err(failure) => return failure.print(io.err),
            }
        } else {
            None
        };
        Some(compose(&args.words, piped.as_deref()))
    } else if io.interactive {
        None
    } else {
        match read_stdin(io, locale) {
            Ok(text) => Some(text.trim().to_owned()),
            Err(failure) => return failure.print(io.err),
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut conversation = Conversation {
        session: Session::new(),
        model: plan.model.clone(),
        system: plan.chat.system.clone(),
    };
    match prompt {
        Some(prompt) => {
            if prompt.trim().is_empty() {
                return Failure::usage(tr(locale, "chat.empty_prompt")).print(io.err);
            }
            conversation.session.push(Message::new(Role::User, prompt));
            turn(&runtime, &plan, &mut conversation, locale, io)
        }
        None => repl(&runtime, &plan, conversation, locale, io),
    }
}

fn read_stdin(io: &mut Io<'_>, locale: Locale) -> Result<String, Failure> {
    let mut text = String::new();
    io.input.read_to_string(&mut text).map_err(|error| {
        Failure::usage(tr_with(
            locale,
            "fail.input",
            &[("reason", &error.to_string())],
        ))
    })?;
    Ok(text)
}

/// Joins the words of the prompt. A lone `-` is replaced by what came in on
/// standard input, set apart by blank lines.
fn compose(words: &[String], piped: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut spoken: Vec<&str> = Vec::new();
    for word in words {
        if word == "-" {
            if !spoken.is_empty() {
                parts.push(spoken.join(" "));
                spoken.clear();
            }
            if let Some(text) = piped {
                parts.push(text.trim_end().to_owned());
            }
        } else {
            spoken.push(word);
        }
    }
    if !spoken.is_empty() {
        parts.push(spoken.join(" "));
    }
    parts.retain(|part| !part.trim().is_empty());
    parts.join("\n\n")
}

/// One exchange: send the conversation, print the answer as it arrives.
///
/// The last message of the session is the user's. It stays only if the
/// model answered, so a failed turn can simply be asked again.
fn turn(
    runtime: &Runtime,
    plan: &Plan,
    conversation: &mut Conversation,
    locale: Locale,
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let request = ChatRequest {
        model: conversation.model.clone(),
        system: conversation.system.clone(),
        messages: conversation.session.messages().to_vec(),
        max_tokens: plan.chat.max_tokens,
        temperature: plan.chat.temperature,
        stream: plan.chat.stream,
    };

    let (result, flushed) = {
        let mut printer = Printer::new(
            &mut *io.out,
            &mut *io.err,
            io.color,
            plan.chat.show_reasoning,
        );
        let result = runtime.block_on(
            plan.client
                .chat(&request, &mut |event: StreamEvent| printer.event(event)),
        );
        (result, printer.finish())
    };
    flushed?;

    match result {
        Ok(outcome) => {
            answered(plan, conversation, outcome, locale, io)?;
            Ok(OK)
        }
        Err(error) => {
            conversation.session.pop();
            provider_failure(locale, &error, &[]).print(io.err)
        }
    }
}

/// Records the answer and says what the user should know about it.
fn answered(
    plan: &Plan,
    conversation: &mut Conversation,
    outcome: ChatOutcome,
    locale: Locale,
    io: &mut Io<'_>,
) -> io::Result<()> {
    if outcome.text.is_empty() {
        // Nothing to keep. Dropping the question keeps the roles alternating.
        conversation.session.pop();
    } else {
        conversation
            .session
            .push(Message::new(Role::Assistant, outcome.text.clone()));
    }

    let note = match &outcome.finish {
        FinishReason::Length => Some("chat.cut.length"),
        FinishReason::ContentFilter => Some("chat.cut.filter"),
        FinishReason::ToolCalls => Some("chat.cut.tools"),
        _ if outcome.text.is_empty() => Some("chat.empty_answer"),
        _ => None,
    };
    if let Some(key) = note {
        writeln!(io.err, "{}", tr(locale, key))?;
    }
    if plan.chat.show_usage && !outcome.usage.is_empty() {
        writeln!(io.err, "{}", usage_line(locale, &outcome.usage))?;
    }
    Ok(())
}

fn usage_line(locale: Locale, usage: &Usage) -> String {
    let mut parts = Vec::new();
    let mut add = |key: &str, count: Option<u64>| {
        if let Some(count) = count {
            parts.push(tr_with(locale, key, &[("n", &count.to_string())]));
        }
    };
    add("chat.usage.in", usage.input_tokens);
    add("chat.usage.out", usage.output_tokens);
    add("chat.usage.cached", usage.cached_input_tokens);
    add("chat.usage.reasoning", usage.reasoning_tokens);
    tr_with(locale, "chat.usage", &[("parts", &parts.join(", "))])
}

/// Writes the pieces of an answer where they belong, as they arrive.
struct Printer<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    color: bool,
    show_reasoning: bool,
    /// A block of reasoning is open on `err`.
    reasoning_open: bool,
    /// Some answer text has been written.
    wrote: bool,
    ends_with_newline: bool,
    /// The first write error. The stream is cut short once there is one.
    failure: Option<io::Error>,
}

impl<'a> Printer<'a> {
    fn new(
        out: &'a mut dyn Write,
        err: &'a mut dyn Write,
        color: bool,
        show_reasoning: bool,
    ) -> Self {
        Self {
            out,
            err,
            color,
            show_reasoning,
            reasoning_open: false,
            wrote: false,
            ends_with_newline: true,
            failure: None,
        }
    }

    fn event(&mut self, event: StreamEvent) -> ControlFlow<()> {
        if self.failure.is_some() {
            return ControlFlow::Break(());
        }
        let result = match event {
            StreamEvent::Text(piece) => self.text(&piece),
            StreamEvent::Reasoning(piece) => self.reasoning(&piece),
        };
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                self.failure = Some(error);
                ControlFlow::Break(())
            }
        }
    }

    fn reasoning(&mut self, piece: &str) -> io::Result<()> {
        if !self.show_reasoning {
            return Ok(());
        }
        if !self.reasoning_open {
            if self.color {
                // Dim, so that it reads as a side note.
                self.err.write_all(b"\x1b[2m")?;
            }
            self.reasoning_open = true;
        }
        self.err.write_all(piece.as_bytes())?;
        self.err.flush()
    }

    fn close_reasoning(&mut self) -> io::Result<()> {
        if self.reasoning_open {
            if self.color {
                self.err.write_all(b"\x1b[0m")?;
            }
            self.err.write_all(b"\n")?;
            self.err.flush()?;
            self.reasoning_open = false;
        }
        Ok(())
    }

    fn text(&mut self, piece: &str) -> io::Result<()> {
        if piece.is_empty() {
            return Ok(());
        }
        self.close_reasoning()?;
        self.out.write_all(piece.as_bytes())?;
        self.out.flush()?;
        self.wrote = true;
        self.ends_with_newline = piece.ends_with('\n');
        Ok(())
    }

    /// Closes what is open and makes sure the answer ends with a newline.
    fn finish(mut self) -> io::Result<()> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        self.close_reasoning()?;
        if self.wrote && !self.ends_with_newline {
            self.out.write_all(b"\n")?;
            self.out.flush()?;
        }
        Ok(())
    }
}

enum Flow {
    Continue,
    Exit,
}

/// The interactive session.
fn repl(
    runtime: &Runtime,
    plan: &Plan,
    mut conversation: Conversation,
    locale: Locale,
    io: &mut Io<'_>,
) -> io::Result<u8> {
    writeln!(
        io.err,
        "{}",
        tr_with(
            locale,
            "repl.banner",
            &[
                ("provider", &plan.provider_name),
                ("model", &conversation.model)
            ]
        )
    )?;
    loop {
        write!(io.err, "> ")?;
        io.err.flush()?;

        let mut line = String::new();
        match io.input.read_line(&mut line) {
            Ok(0) => {
                writeln!(io.err)?;
                return Ok(OK);
            }
            Ok(_) => {}
            Err(error) => {
                let reason = error.to_string();
                return Failure::usage(tr_with(locale, "fail.input", &[("reason", &reason)]))
                    .print(io.err);
            }
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((name, rest)) = slash_command(line) {
            if let Flow::Exit = command(name, rest, &mut conversation, locale, io)? {
                return Ok(OK);
            }
            continue;
        }
        conversation.session.push(Message::new(Role::User, line));
        // A failed turn has already been reported. The session goes on.
        turn(runtime, plan, &mut conversation, locale, io)?;
    }
}

/// Splits `/name rest`. A line that starts with a path, such as
/// `/etc/hosts what is this?`, is a message, not a command.
fn slash_command(line: &str) -> Option<(&str, &str)> {
    let body = line.strip_prefix('/')?;
    let (name, rest) = match body.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (body, ""),
    };
    if name.contains('/') {
        return None;
    }
    Some((name, rest))
}

fn command(
    name: &str,
    rest: &str,
    conversation: &mut Conversation,
    locale: Locale,
    io: &mut Io<'_>,
) -> io::Result<Flow> {
    let say = |io: &mut Io<'_>, key: &str, args: &[(&str, &str)]| {
        writeln!(io.err, "{}", tr_with(locale, key, args))
    };
    match name {
        "exit" | "quit" | "q" => return Ok(Flow::Exit),
        "help" | "?" => say(io, "repl.help", &[])?,
        "clear" => {
            conversation.session.clear();
            say(io, "repl.cleared", &[])?;
        }
        "model" if rest.is_empty() => say(io, "repl.model", &[("model", &conversation.model)])?,
        "model" => {
            conversation.model = rest.to_owned();
            say(io, "repl.model_set", &[("model", rest)])?;
        }
        "system" if rest.is_empty() => match &conversation.system {
            Some(text) => say(io, "repl.system", &[("text", text)])?,
            None => say(io, "repl.system_none", &[])?,
        },
        "system" if rest == "off" => {
            conversation.system = None;
            say(io, "repl.system_cleared", &[])?;
        }
        "system" => {
            conversation.system = Some(rest.to_owned());
            say(io, "repl.system_set", &[])?;
        }
        other => say(io, "repl.unknown", &[("name", &format!("/{other}"))])?,
    }
    Ok(Flow::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_prompt_is_its_words_joined() {
        assert_eq!(
            compose(&words(&["tell", "me", "a", "joke"]), None),
            "tell me a joke"
        );
    }

    #[test]
    fn a_dash_brings_in_standard_input() {
        assert_eq!(
            compose(&words(&["Summarize:", "-"]), Some("line one\nline two\n\n")),
            "Summarize:\n\nline one\nline two"
        );
        assert_eq!(
            compose(&words(&["-", "Explain", "this"]), Some("code")),
            "code\n\nExplain this"
        );
        assert_eq!(
            compose(&words(&["Before", "-", "after"]), Some("middle")),
            "Before\n\nmiddle\n\nafter"
        );
        assert_eq!(
            compose(&words(&["-"]), Some("   \n")),
            "",
            "blank input counts as nothing"
        );
    }

    #[test]
    fn slash_commands_are_told_from_paths() {
        assert_eq!(slash_command("/exit"), Some(("exit", "")));
        assert_eq!(slash_command("/model  glm 4 "), Some(("model", "glm 4")));
        assert_eq!(
            slash_command("/system Be brief."),
            Some(("system", "Be brief."))
        );
        assert_eq!(slash_command("/"), Some(("", "")));
        assert_eq!(slash_command("/etc/hosts what is this?"), None);
        assert_eq!(slash_command("hello /exit"), None);
    }

    #[test]
    fn usage_names_only_what_the_provider_reported() {
        let usage = Usage {
            input_tokens: Some(12),
            output_tokens: Some(34),
            cached_input_tokens: None,
            reasoning_tokens: Some(5),
        };
        assert_eq!(
            usage_line(Locale::En, &usage),
            "tokens: 12 in, 34 out, 5 reasoning"
        );
        assert_eq!(
            usage_line(Locale::Fr, &usage),
            "jetons : 12 en entrée, 34 en sortie, 5 de raisonnement"
        );
    }

    #[test]
    fn the_printer_keeps_the_answer_and_the_reasoning_apart() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut printer = Printer::new(&mut out, &mut err, true, true);
            let _ = printer.event(StreamEvent::Reasoning("hm".into()));
            let _ = printer.event(StreamEvent::Reasoning("mm".into()));
            let _ = printer.event(StreamEvent::Text("Hel".into()));
            let _ = printer.event(StreamEvent::Text("lo".into()));
            printer.finish().unwrap();
        }
        assert_eq!(String::from_utf8(out).unwrap(), "Hello\n");
        assert_eq!(String::from_utf8(err).unwrap(), "\x1b[2mhmmm\x1b[0m\n");
    }

    #[test]
    fn reasoning_is_dropped_unless_asked_for_and_plain_without_colour() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut printer = Printer::new(&mut out, &mut err, false, false);
            let _ = printer.event(StreamEvent::Reasoning("secret thoughts".into()));
            let _ = printer.event(StreamEvent::Text("answer\n".into()));
            printer.finish().unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "answer\n",
            "no second newline"
        );
        assert!(err.is_empty());

        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut printer = Printer::new(&mut out, &mut err, false, true);
            let _ = printer.event(StreamEvent::Reasoning("thinking".into()));
            printer.finish().unwrap();
        }
        assert!(out.is_empty(), "no answer, no newline");
        assert_eq!(String::from_utf8(err).unwrap(), "thinking\n");
    }

    #[test]
    fn a_write_error_stops_the_stream_and_is_reported() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut err = Vec::new();
        let mut broken = Broken;
        let mut printer = Printer::new(&mut broken, &mut err, false, false);
        assert!(printer.event(StreamEvent::Text("a".into())).is_break());
        assert!(printer.event(StreamEvent::Text("b".into())).is_break());
        let error = printer.finish().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
}
