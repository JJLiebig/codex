import os
from contextlib import closing
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import zipfile
from unittest.mock import patch

import test_recover


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/install/install-codex-plus-plus.ps1"


@unittest.skipUnless(os.name == "nt", "Windows managed upgrade")
class WindowsUpgradeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.compiled = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.compiled.cleanup)
        source = Path(cls.compiled.name) / "fixture.cs"
        source.write_text("""using System;
using System.Diagnostics;
using System.IO;
class Fixture {
  static int Main(string[] args) {
    if (args.Length > 0 && args[0] == "debug") {
      Console.WriteLine(Path.Combine(Environment.GetEnvironmentVariable("CODEX_SQLITE_HOME"), "thread_history_1.sqlite"));
      return 0;
    }
    if (args.Length > 0 && args[0] == "stage") {
      var p = Process.Start(new ProcessStartInfo("powershell.exe", Environment.GetEnvironmentVariable("TEST_INSTALL_ARGS")) { UseShellExecute = false });
      p.WaitForExit(); return p.ExitCode;
    }
    if (args.Length > 0 && args[0] == "hold") {
      Console.WriteLine("ready");
      while (!File.Exists(Environment.GetEnvironmentVariable("TEST_RELEASE"))) System.Threading.Thread.Sleep(50);
      return 0;
    }
    Console.WriteLine("fixture started"); return 0;
  }
}""")
        cls.binary = source.with_name("codex.exe")
        compiler = (
            Path(os.environ["WINDIR"]) / "Microsoft.NET/Framework64/v4.0.30319/csc.exe"
        )
        subprocess.run(
            [str(compiler), "/nologo", f"/out:{cls.binary}", str(source)],
            check=True,
            capture_output=True,
        )

    def setUp(self):
        self.fixture = test_recover.RecoveryTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.home = self.root / "home"
        self.home.mkdir()
        self.fixture.path = self.home / "thread_history_1.sqlite"
        self.shim = self.root / "shim"
        self.package = self.root / "package"
        (self.package / "bin").mkdir(parents=True)
        shutil.copyfile(self.binary, self.package / "bin/codex.exe")
        (self.package / "codex-package.json").write_text("{}")
        sys.path.insert(0, str(ROOT / "scripts"))
        from codex_package.codex_plus_plus.history_upgrade import add_recovery

        add_recovery(self.package)
        # Only this synthetic package substitutes the global process enumerator.
        # Real DB-holder discovery and generation leases remain in use. The native
        # enumerator itself is exercised separately below; user processes are untouched.
        writers = self.package / "codex-resources/history-upgrade/windows_writers.py"
        with writers.open("a") as stream:
            stream.write("\ndef codex_processes():\n    return set()\n")
        self.env = {
            **os.environ,
            "CODEX_HOME": str(self.home),
            "CODEX_SQLITE_HOME": str(self.home),
        }
        self.release = self.root / "release"
        self.env["TEST_RELEASE"] = str(self.release)
        self.command = [
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(INSTALLER),
            "-TargetExe",
            str(self.package / "bin/codex.exe"),
            "-ShimDir",
            str(self.shim),
            "-PythonExe",
            sys.executable,
            "-Install",
        ]
        self.assertEqual(self.install().returncode, 0)
        self.pointer = self.shim / ".codex-plus-plus-current"
        self.original = self.pointer.read_bytes()
        self.pending = self.shim / ".codex-plus-plus-pending.json"

    def install(self, command=None):
        return subprocess.run(
            command or self.command,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
        )

    def affected(self):
        self.fixture.fixture(newline="crlf", journal="wal").close()

    def launch(self, command, input_pipe=True):
        process = subprocess.Popen(
            command,
            env=self.env,
            stdin=subprocess.PIPE if input_pipe else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def cleanup():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream:
                    stream.close()

        self.addCleanup(cleanup)
        return process

    def shim_command(self, kind="ps", *args):
        if kind == "ps":
            return [
                "powershell.exe",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(self.shim / "codex.ps1"),
                *args,
            ]
        return [os.environ["COMSPEC"], "/d", "/c", str(self.shim / "codex.cmd"), *args]

    def wait_gates(self):
        deadline = time.monotonic() + 20
        while not list(self.shim.glob(".codex-plus-plus-leases/*/.pruning")):
            if time.monotonic() > deadline:
                self.fail("maintenance did not fence launches")
            time.sleep(0.05)

    def test_self_update_returns_pending_then_next_launch_drains_leases_and_idle_holder(
        self,
    ):
        import windows_writers

        self.affected()
        old = self.launch(self.shim_command("ps", "hold"), input_pipe=False)
        self.assertEqual(old.stdout.readline().strip(), "ready")
        # Pause an already loaded old CMD wrapper after its first gate check,
        # before it publishes a lease. It must recheck before starting its target.
        pause = self.root / "pause.py"
        pause.write_text(
            "import pathlib,time,sys\np=pathlib.Path(sys.argv[1]); p.with_suffix('.ready').touch()\nwhile not p.exists(): time.sleep(.05)\n"
        )
        released = self.root / "racing-launch"
        stale = self.shim / "already-loaded.cmd"
        source = (self.shim / "codex.cmd").read_text()
        pause_command = subprocess.list2cmdline(
            [sys.executable, str(pause), str(released)]
        )
        stale.write_text(
            source.replace(
                'set "CODEX_PLUS_PLUS_LEASE=',
                pause_command + '\nset "CODEX_PLUS_PLUS_LEASE=',
            )
        )
        racing = self.launch([str(stale)])
        deadline = time.monotonic() + 10
        while not released.with_suffix(".ready").exists():
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)
        # This updater actually runs inside an old CMD-generation lease.
        self.env["TEST_INSTALL_ARGS"] = subprocess.list2cmdline(self.command[1:])
        staged = self.install(self.shim_command("cmd", "stage"))
        self.assertEqual(staged.returncode, 75, staged.stdout + staged.stderr)
        self.assertTrue(self.pending.exists())
        self.assertEqual(self.pointer.read_bytes(), self.original)
        writer = self.launch(
            [
                sys.executable,
                "-u",
                "-c",
                "import sqlite3,sys; db=sqlite3.connect(sys.argv[1]); db.execute('SELECT * FROM thread_turns').fetchall(); print('ready',flush=True); input(); db.close()",
                str(self.fixture.path),
            ]
        )
        self.assertEqual(writer.stdout.readline().strip(), "ready")
        self.assertIn(
            writer.pid,
            {pid for pid, _ in windows_writers.database_users(self.fixture.path)},
        )
        finishing = self.launch(self.shim_command("cmd"))
        self.wait_gates()
        released.touch()
        self.assertEqual(self.pointer.read_bytes(), self.original)
        self.release.touch()
        old.communicate(timeout=10)
        time.sleep(1.2)
        self.assertIsNone(finishing.poll(), "idle pool must prevent activation")
        self.assertIsNone(
            racing.poll(),
            "an already loaded launcher must respect the maintenance fence",
        )
        writer.communicate("\n", timeout=10)
        out, err = finishing.communicate(timeout=30)
        self.assertEqual(finishing.returncode, 0, out + err)
        self.assertIn("fixture started", out)
        raced_out, raced_err = racing.communicate(timeout=10)
        if racing.returncode == 0:
            self.assertIn("fixture started", raced_out)
        else:
            # An old wrapper may fail reading the pointer during File.Replace;
            # it must never start the old target while maintenance is fenced.
            self.assertNotIn("fixture started", raced_out, raced_err)
            self.assertEqual(self.install(self.shim_command()).returncode, 0)
        self.assertFalse(self.pending.exists())
        self.assertNotEqual(self.pointer.read_bytes(), self.original)
        import recover

        self.assertEqual(recover.recover(self.fixture.path)["class"], "canonical")
        backups = list(
            (self.home / "history-recovery-backups").glob("*/thread_history_1.sqlite")
        )
        self.assertEqual(len(backups), 1)
        self.assertEqual(recover.recover(backups[0])["class"], "mixed")
        self.assertEqual(self.install().returncode, 0)
        self.assertEqual(
            len(list((self.home / "history-recovery-backups").iterdir())), 1
        )

    def test_downloaded_upgrade_keeps_payload_after_downloader_cleanup(self):
        import io
        import json

        sys.path.insert(0, str(ROOT / "scripts/install"))
        import test_install_codex_plus_plus_latest as latest

        self.affected()
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as archive:
            for path in self.package.rglob("*"):
                if path.is_file():
                    archive.write(path, path.relative_to(self.package))
        with latest.ReleaseFixture(self.root / "download-fixture") as release:
            release.assets[release.archive_name] = payload.getvalue()
            release.assets[release.archive_name + ".sha256"] = latest.checksum(
                release.archive_name, payload.getvalue()
            )
            result = latest.run_installer(self.root, release, self.env)
            self.assertEqual(result.returncode, 75, result.stdout + result.stderr)
        pending = json.loads(self.pending.read_text())
        shutil.rmtree(self.package)
        self.assertTrue(Path(pending["Installer"]).is_file())
        result = self.install(self.shim_command())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.pending.exists())

    def test_refusals_preserve_history_and_install_then_compatible_history_skips_namespace_gate(
        self,
    ):
        import recover

        second = self.command.copy()
        second[second.index("-ShimDir") + 1] = str(self.root / "other-shim")
        self.assertEqual(self.install(second).returncode, 0)
        self.affected()
        before = recover.recover(self.fixture.path)
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("single managed", result.stderr)
        missing = self.command.copy()
        missing[missing.index("-PythonExe") + 1] = str(self.root / "missing-python.exe")
        self.assertNotEqual(self.install(missing).returncode, 0)
        self.assertEqual(self.pointer.read_bytes(), self.original)
        self.assertEqual(recover.recover(self.fixture.path), before)
        self.assertFalse(self.pending.exists())
        # Known compatible history does not require draining or namespace exclusion.
        self.fixture.apply()
        self.assertEqual(self.install().returncode, 0)
        with closing(recover.connect(self.fixture.path, "rw")) as db:
            db.execute("UPDATE _sqlx_migrations SET checksum=x'00' WHERE version=8")
        selected = self.pointer.read_bytes()
        self.assertNotEqual(self.install().returncode, 0)
        self.assertEqual(self.pointer.read_bytes(), selected)
        self.assertFalse(self.pending.exists())

    def test_committed_repair_resumes_activation_after_interruption(self):
        import json

        self.affected()
        self.assertEqual(self.install().returncode, 75)
        pending = json.loads(self.pending.read_text())
        helper = Path(pending["Installer"]).with_name("windows_upgrade.ps1")
        source = helper.read_text()
        helper.write_text(
            source.replace(
                "        & $Activate", "        throw 'synthetic interruption'"
            )
        )
        interrupted = self.install(self.shim_command())
        self.assertNotEqual(interrupted.returncode, 0)
        self.assertTrue(self.pending.exists())
        self.assertEqual(self.pointer.read_bytes(), self.original)
        helper.write_text(source)
        resumed = self.install(self.shim_command())
        self.assertEqual(resumed.returncode, 0, resumed.stdout + resumed.stderr)
        self.assertFalse(self.pending.exists())
        self.assertEqual(
            len(list((self.home / "history-recovery-backups").iterdir())), 1
        )

    def test_native_process_identity_and_recovery_lock_survive_process_exit(self):
        import windows_writers
        import recover

        process = self.launch([str(self.binary), "hold"])
        self.assertEqual(process.stdout.readline().strip(), "ready")
        native_identity = windows_writers.identity
        with patch.object(
            windows_writers,
            "identity",
            side_effect=lambda pid: (
                native_identity(pid) if pid == process.pid else None
            ),
        ):
            self.assertIn(
                native_identity(process.pid), windows_writers.codex_processes()
            )
        with patch.object(windows_writers, "identity", side_effect=PermissionError()):
            with self.assertRaises(PermissionError):
                windows_writers.codex_processes()
        self.release.touch()
        process.communicate(timeout=10)
        lock = self.root / "crash-lock"
        child = self.launch(
            [
                sys.executable,
                "-u",
                "-c",
                "import sys; sys.path.insert(0,sys.argv[1]); import recover; from pathlib import Path; ctx=recover.invocation_lock(Path(sys.argv[2])); ctx.__enter__(); print('ready',flush=True); input()",
                str(Path(__file__).parent),
                str(lock),
            ]
        )
        self.assertEqual(child.stdout.readline().strip(), "ready")
        with self.assertRaises(recover.Refused):
            with recover.invocation_lock(lock):
                self.fail("competing recovery entered")
        child.kill()  # Deliberate crash of our synthetic lock owner, never a client.
        child.wait(timeout=10)
        with recover.invocation_lock(lock):
            pass


if __name__ == "__main__":
    unittest.main()
