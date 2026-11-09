// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

//! The read-only commands: `providers`, `config show`, `config explain`.

use std::io::{self, Write};
use std::path::PathBuf;

use marelle_config::file;
use marelle_i18n::{Locale, tr, tr_with};
use marelle_providers::{ProviderConfig, provider_names};

use crate::Io;
use crate::args::FileChoice;
use crate::messages::{USAGE_ERROR, config_failure, provider_failure};
use crate::schema;
use crate::settings::{build_config, config_paths, display_value, env_lookup, redact_url};

const OK: u8 = 0;

/// Lists providers and where their keys come from, as a table.
pub(crate) fn providers(
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let config = match build_config(env_vars, &[]) {
        Ok(config) => config,
        Err(error) => return config_failure(locale, &error).print(io.err),
    };
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
    let config = match build_config(env_vars, &[]) {
        Ok(config) => config,
        Err(error) => return config_failure(locale, &error).print(io.err),
    };
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
    let config = match build_config(env_vars, &[]) {
        Ok(config) => config,
        Err(error) => return config_failure(locale, &error).print(io.err),
    };

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

/// Prints one effective value, or says that it is not set.
pub(crate) fn get(
    locale: Locale,
    key: &str,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    if key.trim().is_empty() {
        writeln!(io.err, "{}", tr(locale, "error.missing_key"))?;
        return Ok(USAGE_ERROR);
    }
    let config = match build_config(env_vars, &[]) {
        Ok(config) => config,
        Err(error) => return config_failure(locale, &error).print(io.err),
    };
    match config.get(key) {
        Some(value) => {
            let layer = config
                .explain(key)
                .last()
                .map(|origin| origin.layer.to_string())
                .unwrap_or_default();
            writeln!(io.out, "{} = {}  [{layer}]", key, display_value(key, value))?;
        }
        None => writeln!(io.out, "{}: {key}", tr(locale, "config.not_set"))?,
    }
    Ok(OK)
}

/// Prints the settings Marelle knows, with their type and description.
pub(crate) fn schema(locale: Locale, prefix: Option<&str>, io: &mut Io<'_>) -> io::Result<u8> {
    let mut printed = 0;
    let mut last_family = "";
    for spec in schema::rows() {
        if let Some(prefix) = prefix {
            if !spec.key.starts_with(prefix) {
                continue;
            }
        }
        let family = spec.key.split('.').next().unwrap_or(spec.key);
        if family != last_family {
            if printed > 0 {
                writeln!(io.out)?;
            }
            last_family = family;
        }
        let kind = spec.kind.name();
        match spec.default {
            Some(default) => writeln!(
                io.out,
                "{}  ({kind}, {} {default})",
                spec.key,
                tr(locale, "config.schema.default")
            )?,
            None => writeln!(io.out, "{}  ({kind})", spec.key)?,
        }
        writeln!(io.out, "  {}", spec.description)?;
        printed += 1;
    }
    if printed == 0 {
        writeln!(io.out, "{}", tr(locale, "config.schema.empty"))?;
    }
    Ok(OK)
}

/// Prints where the setting files are and whether they exist.
pub(crate) fn paths(
    locale: Locale,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    let paths = config_paths(env_vars);
    match &paths.global {
        Some(path) => writeln!(
            io.out,
            "{}: {} ({})",
            tr(locale, "config.path.global"),
            path.display(),
            state(locale, path)
        )?,
        None => writeln!(
            io.out,
            "{}: {}",
            tr(locale, "config.path.global"),
            tr(locale, "config.path.none")
        )?,
    }
    writeln!(
        io.out,
        "{}: {} ({})",
        tr(locale, "config.path.project"),
        paths.project.display(),
        state(locale, &paths.project)
    )?;
    Ok(OK)
}

fn state(locale: Locale, path: &std::path::Path) -> String {
    tr(
        locale,
        if path.is_file() {
            "config.path.exists"
        } else {
            "config.path.missing"
        },
    )
}

/// Where a write should go: the chosen file, or a reason why it cannot.
fn target(
    locale: Locale,
    choice: &FileChoice,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<Result<PathBuf, u8>> {
    let paths = config_paths(env_vars);
    match choice {
        FileChoice::Path(path) => Ok(Ok(PathBuf::from(path))),
        FileChoice::Project => Ok(Ok(paths.project)),
        FileChoice::Global => match paths.global {
            Some(path) => Ok(Ok(path)),
            None => {
                writeln!(io.err, "{}", tr(locale, "config.path.no_home"))?;
                Ok(Err(USAGE_ERROR))
            }
        },
    }
}

/// Writes one setting to a file after checking its shape.
pub(crate) fn set(
    locale: Locale,
    key: &str,
    value: &str,
    choice: &FileChoice,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    if key.trim().is_empty() {
        writeln!(io.err, "{}", tr(locale, "error.missing_key"))?;
        return Ok(USAGE_ERROR);
    }
    let Some(spec) = schema::lookup(key) else {
        writeln!(io.err, "{}: {key}", tr(locale, "error.unknown_setting"))?;
        return Ok(USAGE_ERROR);
    };
    let last = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
    if ["api_key", "apikey", "key", "token", "secret"].contains(&last.as_str()) {
        writeln!(
            io.err,
            "{}",
            tr_with(locale, "fail.secret_in_settings", &[("key", key)])
        )?;
        return Ok(USAGE_ERROR);
    }
    if let Err(reason) = spec.kind.check(value) {
        writeln!(
            io.err,
            "{}",
            tr_with(
                locale,
                "fail.invalid_value",
                &[("key", key), ("reason", &reason)]
            )
        )?;
        return Ok(USAGE_ERROR);
    }
    let path = match target(locale, choice, env_vars, io)? {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    if let Err(error) = file::set(&path, key, value, spec.kind.value_kind()) {
        return config_failure(locale, &error).print(io.err);
    }
    writeln!(
        io.out,
        "{}",
        tr_with(
            locale,
            "config.set",
            &[
                ("key", key),
                ("value", &display_value(key, value)),
                ("path", &path.display().to_string())
            ]
        )
    )?;
    Ok(OK)
}

/// Removes one setting from a file.
pub(crate) fn unset(
    locale: Locale,
    key: &str,
    choice: &FileChoice,
    env_vars: &[(String, String)],
    io: &mut Io<'_>,
) -> io::Result<u8> {
    if key.trim().is_empty() {
        writeln!(io.err, "{}", tr(locale, "error.missing_key"))?;
        return Ok(USAGE_ERROR);
    }
    let path = match target(locale, choice, env_vars, io)? {
        Ok(path) => path,
        Err(code) => return Ok(code),
    };
    if let Err(error) = file::unset(&path, key) {
        return config_failure(locale, &error).print(io.err);
    }
    writeln!(
        io.out,
        "{}",
        tr_with(
            locale,
            "config.unset",
            &[("key", key), ("path", &path.display().to_string())]
        )
    )?;
    Ok(OK)
}
