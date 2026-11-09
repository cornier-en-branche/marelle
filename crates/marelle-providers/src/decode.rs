// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! What the wire formats have in common.

use std::ops::ControlFlow;

use serde_json::{Map, Value};

use crate::chat::{ChatOutcome, Sink};
use crate::error::ProviderError;
use crate::sse::SseEvent;

/// Turns a provider's events into text. There is one implementation per wire
/// format, and the client drives it without knowing which one it is.
pub(crate) trait Decode {
    /// Handles one event of a stream.
    fn feed(
        &mut self,
        event: &SseEvent,
        sink: &mut Sink<'_>,
    ) -> Result<ControlFlow<()>, ProviderError>;

    /// Whether the provider has signalled the end of the answer, so that
    /// nothing more needs to be read.
    fn done(&self) -> bool;

    /// Ends a stream that ran to its end. Fails when the stream stopped
    /// before the answer was complete.
    fn finish(self: Box<Self>) -> Result<ChatOutcome, ProviderError>;

    /// Reads a response that was not streamed.
    fn complete(
        self: Box<Self>,
        body: &[u8],
        sink: &mut Sink<'_>,
    ) -> Result<ChatOutcome, ProviderError>;

    /// Ends the exchange because the sink asked to stop.
    fn interrupted(self: Box<Self>) -> ChatOutcome;
}

/// Adds the user's extra body fields. `messages` and `stream` stay under the
/// client's control, because the rest of the request depends on them.
pub(crate) fn merge_extra(body: &mut Map<String, Value>, extra: &Map<String, Value>) {
    for (key, value) in extra {
        if key != "messages" && key != "stream" {
            body.insert(key.clone(), value.clone());
        }
    }
}

pub(crate) fn as_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

/// The human-readable part of an error: a string as it is, or the `message`
/// of an error object followed by its code or type in parentheses.
pub(crate) fn error_text(error: &Value) -> String {
    match error {
        Value::String(text) => snippet(text),
        Value::Object(map) => {
            let message = ["message", "msg", "detail", "error"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str));
            let code = map
                .get("code")
                .filter(|code| !code.is_null())
                .map(|code| match code {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .or_else(|| map.get("type").and_then(Value::as_str).map(str::to_owned));
            match (message, code) {
                (Some(message), Some(code)) if !message.contains(&code) => {
                    format!("{} ({code})", snippet(message))
                }
                (Some(message), _) => snippet(message),
                (None, _) => snippet(&error.to_string()),
            }
        }
        other => snippet(&other.to_string()),
    }
}

/// The start of a text, on one line.
pub(crate) fn snippet(text: &str) -> String {
    const LIMIT: usize = 300;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut shown: String = one_line.chars().take(LIMIT).collect();
    if one_line.chars().count() > LIMIT {
        shown.push_str("...");
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extra_fields_cannot_replace_the_conversation() {
        let mut body = Map::new();
        body.insert("messages".into(), json!([1]));
        body.insert("stream".into(), json!(true));
        let extra = json!({"messages": [], "stream": false, "thinking": {"type": "disabled"}});
        merge_extra(&mut body, extra.as_object().unwrap());
        assert_eq!(body["messages"], json!([1]));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["thinking"]["type"], "disabled");
    }

    #[test]
    fn error_text_covers_the_common_shapes() {
        // OpenAI and compatible.
        let openai = json!({"message": "Incorrect API key", "type": "invalid_request_error", "code": "invalid_api_key"});
        assert_eq!(error_text(&openai), "Incorrect API key (invalid_api_key)");
        // Anthropic.
        let anthropic = json!({"type": "authentication_error", "message": "invalid x-api-key"});
        assert_eq!(
            error_text(&anthropic),
            "invalid x-api-key (authentication_error)"
        );
        // Zhipu, with a numeric-looking code.
        let zhipu = json!({"code": "1113", "message": "insufficient balance"});
        assert_eq!(error_text(&zhipu), "insufficient balance (1113)");
        // A numeric code.
        let router = json!({"message": "No auth credentials found", "code": 401});
        assert_eq!(error_text(&router), "No auth credentials found (401)");
        // Plain strings and unknown shapes.
        assert_eq!(error_text(&json!("boom")), "boom");
        assert_eq!(error_text(&json!({"weird": 1})), r#"{"weird":1}"#);
        // The code is not repeated when the message already has it.
        assert_eq!(
            error_text(&json!({"message": "error 42", "code": "42"})),
            "error 42"
        );
    }

    #[test]
    fn snippets_stay_short_and_on_one_line() {
        assert_eq!(snippet("a\n  b\tc"), "a b c");
        let long = "x".repeat(400);
        let shown = snippet(&long);
        assert_eq!(shown.chars().count(), 303);
        assert!(shown.ends_with("..."));
    }
}
