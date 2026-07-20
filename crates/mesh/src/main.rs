//! mesh — an interactive-first Unix shell.
//!
//! This is the **M0** build track: the smallest thing that reads a line, splits
//! it into words, and launches the external command it names (plus the `cd` and
//! `exit` builtins). None of the mesh *language* from `DESIGN.md` is implemented
//! yet — see `ROADMAP.md` for the path from here.

mod builtins;
mod exec;
mod expand;
mod lexer;
mod repl;
mod vars;

use std::process::ExitCode;

fn main() -> ExitCode {
    repl::run()
}
