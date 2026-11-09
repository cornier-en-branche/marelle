# Marelle on Termux

English | [Русский](TERMUX.ru.md)

Marelle runs in [Termux](https://termux.dev/en/), the terminal for Android. This page shows how to install it, give it an API key, and fix the problems you are most likely to meet.

> **Status.** The Android binaries are built by CI in this repository, and CI checks that they compile, link and have the right architecture. Nobody has run them on a phone yet, so the first reports matter. If anything goes wrong, open an issue with the output of `marelle version` and `uname -m`.

## What you need

- Android 7.0 or newer. The binaries target API level 24.
- Termux, installed from [F-Droid](https://github.com/termux/termux-app#f-droid) or [GitHub](https://github.com/termux/termux-app#github).
- An API key from a provider, for example Z.ai.

## Install a prebuilt binary

Find out which build fits your phone:

```sh
uname -m
```

`aarch64` is nearly every phone made in the last several years. `armv7l` and `armv8l` mean a 32-bit system. The matching archive names are `aarch64-linux-android` and `armv7-linux-androideabi`. For `x86_64` (an emulator or a Chromebook) build on the device, as described in the next section.

Then download, verify and install. Take the newest version from the [Releases page](https://github.com/cornier-en-branche/marelle/releases).

```sh
VERSION=v0.1.0-alpha.1
ARCH=aarch64-linux-android        # or armv7-linux-androideabi
BASE=https://github.com/cornier-en-branche/marelle/releases/download/$VERSION

cd ~
curl -fLO "$BASE/marelle-$VERSION-$ARCH.tar.gz"
curl -fLO "$BASE/marelle-$VERSION-$ARCH.tar.gz.sha256"
sha256sum -c "marelle-$VERSION-$ARCH.tar.gz.sha256"
tar xzf "marelle-$VERSION-$ARCH.tar.gz"
install -m 755 "marelle-$VERSION-$ARCH/marelle" "$PREFIX/bin/marelle"
marelle version
```

`sha256sum` must answer `OK`. If it does not, delete the files and download again.

## Or build it on the phone

This works when there is no release for your CPU, or when you want to build from a newer commit.

```sh
pkg update && pkg upgrade
pkg install rust git
termux-wake-lock
cargo install --locked --root "$PREFIX" --git https://github.com/cornier-en-branche/marelle marelle-cli
marelle version
```

- Use the Termux package. `rustup` does not work on Termux, and `pkg install rust` also brings the C compiler (`clang`) that the build needs.
- `termux-wake-lock` keeps Android from suspending Termux when the screen turns off. Release it with `termux-wake-unlock`.
- The build compiles about a hundred crates. Expect a while, and a warm phone. Plug it in.
- If the build runs out of memory, use fewer jobs and skip link-time optimisation:

  ```sh
  CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_LTO=off \
    cargo install --locked --root "$PREFIX" --git https://github.com/cornier-en-branche/marelle marelle-cli
  ```

If the build dies with `[Process completed (signal 9)]`, see Troubleshooting below.

## Give it your key

Marelle never keeps a key in its settings. It reads the key from an environment variable or from a file that you name. In Termux, a file in your home directory is private to the app.

```sh
mkdir -p ~/.config/marelle
( umask 077; read -rsp "Z.ai API key: " key; echo; printf '%s' "$key" > ~/.config/marelle/zai.key )
```

This asks for the key without showing it, so it does not end up in the shell history. Paste it and press Enter.

Make the settings permanent. These lines hold no secret:

```sh
cat >> ~/.bashrc <<'EOF'
export MARELLE_PROVIDERS__ZAI__API_KEY_FILE="$HOME/.config/marelle/zai.key"
export MARELLE_CHAT__PROVIDER=zai
export MARELLE_CHAT__MODEL=MODEL
EOF
source ~/.bashrc
```

Replace `MODEL` with a model name from your provider's list. `marelle providers` shows every provider that Marelle knows and whether its key was found. With a GLM Coding Plan key, use the provider `zai_coding` instead of `zai`, and name the key file with `MARELLE_PROVIDERS__ZAI_CODING__API_KEY_FILE`.

Settings are read from environment variables for now. Setting files are the next step on the [roadmap](ARCHITECTURE.md#roadmap).

## First chat

```sh
marelle providers                              # which providers exist, and whether their key was found
marelle chat "Explain hopscotch in one sentence"
marelle chat                                   # interactive: /help lists the commands, /exit quits
git diff | marelle chat "Review this diff:" -  # "-" stands for what was piped in
```

The answer goes to stdout. Reasoning (`--show-reasoning`), token counts (`--show-usage`), notes and errors go to stderr, so `marelle chat "..." > answer.txt` saves only the answer.

The interface speaks English, Russian, French and Ukrainian: `MARELLE_LANG=ru marelle help`.

## Limits of this version

- Settings come from the environment and flags only.
- The interactive session has no line editing or history beyond what the terminal offers.
- Ctrl-C ends the program, not just the current answer.
- Chat only: no tools, no agent loop yet.

## Troubleshooting

**`marelle: command not found`.** Check that `$PREFIX/bin/marelle` exists. If you installed with `cargo install` and no `--root`, the binary is in `~/.cargo/bin`.

**`Exec format error` or `cannot execute binary file`.** This is the build for another CPU. Compare `uname -m` with the archive name.

**`Cannot talk to the provider`.** Check that the phone is online and that a VPN or a proxy is not in the way. Marelle carries its own list of trusted certificate authorities, so it does not use Android's. Behind a proxy that inspects HTTPS, point Marelle at the proxy's certificate with `export SSL_CERT_FILE=/path/to/ca.pem`, and at the proxy itself with `export MARELLE_NETWORK__PROXY=http://host:port`.

**The build is killed with `signal 9`.** Android's phantom process killer ends long-running background processes. On Android 14 and later, turn on Developer Options and choose "Disable child process restrictions". Android 12 and 13 need an ADB command instead; this [guide](https://cosyra.com/guides/termux-signal-9-fix.html) explains it. The prebuilt binary avoids the problem.

**A key was rejected (HTTP 401).** Check the key and the account behind it. `marelle providers` shows where each provider looks for its key.

## Update or remove

To update, repeat the install steps with the new version. To remove Marelle, run `rm "$PREFIX/bin/marelle"`. Delete `~/.config/marelle` too if you want the key files gone.
