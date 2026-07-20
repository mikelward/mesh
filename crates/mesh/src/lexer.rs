//! Lexer: split a line into words of pieces, honoring quotes, escapes, and
//! `$` interpolation.
//!
//! Quoting is **Model B** (`DESIGN.md` §"Quoting and escaping"):
//!
//! - Bare words: a backslash escapes the next character; `$name` interpolates.
//! - `"…"` — interpolates (`$name`, `${…}`) and escapes (`\n \t \r \e \\ \" \$`,
//!   `\u{…}`).
//! - `'…'` — escapes but does *not* interpolate (`\n \t \r \e \\ \'`, `\u{…}`);
//!   `$` is always literal.
//! - `r'…'` / `r"…"` — raw: no escapes and no interpolation.
//! - An unknown escape inside a quote is an error.
//!
//! Each word is a list of [`Piece`]s: `Text` (literal or expandable) and `Var`
//! (an interpolation resolved later, in [`crate::expand`], against the variable
//! store). Interpolation and expansion never word-split.
//!
//! Deferred: `:` value modifiers, `${…}` beyond a name/`.member`, heredocs, and
//! `\`-newline continuation across input lines.

/// A variable reference: `$name`, `${name}`, or `$env.member`.
#[derive(Debug, PartialEq, Eq)]
pub struct VarRef {
    pub name: String,
    /// A single `.member` access, e.g. `$env.PATH` → name `env`, member `PATH`.
    pub member: Option<String>,
}

/// One piece of a word.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    /// Literal or bare text. `expandable` is true for unquoted text (eligible
    /// for tilde/glob expansion), false for quoted or escaped text.
    Text { text: String, expandable: bool },
    /// A `$…` interpolation, resolved against the variable store.
    Var(VarRef),
}

/// A word: adjacent pieces that concatenate into one argument. An empty piece
/// list is a genuine empty argument (e.g. from `""`).
#[derive(Debug, PartialEq, Eq)]
pub struct Word(pub Vec<Piece>);

/// A lexing (syntax) error.
#[derive(Debug, PartialEq, Eq)]
pub enum LexError {
    UnterminatedQuote(char),
    UnknownEscape(char),
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

/// Split `line` into words.
pub fn split(line: &str) -> Result<Vec<Word>, LexError> {
    let chars: Vec<char> = line.chars().collect();
    let mut words = Vec::new();
    let mut current: Option<Vec<Piece>> = None;
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
        // `\`-newline is line continuation: drop the pair without starting a
        // word. Cross-line continuation is still deferred.
        if c == '\\' && chars.get(i + 1) == Some(&'\n') {
            i += 2;
            continue;
        }
        let at_word_start = current.is_none();
        let word = current.get_or_insert_with(Vec::new);
        // A raw string is recognized where a string piece can begin: at a word
        // start, and right after an unescaped `=`. A bare `'…'`/`"…"` already
        // starts a piece there (`--flag='a b'`), so `k=r'v'`, `k='v'`, `k="v"`
        // all yield `k=v`; this also covers the value of a `name=r'…'` binding.
        let raw_eligible = at_word_start || ends_with_bare_equals(word);
        match c {
            '\\' => match chars.get(i + 1) {
                Some(&next) => {
                    push_char(word, next, false);
                    i += 2;
                }
                None => {
                    push_char(word, '\\', false);
                    i += 1;
                }
            },
            '$' => match parse_var(&chars, i + 1, true) {
                Some((vref, next)) => {
                    word.push(Piece::Var(vref));
                    i = next;
                }
                // A `$` not starting a valid interpolation is a literal char.
                None => {
                    push_char(word, '$', true);
                    i += 1;
                }
            },
            '\'' => {
                i = lex_escaped(&chars, i + 1, '\'', word)?;
            }
            '"' => {
                i = lex_escaped(&chars, i + 1, '"', word)?;
            }
            'r' if raw_eligible && matches!(chars.get(i + 1), Some('\'') | Some('"')) => {
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

/// Lex a `"…"` or `'…'` string (Model B). `"…"` also interpolates `$…`. `start`
/// is the index just past the opening `quote`; returns the index past the close.
fn lex_escaped(
    chars: &[char],
    start: usize,
    quote: char,
    word: &mut Vec<Piece>,
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
        if double && c == '$' {
            // Inside a string an unbraced `$name.member` is `$name` + literal
            // `.member`; only `${…}` parses member access here.
            if let Some((vref, next)) = parse_var(chars, i + 1, false) {
                push_text(word, &buf, false);
                buf.clear();
                word.push(Piece::Var(vref));
                i = next;
                continue;
            }
            buf.push('$');
            i += 1;
            continue;
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
    push_text(word, &buf, false);
    Ok(i)
}

/// Lex a raw string `r'…'` / `r"…"`: no escapes, no interpolation.
fn lex_raw(
    chars: &[char],
    start: usize,
    delim: char,
    word: &mut Vec<Piece>,
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
    push_text(word, &buf, false);
    Ok(i)
}

/// Parse a `$…` interpolation starting at `at` (the index just past `$`).
/// Returns the reference and the index just past it, or `None` if `$` is not
/// followed by a valid variable (so the `$` is a literal character).
///
/// `member_after_name` controls whether a `.member` after an unbraced `$name` is
/// consumed as member access. It is **true** outside strings (`$m.key` is access)
/// and **false** inside `"…"`, where an unbraced `$name.member` is `$name` plus
/// the literal `.member` (per `DESIGN.md` — use `${…}` for access in a string).
/// The braced `${…}` form always parses member access.
fn parse_var(chars: &[char], at: usize, member_after_name: bool) -> Option<(VarRef, usize)> {
    if chars.get(at) == Some(&'{') {
        let start = at + 1;
        let mut j = start;
        while let Some(&c) = chars.get(j) {
            if c == '}' {
                let inner: String = chars[start..j].iter().collect();
                return parse_var_ref(&inner).map(|v| (v, j + 1));
            }
            j += 1;
        }
        return None; // unterminated `${` → treat the `$` as literal
    }
    let (name, mut j) = read_name(chars, at)?;
    let mut member = None;
    if member_after_name && chars.get(j) == Some(&'.') {
        if let Some((m, k)) = read_name(chars, j + 1) {
            member = Some(m);
            j = k;
        }
    }
    Some((VarRef { name, member }, j))
}

/// Is `s` a valid kebab identifier? Uses the same rule as [`read_name`] (so an
/// assignment target and a `$name` read agree — e.g. `a--b` is not a name).
pub fn is_ident(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    matches!(read_name(&chars, 0), Some((_, n)) if n == chars.len())
}

/// Parse the inner content of a `${…}` — a `name` with an optional `.member`.
fn parse_var_ref(inner: &str) -> Option<VarRef> {
    let chars: Vec<char> = inner.chars().collect();
    let (name, mut j) = read_name(&chars, 0)?;
    let mut member = None;
    if chars.get(j) == Some(&'.') {
        let (m, k) = read_name(&chars, j + 1)?;
        member = Some(m);
        j = k;
    }
    if j != chars.len() {
        return None; // trailing junk
    }
    Some(VarRef { name, member })
}

/// Read a kebab identifier at `start`: an alphabetic/`_` head, then
/// alphanumeric/`_`, plus interior `-` (a hyphen only when the next char is
/// alphanumeric, so `$a-$b` is `$a` + `-` + `$b` while `$auto-fetch` is one name).
fn read_name(chars: &[char], start: usize) -> Option<(String, usize)> {
    let first = *chars.get(start)?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut name = String::from(first);
    let mut i = start + 1;
    while let Some(&c) = chars.get(i) {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
        } else if c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphanumeric()) {
            name.push('-');
        } else {
            break;
        }
        i += 1;
    }
    Some((name, i))
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

/// Append `text` to `word`, coalescing with a trailing `Text` piece of the same
/// expandability. Empty text is dropped.
fn push_text(word: &mut Vec<Piece>, text: &str, expandable: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(Piece::Text {
        text: last,
        expandable: e,
    }) = word.last_mut()
    {
        if *e == expandable {
            last.push_str(text);
            return;
        }
    }
    word.push(Piece::Text {
        text: text.to_string(),
        expandable,
    });
}

/// Does `word` end in an unquoted `=`? Used to let a raw-string prefix begin a
/// piece right after `=` (`--flag=r'v'`, and the value of a `name=r'…'` binding),
/// matching where a bare `'…'`/`"…"` already starts one. The `=` is a bare,
/// expandable char, so it lands in a trailing expandable `Text` piece.
fn ends_with_bare_equals(word: &[Piece]) -> bool {
    matches!(
        word.last(),
        Some(Piece::Text { text, expandable: true }) if text.ends_with('=')
    )
}

fn push_char(word: &mut Vec<Piece>, c: char, expandable: bool) {
    if let Some(Piece::Text {
        text: last,
        expandable: e,
    }) = word.last_mut()
    {
        if *e == expandable {
            last.push(c);
            return;
        }
    }
    word.push(Piece::Text {
        text: c.to_string(),
        expandable,
    });
}

#[cfg(test)]
mod tests {
    use super::{LexError, Piece, VarRef, Word, split};

    fn exp(text: &str) -> Piece {
        Piece::Text {
            text: text.to_string(),
            expandable: true,
        }
    }
    fn lit(text: &str) -> Piece {
        Piece::Text {
            text: text.to_string(),
            expandable: false,
        }
    }
    fn var(name: &str) -> Piece {
        Piece::Var(VarRef {
            name: name.to_string(),
            member: None,
        })
    }

    fn words(line: &str) -> Vec<Word> {
        split(line).expect("lex")
    }

    #[test]
    fn splits_bare_words_on_whitespace() {
        assert_eq!(
            words("ls -l"),
            [Word(vec![exp("ls")]), Word(vec![exp("-l")])]
        );
    }

    #[test]
    fn backslash_escapes_a_space_into_one_word() {
        assert_eq!(words(r"a\ b"), [Word(vec![exp("a"), lit(" "), exp("b")])]);
    }

    #[test]
    fn bare_dollar_name_is_a_var() {
        assert_eq!(words("$x"), [Word(vec![var("x")])]);
        assert_eq!(words("pre$x"), [Word(vec![exp("pre"), var("x")])]);
    }

    #[test]
    fn braced_and_member_vars() {
        assert_eq!(words("${x}post"), [Word(vec![var("x"), exp("post")])]);
        assert_eq!(
            words("$env.PATH"),
            [Word(vec![Piece::Var(VarRef {
                name: "env".into(),
                member: Some("PATH".into())
            })])]
        );
    }

    #[test]
    fn hyphen_is_interior_only_in_var_names() {
        // `$a-$b` is `$a`, a literal `-`, then `$b`.
        assert_eq!(words("$a-$b"), [Word(vec![var("a"), exp("-"), var("b")])]);
        // but a hyphen between name chars stays in the name.
        assert_eq!(words("$auto-fetch"), [Word(vec![var("auto-fetch")])]);
    }

    #[test]
    fn double_quotes_interpolate_single_quotes_do_not() {
        assert_eq!(words(r#""a$x""#), [Word(vec![lit("a"), var("x")])]);
        assert_eq!(words(r"'a$x'"), [Word(vec![lit("a$x")])]);
    }

    #[test]
    fn single_quotes_escape_but_do_not_interpolate() {
        assert_eq!(words(r"'can\'t'"), [Word(vec![lit("can't")])]);
        assert_eq!(words(r"'a\nb'"), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r"'$x'"), [Word(vec![lit("$x")])]);
    }

    #[test]
    fn single_quote_unknown_escape_is_an_error() {
        assert_eq!(split(r"'\d'"), Err(LexError::UnknownEscape('d')));
    }

    #[test]
    fn double_quote_escapes_and_errors() {
        assert_eq!(words(r#""a\nb""#), [Word(vec![lit("a\nb")])]);
        assert_eq!(words(r#""\$5""#), [Word(vec![lit("$5")])]);
        assert_eq!(split(r#""\z""#), Err(LexError::UnknownEscape('z')));
        assert_eq!(split(r#""\uZ""#), Err(LexError::BadUnicodeEscape));
    }

    #[test]
    fn raw_strings_take_no_escapes_or_interpolation() {
        assert_eq!(words(r"r'\d+\.txt'"), [Word(vec![lit(r"\d+\.txt")])]);
        assert_eq!(words(r"r'$x'"), [Word(vec![lit("$x")])]);
    }

    #[test]
    fn raw_prefix_only_at_word_start() {
        assert_eq!(words(r"ptr'x'"), [Word(vec![exp("ptr"), lit("x")])]);
    }

    #[test]
    fn empty_quote_does_not_reset_word_start() {
        assert_eq!(words(r#"""r'x'"#), [Word(vec![exp("r"), lit("x")])]);
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert_eq!(split("'oops"), Err(LexError::UnterminatedQuote('\'')));
        assert_eq!(split(r"r'oops"), Err(LexError::UnterminatedQuote('\'')));
    }
}
