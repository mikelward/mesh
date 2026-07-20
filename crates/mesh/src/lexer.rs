//! Lexer: split a line into words, honoring quotes and backslash escapes.
//!
//! Implements the quoting model from `DESIGN.md` (§ "Quoting and escaping"):
//! bare words with backslash escapes, raw single quotes, double quotes with a
//! C-style escape set, and adjacent-piece concatenation. Each character is
//! tagged **expandable** (unquoted/unescaped — eligible for later tilde/glob
//! expansion) or **literal** (quoted or backslash-escaped — exempt), so quoting
//! *suppresses* expansion in [`crate::expand`].
//!
//! Not yet handled here (noted for later): `$`-interpolation inside `"…"`
//! (task 6 — a bare `$name` stays literal for now) and `\`-newline line
//! continuation across multiple input lines (needs a multi-line reader).

/// A run of characters within a word, tagged with whether it is subject to
/// later expansion. Unquoted, unescaped text is `expandable`; quoted or
/// backslash-escaped text is literal.
#[derive(Debug, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub expandable: bool,
}

/// A word: a sequence of adjacent segments that concatenate into one argument.
/// An empty segment list is a genuine empty argument (e.g. from `""`).
#[derive(Debug, PartialEq, Eq)]
pub struct Word(pub Vec<Segment>);

/// A lexing error — currently only unterminated quotes.
#[derive(Debug, PartialEq, Eq)]
pub enum LexError {
    UnterminatedQuote(char),
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnterminatedQuote(q) => write!(f, "syntax error: unterminated {q} quote"),
        }
    }
}

/// Split `line` into words. Unquoted whitespace separates words; quotes and
/// backslashes escape it. Returns an error for an unterminated quote.
pub fn split(line: &str) -> Result<Vec<Word>, LexError> {
    let chars: Vec<char> = line.chars().collect();
    let mut words = Vec::new();
    let mut current: Option<Vec<Segment>> = None;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if let Some(word) = current.take() {
                words.push(Word(word));
            }
            i += 1;
            continue;
        }
        // `\`-newline is line continuation: drop the pair *without* starting a
        // word, so a lone `\<newline>` or a trailing one adds no spurious empty
        // argument. Cross-line continuation (joining the next input line) is
        // still deferred. Inside a word, this just fuses across the newline.
        if c == '\\' && chars.get(i + 1) == Some(&'\n') {
            i += 2;
            continue;
        }
        let word = current.get_or_insert_with(Vec::new);
        match c {
            '\\' => {
                match chars.get(i + 1) {
                    Some(&next) => {
                        push_char(word, next, false);
                        i += 2;
                    }
                    // Trailing backslash: keep it literal (continuation TBD).
                    None => {
                        push_char(word, '\\', false);
                        i += 1;
                    }
                }
            }
            '\'' => {
                i = lex_single_quote(&chars, i + 1, word)?;
            }
            '"' => {
                i = lex_double_quote(&chars, i + 1, word)?;
            }
            _ => {
                push_char(word, c, true);
                i += 1;
            }
        }
    }

    if let Some(word) = current.take() {
        words.push(Word(word));
    }
    Ok(words)
}

/// Raw single quotes: the only escapes are `\'` and `\\`; every other backslash
/// is literal. `start` is the index just past the opening quote; returns the
/// index just past the closing quote.
fn lex_single_quote(
    chars: &[char],
    start: usize,
    word: &mut Vec<Segment>,
) -> Result<usize, LexError> {
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote('\''));
        };
        match c {
            '\'' => {
                i += 1;
                break;
            }
            '\\' if matches!(chars.get(i + 1), Some('\'') | Some('\\')) => {
                buf.push(chars[i + 1]);
                i += 2;
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    push_str(word, &buf, false);
    Ok(i)
}

/// Double quotes: a C-style escape set applies (`\n \t \r \e \\ \" \$` and
/// `\u{…}`); an unknown escape keeps the backslash literal. `$`-interpolation is
/// deferred, so a bare `$name` stays literal for now. Content is not expandable.
fn lex_double_quote(
    chars: &[char],
    start: usize,
    word: &mut Vec<Segment>,
) -> Result<usize, LexError> {
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote('"'));
        };
        if c == '"' {
            i += 1;
            break;
        }
        if c == '\\' {
            if let Some(&next) = chars.get(i + 1) {
                match next {
                    'n' => buf.push('\n'),
                    't' => buf.push('\t'),
                    'r' => buf.push('\r'),
                    'e' => buf.push('\x1b'),
                    '\\' => buf.push('\\'),
                    '"' => buf.push('"'),
                    '$' => buf.push('$'),
                    'u' => {
                        if let Some((ch, consumed)) = parse_unicode_escape(&chars[i + 2..]) {
                            buf.push(ch);
                            i += 2 + consumed;
                            continue;
                        }
                        // Malformed \u: keep both chars literal (`\uX` stays
                        // `\uX`), so invalid input is not silently altered.
                        buf.push('\\');
                        buf.push('u');
                    }
                    // Unknown escape: the backslash stays literal.
                    other => {
                        buf.push('\\');
                        buf.push(other);
                        i += 2;
                        continue;
                    }
                }
                i += 2;
                continue;
            }
            // Trailing backslash inside quotes: literal.
            buf.push('\\');
            i += 1;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push_str(word, &buf, false);
    Ok(i)
}

/// Parse the `{HEX}` body of a `\u` escape (from just past the `u`). Returns the
/// decoded char and how many chars were consumed (including the braces).
fn parse_unicode_escape(rest: &[char]) -> Option<(char, usize)> {
    if rest.first() != Some(&'{') {
        return None;
    }
    let mut j = 1;
    let mut hex = String::new();
    while let Some(&c) = rest.get(j) {
        if c == '}' {
            let code = u32::from_str_radix(&hex, 16).ok()?;
            let ch = char::from_u32(code)?;
            return Some((ch, j + 1));
        }
        hex.push(c);
        j += 1;
    }
    None
}

/// Append `text` to `word`, coalescing with the last segment if the
/// expandability matches. Empty text is dropped.
fn push_str(word: &mut Vec<Segment>, text: &str, expandable: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = word.last_mut() {
        if last.expandable == expandable {
            last.text.push_str(text);
            return;
        }
    }
    word.push(Segment {
        text: text.to_string(),
        expandable,
    });
}

fn push_char(word: &mut Vec<Segment>, c: char, expandable: bool) {
    if let Some(last) = word.last_mut() {
        if last.expandable == expandable {
            last.text.push(c);
            return;
        }
    }
    word.push(Segment {
        text: c.to_string(),
        expandable,
    });
}

#[cfg(test)]
mod tests {
    use super::{LexError, Segment, Word, split};

    fn exp(text: &str) -> Segment {
        Segment {
            text: text.to_string(),
            expandable: true,
        }
    }
    fn lit(text: &str) -> Segment {
        Segment {
            text: text.to_string(),
            expandable: false,
        }
    }

    fn words(line: &str) -> Vec<Word> {
        split(line).expect("lex")
    }

    #[test]
    fn splits_bare_words_on_whitespace() {
        assert_eq!(
            words("ls -l /tmp"),
            [
                Word(vec![exp("ls")]),
                Word(vec![exp("-l")]),
                Word(vec![exp("/tmp")])
            ]
        );
    }

    #[test]
    fn backslash_escapes_a_space_into_one_word() {
        // `a\ b` is a single word: the escaped space does not separate.
        assert_eq!(words(r"a\ b"), [Word(vec![exp("a"), lit(" "), exp("b")])]);
    }

    #[test]
    fn backslash_escapes_a_glob_char_as_literal() {
        // `\*` is a literal star, not expandable.
        assert_eq!(words(r"\*"), [Word(vec![lit("*")])]);
    }

    #[test]
    fn single_quotes_are_raw() {
        assert_eq!(words(r"'\d+\.txt'"), [Word(vec![lit(r"\d+\.txt")])]);
    }

    #[test]
    fn single_quote_escapes_quote_and_backslash() {
        assert_eq!(words(r"'can\'t'"), [Word(vec![lit("can't")])]);
        assert_eq!(words(r"'C:\\'"), [Word(vec![lit(r"C:\")])]);
    }

    #[test]
    fn double_quotes_interpret_c_escapes() {
        assert_eq!(words(r#""a\nb""#), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r#""\$5""#), [Word(vec![lit("$5")])]);
        assert_eq!(words(r#""\u{41}""#), [Word(vec![lit("A")])]);
    }

    #[test]
    fn adjacent_pieces_concatenate() {
        // "a"b'c' → one word of three segments, but coalescing keeps quoted runs
        // separate from the unquoted `b`.
        assert_eq!(
            words(r#""a"b'c'"#),
            [Word(vec![lit("a"), exp("b"), lit("c")])]
        );
    }

    #[test]
    fn empty_double_quotes_are_an_empty_word() {
        assert_eq!(words(r#""""#), [Word(vec![])]);
    }

    #[test]
    fn blank_line_has_no_words() {
        assert!(words("   \t").is_empty());
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(split("'oops"), Err(LexError::UnterminatedQuote('\'')));
        assert_eq!(split(r#""oops"#), Err(LexError::UnterminatedQuote('"')));
    }
}
