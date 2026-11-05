"""Issue #260 maintenance, called only under the managed installer's launch gates."""

import argparse
from pathlib import Path
import sqlite3
import sys
import time
import uuid

import recover
import windows_writers


def needed(path):
    if not path.exists():
        return False
    result = recover.recover(path)
    return bool(result["normalize"] or result["class"] in ("mixed", "attribution-only"))


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
    if needed(path):
        before = recover.recover(path)
        backups = path.parent / "history-recovery-backups"
        backups.mkdir(exist_ok=True)
        result = recover.recover(
            path,
            apply=True,
            expected=before["class"],
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
            return 75 if needed(args.database) else 0
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
