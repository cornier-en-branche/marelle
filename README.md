```text
 +---+---+---+---+---+---+---+
 | m | a | r | e | l | l | e |
 +---+---+---+---+---+---+---+
```

**A configurable AI agent for your terminal.**
Bring your own API keys: Z.ai (Zhipu GLM), Anthropic, OpenAI, OpenRouter, a local Ollama, or any OpenAI-compatible server. Linux and Termux first.

English | [Русский](README.ru.md)

> **Pre-alpha, version 0.1.0-alpha.1.** Chat works: streaming answers, pipes and an interactive session. The agent loop with tools is not there yet, and settings will still change. What comes next is under [Roadmap](#roadmap).

*Marelle* is French for hopscotch. Each square is a step.

## Install

**Termux on Android.** Download a prebuilt binary from [Releases](https://github.com/cornier-en-branche/marelle/releases), or build on the phone. Step by step: [docs/TERMUX.md](docs/TERMUX.md).

**Linux, and anywhere else Rust runs.** Rust 1.85 or newer:

```sh
cargo install --locked --git https://github.com/cornier-en-branche/marelle marelle-cli
```

Or clone the repository and run `cargo build --release`. The binary is `target/release/marelle`, about 4 MiB.

## First chat

Keep the key in a file that only you can read, and tell Marelle where it is. Marelle never stores a key in its settings.

```sh
mkdir -p ~/.config/marelle
( umask 077; read -rsp "API key: " key; echo; printf '%s' "$key" > ~/.config/marelle/zai.key )
export MARELLE_PROVIDERS__ZAI__API_KEY_FILE=~/.config/marelle/zai.key

marelle providers                                    # what exists, and whether the key was found
marelle chat -m zai/MODEL "Explain hopscotch in one sentence"
```

Replace `MODEL` with a model name from your provider's list. Other ways to use it:

```sh
marelle chat -m zai/MODEL                            # interactive session: /help, /clear, /model, /system, /exit
git diff | marelle chat -m zai/MODEL "Review this:" -   # "-" stands for what was piped in
marelle chat -p anthropic -m MODEL --show-usage --show-reasoning "Why is the sky blue?"
```

The answer goes to stdout. Reasoning, token counts, notes and errors go to stderr, so `marelle chat ... > answer.txt` saves only the answer.

Exit codes: `0` success, `1` provider or network error, `2` usage or configuration error.

## Providers

| id | Protocol | Key variable | |
| --- | --- | --- | --- |
| `zai` | OpenAI-compatible | `ZAI_API_KEY` | Z.ai (Zhipu GLM), international |
| `zai_coding` | OpenAI-compatible | `ZAI_API_KEY` | Z.ai GLM Coding Plan |
| `zai_anthropic` | Anthropic | `ZAI_API_KEY` | Z.ai, Anthropic-compatible endpoint |
| `zhipu` | OpenAI-compatible | `ZHIPUAI_API_KEY` | Zhipu BigModel, China |
| `zhipu_coding` | OpenAI-compatible | `ZHIPUAI_API_KEY` | Zhipu BigModel Coding Plan, China |
| `anthropic` | Anthropic | `ANTHROPIC_API_KEY` | Claude |
| `openai` | OpenAI-compatible | `OPENAI_API_KEY` | OpenAI |
| `openrouter` | OpenAI-compatible | `OPENROUTER_API_KEY` | many models behind one key |
| `ollama` | OpenAI-compatible | none | Ollama on this machine |

The Z.ai and Zhipu endpoints come from third-party documentation, not from the vendor's own pages. Check them against your plan, and override any field if they differ.

Every field of a provider can be changed, and a new provider is just a name with an address:

```sh
export MARELLE_PROVIDERS__GATEWAY__BASE_URL=https://llm.example.com/v1
export MARELLE_PROVIDERS__GATEWAY__API_KEY_ENV=GATEWAY_KEY
marelle chat -p gateway -m some-model "hello"
```

Other provider fields: `kind` (`openai-compatible` or `anthropic`), `api_key_file`, `auth` (`bearer`, `x-api-key`, `api-key`, `none`), `max_tokens_field`, `stream_usage` and `extra_body`. If the variable named by `api_key_env` is set, it wins; otherwise the file is read.

## Settings

Settings come in layers: built-in defaults, then environment variables, then flags. Setting files slot in between the defaults and the environment next.

- `MARELLE_CHAT__MODEL=zai/MODEL` sets `chat.model`. A double underscore stands for a dot.
- `marelle config show [prefix]` lists settings and the layer each came from.
- `marelle config explain <key>` shows every layer that set a key, lowest to highest priority.

| Key | Default | Meaning |
| --- | --- | --- |
| `chat.provider`, `chat.model` | none | What to talk to. `chat.model` can be `provider/model`. |
| `chat.system` | none | System prompt |
| `chat.max_tokens`, `chat.temperature` | none | Left to the provider when unset |
| `chat.stream` | `true` | Print the answer as it arrives |
| `chat.show_reasoning`, `chat.show_usage` | `false` | Reasoning and token counts on stderr |
| `network.connect_timeout_secs` | `15` | Time allowed to connect |
| `network.idle_timeout_secs` | `300` | Silence allowed while waiting for data |
| `network.proxy` | `system` | `system`, `none`, or a proxy URL |
| `network.ca_bundle` | none | An extra file of trusted certificate authorities |
| `network.allow_insecure_http` | `false` | Allow a key over plain `http` to a non-local host |

Marelle carries its own list of trusted certificate authorities, so it also works where the system has none, as in Termux. `SSL_CERT_FILE` adds more.

## Your keys stay yours

- Settings name a variable or a file that holds the key, never the key. A setting such as `providers.x.api_key` is refused.
- The key is not printed by `config show`, `config explain` or `providers`, and is removed from any error text that comes back from a server.
- It is sent only to the provider's address, in one header, and never over plain `http` to another machine unless you allow it.
- Redirects are not followed, so a key cannot be forwarded somewhere else.

## Languages

The interface speaks English, Russian, French and Ukrainian. Choose with `--lang <code>` or `MARELLE_LANG`; otherwise `LC_ALL`, `LC_MESSAGES` and `LANG` decide. Every message lives in a catalog, and a test fails if a language misses one. The goal is every language of Eurasia and then more.

## Roadmap

1. **Talk.** Streaming chat with OpenAI-compatible and Anthropic providers. *Done.*
2. **Settings and look.** TOML setting files, global and per project, with `config set`, `get` and `unset` and a description for every key. A themed interface in the spirit of [Hermes Agent](https://github.com/NousResearch/hermes-agent): banner, information panel, framed answers, spinner, in wisteria purple by default with any colour settable as HEX. *Done.*
3. **Many models.** Model aliases, asking several models at once and comparing the answers, fallback chains, and a judge model that merges them. *Done.*
4. **Act.** A tool loop, built-in tools and a permission policy: allow, ask or deny.
5. **Remember.** Sessions and memory.
6. **Extend.** MCP, plugins and skills.
7. **Reach.** Gateways and scheduled tasks.

The reasoning behind the design is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Known limits

- The interactive session has no line editing or history beyond what the terminal gives.
- Ctrl-C ends the program, not just the current answer.
- Tools and the agent loop are not there yet (Act is next).

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Tests never touch the network: they talk to a mock server in `crates/marelle-testkit`. CI also builds the Android binaries and checks the declared minimum Rust version.

## License

[AGPL-3.0-only](LICENSE). Third-party notices are in [NOTICE](NOTICE). Release archives include `THIRD-PARTY-LICENSES.txt` for the Rust crates inside the binary.
