#!/usr/bin/env python3
"""Exercise installed action outputs and Solidity compilation on hosted runners."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def shared_cache_hit(stderr):
    plain = re.sub(r"\x1b\[[0-9;]*m", "", stderr)
    return 'outcome="hit"' in plain or 'outcome=hit' in plain


def verify(env):
    forgeds = Path(env["FORGEDS_PATH"])
    solar = Path(env["SOLAR_PATH"])
    alias = Path(env["FORGE_PATH"])
    assert forgeds.is_file() and solar.is_file() and alias.is_file()
    assert env["WARM_DOWNLOADS"] == "0", "Warm installation downloaded archives"
    # The CLI banner uses argv[0]; compare build identity and configuration instead.
    forgeds_version = subprocess.check_output([str(forgeds), "--version"], text=True).splitlines()
    alias_version = subprocess.check_output([str(alias), "--version"], text=True).splitlines()
    assert forgeds_version[1:] == alias_version[1:]
    assert Path(shutil.which("forgeds", path=env["PATH"])).resolve() == forgeds.resolve()
    assert Path(shutil.which("forge", path=env["PATH"])).resolve() == alias.resolve()
    with tempfile.TemporaryDirectory(dir=env["RUNNER_TEMP"], prefix="setup-forgeds-smoke-") as directory:
        root = Path(directory)
        (root / "foundry.toml").write_text("[profile.default]\nevm_version = 'cancun'\n")
        (root / "test").mkdir()
        (root / "test/Setup.t.sol").write_text(
            "// SPDX-License-Identifier: MIT\npragma solidity ^0.8.0;\n"
            "contract SetupTest { function testArithmetic() public pure { assert(2 + 2 == 4); } }\n"
        )
        for binary in (forgeds, alias):
            config = json.loads(subprocess.check_output([str(binary), "config", "--root", str(root), "--json"], text=True))
            assert (config["out"], config["cache_path"]) == ("forgeds-out", "forgeds-cache")
        command = [str(forgeds), "test", "--root", str(root), "--use", str(solar), "--json"]
        for phase in ("cold", "shared-cache"):
            if phase == "shared-cache":
                shutil.rmtree(root / "forgeds-out")
                shutil.rmtree(root / "forgeds-cache")
            run = subprocess.run(command, capture_output=True, text=True,
                                 env={**env, "RUST_LOG": "foundry_compilers::compiler_cache=trace"})
            assert run.returncode == 0, run.stdout + run.stderr
            result = json.loads(run.stdout)
            tests = [test for suite in result.values() for test in suite["test_results"].values()]
            assert len(tests) == 1 and tests[0]["status"] == "Success", result
            if phase == "shared-cache":
                assert shared_cache_hit(run.stderr), run.stderr
    print("Verified action PATH, native alias, zero warm downloads, Solar test and shared compiler cache.")


if __name__ == "__main__":
    verify(os.environ)
