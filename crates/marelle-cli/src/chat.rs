// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! `marelle chat`: one question, a pipe, or an interactive session.
//!
//! The answer goes to standard output and nothing else does. The reasoning,
//! the token counts, notes and errors go to standard error, so that
//! `marelle chat ... > answer.txt` captures exactly the answer.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::ops::ControlFlow;

use marelle_config::{Config, Expected, SettingError};
use marelle_core::{Message, Role, Session, ToolCall};
use marelle_i18n::{Locale, tr, tr_with};
use marelle_providers::{
    ChatOutcome, ChatRequest, Client, FinishReason, NetworkSettings, ProviderConfig, ProviderError,
    StreamEvent, Usage, normalize_name, provider_names,
};
use tokio::runtime::Runtime;
use tokio::task::JoinSet;
use serde_json::{Value, json};

use crate::Io;
use crate::VERSION;
use crate::args::ChatArgs;
use crate::messages::{Failure, config_failure, provider_failure, setting_failure};
use crate::settings::{build_config, env_lookup};
use crate::theme::{self, Role as ThemeRole, Theme};
use crate::tui::{InputAction, SelectorItem, Tui};

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

/// The `ui.*` settings and the palette they select.
#[derive(Debug)]
struct UiSettings {
    theme: Theme,
    banner: bool,
    panel: bool,
    frame: bool,
    spinner: bool,
    width: usize,
}

impl UiSettings {
    fn from_config(config: &Config, columns: Option<&str>) -> Result<Self, SettingError> {
        Ok(Self {
            theme: Theme::from_config(config)?,
            banner: config.get_bool("ui.banner")?.unwrap_or(true),
            panel: config.get_bool("ui.panel")?.unwrap_or(true),
            frame: config.get_bool("ui.frame")?.unwrap_or(true),
            spinner: config.get_bool("ui.spinner")?.unwrap_or(true),
            width: theme::width(config.get_u64("ui.width")?, columns),
        })
    }
}

/// One model, with its own client and the settings that belong to it.
struct Target {
    /// How the model is named in a setting or on the command line.
    reference: String,
    /// What the user sees: the alias, or `provider/model`.
    label: String,
    provider_name: String,
    model: String,
    client: Client,
    system: Option<String>,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
}

/// The fields a `models.<alias>.*` table may override.
struct Alias {
    provider: Option<String>,
    model: String,
    system: Option<String>,
    max_tokens: Option<u64>,
    temperature: Option<f64>,
}

/// Reads a model alias, if `name` names one.
fn alias(config: &Config, name: &str) -> Result<Option<Alias>, SettingError> {
    let key = |field: &str| format!("models.{name}.{field}");
    let Some(model) = config
        .get(&key("model"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let max_tokens = match config.get_u64(&key("max_tokens"))? {
        Some(0) => {
            return Err(SettingError::new(
                key("max_tokens"),
                Expected::PositiveWholeNumber,
            ));
        }
        other => other,
    };
    Ok(Some(Alias {
        provider: config
            .get(&key("provider"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        model,
        system: config
            .get(&key("system"))
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
        max_tokens,
        temperature: config.get_f64(&key("temperature"))?,
    }))
}

/// Turns a name into a client. The name may be an alias, `provider/model`,
/// or a bare model used with the chosen provider.
fn resolve_target(
    config: &Config,
    locale: Locale,
    lookup: &impl Fn(&str) -> Option<String>,
    reference: &str,
    fallback_provider: Option<&str>,
    chat: &ChatSettings,
) -> Result<Target, Failure> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err(Failure::usage(tr(locale, "chat.no_model")));
    }
    let known = provider_names(config);
    let fail = |error: ProviderError| provider_failure(locale, &error, &known);
    let setting = |error: SettingError| setting_failure(locale, &error);

    let alias = alias(config, reference).map_err(setting)?;
    let raw_model = alias
        .as_ref()
        .map_or_else(|| reference.to_owned(), |alias| alias.model.clone());
    let explicit_provider = alias
        .as_ref()
        .and_then(|alias| alias.provider.clone())
        .or_else(|| fallback_provider.map(str::to_owned))
        .or_else(|| {
            config
                .get("chat.provider")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        });
    let (provider_name, model) = match explicit_provider {
        Some(provider) => {
            let provider = normalize_name(&provider);
            // `mock/model` stays `model` once `mock` is the chosen provider,
            // but OpenRouter's `anthropic/claude-...` keeps its slash.
            let model = match raw_model.split_once('/') {
                Some((prefix, rest)) if !rest.is_empty() && normalize_name(prefix) == provider => {
                    rest.to_owned()
                }
                _ => raw_model,
            };
            (provider, model)
        }
        None => match raw_model.split_once('/') {
            Some((prefix, rest)) if !rest.is_empty() && known.contains(&normalize_name(prefix)) => {
                (normalize_name(prefix), rest.to_owned())
            }
            _ => return Err(Failure::usage(tr(locale, "chat.no_provider"))),
        },
    };

    let provider = ProviderConfig::from_config(config, &provider_name).map_err(fail)?;
    let key = provider.api_key_with(lookup).map_err(fail)?;
    let network = NetworkSettings::from_config(config, lookup).map_err(fail)?;
    let client = Client::new(&provider, key, &network).map_err(fail)?;
    Ok(Target {
        reference: reference.to_owned(),
        label: if alias.is_some() {
            reference.to_owned()
        } else {
            format!("{provider_name}/{model}")
        },
        provider_name,
        model,
        client,
        system: alias
            .as_ref()
            .and_then(|alias| alias.system.clone())
            .or_else(|| chat.system.clone()),
        max_tokens: alias
            .as_ref()
            .and_then(|alias| alias.max_tokens)
            .or(chat.max_tokens),
        temperature: alias
            .as_ref()
            .and_then(|alias| alias.temperature)
            .or(chat.temperature),
    })
}

/// Everything decided before the first byte is sent.
struct Plan {
    config: Config,
    /// The model chosen with `chat.model`, absent when only comparing.
    primary: Option<Target>,
    compare: Vec<String>,
    fallback: Vec<String>,
    judge: Option<String>,
    chat: ChatSettings,
    ui: UiSettings,
}

/// Chooses the providers and the models, checks the settings, finds the keys.
///
/// A model may be written `provider/model`, but only when no provider is
/// chosen otherwise. That keeps names such as OpenRouter's
/// `anthropic/claude-...` intact for anyone who chose a provider.
fn plan(
    config: &Config,
    locale: Locale,
    lookup: impl Fn(&str) -> Option<String>,
    allow_no_model: bool,
) -> Result<Plan, Failure> {
    let known = provider_names(config);
    let fail = |error: ProviderError| provider_failure(locale, &error, &known);
    let setting = |error: SettingError| setting_failure(locale, &error);
    let chosen = |key: &str| {
        config
            .get(key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    let chat = ChatSettings::from_config(config).map_err(fail)?;
    let compare = config
        .get_list("chat.compare")
        .map_err(setting)?
        .unwrap_or_default();
    let fallback = config
        .get_list("chat.fallback")
        .map_err(setting)?
        .unwrap_or_default();
    let primary_ref = chosen("chat.model").map(str::to_owned);
    let primary = if allow_no_model {
        None
    } else {
        match &primary_ref {
            Some(reference) => Some(resolve_target(
                config, locale, &lookup, reference, None, &chat,
            )?),
            None if !compare.is_empty() => None,
            None if chosen("chat.provider").is_some() => {
                return Err(Failure::usage(tr(locale, "chat.no_model")));
            }
            None => return Err(Failure::usage(tr(locale, "chat.no_provider"))),
        }
    };
    let ui = UiSettings::from_config(config, lookup("COLUMNS").as_deref()).map_err(setting)?;
    Ok(Plan {
        config: config.clone(),
        primary,
        compare,
        fallback,
        judge: chosen("chat.judge").map(str::to_owned),
        chat,
        ui,
    })
}

/// What to send as the system prompt.
#[derive(Clone)]
enum SystemChoice {
    /// Whatever the model's alias chose, or the global setting.
    Default,
    /// Nothing.
    Off,
    /// This text, whatever the alias says.
    Text(String),
}

impl SystemChoice {
    fn resolve(&self, target: Option<&Target>, global: Option<&str>) -> Option<String> {
        match self {
            SystemChoice::Default => target
                .and_then(|target| target.system.clone())
                .or_else(|| global.map(str::to_owned)),
            SystemChoice::Off => None,
            SystemChoice::Text(text) => Some(text.clone()),
        }
    }
}

/// A conversation stored while another session is active.
#[derive(Clone)]
struct SavedSession {
    name: String,
    session: Session,
    model: String,
    provider_override: Option<String>,
    compare: Vec<String>,
    fallback: Vec<String>,
    judge: Option<String>,
    system: SystemChoice,
}

/// The conversation so far, and what an interactive session may change.
struct Conversation {
    session: Session,
    session_name: String,
    saved_sessions: Vec<SavedSession>,
    next_session_id: usize,
    model: String,
    provider_override: Option<String>,
    compare: Vec<String>,
    fallback: Vec<String>,
    judge: Option<String>,
    system: SystemChoice,
}

fn effective_provider_name_for(
    config: &Config,
    model: &str,
    provider_override: Option<&str>,
    default_provider: Option<&str>,
) -> Option<String> {
    if let Ok(Some(alias)) = alias(config, model) {
        if let Some(provider) = alias.provider {
            return Some(provider);
        }
    }
    provider_override.or(default_provider).map(str::to_owned)
}

fn effective_provider_name(plan: &Plan, conversation: &Conversation) -> Option<String> {
    effective_provider_name_for(
        &plan.config,
        &conversation.model,
        conversation.provider_override.as_deref(),
        plan.primary
            .as_ref()
            .map(|target| target.provider_name.as_str()),
    )
}

impl Conversation {
    fn snapshot(&self) -> SavedSession {
        SavedSession {
            name: self.session_name.clone(),
            session: self.session.clone(),
            model: self.model.clone(),
            provider_override: self.provider_override.clone(),
            compare: self.compare.clone(),
            fallback: self.fallback.clone(),
            judge: self.judge.clone(),
            system: self.system.clone(),
        }
    }

    fn save_current(&mut self) {
        let snapshot = self.snapshot();
        if let Some(existing) = self
            .saved_sessions
            .iter_mut()
            .find(|session| session.name == self.session_name)
        {
            *existing = snapshot;
        } else {
            self.saved_sessions.push(snapshot);
        }
        if self.saved_sessions.len() > 20 {
            self.saved_sessions.remove(0);
        }
    }

    fn new_session(&mut self) {
        self.save_current();
        self.session = Session::new();
        self.session_name = format!("Chat {}", self.next_session_id);
        self.next_session_id += 1;
    }

    fn switch_session(&mut self, name: &str) -> bool {
        if name == self.session_name {
            return true;
        }
        let Some(index) = self
            .saved_sessions
            .iter()
            .position(|session| session.name == name)
        else {
            return false;
        };
        let next = self.saved_sessions.remove(index);
        self.save_current();
        self.session_name = next.name;
        self.session = next.session;
        self.model = next.model;
        self.provider_override = next.provider_override;
        self.compare = next.compare;
        self.fallback = next.fallback;
        self.judge = next.judge;
        self.system = next.system;
        true
    }

    fn session_names(&self) -> Vec<String> {
        let mut names = vec![self.session_name.clone()];
        names.extend(
            self.saved_sessions
                .iter()
                .map(|session| session.name.clone()),
        );
        names
    }

    /// What to show for the models in play. The launch model loses the
    /// provider prefix it was written with, as the panel has its own row.
    fn display_model(&self, plan: &Plan) -> String {
        if !self.compare.is_empty() {
            return self.compare.join(", ");
        }
        if let Ok(Some(alias)) = alias(&plan.config, &self.model) {
            return alias.model;
        }
        match &plan.primary {
            Some(target) if target.reference == self.model => target.model.clone(),
            _ => self.model.clone(),
        }
    }
}

/// The on-disk representation deliberately excludes provider credentials.
/// It contains only conversation messages and the settings needed to resume.
fn encode_message(message: &Message) -> Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    json!({
        "role": role,
        "content": message.content,
        "name": message.name,
        "tool_call_id": message.tool_call_id,
        "tool_calls": message.tool_calls.iter().map(|call| json!({
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments
        })).collect::<Vec<_>>()
    })
}

fn decode_message(value: &Value) -> Option<Message> {
    let role = match value.get("role")?.as_str()? {
        "system" => Role::System,
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => return None,
    };
    let mut message = Message::new(role, value.get("content")?.as_str()?);
    message.name = value.get("name").and_then(Value::as_str).map(str::to_owned);
    message.tool_call_id = value.get("tool_call_id").and_then(Value::as_str).map(str::to_owned);
    message.tool_calls = value
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|call| {
            Some(ToolCall {
                id: call.get("id")?.as_str()?.to_owned(),
                name: call.get("name")?.as_str()?.to_owned(),
                arguments: call.get("arguments")?.as_str()?.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(message)
}

fn encode_system(system: &SystemChoice) -> Value {
    match system {
        SystemChoice::Default => json!({"kind": "default"}),
        SystemChoice::Off => json!({"kind": "off"}),
        SystemChoice::Text(text) => json!({"kind": "text", "value": text}),
    }
}

fn decode_system(value: &Value) -> Option<SystemChoice> {
    match value.get("kind")?.as_str()? {
        "default" => Some(SystemChoice::Default),
        "off" => Some(SystemChoice::Off),
        "text" => Some(SystemChoice::Text(value.get("value")?.as_str()?.to_owned())),
        _ => None,
    }
}

fn encode_saved_session(session: &SavedSession) -> Value {
    json!({
        "name": session.name,
        "messages": session.session.messages().iter().map(encode_message).collect::<Vec<_>>(),
        "model": session.model,
        "provider_override": session.provider_override,
        "compare": session.compare,
        "fallback": session.fallback,
        "judge": session.judge,
        "system": encode_system(&session.system)
    })
}

fn decode_saved_session(value: &Value) -> Option<SavedSession> {
    let mut session = Session::new();
    let messages = value.get("messages")?.as_array()?;
    for message in messages {
        session.push(decode_message(message)?);
    }
    Some(SavedSession {
        name: value.get("name")?.as_str()?.to_owned(),
        session,
        model: value.get("model")?.as_str()?.to_owned(),
        provider_override: value.get("provider_override").and_then(Value::as_str).map(str::to_owned),
        compare: value.get("compare")?.as_array()?.iter()
            .map(|item| item.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?,
        fallback: value.get("fallback")?.as_array()?.iter()
            .map(|item| item.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?,
        judge: value.get("judge").and_then(Value::as_str).map(str::to_owned),
        system: decode_system(value.get("system")?)?,
    })
}

fn session_store_path(env_vars: &[(String, String)]) -> Option<PathBuf> {
    let state = env_lookup(env_vars, "XDG_STATE_HOME")
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env_lookup(env_vars, "HOME")
                .filter(|value| !value.trim().is_empty())
                .map(|home| PathBuf::from(home).join(".local").join("state"))
        })?;
    Some(state.join("marelle").join("sessions.json"))
}

fn sessions_enabled(config: &Config) -> bool {
    config.get_bool("agent.persist_sessions").ok().flatten().unwrap_or(true)
}

fn restore_session_store(
    env_vars: &[(String, String)],
    config: &Config,
    conversation: &mut Conversation,
    continue_session: bool,
) -> io::Result<bool> {
    if !sessions_enabled(config) {
        return Ok(false);
    }
    let Some(path) = session_store_path(env_vars) else {
        return Ok(false);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if text.len() > 32 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "session store exceeds 32 MiB"));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "session store is not valid JSON"))?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported session store version"));
    }
    let active = value.get("active").and_then(decode_saved_session)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "active session is invalid"))?;
    let mut saved = value.get("saved").and_then(Value::as_array)
        .map(|items| items.iter().filter_map(decode_saved_session).collect::<Vec<_>>())
        .unwrap_or_default();
    let next_id = value.get("next_session_id").and_then(Value::as_u64)
        .and_then(|number| usize::try_from(number).ok()).unwrap_or(2).max(2);

    if continue_session {
        conversation.session = active.session;
        conversation.session_name = active.name;
        conversation.model = active.model;
        conversation.provider_override = active.provider_override;
        conversation.compare = active.compare;
        conversation.fallback = active.fallback;
        conversation.judge = active.judge;
        conversation.system = active.system;
        conversation.saved_sessions = saved;
        conversation.next_session_id = next_id;
    } else {
        saved.retain(|session| session.name != active.name);
        if !active.session.is_empty() {
            saved.insert(0, active);
        }
        if saved.len() > 20 {
            saved.truncate(20);
        }
        conversation.saved_sessions = saved;
        conversation.session_name = format!("Chat {next_id}");
        conversation.next_session_id = next_id.saturating_add(1);
    }
    Ok(true)
}

fn persist_session_store(
    env_vars: &[(String, String)],
    config: &Config,
    conversation: &Conversation,
) -> io::Result<()> {
    if !sessions_enabled(config) {
        return Ok(());
    }
    let Some(path) = session_store_path(env_vars) else {
        return Ok(());
    };
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "session store has no parent directory")
    })?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }

    let active = conversation.snapshot();
    let value = json!({
        "version": 1,
        "next_session_id": conversation.next_session_id,
        "active": encode_saved_session(&active),
        "saved": conversation.saved_sessions.iter().map(encode_saved_session).collect::<Vec<_>>()
    });
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    serde_json::to_writer(&mut file, &value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Runs the command and returns the exit code.
pub(crate) fn run(
    args: ChatArgs,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let config = match build_config(env_vars, &args.flags) {
        Ok(config) => config,
        Err(error) => return config_failure(locale, &error).print(io.err),
    };
    let plan = match plan(&config, locale, |name| env_lookup(env_vars, name), args.continue_session) {
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
        session_name: "Chat 1".to_owned(),
        saved_sessions: Vec::new(),
        next_session_id: 2,
        model: plan
            .primary
            .as_ref()
            .map_or_else(String::new, |target| target.reference.clone()),
        provider_override: None,
        compare: plan.compare.clone(),
        fallback: plan.fallback.clone(),
        judge: plan.judge.clone(),
        system: SystemChoice::Default,
    };
    let restored = match restore_session_store(env_vars, &plan.config, &mut conversation, args.continue_session) {
        Ok(restored) => restored,
        Err(error) => {
            writeln!(io.err, "marelle: cannot load saved sessions: {error}")?;
            return Ok(2);
        }
    };
    if args.continue_session && !restored {
        return Failure::usage("No saved Marelle session was found. Start a chat first, or run marelle chat.")
            .print(io.err);
    }
    match prompt {
        Some(prompt) => {
            if prompt.trim().is_empty() {
                return Failure::usage(tr(locale, "chat.empty_prompt")).print(io.err);
            }
            conversation.session.push(Message::new(Role::User, prompt));
            let result = turn(
                &runtime,
                &plan,
                &mut conversation,
                locale,
                env_vars,
                io,
                false,
                None,
            )?;
            persist_session_store(env_vars, &plan.config, &conversation)?;
            Ok(result)
        }
        None => repl(&runtime, &plan, conversation, locale, env_vars, io),
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

/// One exchange. Comparing, when asked for, takes the parallel path.
///
/// The last message of the session is the user's. It stays only if the
/// model answered, so a failed turn can simply be asked again.
fn turn(
    runtime: &Runtime,
    plan: &Plan,
    conversation: &mut Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
    interactive: bool,
    tui: Option<&mut Tui>,
) -> io::Result<u8> {
    if conversation.compare.is_empty() {
        single_turn(
            runtime, plan, conversation, locale, env_vars, io, interactive, tui,
        )
    } else {
        compare_turn(runtime, plan, conversation, locale, env_vars, io)
    }
}

/// Runs the model/tool loop. Tool calls are never executed without passing
/// through Marelle's permission checks.
fn single_turn(
    runtime: &Runtime,
    plan: &Plan,
    conversation: &mut Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
    interactive: bool,
    mut tui: Option<&mut Tui>,
) -> io::Result<u8> {
    let mut attempts = Vec::new();
    if !conversation.model.trim().is_empty() {
        attempts.push(conversation.model.clone());
    }
    attempts.extend(conversation.fallback.iter().cloned());

    let framed = interactive && plan.ui.frame;
    if framed {
        writeln!(io.err, "{}", theme::rule(&plan.ui.theme, plan.ui.width, io.color))?;
    }
    let spinner = interactive && plan.ui.spinner && io.color;
    let provider_hint = effective_provider_name(plan, conversation);
    let known = provider_names(&plan.config);
    let lookup = |name: &str| env_lookup(env_vars, name);
    let max_turns = plan.config.get_u64("agent.max_turns").ok().flatten().unwrap_or(20) as usize;

    if attempts.is_empty() {
        return Failure::usage(tr(locale, "chat.no_model")).print(io.err);
    }

    for (index, reference) in attempts.iter().enumerate() {
        let last_attempt = index + 1 == attempts.len();
        let target = match resolve_target(
            &plan.config, locale, &lookup, reference, provider_hint.as_deref(), &plan.chat,
        ) {
            Ok(target) => target,
            Err(failure) => {
                if last_attempt {
                    if conversation.session.messages().last().is_some_and(|m| m.role == Role::User) {
                        conversation.session.pop();
                    }
                    if framed {
                        writeln!(io.err, "{}", theme::rule(&plan.ui.theme, plan.ui.width, io.color))?;
                    }
                    return failure.print(io.err);
                }
                failure.print(io.err)?;
                writeln!(io.err, "{}", tr_with(locale, "chat.fallback.try", &[
                    ("model", reference), ("next", &attempts[index + 1])
                ]))?;
                continue;
            }
        };

        let mut system = conversation.system.resolve(Some(&target), plan.chat.system.as_deref());
        let project_context = crate::tools::project_context();
        if !project_context.is_empty() {
            system = Some(match system {
                Some(system) if !system.trim().is_empty() => format!("{system}\n\nProject instructions:\n{project_context}"),
                _ => format!("Project instructions:\n{project_context}"),
            });
        }
        let mut tool_rounds = 0usize;
        let mut any_tool_call = false;

        loop {
            let request = ChatRequest {
                model: target.model.clone(),
                system: system.clone(),
                messages: conversation.session.messages().to_vec(),
                max_tokens: target.max_tokens,
                temperature: target.temperature,
                stream: false,
                tools: if tool_rounds < max_turns { crate::tools::definitions() } else { Vec::new() },
            };

            if spinner && !any_tool_call {
                write!(io.err, "{}", tr(locale, "ui.thinking"))?;
                io.err.flush()?;
            }

            // A non-streaming response lets both supported wire protocols
            // return complete, structured tool calls without guessing at
            // partially streamed argument fragments.
            let mut events = Vec::new();
            let result = runtime.block_on(target.client.chat(&request, &mut |event: StreamEvent| {
                events.push(event);
                ControlFlow::Continue(())
            }));
            let waiting = spinner && !any_tool_call;
            let needs_clear = match &result {
                Err(_) => true,
                Ok(outcome) => !outcome.tool_calls.is_empty(),
            };
            if waiting && needs_clear {
                if io.color {
                    write!(io.err, "\r\x1b[2K")?;
                } else {
                    writeln!(io.err)?;
                }
                io.err.flush()?;
            }

            match result {
                Err(error) => {
                    let failure = provider_failure(locale, &error, &known);
                    if !any_tool_call && events.is_empty() && !last_attempt {
                        failure.print(io.err)?;
                        writeln!(io.err, "{}", tr_with(locale, "chat.fallback.try", &[
                            ("model", reference), ("next", &attempts[index + 1])
                        ]))?;
                        break;
                    }
                    if conversation.session.messages().last().is_some_and(|m| m.role == Role::User) {
                        conversation.session.pop();
                    }
                    if framed {
                        writeln!(io.err, "{}", theme::rule(&plan.ui.theme, plan.ui.width, io.color))?;
                    }
                    return failure.print(io.err);
                }
                Ok(outcome) if !outcome.tool_calls.is_empty() => {
                    if tool_rounds >= max_turns {
                        let note = format!("Agent stopped: reached agent.max_turns ({max_turns}).");
                        writeln!(io.err, "{note}")?;
                        conversation.session.push(Message::new(Role::Assistant, note));
                        if framed {
                            writeln!(io.err, "{}", theme::rule(&plan.ui.theme, plan.ui.width, io.color))?;
                        }
                        return Ok(OK);
                    }

                    any_tool_call = true;
                    tool_rounds += 1;
                    conversation.session.push(Message::with_tool_calls(
                        outcome.text,
                        outcome.tool_calls.clone(),
                    ));

                    for call in outcome.tool_calls {
                        writeln!(io.err, "\n[Marelle tool] {}", call.name)?;
                        let mut approve = |label: &str, detail: &str| -> io::Result<bool> {
                            if let Some(ui) = tui.as_deref_mut() {
                                let items = vec![
                                    SelectorItem {
                                        value: "allow".to_owned(),
                                        label: "Allow once".to_owned(),
                                        detail: detail.to_owned(),
                                    },
                                    SelectorItem {
                                        value: "deny".to_owned(),
                                        label: "Deny".to_owned(),
                                        detail: "Do not run this tool call".to_owned(),
                                    },
                                ];
                                let title = format!("{label}?");
                                Ok(ui.select(&title, &items)?.as_deref() == Some("allow"))
                            } else if interactive {
                                writeln!(io.err, "{label}:\n{detail}\nAllow once? [y/N]")?;
                                io.err.flush()?;
                                let mut answer = String::new();
                                io.input.read_line(&mut answer)?;
                                Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
                            } else {
                                Ok(false)
                            }
                        };

                        let result = crate::tools::execute(&call, &plan.config, &mut approve)
                            .unwrap_or_else(|error| format!("Tool error: {error}"));
                        conversation.session.push(Message::tool_result(
                            call.id,
                            call.name,
                            result,
                        ));
                    }
                }
                Ok(outcome) => {
                    let flushed = {
                        let mut printer = Printer::new(
                            &mut *io.out, &mut *io.err, io.color, plan.chat.show_reasoning,
                        )
                        .with_theme(&plan.ui.theme)
                        .with_spinner(spinner && !any_tool_call);
                        for event in events {
                            let _ = printer.event(event);
                        }
                        printer.finish()
                    };
                    flushed?;
                    if framed {
                        writeln!(io.err, "{}", theme::rule(&plan.ui.theme, plan.ui.width, io.color))?;
                    }
                    answered(plan, conversation, outcome, locale, io)?;
                    return Ok(OK);
                }
            }
        }
    }

    Ok(1)
}

/// Asks every compared model at once, then lets a judge merge the answers.
fn compare_turn(
    runtime: &Runtime,
    plan: &Plan,
    conversation: &mut Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let messages = conversation.session.messages().to_vec();
    let lookup = |name: &str| env_lookup(env_vars, name);
    let known = provider_names(&plan.config);
    let references = conversation.compare.clone();
    // The client's future is not `Send` (the encoder and sink stay on one
    // thread), so the requests run on a local set. They still overlap: each
    // request is polled while the others wait for the network.
    let local = tokio::task::LocalSet::new();
    let (mut slots, joined) = runtime.block_on(local.run_until(async {
        let mut slots: Vec<Option<Result<(String, ChatOutcome), Failure>>> =
            (0..references.len()).map(|_| None).collect();
        let mut tasks = JoinSet::new();
        for (index, reference) in references.iter().enumerate() {
            let target = match resolve_target(
                &plan.config,
                locale,
                &lookup,
                reference,
                conversation.provider_override.as_deref(),
                &plan.chat,
            ) {
                Ok(target) => target,
                Err(failure) => {
                    slots[index] = Some(Err(failure));
                    continue;
                }
            };
            let system = conversation
                .system
                .resolve(Some(&target), plan.chat.system.as_deref());
            let request = ChatRequest {
                model: target.model.clone(),
                system,
                messages: messages.clone(),
                max_tokens: target.max_tokens,
                temperature: target.temperature,
                stream: false,
                tools: Vec::new(),
            };
            tasks.spawn_local(async move {
                let mut sink = |_: StreamEvent| ControlFlow::Continue(());
                let result = target.client.chat(&request, &mut sink).await;
                (index, target.label, result)
            });
        }
        let mut joined = Vec::new();
        while let Some(result) = tasks.join_next().await {
            joined.push(result);
        }
        (slots, joined)
    }));
    for result in joined {
        match result {
            Ok((index, label, Ok(outcome))) => slots[index] = Some(Ok((label, outcome))),
            Ok((index, _, Err(error))) => {
                slots[index] = Some(Err(provider_failure(locale, &error, &known)));
            }
            Err(error) => {
                writeln!(
                    io.err,
                    "{}",
                    tr_with(
                        locale,
                        "chat.compare.failed",
                        &[("reason", &error.to_string())]
                    )
                )?;
            }
        }
    }

    let mut answers: Vec<(String, String)> = Vec::new();
    let mut last_code = 1;
    for slot in slots.into_iter().flatten() {
        match slot {
            Err(failure) => {
                last_code = failure.code;
                failure.print(io.err)?;
            }
            Ok((label, outcome)) => {
                writeln!(
                    io.err,
                    "{}",
                    tr_with(locale, "chat.compare.header", &[("label", &label)])
                )?;
                io.out.write_all(outcome.text.as_bytes())?;
                if !outcome.text.is_empty() && !outcome.text.ends_with('\n') {
                    io.out.write_all(b"\n")?;
                }
                io.out.flush()?;
                announce(locale, plan, &outcome, io)?;
                if !outcome.text.trim().is_empty() {
                    answers.push((label, outcome.text));
                }
            }
        }
    }

    if answers.is_empty() {
        conversation.session.pop();
        return Ok(last_code);
    }
    let combined = answers
        .iter()
        .map(|(label, text)| format!("### {label}\n{text}"))
        .collect::<Vec<_>>()
        .join("\n\n");

    if let Some(reference) = conversation.judge.clone() {
        match resolve_target(
            &plan.config,
            locale,
            &lookup,
            &reference,
            conversation.provider_override.as_deref(),
            &plan.chat,
        ) {
            Ok(target) => {
                let question = conversation
                    .session
                    .messages()
                    .iter()
                    .rev()
                    .find(|message| message.role == Role::User)
                    .map_or_else(String::new, |message| message.content.clone());
                let prompt = format!(
                    "Several models answered the same question. Write one final answer \
                     that keeps the best of each. Reply with the answer only.\n\n\
                     Question:\n{question}\n\nAnswers:\n{combined}"
                );
                let system = conversation
                    .system
                    .resolve(Some(&target), plan.chat.system.as_deref());
                let request = ChatRequest {
                    model: target.model.clone(),
                    system,
                    messages: vec![Message::new(Role::User, prompt)],
                    max_tokens: target.max_tokens,
                    temperature: target.temperature,
                    stream: plan.chat.stream,
                    tools: Vec::new(),
                };
                writeln!(
                    io.err,
                    "{}",
                    tr_with(locale, "chat.judge.header", &[("label", &target.label)])
                )?;
                let spinner = plan.ui.spinner && io.color;
                if spinner {
                    write!(io.err, "{}", tr(locale, "ui.thinking"))?;
                    io.err.flush()?;
                }
                let (result, flushed) = {
                    let mut printer = Printer::new(
                        &mut *io.out,
                        &mut *io.err,
                        io.color,
                        plan.chat.show_reasoning,
                    )
                    .with_theme(&plan.ui.theme)
                    .with_spinner(spinner);
                    let result = runtime.block_on(
                        target
                            .client
                            .chat(&request, &mut |event: StreamEvent| printer.event(event)),
                    );
                    (result, printer.finish())
                };
                flushed?;
                match result {
                    Ok(outcome) if !outcome.text.trim().is_empty() => {
                        announce(locale, plan, &outcome, io)?;
                        conversation
                            .session
                            .push(Message::new(Role::Assistant, outcome.text));
                        return Ok(OK);
                    }
                    Ok(outcome) => {
                        announce(locale, plan, &outcome, io)?;
                    }
                    Err(error) => {
                        provider_failure(locale, &error, &known).print(io.err)?;
                    }
                }
            }
            Err(failure) => {
                failure.print(io.err)?;
            }
        }
    }

    conversation
        .session
        .push(Message::new(Role::Assistant, combined));
    Ok(OK)
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
        // Only remove an unanswered user prompt. Tool-call history and its
        // results must remain available for the next turn.
        if conversation.session.messages().last().is_some_and(|message| message.role == Role::User) {
            conversation.session.pop();
        }
    } else {
        conversation
            .session
            .push(Message::new(Role::Assistant, outcome.text.clone()));
    }
    announce(locale, plan, &outcome, io)
}

/// Prints the notes and token counts that go with one finished answer.
fn announce(locale: Locale, plan: &Plan, outcome: &ChatOutcome, io: &mut Io<'_>) -> io::Result<()> {
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
    theme: Theme,
    /// A "waiting" note is on the current line of `err`.
    spinner: bool,
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
            theme: Theme::default(),
            spinner: false,
            reasoning_open: false,
            wrote: false,
            ends_with_newline: true,
            failure: None,
        }
    }

    fn with_theme(mut self, theme: &Theme) -> Self {
        self.theme = theme.clone();
        self
    }

    fn with_spinner(mut self, spinner: bool) -> Self {
        self.spinner = spinner;
        self
    }

    /// Erases the waiting note once something arrives.
    fn clear_spinner(&mut self) -> io::Result<()> {
        if self.spinner {
            self.spinner = false;
            if self.color {
                self.err.write_all(b"\r\x1b[2K")?;
            }
            self.err.flush()?;
        }
        Ok(())
    }

    fn event(&mut self, event: StreamEvent) -> ControlFlow<()> {
        if self.failure.is_some() {
            return ControlFlow::Break(());
        }
        let result = match self.clear_spinner() {
            Ok(()) => match event {
                StreamEvent::Text(piece) => self.text(&piece),
                StreamEvent::Reasoning(piece) => self.reasoning(&piece),
            },
            Err(error) => Err(error),
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
                // Muted, so that it reads as a side note.
                if let Some(color) = self.theme.color(ThemeRole::Reasoning) {
                    self.err.write_all(color.foreground().as_bytes())?;
                }
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
        let painted = self.theme.paint(ThemeRole::Text, piece, self.color);
        self.out.write_all(painted.as_bytes())?;
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
        self.clear_spinner()?;
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
    conversation: Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    if io.interactive {
        let provider = plan.primary.as_ref().map_or_else(
            || tr(locale, "ui.compare"),
            |target| target.provider_name.clone(),
        );
        let model = conversation.display_model(plan);
        if let Ok(mut tui) = Tui::enter(plan.ui.theme.clone(), provider, model, io.color) {
            return repl_tui(runtime, plan, conversation, locale, env_vars, io, &mut tui);
        }
    }

    // A real terminal may not support raw mode or alternate-screen control.
    // Retain the line-oriented REPL as a safe fallback.
    repl_line(runtime, plan, conversation, locale, env_vars, io)
}

fn repl_tui(
    runtime: &Runtime,
    plan: &Plan,
    mut conversation: Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
    tui: &mut Tui,
) -> io::Result<u8> {
    tui.set_session(conversation.session_name.clone())?;
    let (provider, model) = tui_model_labels(plan, &conversation, locale);
    tui.set_model(provider, model)?;
    tui.load_history(conversation.session.messages())?;
    loop {
        match tui.next_action()? {
            InputAction::Continue => {}
            InputAction::OpenPalette => {
                if let Some(command) = tui.select("Command palette", &command_palette_items())? {
                    tui.set_input(format!("/{command} "))?;
                }
            }
            InputAction::Exit => {
                persist_session_store(env_vars, &plan.config, &conversation)?;
                return Ok(OK);
            }
            InputAction::Submit(line) => {
                let model_picker = slash_command(&line)
                    .is_some_and(|(name, rest)| name == "model" && rest.is_empty());
                let provider_picker = slash_command(&line)
                    .is_some_and(|(name, rest)| name == "provider" && rest.is_empty());
                let session_picker = slash_command(&line)
                    .is_some_and(|(name, rest)| name == "session" && rest.is_empty());
                let line = if model_picker {
                    let items = model_selector_items(plan, &conversation);
                    match tui.select("Choose a model", &items)? {
                        Some(value) => format!("/model {value}"),
                        None => continue,
                    }
                } else if provider_picker {
                    let items = provider_selector_items(plan, &conversation);
                    match tui.select("Choose a provider", &items)? {
                        Some(value) => format!("/provider {value}"),
                        None => continue,
                    }
                } else if session_picker {
                    let items = session_selector_items(&conversation);
                    match tui.select("Switch conversation", &items)? {
                        Some(value) => format!("/session {value}"),
                        None => continue,
                    }
                } else {
                    line
                };

                if let Some((name, rest)) = slash_command(&line) {
                    let mut command_output = Vec::new();
                    let mut status_output = Vec::new();
                    let flow = {
                        let mut command_io = Io {
                            input: io.input,
                            out: &mut command_output,
                            err: &mut status_output,
                            interactive: false,
                            color: false,
                        };
                        command(
                            name,
                            rest,
                            plan,
                            &mut conversation,
                            locale,
                            env_vars,
                            &mut command_io,
                        )?
                    };

                    if name == "clear" || (name == "session" && rest == "new") {
                        tui.clear_messages()?;
                    } else if name == "session" {
                        tui.load_history(conversation.session.messages())?;
                    }
                    tui.set_session(conversation.session_name.clone())?;
                    let (provider, model) = tui_model_labels(plan, &conversation, locale);
                    tui.set_model(provider, model)?;
                    tui.push_notice(String::from_utf8_lossy(&status_output).trim().to_owned())?;
                    tui.set_status("Ready")?;

                    persist_session_store(env_vars, &plan.config, &conversation)?;
                    if let Flow::Exit = flow {
                        return Ok(OK);
                    }
                    continue;
                }

                tui.push_user(line.clone())?;
                conversation.session.push(Message::new(Role::User, line));
                tui.start_answer()?;
                let mut answer_output = Vec::new();
                let mut status_output = Vec::new();
                let result = {
                    let mut turn_io = Io {
                        input: io.input,
                        out: &mut answer_output,
                        err: &mut status_output,
                        interactive: false,
                        color: false,
                    };
                    turn(
                        runtime,
                        plan,
                        &mut conversation,
                        locale,
                        env_vars,
                        &mut turn_io,
                        false,
                        Some(tui),
                    )?
                };
                tui.append_answer(&answer_output)?;
                tui.finish_answer()?;
                tui.push_notice(String::from_utf8_lossy(&status_output).trim().to_owned())?;
                tui.set_status(if result == OK {
                    "Ready"
                } else {
                    "Request finished with an error"
                })?;
                persist_session_store(env_vars, &plan.config, &conversation)?;

                let (provider, model) = tui_model_labels(plan, &conversation, locale);
                tui.set_model(provider, model)?;
            }
        }
    }
}

fn command_palette_items() -> Vec<SelectorItem> {
    [
        ("model", "Choose model", "Switch the active model"),
        (
            "provider",
            "Choose provider",
            "Switch the provider for bare model names",
        ),
        (
            "compare",
            "Compare models",
            "Set or inspect parallel model comparison",
        ),
        (
            "fallback",
            "Fallback chain",
            "Set or inspect fallback models",
        ),
        (
            "judge",
            "Judge model",
            "Choose a model to combine comparison results",
        ),
        (
            "system",
            "System prompt",
            "Set, inspect, or disable the system prompt",
        ),
        (
            "session",
            "Switch conversation",
            "Return to an in-memory conversation",
        ),
        (
            "session new",
            "New conversation",
            "Create a fresh in-memory conversation",
        ),
        (
            "clear",
            "Clear conversation",
            "Remove messages from the current conversation",
        ),
        ("help", "Help", "Show available chat commands"),
        ("exit", "Exit", "Leave the interactive session"),
    ]
    .into_iter()
    .map(|(value, label, detail)| SelectorItem {
        value: value.to_owned(),
        label: label.to_owned(),
        detail: detail.to_owned(),
    })
    .collect()
}

fn model_selector_items(plan: &Plan, conversation: &Conversation) -> Vec<SelectorItem> {
    let mut items = Vec::new();
    let current_provider = effective_provider_name(plan, conversation);

    for key in plan.config.keys() {
        let Some(alias_name) = key
            .strip_prefix("models.")
            .and_then(|rest| rest.strip_suffix(".model"))
        else {
            continue;
        };
        let Some(model) = plan
            .config
            .get(key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let provider_key = format!("models.{alias_name}.provider");
        let provider = plan
            .config
            .get(&provider_key)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or(current_provider.as_deref())
            .unwrap_or("provider not set");
        items.push(SelectorItem {
            value: alias_name.to_owned(),
            label: alias_name.to_owned(),
            detail: format!("{provider} / {model}"),
        });
    }

    if !conversation.model.is_empty() && !items.iter().any(|item| item.value == conversation.model)
    {
        items.insert(
            0,
            SelectorItem {
                value: conversation.model.clone(),
                label: format!("Current: {}", conversation.display_model(plan)),
                detail: current_provider.map_or_else(
                    || "current model".to_owned(),
                    |provider| provider.to_owned(),
                ),
            },
        );
    }
    items
}

fn session_selector_items(conversation: &Conversation) -> Vec<SelectorItem> {
    let mut items = vec![SelectorItem {
        value: conversation.session_name.clone(),
        label: format!("{}  (current)", conversation.session_name),
        detail: format!("{} messages", conversation.session.len()),
    }];
    items.extend(
        conversation
            .saved_sessions
            .iter()
            .map(|session| SelectorItem {
                value: session.name.clone(),
                label: session.name.clone(),
                detail: format!("{} messages", session.session.len()),
            }),
    );
    items
}

fn provider_selector_items(plan: &Plan, conversation: &Conversation) -> Vec<SelectorItem> {
    let current = effective_provider_name(plan, conversation);

    provider_names(&plan.config)
        .into_iter()
        .map(|name| {
            let kind = plan
                .config
                .get(&format!("providers.{name}.kind"))
                .unwrap_or("openai-compatible");
            let base_url = plan
                .config
                .get(&format!("providers.{name}.base_url"))
                .unwrap_or("custom endpoint");
            let label = if current
                .as_deref()
                .is_some_and(|value| normalize_name(value) == name)
            {
                format!("{name}  (current)")
            } else {
                name.clone()
            };
            SelectorItem {
                value: name,
                label,
                detail: format!("{kind} · {base_url}"),
            }
        })
        .collect()
}

fn model_for_provider(
    config: &Config,
    reference: &str,
    current_provider: Option<&str>,
) -> Result<String, SettingError> {
    if let Some(alias) = alias(config, reference)? {
        return Ok(alias.model);
    }
    if let Some((provider, model)) = reference.split_once('/') {
        if !model.is_empty()
            && current_provider
                .is_some_and(|current| normalize_name(provider) == normalize_name(current))
        {
            return Ok(model.to_owned());
        }
    }
    Ok(reference.to_owned())
}

fn tui_model_labels(plan: &Plan, conversation: &Conversation, locale: Locale) -> (String, String) {
    let provider = if !conversation.compare.is_empty() {
        tr(locale, "ui.compare")
    } else {
        effective_provider_name(plan, conversation).unwrap_or_else(|| tr(locale, "ui.compare"))
    };
    (provider, conversation.display_model(plan))
}

fn repl_line(
    runtime: &Runtime,
    plan: &Plan,
    mut conversation: Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let theme = &plan.ui.theme;
    let provider_label =
        effective_provider_name(plan, &conversation).unwrap_or_else(|| tr(locale, "ui.compare"));
    if plan.ui.banner {
        write!(
            io.err,
            "{}",
            theme::banner(
                theme,
                VERSION,
                &tr(locale, "ui.tagline"),
                plan.ui.width,
                io.color
            )
        )?;
    }
    if plan.ui.panel {
        let mut rows = vec![
            (tr(locale, "ui.provider"), provider_label.clone()),
            (tr(locale, "ui.model"), conversation.display_model(plan)),
        ];
        if let Some(system) = conversation
            .system
            .resolve(plan.primary.as_ref(), plan.chat.system.as_deref())
        {
            let room = plan.ui.width.saturating_sub(12).max(8);
            rows.push((tr(locale, "ui.system"), shorten(&system, room)));
        }
        write!(
            io.err,
            "{}",
            theme::panel(theme, &rows, plan.ui.width, io.color)
        )?;
    }
    writeln!(
        io.err,
        "{}",
        tr_with(
            locale,
            "repl.banner",
            &[
                ("provider", &provider_label),
                ("model", &conversation.display_model(plan))
            ]
        )
    )?;
    loop {
        write!(io.err, "{}", theme::prompt(theme, io.color))?;
        io.err.flush()?;

        let mut line = String::new();
        match io.input.read_line(&mut line) {
            Ok(0) => {
                writeln!(io.err)?;
                persist_session_store(env_vars, &plan.config, &conversation)?;
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
            let flow = command(name, rest, plan, &mut conversation, locale, env_vars, io)?;
            persist_session_store(env_vars, &plan.config, &conversation)?;
            if let Flow::Exit = flow {
                return Ok(OK);
            }
            continue;
        }
        conversation.session.push(Message::new(Role::User, line));
        // A failed turn has already been reported. The session goes on.
        turn(runtime, plan, &mut conversation, locale, env_vars, io, true, None)?;
        persist_session_store(env_vars, &plan.config, &conversation)?;
    }
}

/// Shortens `text` to `room` characters, ending it with a single ellipsis.
fn shorten(text: &str, room: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= room {
        return flat;
    }
    let mut out: String = flat.chars().take(room.saturating_sub(1)).collect();
    out.push('…');
    out
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
    plan: &Plan,
    conversation: &mut Conversation,
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<Flow> {
    let say = |io: &mut Io<'_>, key: &str, args: &[(&str, &str)]| {
        writeln!(io.err, "{}", tr_with(locale, key, args))
    };
    let lookup = |name: &str| env_lookup(env_vars, name);
    let provider_hint = effective_provider_name(plan, conversation);
    match name {
        "exit" | "quit" | "q" => return Ok(Flow::Exit),
        "help" | "?" => say(io, "repl.help", &[])?,
        "clear" => {
            conversation.session.clear();
            say(io, "repl.cleared", &[])?;
        }
        "provider" if rest.is_empty() => {
            let current = effective_provider_name(plan, conversation)
                .unwrap_or_else(|| "not selected".to_owned());
            writeln!(io.err, "Current provider: {current}")?;
        }
        "session" if rest.is_empty() => {
            writeln!(
                io.err,
                "Conversations: {}",
                conversation.session_names().join(", ")
            )?;
        }
        "session" if rest == "new" => {
            conversation.new_session();
            writeln!(io.err, "Created {}", conversation.session_name)?;
        }
        "session" => {
            if conversation.switch_session(rest) {
                writeln!(io.err, "Switched to {}", conversation.session_name)?;
            } else {
                writeln!(io.err, "No conversation named {rest}")?;
            }
        }
        "provider" if rest == "off" => {
            conversation.provider_override = None;
            writeln!(io.err, "Provider override disabled")?;
        }
        "provider" => {
            let known = provider_names(&plan.config);
            match ProviderConfig::from_config(&plan.config, rest) {
                Ok(provider) => {
                    match model_for_provider(
                        &plan.config,
                        &conversation.model,
                        provider_hint.as_deref(),
                    ) {
                        Ok(model) => {
                            conversation.model = model;
                            conversation.provider_override = Some(provider.name.clone());
                            writeln!(io.err, "Provider set to {}", provider.name)?;
                        }
                        Err(error) => {
                            setting_failure(locale, &error).print(io.err)?;
                        }
                    }
                }
                Err(error) => {
                    provider_failure(locale, &error, &known).print(io.err)?;
                }
            }
        }
        "model" if rest.is_empty() => say(
            io,
            "repl.model",
            &[("model", &conversation.display_model(plan))],
        )?,
        "model" => {
            match resolve_target(
                &plan.config,
                locale,
                &lookup,
                rest,
                provider_hint.as_deref(),
                &plan.chat,
            ) {
                Ok(_) => {
                    conversation.model = rest.to_owned();
                    say(io, "repl.model_set", &[("model", rest)])?;
                }
                Err(failure) => {
                    failure.print(io.err)?;
                }
            }
        }
        "compare" if rest.is_empty() => {
            if conversation.compare.is_empty() {
                say(io, "repl.compare_none", &[])?;
            } else {
                say(
                    io,
                    "repl.compare",
                    &[("models", &conversation.compare.join(", "))],
                )?;
            }
        }
        "compare" if rest == "off" => {
            conversation.compare.clear();
            say(io, "repl.compare_cleared", &[])?;
        }
        "compare" => {
            conversation.compare = list_of(rest);
            say(
                io,
                "repl.compare_set",
                &[("models", &conversation.compare.join(", "))],
            )?;
        }
        "fallback" if rest.is_empty() => {
            if conversation.fallback.is_empty() {
                say(io, "repl.fallback_none", &[])?;
            } else {
                say(
                    io,
                    "repl.fallback",
                    &[("models", &conversation.fallback.join(", "))],
                )?;
            }
        }
        "fallback" if rest == "off" => {
            conversation.fallback.clear();
            say(io, "repl.fallback_cleared", &[])?;
        }
        "fallback" => {
            conversation.fallback = list_of(rest);
            say(
                io,
                "repl.fallback_set",
                &[("models", &conversation.fallback.join(", "))],
            )?;
        }
        "judge" if rest.is_empty() => match &conversation.judge {
            Some(model) => say(io, "repl.judge", &[("model", model)])?,
            None => say(io, "repl.judge_none", &[])?,
        },
        "judge" if rest == "off" => {
            conversation.judge = None;
            say(io, "repl.judge_cleared", &[])?;
        }
        "judge" => {
            match resolve_target(
                &plan.config,
                locale,
                &lookup,
                rest,
                provider_hint.as_deref(),
                &plan.chat,
            ) {
                Ok(_) => {
                    conversation.judge = Some(rest.to_owned());
                    say(io, "repl.judge_set", &[("model", rest)])?;
                }
                Err(failure) => {
                    failure.print(io.err)?;
                }
            }
        }
        "system" if rest.is_empty() => match &conversation.system {
            SystemChoice::Off => say(io, "repl.system_none", &[])?,
            SystemChoice::Text(text) => say(io, "repl.system", &[("text", text)])?,
            SystemChoice::Default => {
                match conversation
                    .system
                    .resolve(plan.primary.as_ref(), plan.chat.system.as_deref())
                {
                    Some(text) => say(io, "repl.system", &[("text", &text)])?,
                    None => say(io, "repl.system_none", &[])?,
                }
            }
        },
        "system" if rest == "off" => {
            conversation.system = SystemChoice::Off;
            say(io, "repl.system_cleared", &[])?;
        }
        "system" => {
            conversation.system = SystemChoice::Text(rest.to_owned());
            say(io, "repl.system_set", &[])?;
        }
        other => say(io, "repl.unknown", &[("name", &format!("/{other}"))])?,
    }
    Ok(Flow::Continue)
}

/// Splits a list written with commas or spaces.
fn list_of(text: &str) -> Vec<String> {
    text.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
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
    fn alias_provider_takes_precedence_when_reporting_active_provider() {
        use marelle_config::Layer;

        let mut config = Config::new();
        config.set(Layer::Default, "models.remote.model", "claude-sonnet");
        config.set(Layer::Default, "models.remote.provider", "anthropic");

        assert_eq!(
            effective_provider_name_for(&config, "remote", Some("openai"), Some("zai")),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            effective_provider_name_for(&config, "bare-model", Some("openai"), Some("zai")),
            Some("openai".to_owned())
        );
        assert_eq!(
            effective_provider_name_for(&config, "bare-model", None, Some("zai")),
            Some("zai".to_owned())
        );
    }

    #[test]
    fn changing_provider_unwraps_aliases_without_breaking_foreign_model_slashes() {
        use marelle_config::Layer;

        let mut config = Config::new();
        config.set(Layer::Default, "models.quick.model", "glm-5.2");

        assert_eq!(
            model_for_provider(&config, "quick", Some("openai")).unwrap(),
            "glm-5.2"
        );
        assert_eq!(
            model_for_provider(&config, "openrouter/anthropic/claude-x", Some("openrouter"))
                .unwrap(),
            "anthropic/claude-x"
        );
        assert_eq!(
            model_for_provider(&config, "anthropic/claude-x", Some("openrouter")).unwrap(),
            "anthropic/claude-x"
        );
    }

    #[test]
    fn switching_named_sessions_preserves_messages_and_model_settings() {
        let mut conversation = Conversation {
            session: Session::new(),
            session_name: "Chat 1".to_owned(),
            saved_sessions: Vec::new(),
            next_session_id: 2,
            model: "quick".to_owned(),
            provider_override: Some("zai".to_owned()),
            compare: Vec::new(),
            fallback: vec!["openai/fallback".to_owned()],
            judge: None,
            system: SystemChoice::Text("Be precise".to_owned()),
        };
        conversation
            .session
            .push(Message::new(Role::User, "first conversation"));

        conversation.new_session();
        assert_eq!(conversation.session_name, "Chat 2");
        assert!(conversation.session.is_empty());
        conversation
            .session
            .push(Message::new(Role::User, "second conversation"));
        conversation.model = "openai/other".to_owned();
        conversation.provider_override = None;

        assert!(conversation.switch_session("Chat 1"));
        assert_eq!(
            conversation.session.messages()[0].content,
            "first conversation"
        );
        assert_eq!(conversation.model, "quick");
        assert_eq!(conversation.provider_override.as_deref(), Some("zai"));
        assert_eq!(conversation.fallback, vec!["openai/fallback".to_owned()]);

        assert!(conversation.switch_session("Chat 2"));
        assert_eq!(
            conversation.session.messages()[0].content,
            "second conversation"
        );
        assert_eq!(conversation.model, "openai/other");
        assert_eq!(conversation.provider_override, None);
        assert!(!conversation.switch_session("missing"));
    }

    #[test]
    fn the_interface_follows_the_settings() {
        use marelle_config::Layer;

        let mut config = Config::new();
        config.set(Layer::Default, "theme.primary", "#112233");
        config.set(Layer::Default, "ui.banner", "false");
        config.set(Layer::Default, "ui.width", "50");
        let ui = UiSettings::from_config(&config, Some("120")).unwrap();
        assert_eq!(ui.width, 50);
        assert!(!ui.banner);
        assert!(ui.frame && ui.spinner);
        assert_eq!(
            ui.theme.color(ThemeRole::Primary),
            theme::parse_hex("#112233")
        );

        config.set(Layer::Default, "theme.primary", "not-a-colour");
        let error = UiSettings::from_config(&config, None).unwrap_err();
        assert_eq!(error.key, "theme.primary");
    }

    #[test]
    fn long_system_prompts_are_shortened_on_one_line() {
        assert_eq!(shorten("be brief", 20), "be brief");
        assert_eq!(shorten("one\n two   three", 20), "one two three");
        assert_eq!(shorten("abcdefghij", 5), "abcd…");
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
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "\x1b[38;2;138;127;149mhmmm\x1b[0m\n"
        );
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
