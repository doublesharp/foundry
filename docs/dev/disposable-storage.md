# Disposable build storage

Cargo can store this checkout's build artifacts and test temporary files in a
directory of your choice, while worktrees share one sccache. Paths are local settings, not a
repository-wide absolute path. Setup requires Python 3.11 or newer and a system
that supports directory symlinks and Unix sockets, such as macOS or Linux.

Create a dedicated directory on your disposable volume, then configure it once:

```sh
python3 .github/scripts/scratch.py configure /path/to/disposable/foundry-checkout \
  --sccache-dir /path/to/shared/sccache
```

Install `sccache` first. Choose a different disposable directory for each checkout
and the same existing shared cache directory for all worktrees. On this project's
macOS development hosts, use `/Volumes/Scratch/foundry/<worktree>` for disposable
files and `/Volumes/Scratch/sccache` for the shared cache. Scratch must be mounted.
The script requires both directories to exist, refuses to replace an ordinary `target` directory or unrelated symlink,
and saves `.cargo/scratch.local.toml`. This file is ignored by Git. Changing the
root later uses the same setup command; old artifacts are retained at the old root.

Continue using normal commands without additional flags or environment variables:

```sh
cargo build
cargo test
make build
```

Cargo's optional configuration include loads the saved settings automatically when
invoked from this checkout or one of its subdirectories. A checkout without local
settings retains Cargo's defaults. Explicit Cargo command-line or environment
configuration can still override the target directory. Cargo's configured `[env]`
values apply to its child processes, including compilers and tests.

The configured checkout directory contains `target`, `tmp`, and `logs`.
An ignored `target` symlink preserves scripts using `target/...`. The separate
shared cache directory holds sccache entries, its socket, and its log. Worktrees
connect to the same cache server; their Cargo target directories remain independent.
A short cache path is required by Unix socket path-length limits. Reconfiguring
an older setup leaves its previous cache and build artifacts intact.

With `--sccache-dir`, Cargo uses sccache and disables incremental compilation by
default. An explicit `CARGO_INCREMENTAL=1` keeps incremental compilation, but those
crates bypass sccache. Explicit compiler wrappers still take precedence through
`RUSTC_WRAPPER`; preserve any required wrapper chain. Omitting `--sccache-dir`
leaves compiler caching to the host's existing configuration.

Reuse requires compatible toolchains, targets, features, flags, and inputs. Unchanged
dependencies can be reused across worktrees; crates with checkout-dependent paths
can still miss. Linking is not cached. Inspect the configured server before and
after a real build, without clearing the shared cache:

```sh
python3 .github/scripts/scratch.py run sccache --show-stats
cargo build
python3 .github/scripts/scratch.py run sccache --show-stats
```

## CI caches

The `forge-ds` release workflow uses `Swatinem/rust-cache` to persist compiled Rust
dependencies per target. Tag builds dispatch onto the same branch so subsequent
builds can restore its caches. GitHub Actions caches are scoped by branch; they do
not share this machine's sccache. Other inherited CI jobs also use sccache.

The test workflow separately saves Foundry's compiler-output cache under
`~/.foundry/cache`. Stable target, profile, suite, filter, and partition identities
let matching PR and branch jobs restore the same cache within GitHub's permitted
branch scope. Compiler outputs remain validated against their compilation inputs.

To check the storage setup and prove Rust dependency reuse in two temporary
checkouts, run this with a temporary directory on your disposable volume:

```sh
TMPDIR=/path/to/disposable/foundry-checkout/tmp \
  FOUNDRY_TEST_SCCACHE=1 python3 -m unittest discover \
  -s .github/scripts/tests -p test_scratch.py
```

This check uses its own temporary cache and stops its own server before cleanup.

## Other commands and cleanup

For commands outside Cargo, an optional helper uses the same saved settings:

```sh
python3 .github/scripts/scratch.py run python3 my-script.py
python3 .github/scripts/scratch.py run sh
```

The second command opens a shell where temporary files and builds use the saved
root. Ordinary log redirections still use the destination you specify. Shared
Cargo downloads, Foundry caches, credentials, installed binaries, source, and Git
history stay in their existing locations.

Stop builds and tests before deleting checkout-owned disposable contents. Never
delete the shared sccache when retiring a worktree. Keep the root directory,
then rerun the configure command to recreate its subdirectories and target symlink.
An absent root causes setup and the helper to fail instead of recreating it. Plain
Cargo uses standard filesystem behavior; keep the disposable volume mounted.

To return to Cargo's defaults, remove `.cargo/scratch.local.toml` and unlink the
`target` symlink. Neither step deletes the external artifacts.

Configure backup and snapshot exclusions for the selected directory or volume
separately. This helper does not change backup policies.

See [Cargo configuration](https://doc.rust-lang.org/cargo/reference/config.html)
and [sccache usage](https://github.com/mozilla/sccache#usage) for the underlying
configuration rules.
