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

### `source`

`source FILE` runs a file's mesh code in **this** shell, so what it defines and
assigns outlives it — the reason a config file can work at all:

```mesh
source lib.mesh                # functions and variables it sets persist
```

Exactly one operand. Arguments for a sourced file would be positional parameters,
and mesh has no way to set those yet, so they are refused rather than ignored. A
missing file reports `127` and an unreadable one `126` — the statuses `mesh FILE`
itself uses, so the same failure answers the same however it is reached. A **syntax
error rejects the whole file**, so none of it runs and a broken rc cannot leave a
half-defined config.

**`return` leaves a sourced file**, and `source` reports the returned value's
status; a bare `return` carries the last status, as a bare `exit` does. `exit` still
ends the **shell**, from a sourced file included, since `source` runs in this shell
rather than a child. A script, a `-c` string, and a typed line have no caller, so
`return` there is an error naming both units that accept one. This is what makes an
early out writable:

```mesh
if $sh.interactive == false { return }   # the rest is interactive-only
```

### `$sh.origin` and `$sh.source`

Two read-only entries say **what is being evaluated**:

| Entry | Value |
| --- | --- |
| `$sh.origin` | `script`, `sourced`, `command` (`-c`), `stdin` (`-s`), or `interactive` |
| `$sh.source` | the file's path for `script` / `sourced`, empty otherwise |

They are deliberately **not** the same question as `$sh.interactive`. The two come
apart today with `-s`: `mesh -s` on a terminal reads typed commands, yet its origin
is `stdin` and `$sh.interactive` is `false`, because only the interactive loop sets
that. (The reverse pairing — a script that is also interactive — waits on `-i`,
which is not implemented yet.)

`$sh.source` reports the **innermost** file, so it changes across a `source` and
changes back afterwards, and a startup file reports itself as `sourced` — which is
how one locates a sibling. `$sh.name` (bash's `$0`) cannot do that: it never
changes.

Together they replace bash's `${BASH_SOURCE[0]}` and the
`[[ "${BASH_SOURCE[0]}" != "$0" ]]` guard, which becomes the direct
`if $sh.origin == script { … }`.

### `$sh.args` and `$sh.name`

Positional arguments are a real list — `$sh.args` — not `$1` / `$@` / `$#`:

| Read | Value |
|---|---|
| `$sh.args` | The arguments, as a list (spread with `...$sh.args`) |
| `$sh.args[0]` | The first argument; out of range is an error |
| `$sh.args:len` | How many there are |
| `$sh.name` | The script's name, or `mesh` when no script was named |

`sh` is a reserved name: it cannot be assigned, used as a function parameter, or
bound by a pattern. (Only `sh` itself is reserved — an ordinary variable may still
be called `status`, `name`, or `args`.) Everything in `$sh` is read-only except
[`$sh.options`](#shoptions), the settings map.

The rest of the read-only runtime surface:

| Read | Value |
|---|---|
| `$sh.status` | The last command's exit status — see [Exit status](#exit-status) |
| `$sh.pipestatus` | That run's per-stage statuses, as a list |
| `$sh.pid` / `$sh.ppid` | This shell's process id, and its parent's |
| `$sh.version` | The shell's version |
| `$sh.interactive` | Whether this is an interactive session |
| `$sh.stdin` / `$sh.stdout` / `$sh.stderr` | Handles for the shell's own streams |
| `$sh.jobs` | The live background jobs, as a map of records |

`$sh.interactive` answers **which loop is running**, not what fd 0 happens to
be — `mesh -s` on a terminal reads commands without being an interactive session,
and reports `false`.

For "is *this stream* a terminal", the stream handles take **`:tty`** — the
`test -t N` replacement:

```mesh
func confirm(question) {
  if $sh.stdin:tty and $sh.stderr:tty {
    …
  }
}
```

A handle has **no byte form**, so it never crosses into argv or a string:
`puts $sh.stdin` is a loud error, the same way a regex is. It exists to
be asked questions, and `:tty` is the question — asking it of anything else is
an error rather than a quiet answer about some unrelated descriptor.

**`$sh.jobs`** is an insertion-ordered map keyed by job id, so job control is a
value you can query rather than text to scrape:

```mesh
sleep 30 &
puts $sh.jobs:len              # 1 — one word for a prompt segment
puts $sh.jobs[1].state         # running
running = $sh.jobs:values:filter(func(j) { $j.state == running })
```

Each record holds `id`, `pid` (the process group leader, which is what the launch
notice reports and what a signal needs), `cmd`, `state` — `running`, `stopped`,
or `done` — and `status`, empty until the job finishes and then its exit code.

Indexing yields a **handle** rather than a copy of the record, so `$sh.jobs[2]`
can be stored and stays current — see [Job control](#job-control).

Reading it **polls but does not reap**: a finished job reports `done` with its
status and stays in the table, so a later `fg` or `wait` still finds it and hands
back that status, and the usual `[1] Done (0) …` notice still arrives at its own
time. Looking at the table never changes what the shell does.

### `$sh.options`

The shell's settings, and the one part of `$sh` you can write. Each is a boolean,
and each is **on**:

| Setting | Off means |
|---|---|
| `bold-input` | The line you are typing is drawn in the terminal's normal weight, not bold |
| `command-notify` | A command that runs longer than ten seconds finishes quietly, instead of raising a desktop notification. `notify` still works — it is called, not drawn |
| `cwd-report` | No `OSC 7` working-directory report, so a new tab or split opens wherever your terminal would have anyway |
| `osc-title` | The window and tab title is left alone, for a terminal or multiplexer that sets it itself |
| `shell-integration` | No prompt marks, so a terminal cannot tell prompt from input from output. `OSC 133`, or `OSC 633` with the command line under VS Code |

Each governs an **interactive decoration** and nothing else: turning one off never
changes what a command does or prints, only what the shell draws around it. They
are already off outside an interactive session — `mesh -s` on a terminal and every
piped run stay byte-exact whatever the settings say.

`osc-title` has one wrinkle worth knowing: mesh clears the title when it exits, so
a window is not left named after a command that finished. That clear is owed to
any title mesh actually wrote, so turning the setting off part-way through a
session still cleans up on the way out — but a session that never wrote one (off
from the start, or a terminal that takes no title) stays silent to the last byte.

Write one at a time, usually from a startup file:

```mesh
$sh.options.bold-input = false          # takes effect at once
puts $sh.options.bold-input             # false
puts ...$sh.options:keys                # bold-input command-notify cwd-report osc-title shell-integration
```

The map is strict in both directions, because a setting that is silently not
applied is worse than one that fails:

```mesh
$sh.options.bold-imput = false   # error: no `bold-imput` in this map
$sh.options.bold-input = "false" # error: a setting is `true` or `false`, got a string
$sh.options = [bold-input: false]# error: assign one setting at a time
unset $sh.options.bold-input     # error: a setting cannot be removed; assign it instead
```

The settings are the session's, not a scope's, so a function that writes one has
written it for the shell — and `global` is refused rather than accepted as a
no-op, since there is no other `$sh` for it to name.

Bracketed paste has no setting on purpose: with it off a pasted newline arrives as
Enter, so every line but the last runs before you can read it.

The hook maps in `DESIGN.md` — `$sh.prompt`, `$sh.preexec`, `$sh.complete`,
`$sh.signal` — are not implemented yet.

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

Interactive command history is saved under `$XDG_STATE_HOME` — see
[History and recall](#history-and-recall). Pass `--no-save-history` (or the
shorter `--no-history` alias) to keep history in memory for that session instead.

---

## Commands

A line is a command: the first word names it, the rest are arguments. Words are
separated by spaces.

```
command arg1 arg2 …
```

### Comments and line breaks

`#` starts a comment that runs to the end of the line. It is only a comment where
a **word begins**, so a `#` inside a word (`file#1`) or inside quotes is ordinary
text — which is what lets a script carry a shebang and a URL fragment alike:

```mesh
# a whole-line comment
puts hi          # and a trailing one
puts a#b         # a#b — not a comment
puts "# text"    # quoted, so literal
```

A `\` at the end of a line **continues** it, joining the next line as though the
break were not there. A blank or whitespace-only line is not a command: it runs
nothing and leaves `$sh.status` as it was.

```mesh
puts one \
     two          # one two
```

Line breaks inside an unclosed `{ … }`, `[ … ]`, `( … )`, or quote continue the
statement too, so a block or a list literal may span lines without a `\`.
Interactively the continuation prompt is `...`.

### Pipelines and sequencing

```
cmd | cmd          # stdout of the left becomes stdin of the right
cmd |& cmd         # stdout *and* stderr, both to the next stage
cmd && cmd         # run the right one only if the left succeeded
cmd || cmd         # run the right one only if the left failed
cmd ; cmd          # run one after the other, whatever happened
cmd &              # run it in the background as a job
```

**`|`** connects stdout to the next stage's stdin. **`|&`** connects stderr as
well — the shorthand for `2>&1 |`, per connector, so one pipeline can mix them:

```mesh
build |& tee log            # both streams into `tee`
noisy |& grep -v warning | wc -l
build 2>&1 | tee log        # the same as the first line, spelled out
```

Every stage runs in its **own process**, a builtin and a mesh function included,
so a `cd` or an assignment inside one does not outlive it (`puts hi | tr a-z A-Z`
runs the builtin in a fork). Stages run concurrently.

A pipeline's status is the **pipefail** status: the last stage to fail, or `0`
when none did — mesh has no option to turn that off. `$sh.pipestatus` breaks it
down by stage, and an upstream `SIGPIPE` is forgiven; see
[Exit status](#exit-status).

**`&&`** and **`||`** are left-associative and share one precedence level, as in
POSIX shells, so `a && b || c` is `(a && b) || c`. They branch on the exit status
of what precedes them — for a value statement, on the [status view](#exit-status)
of the value.

**`;`** and a newline mean the same thing: run the next statement regardless. A
single trailing `;` is allowed.

**`&`** backgrounds a command or a pipeline and prints the job's `[id] pid`
notice; see [Job control](#job-control). It backgrounds a **command**, so
`&` after an expression, an assignment, an `if`, a `match`, a loop, or a
definition is refused rather than forked. The one assignment that takes it is
`j = cmd &`, which binds the job handle — the `&` belongs to the job there, not
to the assignment.

### Postfix guards — `if` and `unless`

A statement can carry its own condition at the end, which is the one-line form of
wrapping it in `if`:

```mesh
puts "no branch" if $branch == ""
puts $iface $addr unless $addr == ""
return early if $n == 0
continue if $line ~ /^#/
```

The condition is a **value expression**, not a command: `puts x if test -d .git`
is not a guard at all (see below). Use a full `if` block for a command condition.

A guard may follow a command, a pipeline stage, a `return` / `break` /
`continue`, or a bare value statement. It may **not** follow an assignment —
`x = 1 if $b` is a syntax error rather than a conditional binding.

When the condition is false the statement **does not run**, and `$sh.status` is
left exactly as it was — a skipped statement reports nothing of its own:

```mesh
sh -c 'exit 3'
puts skipped if false
puts $sh.status               # 3 — the guard reported nothing
```

`if` and `unless` are only guards where the rest of the line **forms a complete
value expression**; otherwise they are ordinary arguments, so a command can still
be handed the word:

```mesh
puts x if test -d .git        # prints: x if test -d .git
puts done if $ok              # a guard — `$ok` completes the expression
puts 'if'                     # quoted, so always an argument
```

### Values as arguments

An argument is usually a word, but it can be a **value expression** wherever the
spelling could not be a word anyway — parentheses, a capture, or an attached call:

```mesh
puts (1 + 2)                  # 3
puts ($n + 3)
puts $(pwd)                   # a capture, in argv
puts before $(pwd) after      # glued to its neighbours, in argument order
puts style(x, fg: red)        # a value call
puts f()                      # a function's value
```

The result is **literal**, exactly as an interpolated variable is: never re-split,
never re-globbed, so `puts $(puts '*')` prints `*`.

Each word is evaluated **as it is reached**, left to right, so a call in one argument
is seen by the words after it and not by the ones before:

```mesh
n = first
func g() { global n = second
  return x }
puts $n g() $n                # first x second
```

Redirect targets come after all of them, which is why `f * > summary` cannot match
the `summary` its own redirection is about to create.

It carries the *value*, not its text. So a builtin that reads values gets one —
`puts f()` on a list prints one element per line — while a command that needs bytes
meets the ordinary argv rule and refuses a collection by name:

```mesh
/bin/echo f()                 # error: a list needs `...` to become command arguments
```

A value argument is a **whole** argument: text attached to it (`pre$(x)post`, `f()x`)
is a syntax error rather than silently three arguments. Quote the word to glue them —
`"pre$(x)post"`, which interpolates (see [Quoting](#quoting)).

A stage that carries a value evaluates it **in its own process**. So backgrounding
one returns at once — `puts $(sleep 10) &` spends the ten seconds in the job, not at
the prompt — and a call in a piped or backgrounded argument keeps its changes in that
stage, the same isolation the stage's own body gets:

```mesh
n = before
func change() { global n = MUTATED
  return x }
puts change() | cat           # x
puts n=$n                     # n=before
```

A command that **redirects** is the exception: it keeps evaluating its values in the
shell, and backgrounding one (`puts $(name) > out &`) is refused. The shell resolves
every stage's targets before it forks any of them, and resolves them in parallel so
`cat < fifo | cmd > fifo` cannot deadlock — so handing the words to the stage while
the targets stayed here would expand the targets *first*, against the order above.
Bind it first: `m = $(…)` then `cmd $m > out &`.

A job listing shows a value it has not evaluated as `$(…)` — `puts $(pwd) &` lists as
`puts $(…)` — since printing it would mean running it in the shell.

A following `<` or `>` is still a **redirection**, so `puts (1 + 2) > out` writes the
file. That is because an argument is parsed just above comparison precedence; a
comparison you actually want gets its own parens (`puts (1 < 2)` prints `true`), and
`&&`, `||` and `|` keep their readings for the same reason.

`[` and `..` are **not** value syntax here: in an argument they are already a glob
character class (`ls src/[ab]*`) and the literal word `1..3`. Spacing decides an
attached call from an argument — `puts(1 + 2)` calls `puts` for a value (and a command
has none), while `puts (1 + 2)` passes it one.

An unknown command prints `command not found` and sets a failing status. When
the name is a bash builtin mesh spells differently, the message names mesh's
spelling in a parenthetical, so a bash reflex has somewhere to go:

```
mesh$ read line
mesh: command not found: read (mesh spells this `gets`)
mesh$ local x = 5
mesh: command not found: local (a plain `x = 5` inside a `func` is already local)
```

A note only names something that works today, so it never trades one error for
another. It checks rather than asserts: while `gets` was still unbuilt the same
note read "which is not built yet", and it retired that caveat by itself the
moment the builtin landed.

`echo` is deliberately *not* intercepted — an external `echo` handles `-n` and
`-e`, which mesh's flag-free [`puts`](#builtins) would print as text; the note
only appears when `PATH` has no `echo` to run.

### Command substitution — `$(…)`

`$(command)` runs a command and becomes its **standard output, as one string**:

```mesh
here = $(pwd)
puts "at $(pwd) now"
puts "$(id -un)@$(hostname)"     # glue it to text by quoting the whole word
```

- **Trailing newlines are trimmed**, all of them; interior ones are kept, so
  `$(printf "a\nb\n")` is the two-line string `a\nb` and not a list. Split it
  when you want the lines (`lines = $(cat log):split("\n")`).
- **Only stdout is captured.** The command's stderr goes where the shell's does,
  so a diagnostic still reaches the terminal instead of ending up in the value.
- **The result is one literal value** — never re-split on spaces, never re-globbed
  — like every other value: `puts $(puts '*')` prints `*`.
- **A failing capture stops the statement.** The assignment or command it was part
  of does not run, and the shell recovers and reads the next one; the failing
  command's own diagnostic is the report.
- The body is ordinary mesh, so it may hold several statements
  (`$(puts a; puts b)`), a pipeline, or another capture.

It is usable wherever a value is — an assignment, a condition, a command argument
(see [Values as arguments](#values-as-arguments)), and inside `"…"` (see
[Quoting](#quoting)) — but **not** inside `'…'`, `r'…'`, or a heredoc body.

### Redirection

The bash operators, and they mean what they do there: `>`, `>>`, `<`, `2>`,
`2>>`, `2>&1`, `>&2`, `<&0`, and `&> file` / `>& file` for both streams.

**Any descriptor**, not just the standard three:

```mesh
sh -c 'read line <&3; echo $line' 3< input.txt
sh -c 'echo detail >&3' 3> trace.log
```

A duplication reaches a pipe as readily as a file, so a stage can hand a
descriptor either end of one:

```mesh
sh -c 'echo aside >&3' 3>&1 | cat      # fd 3 is the pipe onward
puts fed | sh -c 'cat <&3' 3<&0        # fd 3 is the pipe feeding this stage
```

**`n>&-` closes a descriptor** rather than pointing it anywhere, and it is
restored when the redirection's scope ends:

```mesh
noisy 2>&-                             # discard stderr by taking it away
func f() { helper 3>&- }               # the helper does not inherit fd 3
f 3> trace.log
```

Redirections apply **in source order**, so a duplication copies where a
descriptor points *at that moment* — `> out 2>&1` sends both to the file, while
`2>&1 > out` copies stdout's original destination onto stderr and only then
moves stdout. For the same reason, duplicating a descriptor nothing has opened
yet is an error (`EBADF`), even if a later redirection would have opened it — and
so is copying one that an earlier `n>&-` has closed.

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
JSON, and Windows paths where a stray backslash is ordinary text. A **capture** is
not substituted in a body — `$(cmd)` stays as written — though it is in a string.

A body is **data**, so it is never tilde-expanded, globbed, or word-split.

A **quoted** delimiter makes the body raw — no interpolation, no escapes:

```mesh
cat << 'END'
hello $name and \n stay literal
END
```

The delimiter itself is never expanded; it is matched as written — and since a
capture in one would mean running a command to decide where the body ends, a
delimiter containing one (`<<"$(x)"`) is a syntax error. A body of any
size is fine — it reaches the command as a temporary file that is unlinked as
soon as it is opened, so nothing is reachable by name while the command runs and
nothing is left behind after.

Backgrounding a command that has a heredoc (`cat << END &`) works: the body
reaches the stage as memory, and the stage writes the temporary in its own
process.

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
one cannot deadlock, and either can be backgrounded — the body reaches the stage
as memory, and the stage writes the temporary in its own process.

## The interactive session

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

### Line editing

The interactive line editor takes **emacs keys** — the set your fingers already
have:

| Key | Effect |
| --- | --- |
| Ctrl-A / Ctrl-E | Start / end of the line |
| Ctrl-B / Ctrl-F, arrows | Back / forward one character |
| Alt-B / Alt-F | Back / forward one word |
| Ctrl-P / Ctrl-N, up / down | Previous / next history entry |
| Ctrl-R | Search history backwards |
| Ctrl-W / Alt-D | Cut the word before / after the cursor |
| Ctrl-U / Ctrl-K | Cut to the start / end of the line |
| Ctrl-Y | Paste what was cut |
| Ctrl-Z / Ctrl-G | Undo / redo *while editing*; Ctrl-Z stops a **running** command instead |
| Ctrl-L | Clear the screen |
| Tab | Complete — see [Tab completion](#tab-completion) |
| Alt-. | Insert the previous command's last argument; press again to walk back |
| Ctrl-C | Abandon the line, buffered block and all, and re-prompt |
| Ctrl-D | Exit, on an empty line |

Ctrl-C **abandons** rather than runs: nothing executes and `$sh.status` is left
as it was. A line that opens a block or a quote keeps reading at the `...`
continuation prompt until it balances, and Ctrl-C drops the whole thing.

The line is drawn in bold unless `$sh.options.bold-input` is off, and bracketed
paste is always on, so a pasted multi-line block arrives as text to read rather
than as commands already run.

### History and recall

Commands are saved to `$XDG_STATE_HOME/mesh/history.sqlite3` (falling back to
`~/.local/state/mesh/history.sqlite3`), owner-readable only, and a multi-line
command is stored and recalled as the **one logical command** it was typed as.
Recall reaches back through earlier sessions but not sideways into a peer session
that started later, so two shells open at once do not interleave each other's
lines. `--no-save-history` keeps a session's history in memory instead.

Three **word designators** expand against the previous command line before it is
parsed:

| Form | Expands to |
| --- | --- |
| `!^` | Its first argument |
| `!$` | Its last argument |
| `!*` | All of its arguments, separated by spaces |

```
mesh$ mkdir -p build/out
mesh$ cd !$
cd build/out
```

The expanded line is echoed, as above, so what ran is on the screen. Expansion is
quote-safe: a `!` inside `"…"`, `'…'`, or `r'…'`, after a backslash, or not
followed by one of the three designators stays literal — which is what leaves
`!!`, `!string`, and `!n` free to be added later; they are not implemented. A
line with no arguments makes `!*` empty but `!^` / `!$` an error, and with no
previous command at all, all three are an error. Alt-. inserts the same last
argument by hand, and repeating it walks back through earlier commands.

## Builtins

| Builtin | Effect |
| --- | --- |
| `help [name …]` | List every builtin with its usage, then the shapes a line takes, each with a one-line summary. With names, explain each one instead — a builtin's entry is exactly what `name --help` prints, and a keyword's shows its syntax. Every reserved word and every operator answers, asked for as you would type it (`help unless`, `help '+='`); where several share a row, that row explains the family, so `help else` answers with `if`. A name that is neither is an error: an external command's help is its own, so ask it with `name --help`. |
| `puts [arg …]` | Render each argument and print them separated by single spaces, then a newline. No arguments prints a blank line. Rendering is per value: a scalar as itself, a **list** as its elements joined by newlines, a **map** as `key: value` lines. A value with no byte form — a job or stream handle, a function, a pattern — is a loud error rather than a guess, and so is a collection nested inside one. Unlike argv, `puts` sees the real value, so `puts $xs` needs no `...`; a *written* argument keeps its own text, so `puts 007` prints `007`. It takes no flags. |
| `print [arg …]` | The same as `puts` with **no trailing newline**, for partial lines. No arguments prints nothing. |
| `gets [var]` | Read one line from stdin, strip its trailing newline, and bind it to `var`. **At end of input the status is `1` and `var` is left unchanged**, which is what terminates `while gets line { … }`. An empty line is a successful read of `""` — a blank line mid-file must not end a loop — so only a zero-byte read ends it, and a final line with no trailing newline is still a line. A line that is not valid UTF-8 is **refused** rather than repaired — status `2`, and `var` is left alone — following the capture rather than `$env`'s lossy read; status `2` is also what an I/O error reports, so `1` means end of input and nothing else. Interactively, **Ctrl-C cancels a read** — status `130`, and `var` keeps whatever it held, since a cancelled read has read nothing. It reads a byte at a time, so the bytes after the line reach whatever runs next rather than being swallowed by a buffer. With no `var` it consumes the line and reports only whether there was one. The value form `gets()`, which yields the line into an expression, is not wired up yet. |
| `style(text, fg: …, bg: …, bold: …)` | A [styled value](#styled-values) — text plus display attributes. A **value call**, parens attached, because a command position yields a status. Colors are the sixteen ANSI names: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `grey` (or `gray`, or `bright-black`), and `bright-` forms of the rest. |
| `link(text, url)` | A [styled value](#styled-values) carrying an `OSC 8` hyperlink, so `text` is clickable. The url needs a **scheme** (`https://…`, `file://host/path`) and anything RFC 3986 forbids raw is percent-encoded, a space included; over 2083 encoded bytes is refused, since past a terminal's own limit the whole sequence — link text included — is dropped. |
| `glob(pattern)` · `files(dir = ".")` · `dirs(dir = ".")` | The paths a pattern matches, and a directory's immediate files or subdirectories — a **list**, since these are [value calls](#the-glob-family) rather than commands. |
| `cd [dir]` | Change directory. No argument goes to `$env.HOME`; `cd -` returns to the previous directory and prints it. Updates `$env.PWD` and `$env.OLDPWD`. `CDPATH` and autocd are not implemented, so a bare directory name is a command, not a `cd`. |
| `pwd` | Print the working directory. |
| `clip [text …]` | Copy to the terminal's clipboard with `OSC 52`, so it works over `ssh`. Arguments join with a space; with none, stdin is read (`puts hi \| clip`). The bytes are copied as given, a trailing newline included. Goes to the terminal, not stdout, so a redirect cannot swallow it. Whether the copy lands is up to the terminal — xterm needs `allowWindowOps`, tmux `set-clipboard on` — and there is no reply, so success means "asked". |
| `notify [text …]` | Raise a desktop notification through the terminal with `OSC 9`. Arguments or stdin, like `clip`. A command that runs for more than ten seconds notifies on its own, with its outcome and duration — `$sh.options.command-notify = false` turns that off. Inside tmux the sequence is wrapped for passthrough, which tmux forwards only with `allow-passthrough` set. Support is uneven and unreportable — iTerm2, WezTerm, Ghostty, kitty and ConEmu raise these; xterm and Alacritty discard them; tmux needs `allow-passthrough` — so success means "asked". |
| `exit [n]` | Leave the shell with status `n` (default: the last command's status; masked to 0–255). |
| `prompt [text]` | Set the interactive prompt to `text`. With no arguments, print the current prompt; `--reset` restores the status-sensitive default, and `prompt -- --reset` sets that literal text. |
| `prompt-hook [event] name function` | Register a named function for a prompt lifecycle event. The default event is `preprompt`. Reusing `name` within an event replaces that hook without changing its order. |
| `jobs` | List the jobs, one `[id] State command` per line. |
| `fg [job]` | Resume a job in the foreground and wait for it. No argument takes the most recent job. |
| `bg [job]` | Resume a stopped job in the background. No argument takes the most recent job. |
| `wait job` | Wait for a job to finish and report its status — see [Job control](#job-control). |
| `kill [-signal] job\|pid …` | Signal a job's process group, or a pid. Default `TERM`. |
| `disown [-h] [-a \| -r] [job …]` | Stop tracking a job — see [Job control](#job-control). |
| `command [--] name [arg …]` | Run the **program** `name`, past the builtin or function that name would otherwise reach — which is what makes `func ls() { command ls --color=auto }` safe to write, and what reaches `/usr/bin/env` when a function of that name is in the way. Only the words in front of the program are `command`'s own: `command ls --help` asks `ls` for its help, and `--` ends `command`'s options so the word after it is the program however it reads. `--help` is the only option it has, so any other flag-looking word in front of the program is a usage error (status `2`) rather than a program name — `command -v` / `-V` are held for the unbuilt half, and `command -- -v` runs a program called `-v`. The operand is the program with nothing peeled off it, so `command command x` looks for a program called `command`. A builtin's name finds no program, and says so; with no operand at all the status is `2`. |
| `source file` | Run a file's mesh code in this shell — see [`source`](#source). |

### Flags and `--`

Every command mesh owns — builtin or function — reads flags by one rule.

**`--help` prints the generated help**, whether it was written or arrived in a
variable: `x = --help; puts $x` prints the usage, and so does `f $x`. mesh's
expansion safety is about never *splitting* or *globbing* a value; it was never a
promise to launder a word that is a flag.

**`--` ends the options and is consumed**, so it is how you mean a flag-looking word
as data:

```mesh
puts -- --help                # --help
puts -- -- x                  # -- x     — only the first one goes
prompt -- --reset             # sets the prompt to the text `--reset`
kill -- -9 %1                 # looks for a job named `-9`, not signal 9
```

Which command consumes it depends on which has options to end. `puts`, `print`, `gets`,
`clip`, `notify`, `cd`, `source` and `help` have none of their own, so the terminator
is simply removed. `kill`, `disown`, `prompt`, `prompt-hook` and `command` do, so each
ends its own options at `--` — only they know where those stop.

`command` is also where the `--help` rule stops applying, because the arguments
after the program name are not mesh's to read:

```mesh
command grep -- -x file       # `--` reaches grep; it looks for the line `-x`
command grep --help           # grep's own help, not mesh's
command --help                # mesh's help for `command` itself
command -v ls                 # error: `command` has no `-v`; status 2
command -- -v                 # runs a program called `-v`
```

### Job control

`fg`, `bg`, `wait` and `kill` all take the same job references:

| Reference | Names |
|---|---|
| `%2` | Job 2 |
| `%%` / `%+` | The **current** job |
| `%-` | The **previous** job |
| `%prefix` | The most recent job whose command starts with `prefix` |
| `$j` / `$sh.jobs[2]` | The job a handle names — see below |

`fg`, `bg` and `wait` only ever take a job, so a **bare id** is unambiguous
there: `fg 2` is `fg %2`. **`kill` is the exception** — a bare number is a *pid*,
as it is in every shell, so `kill 2` signals process 2 rather than job 2. That
distinction is the reason a handle has no byte form.

A job becomes **current** when it is registered, when it stops, and when `bg`
starts it again — the events that make it the one you most likely mean. The job
it displaces becomes the previous one. When the current job leaves the table the
previous is promoted, and the job behind that fills the `%-` it vacated, so both
sigils keep meaning something without being repointed by hand. `fg` and `bg` take
the current job when given none.

**A job handle is a reference you can hold.** `j = cmd &` binds the job itself
rather than the status of launching it, so `$j.pid` is mesh's replacement for
bash's `$!`:

```mesh
j = make -j8 &
puts $j.pid                     # the process group leader
puts $j.state                   # running … and later, done
wait $j                         # the handle is a job reference
```

Reading a member resolves the handle against the **live** table, so `$j.state`
moves on with the job instead of freezing as it was when bound. `$sh.jobs[2]` is
a handle in the same sense, and both reach `fg`, `bg`, `wait` and `kill`.

A handle has **no byte form** — `puts $j` is a loud error, the same way a stream
handle or a regex is. That is not tidiness: it is what makes `kill $j` a job and
`kill 49001` a pid, with nothing left to guess between them.

Waiting for a job takes it out of the table, so a handle can outlive what it
names; `$j.status` then reports that the job is gone rather than a stale record.
The status is `wait`'s own result — read it from `$sh.status`.

**`kill`** takes any of these references, or a bare pid:

```mesh
kill $j                         # the job's whole process group, with TERM
kill -9 %+                      # -9, -KILL, -SIGKILL, -s KILL, -n 9 all work
kill 49001                      # a pid — just that process
```

A **job** signals its whole process group, since a pipeline is several processes
and signalling only the leader leaves the rest running. A **pid** signals only
that process. Each target is signalled independently, so one bad name does not
stop the rest.

`kill -0` sends nothing and reports whether the target exists and could be
signalled — the liveness probe. Unlike `fg`, `bg` and `wait`, `kill` works from
a pipeline stage (`kill $j | cat`): it neither waits for a job nor takes the
terminal, and signalling needs permission rather than parenthood.

An **id wins over a prefix**, so `%1` is job 1 rather than a command that happens
to start with `1`. A bare `%` names no job, and neither does a prefix nothing
matches; both say so. `%?string`, the *substring* match, is not implemented —
`DESIGN.md` keeps the spelling and defers the behavior, and mesh refuses it by
name rather than reporting a job that does not exist. Note that `?` is a glob
character first, so a `%?…` reference has to be quoted to reach a job builtin at
all.

**`wait`** reports a job's exit status without giving it the terminal, which is
what lets a script hand work to the background and collect the result:

```mesh
sh -c 'sleep 1; echo done' &
wait 1                          # blocks; $sh.status is the job's
```

Waiting is how backgrounded work survives the shell: jobs still running when the
shell exits are hung up, so without a `wait` whatever they had left to do is
lost.

A job that has **already finished** answers from the status its record carries,
so waiting after the fact reports the same thing as waiting through it. The job
then leaves the table, and the usual `[1] Done (0) …` notice is not repeated for
a status you have already been given.

**Ctrl-C abandons the wait, not the job**: it reports `130` and leaves the job
running and listed. That applies to an **interactive** shell, which is the only
one that ignores SIGINT on its own account and so the only one where a wait would
otherwise be inescapable. A non-interactive shell keeps whatever disposition it
was given — an inherited ignore, from a parent that meant interrupts not to take
effect, still holds through a wait.

A job that is **stopped** does not finish on its own, so waiting for one reports
its `128 + signal` stop status straight away rather than blocking on it; `bg`
or `fg` is what a stopped job wants.

A bare `wait` waits for **every job in the table**, oldest first, and several
operands (`wait 1 2`) wait for each in turn. Either way the status is **the last
job to fail, or 0 if none did** — the pipefail rule applied to the other place
where several things finish at once. bash returns 0 from a bare `wait`
regardless, which discards the one thing the caller waited to find out.

Both forms treat a stopped job the way naming it does: its stop status counts
towards the answer, and it is left in the table rather than blocked on. One
Ctrl-C ends the whole wait at 130, however many jobs were still to come; they
keep running and keep their places.

`disown` gives a job up:

```mesh
make -j8 &
disown              # not this shell's job any more
```

A disowned job leaves the table, so `jobs` no longer lists it, `fg` / `bg` /
`wait` can no longer name it, a bare `wait` does not wait for it, and it is not
hung up when the shell exits. It stays the shell's *child* — it is still reaped,
so it cannot become a zombie — but nothing is left that reports its status.

`disown -h` is the narrower form: the job stays in the table in every way, and
only the hangup is skipped. Use `-a` for every job or `-r` for the running ones;
with no operand it is the current job. `-a` and `-r` name different sets, so
giving both is an error rather than a silent choice between them. Neither takes
a job.

"Running" for `-r` means what it says: not stopped, and not already finished
either. A job that has exited but whose status nobody has collected yet is still
in the table with a status to hand back, and `-r` leaves it there.

**A job that is stopped when it is given up does not outlive the shell**, under
either form, and `disown` says so:

```
mesh$ disown
mesh: disown: [1] is stopped and will not survive the shell; `bg %1` first
```

The exemption cannot cover it. Once the shell that parented it goes, its process
group is orphaned, and the kernel — not mesh — sends SIGHUP then SIGCONT to an
orphaned group containing a stopped process. The only way to prevent that would
be to continue the group as the shell leaves, which means resuming a job you
stopped on purpose, without asking, at the one moment you can no longer object.
bash and zsh both decline to do that and warn instead; so does mesh. `bg` it
first and the disown holds.

A `disown -h` job stays in the table, so it can stop *after* it was given up.
The exemption is void just the same, and the shell says so on the way out:

```
mesh: exit: [1] is stopped, so it will not survive the shell
```

A job given up by a plain `disown` gets no such warning, because it left the
table — a stop after that point is not something the shell can see.

| | listed by `jobs` | named by `fg` / `wait` | waited by bare `wait` | hung up at exit |
|---|---|---|---|---|
| ordinary job | yes | yes | yes | yes |
| `disown -h` | yes | yes | yes | **no** |
| `disown` | **no** | **no** | **no** | **no** |

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
| `jobdone` | `id, command, status` | Once per background job the shell finds finished, alongside its `[N] Done` notice. |
| `exit` | `status` | Before an interactive shell exits normally. |

```mesh
func command-started(cmd) { puts "running $cmd" }
func command-finished(cmd, status, elapsed) {
  puts "$cmd exited $status after ${elapsed}ms"
}
prompt-hook preexec log command-started
prompt-hook postexec log command-finished
```

`jobdone` runs where the `[N] Done` notice is printed — at the prompt after the
job ended, not the instant it ended. A job you `wait` for does not reach it: the
status went to the caller, which is what the hook is there to tell you.

On the way out, every job the shell knows about is reported **before** the
`exit` hook, so a handler that tears down what `jobdone` was writing to can rely
on having seen them all. The exception is a completion the `exit` handler itself
brings about — by running `jobs`, or by taking long enough that a job finishes
while it does. That one is reported after the handler has run, because there is
no earlier moment to report it in: the alternative is not reporting it at all,
and a notice without its hook is the one thing `jobdone` is meant to rule out.

```mesh
func job-finished(id, cmd, status) {
  if $status != 0 {
    puts "job $id failed ($status): $cmd"
  }
}
prompt-hook jobdone report job-finished
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

Patterns work the same in a **value** position as in an argument: `xs = *`,
`for f in *.rs { … }` and `puts (*)` all expand. A lone `*` is the one spelling
that has to be told apart from multiplication, and spacing does not do it — the
rule is where it sits. As an operand it is the pattern (`xs = *`); after a value
it is the operator (`4 * 3`). A relative path is a value too: `./x`, `../x`, `.`
and `.*` are all words rather than syntax errors, while `1..3` and `..3` stay
ranges.

### Dotfiles

A `*`, `?` or `[…]` never matches the leading `.` of a name, but a **literal**
`.` in the pattern does — so `*` skips the dotfiles and `.*` finds them. The rule
applies per path component, so `sub/.*` finds the dotfiles in `sub`. A
directory's own `.` and `..` are never matches.

```mesh
puts *            # a.txt b.txt sub
puts .*           # .config .hidden      — not `.` or `..`
puts sub/.*       # sub/.innerdot
```

### Glob qualifiers

A `(…)` **attached** to a pattern narrows what it matched. The options are
comma-separated and ANDed — every one has to hold:

| Qualifier | Keeps |
| --- | --- |
| `f` `d` `l` `p` `s` `b` `c` | Files, directories, symlinks, fifos, sockets, block and char devices — `find -type` letters. |
| `type: file` | The same seven, spelled out: `file` `dir` `symlink` `fifo` `socket` `block` `char`. |
| `type: file\|dir` | Either type. `\|` is how the type dimension says "or". |
| `x`, `exec: true` | Anything with an execute bit. `exec: false` inverts it. |
| `empty: true` | A zero-length file, or a directory with no entries. |

```mesh
for f in *(f) { process $f }   # plain files, no `if $f:type == dir { continue }`
puts *(d)                      # just the directories
puts *(f, x)                   # executable files
puts **/*(f, empty: true)      # every empty file below here
```

A type is read from the link itself, so `l` means the symlink; `exec` and
`empty` follow it, since a symlink's own mode is `0777` and would otherwise make
every link "executable". `*(l, x)` is then "links to something runnable".

The `(` opens qualifiers only when it abuts a word that carries an unquoted
pattern character. Everything else keeps its reading: `style(x, fg: red)` and
`pwd()` are calls, `"*"(d)` is a call on a string, and `ls * (1)` is a pattern
plus a separate value argument.

The `size > 1M` / `age < 1d` comparisons are specified in `DESIGN.md` but **not
implemented yet**. The same type filtering is also reachable as a call — see
[the `glob` family](#the-glob-family) below — and as the `:files` / `:dirs`
[modifiers](#modifiers).

### The `glob` family

The same expansion, asked for as a [value call](#functions), so it answers with a
**list** of paths instead of replacing a word:

| Call | Yields |
| --- | --- |
| `glob(pattern)` | The paths `pattern` matches. |
| `files(dir = ".")` | The files directly in `dir`. |
| `dirs(dir = ".")` | The subdirectories of `dir`. |

```
for d in dirs() { puts "$d/" }     # walk the working directory
for f in files(src) { … }          # or a named one
found = glob("*.log")              # bind the list and reuse it
ls ...$found                       # a stored list still spreads into argv
```

`glob` is what expands a pattern the program **built**, since a value never
re-globs on its own: `ls $p` passes the string `*.jpg` verbatim, and `glob($p)`
hands over its matches. There is no lazy glob value — the call expands when it
runs, and deferring it is an ordinary thunk (`later = func() { glob("*.txt") }`,
re-globbing on each `$later()`).

`files` and `dirs` are that call preset to `DIR/*` plus the type filter the name
already carries — the same filter the `:files` / `:dirs`
[modifiers](#modifiers) name. Entries come back sorted, hidden entries skipped,
and prefixed by the directory: `files(src)` yields `src/deep.txt`, while the
default `.` adds no prefix.

Everything else is the word rule above. **Nothing matched is the empty list**,
which covers a missing or unreadable directory, so programmatic use never throws;
a malformed pattern is the one error, since a `glob()` call — unlike a word,
which can still be a filename — has nothing else to mean. The argument is a plain
string and so takes no tilde expansion, for the same reason `ls $p` takes none;
write `~/…` as a bare argument (`dirs(~/src)`), which the *word* rules expand
before the call sees it, or `glob("$env.HOME/…")`. A path that starts with `.`
needs no quotes — `dirs(.)` and `files(../src)` read as the paths they look like,
`..` staying a range only where no `/` is attached to it.

These are calls, never commands: a bare `dirs` is a command-not-found, and the
names are reserved so `func dirs(…)` is refused.

## Quoting

| Form | Interpolates `$` | Escapes | Notes |
| --- | :---: | :---: | --- |
| bare | yes | `\x` → literal `x` | `*` `?` `[` `~` are active. |
| `"…"` | yes | yes | The everyday quoted string. |
| `'…'` | no | yes | `$` is literal. |
| `r'…'` `r"…"` | no | no | Fully literal; for backslash-heavy text. |

Escape sequences in `"…"` and `'…'`: `\n \t \r \e \a \b \f \v \\ \u{HEX}`,
plus `\"` in double quotes and `\'` in single. `"…"` also takes `\$`. An unknown
escape is a syntax error — including `\0`, which is not in the set because a NUL
cannot cross `execve` or the environment.

Adjacent quoted and bare pieces concatenate into one argument: `--flag='a b'` is
a single argument, `""` is one empty argument.

A `"…"` string interpolates a **capture** as well as a variable, which is how a value
is glued to text:

```mesh
puts "at $(pwd) now"
puts "$(id -un)@$(hostname)"
func host-info() { style("$(hostname)", fg: red) }
```

What the capture produced crosses **whole** — quoted, so it is never re-split and
never re-globbed, however many spaces or `*`s it contains. A capture that fails is an
error there as everywhere, so the statement stops rather than substituting nothing,
and a syntax error inside it is reported when the line is parsed. `'…'` and `r"…"` are
literal, and `\$(` keeps the text in a double-quoted string.

## Variables

```
name = value          # spaced form
name=value            # unspaced form
```

A name starts with a letter, then letters, digits, `_`, and interior `-` (a
hyphen must sit between two name characters). A bare `_` is not a name. At top
level, bindings are session-global.

### Scope: `global` and `unset`

There are exactly two scopes: the **session-global** one, and a fresh
**function-local** one per call. Inside a function, assignment binds a **local by
default** — the deliberate inverse of bash — so writing the session scope is
something you say:

```mesh
count = 0
func tick() {
  n = 1                        # a new local, gone on return
  global count = $count + 1    # the session-global
}
```

`global` takes `=`, `+=`, and destructuring (`global [p q] = $pair`); every name
a pattern binds lands in the one scope named. It governs an assignment only, so
`global f` is a syntax error rather than a call.

`global name = …` writes the global; it does **not** retarget a local already
shadowing that name, so a function that has both keeps reading its own:

```mesh
x = out
func f() {
  x = in
  global x = set
  puts $x                      # in  — the local still shadows
}
f
puts $x                        # set
```

**`unset name`** removes a binding from the **current** scope. This is not the
same as `x = ""`: that is *bound to the empty string*, while unset is *unbound*,
and only the second makes a read fail. Those are the two states that stand in for
a missing null.

```mesh
x = ''
puts $x                        # prints an empty line
unset x
puts $x                        # error: x: unbound variable
```

It takes several names at once, and unsetting a name bound in no visible scope is
a loud, recoverable error — the same fail-loud rule reads follow.

Inside a function, plain `unset` drops the local only. If that local was shadowing
a global, the global becomes visible again; if there was no local, the global is
left alone rather than reached through — matching the rule that makes assignment
local. **`global unset name`** is how to remove a session-global from inside a
function, symmetric with `global name = value`.

A target may instead name a **place inside** a binding — `unset $m.key`,
`unset $xs[0]` — which removes that entry and leaves the binding itself alone. It
is the mirror of [member assignment](#member-assignment) and follows the same
rules: the path may mix members and indices, a negative index counts from the end,
nothing missing along it is forgiven, and the removal is local-by-default with
`global unset $m.key` to reach the outer binding. Removing from a list **shifts**
what follows, so `unset $xs[0]` drops the first element rather than leaving a hole.
Names and places may be mixed in one statement (`unset p $m.k q`), and `$env` /
`$sh` are no more places here than they are on the assignment side.

```mesh
x = outer
func f() { unset x }
f
puts $x                        # outer — untouched
```

Blocks (`if`, `for`, `while`, `loop`) open no scope, so a name bound inside one is
an ordinary binding of the enclosing scope and `unset` reaches it.

Neither word is reserved — only `env` and `sh` are — so a variable may still be
called `global` or `unset`; they lead a statement only where one can follow.
Unsetting `env` or `sh` is refused.

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

### Member assignment

`$m.key = value` and `$xs[0] = value` write **into** a bound collection rather
than rebinding the name, along a path that may mix members and indices:

```
config = [name: mesh, tags: [a b], nested: [depth: 1]]
$config.name = shell            # replace a value
$config.owner = me              # add a key
$config.tags[1] = c             # write a list element
$config.tags[-1] = d            # negative counts from the end, as a read does
$config.nested.depth += 1       # combine, using the same rules as `n += …`
$config["a:b"] = x              # a quoted key, colon and all

func rename(new) { global $config.name = $new }   # write the outer binding
```

A **place** is a member or an index. A modifier is not (`$xs:dedup = …` is a
syntax error, as it already was), nor is a slice — `$xs[0..2] = …` names a copy of
a run of elements, and a length-changing assignment has no defined meaning yet.
`$env` and `$sh` keep their own handling: see [The environment](#the-environment).

Two rules are worth stating outright, because both are choices:

- **`unset $m.key`** is the matching removal — see
  [Scope](#scope-global-and-unset).
- **Local by default**, like every other assignment. Inside a function
  `$m.key = v` shadows an outer `m` rather than reaching through to it — the same
  thing `m += …` and `m = …` already do. **`global $m.key = v`** writes *into* the
  session-global binding instead, so a function can modify a caller's collection
  without rebinding the whole thing; it names the global scope, so it writes there
  even where a local shadows the name, and an unbound global is an error since
  there is nothing to copy inward.
- **Nothing along the path is created.** A missing intermediate key is a loud
  error, not an empty map conjured to hold the write, so `$m.typo.key = v` says so
  instead of quietly building a structure nobody asked for. The one exception is a
  **new key at the end of a map**, which is how a key is added. A list is only ever
  written in place: an out-of-range index is an error, since there is no value to
  fill a gap with, and `+=` on an absent map key has nothing to combine with.

### The environment

`$env.KEY = value` writes the process environment, so children inherit it, and
**`export KEY = value` is the same write** in the spelling every shell user
already has:

```mesh
$env.EDITOR = vim
$env.EDITOR += " -u NONE"     # += concatenates
export EDITOR = vim           # identical to the first line
```

An environment write is **global on purpose**, even inside a function: changing
what children inherit is the point, so it persists after the function returns.

Bare `export NAME` — bash's "mark this existing variable exported" — does **not**
work, because mesh keeps shell bindings and the environment in separate
namespaces, so there is nothing to mark. To copy a binding across, name it:

```mesh
editor = vim
export EDITOR = $editor
```

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
`LD_LIBRARY_PATH`, and `PYTHONPATH`. (`export --list NAME`, which would opt an
arbitrary name in, is not implemented.) Because these read as lists, `$env.PATH`
needs a spread or a join to reach an external command like any other list
(`puts $env.PATH` prints one entry per line). Splitting is **exact** —
every empty component is kept, since `PATH=/usr/bin:` means "…and the cwd", and a
split/join round trip is byte-faithful.

Only a plain `$env.KEY` is an assignment target *here* — any name you can read you
can also assign, including a kebab name like `$env.MY-VAR`. `$env.PATH[0] = …` and
`$env.PATH:dedup = …` describe derived values rather than places, so they are
syntax errors. (An ordinary map or list **does** take an indexed write — see
[Member assignment](#member-assignment) — but `$env` holds bytes rather than typed
values, so it keeps the narrower rule.) Of the other spellings, only `export --list NAME` is still
unimplemented.

`+=` works on the raw bytes already in the environment, so a value that is not
valid UTF-8 survives being appended to. Reading such a value into mesh still
renders it lossily, so `$env.K = $env.K` — an explicit read and write back —
does not round-trip; that waits on `OsString`-backed words.

Member access and list/map indexing have the same meaning inside `"…"` as they do
outside it. A slice remains a list and needs `...` to reach an external command;
omitted
bounds and negative bounds are supported. Use braces to delimit a reference
before literal text: `${x}.txt`.
A malformed `${…}` (no closing `}`, or an invalid name inside) is a syntax error.
A `$` not followed by a name (`$5`) is a literal `$`; a literal `$` in a string
is `\$`.

## Modifiers

Recognized postfix modifiers apply from left to right after a variable, member,
or list access. They work in bare and double-quoted interpolation; braced form
puts the modifier inside the braces (`${file:stem}`). An unrecognized `:name`
is literal text, so `$host:$port` is not mistaken for a modifier chain. A name
mesh **reserves** for a modifier it has not built yet — `:sort`, `:words`,
`:lines`, `:replace`, and the rest of the `DESIGN.md` set — is a loud
`not implemented yet` in a value context rather than a silent no-op.

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
| `:exists` | path or list | Does the path exist? (`test -e`; a broken symlink does not.) |
| `:type` | path or list | The `find -type` word — `file`, `dir`, `link`, `fifo`, `socket`, `block`, `char`. |
| `:read` / `:write` | path or list | Is the path readable / writable by this process, as its effective user? (`test -r` / `-w`.) |
| `:files` / `:f` | list or path | Keep the plain files (`test -f`). |
| `:dirs` / `:d` | list or path | Keep the directories (`test -d`). |
| `:links` / `:l` | list or path | Keep the symlinks (`test -L`) — the one file modifier that does *not* dereference. |
| `:exec` / `:x` | list or path | Keep the executables (`test -x`). |
| `:keys` | map | Keys as an insertion-ordered list. |
| `:values` | map | Values as an insertion-ordered list. |
| `:repr` | any value with a literal form | The value written as the mesh source you would have typed for it, as a string. |
| `:tty` | stream handle | Is that stream a terminal? The `test -t N` replacement — see [`$sh.args` and `$sh.name`](#shargs-and-shname). |
| `:split(SEP)` | string | Split on the literal separator into a list. |
| `:join(SEP)` | list | Fold the list into a string, `SEP` between elements. |
| `:get(KEY, DEFAULT)` | map or list | **Total** access — `DEFAULT` when the key or index is absent. |
| `:stripstart(P)` / `:stripend(S)` | string or list | Drop the affix once if it is there; a no-op otherwise. |
| `:trimstart` / `:trimend` | string or list | Peel whitespace from that end, repeatedly. |
| `:trimstart(CHARS)` / `:trimend(CHARS)` | string or list | Peel any of `CHARS` from that end, repeatedly. |
| `:replaceall(OLD, NEW)` | string or list | Replace every match. |
| `:replacestart(OLD, NEW)` / `:replaceend(OLD, NEW)` | string or list | Replace a **leading** / **trailing** match only. |
| `:map(F)` / `:filter(F)` / `:each(F)` | list | Apply a callable per element — see [Functions](#functions). |
| `:i` `:m` `:s` `:x` | regex | Pattern flags — see [Operators and matching](#operators-and-matching). |
| `:capture` | a **call** | Every channel of the call as a record — see [Functions](#functions). |

Path and case modifiers map over lists. Collection modifiers consume a list or
map as a whole. The **file tests** (`:exists`, `:type`, `:read`, `:write`)
map over a list like the path modifiers, while the **file filters** (`:files`,
`:dirs`, `:links`, `:exec`) keep a list's matching elements and drop the rest —
a subset, not a transform — and chain for AND, so `$paths:f:x` is the executable
plain files. Applied to a single path a filter is instead the boolean its `test`
operator gives, which is what lets `:filter` apply one element at a time
(`$paths:filter(func(p) { $p:exec })` and `$paths:exec` agree).

Every file modifier **dereferences symlinks**, as `test` does, so a live link is
`:files` when its target is and a broken link does not `:exist`. The exceptions
are the two that exist to ask about the link itself: `:links`, and `:type`, which
reports `link`. `:type` is the only file modifier that **errors** on a path that
is not there — the others answer `false`, but a missing file has no type word.
Note that a searchable directory carries the execute bit, so `:exec` alone keeps
directories; `:f:x` is the executable-files idiom. List results retain their type: use `...$xs:rest` in command position,
or bind them directly with `ys = $xs:rest`.

`:repr` is the odd one out: rather than transforming a value it **writes one
down**, as the mesh source you would have typed to get it back.

```mesh
m = [k: 1, 'a b': [2, true]]
puts $m:repr                  # ['k': 1, 'a b': [2, true]]
x = 42
s = "42"
puts $x:repr $s:repr          # 42 '42'
```

The contract is round-trip rather than pretty-printing, and that is what the
quoting is for: `42` and `"42"` are different values, so a string is always
quoted even when it would read as a bare word, and the empty map keeps its own
`[:]` spelling so it cannot come back as the empty list `[]`. It is the natural
way to see what you actually *have*, where [`puts`](#builtins) shows you how a
collection **reads** — one element or `key: value` per line, with `42` and `'42'`
printing alike.

A value with **no** literal form is a loud error rather than an approximation
that would read back as something else: a stream handle, a function, a glob
(writing the pattern back would re-glob it), and for now a regex, whose flags
ride on `:` modifiers that are not implemented yet.

```mesh
puts $sh.stdin:repr           # mesh: :repr: a stream handle has no literal form
```

The guarantee is worth stating plainly: **whatever `:repr` returns, reading it
back gives the same value.** Anything that would not is an error instead.

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

`:get(KEY, DEFAULT)` is the **total** accessor, where `$m.key` and `$xs[i]` fail
loud: it answers `DEFAULT` when the key or index is absent, which is what makes
`$env:get(EDITOR, vim)` the mesh spelling of `${EDITOR:-vim}`. A map takes a
string key and a list an integer index, negative counting from the end. Note the
one difference from bash: a key bound to `""` is **present**, so it wins over the
default, where `${EMPTY:-x}` substitutes. Asking a map for an integer — or a list
for a name — is a loud error rather than a silent default: a key of the wrong
*type* is a mistake in the program, not an absence in the data. A bare `$env` is
the whole environment as a map, which is what gives `:get` an ordinary map to
work on; `$env.NAME` stays the strict read that errors when unset. Note that
`puts $env` is refused: the path-type names are lists, and a collection nested in
a collection has no rendering — the same answer `puts` gives for any such map.
Read it with `$env:keys`, `$env:get(NAME, …)`, or `$env.NAME`.

```mesh
editor = $env:get(EDITOR, vim)
puts $env:get(MESH_DEBUG, false)
xs = [a b c]
puts $xs:get(9, "-")            # -
```

The **affix** family drops a known prefix or suffix **once** — the everyday
"strip a known extension" reach, with no regex escaping and no interior-match
surprise (a global `:replaceall(".tar.gz", "")` would also rewrite
`a.tar.gz.bak`). The **trim** pair peels repeatedly instead: whitespace by
default, or any of a given character set.

```mesh
puts "report.tar.gz":stripend(".tar.gz")   # report
puts "report.tar.gz":stripend(".zip")      # report.tar.gz — no match, no change
puts "///a//":trimend("/")                 # ///a
```

The **replace** family takes a **match slot** first: a *string* `OLD` matches
verbatim, with metacharacters literal, and a *regex* `OLD` (a bare `/…/` here, or
an `re()` value) matches as a pattern. This is the same no-silent-coercion rule
`~` and `:int` follow — a string full of `.` and `*` never quietly becomes a
pattern. `:replaceall` is global, as its name says; `:replacestart` and
`:replaceend` act only on a match at that edge, so a pattern that happens to match
in the middle leaves the string alone. The edge is the **subject's**, not a
line's, so `:m` does not move it. Where several matches reach the edge the
the match is the **regex engine's**, found in the whole subject with the edge
requirement compiled into the pattern. At the trailing edge that comes out as the
longest match, since the engine tries start positions left to right and every
candidate has to finish at the end: `"abc":replaceend(re("ab|bc"), "X")` is `aX`
either way round. At the leading edge every candidate starts in the same place,
so regex's ordinary first-alternative rule decides —
`"abc":replacestart(re("a|ab"), "X")` is `Xbc` and `re("ab|a")` gives `Xc`. Write
the alternative you want first, as you would in any regex.

The subject is never sliced to look for a longer match, because a look-around
assertion reads the bytes around it: `re(r"a\b")` has no match in `"ab"`, and
cutting the subject down to `"a"` would invent the word boundary it asks for.

```mesh
puts "a.b.c":replaceall(".", "-")          # a-b-c   — the string is literal
puts "a.b.c":replaceall(/./, "-")          # -----   — the regex is a pattern
puts "one.js":replaceend(/\.js/, ".ts")    # one.ts
```

Only a **bare** `/…/` reads as a pattern, and its delimiters have to be written as
delimiters. A quoted `"/a/"` is the three-character text it looks like, so
`"x/a/y":replaceall("/a/", "/b/")` is `x/b/y`; so is an escaped delimiter, since
`\/a/` and `/a\/` are that same text. An escape *inside* is untouched —
`/a\/b/` is a regex whose pattern contains an escaped slash. The same rule
decides a `~` right-hand side and a `match` arm.

An **empty** pattern is refused whichever way it is written — `""`, `//`, or
`re("")` — since it matches at every position, so a global replace would
interleave the replacement through the subject and an anchored one would insert
it at an edge.

The replacement is **literal text**: `$1`-style capture backreferences are not
implemented, since their spelling is still provisional in `DESIGN.md`.

Every modifier here is a value modifier, so each **maps element-wise over a
list** — `$paths:stripend(".js")` rewrites each path — except `:get`, which
consumes the collection as a whole.

`:split` operates on the **already-evaluated** value, so a `$(…)` capture has had
its trailing newline trimmed before `:split` runs (`$(printf "a:\n"):split(":")`
is `[a]`). Binding a split modifier to a substitution's *raw* bytes — the
`DESIGN.md` split-modifier behavior, shared with the not-yet-built `:lines` /
`:nulls` / `:raw` family — is deferred. Argument-taking modifiers work in expression position (an assignment right-hand
side or other value context) and in command-argument position
(`echo $dirs:join(":")`). Not yet: the **spread** of one at a command boundary —
`puts ...$x:split(":")` is a syntax error, so bind it first (`xs = $x:split(":")`,
then `puts ...$xs`). `:has(VALUE)` also remains unimplemented.

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

`not` is a **reserved word**. A leading one always negates a value, so
`if not $b { … }` and `while not $b { … }` are conditions rather than a command of
that name, matching the postfix guard (`puts x if not $b`) and the assignment
(`x = not $b`) that already read it so. It never names a command, however the line
continues:

```
not foo              # negates the string "foo" — not a command
not true foo         # a syntax error, not an invocation
not true | cat       # a syntax error: a value cannot be a pipeline stage
```

The escape hatches are the ones any reserved word has — a path or a quoted word:

```mesh
./not foo            # runs the program `not`
"not" foo            # the same, spelled as data
```

`not` as *data* is untouched, since only the command-word position is reserved:
`puts not` prints it and `x = "not"` stores it. A run of `not`s folds to its
**parity** — `not not not $x` is `not $x`, and any even run is the `not not $x` that
coerces truthiness to a bool without inverting it.

The same **whole-statement** rule governs a **word operand** on its own, which is how
a variable names a command. A word is a *value* only when the value is the whole
statement; anything continuing the line makes it the command line it looks like, and
a redirect after the *completed* operand is a redirection rather than a comparison:

```mesh
editor = vim
$editor              # a value — the string "vim"
$editor notes.txt    # runs vim on notes.txt
$editor > log        # runs vim, stdout redirected
$editor ...$files    # runs vim on each of them
$editor | cat        # a pipeline: a value cannot be a pipeline stage
$editor || puts oops # a connector: runs vim, branches on its exit status
$editor &            # backgrounds the command

p = "src/main.rs"
$p:base out          # runs `main.rs` with the argument `out`
$p:base > log        # runs it, redirected — the command word ends after `:base`
```

A **command word** is a word plus its *attached* argument-free `:modifier` suffixes,
and nothing wider. Spacing decides, exactly as it does for an argument — `puts $x :len`
prints `:len` rather than a length — so a spaced postfix after the command word is the
next argument:

```mesh
e = echo
$e:len            # a value: the length of the word "echo"
$e :len           # runs echo with the argument ":len"
```

An operand that cannot be a command word keeps the comparison reading:

```mesh
x = 1
$x + 1 > 1        # a comparison: arithmetic cannot be a command word
ns = [7 8]
$ns[0 + 0] > 0    # a comparison: a computed index cannot be one either
$ns[0] > out.txt  # a command, redirected: a literal index is part of the word
```

Which reading a line gets is settled by *parsing* it and looking at the **leading
operand** — the leftmost thing the expression hangs off. That is the only part of an
expression a command line can also be, so it is the part that decides, and the shape
of the rest does not enter into it:

```mesh
ls / extra        # a command: `/` between words is not a division
exit -1           # a command: `-` after a word is not a subtraction
ls ..             # a command: `..` after a word is not a range
1 == 2            # a value: a numeral leads it, and numerals name no command
1..3              # a value: the range, for the same reason
```

A value loses to a following `&&` / `||` / `&` only when it *is* a **command word** —
an unbroken run of text led by a variable — since that is the reading the shell idiom
is asking for:

```mesh
cmd = nosuchcmd
$cmd || puts failed        # runs the command, branches on its status
p = /x/nosuchcmd
$p:base || puts failed     # the same idiom, with a suffix the command word keeps
${cmd}.exe || puts failed  # `.exe` is literal text, which is what the braces are for
${cmd}[0] || puts failed   # `[0]` globs on it rather than indexing
${cmd}-1 || puts failed    # `-1` is part of the name, not a subtraction
```

**Whitespace** is what separates the two readings, not the shape of the expression.
`${cmd}-1` and `$a - 1` are the same subtraction of the same variable; one is a program
name and the other is arithmetic, and only the spacing says which. So spacing the same
text apart gives the value reading, which reports its own status:

```mesh
a = 5
b = 6
$a - 1 || puts smaller     # arithmetic — the spaces make it an expression
$a == $b || puts ne        # a comparison likewise
$x ~ /b/ && puts matched   # and a match
42 &                       # a refused backgrounded expression, not a program `42`
```

One shape is ruled out whatever the spacing: a `(`, since command position has no call
syntax. That is why `$x:split("-") || puts x` keeps its value reading even though
nothing in it is spaced.

A negation is the other kind of operand: `not` is reserved, so it has no command
reading at all, and `&&` / `||` join the value statement rather than making a command
of it — `not $b && puts x` negates and then branches.

In a condition a spaced comparison still compares, modifiers and all — including
modifiers that take **arguments**, whose parentheses reach past where a command word
could ever end — so `if $xs:len > 5 { … }` and `if $x:split("b"):len > 1 { … }` both
ask about a length. That holds for a **numeral** on the left too, signed or not:
`if 1 < 2`, `if -1 < 0`, and `if 1:repr:len > 0` all compare. (Statement position is
unchanged: `42 > file` still redirects.) And a *derived* value is not a place:
`$xs:dedup = 9` is a syntax error saying so, never an attempt to run a command named
by the value.

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
Its body is raw except that `\/` includes a literal slash. Flags are postfix
modifiers on the pattern, each with a short and a long spelling:

| Flag | Long form | Effect |
| --- | --- | --- |
| `:i` | `:ignorecase` | Match without regard to case. |
| `:m` | `:multiline` | `^` and `$` match at line boundaries. |
| `:s` | `:dotall` | `.` matches a newline. |
| `:x` | `:extended` | Ignore whitespace in the pattern, so it can be spaced out. |

```mesh
case_insensitive = ERROR ~ /error/:i
also_insensitive = ERROR ~ /error/:ignorecase
contains_slash = a/b ~ /a\/b/
```

They chain like any other modifier (`/error/:i:m`), and they apply to a compiled
`re(…)` value as readily as to a literal. A `/…/` literal is one **word**, so it
cannot contain a space — which is why `:x`, whose whole point is a spaced-out
pattern, pairs with `re(…)`:

```mesh
spaced = re("\\d{3} - \\d{4}"):x
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
  [head ...tail] => [$head ...$tail]
  _              => []
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

## `fork` — a subshell

`fork { body }` runs the body in a forked child, so everything it changes about
the process is its own:

```mesh
fork { cd build
  $env.CC = clang
  make }        # the shell's cwd, environment, and bindings are untouched after
```

Isolation is **explicit** in mesh — a plain `func` persists a `cd` on purpose, so
that lifting lines into a helper does not silently change where they run. `fork`
is the opt-in for the other behavior, and the only grade that costs a process.

What crosses back out is **bytes**: the child shares the shell's stdout, so what
it prints appears, but no value returns and no binding survives. Its exit status
becomes the block's, so it composes normally:

```mesh
fork { false } || puts the-subshell-failed
```

An **`exit` inside a subshell ends the child, not the shell** — which is the
property that makes it worth having:

```mesh
fork { exit 3 }
puts "still here, status $sh.status"    # still here, status 3
```

`fork` is contextual, not reserved: it leads a statement only when a `{` follows,
so a command of that name is still reachable as `fork`, `fork --flag`, or
`fork somewhere`.

Control flow cannot cross the boundary either, which falls out of the isolation
rather than being enforced: a `break` inside a subshell ends the *child*, and the
loop it looks like it is in — the parent's — carries on.

Not yet, and a syntax error rather than a quiet surprise if you write one:
piping a subshell (`fork { … } | cat`) or redirecting one (`fork { … } > log`);
backgrounding one (`fork { … } &` is refused, since it has no job-table entry to
resume from); and the `fork func name() { … }` form `DESIGN.md` also specifies.

## Match

`match value { pattern => body ... }` evaluates arms from top to bottom and
uses the first match. Patterns may be exact values, globs, regular expressions,
integer ranges, alternatives separated by `|`, list binding patterns, or `_`.
Arms may have `if` guards, and an unmatched expression yields `""`.

The `=>` is required, and arms are separated by a newline or `;` — never a comma.
An arm's body is either a **value** or a `{ }` **block**, and the arrow decides
how a word reads: `=> markdown` is the string `"markdown"`, while
`=> { tail -f $file }` is a block whose commands run.

One caveat, inherited from how any block yields a value: a block that is a *single
bare word* is read as a scalar when the `match` is in **expression** position, so
`x = match 1 { 1 => { echo } }` binds `"echo"` rather than running it, while the
same arm in statement position runs `echo`. Give the block more than one word — or
use a value arm — when you mean one or the other unambiguously.

```mesh
kind = match $file {
  *.md | *.markdown => markdown
  *.txt             => text
  _                 => other
}
match $sig {
  int  => { cleanup; exit 130 }
  term => { cleanup; exit 143 }
}
```

## Functions

```
func name(params) { body }    # define a named function
name arg ...                  # call it; args bind to the positionals
return [ N ]                  # exit the body early (or a sourced file — see `source`)
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
  the global scope — a function never sees its caller's locals. To write the
  session scope on purpose, say `global` (see below).
- **Resolution.** A name in command position resolves as **builtin → function →
  external**. The supplied arguments must satisfy the signature (a bad count or an
  unknown/misused flag is a loud, recoverable error). A function cannot take a
  builtin's name, and [`command name …`](#builtins) skips the chain entirely to run
  the program — which is what lets `func ls() { command ls --color=auto }` wrap the
  command it names instead of calling itself.
- **Arguments.** A function preserves typed values: a bare list (`f $xs`) arrives
  intact as one list-valued positional, whereas an external command still needs it
  spread (`...$xs`) or joined. A spread contributes one argument per element.
- **Result.** A function's status is its last statement's status, or `0` for an
  empty body — and when that last statement is an expression, its status is the
  view of the resulting value, so a body ending in `1 == 2` fails. `return expr`
  exits early carrying a value (viewed the same way: `return 3` is status `3`,
  masked to 0–255, like `exit`); a bare `return` carries the **result so far** —
  the last value the body produced, or the status of a command that produced
  none, or the empty string if nothing ran. Both stop the rest of the body. At a
  top level `return` is a recoverable error, **except** in a sourced file, which it
  leaves the way it leaves a function body — see [`source`](#source).

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
    exactly two variable scopes; see [Variables](#variables).)
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
  - **A bare `:mod` reference is itself a callable**, so a predicate or mapper can
    take the modifier directly rather than a lambda that only forwards to it:
    `$files:filter(:exec)` is `$files:filter(func(f) { $f:exec })`, and
    `$paths:map(:stem)` is `$paths:map(func(p) { $p:stem })`. Only the
    argument-free **value** modifiers can be referenced — `:join` needs a separator
    and `:map` a callable, and `:capture` wraps an invocation rather than a value,
    so none of them is a one-argument function to denote, and naming one is a loud
    error. `:capture` is refused where the reference is *written*, since rejecting
    it at the call would mean the invocation it was meant to capture had already
    run. A reference **call** starts a value, so it can open a condition or a
    statement (`if :exists(f) { … }`) as well as sit on an assignment's right-hand
    side; a command word beginning with `:` is unchanged, since only the attached
    `:name(…)` form is claimed. The value is a function like any other: no text
    form, and identity rather than name is what equality means, so `:stem` written
    twice is two values. A leading `:` is a reference only in **expression**
    position; a command word starting with `:` is the literal text it always was.
    Nothing rescues the transform-as-predicate footgun the shorter spelling makes
    easy to write — `$paths:filter(:dir)` is the same loud "predicate must return a
    boolean" that the lambda form gives, not a quiet keep-all. A reference is applied through
    the same value-sensitive path the postfix form uses, so *which* modifier
    `:name` is still depends on what it meets: `$regexes:map(:i)` sets
    case-insensitivity, and `:x` is the extended-syntax flag on a regex where it is
    the executable filter on a path. Only a **bare** name is a reference —
    `:'stem'` and `:\stem` compose to the same text but are not references — and
    calling one through a variable takes ordinary call arguments, so a spread
    expands before the single-argument count is checked.

Not yet supported: the richer `:capture` fields `DESIGN.md` leaves open (timing, a `pipestatus`
list). `.out`/`.err` are strings rather than true byte-strings — mesh has no
byte-string type yet, so a capture that is not valid UTF-8 is a loud error.

## Styled values

`style` returns a **string carrying display attributes** — not a new type. Only a
*renderer* reads the attributes, which today means `puts` and `print` writing to a
color-capable terminal:

```mesh
danger = style("danger", fg: red, bold: true)
puts $danger                  # colored at a terminal, plain text anywhere else
```

Everywhere bytes are wanted it behaves exactly as its text, which is what makes it
safe to style a value you also compute with:

```mesh
puts "level: $danger"         # level: danger        — interpolation sees the text
puts $danger:len              # 6                    — and so does a modifier,
p = style("a:b", fg: red)     #                        including one taking
parts = $p:split(":")         # ['a', 'b']             arguments
if $danger == danger { … }    # true                 — equality is by text
if $danger ~ re(dang) { … }   # true                 — so is matching
x = $danger
x += "!"                      # 'danger!'            — a plain string; attributes
                              #                        are rendering-only
/bin/echo $danger             # danger               — argv carries bytes
```

`style` and `link` are reserved names, like `re` and the
[`glob` family](#the-glob-family): a `func style(…)` would be reachable as a
command but never as a value call, so it is refused rather than shipped as a
function whose meaning depends on how it is called.

Attributes are **added**, not replaced, so a segment can be emphasized without
knowing its color — and a call naming no attribute is simply a string:

```mesh
loud = style($danger, bold: true)   # still red, now bold
style(x)                            # the plain string `x`
```

### Hyperlinks

`link(text, url)` builds the same kind of value, with an `OSC 8` hyperlink as the
attribute — so `text` is clickable in a terminal that supports it and is ordinary
text everywhere else:

```mesh
u = link("the docs", "https://example.com/guide")
puts $u                             # clickable "the docs"
puts $u:repr                        # 'the docs' — still just its text
```

The two compose **in either order**, since each sets only the attributes it names:

```mesh
link(style(x, fg: blue), $url)      # the same blue clickable `x`
style(link(x, $url), fg: blue)
```

The url needs a **scheme**. A terminal needs an absolute URI, so a bare path is a
link that silently does nothing; mesh says so rather than guessing at `file://`,
which would need a hostname to be right over `ssh`.

Anything RFC 3986 does not allow raw is **percent-encoded**, so a space in a path
becomes `%20` — `file://host/My File.txt` is an ordinary path and an invalid URI, and
a terminal may reject the whole link over it. That is the same rule that stops an
`ESC` inside a url from ending the sequence early. Delimiters are kept, so `?`, `&`,
`=`, `#` and an existing `%20` all survive as written. Over 2083 encoded bytes is
refused, because past a terminal's own limit the whole sequence is dropped and the
link *text* goes with it.

### When decoration drops itself

Neither escape is written unless the command's own stdout is a terminal — so a pipe,
a redirect, a `$(…)` capture and a `:capture` record all get plain text. The decision
is **per command**, so `puts $danger` is colored while `puts $danger > log` is not.

Past that the two differ:

| | Color | Link |
| --- | --- | --- |
| stdout is not a terminal | dropped | dropped |
| `NO_COLOR` set to anything non-empty | dropped | **kept** — a link is not color, and dropping it loses the url |
| `TERM=dumb` | dropped | dropped |
| `TERM` not known to parse an `OSC` (`linux`, `ansi`, `sun`) | kept — SGR is universal | dropped, or the terminal would print the url |

There is no setting to manage and no capability to probe on either path, because
dropping the attributes is always available.

Both are **value calls**, so the parens are attached to the name — a bare `style …`
would run it in command position and yield a status. They can be written inline as an
argument, or returned from a function, which is how a prompt segment is built:

```mesh
puts style(x, fg: red)                              # inline
func dir-info() { style(tilde-pwd(), fg: blue) }    # a prompt segment
```

## Not yet implemented

The argument-taking modifiers that work today are `:split`, `:join`, `:get`, the
affix family (`:stripstart`, `:stripend`, `:trimstart`, `:trimend`), the replace
family (`:replaceall`, `:replacestart`, `:replaceend`), and `:map` / `:filter` /
`:each`; the rest of the `DESIGN.md` set (`:match`, `:has`, `:words`, `:lines`,
the first-only `:replace`, the time and sort families) is not implemented, and
neither are the regex capture modifiers or a capture backreference in a
replacement. One of them **spread** at a command boundary
(`puts ...$x:split(":")`) is also not implemented — bind it first, which is the
same gap a spread value call hits (`ls ...glob($p)` → `found = glob($p)`,
`ls ...$found`).
`gets` reads a line as a command; the value form `gets()` does not.
Of heredocs, the command-redirection form documented under
Commands works, as do here-strings; a value-producing heredoc spelling does not.
The history designators `!!`, `!string`, and `!n` are not implemented — only
`!^`, `!$`, and `!*` are. `style` covers the sixteen ANSI colors only.
`command` runs a program; the `-v` / `-V` half of it — "what would this name
run?" — is not implemented, and is likely to arrive as a value rather than a flag.
See [`ROADMAP.md`](../ROADMAP.md).
