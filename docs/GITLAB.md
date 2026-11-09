# GitLab mirror and CI

GitHub remains the canonical development repository for now:

- Source: https://github.com/cornier-en-branche/marelle
- Active development branch: `feature/native-terminal-ui`

The repository includes a `.gitlab-ci.yml` pipeline so a GitLab copy can run the same core Rust checks. The pipeline runs formatting, Clippy, the workspace test suite, the Rust 1.85 minimum-supported-version check, and a release build. Android/Termux cross-builds remain covered by GitHub Actions because they require the Android NDK setup already maintained in `.github/workflows/ci.yml`.

## Create or connect the GitLab project

A GitLab project must exist before repository mirroring can be configured. Do not create a second independent development history: use GitHub as the source of truth and configure the GitLab project as a **pull mirror**.

1. Create an empty GitLab project named `marelle` under the intended namespace. Do not initialize it with a README, license, or other files.
2. Open **Settings → Repository → Mirroring repositories**.
3. Add `https://github.com/cornier-en-branche/marelle.git` as the mirror URL.
4. Select **Pull** as the direction and configure authentication if the GitHub repository is private.
5. Trigger an update and verify that `main` and `feature/native-terminal-ui` appear.
6. Open **Build → Pipelines** and confirm the pipeline passes on the mirrored branch.

GitLab pull-mirror availability and limits depend on the GitLab offering and project permissions. If pull mirroring is unavailable, use a controlled push from the canonical GitHub repository to GitLab rather than enabling bidirectional mirroring.

## Source-of-truth policy

- Develop and review changes on GitHub.
- Treat GitLab as a read-only mirror unless the project owner explicitly changes this policy.
- Never develop independently on both remotes; divergence can stop mirroring or overwrite refs.
- Do not place API keys or access tokens in repository files, CI variables printed to logs, or remote URLs committed to Git.
