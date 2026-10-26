#!/usr/bin/env python3
"""Exercise the standalone installer with local archives and an isolated PATH."""

import hashlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

INSTALLER = Path(__file__).with_name("install.sh").resolve()
VERSION = "0.1.11-rc.1"
TREE_NAME = f"arandu-{VERSION}"
TARGET = "x86_64-unknown-linux-gnu"


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="arandu-installer-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.path = self.root / "tools"
        self.path.mkdir()
        self.home = self.root / "home"
        self.home.mkdir()
        self.prefix = self.root / "prefix"
        self.log = self.root / "commands.log"
        self.archive = self.root / "download.tar.gz"
        self.sidecar = self.root / "download.sha256"
        self.tree = self.root / TREE_NAME
        (self.tree / "bin").mkdir(parents=True)
        (self.tree / "share/arandu/stdlib").mkdir(parents=True)
        (self.tree / "lib" / TARGET).mkdir(parents=True)

        # Only listed tools are discoverable. The download and SDK commands are
        # local fixtures; no network or existing Arandu installation is used.
        for command in (
            "awk",
            "tr",
            "mktemp",
            "rm",
            "mkdir",
            "mv",
            "ln",
            "grep",
            "tar",
            "gzip",
        ):
            self.add_tool(command)
        self.write_program("uname", "print('Linux' if '-s' in sys.argv else 'x86_64')")
        self.write_program(
            "curl",
            """
            if '-w' in sys.argv:
                print(os.environ.get('ARANDU_TEST_LATEST_URL', 'https://github.com/arandu-lang/arandu/releases/tag/v0.1.11-rc.1'))
                sys.exit(0)
            output = Path(sys.argv[sys.argv.index('-o') + 1])
            source = 'download.sha256' if sys.argv[-1].endswith('.sha256') else 'download.tar.gz'
            shutil.copyfile(Path(os.environ['ARANDU_TEST_FIXTURES']) / source, output)
            """,
        )
        self.cli = self.tree / "bin/arandu"
        self.cli.write_text(
            self.program(
                f"""
                with open(os.environ['ARANDU_TEST_COMMAND_LOG'], 'a') as log:
                    log.write(' '.join(sys.argv[1:]) + '\\n')
                if sys.argv[1:3] == ['archive', 'validate']:
                    sys.exit(int(os.environ.get('ARANDU_TEST_REJECT_ARCHIVE', '0')))
                elif sys.argv[1:] == ['--version']:
                    print(os.environ.get('ARANDU_TEST_REPORTED_VERSION', 'arandu {VERSION}'))
                elif sys.argv[1] == 'hash-file':
                    # Stand-in for the staged CLI's BLAKE3 command. Cryptographic
                    # correctness belongs to its Rust tests; test ordering here.
                    print(hashlib.sha256(Path(sys.argv[2]).read_bytes()).hexdigest())
                elif sys.argv[1:] == ['doctor']:
                    sys.exit(int(os.environ.get('ARANDU_TEST_DOCTOR_FAILURE', '0')))
                else:
                    sys.exit(2)
                """
            ),
            encoding="utf-8",
        )
        self.cli.chmod(0o755)
        (self.tree / "bin/arandu_cli").symlink_to("arandu")
        (self.tree / "bin/arandu-lsp").write_text("fixture", encoding="ascii")
        (self.tree / "bin/arandu-lsp").chmod(0o755)
        self.payload = self.tree / "share/arandu/stdlib/test.aru"
        self.payload.write_text("fn main() {}\n", encoding="ascii")
        self.sums = self.tree / "BLAKE3SUMS"
        self.sums.write_text(
            hashlib.sha256(self.payload.read_bytes()).hexdigest()
            + "  share/arandu/stdlib/test.aru\n",
            encoding="ascii",
        )
        self.repack()

    @staticmethod
    def program(body):
        return (
            f"#!{sys.executable}\n"
            "import hashlib, os, shutil, signal, subprocess, sys\n"
            "from pathlib import Path\n" + textwrap.dedent(body).lstrip()
        )

    def write_program(self, command, body):
        destination = self.path / command
        if destination.is_symlink():
            destination.unlink()
        destination.write_text(self.program(body), encoding="utf-8")
        destination.chmod(0o755)

    def add_tool(self, command):
        executable = shutil.which(command)
        if executable is None:
            self.skipTest(f"host tool unavailable: {command}")
        (self.path / command).symlink_to(executable)

    def repack(self):
        with tarfile.open(self.archive, "w:gz") as archive:
            archive.add(self.tree, arcname=TREE_NAME)
        self.sidecar.write_text(
            hashlib.sha256(self.archive.read_bytes()).hexdigest() + "\n",
            encoding="ascii",
        )

    def install(self, verifier="sha256sum", **overrides):
        if verifier:
            self.add_tool(verifier)
        environment = {
            "PATH": str(self.path),
            "HOME": str(self.home),
            "PREFIX": str(self.prefix),
            "ARANDU_VERSION": VERSION,
            "ARANDU_TEST_FIXTURES": str(self.root),
            "ARANDU_TEST_COMMAND_LOG": str(self.log),
            **overrides,
        }
        return subprocess.run(
            [shutil.which("bash"), str(INSTALLER)],
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=20,
            check=False,
        )

    def assert_rejected_before_execution(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stdout)
        self.assertFalse(self.log.exists(), result.stdout)
        self.assertFalse(self.prefix.exists(), result.stdout)

    def prepare_previous_install(self):
        previous = self.prefix / TREE_NAME
        previous.mkdir(parents=True)
        (previous / "keep").write_text("previous installation", encoding="ascii")
        (self.prefix / "current").symlink_to(TREE_NAME)
        return previous

    def assert_previous_install(self, previous, result):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual((previous / "keep").read_text(), "previous installation")
        self.assertEqual((self.prefix / "current").readlink(), Path(TREE_NAME))
        self.assertFalse(list(self.prefix.glob("*.old.*")))
        self.assertFalse(list(self.prefix.glob(".staging.*")))

    def test_sha256sum_install_verifies_before_publish(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout)
        calls = self.log.read_text().splitlines()
        self.assertTrue(calls[0].startswith("archive validate "), calls)
        self.assertEqual(calls[1], "--version")
        self.assertTrue(calls[2].startswith("hash-file "), calls)
        self.assertEqual(calls[3], "doctor")
        self.assertTrue((self.prefix / "bin/arandu-lsp").exists())
        self.assertFalse(list(self.prefix.glob(".staging.*")))

    def test_shasum_fallback(self):
        result = self.install("shasum")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_openssl_fallback(self):
        result = self.install("openssl")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_python_fallback(self):
        result = self.install("python3")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_no_verifier_fails_closed(self):
        result = self.install(None)
        self.assert_rejected_before_execution(
            result, "need sha256sum, shasum, openssl, or python3"
        )

    def test_checksum_mismatch(self):
        self.sidecar.write_text("0" * 64 + "\n", encoding="ascii")
        self.assert_rejected_before_execution(
            self.install(), "SHA-256 checksum mismatch"
        )

    def test_malformed_and_missing_checksums(self):
        for contents in (
            "",
            "deadbeef\n",
            "z" * 64,
            "0" * 63,
            "0" * 65,
            "0" * 64 + "\n" + "1" * 64,
        ):
            with self.subTest(contents=contents):
                self.sidecar.write_text(contents, encoding="ascii")
                self.assert_rejected_before_execution(
                    self.install_once(), "malformed SHA-256 sidecar"
                )

    def install_once(self):
        # Reuse a tool between subtests while keeping the isolated PATH intact.
        return self.install(None if (self.path / "sha256sum").exists() else "sha256sum")

    def test_uppercase_checksum_and_filename_sidecar(self):
        digest = self.sidecar.read_text().strip().upper()
        self.sidecar.write_text(f"{digest}  archive.tar.gz\n", encoding="ascii")
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_missing_downloaded_sidecar(self):
        self.sidecar.unlink()
        self.assert_rejected_before_execution(self.install(), "download.sha256")

    def test_hash_tool_failure_does_not_fall_back_to_acceptance(self):
        self.write_program("sha256sum", "sys.exit(9)")
        result = self.install(None)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse(self.log.exists(), result.stdout)

    def test_archive_rejection_preserves_previous_installation(self):
        previous = self.prepare_previous_install()
        result = self.install(ARANDU_TEST_REJECT_ARCHIVE="1")
        self.assert_previous_install(previous, result)
        self.assertFalse(
            any(
                line.startswith("hash-file ")
                for line in self.log.read_text().splitlines()
            )
        )

    def test_wrong_binary_version_is_rejected(self):
        result = self.install(ARANDU_TEST_REPORTED_VERSION="arandu 0.1.0")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("does not match package version", result.stdout)
        self.assertFalse(self.prefix.exists())

    def test_internal_checksum_mismatch_preserves_previous_installation(self):
        previous = self.prepare_previous_install()
        self.payload.write_text("changed after hashing\n", encoding="ascii")
        self.repack()
        result = self.install()
        self.assertIn("BLAKE3SUMS mismatch", result.stdout)
        self.assert_previous_install(previous, result)

    def test_missing_internal_checksums(self):
        self.sums.unlink()
        self.repack()
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("archive missing BLAKE3SUMS", result.stdout)
        self.assertFalse(self.prefix.exists())

    def test_empty_and_unsafe_internal_checksums(self):
        for contents in (
            "",
            "# empty\n",
            "0" * 64 + "  ../outside\n",
            "0" * 64 + "  /outside\n",
            "bad  bin/arandu\n",
        ):
            with self.subTest(contents=contents):
                self.sums.write_text(contents, encoding="ascii")
                self.repack()
                result = self.install_once()
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertRegex(
                    result.stdout, "empty BLAKE3SUMS|malformed BLAKE3SUMS entry"
                )
                self.assertFalse(self.prefix.exists())

    def test_publish_failure_restores_previous_installation(self):
        previous = self.prepare_previous_install()
        result = self.install(ARANDU_TEST_FAIL_PUBLISH="1")
        self.assertIn("previous installation restored", result.stdout)
        self.assert_previous_install(previous, result)

    def test_interrupted_replacement_restores_previous_installation(self):
        previous = self.prepare_previous_install()
        real_mv = shutil.which("mv")
        self.write_program(
            "mv",
            f"""
            result = subprocess.run([{real_mv!r}, *sys.argv[1:]], check=False)
            if result.returncode == 0 and '.old.' in sys.argv[-1]:
                os.kill(os.getppid(), signal.SIGTERM)
            sys.exit(result.returncode)
            """,
        )
        result = self.install()
        self.assertEqual(result.returncode, 143, result.stdout)
        self.assert_previous_install(previous, result)

    def test_failed_doctor_is_not_reported_as_success(self):
        result = self.install(ARANDU_TEST_DOCTOR_FAILURE="1")
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertNotIn("successfully installed", result.stdout)

    def test_invalid_version_rejected_before_download(self):
        result = self.install(ARANDU_VERSION="../invalid")
        self.assert_rejected_before_execution(result, "invalid Arandu version")

    def test_default_version_resolves_stable_release_tag(self):
        result = self.install(ARANDU_VERSION="")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_default_version_with_wget(self):
        (self.path / "curl").unlink()
        self.write_program(
            "wget",
            """
            if '--spider' in sys.argv:
                sys.stderr.write('  Location: https://github.com/arandu-lang/arandu/releases/tag/v0.1.11-rc.1 [following]\\n')
                sys.exit(0)
            output = Path(sys.argv[sys.argv.index('-O') + 1])
            source = 'download.sha256' if sys.argv[-1].endswith('.sha256') else 'download.tar.gz'
            shutil.copyfile(Path(os.environ['ARANDU_TEST_FIXTURES']) / source, output)
            """,
        )
        result = self.install(ARANDU_VERSION="")
        self.assertEqual(result.returncode, 0, result.stdout)

    def test_default_version_rejects_unexpected_redirect(self):
        result = self.install(
            ARANDU_VERSION="", ARANDU_TEST_LATEST_URL="https://example.invalid/v0.1.11"
        )
        self.assert_rejected_before_execution(result, "versioned Arandu tag")


if __name__ == "__main__":
    unittest.main()
