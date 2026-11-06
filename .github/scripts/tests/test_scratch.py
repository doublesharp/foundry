import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).parents[1] / "scratch.py"


class ScratchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.repo = self.base / "checkout with spaces"
        (self.repo / ".cargo").mkdir(parents=True)
        self.root = self.base / "disposable files"
        self.root.mkdir()
        self.cache = self.base / "shared sccache"
        self.cache.mkdir()
        spec = importlib.util.spec_from_file_location("scratch", SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def test_worktrees_share_cache_but_not_build_outputs(self):
        second_repo = self.base / "second checkout"
        second_repo.mkdir()
        second_root = self.base / "second outputs"
        second_root.mkdir()
        self.module.configure(self.repo, self.root, self.cache)
        self.module.configure(second_repo, second_root, self.cache)
        first = self.module.environment(self.repo)
        second = self.module.environment(second_repo)
        self.assertNotEqual(first["CARGO_TARGET_DIR"], second["CARGO_TARGET_DIR"])
        self.assertNotEqual(first["TMPDIR"], second["TMPDIR"])
        self.assertEqual(first["SCCACHE_DIR"], str(self.cache))
        self.assertEqual(first["SCCACHE_DIR"], second["SCCACHE_DIR"])
        self.assertEqual(first["SCCACHE_SERVER_UDS"], second["SCCACHE_SERVER_UDS"])
        self.assertEqual(first["CARGO_INCREMENTAL"], "0")

    def test_missing_shared_cache_is_not_recreated(self):
        with self.assertRaisesRegex(ValueError, "cache"):
            self.module.configure(self.repo, self.root, self.base / "missing cache")
        self.assertFalse((self.base / "missing cache").exists())

    def test_run_preserves_explicit_incremental_setting(self):
        self.module.configure(self.repo, self.root, self.cache)
        with patch.dict(os.environ, {"CARGO_INCREMENTAL": "1"}):
            self.assertNotIn("CARGO_INCREMENTAL", self.module.environment(self.repo))

    @unittest.skipUnless(os.environ.get("FOUNDRY_TEST_SCCACHE"), "opt-in real sccache build")
    def test_separate_checkouts_reuse_compiled_dependency(self):
        self.assertIsNotNone(shutil.which("sccache"), "Install sccache before running this check")
        dependency = self.base / "dependency"
        (dependency / "src").mkdir(parents=True)
        (dependency / "Cargo.toml").write_text(
            '[package]\nname="cache-dependency"\nversion="0.0.0"\nedition="2021"\n'
        )
        (dependency / "src/lib.rs").write_text("pub fn value() -> u32 { 42 }\n")
        environments = []
        for name in ("one", "two"):
            repo = self.base / name
            (repo / "src").mkdir(parents=True)
            (repo / "src/lib.rs").write_text("pub fn value() -> u32 { cache_dependency::value() }\n")
            (repo / "Cargo.toml").write_text(
                '[package]\nname="cache-probe"\nversion="0.0.0"\nedition="2021"\n'
                f'[dependencies]\ncache-dependency={{path={json.dumps(str(dependency))}}}\n'
            )
            root = self.base / (name + "-outputs")
            root.mkdir()
            self.module.configure(repo, root, self.cache)
            (repo / ".cargo/config.toml").write_text('include = ["scratch.local.toml"]\n')
            env = {**os.environ, **self.module.environment(repo), "CARGO_INCREMENTAL": "0"}
            # Plain Cargo reads the saved target directory from configuration.
            # Exporting its path also adds it to sccache's CARGO_* environment key.
            env.pop("CARGO_TARGET_DIR", None)
            env.pop("CARGO_BUILD_TARGET_DIR", None)
            environments.append((repo, env))

        env = environments[0][1]
        def run(command, cwd=None):
            result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return result.stdout

        def hits():
            stats = json.loads(run(["sccache", "--show-stats", "--stats-format=json"]))
            return sum(stats["stats"]["cache_hits"]["counts"].values())

        run(["sccache", "--start-server"])
        try:
            before = hits()
            for repo, env in environments:
                run(["cargo", "build", "--offline", "--lib"], cwd=repo)
                after = hits()
                if repo.name == "one":
                    first = after
            self.assertGreater(after, first, "Second checkout must reuse a cached dependency")
            print(f"sccache hits: before={before}, first={first}, second={after}")
        finally:
            run(["sccache", "--stop-server"])

    def test_configure_and_run_from_saved_config(self):
        self.module.configure(self.repo, self.root)
        config = tomllib.loads((self.repo / ".cargo/scratch.local.toml").read_text())
        self.assertEqual(Path(config["build"]["target-dir"]), self.root / "target")
        self.assertEqual((self.repo / "target").resolve(), self.root / "target")
        env = self.module.environment(self.repo)
        result = subprocess.run(
            [sys.executable, "-c", "import tempfile; print(tempfile.gettempdir())"],
            env={**os.environ, **env}, text=True, capture_output=True, check=True,
        )
        self.assertEqual(Path(result.stdout.strip()), self.root / "tmp")

    def test_plain_cargo_automatically_loads_saved_root_and_defaults(self):
        (self.repo / "src").mkdir()
        (self.repo / "src/main.rs").write_text("fn main() {}\n")
        (self.repo / "Cargo.toml").write_text(
            '[package]\nname="scratch-check"\nversion="0.0.0"\nedition="2021"\n'
        )
        (self.repo / ".cargo/config.toml").write_text(
            (SCRIPT.parents[2] / ".cargo/config.toml").read_text()
        )
        env = os.environ.copy()
        env.pop("CARGO_TARGET_DIR", None)
        env.pop("CARGO_BUILD_TARGET_DIR", None)

        def target_directory():
            result = subprocess.run(
                ["cargo", "metadata", "--offline", "--no-deps", "--format-version=1"],
                cwd=self.repo, env=env, capture_output=True, text=True, check=True,
            )
            return Path(json.loads(result.stdout)["target_directory"]).resolve()

        self.assertEqual(target_directory(), self.repo / "target")
        self.module.configure(self.repo, self.root)
        self.assertEqual(target_directory(), self.root / "target")
        (self.repo / ".cargo/scratch.local.toml").unlink()
        (self.repo / "target").unlink()
        self.assertEqual(target_directory(), self.repo / "target")

    def test_reconfigure_preserves_old_artifacts(self):
        self.module.configure(self.repo, self.root)
        artifact = self.root / "target/keep"
        artifact.write_text("build")
        new_root = self.base / "second location"
        new_root.mkdir()
        self.module.configure(self.repo, new_root)
        self.assertEqual((self.repo / "target").resolve(), new_root / "target")
        self.assertEqual(artifact.read_text(), "build")

    def test_existing_target_is_not_replaced(self):
        (self.repo / "target").mkdir()
        (self.repo / "target/keep").write_text("build")
        with self.assertRaises(ValueError):
            self.module.configure(self.repo, self.root)
        self.assertEqual((self.repo / "target/keep").read_text(), "build")
        self.assertFalse((self.repo / ".cargo/scratch.local.toml").exists())

    def test_missing_root_is_not_recreated(self):
        with self.assertRaises(ValueError):
            self.module.configure(self.repo, self.base / "missing volume/root")
        self.assertFalse((self.base / "missing volume").exists())

    def test_missing_config_has_clear_error(self):
        with self.assertRaisesRegex(ValueError, "configure"):
            self.module.environment(self.repo)


if __name__ == "__main__":
    unittest.main()
