#!/usr/bin/env python3

import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "codex_plus_plus"))

import release  # noqa: E402


VERSION = "0.144.0-fork.1"


def add_tar_file(archive: tarfile.TarFile, name: str, payload: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(payload)
    archive.addfile(info, io.BytesIO(payload))


def write_tarball(path: Path, files: dict[str, bytes]) -> None:
    with tarfile.open(path, "w:gz") as archive:
        for name, payload in files.items():
            add_tar_file(archive, name, payload)


def manifest(version: str) -> bytes:
    return json.dumps(
        {
            "name": release.PACKAGE_NAME,
            "version": version,
            "repository": {
                "type": "git",
                "url": release.REPOSITORY_URL,
                "directory": "codex-cli",
            },
        }
    ).encode()


def write_npm_tarballs(directory: Path) -> None:
    for path, package_version, _tag in release.release_entries(VERSION, directory):
        write_tarball(path, {"package/package.json": manifest(package_version)})


class CodexPlusPlusReleaseTest(unittest.TestCase):
    def test_npm_view_accepts_npm_12_singleton_arrays(self) -> None:
        cases = (
            (release.PACKAGE_NAME, "name", release.PACKAGE_NAME),
            (
                f"{release.PACKAGE_NAME}@{VERSION}",
                "dist.integrity",
                "sha512-example",
            ),
        )
        for spec, field, expected in cases:
            with self.subTest(field=field):
                result = release.subprocess.CompletedProcess(
                    args=[], returncode=0, stdout=json.dumps([expected]), stderr=""
                )
                with patch.object(release.subprocess, "run", return_value=result):
                    self.assertEqual(release.npm_view(spec, field), expected)

    def test_npm_view_rejects_ambiguous_or_malformed_json(self) -> None:
        cases = (
            (json.dumps(["first", "second"]), "ambiguous JSON"),
            ("not JSON", "invalid JSON"),
        )
        for stdout, message in cases:
            with self.subTest(message=message):
                result = release.subprocess.CompletedProcess(
                    args=[], returncode=0, stdout=stdout, stderr=""
                )
                with (
                    patch.object(release.subprocess, "run", return_value=result),
                    self.assertRaisesRegex(RuntimeError, message),
                ):
                    release.npm_view(release.PACKAGE_NAME, "name")

    def test_verify_accepts_exact_native_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archives = root / "archives"
            npm = root / "npm"
            archives.mkdir()
            npm.mkdir()

            for platform in release.PLATFORMS:
                payload = {
                    "codex-package.json": b'{"layoutVersion":1}',
                    f"bin/{platform.binary}": platform.target.encode(),
                }
                archive = release.archive_path(archives, VERSION, platform)
                if platform.archive_suffix == "zip":
                    with zipfile.ZipFile(archive, "w") as output:
                        for name, content in payload.items():
                            output.writestr(name, content)
                else:
                    write_tarball(archive, payload)
                write_tarball(
                    release.npm_tarball_path(npm, VERSION, platform.tag),
                    {
                        f"package/vendor/{platform.target}/{name}": content
                        for name, content in payload.items()
                    },
                )

            release.verify(VERSION, archives, npm)

    def test_single_platform_hydrates_verifies_and_publishes_without_other_targets(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archives, npm, vendor = root / "archives", root / "npm", root / "vendor"
            archives.mkdir()
            npm.mkdir()
            platform = release.PLATFORMS[0]
            payload = {
                "codex-package.json": b'{"layoutVersion":1}',
                "bin/codex": b"native",
            }
            write_tarball(release.archive_path(archives, VERSION, platform), payload)
            release.hydrate(VERSION, archives, vendor, platform=platform.tag)
            self.assertEqual(
                (vendor / platform.target / "bin/codex").read_bytes(), b"native"
            )
            tarball = release.npm_tarball_path(npm, VERSION, platform.tag)
            write_tarball(
                tarball,
                {
                    "package/package.json": manifest(f"{VERSION}-{platform.tag}"),
                    **{
                        f"package/vendor/{platform.target}/{name}": content
                        for name, content in payload.items()
                    },
                },
            )
            release.verify(VERSION, archives, npm, platform=platform.tag)
            integrity = release.tarball_integrity(tarball)
            published = []

            def npm_view(spec: str, field: str) -> str | None:
                if field == "name":
                    return release.PACKAGE_NAME
                self.assertEqual(
                    spec, f"{release.PACKAGE_NAME}@{VERSION}-{platform.tag}"
                )
                return integrity if published else None

            def npm_publish(command: list[str], *, check: bool) -> None:
                published.append((command[2], command[command.index("--tag") + 1]))

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run", side_effect=npm_publish),
            ):
                release.publish(VERSION, npm, platform=platform.tag)
            self.assertEqual(published, [(str(tarball), platform.tag)])

    def test_publish_skips_exact_registry_versions(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)
            integrities = {
                f"{release.PACKAGE_NAME}@{package_version}": release.tarball_integrity(
                    path
                )
                for path, package_version, _tag in release.release_entries(
                    VERSION, npm_dir
                )
            }

            def npm_view(spec: str, field: str) -> str | None:
                if field == "name":
                    return release.PACKAGE_NAME
                return integrities[spec]

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run") as run,
            ):
                release.publish(VERSION, npm_dir)

            run.assert_not_called()

    def test_publish_preflights_conflicts_before_publishing(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)
            conflict_spec = (
                f"{release.PACKAGE_NAME}@{VERSION}-{release.PLATFORMS[1].tag}"
            )

            def npm_view(spec: str, field: str) -> str | None:
                if field == "name":
                    return release.PACKAGE_NAME
                return "sha512-conflict" if spec == conflict_spec else None

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run") as run,
                self.assertRaisesRegex(RuntimeError, "Refusing to skip"),
            ):
                release.publish(VERSION, npm_dir)

            run.assert_not_called()

    def test_publish_dry_run_never_invokes_npm_publish(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)

            def npm_view(_spec: str, field: str) -> str | None:
                return release.PACKAGE_NAME if field == "name" else None

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run") as run,
            ):
                release.publish(VERSION, npm_dir, dry_run=True)

            run.assert_not_called()

    def test_publish_submits_platforms_before_waiting_and_latest_after_confirmation(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)
            integrities = {
                f"{release.PACKAGE_NAME}@{package_version}": release.tarball_integrity(
                    path
                )
                for path, package_version, _tag in release.release_entries(
                    VERSION, npm_dir
                )
            }
            published: set[str] = set()
            root_spec = f"{release.PACKAGE_NAME}@{VERSION}"
            platform_specs = set(integrities) - {root_spec}
            confirmed: set[str] = set()
            ticks = 0

            def wait(_seconds: int) -> None:
                nonlocal ticks
                ticks += 1

            def npm_view(spec: str, field: str) -> str | None:
                if field == "name":
                    self.assertEqual(spec, release.PACKAGE_NAME)
                    return release.PACKAGE_NAME
                if spec not in published:
                    return None
                if spec in platform_specs:
                    self.assertEqual(published, platform_specs)
                    if ticks < 20 * 60 // 5:
                        return None
                    confirmed.add(spec)
                return integrities[spec]

            def npm_publish(command: list[str], *, check: bool) -> None:
                self.assertTrue(check)
                self.assertIn("--provenance", command)
                package_version = release.read_manifest(Path(command[2]))["version"]
                if package_version == VERSION:
                    self.assertEqual(confirmed, platform_specs)
                published.add(f"{release.PACKAGE_NAME}@{package_version}")

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run", side_effect=npm_publish) as run,
                patch.object(release.time, "sleep", side_effect=wait) as sleep,
            ):
                release.publish(VERSION, npm_dir)

            self.assertEqual(run.call_count, 4)
            self.assertEqual(sleep.call_count, 20 * 60 // 5)

    def test_platform_confirmation_failure_does_not_publish_latest(self) -> None:
        for confirmation in (None, "sha512-conflict"):
            with (
                self.subTest(confirmation=confirmation),
                tempfile.TemporaryDirectory() as temp,
            ):
                npm_dir = Path(temp)
                write_npm_tarballs(npm_dir)
                tags = []

                def npm_view(_spec: str, field: str) -> str | None:
                    if field == "name":
                        return release.PACKAGE_NAME
                    return confirmation if tags else None

                def npm_publish(command: list[str], *, check: bool) -> None:
                    tags.append(command[command.index("--tag") + 1])

                with (
                    patch.object(release, "npm_view", side_effect=npm_view),
                    patch.object(release.subprocess, "run", side_effect=npm_publish),
                    patch.object(release.time, "sleep"),
                    self.assertRaisesRegex(
                        RuntimeError, "Timed out confirming|has registry integrity"
                    ),
                ):
                    release.publish(VERSION, npm_dir)

                self.assertEqual(tags, [platform.tag for platform in release.PLATFORMS])

    def test_publish_continues_after_manual_linux_bootstrap(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)
            path, package_version, _tag = release.release_entries(VERSION, npm_dir)[0]
            linux_spec = f"{release.PACKAGE_NAME}@{package_version}"
            linux_integrity = release.tarball_integrity(path)

            def npm_view(spec: str, field: str) -> str | None:
                if field == "name":
                    return release.PACKAGE_NAME if spec == linux_spec else None
                return linux_integrity if spec == linux_spec else None

            with (
                patch.object(release, "npm_view", side_effect=npm_view),
                patch.object(release.subprocess, "run") as run,
            ):
                release.publish(VERSION, npm_dir, dry_run=True)

            run.assert_not_called()

    def test_publish_preserves_manual_bootstrap_guidance_when_package_absent(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp:
            npm_dir = Path(temp)
            write_npm_tarballs(npm_dir)

            with (
                patch.object(release, "npm_view", return_value=None),
                patch.object(release.subprocess, "run") as run,
                self.assertRaisesRegex(
                    RuntimeError, "manually publish its linux-x64 tarball"
                ),
            ):
                release.publish(VERSION, npm_dir)

            run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
