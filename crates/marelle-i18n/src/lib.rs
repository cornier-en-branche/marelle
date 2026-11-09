// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! Message catalogs for user-facing text.
//!
//! Catalogs are plain `key = value` files embedded at build time. English is
//! the reference: a missing translation falls back to English, and tests
//! check that every shipped language covers every English key and uses the
//! same `{placeholders}` in each message.

#[cfg(test)]
use std::collections::BTreeSet;

/// A supported interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// English.
    En,
    /// Russian.
    Ru,
    /// French.
    Fr,
    /// Ukrainian.
    Uk,
}

impl Locale {
    /// Every supported language.
    pub const ALL: [Locale; 4] = [Locale::En, Locale::Ru, Locale::Fr, Locale::Uk];

    /// The two-letter language code.
    pub fn code(self) -> &'static str {
        match self {
            Locale::En => "en",
            Locale::Ru => "ru",
            Locale::Fr => "fr",
            Locale::Uk => "uk",
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
            Locale::Uk => include_str!("../locales/uk.txt"),
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

/// Looks up a message and fills in its `{name}` placeholders.
///
/// Substitution happens in one pass, so a value that happens to contain
/// `{other}` is never expanded again. Placeholders without a matching
/// argument are left as they are.
pub fn tr_with(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    fill(&tr(locale, key), args)
}

fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        match args.iter().find(|(arg, _)| *arg == name) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        found.insert(&after[..end]);
        rest = &after[end + 1..];
    }
    found
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
        assert_eq!(tr(Locale::Uk, "help.usage"), "Використання");
        assert_eq!(tr(Locale::Fr, "no.such.key"), "no.such.key");
    }

    #[test]
    fn parses_ukrainian_tags() {
        assert_eq!(Locale::parse("uk"), Some(Locale::Uk));
        assert_eq!(Locale::parse("uk_UA.UTF-8"), Some(Locale::Uk));
    }

    #[test]
    fn placeholders_are_filled_in_one_pass() {
        // A message without placeholders is returned as it is.
        let text = tr_with(Locale::En, "error.unknown_lang", &[("name", "x")]);
        assert_eq!(text, "Unknown language");

        let template = "Now using {model} via {provider}.";
        let filled = fill(template, &[("model", "{provider}"), ("provider", "zai")]);
        assert_eq!(filled, "Now using {provider} via zai.");

        let untouched = fill(template, &[]);
        assert_eq!(untouched, template);

        // Unbalanced braces survive.
        assert_eq!(fill("a { b", &[("b", "x")]), "a { b");
    }

    #[test]
    fn every_translation_uses_the_same_placeholders_as_english() {
        for locale in Locale::ALL {
            for (key, english) in entries(Locale::En.catalog()) {
                let translated = entries(locale.catalog())
                    .find_map(|(k, v)| (k == key).then_some(v))
                    .unwrap_or_default();
                assert_eq!(
                    placeholders(translated),
                    placeholders(english),
                    "{} differs in placeholders for {key}",
                    locale.code()
                );
            }
        }
    }

    #[test]
    fn catalogs_have_no_duplicate_keys() {
        for locale in Locale::ALL {
            let mut seen = BTreeSet::new();
            for (key, _) in entries(locale.catalog()) {
                assert!(seen.insert(key), "{} repeats {key}", locale.code());
            }
        }
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
