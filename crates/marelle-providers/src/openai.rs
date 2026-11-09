// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The chat-completions wire format that OpenAI introduced and many other
//! providers copy: OpenAI, OpenRouter, Ollama, Z.ai and more.
//!
//! Real providers deviate in small ways, and this module absorbs the ones seen
//! in practice: reasoning arrives under `reasoning_content`, `reasoning` or
//! `reasoning_text`, `data: null` and `: keepalive` payloads appear between
//! events, token counts hide in a final chunk with no choices, and some
//! providers end a failed answer with `finish_reason: "error"`.

use std::ops::ControlFlow;

use marelle_core::{Role, ToolCall};
use serde_json::{Map, Value, json};

use crate::chat::{ChatOutcome, ChatRequest, Collector, FinishReason, Sink, Usage};
use crate::config::ProviderConfig;
use crate::decode::{Decode, as_u64, error_text, merge_extra, snippet};
use crate::error::{ProviderError, protocol};
use crate::sse::SseEvent;

/// The fields that may carry the model's reasoning, in order of preference.
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_text"];

pub(crate) fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

pub(crate) fn build_body(
    provider: &ProviderConfig,
    request: &ChatRequest,
) -> Result<Value, ProviderError> {
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    for message in &request.messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut wire = json!({ "role": role, "content": message.content });
        if message.role == Role::Tool {
            wire["tool_call_id"] = json!(message.tool_call_id.as_deref().unwrap_or_default());
            if let Some(name) = &message.name {
                wire["name"] = json!(name);
            }
        }
        if message.role == Role::Assistant && !message.tool_calls.is_empty() {
            wire["tool_calls"] = Value::Array(
                message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments }
                        })
                    })
                    .collect(),
            );
        }
        messages.push(wire);
    }

    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(request.model.clone()));
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("stream".to_owned(), Value::Bool(request.stream));
    if request.stream && provider.stream_usage {
        body.insert(
            "stream_options".to_owned(),
            json!({ "include_usage": true }),
        );
    }
    if let Some(limit) = request.max_tokens {
        body.insert(provider.max_tokens_field.id().to_owned(), json!(limit));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if !request.tools.is_empty() {
        body.insert(
            "tools".to_owned(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.parameters
                            }
                        })
                    })
                    .collect(),
            ),
        );
        body.insert("tool_choice".to_owned(), json!("auto"));
    }
    merge_extra(&mut body, &provider.extra_body);
    Ok(Value::Object(body))
}

fn usage_from(usage: &Value) -> Usage {
    Usage {
        input_tokens: as_u64(usage.get("prompt_tokens")),
        output_tokens: as_u64(usage.get("completion_tokens")),
        cached_input_tokens: as_u64(usage.pointer("/prompt_tokens_details/cached_tokens"))
            .or_else(|| as_u64(usage.get("cached_tokens")))
            .or_else(|| as_u64(usage.get("prompt_cache_hit_tokens"))),
        reasoning_tokens: as_u64(usage.pointer("/completion_tokens_details/reasoning_tokens")),
    }
}

fn map_finish(reason: &str) -> Result<FinishReason, ProviderError> {
    Ok(match reason {
        "stop" | "end" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "content_filter" | "sensitive" => FinishReason::ContentFilter,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "error" | "network_error" => {
            return Err(ProviderError::Api {
                message: format!("the provider ended the answer with finish_reason {reason}"),
            });
        }
        other => FinishReason::Other(other.to_owned()),
    })
}

/// The text of a `content` field: a string, or a list of parts as newer APIs
/// send it.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

pub(crate) struct Decoder {
    collector: Collector,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            collector: Collector::default(),
        }
    }

    fn apply(
        &mut self,
        value: &Value,
        sink: &mut Sink<'_>,
    ) -> Result<ControlFlow<()>, ProviderError> {
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            return Err(ProviderError::Api {
                message: error_text(error),
            });
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            self.collector.usage.merge(usage_from(usage));
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(ControlFlow::Continue(()));
        };

        // Streamed chunks carry a `delta`, complete responses a `message`.
        if let Some(part) = choice.get("delta").or_else(|| choice.get("message")) {
            let reasoning = REASONING_FIELDS
                .iter()
                .filter_map(|field| part.get(*field).and_then(Value::as_str))
                .find(|text| !text.is_empty());
            if let Some(text) = reasoning {
                if self.collector.reasoning(text, sink).is_break() {
                    return Ok(ControlFlow::Break(()));
                }
            }
            let text = content_text(part.get("content"));
            if self.collector.text(&text, sink).is_break() {
                return Ok(ControlFlow::Break(()));
            }
            if let Some(calls) = part.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let Some(id) = call.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(function) = call.get("function") else {
                        continue;
                    };
                    let Some(name) = function.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| function.get("arguments").map(Value::to_string))
                        .unwrap_or_else(|| "{}".to_owned());
                    self.collector.tool_calls.push(ToolCall {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        arguments,
                    });
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.collector.finish = Some(map_finish(reason)?);
        }
        Ok(ControlFlow::Continue(()))
    }
}

impl Decode for Decoder {
    fn feed(
        &mut self,
        event: &SseEvent,
        sink: &mut Sink<'_>,
    ) -> Result<ControlFlow<()>, ProviderError> {
        if self.collector.done {
            return Ok(ControlFlow::Continue(()));
        }
        let data = event.data.trim();
        if event.event.as_deref() == Some("error") {
            let message = serde_json::from_str::<Value>(data)
                .map(|value| error_text(value.get("error").unwrap_or(&value)))
                .unwrap_or_else(|_| snippet(data));
            return Err(ProviderError::Api { message });
        }
        // Empty flushes, `data: null` and `: keepalive` payloads are noise.
        if data.is_empty() || data == "null" || data.starts_with(':') {
            return Ok(ControlFlow::Continue(()));
        }
        if data == "[DONE]" {
            self.collector.done = true;
            return Ok(ControlFlow::Continue(()));
        }
        let value: Value = serde_json::from_str(data).map_err(|_| {
            protocol(format!(
                "a stream event is not valid JSON: {}",
                snippet(data)
            ))
        })?;
        self.apply(&value, sink)
    }

    fn done(&self) -> bool {
        self.collector.done
    }

    fn finish(self: Box<Self>) -> Result<ChatOutcome, ProviderError> {
        if !self.collector.done && self.collector.finish.is_none() {
            return Err(protocol("the stream ended before the answer was complete"));
        }
        Ok(self.collector.outcome(None))
    }

    fn complete(
        mut self: Box<Self>,
        body: &[u8],
        sink: &mut Sink<'_>,
    ) -> Result<ChatOutcome, ProviderError> {
        let value: Value = serde_json::from_slice(body).map_err(|_| {
            protocol(format!(
                "the response is not valid JSON: {}",
                snippet(&String::from_utf8_lossy(body))
            ))
        })?;
        if self.apply(&value, sink)?.is_break() {
            return Ok(self.interrupted());
        }
        Ok(self.collector.outcome(None))
    }

    fn interrupted(self: Box<Self>) -> ChatOutcome {
        self.collector.outcome(Some(FinishReason::Interrupted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::StreamEvent;
    use crate::config::{AuthStyle, MaxTokensField, ProviderKind};
    use marelle_core::Message;

    fn provider() -> ProviderConfig {
        ProviderConfig {
            name: "test".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://example.test/v1".into(),
            api_key_env: None,
            api_key_file: None,
            auth: AuthStyle::Bearer,
            max_tokens_field: MaxTokensField::MaxTokens,
            stream_usage: true,
            extra_body: Map::new(),
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: "glm-test".into(),
            system: Some("Be brief.".into()),
            messages: vec![
                Message::new(Role::User, "hi"),
                Message::new(Role::Assistant, "hello"),
                Message::new(Role::User, "and?"),
            ],
            max_tokens: Some(100),
            temperature: Some(0.5),
            stream: true,
            tools: Vec::new(),
        }
    }

    fn event(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_owned(),
        }
    }

    /// Runs events through a decoder and returns what the sink saw and the outcome.
    fn run(events: &[&str]) -> (Vec<StreamEvent>, Result<ChatOutcome, ProviderError>) {
        let mut seen = Vec::new();
        let mut sink = |event: StreamEvent| {
            seen.push(event);
            ControlFlow::Continue(())
        };
        let mut decoder = Box::new(Decoder::new());
        for data in events {
            if let Err(error) = decoder.feed(&event(data), &mut sink) {
                return (seen, Err(error));
            }
        }
        let outcome = decoder.finish();
        (seen, outcome)
    }

    #[test]
    fn the_endpoint_appends_the_path_once() {
        assert_eq!(endpoint("https://a/v1"), "https://a/v1/chat/completions");
        assert_eq!(endpoint("https://a/v1/"), "https://a/v1/chat/completions");
    }

    #[test]
    fn body_has_the_conversation_and_the_limits() {
        let body = build_body(&provider(), &request()).unwrap();
        assert_eq!(body["model"], "glm-test");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.5);
        let messages = body["messages"].as_array().unwrap();
        let roles: Vec<_> = messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(messages[0]["content"], "Be brief.");
        assert_eq!(messages[3]["content"], "and?");
    }

    #[test]
    fn body_follows_the_provider_settings() {
        let mut settings = provider();
        settings.max_tokens_field = MaxTokensField::MaxCompletionTokens;
        settings.stream_usage = false;
        settings.extra_body = json!({"thinking": {"type": "disabled"}, "stream": false})
            .as_object()
            .unwrap()
            .clone();
        let body = build_body(&settings, &request()).unwrap();
        assert_eq!(body["max_completion_tokens"], 100);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("stream_options").is_none());
        assert_eq!(body["thinking"]["type"], "disabled");
        assert_eq!(
            body["stream"], true,
            "extra fields cannot turn streaming off"
        );
    }

    #[test]
    fn body_leaves_out_what_was_not_asked_for() {
        let mut bare = request();
        bare.system = None;
        bare.max_tokens = None;
        bare.temperature = None;
        bare.stream = false;
        let body = build_body(&provider(), &bare).unwrap();
        for field in [
            "max_tokens",
            "max_completion_tokens",
            "temperature",
            "stream_options",
        ] {
            assert!(body.get(field).is_none(), "{field}");
        }
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn tool_messages_are_supported_and_preserve_call_ids() {
        let mut with_tool = request();
        with_tool.messages.push(Message::new(Role::Tool, "result"));
        with_tool.messages[0] = Message::with_tool_calls(
            "",
            vec![ToolCall {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            }],
        );
        with_tool
            .messages
            .push(Message::tool_result("call-1", "read_file", "contents"));
        let body = build_body(&provider(), &with_tool).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call-1");
        assert_eq!(messages[2]["tool_call_id"], "call-1");
        assert_eq!(messages[2]["content"], "contents");
    }

    #[test]
    fn a_normal_stream_yields_text_usage_and_a_finish_reason() {
        let (seen, outcome) = run(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":8}}}"#,
            "[DONE]",
        ]);
        assert_eq!(
            seen,
            [
                StreamEvent::Text("Hel".into()),
                StreamEvent::Text("lo".into())
            ]
        );
        let outcome = outcome.unwrap();
        assert_eq!(outcome.text, "Hello");
        assert_eq!(outcome.finish, FinishReason::Stop);
        assert_eq!(outcome.usage.input_tokens, Some(12));
        assert_eq!(outcome.usage.output_tokens, Some(3));
        assert_eq!(outcome.usage.cached_input_tokens, Some(8));
    }

    #[test]
    fn reasoning_arrives_under_any_of_its_names_and_is_not_part_of_the_text() {
        for field in REASONING_FIELDS {
            let chunk = format!(r#"{{"choices":[{{"delta":{{"{field}":"think"}}}}]}}"#);
            let (seen, outcome) = run(&[
                &chunk,
                r#"{"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
            ]);
            assert_eq!(
                seen,
                [
                    StreamEvent::Reasoning("think".into()),
                    StreamEvent::Text("ok".into())
                ],
                "{field}"
            );
            assert_eq!(outcome.unwrap().text, "ok");
        }
    }

    #[test]
    fn duplicate_reasoning_fields_are_not_repeated() {
        let (seen, _) = run(&[
            r#"{"choices":[{"delta":{"reasoning_content":"x","reasoning":"x"},"finish_reason":"stop"}]}"#,
        ]);
        assert_eq!(seen, [StreamEvent::Reasoning("x".into())]);
    }

    #[test]
    fn noise_between_events_is_ignored() {
        let (seen, outcome) = run(&[
            "null",
            "",
            ": keepalive",
            r#"{"choices":[{"delta":{"content":"a"},"finish_reason":"stop"}]}"#,
            "[DONE]",
            "null",
        ]);
        assert_eq!(seen, [StreamEvent::Text("a".into())]);
        assert_eq!(outcome.unwrap().text, "a");
    }

    #[test]
    fn content_may_be_a_list_of_parts() {
        let (seen, _) = run(&[
            r#"{"choices":[{"delta":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]},"finish_reason":"stop"}]}"#,
        ]);
        assert_eq!(seen, [StreamEvent::Text("ab".into())]);
    }

    #[test]
    fn finish_reasons_are_mapped() {
        for (reason, expected) in [
            ("length", FinishReason::Length),
            ("content_filter", FinishReason::ContentFilter),
            ("sensitive", FinishReason::ContentFilter),
            ("tool_calls", FinishReason::ToolCalls),
            (
                "model_context_window_exceeded",
                FinishReason::Other("model_context_window_exceeded".into()),
            ),
        ] {
            let chunk = format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{reason}"}}]}}"#);
            let (_, outcome) = run(&[&chunk]);
            assert_eq!(outcome.unwrap().finish, expected, "{reason}");
        }
    }

    #[test]
    fn an_error_finish_reason_is_an_error() {
        let (_, outcome) = run(&[r#"{"choices":[{"delta":{},"finish_reason":"network_error"}]}"#]);
        assert!(matches!(outcome, Err(ProviderError::Api { .. })));
    }

    #[test]
    fn an_error_object_in_the_stream_is_an_error() {
        let (seen, outcome) = run(&[
            r#"{"choices":[{"delta":{"content":"partial"}}]}"#,
            r#"{"error":{"message":"overloaded","code":"1305"}}"#,
        ]);
        assert_eq!(seen, [StreamEvent::Text("partial".into())]);
        assert_eq!(
            outcome.unwrap_err(),
            ProviderError::Api {
                message: "overloaded (1305)".into()
            }
        );
    }

    #[test]
    fn an_error_event_name_is_an_error_too() {
        let mut decoder = Box::new(Decoder::new());
        let mut sink = |_: StreamEvent| ControlFlow::Continue(());
        let bad = SseEvent {
            event: Some("error".into()),
            data: r#"{"error":{"message":"nope"}}"#.into(),
        };
        assert_eq!(
            decoder.feed(&bad, &mut sink).unwrap_err(),
            ProviderError::Api {
                message: "nope".into()
            }
        );
    }

    #[test]
    fn a_stream_that_stops_early_is_not_accepted_as_complete() {
        let (_, outcome) = run(&[r#"{"choices":[{"delta":{"content":"cut off mid"}}]}"#]);
        assert!(matches!(outcome, Err(ProviderError::Protocol { .. })));
    }

    #[test]
    fn done_without_a_finish_reason_counts_as_complete() {
        let (_, outcome) = run(&[r#"{"choices":[{"delta":{"content":"x"}}]}"#, "[DONE]"]);
        assert_eq!(outcome.unwrap().finish, FinishReason::Stop);
    }

    #[test]
    fn garbage_is_a_protocol_error_that_quotes_it_briefly() {
        let (_, outcome) = run(&["<html>502 Bad Gateway</html>"]);
        match outcome {
            Err(ProviderError::Protocol { detail }) => assert!(detail.contains("502 Bad Gateway")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn the_sink_can_stop_the_stream() {
        let mut decoder = Box::new(Decoder::new());
        let mut sink = |_: StreamEvent| ControlFlow::Break(());
        let flow = decoder
            .feed(
                &event(r#"{"choices":[{"delta":{"content":"a"}}]}"#),
                &mut sink,
            )
            .unwrap();
        assert!(flow.is_break());
        let outcome = decoder.interrupted();
        assert_eq!(outcome.finish, FinishReason::Interrupted);
        assert_eq!(outcome.text, "a");
    }

    #[test]
    fn a_complete_response_is_read_like_one_big_chunk() {
        let body = br#"{"choices":[{"message":{"role":"assistant","content":"Hi there","reasoning_content":"hm"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#;
        let mut seen = Vec::new();
        let mut sink = |event: StreamEvent| {
            seen.push(event);
            ControlFlow::Continue(())
        };
        let outcome = Box::new(Decoder::new()).complete(body, &mut sink).unwrap();
        assert_eq!(outcome.text, "Hi there");
        assert_eq!(outcome.usage.output_tokens, Some(2));
        assert_eq!(
            seen,
            [
                StreamEvent::Reasoning("hm".into()),
                StreamEvent::Text("Hi there".into())
            ]
        );
    }

    #[test]
    fn a_complete_response_that_is_not_json_is_a_protocol_error() {
        let mut sink = |_: StreamEvent| ControlFlow::Continue(());
        let outcome = Box::new(Decoder::new()).complete(b"oops", &mut sink);
        assert!(matches!(outcome, Err(ProviderError::Protocol { .. })));
    }
}
