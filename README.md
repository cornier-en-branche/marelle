```text
 +---+---+---+---+---+---+---+
 | m | a | r | e | l | l | e |
 +---+---+---+---+---+---+---+
```

**A configurable AI agent for your terminal.**
Bring your own API keys: Z.ai (Zhipu GLM), Anthropic, OpenAI, OpenRouter, a local Ollama, or any OpenAI-compatible server. Linux and Termux first.

English | [Русский](README.ru.md)

> **Pre-alpha, version 0.1.0-alpha.1.** Native TUI, multi-provider chat, a permission-aware agent loop, local tools and resumable local sessions are implemented. APIs and settings may still change. This is a prototype, not feature parity with OpenCode or Hermes.

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
marelle config set chat.provider zai
marelle config set chat.model glm-5.2
marelle                                                # open the interactive terminal UI
marelle chat -m zai/MODEL "Explain hopscotch in one sentence"
```

Replace `MODEL` with a model name from your provider's list. Other ways to use it:

```sh
marelle                                                # interactive TUI (when chat.model is configured)
marelle --tui                                           # explicit TUI launch
marelle --continue                                      # resume the most recent saved conversation
marelle chat -q "Explain this repository"               # one prompt, then exit
marelle chat -m zai/MODEL                               # interactive TUI with a one-time model override
git diff | marelle chat -m zai/MODEL "Review this:" -   # "-" stands for what was piped in
marelle chat -p anthropic -m MODEL --show-usage "Why is the sky blue?"
```

The answer goes to stdout. Reasoning, token counts, notes and errors go to stderr, so `marelle chat ... > answer.txt` saves only the answer.

## Agent tools

Marelle sends structured tool definitions through OpenAI-compatible and Anthropic APIs. The built-in tool set is:

| Tool | Purpose |
| --- | --- |
| `terminal` | Run a shell command from the launch directory |
| `read_file`, `write_file`, `edit_file` | Read and modify UTF-8 text files |
| `list_directory`, `search_text` | Inspect and search project files |
| `fetch_url` | Fetch a public HTTP(S) page as text (requires `curl`) |
| `list_skills`, `load_skill` | Discover and load local `SKILL.md` instructions |

Use `/tools` to inspect the tool list and `/skills` to list available skills. Project guidance is loaded from `AGENTS.md`, `CLAUDE.md`, `SOUL.md`, `.cursorrules` and selected `.marelle/` files when they are inside allowed paths.

File tools are confined to `permissions.allowed_paths` (default: the current project directory) and respect `permissions.denied_paths`. Reading is allowed by default; file changes, shell commands and network fetches ask before running. The shell permission applies to host commands and is not a filesystem sandbox: keep it at `ask` unless you deliberately trust the model with command execution.

The agent stops after `agent.max_turns` tool round-trips. Tool responses are treated as untrusted content. Requests use complete model responses so tool arguments are not assembled from incomplete stream fragments.

## Sessions

Conversations are saved locally by default, including tool-call history and model/session settings. On Unix the session directory is mode `0700` and the JSON file is mode `0600`. The file is stored at `$XDG_STATE_HOME/marelle/sessions.json`, or `~/.local/state/marelle/sessions.json` when `XDG_STATE_HOME` is unset. API keys are never written to this file. Disable persistence with:

```sh
marelle config set agent.persist_sessions false
```

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
| `chat.stream` | `true` | Stream the optional judge response; tool-enabled agent turns currently use complete responses so tool calls stay intact |
| `chat.show_reasoning`, `chat.show_usage` | `false` | Reasoning and token counts on stderr |
| `agent.max_turns` | `20` | Maximum tool round-trips per user request |
| `agent.persist_sessions` | `true` | Save local conversations between runs |
| `permissions.shell` | `ask` | Shell command execution: `allow`, `ask`, `deny` |
| `permissions.files_read` | `allow` | File reads: `allow`, `ask`, `deny` |
| `permissions.files_write` | `ask` | File changes: `allow`, `ask`, `deny` |
| `permissions.network` | `ask` | URL fetching: `allow`, `ask`, `deny` |
| `permissions.allowed_paths` | `["."]` | Allowed roots for file tools |
| `permissions.denied_paths` | none | Explicitly blocked file paths |
| `network.connect_timeout_secs` | `15` | Time allowed to connect |
| `network.idle_timeout_secs` | `300` | Silence allowed while waiting for data |
| `network.proxy` | `system` | `system`, `none`, or a proxy URL |
| `network.ca_bundle` | none | An extra file of trusted certificate authorities |
| `network.allow_insecure_http` | `false` | Allow a key over plain `http` to a non-local host |
| `update.check` | `true` | Ask GitHub and GitLab for a newer version when an interactive session starts |
| `update.github_repo` | `cornier-en-branche/marelle` | GitHub `owner/repo` the check asks for the newest release |
| `update.gitlab_repo` | none | GitLab `owner/repo` the check also asks. Empty disables GitLab |
| `update.timeout_secs` | `3` | Seconds the check may spend on one source |

Marelle carries its own list of trusted certificate authorities, so it also works where the system has none, as in Termux. `SSL_CERT_FILE` adds more.

When a newer version is found, the session mentions it once — the version, what you have, and where to get it. A release is asked for first; a repository that only tags its commits falls back to its newest tag. Anything that goes wrong stays silent.

## Your keys stay yours

- Settings name a variable or a file that holds the key, never the key. A setting such as `providers.x.api_key` is refused.
- The key is not printed by `config show`, `config explain` or `providers`, and is removed from any error text that comes back from a server.
- It is sent only to the provider's address, in one header, and never over plain `http` to another machine unless you allow it.
- Redirects are not followed, so a key cannot be forwarded somewhere else.

## Languages

The interface speaks English, Russian, French and Ukrainian. Choose with `--lang <code>` or `MARELLE_LANG`; otherwise `LC_ALL`, `LC_MESSAGES` and `LANG` decide. Every message lives in a catalog, and a test fails if a language misses one. The goal is every language of Eurasia and then more.

## Roadmap

1. **Talk.** OpenAI-compatible and Anthropic adapters, with both SSE streaming and complete-response decoding. Tool-enabled agent turns currently use complete responses. *Done.*
2. **Settings and look.** TOML setting files, global and per project, with `config set`, `get` and `unset` and a description for every key. A themed interface in the spirit of [Hermes Agent](https://github.com/NousResearch/hermes-agent): banner, information panel, framed answers, spinner, in wisteria purple by default with any colour settable as HEX. *Done.*
3. **Many models.** Model aliases, asking several models at once and comparing the answers, fallback chains, and a judge model that merges them. *Done.*
4. **Act.** Structured tool calling for OpenAI-compatible and Anthropic providers, local file/search/terminal tools, URL fetching, local skills and allow/ask/deny permissions, with the transcript showing every tool call and its result. *Done.*
5. **Remember.** Named conversations persist locally between runs. Long-term semantic memory is still ahead.
6. **Extend.** MCP servers, external plugins, subagents and richer skills support.
7. **Reach.** Browser automation, messaging gateways and scheduled tasks.

The reasoning behind the design is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Known limits

- Provider and model selectors list configured providers and model aliases. Marelle does not yet discover remote model catalogs.
- Selectors support mouse click-to-focus and wheel scrolling; the transcript and input editor remain keyboard-driven.
- Ctrl-C exits the interactive session. It does not yet cancel only the current response.
- The agent loop, permission-aware local tools and persistent local sessions are implemented, but Marelle remains a pre-alpha prototype; see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the current design and boundaries.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Tests never touch the network: they talk to a mock server in `crates/marelle-testkit`. GitHub Actions also builds the Android binaries and checks the declared minimum Rust version. A GitLab CI pipeline is provided for a GitLab mirror; see [docs/GITLAB.md](docs/GITLAB.md).

## License

[AGPL-3.0-only](LICENSE). Third-party notices are in [NOTICE](NOTICE). Release archives include `THIRD-PARTY-LICENSES.txt` for the Rust crates inside the binary.
