"""Windows fork archives must carry only the receipt-verified voice payload."""

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from codex_package.codex_plus_plus import voice
from runtime import digest


class VoicePackageTests(unittest.TestCase):
    def test_fork_archive_keeps_verified_runtime_and_rejects_tampering(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app, work = root / "app", root / "voice"
            (app / "bin").mkdir(parents=True)
            (app / "bin/codex.exe").write_bytes(b"app")
            (app / "codex-package.json").write_text(
                json.dumps(
                    {
                        "layoutVersion": 1,
                        "version": "0.155.1-fork.2",
                        "target": "x86_64-pc-windows-msvc",
                        "variant": "codex",
                        "entrypoint": "bin/codex.exe",
                        "resourcesDir": "codex-resources",
                        "pathDir": "codex-path",
                    }
                )
            )
            runtime = work / "runtime"
            (runtime / "bin").mkdir(parents=True)
            (work / "codex-voice-host.exe").write_bytes(b"helper")
            plugins = [
                f"bin/gst{name}.dll"
                for name in (
                    "app",
                    "audioconvert",
                    "audioresample",
                    "coreelements",
                    "opus",
                    "rtp",
                    "rtpmanager",
                )
            ]
            files = plugins + [
                "bin/gio-2.0-0.dll",
                "bin/gstreamer-1.0-0.dll",
                "bin/vcruntime140.dll",
            ]
            records = []
            for name in files:
                (runtime / name).write_bytes(name.encode())
                records.append({"path": name, "sha256": digest(runtime / name)})
            commit = "a" * 40
            (runtime / "runtime.json").write_text(
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "developmentOnly": False,
                        "distribution": "publicRelease",
                        "target": "x86_64-pc-windows-msvc",
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
            archive = root / "release.zip"
            # Native startup is proved by the mandatory moved-package smoke in release jobs.
            with patch.object(voice, "smoke"):
                voice.package(app, work, root / "output", archive, commit)
            before = archive.read_bytes()
            with zipfile.ZipFile(archive) as contents:
                self.assertEqual(contents.read("bin/codex.exe"), b"app")
                self.assertEqual(
                    contents.read("codex-resources/voice/bin/codex-voice-host.exe"),
                    b"helper",
                )
                self.assertEqual(
                    contents.read("codex-resources/voice/bin/vcruntime140.dll"),
                    b"bin/vcruntime140.dll",
                )
                self.assertNotIn(
                    "codex-resources/voice/bin/unlisted.dll", contents.namelist()
                )
                self.assertEqual(
                    contents.read("codex-resources/voice/licenses/LGPL-2.1.txt"),
                    (
                        voice.REPO / "third_party/voice/licenses/LGPL-2.1.txt"
                    ).read_bytes(),
                )
            (runtime / "bin/vcruntime140.dll").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "digest mismatch"):
                voice.package(app, work, root / "rejected", archive, commit)
            self.assertEqual(archive.read_bytes(), before)
            self.assertFalse((root / "rejected").exists())


if __name__ == "__main__":
    unittest.main()
