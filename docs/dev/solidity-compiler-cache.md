# Solidity compiler caching

`forgeds` shares complete compiler results across worktrees through
`~/.foundry/cache/compilers`. The key includes the compiler executable's contents,
the complete standard-JSON input, and compilation arguments. Source names and
compiler settings are preserved. Identical local requests share a file lock;
unresolved external inputs bypass this cache.

## Optional sccache storage

Set `FORGEDS_SCCACHE=1` to use an already-running **sccache 0.19.0** daemon as
secondary storage for Solidity results, including results produced by Solar:

```sh
sccache --start-server
FORGEDS_SCCACHE=1 forgeds build --use /path/to/solar
```

Local hits do not contact sccache. On a local miss, `forgeds` retrieves the stored
compiler output before deciding whether to invoke the compiler. New results are
stored locally and sent to sccache. `--force` and disabled caching bypass both.
This is independent of `RUSTC_WRAPPER`, which only affects Rust builds.

The integration uses sccache's private raw-storage protocol and checks the running
daemon's exact version before using it. It currently supports loopback TCP on
`SCCACHE_SERVER_PORT` (default `4226`); Unix socket configurations bypass it.
Missing or incompatible daemons, corrupt entries, and bounded transport failures
fall back to local caching and normal compilation. Individual compressed entries
must fit sccache's 8 MiB frame limit; decoded entries are limited to 64 MiB.

For CI, configure the daemon's supported storage backend and use the same compiler
binary, inputs, and settings on each runner. Local file locking coalesces requests
on one machine; this integration does not provide distributed compilation locks.

Use `RUST_LOG=foundry_compilers::compiler_cache=trace` to distinguish
`sccache_hit`, `sccache_miss`, `sccache_write`, and fallback events. Raw-storage
operations do not increment sccache's ordinary compiler hit counters.
