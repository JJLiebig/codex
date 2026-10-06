"""Find native voice artifacts from trusted release runs across tag scopes."""

import json
import os
from pathlib import Path
import subprocess
from urllib.parse import quote


def find_cache(repository: str, name: str, current_run: int) -> int | None:
    pages = json.loads(
        subprocess.check_output(
            [
                "gh",
                "api",
                "--paginate",
                "--slurp",
                f"repos/{repository}/actions/artifacts?name={quote(name)}&per_page=100",
            ],
            text=True,
        )
    )
    for page in pages:
        for artifact in page["artifacts"]:
            if artifact["name"] != name or artifact["expired"]:
                continue
            run_id = artifact["workflow_run"]["id"]
            # A failed-job retry may reuse its own already-completed native output.
            if run_id == current_run:
                return run_id
            run = json.loads(
                subprocess.check_output(
                    ["gh", "api", f"repos/{repository}/actions/runs/{run_id}"],
                    text=True,
                )
            )
            if (
                run["event"] == "push"
                and run["path"] == ".github/workflows/codex-plus-plus-release.yml"
                and run["conclusion"] == "success"
                and (run["head_branch"] or "").startswith("codex-plus-plus-v")
            ):
                return run_id
    return None


if __name__ == "__main__":
    run_id = find_cache(
        os.environ["GITHUB_REPOSITORY"],
        os.environ["CACHE_NAME"],
        int(os.environ["GITHUB_RUN_ID"]),
    )
    with Path(os.environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        output.write(f"run-id={run_id or ''}\n")
