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

## When something is missing

Reading a name you never set is an error, not a silent blank — and the shell
recovers and keeps going:

<pre>
mesh$ <strong>puts $nope</strong>
mesh: nope: unbound variable
mesh$ <strong>puts still here</strong>
still here
</pre>

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
