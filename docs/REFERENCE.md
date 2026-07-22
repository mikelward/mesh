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

Lists are bracketed, space-separated values. They preserve nesting: `$xs` in a
literal inserts a list as one nested element, while `...$xs` flattens exactly
one level. The same distinction applies when appending and when an indexed
element is a list.

```mesh
inner = [two three]
nested = [one $inner four]
flat = [one ...$inner four]
puts ...$nested[1]       # two three
flat += [five six]       # extends by two elements
```

Lists do not flatten implicitly. A nested list must be indexed or otherwise
selected before its string elements can be spread into command arguments.

Maps use comma-separated `key: value` pairs. Keys are strings and entries retain
insertion order. A later duplicate replaces the value without moving the key;
spreads and `+=` use the same right-side-wins rule. `[:]` is the empty map.

```mesh
ports = [http: 80, https: 443]
overrides = [http: 8080]
ports += $overrides
copy = [...$ports, ssh: 22]
puts $copy.http             # 8080
```

A map cannot cross the command boundary as a single argument because it has no
canonical string representation. Select a value, or explicitly spread its
`:keys` or `:values` list instead.

| Read | Meaning |
| --- | --- |
| `$name` | The value of `name`. |
| `${name}` | Same, when the following character would run into the name. |
| `$env.KEY` | The environment variable `KEY`. |
| `$xs[N]` | List element `N`; negative indexes count from the end. |
| `$map.key` | Map value for the identifier key; a missing key is an error. |
| `$map[key]` | Map value for a literal string key. |
| `${map[$key]}` | Map value for a key read from a string variable. |
| `...$xs[A..B]` | Spread a clamped, end-exclusive list slice. |
| `...$xs[A..=B]` | Spread a clamped, end-inclusive list slice. |

Reading an unset variable (or an unset `$env.KEY`) is an error; the shell
recovers and continues. An interpolated value is a single literal value — it is
never split on spaces or matched against filenames. Interpolation happens in bare
words and `"…"`, never in `'…'` or `r'…'`.

Member access and list/map indexing have the same meaning inside `"…"` as they do
outside it. A slice remains a list and needs `...` in command position; omitted
bounds and negative bounds are supported. Use braces to delimit a reference
before literal text: `${x}.txt`.
A malformed `${…}` (no closing `}`, or an invalid name inside) is a syntax error.
A `$` not followed by a name (`$5`) is a literal `$`; a literal `$` in a string
is `\$`.

## Modifiers

Recognized postfix modifiers apply from left to right after a variable, member,
or list access. They work in bare and double-quoted interpolation; braced form
puts the modifier inside the braces (`${file:stem}`). An unrecognized `:name`
is literal text, so `$host:$port` is not mistaken for a modifier chain.

| Modifier | Input | Result |
| --- | --- | --- |
| `:dir` | string or list | Parent-directory portion. |
| `:base` | string or list | Final path component. |
| `:ext` | string or list | Last extension, without the dot. |
| `:exts` | string or list | All extensions, without the first dot. |
| `:stem` | string or list | Basename without the last extension. |
| `:bare` | string or list | Basename without any extensions. |
| `:upper` / `:lower` | string or list | Change case; maps over list elements. |
| `:int` | string | Parse an integer, failing loudly on invalid input. |
| `:len` | string, list, or map | Character, element, or entry count as an integer. |
| `:first` / `:last` | list | First or last element; an empty list is an error. |
| `:rest` / `:init` | list | All but the first or last element; empty and one-element lists yield `[]` where appropriate. |
| `:dedup` | list | Remove later duplicates, preserving first occurrence order. |
| `:keys` | map | Keys as an insertion-ordered list. |
| `:values` | map | Values as an insertion-ordered list. |

Path and case modifiers map over lists. Collection modifiers consume a list or
map as a whole. List results retain their type: use `...$xs:rest` in command position,
or bind them directly with `ys = $xs:rest`. Modifier arguments—including
`:join(SEP)`, `:split(SEP)`, and `:get(KEY, DEFAULT)`—are not implemented yet.

Bare decimal literals and `true` / `false` produce typed integer and boolean
values. Arithmetic requires integers, comparisons return booleans, and strings
are never implicitly parsed as numbers. Integers and booleans have canonical
command/interpolation renderings (`42`, `true`, and `false`). Lists and maps keep
requiring an explicit spread, access, or modifier at the byte-oriented command
boundary. A whole typed value, including a list or map, passes unchanged as one
positional argument to an in-shell function.

## Operators and matching

Value expressions support integer arithmetic (`+`, `-`, `*`, `/`, `%`), unary
`-`, equality (`==`, `!=`), ordered comparisons (`<`, `<=`, `>`, `>=`),
membership (`in`), and boolean `not`, `and`, and `or`. Ordered comparisons
require two integers or two strings; arithmetic never implicitly parses a
string (use `:int` explicitly). Comparisons cannot be chained.

`~` tests a string against a bare glob or a regex; `!~` negates the result.
Globs match the whole string, while regexes search for a match unless explicitly
anchored:

```mesh
is_source = src/main.rs ~ src/*.rs
has_number = item42 ~ /\d+/
exact_number = item42 ~ /^item\d+$/
not_source = notes.txt !~ *.rs
```

A slash-delimited regex is recognized only in the right operand of `~` or `!~`.
Its body is raw except that `\/` includes a literal slash. Append `:i`, `:m`, or
`:s` for case-insensitive, multiline, or dot-matches-newline behavior:

```mesh
case_insensitive = ERROR ~ /error/:i
contains_slash = a/b ~ /a\/b/
```

Use `re(STRING)` to compile a regex for reuse or to build one from a value, and
`re(STRING, literal: true)` to quote regex metacharacters and match the supplied
text literally. A quoted string on the right of `~` is rejected rather than
silently treated as either a glob or regex.

## Conditionals

```
if command { body }
if command { body } else { body }
if command { body } else if command { body }
name = if command { value } else { value }
```

An `if` accepts either a command condition (status `0` is true) or a value
expression condition. Only the selected body runs. Bodies may span lines, and
`return` or `exit` in a selected body keeps its normal control-flow effect.

In assignment position, the selected body's final physical line supplies the
value. The current value forms are one string, a list or map literal, a whole
variable value, or a nested `if`; earlier lines in that body run for effect. A
false conditional with no `else` yields `""`. A list-pattern condition binds
only when the value has the requested shape; a mismatch selects `else` without
changing any bindings:

```mesh
if [head ...tail] = $items { puts $head ...$tail }
```

## List patterns

List patterns are shared by assignment, conditional binding, loops, and list
arms in `match`. Names bind positions, `_` discards one, and `...rest` binds the
variable-length middle (including an empty list). Fixed names after the rest
remain pinned to the end:

```mesh
[first ...middle last] = $items
for [key value] in $pairs { puts $key $value }
result = match $items {
  [head ...tail] { [$head ...$tail] }
  _ { [] }
}
```

An unconditional mismatch is a loud error and binds nothing. Conditional and
`match` mismatches simply try the other branch or arm. Duplicate and reserved
bindings are rejected before any value is committed.

## For loops

`for name in value { body }` runs the body once for each top-level list element or
expanded word. An element containing whitespace remains one value when read
through `$name`; braces may span lines. Empty lists run the body zero times.
Bounded integer ranges use the same half-open/inclusive spelling as slices, and
ordered maps use two binders and retain insertion order:

```mesh
for item in $items {
  puts $item
}
for i in 1..=3 { puts $i }
for key, value in $settings { puts "$key=$value" }
for [key value] in $pairs { puts "$key=$value" }
```

`break` exits the nearest loop and `continue` skips to its next iteration.

## Match

`match value { pattern { body } ... }` evaluates arms from top to bottom and
uses the first match. The implemented slice supports exact value patterns,
list patterns, `_`, and guards; an unmatched expression yields `""`. Its
literal/glob/regex/range pattern surface and alternation remain to be completed.

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
- **Arguments.** A function preserves typed values: a bare list (`f $xs`) arrives
  intact as one list-valued positional, whereas an external command still needs it
  spread (`...$xs`) or joined. A spread contributes one argument per element.
- **Result.** A function's status is its last command's status, or `0` for an
  empty body. `return N` exits early with status `N` (masked to 0–255, like
  `exit`); a bare `return` uses the status so far. Both stop the rest of the body.
  At top level `return` is a recoverable error.

Not yet supported: a function in a pipeline or with a redirection (or in the
background), and calling for a value (`f(arg)`) as opposed to running it.

## Not yet implemented

The remaining `match` pattern forms (glob/regex/range and alternation), modifier
arguments, regex capture modifiers, and heredocs are not yet implemented.
Function flags/optional/rest parameters and functions in pipelines are also still ahead. See
[`ROADMAP.md`](../ROADMAP.md).
