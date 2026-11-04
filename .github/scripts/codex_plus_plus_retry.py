"""Retry only proven macOS hosted-runner allocation failures, within retained artifacts."""

from datetime import datetime, timedelta, timezone
import json
import os
import re
import subprocess


WORKFLOW = "codex-plus-plus-release.yml"
TAG = re.compile(r"codex-plus-plus-v(\d+)\.(\d+)\.(\d+)-fork\.(\d+)")
MACOS_JOBS = ("build-macos / app", "build-macos / voice")
ALLOCATION_FAILURE = (
    "The job was not acquired by Runner of type hosted even after multiple attempts"
)


def github_api(path, *, method="GET", paginate=False):
    command = ["gh", "api", "--method", method, path]
    if paginate:
        command.extend(["--paginate", "--slurp"])
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout) if result.stdout.strip() else None


def retry_latest(api, repository, now):
    root = f"repos/{repository}"
    workflow = api(f"{root}/actions/workflows/{WORKFLOW}")
    cutoff = now - timedelta(days=1)
    since = cutoff.strftime("%Y-%m-%dT%H:%M:%SZ")
    pages = api(
        f"{root}/actions/workflows/{workflow['id']}/runs"
        f"?event=push&created=%3E{since}&per_page=100",
        paginate=True,
    )
    candidates = []
    for page in pages:
        for run in page["workflow_runs"]:
            match = TAG.fullmatch(run["head_branch"] or "")
            if (
                match
                and run["event"] == "push"
                and run["workflow_id"] == workflow["id"]
                and cutoff < datetime.fromisoformat(run["created_at"]) <= now
            ):
                candidates.append((tuple(map(int, match.groups())), run["id"], run))
    if not candidates:
        return {"status": "skipped", "reason": "no recent tag release"}
    _, _, run = max(candidates)
    if run["status"] != "completed":
        return {"status": "skipped", "reason": "latest release still active"}
    # Three extra run attempts total, shared by app and voice; never reset on a new job.
    if run["run_attempt"] >= 4:
        return {"status": "skipped", "reason": "three extra attempts exhausted"}
    commit = api(f"{root}/commits/{run['head_branch']}")
    if commit["sha"] != run["head_sha"]:
        return {"status": "skipped", "reason": "release tag no longer matches source"}

    pages = api(
        f"{root}/actions/runs/{run['id']}/jobs?filter=all&per_page=100",
        paginate=True,
    )
    # A targeted rerun can omit the untouched sibling from its new attempt.
    jobs = {}
    for page in pages:
        for job in page["jobs"]:
            previous = jobs.get(job["name"])
            if previous is None or (job["run_attempt"], job["id"]) > (
                previous["run_attempt"],
                previous["id"],
            ):
                jobs[job["name"]] = job
    if any(job["status"] != "completed" for job in jobs.values()):
        return {"status": "skipped", "reason": "jobs still active"}
    if any(
        job["conclusion"] not in ("success", "skipped")
        for name, job in jobs.items()
        if name not in MACOS_JOBS
    ):
        return {"status": "skipped", "reason": "unrelated job failed or cancelled"}

    pending = []
    for name in MACOS_JOBS:
        job = jobs.get(name)
        if job is None:
            return {"status": "skipped", "reason": "missing macOS job"}
        if job["conclusion"] == "success":
            continue
        if job["conclusion"] not in ("failure", "cancelled"):
            return {"status": "skipped", "reason": "macOS job not retryable"}
        # Use only this job's latest check, never an annotation from an older attempt.
        check_id = job["check_run_url"].rsplit("/", 1)[-1]
        annotations = api(
            f"{root}/check-runs/{check_id}/annotations?per_page=100", paginate=True
        )
        failures = [
            annotation["message"].strip()
            for page in annotations
            for annotation in page
            if annotation["annotation_level"] == "failure"
        ]
        if not failures or any(message != ALLOCATION_FAILURE for message in failures):
            return {
                "status": "skipped",
                "reason": "macOS failure is not hosted allocation",
            }
        pending.append(job)
    if not pending:
        return {"status": "skipped", "reason": "macOS already succeeded"}

    current = api(f"{root}/actions/runs/{run['id']}")
    if current["status"] != "completed" or current["run_attempt"] != run["run_attempt"]:
        return {"status": "skipped", "reason": "release changed during inspection"}
    job = pending[0]
    api(f"{root}/actions/jobs/{job['id']}/rerun", method="POST")
    return {"status": "retried", "run": run["id"], "job": job["id"]}


if __name__ == "__main__":
    result = retry_latest(
        github_api, os.environ["GITHUB_REPOSITORY"], datetime.now(timezone.utc)
    )
    for key, value in result.items():
        print(f"{key}: {value}")
