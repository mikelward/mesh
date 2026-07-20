//! External command execution.
//!
//! M0 launches the command by name, lets the child inherit the shell's stdio,
//! waits, and maps the result to an exit status. No pipes, redirection, job
//! control, or `PATH`-list handling yet — `std::process::Command` resolves the
//! name against the inherited `PATH`.

use std::io::ErrorKind;
use std::process::{Command, ExitStatus};

/// Run `words[0]` with `words[1..]` as arguments and return its exit status.
///
/// `words` is guaranteed non-empty by the caller. Status conventions follow
/// POSIX shells: `127` for a command that could not be found, `126` for one
/// that could not be executed, and `128 + signal` when the child is killed by a
/// signal. These line up with the result/status model in `DESIGN.md`.
pub fn run(words: &[String]) -> u8 {
    match Command::new(&words[0]).args(&words[1..]).status() {
        Ok(status) => status_to_code(status),
        Err(err) => {
            let name = &words[0];
            match err.kind() {
                ErrorKind::NotFound => {
                    eprintln!("mesh: command not found: {name}");
                    127
                }
                ErrorKind::PermissionDenied => {
                    eprintln!("mesh: permission denied: {name}");
                    126
                }
                _ => {
                    eprintln!("mesh: {name}: {err}");
                    126
                }
            }
        }
    }
}

/// Map an `ExitStatus` to a shell exit code (`128 + signal` when signaled).
fn status_to_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128u8.wrapping_add(signal as u8);
        }
    }
    1
}
