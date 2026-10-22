#!/usr/bin/env python3
"""Negative cases for scripts/install-from-tarball.sh.

The fake `bin/arandu` implements archive validate, --version, hash-file and
doctor. Hash output is SHA-256 so the test does not need a built Arandu binary;
the installer compares that output with BLAKE3SUMS.
"""

import hashlib
import os
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

INSTALLER = Path(__file__).with_name("install-from-tarball.sh").resolve()
VERSION = "0.1.11-rc.1"
TARGET = "x86_64-unknown-linux-gnu"
TREE_NAME = f"arandu-{VERSION}"
ARCHIVE_NAME = f"arandu-{VERSION}-{TARGET}.tar.gz"


class TarballInstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="arandu-tarball-install-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.prefix = self.root / "prefix"
        self.profile = self.root / "profile"
        self.tree = self.root / TREE_NAME
        self.archive = self.root / ARCHIVE_NAME
        self.payload = self.tree / "share/arandu/stdlib/test.aru"
        self._build_tree()
        self.repack()

    def _build_tree(self):
        if self.tree.exists():
            subprocess.run(["rm", "-rf", str(self.tree)], check=True)
        (self.tree / "bin").mkdir(parents=True)
        (self.tree / "share/arandu/stdlib").mkdir(parents=True)
        (self.tree / "lib" / TARGET).mkdir(parents=True)
        cli = self.tree / "bin/arandu"
        cli.write_text(
            "#!" + sys.executable + "\n"
            + textwrap.dedent(
                f"""
                import hashlib, sys
                from pathlib import Path
                if sys.argv[1:3] == ["archive", "validate"]:
                    sys.exit(0)
                if sys.argv[1:] == ["--version"]:
                    print("arandu {VERSION}")
                elif sys.argv[1] == "hash-file":
                    print(hashlib.sha256(Path(sys.argv[2]).read_bytes()).hexdigest())
                elif sys.argv[1:] == ["doctor"]:
                    sys.exit(0)
                else:
                    sys.exit(2)
                """
            ).lstrip(),
            encoding="utf-8",
        )
        cli.chmod(0o755)
        lsp = self.tree / "bin/arandu-lsp"
        lsp.write_text("fixture\n", encoding="ascii")
        lsp.chmod(0o755)
        (self.tree / "lib" / TARGET / "libarandu_runtime.a").write_bytes(b"runtime")
        self.payload.write_text("fn main() {}\n", encoding="ascii")
        self.write_sums()

    def write_sums(self, digest=None):
        payload_hash = digest or hashlib.sha256(self.payload.read_bytes()).hexdigest()
        (self.tree / "BLAKE3SUMS").write_text(
            f"{payload_hash}  share/arandu/stdlib/test.aru\n",
            encoding="ascii",
        )

    def repack(self):
        if self.archive.exists():
            self.archive.unlink()
        with tarfile.open(self.archive, "w:gz") as archive:
            archive.add(self.tree, arcname=TREE_NAME)

    def install(self):
        env = os.environ.copy()
        env.update(
            {
                "PREFIX": str(self.prefix),
                "ARANDU_ALLOW_UNVERIFIED": "1",
                "ARANDU_NO_MODIFY_PATH": "1",
                "ARANDU_PROFILE": str(self.profile),
            }
        )
        return subprocess.run(
            ["bash", str(INSTALLER), str(self.archive)],
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )

    def test_missing_empty_directory_and_symlink_checksums_are_rejected(self):
        self._assert_case(self._missing, "archive missing regular BLAKE3SUMS")
        self._assert_case(self._empty, "empty BLAKE3SUMS")
        self._assert_case(self._directory, "archive missing regular BLAKE3SUMS")
        self._assert_case(self._symlink, "archive missing regular BLAKE3SUMS")

    def _missing(self):
        self._build_tree()
        (self.tree / "BLAKE3SUMS").unlink()
        self.repack()

    def _empty(self):
        self._build_tree()
        (self.tree / "BLAKE3SUMS").write_text("", encoding="ascii")
        self.repack()

    def _directory(self):
        self._build_tree()
        sums = self.tree / "BLAKE3SUMS"
        sums.unlink()
        sums.mkdir()
        self.repack()

    def _symlink(self):
        self._build_tree()
        sums = self.tree / "BLAKE3SUMS"
        target = self.tree / "sums-target"
        target.write_text(sums.read_text(encoding="ascii"), encoding="ascii")
        sums.unlink()
        sums.symlink_to(target.name)
        self.assertTrue(sums.is_symlink())
        self.repack()

    def _assert_case(self, prepare, expected):
        prepare()
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(expected, result.stdout)
        self.assertFalse((self.prefix / TREE_NAME).exists(), result.stdout)

    def test_malformed_entry_and_payload_tamper_keep_the_previous_prefix(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout)
        marker = self.prefix / TREE_NAME / "MARKER"
        marker.write_text("keep\n", encoding="ascii")

        self._build_tree()
        original = hashlib.sha256(self.payload.read_bytes()).hexdigest()
        self.payload.write_text("tampered\n", encoding="ascii")
        self.write_sums(digest=original)
        self.repack()
        tampered = self.install()
        self.assertNotEqual(tampered.returncode, 0, tampered.stdout)
        self.assertIn("BLAKE3SUMS mismatch", tampered.stdout)
        self.assertEqual(marker.read_text(encoding="ascii"), "keep\n")

        self._build_tree()
        (self.tree / "BLAKE3SUMS").write_text(
            f"{'0' * 64}  ../outside\n",
            encoding="ascii",
        )
        self.repack()
        malformed = self.install()
        self.assertNotEqual(malformed.returncode, 0, malformed.stdout)
        self.assertIn("malformed BLAKE3SUMS entry", malformed.stdout)
        self.assertEqual(marker.read_text(encoding="ascii"), "keep\n")


if __name__ == "__main__":
    unittest.main()
