//! Command-line entry point for the mesh shell.

use std::process::ExitCode;

fn main() -> ExitCode {
    // `build.rs` derives this from the checkout; `mesh-core` has no way to read
    // it for itself, which is what keeps the version a commit changes out of the
    // crate a commit would then have to recompile.
    mesh_core::run(env!("MESH_VERSION"))
}
