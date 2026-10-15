#!/usr/bin/env python3
"""Bisect a trusted fuzz reproducer in an isolated POSIX Git worktree.

Commands are JSON argv arrays, never shell snippets. Reproducer exit status:
0 = good, 1 = the specific bug reproduced, 125 = inconclusive. Other failures
abort rather than accidentally attributing an unrelated failure to a commit.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path


def git(repository: Path, *args: str) -> str:
    env = os.environ.copy()
    env["LC_ALL"] = "C"
    return subprocess.check_output(
        ["git", "-C", str(repository), *args],
        text=True,
        stderr=subprocess.PIPE,
        env=env,
    ).strip()


def command(value: str) -> list[str]:
    try:
        argv = json.loads(value)
    except json.JSONDecodeError as error:
        raise argparse.ArgumentTypeError("expected a JSON argv array") from error
    if (
        not isinstance(argv, list)
        or not argv
        or not all(isinstance(arg, str) for arg in argv)
    ):
        raise argparse.ArgumentTypeError("expected a nonempty JSON array of strings")
    if not argv[0] or any("\0" in arg for arg in argv):
        raise argparse.ArgumentTypeError("argv contains an empty executable or NUL")
    return argv


def run_phase(argv: list[str], cwd: Path, log: Path, timeout: float, env: dict) -> dict:
    """Stream output to disk and always reap the isolated process group."""
    try:
        with log.open("wb") as output:
            child = subprocess.Popen(
                argv,
                cwd=cwd,
                env=env,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            timed_out = False
            try:
                child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
            finally:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait()
        return {"exit_code": child.returncode, "timeout": timed_out, "log": str(log)}
    except OSError as error:
        return {"error": str(error), "timeout": False, "log": str(log)}


def evaluate(config: dict, worktree: Path, deadline: float) -> int:
    commit = git(worktree, "rev-parse", "HEAD")
    attempt = Path(config["output"]) / f"attempt-{commit[:12]}-{uuid.uuid4().hex[:8]}"
    attempt.mkdir()
    env = os.environ.copy()
    env["ARANDU_BISECT_ARTIFACT"] = config["artifact"]
    env["ARANDU_BISECT_COMMIT"] = commit
    # Separate target output from any caller override and reuse it only inside
    # this disposable worktree. The caller's builds and checkout stay untouched.
    env["CARGO_TARGET_DIR"] = str(worktree / "target")
    record = {"commit": commit}
    build = run_phase(
        config["build"],
        worktree,
        attempt / "build.log",
        max(0.001, min(config["build_timeout"], deadline - time.monotonic())),
        env,
    )
    record["build"] = build
    if "error" in build:
        result = 128
    elif build["timeout"] or build["exit_code"] != 0:
        result = 125
    else:
        repro = run_phase(
            config["repro"],
            worktree,
            attempt / "repro.log",
            max(0.001, min(config["repro_timeout"], deadline - time.monotonic())),
            env,
        )
        record["repro"] = repro
        if repro["timeout"]:
            result = 125
        elif "error" in repro or repro["exit_code"] not in (0, 1, 125):
            result = 128
        else:
            result = repro["exit_code"]
    record["classification"] = {0: "good", 1: "bad", 125: "skip", 128: "abort"}[result]
    (attempt / "result.json").write_text(
        json.dumps(record, indent=2) + "\n", encoding="utf-8"
    )
    print(f"{commit}: {record['classification']}", flush=True)
    return result


def bisect(args: argparse.Namespace) -> int:
    if os.name != "posix":
        raise ValueError(
            "this tool requires POSIX process groups; Windows needs a Job Object runner"
        )
    repository = Path(git(args.repository, "rev-parse", "--show-toplevel"))
    good = git(
        repository,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{args.good}^{{commit}}",
    )
    bad = git(
        repository,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{args.bad}^{{commit}}",
    )
    if good == bad:
        raise ValueError("good and bad must be different commits")
    git(repository, "merge-base", "--is-ancestor", good, bad)
    artifact = args.artifact.resolve(strict=True)
    if not artifact.is_file():
        raise ValueError("artifact must be a regular reproducer file (source or seed)")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    # Keep the original independent of historical worktree checkouts.
    saved_artifact = output / f"reproducer{artifact.suffix}"
    shutil.copyfile(artifact, saved_artifact)
    with saved_artifact.open("rb") as stream:
        artifact_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    config = {
        "good": good,
        "bad": bad,
        "artifact": str(saved_artifact),
        "artifact_sha256": artifact_sha256,
        "output": str(output),
        "build": args.build_command,
        "repro": args.repro_command,
        "build_timeout": args.build_timeout,
        "repro_timeout": args.repro_timeout,
    }
    (output / "config.json").write_text(
        json.dumps(config, indent=2) + "\n", encoding="utf-8"
    )
    report = {"status": "aborted", "good": good, "bad": bad, "first_bad": None}
    try:
        with tempfile.TemporaryDirectory(prefix="arandu-smith-bisect-") as temporary:
            worktree = Path(temporary) / "checkout"
            git(repository, "worktree", "add", "--detach", str(worktree), bad)
            try:
                deadline = time.monotonic() + args.total_timeout
                # Endpoints are executed, not assumed. A stale or nondiagnostic
                # reproducer must not launch a misleading history search.
                if evaluate(config, worktree, deadline) != 1:
                    raise ValueError("bad endpoint did not reproduce the specific bug")
                git(worktree, "checkout", "--detach", good)
                if evaluate(config, worktree, deadline) != 0:
                    raise ValueError("good endpoint did not pass the reproducer")
                progress = git(worktree, "bisect", "start", bad, good)
                with (output / "bisect.log").open("w", encoding="utf-8") as log:
                    while True:
                        log.write(progress + "\n")
                        history = git(worktree, "bisect", "log")
                        (output / "bisect-history.txt").write_text(
                            history + "\n", encoding="utf-8"
                        )
                        # Git writes this completion marker to the replay log.
                        # LC_ALL=C above makes it independent of the host locale.
                        if any(
                            marker in history
                            for marker in (
                                "# first 'bad' commit: [",
                                "# first bad commit: [",
                            )
                        ):
                            report["status"] = "identified"
                            report["first_bad"] = git(
                                worktree, "rev-parse", "refs/bisect/bad"
                            )
                            break
                        if time.monotonic() >= deadline:
                            report["status"] = "inconclusive"
                            report["reason"] = "total timeout"
                            break
                        result = evaluate(config, worktree, deadline)
                        if result == 128:
                            raise ValueError(
                                "reproducer failed outside the declared 0/1/125 protocol"
                            )
                        try:
                            progress = git(
                                worktree,
                                "bisect",
                                {0: "good", 1: "bad", 125: "skip"}[result],
                            )
                        except subprocess.CalledProcessError as error:
                            log.write(error.output + "\n")
                            (output / "bisect-history.txt").write_text(
                                git(worktree, "bisect", "log") + "\n", encoding="utf-8"
                            )
                            report["status"] = "inconclusive"
                            report["reason"] = (
                                "Git could not isolate a commit; inspect skipped revisions"
                            )
                            break
            finally:
                git(repository, "worktree", "remove", "--force", str(worktree))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        report["reason"] = str(error)
        raise
    finally:
        (output / "report.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )
    print(json.dumps(report, indent=2))
    return 0 if report["status"] == "identified" else 2


def positive_seconds(value: str) -> float:
    import math

    seconds = float(value)
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("timeout must be a finite positive number")
    return seconds


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--good")
    parser.add_argument("--bad")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--build-command", type=command)
    parser.add_argument("--repro-command", type=command)
    parser.add_argument("--build-timeout", type=positive_seconds, default=600)
    parser.add_argument("--repro-timeout", type=positive_seconds, default=30)
    parser.add_argument("--total-timeout", type=positive_seconds, default=3600)
    args = parser.parse_args()
    for name in ("good", "bad", "artifact", "output", "build_command", "repro_command"):
        if getattr(args, name) is None:
            parser.error(f"--{name.replace('_', '-')} is required")
    try:
        return bisect(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"bisect-smith: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
