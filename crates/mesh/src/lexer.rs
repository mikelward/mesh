//! Lexer: split a line into words, honoring quotes and backslash escapes.
//!
//! Implements the **Model B** quoting model from `DESIGN.md` (§ "Quoting and
//! escaping"):
//!
//! - Bare words: a backslash escapes the next character.
//! - `"…"` — interpolates (deferred) and escapes; C-style set `\n \t \r \e \\
//!   \" \$` and `\u{…}`.
//! - `'…'` — does *not* interpolate but *does* escape: the same set with the
//!   quote swapped (`\'` in place of `\"`), `$` always literal.
//! - `r'…'` / `r"…"` — raw: no escapes at all, the delimiter is the only special
//!   character (the home for regex source and paths).
//! - An **unknown escape inside a quote is an error** (`'\d'`, `"\z"`).
//!
//! Each character is tagged **expandable** (unquoted/unescaped — eligible for
//! later tilde/glob expansion) or **literal** (quoted or backslash-escaped —
//! exempt), so quoting *suppresses* expansion in [`crate::expand`].
//!
//! Not yet handled here (noted for later): `$`-interpolation inside `"…"`
//! (task 6 — a bare `$name` stays literal for now); `\`-newline continuation
//! across multiple input lines; and heredocs.

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

/// A lexing (syntax) error.
#[derive(Debug, PartialEq, Eq)]
pub enum LexError {
    /// A quote (or raw-string delimiter) was never closed.
    UnterminatedQuote(char),
    /// A backslash escape inside a quote is not one of the recognized forms.
    UnknownEscape(char),
    /// A `\u{…}` escape is malformed or names no Unicode scalar.
    BadUnicodeEscape,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexError::UnterminatedQuote(q) => write!(f, "syntax error: unterminated {q} quote"),
            LexError::UnknownEscape(c) => write!(f, "syntax error: invalid escape \\{c}"),
            LexError::BadUnicodeEscape => write!(f, "syntax error: invalid \\u{{…}} escape"),
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
                i = lex_escaped(&chars, i + 1, '\'', word)?;
            }
            '"' => {
                i = lex_escaped(&chars, i + 1, '"', word)?;
            }
            // Raw-string prefix `r'…'` / `r"…"`, recognized only at the start of
            // a word so ordinary words like `grep` or `ptr'x'` are unaffected.
            'r' if word.is_empty() && matches!(chars.get(i + 1), Some('\'') | Some('"')) => {
                let delim = chars[i + 1];
                i = lex_raw(&chars, i + 2, delim, word)?;
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

/// Lex a `"…"` or `'…'` string (Model B: escaped, `"` also interpolates). `start`
/// is the index just past the opening `quote`; returns the index just past the
/// close. The content is literal (not expandable).
///
/// Escapes: `\n \t \r \e \\`, `\u{HEX}`, and `\<quote>` (so `\"` in `"…"`, `\'`
/// in `'…'`). In `"…"`, `\$` is a literal dollar; in `'…'`, `$` is already
/// literal so `\$` is not a valid escape. Any unrecognized escape is an error.
/// (`$`-interpolation in `"…"` is deferred to task 6 — a bare `$name` is
/// literal for now.)
fn lex_escaped(
    chars: &[char],
    start: usize,
    quote: char,
    word: &mut Vec<Segment>,
) -> Result<usize, LexError> {
    let double = quote == '"';
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote(quote));
        };
        if c == quote {
            i += 1;
            break;
        }
        if c == '\\' {
            let Some(&next) = chars.get(i + 1) else {
                return Err(LexError::UnterminatedQuote(quote));
            };
            match next {
                'n' => buf.push('\n'),
                't' => buf.push('\t'),
                'r' => buf.push('\r'),
                'e' => buf.push('\x1b'),
                '\\' => buf.push('\\'),
                'u' => {
                    let (ch, consumed) =
                        parse_unicode_escape(&chars[i + 2..]).ok_or(LexError::BadUnicodeEscape)?;
                    buf.push(ch);
                    i += 2 + consumed;
                    continue;
                }
                q if q == quote => buf.push(q),
                '$' if double => buf.push('$'),
                other => return Err(LexError::UnknownEscape(other)),
            }
            i += 2;
            continue;
        }
        buf.push(c);
        i += 1;
    }
    push_str(word, &buf, false);
    Ok(i)
}

/// Lex a raw string `r'…'` / `r"…"`: no escapes at all — every byte is literal
/// and only the `delim` closes it. `start` is the index just past the opening
/// delimiter; returns the index just past the close.
fn lex_raw(
    chars: &[char],
    start: usize,
    delim: char,
    word: &mut Vec<Segment>,
) -> Result<usize, LexError> {
    let mut buf = String::new();
    let mut i = start;
    loop {
        let Some(&c) = chars.get(i) else {
            return Err(LexError::UnterminatedQuote(delim));
        };
        if c == delim {
            i += 1;
            break;
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
    fn single_quotes_escape_but_do_not_interpolate() {
        // Model B: `'…'` interprets escapes (Python str), `$` is literal.
        assert_eq!(words(r"'can\'t'"), [Word(vec![lit("can't")])]);
        assert_eq!(words(r"'a\nb'"), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r"'$x'"), [Word(vec![lit("$x")])]);
    }

    #[test]
    fn single_quote_unknown_escape_is_an_error() {
        // `\d` and `\$` are not valid single-quote escapes.
        assert_eq!(split(r"'\d'"), Err(LexError::UnknownEscape('d')));
        assert_eq!(split(r"'\$'"), Err(LexError::UnknownEscape('$')));
    }

    #[test]
    fn double_quotes_interpret_c_escapes() {
        assert_eq!(words(r#""a\nb""#), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r#""\$5""#), [Word(vec![lit("$5")])]);
        assert_eq!(words(r#""\u{41}""#), [Word(vec![lit("A")])]);
    }

    #[test]
    fn double_quote_unknown_or_bad_escape_is_an_error() {
        assert_eq!(split(r#""\z""#), Err(LexError::UnknownEscape('z')));
        assert_eq!(split(r#""\uZ""#), Err(LexError::BadUnicodeEscape));
        assert_eq!(split(r#""\u{ZZ}""#), Err(LexError::BadUnicodeEscape));
    }

    #[test]
    fn raw_strings_take_no_escapes() {
        // `r'…'` / `r"…"` are the home for regex source and paths.
        assert_eq!(words(r"r'\d+\.txt'"), [Word(vec![lit(r"\d+\.txt")])]);
        assert_eq!(words(r#"r"can't \d+""#), [Word(vec![lit(r"can't \d+")])]);
    }

    #[test]
    fn raw_prefix_only_at_word_start() {
        // A bare `r` mid-word is not a raw prefix; `ptr'x'` fuses to `ptrx`.
        assert_eq!(words(r"ptr'x'"), [Word(vec![exp("ptr"), lit("x")])]);
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
        assert_eq!(split(r"r'oops"), Err(LexError::UnterminatedQuote('\'')));
    }
}
