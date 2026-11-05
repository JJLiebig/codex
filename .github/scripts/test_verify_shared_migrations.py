from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from verify_shared_migrations import CHAINS


class MigrationGateTests(unittest.TestCase):
    def test_merged_ancestry_and_path_or_byte_drift(self):
        script = Path(__file__).with_name("verify_shared_migrations.py").resolve()
        with tempfile.TemporaryDirectory() as folder:

            def git(*args):
                return (
                    subprocess.check_output(
                        ["git", "-C", folder, *args], stderr=subprocess.PIPE
                    )
                    .decode()
                    .strip()
                )

            def check(expected):
                result = subprocess.run(
                    [sys.executable, str(script)],
                    cwd=folder,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(
                    result.returncode, expected, result.stdout + result.stderr
                )

            git("init")
            git("config", "user.name", "Synthetic Test")
            git("config", "user.email", "synthetic@example.invalid")
            for chain in CHAINS:
                path = Path(folder, chain, "0001.sql")
                path.parent.mkdir(parents=True)
                path.write_bytes(b"SELECT 1;\n")
            git("add", ".")
            git("commit", "-m", "synthetic shared ancestor")
            base = git("rev-parse", "HEAD")
            path.write_bytes(b"SELECT 2;\n")
            git("commit", "-am", "unmerged upstream migration")
            git("update-ref", "refs/remotes/shared-upstream/main", "HEAD")
            git("checkout", "--detach", base)
            check(0)
            path.write_bytes(b"SELECT 1;\r\n")
            git("-c", "core.autocrlf=false", "add", ".")
            git("commit", "-m", "changed exact bytes")
            check(1)
            git("checkout", "--detach", base)
            path.rename(path.with_name("0002.sql"))
            git("add", ".")
            git("commit", "-m", "renumbered")
            check(1)
            git("update-ref", "-d", "refs/remotes/shared-upstream/main")
            check(1)


if __name__ == "__main__":
    unittest.main()
