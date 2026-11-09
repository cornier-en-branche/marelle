// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Turns errors into sentences in the user's language.
//!
//! The error types in the other crates carry facts and an English `Display`
//! for logs. What people read is made here, from the catalogs.

use std::io::{self, Write};

use marelle_config::Expected;
use marelle_i18n::{Locale, tr, tr_with};
use marelle_providers::ProviderError;

/// Exit code for a mistake in the command line or in the settings.
pub(crate) const USAGE_ERROR: u8 = 2;
/// Exit code for a provider or network failure.
pub(crate) const RUNTIME_ERROR: u8 = 1;

/// Something to tell the user, and the exit code that goes with it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub message: String,
    pub hint: Option<String>,
    pub code: u8,
}

impl Failure {
    /// A mistake the user can fix by changing the command or the settings.
    pub fn usage(message: String) -> Self {
        Self {
            message,
            hint: None,
            code: USAGE_ERROR,
        }
    }

    /// Writes the failure to `err` and returns its exit code.
    pub fn print(&self, err: &mut dyn Write) -> io::Result<u8> {
        writeln!(err, "marelle: {}", self.message)?;
        if let Some(hint) = &self.hint {
            writeln!(err, "  {hint}")?;
        }
        Ok(self.code)
    }
}

/// How an expectation about a setting reads.
pub(crate) fn expected(locale: Locale, expected: &Expected) -> String {
    match expected {
        Expected::Bool => tr(locale, "expected.bool"),
        Expected::WholeNumber => tr(locale, "expected.whole_number"),
        Expected::PositiveWholeNumber => tr(locale, "expected.positive_whole_number"),
        Expected::Number => tr(locale, "expected.number"),
        Expected::EnvName => tr(locale, "expected.env_name"),
        Expected::Url => tr(locale, "expected.url"),
        Expected::Proxy => tr(locale, "expected.proxy"),
        Expected::JsonObject => tr(locale, "expected.json_object"),
        Expected::OneOf(values) => {
            tr_with(locale, "expected.one_of", &[("values", &values.join(", "))])
        }
    }
}

/// Describes a provider error. `known` lists the providers that exist, for
/// the error about an unknown one.
pub(crate) fn provider_failure(locale: Locale, error: &ProviderError, known: &[String]) -> Failure {
    let say = |key: &str, args: &[(&str, &str)]| tr_with(locale, key, args);
    let hint_for = |key: &str| Some(tr(locale, key));
    let usage = |message: String| Failure::usage(message);
    let runtime = |message: String, hint: Option<String>| Failure {
        message,
        hint,
        code: RUNTIME_ERROR,
    };

    match error {
        ProviderError::Setting(setting) => usage(say(
            "fail.setting",
            &[
                ("key", &setting.key),
                ("expected", &expected(locale, &setting.expected)),
            ],
        )),
        ProviderError::MissingSetting { key } => {
            usage(say("fail.missing_setting", &[("key", key)]))
        }
        ProviderError::SecretInSettings { key } => {
            usage(say("fail.secret_in_settings", &[("key", key)]))
        }
        ProviderError::UnknownProvider { name } => usage(say(
            "fail.unknown_provider",
            &[("name", name), ("known", &known.join(", "))],
        )),
        ProviderError::MissingApiKey { env } => usage(say("fail.no_api_key", &[("env", env)])),
        ProviderError::ApiKeyFile { path, reason } => usage(say(
            "fail.api_key_file",
            &[("path", path), ("reason", reason)],
        )),
        ProviderError::InvalidApiKey => usage(say("fail.invalid_api_key", &[])),
        ProviderError::InsecureTransport { host } => usage(say("fail.insecure", &[("host", host)])),
        ProviderError::Unsupported { detail } => {
            usage(say("fail.unsupported", &[("detail", detail)]))
        }
        ProviderError::CaBundle { source } => usage(say("fail.ca_bundle", &[("source", source)])),
        ProviderError::CaBundleFile { path, reason } => usage(say(
            "fail.ca_bundle_file",
            &[("path", path), ("reason", reason)],
        )),
        ProviderError::Http { status, message } => {
            let hint = match status {
                401 | 403 => hint_for("fail.hint.auth"),
                404 => hint_for("fail.hint.not_found"),
                429 => hint_for("fail.hint.rate"),
                500..=599 => hint_for("fail.hint.server"),
                _ => None,
            };
            runtime(
                say(
                    "fail.http",
                    &[("status", &status.to_string()), ("message", message)],
                ),
                hint,
            )
        }
        ProviderError::Api { message } => runtime(say("fail.api", &[("message", message)]), None),
        ProviderError::Transport { detail } => {
            runtime(say("fail.transport", &[("detail", detail)]), None)
        }
        ProviderError::Timeout => runtime(say("fail.timeout", &[]), None),
        ProviderError::Protocol { detail } => {
            runtime(say("fail.protocol", &[("detail", detail)]), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marelle_config::SettingError;

    fn every_error() -> Vec<ProviderError> {
        vec![
            ProviderError::Setting(SettingError::new(
                "chat.max_tokens",
                Expected::PositiveWholeNumber,
            )),
            ProviderError::MissingSetting {
                key: "providers.x.base_url".into(),
            },
            ProviderError::SecretInSettings {
                key: "providers.x.api_key".into(),
            },
            ProviderError::UnknownProvider {
                name: "nope".into(),
            },
            ProviderError::MissingApiKey {
                env: "ZAI_API_KEY".into(),
            },
            ProviderError::ApiKeyFile {
                path: "/k".into(),
                reason: "gone".into(),
            },
            ProviderError::InvalidApiKey,
            ProviderError::InsecureTransport {
                host: "example.com".into(),
            },
            ProviderError::Unsupported {
                detail: "tools".into(),
            },
            ProviderError::CaBundle {
                source: "/c.pem".into(),
            },
            ProviderError::CaBundleFile {
                path: "/c.pem".into(),
                reason: "gone".into(),
            },
            ProviderError::Http {
                status: 401,
                message: "bad key".into(),
            },
            ProviderError::Api {
                message: "overloaded".into(),
            },
            ProviderError::Transport {
                detail: "dns".into(),
            },
            ProviderError::Timeout,
            ProviderError::Protocol {
                detail: "garbage".into(),
            },
        ]
    }

    #[test]
    fn every_error_has_a_sentence_in_every_language() {
        let known = ["ollama".to_owned(), "zai".to_owned()];
        for locale in Locale::ALL {
            for error in every_error() {
                let failure = provider_failure(locale, &error, &known);
                assert!(!failure.message.is_empty(), "{locale:?} {error:?}");
                assert!(
                    !failure.message.contains("fail."),
                    "a missing translation shows its key: {} ({locale:?})",
                    failure.message
                );
                assert!(
                    !failure.message.contains('{'),
                    "unfilled placeholder: {}",
                    failure.message
                );
            }
        }
    }

    #[test]
    fn the_facts_survive_the_translation() {
        let known = ["ollama".to_owned(), "zai".to_owned()];
        for locale in Locale::ALL {
            let text = |error: ProviderError| provider_failure(locale, &error, &known).message;
            assert!(
                text(ProviderError::MissingApiKey {
                    env: "ZAI_API_KEY".into()
                })
                .contains("ZAI_API_KEY")
            );
            assert!(
                text(ProviderError::UnknownProvider {
                    name: "nope".into()
                })
                .contains("nope")
            );
            assert!(
                text(ProviderError::UnknownProvider {
                    name: "nope".into()
                })
                .contains("ollama, zai")
            );
            assert!(
                text(ProviderError::Http {
                    status: 429,
                    message: "slow".into()
                })
                .contains("429")
            );
            let setting =
                ProviderError::Setting(SettingError::new("chat.temperature", Expected::Number));
            assert!(text(setting).contains("chat.temperature"));
        }
    }

    #[test]
    fn exit_codes_separate_mistakes_from_failures() {
        let known = [];
        let code = |error: &ProviderError| provider_failure(Locale::En, error, &known).code;
        assert_eq!(
            code(&ProviderError::MissingApiKey { env: "X".into() }),
            USAGE_ERROR
        );
        assert_eq!(code(&ProviderError::InvalidApiKey), USAGE_ERROR);
        assert_eq!(
            code(&ProviderError::Http {
                status: 500,
                message: String::new()
            }),
            RUNTIME_ERROR
        );
        assert_eq!(code(&ProviderError::Timeout), RUNTIME_ERROR);
        assert_eq!(
            code(&ProviderError::Transport {
                detail: String::new()
            }),
            RUNTIME_ERROR
        );
    }

    #[test]
    fn http_statuses_come_with_advice() {
        let hint = |status: u16| {
            provider_failure(
                Locale::En,
                &ProviderError::Http {
                    status,
                    message: String::new(),
                },
                &[],
            )
            .hint
        };
        assert!(hint(401).unwrap().contains("API key"));
        assert!(hint(403).unwrap().contains("API key"));
        assert!(hint(404).unwrap().contains("model"));
        assert!(hint(429).unwrap().contains("rate limit"));
        assert!(hint(503).unwrap().contains("trouble"));
        assert_eq!(hint(400), None);
    }

    #[test]
    fn expectations_read_naturally() {
        assert_eq!(expected(Locale::En, &Expected::Bool), "true or false");
        assert_eq!(
            expected(Locale::En, &Expected::OneOf(&["a", "b"])),
            "one of: a, b"
        );
        assert!(expected(Locale::Ru, &Expected::PositiveWholeNumber).contains("больше нуля"));
    }

    #[test]
    fn a_failure_prints_its_message_and_returns_its_code() {
        let failure = Failure {
            message: "it broke".into(),
            hint: Some("try again".into()),
            code: 1,
        };
        let mut out = Vec::new();
        assert_eq!(failure.print(&mut out).unwrap(), 1);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "marelle: it broke\n  try again\n"
        );
    }
}
