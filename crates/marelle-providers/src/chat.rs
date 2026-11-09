// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Provider-neutral request and response types.

use std::ops::ControlFlow;

use marelle_core::Message;

/// One chat request, independent of the provider's wire format.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    /// The model identifier, as the provider spells it.
    pub model: String,
    /// A system prompt, sent ahead of the conversation.
    pub system: Option<String>,
    /// The conversation so far, oldest first.
    pub messages: Vec<Message>,
    /// The longest answer to allow, in tokens.
    pub max_tokens: Option<u64>,
    /// The sampling temperature.
    pub temperature: Option<f64>,
    /// Whether to receive the answer as a stream.
    pub stream: bool,
}

/// A piece of an answer, delivered as it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Part of the answer text.
    Text(String),
    /// Part of the model's reasoning, for models that expose it.
    Reasoning(String),
}

/// The receiver of stream events. Returning [`ControlFlow::Break`] stops the
/// request early, for example when the output pipe has closed.
pub type Sink<'a> = dyn FnMut(StreamEvent) -> ControlFlow<()> + 'a;

/// Token counts reported by the provider. Every field is optional because
/// providers differ in what they report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Tokens in the prompt.
    pub input_tokens: Option<u64>,
    /// Tokens in the answer.
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache.
    pub cached_input_tokens: Option<u64>,
    /// Tokens spent on reasoning, when reported separately.
    pub reasoning_tokens: Option<u64>,
}

impl Usage {
    /// Takes every field that `newer` reports, keeping the rest.
    pub fn merge(&mut self, newer: Usage) {
        self.input_tokens = newer.input_tokens.or(self.input_tokens);
        self.output_tokens = newer.output_tokens.or(self.output_tokens);
        self.cached_input_tokens = newer.cached_input_tokens.or(self.cached_input_tokens);
        self.reasoning_tokens = newer.reasoning_tokens.or(self.reasoning_tokens);
    }

    /// Whether the provider reported nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == Usage::default()
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// The model finished its answer.
    Stop,
    /// The answer hit the length limit.
    Length,
    /// The provider's content filter stopped the answer.
    ContentFilter,
    /// The model asked to call tools.
    ToolCalls,
    /// The caller stopped the request early.
    Interrupted,
    /// Any other reason, as the provider spelled it.
    Other(String),
}

/// The finished answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatOutcome {
    /// The whole answer text. Reasoning is not included.
    pub text: String,
    /// What the provider reported about token use.
    pub usage: Usage,
    /// Why the model stopped.
    pub finish: FinishReason,
}

/// Collects what a decoder sees and forwards events to the sink.
#[derive(Debug, Default)]
pub(crate) struct Collector {
    pub text: String,
    pub usage: Usage,
    pub finish: Option<FinishReason>,
    pub done: bool,
}

impl Collector {
    pub fn text(&mut self, piece: &str, sink: &mut Sink<'_>) -> ControlFlow<()> {
        if piece.is_empty() {
            return ControlFlow::Continue(());
        }
        self.text.push_str(piece);
        sink(StreamEvent::Text(piece.to_owned()))
    }

    pub fn reasoning(&mut self, piece: &str, sink: &mut Sink<'_>) -> ControlFlow<()> {
        if piece.is_empty() {
            return ControlFlow::Continue(());
        }
        sink(StreamEvent::Reasoning(piece.to_owned()))
    }

    pub fn outcome(self, finish: Option<FinishReason>) -> ChatOutcome {
        ChatOutcome {
            text: self.text,
            usage: self.usage,
            finish: finish.or(self.finish).unwrap_or(FinishReason::Stop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_old_values_the_newer_report_lacks() {
        let mut usage = Usage {
            input_tokens: Some(10),
            ..Usage::default()
        };
        usage.merge(Usage {
            output_tokens: Some(5),
            ..Usage::default()
        });
        usage.merge(Usage {
            input_tokens: Some(11),
            ..Usage::default()
        });
        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(5));
        assert!(!usage.is_empty());
        assert!(Usage::default().is_empty());
    }

    #[test]
    fn collector_skips_empty_pieces_and_forwards_the_rest() {
        let mut seen = Vec::new();
        let mut sink = |event: StreamEvent| {
            seen.push(event);
            ControlFlow::Continue(())
        };
        let mut collector = Collector::default();
        let _ = collector.text("", &mut sink);
        let _ = collector.text("ab", &mut sink);
        let _ = collector.reasoning("why", &mut sink);
        let _ = collector.text("c", &mut sink);
        assert_eq!(collector.text, "abc");
        assert_eq!(
            seen,
            [
                StreamEvent::Text("ab".into()),
                StreamEvent::Reasoning("why".into()),
                StreamEvent::Text("c".into())
            ]
        );
    }
}
