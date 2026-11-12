import base64
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import threading
import urllib.parse
import sys
import unittest


class PlatformRefreshTest(unittest.TestCase):
    @unittest.skipUnless(
        sys.platform == "linux" and os.uname().machine == "x86_64",
        "Linux native fixture",
    )
    def test_late_native_build_refreshes_an_unchanged_real_launcher(self):
        repo = Path(__file__).resolve().parents[3]
        npm = shutil.which("npm")
        if npm is None:
            self.skipTest("npm is required")
        temporary = tempfile.TemporaryDirectory(prefix="codex-platform-refresh-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        name = "@jjliebig/codex-plus-plus"
        version = "1.0.0-fork.5"
        old = "1.0.0-fork.4"
        linux_a, linux_b = old + "-linux-x64", version + "-linux-x64"
        tags = {
            "latest": version,
            "linux-x64": linux_a,
            "darwin-arm64": old + "-darwin-arm64",
            "win32-x64": old + "-win32-x64",
        }
        versions, blobs = {}, {}
        env = {
            **os.environ,
            "npm_config_cache": str(root / "npm-cache"),
            "npm_config_userconfig": str(root / "npmrc"),
            "npm_config_globalconfig": str(root / "global-npmrc"),
            "npm_config_audit": "false",
            "npm_config_fund": "false",
            "BUN_INSTALL": str(root / "bun"),
            "BUN_INSTALL_CACHE_DIR": str(root / "bun-cache"),
        }
        (root / "npmrc").write_text("")

        def run(command):
            result = subprocess.run(
                command, cwd=root, env=env, capture_output=True, text=True, timeout=120
            )
            if result.returncode:
                raise RuntimeError(
                    json.dumps(
                        {
                            "command": command,
                            "code": result.returncode,
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                        }
                    )
                )
            return result.stdout.strip()

        def register(manifest, blob):
            key = manifest["version"]
            blobs[key] = blob
            versions[key] = {
                **manifest,
                "dist": {
                    "integrity": "sha512-"
                    + base64.b64encode(hashlib.sha512(blob).digest()).decode()
                },
            }

        def native(package_version, os_name, cpu, target, marker):
            manifest = {
                "name": name,
                "version": package_version,
                "os": [os_name],
                "cpu": [cpu],
            }
            raw = io.BytesIO()
            binary = "codex.exe" if os_name == "win32" else "codex"
            files = {
                "package.json": json.dumps(manifest).encode(),
                "vendor/" + target + "/bin/" + binary: (
                    '#!/bin/sh\nprintf "%s\\n" "' + marker + '"\n'
                ).encode(),
            }
            with tarfile.open(fileobj=raw, mode="w") as archive:
                for filename, content in files.items():
                    member = tarfile.TarInfo("package/" + filename)
                    member.mode = 0o755 if filename.endswith("/" + binary) else 0o644
                    member.size = len(content)
                    archive.addfile(member, io.BytesIO(content))
            register(manifest, gzip.compress(raw.getvalue(), mtime=0))

        class Registry(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                path = urllib.parse.unquote(urllib.parse.urlparse(self.path).path)
                if path == "/" + name:
                    body, kind = (
                        json.dumps(
                            {"name": name, "dist-tags": tags, "versions": versions}
                        ).encode(),
                        "application/json",
                    )
                elif (
                    path.startswith("/tarball/")
                    and path.removeprefix("/tarball/") in blobs
                ):
                    body, kind = (
                        blobs[path.removeprefix("/tarball/")],
                        "application/octet-stream",
                    )
                else:
                    self.send_response(404)
                    self.end_headers()
                    return
                self.send_response(200)
                self.send_header("Content-Type", kind)
                self.send_header("Cache-Control", "public, max-age=300")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        server = None
        try:
            run(
                [
                    sys.executable,
                    str(repo / "codex-cli/scripts/build_npm_package.py"),
                    "--release-version",
                    version,
                    "--package",
                    "codex-plus-plus",
                    "--staging-dir",
                    str(root / "root-stage"),
                    "--pack-output",
                    str(root / "root.tgz"),
                ]
            )
            with tarfile.open(root / "root.tgz") as archive:
                manifest = json.load(archive.extractfile("package/package.json"))
            register(manifest, (root / "root.tgz").read_bytes())
            native(linux_a, "linux", "x64", "x86_64-unknown-linux-musl", "A")
            native(linux_b, "linux", "x64", "x86_64-unknown-linux-musl", "B")
            native(
                tags["darwin-arm64"],
                "darwin",
                "arm64",
                "aarch64-apple-darwin",
                "MAC_PREVIOUS",
            )
            native(
                tags["win32-x64"],
                "win32",
                "x64",
                "x86_64-pc-windows-msvc",
                "WINDOWS_PREVIOUS",
            )
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Registry)
            registry = "http://127.0.0.1:" + str(server.server_port)
            for key, entry in versions.items():
                entry["dist"]["tarball"] = registry + "/tarball/" + key
            threading.Thread(target=server.serve_forever, daemon=True).start()
            env["npm_config_registry"] = registry
            (root / ".npmrc").write_text("registry=" + registry + "\n")
            (root / "npmrc").write_text("registry=" + registry + "\n")
            for manager in ("npm", "bun") if shutil.which("bun") else ("npm",):
                tags["linux-x64"] = linux_a
                env["npm_config_prefix"] = str(root / "npm-prefix")
                initial_command = (
                    [npm, "install", "-g", name, "--prefer-online"]
                    if manager == "npm"
                    else ["bun", "install", "-g", name]
                )
                update_command = (
                    initial_command
                    if manager == "npm"
                    else ["bun", "update", "-g", name + "@latest"]
                )
                executable = root / (
                    "npm-prefix/bin/codex" if manager == "npm" else "bun/bin/codex"
                )
                run(initial_command)
                initial = run([str(executable), "--version"])
                tags["linux-x64"] = linux_b
                run(update_command)
                updated = run([str(executable), "--version"])
                root_manifest = root / (
                    "npm-prefix/lib/node_modules/@jjliebig/codex-plus-plus/package.json"
                    if manager == "npm"
                    else "bun/install/global/node_modules/@jjliebig/codex-plus-plus/package.json"
                )
                installed = json.loads(root_manifest.read_text())
                self.assertEqual(initial, "A", manager)
                self.assertEqual(updated, "B", manager)
                self.assertEqual(installed["version"], version)

        finally:
            if server:
                server.shutdown()
                server.server_close()


if __name__ == "__main__":
    unittest.main()
