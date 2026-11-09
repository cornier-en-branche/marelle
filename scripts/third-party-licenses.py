#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Marelle contributors
"""Prints the licence notices of the crates that go into a Marelle build.

    scripts/third-party-licenses.py [--target TRIPLE] > THIRD-PARTY-LICENSES.txt

MIT, BSD and Apache-2.0 code may be redistributed in a binary only if its
licence text travels with it. This script reads the locked dependency graph
from `cargo metadata`, collects the licence files that each crate ships, and
prints every distinct text once, followed by the crates it covers. Crates that
ship no licence file are listed separately with the licence they declare.

The graph includes crates that are only used while building, so the list is a
superset of what ends up in the binary. Run it after a build or `cargo fetch`,
so that the crate sources are on disk. The output is deterministic.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys

LICENSE_PREFIXES = ("license", "licence", "copying", "unlicense", "notice")


def cargo_metadata(target):
    command = ["cargo", "metadata", "--locked", "--format-version", "1"]
    if target:
        command += ["--filter-platform", target]
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def third_party_packages(metadata):
    """The external crates in the resolved graph, sorted by name and version."""
    in_graph = {node["id"] for node in metadata["resolve"]["nodes"]}
    packages = [
        package
        for package in metadata["packages"]
        if package["id"] in in_graph and package.get("source") is not None
    ]
    return sorted(packages, key=lambda p: (p["name"], p["version"]))


def licence_files(package):
    folder = os.path.dirname(package["manifest_path"])
    found = []
    for name in sorted(os.listdir(folder)):
        path = os.path.join(folder, name)
        if os.path.isfile(path) and name.lower().startswith(LICENSE_PREFIXES):
            found.append(path)
    return found


def read_text(path):
    with open(path, "rb") as handle:
        text = handle.read().decode("utf-8", errors="replace")
    return text.replace("\r\n", "\n").strip("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--target", help="Rust target triple, e.g. aarch64-linux-android")
    args = parser.parse_args()

    packages = third_party_packages(cargo_metadata(args.target))

    texts = {}  # digest -> [text, [crate labels]]
    without_files = []
    for package in packages:
        label = f"{package['name']} {package['version']}"
        files = licence_files(package)
        if not files:
            without_files.append(package)
            continue
        for path in files:
            text = read_text(path)
            if not text:
                continue
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            entry = texts.setdefault(digest, [text, []])
            if label not in entry[1]:
                entry[1].append(label)

    out = sys.stdout
    rule = "=" * 78
    thin = "-" * 78
    out.write("Third-party software in Marelle\n")
    out.write(rule + "\n\n")
    out.write(
        "Marelle itself is licensed under AGPL-3.0-only (see LICENSE). It is built\n"
        "from the Rust crates below. Each is under the licence it declares; the\n"
        "full texts follow the list.\n\n"
    )
    out.write("Crates (name, version, declared licence)\n" + thin + "\n")
    for package in packages:
        declared = package.get("license") or "see the crate"
        out.write(f"{package['name']} {package['version']}: {declared}\n")

    out.write("\n\nLicence texts\n" + rule + "\n")
    ordered = sorted(texts.values(), key=lambda entry: (entry[1][0], entry[0]))
    for text, labels in ordered:
        out.write("\nUsed by: " + ", ".join(sorted(labels)) + "\n" + thin + "\n")
        out.write(text + "\n")

    if without_files:
        out.write("\n\nCrates that ship no licence file\n" + rule + "\n")
        out.write("Their declared licence and repository are the best record available.\n\n")
        for package in without_files:
            declared = package.get("license") or "not declared"
            repository = package.get("repository") or "no repository listed"
            out.write(f"{package['name']} {package['version']}: {declared} ({repository})\n")


if __name__ == "__main__":
    main()
