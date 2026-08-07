# mesh grammar

The grammar mesh accepts **today**, as implemented in
`crates/mesh-core/src/parser.rs`: what is a token, what is a statement, what is
a value expression, and how the two readings are told apart. Anything the
implementation does not yet parse is collected under
[Not yet parsed](#not-yet-parsed) at the end; everything above that section is
accepted by the shell as it stands.

Three neighboring documents, so the same thing is not written twice:

- [`docs/DESIGN.md`](docs/DESIGN.md) — the language mesh is aiming at, and *why* each rule
  reads the way it does. This file says what parses; `DESIGN.md` argues it.
- [`docs/REFERENCE.md`](docs/REFERENCE.md) — what each construct *means* at run
  time: expansion, scope, statuses, builtins, modifiers, job control.
- [`TODO.md`](TODO.md) — the working front.

The parser deliberately separates three concerns, and this file describes only
the first two:

1. the lexer preserves spelling, quoting, adjacency, and source spans;
2. the parser turns tokens into syntax without expanding variables or globs;
3. evaluation resolves values, performs expansion, and runs commands.

## Notation

`=` defines a production and `;` ends it. `*` is zero or more, `+` one or more,
`?` optional, and `|` separates alternatives. `"…"` is a literal spelling, an
UPPERCASE name is a terminal the lexer produces, `NL` is a significant newline,
and `EOS` is end of input. `#` starts a comment on the grammar itself.

These character classes and small terminals are used throughout and defined
once here:

```ebnf
CHAR            = <any Unicode scalar value> ;
whitespace      = " " | "\t" | "\r" ;
bare-char       = CHAR - ( whitespace | NL | "\" | "'" | '"'
                         | <punctuation that is a token here> ) ;
single-char     = CHAR - ( "'" | "\" ) ;
double-char     = CHAR - ( '"' | "\" | "$" ) ;
raw-char        = CHAR - <the raw string's own closing quote> ;
regex-char      = CHAR - ( NL | <an unescaped "/"> ) ;   # a literal is one line
DIGITS          = digit+ ;
digit           = "0" … "9" ;
alpha           = <any Unicode alphabetic character> ;
alnum           = alpha | digit ;
alnum-or-underscore
                = alnum | "_" ;
interior-hyphen = "-" ;                       # never doubled, never final
signed-integer  = "-"? DIGITS ;
boolean         = "true" | "false" ;
quoted-string   = single | double ;
glob-word       = WORD ;                      # whose bare text holds `*`, `?`, or `[`
regex-literal   = "/" regex-char* "/" regex-flag* ;   # in a regex slot only
regex-flag      = ":" ( "i" | "ignorecase" | "m" | "multiline"
                      | "s" | "dotall" | "x" | "extended" ) ;
```

`bare-char` is the one class that cannot be closed off here, because *which*
punctuation ends a bare run is positional: the arithmetic and match operators are
tokens only with a boundary on each side, so the `-` in `a-b` is bare text where
the `-` in `a - b` is not. [Punctuation](#punctuation) states that rule, and it
is what `<punctuation that is a token here>` stands for.

The flag vocabulary is fixed, and the whole chain has to be flags: `/a/:upper`
is not a regex with an unknown flag but the ordinary string `/A/`, since `:upper`
is a transform. A link that is a real modifier makes the literal a word again; a
link that is *no* modifier either — `/a/:i:g` — is an error naming the flag
vocabulary, since no reading is left for the word to fall back to.

## Lexical structure

Input must be valid UTF-8; malformed input is rejected rather than replaced.

### Trivia

Spaces, tabs, and carriage returns separate tokens and carry no other meaning. A
`#` where a word could start runs to the end of the line and is discarded — a
`#` *inside* a word (`a#b`) is ordinary text.

A backslash immediately before a newline is a line continuation and is removed —
but only **between tokens**, where the lexer is looking for the next one. Inside
a word the same two characters are an ordinary escape, so `a\` ⏎ `b` is the one
word `a`, newline, `b` rather than `ab`.

### Newlines

A newline is a token, not trivia, because it terminates a statement. The rule
for when it is **layout** instead — skipped, meaning nothing — is positional:
a newline is layout wherever the parse is mid-construct, at a point where what
came before cannot end one. That covers, among others:

- **Inside a `( … )` group**, which holds one expression, so a newline between
  its operands is layout — `(1 +` ⏎ `2)`, `(1` ⏎ `+ 2)`, and `(` ⏎ `1 + 2)` all
  give `3`. The middle one is the group's own rule: a newline *before* a binary
  or range operator is layout only in here, which is why the identical `[1` ⏎
  `+ 2]` is an error. It applies to the braced `"${ … }"` expression too, and
  **does not reach into** a nested `[ … ]`, `{ … }`, or argument list — though a
  fresh `( … )` written inside one turns it back on, so `[1, (2` ⏎ `+ 3)]` is two
  elements. Not after a *prefix* operator either: `(-` ⏎ `2)` and `(...` ⏎ `$xs)`
  are errors, the prefix parser recursing without skipping the newline, and nor
  before a postfix access — `($m` ⏎ `.a)` is an error. A **capture** is a
  different case again: `$( … )` holds a statement list, so a newline in one
  separates statements as it would anywhere else — **unquoted**. Inside `"…"` a
  capture or a `${ … }` may not span a line: the string scanner stops at the
  newline and reports the opener unclosed, though the string itself may hold
  literal newlines. Tracked in `TODO.md`.
- **After any binary or range operator** — `x = 1 +` ⏎ `2`, `x = 1 ..` ⏎ `3`,
  and `x = true and` ⏎ `false` all parse — and after a trailing `|`, `|&`,
  `&&`, or `||`.
- **In a bracketed or parenthesized list** — arguments, parameters, glob
  qualifiers, an index — after the opener, after each `,`, before each `,`,
  and before the closer, so `func f(x = 1` ⏎ `, y)` parses just as
  `g(1` ⏎ `, 2)` does.
- **Around a block's braces**, after a `match`'s `{`, and around a `match`
  arm's `=>`.
- **After a parameter default's `=`**, an `alias`'s `=`, and between a
  signature's `)` and its block.

Each production below carries the `NL*` slots it accepts, and those are the
authority; the list here is the shape of the rule rather than an enumeration.

Two positions are worth naming because they look like they should be layout and
are not. An **ordinary assignment's `=`** is not a layout slot — `x =` ⏎ `1` is
a syntax error, unlike a parameter's default. And inside `[ … ]` a newline
*separates items* rather than being ignored, so `[1` ⏎ `2]` is two elements
while `[1` ⏎ `+ 2]` is an error rather than `[3]`. Inside `{ … }` it *separates
statements*, exactly as `;` does.

A newline after a *complete* operand terminates the statement. Continuing such
an expression across lines requires parentheses or a backslash-newline.

### Punctuation

Longest match wins. These are one token each:

```text
$(   ...   ..=   |&   &&   ||   >>   <<<   <<   >&   <&   &>   +=
==   !=    <=    >=   !~   ..
;    &     |     <    >    (    )    [    ]    {    }    ,    :    .    =
```

Redirection operators are tokens even with no surrounding whitespace, which is
what makes `ls>out` a redirect.

The arithmetic and match operators `+ - * / % ~`, and the match-arm `=>`, are
tokens **only with a boundary on each side** — whitespace, end of input, or one
of `,()[]{};=:`. Without that they are ordinary word text, which is what keeps
`a-b` one word, `--flag=x` one argument, and `puts value=>out` a `value=` word
followed by a `>out` redirection. The one exception is a prefix `-` attached to
its operand (`-$n`, `-1`, `-(…)`), which the expression grammar owns.

The word operators `and`, `or`, `in`, `not`, and `unless` are ordinary words to
the lexer and are recognized only where the grammar expects an operator.

### Words

A word is a run of adjacent pieces that concatenate. Each piece is *expandable*
(bare — eligible for tilde and glob expansion) or *literal* (quoted or escaped,
and therefore exempt), so quoting suppresses expansion.

```ebnf
WORD            = ( raw | piece ) piece* ;    # adjacent pieces fuse; `raw` starts a word
piece           = bare | escape | single | double | variable-ref ;
bare            = bare-char+ ;                # `*`, `?`, `[`, `~` are active here
escape          = "\" CHAR ;                  # the next character, literal
single          = "'" ( single-char | escape )* "'" ;
double          = '"' ( double-char | escape | variable-ref | capture
                      | braced-expression )* '"' ;
raw             = "r'" raw-char* "'" | 'r"' raw-char* '"' ;
braced-expression
                = "${" NL* wrapped-expression NL* "}" ;  # a `"…"` piece only
literal-WORD    = WORD ;                      # no piece whose text is computed
bare-WORD       = WORD ;                      # every piece bare; `=` ends it
```

`"a"b'c'` is one word; whitespace starts the next. A quoted or escaped `*`, `~`,
`(`, `|`, or `;` is word content rather than syntax. The parser never
reconstructs quote context from a flattened string — each piece keeps the mode
it was written in.

The escape sets are shared between `'…'` and `"…"`: `\n \t \r \e \a \b \f \v
\\`, and `\u{HEX}`. `"…"` adds `\"` and `\$`; `'…'` adds `\'` and never
interpolates. `\0` is in neither, since a NUL cannot cross `execve`. An unknown
escape inside a quote is a syntax error. `r'…'` / `r"…"` take no escapes at all
— the home for regex source and Windows-ish paths — and the `r` is an
introducer only at the **start of a word**: `x = ar'\q'` is the bare `a`
followed by an ordinary `'…'` piece, so its `\q` is the usual unknown-escape
error, while `x = r'\q'` yields `\q`. A raw piece still fuses with what follows
it (`r'a'b` is `ab`).

A **variable** interpolates in bare text and in `"…"`, never in `'…'` or
`r'…'`. A **capture** and a **braced expression** interpolate in `"…"` only:
`"at $(pwd) now"` and `"${greeting()}"` are word pieces, while a bare
`pre$(puts x)post` is a syntax error rather than one word, and a bare
`${$n + 1}` expects a variable name after the brace. Unquoted, a `$( … )` is a
standalone `capture` in the expression grammar rather than part of a word. Both
positions yield the same value — one string — so the difference is purely
grammatical; see `DESIGN.md` §"Command substitution".

### Names

```ebnf
name            = ( alpha | "_" alnum-or-underscore )
                  ( alnum-or-underscore | interior-hyphen alnum-or-underscore )* ;
```

A name starts with a letter, or with `_` followed by an alphanumeric or another
`_`. Hyphens are interior and never doubled, so `$auto-fetch` is one name while
`$a-$b` is `$a`, `-`, `$b`. `env` and `sh` are reserved namespaces.

### Variable references

```ebnf
variable-ref    = "$" name access* modifier*
                | "${" name access* modifier* "}" ;
access          = "." name | "[" subscript "]" ;
modifier        = ":" name ;
subscript       = signed-integer | range | name | "$" name | quoted-string ;
range           = signed-integer? ".." signed-integer?
                | signed-integer? "..=" signed-integer ;
```

Accesses and modifiers chain left to right (`$m.rows[1].name:stem`). Braces
delimit a reference when literal text follows (`"${file}.txt"`), and a
malformed `${…}` is a syntax error rather than literal text. A bare `$` not
followed by a name stays literal.

A `:` followed by a **bare identifier** is a modifier, and an unknown one is a
syntax error (`` `:nosuch` is not a modifier ``) rather than literal text. What
preserves `$host:$port` is that `$port` is not a bare identifier, so its `:` was
never a modifier colon.

In this position modifiers are argument-free; the parenthesized form
(`:split(SEP)`) is a value expression — see
[Value expressions](#value-expressions).

### Globs and glob qualifiers

A bare word containing `*`, `?`, or `[` is a glob pattern. A glob may carry
qualifiers in parentheses attached to the pattern, narrowing which matches
survive:

```ebnf
qualified-glob  = glob-word "(" NL* ( qualifier ( NL* "," NL* qualifier )* NL* ","? NL* )? ")" ;
qualifier       = type-letter | "x"
                | "type" ":" file-type ( "|" file-type )*
                | "exec" ":" boolean
                | "empty" ":" boolean ;
file-type       = "file" | "dir" | "symlink" | "fifo" | "socket" | "block" | "char" ;
type-letter     = "f" | "d" | "l" | "p" | "s" | "b" | "c" ;
```

A `qualified-glob` is reachable both as a `command-item` and as a `primary`
(`puts *.rs(f)`, `x = *.rs(f)`), and in the expression grammar it is listed
**before** `value-call`, since the two compete for the same `word(` shape: only
a word whose bare text has glob syntax takes qualifiers, so an ordinary word
followed by `(` is still a call, and `*.rs(nosuch)` is an unknown-qualifier
error rather than a call to `*.rs`. Each dimension may be answered once;
`*(f, d)` is rejected in favor of the `type: file|dir` alternation. An empty
list (`*()`) and a trailing comma (`*(f,)`) are both accepted.

### Regex literals

A `/…/` literal is read whole, and only in a **regex slot**: the right-hand side
of `~` / `!~`, a `match` arm pattern, and the first argument of the pattern-taking
modifiers `:replaceall`, `:replacestart`, `:replaceend`, `:match`, and `:matches`
(`$s:replaceall(/[a-z]+/, "-")`). Anywhere else a leading slash is a path or a
glob, so absolute paths need no escaping. Flags are `:` modifiers (`/\d+/:i`).

### Heredoc bodies

`HEREDOC_BODY` is an opaque, span-carrying token: the lines after the command
line's newline through the matching delimiter. The lexer queues each `<<`
delimiter on the line, reads the queued bodies in source order, and associates
each body with its redirection; a quoted delimiter also records that the body is
raw. The parser does not interpret body contents.

## Statements

```ebnf
source          = statement-list EOS ;
statement-list  = NL* ( statement ( separator statement )* separator? )? ;
statement       = and-or ;
separator       = separator-run
                | background ;
background      = "&" separator-run? ;        # backgrounds what precedes it, and separates
separator-run   = NL+ ( ";" NL* )?            # one `;` at most — a second commands nothing
                | ";" NL* ;
terminator      = ";" | NL ;

and-or          = executable ( ( "&&" | "||" ) NL* executable )* ;
executable      = definition
                | control-flow
                | scoped
                | "not"* ( binding | pipeline | expression-statement ) ;

# A compound reaches stage position only *after* a `|`. The first stage is a
# command: `executable` tries `control-flow` before `pipeline`, so a leading
# `while` is the whole statement and `while … { } | cat` is a syntax error — and
# the first position is spelled `command` here rather than `stage` because `not`
# is consumed before `pipeline` is entered, which would otherwise let
# `not loop { … }` through as a lone compound stage.
pipeline        = command ( ( "|" | "|&" ) NL* stage )* ;
stage           = compound-stage | command ;
# `fork` and `with` are excluded: both are contextual words, so a command of
# either name stays reachable in stage position.
compound-stage  = if-expr | match-expr | for-expr | while-expr | loop-expr ;
command         = env-prefix* redirection* command-word command-item* guard? ;
command-word    = qualified-glob | WORD ;     # names the command; must come first
command-item    = command-word | redirection | value-argument ;
guard           = ( "if" | "unless" ) value-expression ;
env-prefix      = name ( "=" | "+=" ) attached-value? ;  # unspaced; `FOO=1 cmd`
attached-value  = value-expression ;          # must abut the operator

expression-statement
                = value-expression guard? ;
```

A `&` in separator position does two jobs: it backgrounds the `and-or` before it
*and* separates, so `sleep 1 & puts ready` needs no `;`. What it backgrounds is
the complete preceding `and-or` — in `a | b && c & d` the background node holds
`a | b && c`, and `d` is the next statement. A trailing `&` is valid.

Separator position is what tells that `&` from the `func-ref` one: backgrounding
*follows* a statement, while a reference **opens an operand** and is written tight
against its name. So the two never compete for the same `&`, and a statement may
begin with one only in the reference reading — a leading `&` is otherwise "a
background operator needs a command".

Newlines around a statement are layout — a leading or repeated newline is fine —
but a leading `;` (`; puts x`) and a repeated one (`puts x;; puts y`) are
errors, as is running two statements together with no separator at all. One
trailing separator is allowed. A statement's status is the last thing it ran.

The repeated-separator rule reads the **whole run**: what separates two
statements is one `;` and any newlines around it, so a second `;` anywhere in
that run is refused — `puts x;; puts y`, `puts x` ⏎ `;; puts y` and `puts x;` ⏎
`; puts y` alike. Only the count matters, not the layout, so one `;` still
separates from anywhere in the run: `puts x` ⏎ `; puts y` parses. A backgrounding
`&` separates on its own and the run after it may still hold its one `;`, which
is what keeps `puts x &; puts y` — but `puts x &;; puts y` is the same error as
the rest.

An `env-prefix` value is whatever **abuts** the operator, parsed as a value
expression rather than only as a word, so `A=[1] cmd` and
`with A=style(x) { … }` both parse. Nothing abutting it is the empty string, so
`A= cmd` sets `A` to `""`. The same shape serves the command prefix, a `with`
header, and the unspaced `export A=1 B=2`.

### Redirection

```ebnf
redirection     = fd? ( "<" | ">" | ">>" ) WORD
                | fd? "<&" WORD
                | fd ">&" WORD                # with a descriptor, any target
                | ">&" ( DIGITS | literal-WORD )   # bare: written out, never computed
                | "&>" WORD
                | "<<" heredoc-delimiter HEREDOC_BODY
                | "<<<" WORD ;
fd              = DIGITS ;                    # must abut the operator
heredoc-delimiter
                = heredoc-word ;              # no capture or braced expression
heredoc-word    = WORD ;                      # a variable piece is allowed, taken literally
```

A heredoc delimiter is refused only for a piece the lexer would have to *run*:
`<<"$(puts END)"` and `<<"${1 + 2}"` are errors, since the terminator has to be
known before the body is read. A **variable** piece is not that case — `<<$x`
and `<<${x}` parse, with the sigiled text itself as the delimiter rather than
the variable's value, so the body ends at a line reading `$x`. This is looser
than `literal-WORD`, which is why the two have separate names.

The descriptor prefix must abut its operator, so spacing decides: `echo 2 > f`
writes `2` to `f`, while `echo 2> f` redirects stderr. Only a bare run of digits
counts — `""2>f` and `\2>f` are an argument plus a stdout redirect.

A **bare `>&`** is the one ambiguous operator, since `>&2` duplicates a
descriptor while `>& file` is the both-streams form. It is settled by what the
target is written as: digits duplicate, a literal word takes both streams, and a
computed target is refused — `>&$fd` reports that it wants `1>&$fd` to duplicate
or `&> $file` for both streams. `<&` has no such ambiguity, so `<&$f` is fine.

A redirection attaches to the nearest simple command: in `a | b >out` the
redirect belongs to `b`, in `a >out | b` to `a`. Redirections may interleave
with command words, and the last one for a descriptor wins at evaluation time.

### Value arguments

```ebnf
value-argument  = value-expression ;          # only after a command word
```

A value expression may appear in argument position — `puts (1 + 2)`,
`puts $(pwd)`, `puts style(x, fg: red)`, `puts $x:upper`. Four rules tell it
apart from a `WORD`:

1. **Only the shapes a word cannot spell** start one: `$(`, `(`, an *attached*
   `name(` / `$name(` / `:name(`, and an *attached* `:modifier` chain. `[` and
   `..` are excluded — in an argument they are a glob character class and
   literal text — so a list literal or a range there stays a word.
2. **Only after a command word**, which is why `command` reaches its
   `command-word` before any `command-item`: a leading redirection can precede
   the name, but a value cannot *be* the name, so `>f (1)` is
   `expected a command word` while `>f puts (1)` parses.
3. **Parsed above comparison precedence**, so a following `<` / `>` is a
   redirection rather than a comparison. A comparison in an argument needs its
   own parentheses; `&&`, `||`, and `|` are below comparison and keep their
   readings.
4. **Whole-argument.** Adjacent text on either side is a syntax error rather
   than a second item, so `pre$(x)post` does not silently become three
   arguments. Every step of an attached chain must abut the one before it:
   `puts $x :upper` is two arguments, `puts $x:upper` is one value. That
   includes a call's `(`, which is spaceable elsewhere (`g ()`) but not here,
   spacing being what separates one argument from the next: `puts (a()) (b())`
   passes two values, while `puts (a())(b())` calls the first on the second.
   The rule covers the argument's own top level only — written inside a group
   or an argument list, `(a()) (b())` is a call again.

### Bindings and assignment

```ebnf
binding         = pattern-binding | member-binding | env-binding ;
pattern-binding = pattern "=" assignment-value | name "+=" value-expression ;
member-binding  = member-target ( "=" | "+=" ) value-expression ;
env-binding     = ( env-target | env-element-target ) ( "=" | "+=" ) value-expression ;
assignment-value
                = value-expression | background-job ;
background-job  = pipeline "&" ;

pattern         = name | "_" | list-pattern ;
list-pattern    = "[" NL* ( list-pattern-item ","? NL* )* "]" ;
list-pattern-item
                = name | "_" | "..." name ;

member-target   = "$" root place-access+      # `$m.key`, `$xs[0]`, `$m.rows[1].name`
                | "${" root place-access+ "}" ;   # `${m.a} = 2`, `${xs[0]} += 2`
place-access    = "." name | index-subscript ;    # an `access` whose subscript is no range
root            = name ;                      # never `env` — see `env-target`
env-target      = "$env" env-entry | "${env" env-entry "}" ;
env-entry       = "." name
                | "[" key-subscript "]" ;     # a computed key, resolved at run time
env-element-target                            # `$env.PATH[0]`, `${env[$k][1]}`
                = "$env" env-entry index-subscript+
                | "${env" env-entry index-subscript+ "}" ;
index-subscript = "[" key-subscript "]" ;     # no range: a slice is not a place
key-subscript   = signed-integer | name | "$" name | quoted-string ;   # no range
```

`=` binds and `+=` appends. Every target takes both — `$m.a += 5` and
`$env.NAME += value` are appends — except a **list pattern**, which has nothing
to append to, so `+=` there requires a plain name. A `member-target` writes
*into* a bound collection rather than rebinding the name; an `env-target` writes
the process environment. A modifier is not a place, so `$xs:dedup = …` is a
syntax error about places rather than a command.

Neither is a **slice**: it names a copy of a run of elements rather than somewhere
to store one, so `$xs[0..1] = …`, `$m.rows[1..] += …` and `$env[0..2] = …` are
syntax errors, on every target and under `global` alike — and so is
`unset $xs[0..1]`, since `unset-target` takes the same `place-access`. Refusing
them here rather than where the write happens is what keeps an assignment's
right-hand side from running first: `$xs[0..1] = $(cmd)` does not run `cmd`. A
slice is told from a key by the same `..` in the subscript text that a *read*
uses, so one classification serves both, including its edge: a quoted key
containing `..` is a slice to either, and neither can spell it. The run-time
refusal stays for what the grammar cannot see, since a computed subscript
(`$xs[$i] = …`) is not a range until it is evaluated.

The two are disjoint, and `env` is spelled out separately because the rules
differ. A `member-target` chains freely — `$m.rows[1].name` is a place — while
`$env` reaches an entry and then only *indexes*: an entry is the smallest thing
the environment has, and a `.member` under one is refused in the grammar because
no entry is ever a map, whatever its name.

Both targets take the braced spelling as well as the bare one, since the braces
are stripped before the accesses are walked: `${m.a} = 2` and `${env[$k]} += x`
are the same places `$m.a` and `$env[$k]` are.

An `env-target` names a whole entry, and a bare `$env` is the table rather than
one of them, so it takes exactly one access. Its subscript is a `key-subscript`
rather than a full one, so `$env[0..2] = x` is out: a slice names a copy of a run
of entries, not somewhere to store one. A key naming a computed entry rides along
as text, since which entry it names is a run-time question.

An `env-element-target` reaches one further, into the **value** an entry reads
as: the path-type names are lists, so `$env.PATH[0] = /z` replaces one directory.
It is a whole-entry write underneath — the entry is read, the element changed,
and the whole thing serialized back — which is why it is spelled here rather than
folded into `member-target`, and why `+=` on one appends to the *element*.
Whether a name is path-type is not a question the grammar can answer, so
`$env.HOME[0] = x` parses and the run time reports it, the way the matching read
already does. A modifier ends any of this: it names a derived value, so
`$env.PATH:dedup = …` and `${env.PATH[0]:upper} = …` are syntax errors about
places rather than about the environment.

In a `list-pattern`, names bind by position, `_` discards, and at most one `...`
rest binding is permitted; items are not recursively patterns. The rest need not
come last — it may sit anywhere, and the items after it bind from the **end**, so
`[...init, last] = [1, 2, 3]` gives `init` the first two and `last` the third,
and `[a, ...mid, z]` binds both ends. That is unlike a **parameter** list, where
`...rest` must be final.

An `=` may take a trailing-`&` pipeline as its right-hand side. In
`j = make -j8 &` the ampersand belongs to `background-job`, so the pipeline is
launched and its job handle bound; it does not background an assignment. `+=` is
value-only.

`NAME=value cmd` is an environment **prefix**, not an assignment — only what
follows the bindings tells them apart. It binds per pipeline **stage**, so
`FOO=1 a | FOO=2 b` gives each side its own and `FOO=1 a && b` leaves `b` alone.

### Scoped statements

```ebnf
scoped          = "global" ( global-binding | member-binding | "unset" unset-target+ )
                | "unset" unset-target+
                | "export" ( env-prefix+ | name ( "=" | "+=" ) value-expression ) ;
unset-target    = name | member-target | env-target ;
global-binding  = pattern "=" value-expression | name "+=" value-expression ;
```

A `global-binding` is `pattern-binding` without the `background-job` right-hand
side: `j = sleep 1 &` binds a job handle, but `global j = sleep 1 &` is a syntax
error, `global` taking a value only.

`unset` drops a binding, a place inside one, or an environment entry
(`unset $env.KEY`, `unset $env[$name]`) — the same `env-target` the assignment
side takes, so a write and a removal cannot disagree about what `$env[…]` means.

`global` does not govern the environment, since the environment is the process's
whatever function is running — but the two spellings refuse it at different
stages. `global $env.X = y` is a **syntax** error, an `env-target` not being one
of the places `global` can take, while `global unset $env.X` parses and is
refused when it runs.

`global`, `unset`, `export`, `fork`, `with`, `wrapper`, and `alias` are
**contextual**: each leads a statement only in the shape that statement takes,
so a command or variable of the same name stays reachable. `global = 5` still
binds a variable named `global`.

`export A=1 B=2` takes the same unspaced run a prefix and a `with` header take;
`export NAME = value` is the single-binding spaced spelling.

### Definitions

```ebnf
definition      = "wrapper"? "func" definition-name parameter-list capture-list? NL* block
                | "alias" alias-name capture-list? "=" NL* alias-command ;
definition-name = bare-WORD ;                 # unjudged here; checked when it runs
alias-name      = definition-name
                | computed-name ;             # a word holding an interpolation
computed-name   = WORD ;                      # quoted or not; read where it runs
alias-command   = env-prefix* redirection* command-word command-item* ;  # no guard
parameter-list  = "(" NL* ( parameter ( NL* "," NL* parameter )* NL* )? ")" ;
parameter       = name                        # required positional
                | name "=" NL* value-expression     # optional positional
                | flag-name                   # switch flag
                | flag-name "=" NL* value-expression   # valued flag
                | "..." name ;                # rest, last; the name must abut the `...`
flag-name       = "--" name ;                 # one token: the dashes abut the name
block           = "{" statement-list "}" ;
```

Parameters are comma-separated, and the comma is required between two of them
with no trailing one allowed: `func f(a b)` and `func f(a,)` are both errors.
Required positionals precede optional ones, `...rest` comes last, and flags are
order-independent. An optional positional and a `...rest` cannot coexist. A rest
parameter's name must **abut** its `...`, so `func f(... xs)` is an error while
`func f(...xs)` is the rest parameter. Parameter names must be distinct and
cannot be either reserved namespace, `env` or `sh`. A `wrapper` may declare no
flags of its own — that is the whole content of the marker.

A **`definition-name` is taken unjudged**, which is the one place this grammar
deliberately accepts more than the shell will run. It is assembled from the same
pieces a bare word in *command* position is, so the spellings the lexer splits
(`a.b`, `a:b`, `a[0]`) arrive whole instead of stopping the parser mid-name, and
every rule about *which* names are allowed is checked when the definition runs:
not a reserved word, not a built-in value call (`re`, `style`, `link`, `glob`,
`files`, `dirs`), no `.` (which reads as member access), not the discard `_`, and
otherwise an ordinary `name`. So `func x-() { }` parses and reports ``func: `x-`
is not a name`` when it runs, costing only that definition rather than the file
around it.

Two things still end it at parse time. `=` does, and only in an **alias** —
that is what tells `alias NAME = COMMAND` from a command — while `func` has `(`
for the job and treats `=` as ordinary text, so `func a=b()` reports at its own
definition. And a **quoted** name is still `expected a name`, that being a rule
about a name being a name rather than about which names are taken.

An **alias may be named by a word instead**, and that is the one place a quoted
word reaches a definition: a `computed-name` is any word carrying an
interpolation — `$name`, `"${prefix}-st"`, `"${f()}"` — and it is evaluated where
the definition runs rather than read as text here. A `WORD`, so a modifier
belongs to the reference inside it (`$n:upper` is one word and applies); a
modifier written after a *closing quote* — `"$n":upper` — is a value expression
everywhere else in the language, and the name slot does not take one, so it
stays the text `foo:upper` and is refused as a name. `TODO.md` carries it. A word with nothing computed
in it is a `definition-name` and the paragraph above governs it unchanged, so
`alias "foo" = …` is still `expected a name` while `alias "$x" = …` is a name
built at the definition. What comes back is judged by the same runtime rules a
written name is; only `alias` takes one, `func` does not.

A **`capture-list` on a definition** is the same list a lambda takes, in the same
words, and means the same thing: the names are read where the definition *runs*
and copied into the stored function, so a definition written in a loop keeps that
pass's values. Without one a body reads its names when it is *called*, which is
unchanged — the list is opt-in, and every definition that predates it behaves as
it did. On an `alias` it sits **before the `=`**, because after the `=` every word
belongs to the command being aliased and a trailing list would be arguments to
that. A name in both the capture list and the parameter list is refused, on a
definition as on a lambda; for an `alias` that means `$args`, the rest parameter
the desugaring synthesizes.

`alias NAME = command` is sugar for the `wrapper func` that forwards `...args`;
a **command head** equal to the alias's own name is emitted as `command NAME`, so
a self-naming alias reaches the program rather than recursing. The head is the
first item that is not a redirection, since one may be written in front of the
command it belongs to (`alias e = > /dev/null e hi`). Its right-hand side
is a plain command: a postfix guard is refused (`alias a = puts x if true` is an
error) and belongs in a `wrapper func` body instead. A **quoted multiword first
word** is refused too — `alias ll = 'ls -l'` is the bash reflex, and here the
quotes make one word, so it would name a program with a space in it; write
`alias ll = ls -l`.

### Control flow

```ebnf
control-flow    = if-expression
                | match-expression
                | for-expression
                | while-expression
                | loop-expression
                | fork-expression
                | with-expression
                | control-statement ;

if-expression   = "if" condition NL* block ( NL* "else" NL* ( if-expression | block ) )? ;
condition       = "not"* ( binding-condition | pipeline ) | value-expression ;
binding-condition
                = pattern "=" value-expression ;

for-expression  = "for" pattern ( "," pattern )? "in" value-expression NL* block ;
while-expression
                = "while" condition NL* block ;
loop-expression = "loop" NL* block ;
fork-expression = "fork" NL* block ;
with-expression = "with" env-prefix+ NL* block ;

match-expression
                = "match" value-expression NL* "{" NL*
                  ( match-arm ( terminator+ match-arm )* terminator* )? "}" ;
match-arm       = match-pattern ( "|" NL* match-pattern )* match-guard? NL* "=>" NL* match-body ;
match-guard     = "if" value-expression ;
match-body      = value-expression | block ;
match-pattern   = "_" | "*" | list-pattern | pattern-value ;
pattern-value   = value-expression ;          # may not *begin* with `[`

control-statement
                = "return" channel-word? control-value? guard?
                | ( "fail" | "break" | "continue" ) control-value? guard? ;
channel-word    = "status" | "value" ;        # only with no `(` abutting it
control-value   = value-expression ;          # may not *begin* with `if` / `unless`
```

A `condition` may be a command, whose status decides, or a value, whose
truthiness does. `not` is recognized in both positions and folds to its parity;
before a value it is the ordinary `not` operator, before a command it negates
the status the command really exited with.

A `binding-condition` is a distinct test-and-bind node rather than an ordinary
assignment statement, and it negates like any other condition:
`if not [head, ...tail] = $xs { … }` branches on the destructuring having
failed, in `while` as much as in `if`.

Match arms are terminator-separated — a newline or `;`, never a comma — and the
separator is required, so `a => 1 b => 2` does not parse. Only newlines lead the
body, though: `match x {; a => 1 }` is an error, unlike the trailing terminator
after the last arm. `=> value` is a value
expression, where a bare word is a string; `=> { … }` is a block in ordinary
statement context, where a bare word runs. A `match-pattern` uses the
value-expression grammar, so it includes exact values, ranges, bare globs, and
regex literals; `*` is listed separately because it is accepted as a catch-all
glob even though it cannot begin an ordinary value expression.

A leading `[` is the exception, and always means `list-pattern` — a bracketed arm
**destructures** rather than compares. Its elements must therefore be binder
names, `_`, or a `...rest`, so `[h, ...t] =>` and `[_] =>` are arms
while `[1] =>` is `expected a name` rather than a one-element exact match. Test
an exact list by binding and comparing in the guard.

A guard attaches to **one** simple command, control statement, or value
expression — before any `;`, newline, `&`, pipeline, or conditional operator. It
is not a suffix on a whole pipeline. `unless` negates its condition.

A **channel word** is recognized only directly after `return`, and says which
channel the operand fills: `return status 5` is the status `5` (sugar for
`return status(5)`), `return value 5` is the value `5`, which is what a bare
`return 5` already means. An **attached `(` is a call, never a channel word** — the
same discrimination `f arg` and `f(arg)` already draw — so `return value(5)` calls
whatever `value` names and `func value` stays legal. The words reserve nothing, but
a channel word with no operand is an error rather than the string it used to bind.

Because a control statement's guard is claimed first, its value may not *begin*
with `if` or `unless`: `return if true { 1 }` reads the `if` as the guard and
then fails. Parenthesize to return the branch's value —
`return (if true { 1 })`.

`fork` and `with` take the block form only where a block or a `NAME=` binding
follows, which is what keeps a command of either name reachable. The newline
form (`fork` ⏎ `{ … }`) needs the whole source at once, so it holds in a script
but not at an interactive prompt, where a lone `fork` is already a complete
command.

## Value expressions

Precedence from lowest to highest. Binary tiers associate left except comparison
and range, which are non-associative; postfix operations associate left.
Assignment and command `&&` / `||` are statement grammar and do not appear here.

| Precedence | Forms | Associativity |
| --- | --- | --- |
| 1 | `or` | left |
| 2 | `and` | left |
| 3 | `not` | prefix |
| 4 | `==`, `!=`, `<`, `<=`, `>`, `>=`, `~`, `!~`, `in` | none |
| 5 | `..`, `..=` | none |
| 6 | `+`, `-` | left |
| 7 | `*`, `/`, `%` | left |
| 8 | prefix `-`, spread `...`, function reference `&` | prefix |
| 9 | call, member access, index, `:` modifier | left postfix |
| 10 | primary values and adjacent word pieces | n/a |

```ebnf
value-expression = or-expression ;
or-expression   = and-expression ( "or" NL* and-expression )* ;
and-expression  = not-expression ( "and" NL* not-expression )* ;
not-expression  = "not" not-expression | comparison ;   # no NL after `not`
comparison      = range-expression ( compare-op NL* range-expression )? ;
compare-op      = "==" | "!=" | "<" | "<=" | ">" | ">=" | "~" | "!~" | "in" ;
range-expression
                = additive ( ( ".." | "..=" ) NL* additive? )?
                | ( ".." | "..=" ) additive? ;    # no NL after the operator here
additive        = multiplicative ( ( "+" | "-" ) NL* multiplicative )* ;
multiplicative  = prefix ( ( "*" | "/" | "%" ) NL* prefix )* ;
prefix          = ( "-" | "..." ) prefix | func-ref | postfix ;
func-ref        = "&" name ;                  # `&` must abut the name; no operand
postfix         = primary postfix-part* ;
postfix-part    = call-arguments | member-access
                | index-access                # `[` must abut what it indexes
                | modifier-call ;             # `:` must abut, and its `(` must abut the name
call-arguments  = "(" NL* argument-list? NL* ")" ;
member-access   = "." name ;
index-access    = "[" NL* value-expression NL* "]" ;
modifier-call   = ":" name call-arguments? ;

primary         = qualified-glob                # before `value-call`: see below
                | WORD | variable-ref | list | map | capture | lambda
                | value-call | modifier-ref | regex-literal
                | "(" NL* wrapped-expression NL* ")"
                | if-expression | for-expression | match-expression ;
wrapped-expression
                = value-expression ;          # `NL*` *precedes* every binary and
                                              # range operator in here as well
value-call      = name call-arguments ;
lambda          = "func" parameter-list capture-list? NL* block ;
capture-list    = "with" NL* "(" NL* ( capture ( NL* "," NL* capture )* NL* ","? )? NL* ")" ;
capture         = "$" name ;                  # a read, not a declaration
modifier-ref    = ":" name ;
capture         = "$(" statement-list ")" ;

list            = "[" NL* list-items? "]" ;
list-items      = list-item ( list-separator? list-item )* list-separator? ;
list-item       = value-expression | "..." value-expression ;
list-separator  = NL* "," NL* | NL+ ;
map             = "[" ":" "]" | "[" NL* map-items "]" ;
map-items       = spread-prefix pair ( map-comma map-item )* map-comma? NL* ;
spread-prefix   = ( "..." value-expression list-separator? )* ;   # separated as a list
map-comma       = NL* "," NL* ;
map-item        = pair | "..." value-expression ;
pair            = value-expression ":" value-expression ;
argument-list   = argument ( NL* "," NL* argument )* NL* ","? NL* ;
argument        = value-expression
                | name ":" value-expression
                | "..." value-expression ;
```

A `capture` runs its statements in command-substitution mode and produces
captured output for the following postfix chain, so `$(cmd):raw` is a modifier
applied to the capture node. A `lambda` is the named definition form with the
name left off and reuses the same parameter and block grammar.

A `modifier-ref` — a bare `:name` in expression position — denotes the
one-argument function that applies that modifier, so `$paths:map(:stem)` says
what `$paths:map(func(p) { $p:stem })` says. Only there: a command word
beginning with `:` stays literal text, and the colon of a map key or a named
argument is unaffected. The attached call form `:name(…)` also starts a value,
so it can open a condition or a statement.

`[…]` is a **map** if it contains a `key: value` pair or uses the empty-map form
`[:]`, and a **list** otherwise; `[]` is the empty list. Once pair syntax
selects a map, every entry must be a pair and entries are comma-separated —
mixed entries are a syntax error. Quote a literal colon where that distinction
would otherwise select syntax.

That switch happens mid-literal, which is what `spread-prefix` records: the
parser reads in list mode until a pair arrives, so **leading spreads separate
like list items** and need no comma. `[...$m k: 2]` is a map, and so is
`[...$m ...$n k: 2]`, while `[...$m ...$n]` — never having seen a pair — is a
two-element list. After the first pair the map rules apply for the rest of the
literal: `[...$m k: 2 j: 3]` wants the comma, and `[1 k: 2]` is refused outright,
since only a spread may precede a pair without one.

A list's separator is optional, so `[a b]` is two elements just as `[a, b]` is,
and a trailing one is allowed (`[a,]`). A map needs its commas: `[k: 1 j: 2]` is
an error, a newline there not standing in for one. Neither doubles a comma —
`[a,,b]` is an error either way. Both take newlines after the `[` and around
each comma, which is what lets a map be written a pair per line.

Ranges sit above comparison and below arithmetic and postfix access, so
`$xs[1 + 1..$n - 1]` has arithmetic endpoints and `$x in 1..=10` compares `$x`
with one range. Chained comparisons (`a < b < c`) are errors; use `and`.

A range gets the same guard, and for the same reason — a range is not an endpoint,
so `1 .. 2 .. 3` is `ranges cannot be chained` rather than something that parses
and then fails at evaluation. It covers the spellings that reach that shape
through an operand too, since both endpoints are optional: `1 .. ..3` and
`..1 .. 2` answer alike. Group to say it (`(1 .. 2) .. 3` parses, and is the
engine's problem from there).

Only the **infix** operator takes newlines after it: `1 ..` ⏎ `2` is one range,
while `..` ⏎ `3` is an open-ended range followed by a separate statement.

A postfix chain is one chain: `$x.a[0]:get(k):len` parses left to right.
Argument-free modifiers take no empty parentheses — `:first()` is rejected where
`first` is defined as argument-free.

**An index and a modifier must abut** what they apply to, and a modifier's
argument list must abut its name: `$xs [0]`, `abc :upper`, and
`abc:replaceall ("b", "-")` are all errors. Member access and a call are not
held to this, so `$m .a` and `g ()` parse — except in argument position, where
a call's `(` must abut too, since the spacing there separates arguments (rule 4
under [value arguments](#value-arguments)).

An attached `..` before a slash stays a path: `../x` is the parent directory,
not a range. A lone `*` is the glob rather than multiplication, since a binary
`*` is consumed before its right operand is parsed.

## Command or value

An assignment, definition, or value expression is never inferred by *expanding*
a word — the parser selects it from unquoted syntax. A bare word on an
assignment right-hand side stays a string (`x = greet`), while attached
parentheses select a value call (`x = greet()`).

A syntactically recognizable value expression is valid as a statement, which is
how a block returns a scalar, collection, capture, or operator expression as its
value. A command-shaped bare word remains a command, preserving the
shell-oriented default.

Where the two readings collide, three rules decide, in order:

1. **A redirect is found by scanning to the end of the command word** — a word
   plus its *attached* argument-free `:modifier` suffixes. A spaced one is the
   next argument, so `$e :len` runs `echo :len`. That bound is why `$x + 1 > 1`
   and `$xs[0 + 0] > 0` stay comparisons: neither can be a command word, since a
   word cannot contain a nested expression. `$xs[0] > f` does redirect, a
   literal index being part of the word.
2. **Position decides a spaced `<` / `>`**: a comparison in a condition, a
   redirection in a statement. So `if $xs:len > 5` compares while
   `$p:base > log` redirects.
3. **Everything else is settled by parsing the statement and looking at the
   expression's leading operand** — the leftmost thing it hangs off, since that
   is the only part a command line can also be. A bare word leading it keeps the
   command reading (`ls / extra` is not a division, `exit -1` not a subtraction,
   `ls ..` not a range); a variable, integer literal, quoted word, list, group,
   or capture has no command spelling and takes the value reading.

The value must also account for the whole statement. `$editor file`,
`$editor ...$files`, and `$editor | cat` are command lines, and so are
`$editor || puts oops` and `$editor &`, because a connector or a backgrounding
`&` picks the command whenever the value *is* a command word — an unbroken run
of text led by a variable. That is the `$cmd || fallback` idiom, and
`$p:base || fallback` is the same idiom with a suffix the command word keeps;
so are `${cmd}.exe`, `${cmd}[0]`, and `${cmd}-1`.

**Whitespace separates the two readings**, not the shape of the expression —
`${cmd}-1` and `$a - 1` are the same subtraction of the same variable — so
spacing the text apart gives the value reading, and `$a - 1 || puts smaller`,
`$a == $b || puts ne`, and `$x ~ /b/ && puts matched` all keep theirs. A `(` is
ruled out whatever the spacing, command position having no call syntax, which is
why `$x:split("-") || puts x` keeps its value reading. A numeral leads no
command word at all, so `1 == 2 || puts no` compares.

An `if` or `unless` in command position starts a postfix guard only when the
remaining tokens form a complete value expression; quoting the word, or leaving
no viable guard expression, keeps it a command argument. Keywords are recognized
only where the grammar expects them, so an ordinary command may still receive
`if`, `for`, or `match` as an argument.

Rule 3 parses the statement as a value, and a parse that **fails** normally hands
the text back to the command reading — text that is not an expression is a
command. A **chaining** error is the exception: `1 .. 2 .. 3` and `$x == 2 == 3`
are refused as statements rather than run as commands, since the parse got two
operands and an operator before the second operator arrived, which is not "never
a value." Handing those back would read the operators as arguments and run `$x`.

The exception is held to the same two tests a *successful* parse takes, so it
never claims a command line whose arguments merely look like operators. A bare
word still leads a command (`puts .. 2 .. 3` prints `.. 2 .. 3`, `ls .. ..` lists
two parent directories), and an unbroken run is still one command word
(`${cmd}==a==b` names a program). Only where the leading operand has no command
spelling — an integer, boolean, quoted word, variable, list, group, or capture —
does the chain win.

## Completeness and errors

The parser reports one of three outcomes: a complete syntax tree, **incomplete**
input, or an error. Input is incomplete only when adding tokens could complete
the current production — an unclosed quote or delimiter, a trailing connector or
operator, or an unfinished heredoc body. A line-at-a-time reader keeps reading;
at the end of a script or a `-c` string the same state is a syntax error, and
one that says where. An unfinished heredoc is reported distinctly so a reader
can wait for that one delimiter line instead of re-parsing the buffer per line.

An unexpected closer, a repeated separator, or an operator that cannot continue
the current production is immediately an error. Nesting deeper than 100 levels
is refused before the recursive descent runs out of stack.

There is **no error recovery inside a parse unit**. The first error ends the
parse and is what `parser::parse` returns, so a syntax error on one line of a
script aborts the whole file rather than resuming at the next statement — in
`puts before` ⏎ `x = = =` ⏎ `puts after`, nothing runs. The recovery boundary is
the *reader's*: a line-at-a-time reader — interactive or piped — reports the
error and reads on, so the same three lines print `before` and `after` around
the diagnostic. A bad line costs only that line at the prompt, and the whole
script in a file.

## Not yet parsed

These are syntax errors today:

- Piping or redirecting a `fork` block (`fork { … } | cat`, `fork { … } > log`),
  and the `fork func name(params) { … }` form.
- Recursive list patterns: a `list-pattern` item is a name, `_`, or `...name`,
  never a nested pattern.

## Parsed but not executable

The grammar accepts these and the refusal comes when the statement runs, with a
message naming what is missing rather than a silent reinterpretation. They are
listed because a `mesh -n` parse check passing does not mean they work:

- A redirection with no command (`> f`). The refusal names the working
  spelling: `exec > f` retargets the shell itself.
- Backgrounding anything but a command or pipeline. `&` on an expression,
  assignment, `if` / `match`, loop, `fork` block, or definition is refused
  rather than run in the foreground.
- `global` on an environment removal (`global unset $env.KEY`), per
  [Scoped statements](#scoped-statements).

See [`TODO.md`](TODO.md) for the working front and [`docs/DESIGN.md`](docs/DESIGN.md) for
the forms still under design.
