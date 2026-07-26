//! Builtins.
//!
//! The commands that must run inside the shell process because they read or
//! mutate its own state. Session-aware builtins such as `prompt` are dispatched
//! by the REPL; the stateless builtins live here. Everything else is external.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) const NAMES: &[&str] = &[
    "cd",
    "pwd",
    "puts",
    "exit",
    "fg",
    "bg",
    "jobs",
    "wait",
    "disown",
    "kill",
    "prompt",
    "prompt-hook",
    "source",
];

/// Outcome of a builtin. `Status` reports an exit status and continues the loop;
/// `Exit` ends the shell with the given status.
pub enum Builtin {
    Status(u8),
    Exit(u8),
}

/// Does `name` name a builtin? Used to route one to the in-shell path — run
/// directly for a plain command, or in a forked stage inside a pipeline.
pub fn is_builtin(name: &str) -> bool {
    NAMES.contains(&name)
}

/// Print the canned command-line help for a builtin.
pub fn print_help(name: &str) -> u8 {
    let Some(help) = help(name) else {
        return 1;
    };
    write_stdout(name, help.as_bytes())
}

/// Return the same help text printed by `NAME --help`.
pub(crate) fn help(name: &str) -> Option<String> {
    let usage = match name {
        "cd" => "cd [DIR]",
        "pwd" => "pwd",
        "puts" => "puts [ARG ...]",
        "exit" => "exit [N]",
        "fg" | "bg" => return Some(format_help(&format!("{name} [JOB]"))),
        "jobs" => "jobs",
        "wait" => "wait [JOB …]",
        "disown" => "disown [-h] [-a | -r] [JOB …]",
        "kill" => "kill [-SIGNAL] JOB|PID ...",
        "prompt" => "prompt [--reset | TEXT]",
        "prompt-hook" => "prompt-hook [--remove] [EVENT] NAME [FUNCTION]",
        "source" => "source FILE",
        _ => return None,
    };
    Some(format_help(usage))
}

/// Write generated help through the builtin-safe stdout path.
pub fn print_generated_help(name: &str, help: &str) -> u8 {
    write_stdout(name, help.as_bytes())
}

fn format_help(usage: &str) -> String {
    format!("Usage: {usage}\n\nOptions:\n  --help  Print help\n")
}

/// If `words[0]` names a builtin, run it and return its outcome; otherwise
/// return `None` so the caller falls through to external execution.
///
/// `words` is guaranteed non-empty by the caller. `last` is the status of the
/// previous command, used as the default for a bare `exit`.
pub fn dispatch(words: &[String], last: u8) -> Option<Builtin> {
    match words[0].as_str() {
        "cd" => Some(Builtin::Status(cd(&words[1..]))),
        "pwd" => Some(Builtin::Status(pwd(&words[1..]))),
        "puts" => Some(Builtin::Status(puts(&words[1..]))),
        "exit" => Some(exit(&words[1..], last)),
        _ => None,
    }
}

/// `cd [DIR]` — change directory. No argument → `$HOME`; `cd -` → `$OLDPWD`
/// (and prints the destination, as POSIX does). Updates `$PWD` and `$OLDPWD` on
/// success so child processes that read them see the new directory.
///
/// Not yet implemented (deferred to the language layer): `CDPATH`, `--physical`,
/// autocd, and a shell-maintained *logical* cwd — `$PWD` is the physical
/// `getcwd` path for now.
fn cd(args: &[String]) -> u8 {
    if args.len() > 1 {
        note!("mesh: cd: too many arguments");
        return 1;
    }
    // Keep targets as `OsString` so a non-UTF-8 `$HOME`/`$OLDPWD` reaches the OS
    // unchanged rather than being mangled by lossy UTF-8 conversion.
    let mut echo_destination = false;
    let target: OsString = match args.first().map(String::as_str) {
        None => match env::var_os("HOME") {
            Some(home) => home,
            None => {
                note!("mesh: cd: HOME not set");
                return 1;
            }
        },
        Some("-") => match env::var_os("OLDPWD") {
            Some(old) => {
                echo_destination = true; // `cd -` prints where it landed
                old
            }
            None => {
                note!("mesh: cd: OLDPWD not set");
                return 1;
            }
        },
        Some(dir) => dir.into(),
    };

    let previous = env::current_dir().ok();
    let path = Path::new(&target);
    if let Err(err) = env::set_current_dir(path) {
        note!("mesh: cd: {}: {err}", path.display());
        return 1;
    }

    let mut status = 0;
    // SAFETY: the shell runs this loop single-threaded, so mutating the
    // environment here races with nothing.
    unsafe {
        if let Some(previous) = previous {
            env::set_var("OLDPWD", previous);
        }
        if let Ok(current) = env::current_dir() {
            env::set_var("PWD", &current);
            if echo_destination {
                status = write_stdout("cd", &path_line(current.as_os_str()));
            }
        }
    }
    status
}

/// Print `line` and a newline on stdout as a builtin does: `0` on success, `1`
/// on a write error. Use this instead of `println!` anywhere a builtin's output
/// can be redirected — `println!` panics on a failed write, which would abort the
/// whole shell over `prompt >/dev/full`.
pub(crate) fn print_line(label: &str, line: &str) -> u8 {
    let mut bytes = line.as_bytes().to_vec();
    bytes.push(b'\n');
    write_stdout(label, &bytes)
}

/// The bytes to print for a path: its raw `OsStr` bytes plus a newline, so a
/// non-UTF-8 path is emitted exactly rather than lossily via `Display`.
fn path_line(path: &OsStr) -> Vec<u8> {
    let mut line = path.as_bytes().to_vec();
    line.push(b'\n');
    line
}

/// Write `bytes` to stdout, returning a builtin status: `0` on success, `1` on
/// error. An ordinary I/O failure (a full disk, a closed pipe) must report a
/// failure, never crash the REPL — so this never panics the way `println!` does.
/// A broken pipe is silent (the reader went away), the way a shell takes SIGPIPE.
pub(crate) fn write_stdout(label: &str, bytes: &[u8]) -> u8 {
    match std::io::stdout().write_all(bytes) {
        Ok(()) => 0,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => 1,
        Err(err) => {
            note!("mesh: {label}: {err}");
            1
        }
    }
}

/// `pwd` — print the current working directory (physical `getcwd`).
///
/// M0-level: no `-L`/`-P` flags and no logical-cwd tracking yet.
fn pwd(args: &[String]) -> u8 {
    if !args.is_empty() {
        note!("mesh: pwd: too many arguments");
        return 1;
    }
    match env::current_dir() {
        Ok(dir) => write_stdout("pwd", &path_line(dir.as_os_str())),
        Err(err) => {
            note!("mesh: pwd: {err}");
            1
        }
    }
}

/// `puts [ARG ...]` — write the arguments separated by single spaces, followed
/// by a newline (no args → a blank line). The basic string form; list/value
/// formatting arrives with the value system.
fn puts(args: &[String]) -> u8 {
    let mut line = args.join(" ").into_bytes();
    line.push(b'\n');
    write_stdout("puts", &line)
}

/// `exit [N]` — leave the shell with status `N`. With no argument it exits with
/// the **last command's status** (`last`), the POSIX convention (`false; exit`
/// leaves 1), not a bare 0. The status is an 8-bit process status, so an
/// out-of-range `N` is masked to `0`–`255` (`exit 256` → `0`, `exit -1` → `255`),
/// matching `DESIGN.md` and conventional shells. A non-numeric argument is an
/// error but still exits; a surplus operand is a likely typo, so the shell
/// reports it and keeps running rather than exiting on it.
fn exit(args: &[String], last: u8) -> Builtin {
    if args.len() > 1 {
        note!("mesh: exit: too many arguments");
        return Builtin::Status(1);
    }
    match args.first() {
        None => Builtin::Exit(last),
        Some(arg) => match arg.parse::<i64>() {
            Ok(code) => Builtin::Exit(code.rem_euclid(256) as u8),
            Err(_) => {
                note!("mesh: exit: {arg}: numeric argument required");
                Builtin::Exit(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{is_builtin, path_line};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn path_line_preserves_non_utf8_bytes() {
        // A 0xff byte must survive verbatim, not become U+FFFD.
        assert_eq!(path_line(OsStr::from_bytes(b"/x\xffy")), b"/x\xffy\n");
    }

    #[test]
    fn recognizes_job_builtins() {
        assert!(is_builtin("jobs"));
        assert!(is_builtin("fg"));
        assert!(is_builtin("bg"));
    }
}
