# Offline history recovery for issue #260

This Python 3.10+ standard-library tool repairs only the known fork attribution-8 /
upstream root-9 collision in an explicitly selected `thread_history_1.sqlite`.
The standalone command has no startup hook or automatic process stop. Managed Windows
upgrades can invoke the same repair as guarded installation maintenance described below.
The source-removal PR must restore canonical migrations before this recovery is used. The equality gate intentionally fails until that PR lands.

## Pinned provenance and accepted states

`manifest.json` was extracted with `git ls-tree -r <commit> --
codex-rs/state/thread_history_migrations` and `git cat-file blob <blob-id>`.
Canonical SQL comes from the actual bundled upstream alpha6 commit
`4f093f2748667f3b0e6a07e3d4055121cbd3ce64`; all eight blobs equal merged upstream
`d373d649673ca8a5bca71f54be0ae47dbd79b466`. Historical attribution-8/root-9 comes
from `ff847af67b30b01447007d5e6d986445314fd921`. SQLx uses SHA384 of exact SQL
bytes. The manifest records exact LF/CRLF variants; no
BOM, lone CR, platform guess, working-checkout SQL, or unrelated database is used.

| Class | Accepted schema/ledger | Apply behavior |
| --- | --- | --- |
| `healthy` | Contiguous known 1 through 1–7, no attribution/root | Only normalize recognized checksums to the selected qualified profile; leave migrations pending |
| `canonical` | Known canonical 1–8, nullable TEXT root, optionally retained known attribution | Noop except known checksum normalization |
| `attribution-only` | Known 1–7 + attribution-8, no root | Execute exact canonical root SQL; replace 8 metadata with this root installation |
| `mixed` | Known attribution-8/root-9, both columns | Retire attribution ledger row; promote root-9 to 8, preserving its installation time and duration |

Unknown checksums, future/gapped/dirty ledgers, unexpected schema objects or column
definitions are refused. Attribution values, including nonempty values, remain
in place. Existing root IDs, rowids, item JSON and projection checkpoints remain
unchanged. Only attribution-only adds NULL root values. Repeating repair is a noop.

The Windows apply profile is `upstream-alpha6-windows`. Actual bundled executable
`0.166.0-alpha.6`, SHA256
`925c7d0665ffa49b0dcf748e034a805a04da984164ac92301432073b47789b99`,
created all eight known CRLF ledger hashes in an isolated synthetic home. Its public
resume/read/turns/items calls and reopen projected the synthetic root successfully.
The same executable rejected canonical LF at migration 1. Source LF is therefore
not a valid apply target for this Windows app.

The Unix apply profile is `fork-0.166.0-unix` (LF). The released Linux x64
`0.166.0-fork.6` executable, SHA256
`bfe29321dc599d47a04c885404b62bcd7b2afe7d3a023bdb8677b59ae1189446`,
opened a repaired copy of jjdev history and projected a synthetic turn and item,
then reopened successfully. See [Unix qualification](qualification-unix.md).
Profile selection is explicit; the host OS never chooses checksums.

## Unix manual command

Use Python 3.10+ from the repository or an extracted GitHub source release checkout.
After stopping and upgrading **all** writers and disabling relaunchers as described
below, run from the checkout root (replace both paths; backup directory must be new):

```sh
python3 scripts/history_recovery/recover.py /srv/jjdev/.codex/thread_history_1.sqlite --apply --target-profile fork-0.166.0-unix --expect-class mixed --all-writers-stopped-and-upgraded --backup-dir /srv/jjdev/.codex/history-recovery-unix-backup
```

To inspect first, omit `--apply` and retain `--target-profile fork-0.166.0-unix`.
This is manual maintenance; Unix installers/npm postinstall do not own all external
writers or relaunchers, so cannot supply the required fence.

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
   `python scripts/history_recovery/recover.py <path>/thread_history_1.sqlite --apply --target-profile upstream-alpha6-windows --expect-class mixed --all-writers-stopped-and-upgraded --backup-dir <new-private-directory>`.
   Keep writers stopped until verification and application-open qualification finish.
5. Inspect again with the same profile; expect its canonical ledger (or unchanged healthy prefix), preserved
   counts/hashes and journal mode. Retain the backup and manifests securely: the
   backup contains private conversation data even though manifests contain only
   metadata and hashes. Resume only qualified upstream and fixed-fork clients.

Apply obtains a separate invocation lock, inspects only under `BEGIN IMMEDIATE`, and
uses a second read connection's SQLite backup API while writes are excluded. It
verifies backup integrity, schema, ledger and typed content, then flushes the backup
and metadata manifest before mutation. All changes and post-validation share one
transaction. A failed run leaves the original logically unchanged and any completed
backup intact. `verified-before-commit.json` is verification evidence, **not** proof
that commit completed; inspect the original after interruption. A crash can leave
`.recovery-lock`; remove it manually only after establishing no recovery is running.
On Windows, new invocations use a process-held file lock released on exit/crash;
an old directory lock still refuses recovery and needs the procedure above.
No database replacement, rebuild, drop, rename, JSONL rewrite, WAL/SHM deletion or
journal-mode change is performed. Do not restore a backup over a running database.
Old fork fresh opens fail on the repaired checksum; already-open old pools remain
dangerous and are the reason shutdown is a prerequisite, not a tool guarantee.

The existing repo-checks gate fetches full `openai/codex` ancestry, resolves the actual
common ancestor and compares path/blob inventories for all six shared chains. It
fails on unresolved/shallow history or any difference. It flags known migrator/open
seams for explicit review; it cannot detect arbitrary SQL writes in other code.

## Managed Windows upgrades

The Windows managed installer, downloaded update and local build `--install` share
one maintenance path. The new binary resolves the global configuration's history
location without opening history or auth. Project and per-invocation overrides are
outside this installation maintenance scope. First installs with missing history
are unchanged. Existing managed installs with missing history or a healthy prefix
stage pending completion too: an old lazy writer can create or advance history
after preflight, so it is checked again after managed writers drain. Both checks
read only the ledger and schema; full integrity and content checks run once, under
the repair lock, and only when a repair is needed. Existing
history requires Python 3.10+; local builds pass their interpreter, and the installer
also accepts an absolute `-PythonExe`. No interpreter is downloaded.

Recognized affected histories stage a pending upgrade and return status 75. Close
existing Codex sessions and use the printed shim path (or launch Codex again): that launch completes maintenance
before acquiring a runtime lease or starting Codex. It waits/retries for all retained
generation leases, known Codex processes and users of the exact database/WAL/SHM.
It never stops clients. Obsolete owned generation junctions are retired under the
gates before repair; their packages remain, but preloaded old launchers cannot
restart those generations afterward. Discovery failures remain pending. An upgrade running inside
Codex therefore returns instead of waiting on its own session's lease. New CLIs
report pending; an older CLI may additionally describe status 75 as an update failure.

Maintenance supports the selected managed Windows shim only. Affected histories
with multiple managed namespaces in the same CODEX_HOME refuse maintenance.
Keep raw executables, npm, desktop and supervisors stopped throughout maintenance;
the managed launch fence cannot prevent those external launchers from starting.
Already-compatible histories skip maintenance and its namespace restriction.
Unknown/future histories refuse before changing the selected installation or data.

The verified backup is retained under `history-recovery-backups` beside history.
If interrupted, the pending record remains until activation and gate cleanup both
finish; the next managed launch resumes without repeating a completed repair.
Missing history at completion needs no interpreter; newly created history requires
Python before activation. Leave the pending record and staged package in place.
This is a one-upgrade operation,
not a startup migrator, service, scheduled task or background worker. Native account
stores and the six shared migration chains are unchanged.
