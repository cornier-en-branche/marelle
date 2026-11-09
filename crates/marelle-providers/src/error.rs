// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

use std::error::Error;
use std::fmt;

use marelle_config::SettingError;

/// What can go wrong when resolving a provider or talking to it.
///
/// None of the messages contain an API key. Server-supplied text that might
/// echo one is scrubbed before it ends up here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    /// A setting holds a value that cannot be used.
    Setting(SettingError),
    /// A required setting is not set.
    MissingSetting {
        /// The setting's key.
        key: String,
    },
    /// No provider has this name.
    UnknownProvider {
        /// The name as the user wrote it.
        name: String,
    },
    /// The environment variable that should hold the API key is unset or empty.
    MissingApiKey {
        /// The name of the environment variable.
        env: String,
    },
    /// The file that should hold the API key cannot be used.
    ApiKeyFile {
        /// The path of the file.
        path: String,
        /// Why it cannot be used.
        reason: String,
    },
    /// The API key contains characters that cannot go into an HTTP header.
    InvalidApiKey,
    /// A setting holds a secret itself. Settings only name where a key lives.
    SecretInSettings {
        /// The setting's key.
        key: String,
    },
    /// The key would travel over plain HTTP to a machine other than this one.
    InsecureTransport {
        /// The host the request was going to.
        host: String,
    },
    /// The request cannot be expressed for this provider.
    Unsupported {
        /// What is not supported.
        detail: String,
    },
    /// A bundle of extra CA certificates holds nothing usable.
    CaBundle {
        /// Where the bundle came from, usually a path.
        source: String,
    },
    /// The file with extra CA certificates cannot be read.
    CaBundleFile {
        /// The path of the file.
        path: String,
        /// Why it cannot be read.
        reason: String,
    },
    /// The provider answered with an HTTP error status.
    Http {
        /// The status code.
        status: u16,
        /// The provider's explanation, or the start of its answer.
        message: String,
    },
    /// The provider reported an error inside an otherwise successful response.
    Api {
        /// The provider's explanation.
        message: String,
    },
    /// The network failed: name lookup, connection, TLS or proxy.
    Transport {
        /// A description of the failure.
        detail: String,
    },
    /// The provider did not answer in time.
    Timeout,
    /// The response does not follow the protocol.
    Protocol {
        /// What was wrong with it.
        detail: String,
    },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Setting(error) => write!(f, "{error}"),
            ProviderError::MissingSetting { key } => write!(f, "setting {key} is required"),
            ProviderError::UnknownProvider { name } => write!(f, "unknown provider '{name}'"),
            ProviderError::MissingApiKey { env } => {
                write!(f, "environment variable {env} is not set or is empty")
            }
            ProviderError::ApiKeyFile { path, reason } => {
                write!(f, "cannot use the API key file {path}: {reason}")
            }
            ProviderError::InvalidApiKey => {
                f.write_str("the API key contains characters that cannot be sent in an HTTP header")
            }
            ProviderError::SecretInSettings { key } => write!(
                f,
                "setting {key} would hold a secret; name an environment variable or a file instead"
            ),
            ProviderError::InsecureTransport { host } => {
                write!(f, "refusing to send an API key over plain HTTP to {host}")
            }
            ProviderError::Unsupported { detail } => write!(f, "not supported: {detail}"),
            ProviderError::CaBundle { source } => {
                write!(f, "no usable certificates in {source}")
            }
            ProviderError::CaBundleFile { path, reason } => {
                write!(f, "cannot read the CA bundle {path}: {reason}")
            }
            ProviderError::Http { status, message } => {
                write!(f, "the provider answered HTTP {status}: {message}")
            }
            ProviderError::Api { message } => {
                write!(f, "the provider reported an error: {message}")
            }
            ProviderError::Transport { detail } => {
                write!(f, "could not talk to the provider: {detail}")
            }
            ProviderError::Timeout => f.write_str("the provider did not answer in time"),
            ProviderError::Protocol { detail } => {
                write!(f, "unexpected response from the provider: {detail}")
            }
        }
    }
}

impl Error for ProviderError {}

impl From<SettingError> for ProviderError {
    fn from(error: SettingError) -> Self {
        ProviderError::Setting(error)
    }
}

pub(crate) fn protocol(detail: impl Into<String>) -> ProviderError {
    ProviderError::Protocol {
        detail: detail.into(),
    }
}
