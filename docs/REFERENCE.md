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

Reading an unset variable (or an unset `$env.KEY`) is an error; the shell
recovers and continues. An interpolated value is a single literal value — it is
never split on spaces or matched against filenames. Interpolation happens in bare
words and `"…"`, never in `'…'` or `r'…'`.

Inside `"…"`, a `.` after an unbraced `$name` is literal (`"$x.txt"` is the value
of `x` followed by `.txt`); use `${…}` for anything more. A malformed `${…}` (no
closing `}`, or an invalid name inside) is a syntax error. A `$` not followed by
a name (`$5`) is a literal `$`; a literal `$` in a string is `\$`.

## Sequencing

Join commands on one line; the connector decides whether the next one runs from
the previous command's status.

| Form | Runs the next command |
| --- | --- |
| `a ; b` | Always. |
| `a && b` | Only if `a` succeeded (status `0`). |
| `a \|\| b` | Only if `a` failed (status ≠ `0`). |

The line's status is that of the last command actually run.

## Pipelines

`a | b` connects `a`'s standard output to `b`'s standard input; any number of
stages chain with `|`. A pipeline's status is `0` only when every stage
succeeded; otherwise it reports a failing stage. A stage cut short because a
later stage closed the pipe (SIGPIPE) is not counted as a failure.

## Redirection

Redirections attach to a command; each target is a single file.

| Form | Effect |
| --- | --- |
| `> file` | Send standard output to `file`, replacing it. |
| `>> file` | Append standard output to `file`. |
| `< file` | Read standard input from `file`. |

A target that expands to zero or several words is an error. Descriptor forms
(`2>`, `&>`, `>&2`) are not supported yet.

## Functions

```
func name(param …) { body }
```

Define a named command. Parameters are required named positionals — a call must
pass exactly that many arguments — and bind as local variables in the body. The
body is one or more lines; write it across lines (mesh reads until the closing
`}`) or on a single line. Parameter names must be distinct and cannot be the
reserved name `env`.

| Form | Effect |
| --- | --- |
| `return` | End the function now, keeping the status so far. |
| `return n` | End the function now with status `n` (masked to 0–255). |

A function's result is its last command's status, or `0` for an empty body or a
bare `return` before anything ran. A variable set in the body is **local**: it
shadows a global of the same name and is dropped when the function returns. Reads
reach the body's own locals and the session globals — never another function's
locals. Functions cannot yet appear in a pipeline or take a redirection.

## Not yet implemented

List and map values, `:` modifiers, regex literals, and heredocs. See
[`ROADMAP.md`](../ROADMAP.md).
