"""Ship issue-specific maintenance with the verified Windows package."""

from pathlib import Path
import shutil
import hashlib
import json


def add_recovery(package):
    source = Path(__file__).resolve().parents[2] / "history_recovery"
    canonical = json.loads((source / "manifest.json").read_bytes())["canonical"]
    migrations = source.parents[1] / "codex-rs/state/thread_history_migrations"
    if {path.name for path in migrations.glob("*.sql")} != {
        Path(entry["path"]).name for entry in canonical
    }:
        raise RuntimeError(
            "Windows history recovery profile needs qualification for this migration chain"
        )
    for entry in canonical:
        data = (
            (migrations / Path(entry["path"]).name).read_bytes().replace(b"\r\n", b"\n")
        )
        if hashlib.sha384(data).hexdigest() != entry["lf"]:
            raise RuntimeError(
                "Windows history recovery profile does not match these migrations"
            )
    destination = package / "codex-resources" / "history-upgrade"
    destination.mkdir(parents=True)
    for name in (
        "recover.py",
        "manifest.json",
        "windows_upgrade.py",
        "windows_writers.py",
        "windows_upgrade.ps1",
    ):
        shutil.copyfile(source / name, destination / name)
