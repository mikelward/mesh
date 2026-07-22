# Grammar (implemented subset)

The grammar mesh **currently parses**, grown one task at a time — deliberately a
*subset* of the full language in [`DESIGN.md`](DESIGN.md), just enough for the
features built so far. Where this doc and `DESIGN.md` differ, `DESIGN.md` is the
eventual target and this file is the current reality. Decisions made ahead of the
full design are noted inline and are open to revision.

Notation is EBNF-ish: `*` = zero or more, `?` = optional, `|` = alternative,
`"x"` = literal.

## Complexity audit

The implemented grammar is still small, but a few rules make the lexer more
context-sensitive than the notation above suggests. Before adding general
expressions, the following simplifications would reduce special cases and make
future parsing errors more predictable:

1. **Empty command positions are errors (completed).** Leading and repeated
   separators are rejected, as are trailing `&&` and `||`. A single trailing
   `;` is permitted as a statement terminator. Every conditional operator thus
   has two operands, without status-preservation edge cases.
2. **Tokenize first, validate structure second.** `split_line` currently finds
   words, separators, pipes, redirections, and unsupported descriptor redirects
   in one pass. Descriptor detection consequently needs to inspect both built
   pieces and the original character stream. A small token stream (`Word`,
   `Separator`, `Pipe`, `Redirect`, `Background`) followed by a structural
   parser would centralize longest-match rules and make later operators easier
   to add without changing quote handling.
3. **Keep access syntax in one parser.** Braced and unbraced interpolation share
   names, member access, indices, and slices, but differ in how malformed access
   is reported. General expressions should reuse one variable-reference parser
   with an explicit strict/lenient terminator policy rather than adding another
   copy for each context.

The highest-value behavior change is item 1 because it removes surprising
accepted input. Items 2–3 are implementation constraints to settle before the
expression grammar grows; they do not require changing today’s valid programs.

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
double = '"' ( <text> | c-escape | var )* '"'      # interpolates + escapes
single = "'" ( <text> | s-escape )* "'"           # escapes, no interpolation; $ literal
raw    = ("r'" <bytes> "'") | ('r"' <bytes> '"')  # no escapes at all
```

The escape sets (an **unknown escape inside a quote is a syntax error**):

- `"…"` : `\n \t \r \e \\ \" \$` and `\u{HEX}`.
- `'…'` : `\n \t \r \e \\ \'` and `\u{HEX}`; `$` is always literal (no `\$`).

- **Bare words** are expandable; a backslash makes the next char literal
  (`a\ b` is one word; `\*`, `\~` literal).
- **Double quotes** `"…"` interpolate variables, including member access and
  integer indexing, and interpret the C-style escape set. Braces delimit a
  reference before literal text: `"${file}.txt"`.
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
var     = "$" name access             # $x, $name.member, $xs[-1]
        | "$" "{" name access "}"       # ${x}, ${name.member}, ${xs[-1]}
access  = ("." name)? ("[" signed-integer "]")?
name    = alpha (alnum | "_" | interior "-")*   # kebab identifier
```

- **Assignment** binds a session-global variable. `name=value` (unspaced) is the
  whole statement; `name = value` (spaced) is the compound-value form. Position
  separates assignment from a `k=v` *argument*: `git commit --author=me` and
  `env FOO=1 cmd` are commands, not bindings.
- **`$name` / `${name}`** read a variable; **`$env.KEY` / `${env.KEY}`** read the
  environment (strict), and **`$xs[N]` / `${xs[N]}`** read an exact list element.
  These forms have the same meaning in bare words and `"…"`; braces delimit a
  reference when literal text follows. Interpolation does **not** happen in
  `'…'` or `r'…'`.
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
function-local scope, the `$sh.*` surface, and `$env:get(K, default)`.

## Task 7 — sequencing (`;`, `&&`, `||`)

A line is now a sequence of commands joined by separators, run left to right:

```
line    = segment (sep segment)* ";"?
sep     = ";" | "&&" | "||"
segment = words
```

- **`;`** runs the next command unconditionally; **`&&`** runs it only if the
  previous command **succeeded** (status 0); **`||`** only if it **failed**
  (nonzero). Equal precedence, left-associative — `a && b || c` is `(a && b) || c`.
- A separator is recognized only **bare**: a quoted (`'a;b'`) or backslash-escaped
  (`a\;b`) operator is a literal character.
- A blank line is a no-op. Leading or repeated separators and trailing `&&` or
  `||` are syntax errors; one trailing `;` is allowed. The line's status is the
  last command actually run — so `exit` in a later segment sees it
  (`false; exit` → 1).

## Task 8 — pipes and redirection

Each command is now a **pipeline** of `|`-joined stages, and every stage may carry
`<` / `>` / `>>` redirections:

```
segment = pipeline
pipeline = stage ("|" stage)*
stage    = (word | redir)+            # words and redirections interleave
redir    = ("<" | ">" | ">>") word    # the following word is the target file
```

- **`|`** connects one command's stdout to the next command's stdin (a single
  `|`; `||` is still the sequence separator, matched first). **`>`** truncates (or
  creates) a file with stdout, **`>>`** appends, **`<`** reads stdin from a file.
  A redirection's target is the **next word**; the last redirection of a direction
  wins.
- Operators are recognized only **bare** — a quoted (`'a|b'`) or escaped (`a\|b`)
  operator is literal.
- **Pipeline status is pipefail, ignoring upstream SIGPIPE**: the pipeline fails
  if any stage genuinely fails (`false | true` → 1), but a stage whose stdout fed
  a pipe and was killed by SIGPIPE is not counted (`yes | head` → 0). This is a
  *heuristic* — the exit status alone can't say *why* a stage got SIGPIPE, so a
  self-inflicted SIGPIPE in a piped stage is also excused (an accepted cost of
  avoiding the `yes | head` → 141 footgun).
- An **empty pipeline stage** (`| cat`, `ls |`, `ls | | wc`) and a **redirection
  with no target** (`cat >`) are syntax errors (status 2); the shell recovers.

**Known limitation** (deferred to the fork-based executor, M2 job control): a
**FIFO** used as a redirection target in a pipeline can deadlock when its peer is
opened by a *pipeline command* rather than another stage's redirection
(`sh -c 'printf x >f' | cat <f`). Redirections between *stages* open concurrently
(`cat <f | echo >f` is fine), but opening a redirection still happens before any
command spawns; fully interleaving open and spawn needs per-child fd setup after
`fork`, which arrives with job control. Ordinary file redirection and pipes are
unaffected.

Deferred: a **builtin** in a multi-stage pipeline or with a redirection is not
supported yet (needs a forked child / an output sink) and is rejected with a
clear message — use an external command (`echo … > f`) meanwhile. A **descriptor
redirect** (`2>`, `&>`, and their `>>` forms) is also deferred and **rejected as
a syntax error** rather than silently reinterpreted. Also deferred: here-strings
and a redirection with no command (`> f`).

## M2 job builtins

Bare `&` ends the preceding command or pipeline, launches it in a background
process group, and acts as a sequence boundary (`sleep 1 & puts ready`). Its
stdin defaults to `/dev/null`, preventing a background command from consuming
later shell input. Quoted or escaped `&` remains literal. An empty `&` is a
syntax error. Assignments and builtins cannot be launched in the background yet.

Ctrl-Z also registers a stopped foreground pipeline in the same job table.
`jobs` lists registered jobs; `fg [N|%N]` foregrounds one, and `bg [N|%N]`
continues one in the background. With no reference, `fg` and `bg` select the
newest job. These builtins are command forms rather than new grammar productions.

## M3 list-value slice

The first non-string value is a list of strings. In assignment position,
bracketed, space-separated words form a list; `[]` remains distinct from an
empty string:

```
list-assign = name "=" ws? "[" (word (ws word)*)? "]"
spread      = "...$" name                  # whole command word, for now
index       = "$" name "[" signed-integer "]"
slice       = "$" name "[" signed-integer? (".." signed-integer? | "..=" signed-integer) "]"
```

Each literal element uses the existing word expansion rules. A glob can
therefore contribute zero or more elements. `...$name` contributes every list
element as a separate command argument and contributes no arguments for `[]`.
A list used as bare `$name` in command arguments is an error: mesh never
implicitly word-splits or flattens a typed value. Spreading a string is also an
error. Exact indexing is zero-based, accepts negative indices from the end, and
returns one string element. An out-of-range index or indexing a string fails
loudly. A slice is a list value and therefore uses spread in command position
(`...$xs[1..3]`). Half-open (`..`) and inclusive (`..=`) bounds follow Rust's
spelling, negative bounds count from the end, and out-of-range bounds clamp.

This is deliberately a vertical slice rather than the final expression
grammar. Lists currently contain strings only; `+=` concatenates strings,
appends a scalar to a list, or extends a list with a whole list or slice.
Nesting and general expression parsing remain ahead.

## Task 9 — functions (`func`)

A `func` definition binds a named callable. v1 covers **required named
positionals** only:

```
func-def = "func" ws name ws? "(" params? ")" ws? "{" body "}"
params   = param ((ws | ",") param)*        # names, comma- and/or space-separated
param    = name                             # required positional only, for now
call     = name (ws word)*                  # a defined name in command position
return   = "return" (ws signed-integer)?    # early exit, inside a body only
```

- **Definition.** `func greet(name) { … }` — parameters are named, referenced as
  `$name` in the body (never `$1`). Bodies may span **multiple input lines**: the
  reader buffers input until the body's `{ … }` braces balance (a brace inside a
  quote/`r'…'`/escape or a `${…}` interpolation does not count), then defines the
  function. The opening `{` may sit on a later line than the signature (the
  `")" ws? "{"` above, `ws` including a newline); an already-malformed header
  (non-whitespace after the `)`) is reported at once rather than buffered. A single-line `func f(x) { … }` and a nested multi-line definition
  inside a body are buffered the same way. A definition is a **standalone
  statement**: it does not yet compose with `;` / `&&` / `||` / `|` (text after
  the closing `}` is an error).
- **Signature.** Parameters are required named positionals, separated by commas
  and/or whitespace; a comma must sit between two names (a leading, trailing, or
  doubled comma is an error). Names must be distinct and cannot be the reserved
  `env`. The deferred forms — optional/default (`x = v`), flags (`--flag`), and
  rest (`...xs`) — are rejected with a clear "not supported yet" message.
- **Name.** A function name cannot be a reserved word (`func` / `return`) or a
  builtin (`cd` / `pwd` / `puts` / `exit` / `jobs` / `fg` / `bg`), since those
  resolve first and the definition could never be reached.
- **Call.** A defined name in command position runs the function. Resolution is
  **builtins → functions → external**; the argument count must match the
  positionals (an arity mismatch is a loud, recoverable error). Arguments bind
  left to right. Unlike an external command, an in-shell function preserves
  **typed values**: a bare, unspread list (`f $xs`) arrives intact as one list
  value — it counts as a single positional — rather than being rejected by the
  external-argv rule. A spread (`f ...$xs`) still contributes one argument per
  element, and every other word binds as a string.
- **Scope.** Each call runs in a fresh **function-local** scope: `x = 5` in a
  body binds a local (gone on return). Reads resolve the innermost local scope,
  then the global scope only — a callee never sees its caller's locals (lexical,
  not dynamic).
- **`return`.** `return N` sets the status (masked to 0–255, like `exit`); a bare
  `return` uses the status so far. Either stops the rest of the body. A function's
  result is its last command's status, or **0** for an empty body or a bare
  `return` before anything ran (`DESIGN.md`). At top level `return` is a
  recoverable error that does **not** abort a `;` sequence.
- **Deferred:** a function in a multi-stage pipeline or with a redirection (or in
  the background) is rejected (needs the fork-based executor); flags/optionals/
  rest parameters; `func` composing with separators; and calling for a value
  (`f(arg)`) vs. running (`f arg`) — only the run form exists today.

## Task 10 — `if` expressions

An `if` selects a brace-delimited branch using a command's exit status. Status
zero takes the first branch; nonzero takes `else`, when present. Branches can
span lines and `else if` chains without the POSIX `then` / `elif` / `fi` words.

```
if-expr = "if" ws command ws? "{" body "}"
          (ws? "else" ws? (if-expr | "{" body "}"))?
if-assign = name ws? "=" ws? if-expr
```

For example:

```
if grep -q needle file { puts found } else { puts absent }
label = if test -d .git { "git tree" } else { directory }
items = if true { [one "two three"] } else { [] }
```

In statement position, the selected body runs normally and the other body is
not evaluated. `return` and `exit` in a selected body retain their control-flow
behavior. In assignment position, the selected branch's final physical line is
a value expression: currently one string value, a list literal, a whole variable
value, or a nested `if`. Earlier lines run for effect. A false `if` with no
`else` yields the empty string. General boolean and comparison expressions, and
conditional destructuring assignments, arrive with the general expression
parser.

### Not yet parsed
Nested/general list expressions, maps, bare `{ }` blocks, `for` / `match`, `:`
modifiers, and heredocs. Each arrives with the task that needs it, and this file
grows to match.

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
