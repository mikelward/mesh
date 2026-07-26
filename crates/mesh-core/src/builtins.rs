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

use crate::vars::Value;

pub(crate) const NAMES: &[&str] = &[
    "cd",
    "pwd",
    "puts",
    "print",
    "clip",
    "notify",
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
        "print" => "print [ARG ...]",
        "clip" => "clip [TEXT ...]",
        "notify" => "notify [TEXT ...]",
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
        "puts" => Some(Builtin::Status(puts(&words[1..], true))),
        "print" => Some(Builtin::Status(puts(&words[1..], false))),
        "clip" => Some(Builtin::Status(clip(&words[1..]))),
        "notify" => Some(Builtin::Status(notify(&words[1..]))),
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

/// May a styled value emit its attributes on **this process's** stdout?
///
/// Two conditions, from `DESIGN.md` §"Hooks and the prompt": stdout is a
/// color-capable terminal, and `NO_COLOR` is unset. That is the whole capability
/// story — there is no `$color` setting and no probe, because the attributes are
/// data and dropping them is always available.
///
/// `NO_COLOR` follows the [no-color.org](https://no-color.org) rule: **set at all**
/// disables color, whatever its value, so `NO_COLOR=0` still means no color. An
/// empty value is the documented exception and does not count as set.
///
/// `TERM=dumb` is out because it declares a terminal that renders no attributes —
/// the one name for which SGR is text rather than styling. Otherwise no allowlist:
/// SGR is universal in a way `OSC` is not, so the [`OSC_TERMS`](crate::repl)
/// reasoning does not carry over.
///
/// The caller decides where "this command's stdout" actually goes — a redirect or a
/// pipe replaces it after the words are rendered — so this answers only for the
/// descriptor as it stands.
pub(crate) fn colors_wanted() -> bool {
    use std::io::IsTerminal;

    if env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return false;
    }
    if env::var("TERM").is_ok_and(|term| term == "dumb") {
        return false;
    }
    std::io::stdout().is_terminal()
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

/// `puts [ARG ...]` and `print [ARG ...]` — write the arguments separated by
/// single spaces; `puts` appends a newline (no args → a blank line) and `print`
/// does not (no args → nothing at all).
///
/// The arguments arrive already rendered from values by
/// [`rendered_for_output`](rendered_for_output), joined into a single word, so
/// the space-joining here is only for the argv path — a piped or forked stage
/// that hands the builtin plain text.
fn puts(args: &[String], newline: bool) -> u8 {
    let name = if newline { "puts" } else { "print" };
    let mut line = args.join(" ").into_bytes();
    if newline {
        line.push(b'\n');
    }
    if line.is_empty() {
        return 0;
    }
    write_stdout(name, &line)
}

/// The text `puts` writes for one argument, per `DESIGN.md` §"I/O".
///
/// One order-preserving rule: a scalar renders as itself, a **list** as its
/// elements joined by newlines (a list *is* a sequence of lines), a **map** as
/// `key: value` entries joined by newlines. A value with no canonical byte form —
/// a stream or job handle, a function, a pattern — is a **loud error** rather than
/// a guessed rendering, exactly as at the argv boundary.
///
/// This is why `puts` takes its arguments as values rather than as words: the
/// argv boundary refuses a list outright, since an external command needs bytes
/// and there is no canonical separator to pick. `puts` is a builtin looking at a
/// real value, so it can answer — and newline is the answer a list has.
///
/// `decorate` says whether a **styled** value may emit its attributes. This is the
/// one place they are read, so it is also the one place the color-capability
/// decision applies — see [`colors_wanted`](colors_wanted). Every element of a
/// collection is asked the same question, so a list of styled values keeps each
/// element's own color.
pub(crate) fn rendered_for_output(value: &Value, decorate: bool) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Styled(styled) if decorate => Ok(format!(
            "{}{}{}",
            styled.style.prefix(),
            styled.text,
            styled.style.suffix()
        )),
        Value::Styled(styled) => Ok(styled.text.clone()),
        Value::Integer(number) => Ok(number.to_string()),
        Value::Boolean(flag) => Ok(flag.to_string()),
        Value::List(items) => {
            let mut lines = Vec::with_capacity(items.len());
            for item in items {
                lines.push(match item {
                    // A list of lines cannot nest: the rendering would have to
                    // invent a second separator, and `DESIGN.md` refuses the guess
                    // here as it does at every other boundary.
                    Value::List(_) => return Err("a list inside a list has no rendering".into()),
                    Value::Map(_) => return Err("a map inside a list has no rendering".into()),
                    scalar => rendered_for_output(scalar, decorate)?,
                });
            }
            Ok(lines.join("\n"))
        }
        Value::Map(entries) => {
            let mut lines = Vec::with_capacity(entries.len());
            for (key, entry) in entries {
                let rendered = match entry {
                    Value::List(_) => return Err("a list inside a map has no rendering".into()),
                    Value::Map(_) => return Err("a map inside a map has no rendering".into()),
                    scalar => rendered_for_output(scalar, decorate)?,
                };
                lines.push(format!("{key}: {rendered}"));
            }
            Ok(lines.join("\n"))
        }
        Value::Regex(_) | Value::Glob(_) => {
            Err("a pattern has no text form; match with it instead".into())
        }
        Value::Stream(_) => Err("a stream handle has no text form".into()),
        Value::Job(_) => Err("a job handle has no text form; ask it for a member".into()),
        Value::Function(_) => Err("a function value has no text form; call it".into()),
    }
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
    let text = match read_argument_text(args, "clip") {
        Ok(text) => text,
        Err(status) => return status,
    };
    let encoded = base64(&text);
    if encoded.len() > CLIP_LIMIT {
        note!(
            "mesh: clip: {} bytes is more than the {CLIP_LIMIT}-byte limit terminals accept",
            encoded.len()
        );
        return 1;
    }
    write_terminal("clip", &format!("\x1b]52;c;{encoded}\x1b\\"))
}

/// Text safe to carry inside an `OSC` sequence: control characters replaced by
/// spaces, and no longer than `limit` characters — the ellipsis of a cut included,
/// so a caller reserving room for a suffix keeps all of it.
///
/// The stripping is not tidiness. Every payload mesh sends holds text it did not
/// choose — a command line, a directory name, a message from a script — and a
/// filename may contain an `ESC`: `touch $'\e]0;x\a'` in a directory the user then
/// `cd`s into would otherwise close mesh's sequence early and start one of its own,
/// with the rest of the payload as its argument. A control character cannot draw
/// anything in a title bar or a notification, so replacing it costs nothing and
/// ends the question. Spaces rather than deletion, so a pasted two-line command
/// does not read as one joined word.
///
/// The limit belongs to the caller: a title bar and a notification have very
/// different room, and the sequence that is cut should be the one deciding.
pub(crate) fn osc_payload(text: &str, limit: usize) -> String {
    let safe = || {
        text.chars().map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
    };
    if text.chars().count() <= limit {
        return safe().collect();
    }
    // The ellipsis counts against the limit, so the result really is no longer
    // than asked. It reading as one character too long is how a caller reserving
    // room for a suffix lost the last character of it.
    let mut payload: String = safe().take(limit.saturating_sub(1)).collect();
    // Trim first: a cut that lands mid-word reads better without the space before
    // the ellipsis.
    payload = payload.trim_end().to_owned();
    payload.push('…');
    payload
}

/// The terminal multiplexer between mesh and the terminal, if any.
///
/// Read from the environment rather than from `$env.TERM`, which cannot answer it:
/// tmux is commonly configured to set `TERM=screen-256color`, so the terminal name
/// tells you a multiplexer is there without telling you which. `$TMUX` and `$STY`
/// are set by the program that is actually running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Multiplexer {
    None,
    Tmux,
    Screen,
}

/// Which multiplexer this session is inside, held for the session.
pub(crate) fn multiplexer() -> Multiplexer {
    static INSIDE: std::sync::OnceLock<Multiplexer> = std::sync::OnceLock::new();
    *INSIDE.get_or_init(|| {
        if env::var_os("TMUX").is_some() {
            Multiplexer::Tmux
        } else if env::var_os("STY").is_some() {
            Multiplexer::Screen
        } else {
            Multiplexer::None
        }
    })
}

/// `sequence`, wrapped so it survives the multiplexer in between.
///
/// A multiplexer parses the stream itself, so a sequence it does not implement is
/// consumed rather than forwarded — which is why an unwrapped `OSC 9` never reaches
/// the outer terminal from inside tmux, `allow-passthrough` or not. Raised in
/// review on #242, where the code sent the raw form and the comment claimed
/// passthrough would carry it.
///
/// tmux forwards a `DCS tmux;` envelope with every `ESC` in the payload doubled,
/// when `allow-passthrough` is on; with it off the whole envelope is discarded,
/// which is the same silence as before and no worse.
///
/// Screen is left alone: its passthrough has a payload limit and quirks mesh has
/// no way to test against here, and a wrong envelope *prints*, which is the failure
/// the allowlist exists to avoid. `TODO.md` carries it.
pub(crate) fn through_multiplexer(sequence: &str, inside: Multiplexer) -> String {
    match inside {
        Multiplexer::Tmux => format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b")),
        Multiplexer::None | Multiplexer::Screen => sequence.to_owned(),
    }
}

/// Write a control sequence to the terminal itself, returning a builtin status.
///
/// `/dev/tty` rather than stdout, because these sequences are messages to the
/// terminal and not program output: `clip x > file` would otherwise put escape
/// bytes in the file and nothing on the clipboard, and either builtin inside a
/// pipeline would corrupt the stream. It is also what lets a *script* reach the
/// terminal, which is the point of having these as builtins rather than
/// hand-emitted escapes.
fn write_terminal(label: &str, sequence: &str) -> u8 {
    match OpenOptions::new().write(true).open("/dev/tty") {
        Ok(mut terminal) => match terminal.write_all(sequence.as_bytes()) {
            Ok(()) => 0,
            Err(err) => {
                note!("mesh: {label}: {err}");
                1
            }
        },
        Err(err) => {
            note!("mesh: {label}: no terminal to reach: {err}");
            1
        }
    }
}

/// How much of a notification mesh will send. Generous next to a title, since a
/// notification is read once and has room for a line or two, but still bounded:
/// the text can be a command line, and there is no reason to hand a notification
/// daemon an unbounded one.
pub(crate) const NOTIFY_LIMIT: usize = 256;

/// `notify [TEXT …]` — raise a desktop notification through the terminal with
/// `OSC 9`, per `DESIGN.md` "terminal control". Arguments join with a space, as
/// `puts` does; with none, stdin is read, so `puts done | notify` works.
///
/// Support is uneven and unreportable: iTerm2, WezTerm, Ghostty, kitty and ConEmu
/// raise these, while xterm and Alacritty parse the sequence and discard it, and
/// tmux swallows it without `allow-passthrough`. There is no reply, so a
/// successful write means "asked", exactly as for [`clip`].
fn notify(args: &[String]) -> u8 {
    let text = match read_argument_text(args, "notify") {
        Ok(text) => text,
        Err(status) => return status,
    };
    let message = osc_payload(&String::from_utf8_lossy(&text), NOTIFY_LIMIT);
    if message.trim().is_empty() {
        note!("mesh: notify: nothing to say");
        return 1;
    }
    let sequence = through_multiplexer(&format!("\x1b]9;{message}\x07"), multiplexer());
    write_terminal("notify", &sequence)
}

/// The text a builtin was given: its arguments joined with a space, or all of
/// stdin when there are none. Shared by `clip` and `notify` so `x | clip` and
/// `x | notify` read the same way.
fn read_argument_text(args: &[String], label: &str) -> Result<Vec<u8>, u8> {
    if !args.is_empty() {
        return Ok(args.join(" ").into_bytes());
    }
    let mut buffer = Vec::new();
    match std::io::Read::read_to_end(&mut std::io::stdin(), &mut buffer) {
        Ok(_) => Ok(buffer),
        Err(err) => {
            note!("mesh: {label}: {err}");
            Err(1)
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
    use super::{Value, base64, help, is_builtin, path_line, rename_note, rendered_for_output};
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

    #[test]
    fn print_is_a_builtin_with_help() {
        assert!(is_builtin("print"));
        assert_eq!(
            help("print").as_deref(),
            Some("Usage: print [ARG ...]\n\nOptions:\n  --help  Print help\n")
        );
    }

    #[test]
    fn a_scalar_renders_as_itself() {
        assert_eq!(
            rendered_for_output(&Value::String("hi".into()), false).as_deref(),
            Ok("hi")
        );
        assert_eq!(
            rendered_for_output(&Value::Integer(-7), false).as_deref(),
            Ok("-7")
        );
        assert_eq!(
            rendered_for_output(&Value::Boolean(true), false).as_deref(),
            Ok("true")
        );
    }

    #[test]
    fn a_collection_renders_one_entry_per_line() {
        let list = Value::List(vec![Value::String("a".into()), Value::Integer(2)]);
        assert_eq!(rendered_for_output(&list, false).as_deref(), Ok("a\n2"));
        let map = Value::Map(vec![
            ("k".to_owned(), Value::String("v".into())),
            ("n".to_owned(), Value::Boolean(false)),
        ]);
        assert_eq!(
            rendered_for_output(&map, false).as_deref(),
            Ok("k: v\nn: false")
        );
        // Empty is empty, not a stray separator.
        assert_eq!(
            rendered_for_output(&Value::List(vec![]), false).as_deref(),
            Ok("")
        );
        assert_eq!(
            rendered_for_output(&Value::Map(vec![]), false).as_deref(),
            Ok("")
        );
    }

    #[test]
    fn a_value_with_no_byte_form_is_a_loud_error() {
        // Naming the type beats guessing a rendering, the same answer the argv
        // boundary gives.
        for value in [
            Value::Glob("*.rs".into()),
            Value::Stream(1),
            Value::Job(1),
            Value::List(vec![Value::List(vec![])]),
            Value::Map(vec![("k".to_owned(), Value::Map(vec![]))]),
        ] {
            assert!(rendered_for_output(&value, false).is_err(), "{value:?}");
        }
    }
}
