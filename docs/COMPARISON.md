# mesh compared with bash, zsh, fish, elvish, and nushell

Five shells worth measuring mesh against, because each one answers the same
question differently.

- **bash** is the baseline — what nearly everyone arrives from, and the source
  of the specific problems mesh exists to fix.
- **zsh** is bash's machinery with better defaults. It is the interesting one:
  it fixed the biggest footgun while keeping POSIX syntax and an emulation mode
  that still runs old scripts — so the fix cost far less than a new language.
  The safe default is itself a departure from POSIX expansion; the point is how
  small a departure bought how much.
- **fish** is the friendly shell that broke cleanly and kept the Unix shape:
  byte pipes, external commands everywhere, no word splitting.
- **elvish** is the closest relative by design — real values, byte pipes, and a
  strictness that in two places exceeds mesh's own. It is the one to read if you
  want to know whether mesh's ideas needed a new shell to hold them.
- **nushell** is the structured-data shell: the pipeline carries tables and
  records rather than bytes, and much of coreutils is replaced by builtins that
  speak that format.

mesh sits between fish and elvish, which is the order the tables below are
written in — roughly from most POSIX to least. It takes fish's answer to
expansion safety and pushes it further into a real type system — lists, maps,
integers, booleans — while keeping nushell's ambition out of the pipe: **pipes
carry bytes**, so `grep`, `jq`, `ffmpeg`, and everything else you already run
works unchanged. See [`DESIGN.md`](DESIGN.md) for why each call went the way it
did, and [`REFERENCE.md`](REFERENCE.md) for what is actually implemented today —
this page compares designs, and mesh's is still being built.

## At a glance

| | bash | zsh | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- |
| Space in unquoted `$x` | **splits** | one arg | one arg | one arg | one arg | one value |
| `*` in unquoted `$x` | **re-globs** | literal | literal | literal | literal | literal |
| Unquoted command substitution | **splits on `IFS`** | **splits on `IFS`** | splits on newlines | one string | splits on newlines | one value |
| Unquoted empty `$x` | **vanishes** | **vanishes** | one empty arg | one empty arg | one empty arg | one empty value |
| Lists | bolted-on | native | native | native | native | native |
| List → argv | `"${a[@]}"` | implicit | implicit | `...$a` | `$@a` | `...$a` |
| Pipe payload | bytes | bytes | bytes | bytes | bytes + values | **structured** |
| Coreutils | first-class | first-class | first-class | first-class | first-class | second-class |
| Unset variable | `""` | `""` | `""` | error (at run time) | **compile error** | **parse error** |
| A failed command | status | status | status | status | **aborts** | **aborts** |
| `pipefail` | opt-in | opt-in | `$pipestatus` | **always on** | aborts, reports all | any stage aborts |
| Truthiness | status + tests | status + tests | status | **none** | value | typed |
| `'…'` | raw | raw | nearly raw | **escapes** | raw | raw |
| `"…"` interpolates | yes | yes | yes | yes | **no** | **no** (`$"…"`) |
| `"\n"` is a newline | no (`$'\n'`) | no (`$'\n'`) | no | yes | yes | yes |
| Runs POSIX scripts | yes | mostly | no | no | no | no |
| Config language | bash | zsh | fish | mesh | elvish | nu |

Command substitution is spelled `$(…)` in bash, zsh, mesh, and fish; elvish and
nushell write a subexpression as `(…)`, and `$(…)` in nushell is a parse error.

**Two rows go against mesh, and they are the two worth reading first.** On the
unset-variable row mesh is the *weakest* of the three shells that refuse at all:
elvish catches it when it compiles the code and nushell catches it while
parsing, both before any of the program runs, where mesh only reports it once
execution reaches the statement — so a typo down a branch your tests never take
survives in mesh and does not in either of them. And a command that fails aborts
the script by default in *both* of them, where mesh leaves it in `$sh.status`:

```
$ elvish -c 'false; echo AFTER'        # AFTER never prints
$ nu -c '^false; print AFTER'          # AFTER never prints, nu exits 1
```

nushell is the less obvious of the two — a failing external raises a catchable
error rather than an exception, so `try`/`catch` and `complete` recover the
status, but left alone it ends the script exactly as elvish does, and it does so
for a failure in *any* pipeline stage, not only the last. Both are stricter
readings of the same instinct this page argues for, and the first is now on
[`TODO.md`](../TODO.md).

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
  stays load-bearing to pass an empty string. fish, mesh, elvish, and nushell
  all print `[a][][b]`.

  The distinction the other three can make and bash and zsh cannot is between a
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
thing. bash, zsh, fish, elvish, and nushell all take those bare.

**`'…'` takes escapes, unlike every other shell.** In bash, zsh, fish, elvish,
and nushell a single-quoted string is (near enough) raw, which is why every sed
one-liner in existence is written in one. mesh follows Python instead: `'…'` is
`"…"` minus interpolation, with the same `\n \t \e \u{…}` set, and an unknown
escape is an error rather than a literal backslash. So this is a syntax error in
mesh and fine in the other five:

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

| | bash | zsh | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- |
| Interpolating + escapes | — | — | — | `"…"` | — | `$"…"` |
| Interpolating only | `"…"` | `"…"` | `"…"` (few escapes) | — | — | — |
| Escapes only | `$'…'` | `$'…'` | — | `'…'` | `"…"` | — |
| Fully raw | `'…'` | `'…'` | `'…'` (bar `\'`, `\\`) | `r'…'` / `r"…"` | `'…'` | `'…'`, `r#'…'#` |
| Both quote kinds, no escaping | heredoc | heredoc | heredoc-ish | `<< 'END'` heredoc | — | `r#'…'#` |

The top row is the surprising one: **only mesh and nushell can spell
interpolate-and-escape at all.** bash and zsh have the two halves in separate
forms — `"…"` interpolates but leaves `\n` as backslash-n, and `$'…'` reads the
escape but not the variable (`x=foo; echo $'$x'` prints `$x`) — so wanting both
in one string means concatenating two of them. That is the gap `$"…"` fills in
nushell, and the reason mesh's `'…'` and `"…"` differ on exactly one axis.

elvish is the odd one: it has **no interpolating string at all**. `"hi $n"`
prints `hi $n` literally, and you concatenate instead — `"hi "$n`, since
adjacent words fuse there as they do in mesh. That is a real cost of its
consistency, and the one place its quoting is less convenient than every other
shell here.

## Values and the pipeline

The deepest split among the six is what a pipe carries, and there are three
answers rather than two.

bash, zsh, fish, and mesh all carry **bytes**, which is why every program ever
written for a Unix shell works in them. nushell carries **structured data**,
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

A list reaching argv is **explicit** in mesh, elvish, and nushell, **implicit**
in zsh and fish, and punctuation in bash:

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
far better than bash. mesh's `...` and elvish's `$@` make it readable from the
line instead.

## Globbing

| | bash | zsh | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- |
| Bare `*.txt` | expands | expands | expands | expands | expands | expands |
| `$x` where `x` is `"*"` | **re-globs** | literal | literal | literal | literal | literal |
| No match | left as text | **error** | **error** | no arguments | **error** | left as text |
| Tolerate no match | — | `*.txt(N)` | — | default | `*[nomatch-ok]` | — |
| Glob a variable | (always) | `${~x}` | — | `glob($x)` | **impossible** | `into glob` |

bash's "no match, so pass the pattern through as a filename" is a quiet
correctness bug — `grep foo *.log` in a directory with no logs searches a file
literally named `*.log`. zsh, fish, and elvish error, which is loud but stops
the command, and two of them let you say "empty is fine" per pattern: zsh with a
glob qualifier (`*.log(N)`, or `setopt nullglob` for all of them) and elvish
with `*[nomatch-ok]`. mesh drops the word, so a pattern that matches nothing
contributes nothing and no exemption is needed — the trade being that a typo'd
pattern is silently empty rather than loud.

The row that matters most is the second one, and bash is alone in getting it
wrong: a variable's contents are re-scanned for glob characters at use, so a
filename containing `[` breaks a script that never mentioned globbing. In the
other five a glob is a glob because of how the *source* is written.

The last row is where they diverge on how much rope to leave. zsh keeps a
sigil for it (`${~x}`), mesh a function (`glob($x)`), nushell a cast
(`into glob`) — and **elvish offers nothing at all**: there is no way to turn a
string's contents into a pattern, so the question cannot come up. That is the
strictest position on this page, stricter than mesh's, and the cost is that a
pattern genuinely computed at run time has nowhere to go.

## Errors and strictness

```mesh
puts "a$undefined"
```

```text
mesh: undefined: unbound variable
```

bash, zsh, and fish all give you the empty string there — the failure mode
behind every `rm -rf "$prefix/"` horror story. The other three refuse, but not
at the same moment, and the difference matters more than the agreement:

| | When an unbound `$x` is caught |
| --- | --- |
| **mesh** | when execution reaches it — `puts BEFORE; puts $nosuch` prints `BEFORE` first |
| **elvish** | at compile time, before any of the script runs |
| **nushell** | at parse time — `^echo BEFORE; print $nosuch` prints nothing at all |

So mesh is the *weakest* of the three here. A typo down a branch the tests never
take survives in mesh and does not in elvish or nushell, and in a script that
has already deleted something it surfaces halfway through. Closing that gap is
on [`TODO.md`](../TODO.md), where it turns out to need a language decision
rather than a pass: mesh binds names by executing statements, so any check
earlier than execution is guessing at what execution will bind.

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
- **There are no truthy values.** A condition is a bool or a command, and
  nothing else. `if $xs:len` is refused, naming `if $xs:len > 0` as the fix —
  where bash's `[ $x ]` and fish's `test` both quietly answer a question you did
  not ask.

## Syntax

mesh is a clean break from POSIX, like fish, elvish, and nushell — none of the
four runs your old `sh` scripts, and all four run your old *programs* fine.

```mesh
if $sh.status == 0 { puts ok }
for f in *.md { puts $f }
func greet(name) { puts "hi $name" }
```

The shapes come from C-family languages rather than from `fi` / `esac` / `done`.
fish keeps `end`; nushell uses braces and closures; bash and zsh keep the Bourne
keywords and the `[[ … ]]` grammar bolted beside them — zsh adding a large
second layer of its own on top (`${(@f)x}` parameter flags, `**/` recursive
globs, glob qualifiers like `*(.om[1])`), which is powerful and is the other
reason it is not a small language.

Two mesh choices that have no counterpart in the other five:

- **`~` is the match operator**, with `/…/` regex literals in a match slot only,
  so absolute paths need no wrapper — `$p ~ /usr/bin` is a path, `$p ~ /error/`
  is a regex, decided by word shape.
- **Modifiers chain**: `$m.rows[1].name:stem:upper` reads left to right, where
  bash has `${x%%…}` sigil soup and nushell reaches for a pipeline.

## Implementation

Two properties of a shell's implementation that a user feels only indirectly:
what it is written in, and how many ways it can be told to behave differently.

| | bash 5.2 | zsh 5.9 | fish 4.0 | mesh | elvish 0.21 | nushell 0.114 |
| --- | --- | --- | --- | --- | --- | --- |
| Language | C | C | Rust | Rust | Go | Rust |
| Options | 27 `set -o` + 57 `shopt` | **185** `set -o` | 7 feature flags | 5 | none | config record |
| …that change what a line *means* | many | many | 7, transitional | **none** | none | none |

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

**What this table does not tell you is how much simpler any of it is.** mesh is
about 40k lines of Rust against zsh's 143k of C, and that comparison flatters
mesh, because mesh is not finished. Job control, signals, and terminal handling
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

| | bash | zsh | fish | mesh | elvish | nushell |
| --- | --- | --- | --- | --- | --- | --- |
| History store | text file | text file | text file | **SQLite** | BoltDB | text (SQLite opt-in) |
| Saved by default | yes | **no** | yes | yes | yes | yes |
| Completion on by default | with a package | needs `compinit` | yes | yes | yes | yes |
| Fuzzy matching | no | `zstyle` opt-in | subsequence | **default** | opt-in (`match-subseq`) | opt-in (`algorithm`) |
| Named hook events | none | 7 | 5 kinds | 7 | 3 | 5 |
| Registering a hook | reassign a var | `add-zsh-hook` (autoload) | `--on-…` flag | `on` / `$sh.<event>` | append to a list | `$env.config.hooks` |
| Backgrounds a *function* | yes | yes | **no** (externals only) | yes | yes | yes (`job spawn`) |
| Reads a background job's status | `wait` | `wait` | **no** | `wait` | **no** | **sent, not read** |
| Waiting with a deadline | no | no | no | **no** | no | `job recv --timeout` |

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

But zsh is the outlier there, and the other four all arrive working. The rest of
this section is the fairer comparison.

### Job control

The three rows on job control above are the ones that decide whether a config can
put a **time limit** on work it does not control — a prompt calling an
overridable hook that might block, say. That needs three things in sequence:
start the hook without blocking, get its exit status back, and give up after a
deadline. Not one of the three is common to all six — fish lacks even the
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

**bash, zsh and mesh have the first two and not the third.** They background a
function and `wait` for its status, then have to build the deadline by hand out
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

bash is the one with no hook *system* at all: you reassign `PROMPT_COMMAND`,
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

Everyone else has named events. What differs is how you attach to one:

| Shell | Events | How you attach |
| --- | --- | --- |
| **zsh** | `precmd`, `preexec`, `chpwd`, `periodic`, `zshaddhistory`, `zshexit`, `zsh_directory_name` | define the function, or `add-zsh-hook` for more than one per event — after `autoload -Uz add-zsh-hook` |
| **fish** | by *kind* rather than by name — `--on-event`, `--on-variable`, `--on-signal`, `--on-job-exit`, `--on-process-exit` | a flag on the function definition |
| **elvish** | `before-readline`, `after-readline`, `after-command` | append a function to the `$edit:…` list |
| **nushell** | `pre_prompt`, `pre_execution`, `env_change`, `display_output`, `command_not_found` | assign into `$env.config.hooks` |
| **mesh** | `preprompt`, `preexec`, `postexec`, `precd`, `postcd`, `jobdone`, `exit` | `on <event> <name> <func>`, or `$sh.<event>.<name> = <func>` |

fish's is the most general — an event *kind* system, so `--on-variable PATH` or
`--on-process-exit` cover cases the others have no name for. elvish's three are
the narrowest, and are about the line editor rather than the shell.

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
the most likely to be imprecise on any given command.

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
3. **Maturity.** bash is thirty-five years old, zsh, fish, elvish, and nushell
   all have real ecosystems and package managers, and mesh's language design is
   still in draft. See [`ROADMAP.md`](../ROADMAP.md).
4. **No POSIX compatibility.** Same as fish, elvish, and nushell, and unlike
   zsh — your `.bashrc` does not port, and neither does any script you wrote.
5. **No structured pipeline.** `ls | where size > 10mb` is nushell's, and mesh
   does not offer it. Bytes on the wire is a deliberate ceiling.
6. **The two extra quoting rules** above: `word:identifier` and `'…'` escapes.
7. **Portability.** bash is on every machine you ssh into. mesh is a shell you
   install, and — as with fish and nushell — the remote end still has `sh`.

## Others in the family

Beyond the five above, the same design space is worked by:

| Shell | The idea | Escaping stance |
| --- | --- | --- |
| **PowerShell** | Objects on the pipe, .NET underneath | Backtick escapes; `'…'` is raw; globs are expanded by *cmdlets*, never for external programs, and native-argument passing needed a rewrite (`PSNativeCommandArgumentPassing`) to stop double-escaping |
| **rc** (Plan 9) | The original clean break: real lists, no splitting | `'…'` only, doubled to escape itself; no backslash at all |

**YSH** (Oils) belongs in the table above rather than this list — it is the
other project attacking this problem from inside POSIX — and it is missing for
a boring reason: everything on this page was measured against a shell built and
run here, and YSH was the one that could not be. It is on
[`TODO.md`](../TODO.md) to add once it can be, rather than described from
recollection.

mesh's closest relatives are elvish and rc on the value model, and fish on the
day-to-day feel. The distinguishing bet is that a shell can have a real type
system *inside* while staying an ordinary Unix shell *outside* — no adapters, no
reimplemented coreutils, no structured pipeline to convert into and out of.

## Choosing

- Use **bash** when the target machine is not yours, or the script has to run
  anywhere.
- Use **zsh** when you want most of the safety while keeping POSIX syntax, the
  ecosystem, and an emulation mode that still runs old scripts — the pragmatic
  choice, and the one with the least to argue against it. If the case for a
  clean break does not land for you, this is the shell that makes it.
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

- [ ] **Add YSH (Oils) to the tables.** It belongs here on merit — the other
      project attacking this problem from inside POSIX — and is missing for a
      boring reason: every cell on this page was measured against a shell built
      and run locally, and YSH was the one that could not be. Neither
      `oils.pub` nor `oilshell.org` was reachable from the sandbox this was
      written in, and building from the repository needs the MyPy-translation
      toolchain rather than a release tarball. zsh, fish, nushell, and elvish
      were all built and run; YSH should get the same treatment rather than a
      column of recollections. Worth checking when it lands: whether
      `shopt --set ysh:all` ends word splitting as documented, how `r'…'` /
      `u'…'` / `b'…'` line up against mesh's `r'…'`, whether unset variables
      are an error, and what the pipeline carries.

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
