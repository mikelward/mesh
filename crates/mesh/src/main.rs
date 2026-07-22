//! Command-line entry point for the mesh shell.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--mesh-background-redirect") {
        return mesh_core::run_background_redirect(args.collect());
    }
    mesh_core::run()
}
