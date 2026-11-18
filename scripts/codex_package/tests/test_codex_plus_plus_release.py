#!/usr/bin/env python3

import base64
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from urllib.parse import parse_qs, urlsplit
from unittest.mock import patch
import zipfile


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "codex_plus_plus"))

import release  # noqa: E402
import github_release  # noqa: E402


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


class FakeGitHub:
    """Persist releases and uploaded bytes, rejecting overwrite like GitHub."""

    def __init__(self):
        self.releases = {}
        self.duplicate_drafts = []
        self.latest = None
        self.commit = "tag-source"
        self.installers = {}

    def create(self, fields):
        release_id = len(self.releases) + len(self.duplicate_drafts) + 1
        result = {
            **fields,
            "id": release_id,
            "files": {},
            "upload_url": (
                f"https://uploads.github.com/repos/owner/repo/releases/{release_id}/assets"
                "{?name,label}"
            ),
        }
        if self.releases.setdefault(fields["tag_name"], result) is not result:
            self.duplicate_drafts.append(result)
        return result

    def gh(self, *args, body=None, missing=False, output=None):
        url = urlsplit(args[1])
        endpoint = url.path.lstrip("/").split("/", 3)[3]
        releases = [*self.releases.values(), *self.duplicate_drafts]
        if url.netloc == "uploads.github.com":
            release_id = int(endpoint.split("/")[1])
            result = next(r for r in releases if r["id"] == release_id)
            path = Path(args[args.index("--input") + 1])
            (name,) = parse_qs(url.query)["name"]
            if name in result["files"]:
                raise RuntimeError("Refusing overwrite")
            result["files"][name] = path.read_bytes()
            return None
        if endpoint.startswith("commits/"):
            return {"sha": self.commit}
        if endpoint.startswith("contents/"):
            path = endpoint.removeprefix("contents/")
            (ref,) = parse_qs(url.query)["ref"]
            if ref != self.commit:
                raise RuntimeError("Wrong installer source")
            data = self.installers.setdefault(path, ("exact-tag:" + path).encode())
            return {"content": base64.b64encode(data).decode()}
        if endpoint == "releases/latest":
            return self.releases.get(self.latest)
        if endpoint.startswith("releases/tags/"):
            result = self.releases.get(endpoint.removeprefix("releases/tags/"))
            return result if result and not result["draft"] else None
        if endpoint == "releases":
            if body is not None:
                return self.create(body)
            return [[r] for r in sorted(releases, key=lambda r: -r["id"])]
        if endpoint.startswith("releases/assets/"):
            release_id, index = divmod(int(endpoint.rsplit("/", 1)[1]), 100)
            result = next(r for r in releases if r["id"] == release_id)
            output.write(list(result["files"].values())[index])
            return None
        release_id = int(endpoint.split("/")[1])
        result = next(r for r in releases if r["id"] == release_id)
        if endpoint.endswith("/assets"):
            return [
                [
                    {
                        "id": release_id * 100 + index,
                        "name": name,
                        "digest": None
                        if name == "install-codex-plus-plus.sh"
                        else "sha256:" + hashlib.sha256(data).hexdigest(),
                    }
                    for index, (name, data) in enumerate(result["files"].items())
                ]
            ]
        result.update(body)
        if body["make_latest"] == "true":
            self.latest = result["tag_name"]
        return result


class GitHubReleaseTest(unittest.TestCase):
    def test_ready_platform_append_repeat_conflict_and_late_old_retry(self):
        with tempfile.TemporaryDirectory() as temporary:
            archives = Path(temporary)
            github = FakeGitHub()
            version = "1.2.3-fork.2"
            tag = "codex-plus-plus-v" + version
            with patch.object(github_release, "gh", side_effect=github.gh):
                for index, platform in enumerate(release.REQUIRED_PLATFORMS):
                    archive = release.archive_path(archives, version, platform)
                    archive.write_bytes(platform.target.encode())
                    github_release.publish(
                        "owner/repo",
                        version,
                        "tag-source",
                        archives,
                        None,
                        platform.target,
                    )
                    published = github.releases[tag]["files"].copy()
                    self.assertEqual(github.latest, tag if index else None)
                    self.assertEqual(github.releases[tag]["draft"], index == 0)
                    self.assertEqual(
                        len([n for n in published if n.startswith("codex-plus-plus-")]),
                        (index + 1) * 2,
                    )
                    self.assertEqual(published[archive.name], platform.target.encode())
                    self.assertEqual(
                        published["install-codex-plus-plus.sh"],
                        b"exact-tag:scripts/install/install-codex-plus-plus.sh",
                    )
                    if index == 0:
                        duplicate = github.create({"tag_name": tag, "draft": True})
                    github_release.publish(
                        "owner/repo",
                        version,
                        "tag-source",
                        archives,
                        None,
                        platform.target,
                    )
                    self.assertEqual(github.duplicate_drafts, [duplicate])
                    self.assertEqual(duplicate["files"], {})
                    self.assertEqual(github.releases[tag]["files"], published)
                with tempfile.TemporaryDirectory() as npm_temporary:
                    npm = Path(npm_temporary)
                    (npm / "root.tgz").write_bytes(
                        b"root with required platforms ready"
                    )
                    github_release.publish(
                        "owner/repo", version, "tag-source", archives, npm, None
                    )
                    for name, data in published.items():
                        self.assertEqual(github.releases[tag]["files"][name], data)
                    published = github.releases[tag]["files"].copy()
                    self.assertEqual(
                        published["root.tgz"], b"root with required platforms ready"
                    )
                archive.write_bytes(b"conflicting bytes")
                with self.assertRaisesRegex(RuntimeError, "Conflicting release asset"):
                    github_release.publish(
                        "owner/repo",
                        version,
                        "tag-source",
                        archives,
                        None,
                        platform.target,
                    )
                self.assertEqual(github.releases[tag]["files"], published)
                archive.write_bytes(platform.target.encode())
                newer = "1.2.3-fork.10"
                for platform in release.REQUIRED_PLATFORMS:
                    newer_archive = release.archive_path(archives, newer, platform)
                    newer_archive.write_bytes(b"newer")
                    github_release.publish(
                        "owner/repo",
                        newer,
                        "tag-source",
                        archives,
                        None,
                        platform.target,
                    )
                macos = release.selected_platforms("darwin-arm64")[0]
                archive = release.archive_path(archives, version, macos)
                archive.write_bytes(b"late macOS archive")
                with tempfile.TemporaryDirectory() as npm_temporary:
                    npm = Path(npm_temporary)
                    tarball = release.npm_tarball_path(npm, version, macos.tag)
                    tarball.write_bytes(b"late macOS npm payload")
                    github_release.publish(
                        "owner/repo", version, "tag-source", archives, npm, macos.target
                    )
                    self.assertEqual(
                        github.releases[tag]["files"][tarball.name],
                        b"late macOS npm payload",
                    )
                self.assertEqual(github.latest, "codex-plus-plus-v" + newer)
                for name, data in published.items():
                    self.assertEqual(github.releases[tag]["files"][name], data)
                self.assertEqual(
                    github.releases[tag]["files"][archive.name], b"late macOS archive"
                )
                published = github.releases[tag]["files"].copy()
                with self.assertRaisesRegex(RuntimeError, "validated source run"):
                    github_release.publish(
                        "owner/repo",
                        version,
                        "other-source",
                        archives,
                        None,
                        platform.target,
                    )
                self.assertEqual(github.releases[tag]["files"], published)


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

    def test_verify_accepts_npm_projection_and_rejects_payload_changes(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archives = root / "archives"
            npm = root / "npm"
            archives.mkdir()
            npm.mkdir()

            for platform in release.PLATFORMS:
                exec_server = (
                    "codex-exec-server.exe"
                    if platform.binary.endswith(".exe")
                    else "codex-exec-server"
                )
                payload = {
                    "codex-package.json": json.dumps(
                        {"layoutVersion": 1, "execServer": f"bin/{exec_server}"}
                    ).encode(),
                    f"bin/{platform.binary}": platform.target.encode(),
                    f"bin/{exec_server}": b"standalone exec server",
                }
                archive = release.archive_path(archives, VERSION, platform)
                if platform.archive_suffix == "zip":
                    with zipfile.ZipFile(archive, "w") as output:
                        for name, content in payload.items():
                            output.writestr(name, content)
                else:
                    write_tarball(archive, payload)
                npm_payload = {
                    "codex-package.json": b'{\n  "layoutVersion": 1\n}\n',
                    f"bin/{platform.binary}": platform.target.encode(),
                }
                cases = {
                    "npm projection": npm_payload,
                    "changed binary": {
                        **npm_payload,
                        f"bin/{platform.binary}": b"damaged",
                    },
                    "changed metadata": {
                        **npm_payload,
                        "codex-package.json": b'{\n  "layoutVersion": 2\n}\n',
                    },
                    "missing binary": {
                        "codex-package.json": npm_payload["codex-package.json"]
                    },
                    "extra file": {**npm_payload, "unexpected": b"extra"},
                }
                for case, files in cases.items():
                    with self.subTest(platform=platform.tag, case=case):
                        write_tarball(
                            release.npm_tarball_path(npm, VERSION, platform.tag),
                            {
                                f"package/vendor/{platform.target}/{name}": content
                                for name, content in files.items()
                            },
                        )
                        if case == "npm projection":
                            release.verify(
                                VERSION, archives, npm, platform=platform.tag
                            )
                        else:
                            with self.assertRaisesRegex(
                                RuntimeError, "Native payload mismatch"
                            ):
                                release.verify(
                                    VERSION, archives, npm, platform=platform.tag
                                )

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
                "codex-package.json": b'{\n  "layoutVersion": 1\n}\n',
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

    def test_publish_submits_required_platforms_before_latest_with_macos_absent(
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
                    processing_minutes = 26 if spec.endswith("-win32-x64") else 20
                    if ticks < processing_minutes * 60 // 5:
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

            self.assertEqual(run.call_count, 3)
            self.assertEqual(sleep.call_count, 26 * 60 // 5)

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

                self.assertEqual(tags, ["linux-x64", "win32-x64"])

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
