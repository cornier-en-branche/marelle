// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The Anthropic Messages wire format.
//!
//! Streamed answers arrive as typed events: `message_start`, then blocks of
//! `content_block_start`, `content_block_delta` and `content_block_stop`,
//! then `message_delta` with the stop reason and `message_stop`. Text and
//! "thinking" blocks are forwarded; other block types are ignored for now.

use std::ops::ControlFlow;

use marelle_core::{Role, ToolCall};
use serde_json::{Map, Value, json};

use crate::chat::{ChatOutcome, ChatRequest, Collector, FinishReason, Sink, Usage};
use crate::config::ProviderConfig;
use crate::decode::{Decode, as_u64, error_text, merge_extra, snippet};
use crate::error::{ProviderError, protocol};
use crate::sse::SseEvent;

/// The API version header value.
pub(crate) const VERSION: &str = "2023-06-01";

/// The Messages API insists on a length limit. This is used when the user
/// sets none.
pub(crate) const DEFAULT_MAX_TOKENS: u64 = 4096;

pub(crate) fn endpoint(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

pub(crate) fn build_body(
    provider: &ProviderConfig,
    request: &ChatRequest,
) -> Result<Value, ProviderError> {
    // The API has no system role inside `messages`. System text from the
    // request and from the conversation goes into one `system` field.
    let mut system: Vec<&str> = request.system.iter().map(String::as_str).collect();
    let mut messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        match message.role {
            Role::System => system.push(&message.content),
            Role::User => messages.push(json!({ "role": "user", "content": message.content })),
            Role::Assistant if message.tool_calls.is_empty() => {
                messages.push(json!({ "role": "assistant", "content": message.content }));
            }
            Role::Assistant => {
                let mut content = Vec::new();
                if !message.content.is_empty() {
                    content.push(json!({ "type": "text", "text": message.content }));
                }
                for call in &message.tool_calls {
                    let input = serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({}));
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input
                    }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            Role::Tool => {
                let result = json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.as_deref().unwrap_or_default(),
                    "content": message.content
                });
                let mut grouped = false;
                if let Some(last) = messages.last_mut() {
                    if last.get("role").and_then(Value::as_str) == Some("user") {
                        if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                            content.push(result.clone());
                            grouped = true;
                        }
                    }
                }
                if !grouped {
                    messages.push(json!({ "role": "user", "content": [result] }));
                }
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".to_owned(), Value::String(request.model.clone()));
    body.insert(
        "max_tokens".to_owned(),
        json!(request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
    );
    body.insert("messages".to_owned(), Value::Array(messages));
    body.insert("stream".to_owned(), Value::Bool(request.stream));
    if !system.is_empty() {
        body.insert("system".to_owned(), Value::String(system.join("\n\n")));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_owned(), json!(temperature));
    }
    if !request.tools.is_empty() {
        body.insert("tools".to_owned(), Value::Array(request.tools.iter().map(|tool| json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.parameters
        })).collect()));
    }
    merge_extra(&mut body, &provider.extra_body);
    Ok(Value::Object(body))
}

fn usage_from(usage: &Value) -> Usage {
    Usage {
        input_tokens: as_u64(usage.get("input_tokens")),
        output_tokens: as_u64(usage.get("output_tokens")),
        cached_input_tokens: as_u64(usage.get("cache_read_input_tokens")),
        reasoning_tokens: None,
    }
}

fn map_stop(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "refusal" => FinishReason::ContentFilter,
        "tool_use" => FinishReason::ToolCalls,
        other => FinishReason::Other(other.to_owned()),
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

    fn text(&mut self, text: Option<&Value>, sink: &mut Sink<'_>) -> ControlFlow<()> {
        match text.and_then(Value::as_str) {
            Some(text) => self.collector.text(text, sink),
            None => ControlFlow::Continue(()),
        }
    }

    fn thinking(&mut self, text: Option<&Value>, sink: &mut Sink<'_>) -> ControlFlow<()> {
        match text.and_then(Value::as_str) {
            Some(text) => self.collector.reasoning(text, sink),
            None => ControlFlow::Continue(()),
        }
    }

    fn apply(
        &mut self,
        value: &Value,
        sink: &mut Sink<'_>,
    ) -> Result<ControlFlow<()>, ProviderError> {
        let flow = match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(usage) = value.pointer("/message/usage") {
                    self.collector.usage.merge(usage_from(usage));
                }
                ControlFlow::Continue(())
            }
            Some("content_block_start") => {
                let block = value.get("content_block");
                match block.and_then(|b| b.get("type")).and_then(Value::as_str) {
                    Some("text") => self.text(block.and_then(|b| b.get("text")), sink),
                    Some("thinking") => self.thinking(block.and_then(|b| b.get("thinking")), sink),
                    _ => ControlFlow::Continue(()),
                }
            }
            Some("content_block_delta") => {
                let delta = value.get("delta");
                match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => self.text(delta.and_then(|d| d.get("text")), sink),
                    Some("thinking_delta") => {
                        self.thinking(delta.and_then(|d| d.get("thinking")), sink)
                    }
                    _ => ControlFlow::Continue(()),
                }
            }
            Some("message_delta") => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.collector.finish = Some(map_stop(reason));
                }
                if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
                    self.collector.usage.merge(usage_from(usage));
                }
                ControlFlow::Continue(())
            }
            Some("message_stop") => {
                self.collector.done = true;
                ControlFlow::Continue(())
            }
            Some("error") => {
                let message = value
                    .get("error")
                    .map(error_text)
                    .unwrap_or_else(|| snippet(&value.to_string()));
                return Err(ProviderError::Api { message });
            }
            // `ping`, `content_block_stop` and event types added later.
            _ => ControlFlow::Continue(()),
        };
        Ok(flow)
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
        if data.is_empty() || data.starts_with(':') {
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
        if value.get("type").and_then(Value::as_str) == Some("error") {
            let message = value
                .get("error")
                .map(error_text)
                .unwrap_or_else(|| snippet(&value.to_string()));
            return Err(ProviderError::Api { message });
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            self.collector.usage.merge(usage_from(usage));
        }
        for block in value
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let flow = match block.get("type").and_then(Value::as_str) {
                Some("text") => self.text(block.get("text"), sink),
                Some("thinking") => self.thinking(block.get("thinking"), sink),
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str);
                    let name = block.get("name").and_then(Value::as_str);
                    if let (Some(id), Some(name)) = (id, name) {
                        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        self.collector.tool_calls.push(ToolCall {
                            id: id.to_owned(),
                            name: name.to_owned(),
                            arguments: input.to_string(),
                        });
                    }
                    ControlFlow::Continue(())
                }
                _ => ControlFlow::Continue(()),
            };
            if flow.is_break() {
                return Ok(self.interrupted());
            }
        }
        if let Some(reason) = value.get("stop_reason").and_then(Value::as_str) {
            self.collector.finish = Some(map_stop(reason));
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
            kind: ProviderKind::Anthropic,
            base_url: "https://example.test".into(),
            api_key_env: None,
            api_key_file: None,
            auth: AuthStyle::XApiKey,
            max_tokens_field: MaxTokensField::MaxTokens,
            stream_usage: true,
            extra_body: Map::new(),
        }
    }

    fn request() -> ChatRequest {
        ChatRequest {
            model: "claude-test".into(),
            system: Some("Be brief.".into()),
            messages: vec![
                Message::new(Role::System, "Answer in French."),
                Message::new(Role::User, "hi"),
                Message::new(Role::Assistant, "salut"),
                Message::new(Role::User, "and?"),
            ],
            max_tokens: None,
            temperature: Some(0.2),
            stream: true,
            tools: Vec::new(),
        }
    }

    fn run(events: &[&str]) -> (Vec<StreamEvent>, Result<ChatOutcome, ProviderError>) {
        let mut seen = Vec::new();
        let mut sink = |event: StreamEvent| {
            seen.push(event);
            ControlFlow::Continue(())
        };
        let mut decoder = Box::new(Decoder::new());
        for data in events {
            let event = SseEvent {
                event: None,
                data: (*data).to_owned(),
            };
            if let Err(error) = decoder.feed(&event, &mut sink) {
                return (seen, Err(error));
            }
        }
        let outcome = decoder.finish();
        (seen, outcome)
    }

    #[test]
    fn the_endpoint_copes_with_and_without_v1() {
        assert_eq!(
            endpoint("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://api.anthropic.com/"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(endpoint("https://x/v1"), "https://x/v1/messages");
        assert_eq!(
            endpoint("https://api.z.ai/api/anthropic"),
            "https://api.z.ai/api/anthropic/v1/messages"
        );
    }

    #[test]
    fn system_text_moves_out_of_the_conversation() {
        let body = build_body(&provider(), &request()).unwrap();
        assert_eq!(body["system"], "Be brief.\n\nAnswer in French.");
        let roles: Vec<_> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
    }

    #[test]
    fn a_length_limit_is_always_sent() {
        let body = build_body(&provider(), &request()).unwrap();
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
        let mut limited = request();
        limited.max_tokens = Some(64);
        assert_eq!(build_body(&provider(), &limited).unwrap()["max_tokens"], 64);
    }

    #[test]
    fn there_is_no_system_field_without_system_text() {
        let mut plain = request();
        plain.system = None;
        plain.messages.remove(0);
        let body = build_body(&provider(), &plain).unwrap();
        assert!(body.get("system").is_none());
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn a_full_stream_is_decoded() {
        let (seen, outcome) = run(&[
            r#"{"type":"message_start","message":{"id":"m","usage":{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":10}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me see"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hel"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lo"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        assert_eq!(
            seen,
            [
                StreamEvent::Reasoning("Let me see".into()),
                StreamEvent::Text("Hel".into()),
                StreamEvent::Text("lo".into())
            ]
        );
        let outcome = outcome.unwrap();
        assert_eq!(outcome.text, "Hello");
        assert_eq!(outcome.finish, FinishReason::Stop);
        assert_eq!(outcome.usage.input_tokens, Some(25));
        assert_eq!(
            outcome.usage.output_tokens,
            Some(15),
            "the later count replaces the first"
        );
        assert_eq!(outcome.usage.cached_input_tokens, Some(10));
    }

    #[test]
    fn stop_reasons_are_mapped() {
        for (reason, expected) in [
            ("max_tokens", FinishReason::Length),
            ("refusal", FinishReason::ContentFilter),
            ("tool_use", FinishReason::ToolCalls),
            ("stop_sequence", FinishReason::Stop),
            ("something_new", FinishReason::Other("something_new".into())),
        ] {
            let delta =
                format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{reason}"}}}}"#);
            let (_, outcome) = run(&[&delta, r#"{"type":"message_stop"}"#]);
            assert_eq!(outcome.unwrap().finish, expected, "{reason}");
        }
    }

    #[test]
    fn an_error_event_is_an_error() {
        let (seen, outcome) = run(&[
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ]);
        assert_eq!(seen, [StreamEvent::Text("x".into())]);
        assert_eq!(
            outcome.unwrap_err(),
            ProviderError::Api {
                message: "Overloaded (overloaded_error)".into()
            }
        );
    }

    #[test]
    fn a_stream_without_message_stop_is_incomplete() {
        let (_, outcome) = run(&[
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#,
        ]);
        assert!(matches!(outcome, Err(ProviderError::Protocol { .. })));
    }

    #[test]
    fn unknown_events_and_blocks_are_skipped() {
        let (seen, outcome) = run(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t","name":"f","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
            r#"{"type":"brand_new_event"}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        assert!(seen.is_empty());
        assert_eq!(outcome.unwrap().text, "");
    }

    #[test]
    fn a_complete_response_is_decoded() {
        let body = br#"{"id":"m","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Hi"},{"type":"text","text":" there"}],"stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":4}}"#;
        let mut seen = Vec::new();
        let mut sink = |event: StreamEvent| {
            seen.push(event);
            ControlFlow::Continue(())
        };
        let outcome = Box::new(Decoder::new()).complete(body, &mut sink).unwrap();
        assert_eq!(outcome.text, "Hi there");
        assert_eq!(outcome.usage.input_tokens, Some(3));
        assert_eq!(outcome.finish, FinishReason::Stop);
        assert_eq!(seen[0], StreamEvent::Reasoning("hm".into()));
    }

    #[test]
    fn a_complete_error_response_is_an_error() {
        let mut sink = |_: StreamEvent| ControlFlow::Continue(());
        let body = br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#;
        let outcome = Box::new(Decoder::new()).complete(body, &mut sink);
        assert_eq!(
            outcome.unwrap_err(),
            ProviderError::Api {
                message: "bad (invalid_request_error)".into()
            }
        );
    }
}
