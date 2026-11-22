#!/usr/bin/env python3
"""Publish a tested action without making consumers download the Foundry source tree."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


FILES = {
    "action.yml": "action.yml",
    "SETUP_FORGEDS.md": "README.md",
    "LICENSE-MIT": "LICENSE-MIT",
    "LICENSE-APACHE": "LICENSE-APACHE",
    ".github/forgeds-toolchain.json": ".github/forgeds-toolchain.json",
    ".github/scripts/setup-forgeds.py": ".github/scripts/setup-forgeds.py",
}


def output(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def package_tree(source, env):
    with tempfile.TemporaryDirectory(dir=env["RUNNER_TEMP"], prefix="setup-forgeds-index-") as directory:
        index_env = {**env, "GIT_INDEX_FILE": str(Path(directory) / "index")}
        subprocess.run(["git", "read-tree", "--empty"], env=index_env, check=True)
        for original, destination in FILES.items():
            # Release checkouts already have the action guide as README.md.
            if original == "SETUP_FORGEDS.md" and not Path(original).is_file():
                original = "README.md"
            blob = output("git", "rev-parse", f"{source}:{original}")
            subprocess.run(["git", "update-index", "--add", "--cacheinfo", f"100644,{blob},{destination}"],
                           env=index_env, check=True)
        return output("git", "write-tree", env=index_env)


def remote_tag(tag):
    result = output("git", "ls-remote", "origin", "refs/tags/" + tag)
    return result.split()[0] if result else None


def highest_remote_version(major_num):
    """Return the highest published (major, minor, patch) tuple for this major series, if any."""
    result = output("git", "ls-remote", "--tags", "origin", f"refs/tags/setup-forgeds-v{major_num}.*")
    pattern = re.compile(rf"^refs/tags/setup-forgeds-v({major_num})\.(\d+)\.(\d+)$")
    versions = []
    for line in result.splitlines():
        parts = line.split()
        if len(parts) < 2:
            continue
        match = pattern.match(parts[1])
        if match:
            versions.append(tuple(int(group) for group in match.groups()))
    return max(versions, default=None)


def publish(env):
    version = env.get("RELEASE_VERSION", "").strip() or env.get("GITHUB_REF_NAME", "").removeprefix("setup-forgeds-release-v")
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("Release version must be an exact version such as 1.0.1")
    version_tuple = tuple(int(part) for part in version.split("."))
    source = output("git", "rev-parse", "HEAD")
    tree = package_tree(source, env)
    tag = "setup-forgeds-v" + version
    major = "setup-forgeds-v" + version.split(".")[0]
    previous = remote_tag(tag)
    if previous:
        subprocess.run(["git", "fetch", "--no-tags", "origin", "refs/tags/" + tag], check=True)
        if output("git", "rev-parse", "FETCH_HEAD^{tree}") != tree:
            raise ValueError(f"{tag} already exists with different action contents; release a new version")
        commit = output("git", "rev-parse", "FETCH_HEAD^{commit}")
    else:
        commit = output("git", "commit-tree", tree, "-m", f"release: setup-forgeds v{version}\n\nSource: {source}")
        subprocess.run(["git", "push", "origin", f"{commit}:refs/tags/{tag}"], check=True)
    repository = env["GH_REPO"]
    existing = subprocess.run(["gh", "release", "view", tag, "--repo", repository], capture_output=True)
    if existing.returncode:
        notes = (
            f"Install verified prebuilt forgeds and optional Solar with `{repository}@{tag}`. "
            "Exact executable caching avoids downloads on warm runners; shared Solidity compiler caching is optional. "
            "The release contains only the action, installer, pinned checksums, documentation and licenses.\n\n"
            f"Source: https://github.com/{repository}/commit/{source}\n"
        )
        notes_path = Path(env["RUNNER_TEMP"]) / "setup-forgeds-release-notes.md"
        notes_path.write_text(notes, encoding="utf-8")
        subprocess.run(["gh", "release", "create", tag, "--repo", repository, "--verify-tag",
                        "--target", commit, "--title", "Setup forgeds and Solar v" + version,
                        "--notes-file", str(notes_path), "--latest=false"], check=True)
    # Update the moving major tag only when this release is at least as new as any existing tag
    # in the same major series, preventing backports or reruns from downgrading consumers.
    highest = highest_remote_version(version_tuple[0])
    updated_major = highest is None or version_tuple >= highest
    if updated_major:
        old_major = remote_tag(major)
        subprocess.run(["git", "push", "--force-with-lease=refs/tags/" + major + ":" + (old_major or ""),
                        "origin", f"{commit}:refs/tags/{major}"], check=True)
    print(json.dumps({"tag": tag, "major": major, "updated_major": updated_major, "commit": commit, "source": source,
                      "url": f"https://github.com/{repository}/releases/tag/{tag}"}))
    if env.get("GITHUB_STEP_SUMMARY"):
        major_status = f"updated `{major}`" if updated_major else f"left `{major}` on a newer release"
        with Path(env["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as summary:
            summary.write(f"Published `{repository}@{tag}` and {major_status}.\n\n"
                          f"[List this version in Marketplace](https://github.com/{repository}/releases/edit/{tag}) "
                          "by selecting **Publish this Action to the GitHub Marketplace**.\n")


if __name__ == "__main__":
    publish(os.environ)
