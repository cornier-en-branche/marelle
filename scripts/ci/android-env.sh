#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Marelle contributors
#
# Prepares the Android NDK toolchain for one Rust target.
#
# GitHub's Ubuntu runners ship the NDK, so no download is needed there. On a
# runner the variables are appended to $GITHUB_ENV; anywhere else they are
# printed as `export` lines, so you can use it from a shell with `eval`:
#
#   eval "$(ANDROID_NDK_HOME=/path/to/ndk scripts/ci/android-env.sh aarch64-linux-android)"
#   cargo build --release --target aarch64-linux-android -p marelle-cli
#
# Usage: android-env.sh <rust-target> [api-level]
# The default API level is 24 (Android 7.0), the oldest that Termux supports.

set -euo pipefail

target="${1:?usage: android-env.sh <rust-target> [api-level]}"
api="${2:-24}"

ndk="${ANDROID_NDK_LATEST_HOME:-${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}}"
if [ -z "$ndk" ] || [ ! -d "$ndk" ]; then
  echo "android-env: the Android NDK was not found. Set ANDROID_NDK_HOME." >&2
  exit 1
fi

bin="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"

# The NDK names its compilers after the Android ABI, which differs from the
# Rust target name on 32-bit ARM.
case "$target" in
  aarch64-linux-android)   clang="aarch64-linux-android${api}-clang" ;;
  armv7-linux-androideabi) clang="armv7a-linux-androideabi${api}-clang" ;;
  x86_64-linux-android)    clang="x86_64-linux-android${api}-clang" ;;
  i686-linux-android)      clang="i686-linux-android${api}-clang" ;;
  *)
    echo "android-env: unsupported target: $target" >&2
    exit 1
    ;;
esac

if [ ! -x "$bin/$clang" ]; then
  echo "android-env: $bin/$clang does not exist or is not executable." >&2
  exit 1
fi

# cc-rs reads CC_<target> and AR_<target> (dashes become underscores); Cargo
# reads CARGO_TARGET_<TARGET>_LINKER (upper case).
lower="${target//-/_}"
upper="$(printf '%s' "$lower" | tr '[:lower:]' '[:upper:]')"

emit() {
  if [ -n "${GITHUB_ENV:-}" ]; then
    printf '%s=%s\n' "$1" "$2" >> "$GITHUB_ENV"
  else
    printf 'export %s=%q\n' "$1" "$2"
  fi
}

emit "CC_${lower}" "$bin/$clang"
emit "AR_${lower}" "$bin/llvm-ar"
emit "CARGO_TARGET_${upper}_LINKER" "$bin/$clang"
emit "ANDROID_TOOLCHAIN_BIN" "$bin"
