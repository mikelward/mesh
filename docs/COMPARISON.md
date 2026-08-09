# mesh compared with bash, zsh, YSH, fish, elvish, and nushell

Six shells worth measuring mesh against, because each one answers the same
question differently.

- **bash** is the baseline — what nearly everyone arrives from, and the source
  of the specific problems mesh exists to fix.
- **zsh** is bash's machinery with better defaults. It is the interesting one:
  it fixed the biggest footgun while keeping POSIX syntax and an emulation mode
  that still runs old scripts — so the fix cost far less than a new language.
  The safe default is itself a departure from POSIX expansion; the point is how
  small a departure bought how much.
- **YSH** (Oils) is the upgrade path taken from *inside* POSIX. One binary is
  two shells: run it as `osh` and it is a bash-compatible shell that runs your
  old scripts; run it as `ysh` and the same interpreter turns on a new language
  with real values and no word splitting. Nobody else on this page offers a
  migration that granular.
- **fish** is the friendly shell that broke cleanly and kept the Unix shape:
  byte pipes, external commands everywhere, no word splitting.
- **elvish** is the closest relative by design — real values, byte pipes, and a
  strictness that in two places exceeds mesh's own. It is the one to read if you
  want to know whether mesh's ideas needed a new shell to hold them.
- **nushell** is the structured-data shell: the pipeline carries tables and
  records rather than bytes, and much of coreutils is replaced by builtins that
  speak that format.

mesh sits between fish and elvish, which is the order the tables below are
written in — roughly from most POSIX to least, with YSH placed by its `osh`
half rather than by the language it turns on. It takes fish's answer to
expansion safety and pushes it further into a real type system — lists, maps,
integers, booleans — while keeping nushell's ambition out of the pipe: **pipes
carry bytes**, so `grep`, `jq`, `ffmpeg`, and everything else you already run
works unchanged. See [`DESIGN.md`](DESIGN.md) for why each call went the way it
did, and [`REFERENCE.md`](REFERENCE.md) for what is actually implemented today —
this page compares designs, and mesh's is still being built.

## At a glance

| | bash | zsh | YSH | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Space in unquoted `$x` | **splits** | one arg | one arg | one arg | one arg | one arg | one value |
| `*` in unquoted `$x` | **re-globs** | literal | literal | literal | literal | literal | literal |
| Unquoted command substitution | **splits on `IFS`** | **splits on `IFS`** | one string | splits on newlines | one string | splits on newlines | one value |
| Unquoted empty `$x` | **vanishes** | **vanishes** | one empty arg | one empty arg | one empty arg | one empty arg | one empty value |
| Lists | bolted-on | native | native | native | native | native | native |
| List → argv | `"${a[@]}"` | implicit | `@a` | implicit | `...$a` | `$@a` | `...$a` |
| Chained value transform | none — `${…}` takes a name, not a value | `${p:t:r}` | `=> f()`, expression mode | `path`/`string` pipeline | `$p:base:stem`, anywhere | nested call | pipeline |
| Pipe payload | bytes | bytes | bytes | bytes | bytes | bytes + values | **structured** |
| Coreutils | first-class | first-class | first-class | first-class | first-class | first-class | second-class |
| Unset variable | `""` | `""` | error (at run time) | `""` | error (at run time) | **compile error** | **parse error** |
| A failed command | status | status | **aborts** | status | status | **aborts** | **aborts** |
| `pipefail` | opt-in | opt-in | **always on** | `$pipestatus` | **always on** | aborts, reports all | any stage aborts |
| Truthiness | status + tests | status + tests | typed | status | **none** | value | typed |
| `'…'` | raw | raw | **raw, no way out** | nearly raw | **escapes** | raw | raw |
| `"…"` interpolates | yes | yes | yes | yes | yes | **no** | **no** (`$"…"`) |
| `"\n"` is a newline | no (`$'\n'`) | no (`$'\n'`) | **no — an error** | no | yes | yes | yes |
| Runs POSIX scripts | yes | mostly | **yes, as `osh`** | no | no | no | no |
| Config language | bash | zsh | YSH | fish | mesh | elvish | nu |

Command substitution is spelled `$(…)` in bash, zsh, YSH, mesh, and fish; elvish
and nushell write a subexpression as `(…)`, and `$(…)` in nushell is a parse
error.

**Three rows go against mesh, and they are the ones worth reading first.** On
the unset-variable row mesh is the weakest of the four shells that refuse at
all: elvish catches it when it compiles the code and nushell catches it while
parsing, both before any of the program runs, where mesh and YSH only report it
once execution reaches the statement — so a typo down a branch your tests never
take survives in mesh and does not in the first two. And a command that fails
aborts the script by default in all three, where mesh leaves it in `$sh.status`:

```
$ elvish -c 'false; echo AFTER'        # AFTER never prints
$ nu -c '^false; print AFTER'          # AFTER never prints, nu exits 1
$ ysh -c 'false; echo AFTER'           # AFTER never prints
```

nushell is the least obvious of the three — a failing external raises a
catchable error rather than an exception, so `try`/`catch` and `complete`
recover the status, but left alone it ends the script exactly as elvish does,
and it does so for a failure in *any* pipeline stage, not only the last. YSH
gets there from the opposite direction: it is `errexit` from bash, but repaired
rather than inherited, since the shell that gave everyone `set -e` is also the
one whose `set -e` is famous for the cases it silently skips. Three stricter
readings of the same instinct this page argues for, and the abort is now on
[`TODO.md`](../TODO.md).

The third row is `'…'`, and it is the one place on this table where mesh and
YSH are the two outliers pointing in **opposite** directions — covered under
[the string forms](#the-string-forms-side-by-side) below.

## Quoting and escaping

This is the part of a shell you feel every day, so it gets the most room.

### bash: expansion is a second round of evaluation

The root of it is that bash does not evaluate a word once. It runs a **pipeline
of expansion stages** over each word, in a fixed order, and the later stages
operate on the *output* of the earlier ones:

1. brace expansion — `{a,b}`
2. tilde expansion — `~`
3. parameter, arithmetic, and command expansion — `$x`, `$((…))`, `$(…)`
4. **word splitting**, on the result of 3, using `IFS`
5. **pathname expansion** (globbing), on the result of 4
6. quote removal

Stages 4 and 5 are the problem. They re-scan text that stage 3 *produced* — so a
value's own bytes get read back as though they were source code. That is `eval`
in all but name, applied to every unquoted expansion in the script:

```bash
file='My Photo.jpg'
rm $file            # two arguments: "My" and "Photo.jpg"  — stage 4
rm "$file"          # one — the quotes suppress stages 4 and 5

pattern='report[2024].txt'
ls $pattern         # a bracket expression, not a filename    — stage 5
```

Nothing about `rm $file` in the source says whether it is one argument or five.
The answer arrives at run time from three places at once: the bytes in the
variable, the current value of `IFS`, and what happens to be sitting in the
directory.

### The three defenses, and why none of them is enough

bash gives you three tools, and safe scripts reach for all of them:

| Defense | Stops | Does not stop |
| --- | --- | --- |
| Quote every expansion — `"$x"`, `"${a[@]}"`, `"$@"` | 4 and 5 | anything you forget, once |
| `IFS=` (or the strict-mode `IFS=$'\n\t'`) | 4 | globbing |
| `set -f` / `set -o noglob` | 5 | splitting |

Quoting is the only one that is correct, and it is per-site: it has to be right
at every expansion in the program, and a single missed one is a live bug that
shows up the day a filename has a space in it. The other two are **global
modes** — dynamic state, not a property of the line you are reading. `IFS` is
an ordinary variable, so a sourced file, a function, or a caller can change what
your word splitting does; `set -f` turns off the globbing you *wanted* along
with the globbing you did not. Neither is visible at the site it affects, which
is why the "unofficial strict mode" preamble exists at all:

```bash
set -euo pipefail
IFS=$'\n\t'
```

And each hop multiplies it. Text that passes through `eval`, `ssh host "$cmd"`,
`find -exec sh -c`, or `xargs` is parsed *again* at the far end, so the quoting
has to survive two or three rounds — which is where the four-backslash
incantations come from.

So in bash quoting is not about literalness. It is how you *turn off* an
evaluation stage that is on by default. Arrays inherit the whole thing and add
ceremony: `"${a[@]}"` is four pieces of punctuation that all have to be right.

### zsh: the same machinery, better defaults

zsh is the counter-argument to this whole page, and it deserves to be made
properly: it kept bash's expansion pipeline and simply **turned the dangerous
stages off by default**. A parameter expansion is not split and not re-globbed,
with no options set and no preamble:

```sh
x='My Photo.jpg'
printf '[%s]\n' $x         # [My Photo.jpg] — one argument, unquoted

p='report[2024].txt'
printf '[%s]\n' $p         # [report[2024].txt] — literal, not a pattern
```

That is the single biggest bug in shell programming, fixed, in a shell that
still *looks* like `sh` and can still run old scripts under `sh` invocation or
`emulate sh` — the exemption above is a deliberate non-POSIX default, and the
compatibility lives in the emulation beside it. It is also why zsh sits second
in these tables rather than beside bash — most of what the rest of this page
argues for, zsh already has. Arrays are native, `$a` expands one word per
element with the spaces intact, an unmatched glob is an **error** rather than
silently passed through, and `$pipestatus` is a real array. One wrinkle to know
if you move between them: zsh indexes arrays from **1**, where mesh's lists —
like most languages, and unlike zsh — start at **0**, so `$xs[0]` is the first
element.

Three residuals remain, and they are what mesh is answering rather than
repeating:

- **Command substitution still splits.** The exemption is for *parameter*
  expansion only, so `$(…)` goes through stage 4 as before, on `IFS`:

  ```sh
  printf '[%s]\n' $(echo 'a b c')     # [a] [b] [c] — three arguments
  x=$(echo 'a b c'); printf '[%s]\n' $x   # [a b c] — one
  ```

  The same characters mean different arities depending on whether the value was
  bound to a name first. In mesh a capture is one string in both spellings, and
  splitting is written (`$(…):words`).

- **An empty unquoted expansion still vanishes.** `e=''; printf '[%s]' a $e b`
  prints `[a][b]` in both bash and zsh — two arguments, not three — so `"$e"`
  stays load-bearing to pass an empty string. YSH, fish, mesh, elvish, and
  nushell all print `[a][][b]`.

  The distinction the other five can make and bash and zsh cannot is between a
  value that is empty and no value at all. An empty *list* should contribute
  nothing, and does — fish's `set e; printf '[%s]' a $e b` gives `[a][b]`,
  correctly, because there are zero elements. A list holding one empty string is
  a different thing, and survives. In bash and zsh both collapse to the same
  disappearance, which is why the quotes are needed to tell them apart.

- **The safety is a *default*, not a *rule*.** `setopt shwordsplit` brings
  splitting back, `setopt globsubst` brings re-globbing back, and `emulate sh`
  brings back both at once — verified, all three. Options are global dynamic
  state, so this is the same objection as `IFS` and `set -f` one level up: what
  a line of zsh does depends on what has been `setopt`-ed by the time it runs,
  including by a sourced file or a function you did not write. It is a much
  better default and it is still a default.

That last point is the whole difference in one sentence. **zsh made the safe
behavior the default; mesh makes it the grammar.** There is no mesh option that
reintroduces word splitting, because there is no splitting stage to switch back
on.

#### How close can you tune zsh?

Worth knowing if you are staying, and it is a sharper test of the claim above
than any assertion: which of these gaps close by configuration? Every one of
zsh's 185 options was tried.

Two close outright, and both are worth setting:

```sh
setopt nounset          # $nosuch is an error, not ""      — as in mesh
setopt pipefail         # a pipeline reports its failure   — as in mesh
setopt warncreateglobal # catches an accidental global in a function
```

The two expansion residuals close at **no setting**. No option stops the empty
elision — not one of the 185 — and none stops command substitution splitting;
the only lever for that is `IFS=`, a variable rather than an option, and it
takes the splitting you *wanted* with it: under `IFS=` a `for w in $(…)` loop
runs once with the whole output, and `read -A parts` on `a:b:c` gives one field
instead of three. The scoped form (`IFS=$'\n' read -r line`, or an explicit
`${(f)"$(…)"}`) is the answer, per site.

Which is the same shape as the argument itself. The gaps that close are the ones
that were **policy** — what to do about an unset name, how a pipeline reports.
The gaps that do not close are the ones built into **expansion**, and no amount
of configuration reaches them, because they are not configured.

### mesh: the shape is fixed at parse time, and values are eager

mesh has no stage 4 and no stage 5. A word's **arity and type are decided by how
the source is written**, before anything runs, and a value is produced once and
never re-scanned:

- **Each piece of a word keeps the mode it was written in.** The parser never
  reconstructs quote context from a flattened string, so whether a `*` is a
  pattern is settled by the source that produced it — a bare `*` globs, a `"*"`
  or a `*` that arrived inside a variable is a character.
- **A string is one argument, always.** There is nothing to split on, so there
  is no `IFS`. A capture is one string too; splitting is a written modifier
  (`$(…):words`), never a default.
- **A list reaches argv only through a written `...`.** Arity is readable from
  the line rather than inferred from the data.

The consequence is that mesh has no `IFS` and no `set -f`, because it has
nothing for them to switch off — and no global mode can change what an
already-written line means:

```mesh
file = "My Photo.jpg"
pattern = "report[2024].txt"
rm $file                    # one argument, and no quotes needed to say so
ls $pattern                 # the literal name — not a bracket expression
ls *.txt                    # a glob, because the source says so
```

Compare the columns: bash's safety is a property of the *program* (did you quote
every site, and is `IFS` what you think), where mesh's is a property of the
*grammar*. That is the whole trade the rest of this page is describing.

### What fish, mesh, and nushell make you quote

All three make the safe case the default — as does zsh for the parameter case
above. A value is one value; nothing is inferred from its bytes:

```fish
set photo 'My Photo.jpg'
mv $photo album/        # one argument
```

```mesh
photo = "My Photo.jpg"
mv $photo album/        # one argument
```

```nu
let photo = 'My Photo.jpg'
mv $photo album/        # one argument
```

Quotes go back to meaning what they mean in every other language — "this is
literal text" — and you reach for them when the *source* has a character the
parser would otherwise read as syntax, not when a *value* might.

### When mesh requires an escape

An unquoted mesh word is literal text. These are the cases where it is not, and
where a `\` or a quote is needed to get the character itself:

| In an unquoted word | Why | Escape hatch |
| --- | --- | --- |
| space, tab | separates arguments | `a\ b`, `"a b"` |
| `*` `?` `[` | glob pattern | `\*`, `"*.rs"` |
| `~` at word start | home directory | `\~`, `"~"` |
| `$` | variable reference | `\$`, `'$'` |
| `#` where a word could start | comment (`a#b` is fine) | `\#`, `"#"` |
| `!` before a designator | history expansion | `\!`, `'!'`, `"!"` |
| `;` `&` `\|` `<` `>` | separators and redirections — tokens with no whitespace needed, which is what makes `ls>out` a redirect | quote or `\` |
| `(` `)` `{` `}` | grouping, and a `(` attached to a word is a call or a glob qualifier | quote or `\` |
| `,` where a list is being read | separates items, so `[a,b]` is two | quote or `\` |
| `=` leading a statement | assignment or an environment prefix — `x=1`, `FOO=bar cmd` | quote or `\` |
| `+ - * / % ~` and `=>` with a boundary each side | operators | quote or `\` |
| `:` followed by an identifier | a modifier chain | `"ubuntu:latest"` |
| `'` `"` `\` | quoting itself | `\'`, `\"`, `\\` |

The last rows are written per **position** rather than per character, because
mesh's punctuation is a token *where the grammar expects one* — so punctuation
attached to ordinary text mostly stays text. `a-b` is one word, `--flag=x` is
one argument, `puts a,b` prints `a,b`, and `puts value=>out` is a `value=` word
followed by a `>out` redirection. `[` and `]` are in the glob row above rather
than here: `xs[0]` is a pattern, so it finds a file named `xs0` and otherwise
contributes nothing.

Inside `"…"` and `'…'` the active set is the delimiter, `\`, and `$` in `"…"`
only. **The backslash stays live in both** — they decode the same escape set and
an unknown escape is an error rather than a literal, so `'a\b'` is a backspace
and `'\('` is refused. Only `r'…'` / `r"…"` are fully inert. Adjacent pieces
fuse, so a word can mix modes freely — `"$dir"/'sub'/$file` is one path.

Two of those rows are places mesh asks for **more** than bash, fish, or nushell
do, and both are the price of a feature bought elsewhere.

**`word:identifier` is reserved.** `:` starts a
[modifier](REFERENCE.md#modifiers) chain — `$path:stem`, `$xs:len`,
`$s:upper` — and the shape is reserved in argument position too, so the
vocabulary can grow without silently changing what an existing line means:

```text
mesh: syntax error: `:latest` is not a modifier; quote the whole word to keep
it as text ("x:latest"), or brace the name when it comes from a variable
("${x}:latest")
```

The cost is real and lands on familiar lines — `docker run "ubuntu:latest"`,
`git show "HEAD:file"`, `rsync "host:src" dst`, `curl -H "Accept:application/json"`.
The quotes go around the **whole** token; `"ubuntu":latest` is not the same
thing. bash, zsh, YSH, fish, elvish, and nushell all take those bare.

**`'…'` takes escapes, unlike every other shell.** In bash, zsh, YSH, fish,
elvish, and nushell a single-quoted string is (near enough) raw, which is why
every sed one-liner in existence is written in one. mesh follows Python instead:
`'…'` is `"…"` minus interpolation, with the same `\n \t \e \u{…}` set, and an
unknown escape is an error rather than a literal backslash. So this is a syntax
error in mesh and fine in the other six:

```bash
sed 's/\(a\)/[\1]/' file
```

The answer is the **raw** string, and the diagnostic says so:

```text
mesh: syntax error: invalid escape \(; for text holding its own backslashes
(a sed or awk program, a Windows path) use a raw string, `r'…'`
```

```mesh
sed r's/\(a\)/[\1]/' file
```

What the trade buys is that `'\n'` and `"\n"` mean the same thing, so mesh needs
no `$'…'` form — bash's third string type exists only because its `"…"` cannot
spell a newline.

### The string forms, side by side

| | bash | zsh | YSH | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Interpolating + escapes | — | — | — | — | `"…"` | — | `$"…"` |
| Interpolating only | `"…"` | `"…"` | `"…"` (`\$ \" \\` only) | `"…"` (few escapes) | — | — | — |
| Escapes only | `$'…'` | `$'…'` | `u'…'` / `b'…'` | — | `'…'` | `"…"` | — |
| Fully raw | `'…'` | `'…'` | `'…'` | `'…'` (bar `\'`, `\\`) | `r'…'` / `r"…"` | `'…'` | `'…'`, `r#'…'#` |
| Both quote kinds, no escaping | heredoc | heredoc | heredoc | heredoc-ish | `<< 'END'` heredoc | — | `r#'…'#` |

The top row is the surprising one: **only mesh and nushell can spell
interpolate-and-escape at all.** bash, zsh, and YSH have the two halves in
separate forms — `"…"` interpolates but leaves `\n` as backslash-n, and `$'…'`
reads the escape but not the variable (`x=foo; echo $'$x'` prints `$x`) — so
wanting both in one string means concatenating two of them. That is the gap
`$"…"` fills in nushell, and the reason mesh's `'…'` and `"…"` differ on exactly
one axis.

YSH is stricter than bash on its half of that split, and the strictness is the
interesting part. Its `"…"` keeps the escapes that are about the syntax —
`\$`, `\"`, `\\` — and refuses the ones that are about characters, where bash
passes them through as a literal backslash-n:

```text
$ ysh -c 'echo "a\nb"'
Invalid char escape in double quoted string (OILS-ERR-12)
```

The escapes live in `u'…'` (Unicode, valid UTF-8 only) and `b'…'` (bytes, which
also takes `\yff`), both borrowed from **J8 Notation**, Oils' JSON superset — so
a J8 data literal is also valid YSH source, which is a genuinely nice property
and the reason the prefixes exist at all. What it costs is that YSH cannot fuse
two of them the way bash can. Adjacent quoted parts are a hard error, so the
bash trick of writing `"x"$'\n'` has no YSH spelling in command mode; you drop
into expression mode and concatenate explicitly:

```text
$ ysh -c 'var s = "x" ++ u'\''\n'\''; write -n -- $s'
```

**And `'…'` has no escape hatch at all.** Every other shell here gives you *some*
way to put an apostrophe inside single-quote syntax — bash, zsh, and fish take
the POSIX close-reopen trick, elvish and nushell double the quote. YSH takes
neither, and says so:

```text
$ ysh -c "echo 'it'\''s'"     # the bash spelling
Invalid quoted word part in YSH (OILS-ERR-17)
$ ysh -c "echo 'it''s'"       # the elvish spelling
Invalid quoted word part in YSH (OILS-ERR-17)
```

The answer is to change string type — `u'it\'s'` or `"it's"`. That is a
defensible call, and it lands on exactly the sed-one-liner ground the row above
says `'…'` exists to protect. So mesh and YSH are the two shells that made
`'…'` less raw than the pack, in opposite directions: mesh by decoding escapes
inside it and handing you `r'…'` when you want none, YSH by keeping it perfectly
raw and removing the ways out. Both cost something. mesh's cost is the sed
one-liner; YSH's is the apostrophe.

elvish is the odd one: it has **no interpolating string at all**. `"hi $n"`
prints `hi $n` literally, and you concatenate instead — `"hi "$n`, since
adjacent words fuse there as they do in mesh. That is a real cost of its
consistency, and the one place its quoting is less convenient than every other
shell here.

## Values and the pipeline

The deepest split among the seven is what a pipe carries, and there are three
answers rather than two.

bash, zsh, YSH, fish, and mesh all carry **bytes**, which is why every program
ever written for a Unix shell works in them. nushell carries **structured
data**,
which buys a genuinely better `ls | where size > 10mb` and costs you the
ecosystem: an external command's output arrives as text, and the way back to
structure is a parse (`| lines`, `| split column`, `| from json`). nushell's
answer is to reimplement the common tools as builtins that speak the format,
which works well until you need one it has not reimplemented.

**elvish takes the third answer: both, on separate channels.** A value channel
runs alongside stdin and stdout, so elvish commands hand each other real values
while bytes keep flowing to and from everything else. The seam is where a
pipeline crosses to an external command, and it is worth knowing which way you
are crossing:

| | What arrives |
| --- | --- |
| elvish → elvish | the value itself — `put [1 2] \| each {\|x\| put (kind-of $x)}` says `list` |
| elvish → external | **nothing** — `put [&a=1] \| cat` prints nothing at all |
| external → elvish | bytes, split into one value per line |

That last row is the pleasant one: `printf 'a\nb\n' | each {|x| put "["$x"]"}` gives
`[a]` and `[b]` without a parse step. The second is the cost — a value piped at a
program that cannot receive one is silently dropped rather than serialized.

mesh does not have the second channel, and that is the deliberate part: one
payload means one rule about what crosses a pipe, and nothing to lose when it
crosses.

mesh's position is that this is the one thing that rules out the nushell model
here. Values are real *inside* the shell — lists, maps, integers, booleans, with
modifiers that operate on them — and bytes on the wire:

```mesh
xs = [a b c]
puts $xs:len
config = [host: localhost, port: 8080]
puts $config.port
ps aux | grep ssh
```

Where bash flattens everything to a string the moment it leaves a variable, mesh
keeps the type until something asks for text. `$env.PATH` is a list, not a
colon-joined string, so the `IFS=:` juggling disappears.

### Spread

A list reaching argv is **explicit** in YSH, mesh, elvish, and nushell,
**implicit** in zsh and fish, and punctuation in bash:

```bash
a=(one two)
cmd "${a[@]}"
```

```sh
a=(one two)      # zsh
cmd $a
```

```fish
set a one two
cmd $a
```

```ysh
var a = ['one', 'two']
cmd @a
```

```mesh
a = [one two]
cmd ...$a
```

```elvish
var a = [one two]
cmd $@a
```

```nu
let a = [one two]
cmd ...$a
```

The implicit shells are the terse option, and the cost is that `$a` in argument
position is a different arity depending on what the variable holds — the same
data-dependence mesh is trying to remove, in a milder form. It is milder because
the arity tracks the *value's* structure rather than its bytes: `$a` is one word
per element and stays that way whatever the elements contain, which is already
far better than bash. mesh's `...`, elvish's `$@`, and YSH's `@` make it
readable from the line instead.

YSH is the strictest of the explicit four about the other half of that rule.
Writing `$a` for a list is not a quiet stringification but a run-time error
naming the type:

```text
$ ysh -c 'var a = ["one", "two"]; write -- $a'
fatal: Word eval got a List, which can't be stringified (OILS-ERR-203)
```

## Transforming a value

Every shell answers the same small question — *given a path, give me the stem* —
and it is where the seven are furthest apart.

bash has the pieces and no way to join them. `${p##*/}` is a basename and
`${f%.*}` strips the last extension, but what a `${…}` operates on has to be a
**variable name** — `${${p##*/}%.*}` is a `bad substitution`, because there is no
receiver slot an expansion can sit in. (Nesting into the *operand* is fine:
`${x:-${y%.*}}` works. It is the half that does not help.) So a two-step
transform is two statements and a variable you did not want:

```bash
file=${path##*/}
stem=${file%.*}
```

mesh writes the same two steps as two links of one chain, and the intermediate
never gets a name:

```mesh
stem = $path:base:stem
```

**That missing receiver slot is the whole origin of mesh's `:`.** What it grew into is
more general than paths: `subject:name` applies `name` to `subject`, the result
becomes the subject of whatever follows, and the vocabulary is open —
[`func _s:name()`](REFERENCE.md#declaring-a-modifier) adds to it — so a chain is
not limited to what the shell shipped.

### The seven, side by side

| | Postfix operator | Vocabulary | Where it works | Yours to extend |
| --- | --- | --- | --- | --- |
| bash | none — `${x@U}` is a suffix, one deep | fixed sigil forms, plus nine `@` letters (5.1) | inside `${…}` only | no |
| zsh | `:` + a **letter** | fixed: `h t r e s a A l u q Q x` … | `${…}`, history words, glob qualifiers | no |
| YSH | `=>` | any function | **expression mode only** | yes |
| fish | none | the `string` and `path` command families | pipelines and `(…)` | yes — write a function |
| **mesh** | **`:` + a word** | built-ins, plus yours | **anywhere a value is read**, argv included | yes — `func _s:name()` |
| elvish | none — `:` is the *module* separator (`path:base`) | module functions | prefix calls, nested or piped | yes |
| nushell | none | the `str` and `path` command families | pipelines | yes |

The same job — basename, then strip the last extension — in each:

```zsh
${p:t:r}                                      # zsh: two chained modifiers, one expansion
```
```fish
path basename $p | path change-extension ''   # fish: a pipeline of two builtins
```
```nu
$p | path parse | get stem                    # nushell: one parse to a record, then a field
```
```elvish
var b = (path:base $p); str:trim-suffix $b (path:ext $b)   # elvish: no stem function
```
```mesh
$p:base:stem                                  # mesh
```

### zsh is the ancestor, and it shows the ceiling

csh invented the colon modifier, zsh chained it and carried it onto parameter
expansion, and vim borrowed the same letters wholesale —
`fnamemodify(p, ':t:r:r')` and `expand('%:p:h')` are csh's `:t`, `:r` and `:h`
still in service decades later. mesh's [`Modifiers`](DESIGN.md#modifiers) section
says outright that this is the idea it is taking.

It is the closest prior art by some distance, and it stops at three walls:

1. **The letters are a closed set.** `:h :t :r :e :s :a :A :l :u :q :Q :x` and a
   handful more, and what you want next is not among them. There is no
   `${p:stem}` and no way to add one, so anything past the list is back to `sed`
   in a subshell — the exact complaint behind
   [fish #4002](https://github.com/fish-shell/fish-shell/issues/4002), which
   mesh's modifier system is the direct answer to.
2. **They are cryptic by construction.** One letter cannot carry a meaning, so
   `:r` (remove the extension) and `:e` (keep only the extension) are a pair you
   look up every time. mesh keeps the operator and spends the characters on the
   name: `:stem`, `:ext`, `:dir`, `:base`, `:bare`, `:ancestors`.
3. **They are not values.** A zsh modifier applies to an expansion, a history
   word, or a glob qualifier — it is part of the expansion grammar rather than
   an operator over values, so there is no `$(cmd):lines` and no
   `$xs:map(:base)`.

bash inherited only the history half (`!$:h`) and never brought the modifiers to
parameters at all; its answer is the `${x#…}` / `${x%…}` sigil family plus the
`${x@U}` transformations added in 4.4 and 5.1, and neither can take the other's
result — or its own — as a subject.

### YSH's `=>` is the nearest living relative

YSH is the only other shell here with a general postfix call. The fat arrow
chains free functions left to right, and the thin arrow calls mutating methods:

```ysh
var trimmed = line.trim() => upper()
call mylist->pop()
```

That is the same idea mesh's `:` is, arrived at independently, and the design
converges on the same reasons: functions read better applied left to right, and
a chain beats a temporary. Two differences are worth naming, and only one of
them favors mesh.

**`=>` lives in expression mode.** YSH's [two modes](#syntax) mean the chain is
available where you have already entered an expression — after `var`, or inside
`(…)` — and reaching it from command position costs a wrapper (`echo $[x =>
upper()]`). mesh has one mode, so a chain is an ordinary word:

```mesh
cp $f:base $dest/
if $f:ext == gz { … }
for d in $env.PATH:dedup { … }
```

Argument position is where a shell spends its day, and as far as this page's
survey found **no other Unix shell puts a postfix call chain there** — fish and
nushell need a pipeline or a `(…)`, elvish needs a nested call, YSH needs
expression mode. PowerShell manages it, because `$f.BaseName.ToUpper()` is an
ordinary .NET member access and PowerShell's argument mode accepts one; that is
the closest anything comes, and it is not a Unix shell.

**`=>` costs nothing in the word grammar, and `:` costs something.** Two
characters with whitespace around them can never be mistaken for text. mesh's
one character has to be taken away from bare words: `ubuntu:latest` is now a
modifier chain and needs quoting, which is one of
[the two extra quoting rules](#what-mesh-gives-up) mesh admits to. That trade was
made knowingly — the reservation is of the *shape* (`:` followed by a bare
identifier), so `key:2`, `key:/path`, `$host:$port` and `http://x` are all
untouched — but YSH's operator genuinely pays less for the same expressiveness,
and this page should say so rather than count the terseness as free.

### Outside the shell

The idea is old and widely re-derived; almost every piece of mesh's version has
precedent somewhere.

| Where | Spelling | What it contributes |
| --- | --- | --- |
| csh / tcsh / zsh / vim | `$p:h:t`, `%:p:h` | colon, postfix, chains — the direct ancestor |
| GNU make | `$(SRCS:.c=.o)` | a colon postfix transform on a variable; one rewrite rule, no chaining |
| cmd.exe | `%~dpn1` | letters again, and no way to chain |
| PostgreSQL | `x::text::int` | a colon postfix operator that *does* chain — for casts only |
| PowerShell | `$p.BaseName.ToUpper()` | real methods, and `.` auto-maps over a collection |
| Python | `Path(p).stem`, `.suffixes`, `.parents`, `.as_uri()` | where mesh's path vocabulary comes from, near name for name |
| D / Nim | `x.f(y)` ≡ `f(x, y)` | UFCS: *every* free function is already a postfix call |
| Raku | `.uc.trim`, `.&myfunc`, `x ==> f()` | three spellings of the idea in one language |
| Elixir / Gleam / R | `x \|> f() \|> g()` | the pipe as a language operator; the subject becomes the **first** argument (Gleam and R add a `_` placeholder to move it) |
| F# / OCaml | `x \|> f a` | the same operator, but partial application makes the subject the **last** argument |
| Clojure | `(-> x f g)`, `->>`, `some->` | threading, with separate operators for first-argument, last-argument, and nil-short-circuit |
| Hack | `x \|> f($$)` | a **topic token**, so the subject can land anywhere in the call |
| JS proposal | `x \|> f(%)` | the Hack form, Stage 2; the token itself (`%`, `^`, …) is still unsettled |
| Jinja (Nunjucks, Twig) | `{{ p \| basename \| default('vi') }}` | a word, parens only when there are arguments — mesh's exact call shape |
| Liquid, Go templates | `{{ p \| default: 'vi' }}`, `{{ .Path \| printf "%s" }}` | the same left-to-right filter chain; arguments after a `:` or a space, not in parens |
| jq | `.a \| ascii_upcase` | |
| Wolfram | `x // f // g` | postfix application as a plain binary operator |
| Factor / Forth | `x f g` | concatenative: postfix is the only order there is |

Three of those are worth more than a row.

**Python's `pathlib` is the vocabulary mesh copied**, and the correspondence is
close enough to be a check on the naming: `:stem`/`.stem`, `:base`/`.name`,
`:ext`/`.suffix`, `:exts`/`.suffixes`, `:dir`/`.parent`, `:real`/`.resolve()`,
`:url`/`.as_uri()`. The one deliberate divergence is
[`:ancestors`](DESIGN.md#modifiers) against `.parents`: pathlib's excludes the
path itself, mesh's includes it, and mesh rejected the name `.parents` precisely
because it reads as excluding the thing every upward search starts from.

**PowerShell's member enumeration is the cautionary tale, not the model.** Its
`.` maps over a collection automatically, which is the same convenience as mesh's
value modifiers — but it decides *per value at run time*: the collection's own
member wins, and enumeration is the fallback when the collection hasn't got one.
So `.Count` on a list whose elements each have a `Count` means one thing for a
`List[string]` and another for an array of custom objects, and a property that
doesn't exist enumerates into a list of `$null` rather than reporting. mesh
cannot land there, because the category is fixed by the **declaration** rather
than by the value: `func _s:name()` is element-wise and `func ..._xs:name()`
takes the whole collection, so `:len` is a collection modifier and `:stem` is a
value modifier no matter what either one is handed.

**Hack's topic token is the feature mesh does not have.** Every pipe operator has
to answer *where does the subject go*, and the answers do not agree: Elixir says
first, F# says last (by partial application, not by decree), Clojure ships `->`
and `->>` as two operators rather than choose, and Hack's `x |> f($$)` names a
slot so the subject can land anywhere. TC39 has spent years on which shape to
adopt; the Hack form is at Stage 2 with even the placeholder token unsettled.
mesh sidesteps the question by fixing the subject at the *declaration* — left of
the colon, where the call site puts it — the way a Kotlin extension function or a
Rust method does. That is the right default and it is not free: see the borrow
candidates below.

### What is actually new here

Every ingredient has prior art. The combination does not, as far as this survey
found:

- a **postfix call chain** — YSH, and nobody else among Unix shells;
- **in ordinary command-argument position** — mesh alone among them, PowerShell
  aside;
- spelled with a **word, not a letter** — mesh alone among the colon shells;
- **open to user declaration** — YSH, fish, elvish and nushell have this for
  their own shapes; the colon shells do not;
- **auto-mapping over a list, with the element-wise/whole-collection split fixed
  at declaration** — mesh alone; PowerShell has the mapping without the split;
- and **modifiers as values**, so `$xs:map(:base)` hands one chain link to
  another.

The last piece is the one that turns a modifier system into a function-call
syntax rather than a fixed operator table, and it is what makes "postfix call
chain" the honest description of `:` rather than a grander name for zsh's `:h`.

### Borrow candidates

Four things this survey turned up that mesh does not have, in rough order of how
much they would buy. None is decided; they are tracked in
[`TODO.md`](../TODO.md).

1. **A way to postfix-apply a function that was not declared as a modifier.**
   Raku's `.&f`, D and Nim's UFCS, and Hack's `$$` all let an existing function
   join a chain without being written for one. mesh requires `func _s:name()`,
   which is
   [deliberate](DESIGN.md#modifiers) — it keeps a private one-argument helper
   from silently becoming public vocabulary — but it means a chain stops dead at
   any function someone wrote before they knew it would be chained. `:map(:base)`
   already proves the machinery takes a chain link as a value, so an
   `:apply(f)` / `:then(f)` escape hatch is close by.
2. **`with_suffix`.** pathlib's `.with_suffix('.png')` is the one path operation
   mesh has no single spelling for; the design's answer is `($f:stem).png`, which
   is not implemented and reads worse than the rest of the family.
3. **`path parse` as an alternative to a vocabulary.** nushell gives you every
   path component at once as a record — one command to learn instead of ten
   modifiers. mesh's bet is that `$p:stem:upper` beats
   `($p | path parse).stem | str upcase` often enough to justify the vocabulary,
   which is a defensible call and worth writing down as one.
4. **A short-circuiting link**, Clojure's `some->` or Swift's `?.`. mesh's
   position is that absence is loud (see [`INTRO.md`](INTRO.md)) and
   `:get(k, default)` is the opt-in, so this is most likely a considered no —
   but the chain is exactly where a missing intermediate hurts most, and nothing
   currently records the decision.

## Globbing

| | bash | zsh | YSH | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Bare `*.txt` | expands | expands | expands | expands | expands | expands | expands |
| `$x` where `x` is `"*"` | **re-globs** | literal | literal | literal | literal | literal | literal |
| No match | left as text | **error** | no arguments | **error** | no arguments | **error** | left as text |
| Tolerate no match | — | `*.txt(N)` | default | — | default | `*[nomatch-ok]` | — |
| Glob a variable | (always) | `${~x}` | `glob(pat)` | — | `glob($x)` | **impossible** | `into glob` |

bash's "no match, so pass the pattern through as a filename" is a quiet
correctness bug — `grep foo *.log` in a directory with no logs searches a file
literally named `*.log`. zsh, fish, and elvish error, which is loud but stops
the command, and two of them let you say "empty is fine" per pattern: zsh with a
glob qualifier (`*.log(N)`, or `setopt nullglob` for all of them) and elvish
with `*[nomatch-ok]`. mesh drops the word, so a pattern that matches nothing
contributes nothing and no exemption is needed — the trade being that a typo'd
pattern is silently empty rather than loud.

**YSH lands on mesh's answer, and it is the one row where the two agree against
everyone else.** `shopt --set ysh:all` turns `nullglob` on, so a pattern that
matches nothing contributes zero words rather than erroring or passing itself
through — the same trade, arrived at independently, with `failglob` available
for anyone who wants the loud version instead.

The row that matters most is the second one, and bash is alone in getting it
wrong: a variable's contents are re-scanned for glob characters at use, so a
filename containing `[` breaks a script that never mentioned globbing. In the
other six a glob is a glob because of how the *source* is written.

The last row is where they diverge on how much rope to leave. zsh keeps a
sigil for it (`${~x}`), mesh a function (`glob($x)`), nushell a cast
(`into glob`) — and **elvish offers nothing at all**: there is no way to turn a
string's contents into a pattern, so the question cannot come up. That is the
strictest position on this page, stricter than mesh's, and the cost is that a
pattern genuinely computed at run time has nowhere to go. **YSH lands on mesh's
answer again**, down to the name: `glob(pat)` is a function returning a list, so
a computed pattern is an ordinary call rather than a sigil that re-enables an
expansion stage. That the two arrived independently at a plain function, while
zsh reaches for `${~x}` and elvish refuses the question, is the strongest
convergence on this page.

## Errors and strictness

```mesh
puts "a$undefined"
```

```text
mesh: undefined: unbound variable
```

bash, zsh, and fish all give you the empty string there — the failure mode
behind every `rm -rf "$prefix/"` horror story. The other four refuse, but not
at the same moment, and the difference matters more than the agreement:

| | When an unbound `$x` is caught |
| --- | --- |
| **mesh** | when execution reaches it — `puts BEFORE; puts $nosuch` prints `BEFORE` first |
| **YSH** | when execution reaches it — `fatal: Undefined variable 'nosuch'` |
| **elvish** | at compile time, before any of the script runs |
| **nushell** | at parse time — `^echo BEFORE; print $nosuch` prints nothing at all |

So mesh is the weakest of the four here, tied with YSH. A typo down a branch the
tests never take survives in both and does not in elvish or nushell, and in a
script that has already deleted something it surfaces halfway through. Closing
that gap is on [`TODO.md`](../TODO.md), where it turns out to need a language
decision rather than a pass: mesh binds names by executing statements, so any
check earlier than execution is guessing at what execution will bind. YSH has
the same constraint for the same reason, which is some evidence the run-time
answer is where a shell with shell-shaped scoping ends up rather than a corner
mesh painted itself into.

mesh goes further than YSH in one direction here and not the other. Both refuse
an unbound name at the moment of use; only YSH also **aborts the script** on a
failed command, which is the row above.

mesh goes further than the POSIX shells in two places:

- **`pipefail` is always on.** A pipeline's status is the **last** stage to
  fail, or `0` when none did, with no option to turn it off — and
  `$sh.pipestatus` breaks the same run down by stage as a real list, rather
  than bash's magic `PIPESTATUS` array. An upstream stage killed by `SIGPIPE`
  because a later stage stopped reading is forgiven, which is the case
  `set -o pipefail` gets wrong.

  **elvish goes further still, and unconditionally** — there is no option
  because elvish has no options. Any stage failing anywhere raises, and rather
  than electing one status it reports *every* failure at once:

  ```text
  $ elvish -c '/bin/sh -c "exit 3" | /bin/sh -c "exit 7"'
  Exception: (/bin/sh exited with 3 | /bin/sh exited with 7)
  ```

  Where mesh answers `7` and files `[3 7]` in `$sh.pipestatus`, elvish declines
  to pick and hands you both. Suppression is per site — `?(…)` around the
  pipeline yields a structured `pipeline-error` value holding both exceptions,
  which is more than a status list: each carries its command, exit status, and
  stack trace.

  **YSH is on always too, and combines it with the abort**, which is the
  strictest of the three readings in practice: `false | true` ends the script
  rather than reporting `1`, so a failure anywhere in a pipeline stops it by
  default. The per-stage breakdown is `_pipeline_status`, a real list as mesh's
  is, and `try { … }` is the per-site suppression that puts the status back in
  `_status` where you can read it.
- **There are no truthy values.** A condition is a bool, a status, or a command,
  and nothing else. `if $xs:len` is refused, naming `if $xs:len > 0` as the fix —
  where bash's `[ $x ]` and fish's `test` both quietly answer a question you did
  not ask. A status is admitted because success and failure are the whole of what
  it encodes, which is what a command in a condition was already being read for —
  not an exception to the rule but the same rule, since `$sh.status` *is* what
  the command left.

  This is the one strictness row where mesh is alone. YSH keeps Python's
  truthiness inside `(…)` — `if (0)` is false, `if (xs)` is true for a non-empty
  list — so the question mesh refuses to answer, YSH answers the way Python
  would. That is a defensible place to land and it is not mesh's: an empty list
  and a zero are different kinds of nothing, and mesh's position is that a
  condition should say which one it means.

## Syntax

mesh is a clean break from POSIX, like fish, elvish, and nushell — none of the
four runs your old `sh` scripts, and all four run your old *programs* fine.
YSH is the one that refuses the choice: the break is a per-file option
(`shopt --set ysh:all`) on an interpreter whose other mode is bash.

```mesh
if $sh.status { puts ok }
for f in *.md { puts $f }
func greet(name) { puts "hi $name" }
```

The shapes come from C-family languages rather than from `fi` / `esac` / `done`.
fish keeps `end`; nushell uses braces and closures; bash and zsh keep the Bourne
keywords and the `[[ … ]]` grammar bolted beside them — zsh adding a large
second layer of its own on top (`${(@f)x}` parameter flags, `**/` recursive
globs, glob qualifiers like `*(.om[1])`), which is powerful and is the other
reason it is not a small language. YSH takes the braces too, and adds the one
structural idea nothing else here has: **two modes in one grammar.** Command
mode is shell — bare words, `$x`, redirections. Expression mode, entered by
`(…)`, is Python — bare names, operators, real precedence.

That split is what makes a variable's spelling depend on where it appears:

```ysh
for i, item in (mylist) {     # expression mode: bare name
  echo "[$i] item $item"      # command mode: sigils
}
```

Both halves of that loop are consistent with their own mode, and YSH enforces
the boundary in both directions — `($mylist)` is an error whose message reads
`In expressions, remove $ and use `mylist`, or sometimes "$mylist"`. But
"sometimes" is doing real work in a diagnostic, and one `for` line straddling
both modes is the cost of the design: before you can spell a name you have to
know which mode you are in. mesh has one mode and one spelling, which is less
expressive inside an expression and has nothing to learn at the boundary,
because there is no boundary.

Two mesh choices that have no counterpart in the other six:

- **`~` is the match operator**, with `/…/` regex literals in a match slot only,
  so absolute paths need no wrapper — `$p ~ /usr/bin` is a path, `$p ~ /error/`
  is a regex, decided by word shape.
- **Modifiers chain**: `$m.rows[1].name:stem:upper` reads left to right, where
  bash has `${x%%…}` sigil soup and nushell reaches for a pipeline. YSH has the
  same operator as `=>` but only inside an expression — see
  [Transforming a value](#transforming-a-value).

## Implementation

Two properties of a shell's implementation that a user feels only indirectly:
what it is written in, and how many ways it can be told to behave differently.

| | bash 5.2 | zsh 5.9 | YSH 0.37 | fish 4.0 | mesh | elvish 0.21 | nushell 0.114 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Language | C | C | typed Python 2 → C++ | Rust | Rust | Go | Rust |
| Options | 27 `set -o` + 57 `shopt` | **185** `set -o` | 79 `shopt` | 7 feature flags | 5 | none | config record |
| …that change what a line *means* | many | many | **most of 79** | 7, transitional | **none** | none | none |

**The last row is the one that compounds.** Every option that changes the
meaning of a line multiplies the behavior a test suite has to cover, and the
multiplication is combinatorial rather than additive — `SH_WORD_SPLIT` interacts
with `GLOB_SUBST` interacts with `KSH_ARRAYS`, and each expansion path has to be
right under every combination. Nobody can enumerate that space, so how much of
it a suite reaches is bounded by the design and not by anyone's diligence.

mesh's five options are `bold-input`, `command-notify`, `cwd-report`,
`osc-title`, and `shell-integration` — every one of them terminal display. Not
one changes what a line of code means, and that is a property to hold onto
rather than a side effect of being early: an option that changes expansion is
exactly the thing the grammar rules out three sections up.

fish's seven are worth distinguishing from zsh's 185: each names the release
that introduced it and the migration it eases, and one is already frozen
(`stderr-nocaret`, "can no longer be changed"). That is a deprecation ratchet
working itself out, not a permanent configuration matrix.

**YSH's 79 are the interesting case, because there the multiplication is the
product.** Almost every one changes what a line means — that is what they are
for, and `shopt --set ysh:all` flips 60 of them at once to cross from bash to
YSH. Read against the row above it looks like the worst number on the table, and
read as a migration tool it is the best idea on it: the options are the ramp,
`ysh:upgrade` is the 28-option halfway house, and a team can move one file at a
time instead of rewriting a codebase. mesh has no such ramp and needs none,
having no bash to leave — which is easy to say when you have no users to
migrate. The honest read is that these are different problems, and YSH's is the
harder one.

**What this table does not tell you is how much simpler any of it is.** mesh is
about 40k lines of Rust against zsh's 143k of C and Oils' 63k of Python, and
that comparison flatters mesh, because mesh is not finished. Job control, signals, and terminal handling
are the same problem in any language and any era. Rust removes a class of memory
bug; it does not make shell semantics simpler, and a confusing design is just as
confusing in a safe language. The honest claim is narrower than "simpler": the
expansion core is smaller and far more testable, because it has no stages to
re-enter and no modes to cross-multiply — and that is where the long-lived bugs
in this family of software actually live.

## Interactive defaults

The rest of this page is about the language. This section is about the shell you
sit in, which for mesh is the point rather than a bonus — see
[`INTRO.md`](INTRO.md).

| | bash | zsh | YSH | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- | --- |
| History store | text file | text file | text file | text file | **SQLite** | BoltDB | text (SQLite opt-in) |
| Saved by default | yes | **no** | yes | yes | yes | yes | yes |
| Completion on by default | with a package | needs `compinit` | yes | yes | yes | yes | yes |
| Fuzzy matching | no | `zstyle` opt-in | no | subsequence | **default** | opt-in (`match-subseq`) | opt-in (`algorithm`) |
| Named hook events | none | 7 | none | 5 kinds | 7 | 3 | 5 |
| Registering a hook | reassign a var | `add-zsh-hook` (autoload) | bash's `trap` | `--on-…` flag | `on` / `$sh.<event>` | append to a list | `$env.config.hooks` |
| Backgrounds a *function* | yes | yes | `fork` (not `&`) | **no** (externals only) | yes | yes | yes (`job spawn`) |
| Reads a background job's status | `wait` | `wait` | `wait` | **no** | `wait` | **no** | **sent, not read** |
| Waiting with a deadline | no | no | no | no | **no** | no | `job recv --timeout` |

The comparison that flatters mesh is against a **bare** zsh, and it is stark.
With no configuration at all:

```sh
HISTFILE=[]  HISTSIZE=[30]  SAVEHIST=[0]     # history is not saved. at all.
sharehistory / extendedhistory / histignoredups   # every one off
add-zsh-hook                                 # command not found — needs autoload
completion                                   # needs compinit
```

Against that, mesh out of the box: history in **SQLite**
(`$XDG_STATE_HOME/mesh/history.sqlite3`, owner-readable, a multi-line command
stored and recalled as the one logical command it was typed as), **fuzzy
smart-case completion** with a columnar menu, command specs layered from a
curated file → the command's **man page** → a bounded **`--help` probe**, and
hooks reachable as both `on <event>` and the `$sh.<event>` maps.

But zsh is the outlier there, and the other five all arrive working. The rest of
this section is the fairer comparison.

YSH is the one whose interactive surface is deliberately *inherited* rather than
designed: readline, a text history file (`YSH_HISTFILE`), bash's `complete` /
`compgen` builtins, bash's `trap` and `PROMPT_COMMAND`. The one addition is
`renderPrompt(io)`, a real YSH function replacing `PS1` string-escape soup —
which is the same instinct as mesh's hooks, applied to one thing rather than
seven. That is consistent with the project: the interactive layer is not where
Oils is spending its novelty budget.

### Job control

The three rows on job control above are the ones that decide whether a config can
put a **time limit** on work it does not control — a prompt calling an
overridable hook that might block, say. That needs three things in sequence:
start the hook without blocking, get its exit status back, and give up after a
deadline. Not one of the three is common to all seven — fish lacks even the
first, for a function or a block.

**fish backgrounds externals and nothing else.** `command sleep 3 &` returns at
once; `slow &` for a function, and `begin; …; end &` for a block, both run to
completion first, and `$last_pid` is left holding whatever it held before:

```fish
function slow; command sleep 3; end
slow &                      # returns after 3s. $last_pid unchanged
command sleep 3 &           # returns immediately, $last_pid set
```

**elvish backgrounds anything and can tell you nothing about it.** A trailing `&`
starts a function or an external concurrently, but `wait`, `jobs`, `bg` and
`disown` are all unbound — `fg` exists and is not job control in the usual sense
— so there is no way to reach a background command's status, and nothing to wait
on. `run-parallel` and `peach` are the concurrency it does offer, and both block
until *everything* finishes, which is the opposite of a deadline.

**nushell is the only one with the deadline**, and it gets there by a different
route than `wait`. `job recv` is a **mailbox receive**, not a status read: it
returns a message some job chose to send, so a spawned job's exit status is not
readable at all and the answer has to be sent explicitly. That is a real
difference from `wait`, not a spelling — a job that fails or exits without
sending leaves nothing to receive, so the caller has to treat "no message" as
its own outcome rather than reading a status the shell kept for it. The tag
matters for the same reason: an untagged receive takes any message in the
mailbox in FIFO order, including one the caller never asked for.

What nushell does have, and nothing else here does, is `--timeout` on that
receive — so the deadline needs no polling and no signal handling:

```nu
let parent = (job id)
let j = (job spawn { (slow-predicate) | job send --tag 8737 $parent })
let answer = (try { job recv --tag 8737 --timeout 2sec } catch { null })
job kill $j     # null means it never answered: gave up, or died without sending
```

**YSH refuses `&` outright and gives you a builtin instead.** `slow &` is a
syntax error naming its replacement — `Use the 'fork' builtin instead of &` — so
`fork { slow }` starts the block and `wait $!` reads its status. That is the
same capability as bash's with the punctuation retired, and it is a good example
of what YSH is doing generally: keep the mechanism, replace the spelling that
nobody can parse at a glance. There is still no deadline.

**bash, zsh, YSH and mesh have the first two and not the third.** They background
a function and `wait` for its status, then have to build the deadline by hand out
of a second background job that sleeps and signals. mesh does that better than
bash in one respect that matters: `kill $j` signals the job's whole **process
group**, so a hook that wraps its blocking command in more shell is still
reached. bash cannot copy that — with monitor mode off the job shares the
shell's own process group, so the group signal takes the shell down with it, and
`kill $!` reaches only the job itself while a grandchild runs on.

Against that, mesh has no way to background quietly: `[1] 1234` on stderr at the
start and `[1] Done` at the next prompt are right for interactive work and wrong
for a job the prompt starts on every render, and `$sh.options` has no monitor-mode
equivalent. bash's `set +m` covers exactly that. Both gaps — the bounded wait and
the quiet background — are on [`TODO.md`](../TODO.md) under *Bounding a wait*.

### Hooks

bash is the one with no hook *system* at all — and YSH inherits exactly that,
so read this section as covering both: you reassign `PROMPT_COMMAND`,
set `PS0`, or `trap` on `DEBUG` / `ERR` / `EXIT`. Since 5.1 `PROMPT_COMMAND`
may be an **array**, and bash runs each set element in turn —

```bash
PROMPT_COMMAND=("echo one" "echo two")   # both run, every prompt
```

— so two prompt integrations no longer have to collide or chain strings by
hand, which they did for decades before it. What bash still has no equivalent
of is a *named* registry: the entries are positional, nothing identifies whose
is whose, and a `trap` is still single-valued, so two things wanting `EXIT`
still overwrite each other.

The other five have named events, and what differs is how you attach to one.
YSH sits in the table below carrying bash's traps rather than events of its own
— it is there to be compared against, not because it joined them:

| Shell | Events | How you attach |
| --- | --- | --- |
| **zsh** | `precmd`, `preexec`, `chpwd`, `periodic`, `zshaddhistory`, `zshexit`, `zsh_directory_name` | define the function, or `add-zsh-hook` for more than one per event — after `autoload -Uz add-zsh-hook` |
| **YSH** | bash's `DEBUG`, `ERR`, `EXIT`, `RETURN`, plus `renderPrompt()` | `trap`, or define the `renderPrompt` func |
| **fish** | by *kind* rather than by name — `--on-event`, `--on-variable`, `--on-signal`, `--on-job-exit`, `--on-process-exit` | a flag on the function definition |
| **elvish** | `before-readline`, `after-readline`, `after-command` | append a function to the `$edit:…` list |
| **nushell** | `pre_prompt`, `pre_execution`, `env_change`, `display_output`, `command_not_found` | assign into `$env.config.hooks` |
| **mesh** | `preprompt`, `preexec`, `postexec`, `precd`, `postcd`, `jobdone`, `exit` | `on <event> <name> <func>`, or `$sh.<event>.<name> = <func>` |

fish's is the most general — an event *kind* system, so `--on-variable PATH` or
`--on-process-exit` cover cases the others have no name for. elvish's three are
the narrowest, and are about the line editor rather than the shell. YSH's is
bash's, with the one upgrade that matters most in practice: `renderPrompt(io)`
is a function returning a string, so a prompt is written in the language instead
of in `PS1` escape codes.

mesh's distinguishing bit is small but real: a handler is **named**, so
`on postcd fetch …` can be replaced or removed by that name later, and the same
registry is readable and writable as a map. zsh's is a list you append to and
must remove by function name; fish's is attached to the function definition
itself, so replacing it means redefining the function. Which matters exactly
when a config is assembled from pieces that do not know about each other, and
not at all otherwise.

Both of mesh's less usual events have counterparts; what differs is the payload
and how you register. `postexec` lines up with elvish's `after-command`, and
mesh's carries `command, status, elapsed` — the elapsed milliseconds being the
part a prompt otherwise reconstructs with a `date` call on either side.
`jobdone` lines up with fish's `--on-job-exit`, but fish's is registered
**against a specific PID**, so you attach it to a job you already have in hand;
mesh's fires for any background job the shell reaps, with its `id`, `command`,
and `status`. Neither is a new idea, and the useful difference is that mesh's
fire for work you did not arrange in advance.

### Completion

The gap here runs the other way, and it is worth being blunt about: **zsh has
the best completions of any shell on this page**, by a distance that a decade of
curated `_command` functions bought and nothing else can shortcut. fish is
second and gets there partly by generation. mesh is not competing on coverage.

| Shell | Where completions come from |
| --- | --- |
| **bash** | `complete` / `compgen`, plus the `bash-completion` package if installed |
| **YSH** | bash's `complete` / `compgen` reimplemented, so most `bash-completion` scripts work, plus `compadjust` |
| **zsh** | a large curated `_command` set, `compinit`, `_gnu_generic` for `--help`-style commands, `zstyle` for matcher and menu behavior |
| **fish** | shipped completions, plus `fish_update_completions` generating from man pages |
| **elvish** | `$edit:completion:arg-completer` — a map from command name to a function you write |
| **nushell** | `extern` signatures with types, plus custom completers |
| **mesh** | four layers tried in order: curated file → man page → bounded `--help` probe → files |

The shapes differ more than the table shows. elvish and nushell put the
mechanism in your hands and ship little: nushell's `extern` is elegant — you
declare a command's signature with types and get completion from it — but
someone has to write the declaration. zsh and fish ship the answers. mesh tries
to derive them, which is the cheapest thing to be right about *by default* and
the most likely to be imprecise on any given command. YSH takes the fourth
route and **inherits an ecosystem**: it reimplements bash's `complete` API, so
the existing `bash-completion` corpus largely runs on it as-is — "mostly
compatible" is Oils' own wording, not a guarantee. That is still the best
coverage-per-unit-effort on this table, and it is available only to a shell that
kept bash compatibility, which is precisely the thing mesh, fish, elvish, and
nushell each gave up.

So the honest statement is that mesh's completion is better than nothing without
work, and worse than zsh's with it.

The one row mesh does lead on is **fuzzy matching by default**, and it is a
narrow lead: fish matches subsequences out of the box, while zsh, elvish, and
nushell all ship prefix matching and put the better matcher behind a setting
(`zstyle … matcher-list`, `$edit:completion:matcher`, and
`$env.config.completions.algorithm` respectively). Being right by default rather
than after configuration is the same argument this page makes about expansion,
pointed at a much smaller target.

Neither derived layer is a new idea, and it would be wrong to claim otherwise:
fish ships `fish_update_completions`, which runs a Python script over your
manpath and writes generated completion files, and zsh ships `_gnu_generic`,
which parses a command's `--help` output. What mesh does differently is *when*
and *how much*: those are a batch step you remember to re-run and a per-command
`compdef` opt-in respectively, where mesh consults the four sources in order at
the moment you press Tab, for any command, with nothing generated ahead of time
and no Python in the picture. The layering is the feature, not either source.

**But nobody runs a bare zsh, and that is the honest counter-case.**
Distributions ship a starter `zshrc`, and the setup people actually have is zsh
plus a framework — oh-my-zsh, prezto, zinit — plus `fzf`, plus
`zsh-autosuggestions`. That combination is *excellent*, and it beats mesh
comfortably on completion coverage, on breadth of plugins, and on every question
that a decade of accumulated community effort answers.

So the claim here is not "better than a configured zsh". It is that the
configuration step is where people stall, that a shell arriving good is worth
something on its own, and that when the defaults are the product they have to be
defended rather than deferred to a plugin.

## What mesh gives up

Honest costs, in the order they will bite:

1. **zsh already gets you most of the way**, and it is the honest objection
   rather than a footnote. Unquoted `$x` is safe there too, arrays are real, you
   keep POSIX *syntax* and thirty years of ecosystem, and old scripts still run
   under `sh` invocation or `emulate sh`. (Native zsh mode is itself a departure
   from POSIX expansion — that is exactly what the safe default *is* — so the
   compatibility lives in the emulation, not alongside the default.) What is
   left is
   the last mile — `$(…)` splitting, empty elision, and safety-by-default rather
   than safety-by-grammar — plus the parts of mesh that are not about safety at
   all (typed values, modifiers, `match`). Whether that last mile is worth a new
   shell is a real question, and for many people the answer is no.
2. **elvish already ships the value model**, has done for years, and is
   *stricter* than mesh in the two places noted under the first table — an
   unbound variable caught at compile time, and a failed command that aborts.
   nushell is stricter on both counts too, catching the unbound variable while
   parsing and ending the script on a failed command. If those are what drew you
   here, two shells have them now. mesh's answer is interpolation, modifiers,
   and the interactive defaults, not the core idea.
3. **YSH gets most of the same safety without asking you to leave.** No word
   splitting, no re-globbing, real lists, an unbound variable caught at the same
   moment mesh catches it, plus an abort mesh does not have — and the same
   binary still runs your existing scripts as `osh`, keeps `bash-completion`
   working, and lets you convert one file at a time. Against a shell offering
   that migration path, "you must rewrite everything" is a steep ask, and it is
   the objection mesh has the least to say against. What is left is the language
   itself — one mode rather than two, modifiers, `match`, `'…'` you can put an
   apostrophe in — and whether that is worth a rewrite is a fair question with a
   defensible "no."
4. **Maturity.** bash is thirty-five years old, zsh, fish, elvish, and nushell
   all have real ecosystems and package managers, and mesh's language design is
   still in draft. See [`ROADMAP.md`](../ROADMAP.md).
5. **No POSIX compatibility.** Same as fish, elvish, and nushell, and unlike
   zsh or YSH's `osh` half — your `.bashrc` does not port, and neither does any
   script you wrote.
6. **No structured pipeline.** `ls | where size > 10mb` is nushell's, and mesh
   does not offer it. Bytes on the wire is a deliberate ceiling.
7. **The two extra quoting rules** above: `word:identifier` and `'…'` escapes.
8. **Portability.** bash is on every machine you ssh into. mesh is a shell you
   install, and — as with fish and nushell — the remote end still has `sh`.

## Others in the family

Beyond the six above, the same design space is worked by:

| Shell | The idea | Escaping stance |
| --- | --- | --- |
| **PowerShell** | Objects on the pipe, .NET underneath | Backtick escapes; `'…'` is raw; globs are expanded by *cmdlets*, never for external programs, and native-argument passing needed a rewrite (`PSNativeCommandArgumentPassing`) to stop double-escaping |
| **rc** (Plan 9) | The original clean break: real lists, no splitting | `'…'` only, doubled to escape itself; no backslash at all |

mesh's closest relatives are elvish and rc on the value model, YSH on which
problem it thinks is worth solving, and fish on the day-to-day feel. The
distinguishing bet is that a shell can have a real type system *inside* while
staying an ordinary Unix shell *outside* — no adapters, no reimplemented
coreutils, no structured pipeline to convert into and out of.

## Choosing

- Use **bash** when the target machine is not yours, or the script has to run
  anywhere.
- Use **zsh** when you want most of the safety while keeping POSIX syntax, the
  ecosystem, and an emulation mode that still runs old scripts — the pragmatic
  choice, and the one with the least to argue against it. If the case for a
  clean break does not land for you, this is the shell that makes it.
- Use **YSH** when you have bash scripts you cannot abandon but want to stop
  writing new ones. It is the only shell here that lets you keep the old code
  running as `osh` and convert it a file at a time, and the safety you get at
  the end of that path is close to mesh's.
- Use **fish** for a safer interactive shell with a mature ecosystem, if you are
  happy writing scripts in a second language.
- Use **elvish** if you want mesh's value model today, from a shell that has
  been shipping for years, and you can live without string interpolation.
- Use **nushell** when your work is mostly data wrangling and its builtins cover
  your tools.
- **mesh** is for someone who wants fish's safety, real values in the language,
  and coreutils to stay first-class — and who is willing to be early.

## TODO for this page

Known gaps, kept here rather than in `TODO.md` because they are about the page
rather than about the shell.

- [ ] **YSH was measured on the Python dev build, not a release binary.** Oils
      0.37.0, built from the git repository with `build/py.sh minimal`, which
      runs the interpreter under Python rather than the translated C++ that a
      release tarball ships. The semantics are the same code, so the cells hold;
      what the dev build does not exercise is the shipped binary's own
      behavior — startup, `--version` provenance, anything where translation
      could differ. Re-check against a release binary when one is reachable.

- [ ] **Decide whether the zsh case wants consolidating.** The argument for
      preferring mesh over zsh is currently spread across three places — the
      residuals under "zsh: the same machinery, better defaults", the tuning
      subsection, and item 1 of "What mesh gives up". It reads fine in flow,
      but a reader looking for "why not just use zsh" has to assemble it. The
      deliberate omission is anything about zsh's maintenance — open bug
      reports, test coverage, how patches are received — which is real, is part
      of why mesh exists, and is not currently claimed anywhere on the page.
      Whether to say it, and how neutrally, is a judgment call rather than a
      fact to check.

- [ ] **Reconsider the length.** The page is comfortably the longest in `docs/`.
      "Implementation" is the most cuttable section if it starts to read as
      padding — it supports the testability argument but is not load-bearing
      for the quoting material that motivated the page.

- [ ] **elvish's interactive surface is documented from source, not a session.**
      Its hooks and completion API were read out of the module source, since a
      non-interactive `elvish -c` does not load `$edit:`. Worth confirming
      against a live session before anyone leans on those rows.
