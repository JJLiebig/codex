#!/usr/bin/env python3
"""Build a Codex++ package with a temporary public fork version."""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
CODEX_RS = REPO_ROOT / "codex-rs"
WORKSPACE_MANIFEST = CODEX_RS / "Cargo.toml"
CARGO_LOCK = CODEX_RS / "Cargo.lock"
DEFAULT_PACKAGE_DIR = "dist/codex-plus-plus"
PACKAGE_VERSION_PATTERN = re.compile(
    r'^(?P<indent>\s*)version(?:\.workspace)?\s*=\s*(?:true|"[^"]*")'
    r"(?P<suffix>\s*(?:#.*)?)$"
)
VERSIONED_PACKAGE_MANIFESTS = {
    "codex": CODEX_RS / "cli" / "Cargo.toml",
    "codex-app-server": CODEX_RS / "app-server" / "Cargo.toml",
}

sys.path.insert(0, str(REPO_ROOT / "scripts"))

from codex_package.cli import parse_args as parse_package_args  # noqa: E402
from codex_package.codex_plus_plus.source_mtime_cache import (  # noqa: E402
    restore_source_mtimes,
)
from codex_package.codex_plus_plus.source_mtime_cache import (  # noqa: E402
    save_source_mtimes,
)
from codex_package.version import read_workspace_version  # noqa: E402


def main() -> int:
    args, package_args = parse_args()
    base_version = read_workspace_version()
    fork_version = args.fork_version or (
        suffixed_version(base_version, args.suffix)
        if args.suffix is not None
        else next_fork_version(base_version)
    )
    tag_name = f"{args.tag_prefix}{fork_version}"
    package_args = [
        *default_package_args(package_args),
        "--package-version",
        fork_version,
    ]
    package_manifest = versioned_package_manifest(package_args)
    build_cache_state = resolve_build_cache_state(args.build_cache_state)
    build_entrypoint = parse_package_args(package_args).entrypoint_bin is None

    original_package_manifest = (
        package_manifest.read_bytes() if build_entrypoint else None
    )
    original_lock = CARGO_LOCK.read_bytes() if CARGO_LOCK.exists() else None
    try:
        if original_package_manifest is not None:
            package_manifest.write_bytes(
                replace_package_version(
                    original_package_manifest.decode("utf-8"), fork_version
                ).encode("utf-8")
            )
            normalize_workspace_lockfile(package_args)
        if build_cache_state is not None:
            try:
                restore_source_mtimes(CODEX_RS, build_cache_state)
            except OSError as error:
                print(
                    f"Ignoring source mtime cache restore failure: {error}",
                    file=sys.stderr,
                )

        print(f"Codex++ package version: {fork_version}", flush=True)
        print(f"Suggested git tag: {tag_name}", flush=True)
        build_status = subprocess.call(
            [
                sys.executable,
                str(REPO_ROOT / "scripts" / "build_codex_package.py"),
                *package_args,
            ],
            cwd=REPO_ROOT,
        )
        if build_status == 0 and build_cache_state is not None:
            try:
                save_source_mtimes(CODEX_RS, build_cache_state)
            except OSError as error:
                print(
                    f"Ignoring source mtime cache save failure: {error}",
                    file=sys.stderr,
                )
    finally:
        if original_package_manifest is not None:
            package_manifest.write_bytes(original_package_manifest)
        if original_lock is None:
            CARGO_LOCK.unlink(missing_ok=True)
        else:
            CARGO_LOCK.write_bytes(original_lock)

    if build_status != 0 or not args.install:
        return build_status
    return install_package(package_args)


def parse_args() -> tuple[argparse.Namespace, list[str]]:
    parser = argparse.ArgumentParser(
        description="Build Codex++ with an exact public package version.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--fork-version",
        help="Exact temporary package version to use for the packaged entrypoint.",
    )
    parser.add_argument(
        "--suffix",
        help="Override the suffix appended when --fork-version is omitted.",
    )
    parser.add_argument(
        "--tag-prefix",
        default="codex-plus-plus-v",
        help="Prefix used when printing the suggested fork tag.",
    )
    parser.add_argument(
        "--build-cache-state",
        type=Path,
        help=(
            "Optional source-mtime state file used with a restored Cargo target "
            "cache. Relative paths are resolved from the repository root."
        ),
    )
    parser.add_argument(
        "--install",
        action="store_true",
        help="Install the completed native package as the active Codex++ command.",
    )
    parser.add_argument(
        "package_args",
        nargs=argparse.REMAINDER,
        help=(
            "Arguments forwarded to scripts/build_codex_package.py. "
            "Use -- before these."
        ),
    )
    args = parser.parse_args()
    package_args = args.package_args
    if package_args[:1] == ["--"]:
        package_args = package_args[1:]
    return args, package_args


def suffixed_version(version: str, suffix: str) -> str:
    suffix = suffix.strip().strip("-")
    if not suffix:
        return version
    return f"{version}-{suffix}"


def next_fork_version(version: str) -> str:
    tag_prefix = f"codex-plus-plus-v{version}-fork."
    result = subprocess.run(
        ["git", "tag", "--list", f"{tag_prefix}*"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    revisions = [
        int(revision)
        for tag in result.stdout.splitlines()
        if (revision := tag.removeprefix(tag_prefix)).isdigit()
    ]
    return f"{version}-fork.{max(revisions, default=0) + 1}"


def default_package_args(package_args: list[str]) -> list[str]:
    defaults: list[str] = []
    if option_value(package_args, "--cargo-profile") is None:
        defaults.extend(["--cargo-profile", "release-fast"])
    if package_dir_arg(package_args) is None:
        defaults.extend(["--package-dir", DEFAULT_PACKAGE_DIR, "--force"])
    return [*defaults, *package_args]


def option_value(args: list[str], option: str) -> str | None:
    for index in range(len(args) - 1, -1, -1):
        arg = args[index]
        if arg == option:
            return args[index + 1] if index + 1 < len(args) else None
        prefix = f"{option}="
        if arg.startswith(prefix):
            return arg[len(prefix) :]
    return None


def package_dir_arg(args: list[str]) -> Path | None:
    return getattr(parse_package_args(args), "package_dir", None)


def versioned_package_manifest(args: list[str]) -> Path:
    variant = parse_package_args(args).variant
    try:
        return VERSIONED_PACKAGE_MANIFESTS[variant]
    except KeyError as error:
        raise RuntimeError(
            f"Codex++ does not know which package manifest owns variant {variant!r}"
        ) from error


def normalize_workspace_lockfile(package_args: list[str]) -> None:
    cargo = getattr(parse_package_args(package_args), "cargo", "cargo")
    subprocess.run(
        [
            cargo,
            "update",
            "--workspace",
            "--manifest-path",
            str(WORKSPACE_MANIFEST),
        ],
        cwd=REPO_ROOT,
        check=True,
    )


def resolve_build_cache_state(path: Path | None) -> Path | None:
    if path is None or path.is_absolute():
        return path
    return REPO_ROOT / path


def replace_package_version(cargo_toml: str, version: str) -> str:
    in_section = False
    lines = cargo_toml.splitlines(keepends=True)
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped == "[package]":
            in_section = True
            continue
        if in_section and stripped.startswith("["):
            break
        if not in_section:
            continue

        body = line.rstrip("\r\n")
        newline = line[len(body) :]
        match = PACKAGE_VERSION_PATTERN.match(body)
        if match is not None:
            lines[index] = (
                f'{match.group("indent")}version = "{version}"'
                f"{match.group('suffix')}{newline}"
            )
            return "".join(lines)

    raise RuntimeError("Could not find [package].version in Cargo manifest")


def install_package(package_args: list[str]) -> int:
    package_dir = package_dir_arg(package_args)
    if package_dir is None:
        raise RuntimeError("Codex++ package directory was not configured")
    if not package_dir.is_absolute():
        package_dir = REPO_ROOT / package_dir
    target_exe = package_dir / "bin" / ("codex.exe" if os.name == "nt" else "codex")
    if not target_exe.is_file():
        print(f"Built Codex++ executable was not found: {target_exe}", file=sys.stderr)
        return 1

    if os.name == "nt":
        command = [
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(REPO_ROOT / "scripts" / "install" / "install-codex-plus-plus.ps1"),
            "-TargetExe",
            str(target_exe),
            "-PythonExe",
            sys.executable,
            "-Install",
            "-AddToUserPath",
        ]
    else:
        command = [
            "sh",
            str(REPO_ROOT / "scripts" / "install" / "install-codex-plus-plus.sh"),
            "--target-exe",
            str(target_exe),
            "--install",
        ]
    return subprocess.call(command, cwd=REPO_ROOT)


if __name__ == "__main__":
    raise SystemExit(main())
