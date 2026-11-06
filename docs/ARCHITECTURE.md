# Architecture

Marelle is a terminal AI agent that you point at your own API keys. These goals shape the design:

1. **Configurable to the bone.** Settings come in layers (defaults, global file, project file, environment, flags). Every value remembers which layer set it, and `marelle config explain <key>` shows the whole history.
2. **Many interface languages.** User-facing text lives in catalogs, never in code. English is the reference, and a test fails if a shipped language misses a message.
3. **Your keys stay yours.** Configuration refers to API keys by environment variable name and never contains them. Values of keys that look secret are redacted in output.
4. **Extensible in any language.** Plugins talk to the core over a protocol (MCP first), so they can be written in anything.
5. **Small and portable.** One binary, no runtime. Linux and Termux first.

## Crates

| Crate | Role | State |
| --- | --- | --- |
| `marelle-core` | Messages, roles, sessions | Basic types |
| `marelle-config` | Layered configuration with provenance | Layers and environment variables; config files next |
| `marelle-i18n` | Message catalogs (en, ru, fr) | Working |
| `marelle-providers` | Provider kinds and API key lookup | Vocabulary only, no network |
| `marelle-cli` | The `marelle` command | `help`, `version`, `providers`, `config explain` |

Provider kinds start with two: OpenAI-compatible endpoints (OpenAI, OpenRouter, Ollama and others) and the Anthropic Messages API. Support for other vendors, such as Zhipu, will go through whichever of the two their API matches. That has to be verified per vendor.

## Roadmap

1. **Talk.** HTTP adapters for both provider kinds and a streaming `marelle chat` in the terminal.
2. **Act.** The tool loop, built-in tools (files, shell, fetch) and a permission policy: allow, ask or deny by path and command.
3. **Configure.** TOML config files (global and project), profiles, model routing per task, fallback chains, budgets and hooks.
4. **Remember.** Sessions and memory in SQLite.
5. **Extend.** An MCP client, a plugin protocol and skills.
6. **Reach.** Gateways (Telegram first) and scheduled tasks.

## Reference

[Hermes Agent](https://github.com/NousResearch/hermes-agent) (MIT) is the main reference for what a full agent looks like. See [NOTICE](../NOTICE) for how code from it, or from other projects, must be recorded if it is ever added.
