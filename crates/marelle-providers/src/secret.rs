// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

use std::fmt;

/// An API key.
///
/// `Debug` prints no content and there is no `Display`, so a key cannot reach
/// a log or an error message by accident. [`Secret::expose`] is the only way
/// to read it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

/// Keys shorter than this are not scrubbed from text: replacing a few letters
/// would garble messages, and no real key is that short.
const MIN_SCRUBBED_LEN: usize = 8;

impl Secret {
    /// Wraps a key.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The key itself. Use it only to build a request.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Replaces every occurrence of the key in `text` with `***`.
    pub fn scrub(&self, text: &str) -> String {
        if self.0.len() < MIN_SCRUBBED_LEN {
            return text.to_owned();
        }
        text.replace(&self.0, "***")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_key() {
        let secret = Secret::new("sk-very-secret-value");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(format!("{:?}", Some(&secret)), "Some(Secret(***))");
        assert_eq!(secret.expose(), "sk-very-secret-value");
    }

    #[test]
    fn scrub_removes_every_occurrence() {
        let secret = Secret::new("sk-very-secret-value");
        let text = "bad key sk-very-secret-value, again sk-very-secret-value.";
        assert_eq!(secret.scrub(text), "bad key ***, again ***.");
    }

    #[test]
    fn tiny_keys_are_left_alone_to_avoid_garbling() {
        assert_eq!(Secret::new("k").scrub("kkk"), "kkk");
    }
}
