//! Word expansion: interpolation, then tilde and filesystem globs.
//!
//! Each word is a list of pieces (`Text` expandable/literal, or `Var`). We first
//! resolve `Var` pieces against the variable store — an interpolated value is
//! **literal** (never re-split or re-globbed, per the no-word-splitting rule) —
//! then run tilde/glob on the expandable text. Only unquoted (`expandable`) text
//! supplies tilde/glob syntax; quoted text is kept verbatim (glob-escaped).
//!
//! Results are `String` args, so a non-UTF-8 `$HOME`/match/`$env` value is
//! rendered lossily; the real fix is `OsString` words later.

use std::env;
use std::fs;

/// One access step applied from left to right to a variable value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Member(String),
    Subscript(String),
    Slice {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Dir,
    Base,
    Ext,
    Exts,
    Stem,
    Bare,
    Len,
    First,
    Last,
    Rest,
    Init,
    Dedup,
    Keys,
    Values,
    Upper,
    Lower,
    /// Peel whitespace from the front / back, repeatedly. The argument-free
    /// members of the affix family — the char-set forms (`:trimstart("/")`) take
    /// an argument and so are evaluated with the rest of that family, not here.
    TrimStart,
    TrimEnd,
    Int,
    /// `:words` — split a string on runs of ASCII whitespace, the classic IFS
    /// word-split. A **split** modifier, so it consumes one string and yields a
    /// list; the argument-taking member of the family is `:split(SEP)`.
    Words,
    /// The fixed-separator split modifiers — `:lines`, `:nulls`, `:tabs`. Each is
    /// `:split(SEP)` with the separator spelled by the name, terminator semantics
    /// and all, so the three cannot drift from the general form or each other.
    /// `:lines` is the explicit spelling of a capture's **default** split, not a
    /// second pass over it.
    Lines,
    Nulls,
    Tabs,
    /// This value written as the mesh source you would have typed for it, as a
    /// string. The inverse of reading a literal, so `$x:repr` on a value that
    /// has no literal form is an error rather than an approximation — see
    /// [`Value::to_literal`](crate::vars::Value::to_literal).
    Repr,
    /// `:real` — the path with every symlink, `.` and `..` resolved, absolute.
    /// Grouped with the path components in `DESIGN.md` and maps over a list like
    /// them, but it is the one that **asks the filesystem** rather than slicing
    /// the string, so it can fail where they cannot.
    Real,
    // File tests: `-e`, `find -type`, `-r`, `-w`. Scalar questions about one path,
    // mapping element-wise over a list like the transforms above.
    Exists,
    Type,
    Read,
    Write,
    // File-type filters: `-f`, `-d`, `-L`, `-x`. On a list they keep the matching
    // elements and drop the rest — a subset, not a transform — and chain for AND
    // (`:f:x` is executable files); on one path they are the bare test.
    Files,
    Dirs,
    Links,
    Exec,
    /// `test -t N`: is this stream a terminal? Asked of a [`Value::Stream`], the
    /// only value that carries a descriptor — a bare integer is refused so the
    /// question cannot be pointed at an unrelated one.
    Tty,
}

impl Modifier {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "dir" => Self::Dir,
            "base" => Self::Base,
            "ext" => Self::Ext,
            "exts" => Self::Exts,
            "stem" => Self::Stem,
            "bare" => Self::Bare,
            "real" => Self::Real,
            "len" => Self::Len,
            "tty" => Self::Tty,
            "first" => Self::First,
            "last" => Self::Last,
            "rest" => Self::Rest,
            "init" => Self::Init,
            "dedup" => Self::Dedup,
            "keys" => Self::Keys,
            "values" => Self::Values,
            "upper" => Self::Upper,
            "lower" => Self::Lower,
            "trimstart" => Self::TrimStart,
            "trimend" => Self::TrimEnd,
            "int" => Self::Int,
            // The split family carries a two-letter alias each, since a split is
            // what a line loop or a `-print0` pipeline writes on every use. They
            // are *systematic* — initial plus `s` — rather than the `test`-derived
            // single letters `:f` / `:d` / `:l` / `:x`, which is why none of them
            // collides: `:l` is already `:links`.
            "words" | "ws" => Self::Words,
            "lines" | "ls" => Self::Lines,
            "nulls" | "ns" => Self::Nulls,
            "tabs" | "ts" => Self::Tabs,
            "repr" => Self::Repr,
            "exists" => Self::Exists,
            "type" => Self::Type,
            "read" => Self::Read,
            "write" => Self::Write,
            "files" | "f" => Self::Files,
            "dirs" | "d" => Self::Dirs,
            "links" | "l" => Self::Links,
            "exec" | "x" => Self::Exec,
            _ => return None,
        })
    }
}

/// One step of a reference's modifier chain.
///
/// A name this path has no implementation for is **carried** rather than rejected
/// when the reference is built, so the steps before it still run — and still report
/// first. Reporting at build time named the wrong step: `${s:keys:lines}` blamed
/// `:lines` for a chain that never got past `:keys` requiring a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModifierStep {
    /// `name` rides along because a pattern reads some names differently from
    /// every other value — `:x` is `extended` there and the executable-file filter
    /// elsewhere — and which one is meant cannot be known until the value is.
    Apply { modifier: Modifier, name: String },
    /// A name this path has no [`Modifier`] for.
    ///
    /// `name` is kept because one set *can* be applied here after all — the regex
    /// flags, which need only the value to know they apply. Both messages are
    /// ready-made because which names take arguments, and which live elsewhere, is
    /// the caller's knowledge; choosing between them is this layer's, since only it
    /// has the value. `regex_message` is what a **pattern** hears for a name that is
    /// not a flag, and differs wherever the reason is not the value's type —
    /// `:capture` needs an invocation whatever it is applied to.
    Unavailable {
        name: String,
        message: String,
        regex_message: String,
    },
}

impl ModifierStep {
    fn name(&self) -> &str {
        match self {
            ModifierStep::Apply { name, .. } | ModifierStep::Unavailable { name, .. } => name,
        }
    }

    /// What a **pattern** is told when this step is not one of its flags.
    ///
    /// A pattern takes the four flags and nothing else, so the reason is normally
    /// the value's type. The caller can say otherwise, which is how `:capture`
    /// keeps "needs an invocation" whatever value it meets.
    fn regex_message(&self) -> String {
        match self {
            ModifierStep::Unavailable { regex_message, .. } => regex_message.clone(),
            ModifierStep::Apply { name, .. } => {
                format!("modifier :{name} is not valid for a regex")
            }
        }
    }
}

/// Apply one step of a modifier chain — **the** implementation, for every spelling.
///
/// `${x:m}` reaches it through [`resolve_value`] and `$x:m` through the caller's
/// dispatcher. While those were two implementations they disagreed three times over
/// — about which names are flags, about which table wins on a pattern, and about
/// whether a flag change is validated — each time silently, and each time in the
/// direction of the spelling that was written second. So the rule is that neither
/// side re-derives anything decided here.
pub(crate) fn apply_modifier_step(
    mut value: Value,
    step: &ModifierStep,
) -> Result<Value, ExpandError> {
    // A pattern answers first, and on its own terms: `:x` is the `extended` flag
    // here and `Modifier::Exec` everywhere else, so a name table consulted before
    // the value is known answers the wrong one.
    if let Value::Regex(regex) = &mut value {
        if !set_regex_flag(regex, step.name()) {
            return Err(ExpandError::ModifierUnavailable(step.regex_message()));
        }
        // A flag can invalidate a pattern that parsed without it, so the change is
        // validated here rather than left to whatever matches with it later.
        compile_regex(regex).map_err(ExpandError::ModifierUnavailable)?;
        return Ok(value);
    }
    match step {
        ModifierStep::Apply { modifier, .. } => apply_modifier(value, *modifier),
        ModifierStep::Unavailable { message, .. } => {
            Err(ExpandError::ModifierUnavailable(message.clone()))
        }
    }
}

/// Build `value` into a usable pattern, or say why it cannot be one.
///
/// Shared with the caller so both spellings of a flag change validate identically:
/// `:x` turns `#` into a comment introducer, so a pattern that parsed without it
/// can fail to parse with it.
pub(crate) fn compile_regex(value: &crate::vars::RegexValue) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(&value.pattern)
        .case_insensitive(value.case_insensitive)
        .multi_line(value.multi_line)
        .dot_matches_new_line(value.dot_matches_new_line)
        .ignore_whitespace(value.ignore_whitespace)
        .build()
        .map_err(|error| format!("invalid regex: {error}"))
}

/// Set the flag `name` spells on a pattern, reporting whether it named one.
///
/// The four flags are the whole of what a pattern takes, so a name that is not one
/// of them does not apply to a pattern either. Shared with the caller's dispatcher
/// so the two spellings of the same chain cannot drift.
pub(crate) fn set_regex_flag(regex: &mut crate::vars::RegexValue, name: &str) -> bool {
    match name {
        "i" | "ignorecase" => regex.case_insensitive = true,
        "m" | "multiline" => regex.multi_line = true,
        "s" | "dotall" => regex.dot_matches_new_line = true,
        "x" | "extended" => regex.ignore_whitespace = true,
        _ => return false,
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    pub name: String,
    pub accesses: Vec<Access>,
    pub modifiers: Vec<ModifierStep>,
    pub quoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text {
        text: String,
        expandable: bool,
    },
    Var(VarRef),
    /// A value argument (`puts (1 + 2)`, `puts $(pwd)`, `puts style(x, fg: red)`),
    /// **already evaluated**.
    ///
    /// Evaluating it needs the shell — a `$(…)` launches a command, a call runs a
    /// function — so it happens where the shell is, before expansion, and the result
    /// rides in here. Like an interpolated variable the value is *literal*: never
    /// re-split, never re-globbed.
    ///
    /// Carrying the value rather than its text is what lets `puts style(x, fg: red)`
    /// keep its attributes and `puts (…)` on a list render per-line, since
    /// [`expand_call_values`] can hand the value straight over.
    Value(Value),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub pieces: Vec<Piece>,
    /// The glob's `(…)` options, when the word carried them. Applied after the
    /// pattern has matched, since every one of them asks the filesystem about a
    /// path that already exists.
    pub qualifiers: Option<GlobQualifiers>,
}

use crate::parser::{FileKind, GlobQualifiers};
use crate::vars::{Value, Vars};

/// An expansion error — an unbound read fails loud (no null), per `DESIGN.md`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpandError {
    UnboundVar(String),
    UnsetEnv(String),
    Unsupported(String),
    /// A modifier this path cannot apply, reported when the chain reaches it. The
    /// message is built by the caller, so it renders verbatim.
    ModifierUnavailable(String),
    ListNeedsSpread(String),
    /// A map has no such key. Its own variant because `Unsupported` renders as
    /// "… not supported yet", which reads as an unimplemented feature — and a
    /// missing key is a normal, permanent error, the loud no-such-field
    /// `f(…):capture` relies on for a `.value` an external cannot have.
    NoSuchKey {
        name: String,
        key: String,
    },
    /// A function value reached a place that needs bytes — a command argument, an
    /// interpolation, the environment. It is the one value with no text form.
    NoTextForm {
        name: String,
        kind: &'static str,
    },
    /// A handle whose job has left the table. Its own variant because the handle
    /// is perfectly valid — the job it names is simply finished and reaped, which
    /// is a different thing from an unbound variable or a missing key.
    GoneJob {
        id: usize,
    },
    NotAList(String),
    /// A **value argument** with no byte form — `ls (1..3)`. Carries the whole
    /// diagnostic rather than a category, because the argv rules read better named
    /// ("a list needs `...`") than classified, and there is no variable to name.
    ArgumentValue(String),
    IndexOutOfRange {
        name: String,
        index: i64,
    },
    Modifier {
        name: String,
        message: String,
    },
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::UnboundVar(n) => write!(f, "{n}: unbound variable"),
            ExpandError::UnsetEnv(k) => write!(f, "$env.{k}: not set"),
            ExpandError::Unsupported(s) => write!(f, "{s}: not supported yet"),
            ExpandError::ModifierUnavailable(message) => write!(f, "{message}"),
            ExpandError::ListNeedsSpread(n) => {
                write!(f, "${n}: list value needs `...` in command arguments")
            }
            ExpandError::NoSuchKey { name, key } => {
                write!(f, "${name}: no `{key}` in this map")
            }
            ExpandError::NoTextForm { name, kind } => {
                write!(f, "${name}: a {kind} has no text form")
            }
            ExpandError::GoneJob { id } => {
                write!(f, "job {id} is no longer in the job table")
            }
            ExpandError::NotAList(n) => write!(f, "${n}: cannot index a string value"),
            ExpandError::ArgumentValue(message) => write!(f, "{message}"),
            ExpandError::IndexOutOfRange { name, index } => {
                write!(f, "${name}[{index}]: list index out of range")
            }
            ExpandError::Modifier { name, message } => write!(f, ":{name}: {message}"),
        }
    }
}

/// Expand each word into zero or more argument strings (the external-argv rule:
/// a bare list value is an error — spread or join it).
pub fn expand(words: Vec<Word>, vars: &Vars) -> Result<Vec<String>, ExpandError> {
    let mut out = Vec::new();
    for word in words {
        if let Some(vref) = spread_var(&word) {
            out.extend(spread_strings(vref, vars)?);
            continue;
        }
        expand_word(word, vars, &mut out)?;
    }
    Ok(out)
}

/// Expand words into typed argument values for an **in-shell function call**,
/// preserving typed values rather than applying the external-argv rule.
/// `...$xs` spreads into one value per element; an otherwise whole bare variable
/// reference arrives as one value. Bare integer and boolean literals are typed;
/// every other word yields string argument(s) via ordinary expansion.
pub fn expand_values(words: Vec<Word>, vars: &Vars) -> Result<Vec<Value>, ExpandError> {
    Ok(expand_call_values(words, vars)?
        .into_iter()
        .map(|(value, _)| value)
        .collect())
}

/// Like [`expand_values`], but tags each value with whether it came from a single
/// **bare literal** word — one unquoted, un-interpolated `Text` piece. The tag
/// lets a function call type the attached value of `--flag=value` exactly as it
/// would the same token passed positionally: a bare `--n=2` is the integer `2`,
/// while a quoted (`--n="2"`) or interpolated (`--n=$s`) value keeps its expanded
/// string type. Spread and whole-variable values are never bare.
/// [`expand_values`] for one word, plus whether it reached the filesystem.
///
/// The flag is what decides whether a lone expanded value collapses to a scalar.
/// Qualifiers count as globbing whatever the pattern did, since `*(d)` asks the
/// filesystem about every path it kept.
pub fn expand_word_values(word: Word, vars: &Vars) -> Result<(Vec<Value>, bool), ExpandError> {
    let qualified = word.qualifiers.is_some();
    let mut strings = Vec::new();
    let globbed = expand_word(word, vars, &mut strings)?;
    Ok((
        strings.into_iter().map(Value::String).collect(),
        globbed || qualified,
    ))
}

pub fn expand_call_values(
    words: Vec<Word>,
    vars: &Vars,
) -> Result<Vec<(Value, bool)>, ExpandError> {
    let mut out = Vec::new();
    for word in words {
        if let Some(vref) = spread_var(&word) {
            out.extend(spread_values(vref, vars)?.into_iter().map(|v| (v, false)));
        } else if let Some(value) = whole_value(&word, vars) {
            out.push((value?, false));
        } else if let Some(value) = scalar_literal(&word) {
            out.push((value, true));
        } else {
            // A single unquoted, un-interpolated word with no glob metacharacters
            // is a bare literal. A glob is not: even when it matches exactly one
            // path the value came from filesystem expansion (like a positional
            // glob), so it stays a string rather than being typed from its bytes.
            let bare = matches!(
                word.pieces.as_slice(),
                [Piece::Text { text, expandable: true }] if !has_glob_meta(text)
            );
            let mut strings = Vec::new();
            expand_word(word, vars, &mut strings)?;
            out.extend(strings.into_iter().map(|s| (Value::String(s), bare)));
        }
    }
    Ok(out)
}

fn spread_values(vref: &VarRef, vars: &Vars) -> Result<Vec<Value>, ExpandError> {
    match resolve_value(vref, vars)? {
        Value::List(values) => Ok(values),
        // A styled value is a string, so it is not a list for the same reason.
        Value::String(_) | Value::Styled(_) => Err(ExpandError::Unsupported(format!(
            "...${}: value is not a list",
            vref.name
        ))),
        Value::Map(_) => Err(ExpandError::Unsupported(
            "a map cannot be spread here".into(),
        )),
        Value::Integer(_)
        | Value::Boolean(_)
        | Value::Regex(_)
        | Value::Glob(_)
        | Value::Stream(_)
        | Value::Job(_) => Err(ExpandError::Unsupported(format!(
            "...${}: value is not a list",
            vref.name
        ))),
        Value::Function(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "function value",
        }),
    }
}

/// Resolve a `...$name` spread to its element strings (a whole list or a slice).
/// An indexed element can itself be a list. A string, scalar element, or unbound
/// name is an error, matching the command-position spread rules.
fn spread_strings(vref: &VarRef, vars: &Vars) -> Result<Vec<String>, ExpandError> {
    match resolve_value(vref, vars)? {
        Value::List(values) => strings(values, &vref.name),
        Value::String(_) | Value::Styled(_) => Err(ExpandError::Unsupported(format!(
            "...${}: value is not a list",
            vref.name
        ))),
        Value::Map(_) => Err(ExpandError::Unsupported(
            "a map cannot be spread into argv".into(),
        )),
        Value::Integer(_)
        | Value::Boolean(_)
        | Value::Regex(_)
        | Value::Glob(_)
        | Value::Stream(_)
        | Value::Job(_) => Err(ExpandError::Unsupported(format!(
            "...${}: value is not a list",
            vref.name
        ))),
        Value::Function(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "function value",
        }),
    }
}

/// The text a **value argument** contributes to argv — the same bytes-only rule
/// every other command argument meets, since an external takes bytes.
///
/// This is the fallback, not the main path: a word that is *only* a value argument
/// reaches [`whole_value`] first and stays typed, so `puts style(x, fg: red)` keeps
/// its attributes. Rendering here is for the cases that genuinely need bytes — an
/// external command, or a value argument glued to text (`ls dir$(suffix)`).
fn value_argument_text(value: &Value) -> Result<String, ExpandError> {
    let refuse = |message: &str| Err(ExpandError::ArgumentValue(message.to_owned()));
    match value {
        Value::String(text) => Ok(text.clone()),
        // Its text is what crosses; argv carries bytes, not attributes.
        Value::Styled(styled) => Ok(styled.text.clone()),
        Value::Integer(number) => Ok(number.to_string()),
        Value::Boolean(flag) => Ok(flag.to_string()),
        Value::List(_) => refuse("a list needs `...` to become command arguments"),
        Value::Map(_) => refuse("a map cannot be a command argument"),
        Value::Regex(_) | Value::Glob(_) => refuse("a pattern cannot be a command argument"),
        Value::Stream(_) => refuse("a stream handle has no text form"),
        Value::Job(_) => refuse("a job handle has no text form"),
        Value::Function(_) => refuse("a function value has no text form"),
    }
}

fn strings(values: Vec<Value>, name: &str) -> Result<Vec<String>, ExpandError> {
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Ok(value),
            // Its text is what crosses; argv carries bytes, not attributes.
            Value::Styled(styled) => Ok(styled.text),
            Value::Integer(value) => Ok(value.to_string()),
            Value::Boolean(value) => Ok(value.to_string()),
            Value::List(_) => Err(ExpandError::Unsupported(format!(
                "...${name}: nested list element cannot be a command argument"
            ))),
            Value::Map(_) => Err(ExpandError::Unsupported(format!(
                "...${name}: map element cannot be a command argument"
            ))),
            Value::Regex(_) | Value::Glob(_) => Err(ExpandError::Unsupported(format!(
                "...${name}: pattern element cannot be a command argument"
            ))),
            // A handle belongs with the function value, not the patterns: what
            // it lacks is a byte form, not a way to be matched against.
            Value::Stream(_) => Err(ExpandError::NoTextForm {
                name: name.to_string(),
                kind: "stream handle",
            }),
            Value::Job(_) => Err(ExpandError::NoTextForm {
                name: name.to_string(),
                kind: "job handle",
            }),
            Value::Function(_) => Err(ExpandError::NoTextForm {
                name: name.to_string(),
                kind: "function value",
            }),
        })
        .collect()
}

/// Preserve a whole bare variable reference at an in-shell value boundary.
fn whole_value(word: &Word, vars: &Vars) -> Option<Result<Value, ExpandError>> {
    // A value argument is already a value; handing it over untouched is what lets
    // `puts style(x, fg: red)` keep its attributes and `puts (…)` on a collection
    // render per-line rather than meeting the argv rule.
    if let [Piece::Value(value)] = word.pieces.as_slice() {
        return Some(Ok(value.clone()));
    }
    let [Piece::Var(vref)] = word.pieces.as_slice() else {
        return None;
    };
    if vref.quoted {
        return None;
    }
    Some(resolve_value(vref, vars))
}

fn scalar_literal(word: &Word) -> Option<Value> {
    let [
        Piece::Text {
            text,
            expandable: true,
        },
    ] = word.pieces.as_slice()
    else {
        return None;
    };
    // A bare word that is not a typed literal falls through to ordinary string
    // expansion, so only surface `true`/`false`/integers as typed values here.
    match typed_scalar(text) {
        Value::String(_) => None,
        typed => Some(typed),
    }
}

/// Type a bare scalar string the way an in-shell function argument is typed:
/// `true`/`false` become booleans, an integer literal becomes an integer, and
/// anything else stays a string. Used both for bare literal arguments and for the
/// attached value of a valued `--flag=value`, so `--n=2` is the integer `2` just
/// like a positional `2` or the flag's default expression.
pub(crate) fn typed_scalar(text: &str) -> Value {
    match text {
        "true" => Value::Boolean(true),
        "false" => Value::Boolean(false),
        _ => text
            .parse()
            .map(Value::Integer)
            .unwrap_or_else(|_| Value::String(text.to_owned())),
    }
}

/// Recognize the deliberately narrow first spread form: `...$name` as a whole
/// word. General expression spreading arrives with the parser.
fn spread_var(word: &Word) -> Option<&VarRef> {
    match word.pieces.as_slice() {
        [
            Piece::Text {
                text,
                expandable: true,
            },
            Piece::Var(vref),
        ] if text == "..." => Some(vref),
        _ => None,
    }
}

/// A word reduced to `(text, expandable)` pieces, after interpolation and tilde.
type Pieces = Vec<(String, bool)>;

/// Expand one word into `out`, reporting whether it expanded against the
/// **filesystem** — which the caller needs, because a glob is a list however many
/// paths it matched while an ordinary word collapses to the one value it made.
///
/// Reported *by* expansion rather than re-derived by the caller: the predicate is
/// subtler than "has a `*` in it" — the expandable segments must also form a valid
/// pattern on their own — and a second copy of it drifted immediately, binding
/// `x = a[` as a one-element list where expansion had correctly fallen back to the
/// literal text.
fn expand_word(word: Word, vars: &Vars, out: &mut Vec<String>) -> Result<bool, ExpandError> {
    // Resolve interpolations first; an interpolated value is literal.
    let mut pieces: Pieces = Vec::new();
    for piece in word.pieces {
        match piece {
            Piece::Text { text, expandable } => pieces.push((text, expandable)),
            Piece::Var(vref) => pieces.push((resolve(&vref, vars)?, false)),
            // Literal, exactly as an interpolated variable is: never re-split and
            // never re-globbed, so `puts $(ls)` cannot glob what it just listed.
            Piece::Value(value) => pieces.push((value_argument_text(&value)?, false)),
        }
    }
    apply_tilde(&mut pieces);

    // A word globs only if it has glob syntax *and* its expandable segments form a
    // valid pattern on their own (literals stood in by a placeholder), so an escaped
    // literal fragment can't complete a broken class in an adjacent expandable
    // segment. Anything else is the literal text it looks like.
    let structure: String = pieces
        .iter()
        .map(|(t, e)| if *e { t.clone() } else { "a".to_string() })
        .collect();
    let matches = if pieces.iter().any(|(t, e)| *e && has_glob_meta(t))
        && glob::Pattern::new(&structure).is_ok()
    {
        glob_matches(&glob_pattern(&pieces))
    } else {
        None
    };
    // A pattern `glob` refused falls back to its literal text, and a literal is not
    // a glob — so this is `Some` only when the filesystem was really consulted.
    let globbed = matches.is_some();
    let mut matches = matches.unwrap_or_else(|| vec![literal(&pieces)]);
    // Qualifiers apply to whatever the word produced, including the literal a
    // pattern `glob` refuses falls back to. Filtering only real matches would let
    // `*[(f)` drop its `(f)` silently, and a qualifier that is quietly ignored is
    // worse than one that takes the word down to nothing.
    if let Some(qualifiers) = &word.qualifiers {
        matches.retain(|path| qualifies(path, qualifiers));
    }
    out.extend(matches);
    Ok(globbed)
}

/// Does this path satisfy every one of a glob's qualifiers?
///
/// The tests are ANDed and all of them ask the filesystem, which is why they belong
/// to globbing rather than to string matching (`DESIGN.md` §"Globbing"). A path that
/// cannot be stat'd qualifies for nothing: it is a broken symlink or a race, and
/// there is no evidence it is what was asked for.
///
/// **Symlinks are read two ways, because the two questions differ.** A *type* is
/// about the name, so it comes from `lstat` — that is what makes `l` mean anything
/// at all, and it is how `find -type` reads without `-L`. `exec` and `empty` are
/// about the thing you would open, so they follow the link: a symlink's own mode is
/// `0777` on Linux, so an `lstat` reading of `exec` would answer "yes" for every
/// symlink in the directory and `*(x)` would be useless. `*(l, x)` is then the
/// readable spelling of "links to something runnable".
fn qualifies(path: &str, qualifiers: &GlobQualifiers) -> bool {
    let Ok(named) = fs::symlink_metadata(path) else {
        return false;
    };
    if !qualifiers.types.is_empty() && !qualifiers.types.iter().any(|kind| is_kind(*kind, &named)) {
        return false;
    }
    if qualifiers.exec.is_none() && qualifiers.empty.is_none() {
        return true;
    }
    // Following the link can fail where naming it did not — a dangling symlink.
    // Nothing is known about a target that isn't there, so it satisfies neither test.
    let Ok(target) = fs::metadata(path) else {
        return false;
    };
    if let Some(want) = qualifiers.exec {
        use std::os::unix::fs::PermissionsExt;
        // Any of the three execute bits, as `find -perm -111` asks. Whether *this*
        // process could run it is a question about uid, gid and the mount, which
        // neither this nor `find` sets out to answer.
        if (target.permissions().mode() & 0o111 != 0) != want {
            return false;
        }
    }
    if let Some(want) = qualifiers.empty {
        // A directory is empty when it has no entries, a regular file when it has no
        // bytes, and **nothing else is empty at all**. `find -empty` draws the same
        // line, and it matters: a fifo, a socket and most device nodes all report a
        // zero length without that being a statement about their contents, so
        // reading the number would put every one of them in `*(empty: true)`.
        let empty = if target.is_dir() {
            fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
        } else {
            target.is_file() && target.len() == 0
        };
        if empty != want {
            return false;
        }
    }
    true
}

fn is_kind(kind: FileKind, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let file_type = metadata.file_type();
    match kind {
        FileKind::File => file_type.is_file(),
        FileKind::Dir => file_type.is_dir(),
        FileKind::Symlink => file_type.is_symlink(),
        FileKind::Fifo => file_type.is_fifo(),
        FileKind::Socket => file_type.is_socket(),
        FileKind::Block => file_type.is_block_device(),
        FileKind::Char => file_type.is_char_device(),
    }
}

/// Every path a pattern matches, with the dotfile rule applied: a `*`, `?` or
/// `[…]` never matches a leading `.`, but a **literal** `.` written in the pattern
/// does — so `*` skips dotfiles and `.*` finds them.
///
/// `glob`'s own `require_literal_leading_dot` cannot express that second half. It
/// drops every dot-prefixed name from the directory listing before the pattern is
/// ever consulted (`fill_todo`), so under it `.*` matched no dotfile at all — only
/// the synthetic `.` and `..` entries the crate adds back for a dot-led pattern.
/// The matcher does implement the rule correctly, so enumerate with the option
/// **off** and re-check each candidate with it **on**: the walk sees the dotfiles,
/// and the decision stays the crate's.
///
/// `.` and `..` are dropped whatever the pattern. They are the crate's synthesis
/// rather than directory entries, they arrive spelled against the pattern's base
/// (`./..`), and a loop over `.*` wants the dotfiles, not its own directory.
///
/// `None` means the pattern is one `glob` refuses, which leaves the word literal.
fn glob_matches(pattern: &str) -> Option<Vec<String>> {
    let literal_dot = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..glob::MatchOptions::new()
    };
    let walk = glob::MatchOptions {
        require_literal_leading_dot: false,
        ..literal_dot
    };
    // The walk reports a match in its own spelling, which is not always the one
    // the pattern was written in: a leading `./` is normalized away, so `./tool[0]`
    // comes back as `tool0`. Re-checking has to compare like with like or every
    // `./`-led pattern would filter its own matches out.
    let compiled = glob::Pattern::new(undotted(pattern)).ok()?;
    let paths = glob::glob_with(pattern, walk).ok()?;
    Some(
        paths
            .flatten()
            .map(|path| path.to_string_lossy().into_owned())
            .filter(|path| {
                !names_self_or_parent(path) && compiled.matches_with(undotted(path), literal_dot)
            })
            .collect(),
    )
}

/// A path minus a leading `./`, the one spelling difference between what a pattern
/// says and what the walk reports.
fn undotted(path: &str) -> &str {
    path.strip_prefix("./").unwrap_or(path)
}

/// Does this path name a directory's own `.` or `..` entry? Checked as text
/// because `Path` normalizes a trailing `.` out of existence (`dir/.` keeps only
/// `dir`), which would let the entry through as its own parent.
fn names_self_or_parent(path: &str) -> bool {
    matches!(path, "." | "..") || path.ends_with("/.") || path.ends_with("/..")
}

/// Expand a pattern to the paths it matches, the way a bare glob word does —
/// same hidden-entry rule, and no match is the empty list rather than an error.
///
/// The pattern arrives as a *string* rather than as word pieces, so unlike
/// [`expand_word`] there is nothing here to keep literal and nothing to escape:
/// the caller wrote `glob(…)` to ask for the whole string to be read as a
/// pattern. That also means a malformed pattern is an **error** rather than the
/// silent fall-back-to-literal a bare word takes — a word can still be a
/// filename, an explicit `glob()` call cannot.
pub(crate) fn glob_paths(pattern: &str) -> Result<Vec<String>, String> {
    // Compiled here rather than inside the walk so a malformed pattern can carry
    // the crate's own message out; `glob_matches` only reports that it refused.
    glob::Pattern::new(pattern).map_err(|error| error.msg.to_string())?;
    Ok(glob_matches(pattern).unwrap_or_default())
}

/// A directory's immediate entries that pass a file-type filter — the expansion
/// behind `files(DIR)` and `dirs(DIR)` (`DESIGN.md` §"Globbing").
///
/// Built as `DIR/*` so the wrappers inherit globbing's policies rather than
/// growing their own: entries come back sorted, a hidden entry is skipped, and a
/// missing or unreadable directory is the empty list, exactly as the pattern
/// would have been had it been written out.
pub(crate) fn directory_entries(directory: &str, filter: Modifier) -> Result<Vec<String>, String> {
    let Some(pattern) = entries_pattern(directory) else {
        return Ok(Vec::new());
    };
    let paths = glob_paths(&pattern)?;
    Ok(paths
        .into_iter()
        .filter(|path| matches_file_filter(path, filter))
        .collect())
}

/// The `DIR/*` pattern whose matches are `DIR`'s immediate entries, or `None` for
/// a directory that names none.
///
/// `.` contributes no prefix, so `files()` yields `a.txt` rather than `./a.txt` —
/// the spelling a bare `*` would have produced, and the one that reads back as a
/// path relative to where the caller stands. The directory is escaped because it
/// is a **path**, not a pattern: `dirs("src/[old]")` looks inside that directory
/// rather than matching a character class.
fn entries_pattern(directory: &str) -> Option<String> {
    // The empty path names no directory, so it is the empty list a missing one
    // gives. Checked before the trim, which cannot tell it apart from `/`
    // afterwards — and answering `/*` would widen `files("")` from "nothing" to
    // the whole root, the one wrong answer available.
    if directory.is_empty() {
        return None;
    }
    let trimmed = directory.trim_end_matches('/');
    Some(match trimmed {
        // `/` trims to nothing but does name a directory: the root. `.` is the
        // caller's own.
        "" => "/*".to_string(),
        "." => "*".to_string(),
        _ => format!("{}/*", glob::Pattern::escape(trimmed)),
    })
}

/// Escape literal pieces without allowing `-` to become a range operator when
/// it occurs inside an active character class. `glob` has no escape spelling
/// for an in-class hyphen, but treats one at the end of the class literally.
fn glob_pattern(pieces: &Pieces) -> String {
    let mut pattern = String::new();
    let mut in_class = false;
    let mut literal_hyphens = 0;
    let mut class_start = 0;

    for (text, expandable) in pieces {
        if !*expandable {
            if in_class {
                for ch in text.chars() {
                    if ch == '-' {
                        literal_hyphens += 1;
                    } else {
                        pattern.push_str(&glob::Pattern::escape(&ch.to_string()));
                    }
                }
            } else {
                pattern.push_str(&glob::Pattern::escape(text));
            }
            continue;
        }

        for ch in text.chars() {
            if in_class && ch == ']' {
                pattern.insert_str(class_start, &"-".repeat(literal_hyphens));
                literal_hyphens = 0;
                in_class = false;
            } else if !in_class && ch == '[' {
                in_class = true;
                class_start = pattern.len() + 1;
            }
            pattern.push(ch);
        }
    }
    pattern
}

/// Resolve a variable reference to its string value.
///
/// `$env.KEY` reads the process environment (strict: unset is an error), as a
/// list for the path-type names and a string otherwise.
/// `$sh` is the shell's own read-only namespace, resolved as a map so member
/// access, indexing, and modifiers work through the usual paths. `$name` reads
/// the variable store (unbound is an error). Member access on any namespace
/// other than `env` and `sh`, and a bare `$env`, are not supported yet.
pub(crate) fn resolve(vref: &VarRef, vars: &Vars) -> Result<String, ExpandError> {
    match resolve_value(vref, vars)? {
        Value::String(value) => Ok(value),
        // `"$x"` and an argv word see the text: only a renderer reads the
        // attributes (`DESIGN.md` §"Hooks and the prompt").
        Value::Styled(styled) => Ok(styled.text),
        Value::Integer(value) => Ok(value.to_string()),
        Value::Boolean(value) => Ok(value.to_string()),
        Value::List(_) | Value::Map(_) | Value::Regex(_) | Value::Glob(_) => {
            Err(ExpandError::ListNeedsSpread(vref.name.clone()))
        }
        // A stream handle has no byte form at all, so it never crosses to argv
        // or into a string — `DESIGN.md` puts it in the same row as a regex.
        Value::Stream(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "stream handle",
        }),
        Value::Job(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "job handle",
        }),
        Value::Function(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "function value",
        }),
    }
}

pub(crate) fn resolve_value(vref: &VarRef, vars: &Vars) -> Result<Value, ExpandError> {
    // `$env.KEY` consumes its member to name the entry; any further access
    // indexes into the value it read, which is what makes `$env.PATH[0]` and
    // `$env.PATH[1..]` work on the path-type lists.
    let (mut value, accesses) = if vref.name == "env" {
        match vref.accesses.as_slice() {
            [Access::Member(key), rest @ ..] => (
                crate::environ::read(key).ok_or_else(|| ExpandError::UnsetEnv(key.clone()))?,
                rest,
            ),
            // A bare `$env` is the whole table as a map, so the total accessor
            // `$env:get(EDITOR, vim)` needs no rule of its own. A subscript or a
            // slice still has no meaning here: the environment is keyed by name,
            // and `$env[0]` would name whichever entry happened to sort first.
            accesses => (crate::environ::snapshot(), accesses),
        }
    } else if vref.name == "sh" {
        (vars.shell_namespace(), vref.accesses.as_slice())
    } else {
        (
            vars.get(&vref.name)
                .ok_or_else(|| ExpandError::UnboundVar(vref.name.clone()))?
                .clone(),
            vref.accesses.as_slice(),
        )
    };
    for access in accesses {
        // A handle is a live *reference*, not a snapshot taken when it was
        // bound: reading through one looks the job up in the table as it stands
        // now, so `$j.state` cannot go stale the way a captured record would.
        //
        // Per access rather than once up front, because a handle can arrive
        // part-way along a chain: `$sh.jobs[2]` indexes the table *to* one, and
        // then has to be read through in turn.
        //
        // A bare `$j` never reaches here, which is what leaves it with no byte
        // form and lets `kill $j` mean a job where `kill 49001` means a pid.
        if let Value::Job(id) = value {
            value = vars.job_record(id).ok_or(ExpandError::GoneJob { id })?;
        }
        value = match access {
            Access::Member(key) => map_value_access(value, key, &vref.name)?,
            Access::Subscript(subscript) => {
                let key = subscript_key(subscript, vars)?;
                match value {
                    Value::List(values) => {
                        let index = key.parse::<i64>().map_err(|_| {
                            ExpandError::Unsupported("list index must be an integer".into())
                        })?;
                        let offset = if index < 0 {
                            values.len() as i128 + index as i128
                        } else {
                            index as i128
                        };
                        usize::try_from(offset)
                            .ok()
                            .and_then(|offset| values.get(offset))
                            .cloned()
                            .ok_or_else(|| ExpandError::IndexOutOfRange {
                                name: vref.name.clone(),
                                index,
                            })?
                    }
                    Value::Map(_) => map_value_access(value, &key, &vref.name)?,
                    // A styled value indexes exactly as its text does — which is
                    // to say it does not, since mesh has no string subscript.
                    Value::String(_)
                    | Value::Styled(_)
                    | Value::Integer(_)
                    | Value::Boolean(_)
                    | Value::Regex(_)
                    | Value::Glob(_)
                    | Value::Stream(_)
                    | Value::Job(_)
                    | Value::Function(_) => {
                        return Err(ExpandError::NotAList(vref.name.clone()));
                    }
                }
            }
            Access::Slice {
                start,
                end,
                inclusive,
            } => match value {
                Value::List(values) => {
                    Value::List(slice(&values, *start, *end, *inclusive).to_vec())
                }
                _ => return Err(ExpandError::NotAList(vref.name.clone())),
            },
        };
    }
    for step in &vref.modifiers {
        value = apply_modifier_step(value, step)?;
    }
    Ok(value)
}

fn map_value_access(value: Value, key: &str, name: &str) -> Result<Value, ExpandError> {
    match value {
        Value::Map(entries) => entries
            .into_iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
            .ok_or_else(|| ExpandError::NoSuchKey {
                name: name.to_string(),
                key: key.to_string(),
            }),
        _ => Err(ExpandError::Unsupported(format!(
            "${name}: value is not a map"
        ))),
    }
}

pub(crate) fn subscript_key(subscript: &str, vars: &Vars) -> Result<String, ExpandError> {
    if let Some(variable) = subscript.strip_prefix('$') {
        return match vars.get(variable) {
            Some(Value::String(value)) => Ok(value.clone()),
            Some(_) => Err(ExpandError::Unsupported("map key must be a string".into())),
            None => Err(ExpandError::UnboundVar(variable.into())),
        };
    }
    if let Some(value) = subscript
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
    {
        return decode_subscript_string(value, '"');
    }
    if let Some(value) = subscript
        .strip_prefix('\'')
        .and_then(|v| v.strip_suffix('\''))
    {
        return decode_subscript_string(value, '\'');
    }
    Ok(subscript.to_string())
}

fn decode_subscript_string(value: &str, quote: char) -> Result<String, ExpandError> {
    let mut decoded = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| ExpandError::Unsupported("unterminated escape in map key".into()))?;
        decoded.push(match escaped {
            '\\' => '\\',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            c if c == quote => c,
            '$' if quote == '"' => '$',
            _ => return Err(ExpandError::Unsupported("invalid escape in map key".into())),
        });
    }
    Ok(decoded)
}

pub(crate) fn apply_modifier(value: Value, modifier: Modifier) -> Result<Value, ExpandError> {
    use Modifier::{Dedup, First, Init, Keys, Last, Len, Rest, Values};
    let name = modifier_name(modifier);
    // A modifier *transforms* a value, and display attributes are rendering-only,
    // so a styled value is modified as its text and the result is plain — the same
    // rule `+=` follows (`DESIGN.md` §"Hooks and the prompt"). Flattening here is
    // what lets every arm below reason about a string; the `Value::Styled` arms in
    // them are the exhaustiveness the compiler wants, not a reachable case.
    let value = value.plain();
    match modifier {
        Len => match value {
            Value::String(value) => Ok(Value::Integer(value.chars().count() as i64)),
            Value::List(values) => Ok(Value::Integer(values.len() as i64)),
            Value::Map(values) => Ok(Value::Integer(values.len() as i64)),
            Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a string or collection".into(),
            }),
        },
        // Every value is welcome here — the ones without a literal form are
        // refused by the writer itself, naming what it could not write, so this
        // does not restate the list and cannot drift from it.
        Modifier::Repr => {
            value
                .to_literal()
                .map(Value::String)
                .map_err(|kind| ExpandError::Modifier {
                    name: name.into(),
                    message: kind.to_string(),
                })
        }
        Keys => match value {
            Value::Map(values) => Ok(Value::List(
                values.into_iter().map(|(k, _)| Value::String(k)).collect(),
            )),
            _ => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a map".into(),
            }),
        },
        Values => match value {
            Value::Map(values) => Ok(Value::List(values.into_iter().map(|(_, v)| v).collect())),
            _ => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a map".into(),
            }),
        },
        First | Last => match value {
            Value::List(values) => values
                .first()
                .filter(|_| modifier == First)
                .or_else(|| values.last().filter(|_| modifier == Last))
                .cloned()
                .ok_or_else(|| ExpandError::Modifier {
                    name: name.into(),
                    message: "empty list has no element".into(),
                }),
            Value::String(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
            Value::Map(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
        },
        Rest | Init => match value {
            Value::List(values) => {
                let range = if modifier == Rest {
                    1.min(values.len())..values.len()
                } else {
                    0..values.len().saturating_sub(1)
                };
                Ok(Value::List(values[range].to_vec()))
            }
            Value::String(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
            Value::Map(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
        },
        Dedup => match value {
            Value::List(values) => {
                let mut seen = std::collections::HashSet::new();
                Ok(Value::List(
                    values
                        .into_iter()
                        .filter(|v| seen.insert(v.clone()))
                        .collect(),
                ))
            }
            Value::String(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
            Value::Map(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a list".into(),
            }),
        },
        // `$sh.stdin:tty` — the `test -t N` replacement, and the reason a handle
        // is a value at all: it answers questions rather than being one. The
        // descriptor stays inside `Value::Stream`, so an integer is refused.
        Modifier::Tty => match value {
            Value::Stream(fd) => Ok(Value::Boolean({
                // SAFETY: `isatty` only inspects the descriptor and cannot fail
                // in a way that matters here — a bad one answers "no".
                unsafe { libc::isatty(fd) == 1 }
            })),
            // A scalar question maps element-wise over a list, the same way the
            // file tests below do.
            Value::List(values) => values
                .into_iter()
                .map(|value| apply_modifier(value, modifier))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            _ => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a stream handle, such as `$sh.stdin`".into(),
            }),
        },
        Modifier::Int => match value {
            Value::String(value) => {
                value
                    .parse()
                    .map(Value::Integer)
                    .map_err(|_| ExpandError::Modifier {
                        name: name.into(),
                        message: format!("cannot parse `{value}` as an integer"),
                    })
            }
            _ => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a string".into(),
            }),
        },
        // Resolving is a syscall, not string surgery: every component on the way
        // has to exist for the kernel to follow it, so a path that is not there has
        // no real path to report and this errors rather than inventing one. `:type`
        // is the other file modifier that errors, for the same reason — the
        // remaining ones answer a yes/no question, which a missing file can still
        // answer with `false`. The error carries the OS's own words, so a resolvable
        // path refused for permissions does not read as a missing one.
        Modifier::Real => match value {
            Value::String(path) => std::fs::canonicalize(&path)
                .map(|resolved| Value::String(resolved.to_string_lossy().into_owned()))
                .map_err(|error| ExpandError::Modifier {
                    name: name.into(),
                    message: format!("`{path}`: {error}"),
                }),
            Value::List(values) => values
                .into_iter()
                .map(|value| apply_modifier(value, modifier))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Value::Map(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a path".into(),
            }),
        },
        // A file test asks one question of one path, so on a list it maps
        // element-wise like any other value modifier.
        Modifier::Exists | Modifier::Type | Modifier::Read | Modifier::Write => match value {
            Value::String(path) => {
                file_test(&path, modifier).ok_or_else(|| ExpandError::Modifier {
                    name: name.into(),
                    message: format!("no such file: `{path}`"),
                })
            }
            Value::List(values) => values
                .into_iter()
                .map(|value| apply_modifier(value, modifier))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Value::Map(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a path".into(),
            }),
        },
        // A filter keeps the matching elements of a list and drops the rest. On a
        // single path it is the scalar predicate the matching `test` operator asks
        // (`:f` is `-f`, `:d` is `-d`, `:l` is `-L`, `:x` is `-x`) — which is also
        // what makes `$paths:filter(:x)` work, since `:filter` hands it one element
        // at a time.
        Modifier::Files | Modifier::Dirs | Modifier::Links | Modifier::Exec => match value {
            Value::String(path) => Ok(Value::Boolean(matches_file_filter(&path, modifier))),
            Value::List(values) => {
                let mut kept = Vec::with_capacity(values.len());
                for value in values {
                    let Value::String(path) = &value else {
                        return Err(ExpandError::Modifier {
                            name: name.into(),
                            message: "requires a list of paths".into(),
                        });
                    };
                    if matches_file_filter(path, modifier) {
                        kept.push(value);
                    }
                }
                Ok(Value::List(kept))
            }
            Value::Map(_)
            | Value::Styled(_)
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a path or a list of paths".into(),
            }),
        },
        // A **split** modifier, so it consumes exactly one string rather than
        // mapping element-wise: `$line:words` and `$line:split(" ")` have to agree
        // about what a list subject means, and `split_value` refuses one.
        Modifier::Words => words_value(value),
        // The same family, and the same one-string rule: each is `:split(SEP)` with
        // the separator the name spells, so a subject that `:split` refuses is
        // refused here too and the diagnostic names the modifier the reader wrote.
        Modifier::Lines | Modifier::Nulls | Modifier::Tabs => {
            split_named(value, name, fixed_separator(modifier))
        }
        _ => match value {
            Value::String(value) => Ok(Value::String(modify_string(value, modifier))),
            Value::List(values) => values
                .into_iter()
                .map(|value| apply_modifier(value, modifier))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            Value::Map(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "cannot map over a map".into(),
            }),
            Value::Styled(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Job(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "cannot apply string modifier to this value".into(),
            }),
            Value::Integer(_) | Value::Boolean(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a string".into(),
            }),
        },
    }
}

/// `:split(SEP)` — turn a string into a list by splitting on the literal
/// separator `separator`. The separator is a **terminator, not a separator**:
/// a trailing run of empty fields is dropped (`"a:b:"` → `[a b]`), while interior
/// empties are kept (`"a::b"` → `[a "" b]`). An empty string — or one that is
/// only separators — yields the empty list. Maps over neither lists nor maps: it
/// consumes exactly one string (per `DESIGN.md`, split modifiers act on a single
/// string/capture, not element-wise).
pub(crate) fn split_value(value: Value, separator: &str) -> Result<Value, ExpandError> {
    if separator.is_empty() {
        return Err(ExpandError::Modifier {
            name: "split".into(),
            message: "separator must not be empty".into(),
        });
    }
    split_named(value, "split", separator)
}

/// The separator each fixed-separator split modifier spells.
fn fixed_separator(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Lines => "\n",
        Modifier::Nulls => "\0",
        Modifier::Tabs => "\t",
        // `split_named`'s callers pick the separator; only the three above have one
        // baked into the name, and the dispatch above reaches here with no other.
        _ => unreachable!("not a fixed-separator split modifier"),
    }
}

/// [`split_value`] with the modifier name to blame in a diagnostic — `:nulls` says
/// `:nulls`, not `:split`. The separator is never empty on these paths.
fn split_named(value: Value, name: &str, separator: &str) -> Result<Value, ExpandError> {
    let Value::String(text) = value else {
        return Err(ExpandError::Modifier {
            name: name.into(),
            message: "requires a string".into(),
        });
    };
    Ok(split_text(&text, separator))
}

/// Split `text` on `separator` with **terminator** semantics, the one place that
/// rule lives: every split modifier and the default capture split come through
/// here, so they cannot come to disagree about a trailing blank.
pub(crate) fn split_text(text: &str, separator: &str) -> Value {
    let mut fields: Vec<Value> = text
        .split(separator)
        .map(|s| Value::String(s.into()))
        .collect();
    // Drop the trailing run of empty fields (terminator semantics).
    while matches!(fields.last(), Some(Value::String(s)) if s.is_empty()) {
        fields.pop();
    }
    Value::List(fields)
}

/// `:words` — turn a string into a list by splitting on runs of whitespace, the
/// classic IFS word-split (`DESIGN.md` §"Modifiers"). Unlike `:split(SEP)` it
/// yields no empty elements at all: leading, trailing and interior runs are each
/// one boundary, so a column-padded line — what `getent`, `ip -o` and `df` all
/// produce — comes apart into its columns without a caller dropping empties by
/// hand. A string that is empty or all whitespace yields the empty list.
///
/// **ASCII** whitespace, not Unicode: a non-breaking space is *data* — it turns
/// up inside filenames and in program output — and splitting a field on one would
/// corrupt it with no way for the caller to opt out. The set is written out rather
/// than taken from `split_ascii_whitespace`, which omits the vertical tab; the
/// three that matter (` `, `\t`, `\n`) are what IFS and awk mean by whitespace,
/// and the rest are here so the set has no arbitrary hole.
///
/// Consumes exactly one string, as `split_value` does: these two are the same
/// family and a list subject cannot mean one thing to one and something else to
/// the other.
pub(crate) fn words_value(value: Value) -> Result<Value, ExpandError> {
    let Value::String(text) = value else {
        return Err(ExpandError::Modifier {
            name: "words".into(),
            message: "requires a string".into(),
        });
    };
    Ok(Value::List(
        text.split(is_field_separator)
            .filter(|word| !word.is_empty())
            .map(|word| Value::String(word.into()))
            .collect(),
    ))
}

/// The whitespace `:words` splits on — every ASCII whitespace character.
fn is_field_separator(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// `:join(SEP)` — fold a list back into a single string, placing `separator`
/// between elements. Each element is stringified (string as-is, integer and
/// boolean rendered); a nested list or map is a fail-loud error, as there is no
/// implicit deep flattening (per `DESIGN.md`).
pub(crate) fn join_value(value: Value, separator: &str) -> Result<Value, ExpandError> {
    let Value::List(items) = value else {
        return Err(ExpandError::Modifier {
            name: "join".into(),
            message: "requires a list".into(),
        });
    };
    let mut out = String::new();
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            out.push_str(separator);
        }
        match item {
            Value::String(s) => out.push_str(&s),
            // `:join` builds a string, so a styled element contributes its text —
            // the joined result is plain, as `+=` is.
            Value::Styled(styled) => out.push_str(&styled.text),
            Value::Integer(n) => out.push_str(&n.to_string()),
            Value::Boolean(b) => out.push_str(&b.to_string()),
            Value::List(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a nested list".into(),
                });
            }
            Value::Map(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a map element".into(),
                });
            }
            Value::Function(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a function value".into(),
                });
            }
            Value::Regex(_) | Value::Glob(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a pattern element".into(),
                });
            }
            Value::Stream(_) | Value::Job(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a stream handle".into(),
                });
            }
        }
    }
    Ok(Value::String(out))
}

/// Run a per-string transform over a value, mapping element-wise across a list
/// the way the argument-free string modifiers do (`DESIGN.md` §"String": a
/// replace modifier "is a value modifier, so it maps over a list element-wise
/// like `:stem`"). Shared by the whole affix and replace family so they cannot
/// disagree about which values they accept.
///
/// A styled subject is flattened to its text before `transform` sees it, matching
/// [`apply_modifier`]; the arms below are the exhaustiveness the compiler wants.
pub(crate) fn map_strings(
    value: Value,
    name: &str,
    transform: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<Value, ExpandError> {
    let fail = |message: &str| ExpandError::Modifier {
        name: name.into(),
        message: message.into(),
    };
    match value.plain() {
        Value::String(text) => transform(&text)
            .map(Value::String)
            .map_err(|message| fail(&message)),
        Value::List(values) => values
            .into_iter()
            .map(|value| map_strings(value, name, transform))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::List),
        Value::Map(_) => Err(fail("cannot map over a map")),
        Value::Integer(_) | Value::Boolean(_) => Err(fail("requires a string")),
        Value::Styled(_)
        | Value::Regex(_)
        | Value::Glob(_)
        | Value::Stream(_)
        | Value::Job(_)
        | Value::Function(_) => Err(fail("cannot apply a string modifier to this value")),
    }
}

/// `:get(KEY, DEFAULT)` — the **total** accessor. Where `$m.key` and `$xs[i]`
/// fail loudly on a miss, this answers `default`, which is what makes it the
/// mesh spelling of `${VAR:-default}` (`DESIGN.md` §"Arrays / lists").
///
/// A map takes a string key and a list an integer index, negative counting from
/// the end as a subscript does. Asking a map for an integer — or a list for a
/// name — is a loud error rather than a silent `default`: a key of the wrong
/// *type* is a mistake in the program, not an absence in the data, and returning
/// the default would hide it forever.
pub(crate) fn get_value(value: Value, key: Value, default: Value) -> Result<Value, ExpandError> {
    let fail = |message: String| ExpandError::Modifier {
        name: "get".into(),
        message,
    };
    // The key is flattened like the subject: display attributes are rendering-only,
    // so a `style()`d key is the string it shows, and looking one up must not
    // depend on how it happens to be colored. `default` is left alone — it is
    // returned as a value rather than consumed as text.
    let key = key.plain();
    match value.plain() {
        Value::Map(entries) => {
            let key = match key {
                Value::String(key) => key,
                other => {
                    return Err(fail(format!(
                        "a map key is a string, got {}",
                        value_kind(&other)
                    )));
                }
            };
            Ok(entries
                .into_iter()
                .find(|(name, _)| *name == key)
                .map_or(default, |(_, found)| found))
        }
        Value::List(values) => {
            let index = match key {
                Value::Integer(index) => index,
                other => {
                    return Err(fail(format!(
                        "a list index is an integer, got {}",
                        value_kind(&other)
                    )));
                }
            };
            let offset = if index < 0 {
                values.len() as i128 + i128::from(index)
            } else {
                i128::from(index)
            };
            Ok(usize::try_from(offset)
                .ok()
                .and_then(|offset| values.into_iter().nth(offset))
                .unwrap_or(default))
        }
        other => Err(fail(format!(
            "requires a map or a list, got {}",
            value_kind(&other)
        ))),
    }
}

/// The word a diagnostic uses for a value's type. Local to the modifiers here so
/// they can name what they were handed without reaching into the runtime.
fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "a string",
        Value::Styled(_) => "a styled string",
        Value::Integer(_) => "an integer",
        Value::Boolean(_) => "a boolean",
        Value::List(_) => "a list",
        Value::Map(_) => "a map",
        Value::Regex(_) => "a regex",
        Value::Glob(_) => "a glob",
        Value::Stream(_) => "a stream handle",
        Value::Job(_) => "a job handle",
        Value::Function(_) => "a function value",
    }
}

fn modifier_name(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Dir => "dir",
        Modifier::Base => "base",
        Modifier::Ext => "ext",
        Modifier::Exts => "exts",
        Modifier::Stem => "stem",
        Modifier::Bare => "bare",
        Modifier::Real => "real",
        Modifier::Len => "len",
        Modifier::First => "first",
        Modifier::Last => "last",
        Modifier::Rest => "rest",
        Modifier::Init => "init",
        Modifier::Dedup => "dedup",
        Modifier::Keys => "keys",
        Modifier::Values => "values",
        Modifier::Upper => "upper",
        Modifier::Lower => "lower",
        Modifier::TrimStart => "trimstart",
        Modifier::TrimEnd => "trimend",
        Modifier::Int => "int",
        Modifier::Words => "words",
        Modifier::Lines => "lines",
        Modifier::Nulls => "nulls",
        Modifier::Tabs => "tabs",
        Modifier::Repr => "repr",
        Modifier::Exists => "exists",
        Modifier::Type => "type",
        Modifier::Read => "read",
        Modifier::Write => "write",
        Modifier::Files => "files",
        Modifier::Dirs => "dirs",
        Modifier::Links => "links",
        Modifier::Exec => "exec",
        Modifier::Tty => "tty",
    }
}

/// Ask the kernel, rather than reinterpreting permission bits by hand — bits alone
/// get root and ACLs wrong.
///
/// `AT_EACCESS` is load-bearing: plain `access(2)` answers for the **real** UID/GID,
/// so a process that has dropped its effective privileges while keeping a saved ID
/// would be told it can read a file that an `open` then refuses. This is the
/// question `test -r` answers (coreutils reaches for `euidaccess` for the same
/// reason), and the one that predicts what the shell can actually do.
fn accessible(path: &str, mode: libc::c_int) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path) else {
        // An interior NUL cannot name a file, so nothing is accessible.
        return false;
    };
    accessible_c(&c_path, mode)
}

/// The syscall itself, split out so a caller that must not allocate — the forked
/// child in the effective-user test — can reach it with a string built beforehand.
fn accessible_c(path: &std::ffi::CStr, mode: libc::c_int) -> bool {
    // SAFETY: `path` is a valid NUL-terminated string for the duration of the call.
    // No `AT_SYMLINK_NOFOLLOW`, so this dereferences like every other file test.
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), mode, libc::AT_EACCESS) == 0 }
}

/// The `find -type` word for a path, or `None` when it does not exist.
///
/// Read with `symlink_metadata`, so a symlink reports `link` rather than its
/// target's type — `:type == link` is how you ask about the link itself, while every
/// other test dereferences (`DESIGN.md`).
fn file_type_word(path: &str) -> Option<&'static str> {
    use std::os::unix::fs::FileTypeExt;
    let kind = std::fs::symlink_metadata(path).ok()?.file_type();
    Some(if kind.is_symlink() {
        "link"
    } else if kind.is_dir() {
        "dir"
    } else if kind.is_file() {
        "file"
    } else if kind.is_fifo() {
        "fifo"
    } else if kind.is_socket() {
        "socket"
    } else if kind.is_block_device() {
        "block"
    } else if kind.is_char_device() {
        "char"
    } else {
        "unknown"
    })
}

/// Answer a file test for one path. `None` only for `:type` on a path that does not
/// exist, which has no word to report.
fn file_test(path: &str, modifier: Modifier) -> Option<Value> {
    Some(match modifier {
        // `metadata` follows symlinks, so a broken link does not exist — the answer
        // `test -e` gives.
        Modifier::Exists => Value::Boolean(std::fs::metadata(path).is_ok()),
        Modifier::Type => Value::String(file_type_word(path)?.to_string()),
        Modifier::Read => Value::Boolean(accessible(path, libc::R_OK)),
        Modifier::Write => Value::Boolean(accessible(path, libc::W_OK)),
        _ => return None,
    })
}

/// Does this path match a file-type filter? Everything but `:links` dereferences,
/// as `test -f` and `test -d` do.
fn matches_file_filter(path: &str, modifier: Modifier) -> bool {
    match modifier {
        Modifier::Files => std::fs::metadata(path).is_ok_and(|m| m.is_file()),
        Modifier::Dirs => std::fs::metadata(path).is_ok_and(|m| m.is_dir()),
        Modifier::Links => std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()),
        Modifier::Exec => accessible(path, libc::X_OK),
        _ => false,
    }
}

fn modify_string(value: String, modifier: Modifier) -> String {
    use std::path::Path;
    let path = Path::new(&value);
    match modifier {
        Modifier::Dir => path.parent().map_or_else(
            || if path.has_root() { "/" } else { "." }.to_string(),
            |p| {
                let parent = p.to_string_lossy();
                if parent.is_empty() {
                    ".".into()
                } else {
                    parent.into_owned()
                }
            },
        ),
        Modifier::Base => path
            .file_name()
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
        Modifier::Ext => path
            .extension()
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
        Modifier::Stem => path
            .file_stem()
            .map_or_else(String::new, |p| p.to_string_lossy().into_owned()),
        Modifier::Exts => extensions(path.file_name().and_then(|p| p.to_str())).to_string(),
        Modifier::Bare => bare_name(path.file_name().and_then(|p| p.to_str())).to_string(),
        Modifier::Upper => value.to_uppercase(),
        Modifier::Lower => value.to_lowercase(),
        Modifier::TrimStart => value.trim_start().to_string(),
        Modifier::TrimEnd => value.trim_end().to_string(),
        // A path component in `DESIGN.md`'s table, but it reads the filesystem and
        // can fail, so it is handled where a `Result` is available rather than here.
        Modifier::Real
        | Modifier::Int
        | Modifier::Words
        | Modifier::Lines
        | Modifier::Nulls
        | Modifier::Tabs
        | Modifier::Len
        | Modifier::First
        | Modifier::Last
        | Modifier::Rest
        | Modifier::Init
        | Modifier::Dedup
        | Modifier::Keys
        | Modifier::Values
        | Modifier::Exists
        | Modifier::Type
        | Modifier::Read
        | Modifier::Write
        | Modifier::Files
        | Modifier::Dirs
        | Modifier::Links
        | Modifier::Exec
        // `:repr` answers for every type, so it is applied to the whole value
        // rather than routed through the string path like `:upper`.
        | Modifier::Repr
        | Modifier::Tty => unreachable!("non-string modifier handled separately"),
    }
}

fn extensions(name: Option<&str>) -> &str {
    let Some(name) = name else { return "" };
    name.strip_prefix('.')
        .unwrap_or(name)
        .split_once('.')
        .map_or("", |(_, extensions)| extensions)
}

fn bare_name(name: Option<&str>) -> &str {
    let Some(name) = name else { return "" };
    let offset = usize::from(name.starts_with('.'));
    name[offset..]
        .find('.')
        .map_or(name, |dot| &name[..offset + dot])
}

pub(crate) fn slice<T>(
    values: &[T],
    start: Option<i64>,
    end: Option<i64>,
    inclusive: bool,
) -> &[T] {
    let len = values.len() as i128;
    let clamp = |bound: i64, inclusive| -> usize {
        let bound = bound as i128;
        let offset = if bound < 0 { len + bound } else { bound } + i128::from(inclusive);
        offset.clamp(0, len) as usize
    };
    let start = start.map_or(0, |bound| clamp(bound, false));
    let end = end.map_or(values.len(), |bound| clamp(bound, inclusive));
    if start >= end {
        &values[0..0]
    } else {
        &values[start..end]
    }
}

/// The literal value of a word: its piece texts concatenated.
fn literal(pieces: &Pieces) -> String {
    pieces.iter().map(|(t, _)| t.as_str()).collect()
}

/// Replace a leading expandable `~`/`~/…` with `$HOME` (kept literal). A quoted
/// or interpolated leading `~` is not expandable and is skipped.
fn apply_tilde(pieces: &mut Pieces) {
    let Some((text, true)) = pieces.first().map(|(t, e)| (t.clone(), *e)) else {
        return;
    };
    if text == "~" {
        let next = pieces.iter().skip(1).find(|(text, _)| !text.is_empty());
        if (next.is_none() || next.is_some_and(|(text, _)| text.starts_with('/')))
            && let Some(home) = home()
        {
            pieces[0] = (home, false);
        }
    } else if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = home()
    {
        pieces[0] = (home, false);
        pieces.insert(1, (format!("/{rest}"), true));
    }
}

fn home() -> Option<String> {
    env::var_os("HOME").map(|h| h.to_string_lossy().into_owned())
}

fn has_glob_meta(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '*' | '?' | '['))
}

#[cfg(test)]
mod tests {
    use super::{
        Access, ExpandError, Modifier, ModifierStep, VarRef, accessible_c, apply_modifier,
        apply_tilde, entries_pattern, get_value, has_glob_meta, join_value, map_strings,
        resolve_value, split_value, words_value,
    };
    use crate::vars::{Value, Vars};

    fn list(items: &[&str]) -> Value {
        Value::List(items.iter().map(|s| Value::String((*s).into())).collect())
    }

    #[test]
    fn split_drops_only_the_trailing_empty_run() {
        assert_eq!(
            split_value(Value::String("a:b:c".into()), ":"),
            Ok(list(&["a", "b", "c"]))
        );
        // A trailing run of separators contributes no fields (terminator, not separator).
        assert_eq!(
            split_value(Value::String("a:b:".into()), ":"),
            Ok(list(&["a", "b"]))
        );
        assert_eq!(
            split_value(Value::String("a:b::".into()), ":"),
            Ok(list(&["a", "b"]))
        );
        // Interior empties survive.
        assert_eq!(
            split_value(Value::String("a::b".into()), ":"),
            Ok(list(&["a", "", "b"]))
        );
    }

    #[test]
    fn split_of_empty_or_all_separators_is_the_empty_list() {
        assert_eq!(
            split_value(Value::String(String::new()), ":"),
            Ok(Value::List(vec![]))
        );
        assert_eq!(
            split_value(Value::String("::".into()), ":"),
            Ok(Value::List(vec![]))
        );
    }

    #[test]
    fn split_supports_a_multi_character_separator() {
        assert_eq!(
            split_value(Value::String("a::b::c".into()), "::"),
            Ok(list(&["a", "b", "c"]))
        );
    }

    #[test]
    fn split_rejects_an_empty_separator_and_non_strings() {
        assert!(matches!(
            split_value(Value::String("abc".into()), ""),
            Err(ExpandError::Modifier { name, .. }) if name == "split"
        ));
        assert!(matches!(
            split_value(list(&["a", "b"]), ":"),
            Err(ExpandError::Modifier { name, .. }) if name == "split"
        ));
    }

    /// The three fixed-separator members are `:split(SEP)` with the name choosing
    /// the separator, so the terminator rule holds for them without restating it.
    #[test]
    fn the_fixed_separator_splits_spell_their_own_separator() {
        let cases = [
            (Modifier::Lines, "a\nb\n"),
            (Modifier::Nulls, "a\0b\0"),
            (Modifier::Tabs, "a\tb\t"),
        ];
        for (modifier, text) in cases {
            assert_eq!(
                apply_modifier(Value::String(text.into()), modifier),
                Ok(list(&["a", "b"])),
                "{modifier:?} split {text:?} wrong"
            );
        }
    }

    /// The point of `:nulls`: it splits on NUL and nothing else, so a `find -print0`
    /// name holding a newline arrives whole rather than torn at the newline.
    #[test]
    fn nulls_leaves_a_newline_inside_a_field_alone() {
        assert_eq!(
            apply_modifier(Value::String("we\nird\0plain\0".into()), Modifier::Nulls),
            Ok(list(&["we\nird", "plain"]))
        );
    }

    /// A split consumes one string, so the diagnostic has to name the modifier the
    /// reader wrote — blaming `:split` for a `:nulls` sends them to the wrong line.
    #[test]
    fn a_fixed_separator_split_refuses_a_list_under_its_own_name() {
        assert!(matches!(
            apply_modifier(list(&["a", "b"]), Modifier::Nulls),
            Err(ExpandError::Modifier { name, .. }) if name == "nulls"
        ));
    }

    #[test]
    fn words_splits_on_runs_and_never_yields_an_empty_element() {
        assert_eq!(
            words_value(Value::String("a b c".into())),
            Ok(list(&["a", "b", "c"]))
        );
        // The difference from `:split(" ")`, and the whole reason it exists: a run
        // is one boundary, so a column-padded line comes apart into its columns.
        assert_eq!(
            words_value(Value::String("a   b  c".into())),
            Ok(list(&["a", "b", "c"]))
        );
        // Leading and trailing whitespace contribute nothing at either end, unlike
        // `:split`, which drops only the trailing run.
        assert_eq!(
            words_value(Value::String("   a b   ".into())),
            Ok(list(&["a", "b"]))
        );
    }

    #[test]
    fn words_takes_the_whole_ascii_whitespace_set() {
        assert_eq!(
            words_value(Value::String("a\tb\nc\r\x0bd\x0ce".into())),
            Ok(list(&["a", "b", "c", "d", "e"]))
        );
    }

    #[test]
    fn words_leaves_a_non_breaking_space_in_the_field() {
        // U+00A0 is data, not a separator — `char::is_whitespace` would split here
        // and corrupt a filename that contains one.
        assert_eq!(
            words_value(Value::String("a\u{a0}b c".into())),
            Ok(list(&["a\u{a0}b", "c"]))
        );
    }

    #[test]
    fn words_of_empty_or_all_whitespace_is_the_empty_list() {
        assert_eq!(
            words_value(Value::String(String::new())),
            Ok(Value::List(vec![]))
        );
        assert_eq!(
            words_value(Value::String("  \t\n ".into())),
            Ok(Value::List(vec![]))
        );
    }

    #[test]
    fn words_rejects_a_non_string_the_way_split_does() {
        // Both are split modifiers, so a list subject has to mean the same thing to
        // each of them — element-wise mapping for one and not the other would be a
        // trap.
        assert!(matches!(
            words_value(list(&["a b", "c d"])),
            Err(ExpandError::Modifier { name, .. }) if name == "words"
        ));
        assert!(matches!(
            words_value(Value::Integer(42)),
            Err(ExpandError::Modifier { name, .. }) if name == "words"
        ));
    }

    #[test]
    fn words_reaches_the_modifier_table_by_name() {
        assert_eq!(Modifier::from_name("words"), Some(Modifier::Words));
        assert_eq!(
            apply_modifier(Value::String("a  b".into()), Modifier::Words),
            Ok(list(&["a", "b"]))
        );
    }

    #[test]
    fn join_folds_a_list_and_stringifies_scalars() {
        assert_eq!(
            join_value(list(&["/usr/bin", "/bin"]), ":"),
            Ok(Value::String("/usr/bin:/bin".into()))
        );
        assert_eq!(
            join_value(
                Value::List(vec![
                    Value::Integer(1),
                    Value::Integer(2),
                    Value::Boolean(true)
                ]),
                "+",
            ),
            Ok(Value::String("1+2+true".into()))
        );
        assert_eq!(
            join_value(Value::List(vec![]), ","),
            Ok(Value::String(String::new()))
        );
    }

    #[test]
    fn get_answers_the_default_only_when_the_key_is_absent() {
        let map = Value::Map(vec![
            ("EDITOR".into(), Value::String("vim".into())),
            ("EMPTY".into(), Value::String(String::new())),
        ]);
        assert_eq!(
            get_value(
                map.clone(),
                Value::String("EDITOR".into()),
                Value::String("nano".into())
            ),
            Ok(Value::String("vim".into()))
        );
        assert_eq!(
            get_value(
                map.clone(),
                Value::String("PAGER".into()),
                Value::String("less".into())
            ),
            Ok(Value::String("less".into()))
        );
        // A key bound to `""` is *present*, so it wins over the default — the one
        // place this differs from bash's `${EMPTY:-less}`, which substitutes.
        assert_eq!(
            get_value(
                map,
                Value::String("EMPTY".into()),
                Value::String("less".into())
            ),
            Ok(Value::String(String::new()))
        );
    }

    #[test]
    fn get_indexes_a_list_from_either_end_and_falls_back_past_it() {
        let xs = list(&["a", "b", "c"]);
        let fallback = || Value::String("-".into());
        assert_eq!(
            get_value(xs.clone(), Value::Integer(1), fallback()),
            Ok(Value::String("b".into()))
        );
        assert_eq!(
            get_value(xs.clone(), Value::Integer(-1), fallback()),
            Ok(Value::String("c".into()))
        );
        assert_eq!(
            get_value(xs.clone(), Value::Integer(99), fallback()),
            Ok(fallback())
        );
        assert_eq!(
            get_value(xs, Value::Integer(-99), fallback()),
            Ok(fallback())
        );
    }

    #[test]
    fn get_refuses_a_key_of_the_wrong_type_rather_than_defaulting() {
        // A name asked of a list — or an index asked of a map — is a mistake in the
        // program, not an absence in the data, so answering `default` would bury it.
        assert!(matches!(
            get_value(list(&["a"]), Value::String("a".into()), Value::Integer(0)),
            Err(ExpandError::Modifier { name, .. }) if name == "get"
        ));
        assert!(matches!(
            get_value(
                Value::Map(vec![("k".into(), Value::Integer(1))]),
                Value::Integer(0),
                Value::Integer(0)
            ),
            Err(ExpandError::Modifier { name, .. }) if name == "get"
        ));
        assert!(matches!(
            get_value(Value::String("s".into()), Value::Integer(0), Value::Integer(0)),
            Err(ExpandError::Modifier { name, .. }) if name == "get"
        ));
    }

    #[test]
    fn a_string_transform_maps_over_a_list_and_refuses_a_map() {
        let mut upper = |text: &str| Ok(text.to_uppercase());
        assert_eq!(
            map_strings(list(&["a", "b"]), "t", &mut upper),
            Ok(list(&["A", "B"]))
        );
        assert_eq!(
            map_strings(Value::String("a".into()), "t", &mut upper),
            Ok(Value::String("A".into()))
        );
        assert!(matches!(
            map_strings(Value::Map(vec![]), "t", &mut upper),
            Err(ExpandError::Modifier { name, .. }) if name == "t"
        ));
    }

    #[test]
    fn trim_peels_whitespace_from_the_named_end_only() {
        assert_eq!(
            apply_modifier(Value::String("  hi  ".into()), Modifier::TrimStart),
            Ok(Value::String("hi  ".into()))
        );
        assert_eq!(
            apply_modifier(Value::String("  hi  ".into()), Modifier::TrimEnd),
            Ok(Value::String("  hi".into()))
        );
        // A string transform, so it maps element-wise like `:upper`.
        assert_eq!(
            apply_modifier(list(&[" a", " b"]), Modifier::TrimStart),
            Ok(list(&["a", "b"]))
        );
    }

    #[test]
    fn split_then_join_round_trips_without_a_trailing_separator() {
        let split = split_value(Value::String("a,b,c".into()), ",").unwrap();
        assert_eq!(join_value(split, ","), Ok(Value::String("a,b,c".into())));
    }

    #[test]
    fn join_then_split_is_lossy_on_a_trailing_empty_element() {
        // `:split` trims the trailing empty field, so the two are not exact
        // inverses — a final "" does not survive a round trip.
        let joined = join_value(list(&["a", ""]), ":").unwrap();
        assert_eq!(joined, Value::String("a:".into()));
        assert_eq!(split_value(joined, ":"), Ok(list(&["a"])));
    }

    #[test]
    fn join_rejects_non_lists_and_nested_collections() {
        assert!(matches!(
            join_value(Value::String("hi".into()), ","),
            Err(ExpandError::Modifier { name, .. }) if name == "join"
        ));
        assert!(matches!(
            join_value(Value::List(vec![list(&["a"])]), ","),
            Err(ExpandError::Modifier { name, .. }) if name == "join"
        ));
    }

    #[test]
    fn detects_glob_metacharacters() {
        assert!(has_glob_meta("*.rs"));
        assert!(!has_glob_meta("plain.txt"));
    }

    #[test]
    fn directory_entries_glob_the_directorys_own_children() {
        // `.` contributes no prefix, so the entries read as a bare `*` would spell
        // them; every other directory is a *path*, so its own metacharacters are
        // escaped rather than matched.
        assert_eq!(entries_pattern("."), Some("*".to_string()));
        assert_eq!(entries_pattern("src"), Some("src/*".to_string()));
        // A trailing slash is the same directory, not an empty child component.
        assert_eq!(entries_pattern("src/"), Some("src/*".to_string()));
        assert_eq!(
            entries_pattern("src/[old]"),
            Some("src/[[]old[]]/*".to_string())
        );
        // `/` names the root; the empty path names no directory at all, and the
        // two must not collapse into each other once the trailing slash is gone.
        assert_eq!(entries_pattern("/"), Some("/*".to_string()));
        assert_eq!(entries_pattern("//"), Some("/*".to_string()));
        assert_eq!(entries_pattern(""), None);
    }

    #[test]
    fn quoted_tilde_is_not_expanded() {
        let mut pieces = vec![("~".to_string(), false)];
        apply_tilde(&mut pieces);
        assert_eq!(pieces, vec![("~".to_string(), false)]);
    }

    #[test]
    fn environment_values_receive_command_word_modifiers() {
        let key = "MESH_TEST_ENV_MODIFIER";
        // SAFETY: this test uses a process-specific key that no other test reads.
        unsafe { std::env::set_var(key, "abcd") };
        let reference = VarRef {
            name: "env".into(),
            accesses: vec![Access::Member(key.into())],
            modifiers: vec![ModifierStep::Apply {
                modifier: Modifier::Len,
                name: "len".into(),
            }],
            quoted: false,
        };

        assert_eq!(
            resolve_value(&reference, &Vars::new()),
            Ok(Value::Integer(4))
        );
        // SAFETY: the test owns this process-specific key.
        unsafe { std::env::remove_var(key) };
    }

    /// `accessible` must answer for the **effective** user. Plain `access(2)` uses
    /// the *real* UID/GID, so a process that has dropped effective privileges while
    /// keeping a saved ID is told it can read a file `open` then refuses — the state
    /// this reproduces. `AT_EACCESS` is what makes the answer match reality.
    ///
    /// Creating that split requires privileges, so where the suite cannot the test
    /// has nothing to observe: the two answers only differ once they do.
    #[test]
    fn accessible_answers_for_the_effective_user() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let dir = std::env::temp_dir().join(format!("mesh_eaccess_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("root-only");
        std::fs::write(&path, "x").expect("write fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict fixture");
        // Built before the fork: allocating in a forked child of a threaded process
        // can deadlock on a malloc lock the parent held.
        let c_path = std::ffi::CString::new(path.to_str().expect("utf-8 path")).unwrap();

        assert!(
            accessible_c(&c_path, libc::R_OK),
            "root should read its own file"
        );

        // A child, so the dropped privileges cannot reach the rest of the suite.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            // The real UID stays 0 while the effective one drops, which is exactly
            // the state `access(2)` answers wrongly. `setreuid` rather than
            // `setresuid` because only the former is in libc's portable surface;
            // the saved ID it also moves does not matter to a child that exits here.
            let dropped = unsafe { libc::setreuid(0, NOBODY) };
            let correct = dropped == 0 && !accessible_c(&c_path, libc::R_OK);
            unsafe { libc::_exit(i32::from(!correct)) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "a file readable only by root must not be `:read` once euid is dropped \
             (child status {status})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The conventional unprivileged UID, and the one this test only ever *drops* to.
    const NOBODY: libc::uid_t = 65534;
}
