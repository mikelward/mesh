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

/// `cd [DIR]` — change directory; no argument means `$HOME`.
///
/// M0 does not yet implement `cd -`, `CDPATH`, or the autocd behavior from
/// `DESIGN.md`; those come with the language layer.
fn cd(args: &[String]) -> u8 {
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
    let path = Path::new(&target);
    if let Err(err) = env::set_current_dir(path) {
        eprintln!("mesh: cd: {}: {err}", path.display());
        return 1;
    }
    0
}

/// `exit [N]` — leave the shell with status `N` (default 0). A non-numeric
/// argument is an error but still exits, matching the conventional shell.
fn exit(args: &[String]) -> Builtin {
    match args.first() {
        None => Builtin::Exit(0),
        Some(arg) => match arg.parse::<u8>() {
            Ok(code) => Builtin::Exit(code),
            Err(_) => {
                eprintln!("mesh: exit: {arg}: numeric argument required");
                Builtin::Exit(2)
            }
        },
    }
}
