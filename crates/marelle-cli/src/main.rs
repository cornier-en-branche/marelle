// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Marelle contributors

use std::io::{self, BufReader, IsTerminal};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let env_vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();

    // Colour only for a person: a terminal, with NO_COLOR unset and a
    // terminal that is not "dumb".
    let wants_no_colour = env_vars
        .iter()
        .any(|(name, value)| name == "NO_COLOR" && !value.is_empty());
    let dumb = env_vars
        .iter()
        .any(|(name, value)| name == "TERM" && value == "dumb");
    let color = stderr.is_terminal() && !wants_no_colour && !dumb;

    let interactive = stdin.is_terminal()
        && stdout.is_terminal()
        && stderr.is_terminal()
        && !dumb;
    // Keep stdin unlocked for the terminal event reader used by the TUI.
    // BufReader only reads when a command consumes input; the TUI uses
    // Crossterm's event source while raw mode is active.
    let mut input = BufReader::new(stdin);
    let mut out = stdout;
    let mut err = stderr;
    let mut streams = marelle_cli::Io {
        input: &mut input,
        out: &mut out,
        err: &mut err,
        interactive,
        color,
    };

    match marelle_cli::run(&args, &env_vars, &mut streams) {
        Ok(code) => ExitCode::from(code),
        // Output that cannot be written, such as a closed pipe.
        Err(_) => ExitCode::FAILURE,
    }
}
