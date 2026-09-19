"""Prepare and verify the Windows x64 voice payload using upstream tooling."""

# ruff: noqa: E402 -- upstream script imports require their sibling directory.

import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import struct
import subprocess
import sys
import tempfile
import threading

REPO = Path(__file__).resolve().parents[3]
TARGET = "x86_64-pc-windows-msvc"
sys.path.insert(0, str(REPO / "third_party/voice"))
sys.path.insert(0, str(REPO / "scripts"))
os.environ.setdefault("CODEX_REPO_ROOT", str(REPO))
from assemble_package import assemble
from package_runtime import runtime_files
from prepare_built_runtime import prepare_built
from release_runtime import seal
from runtime import digest
from windows_runtime import EXTERNAL_IMPORTS
from windows_runtime import inspect
from codex_package.archive import write_archive


def prepare(work: Path, commit: str, redist: Path) -> None:
    status = work / "status.txt"
    status.write_text(f"STABLE_GIT_COMMIT {commit}\n", encoding="utf-8")
    runtime = work / "runtime"
    prepare_built(
        work / "native/prefix",
        work / "native/built.json",
        status,
        TARGET,
        runtime,
        sdk_output=work / "sdk",
    )
    # Upstream's development projection treats this redistributable as external.
    # Ship the licensed VS copy beside the helper rather than require an install.
    source = redist / "vcruntime140.dll"
    binary = inspect(source, TARGET)
    if not set(binary.imports).issubset(EXTERNAL_IMPORTS):
        raise ValueError("VC runtime requires additional private dependencies")
    destination = runtime / "bin/vcruntime140.dll"
    shutil.copy2(source, destination)
    manifest_path = runtime / "runtime.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["libraries"].append(
        {
            "path": "bin/vcruntime140.dll",
            "sourcePath": "Microsoft.VC143.CRT/vcruntime140.dll",
            "sourceSha256": digest(source),
            "sha256": digest(destination),
            "imports": list(binary.imports),
        }
    )
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    seal(runtime, TARGET)


def verify_app_identity(package: Path, commit: str) -> None:
    # exec-server and the TUI share this executable's compiled BuildInfo stamp.
    with tempfile.TemporaryDirectory(prefix="voice identity ") as home:
        process = subprocess.Popen(
            [package / "bin/codex.exe", "exec-server", "--listen", "stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env={**os.environ, "CODEX_HOME": home},
        )
        replies = queue.Queue()
        threading.Thread(
            target=lambda: replies.put(process.stdout.readline()), daemon=True
        ).start()
        try:
            request = {
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientName": "voice-package-check",
                    "resumeSessionId": None,
                },
            }
            process.stdin.write(json.dumps(request) + "\n")
            process.stdin.flush()
            response = json.loads(replies.get(timeout=30))
            actual = (
                response.get("result", {}).get("environmentInfo", {}).get("providerId")
            )
            expected = (
                "sha256:"
                + hashlib.sha256(f"git:{commit}:{TARGET}".encode()).hexdigest()
            )
            if actual != expected:
                raise RuntimeError(
                    "App and voice helper must have the same compiled commit"
                )
        finally:
            process.stdin.close()
            process.stdin = None
            try:
                process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()


def smoke(package: Path, commit: str) -> None:
    """Relocated helper startup and plugin loading, without opening audio devices."""
    runtime_files(package / "codex-resources/voice", TARGET, public_release=True)
    verify_app_identity(package, commit)
    environment = dict(os.environ)
    environment.update(
        {
            "GST_PLUGIN_PATH": "",
            "GST_PLUGIN_PATH_1_0": "",
            "GST_PLUGIN_SYSTEM_PATH": "",
            "GST_PLUGIN_SYSTEM_PATH_1_0": "",
            "GST_REGISTRY": "NUL",
            "GST_REGISTRY_UPDATE": "no",
            "GST_REGISTRY_FORK": "no",
            "PATH": str(Path(os.environ["SystemRoot"]) / "System32"),
        }
    )
    helper = package / "codex-resources/voice/bin/codex-voice-host.exe"
    process = subprocess.Popen(
        [helper],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        cwd=package,
    )
    replies = queue.Queue()

    def read():
        try:
            while header := process.stdout.read(4):
                (length,) = struct.unpack(">I", header)
                if length > 128 * 1024:
                    raise ValueError("invalid helper frame")
                replies.put(json.loads(process.stdout.read(length)))
        except Exception as error:
            replies.put(error)
        finally:
            replies.put(None)

    threading.Thread(target=read, daemon=True).start()
    try:
        for request, response in (
            ({"type": "hello", "protocol": 1, "buildCommit": commit}, "ready"),
            ({"type": "initializeRuntime"}, "runtimeReady"),
            ({"type": "close"}, "closed"),
        ):
            payload = json.dumps(request).encode()
            process.stdin.write(struct.pack(">I", len(payload)) + payload)
            process.stdin.flush()
            reply = replies.get(timeout=60)
            if reply != {"type": response}:
                raise RuntimeError(
                    f"Voice helper did not acknowledge {request['type']}: {reply}"
                )
        if process.wait(timeout=10) != 0:
            raise RuntimeError("Voice helper exited unsuccessfully")
    finally:
        if process.poll() is None:
            process.kill()
        process.communicate()


def package(app: Path, work: Path, output: Path, archive: Path, commit: str) -> None:
    version = json.loads((app / "codex-package.json").read_text(encoding="utf-8"))[
        "version"
    ]
    assemble(
        app,
        work / "codex-voice-host.exe",
        TARGET,
        commit,
        output,
        runtime=work / "runtime",
        release_version=version,
    )
    with tempfile.TemporaryDirectory(prefix="codex voice moved ") as temporary:
        moved = Path(temporary) / "package"
        shutil.copytree(output, moved)
        smoke(moved, commit)
    write_archive(output, archive, force=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    prepare_parser = commands.add_parser("prepare")
    prepare_parser.add_argument("--redist", type=Path, required=True)
    package_parser = commands.add_parser("package")
    for name in ("app", "output", "archive"):
        package_parser.add_argument(f"--{name}", type=Path, required=True)
    for command in (prepare_parser, package_parser):
        command.add_argument("--work", type=Path, required=True)
        command.add_argument("--commit", required=True)
    args = parser.parse_args()
    if args.operation == "prepare":
        prepare(args.work.resolve(), args.commit, args.redist.resolve())
    else:
        package(
            args.app.resolve(),
            args.work.resolve(),
            args.output.resolve(),
            args.archive.resolve(),
            args.commit,
        )
