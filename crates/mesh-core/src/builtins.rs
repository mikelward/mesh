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

use crate::vars::{Decoration, Value};

/// Every builtin as `(usage, summary)`: the usage line its `--help` prints, and
/// the one-line description that sits beside it in `help`'s listing.
///
/// One table rather than a list of names beside a `match` of usages, so a new
/// builtin cannot arrive dispatchable but unlisted, or listed but unexplained.
/// The **name is the usage's first word** — a usage that did not start with the
/// command you type would be wrong anyway, so there is nothing to keep in step.
const TABLE: &[(&str, &str)] = &[
    ("cd [DIR]", "Change the working directory"),
    ("pwd", "Print the working directory"),
    ("puts [ARG ...]", "Render the arguments, then a newline"),
    ("print [ARG ...]", "As `puts`, with no trailing newline"),
    ("gets [VAR]", "Read one line from stdin"),
    ("clip [TEXT ...]", "Copy text to the terminal's clipboard"),
    ("notify [TEXT ...]", "Raise a desktop notification"),
    ("exit [N]", "Leave the shell"),
    ("fg [JOB]", "Resume a job in the foreground"),
    ("bg [JOB]", "Resume a stopped job in the background"),
    ("jobs", "List the jobs"),
    ("wait [JOB …]", "Wait for a job to finish"),
    ("disown [-h] [-a | -r] [JOB …]", "Stop tracking a job"),
    ("kill [-SIGNAL] JOB|PID ...", "Signal a job or a process"),
    (
        "prompt [--reset | TEXT]",
        "Set or print the interactive prompt",
    ),
    (
        "prompt-hook [--remove] [EVENT] NAME [FUNCTION]",
        "Register a function for a prompt event",
    ),
    (
        "command [--] NAME [ARG ...]",
        "Run a program, past builtins and functions",
    ),
    ("source FILE", "Run a file's commands in this shell"),
    ("help [NAME ...]", "List the builtins, or explain one"),
];

/// The name a usage line belongs to: its first word.
fn name_of(usage: &'static str) -> &'static str {
    usage.split(' ').next().unwrap_or(usage)
}

/// Does this builtin read **options of its own**, rather than taking every argument
/// as data?
///
/// Read off the usage line, like [`name_of`], so it cannot go stale: a builtin whose
/// usage shows a `-…` token takes options, and one that shows only operands does not.
///
/// The distinction decides who consumes the `--` terminator. A builtin with options
/// **owns** it, because only that builtin knows where its options end — `kill` reads
/// a leading `-SIGNAL`, so `kill -- -9 %1` has to reach `kill` with the `--` intact
/// or `-9` goes back to being a signal. One with no options has nothing to end, so
/// the terminator is pure noise in its arguments and is taken out centrally.
pub(crate) fn reads_options(name: &str) -> bool {
    entry(name).is_some_and(|(usage, _)| {
        usage
            .split(' ')
            .skip(1)
            .any(|token| token.trim_start_matches('[').starts_with('-'))
    })
}

/// Every builtin's name, in table order. The completion tables want the set of
/// names without the prose that goes with them.
pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    TABLE.iter().map(|(usage, _)| name_of(usage))
}

/// The table row for `name`, if it names a builtin.
fn entry(name: &str) -> Option<&'static (&'static str, &'static str)> {
    TABLE.iter().find(|(usage, _)| name_of(usage) == name)
}

/// Outcome of a builtin. `Status` reports an exit status and continues the loop;
/// `Exit` ends the shell with the given status.
pub enum Builtin {
    Status(u8),
    Exit(u8),
}

/// Does `name` name a builtin? Used to route one to the in-shell path — run
/// directly for a plain command, or in a forked stage inside a pipeline.
pub fn is_builtin(name: &str) -> bool {
    entry(name).is_some()
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

/// What `command not found` adds for a name mesh has an answer for: one bash
/// spells differently, so a bash reflex lands on mesh's spelling instead of a
/// dead end, or one that names a builtin, which only `command` can have looked
/// past. `None` for every other name — a typo gets the plain message.
pub(crate) fn rename_note(name: &str) -> Option<String> {
    // A builtin's name only reaches an external lookup through `command`, which
    // is defined to look past the builtins. Saying so beats a bare "not found"
    // for a name the reader can see in `help`.
    if is_builtin(name) {
        return Some(format!(
            "`{name}` is a builtin; `command` looks for a program"
        ));
    }
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
    let (usage, summary) = entry(name)?;
    Some(format_help(usage, summary))
}

/// Write generated help through the builtin-safe stdout path.
pub fn print_generated_help(name: &str, help: &str) -> u8 {
    write_stdout(name, help.as_bytes())
}

/// The summary leads, as it does in the help clap generates and the completion
/// parser already reads: a prose line carries no options, so it adds a sentence
/// for a reader without changing what a spec is built from.
fn format_help(usage: &str, summary: &str) -> String {
    format!("{summary}\n\nUsage: {usage}\n\nOptions:\n  --help  Print help\n")
}

/// The shape of a line, as `(names, form, summary)`: what to type, and the words
/// `help` will find it by. A keyword is looked up by itself, an operator by the
/// symbol, and a construct's other half by the word a reader would ask about —
/// `help else` is a question about `if`, so it answers with `if`.
///
/// Not the grammar: `PARSER.md` has that, and a copy of it here would be wrong
/// within a release. This is the one-screen reminder of what a mesh line can be,
/// which is what a reader in front of a prompt is asking for. What it does owe
/// is *reachability* — every reserved word the parser knows and every operator a
/// line can carry answers to `help`, even where several share one row, because a
/// reader who is told "not a keyword" about `unless` has been told something
/// false. The tests hold both lists against this table.
const SYNTAX: &[(&[&str], &str, &str)] = &[
    // Looked up as `cmd`, the word the form is written in, because `command` is
    // a builtin of its own now — and `help command` is a question about that
    // builtin, which answers with its usage.
    (
        &["cmd"],
        "cmd arg …",
        "Run a builtin, a function, or a program",
    ),
    (
        &["|", "|&"],
        "cmd | cmd",
        "Pipe stdout on; `|&` carries stderr too",
    ),
    (
        &["&&", "||", ";"],
        "cmd && cmd",
        "Chain on success; `||` on failure, `;` always",
    ),
    (
        &[">", "<", ">>", "2>", ">&", "<&", "&>"],
        "cmd > FILE",
        "Redirect; `<` reads, `>>` appends, `&>` both",
    ),
    (
        &["<<", "<<<"],
        "cmd << END … END",
        "Feed a heredoc; `<<< TEXT` is one line of it",
    ),
    (&["&"], "cmd &", "Run in the background as a job"),
    (
        &["=", "+="],
        "NAME = VALUE",
        "Bind a name — local in a `func`; `+=` appends",
    ),
    (
        &["$", "."],
        "$NAME",
        "Read a value; `$m.key` and `$xs[0]` too",
    ),
    (&["$("], "$(cmd)", "Capture a command's output as a value"),
    (
        &["[", "]", ",", "..", "..="],
        "[a b]  [key: value]",
        "A list and a map; `1..=3` is a range",
    ),
    (&["..."], "...$xs", "Spread a list into separate arguments"),
    (
        &[":"],
        "$path:base",
        "Postfix modifiers: `:base` `:len` `:int` …",
    ),
    (
        &["\"", "'"],
        "\"text $NAME\"",
        "Interpolate; `'…'` and `r'…'` do not",
    ),
    (
        &["*", "?", "~"],
        "*.txt ~/dir",
        "Globs and `~` expand; quoting stops them",
    ),
    (
        &["==", "!=", "<=", ">="],
        "$x == $y",
        "Compare: `==` `!=` `<` `>` `<=` `>=`, and `in`",
    ),
    (
        &["+", "-", "/", "%", "(", ")"],
        "n = (1 + 2)",
        "Arithmetic: `+` `-` `*` `/` `%`",
    ),
    (
        &["!~", "re"],
        "$name ~ *.txt",
        "Match a glob, or `re(…)`; `!~` inverts it",
    ),
    (
        &["global"],
        "global NAME = VALUE",
        "Bind in the session scope instead",
    ),
    (
        &["export"],
        "export NAME = VALUE",
        "Bind a name in the environment",
    ),
    (&["unset"], "unset NAME", "Remove a binding from this scope"),
    (
        &["if", "else", "{", "}"],
        "if COND { … } else { … }",
        "Run a body when a condition holds",
    ),
    (
        &["unless"],
        "cmd if COND",
        "Guard one line; `unless` is the inverse",
    ),
    (
        &["match", "=>"],
        "match VALUE { PAT => … ; … }",
        "Take the first arm whose pattern matches",
    ),
    (
        &["for", "in"],
        "for NAME in VALUE { … }",
        "Repeat over a list, a range, or a map",
    ),
    (
        &["while"],
        "while COND { … }",
        "Repeat while a condition holds",
    ),
    (&["loop"], "loop { … }", "Repeat until something breaks out"),
    (
        &["break", "continue"],
        "break",
        "Leave the nearest loop; `continue` restarts",
    ),
    (
        &["not", "and", "or"],
        "not $x",
        "Negate a value; `and` and `or` join two",
    ),
    (
        &["func"],
        "func NAME(PARAMS) { … }",
        "Define a function; call it by name",
    ),
    (
        &["return"],
        "return [VALUE]",
        "Leave a function, or a sourced file",
    ),
    (&["fork"], "fork { … }", "Run a body in a forked child"),
    // The value constructors have no operator to be documented at, unlike `re`,
    // which the `~` row covers. They still have to answer: the parser reserves all
    // three names, so a reader who is told `style` is "not a builtin or a keyword"
    // has been told something false — and pointed at `style --help`, which is a
    // command-position call and reports command-not-found.
    (
        &["style", "link"],
        "style(TEXT, fg: NAME, bold: BOOL) · link(TEXT, URL)",
        "Build a styled value; parens, not a command",
    ),
    // Reserved on the same parser check, and asked about for the same reason: a
    // reader who typed `dirs()` and got a diagnostic needs `dirs --help` to answer
    // rather than report a command that does not exist.
    (
        &["glob", "files", "dirs"],
        "glob(PATTERN) · files(DIR=.) · dirs(DIR=.)",
        "Expand to matching paths; parens, not a command",
    ),
];

/// Return the help text for a syntax entry — the keyword shape of what `help`
/// prints for a builtin. No `Options:` section, because a keyword takes no
/// flags: `if --help` is an `if` whose condition is a command called `--help`.
fn syntax_help(name: &str) -> Option<String> {
    let (_, form, summary) = SYNTAX.iter().find(|(names, ..)| names.contains(&name))?;
    Some(format!("{summary}\n\nSyntax: {form}\n"))
}

/// The widest form the summary column makes room for. `prompt-hook`'s full
/// signature is half again as wide as any other, and indenting every summary
/// past it would push the shortest lines — `jobs`, `pwd` — into empty space.
const USAGE_COLUMN: usize = 32;

const OVERVIEW_HEADER: &str = "\
mesh, in one screen. `help NAME` explains one entry; for a builtin,
`NAME --help` prints the same thing.

Builtins:
";

const SYNTAX_HEADER: &str = "\nSyntax:\n";

/// The listing `help` prints with no arguments: every builtin and every shape a
/// line can take, each with what it does beside it — `bash`'s `help` in mesh's
/// two-column shape.
///
/// The builtins are alphabetical, because that list is read by looking a name
/// up; the table itself stays in its own order, which groups the job builtins
/// together. The syntax keeps its authored order instead, which runs from a bare
/// command out to functions: that section is read from the top, as a tour.
fn overview() -> String {
    // Char counts, not byte lengths: `wait [JOB …]` carries a three-byte
    // ellipsis, and padding by bytes would leave it a column short.
    let width = |form: &str| form.chars().count();
    // One column across both sections, so the summaries read down the page as a
    // single list however the two tables happen to be worded.
    let column = TABLE
        .iter()
        .map(|(usage, _)| *usage)
        .chain(SYNTAX.iter().map(|(_, form, _)| *form))
        .map(width)
        .filter(|form| *form <= USAGE_COLUMN)
        .max()
        .unwrap_or(USAGE_COLUMN);
    let row = |listing: &mut String, form: &str, summary: &str| {
        listing.push_str("  ");
        listing.push_str(form);
        if width(form) > column {
            // Too wide to share the line: the summary takes the next one, still
            // in the column the rest of the listing reads down.
            listing.push('\n');
            listing.push_str(&" ".repeat(column + 4));
        } else {
            listing.push_str(&" ".repeat(column - width(form) + 2));
        }
        listing.push_str(summary);
        listing.push('\n');
    };
    let mut builtins: Vec<_> = TABLE.iter().collect();
    builtins.sort_by_key(|(usage, _)| name_of(usage));
    let mut listing = String::from(OVERVIEW_HEADER);
    for (usage, summary) in builtins {
        row(&mut listing, usage, summary);
    }
    listing.push_str(SYNTAX_HEADER);
    for (_, form, summary) in SYNTAX {
        row(&mut listing, form, summary);
    }
    listing
}

/// `help [NAME ...]` — with no arguments, list every builtin and every shape a
/// line can take; with names, explain each one. A builtin's entry is exactly
/// what its `--help` prints.
///
/// A name that is neither is an error rather than a lookup of some other kind:
/// mesh has no help of its own for a function or an external command, and
/// `NAME --help` is that command's own answer — which is what the note points
/// at. The other names still print, so one typo in a list does not cost the rest.
fn run_help(args: &[String]) -> u8 {
    if args.is_empty() {
        return write_stdout("help", overview().as_bytes());
    }
    let mut status = 0;
    let mut text = String::new();
    for name in args {
        match help(name).or_else(|| syntax_help(name)) {
            Some(entry) => {
                // A blank line between entries, so two of them do not read as one.
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&entry);
            }
            None => {
                note!(
                    "mesh: help: {name}: not a builtin or a keyword; try `{name} --help`, or `help` for the list"
                );
                status = 1;
            }
        }
    }
    status | write_stdout("help", text.as_bytes())
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
        "help" => Some(Builtin::Status(run_help(&words[1..]))),
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

/// Which escapes a styled value may emit on **this process's** stdout.
///
/// Nothing at all unless stdout is a terminal. Beyond that the two bits diverge,
/// which is the whole reason [`Decoration`] has two:
///
/// - **Color** additionally wants `NO_COLOR` unset and `TERM` other than `dumb`.
///   `NO_COLOR` follows the [no-color.org](https://no-color.org) rule — **set at
///   all** disables color, whatever its value, so `NO_COLOR=0` still means no
///   color, with an empty value the documented exception. `dumb` declares a
///   terminal that renders no attributes, the one name for which SGR is text rather
///   than styling. No allowlist beyond it: SGR is universal in a way `OSC` is not.
/// - **Links** ignore `NO_COLOR` — a hyperlink is not color, and dropping it would
///   lose the URL rather than make the output plainer — but they *do* want a
///   terminal known to parse an `OSC`, since `TERM=linux` reads `ESC ]` as the start
///   of a palette sequence and leaves the rest on screen. That is the same question
///   the title and the notification ask, so it is the same allowlist.
///
/// There is no setting and no probe on either path, because the attributes are data
/// and dropping them is always available.
///
/// The caller decides where "this command's stdout" actually goes — a redirect or a
/// pipe replaces it after the words are rendered — so this answers only for the
/// descriptor as it stands.
pub(crate) fn terminal_decoration(links_supported: bool) -> Decoration {
    use std::io::IsTerminal;

    if !std::io::stdout().is_terminal() {
        return Decoration::plain();
    }
    let dumb = env::var("TERM").is_ok_and(|term| term == "dumb");
    let no_color = env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    Decoration {
        color: !dumb && !no_color,
        links: links_supported,
    }
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
/// `decoration` says which escapes a **styled** value may emit. This is the one
/// place attributes are read, so it is also the one place the capability decision
/// applies — see [`terminal_decoration`](terminal_decoration). Every element of a
/// collection is asked the same question, so a list of styled values keeps each
/// element's own color and link.
pub(crate) fn rendered_for_output(value: &Value, decoration: Decoration) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Styled(styled) => Ok(styled.style.render(&styled.text, decoration)),
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
                    scalar => rendered_for_output(scalar, decoration)?,
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
                    scalar => rendered_for_output(scalar, decoration)?,
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
    use super::{
        Decoration, SYNTAX, TABLE, Value, base64, help, is_builtin, names, overview, path_line,
        reads_options, rename_note, rendered_for_output, syntax_help,
    };
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
        // The caveat retired itself when `gets` landed: the note checks
        // `is_builtin`, so it now sends the reader straight to a working name.
        assert_eq!(
            rename_note("read").as_deref(),
            Some("mesh spells this `gets`")
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
        // The note is for a bash reflex or a builtin, not for a typo — anything
        // else keeps the bare message.
        assert_eq!(rename_note("nosuchcmd"), None);
        assert_eq!(rename_note(""), None);
    }

    #[test]
    fn a_builtin_that_was_looked_for_as_a_program_says_so() {
        // Only `command` can have got here: it is defined to look past the
        // builtins, so "not found" alone would read as a lie about a name the
        // reader can see in `help`.
        assert_eq!(
            rename_note("puts").as_deref(),
            Some("`puts` is a builtin; `command` looks for a program")
        );
    }

    #[test]
    fn clip_is_a_builtin_with_help() {
        assert!(is_builtin("clip"));
        assert_eq!(
            help("clip").as_deref(),
            Some(
                "Copy text to the terminal's clipboard\n\nUsage: clip [TEXT ...]\n\nOptions:\n  --help  Print help\n"
            )
        );
    }

    #[test]
    fn command_is_a_builtin_that_owns_its_terminator() {
        assert!(is_builtin("command"));
        assert_eq!(
            help("command").as_deref(),
            Some(
                "Run a program, past builtins and functions\n\nUsage: command [--] NAME [ARG ...]\n\nOptions:\n  --help  Print help\n"
            )
        );
        // Everything after the program name belongs to the program, so the
        // terminator cannot be taken out centrally: `command grep -- -x file`
        // has to reach `grep` as written.
        assert!(reads_options("command"));
    }

    #[test]
    fn a_bare_command_line_is_explained_as_syntax_and_the_builtin_as_itself() {
        // The word `command` now names a builtin, so `help command` is a question
        // about it; the shape of a plain command line answers to `cmd`, the word
        // its form is written in.
        assert!(help("command").is_some());
        assert_eq!(syntax_help("command"), None);
        assert!(syntax_help("cmd").is_some_and(|help| help.contains("cmd arg …")));
    }

    #[test]
    fn print_is_a_builtin_with_help() {
        assert!(is_builtin("print"));
        assert_eq!(
            help("print").as_deref(),
            Some(
                "As `puts`, with no trailing newline\n\nUsage: print [ARG ...]\n\nOptions:\n  --help  Print help\n"
            )
        );
    }

    #[test]
    fn a_scalar_renders_as_itself() {
        assert_eq!(
            rendered_for_output(&Value::String("hi".into()), Decoration::plain()).as_deref(),
            Ok("hi")
        );
        assert_eq!(
            rendered_for_output(&Value::Integer(-7), Decoration::plain()).as_deref(),
            Ok("-7")
        );
        assert_eq!(
            rendered_for_output(&Value::Boolean(true), Decoration::plain()).as_deref(),
            Ok("true")
        );
    }

    #[test]
    fn a_collection_renders_one_entry_per_line() {
        let list = Value::List(vec![Value::String("a".into()), Value::Integer(2)]);
        assert_eq!(
            rendered_for_output(&list, Decoration::plain()).as_deref(),
            Ok("a\n2")
        );
        let map = Value::Map(vec![
            ("k".to_owned(), Value::String("v".into())),
            ("n".to_owned(), Value::Boolean(false)),
        ]);
        assert_eq!(
            rendered_for_output(&map, Decoration::plain()).as_deref(),
            Ok("k: v\nn: false")
        );
        // Empty is empty, not a stray separator.
        assert_eq!(
            rendered_for_output(&Value::List(vec![]), Decoration::plain()).as_deref(),
            Ok("")
        );
        assert_eq!(
            rendered_for_output(&Value::Map(vec![]), Decoration::plain()).as_deref(),
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
            assert!(
                rendered_for_output(&value, Decoration::plain()).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn every_name_the_shell_recognizes_is_listed_with_help() {
        // The listing is the reason the table exists: a builtin that answers to
        // its name but never appears here is one a reader cannot discover.
        let listing = overview();
        let mut seen = Vec::new();
        for name in names() {
            assert!(is_builtin(name), "{name}");
            assert!(help(name).is_some(), "{name}");
            assert!(
                listing.contains(&format!("  {name}")),
                "{name} is missing from the listing"
            );
            assert!(!seen.contains(&name), "{name} is in the table twice");
            seen.push(name);
        }
        assert_eq!(seen.len(), TABLE.len());
    }

    /// The rows of one section of the listing, without its heading.
    fn section(listing: &str, heading: &str) -> Vec<String> {
        listing
            .lines()
            .skip_while(|line| *line != heading)
            .skip(1)
            .take_while(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_builtins_are_listed_alphabetically() {
        // The builtins are a lookup table, so they are read by name. (The syntax
        // section is a tour, and deliberately keeps its authored order.)
        let listing = overview();
        let names: Vec<_> = section(&listing, "Builtins:")
            .iter()
            .filter(|row| !row.starts_with("   "))
            .filter_map(|row| row.split_whitespace().next().map(str::to_owned))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "{listing}");
    }

    #[test]
    fn every_summary_starts_in_one_column() {
        // Both sections share a column, including the summary pushed onto its own
        // line by an over-wide usage — measured in characters, since
        // `wait [JOB …]` pads around a three-byte ellipsis.
        let listing = overview();
        let rows: Vec<_> = section(&listing, "Builtins:")
            .into_iter()
            .chain(section(&listing, "Syntax:"))
            .collect();
        let indent = |row: &str| row.chars().take_while(|c| *c == ' ').count();
        let continuation = rows
            .iter()
            .find(|row| row.starts_with("     "))
            .expect("prompt-hook's summary wraps onto its own line")
            .clone();
        for row in &rows {
            assert!(
                indent(row) == 2 || indent(row) == indent(&continuation),
                "{row:?} starts in neither column"
            );
        }
    }

    #[test]
    fn a_keyword_is_explained_by_the_form_it_is_written_in() {
        assert_eq!(
            syntax_help("while").as_deref(),
            Some("Repeat while a condition holds\n\nSyntax: while COND { … }\n")
        );
        // The other half of a construct answers with the construct: `help else`
        // is a question about `if`, and an operator is asked for by its symbol.
        assert_eq!(syntax_help("else"), syntax_help("if"));
        assert_eq!(syntax_help("continue"), syntax_help("break"));
        assert_eq!(syntax_help("in"), syntax_help("for"));
        assert!(syntax_help("|").is_some_and(|help| help.contains("cmd | cmd")));
    }

    #[test]
    fn every_syntax_entry_is_listed_and_reachable_by_each_of_its_names() {
        let listing = overview();
        for (names, form, _) in SYNTAX {
            assert!(
                listing.contains(&format!("  {form}")),
                "{form} is missing from the listing"
            );
            for name in *names {
                assert!(syntax_help(name).is_some(), "{name}");
                // A keyword is not a builtin, so the two lookups cannot collide.
                assert!(!is_builtin(name), "{name}");
            }
        }
    }

    #[test]
    fn every_keyword_the_parser_reserves_is_explained() {
        // Mirrors the reserved words `parser.rs` matches on. A keyword the parser
        // knows and `help` does not is a reader being told, falsely, that a word
        // they just used is not a keyword.
        for keyword in [
            "func", "return", "if", "else", "unless", "match", "for", "in", "while", "loop",
            "break", "continue", "fork", "global", "unset", "export", "not", "and", "or", "re",
            // Reserved as built-in *value* names rather than as statement
            // keywords, but reserved by the same parser check, so the same rule
            // applies: `func style(…)` is refused, and a reader told `style` is not
            // a keyword has been told something false.
            "style", "link", "glob", "files", "dirs",
        ] {
            assert!(syntax_help(keyword).is_some(), "{keyword}");
        }
    }

    #[test]
    fn every_operator_a_line_can_carry_is_explained() {
        // Several share a row — `<` and `<=` are read in different places — but
        // each has to answer, since a reader asks with the symbol they typed.
        for operator in [
            "|", "|&", "&&", "||", ";", "&", ">", "<", ">>", "2>", ">&", "<&", "&>", "<<", "<<<",
            "=", "+=", "==", "!=", "<=", ">=", "+", "-", "/", "%", "*", "?", "~", "!~", "$", "$(",
            "...", ":", ".", ",", "(", ")", "[", "]", "{", "}", "..", "..=", "=>",
        ] {
            assert!(syntax_help(operator).is_some(), "{operator}");
        }
    }

    #[test]
    fn a_name_that_is_neither_a_builtin_nor_a_keyword_has_no_help() {
        // `help ls` must not answer for a command mesh does not own; `ls --help`
        // is that command's own business.
        for name in ["ls", "", "help ", "fi", "then", "esac", "elif"] {
            assert_eq!(help(name), None, "{name}");
            assert_eq!(syntax_help(name), None, "{name}");
        }
    }
}
