import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import unittest
from unittest import mock


SCRIPT = Path(__file__).parents[1] / "matrices.py"


class MatrixCacheTests(unittest.TestCase):
    def matrix(self, event="pull_request", profile="default", filter=None, partitions=1):
        with mock.patch.dict(os.environ, {"EVENT_NAME": event, "PROFILE": profile}):
            spec = importlib.util.spec_from_file_location("matrices", SCRIPT)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            if filter is not None:
                module.config[0].filter = filter
            module.config[0].n_partitions = partitions
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                module.main()
        return json.loads(output.getvalue())["include"]

    def test_pull_requests_reuse_matching_push_caches(self):
        pushed = self.matrix(event="push")
        for entry in self.matrix():
            match = next(
                candidate for candidate in pushed
                if (candidate["target"], candidate["filter"], candidate["partition"])
                == (entry["target"], entry["filter"], entry["partition"])
            )
            self.assertNotEqual(entry["name"], match["name"])
            self.assertIn("cache_key", entry)
            self.assertEqual(entry["cache_key"], match["cache_key"])

    def test_targets_suites_and_partitions_have_separate_caches(self):
        entries = self.matrix(event="push", partitions=2)
        self.assertEqual(len({entry["cache_key"] for entry in entries}), len(entries))

    def test_changed_filter_does_not_restore_previous_suite_cache(self):
        original = self.matrix()[0]
        changed = self.matrix(filter="package(=forge)")[0]
        self.assertEqual(original["name"], changed["name"])
        self.assertNotEqual(original["cache_key"], changed["cache_key"])

    def test_partition_count_and_profile_change_cache_identity(self):
        original = self.matrix()[0]["cache_key"]
        self.assertNotEqual(original, self.matrix(partitions=2)[0]["cache_key"])
        self.assertNotEqual(original, self.matrix(profile="nightly")[0]["cache_key"])


if __name__ == "__main__":
    unittest.main()
