"""Only matching artifacts from trusted releases may supply native libraries."""

import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from codex_package.codex_plus_plus.voice_cache import find_cache


class VoiceCacheTests(unittest.TestCase):
    def test_cross_tag_reuse_rejects_untrusted_failed_and_expired_artifacts(self):
        artifacts = [
            {"name": "other-key", "expired": False, "workflow_run": {"id": 99}},
            {"name": "native-key", "expired": True, "workflow_run": {"id": 98}},
            *(
                {"name": "native-key", "expired": False, "workflow_run": {"id": i}}
                for i in range(97, 92, -1)
            ),
        ]
        trusted = {
            "event": "push",
            "path": ".github/workflows/codex-plus-plus-release.yml",
            "conclusion": "success",
            "head_branch": "codex-plus-plus-v0.160.1-fork.2",
        }
        runs = {
            97: {**trusted, "event": "pull_request"},
            96: {**trusted, "path": ".github/workflows/unrelated.yml"},
            95: {**trusted, "conclusion": "failure"},
            94: {**trusted, "head_branch": "main"},
            93: trusted,
        }

        def api(command, **_):
            endpoint = command[-1]
            if "/artifacts?" in endpoint:
                # The eligible release is on a later API page.
                return json.dumps(
                    [{"artifacts": artifacts[:4]}, {"artifacts": artifacts[4:]}]
                )
            return json.dumps(runs[int(endpoint.rsplit("/", 1)[1])])

        with patch("subprocess.check_output", side_effect=api):
            self.assertEqual(find_cache("owner/repo", "native-key", 100), 93)

    def test_retry_reuses_its_own_completed_native_artifact(self):
        listing = [
            {
                "artifacts": [
                    {
                        "name": "native-key",
                        "expired": False,
                        "workflow_run": {"id": 100},
                    }
                ]
            }
        ]
        with patch("subprocess.check_output", return_value=json.dumps(listing)):
            self.assertEqual(find_cache("owner/repo", "native-key", 100), 100)

    def test_no_matching_artifact_is_a_cold_build(self):
        with patch(
            "subprocess.check_output", return_value=json.dumps([{"artifacts": []}])
        ):
            self.assertIsNone(find_cache("owner/repo", "native-key", 100))


if __name__ == "__main__":
    unittest.main()
