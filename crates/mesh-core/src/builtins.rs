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
use std::path::{Path, PathBuf};

use crate::vars::{Decoration, NEST_INDENT, Value};

/// Every builtin as `(usage, summary)`: the usage line its `--help` prints, and
/// the one-line description that sits beside it in `help`'s listing.
///
/// One table rather than a list of names beside a `match` of usages, so a new
/// builtin cannot arrive dispatchable but unlisted, or listed but unexplained.
/// The **name is the usage's first word** — a usage that did not start with the
/// command you type would be wrong anyway, so there is nothing to keep in step.
const TABLE: &[(&str, &str)] = &[
    ("cd [DIR]", "Change the working directory"),
    // Two spellings, like `gets`: the command prints the path and the call yields
    // it, so a prompt segment can say `style(pwd(), fg: blue)` without forking a
    // `$(pwd)` to get the same string.
    (
        "pwd · pwd()",
        "Print the working directory, or yield it as a value",
    ),
    ("puts [ARG ...]", "Render the arguments, then a newline"),
    ("print [ARG ...]", "As `puts`, with no trailing newline"),
    // Two spellings on one line — the only builtin with both, and `·` is what the
    // `SYNTAX` rows already use to separate them. `--nulls` is written into both,
    // since the value form takes it too and a usage that showed it on one would
    // describe the other as the spelling that cannot read a `-print0` stream.
    (
        "gets [--nulls] [VAR] · gets([--nulls])",
        "Read one line from stdin, or a NUL-terminated item",
    ),
    // Two spellings, like `gets`: the call is the constructor, and the command
    // form is what a `match` arm or a block tail writes. Both yield the value —
    // the command form leaves it as the statement's result rather than printing
    // it, which is what `x = match … { a => { status 5 } }` reads.
    (
        "status CODE · status(CODE)",
        "A status value — how a command went",
    ),
    ("clip [TEXT ...]", "Copy text to the terminal's clipboard"),
    ("notify [TEXT ...]", "Raise a desktop notification"),
    // Two spellings, because `[status] [N]` would say they are independently
    // optional and advertise a bare `exit status`, which is refused.
    ("exit [N] · exit status N", "Leave the shell"),
    ("fg [JOB]", "Resume a job in the foreground"),
    ("bg [JOB]", "Resume a stopped job in the background"),
    ("jobs", "List the jobs"),
    (
        "wait [--timeout DURATION] [JOB …]",
        "Wait for a job to finish",
    ),
    (
        "timeout DURATION COMMAND [ARG …]",
        "Run a command under a time limit",
    ),
    ("disown [-h] [-a | -r] [JOB …]", "Stop tracking a job"),
    ("kill [-SIGNAL] JOB|PID ...", "Signal a job or a process"),
    (
        "prompt [--reset | TEXT]",
        "Set or print the interactive prompt",
    ),
    ("title TEXT", "Set the window and tab title"),
    (
        "on [--remove] EVENT NAME [FUNCTION]",
        "Register a function as an event handler",
    ),
    (
        "command [--] NAME [ARG ...]",
        "Run a program, past builtins and functions",
    ),
    (
        "exec [--] CMD [ARG ...]",
        "Replace the shell with a program",
    ),
    ("source FILE", "Run a file's commands in this shell"),
    ("help [NAME ...]", "List the builtins, or explain one"),
    (
        "type [-t|-P|-a|--quiet] NAME ...",
        "Say what a name is: builtin, function, …",
    ),
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

/// The usage line for a builtin, if `name` is one. `whence` reports it as the
/// builtin's definition, from the same table `help` reads — so a builtin cannot
/// be described one way by one and another way by the other.
pub(crate) fn usage(name: &str) -> Option<&'static str> {
    entry(name).map(|(usage, _)| *usage)
}

/// The form a syntax entry is written as, if `name` is one — the [`SYNTAX`]
/// half of [`usage`], found by any of the words that row answers to.
pub(crate) fn syntax_form(name: &str) -> Option<&'static str> {
    SYNTAX
        .iter()
        .find(|(names, ..)| names.contains(&name))
        .map(|(_, form, _)| *form)
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

/// The names other shells use for a mesh builtin, mapped to mesh's spelling.
/// Kept as a *name* rather than prose so `is_builtin` can say whether the
/// replacement is wired up yet, and the caveat retires itself when it lands.
///
/// `echo` is here for the stripped-`PATH` case only: an external `echo` almost
/// always shadows this, which is deliberate — `echo -n` / `-e` keep working
/// through `/bin/echo`, where a mesh builtin would print the flag as text.
///
/// The other spellings of `type` are the point of those entries rather than a
/// footnote: the lookup command is the one every shell names differently, so
/// whichever one the reader's fingers know has to say where it went. `whence` is
/// ksh's and `where` zsh's; `what` is nobody's, and is here because it reads like
/// the question. None of them is reserved, so a user function may still take the
/// name — the pointer only fires when nothing else answers.
///
/// **`which` is deliberately absent.** In bash it is an external program that
/// cannot see builtins or functions, and mesh keeps that rather than shadowing a
/// binary, so `which cd` finds nothing here exactly as it finds nothing there. It
/// is also the only one of these names with a file on disk, so a pointer would
/// never fire for it anyway.
const RENAMED: &[(&str, &str)] = &[
    ("echo", "puts"),
    ("read", "gets"),
    ("whence", "type"),
    ("what", "type"),
    ("where", "type"),
];

const LOCAL: &str = "a plain `x = 5` inside a `func` is already local";
/// A bare `alias` is not the definition form, so it lands here. The spelling it
/// points at is the one that works: `alias NAME = COMMAND`, spaces and all,
/// since the bash-style `alias NAME=VALUE` tokenizes as a single word.
const ALIAS_SPELLING: &str = "an alias is `alias ll = ls -l` -- spaces around the `=`";
const NO_UNALIAS: &str =
    "an alias is a `wrapper func`; redefine the name to replace one, there is no `unalias`";
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
    ("alias", ALIAS_SPELLING),
    ("declare", SCOPE),
    ("function", "functions are `func name(params) { … }`"),
    ("let", "arithmetic is `n = (1 + 2)`"),
    ("local", LOCAL),
    ("typeset", SCOPE),
    ("unalias", NO_UNALIAS),
];

/// What `command not found` adds for a name mesh has an answer for: one bash
/// spells differently, so a bash reflex lands on mesh's spelling instead of a
/// dead end, or one that names a builtin, which only `command` can have looked
/// past. `None` for every other name — a typo gets the plain message.
pub(crate) fn rename_note(name: &str) -> Option<String> {
    // A numeral that is not an integer's own spelling is a **string**, so it is a
    // bare word and names a command like any other. That is consistent, and it is
    // also the last thing someone typing `0755` at a prompt expects, so the
    // not-found says which reading it took rather than leaving them to infer it.
    //
    // Deliberately only the words whose category this rule decides — text that
    // parses as an `i64` without being canonical. A broader "starts with a digit"
    // test would attach this to `2to3` and `7z`, which are ordinary program names
    // and have nothing to do with it.
    if name.parse::<i64>().is_ok() && crate::parser::canonical_integer(name).is_none() {
        return Some(format!(
            "`{name}` is a string, not the number it looks like; `puts {name}` prints it"
        ));
    }
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
///
/// A builtin's **own** options are listed above `--help`, read off the usage line
/// by [`usage_options`] rather than written a second time. Only `--help` used to
/// be listed, which made the `Options:` block wrong about every builtin that has
/// options of its own — `whence --help` did not mention `--all`, and since the
/// completion tables are built from this text, `whence --a<Tab>` had nothing to
/// offer either. One block, both readers.
fn format_help(usage: &str, summary: &str) -> String {
    let mut options = String::new();
    for option in usage_options(usage) {
        options.push_str(&format!("  {option}\n"));
    }
    format!("{summary}\n\nUsage: {usage}\n\nOptions:\n{options}  --help  Print help\n")
}

/// The literal options a usage line offers, in the order written.
///
/// Read off the usage line like [`name_of`] and [`reads_options`], so a builtin
/// cannot document a flag it does not take or take one it does not document.
/// `[--all|--quiet]` yields both, and `[-a | -r]` likewise: the brackets say
/// optional and the bar separates alternatives, so neither is part of a name.
///
/// Two things that look like options are deliberately excluded:
///
/// - A **metavariable**, which is what the case test is for: `kill [-SIGNAL]
///   JOB|PID` writes a placeholder for whichever signal you name, not a flag
///   spelled `-SIGNAL`. Usage lines spell placeholders as upper-case *words*
///   (`DIR`, `JOB`, `NAME`, `TEXT`), so length tells them apart from a flag
///   without a second list to maintain: `-P` is `type`'s, taken from bash along
///   with the rest of its surface, while `-SIGNAL` stands for whichever signal
///   you name. A one-character name is a flag whatever its case.
/// - The bare **`--` terminator**, which `command [--] NAME` writes to show where
///   it accepts one. It ends the options rather than being one, and offering it as
///   a flag would put it in the `Options:` block of every builtin that documents
///   taking it.
pub(crate) fn usage_options(usage: &str) -> impl Iterator<Item = &str> {
    usage
        .split(' ')
        .skip(1)
        .flat_map(|token| token.trim_matches(['[', ']']).split('|'))
        .map(str::trim)
        .filter(|token| {
            let name = token.trim_start_matches('-');
            token.starts_with('-')
                // Non-empty rules out a bare `--`; a multi-character upper-case
                // word rules out a metavariable, while leaving `-P` a flag.
                && !name.is_empty()
                && (name.chars().count() == 1
                    || name.chars().all(|character| !character.is_ascii_uppercase()))
        })
}

/// The shape of a line, as `(names, form, summary)`: what to type, and the words
/// `help` will find it by. A keyword is looked up by itself, an operator by the
/// symbol, and a construct's other half by the word a reader would ask about —
/// `help else` is a question about `if`, so it answers with `if`.
///
/// Not the grammar: `GRAMMAR.md` has that, and a copy of it here would be wrong
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
    // The booleans are words rather than punctuation, so `help true` has to answer
    // — and answering "not a builtin or a keyword" about a word the parser reads
    // as a value is the same falsehood `fail` used to be told about.
    (
        &["true", "false"],
        "true · false",
        "The booleans; a bare one is the value, not a command",
    ),
    (
        &["func"],
        "func NAME(PARAMS) { … }",
        "Define a function; call it by name",
    ),
    (
        &["wrapper"],
        "wrapper func NAME(…ARGS) { … }",
        "Define a function that parses no flags of its own",
    ),
    (
        &["alias"],
        "alias NAME = CMD ARG …",
        "Shorthand for a `wrapper func` that forwards to CMD",
    ),
    (
        &["return"],
        "return [VALUE]",
        "Leave a function, or a sourced file",
    ),
    (
        &["fail"],
        "fail [STATUS]",
        "Leave the same unit with a nonzero status",
    ),
    (&["fork"], "fork { … }", "Run a body in a forked child"),
    (
        &["with"],
        "with NAME=value … { … }",
        "Run a body with environment entries set, restoring them after",
    ),
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

/// What the parser does with a reserved word — the one fact the three views over
/// [`RESERVED_WORDS`] disagree about, and `and` separates all three: `help and`
/// must answer, `func and()` is allowed, and `and:kind` is not `keyword`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Claim {
    /// Taken **in command position**, so a bare one never reaches command lookup.
    Command,
    /// Claimed only by **what follows it**: `fork` is the subshell keyword only
    /// before a block, `unless` and `if` are postfix guards after a statement,
    /// `global` / `unset` / `export` need an assignment, and `and` / `or` / `in`
    /// are value operators. Each is a legal command word and a legal function
    /// name — the repo's own `a_command_named_fork_is_still_reachable` covers
    /// `fork` — so a bare one is an ordinary lookup that ends in `command not
    /// found` if nothing defines it.
    Contextual,
    /// A built-in **value call**. The parser refuses it as a *function* name, but
    /// a command-position `style …` is still a lookup that reports `command not
    /// found`, so it is not a command keyword either.
    ValueCall,
    /// A **literal**: `true` and `false` are the booleans in every position a
    /// statement is read (`docs/DESIGN.md` §"Bare words and quoted values"), so a
    /// bare one is a value and never reaches the program of that name.
    ///
    /// Like [`Claim::Command`] in that command position is settled before any
    /// lookup, and unlike it in what the word means there: a value rather than a
    /// construct. `type` keeps them apart for that reason — calling `true` a shell
    /// keyword would name the wrong thing — while both stop a `func` of the name.
    Literal,
}

/// Every word the parser reserves, and what it claims.
///
/// **The** table: `help`'s coverage, `func`'s refusal and `whence`'s keyword
/// answer are all derived from it below, so a new reserved word is added here
/// once instead of in three places that drift apart. The views stay distinct
/// rather than collapsing into one predicate — that would either misclassify
/// `and` as a keyword or stop documenting it.
///
/// Being listed here does *not* make a word unavailable: only [`Claim::Command`]
/// means a bare one does something, and `fork`, `unless`, `and` are all legal
/// function names.
pub(crate) const RESERVED_WORDS: &[(&str, Claim)] = &[
    ("func", Claim::Command),
    ("return", Claim::Command),
    // Taken on the same parser line as `return` / `break` / `continue`
    // (`parser.rs`, `Parser::control`), so a bare one is control flow and never a
    // lookup. It was missing from both hand-maintained lists this table replaces,
    // which is why `help fail` used to answer "not a builtin or a keyword".
    ("fail", Claim::Command),
    ("if", Claim::Command),
    ("match", Claim::Command),
    ("for", Claim::Command),
    ("while", Claim::Command),
    ("loop", Claim::Command),
    ("break", Claim::Command),
    ("continue", Claim::Command),
    ("global", Claim::Command),
    ("unset", Claim::Command),
    ("export", Claim::Command),
    ("not", Claim::Command),
    ("wrapper", Claim::Contextual),
    ("alias", Claim::Contextual),
    ("else", Claim::Contextual),
    ("unless", Claim::Contextual),
    ("in", Claim::Contextual),
    ("fork", Claim::Contextual),
    ("with", Claim::Contextual),
    ("and", Claim::Contextual),
    ("or", Claim::Contextual),
    // Not keywords and not calls: the parser reads a lone bare one as the boolean
    // (`parser::boolean_literal`), so `if true` forks nothing and `type true` must
    // not report the program that spelling no longer reaches.
    ("true", Claim::Literal),
    ("false", Claim::Literal),
    ("re", Claim::ValueCall),
    ("style", Claim::ValueCall),
    ("link", Claim::ValueCall),
    ("glob", Claim::ValueCall),
    ("files", Claim::ValueCall),
    ("dirs", Claim::ValueCall),
];

fn claim_of(name: &str) -> Option<Claim> {
    RESERVED_WORDS
        .iter()
        .find(|(word, _)| *word == name)
        .map(|(_, claim)| *claim)
}

/// Every reserved **word** — what `help` owes an answer for, and what completion
/// offers for a name argument.
///
/// The word view, as opposed to [`SYNTAX`]'s operator rows (`+`, `$(`, `"`) and
/// its `command` row for the generic shape of a line, which document punctuation
/// and a shape rather than names.
pub(crate) fn syntax_words() -> impl Iterator<Item = &'static str> {
    RESERVED_WORDS.iter().map(|(word, _)| *word)
}

/// Does the parser take a bare `name` in command position?
///
/// What `whence` asks to decide whether a syntax row is the answer to "what
/// runs" — a contextual word must not outrank the function or executable a bare
/// one would actually reach. Narrower than being in [`RESERVED_WORDS`]: see
/// [`Claim::Contextual`].
pub(crate) fn is_command_keyword(name: &str) -> bool {
    claim_of(name) == Some(Claim::Command)
}

/// Is `name` a built-in value call? The parser refuses these as function names.
pub(crate) fn is_value_call(name: &str) -> bool {
    claim_of(name) == Some(Claim::ValueCall)
}

/// Is a bare `name` a literal — one of the booleans? See [`Claim::Literal`].
///
/// Asked where "what does a bare one do" is the question and the answer is
/// neither a lookup nor a construct: `type` reports the value, and a definition
/// of the name is refused because nothing could ever reach it.
pub(crate) fn is_literal(name: &str) -> bool {
    claim_of(name) == Some(Claim::Literal)
}

/// Builtins that answer to the **call** spelling as well as the command one, so
/// `name(…)` yields a value where a bare `name …` reports a status.
///
/// Deliberately not a [`Claim`]: a `Claim::ValueCall` name is *only* ever a call —
/// `style …` in command position is a `command not found` — where these run both
/// ways. `gets` is the first, and `DESIGN.md` §"Builtins" gives it both spellings
/// in one sentence: read a line into `VAR`, *and* return that line as its value.
///
/// The list exists so nothing has to ask "is this a call?" twice and get two
/// answers — `gets():capture` must record the *call*, not run the command form and
/// discard the line it read.
const CALLABLE_BUILTINS: &[&str] = &["gets", "status", "pwd"];

/// Does `name` name a builtin with a value-call spelling? See [`CALLABLE_BUILTINS`].
pub(crate) fn is_callable_builtin(name: &str) -> bool {
    CALLABLE_BUILTINS.contains(&name)
}

/// Return the help text for a syntax entry — the keyword shape of what `help`
/// prints for a builtin. No `Options:` section, because a keyword takes no
/// flags: `if --help` is an `if` whose condition is a command called `--help`.
fn syntax_help(name: &str) -> Option<String> {
    let (_, form, summary) = SYNTAX.iter().find(|(names, ..)| names.contains(&name))?;
    Some(format!("{summary}\n\nSyntax: {form}\n"))
}

/// The widest form the summary column makes room for. `on`'s full
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
        // `cd` is absent on purpose: it is dispatched by the REPL, which owns the
        // `precd` / `postcd` hooks that bracket the move.
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

/// Where a pending `cd` is headed.
///
/// Split out from the move itself so the REPL can run the `precd` hooks between
/// the two — `DESIGN.md` requires the target to be **absolute before `precd`**,
/// so that a handler which itself `cd`s elsewhere cannot make a *relative* outer
/// `cd` land somewhere unintended.
pub(crate) struct CdTarget {
    /// Absolute, with symlinks and `..` already resolved: what `$env.PWD` will
    /// read after the move, which is what a `precd` handler is told.
    path: std::path::PathBuf,
    /// `cd -`, and a `CDPATH` hit, print where they landed, as POSIX does.
    echo: bool,
}

impl CdTarget {
    /// The resolved destination, for the `precd` hook argument.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve `cd`'s operand without moving: no argument → `$HOME`; `cd -` →
/// `$OLDPWD`; a plain relative name → the first `$CDPATH` entry that holds it.
/// `Err` carries the status, its diagnostic already reported.
///
/// Resolution is `canonicalize`, so the path handed on is the physical one
/// `$PWD` will hold, and a destination that does not exist is reported here —
/// before any hook has run for a move that was never going to happen.
///
/// Not yet implemented (deferred to the language layer): `--physical`, autocd,
/// and a shell-maintained *logical* cwd — `$PWD` is the physical `getcwd` path
/// for now.
pub(crate) fn cd_target(args: &[String]) -> Result<CdTarget, u8> {
    if args.len() > 1 {
        note!("mesh: cd: too many arguments");
        return Err(1);
    }
    // Keep targets as `OsString` so a non-UTF-8 `$HOME`/`$OLDPWD` reaches the OS
    // unchanged rather than being mangled by lossy UTF-8 conversion.
    let mut echo = false;
    let target: OsString = match args.first().map(String::as_str) {
        None => match env::var_os("HOME") {
            Some(home) => home,
            None => {
                note!("mesh: cd: HOME not set");
                return Err(1);
            }
        },
        Some("-") => match env::var_os("OLDPWD") {
            Some(old) => {
                echo = true;
                old
            }
            None => {
                note!("mesh: cd: OLDPWD not set");
                return Err(1);
            }
        },
        Some(dir) => match cdpath_hit(dir) {
            Some((found, announce)) => {
                echo = announce;
                found
            }
            None => dir.into(),
        },
    };

    let path = Path::new(&target);
    match path.canonicalize() {
        Ok(resolved) => Ok(CdTarget {
            path: resolved,
            echo,
        }),
        // Reported against the operand as written, not the resolution attempt,
        // so `cd nope` still says `nope`.
        Err(err) => {
            note!("mesh: cd: {}: {err}", path.display());
            Err(1)
        }
    }
}

/// Search `$CDPATH` for `operand`, yielding the directory found and whether the
/// move should announce itself.
///
/// `CDPATH` is a search path for `cd` the way `PATH` is one for commands, and
/// mesh already carries it as a [path-type list][crate::environ] — so it was
/// splittable, appendable, and exported, while `cd` itself ignored it. Entries
/// are tried in order and the first that *holds a directory* of that name wins;
/// with no hit the caller falls back to resolving against the current directory,
/// so setting `CDPATH` never breaks a plain `cd subdir`.
///
/// Two conventions come from POSIX, and bash reads them the same way:
///
/// - **A dot-relative or absolute operand never searches.** `.`, `..`, `./x`,
///   `../x`, and `/x` resolve from where you are, so `cd ../` cannot land in a
///   `CDPATH` entry. An **empty** operand does not search either — `entry/""` is
///   the entry itself, which would turn `cd ""` into a jump.
/// - **A hit through a non-empty entry prints where it landed**, since the
///   destination is not the one the operand appears to name. An empty entry *is*
///   the current directory, so that one is silent.
fn cdpath_hit(operand: &str) -> Option<(OsString, bool)> {
    if operand.is_empty() || resolves_from_here(operand) {
        return None;
    }
    let cdpath = env::var_os("CDPATH")?;
    env::split_paths(&cdpath).find_map(|entry| {
        let candidate = entry.join(operand);
        // `is_dir` follows symlinks, so a link to a directory is one — the same
        // answer `cd` itself would give.
        candidate
            .is_dir()
            .then(|| (candidate.into_os_string(), !entry.as_os_str().is_empty()))
    })
}

/// Is this operand one that always resolves from the current directory? An
/// absolute path, or the dot-relative forms — including bare `.` and `..`, which
/// name the same places their slashed spellings do.
fn resolves_from_here(operand: &str) -> bool {
    Path::new(operand).is_absolute()
        || matches!(operand, "." | "..")
        || operand.starts_with("./")
        || operand.starts_with("../")
}

/// Move to an already-resolved target, updating `$PWD` and `$OLDPWD` so child
/// processes that read them see the new directory.
///
/// `previous` is the directory to record as `$OLDPWD`; the caller captures it
/// *before* the `precd` hooks run, so a handler that `cd`s away cannot become
/// what `cd -` comes back to. `Err` is the move failing — canonicalizing does
/// not prove the directory can be entered — and is the caller's signal that no
/// `postcd` is owed.
pub(crate) fn cd_change(target: &CdTarget, previous: Option<&Path>) -> Result<u8, u8> {
    if let Err(err) = env::set_current_dir(&target.path) {
        note!("mesh: cd: {}: {err}", target.path.display());
        return Err(1);
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
            if target.echo {
                status = write_stdout("cd", &path_line(current.as_os_str()));
            }
        }
    }
    Ok(status)
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

/// The working directory **both** `pwd` spellings report.
///
/// One reader so the command and the call cannot come to disagree about what the
/// cwd is, or about how a failure reads. Physical `getcwd` for now: the
/// shell-owned logical cwd of `DESIGN.md` §"Built-ins" — maintained by `cd` and
/// validated against a stale or forged `$env.PWD` — is not built yet, and when it
/// lands it lands here, for both spellings at once.
pub(crate) fn working_directory() -> Result<PathBuf, String> {
    env::current_dir().map_err(|err| err.to_string())
}

/// `pwd` — print the current working directory.
///
/// M0-level: no `-L`/`-P` flags. The value spelling is `pwd()`, in `repl.rs`.
fn pwd(args: &[String]) -> u8 {
    if !args.is_empty() {
        note!("mesh: pwd: too many arguments");
        return 1;
    }
    match working_directory() {
        Ok(dir) => write_stdout("pwd", &path_line(dir.as_os_str())),
        Err(message) => {
            note!("mesh: pwd: {message}");
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
/// A **nested** collection keeps that rule and moves down a level: under a map key
/// it goes on the lines below, [indented](indented); as a list element it takes a
/// [`- ` bullet](bulleted). The bullet is only where the ambiguity is — a scalar
/// element needs no marker because the line break already separates it, while a
/// nested collection's line breaks are *inside* it, so `[[1 2] [3 4]]` would
/// otherwise print exactly as the flat `[1 2 3 4]`.
///
/// The result reads like YAML and is deliberately not YAML: nothing here quotes or
/// escapes, so a scalar holding a newline — or one that itself starts with `- ` —
/// renders ambiguously. That is the standing trade for output meant to be read.
/// [`:repr`](crate::repl) is the form that survives a round trip.
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
        // The bare number, as at every other byte boundary: mesh already loses
        // type there — the int `5` and the string `"5"` both write `5` — so a
        // status needs no rendering of its own. `:repr` is the form that keeps it.
        Value::Status(code) => Ok(code.to_string()),
        // Reached only for a flag nested in a collection (`puts [--force]`): a
        // top-level one is refused before here, since `puts` declares no options
        // and a flag in a call is an option. Inside a list it is data being
        // displayed, so it shows the text it was written with.
        Value::Flag(flag) => Ok(flag.text()),
        Value::FlagTerminator => Ok("--".to_owned()),
        Value::List(items) => {
            let mut lines = Vec::with_capacity(items.len());
            for item in items {
                lines.push(match item {
                    nested @ (Value::List(_) | Value::Map(_)) => {
                        bulleted(&rendered_for_output(nested, decoration)?)
                    }
                    scalar => rendered_for_output(scalar, decoration)?,
                });
            }
            Ok(lines.join("\n"))
        }
        Value::Map(entries) => {
            let mut lines = Vec::with_capacity(entries.len());
            for (key, entry) in entries {
                lines.push(match entry {
                    nested @ (Value::List(_) | Value::Map(_)) => {
                        match rendered_for_output(nested, decoration)? {
                            // An empty collection has no block to hang below the
                            // key, and a bare `key:` beats a line of trailing space.
                            block if block.is_empty() => format!("{key}:"),
                            block => format!("{key}:\n{}", indented(&block)),
                        }
                    }
                    scalar => format!("{key}: {}", rendered_for_output(scalar, decoration)?),
                });
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

/// Shift a rendered block one level right, to sit under the map key that holds it.
///
/// An empty line stays empty: indenting it would emit trailing whitespace that no
/// reader can see and every diff can.
fn indented(block: &str) -> String {
    block
        // `split` rather than `lines`: a scalar renders as itself, so a trailing
        // newline is a blank line the block still has, and a `\r\n` is the text's
        // own bytes. `lines` would eat both.
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{NEST_INDENT}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Mark a rendered block as **one** element of the list holding it: `- ` on its
/// first line, the same width of indent on the rest, so where an element begins
/// stays visible however many lines it takes.
fn bulleted(block: &str) -> String {
    if block.is_empty() {
        // A bare `-`, not `- ` — an empty element still needs its marker, and the
        // trailing space would be invisible whitespace.
        return "-".to_owned();
    }
    block
        // See [`indented`](indented) on why this is not `lines`.
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("- {line}")
            } else if line.is_empty() {
                String::new()
            } else {
                format!("{NEST_INDENT}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
pub(crate) fn write_terminal(label: &str, sequence: &str) -> u8 {
    match OpenOptions::new().write(true).open(Path::new("/dev/tty")) {
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
///
/// **`exit status N` is accepted** beside `exit N`, so the two ways of leaving
/// with a status are spelled alike: `return status 5` fills `return`'s status
/// channel, and someone who has written that writes this next. It disambiguates
/// nothing — `exit` fills only the status channel, so `exit 5` already says the
/// whole thing — which is why it is a *spelling* rather than a channel word: it
/// is read here as a literal leading operand, the way `cd -` and `wait
/// --timeout` read theirs, not by the parser. `exit status(5)` needs no rule at
/// all; the call is one word that renders as its code.
///
/// The other channel word is refused **by name**. `return value X` is real, so
/// `exit value 5` is the mistake the `status` spelling invites, and `too many
/// arguments` answered a question nobody asked: the problem is not how many
/// operands there are, it is that `exit` has no value channel to fill — a value
/// needs somewhere to go, and a leaving shell has nowhere.
fn exit(args: &[String], last: u8) -> Builtin {
    if args.first().is_some_and(|word| word == "value") {
        note!("mesh: exit: `exit` has no value channel, use `exit N` or `exit status N`");
        // The message changes what is *said*, never what happens. This layer sees
        // strings, so a written `exit value` and a quoted or computed
        // `exit "value"` are the same bytes here — and a diagnostic must not be
        // what decides whether the shell leaves, or one particular operand would
        // silently stop being fatal. So the outcome is whatever the operand
        // would have produced without the message: alone it is a non-numeric
        // operand, which reports and still exits with `2`; with more after it,
        // it is a surplus, which reports and stays.
        return if args.len() > 1 {
            Builtin::Status(1)
        } else {
            Builtin::Exit(2)
        };
    }
    let args = match args {
        [word, rest @ ..] if word == "status" => {
            if rest.is_empty() {
                // The word promised a code, as `return status` does. Reported
                // without exiting, since a spelling that lost its operand is the
                // same kind of typo a surplus one is.
                note!("mesh: exit: expected a status code after `status`");
                return Builtin::Status(1);
            }
            rest
        }
        _ => args,
    };
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

/// The status a bounded wait reports when its limit runs out.
///
/// `timeout(1)`'s number, so a script already written against that keeps
/// reading. For `wait --timeout` this is the *builtin's* status and not the
/// job's: the job has not exited, keeps its place in the table, and its own
/// `status` stays empty -- which is what tells a caller "still running" apart
/// from "exited 124".
pub(crate) const TIMED_OUT_CODE: u8 = 124;

/// Read a duration written the way a person writes one: `500ms`, `2s`, `1m`,
/// `1h`, or the compound forms `1m30s` and `2h5m`. A bare number is seconds,
/// which is the unit every caller means when they leave it off.
///
/// The compound forms are here because [`crate::repl::duration_words`] already
/// *prints* them -- a shell that says `1m30s` and cannot read it back is one
/// whose own output is not valid input.
///
/// A *string* rather than a value type: mesh has no duration in its type system
/// today, and adding one to give `wait --timeout` an argument would be a
/// language change made for a builtin's convenience. Parsing here leaves that
/// open -- a real duration type can accept these same spellings later without
/// invalidating anything already written.
///
/// Rejects rather than saturates. `2x` is a typo, not two of something, and a
/// wait that silently used a limit the caller did not ask for is worse than one
/// that refuses.
pub(crate) fn parse_duration(text: &str) -> Option<std::time::Duration> {
    if text.is_empty() {
        return None;
    }
    let mut millis = 0f64;
    let mut rest = text;
    let mut parts = 0;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let (digits, tail) = rest.split_at(digits_end);
        // Fractions are allowed because `0.5s` is the obvious way to ask for
        // half a second and the alternative spelling, `500ms`, is not obvious.
        let count: f64 = digits.parse().ok()?;
        if !count.is_finite() || count < 0.0 {
            return None;
        }
        // `ms` before `m`, or every millisecond is read as a minute.
        let (scale_ms, tail) = if let Some(tail) = tail.strip_prefix("ms") {
            (1f64, tail)
        } else if let Some(tail) = tail.strip_prefix('s') {
            (1_000f64, tail)
        } else if let Some(tail) = tail.strip_prefix('m') {
            (60_000f64, tail)
        } else if let Some(tail) = tail.strip_prefix('h') {
            (3_600_000f64, tail)
        } else if tail.is_empty() && parts == 0 {
            // The bare form, and only on its own: `1m30` would be asking this to
            // guess whether the 30 is seconds or another minute.
            (1_000f64, tail)
        } else {
            return None;
        };
        millis += count * scale_ms;
        rest = tail;
        parts += 1;
    }
    if millis > u64::MAX as f64 {
        return None;
    }
    Some(std::time::Duration::from_millis(millis as u64))
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn a_duration_reads_the_units_a_person_writes() {
        use std::time::Duration;
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        // No suffix is seconds, which is what a caller who leaves it off means.
        assert_eq!(parse_duration("3"), Some(Duration::from_secs(3)));
        // Fractions, because `0.5s` is the obvious way to ask for half a second.
        assert_eq!(parse_duration("0.5s"), Some(Duration::from_millis(500)));
        // The compound forms duration_words prints, read back.
        assert_eq!(parse_duration("1m30s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("2h5m"), Some(Duration::from_secs(7500)));
    }

    #[test]
    fn a_duration_that_does_not_parse_is_refused_rather_than_guessed() {
        // Each of these could be saturated or partially read into *something*.
        // A wait silently using a limit nobody asked for is the worse failure.
        for bad in [
            "2x", "", "s", "ms", "-1s", "one", "1 s", "nan", "1m30", "1m2x", "..", "1..2s",
        ] {
            assert_eq!(parse_duration(bad), None, "accepted {bad:?}");
        }
    }
    use super::{
        Claim, Decoration, RESERVED_WORDS, SYNTAX, TABLE, Value, base64, help, is_builtin,
        is_command_keyword, is_literal, is_value_call, name_of, names, overview, path_line,
        reads_options, rename_note, rendered_for_output, syntax_help, syntax_words, usage_options,
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

    fn rendered(value: &Value) -> String {
        rendered_for_output(value, Decoration::plain()).expect("renders")
    }

    #[test]
    fn a_nested_collection_moves_down_a_level() {
        // Under a key, the block sits below and indented — the `$env` shape.
        let map = Value::Map(vec![
            ("EDITOR".to_owned(), Value::String("vim".into())),
            (
                "PATH".to_owned(),
                Value::List(vec![
                    Value::String("/usr/bin".into()),
                    Value::String("/bin".into()),
                ]),
            ),
        ]);
        assert_eq!(rendered(&map), "EDITOR: vim\nPATH:\n  /usr/bin\n  /bin");

        // As a list element it takes a bullet, so where each element starts stays
        // visible — without it this is indistinguishable from a flat `[1 2 3 4]`.
        let nested = Value::List(vec![
            Value::List(vec![Value::Integer(1), Value::Integer(2)]),
            Value::List(vec![Value::Integer(3), Value::Integer(4)]),
        ]);
        assert_eq!(rendered(&nested), "- 1\n  2\n- 3\n  4");

        // A scalar element keeps its bare line: the marker goes only where the
        // ambiguity is.
        let mixed = Value::List(vec![
            Value::String("a".into()),
            Value::List(vec![Value::String("b".into())]),
        ]);
        assert_eq!(rendered(&mixed), "a\n- b");

        // Depth is not capped; each level shifts by one indent.
        let deep = Value::Map(vec![(
            "a".to_owned(),
            Value::Map(vec![("b".to_owned(), Value::List(vec![Value::Integer(1)]))]),
        )]);
        assert_eq!(rendered(&deep), "a:\n  b:\n    1");

        // A map inside a list bullets its first entry and aligns the rest.
        let maps = Value::List(vec![Value::Map(vec![
            ("a".to_owned(), Value::Integer(1)),
            ("b".to_owned(), Value::Integer(2)),
        ])]);
        assert_eq!(rendered(&maps), "- a: 1\n  b: 2");
    }

    /// "A scalar renders as itself" has to keep holding once the scalar is nested:
    /// indenting shifts the block sideways, it does not rewrite the text. A
    /// line-splitter that ate the terminators would silently normalize both of
    /// these, and only when nested — the top level would still be right.
    #[test]
    fn nesting_shifts_a_scalar_without_rewriting_it() {
        // A trailing newline is a blank line the block still has.
        let map = Value::Map(vec![(
            "k".to_owned(),
            Value::List(vec![Value::String("a\n".into())]),
        )]);
        assert_eq!(rendered(&map), "k:\n  a\n");

        // `\r\n` is the text's own bytes, not a line ending to normalize.
        let crlf = Value::Map(vec![(
            "k".to_owned(),
            Value::List(vec![Value::String("a\r\nb".into())]),
        )]);
        assert_eq!(rendered(&crlf), "k:\n  a\r\n  b");

        // The same through a bullet.
        let list = Value::List(vec![Value::List(vec![Value::String("a\n".into())])]);
        assert_eq!(rendered(&list), "- a\n");
    }

    #[test]
    fn an_empty_nested_collection_renders_without_trailing_space() {
        let map = Value::Map(vec![("k".to_owned(), Value::List(vec![]))]);
        assert_eq!(rendered(&map), "k:");
        let list = Value::List(vec![Value::Map(vec![]), Value::Integer(1)]);
        assert_eq!(rendered(&list), "-\n1");
    }

    #[test]
    fn a_value_with_no_byte_form_is_a_loud_error() {
        // Naming the type beats guessing a rendering, the same answer the argv
        // boundary gives.
        for value in [
            Value::Glob("*.rs".into()),
            Value::Stream(1),
            Value::Job(1),
            // Nesting renders (see `a_nested_collection_moves_down_a_level`); a
            // value with no byte form stays an error wherever it is found.
            Value::List(vec![Value::List(vec![Value::Stream(1)])]),
            Value::Map(vec![("k".to_owned(), Value::Job(1))]),
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
            .expect("on's summary wraps onto its own line")
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
        // `RESERVED_WORDS` mirrors the words `parser.rs` matches on. A keyword the
        // parser knows and `help` does not is a reader being told, falsely, that a
        // word they just used is not a keyword.
        for keyword in syntax_words() {
            assert!(syntax_help(keyword).is_some(), "{keyword}");
        }
    }

    #[test]
    fn a_syntax_word_is_a_word_and_not_every_syntax_row() {
        // The two are for different questions, and conflating them is what made
        // `whence --quiet +` claim the operator resolved. Every syntax *word* is a
        // name; `SYNTAX` also documents operators and a `command` row for the shape
        // of a line, which name nothing.
        for word in syntax_words() {
            assert!(
                word.chars().all(|c| c.is_ascii_alphabetic()),
                "{word} is not a word"
            );
        }
        for documented in ["+", "$(", "\"", ".."] {
            assert!(syntax_help(documented).is_some(), "{documented}");
            assert!(!is_command_keyword(documented), "{documented}");
        }
    }

    #[test]
    fn a_builtins_own_options_are_read_off_its_usage_line() {
        let options = |usage| usage_options(usage).collect::<Vec<_>>();
        // Brackets say optional and the bar separates alternatives, so neither is
        // part of a name.
        assert_eq!(
            options("type [-t|-P|-a|--quiet] NAME ..."),
            ["-t", "-P", "-a", "--quiet"]
        );
        assert_eq!(options("disown [-h] [-a | -r] [JOB …]"), ["-h", "-a", "-r"]);
        assert_eq!(options("prompt [--reset | TEXT]"), ["--reset"]);
        // A metavariable is not an option: `-SIGNAL` stands for whichever signal
        // you name, so offering it as a flag would be offering a word that does
        // not exist.
        assert_eq!(options("kill [-SIGNAL] JOB|PID ..."), Vec::<&str>::new());
        // Nor is the terminator: `command [--]` documents *taking* a `--`, and
        // listing it would put `--` in the `Options:` block of every builtin that
        // says so.
        assert_eq!(options("command [--] NAME [ARG ...]"), Vec::<&str>::new());
        // Nothing to find where a builtin takes only operands.
        assert_eq!(options("cd [DIR]"), Vec::<&str>::new());
        assert_eq!(options("puts [ARG ...]"), Vec::<&str>::new());
        // The name itself is skipped even when it starts with a dash-like word.
        assert_eq!(options("pwd"), Vec::<&str>::new());
    }

    #[test]
    fn generated_help_lists_a_builtins_own_options() {
        // The `Options:` block used to name only `--help`, which made it wrong
        // about every builtin that has options — and, because the completion
        // tables are built from this text, made those flags uncompletable too.
        let help = help("type").expect("type is a builtin");
        assert!(
            help.contains("\nOptions:\n  -t\n  -P\n  -a\n  --quiet\n  --help"),
            "{help}"
        );
        // Listing a flag implies reading options — but not the reverse, and the gap
        // is deliberate. `reads_options` asks "who owns the `--` terminator", which
        // `kill` does because it reads a leading signal; `usage_options` asks "what
        // literal flags are there to offer", and a `-SIGNAL` placeholder is none.
        for (usage, _) in TABLE {
            let name = name_of(usage);
            if usage_options(usage).next().is_some() {
                assert!(reads_options(name), "{name} lists a flag it does not read");
            }
        }
        assert!(reads_options("kill"));
        assert_eq!(usage_options("kill [-SIGNAL] JOB|PID ...").count(), 0);
    }

    #[test]
    fn the_reference_sorts_the_builtins_by_who_owns_the_terminator() {
        // `docs/REFERENCE.md` splits the builtins into the ones that end their own
        // options at `--` and the ones the terminator is stripped for. Nothing tied
        // that prose to `reads_options`, so it went stale the moment `gets` declared
        // `--nulls` — teaching the wrong parsing rule for the builtin that had just
        // changed sides. The owners side is checked for completeness, not just for
        // accuracy: a builtin with options that is named in neither list leaves the
        // reader to guess, which is how `exec` sat unlisted.
        const REFERENCE: &str = include_str!("../../../docs/REFERENCE.md");
        let paragraph = REFERENCE
            .split("Which command consumes it depends on which has options to end.")
            .nth(1)
            .expect("REFERENCE.md says who consumes the terminator");
        let (none, rest) = paragraph
            .split_once("have none of their own")
            .expect("the sentence naming the builtins with no options");
        let (owners, _) = rest
            .split_once("do, so each ends its own options at")
            .expect("the sentence naming the builtins that end their own");

        // The names are the backticked words, in a sentence that holds nothing else.
        fn named(text: &str) -> Vec<&str> {
            text.split('`').skip(1).step_by(2).collect()
        }

        for name in named(none) {
            assert!(names().any(|n| n == name), "{name} is not a builtin");
            assert!(
                !reads_options(name),
                "REFERENCE.md says {name} has no options, but it reads them"
            );
        }
        let owners = named(owners);
        for &name in &owners {
            assert!(names().any(|n| n == name), "{name} is not a builtin");
            assert!(
                reads_options(name),
                "REFERENCE.md says {name} ends its own options, but it has none"
            );
        }
        for (usage, _) in TABLE {
            let name = name_of(usage);
            assert_eq!(
                reads_options(name),
                owners.contains(&name),
                "{name} reads options iff REFERENCE.md lists it as ending its own"
            );
        }
    }

    #[test]
    fn every_reserved_word_appears_once_and_claims_one_thing() {
        // `claim_of` answers with the first row it finds, so a word listed twice
        // would take the earlier claim and quietly ignore the later one — the exact
        // drift the single table exists to stop, reintroduced inside it.
        let mut seen = std::collections::HashSet::new();
        for (word, _) in RESERVED_WORDS {
            assert!(seen.insert(*word), "{word} is listed twice");
        }
        // Each view is a partition of the table, not an overlapping filter: no word
        // is both taken in command position and a value call.
        for (word, claim) in RESERVED_WORDS {
            assert_eq!(is_command_keyword(word), *claim == Claim::Command, "{word}");
            assert_eq!(is_value_call(word), *claim == Claim::ValueCall, "{word}");
            assert_eq!(is_literal(word), *claim == Claim::Literal, "{word}");
        }
    }

    #[test]
    fn the_literal_rows_are_the_words_the_parser_reads_as_booleans() {
        // The table decides what `type` reports and what `func` refuses, so a word
        // claimed here that the parser does not read as a value would refuse a
        // definition that works — and one the parser reads and the table misses is
        // the silently dead `func true()` this pair of rows exists to stop.
        for (word, claim) in RESERVED_WORDS {
            assert_eq!(
                *claim == Claim::Literal,
                crate::parser::boolean_literal(word).is_some(),
                "{word}"
            );
        }
        for spelling in ["true", "false"] {
            assert!(is_literal(spelling), "{spelling}");
            assert!(!is_command_keyword(spelling), "{spelling}");
            assert!(!is_value_call(spelling), "{spelling}");
            // Not a builtin, so the two lookups cannot disagree about it.
            assert!(!is_builtin(spelling), "{spelling}");
        }
    }

    #[test]
    fn a_command_keyword_is_a_syntax_word_the_parser_always_takes() {
        // Being a syntax word is now structural — a command keyword *is* a row in
        // `RESERVED_WORDS` — so what is left to get wrong is the claim on each row.
        for keyword in ["func", "return", "if", "match", "for", "while", "not"] {
            assert!(is_command_keyword(keyword), "{keyword}");
            assert!(!is_value_call(keyword), "{keyword}");
        }
        // `fail` was missing from both lists this table replaces, so `help fail`
        // answered "not a builtin or a keyword" about a word the parser takes on
        // the same line as `return`. It is control flow, not a lookup.
        assert!(is_command_keyword("fail"));
        assert!(syntax_help("fail").is_some());
        // The contextual words are deliberately *out*. Each is claimed only by what
        // follows it, so a bare one is an ordinary command word — legal as a
        // function name, and `command not found` when nothing defines it. Treating
        // these as keywords made `whence fork` outrank a real `func fork()`.
        for contextual in [
            "fork", "with", "wrapper", "alias", "unless", "else", "and", "or", "in",
        ] {
            assert!(!is_command_keyword(contextual), "{contextual}");
            assert!(!is_value_call(contextual), "{contextual}");
        }
        // Refused as *function* names, but a command-position one is still a
        // lookup that reports `command not found` — they are value calls.
        for constructor in ["re", "style", "link", "glob", "files", "dirs"] {
            assert!(is_value_call(constructor), "{constructor}");
            assert!(!is_command_keyword(constructor), "{constructor}");
        }
        // A word nobody reserves gets no claim at all, which is what keeps a real
        // program of that name reachable.
        for ordinary in ["cmd", "pwd", "git"] {
            assert!(!is_command_keyword(ordinary), "{ordinary}");
            assert!(!is_value_call(ordinary), "{ordinary}");
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
