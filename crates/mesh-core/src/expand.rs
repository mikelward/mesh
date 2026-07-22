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

use crate::lexer::{Access, Piece, VarRef, Word};
use crate::vars::{Value, Vars};

/// An expansion error — an unbound read fails loud (no null), per `DESIGN.md`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpandError {
    UnboundVar(String),
    UnsetEnv(String),
    Unsupported(String),
    ListNeedsSpread(String),
    NotAList(String),
    IndexOutOfRange { name: String, index: i64 },
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
            ExpandError::NotAList(n) => write!(f, "${n}: cannot index a string value"),
            ExpandError::IndexOutOfRange { name, index } => {
                write!(f, "${name}[{index}]: list index out of range")
            }
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
/// preserving list values rather than applying the external-argv rule
/// (`DESIGN.md` §"Arguments do not word-split": a bare list arrives at an in-shell
/// function as one value). `...$xs` spreads into one value per element; a bare,
/// unquoted `$name` (or `$name[slice]`) holding a list arrives as a single
/// `Value::List`; every other word yields `Value::String` argument(s) via ordinary
/// expansion (so a glob still expands to its matches, an index to one element).
pub fn expand_values(words: Vec<Word>, vars: &Vars) -> Result<Vec<Value>, ExpandError> {
    let mut out = Vec::new();
    for word in words {
        if let Some(vref) = spread_var(&word) {
            out.extend(spread_strings(vref, vars)?.into_iter().map(Value::String));
        } else if let Some(list) = whole_list_value(&word, vars) {
            out.push(Value::List(list));
        } else {
            let mut strings = Vec::new();
            expand_word(word, vars, &mut strings)?;
            out.extend(strings.into_iter().map(Value::String));
        }
    }
    Ok(out)
}

/// Resolve a `...$name` spread to its element strings (a whole list or a slice).
/// Spreading a single element (`...$xs[0]`), a string, or an unbound name is an
/// error, matching the command-position spread rules.
fn spread_strings(vref: &VarRef, vars: &Vars) -> Result<Vec<String>, ExpandError> {
    match vars.get(&vref.name) {
        Some(Value::List(values)) => match &vref.access {
            None => Ok(values.clone()),
            Some(Access::Slice {
                start,
                end,
                inclusive,
            }) => Ok(slice(values, *start, *end, *inclusive).to_vec()),
            Some(Access::Index(_)) => Err(ExpandError::Unsupported(format!(
                "...${}: cannot spread a single list element",
                vref.name
            ))),
        },
        Some(Value::String(_)) => Err(ExpandError::Unsupported(format!(
            "...${} on a string",
            vref.name
        ))),
        None => Err(ExpandError::UnboundVar(vref.name.clone())),
    }
}

/// If `word` is exactly a bare, unquoted `$name` (or `$name[slice]`) that resolves
/// to a list, return that list (to bind as one typed function argument). A string,
/// an index access, a member access, a quoted reference, or an unbound name
/// returns `None`, so ordinary expansion handles it (and reports any error).
fn whole_list_value(word: &Word, vars: &Vars) -> Option<Vec<String>> {
    let [Piece::Var(vref)] = word.0.as_slice() else {
        return None;
    };
    if vref.member.is_some() || vref.quoted {
        return None;
    }
    match (vars.get(&vref.name), &vref.access) {
        (Some(Value::List(values)), None) => Some(values.clone()),
        (
            Some(Value::List(values)),
            Some(Access::Slice {
                start,
                end,
                inclusive,
            }),
        ) => Some(slice(values, *start, *end, *inclusive).to_vec()),
        _ => None,
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
        ] if text == "..." && vref.member.is_none() => Some(vref),
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
                pattern.extend(std::iter::repeat_n('-', literal_hyphens));
                literal_hyphens = 0;
                in_class = false;
            } else if !in_class && ch == '[' {
                in_class = true;
            }
            pattern.push(ch);
        }
    }
    pattern
}

/// Resolve a variable reference to its string value.
///
/// `$env.KEY` reads the process environment (strict: unset is an error).
/// `$name` reads the variable store (unbound is an error). Member access on any
/// namespace other than `env`, and a bare `$env`, are not supported yet.
fn resolve(vref: &VarRef, vars: &Vars) -> Result<String, ExpandError> {
    match (vref.name.as_str(), &vref.member, &vref.access) {
        ("env", Some(key), None) => env::var_os(key)
            .map(|v| v.to_string_lossy().into_owned())
            .ok_or_else(|| ExpandError::UnsetEnv(key.clone())),
        ("env", None, None) => Err(ExpandError::Unsupported("$env".to_string())),
        (name, None, Some(Access::Index(index))) => vars
            .get(name)
            .ok_or_else(|| ExpandError::UnboundVar(name.to_string()))
            .and_then(|value| match value {
                Value::String(_) => Err(ExpandError::NotAList(name.to_string())),
                Value::List(values) => {
                    let offset = if *index < 0 {
                        values.len() as i128 + *index as i128
                    } else {
                        *index as i128
                    };
                    usize::try_from(offset)
                        .ok()
                        .and_then(|offset| values.get(offset))
                        .cloned()
                        .ok_or_else(|| ExpandError::IndexOutOfRange {
                            name: name.to_string(),
                            index: *index,
                        })
                }
            }),
        (name, None, Some(Access::Slice { .. })) => vars
            .get(name)
            .ok_or_else(|| ExpandError::UnboundVar(name.to_string()))
            .and_then(|value| match value {
                Value::String(_) => Err(ExpandError::NotAList(name.to_string())),
                Value::List(_) => Err(ExpandError::ListNeedsSpread(name.to_string())),
            }),
        (name, None, None) => vars
            .get(name)
            .ok_or_else(|| ExpandError::UnboundVar(name.to_string()))
            .and_then(|value| match value {
                Value::String(value) => Ok(value.clone()),
                Value::List(_) => Err(ExpandError::ListNeedsSpread(name.to_string())),
            }),
        (name, Some(member), None) => Err(ExpandError::Unsupported(format!("${name}.{member}"))),
        (name, member, Some(access)) => Err(ExpandError::Unsupported(format!(
            "${name}{}[{access:?}]",
            member.as_ref().map(|m| format!(".{m}")).unwrap_or_default()
        ))),
    }
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
        let followed_by_slash = pieces.get(1).is_some_and(|(t, _)| t.starts_with('/'));
        if pieces.len() == 1 || followed_by_slash {
            if let Some(home) = home() {
                pieces[0] = (home, false);
            }
        }
    } else if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = home() {
            pieces[0] = (home, false);
            pieces.insert(1, (format!("/{rest}"), true));
        }
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
    use super::{apply_tilde, has_glob_meta};

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
}
