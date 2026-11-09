#!/usr/bin/env python3
"""Install verified prebuilt forgeds and optional Solar for GitHub Actions."""

from concurrent.futures import ThreadPoolExecutor
import hashlib
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
import time
import urllib.error
import urllib.request
import zipfile


PINS = json.loads((Path(__file__).parents[1] / "forgeds-toolchain.json").read_text())
FORGEDS_URL = "https://github.com/doublesharp/foundry/releases/download/forgeds/"
SOLAR_URL = "https://github.com/paradigmxyz/solar/releases/download/"
PLATFORMS = {
    "Linux-X64": ("linux_amd64", "x86_64-unknown-linux-gnu.tar.gz"),
    "Linux-ARM64": ("linux_arm64", "aarch64-unknown-linux-gnu.tar.gz"),
    "macOS-ARM64": ("darwin_arm64", "aarch64-apple-darwin.tar.gz"),
    "Windows-X64": ("win32_amd64", "x86_64-pc-windows-msvc.zip"),
}


def boolean(env, name, default):
    value = env.get(name, default).strip().lower()
    if value not in ("true", "false"):
        raise ValueError(f"{name} must be true or false")
    return value == "true"


def download(url, destination):
    """Hash while streaming, and retry transient download failures only."""
    for attempt in range(3):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "setup-forgeds"})
            digest = hashlib.sha256()
            with urllib.request.urlopen(request, timeout=60) as response, destination.open("wb") as output:
                for block in iter(lambda: response.read(1024 * 1024), b""):
                    digest.update(block)
                    output.write(block)
            return digest.hexdigest()
        except (OSError, urllib.error.URLError) as error:
            if isinstance(error, urllib.error.HTTPError) and error.code < 500 and error.code not in (408, 429):
                raise
            if attempt == 2:
                raise
            time.sleep(attempt + 1)


def read_remote(url):
    with tempfile.TemporaryDirectory(dir=os.environ.get("RUNNER_TEMP")) as directory:
        path = Path(directory) / "value"
        download(url, path)
        return path.read_text().strip()


def make_plan(env):
    platform = env["RUNNER_OS"] + "-" + env["RUNNER_ARCH"]
    if platform not in PLATFORMS:
        raise ValueError(f"No prebuilt forgeds for {platform}; supported: {', '.join(PLATFORMS)}")
    forgeds_version = env.get("FORGEDS_VERSION", "").strip() or PINS["forgeds"]["commit"]
    if forgeds_version == "latest":
        forgeds_version = read_remote(FORGEDS_URL + "forgeds_latest.txt")
    if not re.fullmatch(r"[0-9a-f]{40}", forgeds_version):
        raise ValueError("forgeds-version must be a full 40-character commit SHA or latest")
    solar_version = env.get("SOLAR_VERSION", "").strip()
    if solar_version and not re.fullmatch(r"v?\d+\.\d+\.\d+", solar_version):
        raise ValueError("solar-version must be an exact release such as v0.3.0, or empty")
    alias = boolean(env, "FORGE_ALIAS", "false")
    binary_cache = boolean(env, "CACHE_BINARIES", "true")
    compiler_cache = boolean(env, "CACHE_COMPILERS", "false")
    namespace = env.get("CACHE_PREFIX", "default").strip()
    if not re.fullmatch(r"[a-zA-Z0-9_.-]{1,80}", namespace):
        raise ValueError("cache-prefix must contain 1-80 letters, numbers, dots, underscores or hyphens")
    forgeds_asset = "forgeds-" + forgeds_version[:7] + "_" + PLATFORMS[platform][0] + ".tar.gz"
    tools = {"forgeds": {
        "url": FORGEDS_URL + forgeds_asset, "commit": forgeds_version,
        "sha256": PINS["forgeds"]["assets"][platform]["sha256"] if forgeds_version == PINS["forgeds"]["commit"] else None,
    }}
    if solar_version:
        solar_version = "v" + solar_version.removeprefix("v")
        pinned = solar_version == PINS["solar"]["version"]
        tools["solar"] = {
            "url": SOLAR_URL + solar_version + "/solar-" + PLATFORMS[platform][1],
            "version": solar_version[1:], "commit": PINS["solar"]["commit"] if pinned else None,
            "sha256": PINS["solar"]["assets"][platform]["sha256"] if pinned else None,
        }
    identity = json.dumps({"platform": platform, "tools": tools, "alias": alias}, sort_keys=True)
    key = "setup-forgeds-tools-v1-" + platform + "-" + hashlib.sha256(identity.encode()).hexdigest()[:24]
    root = Path(env.get("RUNNER_TOOL_CACHE") or env["RUNNER_TEMP"]) / "setup-forgeds" / key
    prefix = "setup-forgeds-compilers-v1-" + namespace + "-" + key.removeprefix("setup-forgeds-tools-v1-") + "-"
    return {
        "tools": tools, "platform": platform, "alias": alias,
        "binary-cache": binary_cache, "compiler-cache": compiler_cache,
        "bin-dir": str(root / "bin"), "tool-key": key,
        "compiler-path": str(Path.home() / ".foundry" / "cache" / "compilers"),
        "compiler-key": prefix + env.get("GITHUB_SHA", "local"), "compiler-prefix": prefix,
    }


def version(binary, tool):
    if not binary.is_file():
        return None
    try:
        result = subprocess.run([str(binary), "--version"], text=True, capture_output=True, timeout=20)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode:
        return None
    lines = result.stdout.splitlines()
    commits = [line.removeprefix("Commit SHA: ") for line in lines if line.startswith("Commit SHA: ")]
    if len(commits) != 1 or not re.fullmatch(r"[0-9a-f]{40}", commits[0]):
        return None
    if tool.get("commit") and tool["commit"] != commits[0]:
        return None
    if tool.get("version") and (not lines or not lines[0].startswith("solar " + tool["version"] + "+")):
        return None
    return commits[0]


def extract(archive, destination):
    """Copy only one regular executable; archive paths and links are never extracted."""
    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as bundle:
            members = [m for m in bundle.infolist() if not m.is_dir()
                       and not stat.S_ISLNK(m.external_attr >> 16)
                       and Path(m.filename).name == destination.name]
            if len(members) != 1:
                raise ValueError(f"Expected one regular {destination.name} executable in archive")
            with bundle.open(members[0]) as source, destination.open("wb") as output:
                shutil.copyfileobj(source, output)
    else:
        with tarfile.open(archive) as bundle:
            members = [m for m in bundle.getmembers() if m.isfile() and Path(m.name).name == destination.name]
            if len(members) != 1:
                raise ValueError(f"Expected one regular {destination.name} executable in archive")
            with bundle.extractfile(members[0]) as source, destination.open("wb") as output:
                shutil.copyfileobj(source, output)
    destination.chmod(0o755)


def install_tool(name, tool, directory, temp, windows=False):
    binary = directory / (name + (".exe" if windows else ""))
    commit = version(binary, tool)
    if commit:
        print(f"{name} {commit}: already installed; no download", flush=True)
        return commit, 0
    with tempfile.TemporaryDirectory(dir=temp, prefix="setup-forgeds-") as work:
        archive = Path(work) / "archive"
        expected = tool["sha256"]
        if expected is None:
            checksum = Path(work) / "checksum"
            download(tool["url"] + ".sha256", checksum)
            fields = checksum.read_text().split()
            if not fields or not re.fullmatch(r"[0-9a-fA-F]{64}", fields[0]):
                raise ValueError(f"Invalid {name} SHA-256 sidecar")
            expected = fields[0].lower()
        actual = download(tool["url"], archive)
        if actual != expected:
            raise ValueError(f"{name} archive checksum mismatch")
        # Stage on the destination filesystem so replacement is atomic, including on Windows.
        with tempfile.TemporaryDirectory(dir=directory, prefix=".install-") as staged:
            candidate = Path(staged) / binary.name
            extract(archive, candidate)
            commit = version(candidate, tool)
            if not commit:
                raise ValueError(f"{name} binary version or commit mismatch")
            candidate.replace(binary)
    print(f"{name} {commit}: installed verified prebuilt binary", flush=True)
    return commit, 1


def install(plan, env):
    directory = Path(plan["bin-dir"])
    directory.mkdir(parents=True, exist_ok=True)
    temp = Path(env["RUNNER_TEMP"])
    temp.mkdir(parents=True, exist_ok=True)
    windows = plan["platform"].startswith("Windows-")
    with ThreadPoolExecutor(max_workers=len(plan["tools"])) as pool:
        tasks = {name: pool.submit(install_tool, name, tool, directory, temp, windows)
                 for name, tool in plan["tools"].items()}
        installed = {name: task.result() for name, task in tasks.items()}
    extension = ".exe" if windows else ""
    forgeds = directory / ("forgeds" + extension)
    alias = directory / ("forge" + extension)
    if plan["alias"]:
        if alias.exists():
            alias.unlink()
        try:
            os.link(forgeds, alias)
        except OSError:
            shutil.copy2(forgeds, alias)
    return {
        "bin-dir": str(directory), "forgeds-path": str(forgeds),
        "forge-path": str(alias) if plan["alias"] else "",
        "solar-path": str(directory / ("solar" + extension)) if "solar" in installed else "",
        "forgeds-commit": installed["forgeds"][0],
        "solar-commit": installed.get("solar", ("", 0))[0],
        "downloads": sum(item[1] for item in installed.values()),
    }


def outputs(values, env):
    with Path(env["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as output:
        for key, value in values.items():
            value = str(value).lower() if isinstance(value, bool) else str(value)
            if "\n" in value or "\r" in value:
                raise ValueError(f"Invalid multiline output: {key}")
            output.write(f"{key}={value}\n")


def main(args, env):
    plan_path = Path(env["RUNNER_TEMP"]) / ("setup-forgeds-plan-" + env.get("GITHUB_ACTION", "local") + ".json")
    if args == ["plan"]:
        plan = make_plan(env)
        plan_path.parent.mkdir(parents=True, exist_ok=True)
        plan_path.write_text(json.dumps(plan), encoding="utf-8")
        outputs({key: value for key, value in plan.items() if key != "tools"}, env)
        outputs({"plan-path": str(plan_path)}, env)
    elif args == ["install"]:
        # Each composite invocation has its own plan, including when used twice in a job.
        plan_path = Path(env["PLAN_PATH"])
        plan = json.loads(plan_path.read_text())
        result = install(plan, env)
        outputs(result, env)
        with Path(env["GITHUB_PATH"]).open("a", encoding="utf-8") as output:
            output.write(result["bin-dir"] + "\n")
    else:
        raise ValueError("Usage: setup-forgeds.py plan | install")


if __name__ == "__main__":
    try:
        main(sys.argv[1:], os.environ)
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"setup-forgeds: {error}", file=sys.stderr)
        sys.exit(1)
