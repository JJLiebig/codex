#!/usr/bin/env python3
"""Offline, opt-in repair of the issue #260 history migration collision."""

import argparse
from contextlib import closing, contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import struct
import sys
import time


MANIFEST_SHA256 = "a5bd50a93666810cd9ec40c72d220f1fef105335f3adbf2448e0a54c569d1dee"
TARGET_PROFILE = "upstream-alpha6-windows"
UNIX_TARGET_PROFILE = "fork-0.166.0-unix"
TARGET_PROFILES = (TARGET_PROFILE, UNIX_TARGET_PROFILE)
LEDGER_SQL = """CREATE TABLE _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);"""


class Refused(Exception):
    pass


def manifest(target_profile=TARGET_PROFILE):
    # Source bytes are pinned LF; the verified binary profile chooses the target hash.
    raw = Path(__file__).with_name("manifest.json").read_bytes()
    if hashlib.sha256(raw).hexdigest() != MANIFEST_SHA256:
        raise Refused("pinned manifest bytes changed")
    result = json.loads(raw)
    for entry in result["canonical"] + result["historical"]:
        sql = entry["sql"].encode()
        blob = b"blob " + str(len(sql)).encode() + b"\0" + sql
        if (
            b"\r" in sql
            or sql.startswith(b"\xef\xbb\xbf")
            or hashlib.sha1(blob).hexdigest() != entry["blob"]
            or hashlib.sha384(sql).hexdigest() != entry["lf"]
            or hashlib.sha384(sql.replace(b"\n", b"\r\n")).hexdigest() != entry["crlf"]
        ):
            raise Refused("invalid pinned migration bytes")
    if target_profile == UNIX_TARGET_PROFILE:
        result["target"] = {
            "profile": UNIX_TARGET_PROFILE,
            "checksum_variant": "lf",
            "executable_version": "0.166.0-fork.6",
            "executable_sha256": "bfe29321dc599d47a04c885404b62bcd7b2afe7d3a023bdb8677b59ae1189446",
        }
    elif target_profile != TARGET_PROFILE:
        raise Refused("unqualified target profile")
    return result


def description(entry):
    return Path(entry["path"]).stem.split("_", 1)[1].replace("_", " ")


def connect(path, mode):
    db = sqlite3.connect(
        path.as_uri() + "?mode=" + mode, uri=True, isolation_level=None, timeout=5
    )
    db.execute("PRAGMA trusted_schema=OFF")
    return db


def schema(db):
    # Ignore formatting outside SQL strings, preserving defaults/predicates exactly.
    rows = db.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name")
    return [
        (
            kind,
            name,
            table,
            re.sub(
                r"'([^']|'')*'|\s+",
                lambda m: m[0] if m[0].startswith("'") else "",
                sql or "",
            ),
        )
        for kind, name, table, sql in rows
    ]


def expected_schema(entries):
    with closing(sqlite3.connect(":memory:")) as db:
        db.executescript(LEDGER_SQL)
        for entry in entries:
            db.executescript(entry["sql"])
        return schema(db)


def classify(db, target):
    rows = db.execute(
        "SELECT version,description,installed_on,success,checksum,execution_time "
        "FROM _sqlx_migrations ORDER BY version"
    ).fetchall()
    versions = [row[0] for row in rows]
    if not rows or versions != list(range(1, len(rows) + 1)) or len(rows) > 9:
        raise Refused("unknown, missing, or future migration versions")
    canonical, historical = target["canonical"], target["historical"]
    entries = canonical[: len(rows)]
    state = "healthy" if len(rows) < 8 else "canonical"
    if len(rows) >= 8 and rows[7][4] in [
        bytes.fromhex(historical[0][k]) for k in ("lf", "crlf")
    ]:
        state = "attribution-only" if len(rows) == 8 else "mixed"
        entries = canonical[:7] + historical[: len(rows) - 7]
    elif len(rows) == 9:
        raise Refused("unknown migration 9")
    for row, entry in zip(rows, entries):
        if (
            row[1] != description(entry)
            or not isinstance(row[2], str)
            or row[3] != 1
            or not isinstance(row[4], bytes)
            or row[4].hex() not in (entry["lf"], entry["crlf"])
            or not isinstance(row[5], int)
            or row[5] < -1
        ):
            raise Refused("unknown or unsuccessful migration ledger entry")
    actual = schema(db)
    expected = expected_schema(entries)
    # The sole tolerated extra column is the preserved, known nullable attr column.
    if state == "canonical" and actual != expected:
        expected = expected_schema(canonical[:7] + historical[:1] + canonical[7:])
    if actual != expected:
        raise Refused("schema does not match the known migration chain")
    normalize = [
        row[0]
        for row, entry in zip(rows, entries)
        if row[4].hex() != entry[target["target"]["checksum_variant"]]
    ]
    return state, normalize


def fingerprint(db, *, future_root=False):
    """Stable typed, length-framed values including rowids; no conversation output."""
    result = {}
    tables = db.execute(
        "SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name"
    ).fetchall()
    for (table,) in tables:
        columns = sorted(row[1] for row in db.execute(f'PRAGMA table_info("{table}")'))
        if future_root and table == "thread_turns":
            columns = sorted(columns + ["root_turn_id"])
        select = [
            "NULL"
            if future_root and table == "thread_turns" and col == "root_turn_id"
            else f'"{col}"'
            for col in columns
        ]
        digest, count = hashlib.sha256(), 0
        for row in db.execute(
            f'SELECT rowid,{",".join(select)} FROM "{table}" ORDER BY rowid'
        ):
            count += 1
            for value in row:
                if value is None:
                    tag, data = b"n", b""
                elif isinstance(value, int):
                    tag, data = b"i", str(value).encode()
                elif isinstance(value, float):
                    tag, data = b"f", struct.pack(">d", value)
                elif isinstance(value, str):
                    tag, data = b"s", value.encode()
                else:
                    tag, data = b"b", value
                digest.update(tag + struct.pack(">Q", len(data)) + data)
        result[table] = {
            "rows": count,
            "columns": columns,
            "sha256": digest.hexdigest(),
        }
    return result


def inspect(db, target):
    state, normalize = classify(db, target)
    if db.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
        raise Refused("database integrity check failed")
    return {
        "class": state,
        "normalize": normalize,
        "schema": schema(db),
        "content": fingerprint(db),
        "journal_mode": db.execute("PRAGMA journal_mode").fetchone()[0],
    }


def save_json(path, value):
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def mutate(db, state, target):
    root = target["canonical"][7]
    if state == "attribution-only":
        started = time.monotonic_ns()
        db.execute(root["sql"])
        db.execute(
            "UPDATE _sqlx_migrations SET description=?, checksum=?, "
            "installed_on=CURRENT_TIMESTAMP, execution_time=? WHERE version=8",
            (
                description(root),
                bytes.fromhex(root[target["target"]["checksum_variant"]]),
                time.monotonic_ns() - started,
            ),
        )
    elif state == "mixed":
        db.execute("DELETE FROM _sqlx_migrations WHERE version=8")
        db.execute("UPDATE _sqlx_migrations SET version=8 WHERE version=9")
    for version, entry in enumerate(target["canonical"], 1):
        db.execute(
            "UPDATE _sqlx_migrations SET checksum=? WHERE version=?",
            (bytes.fromhex(entry[target["target"]["checksum_variant"]]), version),
        )


@contextmanager
def invocation_lock(path):
    if os.name == "nt":
        import msvcrt

        try:
            stream = path.open("a+b")
        except OSError:
            raise Refused(
                "another recovery invocation or stale recovery lock exists"
            ) from None
        with stream:
            stream.seek(0)
            try:
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError:
                raise Refused(
                    "another recovery invocation or stale recovery lock exists"
                ) from None
            yield
    else:
        try:
            path.mkdir()
        except FileExistsError:
            raise Refused(
                "another recovery invocation or stale recovery lock exists"
            ) from None
        try:
            yield
        finally:
            path.rmdir()


def recover(
    path,
    *,
    apply=False,
    expected=None,
    stopped=False,
    backup_dir=None,
    target_profile=None,
):
    path = path.resolve(strict=True)
    if path.name != "thread_history_1.sqlite" or not path.is_file():
        raise Refused("expected the explicit thread_history_1.sqlite file")
    target = manifest(target_profile or TARGET_PROFILE)
    if not apply:
        with closing(connect(path, "ro")) as reader:
            reader.execute("BEGIN")
            return inspect(reader, target)
    if (
        target_profile not in TARGET_PROFILES
        or not stopped
        or expected is None
        or backup_dir is None
    ):
        raise Refused(
            "apply requires verified target profile, writer shutdown/upgrade attestation, matching class, and new backup directory"
        )
    lock = path.with_name(path.name + ".recovery-lock")
    with invocation_lock(lock):
        with closing(connect(path, "rw")) as db:
            db.execute("BEGIN IMMEDIATE")
            try:
                # Full scans are slow on large histories; inspect once, under the lock.
                before = inspect(db, target)
                if before["class"] != expected:
                    raise Refused(
                        f"history class is {before['class']}, expected {expected}"
                    )
                if (
                    before["class"] in ("healthy", "canonical")
                    and not before["normalize"]
                ):
                    db.rollback()
                    return {**before, "result": "noop"}
                backup_dir = backup_dir.resolve()
                backup_dir.mkdir(
                    mode=0o700
                )  # Private; never overwrite a recovery artifact.
                backup = backup_dir / path.name
                # Use a second reader while the reserved writer lock prevents commits.
                # Backing up the active write connection itself would deadlock.
                with (
                    closing(connect(path, "ro")) as source,
                    closing(sqlite3.connect(backup)) as dest,
                ):
                    source.backup(dest)
                    if inspect(dest, target) != before:
                        raise Refused("backup verification failed")
                with backup.open("r+b") as stream:
                    os.fsync(stream.fileno())
                save_json(
                    backup_dir / "manifest.json",
                    {
                        "target_manifest_sha256": MANIFEST_SHA256,
                        "target": target,
                        "before": before,
                        "backup": backup.name,
                    },
                )
                # POSIX file fsync does not persist the newly created names.
                # Windows exposes file flushing through os.fsync, not directory fds.
                if os.name != "nt":
                    for directory in (backup_dir, backup_dir.parent):
                        fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
                        try:
                            os.fsync(fd)
                        finally:
                            os.close(fd)
                # The lock kept content unchanged since inspection.
                preserved = (
                    fingerprint(db, future_root=True)
                    if before["class"] == "attribution-only"
                    else dict(before["content"])
                )
                preserved.pop("_sqlx_migrations")
                mutate(db, before["class"], target)
                after = inspect(db, target)
                content = dict(after["content"])
                content.pop("_sqlx_migrations")
                if (
                    content != preserved
                    or after["journal_mode"] != before["journal_mode"]
                ):
                    raise Refused("content or journal mode changed unexpectedly")
                save_json(backup_dir / "verified-before-commit.json", after)
                db.commit()
                return {**after, "result": "applied", "backup": str(backup_dir)}
            except BaseException:
                db.rollback()
                raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--target-profile", choices=TARGET_PROFILES)
    parser.add_argument(
        "--expect-class", choices=["healthy", "canonical", "attribution-only", "mixed"]
    )
    parser.add_argument("--all-writers-stopped-and-upgraded", action="store_true")
    parser.add_argument("--backup-dir", type=Path)
    args = parser.parse_args()
    try:
        result = recover(
            args.database,
            apply=args.apply,
            expected=args.expect_class,
            stopped=args.all_writers_stopped_and_upgraded,
            backup_dir=args.backup_dir,
            target_profile=args.target_profile,
        )
    except (Refused, OSError, sqlite3.Error) as error:
        # SQLite diagnostics can contain user values; do not print arbitrary errors.
        reason = str(error) if isinstance(error, Refused) else type(error).__name__
        print(f"status: refused\nreason: {reason}", file=sys.stderr)
        return 1
    print(f"status: {result.get('result', 'inspected')}\nclass: {result['class']}")
    print(
        f"normalize[{len(result['normalize'])}]: "
        + ",".join(map(str, result["normalize"]))
    )
    print(f"target_profile: {args.target_profile or TARGET_PROFILE}")
    print(f"tables[{len(result['content'])}]{{name,rows,sha256}}:")
    for name, value in result["content"].items():
        print(f"  {name},{value['rows']},{value['sha256']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
