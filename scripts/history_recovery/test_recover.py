from contextlib import closing
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import recover as recovery


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "thread_history_1.sqlite"
        self.target = recovery.manifest()

    def fixture(self, state="mixed", newline="lf", journal="delete", version=7):
        db = sqlite3.connect(self.path, isolation_level=None)
        self.addCleanup(db.close)
        db.execute(f"PRAGMA journal_mode={journal}")
        db.execute("PRAGMA wal_autocheckpoint=0")
        db.executescript(recovery.LEDGER_SQL)
        entries = self.target["canonical"][: version if state == "healthy" else 7]
        if state == "canonical":
            entries += self.target["canonical"][7:]
        elif state in ("attribution-only", "mixed"):
            entries += self.target["historical"][
                : 1 if state == "attribution-only" else 2
            ]
        for number, entry in enumerate(entries, 1):
            db.executescript(entry["sql"])
            db.execute(
                "INSERT INTO _sqlx_migrations VALUES (?,?,?,1,?,?)",
                (
                    number,
                    recovery.description(entry),
                    f"2026-10-{number:02d} 01:02:03",
                    bytes.fromhex(entry[newline]),
                    number * 123,
                ),
            )
        db.execute(
            "INSERT INTO thread_turns(thread_id,turn_id,rollout_ordinal,status) "
            "VALUES ('synthetic-thread','synthetic-turn',1,'completed')"
        )
        db.execute(
            "INSERT INTO thread_items(thread_id,turn_id,item_id,rollout_ordinal,created_at_ms,item_json) "
            "VALUES ('synthetic-thread','synthetic-turn','item',2,123,'{\"type\":\"userMessage\",\"text\":\"synthetic Ω\"}')"
        )
        db.execute(
            "INSERT INTO thread_history_projection_state VALUES ('synthetic-thread',999,3)"
        )
        if state in ("attribution-only", "mixed"):
            db.execute(
                "UPDATE thread_turns SET inference_attribution_json='{"
                + '"synthetic":true'
                + "}'"
            )
        if state in ("canonical", "mixed"):
            db.execute("UPDATE thread_turns SET root_turn_id='synthetic-root'")
        if state != "healthy" or version >= 5:
            db.execute(
                "INSERT INTO thread_realtime_items VALUES ('synthetic-thread','realtime',3,123,'realtime_session_started','{}')"
            )
        return db

    def apply(self, state="mixed"):
        return recovery.recover(
            self.path,
            apply=True,
            expected=state,
            stopped=True,
            backup_dir=self.root / "backup",
            target_profile=recovery.TARGET_PROFILE,
        )

    def test_repair_and_backup_preserve_committed_data_in_both_journal_modes(self):
        for journal in ("wal", "delete"):
            with self.subTest(journal=journal):
                # Separate fixture scope; WAL writer stays open with committed frames.
                with tempfile.TemporaryDirectory() as folder:
                    self.root = Path(folder)
                    self.path = self.root / "thread_history_1.sqlite"
                    db = self.fixture(journal=journal, newline="crlf")
                    db.execute(
                        "UPDATE _sqlx_migrations SET execution_time=-1 WHERE version=9"
                    )
                    if journal == "wal":
                        self.assertGreater(
                            self.path.with_name(self.path.name + "-wal").stat().st_size,
                            0,
                        )
                    original = recovery.fingerprint(db)
                    metadata = db.execute(
                        "SELECT installed_on,execution_time FROM _sqlx_migrations WHERE version=9"
                    ).fetchone()
                    result = self.apply()
                    self.assertEqual(
                        (result["class"], result["result"], result["normalize"]),
                        ("canonical", "applied", []),
                    )
                    self.assertEqual(
                        db.execute(
                            "SELECT installed_on,execution_time FROM _sqlx_migrations WHERE version=8"
                        ).fetchone(),
                        metadata,
                    )
                    with closing(
                        sqlite3.connect(self.root / "backup" / self.path.name)
                    ) as backup:
                        self.assertEqual(recovery.fingerprint(backup), original)
                        self.assertEqual(
                            backup.execute("PRAGMA integrity_check").fetchall(),
                            [("ok",)],
                        )
                    for table in original.keys() - {"_sqlx_migrations"}:
                        self.assertEqual(result["content"][table], original[table])
                    self.assertEqual(
                        db.execute("PRAGMA journal_mode").fetchone(), (journal,)
                    )
                    self.assertEqual(self.apply("canonical")["result"], "noop")
                    db.close()

    def test_unix_target_repairs_all_states_and_preserves_typed_content(self):
        for state in ("mixed", "attribution-only", "canonical", "healthy"):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as folder:
                self.root = Path(folder)
                self.path = self.root / "thread_history_1.sqlite"
                db = self.fixture(state, newline="crlf")
                before = recovery.fingerprint(
                    db, future_root=state == "attribution-only"
                )
                result = recovery.recover(
                    self.path,
                    apply=True,
                    expected=state,
                    stopped=True,
                    backup_dir=self.root / "backup",
                    target_profile=recovery.UNIX_TARGET_PROFILE,
                )
                self.assertEqual(result["normalize"], [])
                self.assertEqual(
                    db.execute(
                        "SELECT checksum FROM _sqlx_migrations ORDER BY version"
                    ).fetchall(),
                    [
                        (bytes.fromhex(entry["lf"]),)
                        for entry in self.target["canonical"][
                            : 7 if state == "healthy" else 8
                        ]
                    ],
                )
                before.pop("_sqlx_migrations")
                after = recovery.fingerprint(db)
                after.pop("_sqlx_migrations")
                self.assertEqual(after, before)
                self.assertEqual(
                    recovery.recover(
                        self.path,
                        apply=True,
                        expected=result["class"],
                        stopped=True,
                        backup_dir=self.root / "second-backup",
                        target_profile=recovery.UNIX_TARGET_PROFILE,
                    )["result"],
                    "noop",
                )
                db.close()

    def test_attribution_only_adds_null_root_preserving_nonzero_attribution(self):
        db = self.fixture("attribution-only")
        before = recovery.fingerprint(db, future_root=True)
        self.apply("attribution-only")
        after = recovery.fingerprint(db)
        before.pop("_sqlx_migrations")
        after.pop("_sqlx_migrations")
        self.assertEqual(after, before)
        self.assertEqual(
            db.execute(
                "SELECT root_turn_id,inference_attribution_json FROM thread_turns"
            ).fetchall(),
            [(None, '{"synthetic":true}')],
        )
        self.assertEqual(self.apply("canonical")["result"], "noop")

    def test_healthy_prefixes_and_canonical_only_normalize_known_checksums(self):
        for state, version in [("healthy", 1), ("healthy", 7), ("canonical", 8)]:
            with (
                self.subTest(state=state, version=version),
                tempfile.TemporaryDirectory() as folder,
            ):
                self.root = Path(folder)
                self.path = self.root / "thread_history_1.sqlite"
                db = self.fixture(state, newline="lf", version=version)
                before_schema = recovery.schema(db)
                self.assertEqual(self.apply(state)["normalize"], [])
                self.assertEqual(recovery.schema(db), before_schema)
                self.assertEqual(self.apply(state)["result"], "noop")
                db.close()

    def test_inspect_and_refusals_leave_database_unchanged(self):
        mutations = [
            "UPDATE _sqlx_migrations SET checksum=x'00' WHERE version=1",
            "UPDATE _sqlx_migrations SET success=0 WHERE version=8",
            "DELETE FROM _sqlx_migrations WHERE version=3",
            "UPDATE _sqlx_migrations SET version=10 WHERE version=9",
            "ALTER TABLE thread_turns ADD COLUMN unexpected TEXT",
            "ALTER TABLE thread_turns DROP COLUMN root_turn_id",
            "CREATE TRIGGER unexpected AFTER UPDATE ON _sqlx_migrations BEGIN DELETE FROM thread_items; END",
            "DROP INDEX idx_thread_turns_page",
        ]
        for mutation in mutations:
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as folder,
            ):
                self.root = Path(folder)
                self.path = self.root / "thread_history_1.sqlite"
                db = self.fixture()
                recovery.recover(self.path)  # Default inspect never creates a backup.
                self.assertFalse((self.root / "backup").exists())
                db.execute(mutation)
                before = self.path.read_bytes()
                with self.assertRaises(recovery.Refused):
                    self.apply()
                self.assertEqual(self.path.read_bytes(), before)
                db.close()

    def test_rollback_after_mutation_keeps_verified_backup(self):
        db = self.fixture()
        before = recovery.fingerprint(db)
        real_mutate = recovery.mutate

        def fail(*args):
            # A second full invocation can inspect, but cannot enter this repair.
            with self.assertRaisesRegex(recovery.Refused, "recovery lock exists"):
                self.apply()
            real_mutate(*args)
            raise RuntimeError("synthetic failure before commit")

        with patch.object(recovery, "mutate", fail), self.assertRaises(RuntimeError):
            self.apply()
        self.assertEqual(recovery.fingerprint(db), before)
        self.assertEqual(recovery.classify(db, self.target)[0], "mixed")
        self.assertTrue((self.root / "backup" / "manifest.json").is_file())

    def test_backup_failure_competitor_and_missing_attestation_cannot_mutate(self):
        db = self.fixture()
        before = recovery.fingerprint(db)
        with self.assertRaises(recovery.Refused):
            recovery.recover(
                self.path, apply=True, expected="mixed", backup_dir=self.root / "backup"
            )
        with self.assertRaises(recovery.Refused):
            recovery.recover(
                self.path,
                apply=True,
                expected="mixed",
                stopped=True,
                backup_dir=self.root / "backup",
            )
        lock = self.path.with_name(self.path.name + ".recovery-lock")
        lock.mkdir()
        with self.assertRaises(recovery.Refused):
            self.apply()
        lock.rmdir()
        with patch.object(
            recovery, "save_json", side_effect=OSError("synthetic disk failure")
        ):
            with self.assertRaises(OSError):
                self.apply()
        self.assertEqual(recovery.fingerprint(db), before)

    def test_locked_inspection_rejects_unexpected_class(self):
        db = self.fixture()
        with self.assertRaisesRegex(recovery.Refused, "class is mixed"):
            self.apply("attribution-only")
        self.assertEqual(recovery.classify(db, self.target)[0], "mixed")
        self.assertFalse((self.root / "backup").exists())


if __name__ == "__main__":
    unittest.main()
