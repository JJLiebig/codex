"""Fork archives must carry only the receipt-verified voice payload."""

import hashlib
import json
import os
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from codex_package.codex_plus_plus import voice
from runtime import PLUGINS, digest, required_library_paths


class VoicePackageTests(unittest.TestCase):
    def test_application_stamp_must_match_the_helper_commit(self):
        for target in (
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-musl",
        ):
            for provider_id in (
                None,
                "sha256:wrong-build",
                "sha256:"
                + hashlib.sha256(f"git:{'a' * 40}:{target}".encode()).hexdigest(),
            ):
                with (
                    self.subTest(provider_id=provider_id),
                    patch.object(voice.subprocess, "Popen") as spawn,
                ):
                    spawn.return_value.stdout.readline.return_value = json.dumps(
                        {
                            "id": 1,
                            "result": {"environmentInfo": {"providerId": provider_id}},
                        }
                    )
                    if provider_id is None or provider_id.endswith("wrong-build"):
                        with self.assertRaisesRegex(
                            RuntimeError, "same compiled commit"
                        ):
                            voice.verify_app_identity(Path("package"), "a" * 40, target)
                    else:
                        voice.verify_app_identity(Path("package"), "a" * 40, target)

    def test_fork_archive_keeps_verified_runtime_and_rejects_tampering(self):
        self.check_archive("x86_64-pc-windows-msvc", "bin/gst{}.dll")

    @unittest.skipIf(os.name == "nt", "Unix executable modes require a Unix filesystem")
    def test_unix_archives_keep_runtime_and_executable_modes(self):
        for target, plugin in (
            ("aarch64-apple-darwin", "plugins/libgst{}.dylib"),
            ("x86_64-unknown-linux-musl", "lib/gstreamer-1.0/libgst{}.so"),
        ):
            with self.subTest(target=target):
                self.check_archive(target, plugin)

    def check_archive(self, target, plugin):
        suffix = ".exe" if target.endswith("windows-msvc") else ""
        voice_target = target.replace("-musl", "-gnu")
        entrypoint = f"bin/codex{suffix}"
        helper_name = f"codex-voice-host{suffix}"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app, work = root / "app", root / "voice"
            (app / "bin").mkdir(parents=True)
            (app / entrypoint).write_bytes(b"app")
            (app / entrypoint).chmod(0o755)
            (app / "codex-package.json").write_text(
                json.dumps(
                    {
                        "layoutVersion": 1,
                        "version": "0.155.1-fork.3",
                        "target": target,
                        "variant": "codex",
                        "entrypoint": entrypoint,
                        "resourcesDir": "codex-resources",
                        "pathDir": "codex-path",
                    }
                )
            )
            runtime = work / "runtime"
            (runtime / "bin").mkdir(parents=True)
            (work / helper_name).write_bytes(b"helper")
            (work / helper_name).chmod(0o755)
            plugins = [plugin.format(name) for name in PLUGINS]
            files = plugins + list(required_library_paths(voice_target))
            if suffix:
                files += ["bin/gstreamer-1.0-0.dll", "bin/vcruntime140.dll"]
            records = []
            for name in files:
                (runtime / name).parent.mkdir(parents=True, exist_ok=True)
                (runtime / name).write_bytes(name.encode())
                records.append({"path": name, "sha256": digest(runtime / name)})
            commit = "a" * 40
            (runtime / "runtime.json").write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "developmentOnly": False,
                        "distribution": "publicRelease",
                        "target": voice_target,
                        "sourceCommit": commit,
                        "plugins": plugins,
                        "libraries": records,
                        "sourceManifestSha256": digest(
                            voice.REPO / "third_party/voice/sources.json"
                        ),
                    }
                )
            )
            (runtime / "bin/unlisted.dll").write_bytes(
                b"not part of the verified runtime"
            )
            archive = root / ("release.zip" if suffix else "release.tar.gz")
            # Native startup is proved by the mandatory moved-package smoke in release jobs.
            with patch.object(voice, "smoke"):
                voice.package(app, work, root / "output", archive, commit)
            before = archive.read_bytes()
            if suffix:
                with zipfile.ZipFile(archive) as contents:
                    archived = {
                        name: contents.read(name) for name in contents.namelist()
                    }
            else:
                with tarfile.open(archive) as contents:
                    archived = {
                        member.name: contents.extractfile(member).read()
                        for member in contents.getmembers()
                        if member.isfile()
                    }
                    for name in (
                        entrypoint,
                        f"codex-resources/voice/bin/{helper_name}",
                    ):
                        self.assertEqual(contents.getmember(name).mode & 0o111, 0o111)
            self.assertEqual(archived[entrypoint], b"app")
            self.assertEqual(
                archived[f"codex-resources/voice/bin/{helper_name}"], b"helper"
            )
            for name in files:
                self.assertEqual(
                    archived[f"codex-resources/voice/{name}"], name.encode()
                )
            self.assertNotIn("codex-resources/voice/bin/unlisted.dll", archived)
            self.assertEqual(
                archived["codex-resources/voice/licenses/LGPL-2.1.txt"],
                (voice.REPO / "third_party/voice/licenses/LGPL-2.1.txt").read_bytes(),
            )
            (runtime / files[-1]).write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                voice.package(app, work, root / "rejected", archive, commit)
            self.assertEqual(archive.read_bytes(), before)
            self.assertFalse((root / "rejected").exists())


if __name__ == "__main__":
    unittest.main()
