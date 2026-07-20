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
exit [ N ]      # leave the shell. N is masked to 0-255 (default: the last
                # command's status); a surplus operand is reported and the
                # shell keeps running.
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

## Task 5 — quoting and escapes (the real lexer, **Model B**)

The placeholder whitespace tokenizer is replaced by a real lexer. A **word** is
now a sequence of adjacent pieces that concatenate; each piece is *expandable*
(unquoted — eligible for tilde/glob) or *literal* (quoted or escaped — exempt),
so **quoting suppresses expansion**.

```
word   = piece+
piece  = bare | escape | double | single | raw   # adjacent pieces fuse
bare   = <unquoted chars, expandable>             # e.g. * ? [ ~ are active here
escape = "\" <any char>                           # literal next char; \<nl> = continuation
double = '"' ( <text> | c-escape | "$name" )* '"' # interpolates (deferred) + escapes
single = "'" ( <text> | s-escape )* "'"           # escapes, no interpolation; $ literal
raw    = ("r'" <bytes> "'") | ('r"' <bytes> '"')  # no escapes at all
```

The escape sets (an **unknown escape inside a quote is a syntax error**):

- `"…"` : `\n \t \r \e \\ \" \$` and `\u{HEX}`.
- `'…'` : `\n \t \r \e \\ \'` and `\u{HEX}`; `$` is always literal (no `\$`).

- **Bare words** are expandable; a backslash makes the next char literal
  (`a\ b` is one word; `\*`, `\~` literal).
- **Double quotes** `"…"` interpolate (deferred to task 6 — a bare `$name` is
  literal for now) and interpret the C-style escape set.
- **Single quotes** `'…'` do *not* interpolate but *do* escape (Python `str`):
  `'a\nb'` is two lines, `'$x'` is a literal `$x`, and `'\d'` is an **error**.
- **Raw strings** `r'…'` / `r"…"` take no escapes — the home for regex source
  and paths (`r'\d+\.txt'`). The `r` prefix is recognized where a string piece
  can begin: at the start of a word, and immediately after an unescaped `=`
  (`--flag=r'a b'`, and the value of a `name=r'…'` binding) — the same positions
  where a bare `'…'` / `"…"` already starts a piece, so `k=r'v'`, `k='v'`, and
  `k="v"` all yield `k=v`. A string needing both quote kinds uses a (future)
  quoted-delimiter heredoc.
- **Adjacent pieces concatenate**: `"a"b'c'` is one argument `abc`;
  `--flag='a b'` is one argument. `""` is one empty argument.
- **Expansion suppression**: a quoted or escaped `*`/`?`/`[`/`~` is literal, so
  `puts '*'` prints `*`, while unquoted `*`/`~` still expand.
- An **unterminated quote** or **unknown/bad escape** is a syntax error
  (status 2); the shell recovers and continues with the next line.

Deferred within this area: heredocs (incl. the raw both-quotes `<< 'END'` form)
and `\`-newline continuation across multiple input lines. Words are still
`String`-based, so a non-UTF-8 `$HOME`/match is lossy.

## Task 6 — variables, assignment, and interpolation

```
assign  = name "=" value              # unspaced, whole statement
        | name "=" ws value…          # spaced form (for compound values)
var     = "$" name                    # $x
        | "$" "{" name "}"            # ${x}
        | "$" "env" "." key          # $env.KEY  (member access)
name    = alpha (alnum | "_" | interior "-")*   # kebab identifier
```

- **Assignment** binds a session-global variable. `name=value` (unspaced) is the
  whole statement; `name = value` (spaced) is the compound-value form. Position
  separates assignment from a `k=v` *argument*: `git commit --author=me` and
  `env FOO=1 cmd` are commands, not bindings.
- **`$name` / `${name}`** read a variable; **`$env.KEY`** reads the environment
  (strict). Interpolation happens in bare words and `"…"`, **not** in `'…'` or
  `r'…'`.
- **Reads fail loud**: an **unbound** variable is an error (no null / always-on
  `set -u`), and the shell recovers to the next line. Assignment always creates.
  A **malformed `${…}`** (missing `}`, or an invalid name inside) is a syntax
  error too — the braces signal intent, so a typo isn't silently literal text
  (a literal `$` is `\$`). A bare `$` not followed by a name (`$5`) stays literal.
- **No word splitting**: an interpolated value is one literal value — `$x`
  holding `*` is not re-globbed and never splits on spaces.
- **Hyphens** are interior only: `$a-$b` is `$a` + `-` + `$b`, while
  `$auto-fetch` is one name.

Deferred: list/map values (only single-value assignment for now — a glob/list
RHS is an error), the `:` value modifiers (`$f:stem`), `export`, `global`/`unset`,
function-local scope, the `$sh.*` surface, and `$env:get(K default)`.

## Task 7 — sequencing (`;`, `&&`, `||`)

A line is now a sequence of commands joined by separators, run left to right:

```
line    = segment (sep segment)*
sep     = ";" | "&&" | "||"
segment = words?                      # may be empty (a no-op)
```

- **`;`** runs the next command unconditionally; **`&&`** runs it only if the
  previous command **succeeded** (status 0); **`||`** only if it **failed**
  (nonzero). Equal precedence, left-associative — `a && b || c` is `(a && b) || c`.
- A separator is recognized only **bare**: a quoted (`'a;b'`) or backslash-escaped
  (`a\;b`) operator is a literal character.
- Short-circuited and **empty** segments (a blank line, a leading/trailing `;`,
  `;;`) are no-ops that leave the status unchanged. The line's status is the last
  command actually run — so `exit` in a later segment sees it (`false; exit` → 1).

Deferred: `&` (background) and `|` (pipe) are **not** operators yet — a lone
`&`/`|` stays a literal character (they arrive with job control and pipes). A
dangling trailing operator (`a &&`) is currently a lenient no-op, not an error.

### Not yet parsed
Pipes `|`, redirection `>` `<`, `{ }` blocks, `func`, `:` modifiers, heredocs.
Each arrives with the task that needs it, and this file grows to match.

**Design target (still ahead of the lexer above).** The **Model B strings**
direction from `DESIGN.md` is now implemented (see task 5 above). What the lexer
does **not** yet reflect, landing with later tasks:

- **Regex literals `/…/` with the word-shape rule** — a leading-slash word is a
  regex only when its base (minus trailing `:` flag modifiers) is a clean
  `/BODY/`, otherwise a path/glob, so absolute globs/paths go bare. Regex flags
  are `:` modifiers (`/\d+/:i`). The `~`/`match` RHS does **not** coerce a plain
  string to a regex.
- **Heredocs** — `<< END` interpolates; `<< 'END'` is raw (the both-quote-kinds
  raw form).

See the "Quoting and escaping" section in [`DESIGN.md`](DESIGN.md) and
[`TODO.md`](TODO.md).
