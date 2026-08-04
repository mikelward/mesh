# mesh compared with bash, fish, and nushell

Three shells worth measuring mesh against, because each one answers the same
question differently.

- **bash** is the baseline — what nearly everyone arrives from, and the source
  of the specific problems mesh exists to fix.
- **fish** is the friendly shell that kept the Unix shape: byte pipes, external
  commands everywhere, no word splitting. It is the closest relative.
- **nushell** is the structured-data shell: the pipeline carries tables and
  records rather than bytes, and much of coreutils is replaced by builtins that
  speak that format.

mesh sits between fish and nushell, which is the order the tables below are
written in. It takes fish's answer to expansion safety and pushes it further
into a real type system — lists, maps, integers, booleans — while keeping
nushell's ambition out of the pipe: **pipes carry bytes**, so `grep`, `jq`,
`ffmpeg`, and everything else you already run works unchanged. See
[`DESIGN.md`](DESIGN.md) for why each call went the way it did, and
[`REFERENCE.md`](REFERENCE.md) for what is actually implemented today — this page
compares designs, and mesh's is still being built.

## At a glance

| | bash | fish | mesh | nushell |
| --- | --- | --- | --- | --- |
| Unquoted `$x` with a space in it | **splits** | one argument | one argument | one value |
| Unquoted `$x` holding `*` | **re-globs** | literal | literal | literal |
| Lists | bolted-on arrays | native | native | native |
| List → argv | `"${a[@]}"` | implicit | `...$a` | `...$a` |
| Pipe payload | bytes | bytes | bytes | **structured** |
| Coreutils | first-class | first-class | first-class | second-class |
| Unset variable | empty string | empty string | **error** | error |
| `pipefail` | opt-in | n/a (`$pipestatus`) | **always on** | n/a |
| Truthiness | status + string tests | status | **no truthy values** | typed |
| `'…'` | fully raw | nearly raw | **takes escapes** | fully raw |
| `"…"` interpolates | yes | yes | yes | **no** (`$"…"` does) |
| `"\n"` is a newline | no (`$'\n'`) | no | yes | yes |
| Runs POSIX scripts | yes | no | no | no |
| Config language | bash | fish | mesh | nu |

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

All three make the safe case the default. A value is one value; nothing is
inferred from its bytes:

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
thing. bash, fish, and nushell all take those bare.

**`'…'` takes escapes, unlike every other shell.** In bash, fish, and nushell a
single-quoted string is (near enough) raw, which is why every sed one-liner in
existence is written in one. mesh follows Python instead: `'…'` is `"…"` minus
interpolation, with the same `\n \t \e \u{…}` set, and an unknown escape is an
error rather than a literal backslash. So this is a syntax error in mesh and
fine in the other three:

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

| | bash | fish | mesh | nushell |
| --- | --- | --- | --- | --- |
| Interpolating + escapes | `$"…"` doesn't exist; `"…"` interpolates, `$'…'` escapes | `"…"` (few escapes) | `"…"` | `$"…"` |
| Literal, with escapes | — | — | `'…'` | — |
| Fully raw | `'…'` | `'…'` (bar `\'`, `\\`) | `r'…'` / `r"…"` | `'…'`, `r#'…'#` |
| Both quote kinds, no escaping | heredoc | heredoc-ish | `<< 'END'` heredoc | `r#'…'#` |

mesh's four forms answer four questions with no overlap: interpolate or not,
escape or not. bash needs `$'…'` because `"…"` under-delivers; nushell needs
`$"…"` because `"…"` under-delivers the other way.

## Values and the pipeline

The deepest split among the four is what a pipe carries.

bash, fish, and mesh all carry **bytes**, which is why every program ever written
for a Unix shell works in them. nushell carries **structured data**, which buys a
genuinely better `ls | where size > 10mb` and costs you the ecosystem: an
external command's output arrives as text, and the way back to structure is a
parse (`| lines`, `| split column`, `| from json`). nushell's answer is to
reimplement the common tools as builtins that speak the format, which works well
until you need one it has not reimplemented.

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

A list reaching argv is explicit in both mesh and nushell, implicit in fish, and
punctuation in bash:

```bash
a=(one two)
cmd "${a[@]}"
```

```fish
set a one two
cmd $a
```

```mesh
a = [one two]
cmd ...$a
```

```nu
let a = [one two]
cmd ...$a
```

fish's implicit expansion is the terse option, and the cost is that
`$a` in argument position is a different arity depending on the variable — the
same data-dependence mesh is trying to remove, in a milder form. mesh's `...`
makes the arity readable from the line.

## Globbing

| | bash | fish | mesh | nushell |
| --- | --- | --- | --- | --- |
| Bare `*.txt` | expands | expands | expands | expands |
| `$x` where `x` is `"*"` | **re-globs** | literal | literal | literal |
| No match | left as literal text | **error** | contributes no arguments | left as text |
| Explicit call | — | — | `glob($p)` | `glob` / `into glob` |

bash's "no match, so pass the pattern through as a filename" is a quiet
correctness bug — `grep foo *.log` in a directory with no logs searches a file
literally named `*.log`. fish errors, which is loud but stops the command. mesh
drops the word, so a pattern that matches nothing contributes nothing, and
`glob()` is there when you want the matches as a value.

The row that matters most is the second one. In bash a variable's contents are
re-scanned for glob characters at use, so a filename containing `[` breaks a
script that never mentioned globbing. In the other three, a glob is a glob
because of how the *source* is written.

## Errors and strictness

```mesh
puts "a$undefined"
```

```text
mesh: undefined: unbound variable
```

bash and fish both give you the empty string there — the failure mode behind
every `rm -rf "$prefix/"` horror story. nushell and mesh both refuse.

mesh goes further in two places:

- **`pipefail` is always on.** A pipeline's status is the **last** stage to
  fail, or `0` when none did, with no option to turn it off — and
  `$sh.pipestatus` breaks the same run down by stage as a real list, rather
  than bash's magic `PIPESTATUS` array. An upstream stage killed by `SIGPIPE`
  because a later stage stopped reading is forgiven, which is the case
  `set -o pipefail` gets wrong.
- **There are no truthy values.** A condition is a bool or a command, and
  nothing else. `if $xs:len` is refused, naming `if $xs:len > 0` as the fix —
  where bash's `[ $x ]` and fish's `test` both quietly answer a question you did
  not ask.

## Syntax

mesh is a clean break from POSIX, like fish and nushell — none of the three runs
your old `sh` scripts, and all three run your old *programs* fine.

```mesh
if $sh.status == 0 { puts ok }
for f in *.md { puts $f }
func greet(name) { puts "hi $name" }
```

The shapes come from C-family languages rather than from `fi` / `esac` / `done`.
fish keeps `end`; nushell uses braces and closures; bash keeps the Bourne
keywords and the `[[ … ]]` grammar bolted beside them.

Two mesh choices that have no counterpart in the other three:

- **`~` is the match operator**, with `/…/` regex literals in a match slot only,
  so absolute paths need no wrapper — `$p ~ /usr/bin` is a path, `$p ~ /error/`
  is a regex, decided by word shape.
- **Modifiers chain**: `$m.rows[1].name:stem:upper` reads left to right, where
  bash has `${x%%…}` sigil soup and nushell reaches for a pipeline.

## What mesh gives up

Honest costs, in the order they will bite:

1. **Maturity.** bash is thirty-five years old, fish and nushell have real
   ecosystems and package managers, and mesh's language design is still in
   draft. See [`ROADMAP.md`](../ROADMAP.md).
2. **No POSIX compatibility.** Same as fish and nushell — your `.bashrc` does
   not port, and neither does any script you wrote.
3. **No structured pipeline.** `ls | where size > 10mb` is nushell's, and mesh
   does not offer it. Bytes on the wire is a deliberate ceiling.
4. **The two extra quoting rules** above: `word:identifier` and `'…'` escapes.
5. **Portability.** bash is on every machine you ssh into. mesh is a shell you
   install, and — as with fish and nushell — the remote end still has `sh`.

## Others in the family

Beyond the three above, the same design space is worked by:

| Shell | The idea | Escaping stance |
| --- | --- | --- |
| **elvish** | Real values, byte pipes, and a functional core | Barewords take a restricted character set and there is **no backslash escape** outside quotes — you quote instead |
| **PowerShell** | Objects on the pipe, .NET underneath | Backtick escapes; `'…'` is raw; globs are expanded by *cmdlets*, never for external programs, and native-argument passing needed a rewrite (`PSNativeCommandArgumentPassing`) to stop double-escaping |
| **YSH** (Oils) | A new language that can still run your `sh` | Ends word splitting, adds `r'…'` / `u'…'` / `b'…'` string types |
| **rc** (Plan 9) | The original clean break: real lists, no splitting | `'…'` only, doubled to escape itself; no backslash at all |

mesh's closest relatives are elvish and rc on the value model, and fish on the
day-to-day feel. The distinguishing bet is that a shell can have a real type
system *inside* while staying an ordinary Unix shell *outside* — no adapters, no
reimplemented coreutils, no structured pipeline to convert into and out of.

## Choosing

- Use **bash** when the target machine is not yours, or the script has to run
  anywhere.
- Use **fish** for a safer interactive shell with a mature ecosystem, if you are
  happy writing scripts in a second language.
- Use **nushell** when your work is mostly data wrangling and its builtins cover
  your tools.
- **mesh** is for someone who wants fish's safety, real values in the language,
  and coreutils to stay first-class — and who is willing to be early.
