# mesh reference

A terse lookup for everything mesh implements today. For a guided introduction,
read [`TOUR.md`](TOUR.md) first. This file lists the current surface only; it
grows as features land.

---

## Invocation

```text
mesh                       # interactive when stdin and stdout are terminals
mesh script.mesh a b c     # run a script; a b c become $sh.args
mesh -c "puts hi" a b      # run a command string; a b become $sh.args
mesh -s a b                # read commands from stdin, even on a terminal
mesh -i                    # interactive session whatever stdin is
mesh -n script.mesh        # check for syntax errors, run nothing
mesh -l / --login          # login shell (also sources login.mesh)
mesh --rcfile FILE         # use FILE instead of rc.mesh
mesh --norc                # skip rc.mesh
mesh --help / --version
```

With no script and no `-c`, mesh is interactive when both stdin and stdout are
terminals, and otherwise reads commands from stdin — so `echo 'ls' | mesh` works
without `-s`.

`-i` decides the session's **character**, not its input: `$sh.interactive` is
true and `rc.mesh` is sourced, while the commands still come from wherever the
invocation says. It does not conjure a terminal — without one there is nothing to
run a line editor on, so `mesh -i` off a terminal reads stdin the way it always
did, just as an interactive session. That is what makes the half of a config
behind `return unless $sh.interactive` testable without a pty.

**`-n`** (`--no-execute`) parses the input, reports the first thing wrong with
it, and runs nothing — the `sh -n` of other shells. Silent on success, `2` on a
syntax error, so it composes:

```mesh
mesh -n generated.mesh && source generated.mesh
```

It checks interpolated **heredoc bodies** too. The parser treats a body as data
delimited by a line, so a file whose heredoc holds a malformed `${bad` parses
clean and is rejected on the way in — after everything before it has run. The
check looks inside; it does not resolve anything, so an unbound variable in a
body is a runtime failure rather than a syntax error.

It **skips the startup files**. `env.mesh` is ordinary mesh code, so sourcing it
to check an unrelated file would run arbitrary commands, which is the one thing
the flag promises not to do — and the check then answers for the named input
alone rather than for the reader's environment.

**Option parsing stops at the first operand**, as in POSIX shells, so a script's
own flags reach the script rather than mesh: `mesh deploy.mesh --login` passes
`--login` along in `$sh.args`. Use `--` to end option parsing when a script's
name itself looks like an option. `-s` is not an operand — it says where the
commands come from and parsing continues, so `mesh -s -n` checks stdin. Its
operands are `$sh.args`, since the input is already settled and there is no
script to name.

A script is read and parsed as a single unit, so a syntax error anywhere in the
file rejects the whole thing and nothing runs. A **parse** error says where:

```text
mesh: deploy.mesh:42:5: syntax error: unclosed `(`
```

The name is the file for a script or a sourced file, and the origin word
(`stdin`, `command`, `interactive`) for the inputs that have none. The line and
column are 1-based, and the column counts characters rather than bytes.

An **unclosed delimiter** is reported at the delimiter, not at the end of the
file: "the input ended" is the symptom, and the `(` on line 42 is the cause. When
several are open, the innermost is named — that is the one to close first, and
where an editor's own matching would land.

Reading from a pipe, the line number counts the stream as the shell read it. A
command that consumes stdin itself — `head -1`, `dd`, anything interactive —
takes lines the shell never sees, and diagnostics after it are short by that
many. A script or a `-c` string is handed over whole and is always exact.

A **heredoc** is the exception, and reports without a location: an unterminated
one and a malformed interpolation inside a body are both found by their own scan
rather than by the parser, and neither carries a span to report yet.

A script that cannot be found exits `127`; one that exists but cannot be read
exits `126` — the same codes an unrunnable command yields. Otherwise the exit
status is the last command's, or whatever `exit` was given.

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

Exactly one operand: arguments would be positional parameters, which mesh cannot
set yet, so they are refused rather than ignored. A missing file reports `127`
and an unreadable one `126`, the statuses `mesh FILE` itself uses. A **syntax
error rejects the whole file**, so a broken rc cannot leave a half-defined
config.

**A file that broke says so**, whatever it went on to end with. A command's
nonzero status is reported as-is, but an unhandled **evaluation error** — a
syntax error, an unbound variable, a bad definition name — is remembered, and the
first one becomes this `source`'s status even if every later line succeeds.
Otherwise later statements would overwrite it, and this gate could not be written:

```mesh
source ~/.config/mesh/local-env.mesh && source ~/.config/mesh/local-rc.mesh
```

**Only handling it takes it back**, and `f || fallback` does, however deep the
failure was. But an `||` answers for the failure it recovered and no other, `&&`
runs on *success* so it never answers, and `break` / `continue` / `return` leave
without answering. An error stays answerable until execution moves past it — the
next statement, the next `while` test — after which only the construct it was the
outcome *of* can be answered for. Each row below looks handled and is not:

| | |
| --- | --- |
| `$first`<br>`… || puts handled` | an error already standing from earlier in the file survives a later, unrelated recovery |
| `$first \|\| fb`, where `fb` raises its own | the recovery answered for `$first`; nothing answered for what `fb` broke on |
| `if true { $first; $second } \|\| puts handled` | the `\|\|` answers for the `if`'s outcome, `$second`; `$first` went out of reach when `$second` began |
| `if true { $nope; false } \|\| puts handled` | the `\|\|` is answering for what `false` *returned* — a status, not the error |
| `for x in [a] { $nope; break }` | leaving a construct is not handling what broke inside it |
| `$nope`<br>`puts handled if false` | a skipped statement runs nothing, so it settles nothing; the failure is still standing |
| `while cond { … $nope } \|\| fallback` | the loop tested again after the failing pass, so the `\|\|` answers for that test rather than for the error |

The mirror case *is* recovered: `for x in [a] { $nope } || fallback` runs nothing
between the failing pass and the loop's exit, so the failure is still the loop's
own outcome and the `||` answers for it.

**`return` leaves a sourced file**, and `source` reports the returned value's
status; a bare `return` carries the last status, as a bare `exit` does. `exit`
still ends the **shell**, sourced file included, since `source` runs here rather
than in a child. A script, a `-c` string, and a typed line have no caller, so
`return` there is an error naming both units that accept one — which is what makes
an early out writable:

```mesh
if $sh.interactive == false { return }   # the rest is interactive-only
```

### `$sh.origin` and `$sh.source`

Two read-only entries say **what is being evaluated**:

| Entry | Value |
| --- | --- |
| `$sh.origin` | `script`, `sourced`, `command` (`-c`), `stdin` (`-s`), or `interactive` |
| `$sh.source` | the file's path for `script` / `sourced`, empty otherwise |

They are deliberately **not** the same question as `$sh.interactive`, and come
apart in both directions. `mesh -s` on a terminal reads typed commands, yet its
origin is `stdin` and `$sh.interactive` is `false`. And `mesh -i script.mesh` is a
script — origin `script` — that is nonetheless an interactive session, so it sources
`rc.mesh` and reports `$sh.interactive` as `true`.

The `interactive` **origin** is the narrower claim that the commands were *typed
at a prompt*, which only the interactive loop can make. `printf 'ls' | mesh -i` is
an interactive session whose commands came from a pipe, so its origin is `stdin`.

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
[`$sh.options`](#shoptions), the settings map, and the seven
[hook maps](#custom-prompts-and-hooks) (`$sh.preprompt` … `$sh.exit`).

The rest of the read-only runtime surface:

| Read | Value |
|---|---|
| `$sh.status` | The last command's exit status — see [Exit status](#exit-status) |
| `$sh.pipestatus` | That run's per-stage statuses, as a list |
| `$sh.pid` / `$sh.ppid` | This shell's process id, and its parent's |
| `$sh.uid` | This shell's effective user id |
| `$sh.host` / `$sh.fqdn` | This machine's name, cut at the first `.` and whole |
| `$sh.version` | The shell's version |
| `$sh.interactive` | Whether this is an interactive session |
| `$sh.width` | The terminal's column count, or `0` when there is no terminal |
| `$sh.stdin` / `$sh.stdout` / `$sh.stderr` | Handles for the shell's own streams |
| `$sh.jobs` | The live background jobs, as a map of records |

`$sh.interactive` answers **what kind of session this is**, not what fd 0 happens
to be: `mesh -s` on a terminal reads commands without being an interactive session
and reports `false`, while `-i` makes one out of any input and reports `true`.

`$sh.host` is the machine's name up to its first `.`, and `$sh.fqdn` the whole
name — the split bash draws between `\h` and `\H`. A prompt or window title
almost always wants the short one, since the domain is identical on every machine
a person works across and so costs width without distinguishing anything.

Both come from `gethostname(2)` at each access, which is the point: `$(hostname)`
is a fork paid every time the prompt draws, to learn something that does not
change. `$env.HOSTNAME` is deliberately **not** consulted — it is not in the
environment a login shell is given on either platform mesh targets, and a stale
exported copy names the machine you `ssh`'d *from*, which is exactly the case a
host in the prompt is there to distinguish. When the name cannot be read at all
both are the empty string, the same honest sentinel `$sh.width` uses for "there
is no terminal".

`$sh.width` is read from the terminal at each access rather than cached, so it
cannot go stale: `TIOCGWINSZ` is what the kernel answers from and it is current
the instant the window changes, `SIGWINCH` being only the notification that it
did. It costs one `ioctl` when stdout is a terminal, and up to three when it is
not and the fallback below walks on — against the `tput cols` fork it replaces.

The terminal it asks is stdout's, then stderr's, then stdin's: the width that
matters is the one being *looked at*, and a redirected stdout answers `ENOTTY`
rather than the terminal behind it. `mesh script.mesh | less` is what the
fallback is for — stdout is a pipe, and a script sizing a progress line or a
table it writes to stderr still wants the real width.

With no terminal anywhere it is **`0`**, which is not a width — so `$sh.width`
says "there is no terminal" without a caller having to know which number was
invented. A prompt that wants a default can pick its own:

```mesh
columns = if $sh.width == 0 { 80 } else { $sh.width }
```

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

The lifecycle hook maps — `$sh.preprompt`, `$sh.preexec`, and the five others —
are the other writable part of `$sh`; see
[Custom prompts and hooks](#custom-prompts-and-hooks). `$sh.complete` and
`$sh.signal`, which `DESIGN.md` also describes, are not implemented yet.

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

An interactive session sets `$env.SHLVL` to one more than it inherited before
the startup files run, so a config can tell a shell started from inside another
shell (`SHLVL` ≥ 2) from the terminal's or login's own first shell. Matching nu
and fish, a script or `-c` run passes the inherited value through untouched. A
non-numeric inherited value restarts the count at 1.

Each startup file is a sourced file and reports itself the same way, so the
[breakage rule](#source) applies to it. A file that fails does **not** stop the
sequence — `rc.mesh` still runs after a typo in `env.mesh`, since one broken file
is no more a reason to skip the rest than one failing command in it is — but it is
no longer *covered* by one that ran fine. The first file that broke is what the
set reports, and `$sh.status` at the first prompt says so, rather than showing the
last file's success over a shell holding the `PATH` `env.mesh` never finished
setting.

Interactive command history is saved under `$XDG_STATE_HOME` — see
[History and recall](#history-and-recall). Pass `--no-save-history` (or the
shorter `--no-history` alias) to keep history in memory for that session instead.

---

## Commands

A line is a command: the first word names it, the rest are arguments. Words are
separated by spaces.

```text
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

What a continued line break *means* is whatever the bracket holds. A `( … )`
group and a `${ … }` body hold **one expression**, so a newline in them separates
nothing and is layout — it may fall anywhere, the operator ending a line or
opening the next:

```mesh
x = (1
     + 2)         # 3 — the operator may lead
x = (1 +
     2)           # 3 — or trail
```

A `[ … ]` list and a `{ … }` block hold **several** things, so there a newline is
a separator like any other: `[1` / `2]` is a two-element list, not a wrapped sum.
A group still holds exactly one expression however it is spaced, so a second one
on the next line is a syntax error rather than a statement list.

### Pipelines and sequencing

```mesh
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

The **status** a sibling that *runs something* leaves is seen the same way, in a
call's argument list as in a command's words: `puts(false(), source($f))` and
`/bin/echo false() source($f)` both source with `1` standing, exactly as the two
lines written one after the other would. Only running changes it — `status(5)`
builds a status without leaving one, as `re("x")` builds a regex, so
`puts(false(), status(0), source($f))` still sources with `1`.

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

Spacing separates one from the next, so a `(` that does not abut what precedes it
opens the next argument rather than calling it. An expression elsewhere is freer —
`y = f (1)` calls — but here the space is doing the separating:

```mesh
puts (a()) (b())              # two arguments: a's value and b's
puts (a())(b())               # one: a's value called on b's
```

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
attached call from an argument — `puts(1 + 2)` calls `puts` for a value (a command's
is the [status](#exit-status) it leaves), while `puts (1 + 2)` passes it one.

An unknown command prints `command not found` and sets a failing status. When
another shell spells the same thing differently, the message names mesh's
spelling in a parenthetical, so the reflex has somewhere to go:

```text
mesh$ read line
mesh: command not found: read (mesh spells this `gets`)
mesh$ local x = 5
mesh: command not found: local (a plain `x = 5` inside a `func` is already local)
mesh$ whence ls
mesh: command not found: whence (mesh spells this `type`)
```

A **bound name** draws one too. `double = func(x) { … }` makes `double` a
variable, not a command, so `double(5)` is one `$` from working — and the note
says which, rather than sending you after a program that was never the point:

```text
mesh$ double = func(x) { $x * 2 }
mesh$ double(5)
mesh: command not found: double (`double` is a variable holding a function; call it `$double(…)`)
mesh$ puts $double(5)
10
```

It is a diagnostic and not a resolution rule: it is asked only when nothing on
`PATH` answers the name, so a real program still wins over a same-named
variable, and the note can only ever replace a dead end.

The name-lookup command draws four of these — `type`, `what`, `which` and
`where` all point at [`type`](#type) — because it is the one command every
shell names differently. `which` and `where` are real externals on many systems,
so those two notes appear only where the command is genuinely missing.

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
  `$(printf "a\nb\n")` is the two-line string `a\nb`.
- **Nothing splits implicitly** — captures included. A caller who wants the lines
  says so, and the shape is then readable from the line rather than inferred from
  what the command happened to print:

  ```mesh
  for line in $(git status --porcelain):lines { … }
  names = $(find . -print0):nulls        # NUL only, so a newline in a name survives
  blob  = $(cat log):raw                 # the bytes, trailing newline kept
  ```

  A bare `$(cmd)` handed to a loop is **refused**, naming `:lines` — the one place
  where getting the shape wrong used to be silent. See [Modifiers](#modifiers) for
  the family and its two-letter aliases.
- **Quoting a capture changes nothing.** `x = $(pwd)` and `x = "$(pwd)"` are the
  same string, unlike bash, where the unquoted form word-splits. Quote when the
  capture is glued to other text in a word (`"$(id -un)@$(hostname)"`), not to
  defend against splitting.
- **A split modifier binds the raw bytes** rather than the trimmed value — see
  [Modifiers](#modifiers).
- **Only stdout is captured.** The command's stderr goes where the shell's does,
  so a diagnostic still reaches the terminal instead of ending up in the value.
- **Elements are literal** — never re-split on spaces, never re-globbed — like
  every other value: `puts $(puts '*')` prints `*`.
- **A failing capture still yields its output**, and the status travels alongside
  it. A nonzero exit is routinely a *result* rather than an error — `diff` exits 1
  for "they differ" and puts the diff on stdout, `grep` exits 1 for "no match",
  `timeout` exits 124 over whatever was printed before the deadline — so throwing
  the bytes away would discard the thing that was asked for. An **assignment takes
  its right-hand side's capture status**, which is what makes the bash idiom read
  the same here:

  ```mesh
  if out = $(diff old new) {
    puts "no change"
  } else {
    puts $out                    # the diff, on the branch that has it
  }
  ```

  With several captures in one right-hand side the last one decides, as in bash
  (`x = "$(false)$(true)"` leaves `0`).

  **Only an assignment keeps it.** Interpolate a capture into a *command* and the
  command's own status is what remains — `puts "[$(false)]"` finishes with `puts`'s
  `0`, and the capture's failure is no longer recoverable, exactly as in bash. So
  when a capture's failure matters, **bind it first** and branch on that, rather
  than passing it straight to another command and checking afterward:

  ```mesh
  if out = $(cmd) { use $out } else { warn "cmd failed" }
  ```
- The body is ordinary mesh, so it may hold several statements
  (`$(puts a; puts b)`), a pipeline, or another capture.

It is usable wherever a value is — an assignment, a condition, a command argument
(see [Values as arguments](#values-as-arguments)), and inside `"…"` (see
[Quoting](#quoting)) — but **not** inside `'…'`, `r'…'`, or a heredoc body.

### Redirection

The bash operators, and they mean what they do there: `>`, `>>`, `<`, `2>`,
`2>>`, `2>&1`, `>&2`, `<&0`, and `&> file` / `>& file` for both streams.

**The last stage of a pipeline runs in the shell**, not in a fork, whenever it is
something the shell runs itself — a builtin or a function. So a binding it makes
outlives the pipeline:

```mesh
cmd | gets line
puts $line                             # set: the read happened here
```

This is bash's opt-in `lastpipe`, automatic in mesh. In bash without it,
`seq 3 | while read x; do n=$((n+1)); done` leaves `n` at `0`, because the loop
ran in a subshell.

A **compound statement is a stage** as well — `if`, `match`, `for`, `while` and
`loop` — so the shape that loses bash its variables keeps them here:

```mesh
n = 0
puts "a\nb\nc" | while line = gets() { n += 1 }
puts $n                                # 3
```

A stage is a new *position* for those statements, not a new grammar: the same
keywords, read by the same parser. What a compound cannot do is **lead** a
pipeline — `while … { } | cat` is a syntax error, because the statement
dispatcher reads the `while` before a pipeline is looked for. `fork` and `with`
are left out of stage position deliberately, both being contextual words that a
command of the same name has to stay able to use.

It applies where **job control is not active** — scripts and non-interactive
shells — which is the condition bash puts on `lastpipe` too. Under job control an
interactive pipeline hands its forked stages the terminal and the shell is not
among them, so reading the pipe here would leave `cat | gets line` unstoppable:
Ctrl-Z would stop `cat` and not mesh. There the last stage forks as it always
did, and the binding does not outlive it.

Every *earlier* stage still forks, which is what makes a pipeline concurrent, and
so does the last stage in three cases: an **external** (it needs a process to
`exec` into), a **backgrounded** pipeline (the shell is not waiting for it), and
**`exec`** itself (`cmd | exec prog` is observably `cmd | prog`, so replacing the
shell there would end the session for nothing). A function whose *body* reaches
`exec` cannot be kept out that way, so `exec` refuses while the shell is standing
in for a stage — the same refusal it makes inside a capture. A stage carrying a value still
evaluates it in its own process, wherever it sits.

The status and [`$sh.pipestatus`](#shstatus-and-shpipestatus) are the same either way,
and an `exit` in the last stage is still that *stage's* exit, reported as a
status — the shell carries on.

Being a stage still means what it did: `fg`, `bg`, `wait` and `disown` are refused
there, last stage or not, so a pipeline does not read differently depending on
where the builtin sits. `jobs` and `kill` are unaffected — listing and signalling
need no parenthood.

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
| A subcommand or flag of an external command | Whatever the layered spec for that command says — see [Where a command's completions come from](#where-a-commands-completions-come-from) |
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

#### Where a command's completions come from

For an external command, the subcommands and flags on offer come from a **spec**,
and mesh looks for one in four places. The first that answers wins:

| Order | Source | Runs anything? |
| --- | --- | --- |
| 1 | A **curated file** you wrote, in `$XDG_DATA_HOME/mesh/completions/` (or `~/.local/share/mesh/completions/`) | No |
| 2 | The command's **manual page**, rendered with `man` | A formatter, over a data file |
| 3 | A bounded **`--help` probe** of the command itself | Yes — the command runs |
| 4 | Files and directories | No |

A generated spec — 2 or 3 — is cached under `$XDG_CACHE_HOME/mesh/completions/`
(or `~/.cache/mesh/completions/`), keyed so it regenerates when *its own* source
changes: a `--help` spec by the binary's path, size and mtime, a man-page spec by
the page's. A source that yields nothing falls through to the next rather than
answering with an empty list.

**A curated file** is named the way the manual names the same thing — `git`,
`git-commit` — so a subcommand's spec sits beside its command's. One candidate
per line, with a value type spelled out rather than guessed:

```text
# ~/.local/share/mesh/completions/demo
--verbose
--output file            # file, dir, page, or a | list of literal values
--color auto|always|never
build
positional dir
```

Because it is read before the command is even resolved, a curated file works for
something that is not on `PATH`, and it is re-read on every Tab, so editing one
takes effect immediately. A file that says nothing — empty, or only comments —
falls through instead of suppressing what would otherwise be offered.

**A manual page** is only trusted when it belongs to the same install as the
binary: mesh looks beside the executable, so `<prefix>/bin/tool` is documented by
`<prefix>/share/man`. A project-local `./tool` therefore does not inherit
`/usr/bin/tool`'s page — that case falls through to the probe. A page contributes
flags only; subcommands are left to the probe, since pages describe them in prose
rather than in a table.

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
| Ctrl-D | Delete the character under the cursor; on an **empty** line, exit — but only when nothing is buffered behind it |

Ctrl-C **abandons** rather than runs: nothing executes and `$sh.status` is left
as it was. A line that opens a block or a quote keeps reading at the `...`
continuation prompt until it balances, and Ctrl-C drops the whole thing.

Ctrl-D is `delete-char` first, as it is in bash: with characters on the line it
deletes the one under the cursor, and at the end of the line there is nothing to
delete, so nothing happens. Only on an **empty** line does it mean end-of-input,
and there it exits **only when nothing is buffered behind that line** — at the
`...` prompt, with a block or heredoc part-way through, it does nothing. Those
lines are input still in hand, and Ctrl-D never throws typed text away. That
keeps the two gestures distinct: Ctrl-D leaves, Ctrl-C discards. Press Ctrl-C to
drop the block, then Ctrl-D to leave.

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

<!-- no-run: history expansion is interactive-only, so a script sees `!$` as text -->
```text
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
| `puts [arg …]` | Render each argument and print them separated by single spaces, then a newline. No arguments prints a blank line. Rendering is per value: a scalar as itself, a **list** as its elements joined by newlines, a **map** as `key: value` lines. A **nested** collection moves down a level — indented two spaces under its map key, or prefixed with a `- ` bullet as a list element, which is what keeps `[[1 2] [3 4]]` from printing exactly as the flat `[1 2 3 4]`. Depth is not capped. A value with no byte form — a job or stream handle, a function, a pattern — is a loud error rather than a guess, wherever it is nested. Unlike argv, `puts` sees the real value, so `puts $xs` needs no `...`; a *written* argument keeps its own text, so `puts 007` prints `007`. It takes no flags — so a [flag](#flags-and---) handed to it is an option it cannot match and is **reported**, exactly as a `func` with no such parameter reports one. `puts -- --force`, `puts "$x"` and `puts $x:repr` are the three ways to print one; a flag *inside* a collection is data being displayed and needs none of them. |
| `print [arg …]` | The same as `puts` with **no trailing newline**, for partial lines. No arguments prints nothing. |
| `gets [--nulls] [var]` | Read one line from stdin, strip its trailing newline, and bind it to `var`. **`--nulls` reads a NUL-terminated item instead** — what a `find -print0` stream needs, since a newline inside a name is data there and a line read tears the name in half. The separator is *named* rather than passed as a character because `\0` is not a string escape (a NUL crosses neither `execve` nor the environment), and `--nulls` is what the [`:nulls`](#modifiers) family already calls it. The delimiter is a terminator either way, so a final item without one is still an item. **At end of input the status is `1` and `var` is left unchanged**, which is what terminates `while gets line { … }`. An empty line is a successful read of `""` — a blank line mid-file must not end a loop — so only a zero-byte read ends it, and a final line with no trailing newline is still a line. A line that is not valid UTF-8 is **refused** rather than repaired — status `2`, and `var` is left alone — following the capture rather than `$env`'s lossy read; status `2` is also what an I/O error reports, so `1` means end of input and nothing else. Interactively, **Ctrl-C cancels a read** — status `130`, and `var` keeps whatever it held, since a cancelled read has read nothing. It reads a byte at a time, so the bytes after the line reach whatever runs next rather than being swallowed by a buffer. With no `var` it consumes the line and reports only whether there was one. |
| `gets()` | The **value** form of the same read — parens attached, so it yields the line into an expression rather than reporting a status: `line = gets()`, `[k v] = gets():split("=")`, `while line = gets() { … }`. `gets(--nulls)` takes the one flag the command form does, so the composable spelling is not the one that cannot read a `-print0` stream. **At end of input it yields `false`**, which is what stops those loops: an [assignment as a condition](#conditionals) is true iff its right-hand side is truthy, and an empty line is a truthy `""`. It takes **no arguments** — the binding is the assignment it sits in, where the command form takes the name as an operand. Both spellings read through one reader, so everything above holds here: the byte-at-a-time read, the refusal of a non-UTF-8 line, and Ctrl-C cancelling. A failure **raises** rather than yielding, since `false` already means end of input. As the **last stage of a foreground pipeline** the read happens in the shell itself, so `cmd \| gets line` leaves `line` set — see [the last stage](#redirection). In an *earlier* stage, or a backgrounded one, it happens in a forked process and the binding dies with the stage, the same as any builtin. `cmd \| while line = gets() { … }` works for the same reason — a [compound statement is a stage](#redirection), so the loop runs in the shell and keeps what it counts. |
| `style(text, fg: …, bg: …, bold: …)` | A [styled value](#styled-values) — text plus display attributes. A **value call**, parens attached, because a command position yields a status. Colors are the sixteen ANSI names: `black`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `white`, `grey` (or `gray`, or `bright-black`), and `bright-` forms of the rest. |
| `link(text, url)` | A [styled value](#styled-values) carrying an `OSC 8` hyperlink, so `text` is clickable. The url needs a **scheme** (`https://…`, `file://host/path`) and anything RFC 3986 forbids raw is percent-encoded, a space included; over 2083 encoded bytes is refused, since past a terminal's own limit the whole sequence — link text included — is dropped. |
| `glob(pattern)` · `files(dir = ".")` · `dirs(dir = ".")` | The paths a pattern matches, and a directory's immediate files or subdirectories — a **list**, since these are [value calls](#the-glob-family) rather than commands. |
| `cd [dir]` | Change directory. No argument goes to `$env.HOME`; `cd -` returns to the previous directory and prints it. A plain relative name is searched in [`$env.CDPATH`](#cdpath). Updates `$env.PWD` and `$env.OLDPWD`, and runs the [`precd` / `postcd` hooks](#custom-prompts-and-hooks) around the move. Autocd is not implemented, so a bare directory name is a command, not a `cd`. |
| `pwd` | Print the working directory. |
| `clip [text …]` | Copy to the terminal's clipboard with `OSC 52`, so it works over `ssh`. Arguments join with a space; with none, stdin is read (`puts hi \| clip`). The bytes are copied as given, a trailing newline included. Goes to the terminal, not stdout, so a redirect cannot swallow it. Whether the copy lands is up to the terminal — xterm needs `allowWindowOps`, tmux `set-clipboard on` — and there is no reply, so success means "asked". |
| `notify [text …]` | Raise a desktop notification through the terminal with `OSC 9`. Arguments or stdin, like `clip`. A command that runs for more than ten seconds notifies on its own, with its outcome and duration — `$sh.options.command-notify = false` turns that off. Inside tmux the sequence is wrapped for passthrough, which tmux forwards only with `allow-passthrough` set. Support is uneven and unreportable — iTerm2, WezTerm, Ghostty, kitty and ConEmu raise these; xterm and Alacritty discard them; tmux needs `allow-passthrough` — so success means "asked". |
| `status code` · `status(code)` | A [**status value**](#exit-status) — how a command went — from a code between `0` and `255`. Out of range is refused rather than truncated, and so is a code that is not an integer: both spellings read the operand as a *value*, so `code = "5"; status $code` is refused exactly as `status($code)` is. The call is the constructor (`file-not-found = status(5)`), and the command form leaves the same value as the statement's result, which is what a `match` arm writes: `x = match $kind { missing => { status 5 } }`. `status(0)` is legal where `fail 0` is not: naming a zero status is reasonable, while a `fail` that succeeds is a mistake. It writes nothing and answers with a value, so — like `return` — it is refused in a pipeline, under a redirection, and in the background, where that value would be discarded. |
| `exit [n]` · `exit status n` | Leave the shell with status `n` (default: the last command's status; masked to 0–255). **`exit status n` is the same thing**, written the way [`return status n`](#functions) is, so the two ways of leaving with a status read alike — it disambiguates nothing, since `exit` fills only the status channel. `exit status(n)` needs no rule: the call is one word that renders as its code. `exit status` with no code is an error, as `return status` is, and does not end the shell. `exit value n` is refused by name — `exit` has no value channel to fill, since a value needs somewhere to go and a leaving shell has nowhere. That message only changes what is *said*: `exit` reads words, so a quoted `exit "value"` cannot be told from the written one, and each keeps the outcome its operand would have had — alone it still exits `2`, with more after it the shell stays. Leaves the **whole shell**; to leave only the current function with a status, use `fail`. |
| `fail [n]` | Leave the current function (or sourced file) with a nonzero status — `1` by default, `n` when given — carrying that status as its value. A **validating wrapper** over `return status(n)`, not exact sugar for it: `fail 0` is refused, where `status(0)` is legal. `return true` is how a function leaves with success. |
| `prompt [text]` | Set the interactive prompt to `text`. With no arguments, print the current prompt; `--reset` restores the status-sensitive default, and `prompt -- --reset` sets that literal text. |
| `title text` | Name the window and tab with `OSC 0` — `ESC k` inside screen or tmux, where the name belongs to the pane. The shell titles itself already (`user@host: dir` at the prompt, the command line while one runs); this is how a [`preprompt` or `preexec` hook](#custom-prompts-and-hooks) says something else. Control characters become spaces and the text is cut at 96 characters, as it is for the automatic titles. `title ""` clears it. `$sh.options.osc-title = false` silences this along with the rest, and a terminal off the allowlist is sent nothing. There is no `--reset`: the shell holds no title of its own to restore and a terminal cannot be asked what its title is, so the only question `title` answers is "write this now". **Calling `title` takes the window**: the shell stops naming it from then on, so what you set stays until you or a hook sets something else. A session with only an `on preexec` handler therefore loses the automatic idle title too — see `TODO.md`, which tracks replacing this rule with a shipped hook you can override. Goes to the terminal rather than stdout, as `clip` and `notify` do, so a redirect cannot swallow it: `title x > file` names the window and leaves the file empty. **Refused in a forked pipeline stage** (`title x \| cat`): the write would reach the terminal but the clear mesh owes on the way out would die with the stage, leaving the window named after a shell that has gone. A pipeline's *last* stage runs in the shell itself, so it is allowed. |
| `on event name function` | Register a named function for a prompt lifecycle event. Reusing `name` within an event replaces that hook without changing its order. |
| `jobs` | List the jobs, one `[id] State command` per line. |
| `fg [job]` | Resume a job in the foreground and wait for it. No argument takes the most recent job. |
| `bg [job]` | Resume a stopped job in the background. No argument takes the most recent job. |
| `wait [--timeout duration] [job …]` | Wait for a job to finish and report its status. `--timeout` bounds the wait without touching the job — see [Job control](#job-control). |
| `timeout duration cmd [arg …]` | Run a command under a time limit, killing it and reporting `124` if it runs out — see [Job control](#job-control). |
| `kill [-signal] job\|pid …` | Signal a job's process group, or a pid. Default `TERM`. |
| `disown [-h] [-a \| -r] [job …]` | Stop tracking a job — see [Job control](#job-control). |
| `command [--] name [arg …]` | Run the **program** `name`, past the builtin or function that name would otherwise reach — which is what makes `func ls() { command ls --color=auto }` safe to write, and what reaches `/usr/bin/env` when a function of that name is in the way. Only the words in front of the program are `command`'s own: `command ls --help` asks `ls` for its help, and `--` ends `command`'s options so the word after it is the program however it reads. `--help` is the only option it has, so any other flag-looking word in front of the program is a usage error (status `2`) rather than a program name — `command -v` / `-V` are held for the unbuilt half, and `command -- -v` runs a program called `-v`. The operand is the program with nothing peeled off it, so `command command x` looks for a program called `command`. A builtin's name finds no program, and says so; with no operand at all the status is `2`. |
| `exec [--] cmd [arg …]` | Replace the shell process with the **program** `cmd` — the `exec(2)` hand-off, so on success no shell survives: `exec autotmux` in a dispatcher leaves only the session it started. `cmd` resolves as an external executable; a builtin or function has no process image with which to replace the shell, so a name only they answer to is an error saying which kind declined (`127`). Only the leading words are `exec`'s own, exactly as with `command`: `--` ends its options, `exec ls --help` is `ls`'s help, and a flag-looking word in front of the program is a usage error (status `2`) rather than a program name. With redirections and **no** program (`exec > log`, `exec 3< file`) the targets apply to the shell itself and *stay* applied — the one redirection nothing restores; a bare `> f` with no command stays an error whose message points here. A failed replacement reports and keeps an interactive session, while a script exits with the failure (`127` not found, `126` not executable) — it asked to become the program, and there is nothing it was going to do as itself. In a pipeline stage, a `fork` block, or a `&` background command, `exec` replaces that child process and the shell carries on — a directly spelled `exec` stage is kept in its own fork for exactly that reason. Inside a `$(…)` or `:capture` it is refused: mesh's captures run in-process, and their readers are the shell's own threads. It is refused for the same reason when reached from a **function running as a pipeline's last stage**, which runs in the shell rather than a fork: there is no separate process left to spend, and replacing this one would end the session. |
| `source file` | Run a file's mesh code in this shell — see [`source`](#source). |
| `type [-t\|-P\|-a\|--quiet] name …` | Say what each name is — see [`type`](#type). |

### Flags and `--`

Every command mesh owns — builtin or function — reads flags by one rule.

**`--help` prints the generated help**, whether it was written or arrived in a
variable: `x = --help; puts $x` prints the usage, and so does `f $x`. mesh's
expansion safety is about never *splitting* or *globbing* a value; it was never a
promise to launder a word that is a flag.

A builtin's `Options:` block lists **its own flags** alongside `--help`, read off
the usage line rather than written twice — so `type --help` names `-t` and
`--quiet`, and `disown --help` names `-h` / `-a` / `-r`. Tab completion is built
from that same text, so those flags complete too. A *metavariable* is not a flag:
`kill [-SIGNAL]` stands for whichever signal you name, so nothing is listed for
it.

**`--` ends the options and is consumed**, so it is how you mean a flag-looking word
as data:

```mesh
puts -- --help                # --help
puts -- -- x                  # -- x     — only the first one goes
prompt -- --reset             # sets the prompt to the text `--reset`
kill -- -9 %1                 # looks for a job named `-9`, not signal 9
```

Which command consumes it depends on which has options to end. `puts`, `print`,
`clip`, `notify`, `cd`, `source` and `help` have none of their own, so the terminator
is simply removed. `gets`, `kill`, `disown`, `prompt`, `on`, `wait`, `command`, `exec`
and `type` do, so each ends its own options at `--` — only they know where those stop.

`command` is also where the `--help` rule stops applying, because the arguments
after the program name are not mesh's to read:

```mesh
command grep -- -x file       # `--` reaches grep; it looks for the line `-x`
command grep --help           # grep's own help, not mesh's
command --help                # mesh's help for `command` itself
command -v ls                 # error: `command` has no `-v`; status 2
command -- -v                 # runs a program called `-v`
```

### `type`

`type name …` says what each name **is** — bash's name, bash's flags, and bash's
words. `whence` is ksh's spelling and `where` zsh's; both, and `what`, point here.
`which` does **not**: in bash it is an external program that cannot see builtins
or functions, and mesh keeps that, so `which cd` finds nothing here exactly as it
finds nothing there.

The value-side question — what a value is, written back as source — is
[`:repr`](#modifiers), and `$p:type` still asks a path's type. Those are modifiers
on a value; `type` is a command, and neither can be written where the other is
meant.

```mesh
type ll            # ll is a function
                   #     func ll(...args)
type cd            # cd is a shell builtin
                   #     cd [DIR]
type unless        # unless is a shell keyword
                   #     cmd if COND
type true          # true is a boolean literal
                   #     true · false
type rg            # rg is /usr/local/bin/rg
type xs            # xs is a variable
                   #     a list of 3: ['a', 'b', 'c']
```

**Two flags carry the shapes a script consumes**, and both are bash's, because
their output is compared rather than read. `-t` prints one word; `-P` prints only
a `PATH` hit, ignoring functions and builtins:

```mesh
type -t ll         # function
type -t cd         # builtin
type -t rg         # file
type -t if         # keyword         — an always-claimed one; `unless` is contextual
type -t true       # keyword         — the literal; bash has no word of its own for one
type -t xs         # variable        — the one word bash has no use for
type -P rg         # /usr/local/bin/rg
type -P ll         # (nothing, status 1) — a function has no path
```

`-t` is what a guard should compare against instead of matching prose, and `-P`
retires the hand-rolled `for d in $PATH` loop a portable script carries because
`type -P` is not available everywhere. Both print nothing and exit `1` when there
is no answer.

A name is given **without a sigil**. `type xs` asks about the *name* `xs`;
`type $xs` would expand first, so the built-in would never see the name at all.
Because bindings live in their own namespace, a name that is both a command and a
variable is reported as **both** — neither shadows the other — and an `$env` entry
is reported the same way (`type PATH`).

What a name resolves to is reported in **resolution order**: a keyword or a
literal, then a builtin, then a function, then the executables `PATH` holds. Bare, `type` reports
the **winner** and says nothing about what it displaced — describing what a name
could have matched but did not is not worth a line. `-a` is where every match
lives, as in bash:

```mesh
type git           # git is a function
                   #     func git(...args)
type -a git        # git is a function
                   #     func git(...args)
                   # git is /usr/bin/git
```

A word with a `/` in it is a **path operand**, read the way command resolution
reads it — the file itself, with no `PATH` search (`type ./build.mesh` →
`./build.mesh is an executable file`).

**Described is not the same as usable**, and where the two part the report says
what it found and the status still fails — so the printed line explains the
failure rather than contradicting it. Two cases:

- A path that exists but could not be run: a directory, a file without the
  execute bit, or a special file (`type ./p` → `./p is a named pipe`).
  Executing any of them is a `126`, so an execute bit on a fifo does not make it
  a command.
- A shape the parser does not claim in command position. `help` documents three
  different things, and only one of them runs when typed bare:
  - **Always claimed** — `if`, `for`, `while`, `match`, `func`, `return`, `break`,
    `continue`, `not`, `global`, `export`, `unset`. These resolve.
  - **The boolean literals** — `true` and `false`, which the parser reads as the
    value in every position, so command position never reaches the program of
    that name. `type true` reports `true is a boolean literal` and resolves; the
    program is what `-a` shows under it, and what `type ./true` and `type -P true`
    answer with, since `./true` and `command -- true` still reach it. Not a
    keyword: the word is a value, not a construct. `func true() { … }` and
    `alias true = …` are refused for the same reason `func if()` would be — the
    definition could never be reached — while `true = 5` is fine, because a
    binding is read as `$true` and nothing shadows it there.
  - **Contextual** — `fork` is the subshell keyword only before a block, `unless`
    is a postfix guard only *after* a statement, and `and` / `or` / `in` join
    values. A bare one is an ordinary command word, so `unless` on its own is
    `command not found`. These are described but do not resolve — and, because a
    bare one really does reach a lookup, a function or executable of that name is
    the answer to "what runs": `func fork() { … }` then `type fork` reports the
    function, with the keyword beside it and neither shadowing the other.

    `if` is **both**, and lands in the group above: it is the postfix guard after
    a statement *and* the prefix conditional in command position, and the prefix
    role is unconditional, so a bare `if` is a syntax error rather than a lookup.
    Sharing a spelling with a guard does not make a word contextual — where it
    stands in command position is the whole of what decides it.
  - **Punctuation and shapes** — every operator a line can carry. `type +` says
    what `+` is; it names nothing.

  `re`, `style`, `link`, `glob`, `files` and `dirs` sit with the contextual group
  here. The parser refuses them as *function* names, but a command-position
  `style …` is still a lookup that reports `command not found`, since they are
  value calls.

**`-t` parts with the sentence form on one of those two**, and the split is
whether mesh **owns** the name. A value call falls back to `keyword` and
succeeds — the word the sentence form already uses — because `func files() { … }`
is a *syntax* error, so a config asking `type -t` before it binds a function of
that name needs to hear the name claimed rather than be passed over in silence. A
contextual word answers nothing and fails: `and` reserves no name, so a function
may take it.

A **fallback** is all it is, and two things outrank it. A value call's name is
not usable in command position, so a **program on `PATH`** of that name wins the
resolution order first: `link` reports the coreutils program on a host that ships
one. A function cannot be the winner here, unlike everywhere else in that order —
`func link()` is refused for the very reason the fallback exists — so `PATH` is
the whole of that tier. A **binding** answers next, since `-t` prints one word
and the sentence form's both-of-them reading has no room here. So `keyword` is
what is left when neither answers, and a guard copied out of this paragraph has
to allow for a session that happens to bind `files`.

```mesh
type -t files      # keyword     — mesh owns it, and nothing else answers to it
type -t link       # file        — /usr/bin/link wins; `keyword` where there is none
files = 1
type -t files      # variable    — a binding outranks the fallback too
type -t and        # (nothing, status 1) — it reserves no name
```

The status is `0` when every name resolved, `1` when any did not, and `2` for a
misuse. **`--quiet`** leaves only that status — no report, and no not-found note
either — which is mesh's `command -v fzf >/dev/null`:

```mesh
if type --quiet fzf { export FZF_DEFAULT_OPTS = "--height 40%" }
```

Without `--quiet` it is an ordinary command that writes, so a bare `type` in a
condition still prints its report. A missing name is reported on stderr and the
names beside it still print, so one typo does not cost the rest. Where the name
is one another shell spells differently, the note says so — `type whence`,
`type what`, and typing `whence` itself all point at `type`.

A bound value's detail line names its shape and, when the literal fits on the
line, shows it. That literal is **exact or absent, never shortened**: an elided
one would not read back, which is the one thing [`:repr`](#modifiers) must not
do. For a long value, `puts` is right there.

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
puts $j.status                  # "" while it runs, its status once it has ended
```

`$j.status` is a [status value](#exit-status) once the job has finished, for the
reason `$sh.status` is one: `wait $j; return $j.status` has to forward the job's
failure rather than its number. It is `""` until then — the empty-value rule, not a
null.

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

**`timeout duration cmd [arg …]`** runs a command under a limit and **kills it**
when the limit passes, reporting `124` — `timeout(1)`'s number, so a script
already written against that keeps reading. A command that genuinely exits 124 is
indistinguishable from one that ran out of time; that collision is the price of
the convention.

It is the counterpart to `wait --timeout`, and the difference is what each one
owns. `timeout` owns the command's lifetime, so it ends it. `wait` only observes
a job someone else started, so it does not.

```mesh
timeout 2s is-ssh-valid        # a hook that may block, bounded
```

`cmd` may be a **function, a builtin, or an external** — it runs through the same
resolution a bare command does. It registers **no job**: nothing is announced,
nothing appears in `$sh.jobs`, and there is no handle to clean up, which is what
makes it usable from a prompt.

Written with `&` it registers one like any other command, and that job is the
bounded run itself — so `kill` on it ends the timed command too, rather than the
supervisor alone. Redirections apply to the wrapped command:
`timeout 5s cmd > out` sends `cmd`'s output to the file.

Two consequences of running the command in a subshell, both shared with `&`:

* **It cannot change this shell.** An assignment inside `timeout 2s some-func` is
  lost with the subshell. A bounded run of something that has to be killable
  cannot also be a run in this process.
* **The command gets its own process group**, so the kill reaches whatever it
  started rather than only the command itself — a function whose blocking child
  was killed cannot carry on to its own successful `return` and report a run that
  ran out of time as healthy. `timeout(1)` makes the same trade, and has a
  `--foreground` for the cases that would rather keep the shell's group.

Backgrounded, the job **is** the bounded run: any signal that would end or
suspend it is passed to the timed command first, so `kill %1` ends it and
`kill -TSTP %1` really suspends it rather than only reporting it as suspended.
`SIGKILL` and `SIGSTOP` are the two that cannot be caught, so those two still
leave the command behind — the same gap `timeout(1)` has. A `timeout` nested
inside another is reached too: the outer limit takes a snapshot of the process
tree before it signals anything, so it finds the command in the inner one's
group even though it never made that group and cannot name it.

A command that **daemonizes** — a handler that forks a child which calls
`setsid()` — still gets away, because the session and group it detaches into are
made after that snapshot and belong to nothing the limit can name. This is the
same gap `timeout(1)` has, and closing it needs a mechanism that outlives the
process table; `TODO.md` carries it.

The kill is a `SIGTERM`, which a command is free to trap. It gets a short grace
to leave on its own and is then sent a `SIGKILL`, so a `124` means the run really
is over. `timeout(1)` instead keeps waiting and escalates only when asked
(`--kill-after`); mesh cannot, because the subshell it waits on dies to the same
signal it forwards — and a builtin whose point is to come back cannot block on a
command that refused to stop. Making the signal and the grace a caller's choice
is in `TODO.md`.

`timeout` takes exactly one word for itself — the duration — and everything after
it belongs to the wrapped command, so there is no `--` to write. It is resolved
*before* expansion, which is what lets the wrapped command expand under its own
name: `timeout 5s show $xs` passes a list to a function exactly as `show $xs`
does, with or without a redirection or an `&`. Only a bare `timeout` is the
construct; a name that arrives through a variable or a quote is an ordinary
command. `--help` in the duration's position asks *this* builtin; anywhere
further along the line it belongs to the wrapped command, as it does after
`command`.

With a redirection the whole stage runs in the fork — words, targets and command
alike — so a capture in the wrapped command is inside the limit too. Backgrounded
it is the other way round, because the shell resolves every stage's targets
before it forks any of them; a capture there is already refused outright.

**It bounds the stage it prefixes, not a pipeline it starts** —
`timeout 5s producer | consumer` limits `producer` alone. Whether that is the
right reading is in `TODO.md`, together with `time`, which has to answer it the
same way. Which *stage* it bounds is settled: a pipeline stage reads the prefix
exactly as an unpiped command does, in either position.

The duration is spelled as below.

**`wait --timeout duration`** bounds the wait. When the limit passes, `wait`
reports `124` and the job is left **exactly as it was** — still running, still
listed, and with `$j.status` still empty, because it has not exited. The `124` is
the *builtin's* status, not a status invented for a job that has not reported
one, and that distinction is the whole design: an empty `$j.status` next to a
`124` from `wait` is how a caller tells "still going" from "exited 124".

```mesh
if wait --timeout 2s $j { … } else { kill $j }   # the caller decides
```

Giving up on the wait rather than on the job is the same answer Ctrl-C already
gives, with a timer in place of a keystroke.

The duration is `500ms`, `2s`, `1m`, `1h`, or a compound like `1m30s` — the
spellings [`duration_words`](#the-prompt) prints. A bare number is seconds. One
that cannot be read is refused (status `2`) rather than rounded into something
nobody asked for.

**One budget for the call, not per operand.** `wait --timeout 5s %1 %2` gives
both jobs five seconds between them, starting when the call does; if it runs out,
the operands not yet reached are not waited for either, and none of them is given
a status. Both spellings work — `--timeout 2s` and `--timeout=2s` — and `--` ends
the flags, so a job reference beginning with a dash is still reachable.

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

<!-- no-run: needs a job stopped from a terminal -->
```text
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

```text
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
on preprompt cwd refresh-prompt
```

To print a context line containing the short (unqualified) hostname, working
directory, and current Git branch before a minimal `> ` input prompt:

```mesh
func prompt-context() {
  host = $sh.host
  dir = $(pwd)
  branch = $(sh -c 'git branch --show-current 2>/dev/null || true')

  if $branch == "" {
    puts "$host $dir"
  } else {
    puts "$host $dir ($branch)"
  }
}

on preprompt context prompt-context
prompt "> "
```

The hook writes the context above the editor; `prompt "> "` controls only the
input indicator. [`$sh.host`](#shargs-and-shname) is the short hostname, read
without a fork; use `$sh.fqdn` for the whole name. The `sh` wrapper makes the
branch segment empty outside a Git worktree without printing Git's diagnostic on
every prompt.

An external renderer works the same way:

```mesh
func refresh-prompt() { prompt "$(starship prompt)" }
on preprompt renderer refresh-prompt
```

The window title is two hooks rather than a setting, because it says two
different things: where the shell is when it is waiting, and what it is running
when it is busy. `preprompt` and `preexec` are exactly those two moments, so
[`title`](#builtins) in each is the whole feature — and the pair is why there is
no format string to learn. A title and a prompt that should agree share a
function, not a syntax:

```mesh
func where-i-am() { "${sh.host}:$(pwd)" }

func title-idle()      { title "${where-i-am()}" }
func title-busy(cmd)   { title "$cmd — ${where-i-am()}" }

on preprompt title title-idle
on preexec   title title-busy
```

Registering either replaces what the shell writes for itself at that moment. The
two get there differently, which matters only if you are reading the sequence on
the wire: `preexec` runs after the running title is written, so the hook's simply
lands second, while the prompt's own title is written after the `preprompt` hooks
— so that a handler which `cd`s is titled from where it left the shell — and
stands aside for the prompt where one of them called `title`. Either way the last
title written is yours, and the halves are independent: titling the busy window
does not cost you the idle one.

`$sh.options.osc-title = false` turns off both the shell's titles and the hooks',
so it stays the one switch that means "leave my title bar alone".

Hooks are session-local and run in registration order. Re-registering the same
event/name pair replaces it in place, making configuration safe to reload.
Remove one with `on --remove event name`, or through the
[hook map](#the-hook-maps). `preprompt` hooks run only for primary prompts, not
multiline continuation prompts.

| Event | Function parameters | When it runs |
| --- | --- | --- |
| `preprompt` | none | Before each primary prompt is rendered. |
| `preexec` | `command` | Immediately before an interactive command runs. |
| `postexec` | `command, status, elapsed` | After an interactive command; `status` is a [status value](#exit-status) and `elapsed` is integer milliseconds. |
| `precd` | `target` | Before the working directory changes, still in the old one. `target` is where it is about to go. |
| `postcd` | `previous` | After it has changed, in the new directory. `previous` is where it came from. |
| `jobdone` | `id, command, status` | Once per background job the shell finds finished, alongside its `[N] Done` notice. `status` is a [status value](#exit-status). |
| `exit` | `status` | Before the shell exits, however the session ended. `status` is the [status value](#exit-status) it is leaving with. |

Every `status` argument above is a **status value**, not an integer, so a handler
forwarding one reports the failure it was told about. Test it directly — `if not
$status { … }` — since `$status != 0` compares across types and is always true.

```mesh
func command-started(cmd) { puts "running $cmd" }
func command-finished(cmd, status, elapsed) {
  puts "$cmd exited $status after ${elapsed}ms"
}
on preexec log command-started
on postexec log command-finished
```

`jobdone` runs where the `[N] Done` notice is printed — at the prompt after the
job ended, not the instant it ended. A job you `wait` for does not reach it: the
status went to the caller, which is what the hook is there to tell you.

`exit` runs on **every** way a session ends: `exit`, Ctrl-D, the end of a
script or a `-c` string, and an `exit` from a startup file. It is the hook for
tearing down what a session set up, and a script cleaning up after itself is as
much that case as an interactive session is.

```mesh
func clean-up(status) { rm -rf $work-dir }
on exit tmp clean-up
```

`status` is what the shell is leaving with — the argument to `exit N`, or the
last command's status for a bare `exit` or an end of input. It matches bash's
`$?` inside a `trap … EXIT`.

Two things it does not cover yet. A shell **killed by a signal** runs nothing;
so does one that dies on `SIGKILL`, which no shell can catch. And a `fork { … }`
subshell leaving is not the session ending, so it runs no handler.

On the way out, every job the shell knows about is reported **before** the
`exit` hook, so a handler that tears down what `jobdone` was writing to can rely
on having seen them all. The exception is a completion the `exit` handler itself
brings about — by running `jobs`, or by taking long enough that a job finishes
while it does. That one is reported after the handler has run, because there is
no earlier moment to report it in: the alternative is not reporting it at all,
and a notice without its hook is the one thing `jobdone` is meant to rule out.

```mesh
func job-finished(id, cmd, status) {
  if not $status {
    puts "job $id failed ($status): $cmd"
  }
}
on jobdone report job-finished
```

The **directory hooks fire around each actual move**, a `cd` inside a function
included — that is what makes `precd`'s promise to run in the old directory hold,
where waiting for the function to return would not. A handler that only cares
about net movement compares `$env.PWD` itself.

```mesh
global last-visit = ""
func remember(previous) { global last-visit = $previous }
on postcd history remember
```

Three rules make them predictable:

- **The target is resolved before `precd` runs**, to the absolute, physical path
  `$env.PWD` will hold. So a handler that changes directory itself cannot make a
  *relative* outer `cd` land somewhere unintended, and `$env.OLDPWD` still names
  where the move began rather than wherever a handler wandered to.
- **A handler's own `cd` does not fire them again.** Changing directory from a
  handler is allowed; re-dispatching would recurse without end.
- **A `cd` that fails runs neither.** A destination that does not exist is
  reported before `precd`; if the move itself then fails — a directory that
  cannot be entered — the failure is reported and no `postcd` is owed.

### The hook maps

Each event is also a map under `$sh`, keyed by hook name — `$sh.exit`,
`$sh.postcd`, one per event. It is the same registry `on` writes, not a copy, so
either spelling registers, replaces and removes what the other sees:

```mesh
func clean-up(status) { rm -rf $work-dir }

$sh.exit.tmp = clean-up          # the same write as `on exit tmp clean-up`
puts ...$sh.exit:keys            # tmp
unset $sh.exit.tmp               # the same removal as `on --remove exit tmp`
```

A map is present for every event from the start, so `$sh.preprompt:len` answers
`0` rather than failing before anything is registered. Reading one is reading a
snapshot: assigning a whole map to a name copies the handlers of that moment, and
writing into that copy registers nothing.

A handler is written as a function's **name**, the same thing `on` takes, and it
is resolved when the event fires — redefining the function changes what the hook
runs. A [function reference](#functions) says the same thing with the sigil that
marks it as one, and is accepted here beside the bare word:

```mesh
$sh.exit.tmp = &clean-up         # the same registration as the bare word above
```

`DESIGN.md` also writes the map form with a lambda
(`$sh.postcd.fetch = func() { … }`); that is not accepted yet — a lambda has no
name to register.

The map is strict for the same reason the settings map is:

```mesh
$sh.exit = [tmp: clean-up]   # error: assign one handler at a time
$sh.exit.tmp = clean-ip      # error: `clean-ip` is not a function
$sh.exit.tmp += clean-up     # error: a handler is set with `=`, not `+=`
unset $sh.exit               # error: it is the hook map itself
```

Refusing the whole-map assignment is the important one: a map literal that left
out a key would have to mean either "leave that handler alone" or "remove it",
and a config that guessed wrong would silently drop every other handler for the
event. Naming one key at a time has no such question.

This is the currently implemented prompt API. The structured `$sh.prompt` map,
styled segments, and `fill`/`rule`, described as the eventual prompt design in
`DESIGN.md`, are not implemented yet.

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

A status is also a **value**, of its own type. `status(5)` builds one, `$sh.status`
and its siblings hold one, and anything command-shaped yields one: a function whose
body ends in a command, a value call on a command (`grep(zzz)` is `status(1)`), and
a capture record's `.value` and `.status`. It exists so that forwarding a status
forwards the *failure* — `func w() { some-cmd; return $sh.status }` reports what
`some-cmd` reported, where a bare integer there returns the number, successfully.

```mesh
missing = status(5)
puts $missing                         # 5      — the bare number, at every byte boundary
puts $missing:repr                     # status(5)
puts $missing:code                     # 5      — the integer, for arithmetic and comparison
```

A status **renders as its number** wherever bytes are wanted — argv, interpolation,
`puts`, a valued option's payload, a path-type `$env` list, and a map key — since
mesh already loses type there (the int `5` and the string `"5"` both write `5`, and
`[5: found]` and `[status(5): found]` key the same entry). `:repr` writes
`status(5)`, which is forced by its round-trip contract, and
[`:code`](#modifiers) is how you reach the integer.

It compares like every other type, which is to say **strictly**: a cross-type
`==` **reports** rather than answering `false`, so `status(0) == 0` and
`status(0) == true` are both errors naming the spelling to use instead, and
`$s > 1` is an error too. `$s == status(0)` and `$s:code == 0` are what work. So
a `$sh.status == 0` written out of shell reflex no longer says nothing quietly —
it says what to write.

The reason it compares with neither an int nor a bool is that a status **admits
both readings**: `$s:code` is its integer and `not not $s` is its success, and an
equality respecting both would make `0 == status(0) == true` — hence `0 == true`,
which `if 0` refuses to let you even ask. It respects neither, and each reading
keeps its own spelling.

One seam, deliberate and worth knowing: the refusal is the `==` / `!=` operator
only. Underneath, equality stays total — so a `0` arm in a `match` on a status is
silently **skipped** rather than reported, `1 in [1, "a"]` answers, and
`[1, 1, "1"]:dedup` gives two elements. That totality is what `:dedup`, map keys
and `match` dispatch are built on.

A **condition** — the subject of `if` / `while`, a `stmt if cond` guard, a `match`
arm guard, or an operand of `and` / `or` / `not` — is a **bool, a status, or a
command**, and nothing else. A command branches on its exit status (`0` is true); a
status is true iff its code is `0`; a bool branches on itself. Any other type is a
loud error naming the comparison to write instead, so `if $xs:len` is refused and
`if $xs:len > 0` is what you write. There are no truthy values: a status is admitted
because success and failure are the whole of what it encodes, which is what a
command in a condition was already being read for.

```mesh
if $sh.status { puts "that worked" }
puts warn unless $sh.status
bad = $sh.pipestatus:filter(func(c) { not $c })
```

A **value** used as a statement reports the status *view* of that value: a status is
its own code, `false` fails, and every other value *is* a result, so producing one is
success. So `1 == 2 || puts nope` prints, a function whose body ends in a boolean
fails when that boolean is false, a body ending in an integer, string or list
succeeds, and `status(1) || puts fallback` runs the fallback. Naming a status is
[`fail`](#fail)'s and `status`'s job, not a bare `return`'s.

### `$sh.status` and `$sh.pipestatus`

`$sh.status` is the last command's status — the readable replacement for `$?` — and
a [status value](#exit-status) rather than an integer, so `return $sh.status`
forwards a failure and `if $sh.status { … }` is how you ask whether it worked.
`$sh.pipestatus` breaks the same run down by stage, as a **real list** of statuses:

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
bad = $p:filter(func(c) { not $c })
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
| trailing `/` | Directories only — `*/` is the subdirectories, each spelled with its trailing `/` (`sub/`). |

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

In a value position a pattern's result is a **list** however many paths it
matched, so `xs = *.rs` is a one-element list when one file matched and
`$xs:len` counts files rather than the characters of the only name. A word whose
metacharacters do not form a pattern — an unclosed `[`, say — is the literal text
it looks like, exactly as in an argument, and so is an ordinary string: `x = a[`
binds `a[` and `$x:len` is 2. Only a word that reaches the filesystem is a list.

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
| `empty: true` | A zero-length **regular file**, or a directory with no entries. Nothing else is empty — a fifo, a socket and most device nodes report a zero length without that describing their contents, the line `find -empty` draws. |

```mesh
for f in *(f) { process $f }   # plain files, no `if $f:type == dir { continue }`
puts *(d)                      # just the directories
puts *(f, x)                   # executable files
puts **/*(f, empty: true)      # every empty file below here
```

A type is read from the link itself, so `l` means the symlink; `exec` and
`empty` follow it, since a symlink's own mode is `0777` and would otherwise make
every link "executable". `*(l, x)` is then "links to something runnable".

Each **dimension** may be answered once, since the comma is an `and`: `*(f, d)`
and `*(exec: true, exec: false)` are syntax errors rather than one qualifier
quietly winning. A path has exactly one type, so "either" is spelled with the
`|` alternation and nothing else.

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

```mesh
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

Note `'…'` takes escapes too, unlike every other shell, so a pasted program that
carries its own backslashes needs the **raw** form. The error says so:

```text
sed 's/\(a\)/[\1]/' file
mesh: syntax error: invalid escape \(; for text holding its own backslashes
(a sed or awk program, a Windows path) use a raw string, `r'…'`
```

**Every quoted form spans lines.** A string runs until its closing quote, wherever
that falls:

```mesh
sed r's/x/y/
s/a/b/' file
```

Interactively and on piped input the reader waits for the closing quote, showing
the continuation prompt while it does — the same as an open `{`. The cost is the
same too: a quote you never close swallows what follows until you close it or
press Ctrl-C, and the error arrives at end of input rather than on the line that
opened it.

This is the **quotes only**. A bare `${x` cannot be continued — a reference is
scanned by its characters and a newline ends it — so it is reported on its own
line, and what follows still runs.

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
never re-globbed, however many spaces or `*`s it contains. A capture that fails
substitutes what it printed, the same as anywhere else — and interpolating it into a
command **loses its status**, since the command reports its own, so bind it with
`if out = $(…)` first when the failure matters. A syntax error inside it is reported
when the line is parsed. `'…'` and `r"…"` are literal, and `\$(` keeps the text in a
double-quoted string.

## Variables

```mesh
name = value          # spaced form
name=value            # unspaced form
```

A name starts with a letter or `_`, then letters, digits, `_`, and interior `-` (a
hyphen must sit between two name characters). A bare `_` is not a name: it is the
**discard**, which discards a *position* inside a pattern, so `_ = 1` and
`global _ = 1` are refused — there is no position there to discard, and nothing
to assign to. Write a name, or write the value on its own to run it for its
effect. At top level, bindings are session-global.

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
Names and places may be mixed in one statement (`unset p $m.k q`).

`$env` **is** a place here, exactly as it is on the assignment side —
`unset $env.KEY` and `unset $env[$name]` remove the entry from the process
environment, so children stop inheriting the name rather than inheriting it empty
(see [The environment](#the-environment)). It is not a scope, so `global` does not
apply to it. In `$sh` only a [hook](#the-hook-maps) can be removed —
`unset $sh.exit.tmp` is `on --remove exit tmp` — because writable is not the same
as removable and the two writable corners differ on exactly that. A hook map's
keys are yours, and retiring a handler is what removing one means; the
[settings](#shoptions) are a fixed set, so `unset $sh.options.bold-input` is
refused rather than restoring a default. Everything else in `$sh` is read-only,
and is refused by name. `$sh` is no more a scope here than it is on the
assignment side, so `global unset $sh.exit.tmp` is refused too rather than
removing the hook.

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

```mesh
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
syntax error), nor is a slice — `$xs[0..2] = …` names a copy of a run of elements,
and a length-changing assignment has no defined meaning yet. Both are refused
while the line is *read*, so the value on the right never runs: `$xs[0..2] = $(cmd)`
does not run `cmd`. A computed index is the exception the grammar cannot see —
`$xs[$i] = …` is not known to be a range until `$i` is evaluated — so that one is
still reported by the write itself, after the value has been produced. `$env` and
`$sh` keep their own handling: see [The environment](#the-environment).

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
export A=1 B=2 C=3            # a whole run at once, unspaced
```

**`$env[…]` writes by computed name**, which is the twin of the computed read:
any expression that names an entry to read names one to write.

```mesh
name = EDITOR
$env[$name] = vim             # the same write as $env.EDITOR = vim
$env[$name] += " -u NONE"
```

That is what lets a set of changes be applied as *data* rather than as source —
the shape every `shellenv`-style integration has, and what direnv needs (mise too,
once it has a source that reports removals — see [`INTEGRATION.md`](INTEGRATION.md)):

```mesh
for name, value in $changes {
    $env[$name] = $value
}
```

**`unset` removes an entry**, through either spelling, so a child sees the name as
unset rather than as empty — a distinction `${VAR-default}` turns on in every
POSIX shell:

```mesh
unset $env.EDITOR
unset $env[$name]
```

Removing what is not there is a loud error, as it is for any other `unset`
target. The environment is the process's rather than a scope's, so `global` does
not apply to either the write or the removal — both are already global.

A computed name is only known once it is evaluated, so the three names the
process **cannot** hold are reported there: an empty name, one containing `=`, and
one containing a NUL. Every other name is allowed, including ones mesh's own
grammar could not spell as `$env.KEY` — `$env:keys` can hand you one, so a round
trip over the listing has to be able to write back what it read.

An environment write is **global on purpose**, even inside a function: changing
what children inherit is the point, so it persists after the function returns.
To scope one to a block instead, see [`with`](#with--the-environment-for-one-block).

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
$env.PATH = $env.PATH:prepend(/opt/bin):dedup   # both, in one statement
puts $env.PATH[0]
puts $env.PATH:len
$env.PATH[0] = /opt/bin       # replace one entry, in place
```

An **element** of one is a place, so the last line needs no temporary. It is a
whole-entry write underneath — the entry is read, the element changed, and the
whole list joined back — so a child sees the new value the way it sees any other
environment write, and `+=` there appends to the element rather than to the list
(`$env.PATH[0] += /bin` makes the first entry `/opt/bin/bin`). Only path-type
names have elements: `$env.HOME[0] = /x` reports that there is nothing to index,
the same way reading `$env.HOME[0]` does. `unset` does not follow — dropping one
directory is not what removing an entry means, so `unset $env.PATH[0]` is
refused.

The set is fixed for now: `PATH`, `MANPATH`, `CDPATH`, `INFOPATH`,
`LD_LIBRARY_PATH`, and `PYTHONPATH`. (`export --list NAME`, which would opt an
arbitrary name in, is not implemented.) Because these read as lists, `$env.PATH`
needs a spread or a join to reach an external command like any other list
(`puts $env.PATH` prints one entry per line). Splitting is **exact** —
every empty component is kept, since `PATH=/usr/bin:` means "…and the cwd", and a
split/join round trip is byte-faithful.

#### `CDPATH`

`CDPATH` is a search path for [`cd`](#builtins) the way `PATH` is one for
commands: a plain relative operand is looked for in each entry, in order, and the
first entry that holds a directory of that name wins.

```mesh
$env.CDPATH = ['', ~/src, ~/work]
cd mesh                        # ~/src/mesh, if that is where it lives
/home/user/src/mesh
```

Four rules, all of them POSIX's:

- **The first hit wins**, and entries are tried in the order written.
- **A hit through a non-empty entry prints where it landed**, as above, because
  the destination is not the one the operand appears to name. An **empty entry
  is the current directory** — the leading `''` above — so a match there is
  silent. That empty entry is how you say "prefer where I am"; without it, a
  `CDPATH` entry that holds `sub` beats a `./sub` under your feet.
- **A miss falls back to the current directory**, so setting `CDPATH` never
  breaks a plain `cd subdir`.
- **A dot-relative or absolute operand never searches.** `.`, `..`, `./x`,
  `../x`, and `/x` resolve from where you are, so `cd ../` cannot jump to a
  `CDPATH` entry. Neither does an empty operand: `cd ''` is an error, not a jump
  to the first entry.

`$env.CDPATH` is in the environment, so a child process inherits it — a mesh
script, and a `bash` or `zsh` started from here, all search the same path.

An `$env` target names a whole entry with **one access**: a member (`$env.KEY`) or
a subscript (`$env[$name]`). Any name you can read you can also assign, including a
kebab name like `$env.MY-VAR`. Past that, only an **index** goes further, and only
into a path-type entry, which is the element write above (`$env.PATH[0] = …`); a
`.member` under an entry (`$env.HOME.x = …`) is a syntax error naming the entry a
write would replace, since no entry is ever a map. `$env.PATH:dedup = …` and
`$env[0..2] = …` describe derived values rather than places, so they are syntax
errors about places. Of the other spellings, only `export --list NAME` is still
unimplemented; `export`, `with`, and the `NAME=value` command prefix take a
spelled-out name only, since each is a header whose names are read at parse time.

`+=` works on the raw bytes already in the environment, so a value that is not
valid UTF-8 survives being appended to. Reading such a value into mesh still
renders it lossily, so `$env.K = $env.K` — an explicit read and write back —
does not round-trip; that waits on `OsString`-backed words.

Member access and list/map indexing have the same meaning inside `"…"` as they do
outside it. A slice remains a list and needs `...` to reach an external command;
omitted
bounds and negative bounds are supported. Use braces to delimit a reference
before literal text: `${x}.txt`.

The brace is **required** there, which is the one place mesh asks for something
every other shell lets you leave out. `"$file.bak"` reads `.bak` as a member of
`$file`, so on a string it is an error rather than the text you meant — and the
error says so by name, since the mistake is easy to make and lands at run time:

```
$ file = report; puts "$file.bak"
mesh: $file.bak: a string has no members; write `${file}.bak` if the rest is literal text
```

**`${…}` also takes an expression**, not only a reference — a call, or arithmetic:

```mesh
func host-info() { "host" }
puts "${host-info()} at ${$n + 1}"
```

This is what lets a function compose into a string without being bound to a name
first. Two things follow from the quotes:

- **A call is a call, not a command.** `"${f()}"` takes `f`'s **value**, while
  `"$(f)"` *runs* `f` and captures what it **printed**. For a segment-style
  function that returns rather than prints, the two differ silently rather than
  loudly:

  ```mesh
  func host-info() { "host" }
  puts "[${host-info()}]"      # [host]  — the value
  puts "[$(host-info)]"        # []      — it printed nothing
  ```
- **The quotes mean "one string".** A scalar renders — an integer and a boolean
  included — while a list, map, or handle is a loud error, the same answer `"$xs"`
  gives. Spell the join (`"${$xs:join(" ")}"`) when the elements are what you want.
  A [styled value](#styled-values) contributes its text and leaves its attributes
  behind, exactly as `"$styled"` does — quote it and you have asked for the text.

**The sigil-less reference form covers the whole access** — a name, its members, its
indices, and its modifiers, whether or not those modifiers take arguments. Adding an
argument does not change what the body means:

```mesh
xs = [a b]
puts "${xs:len}"            # 2
puts "${xs:join("-")}"      # a-b   — still the binding
puts "${$xs:join("-")}"     # a-b   — the same thing, spelled as an expression
```

**The expression form is ordinary mesh, so a variable keeps its `$`.** That is the
seam worth knowing: the two spellings above agree, but the moment a body stops being
an access — a call, arithmetic — it is an expression, where a bare word is the *word*
and a binding needs its `$` (`${$n + 1}`, not `${n + 1}`).

**An expression body may wrap**, the way a `( … )` group or a `$( … )` body does —
a newline inside the braces is layout, not a terminator. It still holds exactly one
expression, so a second one is a syntax error however it is spaced.

A malformed `${…}` (no closing `}`, or a body that is neither a reference nor an
expression) is a syntax error. A `$` not followed by a name (`$5`) is a literal
`$`; a literal `$` in a string is `\$`.

## Modifiers

Postfix modifiers apply from left to right after a variable, member, list access,
or a **literal** — `abc:upper` is `ABC`, the same as `$x:upper`. They work in bare
and double-quoted interpolation, in every position a word can be written; braced
form puts the modifier inside the braces (`${file:stem}`).

**`:` followed by an identifier is reserved by the grammar**, so a name that is not
a modifier is a syntax error rather than literal text. The *shape* is what reserves
it, never the list of names mesh implements — otherwise adding a modifier would
silently change what an existing string means, and `"$h:port"` would be text until
the day `:port` shipped:

```text
puts ubuntu:latest
mesh: syntax error: `:latest` is not a modifier; quote the whole word to keep it
as text (`"x:latest"`), or brace the name when it comes from a variable
(`"${x}:latest"`)
```

Quoting the *subject* does not help — `"ubuntu":latest` is the same chain. The
colon has to be inside the quotes (`"ubuntu:latest"`), or the name braced when it
interpolates (`"${image}:latest"`).

Only a **bare identifier** after the colon is claimed, so ordinary punctuation is
untouched: `key:2`, `key:/path`, `key:`, `http://x` and `$host:$port` all keep
reading as text. A `[…]` literal's `key:` is a map key, not a chain on the key, so
`[host:upper, port:22]` is a map.

The chain also outranks keyword parsing, so `if:upper` is `IF` rather than the
start of a conditional; `if :upper` — with the space — is still the keyword.

Inside a `"…"` string a `$…` reference is scanned by its **characters**, which
stop at a `(`, so a modifier that takes arguments has nowhere to put them there.
That is a syntax error naming the spelling that does work — `${…}`, whose body is
an expression:

```text
puts "$env:get(HOME, none)"
mesh: syntax error: `:get` takes arguments, which a `$…` interpolation cannot
pass; brace it instead (`"${x:get(…)}"`)

puts "${env:get(HOME, none)}"     # /home/user
puts "${$env:get(HOME, none)}"    # the same — the `$` is optional here
```

The braced body takes the name **sigil-less**, exactly as the argument-free
`${file:stem}` does, so adding an argument does not change how the head reads.
Only an *abutting* `(` after a name that **takes** arguments is this shape; after
an argument-free modifier a `(` is ordinary text, so `"$x:upper(foo)"` is
`AB(foo)` and `"$x:upper (1)"` keeps its reading. `"$x:nosuch(1)"` reports the
unknown `:nosuch` instead — the name is resolved before its arguments are reached.

A bare `$name:mod` chain inside a `"…"` string reads the same wherever the string
sits, so `puts "$x:upper"` and `y = "$x:upper"` both give `AB`, and the error above
is reported in either. That was once true in command position only — the value
reading bound the literal `ab:upper`, silently — which is why the braced form is
still the safer habit in code that has to run on an older mesh.

A name mesh **reserves** for a modifier it has not built yet — `:sort`,
`:replace`, and the rest of the `DESIGN.md` set — parses, then reports a
loud `not implemented yet` in a value context rather than a silent no-op. That is
a different failure from an unknown name, which never parses.

| Modifier | Input | Result |
| --- | --- | --- |
| `:dir` | string or list | Parent-directory portion. |
| `:base` | string or list | Final path component. |
| `:ext` | string or list | Last extension, without the dot. |
| `:exts` | string or list | All extensions, without the first dot. |
| `:stem` | string or list | Basename without the last extension. |
| `:bare` | string or list | Basename without any extensions. |
| `:real` | path or list | The path with every symlink, `.` and `..` resolved, absolute. Errors on a path it cannot resolve. |
| `:url` | path or list | The path as a `file://host/path` URL, absolutized but not resolved. Errors on a path holding a `..`, and on the empty string. |
| `:upper` / `:lower` | string or list | Change case; maps over list elements. |
| `:int` | string | Parse an integer, failing loudly on invalid input. |
| `:bool` | string or boolean | Parse `1`/`true`/`0`/`false`; warn and read `false` for anything else. `:bool(DEFAULT)` answers `DEFAULT` there instead, and says nothing. |
| `:len` | string, list, or map | Character, element, or entry count as an integer. |
| `:code` | status | The integer inside a [status](#exit-status) — the spelling for arithmetic on one, and for a comparison against a number (`$s:code > 1`, where `$s > 1` is a type error). |
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
| `:repr` | any value with a literal form | The value written as the mesh source you would have typed for it, as a string. Always one line. |
| `:pretty` | any value with a literal form | The same literal as `:repr`, laid out over lines with two-space indentation. Every collection breaks; a scalar and the empty `[]` / `[:]` stay as they are. |
| `:tty` | stream handle | Is that stream a terminal? The `test -t N` replacement — see [`$sh.args` and `$sh.name`](#shargs-and-shname). |
| `:words` | string | Split on runs of whitespace into a list — the IFS word-split. Never yields an empty element. Alias `:ws`. |
| `:lines` / `:nulls` / `:tabs` | string | Split on newline / NUL / tab into a list. Each is `:split(SEP)` with the separator its name spells. Aliases `:ls` / `:ns` / `:ts`. |
| `:raw` | a `$(…)` capture | The captured bytes as one string, trailing newline intact — the **no-split** member of the split family. |
| `:split(SEP)` | string | Split on the literal separator into a list. |
| `:join(SEP)` | list | Fold the list into a string, `SEP` between elements. |
| `:get(KEY, DEFAULT)` | map or list | **Total** access — `DEFAULT` when the key or index is absent. |
| `:has(VALUE)` | map or list | Membership, as a boolean — a map is asked about a **key**, a list about a value. |
| `:prepend(VALUE)` / `:append(VALUE)` | list | A new list with `VALUE` added at the front / the back as **one element**, whatever it is. |
| `:extend(LIST)` | list | A new list with `LIST`'s **elements** added at the back. Requires a list. |
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
`:real` errors for the same reason: resolving is a syscall, and every component
on the way has to exist for the kernel to follow it, so an unresolvable path has
no real path to report rather than a `false` to give.

All of that is about the modifiers that **look the path up**. `:url` never does —
it is string work over the path, plus the working directory when the path is
relative — so it neither dereferences a link nor errors on a path that is not
there: `$link:url` names the link, and `$link:real:url` names what it points at.

Note that a searchable directory carries the execute bit, so `:exec` alone keeps
directories; `:f:x` is the executable-files idiom. List results retain their type: use `...$xs:rest` in command position,
or bind them directly with `ys = $xs:rest`.

`:url` names a path the way a URL reader wants it, which is what
[`link`](#hyperlinks) and anything else taking a `file://` needs:

```mesh
puts /etc/hosts:url             # file://host/etc/hosts

report = report.html            # a dotted literal takes no chain — bind it first
puts $report:url                # file://host/home/user/report.html — this host, this directory
puts link($report:base, $report:url)   # a clickable file, named by its basename
```

The **host** is in there because that is what lets a terminal tell a local file
from one inside an `ssh` session, and everything RFC 3986 forbids raw is
percent-encoded — a space would otherwise end the URL, a `#` start a fragment. It
shares its encoder with the `OSC 7` sequence mesh writes on every `cd`, so the
shell and the terminal name a file the same way.

A relative path is absolutized against the working directory, and the **subject
itself is never looked up** — which is the split with `:real`: `:url` names a
file that does not exist yet, where a resolution has nothing to resolve. The one
thing it does need is that working directory, so a *relative* subject fails,
saying so, in a shell whose directory has been removed out from under it; an
absolute subject asks for nothing at all. What it refuses outright
is a `..`, because the two ends disagree about what one names — RFC 3986 §5.2.4
has a reader remove dot segments *before* opening anything, while the kernel
follows each symlink first and applies `..` to wherever it landed, so
`a/link/../report` is two different files that can both exist. `:real:url` is the
spelling that resolves it. A `.` needs no refusal; the empty string is refused
rather than quietly meaning the current directory.

`:prepend`, `:append`, and `:extend` are the **pure** counterparts of `+=`: they
return a new list rather than writing one, so they compose in a chain where a
statement cannot.

```mesh
xs = [b c]
ys = [d e]
puts $xs:prepend(a):join(" ")      # a b c
puts $xs:append($ys):len           # 3        — one element; $ys nested whole
puts $xs:extend($ys):join(" ")     # b c d e  — $ys's own elements
puts $xs:join(" ")                 # b c      — the subject is untouched
```

**None of them reads its argument by type.** `:append` adds exactly one element
whatever it is; `:extend` adds a list's elements and errors on anything else,
naming `:append` when it does. Which you meant is in the name, decided where you
write it rather than inferred from the value's shape — `+=` dispatches on the
right-hand type instead, and that is the one place mesh flattens by type rather
than by an explicit `...`.

`:extend` has no front-loading twin; `[...$ys ...$xs]` is the spelling for that.
All three are lists only: a map has no front or back to add to (its `+=` is a
merge), and a string already has `+=` and interpolation. The payoff is the
guarded PATH in one statement, where the mutating form needs two:

```mesh
$env.PATH = $env.PATH:prepend(/opt/bin):dedup
```

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
collection **reads** — one element or `key: value` per line, nesting by indent and
bullet, with `42` and `'42'` printing alike. That is also why the two disagree
about a nested value: `puts` lays it out to be read and cannot be read back,
`:repr` writes it on one line and can be.

A value with **no** literal form is a loud error rather than an approximation
that would read back as something else: a stream handle, a function, a glob
(writing the pattern back would re-glob it), and for now a regex, whose flags
ride on `:` modifiers that are not implemented yet.

```mesh
puts $sh.stdin:repr           # mesh: :repr: a stream handle has no literal form
```

The guarantee is worth stating plainly: **whatever `:repr` returns, reading it
back gives the same value.** Anything that would not is an error instead.

`:pretty` is that same literal laid out over lines, for the sizes where one line
stops being readable — a whole `$env`, a config map, anything a few levels deep.

```mesh
m = [a: [b: [1, 2]], c: 3]
puts $m:pretty
# [
#   'a': [
#     'b': [
#       1,
#       2
#     ]
#   ],
#   'c': 3
# ]
```

**Every** collection breaks, with no size threshold: a rule like "short values
stay inline" would mean you cannot tell which form you will get without counting
characters, and the compact form already has a name — `:repr` *is* this value on
one line. A scalar and the two empty spellings have nothing to put between the
brackets, so they are written as they are. The indent is two spaces, the same
width `puts` uses for nesting, so the read-it and read-it-back forms of a value
line up.

The round-trip guarantee is unchanged, which is what makes the layout safe here:
the brackets and commas still say where each value starts and ends, so the
indentation is decoration over a spelling that already parsed. `puts` could not do
this — it quotes nothing, so there the layout would be the only thing carrying the
structure. `:pretty` refuses exactly what `:repr` refuses, by the same name, so a
value with no literal form never becomes an approximation.

It is a separate modifier rather than a flag on `:repr` because `:repr` keeps
meaning *one line*: `$a:repr == $b:repr` is a working way to compare two values,
and a one-line literal is what makes its output something you can paste back.

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

`:words` is the other member of that family built so far, and takes no argument:
it splits on **runs** of whitespace and yields no empty element anywhere — leading,
trailing and interior runs are each one boundary, so `"  a   b ":words` is
`[a b]`. That is the difference from `:split(" ")`, which would answer
`["" "" a "" "" b]` on the same string, and it is what makes `:words` the way to
read column-padded output: `getent`, `ip -o`, `df` and `ps` all align their
columns, so every index after the first is wrong under a literal split. It is
bash's `read a b c` and awk's default `FS`, and it composes with list-pattern
binding the same way:

```mesh
[user _ uid] = $line:words       # take a padded line apart positionally
count = $line:words:len          # how many columns
```

The whitespace is the **ASCII** set (` `, `\t`, `\n`, `\r`, and the vertical and
form feeds). A non-breaking space is *data* — it appears inside filenames — so it
stays in the field rather than splitting it.

`:lines`, `:nulls` and `:tabs` are the fixed-separator members of the same family
— each is `:split(SEP)` with the separator its name spells, terminator rule and
all, so `"a\nb\n":lines` is `[a b]` and `"a\n\nb\n":lines` is `[a "" b]`. The
useful one is `:nulls`, which splits on NUL and **nothing else**: a `find -print0`
name that contains a newline arrives whole rather than torn at the newline.

Each fixed-separator member carries a two-letter alias — `:ls` `:ns` `:ts` `:ws` —
because a split is what a line loop or a `-print0` pipeline writes on every use.
They are systematic (initial plus `s`) rather than borrowed from `test` the way
the single letters `:f` `:d` `:l` `:x` are, which is also why none collides: `:l`
is already `:links`.

All of these are **split** modifiers, which consume exactly one string: none maps
element-wise over a list, and a list subject is a loud `requires a string` naming
the modifier that was written. To take a list of lines apart, hand `:words` over
as a callable — `$lines:map(:words)` — which works because it takes no argument.

`:get(KEY, DEFAULT)` is the **total** accessor, where `$m.key` and `$xs[i]` fail
loud: it answers `DEFAULT` when the key or index is absent, which is what makes
`$env:get(EDITOR, vim)` the mesh spelling of `${EDITOR:-vim}`. A map takes a
string key and a list an integer index, negative counting from the end. Note the
one difference from bash: a key bound to `""` is **present**, so it wins over the
default, where `${EMPTY:-x}` substitutes. Asking a map for an integer — or a list
for a name — is a loud error rather than a silent default: a key of the wrong
*type* is a mistake in the program, not an absence in the data. A bare `$env` is
the whole environment as a map, which is what gives `:get` an ordinary map to
work on; `$env.NAME` stays the strict read that errors when unset. `puts $env`
prints it under the ordinary nesting rule — the path-type names are lists, so they
render as indented blocks under their keys — with no rule of its own. To read one
name, reach for `$env:keys`, `$env:get(NAME, …)`, `$env.NAME`, or — for a name held
in a variable — `$env[$name]`, which has a
[writing twin](#the-environment).

```mesh
editor = $env:get(EDITOR, vim)
puts $env:get(MESH_DEBUG, false)
xs = [a b c]
puts $xs:get(9, "-")            # -
```

`:has(VALUE)` is the membership guard beside it, answering a boolean: a map is
asked whether the **key** is present and a list whether any element equals the
value — the same two questions `in` asks of each subject, by the same equality.
`if $env:has(SSH_AUTH_SOCK) { … }` is the guard form, and the wrong-type
refusal is `:get`'s: asking a map with anything but a string is a loud error
rather than a quiet `false`.

`:bool` parses a string as a boolean and is the twin of `:int`, with one
deliberate difference: it does **not** raise on input it cannot read. It warns and
answers `false`. The asymmetry is the types', not a preference — a boolean has a
safe stand-in and an integer does not, so "I could not read this flag, so it is
off" is a real answer where "I could not read this number, so it is 0" would be a
fabrication.

The spellings are `1` / `true` and `0` / `false`, and there are no others. `true`
and `false` are what mesh itself writes, so a boolean round-trips; `1` and `0` are
what every shell flag already uses. A third vocabulary — `yes`, `on`, `y` — is
where a parse becomes a dialect, and each synonym admitted forces a ruling on its
opposite, so anything outside the four is reported rather than guessed at.

```mesh
puts "1":bool                   # true
puts "yes":bool                 # false, and a line on stderr saying so
puts "yes":bool(true)           # true, and nothing on stderr
```

`:bool(DEFAULT)` is the quiet form. Naming a default *is* the statement that an
unreadable value is expected, so mesh stops mentioning it — the same bargain
`:get(KEY, DEFAULT)` makes, which says nothing about the key that was missing. A
value the modifier *can* read is never the default's business in either form.

A boolean subject is the identity, which is what lets `:bool` follow a `:get`
whose default is the bare literal `false`:

```mesh
if $env:get(FAILSAFE, false):bool { puts "failsafe mode" }
```

That is the shape to reach for on an environment flag. Written as a comparison it
needs quotes on both sides — `$env:get(FAILSAFE, "0") == "1"` — and the quotes are
load-bearing rather than decorative: a bare `1` is an integer literal, equality is
type-strict across string and number, so `== 1` is *always* false and the flag
silently never fires.

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
list** — `$paths:stripend(".js")` rewrites each path — except `:get` and
`:has`, which consume the collection as a whole.

A split modifier written **directly on a `$(…)`** binds the capture's *raw*
bytes: it replaces what the capture would otherwise have done rather than running
after it, so `$(printf "a:\n"):split(":")` is `[a "\n"]` — the trailing newline is
still there to be split on. That is what makes `:nulls` safe on `find -print0`
output, and it is the whole job of **`:raw`**, the no-split member, which hands
back one string with the trailing newline the default trims (`$(cmd):raw`).

Raw binding is a property of the *spelling*, not of the bytes: once a capture is
in a variable it is an ordinary string, so `x = $(cmd)` then `$x:split(":")`
splits the already-trimmed value. Whether a line binds raw is readable from that
line. Argument-taking modifiers work in expression position (an assignment right-hand
side or other value context) and in command-argument position
(`echo $dirs:join(":")`). Not yet: the **spread** of one at a command boundary —
`puts ...$x:split(":")` is a syntax error, so bind it first (`xs = $x:split(":")`,
then `puts ...$xs`).

Bare decimal literals and `true` / `false` produce typed integer and boolean
values. Arithmetic requires integers, comparisons return booleans, and strings
are never implicitly parsed as numbers.

A decimal literal types as an integer only when its text is that integer's **own**
spelling, so `7` and `-2` are integers while `007`, `08`, `+5` and `-0` are
strings. An integer carries no record of how it was written, so anything else
would be re-rendered from the number and the text lost — `007` reached a command
as `7`, and putting a `func` in front of a command changed what the command
received. Spelling wins because a numeral whose spelling matters is usually an
identifier rather than a quantity: a mode, a version segment, a zero-padded
index. Arithmetic on one asks for the conversion (`$n:int + 1`), which is the
rule every other string already follows.

`1.0`, `1_0`, `0x10` and `1e3` are also strings today, but for a different reason:
**there is no float type**, and the grouped, radix and exponent forms are **not
implemented yet**. `DESIGN.md` decides them as integer and float literals, so this
is a gap rather than the rule above — and when they land, each will have to settle
which text *it* renders back to, since `0x10` returning as `16` would lose a
spelling exactly as `007` did.

Two consequences of the missing float are worth stating outright, because they
read as working code:

```mesh
(1.0 + 1)      # error: expected integer — `1.0` is the string '1.0'
(10.0 < 2.0)   # true  — ordering falls through to text, so decimals sort
(0.5 < 0.10)   # false   lexicographically rather than numerically
```

Ordered comparison has one numeric path (int against int) and otherwise compares
text, so a decimal-looking string sorts as a string. `1.0 < 2.0` answering
correctly is a coincidence of digit order, not arithmetic.

Integers and booleans have canonical
command/interpolation renderings (`42`, `true`, and `false`). Lists and maps keep
requiring an explicit spread, access, or modifier at the byte-oriented command
boundary. A whole typed value, including a list or map, passes unchanged as one
positional argument to an in-shell function.

## Operators and matching

Value expressions support integer arithmetic (`+`, `-`, `*`, `/`, `%`), unary
`-`, equality (`==`, `!=`), ordered comparisons (`<`, `<=`, `>`, `>=`),
membership (`in`), and boolean `not`, `and`, and `or`. Ordered comparisons
require two integers or two strings; arithmetic never implicitly parses a
string (use `:int` explicitly). Comparisons cannot be chained, and neither can
ranges: `1 .. 2 .. 3` is a syntax error, since a range is not an endpoint.

**Equality is same-type only.** `==` and `!=` **report** when their two operands
are different types rather than answering `false`, because a quiet answer is
indistinguishable from a real inequality:

```mesh
1 == "1"           # error: cannot compare an int with a string
$sh.status == 0    # error: cannot compare a status with an int; …
1 == 1             # true
style("a", fg: red) == "a"   # true — a styled value is its text
```

A styled value and a plain string are one type for this, the only grouping the
rule makes. The refusal is the **top-level operands of `==` / `!=`** and nothing
else: nested pairs, `in`, `:dedup`, map keys and `match` literal arms all use
total equality and answer `false` rather than reporting, since each of those can
only accept a bool. `DESIGN.md`
§"Comparison across types" states that seam and why it is scoped this way.

`not` is a **reserved word**, and it negates one of two things: a **value**, or a
**command's status**. Which one is decided by the operand — a value after it is the
boolean operator, anything else is a command whose exit status is inverted. Both
readings are available in a **condition** (`if` / `while`) and in **statement**
position, which are the two places a command can be written at all:

```mesh
not $b               # the value operator
not have-command(x)  # likewise — a call is a value
not false            # likewise — `true` / `false` are boolean literals

not test -f $config  # negates the command's status: 0 if the file is missing
not grep -q x $file  # `puts found unless …` spelled the other way round
not sh -c $probe | cat   # a pipeline negates as a whole
```

A **postfix guard** and an **assignment's right-hand side** take a value expression
and nothing else, so `not` there is the value operator alone — the same limit those
positions already have without it. A guard given a command is not a guard (see
[Postfix guards](#postfix-guards--if-and-unless)), so `puts ok if not test -e /`
passes those words to `puts` as arguments, and `x = not test -e /` is a syntax
error at the operands. Use a full `if` for a command condition.

`not` still never names a command *itself*, however the line continues. The escape
hatches are the ones any reserved word has — a path or a quoted word:

```mesh
./not foo            # runs the program `not`
"not" foo            # the same, spelled as data
```

`not` as *data* is untouched, since only the command-word position is reserved:
`puts not` prints it and `x = "not"` stores it. A run of `not`s folds to its
**parity** — `not not not $x` is `not $x`, and any even run is the `not not $x` that
coerces truthiness to a bool without inverting it. The same parity applies to a
negated command.

Because a value operand wins, the *programs* `true` and `false` are not reachable
through `not` — but nothing is lost by it, since a boolean and a command exiting `0`
mean the same thing: `not true` and `not /bin/true` both answer false. A command
needs a word to name it, so `not = 5` stays a syntax error rather than trying to run
`=`.

As a **statement**, the negated status is the result, and `$sh.status` carries it —
the rule a value statement already follows, where bare `false` exits `1`:

```mesh
not sh -c 'exit 3'          # status 0
not diff old new && puts differed
```

`$sh.pipestatus` follows `$sh.status`, so a negated statement reports one stage: the
negation produced the code, so there is no per-stage breakdown that explains it. In a
**condition** the negation is only a reading and the command publishes its own code,
so the breakdown survives there — see [Conditionals](#conditionals).

A negated command cannot be backgrounded (`not cmd &` is refused): the status to
invert arrives when the job is waited on, not when it is launched. Bind the job and
negate the wait instead.

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

A slash-delimited regex is recognized in three slots: the right operand of `~` or
`!~`, a `match` arm, and the pattern a replace takes. Its body is raw except that
`\/` includes a literal slash, and it reaches the end of its own closing `/`
rather than the end of a word — so the characters a pattern is made of (`[`, `(`,
`{`, `|`, `,`, `:`, and a space) sit inside one:

```mesh
letters = abc ~ /[A-Za-z]+/
grouped = abc ~ /a(b)c/
either = abc ~ /abc|xyz/
```

The closing `/` has to end the word, which is what keeps a path a path: `/usr/bin`
is the glob it looks like, since the slash before `bin` has a word character after
it and so closes nothing. A pattern that needs an interior slash writes it `\/`.

Some things are read before the literal is, so they cannot appear inside one, and
`re(…)` takes every one of them as ordinary text:

| In a literal | Read first as | Reported |
| --- | --- | --- |
| `#` with whitespace before it | a comment (an attached `a#b` is not) | yes |
| an unmatched `'` or `"` | an unclosed quote — so `/['"]/` needs `re("[\"']")` | yes |
| `<<` | a heredoc | yes |
| an unbalanced `}` or `)` in a `${…}` or `$(…)` body | that body's closing delimiter | no |

A *balanced* pair is fine in a body, which covers the shapes patterns actually
use: `/a{1,2}/` and `/a(b)c/` both work there and at top level.

A trailing `\` continues the line as it does anywhere else, and the two lines are
joined before the pattern is read — so `/a\`⏎`b/` is `ab`, not a pattern holding a
newline.

Flags are postfix modifiers on the pattern, each with a short and a long
spelling:

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
`re(…)` value as readily as to a literal — so `:x`, whose whole point is a
spaced-out pattern, works with either:

```mesh
spaced = "555-1234" ~ /\d{3} - \d{4}/:x     # true — `:x` ignores the spacing
compiled = re("\\d{3} - \\d{4}"):x
```

Only a **flag** chain keeps a literal a pattern. `/a/:upper` is the string `/A/`,
which is what it means everywhere else, and reading it as a regex would both
change that and fail — `:upper` is not a flag.

Use `re(STRING)` to compile a regex for reuse or to build one from a value, and
`re(STRING, literal: true)` to quote regex metacharacters and match the supplied
text literally. A quoted string on the right of `~` is rejected rather than
silently treated as either a glob or regex.

## Conditionals

```mesh
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

An **assignment condition over a value** asks whether there *is* one, not
whether it is true: only `false` is absent, so `""`, `[]` and `0` all bind and
take the branch. That is what lets a function answer `false` for "found
nothing" and be tested for it where bash would test a status — and what ends a
`while` that binds its sentinel:

```mesh
func find-up(_name) { … }              # a path, or `false` on a miss
if path = find-up(Makefile) {
  puts "found $path"
} else {
  puts "none above here"
}

while line = next-line() { puts $line }   # ends when `next-line` answers false
```

A **status** is the other value-level failure, so a failing one takes `else`
here exactly as it does when it is the condition itself — otherwise `if s =
f()` would succeed where `if f()` fails, on the same value. Unlike `false` it
still **binds**, since a status is a result rather than an absence, so the
`else` branch can read the code:

```mesh
if s = build() { puts done } else { puts "build failed: $s" }
```

An absent value binds nothing, so the `else` reads whatever the name held
before — the same rule a list-pattern mismatch and a `gets` at end of input
already follow. A **capture** right-hand side is unaffected: `if out = $(cmd)`
still branches on the command's status, with the output bound either way.

A **command** condition is a command that ran, so the body it selects reads its
status — the code, not just the fact that it failed:

```mesh
if diff old new {
  puts identical
} else {
  puts "differed ($sh.status)"     # 1 for a difference, 2 for trouble
}
```

A **value** condition has no status to report, so it leaves the previous
command's standing — the same rule a guard that skipped its statement follows.
The `if` itself still reports its *body's* status, not its condition's.

`not` before a condition negates it. Before a **value** it is the boolean operator
of [Operators and matching](#operators-and-matching); before anything else it
negates the *command's* status, which is how a condition says "this failed":

```mesh
if not test -f $config { puts "no config" }
if not sh -c $probe | grep -q ready { puts "not ready" }
while not mountpoint -q /mnt { sleep 1 }

if not $ready { … }        # the value operator — `$ready` is a value
if not have-command(fzf) { … }   # likewise, a call is a value
```

A pipeline negates as a whole, and `not not` cancels. The negation is a *reading*
of the status, not a second run of it, so the command still publishes the code it
really exited with and `$sh.pipestatus` keeps its per-stage breakdown:

```mesh
if not sh -c 'exit 130' { puts $sh.status }   # 130, not 1
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

`for name in list { body }` runs the body once for each top-level element. An
element containing whitespace remains one value when read through `$name`;
braces may span lines. Empty lists run the body zero times.

A value that is **not a list is refused**, rather than run once — that is what
made `for line in $text` a silent wrong answer, binding the whole blob while
reading as though it iterated lines. The diagnostic names both fixes, since
which one is meant depends on what the string holds:

```mesh
for x in [$s] { … }        # one value, iterated once
for x in $s:lines { … }    # its lines
```

**The binding belongs to the loop.** It is fresh for each pass and gone when the
loop ends, so reading it afterwards is the usual unbound-variable error, and a name
the loop shadows is put back rather than clobbered:

```mesh
x = "before"
for x in [1 2] { puts $x }   # 1, then 2
puts $x                      # "before" — the loop's binding is over
```

That is what keeps a lambda written in the body from closing over one shared slot
and seeing only its final value — the footgun Go fixed in 1.22 and JavaScript fixed
with `let`, both in the loop rather than in the closure. A body that wants the
value takes it explicitly, and gets that pass's:
`func() with ($x) { … }` — and a `func` or `alias` *definition* in the body takes
the same list, for the same reason (see [Functions](#functions)).

Globs, ranges, `$sh.args` and any bound list are already lists and are
unaffected — a glob is a list however many paths it matched.
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

## `NAME=value cmd` — the environment for one command

A `NAME=value` run in front of a command puts those entries in its environment
and takes them back afterwards, as in every other shell:

```mesh
LC_ALL=C sort names.txt
TZ=UTC LANG=C.UTF-8 date          # as many as you like
PATH+=/opt/bin mytool             # `+=` appends, as it does for `$env.PATH`
```

It binds to **one stage**, so each side of a pipe gets its own and an `&&`
right-hand side gets none:

```mesh
FOO=1 a | FOO=2 b                 # a sees 1, b sees 2
FOO=1 a && b                      # b sees nothing
```

A function or builtin gets the same treatment — the entries are in place for the
call and restored after — since there is no child to inherit them. A name that
was unset goes back to unset rather than to empty, which a child can tell apart.

**Note which namespace this writes.** A prefix sets the **environment**, because
what the child inherits is the whole point. A bare `FOO=bar` with no command
after it is an ordinary [assignment](#variables) and binds a *shell* variable,
which no child ever sees:

```mesh
y=2 sh -c 'echo [$y]'   # [2]    — the environment, for that command
x = 5
sh -c 'echo [$x]'       # []     — a shell binding never crosses
```

The same spelling meaning two namespaces is deliberate — a prefix that wrote a
shell binding would do nothing for the child — but it is a real trap, and
whether a bare `FOO=bar` should mean the environment too is an open question in
`TODO.md`.

The bindings are written **unspaced**, `NAME=value` or `NAME+=value`. With
spaces there is no seeing where one value ends and the next name begins, which is
why `with` and `export` want the same form for a list.

## `with` — the environment for one block

`with NAME=value … { body }` runs the body with those environment entries in
place and puts the environment back afterwards — the block form of what other
shells write as a one-command prefix:

```mesh
with LC_ALL=C {
  sort names.txt }              # LC_ALL is back to what it was after the `}`

with TZ=UTC LANG=C.UTF-8 {      # as many bindings as you like
  date }
```

It is the **environment**, not shell bindings, because the point is what a child
inherits — a `x = 1` before a command has never reached one.

Each binding is written **unspaced**, `NAME=value` or `NAME+=value`, the way the
prefix form is. That is what lets a header hold several without the reader
guessing where one value ends: in `with FOO=a b { … }` the `b` cannot be part of
`FOO`, so it is reported. Values are ordinary words, so they interpolate and
quote as anywhere else, and `+=` appends exactly as the `$env.KEY` write does.
An empty value (`with FOO= { … }`) sets the name to nothing, which is not the
same as leaving it unset.

**The restore is to the previous state**, not to empty: a name that was unset
before goes back to unset, since a child can tell those apart. It happens however
the body leaves — normally, through a failing command, or through `return`,
`break` or `continue`:

```mesh
func build() {
  with CC=clang {
    return }                    # CC is restored on the way out
}
```

Bindings apply left to right, so a later one wins on a repeated name and each is
evaluated against what the ones before it left — `with PATH=/opt PATH+=/usr/bin`
reads as it looks. The restore is still to what the whole header found.

`with` is **contextual**, not reserved: it leads a statement only where a `NAME=`
binding follows it, so `with = 5` still binds a variable and `with somewhere` is
still a command.

Unlike `fork`, this costs no process — only the body's own commands do.

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

Inside a block a bare word is a command whatever its arity, so
`x = match 1 { 1 => { echo } }` *runs* `echo` — its output streams and the block
yields the status, since a block is not a capture — exactly as `{ echo two words }`
does. Quote the word when you mean the string: `1 => { "echo" }`, or capture the
bytes explicitly with `{ $(echo) }`. Outside a block the arrow already gives you
the terse value form, `=> echo`.

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

```mesh
func name(params) { body }    # define a named function
name arg ...                  # call it; args bind to the positionals
return [ N ]                  # exit the body early (or a sourced file — see `source`)
```

Define a callable with `func`. Parameters are **named** — reference them as
`$name` in the body, never `$1`:

```mesh
func greet(name) {
  puts "hi, $name"
}
greet world          # -> hi, world
```

- **Name.** An ordinary name — a letter or `_`, then letters, digits, `_` and
  single `-`s — that is not already taken. Six kinds are:

  | Refused | Why |
  | --- | --- |
  | a reserved word or builtin (`return`, `puts`, `cd`) | it resolves first, so the definition could never be reached |
  | a **boolean literal** (`true`, `false`) | the parser reads a bare one as the value, so command position never reaches a definition of the name at all |
  | a built-in **value call** (`re`, `style`, `link`, `glob`, `files`, `dirs`) | the opposite problem: `re(x)` always builds a regex, so the function would be reachable as a command and never as a call |
  | anything containing a `.` (`a.b`) | a dot is member access, so a dotted name has no call spelling |
  | the bare `_` | it is the discard |
  | anything that is not a name (`2x`) | — |

  Each is reported **where the definition runs**, and costs only that definition:
  the rest of the file still defines. That is what makes a *generated* file of
  definitions workable — a name the generator should have filtered says so and
  takes nothing else with it — and it applies to `alias` identically, since an
  alias is a `wrapper func`.

- **Signature.** Comma-separated parameters carrying the four roles from
  `DESIGN.md`: a **required positional** (`name`), an **optional positional** with
  a default (`name = value`), a **flag** (a boolean switch `--name` or a valued
  `--name = default`), and a trailing **rest** (`...name`). Names must be distinct
  and cannot be `env`.

  ```mesh
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
  - **In a value call, a flag is read from the call site, not from the value.**
    Only a `--name` written as a literal word there is an option, so `f($w)` and
    `f("--sleep=0")` pass data even when the text begins with `--` — the same
    rule as everywhere else that quoting makes a value, and it keeps what a call
    means readable from the line rather than from what a variable happens to
    hold. A **spread** is the exception, and the reason to write one:
    `f(...$args)` does read a `--force` element as the option, which is how a
    wrapper forwards flags it was handed.
  - **An option's value must be one string.** `--tag=$xs` with a list reports
    rather than binding, and so does `--tag=*.txt`, since a glob is a list
    however many paths it matched — including none. The command spelling of a
    glob *does* bind, and the two differ for a reason rather than by oversight:
    command position is argv, where a pattern expands to several **words** and
    the last `--tag=` wins, while a value call passes one typed value per
    argument. Binding the last match there would make an option's value depend on
    which files happen to be on disk and drop the rest silently. Bind the value
    you mean first, or use the command spelling.
  - **A modifier chain on an attached value transforms the value, never the
    name.** `f(--tag=$w:upper)` binds `tag` to the uppercased `$w` — the same
    reading as the command spelling — links nest left to right, and a link may
    take arguments (`--tag=abc:stripend(c)`). The anchor holds for the whole
    postfix run, so everything after the `=` reads as one value expression:
    `--tag=$xs:dedup[0]` binds the first deduped element, and `--tag=g().key`
    calls and projects. The name stays exactly as written, so
    `f(--TAG=V9:lower)` reports `unknown flag --TAG` rather than binding a
    name the chain rewrote. A chain on a switch spelling (`--force:upper` — no
    `=`, so no value part to anchor on) leaves the whole word a chained value,
    which is data like any other composed word.
  - **`--` ends flag parsing** — everything after a bare `--` is positional/rest,
    even if it begins with `--`.
  - **Defaults** are evaluated at call time, in the call's fresh scope, only when
    the parameter is omitted.
- **`wrapper func`** — a function that parses **no flags of its own**. Every
  argument reaches its positionals and `...rest` verbatim: an undeclared
  `--flag`, a bare `--`, and `--help` alike.

  ```mesh
  wrapper func g(...args) { command grep --line-number ...$args }
  g --color=never pattern file    # both flags reach grep
  g --help                        # grep's help, not mesh's
  ```

  This is what a forwarding wrapper needs and a plain `func` cannot give: an
  undeclared long flag is otherwise rejected before `...args` can collect it, so
  every wrapper would need an explicit `--`. A wrapper **cannot validate what it
  forwards** — it does not know the callee's grammar — so the check is
  *relocated* rather than dropped: the wrapped in-shell function's own signature
  rejects a bad flag, or the external program does.

  Everything else about the signature still holds — arity is checked, and
  positionals bind before `...rest` collects the remainder. Only the reading of
  `--`-leading words changes, and it changes in **both call forms**: a value call
  `g(--color=never)` forwards the token exactly as command position does. A
  `key: value` argument is unaffected — that is the caller naming a parameter,
  not a flag being passed through.

  A wrapper **cannot declare a `--flag`** — `wrapper func g(--force, …)` is a
  syntax error. The two statements contradict each other, and the visible
  consequence would be help and completion advertising a flag that every call
  forwards to `...rest`. For the same reason a wrapper advertises no options at
  all: `g --<Tab>` offers nothing, and there is no generated `--help`.

  `wrapper` is **contextual, not reserved**: it leads a definition only where
  `func` follows it, so the word is still free as a variable, a function name,
  and a command. It does not combine with `fork func`.
- **`alias NAME = COMMAND [ARG …]`** — the terse spelling of the above. Sugar
  over `wrapper func`, not a mechanism of its own:

  ```mesh
  alias co = vcs checkout
  # exactly the same definition as
  wrapper func co(...args) { vcs checkout ...$args }
  ```

  So an alias resolves, scopes, and takes arguments like any function, and
  `type co` reports the `wrapper func` it is. There is **no alias mechanism** —
  no parse-time textual expansion and no separate resolution stage; what mesh
  drops is that machinery, not the familiar name for "give this command a
  shorter one."

  A first word equal to the alias's own name reaches the **program**, not the
  definition: `alias grep = grep --color=auto` is the commonest alias there is,
  and it desugars with `command grep` so it cannot recurse — the same escape
  `func ls() { command ls … }` uses, and the same no-self-expansion rule bash
  applies.

  The right-hand side is **a command, not a string**. bash needs
  `alias ll='ls -l'` because its alias body is text; here the quotes would make
  one word naming no program, so that spelling is a syntax error pointing at the
  unquoted form. `alias` is contextual in the same way `wrapper` is: only the
  `alias NAME =` shape claims the word, so `alias = 1` and a function called
  `alias` still work.

  **The name may be computed.** A word carrying an interpolation — `$name`,
  `"${prefix}-st"`, `"${f()}"` — is evaluated where the definition runs, which is
  what lets a list of names define a list of aliases:

  ```mesh
  for name in [status log diff] { alias $name = git }
  ```

  The result is judged by the naming rules above, and reported the same way, so a
  computed name is a way around writing the name down rather than around what a
  name may be. One word: a list is refused, not joined. The self-naming escape
  applies here too, and has to be worked out at the definition for the same
  reason the name is — `alias $n = grep` with `$n` holding `grep` reaches the
  program.

  Quoting is what tells a computed name from a written one when the word holds no
  interpolation: `alias "foo" = …` is still `expected a name`, because a string
  written as a string is not a name. Quotes *with* an interpolation are ordinary,
  since that is how a name gets built out of parts.

  **The body is not computed.** It is syntax, evaluated when the alias runs, as a
  `wrapper func` body is — so `alias $name = puts $name` defines an alias whose
  body reads `$name` at *call* time, where a loop's binding is long out of scope.
  Baking a value into the body still needs the definition written out.
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
  view of the resulting value, so a body ending in `1 == 2` fails. A body ending in
  a **command** yields that command's [status as a value](#exit-status), so
  `func p() { /bin/false }` has the value `status(1)` and `p()` reads it.

  **`return expr` carries a value**, and succeeds (status `0`) unless that value is
  `false` or a nonzero status — so `return 3` is the integer three with status `0`,
  not exit code 3. Naming a status takes the **channel word**: `return status 3`,
  which is sugar for `return status(3)`, leaves `status(3)` with status 3.
  `return value 3` is the explicit spelling of the plain form and means exactly
  what `return 3` means.

  ```mesh
  func port() { return 8080 }         # value 8080, status 0
  func check() { return status 2 }    # value status(2), status 2
  func forward() { some-cmd
    return $sh.status }               # whatever `some-cmd` reported
  ```

  The channel words are recognized **only directly after `return`** and reserve
  nothing: an attached `(` is a call, never a channel word, so `return value(5)`
  calls whatever `value` names and `func value` stays legal. (`status` is taken
  anyway, as every builtin's name is.) Either channel word written without an
  operand is an error naming what is missing, not the string it used to bind.

  `fail` is the other spelling of a named status: bare `fail` is status `1`,
  `fail 123` names a code, and the value it leaves is that same status. It is
  `return status(n)` plus the constraint `n ≥ 1`, so `fail 0` is refused where
  `status(0)` is legal — the spelling for leaving with success is `return true`.
  It takes a status as readily as an integer, so `fail $sh.status` forwards one.

  A bare `return` carries the **result so far** — the last value the body produced,
  or the empty string if nothing ran — with the **last status**, so it means "stop
  here, as if the body ended at this line" and propagates a failure as readily as a
  success. All three stop the rest of the body. At a top level `return` and `fail`
  are a recoverable error, **except** in a sourced file, which they leave the way
  they leave a function body — see [`source`](#source).

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

  ```mesh
  func answer() { 42 }
  x = answer()                        # 42, an integer
  ```

  Only when the whole statement *is* that literal. `42 foo` and `42 > file` are
  still commands, and a bare `-3` is the minus operator rather than one numeral —
  write `return -3` or `(-3)`. mesh has no float literals, so `3.5` is still just
  a word.

  ```mesh
  func double(n) { return $n * 2 }
  x = double(21)                      # x is 42
  ```

  - **Arguments** are expressions, evaluated in the caller's scope. `key: value`
    binds the same parameter as the flag `--key`, so `d(prod, force: true)` and
    `d(prod, --force)` are the same call as `d prod --force`; a bare `--` ends
    option parsing; `...$list` spreads positionals and `...$map` spreads options.
  - **Channels stay independent.** The value returns through the call while the
    body's stdout streams as usual (`DESIGN.md`).
  - **Status** is the usual view of the resulting value: a status is its own code,
    a boolean inverts (`true` is `0`), anything else — an integer included — is
    `0`. A runtime error in the call fails the enclosing statement instead of
    yielding a value.
  - **Every call yields a value**, so a **command** may be value-called too, and
    what it yields is its status: `grep(zzz)` is `status(1)` and `puts(1 + 2)`
    prints and yields `status(0)`. `f`, `$(f)` and `f()` therefore mean the same
    three things for an external as for a mesh function, arguments included — a
    job builtin takes a handle (`wait($j)`) and `puts` renders a collection and
    styles it for the terminal, the same two value-reading families the written
    command has. Neither of those two examples is *useful*: the cost of every call
    having a value is the diagnostic that used to catch them.
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

  ```mesh
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
  - **Scope is a function's scope** — fresh locals, the parameters, the captured
    values, the globals. A lambda does not read the scope it was written in unless
    it says so: naming a local in a `with (…)` list is what brings it in, and a
    local the list does not name fails loud. (mesh has exactly two variable
    scopes; see [Variables](#variables).)
  - **Capture is explicit — `with (…)`.** The list is evaluated where the lambda
    is *written*, and the values are **copied** into the function value:

    ```mesh
    func pick(want) {
      return $paths:filter(func(p) with ($want) { $p:ext == $want }) }
    ```

    Without it a lambda and a function-local are mutually unusable — the same text
    works at top level and fails inside a function, because a lambda's scope
    parent is the session.

    - **Copied, so a later change is not seen.** `x = 1`, then
      `g = func() with ($x) { … }`, then `x = 2` leaves the lambda holding `1`. An
      *uncaptured* read of a session variable is still late and answers `2`,
      because the session outlives every frame — you may read late only from a
      scope that outlives you.
    - **Evaluated at the point of capture**, so an unbound name is loud there,
      while the frame that could explain it is still around — not at a later call.
    - **Per iteration in a loop**, so a lambda built in a `for` body keeps that
      pass's value rather than the last.
    - **A read, not a declaration**, which is why it is spelled `$name`. Only a
      bare variable is a capture: `$m.key` and a quoted word are not.
    - **Each name binds once**, so a name that is both captured and a parameter is
      a syntax error, as is the same capture written twice.
    - A captured value keeps its type — a captured list arrives as a list.

    Capturing under another name (`with (w = $want)`) is not built; `DESIGN.md`
    records it as the open extension.

    - **A definition takes the same list.** `func name(…) with (…) { … }` puts it
      where a lambda does, after the parameters; `alias NAME with (…) = COMMAND`
      puts it **before the `=`**, since after the `=` every word belongs to the
      command being aliased. Everything above holds unchanged — read where the
      definition runs, copied, loud on an unbound name, one binding per name — and
      the list is what lets a loop define one alias per name with that pass's value
      baked in:

      ```mesh
      for h in $hosts { alias $h with ($h) = ssh-to $h }
      ```

      Without the list a body's names are read when the definition is **called**,
      which is unchanged for every definition that carries no list. On an `alias`
      the name that cannot be captured is `$args`, the rest parameter the
      desugaring synthesizes.
  - **A global binding is visible to the body**, which is what lets a lambda
    recurse: `fact = func(n) { if $n == 0 { return 1 }\n return $n * $fact($n - 1) }`.
  - **No text form.** A function value is the one value that cannot be bytes, so a
    command argument, an interpolation, a spread element, and `$env.*` all refuse
    it rather than invent a rendering.
  - **Equality is identity.** A copied binding is the same function; a separately
    written lambda with the same text is a different one.

- **Function references — `&name`.** A declared `func` has no value spelling of
  its own: a bare word is a literal string everywhere else, so `$xs:map(up)`
  passes the *string* `"up"`. A leading `&` names the function instead, so
  anything taking a callable can take one directly:

  ```mesh
  func up(s) { $s:upper }
  puts $xs:map(&up):join(",")         # the lambda `func(s) { up($s) }`, named
  f = &up
  puts $f(z)                          # Z — called through the variable
  ```

  - **Late bound.** The reference holds the *name*, not the definition, and
    resolves when it is **called** — so redefining the target changes what an
    already-stored reference runs, and `f = &later` may name a `func later` that
    is declared further down the file. That is the difference from a lambda,
    which is the body it was written with.

    Binding one is late; *registering* one as a hook is not. A hook slot still
    checks the name when it is assigned, so `$sh.preprompt.x = &later` before
    `func later` is refused — whether that check should be relaxed is an open
    question, D3 in [`HOOKS.md`](HOOKS.md). Late dispatch and eager registration
    are separate things, and only the first of them is what `&` changed.
  - **The command namespace, not a `func`-only table.** A reference resolves
    `value call → builtin → func → external`, so `&reload-config` works without
    the writer knowing which it is. Command position's *keyword* step is skipped,
    so `&if` and `&return` are a syntax error rather than control flow. A call
    through a reference dispatches exactly where a written `name(…)` does, so
    `&glob`, `&dirs`, `&style`, `&re` and `&gets` all reach their value-call form.
  - **A reference to a command calls like one.** `&grep` and `&puts` are fine
    references, and calling one in a value slot runs the command and yields the
    [status](#exit-status) it left, exactly as the written `puts(1 + 2)` does — with
    the same argument handling, so `&puts` renders a collection and `&kill` names a
    job. A name that resolves to nothing is the failure that remains, and says so.
  - **Only a `func` can be applied per element.** `:map` / `:filter` / `:each`
    hand their callable one already-evaluated element, and a value call reads its
    own argument list, so `$xs:map(&glob)` is a loud error naming the lambda
    wrapper that does work (`$xs:map(func(p) { glob($p) })`).
  - **`:capture` works through one**, and records what the name it stands for
    would: `$f("hi"):capture` through `f = &puts` is `puts("hi"):capture`, whose
    `.value` is the status the command left.
  - **The whole signature travels**, because a reference is a name: flags,
    defaults, a rest parameter and the `wrapper` marker all behave as they do at
    a written call, and diagnostics name the function referenced rather than the
    variable it arrived through.
  - **Written tight against the name.** `& up` is not a reference. Backgrounding
    is postfix and belongs to statement position, so it never opens an operand
    and the two readings of `&` cannot meet.
  - A reference is a function value like any other: no text form, and identity
    rather than name is what equality means, so `&up` written twice is two
    values.

- **Both channels at once — `:capture`.** `f(…):capture` runs the call and returns
  a **record of every channel**: `.value` (the return value), `.out` and `.err`
  (its stdout and stderr), and `.status` (a [status](#exit-status)). Read them with
  ordinary field access. The record has a **fixed shape** — every field is always
  present — and `.status` is exactly the status whose code is the view of `.value`:
  the two are one channel with two views, not two answers.

  ```mesh
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
  - **Commands capture too**: `grep(foo):capture` asks for the record. Its
    `.value` is the status the command left, so the record's shape does not depend
    on what was called, and it takes **positional arguments only**, since a command
    has no signature for a `key: value` option or a map spread to bind to. A
    nonzero exit is data: `false():capture` reports `.status` 1 rather than
    failing.

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

  ```mesh
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

The argument-taking modifiers that work today are `:split`, `:join`, `:get`,
`:has`, the list-building family (`:prepend`, `:append`, `:extend`), `:bool`'s
default form, the affix family (`:stripstart`, `:stripend`, `:trimstart`,
`:trimend`), the replace family (`:replaceall`, `:replacestart`, `:replaceend`),
and `:map` / `:filter` / `:each`; the rest of the `DESIGN.md` set (`:match`, the
first-only `:replace`, the time and sort families) is not implemented, and
neither are the regex capture modifiers or a capture backreference in a
replacement. One of them **spread** at a command boundary
(`puts ...$x:split(":")`) is also not implemented — bind it first, which is the
same gap a spread value call hits (`ls ...glob($p)` → `found = glob($p)`,
`ls ...$found`).
Of heredocs, the command-redirection form documented under
Commands works, as do here-strings; a value-producing heredoc spelling does not.
The history designators `!!`, `!string`, and `!n` are not implemented — only
`!^`, `!$`, and `!*` are. `style` covers the sixteen ANSI colors only.
`command` runs a program; the `-v` / `-V` half of it — "what would this name
run?" — is not implemented, and is likely to arrive as a value rather than a flag.
See [`ROADMAP.md`](../ROADMAP.md).
