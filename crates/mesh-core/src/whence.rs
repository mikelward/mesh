//! `type` — say what a name is.
//!
//! The lookup every shell has under a different spelling: bash's `type`, ksh's
//! `whence`, the external `which`. It answers the interactive question "what
//! happens when I type this word" — and, because mesh keeps bindings in their own
//! namespace, "what is bound to this name" alongside it.
//!
//! **Bash's name, flags and words.** `-t` and `-P` are bash's, and are the shapes
//! a script *compares* rather than reads, so they match it byte for byte; the
//! prose is mesh's own, since a sentence is read. `whence` is ksh's spelling and
//! `where` zsh's; both, and `what`, are in `builtins::RENAMED` and point here.
//! **`which` is deliberately absent** — in bash it is an external program that
//! cannot see builtins or functions, and mesh keeps that rather than shadowing a
//! binary.
//!
//! The file is still `whence.rs` because `type` is a Rust keyword and `r#type.rs`
//! reads worse than the history it would tidy.
//!
//! **Names, not values.** The value-side question — what a value *is*, written
//! back as the source you would have typed — is the `:repr` modifier, which
//! already answers it. This asks only about the **name**: what a bare word would
//! run, and what is bound to it.
//!
//! Its own module rather than a function in `builtins`, because it is the one
//! builtin that reads *every* namespace the shell has — the builtin and syntax
//! tables, the function store, the variable scopes, the environment, and `PATH` —
//! and so is dispatched by the REPL with the session in hand.

use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::builtins;
use crate::environ;
use crate::funcs::{FuncDef, Funcs};
use crate::parser::ParamKind;
use crate::vars::{Value, Vars};

/// How wide a bound value's literal may be before the detail line drops it and
/// reports only the shape.
///
/// The repr is **exact or absent, never shortened**: `:repr`'s contract is that
/// what it writes reads back as an equal value, and an elided one would read back
/// as something else — or not at all. A reader who wants a long value in full has
/// `puts` right there.
const REPR_BUDGET: usize = 60;

/// One thing a name turned out to be.
enum Finding {
    /// A shape the parser knows, carrying the form `help` shows for it and
    /// whether a bare one is **taken in command position**.
    ///
    /// That flag does more work than it looks. `help`'s table documents three
    /// different things under one roof: words the parser always claims (`if`,
    /// `for`), words it claims only *contextually* (`fork` before a block,
    /// `unless` after a statement, `and` between values), and punctuation
    /// (`+`, `$(`), one row per symbol a line can carry. Only the first
    /// group answers "what runs when I type this" — a contextual word is an
    /// ordinary command word, and a legal function name besides.
    Syntax { form: &'static str, keyword: bool },
    /// A builtin, carrying its usage line.
    Builtin(&'static str),
    /// A defined function, carrying its reconstructed signature.
    Function(String),
    /// An executable found on `PATH`.
    External(PathBuf),
    /// A path operand — a word with a `/`, which command resolution reads as a
    /// file rather than a name to look up.
    File(FileKind),
    /// A variable binding, and whether it is the function-local one.
    Variable { local: bool, detail: String },
    /// An environment entry — a namespace of its own in mesh, so a name can be
    /// both this and a variable.
    Environment { detail: String },
}

enum FileKind {
    Executable,
    Plain,
    Directory,
    /// Not a regular file — a fifo, socket, or device. Named rather than folded
    /// into `Plain`, because "a file, but not executable" would be a lie about
    /// what it is, and the execute bit these can carry is exactly what made the
    /// unchecked version call them runnable.
    Special(&'static str),
}

impl Finding {
    /// Does this finding mean the name is **usable** — a command that would run,
    /// or a name the language has taken?
    ///
    /// Separate from merely having been found, and the gap is everything worth
    /// *describing* but not worth reporting success for. A path that exists but
    /// could not be run (`./notes.txt`, `/etc`, an execute-bit fifo) is a `126`
    /// on execution, and a syntax row the parser does not claim in command
    /// position (an operator, and every contextual word) is a `127` — neither
    /// is something `whence --quiet` may answer yes to, since the status is
    /// exactly the "can I use this" question.
    fn usable(&self) -> bool {
        match self {
            Finding::Syntax { keyword, .. } => *keyword,
            Finding::File(FileKind::Executable) => true,
            Finding::File(_) => false,
            _ => true,
        }
    }
}

/// What a name was found to be, split by whether it is in the race.
///
/// `commands` is in **resolution order**, so its first entry is what a bare
/// `NAME` would run and the rest are what it shadows. `separate` is everything
/// found that is not competing for that position, and so is always reported in
/// full rather than abridged:
///
/// - a **variable**, reached through `$NAME`, and an **environment entry**,
///   through `$env.NAME` — namespaces of their own, so a name can be a command
///   and either of these at once, with neither shadowing the other;
/// - the **file** a path operand names, which is the only command answer a word
///   with a `/` in it has, so nothing competes with it;
/// - a **contextual** syntax word (`fork`, `unless`, `and`), which is worth
///   describing but is not what a bare one runs — the parser only claims it when
///   something follows to claim it.
#[derive(Default)]
struct Found {
    commands: Vec<Finding>,
    separate: Vec<Finding>,
}

impl Found {
    fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.separate.is_empty()
    }

    /// Did the name **resolve** — is any of what was found actually usable?
    ///
    /// Not the same question as [`is_empty`](Found::is_empty): plenty is worth
    /// describing without being usable, and [`Finding::usable`] draws that line
    /// once for every kind rather than per caller.
    fn resolved(&self) -> bool {
        self.commands
            .iter()
            .chain(&self.separate)
            .any(Finding::usable)
    }
}

/// `whence [--all|--quiet] NAME …` — report what each name is.
///
/// Status `0` when every name [resolved](Found::resolved), `1` when any did not,
/// `2` for a misuse. With `--quiet` that status is the *whole* answer — nothing
/// is printed, and neither is the not-found note — which is mesh's
/// `command -v x >/dev/null`: `if whence --quiet fzf { … }`. Left out, a test in
/// a startup file would have to redirect two streams to stay silent.
///
/// A name that was *described* but is not [usable](Finding::usable) — a path
/// operand that could not be run, a syntax row that reserves no name — still
/// prints its line and still fails, so the report says why the status is what it
/// is rather than contradicting it.
pub(crate) fn type_of(args: &[String], funcs: &Funcs, vars: &Vars) -> u8 {
    let Options {
        all,
        quiet,
        kind,
        path,
        names,
    } = match parse(args) {
        Ok(parsed) => parsed,
        Err(status) => return status,
    };
    let mut status = 0;
    let mut out = Vec::new();
    for name in names {
        let found = look_up(name, funcs, vars);
        // `-t` and `-P` answer about the **winner**, and each has its own notion of
        // failure: `-P` fails on a name that resolves perfectly well as a function,
        // so neither can lean on `resolved`.
        if kind || path {
            let answer = if path {
                found.commands.iter().find_map(external_path)
            } else {
                found.commands.first().or(found.separate.first()).map(word)
            };
            match answer {
                Some(answer) if !quiet => {
                    out.extend_from_slice(answer);
                    out.push(b'\n');
                }
                Some(_) => {}
                None => status = 1,
            }
            continue;
        }
        if !found.resolved() {
            status = 1;
        }
        if quiet {
            continue;
        }
        if found.is_empty() {
            // The same note `command not found` carries, so a `whence` / `where` /
            // `what` reflex lands on mesh's spelling from either direction.
            match builtins::rename_note(name) {
                Some(note) => note!("mesh: type: {name}: not found ({note})"),
                None => note!("mesh: type: {name}: not found"),
            }
            continue;
        }
        report(name, &found, all, &mut out);
    }
    status | builtins::write_stdout("type", &out)
}

/// The one word `-t` prints. Bash's tokens, because this output is *compared*
/// rather than read — `case "$(type -t "$1")" in function)` is what a port
/// carries over. `variable` is the one addition, since bash's `type` cannot see
/// bindings.
fn word(finding: &Finding) -> &'static [u8] {
    match finding {
        Finding::Syntax { .. } => b"keyword",
        Finding::Builtin(_) => b"builtin",
        Finding::Function(_) => b"function",
        Finding::External(_) | Finding::File(_) => b"file",
        Finding::Variable { .. } | Finding::Environment { .. } => b"variable",
    }
}

/// The path `-P` prints, for a finding that has one. A function or a builtin has
/// none, which is the whole point of the flag.
fn external_path(finding: &Finding) -> Option<&[u8]> {
    match finding {
        Finding::External(path) => Some(path.as_os_str().as_bytes()),
        _ => None,
    }
}

/// The parsed command line: which listing was asked for, and the names to
/// answer about.
struct Options<'a> {
    all: bool,
    quiet: bool,
    /// `-t` — one word per name, the shape a guard compares against.
    kind: bool,
    /// `-P` — a `PATH` hit only, ignoring functions and builtins.
    path: bool,
    names: Vec<&'a String>,
}

/// Read the options. Everything after a `--` is a name, which is how a name that
/// looks like a flag is asked about (`whence -- --all`).
fn parse(args: &[String]) -> Result<Options<'_>, u8> {
    let mut all = false;
    let mut quiet = false;
    let mut kind = false;
    let mut path = false;
    let mut names = Vec::new();
    let mut options = true;
    for arg in args {
        if options {
            match arg.as_str() {
                "-a" => {
                    all = true;
                    continue;
                }
                "--quiet" => {
                    quiet = true;
                    continue;
                }
                "-t" => {
                    kind = true;
                    continue;
                }
                "-P" => {
                    path = true;
                    continue;
                }
                "--" => {
                    options = false;
                    continue;
                }
                // A lone `-` is a name, not a malformed option: it is a real
                // enough word to ask about, and nothing reads it as a flag.
                _ if arg.starts_with('-') && arg.len() > 1 => {
                    note!("mesh: type: {arg}: unknown option (`-t`, `-P`, `-a` or `--quiet`)");
                    return Err(2);
                }
                _ => {}
            }
        }
        names.push(arg);
    }
    if names.is_empty() {
        note!("mesh: type: expected a name; `type NAME …` says what each one is");
        return Err(2);
    }
    // Not an error together: `--quiet` asks for no output at all, which is a
    // superset of what `--all` would have printed rather than a contradiction of
    // it. The status is the same either way.
    Ok(Options {
        all,
        quiet,
        kind,
        path,
        names,
    })
}

/// Everything `name` is, gathered in the order the shell itself resolves a
/// command word.
fn look_up(name: &str, funcs: &Funcs, vars: &Vars) -> Found {
    let mut found = Found::default();
    // Syntax first, and before the path rule, because a shape the parser claims is
    // settled before any lookup happens — and because two of the operators the
    // table documents (`/`, `..`) are also spellable as paths, and the answer
    // about `/` that is worth having is "division".
    //
    // But **only a command keyword joins the race.** A contextual word (`fork`,
    // `unless`, `and`) is an ordinary command word when nothing follows to claim
    // it, so putting one in `commands` made it outrank the function or executable
    // that a bare one really reaches — `whence fork` announced itself as syntax
    // shadowing a `func fork()` that is what would actually run. It still gets
    // described, from the side, because "`fork` is also the subshell keyword" is
    // worth knowing; it just is not the answer to "what runs".
    if let Some(form) = builtins::syntax_form(name) {
        let keyword = builtins::is_command_keyword(name);
        let finding = Finding::Syntax { form, keyword };
        if keyword {
            found.commands.push(finding);
        } else {
            found.separate.push(finding);
        }
    }
    // A word with a `/` in it is a **path**, not a name — that is how command
    // resolution reads it, so the lookup must answer about the same thing that
    // would run. `PATH` is not searched for it, and neither are the name tables:
    // a file called `./cd` is not the builtin, and no variable is named `./cd`.
    if name.contains('/') {
        found.separate.extend(file_finding(Path::new(name)));
        return found;
    }
    if let Some(usage) = builtins::usage(name) {
        found.commands.push(Finding::Builtin(usage));
    }
    // A function can never shadow a builtin — `func cd` is refused as a reserved
    // name — so at most one of the two above ever appears. Both are kept in the
    // list anyway rather than short-circuited, so the day that rule changes this
    // reports the truth instead of the assumption.
    if let Some(def) = funcs.get(name) {
        found.commands.push(Finding::Function(signature(name, def)));
    }
    found
        .commands
        .extend(path_hits(name).into_iter().map(Finding::External));
    if let Some(value) = vars.get(name) {
        found.separate.push(Finding::Variable {
            local: vars.is_local(name),
            detail: detail(value),
        });
    }
    if let Some(value) = environ::read(name) {
        found.separate.push(Finding::Environment {
            detail: detail(&value),
        });
    }
    found
}

/// The signature as it would be written, reconstructed from the stored
/// parameters rather than the source text — which is not kept, and which a
/// redefinition would have made stale anyway.
///
/// A default is shown as `…`: it is a parsed expression, and the text it was
/// written as is gone by the time it is stored. That the parameter *has* one is
/// the part that changes how you call it.
fn signature(name: &str, def: &FuncDef) -> String {
    let params: Vec<String> = def
        .params
        .iter()
        .map(|param| match param.kind {
            ParamKind::Required => param.name.clone(),
            ParamKind::Optional(_) => format!("{} = …", param.name),
            ParamKind::Switch => format!("--{}", param.name),
            ParamKind::Flag(_) => format!("--{} = …", param.name),
            ParamKind::Rest => format!("...{}", param.name),
        })
        .collect();
    // A wrapper is reported as it was written: how it treats a `--flag` is the
    // part of its contract a caller most needs to know.
    let lead = if def.wrapper { "wrapper func" } else { "func" };
    format!("{lead} {name}({})", params.join(", "))
}

/// Every executable `name` names on `PATH`, in `PATH` order, so the first is the
/// one that would run.
///
/// A *reimplementation* of the search `execvp` performs, since mesh hands the
/// lookup to the OS rather than doing it itself — the same trade the completion
/// tables already make. It reads the permission bits, so it can disagree with the
/// kernel where an ACL or a differing effective uid decides the answer; being
/// wrong there is cheaper than not answering at all.
///
/// Because it is that reimplementation, it has to follow `execvp` all the way
/// down: with **`PATH` unset** the search does not stop, it falls back to the
/// [system default](default_path). Reporting "not found" there would have been
/// wrong about the one thing this exists to be right about — `env -u PATH mesh`
/// still runs `sh`, so `whence sh` has to still find it.
fn path_hits(name: &str) -> Vec<PathBuf> {
    // Unset and empty are different: unset takes the default below, while a set
    // but empty `PATH` is one empty component — the current directory — which is
    // what `split_paths` already yields and what `execvp` searches.
    let path = match env::var_os("PATH") {
        Some(path) => path,
        None => default_path(),
    };
    let mut seen = HashSet::new();
    env::split_paths(&path)
        // An empty `PATH` component means the current directory, which is what
        // splitting keeps it as; `join` on it yields the bare name, which is the
        // relative path that would be run.
        .map(|directory| directory.join(name))
        .filter(|candidate| is_executable_file(candidate))
        // Deduped by the file it *resolves to*, but reported as the path `PATH`
        // spells: `/bin` is a symlink to `/usr/bin` on most Linux systems, so a
        // literal dedup would report the one `ls` twice and call the second a
        // shadowed command. The spelling kept is the first, which is the one that
        // would run.
        .filter(|candidate| {
            seen.insert(fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone()))
        })
        .collect()
}

/// The search path `execvp` uses when `PATH` is unset — the platform's own
/// answer, `confstr(_CS_PATH)`, rather than a guess: it is `/bin:/usr/bin` on
/// Linux and `/usr/bin:/bin` on macOS, and the order between them is exactly what
/// decides which `sh` a lookup reports.
///
/// Falls back to the POSIX-conventional pair if `confstr` cannot answer, which
/// leaves the search approximate rather than empty.
///
/// Shared with the completion tables rather than kept private: they scan `PATH`
/// for the same command words this reports, so a session with `PATH` unset would
/// otherwise resolve `whence sh` and fail to complete it.
pub(crate) fn default_path() -> OsString {
    // SAFETY: the two-call `confstr` idiom — size first, then a buffer of exactly
    // that size. A zero return means the name is unsupported, and the result is
    // NUL-terminated within the reported length.
    let length = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if length > 1 {
        let mut buffer = vec![0u8; length];
        let written =
            unsafe { libc::confstr(libc::_CS_PATH, buffer.as_mut_ptr().cast(), buffer.len()) };
        if written > 1 && written <= buffer.len() {
            // `written` counts the trailing NUL, which is not part of the value.
            buffer.truncate(written - 1);
            return OsString::from_vec(buffer);
        }
    }
    OsString::from("/bin:/usr/bin")
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|data| data.is_file() && data.permissions().mode() & 0o111 != 0)
}

/// What a path operand is, or `None` when there is nothing there. Follows
/// symlinks, because that is what running it would do.
///
/// **Executable means a regular file with the bit**, the same pair
/// [`is_executable_file`] requires of a `PATH` hit — the execute bit alone is not
/// enough. A fifo, socket, or device can carry it (`mkfifo p; chmod +x p`) and
/// `execve` still refuses them, so testing the bit by itself called `./p`
/// runnable when running it is a `126`.
fn file_finding(path: &Path) -> Option<Finding> {
    let data = fs::metadata(path).ok()?;
    Some(Finding::File(if data.is_dir() {
        FileKind::Directory
    } else if !data.is_file() {
        FileKind::Special(special_kind(&data.file_type()))
    } else if data.permissions().mode() & 0o111 != 0 {
        FileKind::Executable
    } else {
        FileKind::Plain
    }))
}

/// What to call a path that is neither a regular file nor a directory. Named
/// rather than lumped together, since "a named pipe" is the answer that explains
/// why it is not a command.
fn special_kind(kind: &fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;
    if kind.is_fifo() {
        "a named pipe"
    } else if kind.is_socket() {
        "a socket"
    } else if kind.is_block_device() {
        "a block device"
    } else if kind.is_char_device() {
        "a character device"
    } else {
        "not a regular file"
    }
}

/// The detail line for a bound value: what it is, and — when it fits — what it
/// holds. See [`REPR_BUDGET`] for why the literal is never shortened.
fn detail(value: &Value) -> String {
    let shape = shape(value);
    // An empty collection's shape already says everything its literal would.
    let empty = matches!(value, Value::List(items) if items.is_empty())
        || matches!(value, Value::Map(entries) if entries.is_empty());
    if empty {
        return shape;
    }
    match value.to_literal() {
        Ok(literal) if literal.chars().count() <= REPR_BUDGET => format!("{shape}: {literal}"),
        // Either too wide, or a value with no literal form at all (a job handle, a
        // lambda) — the shape is the whole answer for both.
        _ => shape,
    }
}

/// How to name a value here: the diagnostic wording, plus the size of a
/// collection, which is the thing you most want to know when the contents are
/// too wide to print.
fn shape(value: &Value) -> String {
    match value {
        Value::List(items) if items.is_empty() => "an empty list".to_owned(),
        Value::List(items) => format!("a list of {}", items.len()),
        Value::Map(entries) if entries.is_empty() => "an empty map".to_owned(),
        Value::Map(entries) => format!("a map of {}", entries.len()),
        // `value_kind` deliberately files a job under "a stream handle" — for a
        // diagnostic the shared trait (no byte form) is the point. Here the point
        // is the opposite: this is the answer to "what is bound to this name", and
        // `j = cmd &` binds a job whose members and job-reference behavior are
        // nothing like a stream's.
        Value::Job(_) => "a job handle".to_owned(),
        other => crate::repl::value_kind(other).to_owned(),
    }
}

/// Write one name's report.
///
/// Without `--all` the command half is the **winner** plus a note naming what it
/// shadows, which is the interactive question: what runs, and what it hid. With
/// `--all` every match is a line of its own, still in resolution order. The
/// separate namespaces are never abridged — there are at most two, and none of
/// them is in the race.
fn report(name: &str, found: &Found, all: bool, out: &mut Vec<u8>) {
    let shown = if all || found.commands.is_empty() {
        &found.commands[..]
    } else {
        &found.commands[..1]
    };
    for finding in shown {
        line(name, finding, out);
    }
    for finding in &found.separate {
        line(name, finding, out);
    }
}

/// One `NAME is …` headline and the indented detail under it.
///
/// Built as bytes rather than a `String` because a `PATH` entry need not be valid
/// UTF-8, and the path mesh would run has to be printed as the bytes it is —
/// the same care `pwd` takes.
fn line(name: &str, finding: &Finding, out: &mut Vec<u8>) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b" is ");
    match finding {
        Finding::Syntax { .. } => out.extend_from_slice(b"a shell keyword"),
        Finding::Builtin(_) => out.extend_from_slice(b"a shell builtin"),
        Finding::Function(_) => out.extend_from_slice(b"a function"),
        Finding::External(path) => out.extend_from_slice(path.as_os_str().as_bytes()),
        Finding::File(FileKind::Executable) => out.extend_from_slice(b"an executable file"),
        Finding::File(FileKind::Plain) => out.extend_from_slice(b"a file, but not executable"),
        Finding::File(FileKind::Directory) => out.extend_from_slice(b"a directory"),
        Finding::File(FileKind::Special(kind)) => out.extend_from_slice(kind.as_bytes()),
        Finding::Variable { local: true, .. } => out.extend_from_slice(b"a local variable"),
        Finding::Variable { local: false, .. } => out.extend_from_slice(b"a variable"),
        Finding::Environment { .. } => out.extend_from_slice(b"an environment entry"),
    }
    out.push(b'\n');
    let detail = match finding {
        Finding::Syntax { form, .. } => Some(*form),
        Finding::Builtin(usage) => Some(*usage),
        Finding::Function(signature) => Some(signature.as_str()),
        Finding::Variable { detail, .. } | Finding::Environment { detail } => Some(detail.as_str()),
        Finding::External(_) | Finding::File(_) => None,
    };
    if let Some(detail) = detail {
        out.extend_from_slice(b"    ");
        out.extend_from_slice(detail.as_bytes());
        out.push(b'\n');
    }
}

/// The name of a `PATH` entry, for a test that needs one that really exists.
#[cfg(test)]
fn first_path_entry_name() -> Option<String> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .filter_map(|directory| fs::read_dir(directory).ok())
        .flatten()
        .flatten()
        .find(|entry| is_executable_file(&entry.path()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Param;
    use std::os::unix::ffi::OsStringExt;

    fn body() -> crate::parser::Source {
        crate::parser::Source {
            statements: Vec::new(),
            span: crate::parser::Span::default(),
        }
    }

    fn rendered(name: &str, found: &Found, all: bool) -> String {
        let mut out = Vec::new();
        report(name, found, all, &mut out);
        String::from_utf8(out).expect("report is UTF-8 here")
    }

    /// A builtin that is **not** also an external on any usual system, so the
    /// table lookups can be asserted whole without a real `PATH` hit joining the
    /// report. `pwd` and `kill` would both find `/usr/bin/…` here.
    const ONLY_A_BUILTIN: &str = "prompt-hook";

    #[test]
    fn names_a_builtin_by_its_usage() {
        let found = look_up(ONLY_A_BUILTIN, &Funcs::new(), &Vars::new());
        assert_eq!(
            rendered(ONLY_A_BUILTIN, &found, false),
            "prompt-hook is a shell builtin\n    prompt-hook [--remove] [EVENT] NAME [FUNCTION]\n"
        );
    }

    #[test]
    fn names_a_keyword_by_its_form() {
        let found = look_up("unless", &Funcs::new(), &Vars::new());
        assert!(
            rendered("unless", &found, false)
                .starts_with("unless is a shell keyword\n    cmd if COND")
        );
    }

    #[test]
    fn names_a_function_by_its_signature() {
        let mut funcs = Funcs::new();
        funcs.define(
            "deploy".to_owned(),
            FuncDef {
                params: vec![
                    Param {
                        name: "target".to_owned(),
                        kind: ParamKind::Required,
                    },
                    Param {
                        name: "force".to_owned(),
                        kind: ParamKind::Switch,
                    },
                    Param {
                        name: "hosts".to_owned(),
                        kind: ParamKind::Rest,
                    },
                ],
                body: body(),
                wrapper: false,
            },
        );
        let found = look_up("deploy", &funcs, &Vars::new());
        assert_eq!(
            rendered("deploy", &found, false),
            "deploy is a function\n    func deploy(target, --force, ...hosts)\n"
        );
    }

    #[test]
    fn a_variable_and_a_command_are_separate_namespaces() {
        let mut vars = Vars::new();
        vars.set_value(ONLY_A_BUILTIN, Value::Integer(5));
        let found = look_up(ONLY_A_BUILTIN, &Funcs::new(), &vars);
        assert_eq!(
            rendered(ONLY_A_BUILTIN, &found, false),
            "prompt-hook is a shell builtin\n    prompt-hook [--remove] [EVENT] NAME [FUNCTION]\n\
             prompt-hook is a variable\n    an integer: 5\n"
        );
    }

    #[test]
    fn reports_a_collection_by_size_and_contents() {
        let mut vars = Vars::new();
        vars.set_value(
            "xs",
            Value::List(vec![
                Value::String("a".to_owned()),
                Value::String("b".to_owned()),
            ]),
        );
        let found = look_up("xs", &Funcs::new(), &vars);
        assert_eq!(
            rendered("xs", &found, false),
            "xs is a variable\n    a list of 2: ['a', 'b']\n"
        );
    }

    #[test]
    fn drops_a_literal_too_wide_to_print_rather_than_cutting_it() {
        let long = Value::String("x".repeat(REPR_BUDGET));
        // The quotes push it past the budget, so the shape stands alone — rather
        // than a repr that would read back as a shorter string.
        assert_eq!(detail(&long), "a string");
        let fits = Value::String("x".repeat(REPR_BUDGET - 2));
        assert!(detail(&fits).starts_with("a string: 'xx"));
    }

    #[test]
    fn an_empty_collection_says_so_without_a_literal() {
        assert_eq!(detail(&Value::List(Vec::new())), "an empty list");
        assert_eq!(detail(&Value::Map(Vec::new())), "an empty map");
    }

    #[test]
    fn a_value_with_no_literal_form_reports_its_shape() {
        // No `:repr` to print, so the shape is the whole detail rather than an
        // error or a guessed rendering.
        assert_eq!(detail(&Value::Stream(0)), "a stream handle");
        assert_eq!(detail(&Value::Job(1)), "a job handle");
    }

    #[test]
    fn a_builtin_shadows_an_external_of_the_same_name() {
        let mut found = Found::default();
        found.commands.push(Finding::Builtin("cd [DIR]"));
        found
            .commands
            .push(Finding::External(PathBuf::from("/usr/bin/cd")));
        assert_eq!(
            rendered("cd", &found, false),
            "cd is a shell builtin\n    cd [DIR]\n"
        );
        assert_eq!(
            rendered("cd", &found, true),
            "cd is a shell builtin\n    cd [DIR]\ncd is /usr/bin/cd\n"
        );
    }

    /// `-a` lists **every** match in resolution order. The bare form names only the
    /// winner — what a name could have matched but did not is not worth a line, and
    /// this is where that information lives instead.
    #[test]
    fn all_lists_every_match_in_resolution_order() {
        let mut found = Found::default();
        found.commands.push(Finding::Function("func ls()".into()));
        found
            .commands
            .push(Finding::External(PathBuf::from("/usr/local/bin/ls")));
        found
            .commands
            .push(Finding::External(PathBuf::from("/usr/bin/ls")));
        assert_eq!(
            rendered("ls", &found, false),
            "ls is a function\n    func ls()\n"
        );
        assert_eq!(
            rendered("ls", &found, true),
            "ls is a function\n    func ls()\nls is /usr/local/bin/ls\nls is /usr/bin/ls\n"
        );
    }

    #[test]
    fn a_non_utf8_path_prints_as_its_own_bytes() {
        let mut found = Found::default();
        found.commands.push(Finding::External(PathBuf::from(
            std::ffi::OsString::from_vec(b"/bin/\xffx".to_vec()),
        )));
        let mut out = Vec::new();
        report("x", &found, false, &mut out);
        assert_eq!(out, b"x is /bin/\xffx\n");
    }

    #[test]
    fn a_word_with_a_slash_is_a_path_not_a_name() {
        let mut vars = Vars::new();
        vars.set_value("pwd", Value::Integer(5));
        // `./pwd` is neither the builtin nor the variable, whatever is bound.
        let found = look_up("./pwd", &Funcs::new(), &vars);
        assert!(found.is_empty());
    }

    #[test]
    fn a_path_operand_that_cannot_run_is_described_but_does_not_resolve() {
        // Worth a line — that is why it is a finding — but running it is the
        // `126` that `--quiet` must not answer yes to.
        for kind in [FileKind::Plain, FileKind::Directory] {
            let found = Found {
                commands: Vec::new(),
                separate: vec![Finding::File(kind)],
            };
            assert!(!found.is_empty());
            assert!(!found.resolved());
        }
        let runnable = Found {
            commands: Vec::new(),
            separate: vec![Finding::File(FileKind::Executable)],
        };
        assert!(runnable.resolved());
    }

    #[test]
    fn a_binding_resolves_even_though_it_is_not_a_command() {
        let mut vars = Vars::new();
        vars.set_value("xs", Value::Integer(1));
        // The question is "does the shell know this name", and it does.
        assert!(look_up("xs", &Funcs::new(), &vars).resolved());
    }

    #[test]
    fn the_default_search_path_is_the_platforms_own() {
        // `execvp` searches this when `PATH` is unset, so the lookup has to too —
        // otherwise `whence` reports "not found" for a command that would run.
        let default = default_path();
        assert!(!default.is_empty());
        let entries: Vec<_> = env::split_paths(&default).collect();
        assert!(
            entries
                .iter()
                .any(|entry| entry == Path::new("/bin") || entry == Path::new("/usr/bin")),
            "{default:?} holds neither /bin nor /usr/bin"
        );
    }

    #[test]
    fn reports_a_directory_operand() {
        let found = look_up("/", &Funcs::new(), &Vars::new());
        // `/` is division first — the parser settles that before any lookup — and
        // the directory it also names comes after. The two are not competing for
        // the same position, so neither shadows the other.
        assert_eq!(
            rendered("/", &found, false),
            "/ is a shell keyword\n    n = (1 + 2)\n/ is a directory\n"
        );
        // Described twice over and usable as neither: `/ 1 2` is a `127` and
        // running the directory a `126`.
        assert!(!found.resolved());
    }

    #[test]
    fn only_a_shape_the_parser_claims_bare_resolves() {
        // Everything `help` documents is worth describing; only what the parser
        // claims in command position is worth reporting success for. Three groups
        // do *not* qualify, and the first version of this treated all three as
        // resolved:
        //
        // - punctuation, which names nothing;
        // - contextual words, claimed only by what follows them — a bare one is an
        //   ordinary command word, so `command not found` with nothing defined;
        // - the value constructors, refused as function names but still a `127`
        //   when typed as a command.
        for described in [
            "+", "$(", // punctuation
            "fork", "unless", "else", "and", "or", "in", // contextual
            "re", "style", "glob", // value constructors
        ] {
            let found = look_up(described, &Funcs::new(), &Vars::new());
            assert!(!found.is_empty(), "{described} lost its description");
            assert!(!found.resolved(), "{described} must not resolve");
        }
        for keyword in ["if", "func", "for", "while", "match", "return"] {
            assert!(
                look_up(keyword, &Funcs::new(), &Vars::new()).resolved(),
                "{keyword} is claimed in command position"
            );
        }
    }

    #[test]
    fn a_contextual_keyword_does_not_outrank_a_function() {
        let mut funcs = Funcs::new();
        funcs.define(
            "fork".to_owned(),
            FuncDef {
                params: Vec::new(),
                body: body(),
                wrapper: false,
            },
        );
        let found = look_up("fork", &funcs, &Vars::new());
        // The function is what a bare `fork` runs, so it is the only thing in the
        // race; the keyword is described from the side and shadows nothing.
        assert_eq!(
            rendered("fork", &found, false),
            "fork is a function\n    func fork()\nfork is a shell keyword\n    fork { … }\n"
        );
        assert!(found.resolved());
    }

    #[test]
    fn a_job_handle_is_not_reported_as_a_stream() {
        // `value_kind` files both under "a stream handle", which is right for a
        // diagnostic about byte forms and wrong here: `j = cmd &` binds a job, and
        // that is the answer to what the name holds.
        assert_eq!(shape(&Value::Job(1)), "a job handle");
        assert_eq!(shape(&Value::Stream(0)), "a stream handle");
    }

    #[test]
    fn an_execute_bit_on_a_special_file_is_not_executable() {
        // `execve` refuses a fifo whatever its mode, so the bit alone must not
        // make one look runnable — the same regular-file test `PATH` hits get.
        let dir = std::env::temp_dir().join(format!("mesh_whence_fifo_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let fifo = dir.join("p");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("path");
        // SAFETY: an ordinary `mkfifo(3)` call on a path this test owns.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o755) }, 0);
        let Some(Finding::File(kind)) = file_finding(&fifo) else {
            panic!("the fifo was not found");
        };
        assert!(matches!(kind, FileKind::Special("a named pipe")));
        assert!(!Finding::File(kind).usable());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_an_executable_on_path() {
        let Some(name) = first_path_entry_name() else {
            return;
        };
        let found = look_up(&name, &Funcs::new(), &Vars::new());
        assert!(
            found
                .commands
                .iter()
                .any(|finding| matches!(finding, Finding::External(_))),
            "{name} is on PATH but was not found there"
        );
    }

    #[test]
    fn reads_the_environment_as_its_own_namespace() {
        // SAFETY: single-threaded test, and the name is this test's own.
        unsafe { env::set_var("MESH_WHENCE_TEST", "hi") };
        let found = look_up("MESH_WHENCE_TEST", &Funcs::new(), &Vars::new());
        assert_eq!(
            rendered("MESH_WHENCE_TEST", &found, false),
            "MESH_WHENCE_TEST is an environment entry\n    a string: 'hi'\n"
        );
        unsafe { env::remove_var("MESH_WHENCE_TEST") };
    }

    #[test]
    fn rejects_an_unknown_option_and_a_missing_name() {
        assert_eq!(parse(&["--nope".to_owned()]).err(), Some(2));
        assert_eq!(parse(&[]).err(), Some(2));
    }

    #[test]
    fn reads_both_listing_options() {
        let args = ["-a".to_owned(), "--quiet".to_owned(), "ls".to_owned()];
        let options = parse(&args).expect("parses");
        assert!(options.all && options.quiet);
        assert_eq!(options.names, [&"ls".to_owned()]);
    }

    #[test]
    fn a_terminator_makes_a_flag_looking_word_a_name() {
        let args = ["--".to_owned(), "-a".to_owned()];
        let options = parse(&args).expect("parses");
        assert!(!options.all);
        assert_eq!(options.names, [&"-a".to_owned()]);
    }

    #[test]
    fn a_lone_dash_is_a_name() {
        let args = ["-".to_owned()];
        let options = parse(&args).expect("parses");
        assert_eq!(options.names, [&"-".to_owned()]);
    }
}
