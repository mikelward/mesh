//! The read / tokenize / dispatch loop.
//!
//! M0 reads whole lines from stdin (so `echo ls | mesh` and an interactive
//! terminal both work) and has no line editor yet — reedline arrives in M1. The
//! prompt is written to stderr so it never contaminates captured stdout.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::ExitCode;

use crate::builtins::{self, Builtin};
use crate::{exec, lexer};

/// Run the shell until end-of-input or `exit`, returning the last status as the
/// process exit code.
pub fn run() -> ExitCode {
    let interactive = io::stdin().is_terminal();
    let mut input = io::stdin().lock();
    let mut last: u8 = 0;
    let mut line = String::new();

    loop {
        if interactive {
            prompt(last);
        }
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(err) => {
                eprintln!("mesh: read error: {err}");
                return ExitCode::from(1);
            }
        }

        let words = lexer::split(&line);
        if words.is_empty() {
            continue;
        }

        match builtins::dispatch(&words) {
            Some(Builtin::Exit(code)) => return ExitCode::from(code),
            Some(Builtin::Status(code)) => last = code,
            None => last = exec::run(&words),
        }
    }

    if interactive {
        // Terminate the line the final prompt started when the user hits Ctrl-D.
        eprintln!();
    }
    ExitCode::from(last)
}

/// A minimal two-glyph prompt: `mesh$` after success, `mesh!` after failure.
/// The full status-dashboard prompt from `DESIGN.md` is a later milestone.
fn prompt(last: u8) {
    let glyph = if last == 0 { "mesh$ " } else { "mesh! " };
    eprint!("{glyph}");
    let _ = io::stderr().flush();
}
