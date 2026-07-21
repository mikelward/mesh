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

Wrap the name in braces when the next character would otherwise run into it — or
keep the literal part in its own quotes, since pieces sitting next to each other
join into one argument:

```
mesh$ n = 42
mesh$ puts "${n}nd"
42nd
mesh$ puts $n"nd"
42nd
```

> A value is always **one value**. If `$x` holds `*`, it prints as `*` — an
> interpolated value is never re-matched against filenames or split on spaces.

Read an environment variable through `$env`:

```
mesh$ puts $env.HOME
/home/you
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
