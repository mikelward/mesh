# mesh reference

A terse lookup for everything mesh implements today. For a guided introduction,
read [`TOUR.md`](TOUR.md) first. This file lists the current surface only; it
grows as features land.

---

## Commands

A line is a command: the first word names it, the rest are arguments. Words are
separated by spaces.

```
command arg1 arg2 …
```

An unknown command prints `command not found` and sets a failing status.

## Builtins

| Builtin | Effect |
| --- | --- |
| `puts [arg …]` | Print the arguments separated by single spaces, then a newline. No arguments prints a blank line. |
| `cd [dir]` | Change directory. No argument goes to `$env.HOME`; `cd -` returns to the previous directory and prints it. Updates `$env.PWD` and `$env.OLDPWD`. |
| `pwd` | Print the working directory. |
| `exit [n]` | Leave the shell with status `n` (default: the last command's status; masked to 0–255). |

## Exit status

Every command leaves a status. mesh keeps the **last** one and returns it as its
own exit code at end of input.

| Status | Meaning |
| --- | --- |
| `0` | Success. |
| `1`–`125` | Command-specific failure. |
| `126` | Found but not executable. |
| `127` | Command not found. |
| `128 + n` | Killed by signal `n`. |
| `2` | Syntax error (the shell recovers and continues). |

## Expansion

Applied to each word before the command runs.

| Form | Expands to |
| --- | --- |
| `~` / `~/…` | `$env.HOME` (at the start of a word). |
| `*` | Any run of characters in a filename. |
| `?` | Any single character. |
| `[abc]` | Any one of the listed characters. |

A pattern that matches nothing contributes **no arguments**. A word with no
pattern character is a literal and passes through unchanged. Quoting a pattern
character makes it literal.

## Quoting

| Form | Interpolates `$` | Escapes | Notes |
| --- | :---: | :---: | --- |
| bare | yes | `\x` → literal `x` | `*` `?` `[` `~` are active. |
| `"…"` | yes | yes | The everyday quoted string. |
| `'…'` | no | yes | `$` is literal. |
| `r'…'` `r"…"` | no | no | Fully literal; for backslash-heavy text. |

Escape sequences in `"…"` and `'…'`: `\n \t \r \e \\ \u{HEX}`, plus `\"` in
double quotes and `\'` in single. `"…"` also takes `\$`. An unknown escape is a
syntax error.

Adjacent quoted and bare pieces concatenate into one argument: `--flag='a b'` is
a single argument, `""` is one empty argument.

## Variables

```
name = value          # spaced form
name=value            # unspaced form
```

A name starts with a letter, then letters, digits, `_`, and interior `-` (a
hyphen must sit between two name characters). A bare `_` is not a name. Bindings
are session-global.

| Read | Meaning |
| --- | --- |
| `$name` | The value of `name`. |
| `${name}` | Same, when the following character would run into the name. |
| `$env.KEY` | The environment variable `KEY`. |
| `$xs[N]` | List element `N`; negative indexes count from the end. |
| `...$xs[A..B]` | Spread a clamped, end-exclusive list slice. |
| `...$xs[A..=B]` | Spread a clamped, end-inclusive list slice. |

Reading an unset variable (or an unset `$env.KEY`) is an error; the shell
recovers and continues. An interpolated value is a single literal value — it is
never split on spaces or matched against filenames. Interpolation happens in bare
words and `"…"`, never in `'…'` or `r'…'`.

Member access and integer indexing have the same meaning inside `"…"` as they do
outside it. A slice remains a list and needs `...` in command position; omitted
bounds and negative bounds are supported. Use braces to delimit a reference
before literal text: `${x}.txt`.
A malformed `${…}` (no closing `}`, or an invalid name inside) is a syntax error.
A `$` not followed by a name (`$5`) is a literal `$`; a literal `$` in a string
is `\$`.

## Functions

```
func name(params) { body }    # define a named function
name arg ...                  # call it; args bind to the positionals
return [ N ]                  # exit the body early (inside a function only)
```

Define a callable with `func`. Parameters are **named** — reference them as
`$name` in the body, never `$1`:

```
func greet(name) {
  puts "hi, $name"
}
greet world          # -> hi, world
```

- **Signature.** v1 accepts **required named positionals** only, separated by
  commas and/or spaces (`func pair(a, b)` or `func pair(a b)`). Names must be
  distinct and cannot be `env`. Optional/default (`x = v`), flags (`--flag`), and
  rest (`...xs`) parameters are not supported yet.
- **Body.** May span multiple lines; the shell keeps reading until the `{ … }`
  braces balance. Interactively, the continuation prompt is `...`.
- **Scope.** Each call gets a fresh **function-local** scope: `x = 5` in a body
  binds a local that is gone on return. Reads see the innermost local scope, then
  the global scope — a function never sees its caller's locals.
- **Resolution.** A name in command position resolves as **builtin → function →
  external**. The argument count must match the parameters (a mismatch is a loud,
  recoverable error).
- **Result.** A function's status is its last command's status, or `0` for an
  empty body. `return N` exits early with status `N` (masked to 0–255, like
  `exit`); a bare `return` uses the status so far. Both stop the rest of the body.
  At top level `return` is a recoverable error.

Not yet supported: a function in a pipeline or with a redirection (or in the
background), and calling for a value (`f(arg)`) as opposed to running it.

## Not yet implemented

Map values, `:` modifiers, regex literals, and heredocs. Function flags/optional/
rest parameters and functions in pipelines are also still ahead. See
[`ROADMAP.md`](../ROADMAP.md).
