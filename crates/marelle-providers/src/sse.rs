// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! A parser for Server-Sent Events, the framing both provider kinds use for
//! streaming answers.
//!
//! The parser takes bytes in whatever pieces the network delivers them and
//! yields complete events. It follows the WHATWG rules: lines end in `\n`,
//! `\r\n` or `\r`, comment lines start with `:`, several `data:` lines of one
//! event are joined with `\n`, and a blank line ends the event.

use crate::error::{ProviderError, protocol};

/// The longest single line accepted, as a guard against a server that never
/// sends a newline.
const MAX_LINE: usize = 8 * 1024 * 1024;

/// One event from the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, when the server sent one.
    pub event: Option<String>,
    /// The `data:` lines joined with newlines.
    pub data: String,
}

/// Turns a byte stream into events.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
    skip_lf: bool,
    started: bool,
}

impl SseParser {
    /// Creates a parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the next bytes and returns the events they complete.
    ///
    /// Bytes are only decoded once a whole line has arrived, so a multi-byte
    /// character split across two network chunks stays intact.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, ProviderError> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => self.end_line(&mut events),
                b'\r' => {
                    self.end_line(&mut events);
                    self.skip_lf = true;
                }
                _ => {
                    self.line.push(byte);
                    if self.line.len() > MAX_LINE {
                        return Err(protocol("a line in the event stream is too long"));
                    }
                }
            }
        }
        Ok(events)
    }

    /// Ends the stream and returns an event that was still pending because
    /// the server closed the connection without a final blank line.
    pub fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut events);
        }
        self.dispatch(&mut events);
        events
    }

    fn end_line(&mut self, events: &mut Vec<SseEvent>) {
        let bytes = std::mem::take(&mut self.line);
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if !self.started {
            self.started = true;
            if let Some(rest) = text.strip_prefix('\u{feff}') {
                text = rest.to_owned();
            }
        }
        if text.is_empty() {
            self.dispatch(events);
            return;
        }
        if text.starts_with(':') {
            return;
        }
        let (field, value) = match text.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (text.as_str(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
    }

    fn dispatch(&mut self, events: &mut Vec<SseEvent>) {
        let event = self.event.take();
        if let Some(data) = self.data.take() {
            events.push(SseEvent { event, data });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(input: &[u8]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut events = parser.push(input).expect("valid stream");
        events.extend(parser.finish());
        events
    }

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|event| event.data.as_str()).collect()
    }

    #[test]
    fn splits_events_at_blank_lines() {
        let events = parse_all(b"data: one\n\ndata: two\n\n");
        assert_eq!(data(&events), ["one", "two"]);
    }

    #[test]
    fn joins_data_lines_and_keeps_the_event_name() {
        let events = parse_all(b"event: message_start\ndata: a\ndata: b\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[0].data, "a\nb");
    }

    #[test]
    fn the_name_does_not_leak_into_the_next_event() {
        let events = parse_all(b"event: x\ndata: a\n\ndata: b\n\n");
        assert_eq!(events[1].event, None);
    }

    #[test]
    fn handles_every_line_ending() {
        for input in [
            &b"data: a\r\n\r\ndata: b\r\n\r\n"[..],
            b"data: a\r\rdata: b\r\r",
            b"data: a\n\ndata: b\n\n",
        ] {
            assert_eq!(data(&parse_all(input)), ["a", "b"], "{input:?}");
        }
    }

    #[test]
    fn ignores_comments_and_unknown_fields() {
        let events = parse_all(b": keepalive\nid: 7\nretry: 100\ndata: x\n\n");
        assert_eq!(data(&events), ["x"]);
    }

    #[test]
    fn the_space_after_the_colon_is_optional() {
        let events = parse_all(b"data:tight\n\ndata:  two spaces\n\n");
        assert_eq!(data(&events), ["tight", " two spaces"]);
    }

    #[test]
    fn a_bare_data_field_is_an_empty_event() {
        let events = parse_all(b"data\n\n");
        assert_eq!(data(&events), [""]);
    }

    #[test]
    fn skips_a_byte_order_mark() {
        let events = parse_all("\u{feff}data: a\n\n".as_bytes());
        assert_eq!(data(&events), ["a"]);
    }

    #[test]
    fn flushes_a_final_event_without_a_blank_line() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: last").expect("valid").is_empty());
        assert_eq!(data(&parser.finish()), ["last"]);
    }

    #[test]
    fn chunk_boundaries_do_not_matter() {
        let input = "event: a\ndata: привіт, світ\n\ndata: два\r\n\r\ndata: три\n\n".as_bytes();
        let whole = parse_all(input);
        assert_eq!(whole.len(), 3);

        // Split at every possible byte position, including inside characters
        // and between \r and \n.
        for cut in 0..=input.len() {
            let mut parser = SseParser::new();
            let mut events = parser.push(&input[..cut]).expect("valid");
            events.extend(parser.push(&input[cut..]).expect("valid"));
            events.extend(parser.finish());
            assert_eq!(events, whole, "cut at {cut}");
        }

        // And one byte at a time.
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        for byte in input {
            events.extend(parser.push(&[*byte]).expect("valid"));
        }
        events.extend(parser.finish());
        assert_eq!(events, whole);
    }

    #[test]
    fn a_line_that_never_ends_is_refused() {
        let mut parser = SseParser::new();
        let huge = vec![b'a'; MAX_LINE + 1];
        assert!(parser.push(&huge).is_err());
    }
}
