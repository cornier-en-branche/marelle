// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! `marelle chat` from the command line down to the wire, against a mock
//! server. No network and no real key.

use std::io::Cursor;
use std::time::Duration;

use marelle_cli::{Io, run};
use marelle_testkit::{MockServer, Reply};
use serde_json::{Value, json};

const KEY: &str = "sk-test-0123456789abcdef";

struct Outcome {
    code: u8,
    out: String,
    err: String,
}

/// Runs the command line with a fresh environment of exactly these variables.
fn marelle(args: &[&str], env: &[(&str, &str)], stdin: &str, interactive: bool) -> Outcome {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let mut io = Io {
        input: &mut input,
        out: &mut out,
        err: &mut err,
        interactive,
        color: false,
    };
    let code = run(&args, &env, &mut io).expect("writing to memory cannot fail");
    let outcome = Outcome {
        code,
        out: String::from_utf8(out).expect("UTF-8 output"),
        err: String::from_utf8(err).expect("UTF-8 error output"),
    };
    // Whatever happens, the key must not show up anywhere.
    assert!(
        !outcome.out.contains(KEY),
        "the key leaked to stdout:\n{}",
        outcome.out
    );
    assert!(
        !outcome.err.contains(KEY),
        "the key leaked to stderr:\n{}",
        outcome.err
    );
    outcome
}

/// The environment that points a provider called `mock` at the server.
fn env_for<'a>(server: &'a MockServer, extra: &[(&'a str, &'a str)]) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "MARELLE_PROVIDERS__MOCK__BASE_URL".to_owned(),
            server.base_url(),
        ),
        (
            "MARELLE_PROVIDERS__MOCK__API_KEY_ENV".to_owned(),
            "MOCK_KEY".to_owned(),
        ),
        ("MOCK_KEY".to_owned(), KEY.to_owned()),
        ("MARELLE_NETWORK__PROXY".to_owned(), "none".to_owned()),
    ];
    env.extend(
        extra
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    env
}

fn chat(
    server: &MockServer,
    args: &[&str],
    extra_env: &[(&str, &str)],
    stdin: &str,
    interactive: bool,
) -> Outcome {
    let env = env_for(server, extra_env);
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    marelle(args, &env, stdin, interactive)
}

/// A complete non-streamed answer for the agent's tool-capable request path.
fn answer(pieces: &[&str]) -> Reply {
    Reply::json(
        200,
        &json!({
            "choices": [{
                "message": {"content": pieces.concat()},
                "finish_reason": "stop"
            }]
        })
        .to_string(),
    )
}

fn body(server: &MockServer, index: usize) -> Value {
    serde_json::from_str(&server.requests()[index].body).expect("a JSON body")
}

fn contents(body: &Value) -> Vec<(String, String)> {
    const AGENT_GUIDANCE: &str =
        "You are Marelle, a local coding and research assistant.";
    body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|message| {
            let role = message["role"].as_str()?.to_owned();
            let content = message["content"].as_str()?.to_owned();
            if role == "system" {
                if let Some((user_system, _)) =
                    content.split_once(&format!("\\n\\n{AGENT_GUIDANCE}"))
                {
                    if user_system.trim().is_empty() {
                        return None;
                    }
                    return Some((role, user_system.to_owned()));
                }
                if content.starts_with(AGENT_GUIDANCE) {
                    return None;
                }
            }
            Some((role, content))
        })
        .collect()
}

#[test]
fn one_question_writes_the_answer_to_stdout_and_nothing_else() {
    let server = MockServer::start(vec![answer(&["Hel", "lo", " there"])]);
    let result = chat(
        &server,
        &["chat", "-p", "mock", "-m", "glm-x", "Say", "hello"],
        &[],
        "",
        false,
    );

    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "Hello there\n");
    assert_eq!(result.err, "");

    let sent = &server.requests()[0];
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let body = body(&server, 0);
    assert_eq!(body["model"], "glm-x");
    assert_eq!(body["stream"], false);
    assert_eq!(
        contents(&body),
        [("user".to_owned(), "Say hello".to_owned())]
    );
}

#[test]
fn the_model_can_carry_the_provider() {
    let server = MockServer::start(vec![answer(&["ok"])]);
    let result = chat(
        &server,
        &["chat", "--model", "mock/glm-x", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(
        body(&server, 0)["model"],
        "glm-x",
        "the prefix names the provider and is not sent"
    );
}

#[test]
fn a_chosen_provider_keeps_slashes_in_model_names() {
    let server = MockServer::start(vec![answer(&["ok"])]);
    // `anthropic` is also a provider id, and here it is just part of the name.
    let result = chat(
        &server,
        &[
            "chat",
            "--provider",
            "mock",
            "--model",
            "anthropic/claude-x",
            "hi",
        ],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(body(&server, 0)["model"], "anthropic/claude-x");

    let server = MockServer::start(vec![answer(&["ok"])]);
    let result = chat(
        &server,
        &["chat", "--model", "anthropic/claude-x", "hi"],
        &[("MARELLE_CHAT__PROVIDER", "mock")],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(body(&server, 0)["model"], "anthropic/claude-x");
}

#[test]
fn settings_can_come_from_the_environment() {
    let server = MockServer::start(vec![answer(&["ok"])]);
    let result = chat(
        &server,
        &["chat", "hi"],
        &[
            ("MARELLE_CHAT__MODEL", "mock/from-env"),
            ("MARELLE_CHAT__SYSTEM", "Be brief."),
            ("MARELLE_CHAT__MAX_TOKENS", "77"),
            ("MARELLE_CHAT__TEMPERATURE", "0.25"),
        ],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    let body = body(&server, 0);
    assert_eq!(body["model"], "from-env");
    assert_eq!(body["max_tokens"], 77);
    assert_eq!(body["temperature"], 0.25);
    assert_eq!(
        contents(&body)[0],
        ("system".to_owned(), "Be brief.".to_owned())
    );
}

#[test]
fn flags_beat_the_environment() {
    let server = MockServer::start(vec![answer(&["ok"])]);
    let result = chat(
        &server,
        &[
            "chat",
            "-m",
            "mock/from-flag",
            "--max-tokens",
            "5",
            "--system",
            "Flag system",
            "hi",
        ],
        &[
            ("MARELLE_CHAT__MODEL", "mock/from-env"),
            ("MARELLE_CHAT__MAX_TOKENS", "77"),
            ("MARELLE_CHAT__SYSTEM", "Env system"),
        ],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    let body = body(&server, 0);
    assert_eq!(body["model"], "from-flag");
    assert_eq!(body["max_tokens"], 5);
    assert_eq!(contents(&body)[0].1, "Flag system");
}

#[test]
fn without_streaming_the_whole_answer_arrives_at_once() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({"choices": [{"message": {"content": "All at once"}, "finish_reason": "stop"}]})
            .to_string(),
    )]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "--no-stream", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "All at once\n");
    assert_eq!(body(&server, 0)["stream"], false);
}

#[test]
fn a_pipe_is_read_as_the_prompt() {
    let server = MockServer::start(vec![answer(&["fine"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x"],
        &[],
        "What is a lifetime?\nBe short.\n",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "fine\n");
    assert_eq!(
        contents(&body(&server, 0)),
        [(
            "user".to_owned(),
            "What is a lifetime?\nBe short.".to_owned()
        )]
    );
}

#[test]
fn a_dash_puts_the_pipe_into_the_prompt() {
    let server = MockServer::start(vec![answer(&["fine"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "Review this diff:", "-"],
        &[],
        "+ added line\n- removed line\n",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(
        contents(&body(&server, 0))[0].1,
        "Review this diff:\n\n+ added line\n- removed line"
    );
}

#[test]
fn a_prompt_on_the_command_line_does_not_wait_for_the_pipe() {
    // The input holds text, but without a dash it is none of the prompt's business.
    let server = MockServer::start(vec![answer(&["ok"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "just this"],
        &[],
        "ignored input",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(contents(&body(&server, 0))[0].1, "just this");
}

#[test]
fn an_empty_prompt_is_a_usage_error_and_sends_nothing() {
    let server = MockServer::start(vec![answer(&["never"])]);
    let result = chat(&server, &["chat", "-m", "mock/x"], &[], "  \n", false);
    assert_eq!(result.code, 2);
    assert_eq!(result.out, "");
    assert!(result.err.contains("the prompt is empty"), "{}", result.err);
    assert!(server.requests().is_empty());
}

#[test]
fn choosing_nothing_explains_what_to_do() {
    let server = MockServer::start(vec![answer(&["never"])]);

    let result = chat(&server, &["chat", "hi"], &[], "", false);
    assert_eq!(result.code, 2);
    assert!(result.err.contains("No provider chosen"), "{}", result.err);

    let result = chat(
        &server,
        &["chat", "-m", "no-slash-here", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 2);
    assert!(result.err.contains("No provider chosen"), "{}", result.err);

    let result = chat(
        &server,
        &["chat", "-m", "unknownprovider/x", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(
        result.code, 2,
        "an unknown prefix is part of the name, and no provider is chosen"
    );

    let result = chat(&server, &["chat", "-p", "mock", "hi"], &[], "", false);
    assert_eq!(result.code, 2);
    assert!(result.err.contains("No model chosen"), "{}", result.err);
    assert!(server.requests().is_empty());
}

#[test]
fn an_unknown_provider_lists_the_known_ones() {
    let server = MockServer::start(vec![answer(&["never"])]);
    let result = chat(
        &server,
        &["chat", "-p", "nope", "-m", "x", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 2);
    assert!(
        result.err.contains("Unknown provider 'nope'"),
        "{}",
        result.err
    );
    assert!(result.err.contains("mock"), "{}", result.err);
    assert!(result.err.contains("zai"), "{}", result.err);
}

#[test]
fn a_missing_key_names_the_variable_and_sends_nothing() {
    let server = MockServer::start(vec![answer(&["never"])]);
    let env = [
        ("MARELLE_PROVIDERS__MOCK__BASE_URL", server.base_url()),
        (
            "MARELLE_PROVIDERS__MOCK__API_KEY_ENV",
            "MOCK_KEY".to_owned(),
        ),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let result = marelle(&["chat", "-m", "mock/x", "hi"], &env, "", false);
    assert_eq!(result.code, 2);
    assert!(result.err.contains("MOCK_KEY"), "{}", result.err);
    assert!(server.requests().is_empty());
}

#[test]
fn the_key_can_live_in_a_file() {
    let dir = std::env::temp_dir().join(format!("marelle-cli-{}-keyfile", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("mock.key");
    std::fs::write(&file, format!("{KEY}\n")).unwrap();

    let server = MockServer::start(vec![answer(&["ok"])]);
    let env = [
        ("MARELLE_PROVIDERS__MOCK__BASE_URL", server.base_url()),
        (
            "MARELLE_PROVIDERS__MOCK__API_KEY_FILE",
            file.display().to_string(),
        ),
        ("MARELLE_NETWORK__PROXY", "none".to_owned()),
    ];
    let env: Vec<(&str, &str)> = env.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let result = marelle(&["chat", "-m", "mock/x", "hi"], &env, "", false);
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(
        server.requests()[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_key_typed_into_a_setting_is_refused() {
    let server = MockServer::start(vec![answer(&["never"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "hi"],
        &[(
            "MARELLE_PROVIDERS__MOCK__API_KEY",
            "sk-typed-into-the-wrong-place-123456",
        )],
        "",
        false,
    );
    assert_eq!(result.code, 2);
    assert!(
        result.err.contains("providers.mock.api_key"),
        "{}",
        result.err
    );
    assert!(!result.err.contains("sk-typed"), "{}", result.err);
    assert!(server.requests().is_empty());
}

#[test]
fn a_rejected_key_gets_a_clear_message_and_advice() {
    let server = MockServer::start(vec![Reply::json(
        401,
        &json!({"error": {"message": format!("Incorrect API key {KEY}"), "code": "invalid_api_key"}}).to_string(),
    )]);
    let result = chat(&server, &["chat", "-m", "mock/x", "hi"], &[], "", false);
    assert_eq!(result.code, 1);
    assert_eq!(result.out, "");
    assert!(result.err.contains("HTTP 401"), "{}", result.err);
    assert!(
        result.err.contains("Incorrect API key ***"),
        "the key is scrubbed: {}",
        result.err
    );
    assert!(result.err.contains("Check the API key"), "{}", result.err);
}

#[test]
fn a_provider_error_is_reported_without_output() {
    let server = MockServer::start(vec![Reply::json(
        503,
        &json!({"error": {"message": "overloaded"}}).to_string(),
    )]);
    let result = chat(&server, &["chat", "-m", "mock/x", "hi"], &[], "", false);
    assert_eq!(result.code, 1);
    assert_eq!(result.out, "");
    assert!(result.err.contains("overloaded"), "{}", result.err);
}

#[test]
fn a_cut_off_answer_is_called_out() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({
            "choices": [{
                "message": {"content": "Long answ"},
                "finish_reason": "length"
            }]
        })
        .to_string(),
    )]);
    let result = chat(&server, &["chat", "-m", "mock/x", "hi"], &[], "", false);
    assert_eq!(result.code, 0);
    assert_eq!(result.out, "Long answ\n");
    assert!(
        result.err.contains("cut off by the length limit"),
        "{}",
        result.err
    );
    assert!(result.err.contains("--max-tokens"));
}

#[test]
fn an_empty_answer_is_not_silent() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({
            "choices": [{
                "message": {"content": ""},
                "finish_reason": "stop"
            }]
        })
        .to_string(),
    )]);
    let result = chat(&server, &["chat", "-m", "mock/x", "hi"], &[], "", false);
    assert_eq!(result.code, 0);
    assert_eq!(result.out, "");
    assert!(result.err.contains("empty answer"), "{}", result.err);
}

#[test]
fn reasoning_stays_off_stdout_and_appears_only_on_request() {
    let response = || {
        Reply::json(
            200,
            &json!({
                "choices": [{
                    "message": {
                        "reasoning_content": "thinking hard",
                        "content": "42"
                    },
                    "finish_reason": "stop"
                }]
            })
            .to_string(),
        )
    };

    let server = MockServer::start(vec![response()]);
    let quiet = chat(&server, &["chat", "-m", "mock/x", "hi"], &[], "", false);
    assert_eq!(quiet.out, "42\n");
    assert_eq!(quiet.err, "");

    let server = MockServer::start(vec![response()]);
    let loud = chat(
        &server,
        &["chat", "-m", "mock/x", "--show-reasoning", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(loud.out, "42\n");
    assert_eq!(loud.err, "thinking hard\n");
}

#[test]
fn token_counts_are_shown_on_request() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({
            "choices": [{
                "message": {"content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}
        })
        .to_string(),
    )]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "--show-usage", "hi"],
        &[],
        "",
        false,
    );
    assert_eq!(result.out, "ok\n");
    assert_eq!(result.err, "tokens: 12 in, 3 out\n");
}

#[test]
fn messages_follow_the_chosen_language() {
    let server = MockServer::start(vec![answer(&["never"])]);
    for (lang, expected) in [
        ("ru", "Провайдер не выбран"),
        ("fr", "Aucun fournisseur choisi"),
        ("uk", "Провайдера не вибрано"),
        ("en", "No provider chosen"),
    ] {
        let result = chat(&server, &["--lang", lang, "chat", "hi"], &[], "", false);
        assert_eq!(result.code, 2);
        assert!(result.err.contains(expected), "{lang}: {}", result.err);
    }

    let result = chat(
        &server,
        &[
            "--lang",
            "ru",
            "chat",
            "-p",
            "mock",
            "-m",
            "x",
            "--max-tokens",
            "0",
            "hi",
        ],
        &[],
        "",
        false,
    );
    assert_eq!(result.code, 2);
    assert!(result.err.contains("chat.max_tokens"), "{}", result.err);
    assert!(
        result.err.contains("целое число больше нуля"),
        "{}",
        result.err
    );
}

#[test]
fn bad_numbers_name_the_setting() {
    let server = MockServer::start(vec![answer(&["never"])]);
    for (flag, value, key) in [
        ("--max-tokens", "lots", "chat.max_tokens"),
        ("--max-tokens", "0", "chat.max_tokens"),
        ("--temperature", "warm", "chat.temperature"),
    ] {
        let result = chat(
            &server,
            &["chat", "-p", "mock", "-m", "x", flag, value, "hi"],
            &[],
            "",
            false,
        );
        assert_eq!(result.code, 2, "{flag} {value}");
        assert!(result.err.contains(key), "{}", result.err);
        assert!(
            !result.err.contains(&format!("{value}.")),
            "the value is not echoed: {}",
            result.err
        );
    }
    assert!(server.requests().is_empty());
}

#[test]
fn a_session_remembers_the_conversation() {
    let server = MockServer::start(vec![answer(&["first answer"]), answer(&["second answer"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x"],
        &[],
        "hello there\nand then?\n/exit\n",
        true,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "first answer\nsecond answer\n");
    assert!(
        result.err.contains("mock / x"),
        "the banner names the provider and the model: {}",
        result.err
    );

    let second = contents(&body(&server, 1));
    assert_eq!(
        second,
        [
            ("user".to_owned(), "hello there".to_owned()),
            ("assistant".to_owned(), "first answer".to_owned()),
            ("user".to_owned(), "and then?".to_owned()),
        ]
    );
}

#[test]
fn session_commands_change_the_conversation() {
    let server = MockServer::start(vec![answer(&["a"]), answer(&["b"]), answer(&["c"])]);
    let script = [
        "one",
        "/model other-model",
        "/system Answer in French.",
        "two",
        "/clear",
        "three",
        "/model",
        "/system",
        "/system off",
        "/exit",
    ]
    .join("\n");
    let result = chat(
        &server,
        &["chat", "-m", "mock/first-model"],
        &[],
        &format!("{script}\n"),
        true,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "a\nb\nc\n");

    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let first = body(&server, 0);
    assert_eq!(first["model"], "first-model");
    assert_eq!(contents(&first).len(), 1);

    let second = body(&server, 1);
    assert_eq!(second["model"], "other-model");
    assert_eq!(
        contents(&second),
        [
            ("system".to_owned(), "Answer in French.".to_owned()),
            ("user".to_owned(), "one".to_owned()),
            ("assistant".to_owned(), "a".to_owned()),
            ("user".to_owned(), "two".to_owned()),
        ]
    );

    let third = body(&server, 2);
    assert_eq!(
        contents(&third),
        [
            ("system".to_owned(), "Answer in French.".to_owned()),
            ("user".to_owned(), "three".to_owned()),
        ],
        "/clear forgets the conversation and keeps the settings"
    );
    for note in [
        "Model set to other-model.",
        "System prompt set.",
        "Conversation cleared.",
        "Model: other-model",
        "System prompt: Answer in French.",
        "System prompt cleared.",
    ] {
        assert!(
            result.err.contains(note),
            "missing {note:?} in:\n{}",
            result.err
        );
    }
}

#[test]
fn a_failed_turn_can_simply_be_asked_again() {
    let server = MockServer::start(vec![
        Reply::json(500, "upstream trouble"),
        answer(&["recovered"]),
    ]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x"],
        &[],
        "question\nquestion\n/exit\n",
        true,
    );
    assert_eq!(result.code, 0);
    assert_eq!(result.out, "recovered\n");
    assert!(result.err.contains("HTTP 500"), "{}", result.err);
    assert!(result.err.contains("Try again later"), "{}", result.err);

    let retry = contents(&body(&server, 1));
    assert_eq!(
        retry,
        [("user".to_owned(), "question".to_owned())],
        "the failed question was not kept twice"
    );
}

#[test]
fn unknown_session_commands_are_reported_and_paths_are_messages() {
    let server = MockServer::start(vec![answer(&["a"])]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x"],
        &[],
        "/frobnicate\n/etc/hosts what is this?\n/help\n",
        true,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert!(
        result.err.contains("Unknown command /frobnicate"),
        "{}",
        result.err
    );
    assert!(result.err.contains("Commands: /help"), "{}", result.err);
    assert_eq!(contents(&body(&server, 0))[0].1, "/etc/hosts what is this?");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn the_end_of_input_ends_the_session() {
    let server = MockServer::start(vec![answer(&["a"])]);
    let result = chat(&server, &["chat", "-m", "mock/x"], &[], "hello", true);
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "a\n");
}

#[test]
fn an_anthropic_style_provider_works_the_same_way() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({
            "id": "msg_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Bonjour"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 9, "output_tokens": 2}
        })
        .to_string(),
    )]);
    let result = chat(
        &server,
        &[
            "chat",
            "-m",
            "mock/claude-x",
            "--system",
            "Be brief.",
            "--show-usage",
            "hi",
        ],
        &[("MARELLE_PROVIDERS__MOCK__KIND", "anthropic")],
        "",
        false,
    );
    assert_eq!(result.code, 0, "{}", result.err);
    assert_eq!(result.out, "Bonjour\n");
    assert_eq!(result.err, "tokens: 9 in, 2 out\n");
    let sent = &server.requests()[0];
    assert_eq!(sent.path, "/v1/messages");
    assert_eq!(sent.header("x-api-key"), Some(KEY));
    assert!(
        body(&server, 0)["system"]
            .as_str()
            .unwrap()
            .starts_with("Be brief.\\n\\nYou are Marelle"),
    );
}

#[test]
fn a_silent_provider_times_out_with_a_pointer_to_the_setting() {
    let server = MockServer::start(vec![Reply::Stall(Duration::from_secs(4))]);
    let result = chat(
        &server,
        &["chat", "-m", "mock/x", "hi"],
        &[("MARELLE_NETWORK__IDLE_TIMEOUT_SECS", "1")],
        "",
        false,
    );
    assert_eq!(result.code, 1);
    assert!(
        result.err.contains("did not answer in time"),
        "{}",
        result.err
    );
    assert!(
        result.err.contains("network.idle_timeout_secs"),
        "{}",
        result.err
    );
}

#[test]
fn a_key_is_not_sent_over_plain_http_to_another_machine() {
    // 192.0.2.1 is reserved for documentation, so nothing could answer. The
    // refusal must come before any connection is tried.
    let env = [
        ("MARELLE_PROVIDERS__REMOTE__BASE_URL", "http://192.0.2.1/v1"),
        ("MARELLE_PROVIDERS__REMOTE__API_KEY_ENV", "REMOTE_KEY"),
        ("REMOTE_KEY", KEY),
        ("MARELLE_NETWORK__PROXY", "none"),
    ];
    let result = marelle(&["chat", "-m", "remote/x", "hi"], &env, "", false);
    assert_eq!(result.code, 2);
    assert!(
        result
            .err
            .contains("Refusing to send an API key over plain http to 192.0.2.1"),
        "{}",
        result.err
    );
    assert!(
        result.err.contains("network.allow_insecure_http"),
        "{}",
        result.err
    );
}
