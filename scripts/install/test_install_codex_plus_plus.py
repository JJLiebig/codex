#!/usr/bin/env python3

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest


INSTALL_SCRIPT = Path(__file__).with_name("install-codex-plus-plus.ps1")
SHELL_INSTALL_SCRIPT = Path(__file__).with_name("install-codex-plus-plus.sh")
PANE_TOKEN = "dummy-terminal-resume-token"


@unittest.skipUnless(os.name == "nt", "Windows installer test")
class InstallCodexPlusPlusTest(unittest.TestCase):
    def test_concurrent_installs_leave_the_pointer_on_an_existing_release(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"
            processes = [
                subprocess.Popen(
                    installer_command(package_dir, shim_dir),
                    env=installer_env(codex_home),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                for _ in range(2)
            ]

            results = [process.communicate() for process in processes]
            for process, (stdout, stderr) in zip(processes, results, strict=True):
                self.assertEqual(process.returncode, 0, f"{stdout}\n{stderr}")
            target_dir = current_target_dir(shim_dir)
            self.assertTrue((target_dir / "codex.exe").is_file())
            self.assertEqual(len(release_dirs(codex_home)), 2)

    def test_remove_serializes_with_install(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"
            run_installer(package_dir, shim_dir, codex_home)
            other_codex_home = root / "other-codex-home"
            processes = [
                subprocess.Popen(
                    command,
                    env=environment,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                for command, environment in (
                    (
                        installer_command(package_dir, shim_dir),
                        installer_env(codex_home),
                    ),
                    (remove_command(shim_dir), installer_env(other_codex_home)),
                )
            ]

            results = [process.communicate() for process in processes]
            for process, (stdout, stderr) in zip(processes, results, strict=True):
                self.assertEqual(process.returncode, 0, f"{stdout}\n{stderr}")
            installed = [
                (shim_dir / name).exists()
                for name in (
                    "codex.ps1",
                    "codex.cmd",
                    ".codex-plus-plus-current",
                    ".codex-plus-plus-shim",
                )
            ]
            self.assertIn(installed, ([True] * 4, [False] * 4))
            if all(installed):
                self.assertTrue((current_target_dir(shim_dir) / "codex.exe").is_file())

    def test_install_rejects_a_release_store_inside_the_source_package(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            package_dir = create_package(Path(temp_dir) / "package")
            result = invoke_installer(package_dir, package_dir / "bin", package_dir)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "managed install paths must be outside the source package",
                " ".join(result.stderr.split()),
            )

    def test_different_shims_keep_independent_release_namespaces(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            codex_home = root / "codex-home"
            first_shim_dir = root / "first-install" / "bin"
            second_shim_dir = root / "second-install" / "bin"

            run_installer(package_dir, first_shim_dir, codex_home)
            first_target_dir = current_target_dir(first_shim_dir)
            run_installer(package_dir, second_shim_dir, codex_home)

            self.assertTrue((first_target_dir / "codex.exe").is_file())
            self.assertTrue(
                (current_target_dir(second_shim_dir) / "codex.exe").is_file()
            )

    def test_custom_shim_reinstall_passes_directory_to_children(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            shim_dir = root / "custom shim" / "bin"
            codex_home = root / "codex-home"

            run_installer(package_dir, shim_dir, codex_home)
            run_installer(package_dir, shim_dir, codex_home)
            self.assertIn(
                "CODEX_PLUS_PLUS_SHIM_DIR",
                (shim_dir / "codex.cmd").read_text(encoding="utf-8"),
            )
            self.assertIn(
                "CODEX_PLUS_PLUS_SHIM_DIR",
                (shim_dir / "codex.ps1").read_text(encoding="utf-8-sig"),
            )
            command = (
                "if not defined OSC7577_TOKEN exit /b 92&"
                "echo %CODEX_PLUS_PLUS_SHIM_DIR%&echo argument with spaces&exit /b 23"
            )
            pending = shim_dir / ".codex-plus-plus-pending.json"
            probe = root / "pending.ps1"
            probe.write_text(
                "param($ShimDir, [switch]$ResumeUpgrade)\n"
                "if (Test-Path Env:OSC7577_TOKEN) { exit 91 }\n"
                "Remove-Item -LiteralPath (Join-Path $ShimDir '.codex-plus-plus-pending.json')\n",
                encoding="utf-8",
            )
            launchers = (
                [str(shim_dir / "codex.cmd")],
                [
                    "powershell.exe",
                    "-NoProfile",
                    "-File",
                    str(shim_dir / "codex.ps1"),
                ],
            )
            for launcher in launchers:
                with self.subTest(launcher=launcher[0]):
                    pending.write_text(
                        json.dumps({"Installer": str(probe)}),
                        encoding="utf-8",
                    )
                    launched = subprocess.run(
                        [*launcher, "/d", "/s", "/c", command],
                        capture_output=True,
                        check=False,
                        text=True,
                        env={**os.environ, "OSC7577_TOKEN": PANE_TOKEN},
                    )
                    self.assertEqual(launched.returncode, 23, launched.stderr)
                    self.assertEqual(
                        launched.stdout.splitlines(),
                        [str(shim_dir.resolve()), "argument with spaces"],
                    )

    def test_install_rejects_a_shim_inside_the_release_store(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            codex_home = root / "codex-home"
            shim_dir = (
                codex_home
                / "packages"
                / "codex-plus-plus"
                / "releases"
                / "shim"
                / "bin"
            )
            result = invoke_installer(package_dir, shim_dir, codex_home)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "shim and release directories must not overlap",
                result.stderr,
            )

    def test_install_rejects_a_release_store_aliased_into_source_package(self) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir)
            package_dir = create_package(root / "package")
            codex_home = root / "codex-home-alias"
            codex_home.symlink_to(package_dir, target_is_directory=True)
            result = invoke_installer(
                package_dir,
                root / "install" / "bin",
                codex_home,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "managed install paths must be outside the source package",
                " ".join(result.stderr.split()),
            )

    def test_install_switches_generations_and_prunes_only_unlocked_releases(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as temp_dir:
            root = Path(temp_dir) / "täst"
            root.mkdir()
            package_dir = create_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"

            run_installer(package_dir, shim_dir, codex_home)
            first_release = only_release(codex_home)
            shim = (shim_dir / "codex.ps1").read_text(encoding="utf-8-sig")
            self.assertNotIn(str(package_dir), shim)
            self.assertIn(".codex-plus-plus-current", shim)
            self.assertEqual(current_target_dir(shim_dir), first_release / "bin")
            launched = subprocess.run(
                [str(shim_dir / "codex.cmd"), "/c", "exit", "0", "foo)"],
                capture_output=True,
                check=False,
            )
            self.assertEqual(launched.returncode, 0, launched.stderr)
            launched = subprocess.run(
                [
                    "powershell.exe",
                    "-NoProfile",
                    "-File",
                    str(shim_dir / "codex.ps1"),
                    "/c",
                    "exit",
                    "0",
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(launched.returncode, 0, launched.stderr)

            active = subprocess.Popen(
                [
                    str(shim_dir / "codex.cmd"),
                    "/d",
                    "/q",
                    "/c",
                    "pause >nul",
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                lease_dir = shim_dir / ".codex-plus-plus-leases" / first_release.name
                deadline = time.monotonic() + 5
                while not list(lease_dir.glob("cmd.*.lease")):
                    if time.monotonic() >= deadline:
                        self.fail(
                            "CMD shim did not acquire its generation lease: "
                            f"process={active.poll()}, "
                            f"leases={lease_debug(shim_dir)}"
                        )
                    time.sleep(0.05)
                run_installer(package_dir, shim_dir, codex_home)
                run_installer(package_dir, shim_dir, codex_home)
                releases = release_dirs(codex_home)
                self.assertEqual(len(releases), 3)
                self.assertIn(first_release, releases)
            finally:
                if active.stdin is not None:
                    active.stdin.write(b"\n")
                    active.stdin.flush()
                active.wait(timeout=5)
                if active.stdin is not None:
                    active.stdin.close()

            stale_lease = (
                shim_dir
                / ".codex-plus-plus-leases"
                / first_release.name
                / "cmd.999999.1.lease"
            )
            stale_lease.write_bytes(b"")
            run_installer(package_dir, shim_dir, codex_home)
            releases = release_dirs(codex_home)
            self.assertEqual(len(releases), 2)
            self.assertNotIn(first_release, releases)
            self.assertEqual(current_target_dir(shim_dir), releases[-1] / "bin")


@unittest.skipIf(os.name == "nt", "POSIX installer test")
class InstallCodexPlusPlusShellTest(unittest.TestCase):
    def test_custom_shim_reinstall_passes_directory_to_child(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = create_posix_package(root / "package")
            shim_dir = root / "custom shim" / "bin"
            codex_home = root / "codex-home"

            run_shell_installer(package_dir, shim_dir, codex_home)
            run_shell_installer(package_dir, shim_dir, codex_home)
            shim = shim_dir / "codex"
            alias = root / "codex-alias"
            alias.symlink_to(shim)
            self.assertIn(
                "CODEX_PLUS_PLUS_SHIM_DIR",
                shim.read_text(encoding="utf-8"),
            )
            helpers = root / "helpers"
            helpers.mkdir()
            ps = shutil.which("ps")
            assert ps is not None
            helper = helpers / "ps"
            helper.write_text(
                "#!/bin/sh\n"
                '[ "${OSC7577_TOKEN+x}" != x ] || exit 91\n'
                f'exec {shlex.quote(ps)} "$@"\n',
                encoding="utf-8",
            )
            helper.chmod(0o755)
            launched = subprocess.run(
                [str(alias), "provenance", "argument with spaces", "23"],
                capture_output=True,
                check=False,
                text=True,
                env={
                    **os.environ,
                    "PATH": f"{helpers}{os.pathsep}{os.environ.get('PATH', os.defpath)}",
                    "OSC7577_TOKEN": PANE_TOKEN,
                },
            )

            self.assertEqual(launched.returncode, 23, launched.stderr)
            self.assertEqual(
                launched.stdout.splitlines(),
                [str(shim_dir.resolve()), "argument with spaces"],
            )

    def test_concurrent_installs_publish_a_valid_generation(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = create_posix_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"
            command = shell_installer_command(package_dir, shim_dir)
            processes = [
                subprocess.Popen(
                    command,
                    env=installer_env(codex_home),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                for _ in range(2)
            ]

            results = [process.communicate() for process in processes]
            for process, (stdout, stderr) in zip(processes, results, strict=True):
                self.assertEqual(process.returncode, 0, f"{stdout}\n{stderr}")
            generation = (
                (shim_dir / ".codex-plus-plus-current")
                .read_text(encoding="utf-8")
                .strip()
            )
            releases = release_dirs(codex_home)
            self.assertEqual(len(releases), 2)
            self.assertIn(generation, {release.name for release in releases})

    def test_remove_serializes_with_install(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = create_posix_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"
            run_shell_installer(package_dir, shim_dir, codex_home)
            processes = [
                subprocess.Popen(
                    command,
                    env=installer_env(codex_home),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                for command in (
                    shell_installer_command(package_dir, shim_dir),
                    shell_remove_command(shim_dir),
                )
            ]

            results = [process.communicate() for process in processes]
            for process, (stdout, stderr) in zip(processes, results, strict=True):
                self.assertEqual(process.returncode, 0, f"{stdout}\n{stderr}")
            installed = [
                (shim_dir / name).exists()
                for name in ("codex", ".codex-plus-plus-current")
            ]
            self.assertIn(installed, ([True, True], [False, False]))

    def test_install_rejects_a_release_store_aliased_into_source_package(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = create_posix_package(root / "package")
            managed_dir = package_dir / "managed"
            managed_dir.mkdir()
            codex_home = root / "codex-home"
            codex_home.mkdir()
            (codex_home / "packages").symlink_to(
                managed_dir,
                target_is_directory=True,
            )
            result = subprocess.run(
                shell_installer_command(package_dir, root / "install" / "bin"),
                capture_output=True,
                check=False,
                env=installer_env(codex_home),
                text=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(
                "managed install paths must be outside the source package",
                result.stderr,
            )

    def test_install_copies_package_and_preserves_active_generation(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = create_posix_package(root / "package")
            shim_dir = root / "install" / "bin"
            codex_home = root / "codex-home"

            run_shell_installer(package_dir, shim_dir, codex_home)
            first_release = only_release(codex_home)
            stale_chooser = (
                shim_dir / ".codex-plus-plus-install-locks" / "choosing.999999.stale"
            )
            stale_chooser.write_text("999999\nold process\n", encoding="utf-8")
            abandoned_staging = first_release.parent / ".staging.abandoned"
            abandoned_staging.mkdir()
            (abandoned_staging / "partial").write_bytes(b"partial")
            moved_package_dir = root / "package-away"
            package_dir.rename(moved_package_dir)
            launched = subprocess.run(
                [str(shim_dir / "codex"), "hello"],
                capture_output=True,
                check=False,
                text=True,
            )
            moved_package_dir.rename(package_dir)
            self.assertEqual(launched.returncode, 0, launched.stderr)
            self.assertEqual(launched.stdout, "hello\n")

            active = subprocess.Popen(
                [str(shim_dir / "codex"), "wait"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                text=True,
            )
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    active.wait(timeout=0.25)
                run_shell_installer(package_dir, shim_dir, codex_home)
                run_shell_installer(package_dir, shim_dir, codex_home)
                releases = release_dirs(codex_home)
                self.assertEqual(len(releases), 3)
                self.assertIn(first_release, releases)
                self.assertFalse(abandoned_staging.exists())
            finally:
                if active.stdin is not None:
                    active.stdin.write("\n")
                    active.stdin.flush()
                active.wait(timeout=5)
                if active.stdin is not None:
                    active.stdin.close()

            first_lease_dir = shim_dir / ".codex-plus-plus-leases" / first_release.name
            (first_lease_dir / ".pruning").write_bytes(b"")
            (first_lease_dir / f"sh.{os.getpid()}").write_text(
                "mismatched process start\n",
                encoding="utf-8",
            )
            run_shell_installer(package_dir, shim_dir, codex_home)
            releases = release_dirs(codex_home)
            self.assertEqual(len(releases), 2)
            self.assertNotIn(first_release, releases)


def create_package(package_dir: Path) -> Path:
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "codex-package.json").write_text("{}\n", encoding="utf-8")
    shutil.copy2(os.environ["COMSPEC"], package_dir / "bin" / "codex.exe")
    return package_dir


def create_posix_package(package_dir: Path) -> Path:
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "codex-package.json").write_text("{}\n", encoding="utf-8")
    target = package_dir / "bin" / "codex"
    target.write_text(
        "#!/bin/sh\n"
        'if [ "${1-}" = provenance ]; then\n'
        f'  [ "${{OSC7577_TOKEN-}}" = {PANE_TOKEN} ] || exit 92\n'
        '  printf \'%s\\n\' "$CODEX_PLUS_PLUS_SHIM_DIR" "${2-}"\n'
        '  exit "${3-}"\n'
        "fi\n"
        'if [ "${1-}" = wait ]; then IFS= read -r line; exit 0; fi\n'
        "printf '%s\\n' \"${1-}\"\n",
        encoding="utf-8",
    )
    target.chmod(0o755)
    return package_dir


def run_installer(package_dir: Path, shim_dir: Path, codex_home: Path) -> None:
    result = invoke_installer(package_dir, shim_dir, codex_home)
    if result.returncode != 0:
        raise AssertionError(f"installer failed:\n{result.stdout}\n{result.stderr}")


def invoke_installer(
    package_dir: Path,
    shim_dir: Path,
    codex_home: Path,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        installer_command(package_dir, shim_dir),
        capture_output=True,
        check=False,
        env=installer_env(codex_home),
        text=True,
    )


def installer_command(package_dir: Path, shim_dir: Path) -> list[str]:
    return [
        "powershell.exe",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(INSTALL_SCRIPT),
        "-TargetExe",
        str(package_dir / "bin" / "codex.exe"),
        "-ShimDir",
        str(shim_dir),
        "-Install",
    ]


def remove_command(shim_dir: Path) -> list[str]:
    return [
        "powershell.exe",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(INSTALL_SCRIPT),
        "-ShimDir",
        str(shim_dir),
        "-Remove",
    ]


def installer_env(codex_home: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["CODEX_HOME"] = str(codex_home)
    return env


def run_shell_installer(
    package_dir: Path,
    shim_dir: Path,
    codex_home: Path,
) -> None:
    result = subprocess.run(
        shell_installer_command(package_dir, shim_dir),
        capture_output=True,
        check=False,
        env=installer_env(codex_home),
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(f"installer failed:\n{result.stdout}\n{result.stderr}")


def shell_installer_command(package_dir: Path, shim_dir: Path) -> list[str]:
    return [
        "sh",
        str(SHELL_INSTALL_SCRIPT),
        "--target-exe",
        str(package_dir / "bin" / "codex"),
        "--shim-dir",
        str(shim_dir),
        "--install",
    ]


def shell_remove_command(shim_dir: Path) -> list[str]:
    return [
        "sh",
        str(SHELL_INSTALL_SCRIPT),
        "--shim-dir",
        str(shim_dir),
        "--remove",
    ]


def current_target_dir(shim_dir: Path) -> Path:
    generation = (
        (shim_dir / ".codex-plus-plus-current").read_text(encoding="utf-8").strip()
    )
    return (shim_dir / ".codex-plus-plus-generations" / generation).resolve()


def release_dirs(codex_home: Path) -> list[Path]:
    releases_root = codex_home / "packages" / "codex-plus-plus" / "releases"
    return sorted(
        path
        for releases_dir in releases_root.iterdir()
        for path in releases_dir.iterdir()
        if not path.name.startswith(".")
    )


def lease_debug(shim_dir: Path) -> list[tuple[Path, bytes | None]]:
    leases_root = shim_dir / ".codex-plus-plus-leases"
    return [
        (path, path.read_bytes() if path.is_file() else None)
        for path in leases_root.rglob("*")
    ]


def only_release(codex_home: Path) -> Path:
    releases = release_dirs(codex_home)
    if len(releases) != 1:
        raise AssertionError(f"expected one release, got {releases}")
    return releases[0]


if __name__ == "__main__":
    unittest.main()
