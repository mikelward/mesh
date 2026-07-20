//! Builtins.
//!
//! The commands that must run inside the shell process because they read or
//! mutate its own state: `cd` (working directory), `pwd` (reports it), `puts`
//! (output), and `exit` (ends the loop). Everything else mesh runs is external.

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Outcome of a builtin. `Status` reports an exit status and continues the loop;
/// `Exit` ends the shell with the given status.
pub enum Builtin {
    Status(u8),
    Exit(u8),
}

/// If `words[0]` names a builtin, run it and return its outcome; otherwise
/// return `None` so the caller falls through to external execution.
///
/// `words` is guaranteed non-empty by the caller.
pub fn dispatch(words: &[String]) -> Option<Builtin> {
    match words[0].as_str() {
        "cd" => Some(Builtin::Status(cd(&words[1..]))),
        "pwd" => Some(Builtin::Status(pwd(&words[1..]))),
        "puts" => Some(Builtin::Status(puts(&words[1..]))),
        "exit" => Some(exit(&words[1..])),
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
        eprintln!("mesh: cd: too many arguments");
        return 1;
    }
    // Keep targets as `OsString` so a non-UTF-8 `$HOME`/`$OLDPWD` reaches the OS
    // unchanged rather than being mangled by lossy UTF-8 conversion.
    let mut echo_destination = false;
    let target: OsString = match args.first().map(String::as_str) {
        None => match env::var_os("HOME") {
            Some(home) => home,
            None => {
                eprintln!("mesh: cd: HOME not set");
                return 1;
            }
        },
        Some("-") => match env::var_os("OLDPWD") {
            Some(old) => {
                echo_destination = true; // `cd -` prints where it landed
                old
            }
            None => {
                eprintln!("mesh: cd: OLDPWD not set");
                return 1;
            }
        },
        Some(dir) => dir.into(),
    };

    let previous = env::current_dir().ok();
    let path = Path::new(&target);
    if let Err(err) = env::set_current_dir(path) {
        eprintln!("mesh: cd: {}: {err}", path.display());
        return 1;
    }

    // SAFETY: the shell runs this loop single-threaded, so mutating the
    // environment here races with nothing.
    unsafe {
        if let Some(previous) = previous {
            env::set_var("OLDPWD", previous);
        }
        if let Ok(current) = env::current_dir() {
            env::set_var("PWD", &current);
            if echo_destination {
                write_path(current.as_os_str());
            }
        }
    }
    0
}

/// The bytes to print for a path: its raw `OsStr` bytes plus a newline, so a
/// non-UTF-8 path is emitted exactly rather than lossily via `Display`.
fn path_line(path: &OsStr) -> Vec<u8> {
    let mut line = path.as_bytes().to_vec();
    line.push(b'\n');
    line
}

/// Write a path to stdout as raw bytes followed by a newline.
fn write_path(path: &OsStr) {
    let _ = std::io::stdout().write_all(&path_line(path));
}

/// `pwd` — print the current working directory (physical `getcwd`).
///
/// M0-level: no `-L`/`-P` flags and no logical-cwd tracking yet.
fn pwd(args: &[String]) -> u8 {
    if !args.is_empty() {
        eprintln!("mesh: pwd: too many arguments");
        return 1;
    }
    match env::current_dir() {
        Ok(dir) => {
            write_path(dir.as_os_str());
            0
        }
        Err(err) => {
            eprintln!("mesh: pwd: {err}");
            1
        }
    }
}

/// `puts [ARG ...]` — write the arguments separated by single spaces, followed
/// by a newline (no args → a blank line). The basic string form; list/value
/// formatting arrives with the value system.
fn puts(args: &[String]) -> u8 {
    println!("{}", args.join(" "));
    0
}

/// `exit [N]` — leave the shell with status `N` (default 0). The status is an
/// 8-bit process status, so an out-of-range `N` is masked to `0`–`255`
/// (`exit 256` → `0`, `exit -1` → `255`), matching `DESIGN.md` and conventional
/// shells. A non-numeric argument is an error but still exits; a surplus operand
/// is a likely typo, so the shell reports it and keeps running rather than
/// exiting on it.
fn exit(args: &[String]) -> Builtin {
    if args.len() > 1 {
        eprintln!("mesh: exit: too many arguments");
        return Builtin::Status(1);
    }
    match args.first() {
        None => Builtin::Exit(0),
        Some(arg) => match arg.parse::<i64>() {
            Ok(code) => Builtin::Exit(code.rem_euclid(256) as u8),
            Err(_) => {
                eprintln!("mesh: exit: {arg}: numeric argument required");
                Builtin::Exit(2)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::path_line;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn path_line_preserves_non_utf8_bytes() {
        // A 0xff byte must survive verbatim, not become U+FFFD.
        assert_eq!(path_line(OsStr::from_bytes(b"/x\xffy")), b"/x\xffy\n");
    }
}
