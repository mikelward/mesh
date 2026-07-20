//! Word expansion: tilde and filesystem globs.
//!
//! Runs after the lexer and before dispatch. Each word is a list of segments
//! tagged expandable (unquoted) or literal (quoted/escaped). Only **expandable**
//! text supplies tilde/glob syntax; literal text is kept verbatim (its
//! metacharacters are glob-escaped), so quoting suppresses expansion.
//!
//! Expansion produces `String` args, so a non-UTF-8 `$HOME` or glob match is
//! rendered lossily; the real fix is `OsString` words with a later lexer.

use std::env;

use crate::lexer::Word;

/// Expand each word into zero or more argument strings.
pub fn expand(words: Vec<Word>) -> Vec<String> {
    let mut out = Vec::new();
    for word in words {
        expand_word(word, &mut out);
    }
    out
}

/// A word reduced to `(text, expandable)` pieces, after tilde expansion.
type Pieces = Vec<(String, bool)>;

fn expand_word(word: Word, out: &mut Vec<String>) {
    let mut pieces: Pieces = word
        .0
        .into_iter()
        .map(|seg| (seg.text, seg.expandable))
        .collect();
    apply_tilde(&mut pieces);

    // Only expandable text contributes glob syntax.
    let has_meta = pieces.iter().any(|(t, e)| *e && has_glob_meta(t));
    if !has_meta {
        out.push(literal(&pieces));
        return;
    }

    // Build the pattern: expandable text as-is, literal text escaped so its
    // metacharacters match literally.
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
    // Shell defaults: `*`/`?`/`[…]` don't match a leading dot unless the pattern
    // starts with `.`.
    let options = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..glob::MatchOptions::new()
    };
    match glob::glob_with(&pattern, options) {
        // No matches → contribute nothing (the settled empty-list rule).
        // `flatten()` drops unreadable entries; the crate yields matches sorted.
        Ok(paths) => out.extend(paths.flatten().map(|p| p.to_string_lossy().into_owned())),
        // Invalid pattern → the literal word.
        Err(_) => out.push(literal(&pieces)),
    }
}

/// The literal value of a word: its segment texts concatenated (an empty word,
/// e.g. from `""`, yields an empty argument).
fn literal(pieces: &Pieces) -> String {
    pieces.iter().map(|(t, _)| t.as_str()).collect()
}

/// Replace a leading expandable `~` (alone or before `/`) with `$HOME`. The
/// substituted `$HOME` is literal (its metacharacters must not glob). A quoted
/// or escaped `~` is not expandable and so is skipped. `~user` needs a passwd
/// lookup and is left unchanged.
fn apply_tilde(pieces: &mut Pieces) {
    let Some((text, true)) = pieces.first().map(|(t, e)| (t.clone(), *e)) else {
        return;
    };
    if text == "~" {
        // A bare `~`: expand when it is the whole word or is followed by `/`.
        let followed_by_slash = pieces.get(1).is_some_and(|(t, _)| t.starts_with('/'));
        if pieces.len() == 1 || followed_by_slash {
            if let Some(home) = home() {
                pieces[0] = (home, false);
            }
        }
    } else if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = home() {
            // Keep the `/rest` verbatim and still expandable (so `~/*.rs` globs).
            pieces[0] = (home, false);
            pieces.insert(1, (format!("/{rest}"), true));
        }
    }
}

fn home() -> Option<String> {
    env::var_os("HOME").map(|h| h.to_string_lossy().into_owned())
}

/// Does the text contain a glob metacharacter?
fn has_glob_meta(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '*' | '?' | '['))
}

#[cfg(test)]
mod tests {
    use super::{apply_tilde, has_glob_meta};

    #[test]
    fn detects_glob_metacharacters() {
        assert!(has_glob_meta("*.rs"));
        assert!(has_glob_meta("file?"));
        assert!(has_glob_meta("[ab]c"));
        assert!(!has_glob_meta("plain.txt"));
    }

    #[test]
    fn quoted_tilde_is_not_expanded() {
        // A literal (quoted/escaped) leading `~` must be left alone.
        let mut pieces = vec![("~".to_string(), false)];
        apply_tilde(&mut pieces);
        assert_eq!(pieces, vec![("~".to_string(), false)]);
    }

    #[test]
    fn tilde_user_is_left_alone() {
        let mut pieces = vec![("~root/x".to_string(), true)];
        apply_tilde(&mut pieces);
        assert_eq!(pieces, vec![("~root/x".to_string(), true)]);
    }
}
