// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Descriptions of model providers.
//!
//! Network adapters arrive in a later milestone. This crate fixes the
//! vocabulary first: which kinds of provider exist and how API keys are
//! referenced without ever being stored.

use std::error::Error;
use std::fmt;

/// The wire protocol a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Endpoints that speak the OpenAI chat-completions protocol.
    OpenAiCompatible,
    /// The Anthropic Messages API.
    Anthropic,
}

impl ProviderKind {
    /// Every supported kind.
    pub const ALL: [ProviderKind; 2] = [ProviderKind::OpenAiCompatible, ProviderKind::Anthropic];

    /// The identifier used in configuration files and on the command line.
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::OpenAiCompatible => "openai-compatible",
            ProviderKind::Anthropic => "anthropic",
        }
    }

    /// Finds a kind by its identifier.
    pub fn parse(id: &str) -> Option<ProviderKind> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }
}

/// How to reach one provider.
///
/// Holds no secrets: the API key is looked up, by the name of an environment
/// variable, at the moment it is needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// A name of your choosing, such as `work` or `local`.
    pub name: String,
    /// The protocol the provider speaks.
    pub kind: ProviderKind,
    /// The base URL of the API.
    pub base_url: String,
    /// The name of the environment variable that holds the API key.
    pub api_key_env: String,
}

/// What can go wrong when resolving a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// The environment variable that should hold the API key is unset or empty.
    MissingApiKey {
        /// The name of the environment variable.
        env: String,
    },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::MissingApiKey { env } => {
                write!(f, "environment variable {env} is not set or is empty")
            }
        }
    }
}

impl Error for ProviderError {}

impl ProviderConfig {
    /// Resolves the API key using `lookup` to read environment variables.
    pub fn api_key_with(
        &self,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<String, ProviderError> {
        lookup(&self.api_key_env)
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| ProviderError::MissingApiKey {
                env: self.api_key_env.clone(),
            })
    }

    /// Resolves the API key from the process environment.
    pub fn api_key(&self) -> Result<String, ProviderError> {
        self.api_key_with(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProviderConfig {
        ProviderConfig {
            name: "work".to_owned(),
            kind: ProviderKind::Anthropic,
            base_url: "https://api.example.com".to_owned(),
            api_key_env: "EXAMPLE_API_KEY".to_owned(),
        }
    }

    #[test]
    fn kind_identifiers_round_trip() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(kind.id()), Some(kind));
        }
        assert_eq!(ProviderKind::parse("nope"), None);
    }

    #[test]
    fn key_is_read_from_the_named_variable() {
        let key = sample().api_key_with(|name| (name == "EXAMPLE_API_KEY").then(|| "k".to_owned()));
        assert_eq!(key, Ok("k".to_owned()));
    }

    #[test]
    fn unset_key_is_an_error_that_names_the_variable() {
        let error = sample().api_key_with(|_| None).unwrap_err();
        assert_eq!(
            error,
            ProviderError::MissingApiKey {
                env: "EXAMPLE_API_KEY".to_owned()
            }
        );
        assert!(error.to_string().contains("EXAMPLE_API_KEY"));
    }

    #[test]
    fn blank_key_counts_as_missing() {
        let result = sample().api_key_with(|_| Some("   ".to_owned()));
        assert!(result.is_err());
    }
}
