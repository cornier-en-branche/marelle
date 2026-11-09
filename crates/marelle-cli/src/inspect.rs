// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The read-only commands: `providers`, `config show`, `config explain`.

use std::io::{self, Write};

use marelle_i18n::{Locale, tr, tr_with};
use marelle_providers::{ProviderConfig, provider_names};

use crate::Io;
use crate::messages::{USAGE_ERROR, provider_failure};
use crate::settings::{build_config, display_value, env_lookup, redact_url};

const OK: u8 = 0;

/// Lists providers and where their keys come from, as a table.
pub(crate) fn providers(
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let config = build_config(env_vars, &[]);
    let lookup = |name: &str| env_lookup(env_vars, name);
    let names = provider_names(&config);

    let mut rows: Vec<[String; 4]> = Vec::with_capacity(names.len());
    for name in &names {
        match ProviderConfig::from_config(&config, name) {
            Ok(provider) => rows.push([
                name.clone(),
                provider.kind.id().to_owned(),
                redact_url(&provider.base_url),
                key_column(locale, &provider, &lookup),
            ]),
            Err(error) => {
                let reason = provider_failure(locale, &error, &names).message;
                rows.push([
                    name.clone(),
                    "-".to_owned(),
                    "-".to_owned(),
                    tr_with(locale, "providers.broken", &[("reason", &reason)]),
                ]);
            }
        }
    }

    let header = [
        tr(locale, "providers.col.name"),
        tr(locale, "providers.col.protocol"),
        tr(locale, "providers.col.address"),
        tr(locale, "providers.col.key"),
    ];
    write_table(io.out, &header, &rows)?;
    writeln!(io.out)?;
    writeln!(io.out, "{}", tr(locale, "providers.hint"))?;
    Ok(OK)
}

/// Where a provider's key is expected, and whether it is there.
fn key_column(
    locale: Locale,
    provider: &ProviderConfig,
    lookup: &impl Fn(&str) -> Option<String>,
) -> String {
    let mut sources = Vec::new();
    if let Some(env) = &provider.api_key_env {
        let present = lookup(env).is_some_and(|value| !value.trim().is_empty());
        let state = tr(
            locale,
            if present {
                "providers.key.set"
            } else {
                "providers.key.not_set"
            },
        );
        sources.push(format!("{env} ({state})"));
    }
    if let Some(path) = provider.api_key_file_path(lookup) {
        let state = tr(
            locale,
            if path.is_file() {
                "providers.key.found"
            } else {
                "providers.key.missing"
            },
        );
        let label = tr_with(
            locale,
            "providers.file",
            &[("path", &path.display().to_string())],
        );
        sources.push(format!("{label} ({state})"));
    }
    if sources.is_empty() {
        return tr(locale, "providers.key.none");
    }
    sources.join(", ")
}

/// Prints rows under a header, padded to the widest cell of each column.
fn write_table(out: &mut dyn Write, header: &[String; 4], rows: &[[String; 4]]) -> io::Result<()> {
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .chain(std::iter::once(header[column].chars().count()))
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];

    let line = |out: &mut dyn Write, cells: &[String; 4]| -> io::Result<()> {
        let mut text = String::new();
        for (index, cell) in cells.iter().enumerate() {
            text.push_str(cell);
            if index + 1 < cells.len() {
                let padding = widths[index] - cell.chars().count();
                text.extend(std::iter::repeat_n(' ', padding + 2));
            }
        }
        writeln!(out, "{}", text.trim_end())
    };

    line(out, header)?;
    let rule = [
        "-".repeat(widths[0]),
        "-".repeat(widths[1]),
        "-".repeat(widths[2]),
        "-".repeat(widths[3]),
    ];
    line(out, &rule)?;
    for row in rows {
        line(out, row)?;
    }
    Ok(())
}

/// Prints every setting, or those under a prefix, with the layer that won.
pub(crate) fn show(
    locale: Locale,
    prefix: Option<&str>,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let config = build_config(env_vars, &[]);
    let mut keys: Vec<&str> = config
        .keys()
        .into_iter()
        .filter(|key| prefix.is_none_or(|prefix| key.starts_with(prefix)))
        .collect();
    keys.sort_unstable();

    if keys.is_empty() {
        writeln!(io.out, "{}", tr(locale, "config.show.empty"))?;
        return Ok(OK);
    }
    let width = keys
        .iter()
        .map(|key| key.chars().count())
        .max()
        .unwrap_or(0);
    for key in keys {
        let Some(value) = config.get(key) else {
            continue;
        };
        let layer = config
            .explain(key)
            .last()
            .map(|origin| origin.layer.to_string())
            .unwrap_or_default();
        let padding = " ".repeat(width - key.chars().count());
        writeln!(
            io.out,
            "{key}{padding} = {}  [{layer}]",
            display_value(key, value)
        )?;
    }
    Ok(OK)
}

/// Prints the effective value of one setting and where it came from.
pub(crate) fn explain(
    locale: Locale,
    key: Option<&str>,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let Some(key) = key else {
        writeln!(io.err, "{}", tr(locale, "error.missing_key"))?;
        return Ok(USAGE_ERROR);
    };
    let config = build_config(env_vars, &[]);

    writeln!(io.out, "{key}")?;
    match config.get(key) {
        None => writeln!(io.out, "  {}", tr(locale, "config.not_set"))?,
        Some(value) => {
            writeln!(
                io.out,
                "  {}: {}",
                tr(locale, "config.effective"),
                display_value(key, value)
            )?;
            writeln!(io.out, "  {}:", tr(locale, "config.history"))?;
            for origin in config.explain(key) {
                writeln!(
                    io.out,
                    "    {}: {}",
                    origin.layer,
                    display_value(key, &origin.value)
                )?;
            }
        }
    }
    Ok(OK)
}
