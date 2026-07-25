# mesh reference

A terse lookup for everything mesh implements today. For a guided introduction,
read [`TOUR.md`](TOUR.md) first. This file lists the current surface only; it
grows as features land.

---

## Invocation

```
mesh                       # interactive when stdin and stdout are terminals
mesh script.mesh a b c     # run a script; a b c become $sh.args
mesh -c "puts hi" a b      # run a command string; a b become $sh.args
mesh -s a b                # read commands from stdin, even on a terminal
mesh -l / --login          # login shell (also sources login.mesh)
mesh --rcfile FILE         # use FILE instead of rc.mesh
mesh --norc                # skip rc.mesh
mesh --help / --version
```

With no script and no `-c`, mesh is interactive when both stdin and stdout are
terminals, and otherwise reads commands from stdin — so `echo 'ls' | mesh` works
without `-s`.

**Option parsing stops at the first operand**, as in POSIX shells, so a script's
own flags reach the script rather than mesh: `mesh deploy.mesh --login` passes
`--login` along in `$sh.args`. Use `--` to end option parsing when a script's
name itself looks like an option.

A script is read and parsed as a single unit, so a syntax error anywhere in the
file rejects the whole thing and nothing runs. A script that cannot be found
exits `127`; one that exists but cannot be read exits `126` — the same codes an
unrunnable command yields. Otherwise the exit status is the last command's, or
whatever `exit` was given.

Scripts can carry a shebang, since `#` starts a comment:

```mesh
#!/usr/bin/env mesh
puts "hi $sh.args[0]"
```

### `$sh.args` and `$sh.name`

Positional arguments are a real list — `$sh.args` — not `$1` / `$@` / `$#`:

| Read | Value |
|---|---|
| `$sh.args` | The arguments, as a list (spread with `...$sh.args`) |
| `$sh.args[0]` | The first argument; out of range is an error |
| `$sh.args:len` | How many there are |
| `$sh.name` | The script's name, or `mesh` when no script was named |

Both are read-only, and `sh` is a reserved name: it cannot be assigned, used as
a function parameter, or bound by a pattern. (Only `sh` itself is reserved — an
ordinary variable may still be called `status`, `name`, or `args`.) The rest of
the `$sh.*` surface in `DESIGN.md` is not implemented yet.

---

## Startup files

Mesh reads configuration from `$XDG_CONFIG_HOME/mesh`, falling back to
`~/.config/mesh` when `XDG_CONFIG_HOME` is unset or is not an absolute path.
Missing files are ignored. Files are evaluated in the current shell, so their
variables and functions remain available for the session.

| File | When it runs |
|---|---|
| `env.mesh` | Every invocation, including non-interactive input |
| `login.mesh` | Login shells (`-l` or `--login`), after `env.mesh` |
| `rc.mesh` | Interactive shells, after the other startup files |
| `logout.mesh` | When a login shell exits |

`--rcfile FILE` replaces `rc.mesh`, while `--norc` skips the interactive RC
file. Neither option skips `env.mesh` or the login files.

Interactive command history is saved in
`$XDG_STATE_HOME/mesh/history.sqlite3`, falling back to
`~/.local/state/mesh/history.sqlite3`. Pass `--no-save-history` (or the shorter
`--no-history` alias) to keep history in memory for that session instead.

---

## Commands

A line is a command: the first word names it, the rest are arguments. Words are
separated by spaces.

```
command arg1 arg2 …
```

An unknown command prints `command not found` and sets a failing status.

### Heredocs

`<< DELIM` feeds the following lines to a command's standard input, up to a line
that is exactly `DELIM`:

```mesh
name = world
cat << END
hello $name
END
```

An **unquoted** delimiter interpolates the body: `$…` references and the `"…"`
escape set apply. Interpolation goes through the same grammar a double-quoted
string uses, so `$m.key[0]:upper` means the same thing in both, and a malformed
reference (`${bad`) or a malformed `\u{…}` escape is the same error in both. One
difference from a string, on purpose: an escape that is not in the set at all —
`\d`, `\p` — stays as written rather than erroring, because bodies carry regexes,
JSON, and Windows paths where a stray backslash is ordinary text.

A body is **data**, so it is never tilde-expanded, globbed, or word-split.

A **quoted** delimiter makes the body raw — no interpolation, no escapes:

```mesh
cat << 'END'
hello $name and \n stay literal
END
```

The delimiter itself is never expanded; it is matched as written. A body of any
size is fine — it reaches the command as a temporary file that is unlinked as
soon as it is opened, so nothing is reachable by name while the command runs and
nothing is left behind after.

Backgrounding a command that has a heredoc (`cat << END &`) is refused rather
than run against empty input.

### Here-strings

`<<< word` feeds a single word, plus a trailing newline, to standard input:

```mesh
name = world
cat <<< "hello $name"     # → hello world
wc -l <<< hi              # → 1, thanks to that trailing newline
```

The word is an **ordinary argument word**, not a heredoc body: it interpolates,
quoting suppresses that (`<<< 'raw $name'`), and it must come to **exactly one**
word — the rule every redirection target follows. So a list is refused rather
than joined:

```mesh
xs = [a b]
cat <<< $xs               # error: a list needs `...`
cat <<< ...$xs            # error: ambiguous redirect — two words
```

`<<<` always feeds standard input, so it takes no descriptor prefix (`2<<<` is a
syntax error). Like a heredoc it travels by an unlinked temporary file, so a long
one cannot deadlock, and backgrounding it is refused for the same reason.

### Tab completion

In an interactive shell, Tab completes according to the cursor's current word:

| Position | Suggestions |
| --- | --- |
| First word, or a whitespace-separated word after `;`, `|`, `&&`, `||`, `&`, or `{` | Builtins, defined functions, and executable files found on `PATH` |
| Command argument | Files and directories; directory suggestions have a trailing `/` |
| A subcommand or flag described by `command --help` | Lazily generated suggestions, cached under `$XDG_CACHE_HOME/mesh/completions/` (or `~/.cache/mesh/completions/`) |
| A word beginning with `$` | Visible variable names |
| After `$map.` | Keys in that map; nested map paths such as `$config.user.` are followed recursively |

Suggestions are ranked using fuzzy, smart-case matching: an all-lowercase query
ignores case, while any uppercase letter makes the whole query case-sensitive.
Exact-case matches rank ahead of case-folded matches. For example,
`cd pic<Tab>` can complete to `cd Pictures/`. A single suggestion completes
immediately; ambiguous suggestions open a columnar menu, where repeated Tab or
the arrow keys move the selection and Enter accepts it. File, directory, and
enumerated argument types inferred from option declarations narrow suggestions
to suitable values. File and directory positionals inferred from `Usage:` lines,
including Vim's `[file ..]`, do the same for ordinary command arguments.
External-command help probes have null stdin, a two-second timeout, and a
one-MiB output cap.

## Builtins

| Builtin | Effect |
| --- | --- |
| `puts [arg …]` | Print the arguments separated by single spaces, then a newline. No arguments prints a blank line. |
| `cd [dir]` | Change directory. No argument goes to `$env.HOME`; `cd -` returns to the previous directory and prints it. Updates `$env.PWD` and `$env.OLDPWD`. |
| `pwd` | Print the working directory. |
| `exit [n]` | Leave the shell with status `n` (default: the last command's status; masked to 0–255). |
| `prompt [text]` | Set the interactive prompt to `text`. With no arguments, print the current prompt; `--reset` restores the status-sensitive default. |
| `prompt-hook [event] name function` | Register a named function for a prompt lifecycle event. The default event is `preprompt`. Reusing `name` within an event replaces that hook without changing its order. |

### Custom prompts and hooks

The smallest static prompt is:

```mesh
prompt "project> "
```

For a dynamic prompt, register a `preprompt` function and have it update the
prompt. This example includes the current directory:

```mesh
func refresh-prompt() {
  prompt "$(pwd)> "
}
prompt-hook cwd refresh-prompt
```

To print a context line containing the short (unqualified) hostname, working
directory, and current Git branch before a minimal `> ` input prompt:

```mesh
func prompt-context() {
  host = $(hostname -s)
  dir = $(pwd)
  branch = $(sh -c 'git branch --show-current 2>/dev/null || true')

  if $branch == "" {
    puts "$host $dir"
  } else {
    puts "$host $dir ($branch)"
  }
}

prompt-hook context prompt-context
prompt "> "
```

The hook writes the context above the editor; `prompt "> "` controls only the
input indicator. `hostname -s` requests the short hostname. Use `hostname`
instead if the platform's default hostname spelling is preferred. The `sh`
wrapper makes the branch segment empty outside a Git worktree without printing
Git's diagnostic on every prompt.

An external renderer works the same way:

```mesh
func refresh-prompt() { prompt "$(starship prompt)" }
prompt-hook renderer refresh-prompt
```

Hooks are session-local and run in registration order. Re-registering the same
event/name pair replaces it in place, making configuration safe to reload.
Remove one with `prompt-hook --remove [event] name`. `preprompt` hooks run only
for primary prompts, not multiline continuation prompts.

| Event | Function parameters | When it runs |
| --- | --- | --- |
| `preprompt` | none | Before each primary prompt is rendered. This is the default event when omitted. |
| `preexec` | `command` | Immediately before an interactive command runs. |
| `postexec` | `command, status, elapsed` | After an interactive command; `elapsed` is integer milliseconds. |
| `exit` | `status` | Before an interactive shell exits normally. |

```mesh
func command-started(cmd) { puts "running $cmd" }
func command-finished(cmd, status, elapsed) {
  puts "$cmd exited $status after ${elapsed}ms"
}
prompt-hook preexec log command-started
prompt-hook postexec log command-finished
```

This is the currently implemented prompt API. The structured `$sh.prompt` map,
styled segments, `fill`/`rule`, and the `precd`/`postcd` events described as the
eventual prompt design in `DESIGN.md` are not implemented yet.

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

A **value** used as a statement reports the status *view* of that value: an
integer is its own status (masked to 0–255), a boolean inverts (`true` is `0`),
and anything else is `0`. So `1 == 2 || puts nope` prints, and a function whose
body ends in a boolean fails when that boolean is false.

### `$sh.status` and `$sh.pipestatus`

`$sh.status` is the last command's status — the readable replacement for `$?`.
`$sh.pipestatus` breaks the same run down by stage, as a **real list**:

```mesh
sh -c 'exit 3' | sh -c 'exit 0' | sh -c 'exit 7'
puts $sh.status ...$sh.pipestatus     # 7  3 0 7
```

Both are read in **one** command there on purpose. Reading either is itself a
command, so a first `puts` would replace what a second one reports — see the
capture rule below.

Being a list rather than bash's magic `PIPESTATUS` array, it indexes, measures,
and filters like anything else:

```mesh
p = $sh.pipestatus
puts $p:len $p[0]
bad = $p:filter(func(c) { $c != 0 })
```

That capture-first habit matters: **reading either entry is itself a command**,
so it replaces what the next read would report. Take a copy when you need more
than one look — the same care `$?` needs in a POSIX shell.

A command that is not a pipeline reports one entry, so `$sh.pipestatus` is never
empty. The two always describe the **same run**: a compound's status is its
body's, so the breakdown stays the body's too.

```mesh
if true { sh -c 'exit 4' | true }
puts $sh.status ...$sh.pipestatus     # 4  4 0
```

That last point differs from bash, where an `if` or a function call resets
`PIPESTATUS` to its own single status. It holds here because pipefail is always
on: a compound's status is exactly the pipefail status of the pipeline the list
describes, so the pair cannot disagree.

Where they *do* differ from each other is a forgiven `SIGPIPE`. The pipefail rule
ignores an upstream stage killed because a later stage stopped reading, but the
stage really did die, and the list says so:

```mesh
yes | head -1 > /dev/null
puts $sh.status ...$sh.pipestatus     # 0  141 0
```

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

### The environment

`$env.KEY = value` writes the process environment, so children inherit it:

```mesh
$env.EDITOR = vim
$env.EDITOR += " -u NONE"     # += concatenates
```

An environment write is **global on purpose**, even inside a function: changing
what children inherit is the point, so it persists after the function returns.

Only strings cross the boundary — the environment is a flat `KEY=bytes` table,
so a list or map is a loud error telling you to join it first
(`$env.P = $dirs:join(":")`), and an embedded NUL is refused rather than
silently truncated. Integers and booleans cross as their text.

**Path-type names are lists**, split on the way in and `:`-joined on the way
out, which is what makes the guarded-PATH idioms work:

```mesh
$env.PATH += /opt/bin         # append one entry
$env.PATH = $env.PATH:dedup   # drop duplicates
puts $env.PATH[0]
puts $env.PATH:len
```

The set is fixed for now: `PATH`, `MANPATH`, `CDPATH`, `INFOPATH`,
`LD_LIBRARY_PATH`, and `PYTHONPATH`. Because these read as lists, `puts
$env.PATH` needs a spread or a join like any other list. Splitting is **exact** —
every empty component is kept, since `PATH=/usr/bin:` means "…and the cwd", and a
split/join round trip is byte-faithful.

Only a plain `$env.KEY` is an assignment target — any name you can read you can
also assign, including a kebab name like `$env.MY-VAR`. `$env.PATH[0] = …` and
`$env.PATH:dedup = …` describe derived values rather than places, so they are
syntax errors. `export NAME = value`, `export --list NAME`, and `unset` are not
implemented yet.

`+=` works on the raw bytes already in the environment, so a value that is not
valid UTF-8 survives being appended to. Reading such a value into mesh still
renders it lossily, so `$env.K = $env.K` — an explicit read and write back —
does not round-trip; that waits on `OsString`-backed words.

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
| `:split(SEP)` | string | Split on the literal separator into a list. |
| `:join(SEP)` | list | Fold the list into a string, `SEP` between elements. |

Path and case modifiers map over lists. Collection modifiers consume a list or
map as a whole. List results retain their type: use `...$xs:rest` in command position,
or bind them directly with `ys = $xs:rest`.

A modifier that takes an argument writes it in parentheses, comma-separated like a
value call: `$path:split(":")`, `$dirs:join(":")`. `:split(SEP)` turns a string into
a list on the literal `SEP`, and `:join(SEP)` is the complementary fold — it
stringifies each element (a nested list or map is a fail-loud error) and places
`SEP` between them. `:split` treats the separator as a **terminator, not a
separator**: a trailing run of empty fields is dropped (`"a:b:":split(":")` is
`[a b]`), interior empties are kept (`"a::b":split(":")` is `[a "" b]`), and an
empty or all-separator string is the empty list. The two are not exact inverses:
because `:split` trims a trailing empty field, `:join` then `:split` round-trips
losslessly only when the list has no empty final element (`[a ""]:join(":")` is
`"a:"`, which splits back to `[a]`).

`:split` operates on the **already-evaluated** value, so a `$(…)` capture has had
its trailing newline trimmed before `:split` runs (`$(printf "a:\n"):split(":")`
is `[a]`). Binding a split modifier to a substitution's *raw* bytes — the
`DESIGN.md` split-modifier behavior, shared with the not-yet-built `:lines` /
`:nulls` / `:raw` family — is deferred. Argument-taking modifiers currently work in expression position (an
assignment right-hand side or other value context); the command-word form
(`echo $dirs:join(":")`) is not wired up yet. Other argument-taking modifiers such
as `:get(KEY, DEFAULT)` remain unimplemented.

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

## Loops

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

`while condition { body }` tests before each pass, taking the same two condition
forms `if` does — a value's truthiness or a command's exit status. `loop { body }`
repeats until something breaks out:

```mesh
i = 0
while $i < 3 {
  puts $i
  i = $i + 1
}

while test -e /tmp/lock { sleep 1 }

loop {
  if deploy-succeeded { break }
  sleep 5
}
```

`break` exits the nearest loop and `continue` skips to its next iteration; both
work in `for`, `while`, and `loop`, and a `return` inside any of them unwinds the
whole function. A loop reports the status of its last completed pass.

Note that `<` and `>` also spell redirections. In a **condition** a spaced
comparison wins — `while $i < 3` compares — while an attached or command-position
form still redirects (`grep -q x < file`, `$cmd > log`).

## Match

`match value { pattern { body } ... }` evaluates arms from top to bottom and
uses the first match. Patterns may be exact values, globs, regular expressions,
integer ranges, alternatives separated by `|`, list binding patterns, or `_`.
Arms may have `if` guards, and an unmatched expression yields `""`.

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

- **Signature.** Comma-separated parameters carrying the four roles from
  `DESIGN.md`: a **required positional** (`name`), an **optional positional** with
  a default (`name = value`), a **flag** (a boolean switch `--name` or a valued
  `--name = default`), and a trailing **rest** (`...name`). Names must be distinct
  and cannot be `env`.

  ```
  func deploy(target, --region = us-west, --force, --tag = latest, ...hosts) {
    # target  required positional
    # region  valued flag, defaults to us-west
    # force   switch: true iff --force was passed
    # tag     valued flag, defaults to latest
    # hosts   list of the remaining positionals
  }
  deploy prod --force web1 web2       # region=us-west force=true tag=latest hosts=[web1 web2]
  deploy prod --region=eu-west --tag=v9 ...$fleet
  ```

  - **Positionals** bind left to right; an optional one (`= default`) may be
    omitted only from the right, so a required positional cannot follow an
    optional one, and `...rest` keeps its positionals required (an optional and a
    rest cannot coexist).
  - **Flags** are declared with a leading `--`. A bare `--force` is a boolean
    switch, `false` unless passed; `--tag = default` is a valued flag. At the call
    site a switch is `--force` and a valued flag is the **attached** `--tag=v9` (a
    bare `--tag` with no value is an error, never a consume-the-next-token). Flags
    may appear in any order and are not consumed as positionals; a repeated valued
    flag takes its **last** value. An argument that begins with `--` but names no
    declared flag is a loud error.
  - **`--` ends flag parsing** — everything after a bare `--` is positional/rest,
    even if it begins with `--`.
  - **Defaults** are evaluated at call time, in the call's fresh scope, only when
    the parameter is omitted.
- **Body.** May span multiple lines; the shell keeps reading until the `{ … }`
  braces balance. Interactively, the continuation prompt is `...`.
- **Scope.** Each call gets a fresh **function-local** scope: `x = 5` in a body
  binds a local that is gone on return. Reads see the innermost local scope, then
  the global scope — a function never sees its caller's locals.
- **Resolution.** A name in command position resolves as **builtin → function →
  external**. The supplied arguments must satisfy the signature (a bad count or an
  unknown/misused flag is a loud, recoverable error).
- **Arguments.** A function preserves typed values: a bare list (`f $xs`) arrives
  intact as one list-valued positional, whereas an external command still needs it
  spread (`...$xs`) or joined. A spread contributes one argument per element.
- **Result.** A function's status is its last statement's status, or `0` for an
  empty body — and when that last statement is an expression, its status is the
  view of the resulting value, so a body ending in `1 == 2` fails. `return expr`
  exits early carrying a value (viewed the same way: `return 3` is status `3`,
  masked to 0–255, like `exit`); a bare `return` carries the **result so far** —
  the last value the body produced, or the status of a command that produced
  none, or the empty string if nothing ran. Both stop the rest of the body. At
  top level `return` is a recoverable error.

- **Redirection.** A function takes `>`, `>>`, `<`, `2>`, and `2>&1` like any command
  (`f > out.txt`, `r < input`). Because a function runs inside the shell, the
  redirection applies to the shell's own descriptors for the duration of the
  call, so output from the body — including from an external command it runs —
  lands in the target, and an external it runs can read the redirected input.

- **Calling for a value.** `f(arg, key: value, ...$spread)` calls `f` and yields
  its **value** — the last expression of the body, or the value an explicit
  `return` carries — so a function can be used in an expression. Command position
  (`f arg`) is unchanged: it runs the function for its status.

  A block's last expression can be a bare value, including a **lone integer
  literal**:

  ```
  func answer() { 42 }
  x = answer()                        # 42, an integer
  ```

  Only when the whole statement *is* that literal. `42 foo` and `42 > file` are
  still commands, and a bare `-3` is the minus operator rather than one numeral —
  write `return -3` or `(-3)`. mesh has no float literals, so `3.5` is still just
  a word.

  ```
  func double(n) { return $n * 2 }
  x = double(21)                      # x is 42
  ```

  - **Arguments** are expressions, evaluated in the caller's scope. `key: value`
    binds the same parameter as the flag `--key`, so `d(prod, force: true)` and
    `d(prod, --force)` are the same call as `d prod --force`; a bare `--` ends
    option parsing; `...$list` spreads positionals and `...$map` spreads options.
  - **Channels stay independent.** The value returns through the call while the
    body's stdout streams as usual (`DESIGN.md`).
  - **Status** is the usual view of the resulting value: an integer is its own
    status, a boolean inverts (`true` is `0`), anything else is `0`. A runtime
    error in the call fails the enclosing statement instead of yielding a value.
  - **Not backgroundable.** `f() &` — the *value* spelling — is refused, and so
    is `&` on any statement that is not a command or pipeline: an expression, an
    assignment, an `if`/`match`, a loop, a definition. The value is produced in
    this shell, so a backgrounded call would have to hand its result back across
    a fork. The command spelling `f &` is a command, and is backgrounded.

A function can also be a pipeline stage or a background job — `f | sort`,
`echo x | f`, `a | f | b`, `f &`. Each runs in its own forked process, exactly as
an external command does, so pipeline stages run concurrently and a background
function returns the prompt immediately. As in every POSIX shell, whatever such a
call changes stays in that process: a `cd` or an assignment inside `f | cat` does
not outlive it. Its arguments keep their types, so `f $xs` still passes one list.
The same is true of a builtin (`puts hi | tr a-z A-Z`, `puts hi &`).

- **Lambdas.** `func(params) { body }` with no name is an expression that yields
  a **function value** — an anonymous function, using the same signature grammar
  as a declaration. Bind it, then value-call it through the variable:

  ```
  double = func(x) { $x * 2 }
  y = $double(5)                      # y is 10

  func apply(f, x) { $f($x) }
  z = apply($double, 21)              # z is 42 — a lambda is just a value
  ```

  - **The `$` is required.** A bare `double(5)` looks for a *declared* function
    called `double`, because a bare word is a literal string everywhere else.
  - **Any expression can be the callee** once it produces a function value:
    `$fs[0]()`, `$m.go()`. One that produces anything else is a loud
    `value is not callable`.
  - **Scope is a function's scope** — fresh locals, the parameters, the globals.
    A lambda does *not* close over the scope it was written in, so one inside a
    function cannot read that function's locals; the read fails loud. (mesh has
    exactly two variable scopes; see [Variables](#variables-and-assignment).)
  - **A global binding is visible to the body**, which is what lets a lambda
    recurse: `fact = func(n) { if $n == 0 { return 1 }\n return $n * $fact($n - 1) }`.
  - **No text form.** A function value is the one value that cannot be bytes, so a
    command argument, an interpolation, a spread element, and `$env.*` all refuse
    it rather than invent a rendering.
  - **Equality is identity.** A copied binding is the same function; a separately
    written lambda with the same text is a different one.

- **Both channels at once — `:capture`.** `f(…):capture` runs the call and returns
  a **record of every channel**: `.value` (the return value), `.out` and `.err`
  (its stdout and stderr), and `.status` (the exit int). Read them with ordinary
  field access.

  ```
  func build() { puts compiling
    return ok }
  r = build():capture
  puts "$r.value / $r.status"          # ok / 0
  puts "$r.out"                        # compiling
  ```

  - **It wraps execution.** `:capture` is an *invocation-level* modifier, not a
    value modifier: by the time a value modifier saw the return value the stdout
    would already have streamed away, the same reason `$(…)` is a wrapper rather
    than a postfix. So it attaches to a **call** — on anything else it is an
    error — and it takes no arguments.
  - **`.out` and `.err` are the bytes as written**, with no trailing-newline trim
    (unlike `$(…)`), so the record fixes no split policy: divide them with
    `:split` and friends as you need.
  - **Commands capture too**, and this is the one exception to "a command has no
    return value": `grep(foo):capture` asks for the record, not a value. It comes
    back **without `.value`** — reading it is a loud missing-key error — and takes
    **positional arguments only**, since a command has no signature for a
    `key: value` option or a map spread to bind to. A nonzero exit is data:
    `false():capture` reports `.status` 1 rather than failing.

    Builtins are commands here as well: `puts(x):capture` runs the builtin, not a
    program named `puts`, and `pwd():capture` does not reach `/bin/pwd`. `exit`
    still leaves the shell rather than reporting a status into a record.
  - **A background job the call starts holds the record open.** It inherits the
    capture's pipes as its own stdout and stderr, so the capture waits until it
    lets go — the same thing bash's command substitution does, and mesh's own
    `$(…)`. Redirect the child's streams (`sleep 5 > /dev/null 2> /dev/null &`)
    and the capture returns as soon as the call does.
  - **Two kinds of failure.** A statement failing *inside* the body is ordinary:
    the record is produced and the diagnostic is on `.err`. The **call** failing —
    a bad argument count, so the body never ran — fails the enclosing statement
    just as an uncaptured value call does, and the diagnostic is reported on the
    shell's stderr rather than disappearing into a record.

- **The higher-order modifiers.** `:map`, `:filter`, and `:each` each take one
  **callable** and apply it to every element of a list — written inline, or reached
  through a variable:

  ```
  xs = [1 2 3 4]
  doubled = $xs:map(func(x) { $x * 2 })          # [2 4 6 8]
  evens   = $xs:filter(func(x) { $x % 2 == 0 })  # [2 4]
  $xs:each(func(x) { puts $x })                  # for effect

  fs = ["a.txt" "b.md" "c.txt"]
  stems = $fs:filter(func(f) { $f:ext == txt }):map(func(f) { $f:stem })
  ```

  - **The call is an ordinary call.** They go through the same machinery `f(x)`
    does, so `return`, an arity mismatch, a runtime error, an escaped `break`, and
    `exit` behave exactly as they would in a written call — including loop-state
    isolation, so a `break` inside the callable does not escape into a loop the
    caller is running.
  - **`:filter` requires a boolean.** Not a truthy value: mesh's truthiness is the
    shell's, where an integer is true when it is *zero*, so a loose reading would
    make `:filter(func(x) { $x })` keep the zeros. A predicate that must say `true`
    or `false` cannot fall into that.
  - **`:each` yields the empty string**, mesh's "nothing produced" — not the list —
    so a chain cannot silently read side-effecting code as a transform.
  - **A list subject only.** On a map they are a loud error pointing at `:keys` or
    `:values`; elements keep their types, so a list element arrives as a list.

Not yet supported: a bare `:mod` reference as a callable (`$files:filter(:exec)`);
and the richer `:capture` fields `DESIGN.md` leaves open (timing, a `pipestatus`
list). `.out`/`.err` are strings rather than true byte-strings — mesh has no
byte-string type yet, so a capture that is not valid UTF-8 is a loud error.

## Not yet implemented

Most modifier arguments (beyond `:split` / `:join`), the command-word form of an
argument-taking modifier, and regex capture modifiers are not yet implemented.
Of heredocs, the command-redirection form below works, as do here-strings;
backgrounding either, and a value-producing heredoc spelling, do not.
See [`ROADMAP.md`](../ROADMAP.md).
