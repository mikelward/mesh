//! The read / tokenize / dispatch loop.
//!
//! M0 reads whole lines from stdin (so `echo ls | mesh` and an interactive
//! terminal both work) and has no line editor yet — reedline arrives in M1. The
//! prompt is written to stderr so it never contaminates captured stdout.

use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::process::ExitCode;

use crate::builtins::{self, Builtin};
use crate::{exec, lexer};

/// Run the shell until end-of-input or `exit`, returning the last status as the
/// process exit code.
pub fn run() -> ExitCode {
    let interactive = io::stdin().is_terminal();
    // Read commands straight from file descriptor 0, unbuffered. A buffered
    // reader would pull bytes past the command's newline into its own buffer;
    // those bytes belong to any child that inherits stdin (e.g. `cat` reading a
    // here-doc piped after its command line), so buffering them would starve the
    // child and then mis-run the leftovers as commands. `ManuallyDrop` keeps us
    // from closing fd 0 when the shell exits.
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let mut last: u8 = 0;
    let mut line = Vec::new();

    loop {
        if interactive {
            prompt(last);
        }
        line.clear();
        match read_line(&mut *stdin, &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(err) => {
                eprintln!("mesh: read error: {err}");
                return ExitCode::from(1);
            }
        }

        let text = String::from_utf8_lossy(&line);
        let words = lexer::split(&text);
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

/// Read one line (up to and including the newline) into `out`, one byte at a
/// time so nothing beyond the newline is consumed — bytes a spawned child should
/// read on stdin stay in the pipe or file. Returns the number of bytes read; 0
/// signals EOF. `out` is cleared by the caller before each call.
fn read_line(reader: &mut impl Read, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => break, // EOF
            Ok(_) => {
                out.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(out.len())
}

/// A minimal two-glyph prompt: `mesh$` after success, `mesh!` after failure.
/// The full status-dashboard prompt from `DESIGN.md` is a later milestone.
fn prompt(last: u8) {
    let glyph = if last == 0 { "mesh$ " } else { "mesh! " };
    eprint!("{glyph}");
    let _ = io::stderr().flush();
}
