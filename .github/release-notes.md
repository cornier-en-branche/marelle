Marelle, a configurable AI agent for your terminal. This is a pre-release.

**Termux on Android.** Pick the archive for your CPU. Run `uname -m` in Termux: `aarch64` means `aarch64-linux-android`; `armv7l` or `armv8l` means `armv7-linux-androideabi`. Check the download against `SHA256SUMS`, then follow [docs/TERMUX.md](https://github.com/cornier-en-branche/marelle/blob/main/docs/TERMUX.md) (in Russian: [docs/TERMUX.ru.md](https://github.com/cornier-en-branche/marelle/blob/main/docs/TERMUX.ru.md)).

The archives are built by GitHub Actions from the tagged source. They are not yet tested on every device. If something breaks, open an issue with the output of `marelle version` and `uname -m`.

Each archive carries `LICENSE` (AGPL-3.0-only), `NOTICE` and `THIRD-PARTY-LICENSES.txt` with the licences of the Rust crates inside the binary.
