#!/usr/bin/env python3
"""Validate the complete release set and emit deterministic aggregate metadata."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


TARGETS = {
    "x86_64-unknown-linux-gnu": "tar.gz",
    "aarch64-apple-darwin": "tar.gz",
    "x86_64-pc-windows-msvc": "zip",
}
HEX = re.compile(r"^[0-9a-f]{64}$")


def fail(message: str) -> None:
    raise SystemExit(f"prepare-release-assets: {message}")


def read_hex(path: Path) -> str:
    value = path.read_text(encoding="ascii").strip().lower()
    if not HEX.fullmatch(value):
        fail(f"invalid digest in {path.name}")
    return value


def read_sum(path: Path, expected_name: str) -> str:
    parts = path.read_text(encoding="ascii").strip().split()
    if len(parts) != 2 or parts[1].lstrip("*") != expected_name:
        fail(f"invalid checksum line in {path.name}")
    value = parts[0].lower()
    if not HEX.fullmatch(value):
        fail(f"invalid digest in {path.name}")
    return value


def prepare(directory: Path, version: str, tag: str, commit: str) -> None:
    if tag != f"v{version}":
        fail(f"tag {tag!r} does not match version {version!r}")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        fail("commit must be a full lowercase SHA-1")

    archives = [f"arandu-{version}-{target}.{extension}" for target, extension in TARGETS.items()]
    expected = {
        name + suffix
        for name in archives
        for suffix in ("", ".blake3", ".blake3sum", ".sha256", ".sha256sum")
    }
    actual = {path.name for path in directory.iterdir() if path.is_file()}
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        fail(f"release set mismatch; missing={missing}, extra={extra}")

    assets = []
    blake_lines = []
    sha_lines = []
    for target, extension in TARGETS.items():
        name = f"arandu-{version}-{target}.{extension}"
        archive = directory / name
        blake3 = read_hex(directory / f"{name}.blake3")
        if read_sum(directory / f"{name}.blake3sum", name) != blake3:
            fail(f"BLAKE3 sidecars disagree for {name}")
        sha256 = read_hex(directory / f"{name}.sha256")
        if read_sum(directory / f"{name}.sha256sum", name) != sha256:
            fail(f"SHA-256 sidecars disagree for {name}")
        actual_sha256 = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual_sha256 != sha256:
            fail(f"SHA-256 does not match archive bytes for {name}")
        assets.append(
            {
                "archive": name,
                "blake3": blake3,
                "sha256": sha256,
                "size": archive.stat().st_size,
                "target": target,
            }
        )
        blake_lines.append(f"{blake3}  {name}\n")
        sha_lines.append(f"{sha256}  {name}\n")

    manifest = {
        "assets": assets,
        "commit": commit,
        "schema": 1,
        "tag": tag,
        "version": version,
    }
    (directory / "release-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n"
    )
    (directory / "BLAKE3SUMS").write_text("".join(blake_lines), encoding="ascii", newline="\n")
    (directory / "SHA256SUMS").write_text("".join(sha_lines), encoding="ascii", newline="\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    prepare(args.directory, args.version, args.tag, args.commit)


if __name__ == "__main__":
    main()
