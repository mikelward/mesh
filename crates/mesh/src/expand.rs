//! Word expansion: tilde and filesystem globs.
//!
//! Runs after the M0 tokenizer and before dispatch, so `cd ~` and `ls *.rs` are
//! expanded before a builtin or external command sees them.
//!
//! There is **no way to suppress expansion yet** — quoting and escaping arrive
//! with task 5, so for now every leading `~` and every glob metacharacter is
//! active. Expansion works on `String` words, so a non-UTF-8 `$HOME` or match is
//! rendered lossily; the real fix is `OsString` words with the real lexer.

use std::env;

/// Expand each word: a leading `~`/`~/…` via `$HOME`, then filesystem globs for
/// any word containing glob metacharacters. A word without metacharacters passes
/// through literally (even if no such file exists — `touch new`, `puts foo.txt`).
/// A glob that matches nothing contributes zero words (the settled "empty" rule).
pub fn expand(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for word in words {
        let (expanded, literal_prefix) = expand_tilde(&word);
        expand_glob(expanded, literal_prefix, &mut out);
    }
    out
}

/// Replace a leading `~` (alone or before `/`) with `$HOME`. `~user` is not yet
/// supported — it needs a passwd lookup — and is left unchanged.
///
/// Returns the expanded word plus the byte length of the tilde-introduced
/// prefix (the `$HOME` portion). That prefix is *path text*, not something the
/// user typed, so its glob metacharacters must stay literal — see `expand_glob`.
fn expand_tilde(word: &str) -> (String, usize) {
    if word == "~" {
        if let Some(home) = home() {
            let len = home.len();
            return (home, len);
        }
    } else if let Some(rest) = word.strip_prefix("~/") {
        if let Some(home) = home() {
            let home = home.trim_end_matches('/');
            return (format!("{home}/{rest}"), home.len() + 1);
        }
    }
    (word.to_string(), 0)
}

fn home() -> Option<String> {
    env::var_os("HOME").map(|h| h.to_string_lossy().into_owned())
}

/// Does the word contain a glob metacharacter? (Escaping to make one literal is
/// a task-5 concern; for now any `*`, `?`, or `[` triggers globbing.)
fn has_glob_meta(word: &str) -> bool {
    word.chars().any(|c| matches!(c, '*' | '?' | '['))
}

/// Glob-expand `word` into `out`: a non-glob word (or an invalid pattern) passes
/// through literally; a matching glob contributes its sorted matches; a
/// non-matching glob contributes nothing.
///
/// `literal_prefix` marks a leading byte range (from tilde expansion) whose
/// metacharacters must stay literal — only the user-typed remainder supplies
/// glob syntax, and that prefix is escaped before matching.
fn expand_glob(word: String, literal_prefix: usize, out: &mut Vec<String>) {
    if !has_glob_meta(&word[literal_prefix..]) {
        out.push(word);
        return;
    }
    let pattern = if literal_prefix == 0 {
        word.clone()
    } else {
        format!(
            "{}{}",
            glob::Pattern::escape(&word[..literal_prefix]),
            &word[literal_prefix..]
        )
    };
    // Match shell defaults: `*`/`?`/`[…]` do not match a leading dot (so `rm *`
    // skips dotfiles) unless the pattern itself starts with `.`.
    let options = glob::MatchOptions {
        require_literal_leading_dot: true,
        ..glob::MatchOptions::new()
    };
    match glob::glob_with(&pattern, options) {
        // `flatten()` drops unreadable entries (e.g. a permission error mid-walk)
        // and keeps the matches; the `glob` crate yields them already sorted.
        Ok(paths) => out.extend(paths.flatten().map(|p| p.to_string_lossy().into_owned())),
        // An invalid pattern (e.g. an unclosed `[`) is treated as a literal word.
        Err(_) => out.push(word),
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_tilde, has_glob_meta};

    #[test]
    fn detects_glob_metacharacters() {
        assert!(has_glob_meta("*.rs"));
        assert!(has_glob_meta("file?"));
        assert!(has_glob_meta("[ab]c"));
        assert!(!has_glob_meta("plain.txt"));
    }

    #[test]
    fn tilde_user_is_left_alone() {
        // `~user` needs a passwd lookup we don't do yet: unchanged, no prefix.
        assert_eq!(expand_tilde("~root/x"), ("~root/x".to_string(), 0));
    }
}
