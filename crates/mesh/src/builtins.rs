//! M0 builtins.
//!
//! Only the two builtins that *must* live inside the shell process because they
//! mutate its own state: `cd` (changes the process's working directory) and
//! `exit` (ends the loop). Everything else in M0 is an external command.

use std::env;
use std::ffi::OsString;
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
        "exit" => Some(exit(&words[1..])),
        _ => None,
    }
}

/// `cd [DIR]` — change directory; no argument means `$HOME`. Updates `$PWD` and
/// `$OLDPWD` on success, as `DESIGN.md` requires, so child processes that read
/// `$PWD` see the new directory.
///
/// M0 does not yet implement `cd -`, `CDPATH`, the `--physical` flag, or a
/// shell-maintained *logical* cwd; `$PWD` is set to the physical path from
/// `getcwd`. Those refinements come with the language layer.
fn cd(args: &[String]) -> u8 {
    if args.len() > 1 {
        eprintln!("mesh: cd: too many arguments");
        return 1;
    }
    // Keep the target as an `OsString` so a non-UTF-8 `$HOME` (or, later, a
    // non-UTF-8 argument) reaches the OS unchanged rather than being mangled by
    // lossy UTF-8 conversion.
    let target: OsString = match args.first() {
        Some(dir) => dir.into(),
        None => match env::var_os("HOME") {
            Some(home) => home,
            None => {
                eprintln!("mesh: cd: HOME not set");
                return 1;
            }
        },
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
            env::set_var("PWD", current);
        }
    }
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
