# Changelog

All notable changes are listed here, newest first. The project follows [Semantic Versioning](https://semver.org/), and while the version starts with `0.`, anything may change.

## Unreleased

### Added

- Update check: when an interactive session starts, Marelle asks GitHub and GitLab for a newer version and mentions it once in the conversation, with the release page. Releases are asked for first, then tags; every failure stays silent. Sources and the timeout are settings: `update.check`, `update.github_repo`, `update.gitlab_repo`, `update.timeout_secs`.

### Changed

- The name a person sees is always `Marelle`: the version line and the error prefixes now use the capital, like the banner and the help do. Command forms such as `marelle chat` stay lowercase.

## 0.1.0-alpha.1

The first release that talks to a model.

### Added

- `marelle chat`: one question, a pipe (`-` stands for standard input), or an interactive session with `/help`, `/clear`, `/model`, `/system` and `/exit`.
- Streaming answers from OpenAI-compatible endpoints and the Anthropic Messages API, with optional display of the model's reasoning and of token counts.
- Provider presets: OpenAI, Anthropic, Z.ai (three endpoints), Zhipu BigModel (two), OpenRouter and Ollama. Any field can be overridden and new providers added through settings.
- `marelle providers`, `marelle config show` and `marelle config explain`.
- Network settings: timeouts, proxy, extra certificate authorities, and a guard against sending a key over plain `http`.
- Interface languages: English, Russian, French and Ukrainian.
- Android builds for Termux (`aarch64` and `armv7`) produced by CI, with checksums and third-party licenses in each archive.
- Termux guide in English and Russian.

### Security

- API keys are held in a type that cannot be printed, sent in a sensitive header, removed from server-supplied error text, never followed across redirects, and refused as a setting value.
