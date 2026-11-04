#!/usr/bin/env python3
"""Append verified release assets; callers serialize publication across tags."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


def gh(*args, body=None, missing=False):
    result = subprocess.run(
        ["gh", *args],
        input=json.dumps(body).encode() if body is not None else None,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        if missing and b"HTTP 404" in result.stderr:
            return None
        raise RuntimeError(result.stderr.decode().strip())
    return (
        json.loads(result.stdout)
        if args[0] == "api" and result.stdout.strip()
        else None
    )


def version_key(tag):
    match = re.fullmatch(r"codex-plus-plus-v(\d+)\.(\d+)\.(\d+)-fork\.(\d+)", tag)
    if not match:
        raise RuntimeError(f"Unexpected stable release tag: {tag}")
    return tuple(map(int, match.groups()))


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def publish(repo, version, source_sha, archives, npm, target):
    tag = f"codex-plus-plus-v{version}"
    key = version_key(tag)
    api = f"repos/{repo}"
    commit = gh("api", f"{api}/commits/{tag}")["sha"]
    if commit != source_sha:
        raise RuntimeError("Release tag no longer matches the validated source run")
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        assets = (
            list(archives.glob(f"codex-plus-plus-{version}-{target}.*"))
            if target
            else list(archives.iterdir())
        )
        assets = [path for path in assets if not path.name.endswith(".sha256")]
        if not assets:
            raise RuntimeError("Missing verified native archive")
        if npm:
            assets.extend(npm.iterdir())
        for suffix in ("sh", "ps1"):
            for name in (
                f"install-codex-plus-plus.{suffix}",
                f"install-codex-plus-plus-latest.{suffix}",
            ):
                content = gh(
                    "api", f"{api}/contents/scripts/install/{name}?ref={commit}"
                )
                path = directory / name
                path.write_bytes(base64.b64decode(content["content"]))
                assets.append(path)
        files = {path.name: path for path in assets}
        for name, path in list(files.items()):
            if not name.endswith(".sha256"):
                checksum = directory / f"{name}.sha256"
                checksum.write_bytes(f"{sha256(path)}  {name}\n".encode())
                files[checksum.name] = checksum
        current = gh("api", f"{api}/releases/tags/{tag}", missing=True)
        latest = gh("api", f"{api}/releases/latest", missing=True)
        make_latest = latest is None or key >= version_key(latest["tag_name"])
        if current is None:
            current = gh(
                "api",
                f"{api}/releases",
                "--method",
                "POST",
                "--input",
                "-",
                body={
                    "tag_name": tag,
                    "name": f"Codex++ {version}",
                    "draft": True,
                    "generate_release_notes": True,
                    "make_latest": "false",
                },
            )
        # Paginate even for large releases. Verify all existing bytes before uploading.
        pages = gh(
            "api",
            f"{api}/releases/{current['id']}/assets?per_page=100",
            "--paginate",
            "--slurp",
        )
        existing = {asset["name"]: asset for page in pages for asset in page}
        pending = []
        for name, path in files.items():
            if name not in existing:
                pending.append(path)
                continue
            asset = existing[name]
            # GitHub's SHA-256 digest is absent on some older assets.
            digest = asset.get("digest")
            if not digest:
                download = directory / "existing"
                download.mkdir(exist_ok=True)
                gh(
                    "release",
                    "download",
                    tag,
                    "--repo",
                    repo,
                    "--pattern",
                    name,
                    "--dir",
                    str(download),
                )
                digest = f"sha256:{sha256(download / name)}"
            if digest != f"sha256:{sha256(path)}":
                raise RuntimeError(f"Conflicting release asset: {name}")
        if pending and current.get("immutable"):
            raise RuntimeError("Immutable releases cannot append platform assets")
        if pending:
            gh("release", "upload", tag, *map(str, pending), "--repo", repo)
        gh(
            "api",
            f"{api}/releases/{current['id']}",
            "--method",
            "PATCH",
            "--input",
            "-",
            body={
                "draft": False,
                "make_latest": "true" if make_latest else "false",
            },
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--archives-dir", type=Path, required=True)
    parser.add_argument("--npm-dir", type=Path)
    parser.add_argument("--target")
    args = parser.parse_args()
    publish(
        os.environ["GITHUB_REPOSITORY"],
        args.version,
        args.source_sha,
        args.archives_dir,
        args.npm_dir,
        args.target,
    )
