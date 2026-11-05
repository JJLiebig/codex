#!/usr/bin/env python3
"""Compare shared migration paths and Git blobs with actual merged upstream ancestry."""

import subprocess
import sys

CHAINS = [
    f"codex-rs/state/{name}"
    for name in (
        "migrations",
        "logs_migrations",
        "goals_migrations",
        "memory_migrations",
        "queue_migrations",
        "thread_history_migrations",
    )
]
SEAMS = [
    "codex-rs/state/src/migrations.rs",
    "codex-rs/state/src/sqlite.rs",
    "codex-rs/state/src/sqlite",
    "codex-rs/state/src/runtime.rs",
    "codex-rs/thread-store/src/local/mod.rs",
]


def git(*args):
    return subprocess.check_output(
        ["git", *args], text=True, stderr=subprocess.PIPE
    ).strip()


def main():
    try:
        if git("rev-parse", "--is-shallow-repository") != "false":
            raise ValueError("full immutable history is required")
        bases = git(
            "merge-base", "--all", "HEAD", "refs/remotes/shared-upstream/main"
        ).splitlines()
        if len(bases) != 1:
            raise ValueError("could not resolve one merged openai/codex ancestor")
        base = bases[0]
        print(f"merged_upstream: {base}")
        # ls-tree includes mode, object type, blob ID and path: detect byte and path drift.
        if git("ls-tree", "-r", base, "--", *CHAINS) != git(
            "ls-tree", "-r", "HEAD", "--", *CHAINS
        ):
            print(
                "status: failed\nreason: shared migration trees differ from merged upstream"
            )
            print(git("diff", "--name-status", base, "HEAD", "--", *CHAINS))
            return 1
        seams = git("diff", "--name-only", base, "HEAD", "--", *SEAMS).splitlines()
        print("status: passed")
        print(f"explicit_review_seams[{len(seams)}]: " + ",".join(seams))
        return 0
    except (subprocess.CalledProcessError, ValueError) as error:
        reason = (
            str(error)
            if isinstance(error, ValueError)
            else "fetch full openai/codex main history into refs/remotes/shared-upstream/main"
        )
        print(f"status: failed\nreason: {reason}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
