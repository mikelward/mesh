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
    Int,
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
            "int" => Self::Int,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VarRef {
    pub name: String,
    pub accesses: Vec<Access>,
    pub modifiers: Vec<Modifier>,
    pub quoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text { text: String, expandable: bool },
    Var(VarRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word(pub Vec<Piece>);
use crate::vars::{Value, Vars};

/// An expansion error — an unbound read fails loud (no null), per `DESIGN.md`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpandError {
    UnboundVar(String),
    UnsetEnv(String),
    Unsupported(String),
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
    NotAList(String),
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
            ExpandError::ListNeedsSpread(n) => {
                write!(f, "${n}: list value needs `...` in command arguments")
            }
            ExpandError::NoSuchKey { name, key } => {
                write!(f, "${name}: no `{key}` in this map")
            }
            ExpandError::NoTextForm { name, kind } => {
                write!(f, "${name}: a {kind} has no text form")
            }
            ExpandError::NotAList(n) => write!(f, "${n}: cannot index a string value"),
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
                word.0.as_slice(),
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
        Value::String(_) => Err(ExpandError::Unsupported(format!(
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
        | Value::Stream(_) => Err(ExpandError::Unsupported(format!(
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
        Value::String(_) => Err(ExpandError::Unsupported(format!(
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
        | Value::Stream(_) => Err(ExpandError::Unsupported(format!(
            "...${}: value is not a list",
            vref.name
        ))),
        Value::Function(_) => Err(ExpandError::NoTextForm {
            name: vref.name.clone(),
            kind: "function value",
        }),
    }
}

fn strings(values: Vec<Value>, name: &str) -> Result<Vec<String>, ExpandError> {
    values
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Ok(value),
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
            Value::Function(_) => Err(ExpandError::NoTextForm {
                name: name.to_string(),
                kind: "function value",
            }),
        })
        .collect()
}

/// Preserve a whole bare variable reference at an in-shell value boundary.
fn whole_value(word: &Word, vars: &Vars) -> Option<Result<Value, ExpandError>> {
    let [Piece::Var(vref)] = word.0.as_slice() else {
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
    ] = word.0.as_slice()
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
    match word.0.as_slice() {
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

fn expand_word(word: Word, vars: &Vars, out: &mut Vec<String>) -> Result<(), ExpandError> {
    // Resolve interpolations first; an interpolated value is literal.
    let mut pieces: Pieces = Vec::new();
    for piece in word.0 {
        match piece {
            Piece::Text { text, expandable } => pieces.push((text, expandable)),
            Piece::Var(vref) => pieces.push((resolve(&vref, vars)?, false)),
        }
    }
    apply_tilde(&mut pieces);

    let has_meta = pieces.iter().any(|(t, e)| *e && has_glob_meta(t));
    if !has_meta {
        out.push(literal(&pieces));
        return Ok(());
    }

    // A word globs only if its expandable segments form a valid pattern on their
    // own (literals stood in by a placeholder), so an escaped literal fragment
    // can't complete a broken class in an adjacent expandable segment.
    let structure: String = pieces
        .iter()
        .map(|(t, e)| if *e { t.clone() } else { "a".to_string() })
        .collect();
    if glob::Pattern::new(&structure).is_err() {
        out.push(literal(&pieces));
        return Ok(());
    }

    let pattern = glob_pattern(&pieces);
    let options = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..glob::MatchOptions::new()
    };
    match glob::glob_with(&pattern, options) {
        Ok(paths) => out.extend(paths.flatten().map(|p| p.to_string_lossy().into_owned())),
        Err(_) => out.push(literal(&pieces)),
    }
    Ok(())
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
        let [Access::Member(key), rest @ ..] = vref.accesses.as_slice() else {
            return Err(ExpandError::Unsupported("$env".to_string()));
        };
        (
            crate::environ::read(key).ok_or_else(|| ExpandError::UnsetEnv(key.clone()))?,
            rest,
        )
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
                    Value::String(_)
                    | Value::Integer(_)
                    | Value::Boolean(_)
                    | Value::Regex(_)
                    | Value::Glob(_)
                    | Value::Stream(_)
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
    for modifier in &vref.modifiers {
        value = apply_modifier(value, *modifier)?;
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

fn subscript_key(subscript: &str, vars: &Vars) -> Result<String, ExpandError> {
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
    match modifier {
        Len => match value {
            Value::String(value) => Ok(Value::Integer(value.chars().count() as i64)),
            Value::List(values) => Ok(Value::Integer(values.len() as i64)),
            Value::Map(values) => Ok(Value::Integer(values.len() as i64)),
            Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a string or collection".into(),
            }),
        },
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
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
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
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
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
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
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
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
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
            | Value::Integer(_)
            | Value::Boolean(_)
            | Value::Regex(_)
            | Value::Glob(_)
            | Value::Stream(_)
            | Value::Function(_) => Err(ExpandError::Modifier {
                name: name.into(),
                message: "requires a path or a list of paths".into(),
            }),
        },
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
            Value::Regex(_) | Value::Glob(_) | Value::Stream(_) | Value::Function(_) => {
                Err(ExpandError::Modifier {
                    name: name.into(),
                    message: "cannot apply string modifier to this value".into(),
                })
            }
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
    let Value::String(text) = value else {
        return Err(ExpandError::Modifier {
            name: "split".into(),
            message: "requires a string".into(),
        });
    };
    let mut fields: Vec<Value> = text
        .split(separator)
        .map(|s| Value::String(s.into()))
        .collect();
    // Drop the trailing run of empty fields (terminator semantics).
    while matches!(fields.last(), Some(Value::String(s)) if s.is_empty()) {
        fields.pop();
    }
    Ok(Value::List(fields))
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
            Value::Stream(_) => {
                return Err(ExpandError::Modifier {
                    name: "join".into(),
                    message: "cannot join a stream handle".into(),
                });
            }
        }
    }
    Ok(Value::String(out))
}

fn modifier_name(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Dir => "dir",
        Modifier::Base => "base",
        Modifier::Ext => "ext",
        Modifier::Exts => "exts",
        Modifier::Stem => "stem",
        Modifier::Bare => "bare",
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
        Modifier::Int => "int",
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
        Modifier::Int
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
        Access, ExpandError, Modifier, VarRef, accessible_c, apply_tilde, has_glob_meta,
        join_value, resolve_value, split_value,
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
            modifiers: vec![Modifier::Len],
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
