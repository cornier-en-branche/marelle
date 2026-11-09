// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The client against a local mock server. No network, no real key.

use std::net::TcpListener;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use marelle_config::{Config, Layer};
use marelle_core::{Message, Role};
use marelle_providers::{
    ChatOutcome, ChatRequest, Client, FinishReason, NetworkSettings, ProviderConfig, ProviderError,
    ProxySetting, Secret, StreamEvent,
};
use marelle_testkit::{MockServer, Reply};
use serde_json::{Value, json};

const KEY: &str = "sk-test-0123456789abcdef";

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(future)
}

fn provider_at(base_url: &str, kind: &str, extra: &[(&str, &str)]) -> ProviderConfig {
    let mut config = Config::new();
    config.set(Layer::Env, "providers.mock.kind", kind);
    config.set(Layer::Env, "providers.mock.base_url", base_url);
    config.set(Layer::Env, "providers.mock.api_key_env", "MOCK_KEY");
    for (field, value) in extra {
        config.set(Layer::Env, format!("providers.mock.{field}"), *value);
    }
    ProviderConfig::from_config(&config, "mock").expect("provider settings")
}

fn provider(server: &MockServer, kind: &str) -> ProviderConfig {
    provider_at(&server.base_url(), kind, &[])
}

fn network() -> NetworkSettings {
    NetworkSettings {
        proxy: ProxySetting::None,
        ..NetworkSettings::default()
    }
}

fn client_with(provider: &ProviderConfig, network: &NetworkSettings) -> Client {
    Client::new(provider, Some(Secret::new(KEY)), network).expect("a client")
}

fn client(provider: &ProviderConfig) -> Client {
    client_with(provider, &network())
}

fn request(stream: bool) -> ChatRequest {
    ChatRequest {
        model: "glm-test".to_owned(),
        system: Some("Be brief.".to_owned()),
        messages: vec![Message::new(Role::User, "Hello")],
        max_tokens: Some(64),
        temperature: None,
        stream,
    }
}

type Seen = Vec<StreamEvent>;

fn ask(client: &Client, request: &ChatRequest) -> (Seen, Result<ChatOutcome, ProviderError>) {
    let mut seen = Vec::new();
    let result = block_on(client.chat(request, &mut |event: StreamEvent| {
        seen.push(event);
        ControlFlow::Continue(())
    }));
    (seen, result)
}

fn data(value: &Value) -> String {
    format!("data: {value}\n\n")
}

fn delta(content: &str) -> String {
    data(&json!({"choices": [{"delta": {"content": content}}]}))
}

fn text(piece: &str) -> StreamEvent {
    StreamEvent::Text(piece.to_owned())
}

fn body_of(server: &MockServer, index: usize) -> Value {
    serde_json::from_str(&server.requests()[index].body).expect("a JSON request body")
}

#[test]
fn an_openai_style_stream_is_read_to_the_end() {
    let server = MockServer::start(vec![Reply::sse([
        delta("Hel"),
        format!(
            "{}{}",
            delta("lo"),
            data(&json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}))
        ),
        data(&json!({"choices": [], "usage": {"prompt_tokens": 9, "completion_tokens": 2}})),
        "data: [DONE]\n\n".to_owned(),
    ])]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );

    assert_eq!(seen, [text("Hel"), text("lo")]);
    let outcome = outcome.expect("an answer");
    assert_eq!(outcome.text, "Hello");
    assert_eq!(outcome.finish, FinishReason::Stop);
    assert_eq!(outcome.usage.input_tokens, Some(9));
    assert_eq!(outcome.usage.output_tokens, Some(2));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/chat/completions");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("accept"), Some("text/event-stream"));
    assert!(
        sent.header("user-agent")
            .is_some_and(|agent| agent.starts_with("marelle/"))
    );
    assert_eq!(sent.header("x-api-key"), None);

    let body = body_of(&server, 0);
    assert_eq!(body["model"], "glm-test");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "Hello");
}

#[test]
fn an_anthropic_stream_is_read_to_the_end() {
    let event = |name: &str, value: Value| format!("event: {name}\ndata: {value}\n\n");
    let server = MockServer::start(vec![Reply::sse([
        event(
            "message_start",
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 12}}}),
        ),
        event(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Bonjour"}}),
        ),
        event("ping", json!({"type": "ping"})),
        event(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 3}}),
        ),
        event("message_stop", json!({"type": "message_stop"})),
    ])]);
    let (seen, outcome) = ask(&client(&provider(&server, "anthropic")), &request(true));

    assert_eq!(seen, [text("Bonjour")]);
    let outcome = outcome.expect("an answer");
    assert_eq!(outcome.text, "Bonjour");
    assert_eq!(outcome.usage.input_tokens, Some(12));
    assert_eq!(outcome.usage.output_tokens, Some(3));

    let sent = &server.requests()[0];
    assert_eq!(sent.path, "/v1/messages");
    assert_eq!(sent.header("x-api-key"), Some(KEY));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(sent.header("authorization"), None);
    let body = body_of(&server, 0);
    assert_eq!(body["system"], "Be brief.");
    assert_eq!(body["max_tokens"], 64);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn events_split_anywhere_by_the_network_still_decode() {
    let whole = format!("{}{}data: [DONE]\n\n", delta("Привіт, "), {
        let mut finish = delta("світе \u{263a}");
        finish.push_str(&data(
            &json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        ));
        finish
    });
    // One chunk per character boundary of the text, so every cut position is
    // exercised across the whole stream.
    let cuts: Vec<usize> = (1..whole.len())
        .filter(|i| whole.is_char_boundary(*i))
        .collect();
    for cut in cuts.into_iter().step_by(7) {
        let (a, b) = whole.split_at(cut);
        let server = MockServer::start(vec![Reply::sse([a, b])]);
        let (seen, outcome) = ask(
            &client(&provider(&server, "openai-compatible")),
            &request(true),
        );
        let outcome = outcome.unwrap_or_else(|error| panic!("cut at {cut}: {error}"));
        assert_eq!(outcome.text, "Привіт, світе \u{263a}", "cut at {cut}");
        let pieces = seen
            .iter()
            .filter(|event| matches!(event, StreamEvent::Text(_)))
            .count();
        assert!(pieces >= 2, "cut at {cut}: only {pieces} text events");
    }
}

#[test]
fn a_complete_answer_is_read_when_streaming_is_off() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({
            "choices": [{"message": {"role": "assistant", "content": "All at once"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 4, "completion_tokens": 3}
        })
        .to_string(),
    )]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(false),
    );
    assert_eq!(seen, [text("All at once")]);
    let outcome = outcome.expect("an answer");
    assert_eq!(outcome.finish, FinishReason::Length);
    assert_eq!(outcome.usage.output_tokens, Some(3));
    assert_eq!(
        server.requests()[0].header("accept"),
        Some("application/json")
    );
    assert_eq!(body_of(&server, 0)["stream"], false);
}

#[test]
fn a_server_that_ignores_stream_true_is_still_understood() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({"choices": [{"message": {"content": "plain"}, "finish_reason": "stop"}]})
            .to_string(),
    )]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    assert_eq!(seen, [text("plain")]);
    assert_eq!(outcome.expect("an answer").text, "plain");
}

#[test]
fn http_errors_carry_the_status_and_the_providers_explanation() {
    let cases = [
        (
            401,
            json!({"error": {"message": "Incorrect API key provided", "code": "invalid_api_key"}}).to_string(),
            "Incorrect API key provided (invalid_api_key)",
        ),
        (429, "slow down, please".to_owned(), "slow down, please"),
        (500, "<html><body>oops</body></html>".to_owned(), "Internal Server Error"),
        (503, String::new(), "Service Unavailable"),
        (
            400,
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "max_tokens is required"}}).to_string(),
            "max_tokens is required (invalid_request_error)",
        ),
    ];
    for (status, body, message) in cases {
        let server = MockServer::start(vec![Reply::json(status, &body)]);
        let (seen, outcome) = ask(
            &client(&provider(&server, "openai-compatible")),
            &request(true),
        );
        assert!(seen.is_empty());
        assert_eq!(
            outcome.unwrap_err(),
            ProviderError::Http {
                status,
                message: message.to_owned()
            }
        );
    }
}

#[test]
fn a_key_echoed_by_the_server_never_reaches_an_error() {
    let echo = json!({"error": {"message": format!("bad key {KEY} for this account")}}).to_string();
    let server = MockServer::start(vec![Reply::json(401, &echo)]);
    let (_, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    let error = outcome.unwrap_err();
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains(KEY), "{shown}");
    }
    assert!(
        error.to_string().contains("bad key *** for this account"),
        "{error}"
    );

    // The same inside a stream.
    let in_stream = data(&json!({"error": {"message": format!("rejected {KEY}")}}));
    let server = MockServer::start(vec![Reply::sse([in_stream])]);
    let (_, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    let error = outcome.unwrap_err();
    assert!(!format!("{error} {error:?}").contains(KEY));
    assert!(matches!(error, ProviderError::Api { .. }));
}

#[test]
fn an_error_in_the_middle_of_a_stream_keeps_what_came_before() {
    let server = MockServer::start(vec![Reply::sse([
        delta("partial "),
        data(&json!({"error": {"message": "overloaded", "code": "1305"}})),
    ])]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    assert_eq!(seen, [text("partial ")]);
    assert_eq!(
        outcome.unwrap_err(),
        ProviderError::Api {
            message: "overloaded (1305)".to_owned()
        }
    );
}

#[test]
fn a_stream_that_ends_early_is_an_error_not_a_short_answer() {
    let server = MockServer::start(vec![Reply::sse([delta("cut off mid")])]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    assert_eq!(seen, [text("cut off mid")]);
    assert!(matches!(
        outcome.unwrap_err(),
        ProviderError::Protocol { .. }
    ));
}

#[test]
fn a_connection_that_dies_mid_stream_is_an_error() {
    let server = MockServer::start(vec![Reply::sse_abort([delta("then silence")])]);
    let (_, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    let error = outcome.unwrap_err();
    assert!(
        matches!(
            error,
            ProviderError::Transport { .. } | ProviderError::Protocol { .. }
        ),
        "{error:?}"
    );
}

#[test]
fn a_silent_provider_times_out() {
    let server = MockServer::start(vec![Reply::Stall(Duration::from_secs(4))]);
    let patient = NetworkSettings {
        idle_timeout: Duration::from_millis(300),
        ..network()
    };
    let started = Instant::now();
    let (_, outcome) = ask(
        &client_with(&provider(&server, "openai-compatible"), &patient),
        &request(true),
    );
    assert_eq!(outcome.unwrap_err(), ProviderError::Timeout);
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "the timeout fired"
    );
}

#[test]
fn a_refused_connection_is_a_transport_error_without_the_key() {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("a port")
        .local_addr()
        .expect("an address")
        .port();
    // The listener is gone, so nothing answers on the port.
    let provider = provider_at(
        &format!("http://127.0.0.1:{port}"),
        "openai-compatible",
        &[],
    );
    let (_, outcome) = ask(&client(&provider), &request(true));
    let error = outcome.unwrap_err();
    match &error {
        ProviderError::Transport { detail } => {
            assert!(!detail.contains(KEY));
            assert!(
                !detail.contains("127.0.0.1"),
                "the URL is left out: {detail}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn redirects_are_never_followed() {
    let elsewhere = MockServer::start(vec![Reply::json(200, "{}")]);
    let origin = MockServer::start(vec![Reply::redirect(
        307,
        &format!("{}/chat/completions", elsewhere.base_url()),
    )]);
    let (_, outcome) = ask(
        &client(&provider(&origin, "openai-compatible")),
        &request(true),
    );
    match outcome.unwrap_err() {
        ProviderError::Http { status, .. } => assert_eq!(status, 307),
        other => panic!("unexpected: {other:?}"),
    }
    assert!(
        elsewhere.requests().is_empty(),
        "the key must not travel to the redirect target"
    );
}

#[test]
fn the_key_is_sent_the_way_the_provider_wants_it() {
    for (auth, name, expected) in [
        ("bearer", "authorization", Some(format!("Bearer {KEY}"))),
        ("x-api-key", "x-api-key", Some(KEY.to_owned())),
        ("api-key", "api-key", Some(KEY.to_owned())),
        ("none", "authorization", None),
        ("none", "x-api-key", None),
        ("none", "api-key", None),
    ] {
        let server = MockServer::start(vec![Reply::json(
            200,
            &json!({"choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]})
                .to_string(),
        )]);
        let provider = provider_at(&server.base_url(), "openai-compatible", &[("auth", auth)]);
        let (_, outcome) = ask(&client(&provider), &request(false));
        outcome.expect("an answer");
        let sent = &server.requests()[0];
        assert_eq!(sent.header(name), expected.as_deref(), "{auth}: {name}");
    }
}

#[test]
fn a_provider_without_a_key_sends_no_credentials() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({"choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]}).to_string(),
    )]);
    let provider = provider_at(&server.base_url(), "openai-compatible", &[]);
    let local = Client::new(&provider, None, &network()).expect("a client");
    let (_, outcome) = ask(&local, &request(false));
    outcome.expect("an answer");
    let sent = &server.requests()[0];
    for name in ["authorization", "x-api-key", "api-key"] {
        assert_eq!(sent.header(name), None, "{name}");
    }
}

#[test]
fn provider_settings_shape_the_request_body() {
    let server = MockServer::start(vec![Reply::sse([
        data(&json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n".to_owned(),
    ])]);
    let provider = provider_at(
        &server.base_url(),
        "openai-compatible",
        &[
            ("max_tokens_field", "max_completion_tokens"),
            ("stream_usage", "false"),
            (
                "extra_body",
                r#"{"thinking":{"type":"disabled"},"stream":false,"messages":[]}"#,
            ),
        ],
    );
    let (_, outcome) = ask(&client(&provider), &request(true));
    outcome.expect("an answer");
    let body = body_of(&server, 0);
    assert_eq!(body["max_completion_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("stream_options").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(
        body["stream"], true,
        "extra fields cannot turn streaming off"
    );
    assert_eq!(
        body["messages"].as_array().map(Vec::len),
        Some(2),
        "nor replace the conversation"
    );
}

#[test]
fn the_sink_can_end_the_request_early() {
    let server = MockServer::start(vec![Reply::sse([
        delta("one "),
        delta("two "),
        delta("three"),
        "data: [DONE]\n\n".to_owned(),
    ])]);
    let client = client(&provider(&server, "openai-compatible"));
    let mut pieces = 0;
    let outcome = block_on(client.chat(&request(true), &mut |_: StreamEvent| {
        pieces += 1;
        if pieces == 2 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }))
    .expect("an outcome");
    assert_eq!(outcome.finish, FinishReason::Interrupted);
    assert_eq!(outcome.text, "one two ");
}

#[test]
fn reasoning_is_delivered_apart_from_the_answer() {
    let server = MockServer::start(vec![Reply::sse([
        data(&json!({"choices": [{"delta": {"reasoning_content": "hmm"}}]})),
        data(&json!({"choices": [{"delta": {"content": "42"}, "finish_reason": "stop"}]})),
        "data: [DONE]\n\n".to_owned(),
    ])]);
    let (seen, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &request(true),
    );
    assert_eq!(seen, [StreamEvent::Reasoning("hmm".to_owned()), text("42")]);
    assert_eq!(outcome.expect("an answer").text, "42");
}

#[test]
fn a_whole_conversation_is_sent_in_order() {
    let server = MockServer::start(vec![Reply::json(
        200,
        &json!({"choices": [{"message": {"content": "ok"}, "finish_reason": "stop"}]}).to_string(),
    )]);
    let mut conversation = request(false);
    conversation.messages = vec![
        Message::new(Role::User, "one"),
        Message::new(Role::Assistant, "two"),
        Message::new(Role::User, "three"),
    ];
    let (_, outcome) = ask(
        &client(&provider(&server, "openai-compatible")),
        &conversation,
    );
    outcome.expect("an answer");
    let body = body_of(&server, 0);
    let contents: Vec<_> = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["content"].as_str().expect("text").to_owned())
        .collect();
    assert_eq!(contents, ["Be brief.", "one", "two", "three"]);
}
