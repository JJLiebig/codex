"""Issue #260 maintenance, called only under the managed installer's launch gates."""

import argparse
from contextlib import closing
from pathlib import Path
import sqlite3
import sys
import time
import uuid

import recover
import windows_writers


def classify(path):
    # Ledger and schema only; apply checks integrity and content under its lock.
    with closing(recover.connect(path.resolve(strict=True), "ro")) as reader:
        reader.execute("BEGIN")
        return recover.classify(reader, recover.manifest())


def complete(path):
    observed = set()
    announced = False
    while True:
        observed.update(windows_writers.codex_processes())
        observed.update(windows_writers.database_users(path))
        observed = {
            item for item in observed if windows_writers.identity(item[0]) == item
        }
        if not observed:
            break
        if not announced:
            print(
                "Upgrade pending. Close Codex sessions; this launch will retry.",
                flush=True,
            )
            announced = True
        time.sleep(1)
    if not path.exists():
        return
    state, normalize = classify(path)
    if normalize or state in ("mixed", "attribution-only"):
        backups = path.parent / "history-recovery-backups"
        backups.mkdir(exist_ok=True)
        result = recover.recover(
            path,
            apply=True,
            expected=state,
            stopped=not observed,
            backup_dir=backups / str(uuid.uuid4()),
            target_profile=recover.TARGET_PROFILE,
        )
        print(f"History repaired. Backup: {result['backup']}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("inspect", "complete"))
    parser.add_argument("database", type=Path)
    args = parser.parse_args()
    try:
        if args.operation == "inspect":
            # Old managed writers can advance a healthy prefix into the collision.
            state, normalize = classify(args.database)
            return 75 if normalize or state != "canonical" else 0
        complete(args.database)
        return 0
    except (recover.Refused, OSError, sqlite3.Error) as error:
        reason = (
            str(error)
            if isinstance(error, recover.Refused)
            else "Cannot safely inspect or repair history; upgrade remains pending."
        )
        print(reason, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
