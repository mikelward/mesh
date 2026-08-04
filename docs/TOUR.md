# A tour of mesh

A hands-on walk through mesh, one feature at a time. Each section builds on the
last, so read top to bottom the first time. Start the shell with `cargo run -p
mesh` and type along.

`mesh$` is mesh's default prompt. In the transcripts below, the **bold** text
after it is what you type; the plain lines under it are what you see back. When a
line is unfinished — an open brace, a heredoc still gathering — mesh asks for the
rest with a continuation prompt of dots, one per printable character of the
prompt above it, so the two line up:

<pre>
mesh$
.....
</pre>

Three neighbors, depending on what you want:

- [`INTRO.md`](INTRO.md) — the five-minute version, mesh set against the bash you
  would otherwise write.
- [`REFERENCE.md`](REFERENCE.md) — a terse lookup of everything shown here.
- [`DESIGN.md`](DESIGN.md) — why each of these choices was made, and what is still
  being decided.

---

## Running a command

The first word is the command; the rest are its arguments.

<pre>
mesh$ <strong>echo hello</strong>
hello
</pre>

If the command doesn't exist, mesh says so and carries on:

<pre>
mesh$ <strong>nonesuch</strong>
mesh: command not found: nonesuch
</pre>

Leave with `exit`, or press Ctrl-D on an empty line.

## Asking mesh what it knows

`help` prints mesh in one screen: every builtin with its usage, then every
keyword and operator with the shape it is written in.

<pre>
mesh$ <strong>help</strong>
mesh, in one screen. `help NAME` explains one entry; for a builtin,
`NAME --help` prints the same thing.

Builtins:
  bg [JOB]                       Resume a stopped job in the background
  cd [DIR]                       Change the working directory
  …
Syntax:
  cmd arg …                      Run a builtin, a function, or a program
  cmd | cmd                      Pipe one command's output into the next
  …
</pre>

`help NAME` explains one entry. For a builtin that is exactly what `NAME --help`
prints; for a keyword it is the shape you write, which is the only way to ask —
`if --help` would be an `if` whose condition is a command called `--help`.

<pre>
mesh$ <strong>help for</strong>
Repeat over a list, a range, or a map

Syntax: for NAME in VALUE { … }
</pre>

Every keyword the parser reserves and every operator a line can carry answers to
`help`, asked for exactly as you would type it — `help unless`, `help '+='`,
`help '=='`. Where several share a row the row explains the family, so the other
half of a construct answers with the construct: `help else` explains `if`, and
`help continue` explains `break`.

Where `help` explains what mesh has, **`type`** says what one name *is* — bash's
name, bash's flags, bash's words. `whence` is ksh's spelling and `where` zsh's;
both point here:

<!-- no-run: reports where `rg` is installed, which not every host has -->
<pre>
mesh$ <strong>type ll</strong>
ll is a function
    func ll(...args)
mesh$ <strong>type cd</strong>
cd is a shell builtin
    cd [DIR]
mesh$ <strong>type rg</strong>
rg is /usr/local/bin/rg
</pre>

It reports what a bare word would **run** — the winner, and nothing about what it
displaced. `-a` is where every match lives:

<!-- no-run: reports where `git` is installed, which not every host has -->
<pre>
mesh$ <strong>type git</strong>
git is a function
    func git(...args)
mesh$ <strong>type -a git</strong>
git is a function
    func git(...args)
git is /usr/bin/git
</pre>

Two flags answer in a shape a script can compare rather than read. `-t` gives one
word, and `-P` only a <code>PATH</code> hit — so a guard never has to match prose:

<pre>
mesh$ <strong>type -t git</strong>
function
mesh$ <strong>type -P git</strong>
/usr/bin/git
</pre>

Variables answer too, asked for **without the `$`** — `type xs` is a question
about the name, where `$xs` would expand before `type` ever saw it. Bindings
live in their own namespace, so a name that is both a command and a variable is
reported as both:

<pre>
mesh$ <strong>xs = [a b c]</strong>
mesh$ <strong>type xs</strong>
xs is a variable
    a list of 3: ['a', 'b', 'c']
</pre>

`type --quiet NAME` prints nothing and leaves only the status, which is how a
startup file asks whether something is installed:

```mesh
if type --quiet fzf { export FZF_DEFAULT_OPTS = "--height 40%" }
```

## Completing what you type

Press Tab while typing the first word of a command to complete builtins,
functions you have defined, and executable commands on `PATH`:

<pre>
mesh$ <strong>pw&lt;Tab&gt;</strong>
mesh$ <strong>pwd</strong>
</pre>

After the command, Tab completes files and directories in the current or named
directory. Matching is fuzzy and uses smart case: an all-lowercase query ignores
case, while any uppercase letter makes the whole query case-sensitive. Directory
entries matching the query's case rank ahead of case-folded matches. Directory
suggestions end in `/`, so you can keep completing the next path component:

<pre>
mesh$ <strong>cd pic&lt;Tab&gt;</strong>
mesh$ <strong>cd Pictures/</strong>
mesh$ <strong>puts docs/TO&lt;Tab&gt;</strong>
mesh$ <strong>puts docs/TOUR.md</strong>
</pre>

Tab also completes variables after `$`. If the variable contains a map, it
completes map keys after the dot, including keys in nested maps:

<pre>
mesh$ <strong>site = [host: example.com, tls: [enabled: true]]</strong>
mesh$ <strong>puts $site.ho&lt;Tab&gt;</strong>
mesh$ <strong>puts $site.host</strong>
example.com
mesh$ <strong>puts $site.tls.en&lt;Tab&gt;</strong>
mesh$ <strong>puts $site.tls.enabled</strong>
true
</pre>

For external commands, mesh works out the subcommands and flags for itself —
there are no completion scripts to install. It looks for a spec in four places
and takes the first that answers: a curated file you wrote under
`~/.local/share/mesh/completions/`, else the command's manual page, else a
bounded `--help` probe, else plain files and directories. Generated specs are
cached, and re-derived when their own source changes.

The file, directory, and enumerated argument types that come out of it narrow
suggestions to values that fit, so `--color <Tab>` offers only the colors that
command accepts. When you disagree with what mesh guessed, the curated file is
the override — see [Where a command's completions come
from](REFERENCE.md#where-a-commands-completions-come-from).

## Printing with `puts`

`puts` writes its arguments, separated by a single space, and a newline:

<pre>
mesh$ <strong>puts hello world</strong>
hello world
</pre>

With no arguments it prints a blank line.

## The working directory

`pwd` shows where you are; `cd` moves you. `cd` on its own goes home, and `cd -`
jumps back to where you just were, printing where it landed:

<pre>
mesh$ <strong>cd /tmp</strong>
mesh$ <strong>pwd</strong>
/tmp
mesh$ <strong>cd /</strong>
mesh$ <strong>cd -</strong>
/tmp
</pre>

## Pipes and redirection

The shell spine is the one you already know. `|` joins one command's output to
the next command's input:

<pre>
mesh$ <strong>cat words.txt | sort -r | head -2</strong>
gamma
beta
</pre>

`|&` carries **stderr down the pipe as well**, which saves writing `2>&1 |`:

<pre>
mesh$ <strong>sh -c 'echo out; echo trouble &gt;&amp;2' |&amp; sort</strong>
out
trouble
</pre>

Files attach with the usual operators — `>` writes, `>>` appends, `<` reads,
`2>` takes stderr on its own, and `&>` takes both streams:

<pre>
mesh$ <strong>puts one &gt; out.txt</strong>
mesh$ <strong>puts two &gt;&gt; out.txt</strong>
mesh$ <strong>cat &lt; out.txt</strong>
one
two
mesh$ <strong>sh -c 'echo oops &gt;&amp;2' 2&gt; err.txt</strong>
</pre>

Any descriptor works, not just the standard three (`3< input.txt`), and `n>&-`
closes one. For input written in place there is the heredoc, and the here-string
for a single word:

<pre>
mesh$ <strong>name = world</strong>
mesh$ <strong>cat &lt;&lt; END</strong>
..... <strong>hello $name</strong>
..... <strong>END</strong>
hello world
mesh$ <strong>wc -l &lt;&lt;&lt; hi</strong>
1
</pre>

An unquoted heredoc delimiter interpolates the body; quote it (`<< 'END'`) and
the body is raw.

## Chaining, and how a command reports

`;` runs one command after another, `&&` runs the next only if the last
succeeded, and `||` only if it failed:

<pre>
mesh$ <strong>test -e words.txt &amp;&amp; puts found || puts missing</strong>
found
</pre>

`$sh.status` is the last command's exit status — the readable replacement for
`$?`. A pipeline reports the **pipefail** status, always: the last stage to fail,
or `0` when none did. `$sh.pipestatus` is the per-stage breakdown, as a real list:

<pre>
mesh$ <strong>sh -c 'exit 3' | sort</strong>
mesh$ <strong>puts $sh.status ...$sh.pipestatus</strong>
3 3 0
</pre>

Read them in **one** command when you want both: reading either is itself a
command, so a first `puts` would replace what a second one reports.

`not` in front of a command inverts what it reports:

<pre>
mesh$ <strong>not test -e words.txt &amp;&amp; puts missing</strong>
mesh$ <strong>not sh -c 'exit 3'</strong>
mesh$ <strong>puts $sh.status</strong>
0
</pre>

## Background jobs

`&` runs a command in the background and hands you back the prompt. Bind it and
you have a **job handle** — mesh's replacement for `$!`:

<!-- no-run: waits on a 30-second background sleep -->
<pre>
mesh$ <strong>j = sleep 30 &amp;</strong>
[1] 4812
mesh$ <strong>puts $j.state</strong>
running
mesh$ <strong>jobs</strong>
[1] Running sleep 30
mesh$ <strong>wait $j</strong>
</pre>

`fg`, `bg`, `wait`, and `kill` all take a handle or a `%1`-style reference. The
first three only ever mean a job, so a bare number is one there too (`fg 1` is
`fg %1`) — but **`kill` is the exception**: `kill 1` signals *process* 1, as it
does in every shell, so name the job (`kill %1`, `kill $j`) when that is what you
mean.

Because `kill` reads a bare number as a pid, a handle is careful never to *become*
one: `$j` has no text form at all. Printing or interpolating it is an error that
tells you to ask for a member instead, so a handle reaches `kill` as a handle or
not at all — it cannot collapse into a number on the way and signal some unrelated
process:

<!-- no-run: continues the background-job session above -->
<pre>
mesh$ <strong>puts $j</strong>
mesh: puts: a job handle has no text form; ask it for a member
mesh$ <strong>puts $j.id $j.pid $j.state</strong>
1 4812 running
</pre>

The whole table is a value too — `$sh.jobs` — so a prompt can read the live jobs
straight out of it instead of scraping `jobs` output.

## Matching filenames

An unquoted `*`, `?`, or `[…]` is matched against the files in the directory —
the matches come back sorted:

<pre>
mesh$ <strong>puts *.txt</strong>
notes.txt todo.txt
</pre>

> If a pattern matches nothing, it contributes **no arguments** — not the pattern
> itself. A search that finds nothing is simply empty.

The same expansion has a call form, which hands back a **list** you can loop over
or bind. `dirs()` and `files()` are the directory's own subdirectories and files:

<pre>
mesh$ <strong>for d in dirs() { puts "$d/" }</strong>
src/
tests/
mesh$ <strong>notes = glob("*.txt")</strong>
mesh$ <strong>puts $notes:len</strong>
2
</pre>

`glob()` is how a pattern you *built* gets expanded — a stored string is inert, so
`ls $p` passes `*.txt` through as text, and `glob($p)` asks for its matches.

A `~` at the start of a word becomes your home directory:

<pre>
mesh$ <strong>puts ~</strong>
/home/you
</pre>

## Quoting

Three kinds of quotes, each with one job.

**Double quotes** `"…"` read escapes like `\t` and `\n`:

<pre>
mesh$ <strong>puts "a\tb"</strong>
a	b
</pre>

**Single quotes** `'…'` read the same escapes but leave `$` alone:

<pre>
mesh$ <strong>puts 'a\nb'</strong>
a
b
</pre>

**Raw quotes** `r'…'` (or `r"…"`) take everything literally — nothing is
special inside, which makes them the place for backslash-heavy text:

<pre>
mesh$ <strong>puts r'C:\new\tab'</strong>
C:\new\tab
</pre>

Quoting also switches off filename matching, so a quoted `*` stays a `*`:

<pre>
mesh$ <strong>puts '*'</strong>
*
</pre>

## Variables

Bind a value with `=`, read it back with `$name`:

<pre>
mesh$ <strong>greeting=hello</strong>
mesh$ <strong>puts $greeting</strong>
hello
</pre>

Inside double quotes, `$name` is filled in; inside single or raw quotes it stays
literal:

<pre>
mesh$ <strong>puts "$greeting, world"</strong>
hello, world
</pre>

Wrap the name in braces when the next character would otherwise run into it — or
keep the literal part in its own quotes, since pieces sitting next to each other
join into one argument:

<pre>
mesh$ <strong>n = 42</strong>
mesh$ <strong>puts "${n}nd"</strong>
42nd
mesh$ <strong>puts $n"nd"</strong>
42nd
</pre>

> A value is always **one value**. If `$x` holds `*`, it prints as `*` — an
> interpolated value is never re-matched against filenames or split on spaces.

Read an environment variable through `$env`:

<pre>
mesh$ <strong>puts $env.HOME</strong>
/home/you
</pre>

## Capturing a command's output

`$(command)` is the command's standard output as a value — **one string**, with
trailing newlines trimmed:

<pre>
mesh$ <strong>here = $(pwd)</strong>
mesh$ <strong>puts "at $here"</strong>
at /home/you
mesh$ <strong>puts "$(id -un)@$(hostname)"</strong>
you@laptop
</pre>

Nothing splits on its own — that is the promise mesh keeps that bash does not —
so when you want the lines you say so:

<!-- no-run: runs `git`, which not every host has -->
<pre>
mesh$ <strong>for line in $(git status --porcelain):lines { puts "[$line]" }</strong>
[ M docs/DESIGN.md]
[?? notes.txt]
</pre>

`:ls` is the short spelling, and `:nulls` (`:ns`) is the one for `find -print0`,
splitting on NUL only so a newline inside a filename survives. Forget the
modifier and the loop tells you: a `for` over something that is not a list is
refused, and names the fix.

Quoting a capture changes nothing here — `$(pwd)` and `"$(pwd)"` are the same
string. Quote it to glue it to other text in a word, not to defend against
splitting, which is the habit bash teaches.

What comes back is literal — never re-split on spaces, never re-globbed — and a
capture whose command fails still hands back what it printed, since a nonzero exit
is often the answer rather than an error (`diff` says 1 when files differ). Bind
it with `if out = $(cmd) { … } else { … }` when you care which it was.

## Writing the environment

`$env.KEY = value` writes the process environment, so children inherit it.
`export KEY = value` is the same write in the spelling your fingers know:

<pre>
mesh$ <strong>export GREETING = hi</strong>
mesh$ <strong>sh -c 'echo $GREETING'</strong>
hi
</pre>

Path-type names — `PATH`, `MANPATH`, `CDPATH`, and a few more — are **lists** on
the way in and `:`-joined on the way out, so the `IFS` juggling disappears:

<pre>
mesh$ <strong>$env.PATH += /opt/bin</strong>
mesh$ <strong>puts $env.PATH:last</strong>
/opt/bin
mesh$ <strong>$env.PATH = $env.PATH:dedup</strong>
mesh$ <strong>$env.PATH = $env.PATH:prepend(/usr/local/bin):dedup</strong>
</pre>

Only strings cross into the environment, so a list or map has to be joined
first — mesh says so rather than inventing a rendering.

To set a name for **one command** and no longer, put it in front of the command,
the way every other shell spells it:

<pre>
mesh$ <strong>TZ=UTC date</strong>
Sat Aug  1 05:00:00 UTC 2026
mesh$ <strong>sh -c 'echo [$TZ]'</strong>
[]
</pre>

Take as many as you like, and `+=` appends just as it does for `$env.PATH`:
`TZ=UTC LANG=C sort names.txt`, `PATH+=/opt/bin mytool`. The entries are put back
afterwards — a name that was unset goes back to *unset*, which a child can tell
apart from empty.

A prefix binds to one **stage**, so each side of a pipe gets its own and an `&&`
right-hand side gets none:

<pre>
mesh$ <strong>FOO=1 sh -c 'echo $FOO' | FOO=2 sh -c 'echo $FOO; cat'</strong>
2
1
</pre>

Note which namespace that writes. A prefix sets the **environment**, because what
the child inherits is the point; a bare `FOO=bar` with no command after it is an
ordinary assignment and binds a shell variable no child ever sees.

For a whole block rather than one command, `with` is the same idea with braces,
and puts the environment back however the body leaves — normally, through a
failing command, or through `return`, `break` or `continue`:

<pre>
mesh$ <strong>with TZ=UTC LANG=C {</strong>
.....   <strong>date</strong>
..... <strong>}</strong>
Sat Aug  1 05:00:00 UTC 2026
</pre>

Unlike `fork`, neither costs a process.

## Lists preserve structure

Square brackets build a list. Lists may contain other lists, and mesh never
guesses whether you meant to flatten one. A plain list reference is one nested
value; an explicit `...` spread flattens exactly one level:

<pre>
mesh$ <strong>inner = [two three]</strong>
mesh$ <strong>nested = [one $inner four]</strong>
mesh$ <strong>flat = [one ...$inner four]</strong>
mesh$ <strong>puts ...$nested[1]</strong>
two three
mesh$ <strong>puts ...$flat</strong>
one two three four
</pre>

Indexes are zero-based (negative indexes count from the end), slices clamp to
the available range, and `+=` appends a scalar or extends with a list. A nested
list cannot be passed to a command by accident: select and spread the inner list
explicitly.

## Maps preserve insertion order

A bracket literal containing `key: value` pairs is a map. Map keys are strings,
and `[:]` is the empty map (`[]` remains the empty list). Read identifier keys
with dot syntax or use brackets for a computed key:

<pre>
mesh$ <strong>ports = [http: 80, https: 443, http: 8080]</strong>
mesh$ <strong>protocol = https</strong>
mesh$ <strong>puts $ports.http ${ports[$protocol]}</strong>
8080 443
</pre>

Duplicate keys are last-value-wins without changing their original position.
Spreading a map and merging with `+=` follow the same rule:

<pre>
mesh$ <strong>ports += [ssh: 22, http: 8000]</strong>
mesh$ <strong>copy = [...$ports, ssh: 2222]</strong>
mesh$ <strong>puts ...$copy:keys</strong>
http https ssh
mesh$ <strong>puts ...$copy:values</strong>
8000 443 2222
</pre>

`:len` counts map entries. `:keys` and `:values` return real lists in insertion
order, so they need `...` when passed to a command. A missing key is an error,
and a whole map cannot be passed to an external command implicitly.

## Transforming values with modifiers

A postfix `:` modifier transforms a value. Path modifiers provide the common
filename pieces without starting another process:

<pre>
mesh$ <strong>file=src/archive.tar.gz</strong>
mesh$ <strong>puts $file:dir $file:base $file:stem $file:ext</strong>
src archive.tar.gz archive.tar gz
</pre>

`:exts` returns every extension (`tar.gz` above), while `:bare` removes every
extension (`archive`). `:upper` and `:lower` change string case. Modifiers chain
from left to right:

<pre>
mesh$ <strong>puts $file:base:upper</strong>
ARCHIVE.TAR.GZ
</pre>

Lists have collection modifiers. `:len` counts elements; `:first` and `:last`
select one; `:rest` and `:init` return a list without its first or last element;
and `:dedup` removes later duplicates while preserving order:

<pre>
mesh$ <strong>xs = [one two two three]</strong>
mesh$ <strong>puts $xs:len $xs:first $xs:last</strong>
4 one three
mesh$ <strong>puts ...$xs:rest:init:dedup</strong>
two
</pre>

`:prepend(e)` and `:append(e)` add one element at either end, and `:extend(ys)`
adds a list's own elements. All three return a new list rather than writing one —
the pure form of `+=`, which is what lets them chain:

<pre>
mesh$ <strong>puts $xs:append(four):last</strong>
four
mesh$ <strong>$env.PATH = $env.PATH:prepend(/opt/bin):dedup</strong>
</pre>

The name says which addition you meant, so neither reads its argument's type:
`$xs:append($ys)` adds `$ys` as one element and `$xs:extend($ys)` adds its
elements. (`+=` decides that from the right-hand type instead, which is the one
place mesh flattens by type rather than by an explicit `...`.)

A list-returning modifier remains a real list, so spread it with `...` in
command arguments or assign it intact (`ys = $xs:rest`). Path and case
modifiers map over a list element by element. A modifier applies to a literal as
much as to a variable, so `abc:upper` is `ABC`.

`:` followed by an identifier is reserved, so an unknown name is a syntax error
rather than literal text — `ubuntu:latest` has to be written `"ubuntu:latest"`, or
`"${image}:latest"` when the name comes from a variable. Only a bare identifier is
claimed, which is what keeps `$host:$port`, `http://x` and `key:2` reading as text.

A modifier that takes an argument spells it in parentheses. `:join(SEP)` folds a
list back into a string and `:split(SEP)` unfolds a string into a list:

<pre>
mesh$ <strong>dirs = [/usr/bin /bin]</strong>
mesh$ <strong>path = $dirs:join(":")</strong>
mesh$ <strong>puts $path</strong>
/usr/bin:/bin
mesh$ <strong>fields = $path:split(":")</strong>
mesh$ <strong>puts $fields:len</strong>
2
</pre>

`:split` treats the separator as a terminator, so a trailing delimiter adds no
empty element (`"a:b:":split(":")` is `[a b]`). These read the same as a command
argument as they do on the right of an `=` — `puts $dirs:join(":")` — with one
exception: spreading one at a command boundary (`puts ...$path:split(":")`) is
not wired up yet, so bind it first.

`:get(KEY, DEFAULT)` is the **total** accessor, where `$m.key` and `$xs[i]` fail
loud. It is how you read something that may not be there:

<pre>
mesh$ <strong>puts $env:get(EDITOR, vim)</strong>
vim
mesh$ <strong>puts $fields:get(9, "-")</strong>
-
</pre>

A bare `$env` is the whole environment as a map, which is what gives `:get` an
ordinary map to work on; `$env.NAME` stays the strict read that errors when the
name is unset. Note that a name bound to `""` is *present*, so it wins over the
default — bash's `${EMPTY:-vim}` substitutes, and this does not.

Strings have an affix family for dropping a known prefix or suffix, and a replace
family whose first argument is a **pattern slot**: a string matches verbatim, a
`/…/` regex matches as a pattern.

<pre>
mesh$ <strong>puts "report.tar.gz":stripend(".tar.gz")</strong>
report
mesh$ <strong>puts "a.b.c":replaceall(".", "-")</strong>
a-b-c
mesh$ <strong>puts "a.b.c":replaceall(/./, "-")</strong>
-----
</pre>

## Numbers, booleans, and operators

Decimal numbers and `true` / `false` are typed values. Arithmetic operates only
on integers, and comparisons produce booleans:

<pre>
mesh$ <strong>answer = 20 * 2 + 2</strong>
mesh$ <strong>is-answer = $answer == 42</strong>
mesh$ <strong>puts $answer $is-answer</strong>
42 true
</pre>

Strings are not silently converted to numbers; use `:int` when conversion is
intentional. Besides `==`, `!=`, `<`, `<=`, `>`, and `>=`, value expressions
support `in` for membership and `not`, `and`, and `or` for boolean logic.

## Matching strings with `~`

The infix `~` operator matches a string against either a bare filename-style
glob or a slash-delimited regex. `!~` is the negative form. Globs cover the
whole string; regexes search within it unless you add anchors:

<pre>
mesh$ <strong>is-source = src/main.rs ~ src/*.rs</strong>
mesh$ <strong>puts $is-source</strong>
true
mesh$ <strong>has-number = item42 ~ /\d+$/</strong>
mesh$ <strong>puts $has-number</strong>
true
mesh$ <strong>not-source = notes.txt !~ *.rs</strong>
mesh$ <strong>puts $not-source</strong>
true
</pre>

Regex bodies are raw (`$` is an anchor, not interpolation), with `\/` for a
literal slash. Flags are postfix modifiers: `/error/:i` ignores case, `:m`
enables multiline anchors, and `:s` lets `.` match newlines. For a reusable or
computed regex, construct a value with `re(r'^a.c$')`; use
`re('a.c', literal: true)` when the input text must be matched literally. Quoted
strings are deliberately not accepted as patterns on the right of `~`.

## When something is missing

Reading a name you never set is an error, not a silent blank — and the shell
recovers and keeps going:

<pre>
mesh$ <strong>puts $nope</strong>
mesh: nope: unbound variable
mesh$ <strong>puts still here</strong>
still here
</pre>

## Choosing with `if`

`if` runs a command as its condition. Status `0` selects the first body; any
other status selects `else` when one is present:

<pre>
mesh$ <strong>if test -d .git {</strong>
.....   <strong>puts repository</strong>
..... <strong>} else {</strong>
.....   <strong>puts ordinary-directory</strong>
..... <strong>}</strong>
repository
</pre>

Chain another test with `else if`:

<pre>
mesh$ <strong>if false { puts no } else if true { puts yes }</strong>
yes
</pre>

`not` in front of a condition branches on the command having *failed*:

<pre>
mesh$ <strong>if not test -d .git { puts "not a repository" } else { puts repository }</strong>
repository
mesh$ <strong>if not test -e CHANGELOG.md { puts "no changelog yet" }</strong>
no changelog yet
</pre>

An `if` is also a value in an assignment. The selected body's final line is the
value; it can currently be one string, a list or map literal, a whole variable
value, or another `if`:

<pre>
mesh$ <strong>label = if test -d .git { "git tree" } else { "directory" }</strong>
mesh$ <strong>puts $label</strong>
git tree
mesh$ <strong>names = if true { [Ada "Grace Hopper"] } else { [] }</strong>
mesh$ <strong>puts ...$names</strong>
Ada Grace Hopper
</pre>

When a false value-producing `if` has no `else`, it yields the empty string.
Only the selected body runs. A condition can be a command/function status or a
value expression such as `$answer == 42`.

List patterns can test a shape and bind its pieces at the same time. `_` ignores
one element and `...rest` captures any number of middle elements. A mismatch in
an `if` simply chooses `else` without changing any bindings:

<pre>
mesh$ <strong>items = [first middle last]</strong>
mesh$ <strong>if [head ...rest] = $items { puts $head ...$rest }</strong>
first middle last
</pre>

The same patterns work in assignment, `for`, and `match`. A mismatched plain
assignment is an error rather than a partial binding.

## Iterating collections

`for` iterates lists without splitting their elements, integer ranges by value,
and maps in insertion order. Map loops use a key and value binder:

<pre>
mesh$ <strong>for item in [one "two words"] { puts $item }</strong>
one
two words
mesh$ <strong>for n in 1..=3 { puts $n }</strong>
1
2
3
mesh$ <strong>ports = [http: 80, https: 443]</strong>
mesh$ <strong>for protocol, port in $ports { puts "$protocol=$port" }</strong>
http=80
https=443
</pre>

`1..3` excludes `3`; `1..=3` includes it. `break` exits the nearest loop and
`continue` skips to its next iteration. A list-pattern binder destructures each
list element, for example `for [key value] in $pairs { ... }`.

## Repeating with `while` and `loop`

`while` tests before each pass, taking the same two condition forms `if` does —
a value's truthiness or a command's exit status. `loop` repeats until something
breaks out:

<!-- no-run: loops until a deploy that never happens -->
<pre>
mesh$ <strong>i = 0</strong>
mesh$ <strong>while $i &lt; 3 { puts $i; i = $i + 1 }</strong>
0
1
2
mesh$ <strong>while test -e /tmp/lock { sleep 1 }</strong>
mesh$ <strong>loop { if deploy-succeeded { break }; sleep 5 }</strong>
</pre>

In a condition a spaced comparison compares — `while $i < 3` is a test, not a
redirection — while `cmd > log` still writes the file.

## One-line guards

A statement can carry its own condition at the end, which reads better than
wrapping one line in an `if`. `unless` is the inverse:

<pre>
mesh$ <strong>for n in 1..=4 {</strong>
.....   <strong>continue if $n == 2</strong>
.....   <strong>puts $n unless $n == 3</strong>
..... <strong>}</strong>
1
4
</pre>

The condition is a value expression rather than a command, and a false guard
skips the statement without disturbing `$sh.status`. `if` only starts a guard
when what follows completes an expression, so `puts x if test -d .git` still
hands `puts` those words as arguments.

## Selecting a value with `match`

`match` tries arms from top to bottom. It supports exact values, globs, regular
expressions, integer ranges, `|` alternatives, list patterns, `_`, and `if`
guards:

<pre>
mesh$ <strong>command = [start server verbose]</strong>
mesh$ <strong>result = match $command {
.....   [verb ...args] if $verb == start =&gt; [$verb ...$args]
.....   _ =&gt; []
..... }</strong>
mesh$ <strong>puts ...$result</strong>
start server verbose
</pre>

Every arm needs `=>`, and arms are separated by a newline or `;`. A body is either a
value or a `{ }` block, which is what decides how a bare word reads: `=> text` is the
string `"text"`, while `=> { less $file }` runs a command.

Use `~` when a one-line glob or regex boolean test is clearer than a `match`.

## Functions

Give a sequence of commands a name with `func`. Parameters are named — you write
`$name` in the body, not `$1`:

<pre>
mesh$ <strong>func greet(name) {</strong>
.....   <strong>puts "hi, $name"</strong>
..... <strong>}</strong>
mesh$ <strong>greet world</strong>
hi, world
</pre>

A body can be one line too: `func sq(x) { puts $x $x }`. Each call runs in its own
scope, so a variable you set inside a function stays inside it:

<pre>
mesh$ <strong>func work() { tmp = scratch; puts $tmp }</strong>
mesh$ <strong>work</strong>
scratch
mesh$ <strong>puts $tmp</strong>
mesh: tmp: unbound variable
</pre>

A signature can do more than required positionals. A parameter with `= value` is
optional, a `--name` parameter is a flag (a bare `--force` switch or a valued
`--tag = default`), and a trailing `...name` collects whatever is left:

<pre>
mesh$ <strong>func deploy(target, --tag = latest, --force, ...hosts) {</strong>
.....   <strong>puts "$target $tag $force"; puts ...$hosts</strong>
..... <strong>}</strong>
mesh$ <strong>deploy prod --force web1 web2</strong>
prod latest true
web1 web2
mesh$ <strong>deploy prod --tag=v9 web1</strong>
prod v9 false
web1
</pre>

Flags can appear in any order and never get swallowed as positionals; a bare `--`
ends flag parsing so a later `--word` reaches `...hosts` as data.

A function's status is whatever its body did last, so a predicate needs no
`return` at all — it reads as true/false in `&&` / `||` on its own:

<pre>
mesh$ <strong>func check(x) { test -e $x }</strong>
mesh$ <strong>check /etc &amp;&amp; puts present</strong>
present
mesh$ <strong>check /nope || puts missing</strong>
missing
</pre>

`return` leaves early, and what it carries is a **value**, not a status — it is
what a `check()` call hands back. The status that comes with a value is a view of
it: only `false` fails, since every other value *is* a result and producing one is
success. So `return 1` succeeds carrying the number `1`; it is not bash's
`return 1`. When you mean the status, the word is **`fail`**:

<pre>
mesh$ <strong>func check(x) { test -e $x || fail; puts "$x is here" }</strong>
mesh$ <strong>check /nope || puts missing</strong>
missing
</pre>

`fail` takes a number too (`fail 3`), and stops the body the way `return` does.

A function is also an ordinary pipeline stage or background job — `f | sort`,
`echo x | f`, `f &` — each running in its own process, exactly as an external
command does.

To **wrap** a command, give the function the command's own name and reach the
program with `command`, which looks past the builtins and functions a bare name
would find. Without it the body would call the function again:

<pre>
mesh$ <strong>func ls(...args) { command ls --color=auto ...$args }</strong>
mesh$ <strong>ls</strong>
docs  src
</pre>

Everything after the program name belongs to the program, so `command ls --help`
asks `ls` for its help rather than printing mesh's.

## Calling a function for a value

Attach the parentheses and you get the function's **value** rather than its
status: the last expression of the body, or whatever `return` carries. The same
default as the status side — the body's last thing is the answer — so `return` is
for leaving early, not for handing a value back:

<pre>
mesh$ <strong>func double(n) { $n * 2 }</strong>
mesh$ <strong>x = double(21)</strong>
mesh$ <strong>puts $x</strong>
42
</pre>

Arguments there are expressions, and `key: value` binds the same parameter the
`--key` flag does, so `deploy(prod, force: true)` and `deploy prod --force` are
the same call.

When you want every channel at once, `:capture` runs the call and hands back a
record of `.value`, `.out`, `.err`, and `.status`:

<pre>
mesh$ <strong>func build() { puts compiling; return ok }</strong>
mesh$ <strong>r = build():capture</strong>
mesh$ <strong>puts "$r.value $r.status"</strong>
ok 0
mesh$ <strong>puts $r.out:repr</strong>
'compiling\n'
</pre>

## Lambdas, and modifiers that take them

`func(params) { body }` with no name is a **function value**. `:map`, `:filter`,
and `:each` take one and apply it to every element of a list:

<pre>
mesh$ <strong>xs = [1 2 3 4]</strong>
mesh$ <strong>doubled = $xs:map(func(x) { $x * 2 })</strong>
mesh$ <strong>evens = $xs:filter(func(x) { $x % 2 == 0 })</strong>
mesh$ <strong>puts ...$doubled</strong>
2 4 6 8
mesh$ <strong>puts ...$evens</strong>
2 4
</pre>

A bare `:modifier` is itself a callable, so a mapper that only forwards to one
can be written directly — `$paths:map(:base)` says what
`$paths:map(func(p) { $p:base })` says. `:filter` insists on a real boolean, and
`:each` runs for effect and yields nothing.

Bind a lambda and call it through the variable — the `$` is required, since a
bare `double(5)` looks for a *declared* function:

<pre>
mesh$ <strong>double = func(x) { $x * 2 }</strong>
mesh$ <strong>puts $double(5)</strong>
10
</pre>

## Isolating a block with `fork`

A `func` is not a subshell: a `cd` inside one persists, on purpose. `fork { … }`
is the opt-in for the other behavior — the body runs in a forked child, so the
cwd, environment, and bindings it changes are its own:

<pre>
mesh$ <strong>fork { cd /tmp; puts $(pwd) }</strong>
/tmp
mesh$ <strong>puts $(pwd)</strong>
/home/you
</pre>

Only bytes cross back out: what the child prints appears, its exit status becomes
the block's, and an `exit` inside ends the child rather than your shell.

## Color and links

`style` returns a string carrying display attributes, and `link` one carrying a
hyperlink. Both are value calls, so the parens are attached:

<pre>
mesh$ <strong>danger = style("danger", fg: red, bold: true)</strong>
mesh$ <strong>puts $danger</strong>
danger
mesh$ <strong>puts "level: $danger"</strong>
level: danger
mesh$ <strong>puts link("the docs", "https://example.com/guide")</strong>
the docs
</pre>

Everywhere bytes are wanted the value behaves as its plain text — `$danger:len`
is `6`, and a comparison matches the text — and the escapes are written only when
the command's own stdout is a terminal, so a redirect or a capture gets clean
text.

## Running a script

The same language runs from a file. `mesh script.mesh a b c` passes the
arguments as `$sh.args`, a real list, and `#` starts a comment, so a shebang
works:

```mesh
#!/usr/bin/env mesh
puts "$sh.name got $sh.args:len arguments"
for arg in $sh.args { puts $arg }
```

`mesh -c "puts hi"` runs one command string, and with neither, mesh reads
commands from stdin — so `echo 'ls' | mesh` works without a flag. A script is
parsed as a single unit, so a syntax error anywhere in the file means none of it
runs.

## Making it your shell

mesh reads its configuration from `$XDG_CONFIG_HOME/mesh` (or `~/.config/mesh`):
`env.mesh` every invocation, `login.mesh` for login shells, and `rc.mesh` for
interactive ones. `source FILE` runs a file's mesh code in *this* shell, so what
it defines outlives it:

<pre>
mesh$ <strong>source lib.mesh</strong>
mesh$ <strong>greet world</strong>
hi, world
</pre>

## Hooks

`on EVENT NAME FUNC` asks the shell to call a function of yours when something
happens. `postcd` is the one to see it with — it runs after every directory
change, in the new directory, and is handed the one you came from:

<pre>
mesh$ <strong>func arrived(previous) { puts "came from $previous" }</strong>
mesh$ <strong>on postcd note arrived</strong>
mesh$ <strong>cd src</strong>
came from /home/you/project
mesh$ <strong>cd ..</strong>
came from /home/you/project/src
</pre>

`note` is the hook's **name**, not the function's, and that is what makes a
config reloadable: registering `postcd note` again *replaces* that hook in place
rather than adding a second one, so sourcing your `rc.mesh` twice does not give
you two of everything. `on --remove postcd note` takes it off. Hooks are
session-local and run in the order they were registered.

<pre>
mesh$ <strong>on --remove postcd note</strong>
mesh$ <strong>cd src</strong>
</pre>

Seven events, each handed what it is about:

| Event | Parameters | When |
| --- | --- | --- |
| `preprompt` | — | before each prompt is drawn |
| `preexec` | `command` | just before an interactive command runs |
| `postexec` | `command, status, elapsed` | after it, `elapsed` in milliseconds |
| `precd` | `target` | before a directory change, still in the old one |
| `postcd` | `previous` | after it, in the new one |
| `jobdone` | `id, command, status` | when a background job is found finished |
| `exit` | `status` | before the shell leaves, however the session ended |

`preprompt` is the one a dynamic prompt hangs off, since `prompt "text"` on its
own is fixed:

```mesh
func refresh-prompt() { prompt "$(pwd)> " }
on preprompt cwd refresh-prompt

func command-finished(cmd, status, elapsed) {
  puts "$cmd exited $status after ${elapsed}ms"
}
on postexec log command-finished
```

[`REFERENCE.md`](REFERENCE.md#custom-prompts-and-hooks) has the rest: what each
event guarantees, and the `$sh.<event>` maps that hold the same registrations as
values.

Interactive decorations — bold input, the window title, the working-directory
report, shell integration marks, the notification a slow command raises — are all
on out of the box and each turns off on its own:

```mesh
$sh.options.osc-title = false
```

At the prompt itself the keys are the emacs ones (Ctrl-A, Ctrl-R to search
history, Alt-. to pull in the last argument), history is saved across sessions,
and `!$` / `!^` / `!*` reach for the previous command's arguments.

---

That is the tour: the main features, in the order they build on each other. It is
not everything — [`REFERENCE.md`](REFERENCE.md) lists the whole surface, down to
the corners this walk skipped, and `help` at the prompt answers for any builtin,
keyword, or operator by name.
