#!/usr/bin/env python3
"""P7-A: exercise only an installed Arandu SDK outside the checkout."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def isolated_environment(tool_bin: Path, home: Path, cache: Path) -> dict[str, str]:
    env = os.environ.copy()
    for name in ("ARANDU_STDLIB", "CARGO", "CARGO_HOME", "RUSTUP_HOME"):
        env.pop(name, None)
    if os.name == "nt":
        system_root = env.get("SystemRoot", r"C:\Windows")
        env["PATH"] = os.pathsep.join(
            [str(tool_bin), str(Path(system_root) / "System32"), system_root]
        )
        env["USERPROFILE"] = str(home)
        env["LOCALAPPDATA"] = str(home / "AppData" / "Local")
    else:
        env["PATH"] = os.pathsep.join([str(tool_bin), "/usr/bin", "/bin"])
        env["HOME"] = str(home)
        env["XDG_CACHE_HOME"] = str(home / ".cache")
    env["ARANDU_CACHE_DIR"] = str(cache)
    return env


def run(
    command: list[str], cwd: Path, env: dict[str, str], expected: int = 0
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True, timeout=60)
    if result.returncode != expected:
        raise SystemExit(
            f"command failed ({result.returncode}, expected {expected}): {command}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def frame(message: dict) -> bytes:
    payload = json.dumps(message, separators=(",", ":")).encode()
    return f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload


def lsp_smoke(server: Path, cwd: Path, env: dict[str, str]) -> None:
    process = subprocess.Popen(
        [str(server)],
        cwd=cwd,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    def send(message: dict) -> None:
        assert process.stdin is not None
        process.stdin.write(frame(message))
        process.stdin.flush()

    def receive(expected_id: int) -> None:
        assert process.stdout is not None
        while True:
            length = None
            while True:
                line = process.stdout.readline()
                if not line:
                    raise SystemExit(f"LSP closed before response {expected_id}")
                if line == b"\r\n":
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":", 1)[1].strip())
            if length is None:
                raise SystemExit("LSP response missing Content-Length")
            response = json.loads(process.stdout.read(length))
            if "method" in response and "id" not in response:
                continue
            if response.get("id") != expected_id or "error" in response:
                raise SystemExit(f"unexpected LSP response: {response!r}")
            return

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {}}})
        receive(1)
        send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        send({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": None})
        receive(2)
        send({"jsonrpc": "2.0", "method": "exit", "params": None})
        assert process.stdin is not None
        process.stdin.close()
        if process.wait(timeout=10) != 0:
            raise SystemExit(f"LSP exited with {process.returncode}")
    finally:
        if process.poll() is None:
            process.kill()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--arandu", required=True, type=Path)
    parser.add_argument("--lsp", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    arandu = args.arandu.resolve()
    lsp = args.lsp.resolve()
    if not arandu.is_file() or not lsp.is_file():
        raise SystemExit("installed CLI/LSP executable is missing")

    with tempfile.TemporaryDirectory(prefix="arandu-s3c-") as directory:
        work = Path(directory)
        home = work / "home"
        cache = work / "cache"
        home.mkdir()
        env = isolated_environment(arandu.parent, home, cache)
        command = "arandu.exe" if os.name == "nt" else "arandu"
        discovered = shutil.which(command, path=env["PATH"])
        if discovered is None or Path(discovered).resolve() != arandu:
            raise SystemExit(
                f"installed CLI is not authoritative on PATH: expected {arandu}, found {discovered}"
            )
        # Windows CreateProcess resolves a bare executable before applying the
        # child-only environment. The lookup above proves PATH authority; use
        # precisely that discovered executable for portable process creation.
        command = str(Path(discovered))

        version = run([command, "--version"], work, env)
        if version.stdout.strip() != f"arandu {args.expected_version}":
            raise SystemExit(
                f"installed CLI version mismatch: expected arandu {args.expected_version!s}, "
                f"found {version.stdout.strip()!r}"
            )
        run([command, "doctor"], work, env)
        created = run([command, "new", "installed_app", "--vcs=none"], work, env)
        if "arandu check" not in created.stdout or "arandu_cli" in created.stdout:
            raise SystemExit("new scaffold exposes an internal command name")
        project = work / "installed_app"
        run([command, "check"], project, env)
        lock = project / "arandu.lock"
        if not lock.is_file() or b"\r" in lock.read_bytes():
            raise SystemExit("installed SDK did not publish a canonical LF-only lockfile")
        run([command, "run"], project, env)
        run([command, "build"], project, env)
        build_states = list((project / "target").rglob("build-state.json"))
        if len(build_states) != 1:
            raise SystemExit(f"expected one native build state, found {build_states!r}")
        build_state = json.loads(build_states[0].read_text(encoding="utf-8"))
        artifact = build_states[0].parent / build_state["artifact"]
        if not artifact.is_file() or build_state.get("backend") != "cranelift-aot":
            raise SystemExit(f"installed SDK did not publish a native artifact: {build_state!r}")
        run([str(artifact)], project, env)
        run([command, "fmt", "src/main.aru"], project, env)
        lsp_smoke(lsp, project, env)
        run([command, "clean"], project, env)
        if (project / "target").exists():
            raise SystemExit("installed SDK clean left owned build artifacts behind")
        cleaned = run([command, "clean"], project, env)
        if "already clean" not in cleaned.stdout:
            raise SystemExit("installed SDK clean is not idempotent")

        corpus = work / "corpus"
        shutil.copytree(args.corpus.resolve(), corpus)
        run([command, "check"], corpus / "small/hello", env)
        run([command, "run"], corpus / "small/hello", env, expected=42)
        run([command, "check"], corpus / "medium/language_mix", env)
        run([command, "run"], corpus / "medium/language_mix", env, expected=42)
        comptime_project = corpus / "medium/comptime_core"
        run([command, "check"], comptime_project, env)
        run([command, "run"], comptime_project, env)
        run([command, "build"], comptime_project, env)
        states = list((comptime_project / "target").rglob("build-state.json"))
        if len(states) != 1:
            raise SystemExit(f"expected one installed CTFE build state, found {states!r}")
        state = json.loads(states[0].read_text(encoding="utf-8"))
        run([str(states[0].parent / state["artifact"])], comptime_project, env)
        cached = run([command, "build"], comptime_project, env)
        if "incremental: up-to-date" not in cached.stdout:
            raise SystemExit("installed CTFE build did not reuse its successful session")
        limited = run([command, "build", "--ctfe-fuel=1"], comptime_project, env, expected=1)
        if "T045" not in limited.stderr:
            raise SystemExit("installed CTFE build bypassed its configured resource limit")
        cycle = run([command, "check"], corpus / "adversarial/import_cycle", env, expected=1)
        if "N006" not in cycle.stderr:
            raise SystemExit("installed corpus diagnostic lost expected N006")
        invalid = run(
            [command, "check"], corpus / "adversarial/unicode_type_error", env, expected=1
        )
        if "T002" not in invalid.stderr:
            raise SystemExit("installed corpus diagnostic lost expected T002")
    print("P7-A installed lifecycle smoke: ok")


if __name__ == "__main__":
    main()
