#!/usr/bin/env python3
"""Create a byte-deterministic Windows SDK zip from a staged tree."""

import argparse
from datetime import datetime, timezone
from pathlib import Path
import sys
import zipfile
import json
import posixpath
import stat


def validate(path: Path, root: str, version: str, target: str) -> None:
    seen: set[str] = set()
    with zipfile.ZipFile(path) as archive:
        for entry in archive.infolist():
            name = entry.filename
            normalized = posixpath.normpath(name)
            if (
                name != normalized
                or name.startswith("/")
                or "\\" in name
                or normalized == ".."
                or normalized.startswith("../")
            ):
                raise SystemExit(f"unsafe zip entry: {name}")
            unix_type = stat.S_IFMT(entry.external_attr >> 16)
            if unix_type not in (0, stat.S_IFREG):
                raise SystemExit(f"unsupported zip entry type: {name}")
            folded = name.casefold()
            if folded in seen:
                raise SystemExit(f"duplicate zip entry: {name}")
            seen.add(folded)
            if not name.startswith(f"{root}/"):
                raise SystemExit(f"entry outside package root: {name}")
            relative = name[len(root) + 1:]
            allowed = relative in {"bin/arandu.exe", "bin/arandu_cli.exe", "bin/arandu-lsp.exe", f"lib/{target}/arandu_runtime.lib", "BLAKE3SUMS", "LICENSE-MIT", "LICENSE-APACHE", "release-manifest.json"} or relative.startswith("share/arandu/stdlib/")
            if not allowed:
                raise SystemExit(f"unexpected zip content: {name}")
        required = ["bin/arandu.exe", "bin/arandu_cli.exe", "bin/arandu-lsp.exe", f"lib/{target}/arandu_runtime.lib", "BLAKE3SUMS", "LICENSE-MIT", "LICENSE-APACHE", "release-manifest.json"]
        missing = [item for item in required if f"{root}/{item}".casefold() not in seen]
        if missing:
            raise SystemExit(f"zip missing required entries: {', '.join(missing)}")
        manifest = json.loads(archive.read(f"{root}/release-manifest.json"))
        expected = {"schema": 1, "version": version, "target": target, "components": ["arandu", "arandu-lsp", "runtime", "stdlib"], "archive": "zip"}
        if manifest != expected:
            raise SystemExit(f"release manifest mismatch: {manifest!r}")
        if not any(name.startswith(f"{root}/share/arandu/stdlib/") and name.endswith(".aru") for name in (entry.filename for entry in archive.infolist())):
            raise SystemExit("zip contains no stdlib .aru files")


def main() -> None:
    if len(sys.argv) >= 2 and sys.argv[1] == 'validate':
        parser = argparse.ArgumentParser()
        parser.add_argument('command')
        parser.add_argument('archive', type=Path)
        parser.add_argument('--root', required=True)
        parser.add_argument('--version', required=True)
        parser.add_argument('--target', required=True)
        args = parser.parse_args()
        validate(args.archive, args.root, args.version, args.target)
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--epoch", required=True, type=int)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    source = args.source.resolve()
    stamp = datetime.fromtimestamp(max(args.epoch, 315532800), timezone.utc)
    date_time = (stamp.year, stamp.month, stamp.day, stamp.hour, stamp.minute, stamp.second)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.output, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in sorted((p for p in source.rglob("*") if p.is_file()), key=lambda p: p.relative_to(source).as_posix()):
            name = f"{source.name}/{path.relative_to(source).as_posix()}"
            info = zipfile.ZipInfo(name, date_time)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = (0o755 if "/bin/" in name else 0o644) << 16
            archive.writestr(info, path.read_bytes(), compresslevel=9)
    validate(args.output, source.name, args.version, args.target)


if __name__ == "__main__":
    main()
