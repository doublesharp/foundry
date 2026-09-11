//! Forge with isolated default artifact and project-cache paths.

#![cfg_attr(
    target_os = "macos",
    allow(
        linker_messages,
        reason = "Apple ld cannot encode Forge's large unwind table in its compact format"
    )
)]

mod shared;

fn main() {
    foundry_config::set_process_default_paths("forge-ds-out", "forge-ds-cache");
    shared::run();
}
