//! Command-line entry point for the mesh shell.

use std::process::ExitCode;

fn main() -> ExitCode {
    mesh_core::run()
}
