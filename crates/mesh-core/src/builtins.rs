//! Builtins.
//!
//! The commands that must run inside the shell process because they read or
//! mutate its own state. Session-aware builtins such as `prompt` are dispatched
//! by the REPL; the stateless builtins live here. Everything else is external.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) const NAMES: &[&str] = &[
    "cd",
    "pwd",
    "puts",
    "clip",
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

/// Bash builtins mesh renames, mapped to the mesh builtin that replaces them.
/// Kept as a *name* rather than prose so `is_builtin` can say whether the
/// replacement is wired up yet, and the caveat retires itself when it lands.
///
/// `echo` is here for the stripped-`PATH` case only: an external `echo` almost
/// always shadows this, which is deliberate — `echo -n` / `-e` keep working
/// through `/bin/echo`, where a mesh builtin would print the flag as text.
const RENAMED: &[(&str, &str)] = &[("echo", "puts"), ("read", "gets")];

const LOCAL: &str = "a plain `x = 5` inside a `func` is already local";
const NO_ALIASES: &str = "mesh has no aliases; a `func` replaces `alias ll`";
/// `declare` and `typeset` span all three scopes — `-g` asks for a global and
/// `-x` for an environment entry — and the note sees only the command name, so
/// it offers the set rather than assuming the bare, local-by-default case.
const SCOPE: &str = "scope is `x = 5` (local), `global x = 5`, or `export X = value`";

/// Bash builtins whose mesh answer is not a command at all, so the note has to
/// spell out the replacement instead of naming it.
///
/// Everything named here has to *work* when the reader types it: a note that
/// lands on a second error is worse than the bare message it replaced. That is
/// what keeps `shopt` / `setopt` out — their answer is `$sh.options.NAME = on`,
/// which is still unbuilt (`docs/REFERENCE.md`), and unlike the `RENAMED` names
/// there is nothing here for `is_builtin` to check, so a hand-written "not built
/// yet" would go stale silently. Add them with `$sh.options`.
const REPLACED: &[(&str, &str)] = &[
    ("alias", NO_ALIASES),
    ("declare", SCOPE),
    ("function", "functions are `func name(params) { … }`"),
    ("let", "arithmetic is `n = (1 + 2)`"),
    ("local", LOCAL),
    ("typeset", SCOPE),
    ("unalias", NO_ALIASES),
];

/// What `command not found` adds for a name bash spells differently, so a bash
/// reflex lands on mesh's spelling instead of a dead end. `None` for every
/// other name — a typo gets the plain message.
pub(crate) fn rename_note(name: &str) -> Option<String> {
    if let Some((_, mesh)) = RENAMED.iter().find(|(bash, _)| *bash == name) {
        return Some(if is_builtin(mesh) {
            format!("mesh spells this `{mesh}`")
        } else {
            format!("mesh spells this `{mesh}`, which is not built yet")
        });
    }
    REPLACED
        .iter()
        .find(|(bash, _)| *bash == name)
        .map(|(_, note)| (*note).to_string())
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
        "clip" => "clip [TEXT ...]",
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
        "clip" => Some(Builtin::Status(clip(&words[1..]))),
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

/// How much base64 `clip` will send. Terminals bound the sequence they will
/// accept and drop anything longer without saying so; xterm's limit is the
/// smallest of the common ones, and a refusal that names the size beats a
/// clipboard that silently did not change.
const CLIP_LIMIT: usize = 74_994;

/// `clip [TEXT …]` — copy to the terminal's clipboard with `OSC 52`, per
/// `DESIGN.md` "terminal control". With no arguments it reads stdin, so
/// `puts hi | clip` and `clip hi` both work. Arguments join with a space, as
/// `puts` does, and neither form adds a trailing newline: a clipboard holds what
/// was asked for, and a stray newline shows up when it is pasted into a shell.
///
/// The sequence goes to `/dev/tty`, not stdout. It is a message to the terminal
/// rather than program output, so stdout is the wrong channel twice over:
/// `clip x > file` would put escape bytes in the file and nothing on the
/// clipboard, and `clip` inside a pipeline would corrupt the stream. Writing to
/// the terminal directly also lets a *script* copy, which is the point of having
/// this over hand-emitting the escape.
///
/// Whether the copy lands is the terminal's business: OSC 52 is widely but not
/// universally implemented — xterm wants `allowWindowOps`, tmux wants
/// `set-clipboard on` — and there is no reply to wait for, so a successful write
/// means "asked", not "copied". Reading the clipboard back is deliberately not
/// here: it needs a query and a response, so it can block on a terminal that will
/// never answer.
fn clip(args: &[String]) -> u8 {
    let text = if args.is_empty() {
        let mut buffer = Vec::new();
        if let Err(err) = std::io::Read::read_to_end(&mut std::io::stdin(), &mut buffer) {
            note!("mesh: clip: {err}");
            return 1;
        }
        buffer
    } else {
        args.join(" ").into_bytes()
    };
    let encoded = base64(&text);
    if encoded.len() > CLIP_LIMIT {
        note!(
            "mesh: clip: {} bytes is more than the {CLIP_LIMIT}-byte limit terminals accept",
            encoded.len()
        );
        return 1;
    }
    let sequence = format!("\x1b]52;c;{encoded}\x1b\\");
    match OpenOptions::new().write(true).open("/dev/tty") {
        Ok(mut terminal) => match terminal.write_all(sequence.as_bytes()) {
            Ok(()) => 0,
            Err(err) => {
                note!("mesh: clip: {err}");
                1
            }
        },
        Err(err) => {
            note!("mesh: clip: no terminal to copy through: {err}");
            1
        }
    }
}

/// Standard base64, the alphabet `OSC 52` carries its payload in.
///
/// Written out rather than taken as a dependency: it is the one encoding mesh
/// needs, and twenty lines is cheaper than a crate in the build graph.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        // Missing bytes count as zero and their characters become `=`, which is
        // what the padding means: this many sextets carry no data.
        let packed = u32::from(group[0]) << 16
            | u32::from(group.get(1).copied().unwrap_or(0)) << 8
            | u32::from(group.get(2).copied().unwrap_or(0));
        for sextet in 0..4 {
            if sextet <= group.len() {
                let index = (packed >> (18 - 6 * sextet)) & 0x3f;
                out.push(char::from(ALPHABET[index as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
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
    use super::{base64, help, is_builtin, path_line, rename_note};
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

    #[test]
    fn base64_matches_the_rfc_4648_vectors() {
        // Every padding case, from the RFC's own examples: no padding, one `=`,
        // two `=`. Getting the padding wrong is the classic way to write an
        // encoder that works on a third of its inputs.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encodes_bytes_rather_than_text() {
        // A clipboard carries whatever it was handed, so the encoder takes bytes:
        // not UTF-8, and not text at all.
        assert_eq!(base64(b"\xff\xff\xff"), "////");
        assert_eq!(base64(b"\x00\x00\x00"), "AAAA");
        assert_eq!(base64(b"\xff"), "/w==");
        assert_eq!(base64("é".as_bytes()), "w6k=");
        // Both characters that distinguish standard base64 from the URL-safe
        // alphabet, which a terminal would not accept.
        assert_eq!(base64(b"\xfb\xef\xbe"), "++++");
        assert_eq!(base64(b"\xff\xef\xbf"), "/++/");
    }

    #[test]
    fn a_renamed_bash_builtin_names_meshs_spelling() {
        assert_eq!(
            rename_note("echo").as_deref(),
            Some("mesh spells this `puts`")
        );
        // `gets` is designed but unbuilt, so the note says so rather than
        // sending the reader to a second `command not found`.
        assert_eq!(
            rename_note("read").as_deref(),
            Some("mesh spells this `gets`, which is not built yet")
        );
        assert_eq!(
            rename_note("local").as_deref(),
            Some("a plain `x = 5` inside a `func` is already local")
        );
    }

    #[test]
    fn declare_offers_every_scope_it_could_have_meant() {
        // `declare -x` exports and `declare -g` makes a global; the note sees
        // only the name, so pointing at the local case alone would misdirect
        // two of the three.
        for name in ["declare", "typeset"] {
            let note = rename_note(name).expect("a note");
            assert!(note.contains("global x = 5"), "{note}");
            assert!(note.contains("export X = value"), "{note}");
        }
    }

    #[test]
    fn a_note_is_only_offered_for_a_replacement_that_works() {
        // `$sh.options` is unbuilt, so a `shopt` note would trade one error for
        // another. It joins the table when the options API does.
        assert_eq!(rename_note("shopt"), None);
        assert_eq!(rename_note("setopt"), None);
    }

    #[test]
    fn an_ordinary_unknown_name_gets_no_note() {
        // The note is for a bash reflex, not for a typo — anything else keeps
        // the bare message.
        assert_eq!(rename_note("nosuchcmd"), None);
        assert_eq!(rename_note("puts"), None);
        assert_eq!(rename_note(""), None);
    }

    #[test]
    fn clip_is_a_builtin_with_help() {
        assert!(is_builtin("clip"));
        assert_eq!(
            help("clip").as_deref(),
            Some("Usage: clip [TEXT ...]\n\nOptions:\n  --help  Print help\n")
        );
    }
}
