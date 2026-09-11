//! Shared Forge executable bootstrap.

use forge::args::run as run_forge;

#[global_allocator]
static ALLOC: foundry_cli::utils::Allocator = foundry_cli::utils::new_allocator();

pub fn run() {
    if let Err(err) = run_forge() {
        let _ = foundry_common::sh_err!("{err:?}");
        std::process::exit(1);
    }
}
