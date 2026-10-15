#!/usr/bin/env python3
"""Exercise real Git histories without touching the developer's repository."""

import argparse
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("bisect_smith.py")
SPEC = importlib.util.spec_from_file_location("bisect_smith", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


@unittest.skipUnless(os.name == "posix", "bisect runner requires POSIX process groups")
class BisectSmithTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="test-smith-bisect-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repository"
        self.repo.mkdir()
        self.git("init", "--quiet")
        self.git("config", "user.email", "bisect-test@example.invalid")
        self.git("config", "user.name", "Bisect test")
        self.artifact = self.root / "original.seed"
        self.artifact.write_text("exact reproducer\n", encoding="utf-8")

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-C", str(self.repo), *args], text=True, stderr=subprocess.PIPE
        ).strip()

    def revision(self, bug=False, buildable=True, exit_code=None):
        (self.repo / "build.py").write_text(
            f"raise SystemExit({0 if buildable else 1})\n", encoding="utf-8"
        )
        expected = 1 if bug else 0
        (self.repo / "repro.py").write_text(
            "import os\nfrom pathlib import Path\n"
            "assert Path(os.environ['ARANDU_BISECT_ARTIFACT']).read_text() == 'exact reproducer\\n'\n"
            f"raise SystemExit({expected if exit_code is None else exit_code})\n",
            encoding="utf-8",
        )
        self.git("add", "build.py", "repro.py")
        self.git("commit", "--quiet", "--allow-empty", "-m", "test revision")
        return self.git("rev-parse", "HEAD")

    def run_bisect(self, good, bad, *extra):
        output = self.root / "report"
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--repository",
                str(self.repo),
                "--good",
                good,
                "--bad",
                bad,
                "--artifact",
                str(self.artifact),
                "--output",
                str(output),
                "--build-command",
                json.dumps([sys.executable, "build.py"]),
                "--repro-command",
                json.dumps([sys.executable, "repro.py"]),
                *extra,
            ],
            capture_output=True,
            check=False,
            text=True,
            timeout=30,
        )
        return result, json.loads((output / "report.json").read_text(encoding="utf-8"))

    def test_identifies_first_bad_and_preserves_dirty_checkout(self):
        good = self.revision()
        self.revision()
        culprit = self.revision(bug=True)
        self.revision(bug=True)
        bad = self.revision(bug=True)
        (self.repo / "build.py").write_text("local dirty contents\n", encoding="utf-8")
        (self.repo / "untracked.txt").write_text("keep\n", encoding="utf-8")
        status = self.git("status", "--porcelain")
        worktrees = self.git("worktree", "list", "--porcelain")
        result, report = self.run_bisect(good, bad)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(report["first_bad"], culprit)
        self.assertEqual(self.git("rev-parse", "HEAD"), bad)
        self.assertEqual(self.git("status", "--porcelain"), status)
        self.assertEqual(self.git("worktree", "list", "--porcelain"), worktrees)
        self.assertEqual(
            (self.root / "report" / "reproducer.seed").read_bytes(),
            self.artifact.read_bytes(),
        )

    def test_unbuildable_revision_is_skip_and_ambiguity_has_no_culprit(self):
        good = self.revision()
        self.revision(buildable=False)
        bad = self.revision(bug=True)
        result, report = self.run_bisect(good, bad)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertEqual(report["status"], "inconclusive")
        self.assertIsNone(report["first_bad"])
        attempts = [
            json.loads(path.read_text())
            for path in (self.root / "report").glob("attempt-*/result.json")
        ]
        self.assertIn("skip", [attempt["classification"] for attempt in attempts])
        self.assertEqual(len(self.git("worktree", "list").splitlines()), 1)

    def test_adjacent_endpoints_are_both_validated(self):
        good = self.revision()
        bad = self.revision(bug=True)
        result, report = self.run_bisect(good, bad)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(report["first_bad"], bad)
        self.assertEqual(
            len(list((self.root / "report").glob("attempt-*/result.json"))), 2
        )

    def test_rejects_nonreproducing_bad_endpoint(self):
        good = self.revision()
        bad = self.revision()
        result, report = self.run_bisect(good, bad)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(report["status"], "aborted")
        self.assertIn("bad endpoint", result.stderr)
        self.assertEqual(len(self.git("worktree", "list").splitlines()), 1)

    def test_unrelated_reproducer_failure_aborts_instead_of_marking_bad(self):
        good = self.revision()
        self.revision(exit_code=2)
        bad = self.revision(bug=True)
        result, report = self.run_bisect(good, bad)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(report["status"], "aborted")
        self.assertIsNone(report["first_bad"])
        self.assertIn("0/1/125", result.stderr)

    def test_timeout_is_skip_and_child_is_reaped(self):
        outcome = MODULE.run_phase(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            self.repo,
            self.root / "timeout.log",
            0.05,
            os.environ.copy(),
        )
        self.assertTrue(outcome["timeout"])
        self.assertLess(outcome["exit_code"], 0)

    def test_timeout_also_terminates_descendant(self):
        marker = self.root / "orphan-wrote-this"
        descendant = f"import time; from pathlib import Path; time.sleep(0.4); Path({str(marker)!r}).touch()"
        parent = (
            "import subprocess, sys, time; "
            f"subprocess.Popen([sys.executable, '-c', {descendant!r}]); time.sleep(60)"
        )
        outcome = MODULE.run_phase(
            [sys.executable, "-c", parent],
            self.repo,
            self.root / "tree.log",
            0.1,
            os.environ.copy(),
        )
        self.assertTrue(outcome["timeout"])
        time.sleep(0.45)
        self.assertFalse(
            marker.exists(), "descendant survived the process-group timeout"
        )

    def test_argv_is_data_and_requires_an_array(self):
        self.assertEqual(
            MODULE.command('["echo", "$(touch no); & text"]'),
            ["echo", "$(touch no); & text"],
        )
        for malformed in (
            '"echo hello"',
            "[]",
            "[1]",
            '["", "arg"]',
            '["echo", "\\u0000"]',
        ):
            with self.assertRaises(argparse.ArgumentTypeError):
                MODULE.command(malformed)


if __name__ == "__main__":
    unittest.main()
