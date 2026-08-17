//! The `forge` CLI: build, test, fuzz, debug and deploy Solidity contracts.

#![cfg_attr(
    target_os = "macos",
    allow(
        linker_messages,
        reason = "Apple ld cannot encode Forge's large unwind table in its compact format"
    )
)]

mod shared;

fn main() {
    shared::run();
}
