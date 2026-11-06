#!/usr/bin/env python3

import hashlib
import base64
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import threading
import unittest
import zipfile


SCRIPT_DIR = Path(__file__).parent
TAG = "codex-plus-plus-v1.2.3-fork.1"
VERSION = TAG.removeprefix("codex-plus-plus-v")


def is_supported_host() -> bool:
    if os.name == "nt":
        architecture = os.environ.get("PROCESSOR_ARCHITEW6432") or os.environ.get(
            "PROCESSOR_ARCHITECTURE", ""
        )
        return architecture.lower() in {"amd64", "x86_64"}
    host = os.uname()
    return (host.sysname, host.machine.lower()) in {
        ("Darwin", "arm64"),
        ("Darwin", "aarch64"),
        ("Linux", "x86_64"),
        ("Linux", "amd64"),
    }


class ReleaseFixture:
    def __init__(self, root: Path, *, windows: bool = os.name == "nt") -> None:
        self.requests: list[str] = []
        self.target = "x86_64-pc-windows-msvc" if windows else platform_target()
        suffix = "zip" if windows else "tar.gz"
        self.archive_name = f"codex-plus-plus-{VERSION}-{self.target}.{suffix}"
        self.installer_name = f"install-codex-plus-plus.{'ps1' if windows else 'sh'}"
        installer = (SCRIPT_DIR / self.installer_name).read_bytes()
        archive = make_archive(root / "package", self.archive_name, windows=windows)
        self.root = root
        self.npm = None
        self.github = True
        self.assets = {
            self.installer_name: installer,
            f"{self.installer_name}.sha256": checksum(self.installer_name, installer),
            self.archive_name: archive,
            f"{self.archive_name}.sha256": checksum(self.archive_name, archive),
        }

        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                fixture.requests.append(self.path)
                if self.path.startswith("/npm/") and fixture.npm is not None:
                    body = (
                        json.dumps(fixture.npm, indent=2).replace("/", "\\/").encode()
                    )
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(body)
                    return
                if self.path == "/payload.tgz" and "payload.tgz" in fixture.assets:
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(fixture.assets["payload.tgz"])
                    return
                if not fixture.github:
                    self.send_error(404)
                    return
                if self.path == "/releases/latest":
                    self.send_response(302)
                    self.send_header("Location", f"{fixture.url}/releases/tag/{TAG}")
                    self.end_headers()
                    return
                if self.path == f"/releases/tag/{TAG}":
                    self.send_response(200)
                    self.end_headers()
                    return
                prefix = f"/releases/download/{TAG}/"
                asset = fixture.assets.get(self.path.removeprefix(prefix))
                if self.path.startswith(prefix) and asset is not None:
                    self.send_response(200)
                    self.send_header("Content-Length", str(len(asset)))
                    self.end_headers()
                    self.wfile.write(asset)
                    return
                self.send_error(404)

            def log_message(self, format: str, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def publish_npm(
        self, version: str = VERSION, *, installer: bytes | None = None
    ) -> None:
        # Ship the real activator, exactly as package assembly does.
        import sys

        sys.path.insert(0, str(SCRIPT_DIR.parent))
        from codex_package.codex_plus_plus.install import add_installer

        package = self.root / "package"
        if not self.target.endswith("msvc"):
            (package / "bin/codex").write_text(f"#!/bin/sh\necho npm-{version}\n")
        add_installer(package, windows=self.target.endswith("msvc"))
        if installer is not None:
            (package / "codex-resources/install" / self.installer_name).write_bytes(
                installer
            )
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            archive.add(package, f"package/vendor/{self.target}")
        payload = output.getvalue()
        self.assets["payload.tgz"] = payload
        platform = {
            "x86_64-pc-windows-msvc": "win32-x64",
            "aarch64-apple-darwin": "darwin-arm64",
        }.get(self.target, "linux-x64")
        self.npm = {
            "irrelevant": {"version": "9.9.9-fork.9", "dist": {"tarball": "wrong"}},
            "version": f"{version}-{platform}",
            "dist": {
                "tarball": self.url + "/payload.tgz",
                "integrity": "sha512-"
                + base64.b64encode(hashlib.sha512(payload).digest()).decode(),
            },
        }

    def __enter__(self) -> "ReleaseFixture":
        self.thread.start()
        return self

    def __exit__(self, *args: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


@unittest.skipUnless(is_supported_host(), "unsupported Codex++ release target")
class InstallLatestTest(unittest.TestCase):
    def test_npm_without_github_preserves_data_and_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            with ReleaseFixture(root) as release:
                first = run_installer(root, release)
                self.assertEqual(first.returncode, 0, first.stderr)
                home = root / "codex-home"
                data = {
                    "auth.json": b"accounts",
                    "config.toml": b"config",
                    "history.jsonl": b"history",
                    "state.sqlite": b"untouched",
                }
                for name, content in data.items():
                    (home / name).write_bytes(content)
                release.publish_npm("1.2.4-fork.2")
                release.github = False
                fallback = root / "old-activator.sh"
                fallback.write_text("#!/bin/sh\nexit 42\n")
                result = run_installer(
                    root, release, {"CODEX_PLUS_PLUS_INSTALLER_PATH": str(fallback)}
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(
                    {name: (home / name).read_bytes() for name in data}, data
                )
                pointer = root / "shim/.codex-plus-plus-current"
                previous = pointer.read_bytes()
                refused = run_installer(
                    root, release, {"CODEX_PLUS_PLUS_CURRENT_VERSION": "1.2.5-fork.1"}
                )
                self.assertNotEqual(refused.returncode, 0)
                self.assertEqual(pointer.read_bytes(), previous)
                if os.name != "nt":
                    result = subprocess.run(
                        [str(root / "shim/codex"), "npm-working"],
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.stdout.strip(), "npm-1.2.4-fork.2")

    def test_npm_availability_falls_back_but_integrity_failure_does_not(self) -> None:
        for failure in (
            "missing tarball",
            "missing verifier",
            "bad integrity",
            "missing integrity",
            "malformed archive",
        ):
            with (
                self.subTest(failure=failure),
                tempfile.TemporaryDirectory() as temp_dir,
            ):
                root = Path(temp_dir)
                with ReleaseFixture(root) as release:
                    first = run_installer(root, release)
                    self.assertEqual(first.returncode, 0, first.stderr)
                    pointer = root / "shim/.codex-plus-plus-current"
                    previous = pointer.read_bytes()
                    release.requests.clear()
                    release.publish_npm()
                    extra_env = {}
                    if failure == "missing tarball":
                        del release.assets["payload.tgz"]
                    elif failure == "missing verifier":
                        if os.name == "nt":
                            continue
                        tools = root / "tools"
                        tools.mkdir()
                        for name in "sh cat sleep uname curl tar gzip sha256sum shasum awk mktemp rm fold grep dirname sed ps cp mv date cksum tr basename chmod mkdir".split():
                            if executable := shutil.which(name):
                                (tools / name).symlink_to(executable)
                        extra_env["PATH"] = str(tools)
                    elif failure == "missing integrity":
                        del release.npm["dist"]["integrity"]
                    elif failure == "bad integrity":
                        release.assets["payload.tgz"] = b"corrupted download"
                    else:
                        release.assets["payload.tgz"] = b"not a tar"
                        release.npm["dist"]["integrity"] = (
                            "sha512-"
                            + base64.b64encode(
                                hashlib.sha512(b"not a tar").digest()
                            ).decode()
                        )
                    result = run_installer(root, release, extra_env)
                    if failure in ("missing tarball", "missing verifier"):
                        self.assertEqual(result.returncode, 0, result.stderr)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertEqual(pointer.read_bytes(), previous)
                        self.assertFalse(
                            any(
                                "/releases/download/" in path
                                for path in release.requests
                            )
                        )

    def test_newest_usable_platform_wins(self) -> None:
        for version, incomplete in (
            ("1.2.2-fork.9", False),
            ("1.2.2-fork.9", True),
            ("1.2.3-fork.1", False),
            ("1.2.4-fork.1", False),
        ):
            with (
                self.subTest(version=version, incomplete=incomplete),
                tempfile.TemporaryDirectory() as temp_dir,
            ):
                root = Path(temp_dir)
                with ReleaseFixture(root) as release:
                    release.publish_npm(version)
                    if incomplete:
                        del release.assets[release.archive_name]
                    result = run_installer(root, release)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(
                        "/payload.tgz" in release.requests,
                        version != "1.2.2-fork.9" or incomplete,
                    )
                    if os.name != "nt":
                        working = subprocess.run(
                            [str(root / "shim/codex"), "github"],
                            capture_output=True,
                            text=True,
                        )
                        self.assertEqual(
                            working.stdout.strip(),
                            "npm-" + version
                            if version != "1.2.2-fork.9" or incomplete
                            else "github",
                        )

    def test_verified_release_installs_idempotently(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            with ReleaseFixture(root) as release:
                first = run_installer(root, release)
                second = run_installer(root, release)

            self.assertEqual(first.returncode, 0, first.stderr)
            self.assertEqual(second.returncode, 0, second.stderr)
            shim_dir = root / "shim"
            self.assertTrue((shim_dir / platform_shim_name()).is_file())
            self.assertTrue((shim_dir / ".codex-plus-plus-current").is_file())
            downloaded = {f"/releases/download/{TAG}/{name}" for name in release.assets}
            self.assertLessEqual(downloaded, set(release.requests))

    def test_checksum_failure_does_not_mutate_installation(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            with ReleaseFixture(root) as release:
                release.assets[f"{release.archive_name}.sha256"] = checksum(
                    release.archive_name, b"wrong archive"
                )
                result = run_installer(root, release)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SHA-256 mismatch", result.stderr)
            self.assertFalse((root / "shim").exists())
            self.assertFalse((root / "codex-home").exists())

    def test_incomplete_platform_or_bad_checksum_preserves_working_install(
        self,
    ) -> None:
        for failure in ("missing archive", "missing checksum", "bad checksum"):
            with (
                self.subTest(failure=failure),
                tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir,
            ):
                root = Path(temp_dir)
                with ReleaseFixture(root) as release:
                    first = run_installer(root, release)
                    self.assertEqual(first.returncode, 0, first.stderr)
                    shim = root / "shim"
                    before = {
                        str(p.relative_to(shim)): p.read_bytes()
                        for p in shim.rglob("*")
                        if p.is_file()
                    }
                    if failure == "missing archive":
                        del release.assets[release.archive_name]
                    elif failure == "missing checksum":
                        del release.assets[f"{release.archive_name}.sha256"]
                    else:
                        release.assets[f"{release.archive_name}.sha256"] = checksum(
                            release.archive_name, b"bad"
                        )
                    result = run_installer(root, release)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(
                        "SHA-256 mismatch"
                        if failure == "bad checksum"
                        else "Could not download required",
                        result.stderr,
                    )
                    self.assertEqual(
                        {
                            str(p.relative_to(shim)): p.read_bytes()
                            for p in shim.rglob("*")
                            if p.is_file()
                        },
                        before,
                    )
                    # Invoke the preserved global shim, not just its generation pointer.
                    command = (
                        [str(shim / platform_shim_name()), "/c", "echo still-working"]
                        if os.name == "nt"
                        else [str(shim / platform_shim_name()), "still-working"]
                    )
                    working = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(working.returncode, 0, working.stderr)
                    self.assertIn("still-working", working.stdout)

    def test_unsupported_target_fails_before_network_or_mutation(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            with ReleaseFixture(root) as release:
                env = unsupported_target_env(root)
                result = run_installer(root, release, env)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Unsupported Codex++ install target", result.stderr)
            self.assertEqual(release.requests, [])
            self.assertFalse((root / "shim").exists())


def run_installer(
    root: Path,
    release: ReleaseFixture,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.update(
        {
            "CODEX_HOME": str(root / "codex-home"),
            "CODEX_PLUS_PLUS_RELEASE_BASE_URL": release.url,
            "CODEX_PLUS_PLUS_NPM_REGISTRY_URL": release.url + "/npm",
            "HOME": str(root / "home"),
        }
    )
    env.update(extra_env or {})
    script = SCRIPT_DIR / f"install-codex-plus-plus-latest.{platform_script_suffix()}"
    command = (
        [
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
            "-ShimDir",
            str(root / "shim"),
        ]
        if os.name == "nt"
        else ["sh", str(script), "--shim-dir", str(root / "shim")]
    )
    return subprocess.run(command, capture_output=True, check=False, env=env, text=True)


@unittest.skipUnless(
    os.name != "nt" and shutil.which("pwsh"), "PowerShell acquisition harness on Unix"
)
class PowerShellAcquisitionTest(unittest.TestCase):
    def test_npm_verification_fallback_and_pending_exit(self) -> None:
        for mode in (
            "npm offline github",
            "missing payload",
            "bad integrity",
            "pending",
        ):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as temp_dir:
                root = Path(temp_dir)
                with ReleaseFixture(root, windows=True) as release:
                    target_activator = (
                        "param($TargetExe, $ShimDir, [switch]$Install)\n"
                        "[IO.File]::WriteAllText($env:ACTIVATED, $TargetExe)\n"
                        + ("exit 75\n" if mode == "pending" else "exit 0\n")
                    ).encode()
                    release.publish_npm(installer=target_activator)
                    release.assets[release.installer_name] = target_activator
                    release.assets[release.installer_name + ".sha256"] = checksum(
                        release.installer_name, target_activator
                    )
                    if mode == "npm offline github":
                        release.github = False
                    elif mode == "missing payload":
                        del release.assets["payload.tgz"]
                    elif mode == "bad integrity":
                        release.assets["payload.tgz"] = b"corrupt"
                    marker = root / "activated"
                    activator = root / "activate.ps1"
                    activator.write_text("exit 42\n")
                    tar = root / "tar.exe"
                    tar.write_text('#!/bin/sh\nexec tar "$@"\n')
                    tar.chmod(0o755)
                    env = {
                        **os.environ,
                        "OS": "Windows_NT",
                        "PROCESSOR_ARCHITECTURE": "AMD64",
                        "PROCESSOR_ARCHITEW6432": "",
                        "LOCALAPPDATA": str(root),
                        "PATH": str(root) + os.pathsep + os.environ["PATH"],
                        "ACTIVATED": str(marker),
                        "CODEX_PLUS_PLUS_INSTALLER_PATH": str(activator),
                        "CODEX_PLUS_PLUS_NPM_REGISTRY_URL": release.url + "/npm",
                        "CODEX_PLUS_PLUS_RELEASE_BASE_URL": release.url,
                    }
                    result = subprocess.run(
                        [
                            "pwsh",
                            "-NoProfile",
                            "-File",
                            str(SCRIPT_DIR / "install-codex-plus-plus-latest.ps1"),
                            "-ShimDir",
                            str(root / "shim"),
                        ],
                        env=env,
                        capture_output=True,
                        text=True,
                    )
                    if mode == "bad integrity":
                        self.assertNotEqual(result.returncode, 0)
                        self.assertFalse(marker.exists())
                        self.assertFalse(
                            any(
                                "/releases/download/" in path
                                for path in release.requests
                            )
                        )
                    else:
                        self.assertEqual(
                            result.returncode,
                            75 if mode == "pending" else 0,
                            result.stderr,
                        )
                        self.assertIn(
                            "/github/"
                            if mode == "missing payload"
                            else "/npm/package/vendor/",
                            marker.read_text(),
                        )


def unsupported_target_env(root: Path) -> dict[str, str]:
    if os.name == "nt":
        return {"PROCESSOR_ARCHITECTURE": "ARM64", "PROCESSOR_ARCHITEW6432": ""}
    fake_bin = root / "fake-bin"
    fake_bin.mkdir()
    uname = fake_bin / "uname"
    uname.write_text(
        '#!/bin/sh\ncase "$1" in -s) echo Plan9 ;; -m) echo sparc ;; esac\n',
        encoding="utf-8",
    )
    uname.chmod(0o755)
    return {"PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}"}


def make_archive(
    package_dir: Path, archive_name: str, *, windows: bool = os.name == "nt"
) -> bytes:
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "codex-package.json").write_text("{}\n", encoding="utf-8")
    target = package_dir / "bin" / ("codex.exe" if windows else "codex")
    if os.name == "nt":
        shutil.copy2(os.environ["COMSPEC"], target)
    else:
        target.write_text("#!/bin/sh\nprintf '%s\\n' \"${1-}\"\n", encoding="utf-8")
        target.chmod(0o755)
    output = io.BytesIO()
    if archive_name.endswith(".zip"):
        with zipfile.ZipFile(output, "w") as archive:
            for path in package_dir.rglob("*"):
                archive.write(path, path.relative_to(package_dir))
    else:
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for path in package_dir.rglob("*"):
                archive.add(path, path.relative_to(package_dir), recursive=False)
    return output.getvalue()


def checksum(name: str, contents: bytes) -> bytes:
    return f"{hashlib.sha256(contents).hexdigest()}  {name}\n".encode()


def platform_target() -> str:
    return (
        "x86_64-pc-windows-msvc"
        if os.name == "nt"
        else (
            "aarch64-apple-darwin"
            if os.uname().sysname == "Darwin"
            else "x86_64-unknown-linux-musl"
        )
    )


def platform_script_suffix() -> str:
    return "ps1" if os.name == "nt" else "sh"


def platform_shim_name() -> str:
    return "codex.cmd" if os.name == "nt" else "codex"


if __name__ == "__main__":
    unittest.main()
