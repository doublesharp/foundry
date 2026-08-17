//! Forge with isolated default artifact and project-cache paths.

mod shared;

fn main() {
    foundry_config::set_process_default_paths("forge-ds-out", "forge-ds-cache");
    shared::run();
}
