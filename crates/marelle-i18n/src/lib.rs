// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Message catalogs for user-facing text.
//!
//! Catalogs are plain `key = value` files embedded at build time. English is
//! the reference: a missing translation falls back to English, and a test
//! checks that every shipped language covers every English key.

/// A supported interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// English.
    En,
    /// Russian.
    Ru,
    /// French.
    Fr,
}

impl Locale {
    /// Every supported language.
    pub const ALL: [Locale; 3] = [Locale::En, Locale::Ru, Locale::Fr];

    /// The two-letter language code.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Ru => "ru",
            Locale::Fr => "fr",
        }
    }

    /// Parses a language tag such as `ru`, `fr_FR.UTF-8` or `en-US`.
    ///
    /// Returns `None` for unsupported languages and for `C` or `POSIX`.
    pub fn parse(tag: &str) -> Option<Locale> {
        let language = tag.split(['.', '@']).next()?.split(['_', '-']).next()?;
        Self::ALL
            .into_iter()
            .find(|locale| locale.code().eq_ignore_ascii_case(language))
    }

    /// Picks a language from the environment.
    ///
    /// `MARELLE_LANG` wins, then the usual POSIX variables in their usual
    /// order. The first variable that is set and non-empty decides; if it
    /// names an unsupported language the result is English.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Locale {
        ["MARELLE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|name| lookup(name).filter(|value| !value.is_empty()))
            .and_then(|value| Locale::parse(&value))
            .unwrap_or(Locale::En)
    }

    fn catalog(self) -> &'static str {
        match self {
            Locale::En => include_str!("../locales/en.txt"),
            Locale::Ru => include_str!("../locales/ru.txt"),
            Locale::Fr => include_str!("../locales/fr.txt"),
        }
    }
}

fn entries(catalog: &'static str) -> impl Iterator<Item = (&'static str, &'static str)> {
    catalog.lines().filter_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim()))
    })
}

/// Looks up a message.
///
/// Falls back to English when the language lacks the key, and to the key
/// itself when English lacks it too.
pub fn tr(locale: Locale, key: &str) -> String {
    [locale, Locale::En]
        .into_iter()
        .find_map(|candidate| {
            entries(candidate.catalog()).find_map(|(k, v)| (k == key).then_some(v))
        })
        .unwrap_or(key)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_language_tags() {
        assert_eq!(Locale::parse("ru"), Some(Locale::Ru));
        assert_eq!(Locale::parse("fr_FR.UTF-8"), Some(Locale::Fr));
        assert_eq!(Locale::parse("EN-us"), Some(Locale::En));
        assert_eq!(Locale::parse("de_DE"), None);
        assert_eq!(Locale::parse("C"), None);
    }

    #[test]
    fn marelle_lang_beats_posix_variables() {
        let env = |name: &str| match name {
            "MARELLE_LANG" => Some("fr".to_owned()),
            "LANG" => Some("ru_RU.UTF-8".to_owned()),
            _ => None,
        };
        assert_eq!(Locale::from_env(env), Locale::Fr);
    }

    #[test]
    fn lc_all_beats_lang() {
        let env = |name: &str| match name {
            "LC_ALL" => Some("fr_FR.UTF-8".to_owned()),
            "LANG" => Some("ru_RU.UTF-8".to_owned()),
            _ => None,
        };
        assert_eq!(Locale::from_env(env), Locale::Fr);
    }

    #[test]
    fn unset_or_unsupported_falls_back_to_english() {
        assert_eq!(Locale::from_env(|_| None), Locale::En);
        assert_eq!(
            Locale::from_env(|name| (name == "LANG").then(|| "C".to_owned())),
            Locale::En
        );
    }

    #[test]
    fn translates_and_falls_back_to_the_key() {
        assert_eq!(tr(Locale::En, "help.usage"), "Usage");
        assert_eq!(tr(Locale::Ru, "help.usage"), "Использование");
        assert_eq!(tr(Locale::Fr, "help.usage"), "Utilisation");
        assert_eq!(tr(Locale::Fr, "no.such.key"), "no.such.key");
    }

    #[test]
    fn every_language_covers_every_english_key() {
        for locale in Locale::ALL {
            for (key, _) in entries(Locale::En.catalog()) {
                assert!(
                    entries(locale.catalog()).any(|(k, _)| k == key),
                    "{} is missing {key}",
                    locale.code()
                );
            }
        }
    }
}
