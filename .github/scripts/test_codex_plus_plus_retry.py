from datetime import datetime, timedelta, timezone
import unittest

from codex_plus_plus_retry import retry_latest


NOW = datetime(2026, 11, 4, 12, tzinfo=timezone.utc)
ROOT = "repos/owner/repo"
ALLOCATION_FAILURE = (
    "The job was not acquired by Runner of type hosted even after multiple attempts"
)


def source_run(**changes):
    return {
        "id": 100,
        "workflow_id": 7,
        "event": "push",
        "head_branch": "codex-plus-plus-v0.166.0-fork.4",
        "head_sha": "a" * 40,
        "created_at": (NOW - timedelta(hours=1)).isoformat(),
        "status": "completed",
        "conclusion": "failure",
        "run_attempt": 1,
        **changes,
    }


def job(name, identifier, conclusion="success", attempt=1):
    return {
        "id": identifier,
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "run_attempt": attempt,
        "check_run_url": f"https://api.github.com/{ROOT}/check-runs/{identifier}",
    }


class FakeGitHub:
    def __init__(self):
        self.runs = [source_run()]
        self.jobs = [
            job("build-windows / app", 10),
            job("build-linux / app", 20),
            job("build-macos / voice", 30, "cancelled"),
            job("build-macos / app", 40, "cancelled"),
            job("build-macos / build", 50, "skipped"),
        ]
        self.annotations = {
            identifier: [
                [
                    {
                        "annotation_level": "failure",
                        "message": ALLOCATION_FAILURE,
                    }
                ]
            ]
            for identifier in (30, 40)
        }
        self.commit = "a" * 40
        self.commits = {}
        self.current = None
        self.posts = []

    def __call__(self, path, *, method="GET", paginate=False):
        if method == "POST":
            self.posts.append(path)
            return None
        if path == f"{ROOT}/actions/workflows/codex-plus-plus-release.yml":
            return {"id": 7}
        if path.startswith(f"{ROOT}/actions/workflows/7/runs?"):
            assert paginate
            return [{"workflow_runs": self.runs[:1]}, {"workflow_runs": self.runs[1:]}]
        if path.startswith(f"{ROOT}/commits/"):
            return {"sha": self.commits.get(path.rsplit("/", 1)[-1], self.commit)}
        if "/jobs?filter=all&" in path:
            assert paginate
            return [{"jobs": self.jobs[:2]}, {"jobs": self.jobs[2:]}]
        if "/annotations?" in path:
            assert paginate
            return self.annotations[int(path.split("/")[-2])]
        if path.startswith(f"{ROOT}/actions/runs/"):
            return self.current or next(
                run for run in self.runs if run["id"] == int(path.rsplit("/", 1)[-1])
            )
        raise AssertionError(f"Unexpected API request: {method} {path}")


class RetryTest(unittest.TestCase):
    def run_controller(self, api):
        return retry_latest(api, "owner/repo", NOW)

    def test_only_one_allocation_job_retried_and_successful_jobs_preserved(self):
        api = FakeGitHub()
        self.assertEqual(
            self.run_controller(api), {"status": "retried", "run": 100, "job": 40}
        )
        self.assertEqual(api.posts, [f"{ROOT}/actions/jobs/40/rerun"])

    def test_next_pass_retries_untouched_voice_after_app_succeeds(self):
        api = FakeGitHub()
        api.runs[0]["run_attempt"] = 2
        api.runs[0]["conclusion"] = "success"
        api.jobs.append(job("build-macos / app", 60, attempt=2))
        self.assertEqual(self.run_controller(api)["job"], 30)
        self.assertEqual(api.posts, [f"{ROOT}/actions/jobs/30/rerun"])

    def test_cancelled_run_with_only_allocation_failures_is_retryable(self):
        api = FakeGitHub()
        api.runs[0]["conclusion"] = "cancelled"
        self.assertEqual(self.run_controller(api)["job"], 40)
        self.assertEqual(api.posts, [f"{ROOT}/actions/jobs/40/rerun"])

    def test_latest_check_replaces_old_allocation_failure(self):
        api = FakeGitHub()
        api.runs[0]["run_attempt"] = 2
        api.jobs.append(job("build-macos / app", 60, "failure", attempt=2))
        api.annotations[60] = [
            [{"annotation_level": "failure", "message": "compiler failed"}]
        ]
        self.assertEqual(self.run_controller(api)["status"], "skipped")
        self.assertEqual(api.posts, [])

    def test_active_queued_expired_and_capped_runs_do_not_retry(self):
        for changes in (
            {"status": "in_progress"},
            {"status": "queued"},
            {"run_attempt": 4},
            {"created_at": (NOW - timedelta(days=1)).isoformat()},
            {"created_at": (NOW - timedelta(days=2)).isoformat()},
        ):
            with self.subTest(changes=changes):
                api = FakeGitHub()
                api.runs[0].update(changes)
                self.assertEqual(self.run_controller(api)["status"], "skipped")
                self.assertEqual(api.posts, [])

    def test_successful_macos_jobs_do_not_retry(self):
        api = FakeGitHub()
        api.runs[0]["conclusion"] = "success"
        for entry in api.jobs:
            if entry["name"] in ("build-macos / app", "build-macos / voice"):
                entry["conclusion"] = "success"
        self.assertEqual(self.run_controller(api)["status"], "skipped")
        self.assertEqual(api.posts, [])

    def test_identity_requires_exact_workflow_push_tag_and_sha(self):
        for changes, commit in (
            ({"workflow_id": 8}, "a" * 40),
            ({"event": "workflow_dispatch"}, "a" * 40),
            ({"head_branch": "main"}, "a" * 40),
            ({"head_branch": "codex-plus-plus-v0.166.0-fork.4-extra"}, "a" * 40),
            ({}, "b" * 40),
        ):
            with self.subTest(changes=changes, commit=commit):
                api = FakeGitHub()
                api.runs[0].update(changes)
                api.commit = commit
                self.assertEqual(self.run_controller(api)["status"], "skipped")
                self.assertEqual(api.posts, [])

    def test_manual_cancellation_unrelated_failure_and_notices_do_not_retry(self):
        for annotations in (
            [],
            [{"annotation_level": "failure", "message": "The operation was canceled."}],
            [{"annotation_level": "failure", "message": "cargo failed"}],
            [{"annotation_level": "notice", "message": ALLOCATION_FAILURE}],
            [
                {"annotation_level": "failure", "message": ALLOCATION_FAILURE},
                {"annotation_level": "failure", "message": "compiler failed"},
            ],
        ):
            with self.subTest(annotations=annotations):
                api = FakeGitHub()
                api.runs[0]["conclusion"] = "cancelled"
                api.annotations[40] = [annotations]
                self.assertEqual(self.run_controller(api)["status"], "skipped")
                self.assertEqual(api.posts, [])

    def test_unrelated_cancelled_or_running_job_blocks_retry(self):
        for conclusion, status in (("cancelled", "completed"), (None, "in_progress")):
            with self.subTest(conclusion=conclusion):
                api = FakeGitHub()
                api.jobs[0].update(conclusion=conclusion, status=status)
                self.assertEqual(self.run_controller(api)["status"], "skipped")
                self.assertEqual(api.posts, [])

    def test_latest_version_wins_and_does_not_fall_back_to_older_failure(self):
        api = FakeGitHub()
        api.runs = [
            source_run(head_branch="codex-plus-plus-v0.166.0-fork.9"),
            source_run(
                id=101, head_branch="codex-plus-plus-v0.166.0-fork.10", status="queued"
            ),
        ]
        self.assertEqual(self.run_controller(api)["status"], "skipped")
        self.assertEqual(api.posts, [])

    def test_run_restarted_during_inspection_does_not_get_second_retry(self):
        api = FakeGitHub()
        api.current = source_run(status="queued", run_attempt=2)
        self.assertEqual(self.run_controller(api)["status"], "skipped")
        self.assertEqual(api.posts, [])

    def test_newest_exact_source_release_is_selected_after_tag_moved(self):
        api = FakeGitHub()
        api.runs.append(
            source_run(id=101, head_branch="codex-plus-plus-v0.166.0-fork.5")
        )
        api.commits["codex-plus-plus-v0.166.0-fork.5"] = "b" * 40
        self.assertEqual(
            self.run_controller(api), {"status": "retried", "run": 100, "job": 40}
        )
        self.assertEqual(api.posts, [f"{ROOT}/actions/jobs/40/rerun"])

    def test_final_extra_attempt_is_allowed_but_never_resets_for_voice(self):
        api = FakeGitHub()
        api.runs[0]["run_attempt"] = 3
        api.jobs.append(job("build-macos / app", 60, attempt=3))
        self.assertEqual(self.run_controller(api)["job"], 30)
        self.assertEqual(api.posts, [f"{ROOT}/actions/jobs/30/rerun"])
        api.posts.clear()
        api.runs[0]["run_attempt"] = 4
        self.assertEqual(self.run_controller(api)["status"], "skipped")
        self.assertEqual(api.posts, [])


if __name__ == "__main__":
    unittest.main()
