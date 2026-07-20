# A tour of mesh

A hands-on walk through mesh, one feature at a time. Each section builds on the
last, so read top to bottom the first time. Start the shell with `cargo run -p
mesh` and type along.

Lines beginning with `mesh$` are what you type; the lines under them are what you
see back.

For a terse lookup of everything shown here, see [`REFERENCE.md`](REFERENCE.md).

---

## Running a command

The first word is the command; the rest are its arguments.

```
mesh$ echo hello
hello
```

If the command doesn't exist, mesh says so and carries on:

```
mesh$ nonesuch
mesh: command not found: nonesuch
```

Leave with `exit`, or press Ctrl-D on an empty line.

## Printing with `puts`

`puts` writes its arguments, separated by a single space, and a newline:

```
mesh$ puts hello world
hello world
```

With no arguments it prints a blank line.

## The working directory

`pwd` shows where you are; `cd` moves you. `cd` on its own goes home, and `cd -`
jumps back to where you just were, printing where it landed:

```
mesh$ cd /tmp
mesh$ pwd
/tmp
mesh$ cd /
mesh$ cd -
/tmp
```

## Matching filenames

An unquoted `*`, `?`, or `[…]` is matched against the files in the directory —
the matches come back sorted:

```
mesh$ puts *.txt
notes.txt todo.txt
```

> If a pattern matches nothing, it contributes **no arguments** — not the pattern
> itself. A search that finds nothing is simply empty.

A `~` at the start of a word becomes your home directory:

```
mesh$ puts ~
/home/you
```

## Quoting

Three kinds of quotes, each with one job.

**Double quotes** `"…"` read escapes like `\t` and `\n`:

```
mesh$ puts "a\tb"
a	b
```

**Single quotes** `'…'` read the same escapes but leave `$` alone:

```
mesh$ puts 'a\nb'
a
b
```

**Raw quotes** `r'…'` (or `r"…"`) take everything literally — nothing is
special inside, which makes them the place for backslash-heavy text:

```
mesh$ puts r'C:\new\tab'
C:\new\tab
```

Quoting also switches off filename matching, so a quoted `*` stays a `*`:

```
mesh$ puts '*'
*
```

Pieces sitting next to each other join into one argument:

```
mesh$ puts --flag='a b'
--flag=a b
```

## Variables

Bind a value with `=`, read it back with `$name`:

```
mesh$ greeting = hello
mesh$ puts $greeting
hello
```

Inside double quotes, `$name` is filled in; inside single or raw quotes it stays
literal:

```
mesh$ puts "$greeting, world"
hello, world
```

Wrap the name in braces when the next character would otherwise run into it:

```
mesh$ n = 42
mesh$ puts "${n}nd"
42nd
```

> A value is always **one value**. If `$x` holds `*`, it prints as `*` — an
> interpolated value is never re-matched against filenames or split on spaces.

Read an environment variable through `$env`:

```
mesh$ puts $env.HOME
/home/you
```

## Running several commands

Put more than one command on a line. A `;` just runs them in order:

```
mesh$ puts one; puts two
one
two
```

`&&` runs the next command only if the one before it succeeded; `||` only if it
failed:

```
mesh$ true && puts it-worked
it-worked
mesh$ false || puts fell-back
fell-back
```

## Pipelines

A `|` feeds one command's output straight into the next:

```
mesh$ puts hello world | wc -w
2
```

## Sending output to files

`>` writes output to a file, `>>` adds to the end of one, and `<` reads a
command's input from a file:

```
mesh$ puts saved > note.txt
mesh$ puts appended >> note.txt
mesh$ wc -l < note.txt
2
```

## Defining your own commands

`func` gives a block of commands a name. List the values it needs in the
parentheses; inside the body they are ordinary variables:

```
mesh$ func greet(who) {
...   puts "hello, $who"
... }
mesh$ greet world
hello, world
```

The `... ` prompt means mesh is still reading the body — it keeps going until the
closing `}`. A short function fits on one line:

```
mesh$ func greet(who) { puts "hello, $who" }
```

Call it with exactly the values it names. Variables you set inside a function are
its own: they don't leak out, and one function never sees another's:

```
mesh$ label = outer
mesh$ func relabel() { label = inner; puts $label }
mesh$ relabel
inner
mesh$ puts $label
outer
```

> The body sees the session's variables, but its own bindings stay inside. A
> value set in a function is gone once the function returns.

`return` ends a function early — on its own it just stops, and with a number it
sets the status:

```
mesh$ func check(n) {
...   puts "checking $n"
...   return
...   puts unreachable
... }
mesh$ check 7
checking 7
```

## When something is missing

Reading a name you never set is an error, not a silent blank — and the shell
recovers and keeps going:

```
mesh$ puts $nope
mesh: nope: unbound variable
mesh$ puts still here
still here
```

---

That's everything mesh does today. New features land one at a time, and this tour
grows to match.
