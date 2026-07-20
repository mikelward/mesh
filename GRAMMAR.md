# Grammar (implemented subset)

The grammar mesh **currently parses**, grown one task at a time — deliberately a
*subset* of the full language in [`DESIGN.md`](DESIGN.md), just enough for the
features built so far. Where this doc and `DESIGN.md` differ, `DESIGN.md` is the
eventual target and this file is the current reality. Decisions made ahead of the
full design are noted inline and are open to revision.

Notation is EBNF-ish: `*` = zero or more, `?` = optional, `|` = alternative,
`"x"` = literal.

## Task 1 — external commands + `exit`

```
line    = ws? words? ws? newline
words   = word (ws word)*
word    = nonspace+                 # M0: no quoting, escapes, or expansion yet
ws      = whitespace+               # Unicode whitespace (see lexer)
```

A non-empty `line` is a **command**: the first `word` names it. A builtin name
runs in-process; any other name is launched as an external program with the
remaining words as arguments.

```
exit [ N ]      # leave the shell. N is masked to 0-255 (default 0);
                # a surplus operand is reported and the shell keeps running.
```

Input must be valid UTF-8; a malformed line is rejected loudly. (Lossless
handling of non-UTF-8 command bytes is deferred to the real lexer.)

## Task 2 — basic builtins

No change to the line grammar; three more builtin names are recognized in
command position (still whitespace-split words, no quoting yet):

```
cd [ DIR ]      # DIR omitted → $HOME; DIR "-" → $OLDPWD (prints destination).
                # Updates $PWD/$OLDPWD. At most one operand.
pwd             # print the working directory. No operands.
puts [ ARG ... ]  # print the args separated by single spaces + newline.
```

## Task 4 — tilde and glob expansion

After tokenizing, each word is expanded (before dispatch, so `cd ~` and
`ls *.rs` work):

- **Tilde:** a word equal to `~`, or starting with `~/`, has the leading `~`
  replaced by `$HOME`. `~user` is not expanded yet (needs a passwd lookup).
- **Globs:** a word containing a glob metacharacter (`*`, `?`, `[`) is matched
  against the filesystem; matches replace the word (sorted; dotfiles excluded
  unless the pattern starts with `.`). **No match → the word contributes zero
  args** (the settled empty-list rule). A word with no metacharacter is a
  literal and passes through even if no such file exists. An invalid pattern is
  a literal.

## Task 5 — quoting and escapes (the real lexer)

The placeholder whitespace tokenizer is replaced by a real lexer. A **word** is
now a sequence of adjacent pieces that concatenate; each piece is *expandable*
(unquoted — eligible for tilde/glob) or *literal* (quoted or escaped — exempt),
so **quoting suppresses expansion**.

```
word   = piece+
piece  = bare | escape | single | double        # adjacent pieces fuse
bare   = <unquoted chars, expandable>            # e.g. * ? [ ~ are active here
escape = "\" <any char>                          # literal next char; \<nl> = line continuation
single = "'" ( <raw> | "\'" | "\\" )* "'"        # raw: only \' and \\ ; all else literal
double = '"' ( <text> | c-escape )* '"'          # c-escape: \n \t \r \e \\ \" \$ \u{HEX}
```

- **Bare words** are expandable; a backslash makes the next char literal
  (`a\ b` is one word; `\*`, `\~` are literal).
- **Single quotes** are raw — only `\'` and `\\` are escapes; every other
  backslash is literal (the home of regex source / paths).
- **Double quotes** interpret a C-style escape set and are literal text.
  `$`-interpolation is **deferred to task 6** — a bare `$name` inside `"…"`
  stays literal for now.
- **Adjacent pieces concatenate**: `"a"b'c'` is one argument `abc`;
  `--flag='a b'` is one argument. `""` is one empty argument.
- **Expansion suppression**: a quoted or escaped `*`/`?`/`[`/`~` is literal, so
  `puts '*'` prints `*` and `puts '~'` prints `~`, while unquoted `*`/`~` still
  expand.
- An **unterminated quote** is a syntax error (status 2); the shell recovers and
  continues with the next line.

Deferred within this area: `$`-interpolation (task 6), a **heredoc** `<< END`
for the raw both-quotes form (the chosen raw form — see `TODO.md`), and
`\`-newline continuation across multiple input lines (needs a multi-line
reader). Words are still `String`-based, so a non-UTF-8 `$HOME`/match is lossy.

### Not yet parsed
`$` variables and interpolation, pipes `|`, redirection `>` `<`, sequencing `;`
`&&` `||`, `{ }` blocks, `func`, heredocs. Each arrives with the task that needs
it, and this file grows to match.
