# Marelle

A configurable AI agent for your terminal. Bring your own API keys and tune everything: which model handles which task, fallbacks, budgets, permissions and the language of the interface.

> **Status: pre-alpha.** This repository holds the project skeleton. There is no agent loop and no network access yet. The plan is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

[Русская версия](README.ru.md)

## What works today

- `marelle help`, `marelle version`, `marelle providers`
- `marelle config explain <key>` shows where a setting comes from and what it overrode. For now it knows built-in defaults and `MARELLE_*` environment variables (`MARELLE_AGENT__MAX_TURNS` sets `agent.max_turns`). Values of keys that look secret are printed as `***`.
- Interface languages: English, Russian and French. Pick one with `--lang <code>` or `MARELLE_LANG`; otherwise the usual `LC_ALL`, `LC_MESSAGES` and `LANG` variables decide.

## Build

Requires Rust 1.85 or newer (developed with 1.97). There are no other dependencies.

```sh
cargo build --release
./target/release/marelle help
cargo test --workspace
```

Termux on Android should work with `pkg install rust`, but this has not been tested yet.

## Design goals

- **Configurable to the bone.** Layered settings (defaults, global file, project file, environment, flags) where every value remembers where it came from.
- **Many interface languages.** Every message lives in a catalog, and a test fails if a language misses a message.
- **Your keys stay yours.** Configuration refers to API keys by environment variable name and never contains them.
- **Extensible in any language.** Plugins will talk to the core over a protocol, so they can be written in anything.
- **Small and portable.** One binary, no runtime. Linux and Termux first.

## License

[AGPL-3.0-only](LICENSE). Third-party attributions are in [NOTICE](NOTICE).
