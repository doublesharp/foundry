import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile


SCRIPT = Path(__file__).parents[1] / "setup-forgeds.py"
SPEC = importlib.util.spec_from_file_location("setup_forgeds", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class SetupForgedsTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.env = {
            "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64",
            "RUNNER_TEMP": str(self.root), "GITHUB_SHA": "a" * 40,
        }

    def plan(self, **overrides):
        return MODULE.make_plan({**self.env, **overrides})

    def archive(self, name="forgeds", commit=None, paths=None):
        commit = commit or MODULE.PINS["forgeds"]["commit"]
        binary = (f"#!/bin/sh\nprintf '%s\\n' '{name} 0.3.0+commit.{commit[:8]}' 'Commit SHA: {commit}'\n").encode()
        archive = self.root / (name + ".tar.gz")
        with tarfile.open(archive, "w:gz") as bundle:
            for path in paths or ["nested/" + name]:
                member = tarfile.TarInfo(path)
                member.size = len(binary)
                member.mode = 0o755
                bundle.addfile(member, io.BytesIO(binary))
        return archive

    def serve_archive(self, source, url, target):
        shutil.copyfile(source, target)
        return hashlib.sha256(target.read_bytes()).hexdigest()

    def test_default_selects_pinned_prebuilt_without_solar(self):
        plan = self.plan()
        self.assertEqual(list(plan.get("tools", {})), ["forgeds"])
        self.assertEqual(plan["tools"]["forgeds"]["commit"], "4ca15fcba25aa61a1b436d28b428e30b459f3d56")
        self.assertEqual(plan["tools"]["forgeds"]["sha256"], "a1341d56a7fc743b4d87853596baaf0931a6e4348106def1a6994bc793c5b62f")

    def test_plan_needs_no_network_for_pinned_tools(self):
        with patch.object(MODULE, "download", side_effect=AssertionError("network")):
            plan = self.plan(SOLAR_VERSION="0.3.0")
        self.assertEqual(list(plan["tools"]), ["forgeds", "solar"])
        self.assertEqual(plan["tools"]["solar"]["commit"], MODULE.PINS["solar"]["commit"])
        self.assertEqual((plan["alias"], plan["binary-cache"], plan["compiler-cache"]), (False, True, False))

    def test_windows_selects_native_executables_and_zip(self):
        plan = self.plan(RUNNER_OS="Windows", SOLAR_VERSION="v0.3.0")
        self.assertTrue(plan["tools"]["solar"]["url"].endswith("solar-x86_64-pc-windows-msvc.zip"))
        self.assertTrue(plan["tools"]["forgeds"]["url"].endswith("win32_amd64.tar.gz"))

    def test_all_supported_platforms_have_pinned_hashes(self):
        for platform in MODULE.PLATFORMS:
            system, arch = platform.split("-")
            plan = self.plan(RUNNER_OS=system, RUNNER_ARCH=arch, SOLAR_VERSION="v0.3.0")
            for tool in plan["tools"].values():
                self.assertRegex(tool["sha256"], r"^[0-9a-f]{64}$")

    def test_unsupported_platform_fails_before_download(self):
        with self.assertRaisesRegex(ValueError, "No prebuilt forgeds for macOS-X64"):
            self.plan(RUNNER_OS="macOS")

    def test_short_or_injected_versions_and_invalid_booleans_are_rejected(self):
        for overrides in ({"FORGEDS_VERSION": "4ca15fc"}, {"FORGEDS_VERSION": "$(touch bad)"},
                          {"SOLAR_VERSION": "latest"}, {"FORGE_ALIAS": "yes"},
                          {"CACHE_BINARIES": "yes"}, {"CACHE_COMPILERS": "yes"},
                          {"CACHE_PREFIX": "../../bad"}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.plan(**overrides)

    def test_full_custom_commit_and_release_resolve_to_fixed_assets(self):
        plan = self.plan(FORGEDS_VERSION="b" * 40, SOLAR_VERSION="v0.4.0")
        self.assertTrue(plan["tools"]["forgeds"]["url"].endswith("forgeds-bbbbbbb_linux_amd64.tar.gz"))
        self.assertIsNone(plan["tools"]["forgeds"]["sha256"])
        self.assertIsNone(plan["tools"]["solar"]["sha256"])

    def test_latest_is_resolved_before_cache_lookup(self):
        with patch.object(MODULE, "read_remote", return_value="b" * 40) as read:
            plan = self.plan(FORGEDS_VERSION="latest")
        read.assert_called_once_with(MODULE.FORGEDS_URL + "forgeds_latest.txt")
        self.assertEqual(plan["tools"]["forgeds"]["commit"], "b" * 40)

    def test_tool_cache_is_stable_across_source_changes(self):
        original = self.plan()
        other = self.plan(GITHUB_SHA="b" * 40)
        self.assertEqual(original["tool-key"], other["tool-key"])
        self.assertEqual(original["compiler-prefix"], other["compiler-prefix"])
        self.assertNotEqual(original["compiler-key"], other["compiler-key"])
        for overrides in ({"SOLAR_VERSION": "v0.3.0"}, {"FORGE_ALIAS": "true"},
                          {"RUNNER_ARCH": "ARM64"}, {"FORGEDS_VERSION": "b" * 40}):
            self.assertNotEqual(original["tool-key"], self.plan(**overrides)["tool-key"])

    def test_compiler_namespace_does_not_duplicate_binary_cache(self):
        original = self.plan()
        other = self.plan(CACHE_PREFIX="contracts")
        self.assertEqual(original["tool-key"], other["tool-key"])
        self.assertNotEqual(original["compiler-prefix"], other["compiler-prefix"])

    @unittest.skipIf(os.name == "nt", "fixture executable is a POSIX shell script")
    def test_cold_then_warm_install_no_network_and_native_alias(self):
        archive = self.archive()
        plan = self.plan(FORGE_ALIAS="true")
        plan["tools"]["forgeds"]["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
        with patch.object(MODULE, "download", side_effect=lambda u, p: self.serve_archive(archive, u, p)) as download:
            cold = MODULE.install(plan, self.env)
        self.assertEqual(download.call_count, 1)
        self.assertEqual(cold["downloads"], 1)
        self.assertEqual(cold["solar-path"], "")
        self.assertEqual(subprocess.check_output([cold["forge-path"], "--version"]),
                         subprocess.check_output([cold["forgeds-path"], "--version"]))
        with patch.object(MODULE, "download", side_effect=AssertionError("warm install used network")):
            warm = MODULE.install(plan, self.env)
        self.assertEqual(warm["downloads"], 0)

    def test_bad_checksum_is_rejected_before_extraction_or_execution(self):
        plan = self.plan()
        with patch.object(MODULE, "download", return_value="0" * 64), patch.object(MODULE, "extract") as extract:
            with self.assertRaisesRegex(ValueError, "archive checksum mismatch"):
                MODULE.install(plan, self.env)
        extract.assert_not_called()
        self.assertFalse((Path(plan["bin-dir"]) / "forgeds").exists())

    @unittest.skipIf(os.name == "nt", "fixture executable is a POSIX shell script")
    def test_wrong_commit_is_never_published(self):
        archive = self.archive(commit="b" * 40)
        plan = self.plan()
        plan["tools"]["forgeds"]["sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
        with patch.object(MODULE, "download", side_effect=lambda u, p: self.serve_archive(archive, u, p)):
            with self.assertRaisesRegex(ValueError, "binary version or commit mismatch"):
                MODULE.install(plan, self.env)
        self.assertFalse((Path(plan["bin-dir"]) / "forgeds").exists())

    def test_extract_copies_executable_without_traversal(self):
        archive = self.archive(paths=["../../forgeds"])
        destination = self.root / "bin" / "forgeds"
        destination.parent.mkdir()
        MODULE.extract(archive, destination)
        self.assertTrue(destination.is_file())
        self.assertFalse((self.root / "forgeds").exists())
        self.assertEqual(destination.stat().st_mode & 0o777, 0o755)

    def test_duplicate_executable_rejected(self):
        archive = self.archive(paths=["forgeds", "nested/forgeds"])
        with self.assertRaisesRegex(ValueError, "Expected one regular forgeds"):
            MODULE.extract(archive, self.root / "forgeds")

    def test_tar_symlink_is_never_extracted(self):
        archive = self.root / "symlink.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            member = tarfile.TarInfo("forgeds")
            member.type = tarfile.SYMTYPE
            member.linkname = "/bin/sh"
            bundle.addfile(member)
        with self.assertRaisesRegex(ValueError, "Expected one regular forgeds"):
            MODULE.extract(archive, self.root / "forgeds")

    def test_windows_zip_extracts_only_regular_solar_executable(self):
        archive = self.root / "solar.zip"
        with zipfile.ZipFile(archive, "w") as bundle:
            bundle.writestr("../solar.exe", b"solar executable")
            bundle.writestr("../outside.txt", b"do not extract")
        destination = self.root / "solar.exe"
        MODULE.extract(archive, destination)
        self.assertEqual(destination.read_bytes(), b"solar executable")
        self.assertFalse((self.root.parent / "outside.txt").exists())

    def test_zip_symlink_is_never_extracted(self):
        archive = self.root / "symlink.zip"
        with zipfile.ZipFile(archive, "w") as bundle:
            member = zipfile.ZipInfo("solar.exe")
            member.create_system = 3
            member.external_attr = (stat.S_IFLNK | 0o777) << 16
            bundle.writestr(member, "/bin/sh")
        with self.assertRaisesRegex(ValueError, "Expected one regular solar.exe"):
            MODULE.extract(archive, self.root / "solar.exe")

    def test_actual_composite_scripts_forward_inputs_and_outputs(self):
        # Execute the shipped shell fragments, rather than a duplicate installer command.
        action_root = SCRIPT.parents[2]
        action = (action_root / "action.yml").read_text()
        steps = {re.search(r"      id: (\w+)", step)[1]: step
                 for step in action.split("    - name:") if "      id: " in step}
        supplied = {"forgeds-version": "b" * 40, "solar-version": "v0.4.0", "forge-alias": "true",
                    "cache": "false", "cache-compilers": "true", "cache-prefix": "custom"}
        env = {**os.environ, **self.env, "GITHUB_ACTION_PATH": str(action_root),
               "GITHUB_OUTPUT": str(self.root / "outputs"), "GITHUB_PATH": str(self.root / "paths")}
        for name, key in re.findall(r"        ([A-Z_]+): \$\{\{ inputs\.([\w-]+) \}\}", steps["plan"]):
            env[name] = supplied[key]
        fragment = re.search(r"      run: (.+)", steps["plan"])[1]
        subprocess.run(["bash", "-e", "-c", fragment], env=env, check=True)
        result = dict(line.split("=", 1) for line in Path(env["GITHUB_OUTPUT"]).read_text().splitlines())
        plan = json.loads(Path(result["plan-path"]).read_text())
        self.assertEqual(plan["tools"]["forgeds"]["commit"], "b" * 40)
        self.assertEqual(plan["tools"]["solar"]["version"], "0.4.0")
        self.assertEqual((plan["alias"], plan["binary-cache"], plan["compiler-cache"]), (True, False, True))
        self.assertTrue(plan["compiler-prefix"].startswith("setup-forgeds-compilers-v1-custom-"))
        self.assertIn("PLAN_PATH: ${{ steps.plan.outputs.plan-path }}", steps["install"])
        self.assertIn("key: ${{ steps.plan.outputs.tool-key }}", steps["binaries"])

    def test_multiline_outputs_are_rejected(self):
        env = {"GITHUB_OUTPUT": str(self.root / "outputs")}
        with self.assertRaisesRegex(ValueError, "Invalid multiline output"):
            MODULE.outputs({"path": "bad\ninjection=value"}, env)

    def test_release_tree_is_small_self_contained_and_preserves_source_index(self):
        release = SCRIPT.with_name("release-setup-forgeds.py")
        spec = importlib.util.spec_from_file_location("release_setup_forgeds", release)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        repo = self.root / "source"
        repo.mkdir()
        for original in module.FILES:
            destination = repo / original
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = SCRIPT.parents[2] / original
            if original == "SETUP_FORGEDS.md" and not source.exists():
                source = SCRIPT.parents[2] / "README.md"
            shutil.copyfile(source, destination)
        (repo / "unrelated-rust-source.rs").write_text("must stay outside the action package")
        environment = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull,
                       "GIT_AUTHOR_NAME": "Action test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
                       "GIT_COMMITTER_NAME": "Action test", "GIT_COMMITTER_EMAIL": "test@example.invalid"}
        def git(*args):
            return subprocess.check_output(["git", *args], cwd=repo, env=environment, text=True).strip()
        git("init", "--quiet")
        git("add", ".")
        git("-c", "core.hooksPath=" + os.devnull, "commit", "--quiet", "-m", "fixture")
        index = (repo / ".git/index").read_bytes()
        code = (
            "import importlib.util,os;"
            f"s=importlib.util.spec_from_file_location('release',{str(release)!r});"
            "m=importlib.util.module_from_spec(s);s.loader.exec_module(m);"
            "print(m.package_tree('HEAD',os.environ))"
        )
        tree = subprocess.check_output([sys.executable, "-c", code], cwd=repo,
                                       env={**environment, "RUNNER_TEMP": str(self.root)}, text=True).strip()
        self.assertEqual(git("ls-tree", "-r", "--name-only", tree).splitlines(), sorted(module.FILES.values()))
        self.assertEqual(git("show", tree + ":README.md"), (repo / "SETUP_FORGEDS.md").read_text().strip())
        self.assertEqual((repo / ".git/index").read_bytes(), index)


if __name__ == "__main__":
    unittest.main()
