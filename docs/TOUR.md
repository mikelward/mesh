# A tour of mesh

A hands-on walk through mesh, one feature at a time. Each section builds on the
last, so read top to bottom the first time. Start the shell with `cargo run -p
mesh` and type along.

`mesh$` is mesh's default prompt. In the transcripts below, the **bold** text
after it is what you type; the plain lines under it are what you see back.

For a terse lookup of everything shown here, see [`REFERENCE.md`](REFERENCE.md).

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

## Completing what you type

Press Tab while typing the first word of a command to complete builtins,
functions you have defined, and executable commands on `PATH`:

<pre>
mesh$ <strong>pw&lt;Tab&gt;</strong>
mesh$ <strong>pwd</strong>
</pre>

After the command, Tab completes files and directories in the current or named
directory. Directory suggestions end in `/`, so you can keep completing the
next path component:

<pre>
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

For external commands, mesh lazily reads bounded `--help` output to complete
subcommands and flags, then caches the resulting completion spec by executable
path and modification time. Files and directories remain available as the
fallback. Completion currently matches prefixes exactly and case-sensitively;
typed argument values, fuzzy matching, and case-insensitive matching are later
work.

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

## Matching filenames

An unquoted `*`, `?`, or `[…]` is matched against the files in the directory —
the matches come back sorted:

<pre>
mesh$ <strong>puts *.txt</strong>
notes.txt todo.txt
</pre>

> If a pattern matches nothing, it contributes **no arguments** — not the pattern
> itself. A search that finds nothing is simply empty.

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
mesh$ <strong>n=42</strong>
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

A list-returning modifier remains a real list, so spread it with `...` in
command arguments or assign it intact (`ys = $xs:rest`). Path and case
modifiers map over a list element by element. An unknown name is not consumed as
a modifier, which keeps constructions such as `$host:$port` working literally.
Modifier arguments such as `:join(",")` are not implemented yet.

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
...   <strong>puts repository</strong>
... <strong>} else {</strong>
...   <strong>puts ordinary-directory</strong>
... <strong>}</strong>
repository
</pre>

Chain another test with `else if`:

<pre>
mesh$ <strong>if false { puts no } else if true { puts yes }</strong>
yes
</pre>

An `if` is also a value in an assignment. The selected body's final line is the
value; it can currently be one string, a list or map literal, a whole variable
value, or another `if`:

<pre>
mesh$ <strong>label = if test -d .git { "git tree" } else { directory }</strong>
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

## Selecting a value with `match`

`match` tries arms from top to bottom. It supports exact values, globs, regular
expressions, integer ranges, `|` alternatives, list patterns, `_`, and `if`
guards:

<pre>
mesh$ <strong>command = [start server verbose]</strong>
mesh$ <strong>result = match $command {
...   [verb ...args] if $verb == start { [$verb ...$args] }
...   _ { [] }
... }</strong>
mesh$ <strong>puts ...$result</strong>
start server verbose
</pre>

Use `~` when a one-line glob or regex boolean test is clearer than a `match`.

## Functions

Give a sequence of commands a name with `func`. Parameters are named — you write
`$name` in the body, not `$1`:

<pre>
mesh$ <strong>func greet(name) {</strong>
...   <strong>puts "hi, $name"</strong>
... <strong>}</strong>
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

`return` leaves a function early; `return N` also sets its status, so a function
reads as true/false in `&&` / `||`:

<pre>
mesh$ <strong>func check(x) { test -e $x && return 0; return 1 }</strong>
mesh$ <strong>check /etc && puts present</strong>
present
</pre>

---

That's everything mesh does today. New features land one at a time, and this tour
grows to match.
