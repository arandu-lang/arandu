#!/usr/bin/env python3

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


_assets_path = Path(__file__).with_name("prepare_release_assets.py")
if not _assets_path.exists():
    _assets_path = Path(__file__).parent / "oracles" / "prepare_release_assets.py"
SPEC = importlib.util.spec_from_file_location("prepare_release_assets", _assets_path)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class PrepareReleaseAssetsTests(unittest.TestCase):
    def create_set(self, root: Path, version: str = "0.1.0-rc.1") -> None:
        for target, extension in MODULE.TARGETS.items():
            name = f"arandu-{version}-{target}.{extension}"
            contents = name.encode()
            (root / name).write_bytes(contents)
            sha256 = hashlib.sha256(contents).hexdigest()
            blake3 = "a" * 64
            (root / f"{name}.sha256").write_text(sha256 + "\n", encoding="ascii")
            (root / f"{name}.sha256sum").write_text(f"{sha256}  {name}\n", encoding="ascii")
            (root / f"{name}.blake3").write_text(blake3 + "\n", encoding="ascii")
            (root / f"{name}.blake3sum").write_text(f"{blake3}  {name}\n", encoding="ascii")

    def test_emits_deterministic_manifest_and_aggregate_sums(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.create_set(root)
            MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0-rc.1", "b" * 40)
            first = (root / "release-manifest.json").read_bytes()
            manifest = json.loads(first)
            self.assertEqual([asset["target"] for asset in manifest["assets"]], list(MODULE.TARGETS))
            (root / "release-manifest.json").unlink()
            (root / "BLAKE3SUMS").unlink()
            (root / "SHA256SUMS").unlink()
            MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0-rc.1", "b" * 40)
            self.assertEqual((root / "release-manifest.json").read_bytes(), first)

    def test_rejects_missing_or_extra_assets(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.create_set(root)
            (root / "extra.txt").write_text("no")
            with self.assertRaises(SystemExit):
                MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0-rc.1", "b" * 40)

    def test_rejects_archive_digest_mismatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.create_set(root)
            archive = next(path for path in root.iterdir() if path.suffix == ".zip")
            archive.write_bytes(b"tampered")
            with self.assertRaises(SystemExit):
                MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0-rc.1", "b" * 40)

    def test_rejects_tag_or_sidecar_disagreement(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.create_set(root)
            with self.assertRaises(SystemExit):
                MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0", "b" * 40)
            sidecar = next(root.glob("*.blake3sum"))
            sidecar.write_text(f"{'c' * 64}  wrong-name\n", encoding="ascii")
            with self.assertRaises(SystemExit):
                MODULE.prepare(root, "0.1.0-rc.1", "v0.1.0-rc.1", "b" * 40)


if __name__ == "__main__":
    unittest.main()
