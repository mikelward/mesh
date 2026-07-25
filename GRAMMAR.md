# Legacy incremental grammar notes

This file records the task-by-task grammar that preceded M3. It remains useful
for the public compatibility lexer and the history of M0–M2 behavior, but it is
**not** the current execution grammar: M3 replaced that path with the
span-carrying clean-break parser described in [`PARSER.md`](PARSER.md). The
implemented user-facing surface is summarized in
[`docs/REFERENCE.md`](docs/REFERENCE.md), while [`DESIGN.md`](DESIGN.md) remains
the eventual language target.

The sections below are historical snapshots, so statements such as “deferred”
or “still ahead” describe the point at which that slice landed, not current M3
status.

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
var     = "$" name access modifier*             # $x, $name.member, $xs[-1]:stem
        | "$" "{" name access modifier* "}"           # ${xs[-1]:stem}
access  = ("." name)? ("[" signed-integer "]")?
modifier = ":" modifier-name
name    = alpha (alnum | "_" | interior "-")*   # kebab identifier
```

- **Assignment** binds a session-global variable. `name=value` (unspaced) is the
  whole statement; `name = value` (spaced) is the compound-value form. Position
  separates assignment from a `k=v` *argument*: `git commit --author=me` and
  `env FOO=1 cmd` are commands, not bindings.
- **`$env.KEY = value`** (and `+=`) writes the process environment rather than a
  mesh binding, and is global even inside a function. Only a plain member is a
  place *there*: an index or modifier (`$env.PATH[0] =`, `$env.PATH:dedup =`) is a
  syntax error, since `$env` holds bytes rather than typed values.
- **`$m.key = value`**, **`$xs[0] = value`** (and `+=`) write **into** a bound
  collection instead of rebinding the name, along a path that mixes members and
  indices (`$m.rows[1].name = …`). A modifier is not a place (`$xs:dedup = …`), nor
  is a slice, and `$env` / `$sh` keep their own handling. Local-by-default like any
  other assignment: inside a function the write shadows an outer binding rather
  than reaching through to it, and **`global $m.key = value`** is how it writes the
  outer one instead. **`unset $m.key`** / **`unset $xs[0]`** remove such a place
  rather than the binding holding it, under the same rules; a list removal shifts
  what follows. Nothing along the path is created — a missing
  intermediate key is an error — except a **new map key at the end**, which is how
  a key is added.
- **`$name` / `${name}`** read a variable; **`$env.KEY` / `${env.KEY}`** read the
  environment (strict), and **`$xs[N]` / `${xs[N]}`** read an exact list element.
  These forms have the same meaning in bare words and `"…"`; braces delimit a
  reference when literal text follows. Interpolation does **not** happen in
  `'…'` or `r'…'`.
- Recognized argument-free **postfix modifiers** transform values and chain from
  left to right. The initial path set is `:dir`, `:base`, `:ext`, `:exts`,
  `:stem`, and `:bare`; strings also support `:upper` and `:lower`; strings and
  lists support `:len`; and lists support `:first`, `:last`, `:rest`, `:init`,
  and `:dedup`. The file tests `:exists`, `:type`, `:read`, and `:write` ask
  about a path, and the file filters `:files`/`:f`, `:dirs`/`:d`, `:links`/`:l`,
  and `:exec`/`:x` keep a list's matching elements (or, on a single path, answer
  the same `test` question). Path, string, and file-test modifiers map over
  lists, while collection modifiers consume the list as a whole. An unrecognized name after `:` remains
  literal text, preserving constructions such as `$host:$port`. In this
  interpolation form modifiers are argument-free; the parenthesized argument form
  (`:split(SEP)`, `:join(SEP)`) is a value expression — see
  [`PARSER.md`](PARSER.md).
- A **bare `:name` in expression position** is a *reference* to that modifier —
  the one-argument function that applies it — so `$paths:map(:stem)` says what
  `$paths:map(func(p) { $p:stem })` says. Only there: a command word beginning
  with `:` stays literal text, and the colon of a map key (`[stem: 1]`) or a named
  argument (`f(k: v)`) is unaffected, since a reference is written tight against
  its name. The attached call form `:name(…)` also **starts a value**, so it can
  open a condition or a statement (`if :exists($f) { … }`).
- **Reads fail loud**: an **unbound** variable is an error (no null / always-on
  `set -u`), and the shell recovers to the next line. Assignment always creates.
  A **malformed `${…}`** (missing `}`, or an invalid name inside) is a syntax
  error too — the braces signal intent, so a typo isn't silently literal text
  (a literal `$` is `\$`). A bare `$` not followed by a name (`$5`) stays literal.
- **No word splitting**: an interpolated value is one literal value — `$x`
  holding `*` is not re-globbed and never splits on spaces.
- **Hyphens** are interior only: `$a-$b` is `$a` + `-` + `$b`, while
  `$auto-fetch` is one name.

Deferred: maps, most modifier arguments (`:split(SEP)` / `:join(SEP)` now work in a
value expression; the command-word form and others such as `:get(K, default)` are
still ahead), `export`, `global`/`unset`, function-local scope, and the `$sh.*`
surface.

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

A **descriptor prefix** names the stream to retarget: `2> log`, `2>> log`, and
`1> out` alongside the default `> out` (stdout) and `< in` (stdin). The digits
must *abut* the operator, so spacing decides — `echo 2 > f` writes "2" to `f`,
as in bash — and only a bare run of digits counts, so `""2>f` and `\2>f` are an
ordinary argument plus a stdout redirect.

A redirected **builtin** applies the targets to the current shell's descriptors
around the call, as a redirected function does, so no child is involved.

**Descriptor duplication** (`2>&1`, `>&2`, `<&0`) and the both-streams forms
(`&> file`, `>& file`) are implemented, as are heredocs (`<< END`) and
here-strings (`<<< word`). Deferred: descriptors **above 2** and closing one
(`n>&-`) — each rejected with a message naming what is missing rather than
silently reinterpreted. Also deferred: a redirection with no command (`> f`).

## M2 job builtins

Bare `&` ends the preceding command or pipeline, launches it in a background
process group, and acts as a sequence boundary (`sleep 1 & puts ready`). Its
stdin defaults to `/dev/null`, preventing a background command from consuming
later shell input. Quoted or escaped `&` remains literal. An empty `&` is a
syntax error. Only a command or a pipeline can be backgrounded: `&` on anything
else — an expression (a value call included), an assignment, an `if`/`match`, a
loop, or a definition — is refused with `mesh: &: backgrounding … is not
supported yet`, since those run in the shell itself and there is no child to
defer them to. A builtin or function *is* a command, and is backgrounded through
the same fork a pipeline stage gets.

Ctrl-Z also registers a stopped foreground pipeline in the same job table.
`jobs` lists registered jobs; `fg [N|%N]` foregrounds one, and `bg [N|%N]`
continues one in the background. With no reference, `fg` and `bg` select the
newest job. These builtins are command forms rather than new grammar productions.

## M3 list values

Lists contain values, including other lists. In assignment position, bracketed,
space-separated expressions form a list; `[]` remains distinct from an empty
string:

```
list-assign = name "=" ws? list
list        = "[" (value (ws value)*)? "]"
value       = word | list
spread      = "...$" name
index       = "$" name "[" signed-integer "]"
slice       = "$" name "[" signed-integer? (".." signed-integer? | "..=" signed-integer) "]"
```

Each scalar element uses the existing word expansion rules, so a glob can
contribute zero or more elements. Inside a list, `$name` inserts its value as one
element and `...$name` flattens one list level. In command position, spread
contributes each string element as an argument and contributes none for `[]`; a
nested list element cannot cross the command boundary.

A list used as bare `$name` in command arguments is an error: mesh never
implicitly word-splits or flattens a typed value. Spreading a string is also an
error. Exact indexing is zero-based, accepts negative indices from the end, and
returns one value. An out-of-range index or indexing a string fails loudly. A
slice is a list value and therefore uses spread in command position
(`...$xs[1..3]`). Half-open (`..`) and inclusive (`..=`) bounds follow Rust's
spelling, negative bounds count from the end, and out-of-range bounds clamp.

`+=` concatenates strings, appends a scalar or nested list to a list, or extends
a list with a whole list or slice. The clean-break general expression parser
remains ahead.

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
- **`return`.** `return expr` carries a value out of the body; a bare `return`
  carries the result so far — the last value the body produced, or the status of
  a command that produced none. Either stops the rest of the body. In command
  position the status is the usual view of that value (an integer is its own
  status, masked to 0–255, like `exit`). A function's status is otherwise its
  last command's status, or **0** for an empty body or a bare
  `return` before anything ran (`DESIGN.md`). At top level `return` is a
  recoverable error that does **not** abort a `;` sequence.
- **Calling for a value.** `f(arg, key: value, ...$spread)` yields the body's
  value — its last expression, or the value carried by `return` — while `f arg`
  runs the function for its status. `key: value` binds the parameter its `--key`
  flag would.
- **A lone integer literal is a value.** `func answer() { 42 }` yields the integer
  42; before, `42` resolved as a command name and reported "command not found".
  The rule is the narrowest one that closes the gap: the **whole statement** must
  be that literal, so `42 foo` and `42 > file` stay the commands they were. The
  first token is not enough to decide — a word is assembled from adjacent tokens,
  and `3.5` peeks as `3` — so the check parses the statement and requires a bare
  scalar whose *whole* text is an `i64`. A bare `-3` therefore does not qualify
  either: it lexes as the minus operator followed by `3`, and `return -3` or
  `(-3)` carries it. In statement position the value is discarded and the status
  is the usual view of an integer — itself — exactly as `41 + 1` already gave.
- **Lambdas.** `func(params) { body }` — the declaration with the name left off —
  is an expression yielding a **function value**. It reuses the signature grammar
  above in full (defaults, `--flags`, `...rest`). Bind it and value-call it
  through the variable: `double = func(x) { $x * 2 }` then `$double(5)`. The `$`
  is required, because a bare `double(5)` names the *function store* — a bare word
  is a literal string everywhere else. A callee that is any other expression
  (`$fs[0]()`, `$m.go()`) is called the same way once it produces a function
  value; one that produces anything else is a loud "value is not callable".
  Scope is a `func`'s scope: fresh locals, the parameters, and the globals. A
  lambda does **not** capture the scope it was written in, so one inside a
  function cannot read that function's locals. A function value is the one value
  with **no text form** — a command argument, an interpolation, or `$env.*`
  refuses it — and equality is **identity**, so a copied binding is the same
  function and a separately written twin is not.
- **The higher-order modifiers.** `:map`, `:filter`, and `:each` each take one
  **callable** — a lambda, or a function value reached through a variable — and
  apply it to every element of a list: `$xs:map(func(x) { $x * 2 })`. They chain
  with the ordinary modifiers. The call goes through the same machinery a written
  call uses, so `return`, an arity mismatch, a runtime error, an escaped `break`,
  and `exit` behave exactly as they do in `f(x)` — loop state included, so a
  `break` inside the callable does not escape into the caller's loop.
  `:filter` requires the predicate to answer with a **boolean**, not a truthy
  value: mesh's truthiness is the shell's, where an integer is true when it is
  *zero*, so a loose reading would keep the zeros — and would make
  `:filter(:dir)`, easy to write now that a bare `:mod` is callable, keep
  everything. `:each` yields the empty string — mesh's "nothing produced" —
  rather than the list, so a chain cannot
  read side-effecting code as a transform. The subject must be a **list**; a map
  is a loud error pointing at `:keys` / `:values`.
- **`:capture`.** `f(…):capture` runs the call and yields a **record of every
  channel**: `.value`, `.out`, `.err`, `.status`. It is an *invocation-level*
  modifier, recognized on the call before the call runs — a value modifier would
  arrive after the stdout had already streamed away — so it is the one `:` name
  that is not applied to a value. `:capture` takes no arguments, and on anything
  but a call it is an error. `.out`/`.err` are the bytes **as written**: no
  trailing-newline trim, unlike `$(…)`, so the record fixes no split policy.
  `.status` is the exit int.

  A **command** captures too — `grep(foo):capture`, `puts(x):capture` — and is the
  single exception to "a command has no return value", since it asks for the record
  rather than a value. Its record comes back **without `.value`**, so reading it is
  the usual loud missing-key error. Builtin or external, it goes through the
  dispatcher command position uses, so `puts(x):capture` runs the *builtin* and
  `pwd():capture` does not reach `/bin/pwd`; an `exit` still leaves the shell rather
  than reporting a status into a record. Command captures take **positional
  arguments only**: a `key: value` option or a map spread has no signature to bind
  to, and a list positional still needs `...`.

  A background job the call starts inherits the capture's pipes, so the capture
  waits for it — as bash's command substitution and mesh's own `$(…)` both do;
  redirecting the child's own streams releases it.

  A statement failing *inside* the body is ordinary — the record is produced and
  the diagnostic is on `.err`. The **call** failing (a bad argument count, so the
  body never ran) fails the enclosing statement, as an uncaptured value call
  does, and its diagnostic is re-reported on the shell's stderr rather than
  vanishing into a record nobody will read.
- **Deferred:** a function in the background is rejected (needs the fork-based
  executor); and `func` composing with separators.

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

### For loops

The first loop slice iterates string lists and word expressions. The binding is
updated in the current scope for each element; list elements remain single
values even when they contain whitespace.

```ebnf
for-expr = "for" ws name ws "in" ws value… ws? "{" body "}"
```

```mesh
for item in $items {
  puts $item
}
```

### Not yet parsed
Nested/general list expressions, maps, bare `{ }` blocks, `match`,
modifier arguments, and heredocs. Each arrives with the task that needs it, and
this file grows to match.

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
