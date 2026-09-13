import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch


REPO_ROOT = Path(__file__).resolve().parents[3]
STAGE_SCRIPT = REPO_ROOT / "scripts" / "stage_npm_packages.py"
RELEASE_SCRIPT = (
    REPO_ROOT / "scripts" / "codex_package" / "codex_plus_plus" / "release.py"
)
SOURCE_PACKAGE_JSON = json.loads(
    (REPO_ROOT / "codex-cli" / "package.json").read_text(encoding="utf-8")
)


def load_stage_module():
    spec = importlib.util.spec_from_file_location(
        "stage_npm_packages_test", STAGE_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load {STAGE_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_release_module():
    spec = importlib.util.spec_from_file_location(
        "codex_plus_plus_release_test", RELEASE_SCRIPT
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load {RELEASE_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


stage = load_stage_module()
build = stage._BUILD_MODULE
release = load_release_module()

VERSION = "0.144.4-fork.1"
ROOT_PACKAGE = "@jjliebig/codex-plus-plus"
REPOSITORY = {
    "type": "git",
    "url": "git+https://github.com/JJLiebig/codex.git",
    "directory": "codex-cli",
}
PLATFORMS = {
    "codex-plus-plus-linux-x64": {
        "alias": "@jjliebig/codex-plus-plus-linux-x64",
        "target": "x86_64-unknown-linux-musl",
        "tag": "linux-x64",
        "os": "linux",
        "cpu": "x64",
        "binary": "codex",
    },
    "codex-plus-plus-darwin-arm64": {
        "alias": "@jjliebig/codex-plus-plus-darwin-arm64",
        "target": "aarch64-apple-darwin",
        "tag": "darwin-arm64",
        "os": "darwin",
        "cpu": "arm64",
        "binary": "codex",
    },
    "codex-plus-plus-win32-x64": {
        "alias": "@jjliebig/codex-plus-plus-win32-x64",
        "target": "x86_64-pc-windows-msvc",
        "tag": "win32-x64",
        "os": "win32",
        "cpu": "x64",
        "binary": "codex.exe",
    },
}


class CodexPlusPlusNpmTest(unittest.TestCase):
    def test_expands_only_supported_payloads_and_requires_fork_artifacts(self):
        packages = stage.expand_packages(["codex-plus-plus"])
        self.assertEqual(packages, ["codex-plus-plus", *PLATFORMS])
        components = stage.native_components_for_package(next(iter(PLATFORMS)))
        self.assertEqual(
            stage.native_targets_for_component_set(packages, components),
            tuple(config["target"] for config in PLATFORMS.values()),
        )
        argv = [
            str(STAGE_SCRIPT),
            "--release-version",
            VERSION,
            "--package",
            "codex-plus-plus",
        ]
        with patch.object(sys, "argv", argv):
            with self.assertRaisesRegex(RuntimeError, "--vendor-src is required"):
                stage.main()
        argv.extend(["--package", "codex", "--vendor-src", "."])
        with patch.object(sys, "argv", argv):
            with self.assertRaisesRegex(RuntimeError, "must be staged separately"):
                stage.main()

    def test_stages_root_and_platform_tarballs_from_fixture_vendor(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            temp = Path(temp_dir)
            archives = temp / "archives"
            vendor = temp / "vendor"
            output = temp / "output"
            for platform, config in zip(release.PLATFORMS, PLATFORMS.values()):
                target_dir = temp / "packages" / config["target"]
                (target_dir / "bin").mkdir(parents=True)
                (target_dir / "bin" / config["binary"]).write_bytes(
                    f"fixture-{config['target']}".encode()
                )
                (target_dir / "codex-package.json").write_text(
                    '{"version":"0.144.4"}\n', encoding="utf-8"
                )
                self._write_release_archive(
                    release.archive_path(archives, VERSION, platform), target_dir
                )
            release.hydrate(VERSION, archives, vendor)

            argv = [
                str(STAGE_SCRIPT),
                "--release-version",
                VERSION,
                "--package",
                "codex-plus-plus",
                "--vendor-src",
                str(vendor),
                "--output-dir",
                str(output),
            ]
            with patch.object(sys, "argv", argv):
                self.assertEqual(stage.main(), 0)
            release.verify(VERSION, archives, output)

            packages = ["codex-plus-plus", *PLATFORMS]
            tarballs = {
                package: output / stage.tarball_name_for_package(package, VERSION)
                for package in packages
            }
            self.assertEqual(
                {path.name for path in output.iterdir()},
                {path.name for path in tarballs.values()},
            )

            root_manifest, root_members = self._read_tarball(
                tarballs["codex-plus-plus"]
            )
            self.assertEqual(root_manifest["name"], ROOT_PACKAGE)
            self.assertEqual(
                root_manifest["bin"],
                {"codex": "bin/codex.js", "codex-plus-plus": "bin/codex.js"},
            )
            self.assertEqual(
                root_manifest["optionalDependencies"],
                {
                    config["alias"]: f"npm:{ROOT_PACKAGE}@{VERSION}-{config['tag']}"
                    for config in PLATFORMS.values()
                },
            )
            self.assertIn("package/bin/codex.js", root_members)
            self.assertIn("package/bin/launcher.js", root_members)
            self.assertIn(
                "package/bin/codex_plus_plus/windows_upstream_launcher.js",
                root_members,
            )

            for package, config in PLATFORMS.items():
                with self.subTest(package=package):
                    manifest, members = self._read_tarball(tarballs[package])
                    self.assertEqual(
                        manifest,
                        {
                            "name": ROOT_PACKAGE,
                            "version": f"{VERSION}-{config['tag']}",
                            "license": "Apache-2.0",
                            "os": [config["os"]],
                            "cpu": [config["cpu"]],
                            "files": ["vendor"],
                            "repository": REPOSITORY,
                            "engines": SOURCE_PACKAGE_JSON["engines"],
                            "packageManager": SOURCE_PACKAGE_JSON["packageManager"],
                        },
                    )
                    self.assertIn(
                        f"package/vendor/{config['target']}/bin/{config['binary']}",
                        members,
                    )
                    with tarfile.open(tarballs[package], "r:gz") as archive:
                        native_manifest = archive.extractfile(
                            f"package/vendor/{config['target']}/codex-package.json"
                        )
                        self.assertEqual(
                            json.load(native_manifest), {"version": "0.144.4"}
                        )

            self.assertEqual(root_manifest["repository"], REPOSITORY)

    def test_publish_is_serial_root_last_and_skips_only_matching_integrity(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            npm_dir = Path(temp_dir)
            entries = self._write_publish_tarballs(npm_dir)
            integrities = [
                release.tarball_integrity(path) for path, _version, _tag in entries
            ]
            views = [None, ROOT_PACKAGE, integrities[0]]
            for integrity in integrities[1:]:
                views.extend([None, integrity])
            with (
                patch.object(release, "npm_view", side_effect=views),
                patch.object(release.subprocess, "run") as run,
            ):
                release.publish(VERSION, npm_dir)

            self.assertEqual(
                [call.args[0][-1] for call in run.call_args_list],
                ["darwin-arm64", "win32-x64", "latest"],
            )

            views = [ROOT_PACKAGE, None, integrities[0], *integrities[1:]]
            with (
                patch.object(release, "npm_view", side_effect=views),
                patch.object(release.subprocess, "run") as run,
            ):
                release.publish(VERSION, npm_dir)
            self.assertEqual(
                [call.args[0][-1] for call in run.call_args_list], ["linux-x64"]
            )

            with (
                patch.object(
                    release, "npm_view", side_effect=[ROOT_PACKAGE, "sha512-wrong"]
                ),
                patch.object(release.subprocess, "run") as run,
                self.assertRaisesRegex(RuntimeError, "Refusing to skip"),
            ):
                release.publish(VERSION, npm_dir)
            run.assert_not_called()

            with (
                patch.object(release, "npm_view", return_value=None) as view,
                patch.object(release.subprocess, "run") as run,
                self.assertRaisesRegex(RuntimeError, "does not exist"),
            ):
                release.publish(VERSION, npm_dir)
            self.assertEqual(
                [call.args for call in view.call_args_list],
                [
                    (ROOT_PACKAGE, "name"),
                    (f"{ROOT_PACKAGE}@{VERSION}-linux-x64", "name"),
                ],
            )
            run.assert_not_called()

    def test_workflow_keeps_public_release_downstream_of_npm_root(self):
        workflow = (
            REPO_ROOT / ".github" / "workflows" / "codex-plus-plus-release.yml"
        ).read_text(encoding="utf-8")
        push_trigger = workflow[
            workflow.index("  push:") : workflow.index("\n\npermissions:")
        ]
        warm_build_cache = workflow[
            workflow.index("  warm-build-cache:") : workflow.index("\n  build:")
        ]
        build = workflow[
            workflow.index("\n  build:") : workflow.index("\n  stage-npm:")
        ]
        github_job = workflow[workflow.index("  publish-github:") :]
        self.assertEqual(
            push_trigger,
            '''  push:
    branches:
      - main
    tags:
      - "codex-plus-plus-v*"
    paths:
      - ".github/actions/setup-msvc-env/**"
      - ".github/actions/setup-rusty-v8/**"
      - ".github/scripts/install-musl-build-tools.sh"
      - ".github/workflows/codex-plus-plus-release.yml"
      - "codex-rs/**"
      - "scripts/build_codex_plus_plus.py"
      - "scripts/build_codex_package.py"
      - "scripts/codex_package/**"''',
        )
        self.assertIn(
            "if: github.event_name == 'push' && github.ref == 'refs/heads/main'",
            warm_build_cache,
        )
        self.assertIn("- name: Warm full-release build", warm_build_cache)
        self.assertIn("actions/cache/save@", warm_build_cache)
        self.assertNotIn("actions/cache/save@", build)
        for job, build_step in (
            (warm_build_cache, "- name: Warm full-release build"),
            (build, "- name: Build package archive"),
        ):
            self.assertLess(
                job.index("rustup toolchain uninstall stable"),
                job.index("Resolve release build cache identity"),
            )
            self.assertLess(
                job.index("uses: ./.github/actions/setup-msvc-env"),
                job.index("Resolve release build cache identity"),
            )
            self.assertLess(
                job.index("actions/cache/restore@"),
                job.index("Regenerate v8 after cache restore"),
            )
            self.assertLess(
                job.index("uses: ./.github/actions/setup-rusty-v8"),
                job.index(build_step),
            )
            self.assertLess(job.index("actions/cache/restore@"), job.index(build_step))
            self.assertIn(
                'prefix="codex-plus-plus-release-v4-${TARGET}-${image}-${toolchain}-${build_inputs}"',
                job,
            )
            self.assertIn("tree=\"$(git rev-parse 'HEAD^{tree}')\"", job)
            self.assertIn(
                'image="${ImageOS:-${RUNNER_OS}}-${ImageVersion:-unknown}"', job
            )
            self.assertIn(
                "toolchain=\"${{ hashFiles('codex-rs/rust-toolchain.toml') }}\"",
                job,
            )
            self.assertIn('build_inputs="${{ hashFiles(', job)
            for build_input in (
                ".github/actions/setup-msvc-env/**",
                ".github/actions/setup-rusty-v8/**",
                ".github/scripts/install-musl-build-tools.sh",
                ".github/workflows/codex-plus-plus-release.yml",
            ):
                self.assertIn(build_input, job)
            self.assertNotIn("sha256sum codex-rs/rust-toolchain.toml", job)
            self.assertIn("~/.cargo/registry", job)
            self.assertIn("~/.cargo/git", job)
            self.assertNotIn("~/.cargo/registry/index", job)
            self.assertNotIn("~/.cargo/git/db", job)
            self.assertIn(
                "codex-rs/target/${{ matrix.target }}/release",
                job,
            )
            self.assertIn("${{ runner.temp }}/codex-package", job)
            self.assertIn("${{ env.SOURCE_MTIME_STATE }}", job)
            self.assertIn("${{ steps.release-cache.outputs.prefix }}", job)
        self.assertEqual(workflow.count("actions/cache/restore@"), 2)
        self.assertEqual(workflow.count("actions/cache/save@"), 1)
        self.assertNotIn("Swatinem/rust-cache@", workflow)
        self.assertNotIn("cache-workspace-crates:", workflow)
        self.assertEqual(workflow.count("rustup toolchain uninstall stable"), 2)
        self.assertEqual(workflow.count("uses: ./.github/actions/setup-msvc-env"), 2)
        self.assertEqual(workflow.count("uses: ./.github/actions/setup-rusty-v8"), 2)
        self.assertNotIn("  warm-cache:", workflow)
        self.assertIn("--cargo-profile release", warm_build_cache)
        self.assertIn('--build-cache-state "$SOURCE_MTIME_STATE"', warm_build_cache)
        self.assertIn("--cargo-profile release", build)
        self.assertIn('--build-cache-state "$SOURCE_MTIME_STATE"', build)
        self.assertIn("test_source_mtime_cache.py", workflow)
        self.assertIn("source_mtime_cache.py", workflow)
        self.assertEqual(workflow.count("SOURCE_MTIME_STATE:"), 2)
        self.assertIn("      - publish-npm", github_job)
        self.assertIn("      id-token: write", workflow)
        self.assertEqual(workflow.count("npm@11.18.0"), 2)
        self.assertIn("  workflow_dispatch:", workflow)
        self.assertIn("source_run_id:", workflow)
        self.assertIn("release_tag:", workflow)
        self.assertIn("run-id: ${{ needs.prepare.outputs.source_run_id }}", workflow)
        self.assertIn(
            "SOURCE_RUN_ID: ${{ needs.prepare.outputs.source_run_id }}", workflow
        )
        self.assertIn('run_workflow_id" == "$workflow_id', workflow)
        self.assertIn('run_sha" == "$tag_sha', workflow)
        self.assertIn(
            "format('refs/tags/{0}', inputs.release_tag) || github.ref", workflow
        )
        self.assertIn('node-version: "24"', workflow)
        self.assertEqual(workflow.count("package-manager-cache: false"), 2)
        self.assertIn("--generate-notes", github_job)
        self.assertIn('gh release upload "$RELEASE_TAG"', github_job)
        self.assertIn('if [[ "$release_is_draft" == "true" ]]', github_job)
        self.assertNotIn("is already published", github_job)
        self.assertNotIn("NODE_AUTH_TOKEN", workflow)

    @staticmethod
    def _write_release_archive(path: Path, package_dir: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.suffix == ".zip":
            with zipfile.ZipFile(path, "w") as archive:
                for member in package_dir.rglob("*"):
                    archive.write(member, member.relative_to(package_dir))
            return
        with tarfile.open(path, "w:gz") as archive:
            for member in package_dir.rglob("*"):
                archive.add(member, member.relative_to(package_dir), recursive=False)

    @staticmethod
    def _write_publish_tarballs(npm_dir: Path):
        entries = release.release_entries(VERSION, npm_dir)
        for path, version, _tag in entries:
            manifest = json.dumps(
                {"name": ROOT_PACKAGE, "version": version, "repository": REPOSITORY}
            ).encode()
            info = tarfile.TarInfo("package/package.json")
            info.size = len(manifest)
            with tarfile.open(path, "w:gz") as archive:
                archive.addfile(info, io.BytesIO(manifest))
        return entries

    @staticmethod
    def _read_tarball(path: Path):
        with tarfile.open(path, "r:gz") as archive:
            manifest_file = archive.extractfile("package/package.json")
            if manifest_file is None:
                raise AssertionError(f"Missing package.json in {path}")
            return json.load(manifest_file), set(archive.getnames())
