# forgeds

`forgeds` is doublesharp's build of Foundry's `forge`, tuned for local development with many
worktrees and AI coding agents. It accepts the same commands and `foundry.toml` settings as `forge`.
This page covers what is different.

## Install

Prebuilt binaries are on the [`forgeds` release][release]. These links always serve the newest
build:

| Platform | Download |
| --- | --- |
| macOS (Apple Silicon) | [`forgeds_darwin_arm64.tar.gz`][darwin-arm64] |
| Linux x86_64 | [`forgeds_linux_amd64.tar.gz`][linux-amd64] |
| Linux arm64 | [`forgeds_linux_arm64.tar.gz`][linux-arm64] |
| Windows x86_64 | [`forgeds_win32_amd64.tar.gz`][win32-amd64] |

Each archive has a `.sha256` file next to it, and `forgeds_latest.txt` holds the commit the links
currently serve. To pin a version, use the commit-named assets, such as
`forgeds-4ca15fc_darwin_arm64.tar.gz`.

```sh
gh release download forgeds -R doublesharp/foundry -p 'forgeds_darwin_arm64.tar.gz*'
shasum -a 256 -c forgeds_darwin_arm64.tar.gz.sha256
tar -xzf forgeds_darwin_arm64.tar.gz -C ~/.cargo/bin forgeds
forgeds --version   # prints the full Commit SHA
```

On other platforms, build from source. Each build has a `forgeds-<commit>` tag named after the
first seven characters of its commit:

```sh
cargo install --locked --git https://github.com/doublesharp/foundry \
  --tag forgeds-4ca15fc forge --bin forgeds --features forgeds
```

For the fastest development builds, also install [Solar](https://github.com/paradigmxyz/solar)
with `cargo install solar-compiler --locked`.

## Separate output folders

`forgeds` writes artifacts to `forgeds-out/` and its project cache to `forgeds-cache/`, even when
`foundry.toml` sets `out` or `cache_path`. You can run `forge` and `forgeds` in the same project
without either one invalidating the other's builds. `FOUNDRY_OUT`, `FOUNDRY_CACHE_PATH`, `--out`,
and `--cache-path` still override these folders. Add both folders to `.gitignore`.

## Shared compiler cache across worktrees

`forgeds` stores complete compiler results in `~/.foundry/cache/compilers` and reuses them in any
checkout that sends the compiler the same input. A new worktree of an unchanged project skips
compilation. On two `via_ir` projects, a fresh worktree build dropped from 115 s to 0.7 s and
from 364 s to 1.7 s, with byte-identical artifacts.

The cache key covers the compiler binary, the full standard-JSON input, and the compiler
arguments, so a hit is the same output the compiler would produce. It works with solc and Solar
and needs no configuration. `--force` skips it.

To see whether a build used the cache, run it with
`RUST_LOG=foundry_compilers::compiler_cache=trace` and look for `hit`, `miss`, `published`, or
`bypass` together with its reason. The cache can also use a running sccache server as a second
tier; see [Solidity compiler caching](docs/dev/solidity-compiler-cache.md).

`~/.foundry/cache/compilers` can grow large. It can be a symlink to a disk that isn't backed up.

## Instrumented coverage

`forgeds coverage --instrumented` measures coverage by adding probes to your sources before
compiling them, instead of mapping bytecode back to source. It keeps your optimizer and `via_ir`
settings, so it works on projects where `forge coverage` needs `--ir-minimum` or fails with
"stack too deep". It reports lines, statements, branches (including `?:`, `&&`, and `||`), and
functions, including code inside inline assembly.

- Rerunning it with unchanged sources reuses the compiled build and takes about half a second. After any
  source edit it recompiles the whole project.
- Compiler warnings point at your original source, not the instrumented copy.
- Skip code with `// coverage ignore next` or `// coverage ignore file`. The `forge coverage`,
  `istanbul`, and `solidity-coverage` forms of these comments also work.
- The probes change bytecode and gas usage, so use this build only to measure coverage.
- `--report bytecode` is not supported in this mode.

Add `--report gaps` for a compact JSON list of uncovered items.

## Quieter compiler output

Set `compiler_warnings = "changed"` in `foundry.toml`, or `FOUNDRY_COMPILER_WARNINGS=changed`, to
print warnings only for files compiled in the current run. A single line counts the warnings
hidden in unchanged files. Errors, and warnings that `deny` would turn into failures, are always
shown. The default, `"all"`, behaves like `forge`.

When imports can't be resolved, `forgeds` lists each missing file once, along with only the
remappings that could apply to it.

## Suggested agent setup

```toml
[profile.default]
compiler_warnings = "changed"
ignored_warnings_from = ["lib"]
```

- Keep `forgeds-out/` and `forgeds-cache/` out of Git, and leave the shared compiler cache
  enabled. Avoid `--force` and clean builds in routine work.
- During edits, run focused tests with Solar where the project allows it:
  `forgeds test --use solar --mt <test>`.
- Measure coverage on the final change with the project's own compiler:
  `forgeds coverage --instrumented --report summary --report gaps`.

[release]: https://github.com/doublesharp/foundry/releases/tag/forgeds
[darwin-arm64]: https://github.com/doublesharp/foundry/releases/download/forgeds/forgeds_darwin_arm64.tar.gz
[linux-amd64]: https://github.com/doublesharp/foundry/releases/download/forgeds/forgeds_linux_amd64.tar.gz
[linux-arm64]: https://github.com/doublesharp/foundry/releases/download/forgeds/forgeds_linux_arm64.tar.gz
[win32-amd64]: https://github.com/doublesharp/foundry/releases/download/forgeds/forgeds_win32_amd64.tar.gz
