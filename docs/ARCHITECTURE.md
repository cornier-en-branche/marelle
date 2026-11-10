# Architecture

Marelle is a terminal AI agent that you point at your own API keys. These goals shape the design:

1. **Configurable to the bone.** Settings come in layers (defaults, global file, project file, environment, flags). Every value remembers which layer set it, and `marelle config explain <key>` shows the whole history.
2. **Many interface languages.** User-facing text lives in catalogs, never in code. English is the reference, and a test fails if a shipped language misses a message.
3. **Your keys stay yours.** Configuration refers to API keys by environment variable name or by file, and never contains them.
4. **Extensible in any language.** Plugins talk to the core over a protocol (MCP first), so they can be written in anything.
5. **Small and portable.** One binary, no runtime. Linux and Termux first.

## How a chat request flows

```text
marelle chat "question"                      crates/marelle-cli
        |  flags + environment (files next)
        v
 marelle-config      Default < Global < Project < Env < Flag, each value tagged with its layer
        |
        v
 marelle-providers   ProviderConfig (protocol, address, auth, where the key lives)
        |            Client::chat(request, sink)  --- HTTPS --->  the provider
        |            SSE parser + complete-response decoder + protocol decoders
        v
 sink in marelle-cli answer -> stdout; reasoning, usage, notes, errors -> stderr
```

The command line takes its input and output as parameters (`Io`), so a whole session can be driven by a test without touching the process. Both provider adapters decode SSE and complete JSON responses. The tool-enabled agent loop currently requests complete responses so structured tool calls are not assembled from partial fragments; the optional judge response can use streaming.

## Crates

| Crate | Role | State |
| --- | --- | --- |
| `marelle-core` | Messages, roles, sessions | Basic types |
| `marelle-config` | Layered configuration with provenance, typed getters, secret redaction | Defaults, global and project TOML files, environment, flags |
| `marelle-i18n` | Message catalogs (en, ru, fr, uk) | Working |
| `marelle-providers` | Provider settings, presets, the HTTP client, both wire protocols | OpenAI-compatible and Anthropic, SSE streaming and complete responses |
| `marelle-cli` | The `marelle` command | `chat`, `providers`, `config show/get/set/unset/schema/path` |
| `marelle-testkit` | A mock HTTP server for tests (not published) | Used by the provider and CLI tests |

Two protocols cover the vendors we know of: OpenAI-compatible endpoints (OpenAI, OpenRouter, Ollama, Z.ai, Zhipu and others) and the Anthropic Messages API. Support for another vendor goes through whichever of the two its API matches, and that has to be checked per vendor.

## Decisions worth knowing

**One HTTP stack on every platform.** Marelle uses `reqwest` with `rustls`, and builds the TLS configuration itself: the `ring` crypto provider, the Mozilla root list from `webpki-roots`, plus anything from `network.ca_bundle` and `SSL_CERT_FILE`. It never asks the operating system for certificates. This matters on Android, where a native program in Termux has no system certificate store at the usual place, and where the platform verifier used by `rustls-platform-verifier` panics outside an Android app. The crate is still compiled because `reqwest` depends on it, but nothing calls it.

**Keys are handled as a type, not as text.** The key lives in a `Secret` whose `Debug` and `Display` print nothing useful. It goes out in a header marked sensitive. Text that a server sends back, such as an error message, is scrubbed of the key before it is shown. A setting named like `providers.x.api_key` is refused with an error that does not echo the value.

**Redirects are not followed.** A redirect would carry the key to another host. A provider that redirects is reported as an error.

**A key never crosses plain `http` to another machine.** Loopback addresses are fine, for a local Ollama. For anything else the call is refused unless `network.allow_insecure_http=true`.

**Streams fail loudly.** A stream that ends without a finish marker is a protocol error, not a short answer. A server that ignores `stream: true` and answers with plain JSON is read as a complete answer.

**Output follows Unix habits.** stdout carries only the answer, so pipes and redirects work. Everything else is on stderr. Exit codes are `0`, `1` for provider or network trouble and `2` for usage or configuration errors.

**Tests do not need the network.** `marelle-testkit` serves canned answers on localhost, including slow, broken and redirecting ones.

## Roadmap

1. **Talk.** HTTP adapters for both protocols, including SSE streaming and complete-response decoding. Tool-enabled agent turns currently use complete responses. *Done.*
2. **Settings and look.** TOML files, global and per project, with `config set`, `get` and `unset`, and a schema that describes every key (type, default, allowed values). A themed interface: banner, information panel, framed answers, spinner, built to adapt to narrow phone screens. The default theme is wisteria purple, and every colour role can be set as HEX. *Done.*
3. **Many models.** Model aliases, asking several models in parallel and comparing their answers, fallback chains, and a judge model that merges them. *Done.*
4. **Act.** The tool loop, built-in tools (files, shell, fetch) and a permission policy: allow, ask or deny by path and command.
5. **Remember.** Sessions and memory in SQLite.
6. **Extend.** An MCP client, a plugin protocol and skills.
7. **Reach.** Gateways (Telegram first) and scheduled tasks.
8. **Languages.** Catalogs for every language of Eurasia and then more, with right-to-left support and translated documentation.

## References

[Hermes Agent](https://github.com/NousResearch/hermes-agent) (MIT) is the main reference for what a full agent looks like, and the interface takes its look from Hermes's CLI. [OpenCode](https://github.com/sst/opencode) (MIT) is the second reference, for permission patterns and configuration layers. Both are only references: no code from either is included. [NOTICE](../NOTICE) says how code from them, or from any other project, must be recorded if it is ever added.
