//! M0 builtins.
//!
//! M0 ships a single builtin, `exit`, which must live inside the shell process
//! because it ends the loop. `cd` is deferred to a later milestone — it needs
//! the logical-cwd / `CDPATH` / `$env.PWD` handling from `DESIGN.md` — so until
//! then every command mesh runs is external.

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
        "exit" => Some(exit(&words[1..])),
        _ => None,
    }
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
