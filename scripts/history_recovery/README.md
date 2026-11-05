# Offline history recovery for issue #260

This Python 3.10+ standard-library tool repairs only the known fork attribution-8 /
upstream root-9 collision in an explicitly selected `thread_history_1.sqlite`.
There is no startup hook, home discovery, automatic process stop, or runtime repair.
The source-removal PR must restore canonical migrations and LF history checksums
before this recovery is used. The equality gate intentionally fails until that PR lands.

## Pinned provenance and accepted states

`manifest.json` was extracted with `git ls-tree -r <commit> --
codex-rs/state/thread_history_migrations` and `git cat-file blob <blob-id>`.
Canonical SQL comes from the actual bundled upstream alpha6 commit
`4f093f2748667f3b0e6a07e3d4055121cbd3ce64`; all eight blobs equal merged upstream
`d373d649673ca8a5bca71f54be0ae47dbd79b466`. Historical attribution-8/root-9 comes
from `ff847af67b30b01447007d5e6d986445314fd921`. SQLx uses SHA384 of exact SQL
bytes. The manifest records LF targets and historical LF/CRLF alternatives; no
BOM, lone CR, platform guess, working-checkout SQL, or unrelated database is used.

| Class | Accepted schema/ledger | Apply behavior |
| --- | --- | --- |
| `healthy` | Contiguous known 1 through 1–7, no attribution/root | Only normalize recognized CRLF checksums; leave migrations pending |
| `canonical` | Known canonical 1–8, nullable TEXT root, optionally retained known attribution | Noop except known checksum normalization |
| `attribution-only` | Known 1–7 + attribution-8, no root | Execute exact canonical root SQL; replace 8 metadata with this root installation |
| `mixed` | Known attribution-8/root-9, both columns | Retire attribution ledger row; promote root-9 to 8, preserving its installation time and duration |

Unknown checksums, future/gapped/dirty ledgers, unexpected schema objects or column
definitions are refused. Attribution values, including nonempty values, remain
in place. Existing root IDs, rowids, item JSON and projection checkpoints remain
unchanged. Only attribution-only adds NULL root values. Repeating repair is a noop.

## Operator procedure (separate offline authorization required)

1. Rehearse on **synthetic fixtures only**. Run
   `python -m unittest discover -s scripts/history_recovery -p 'test_*.py'`.
   Python migrator emulation is not proof of application compatibility. Before live
   use, qualify scratch homes with both the actual upstream bundled executable and
   the fixed fork executable. Review confinement first: explicit scratch `CODEX_HOME`,
   explicit scratch `sqlite_home`, no imported credentials, no inference, and no
   access to shared history. Verify startup opens the repaired history and retains
   its ledger and content. Record executable hashes, commands and resulting hashes.
2. From an independent offline operator shell, stop **all** upstream/fork CLI,
   desktop, app-server, daemon and supervisor/relauncher writers. Upgrade or disable
   old fork writers before restarting anything. Running agents cannot apply this
   repair to their own shared home. A SQLite write lock does not fence idle old pools.
3. Inspect the explicit authorized path:
   `python scripts/history_recovery/recover.py <path>/thread_history_1.sqlite`.
   Check the class and content hashes. No conversation content is printed.
4. Apply with the observed class, the shutdown/upgrade attestation and a **new**
   backup directory on private storage with sufficient space:
   `python scripts/history_recovery/recover.py <path>/thread_history_1.sqlite --apply --expect-class mixed --all-writers-stopped-and-upgraded --backup-dir <new-private-directory>`.
   Keep writers stopped until verification and application-open qualification finish.
5. Inspect again; expect canonical LF ledger (or unchanged healthy prefix), preserved
   counts/hashes and journal mode. Retain the backup and manifests securely: the
   backup contains private conversation data even though manifests contain only
   metadata and hashes. Resume only qualified upstream and fixed-fork clients.

Apply obtains a separate invocation lock, revalidates under `BEGIN IMMEDIATE`, and
uses a second read connection's SQLite backup API while writes are excluded. It
verifies backup integrity, schema, ledger and typed content, then flushes the backup
and metadata manifest before mutation. All changes and post-validation share one
transaction. A failed run leaves the original logically unchanged and any completed
backup intact. `verified-before-commit.json` is verification evidence, **not** proof
that commit completed; inspect the original after interruption. A crash can leave
`.recovery-lock`; remove it manually only after establishing no recovery is running.
No database replacement, rebuild, drop, rename, JSONL rewrite, WAL/SHM deletion or
journal-mode change is performed. Do not restore a backup over a running database.
Old fork fresh opens fail on the repaired checksum; already-open old pools remain
dangerous and are the reason shutdown is a prerequisite, not a tool guarantee.

The existing repo-checks gate fetches full `openai/codex` ancestry, resolves the actual
common ancestor and compares path/blob inventories for all six shared chains. It
fails on unresolved/shallow history or any difference. It flags known migrator/open
seams for explicit review; it cannot detect arbitrary SQL writes in other code.
