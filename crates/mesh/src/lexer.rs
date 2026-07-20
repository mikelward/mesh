//! M0 tokenizer.
//!
//! **This is a placeholder, not the real lexer.** It splits on ASCII whitespace
//! and nothing else — no quotes, no `$`-expansion, no lists, no operators. It
//! exists only so M0 can name and launch an external command. The real lexer
//! (quoting, expansion, the clean-break grammar) is designed in `DESIGN.md` and
//! lands in a later milestone; do not build features on top of this.

/// Split a line into words on runs of ASCII whitespace, dropping empties.
pub fn split(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(split("ls -l /tmp"), ["ls", "-l", "/tmp"]);
    }

    #[test]
    fn collapses_runs_and_trims() {
        assert_eq!(split("  echo   hi \n"), ["echo", "hi"]);
    }

    #[test]
    fn empty_line_is_no_words() {
        assert!(split("   \t\n").is_empty());
    }
}
