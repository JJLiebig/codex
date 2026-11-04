#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "scripts"))

import build_codex_plus_plus as build  # noqa: E402
from build_codex_plus_plus import CODEX_RS  # noqa: E402
from build_codex_plus_plus import default_package_args  # noqa: E402
from build_codex_plus_plus import next_fork_version  # noqa: E402
from build_codex_plus_plus import package_dir_arg  # noqa: E402
from build_codex_plus_plus import replace_package_version  # noqa: E402
from build_codex_plus_plus import versioned_package_manifest  # noqa: E402


class BuildCodexPlusPlusTest(unittest.TestCase):
    def test_build_commands_accept_default_root_and_explicit_override(self) -> None:
        env = dict(os.environ)
        env.pop("CODEX_REPO_ROOT", None)
        with tempfile.TemporaryDirectory() as temp:
            for script in ("build_codex_package.py", "build_codex_plus_plus.py"):
                for cwd, overrides in (
                    (REPO_ROOT, {}),
                    (Path(temp), {"CODEX_REPO_ROOT": str(REPO_ROOT)}),
                ):
                    with self.subTest(script=script, cwd=cwd):
                        result = subprocess.run(
                            [
                                sys.executable,
                                str(REPO_ROOT / "scripts" / script),
                                "--help",
                            ],
                            cwd=cwd,
                            env={**env, **overrides},
                            capture_output=True,
                            text=True,
                        )
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn("usage:", result.stdout)

    def test_default_version_advances_the_fork_release(self) -> None:
        with patch("build_codex_plus_plus.subprocess.run") as run:
            run.return_value.stdout = "\n".join(
                [
                    "codex-plus-plus-v0.144.4-fork.1",
                    "codex-plus-plus-v0.144.4-fork.2",
                    "codex-plus-plus-v0.144.3-fork.9",
                ]
            )

            self.assertEqual(next_fork_version("0.144.4"), "0.144.4-fork.3")

    def test_forwarded_options_use_the_final_value(self) -> None:
        self.assertEqual(
            package_dir_arg(["--package-dir", "first", "--package-d=second"]),
            Path("second"),
        )
        self.assertEqual(
            default_package_args(["--package-d=custom"]),
            ["--cargo-profile", "release-fast", "--package-d=custom"],
        )

    def test_fork_version_changes_only_the_entrypoint_package(self) -> None:
        manifest = """[package]
name = "codex-cli"
version.workspace = true
edition.workspace = true

[dependencies]
version = "not-a-package-version"
"""

        self.assertEqual(
            replace_package_version(manifest, "0.144.4-fork.3"),
            """[package]
name = "codex-cli"
version = "0.144.4-fork.3"
edition.workspace = true

[dependencies]
version = "not-a-package-version"
""",
        )

    def test_versioned_manifest_follows_package_variant(self) -> None:
        self.assertEqual(
            versioned_package_manifest([]), CODEX_RS / "cli" / "Cargo.toml"
        )
        self.assertEqual(
            versioned_package_manifest(["--variant", "codex-app-server"]),
            CODEX_RS / "app-server" / "Cargo.toml",
        )

    def test_build_restores_source_bytes_and_caches_temporary_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            codex_rs = root / "codex-rs"
            workspace_manifest = codex_rs / "Cargo.toml"
            package_manifest = codex_rs / "cli" / "Cargo.toml"
            lockfile = codex_rs / "Cargo.lock"
            state = codex_rs / "target" / "source-mtimes.json"
            package_manifest.parent.mkdir(parents=True)
            original_workspace_manifest = (
                b'[workspace]\r\nmembers = ["cli"]\r\n\r\n'
                b'[workspace.package]\r\nversion = "0.153.1"\r\n'
            )
            original_package_manifest = (
                b'[package]\r\nname = "codex-cli"\r\nversion.workspace = true\r\n'
            )
            workspace_manifest.write_bytes(original_workspace_manifest)
            package_manifest.write_bytes(original_package_manifest)
            lockfile.write_text("original lock\n", encoding="utf-8")

            def normalize_lock(command: list[str], **_kwargs: object) -> object:
                self.assertEqual(
                    command,
                    [
                        "cargo",
                        "update",
                        "--workspace",
                        "--manifest-path",
                        str(workspace_manifest),
                    ],
                )
                self.assertEqual(
                    workspace_manifest.read_bytes(), original_workspace_manifest
                )
                self.assertIn(b"0.153.1-fork.2", package_manifest.read_bytes())
                lockfile.write_text("normalized lock\n", encoding="utf-8")
                return object()

            def build_package(command: list[str], **_kwargs: object) -> int:
                self.assertEqual(
                    workspace_manifest.read_bytes(), original_workspace_manifest
                )
                self.assertIn(b"0.153.1-fork.2", package_manifest.read_bytes())
                self.assertEqual(command[-2:], ["--package-version", "0.153.1-fork.2"])
                self.assertEqual(lockfile.read_text(), "normalized lock\n")
                return 0

            with (
                patch.object(build, "REPO_ROOT", root),
                patch.object(build, "CODEX_RS", codex_rs),
                patch.object(build, "WORKSPACE_MANIFEST", workspace_manifest),
                patch.object(build, "CARGO_LOCK", lockfile),
                patch.dict(
                    build.VERSIONED_PACKAGE_MANIFESTS,
                    {"codex": package_manifest},
                    clear=True,
                ),
                patch.object(build, "read_workspace_version", return_value="0.153.1"),
                patch.object(build.subprocess, "run", side_effect=normalize_lock),
                patch.object(build.subprocess, "call", side_effect=build_package),
                patch.object(
                    sys,
                    "argv",
                    [
                        "build_codex_plus_plus.py",
                        "--fork-version",
                        "0.153.1-fork.2",
                        "--build-cache-state",
                        str(state),
                        "--",
                        "--package-dir",
                        "dist/package",
                    ],
                ),
            ):
                self.assertEqual(build.main(), 0)

            self.assertEqual(
                workspace_manifest.read_bytes(), original_workspace_manifest
            )
            self.assertEqual(package_manifest.read_bytes(), original_package_manifest)
            self.assertEqual(lockfile.read_text(), "original lock\n")

            cached = json.loads(state.read_text(encoding="utf-8"))["files"]
            self.assertEqual(
                cached["Cargo.toml"]["sha256"],
                hashlib.sha256(original_workspace_manifest).hexdigest(),
            )
            self.assertEqual(
                cached["cli/Cargo.toml"]["sha256"],
                hashlib.sha256(
                    replace_package_version(
                        original_package_manifest.decode("utf-8"),
                        "0.153.1-fork.2",
                    ).encode("utf-8")
                ).hexdigest(),
            )
            self.assertNotIn("target/source-mtimes.json", cached)

    def test_prebuilt_entrypoint_skips_manifest_and_cargo_update(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            package_manifest = root / "codex-rs" / "cli" / "Cargo.toml"
            lockfile = root / "codex-rs" / "Cargo.lock"
            package_manifest.parent.mkdir(parents=True)
            original_manifest = (
                b'[package]\r\nname = "codex-cli"\r\nversion.workspace = true\r\n'
            )
            package_manifest.write_bytes(original_manifest)
            lockfile.write_text("original lock\n", encoding="utf-8")

            def build_package(command: list[str], **_kwargs: object) -> int:
                self.assertIn("--entrypoint-bin", command)
                self.assertEqual(package_manifest.read_bytes(), original_manifest)
                return 0

            with (
                patch.object(build, "REPO_ROOT", root),
                patch.object(build, "CARGO_LOCK", lockfile),
                patch.dict(
                    build.VERSIONED_PACKAGE_MANIFESTS,
                    {"codex": package_manifest},
                    clear=True,
                ),
                patch.object(build, "read_workspace_version", return_value="0.153.1"),
                patch.object(build.subprocess, "run") as run,
                patch.object(build.subprocess, "call", side_effect=build_package),
                patch.object(
                    sys,
                    "argv",
                    [
                        "build_codex_plus_plus.py",
                        "--fork-version",
                        "0.153.1-fork.2",
                        "--",
                        "--entrypoint-bin",
                        "prebuilt-codex",
                        "--package-dir",
                        "dist/package",
                    ],
                ),
            ):
                self.assertEqual(build.main(), 0)

            run.assert_not_called()
            self.assertEqual(package_manifest.read_bytes(), original_manifest)
            self.assertEqual(lockfile.read_text(), "original lock\n")


if __name__ == "__main__":
    unittest.main()
