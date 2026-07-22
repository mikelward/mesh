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

use crate::lexer::{Piece, VarRef, Word};
use crate::vars::{Value, Vars};

/// An expansion error — an unbound read fails loud (no null), per `DESIGN.md`.
#[derive(Debug, PartialEq, Eq)]
pub enum ExpandError {
    UnboundVar(String),
    UnsetEnv(String),
    Unsupported(String),
    ListNeedsSpread(String),
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
        }
    }
}

/// Expand each word into zero or more argument strings.
pub fn expand(words: Vec<Word>, vars: &Vars) -> Result<Vec<String>, ExpandError> {
    let mut out = Vec::new();
    for word in words {
        if let Some(name) = spread_var(&word) {
            match vars.get(name) {
                Some(Value::List(values)) => out.extend(values.iter().cloned()),
                Some(Value::String(_)) => {
                    return Err(ExpandError::Unsupported(format!("...${name} on a string")));
                }
                None => return Err(ExpandError::UnboundVar(name.to_string())),
            }
            continue;
        }
        expand_word(word, vars, &mut out)?;
    }
    Ok(out)
}

/// Recognize the deliberately narrow first spread form: `...$name` as a whole
/// word. General expression spreading arrives with the parser.
fn spread_var(word: &Word) -> Option<&str> {
    match word.0.as_slice() {
        [
            Piece::Text {
                text,
                expandable: true,
            },
            Piece::Var(VarRef { name, member: None }),
        ] if text == "..." => Some(name),
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

    let pattern: String = pieces
        .iter()
        .map(|(t, e)| {
            if *e {
                t.clone()
            } else {
                glob::Pattern::escape(t)
            }
        })
        .collect();
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

/// Resolve a variable reference to its string value.
///
/// `$env.KEY` reads the process environment (strict: unset is an error).
/// `$name` reads the variable store (unbound is an error). Member access on any
/// namespace other than `env`, and a bare `$env`, are not supported yet.
fn resolve(vref: &VarRef, vars: &Vars) -> Result<String, ExpandError> {
    match (vref.name.as_str(), &vref.member) {
        ("env", Some(key)) => env::var_os(key)
            .map(|v| v.to_string_lossy().into_owned())
            .ok_or_else(|| ExpandError::UnsetEnv(key.clone())),
        ("env", None) => Err(ExpandError::Unsupported("$env".to_string())),
        (name, None) => vars
            .get(name)
            .ok_or_else(|| ExpandError::UnboundVar(name.to_string()))
            .and_then(|value| match value {
                Value::String(value) => Ok(value.clone()),
                Value::List(_) => Err(ExpandError::ListNeedsSpread(name.to_string())),
            }),
        (name, Some(member)) => Err(ExpandError::Unsupported(format!("${name}.{member}"))),
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
