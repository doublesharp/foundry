#!/usr/bin/env python3
"""Exercise a packaged forgeds binary before publishing it."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def verify(binary, sha):
    binary = binary.resolve()
    with tempfile.TemporaryDirectory(prefix="forgeds-smoke-") as directory:
        root = Path(directory)
        env = {**os.environ, "NO_COLOR": "1"}

        def run(*args, overrides=None):
            return subprocess.check_output(
                [str(binary), *args], cwd=root, env={**env, **(overrides or {})}, text=True,
            )

        version = run("--version")
        assert version.splitlines()[0].startswith("forgeds "), version
        assert f"Commit SHA: {sha}" in version.splitlines(), version
        assert run("completions", "zsh").splitlines()[0] == "#compdef forgeds"

        (root / "foundry.toml").write_text(
            '[profile.default]\nout="project-out"\ncache_path="project-cache"\n'
        )
        config = json.loads(run("config", "--json"))
        assert (config["out"], config["cache_path"]) == ("forgeds-out", "forgeds-cache")
        config = json.loads(run("config", "--json", overrides={
            "FOUNDRY_OUT": "env-out", "FOUNDRY_CACHE_PATH": "env-cache",
        }))
        assert (config["out"], config["cache_path"]) == ("env-out", "env-cache")
        config = json.loads(run("config", "--json", "--out", "cli-out", "--cache-path", "cli-cache"))
        assert (config["out"], config["cache_path"]) == ("cli-out", "cli-cache")

        (root / "test").mkdir()
        (root / "test/CacheSmoke.t.sol").write_text(
            '// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n'
            'contract CacheSmoke { function testArithmetic() public pure { assert(2 + 2 == 4); } }\n'
        )
        for phase in ("cold", "shared-cache", "project-cache"):
            if phase == "shared-cache":
                shutil.rmtree(root / "forgeds-out")
                shutil.rmtree(root / "forgeds-cache")
            process = subprocess.run(
                [str(binary), "test", "--use", "0.8.35", "--match-contract", "CacheSmoke", "--json"],
                cwd=root, env={**env, "RUST_LOG": "foundry_compilers::compiler_cache=trace"},
                text=True, capture_output=True,
            )
            assert process.returncode == 0, process.stdout + process.stderr
            if phase == "shared-cache":
                assert re.search(r'outcome="?hit"?', process.stderr), process.stderr
            result = json.loads(process.stdout)
            tests = [test for suite in result.values() for test in suite["test_results"].values()]
            assert len(tests) == 1 and tests[0]["status"] == "Success", result
        assert (root / "forgeds-out/CacheSmoke.t.sol/CacheSmoke.json").is_file()
        assert (root / "forgeds-cache").is_dir()
        print(f"Verified forgeds {sha}: version, completions, config, Solidity test and both caches.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("sha")
    args = parser.parse_args()
    verify(args.binary, args.sha)
