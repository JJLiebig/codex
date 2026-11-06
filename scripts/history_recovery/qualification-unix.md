# Unix qualification (2026-11-06)

Profile: `fork-0.166.0-unix`, pinned canonical migration bytes with LF SHA384.
Qualified application: installed Linux x64 Codex++ `0.166.0-fork.6`, SHA256
`bfe29321dc599d47a04c885404b62bcd7b2afe7d3a023bdb8677b59ae1189446`.
This is executable qualification on Linux; macOS was not separately executed.
The Python operator command uses the same LF profile on Unix, with no OS guessing.

## Application proof

All application work used scratch CODEX_HOME, sqlite_home, HOME, profile and temp
paths under `/tmp/unix-history-qualification.jjx152`. Credentials were not imported;
the scratch provider and ChatGPT URLs pointed to closed loopback port 9. No inference
was requested. Existing sessions were not interrupted. Live history was copied while
no WAL existed (checked before and after); the copy passed full integrity checks.
This is one history database, not a coordinated snapshot of the whole Codex home.

The real copy was `mixed`: known LF 1–7, attribution-8, root-9. Before repair, the
installed application returned the exact migration-8 mismatch from history resume,
turns and items requests. The tool verified a SQLite API backup, promoted root-9 to
8, and produced canonical LF 1–8 with integrity `ok`. The typed fingerprints below
were unchanged across repair, including columns, rowids and all values:

| Table | Rows | SHA256 |
| --- | ---: | --- |
| thread_turns | 48 | 993fb8657b8ee8353fd26e49fadbf3a2eab865b6dca5e107a8b6c10c04cf4219 |
| thread_items | 850 | 8bcba98fbcf94fb96dd8a84135388891a0cab5423e313d22a03c304415005e54 |
| thread_history_projection_state | 31 | 311bc370c86c7c54876dbb87348d75cbc5216cb7b837585db03bfec627c9083d |
| thread_realtime_items | 0 | e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 |

Two application opens after repair each passed initialize, thread/resume,
thread/read, thread/turns/list and thread/items/list. A new synthetic paginated
rollout projected `fixture-turn`, `fixture-root`, and a user item. Reopen retained
that projection and all eight LF checksums. Neither post-repair stderr contained
the migration mismatch. This checks real application projection, beyond Python
migration emulation. Synthetic tests additionally cover attribution-only, healthy
prefixes, canonical histories, WAL backup, refusal and rollback behavior.

## Writer fence verdict

Retain the full offline writer/upgrade attestation. A failing open is not a lasting
fence. On a second copy, the same app-server process failed history opens, then
successfully resumed and projected after repair without a process restart.
A retry issued during the transaction saw the old ledger and failed; a retry just
after commit succeeded. `get_or_try_init` in the local thread store retries failed
initialization; the state SQLite opener closes its pool on migration failure.

At the failed-open checkpoint, `fuser` found no holder, neither WAL nor SHM existed,
and a zero-timeout `BEGIN IMMEDIATE` succeeded. Thus the failed application opens
in that experiment left no enduring handle or write lock. Earlier post-run sidecars
were from the qualification harness's own SQLite inspection, not proof of a leaked
application pool. Sidecar absence and `fuser` are only point-in-time observations.

The qualified fork.6 process can recover after commit, but this does not establish
that every current or newly launched client is that executable or that an old
already-initialized pool cannot write. A transaction prevents concurrent commits
while held; it cannot retire an idle old pool or prevent a later old launch. An
exclusive lock plus atomic replacement also cannot fence an already-open inode and
its sidecars; replacement would introduce a second database identity. The existing
in-place transactional repair, verified backup and full operator fence are the
smaller general procedure. No live-repair bypass or atomic swap was added.

## Evidence retained by the operator

Remote scratch: `/tmp/unix-history-qualification.jjx152/case3` and `retry`.
Local metadata/scripts: `C:\Code\.artifacts\codex\unix-history-recovery`:
`qualification.json`, `retry-evidence.json`, `qualify_unix.py`,
`qualify_unix_retry.py`, `qualify_base.py`. The JSON contains metadata and typed
hashes; full RPC replies and private database copies remain in remote scratch.
These are qualification artifacts, not application runtime dependencies.
