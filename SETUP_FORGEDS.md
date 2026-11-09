# Setup forgeds and Solar

Install prebuilt [forgeds](https://github.com/doublesharp/foundry/blob/feature/viair-instrumented-coverage/FORGEDS.md)
and optional [Solar](https://github.com/paradigmxyz/solar) in GitHub Actions.
The action downloads verified binaries, adds them to `PATH`, and reuses an exact binary cache.
It needs no Rust toolchain, source compilation, application launcher, or package installation.

```yaml
- uses: actions/checkout@v4
- uses: doublesharp/foundry@setup-forgeds-v1
- run: forgeds test
```

Use `@setup-forgeds-v1.0.1` or the release commit SHA to pin the action itself. The moving
`@setup-forgeds-v1` tag follows compatible releases. Action release refs contain only the installer,
metadata, pinned checksums, documentation and licenses, so the runner avoids downloading the full Foundry source tree.

## Solar and existing Forge commands

```yaml
- uses: actions/checkout@v4
- uses: doublesharp/foundry@setup-forgeds-v1
  id: tools
  with:
    solar-version: v0.3.0
    forge-alias: 'true'
    cache-compilers: 'true'
- run: forge test --use solar
```

`forge-alias` adds a native executable alias in the action's own directory. It takes precedence
over an existing `forge` through `PATH`; the existing installation stays available at its original
path. Installing Solar leaves compiler selection to your commands or `foundry.toml`.
[Solar 0.3.0 code generation is experimental](https://github.com/paradigmxyz/solar/releases/tag/v0.3.0);
use your production compiler for deployment builds.

## Inputs

| Input | Default | Behavior |
| --- | --- | --- |
| `forgeds-version` | Bundled commit `4ca15fcba25aa61a1b436d28b428e30b459f3d56` | Full 40-character commit SHA with published binaries. `latest` explicitly resolves the rolling build before cache lookup. |
| `solar-version` | Empty | Exact release such as `v0.3.0`; empty skips Solar. |
| `forge-alias` | `false` | Also expose forgeds as `forge`. |
| `cache` | `true` | Restore/save executables with an exact key covering platform, pins and alias selection. |
| `cache-compilers` | `false` | Restore/save `~/.foundry/cache/compilers` for jobs that compile Solidity. |
| `cache-prefix` | `default` | Compiler cache namespace; use separate values for independent job families. |

The default forgeds build and Solar `v0.3.0` use checksums and full commit IDs bundled with the
action. Other exact versions use their release's SHA-256 sidecars. Every downloaded executable
must report the requested version or commit before installation. There is no source-build fallback.

## Caching and speed

Default version selection makes no network requests. Cold installs stream archives directly to
runner temporary storage and calculate checksums in the same pass. When both tools are requested,
their downloads run concurrently. Warm installs verify the installed versions and download zero
archives. The binary cache key stays the same when application source changes.

Compiler caching is optional because its transfer and storage cost depends on the application.
When enabled, each application commit can save new results and restore results from the same
toolchain and namespace. Forgeds checks the compiler binary, full compiler input and arguments
before reusing an entry. GitHub cache branch-access rules still apply; caches are scoped to each
consuming repository. Keep compiler caches separate from deployment artifacts.

The action uses pinned `actions/cache` and Python 3.9 or newer from the runner. It supports Linux
x86_64/ARM64, Apple Silicon macOS, and Windows x86_64. Intel macOS lacks a published forgeds binary.
No GitHub token is needed for public binary downloads.

## Outputs

`bin-dir`, `forgeds-path`, `forge-path`, and `solar-path` are absolute paths. Disabled optional
tools have empty paths. `forgeds-commit` and `solar-commit` report the full verified commit IDs.
`cache-hit` reports an exact executable cache restore; `downloads` counts binary archives
downloaded by this invocation and is `0` on a warm install.

## Releasing

Commit changes to the action, bundled pins and tests, then push an action source-release tag:

```sh
git tag setup-forgeds-release-v1.0.2
git push origin setup-forgeds-release-v1.0.2
```

The `Setup forgeds action` workflow runs unit checks and real cold/warm installs on Linux x86_64,
Linux ARM64, Apple Silicon macOS and Windows. It exercises the native alias, Solar compilation
and shared compiler reuse. After all jobs pass, it packages only the action files, publishes
`setup-forgeds-v1.0.2`, creates its GitHub release, and updates `setup-forgeds-v1`. Published
version tags stay fixed. The source commit is linked in the release notes.

The workflow also supports manual dispatch with a `version` input; empty validates without
publishing. Ordinary pushes touching action files run validation only. Binary releases keep
their existing independent `forgeds-*` workflow.

The workflow publishes GitHub releases and moving major tags automatically. To list a new
version in Marketplace, select **Publish this Action to the GitHub Marketplace** in that
release's editor. GitHub's release API does not expose this checkbox.
