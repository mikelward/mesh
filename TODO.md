# TODO

The working front — concrete, checkable tasks for the current and next
milestone. The stable milestone arc is in [`ROADMAP.md`](ROADMAP.md); update this
file as tasks land.

## M0 — It runs `ls` ✅ (done)

- [x] Cargo workspace, edition 2024, MSRV 1.85, `rust-toolchain.toml`
- [x] `crates/mesh` binary: `main` / `repl` / `lexer` / `builtins` / `exec`
- [x] Read/tokenize/dispatch loop over stdin (TTY + piped)
- [x] Launch external commands; exit-status conventions (127 / 126 / 128+sig)
- [x] Builtin: `exit` (8-bit masking); `cd` punted to M1 (tentative)
- [x] Unit tests (lexer) + end-to-end tests (built binary, std-only)
- [x] CI: fmt + clippy (`-D warnings`) + test on Linux and macOS

## M1 — A shell you'd actually sit in ✅ (done)

- [x] `reedline` line editor for interactive (TTY) input; std byte reader kept
      for piped input. Ctrl-D exits on an empty line; Ctrl-C cancels the line.
- [x] History (in-memory, reedline default). Persisted history: later.
- [x] Two-glyph prompt (`mesh$` / `mesh!`) via reedline.
- [x] Lexer v1 (**Model B**): `"…"` (escape+interpolate), `'…'` (escape, no
      interpolation), `r'…'`/`r"…"` (raw); unknown escape is an error; backslash
      escapes; concatenation; quoting suppresses tilde/glob expansion. Deferred:
      heredocs, `\`-newline continuation across lines.
- [x] Variables (simple): `name = value` / `name=value` assignment (session-
      global), `$name`/`${name}` + `$env.KEY` interpolation (in bare + `"…"`),
      unbound read is a loud error, no word-splitting of interpolated values.
      Deferred: list/map values (single-value assignment only), `:` modifiers,
      `export`, `global`/`unset`, function-local scope, `$sh.*`, `$env:get`.
- [x] Promote internals into `crates/mesh-core` (lib); binary becomes thin `main`
- [x] `;`, `&&`, `||` sequencing (bare only; short-circuit on the previous
      status; quoted/escaped operators literal). `&`/`|` deferred to job
      control/pipes.
- [x] `cd` builtin (basic): `$HOME` default, `cd -`, updates `$PWD`/`$OLDPWD`,
      rejects surplus operands. Still deferred: `CDPATH`, `--physical`, autocd,
      logical cwd.
- [x] `pwd` and `puts` builtins
- [x] Globs + `~` expansion (glob no-match → **empty**). `~user` and expansion
      suppression (quoting) still to come; non-UTF-8 lossy under String words.

## M2 — Pipes, redirection, and job control ✅ (done)

- [x] Pipelines (`a | b | c`) with pipefail status, ignoring an upstream
      `SIGPIPE` caused by a downstream stage closing the pipe.
- [x] Basic redirection (`>`, `>>`, `<`) on external commands, including
      redirections on individual pipeline stages.
- [x] Descriptor redirection to a file: `2>`, `2>>`, and `1>` alongside the
      defaults, on external commands, functions, forked pipeline stages, and
      background commands. The digits must abut the operator, so `echo 2 > f`
      still writes "2". Deferred: redirection without a command.
- [x] Descriptors above 2 (`3< file`, `3> file`, `2>&3`). Redirection state is
      keyed by descriptor rather than a fixed `[Source; 3]`, and anything past
      the standard three is installed by the child itself, since only those have
      a `Stdio` slot. Duplicating an unopened descriptor is `EBADF`.
- [x] Closing a descriptor (`n>&-`), across all four routes, restored with the
      redirection on the in-shell one. A descriptor closed earlier in the list is
      gone, so copying it afterwards is `EBADF` — source order decides here as
      it does everywhere else.
- [x] Descriptor duplication: `2>&1`, `>&2`, `<&0`, and the both-streams forms
      `>& file` and `&> file`. A bare `>&` picks its meaning from the token as
      written, so a computed target (`>&$fd`) is refused rather than guessed at.
- [x] Heredocs: `<< END … END` interpolates its body (`$…` plus the `"…"` escape
      set, resolved through the command grammar so a heredoc and a string cannot
      disagree); `<< 'END'` is raw. The body reaches the command as an unlinked
      temporary file rather than a pipe, so a body larger than the pipe buffer
      cannot deadlock the shell, and a line-at-a-time reader waits for the
      delimiter directly rather than re-parsing the body per line. Backgrounding
      one works too, since the stage writes the temporary in its own process.
      Deferred: the value-producing spelling (still unspecified — see the design
      entry below).
- [x] Here-strings: `cmd <<< word` feeds the expanded word plus a trailing
      newline, bash's behavior. The word expands like any other argument and
      must come to exactly one, the rule every redirection target follows; it
      travels by the same unlinked temporary file a heredoc body uses. `<<<`
      names no descriptor, and backgrounding one works for the reason a
      heredoc's does.
- [x] Fork-based executor and process groups (`fork`/`exec`, `setpgid`,
      `tcsetpgrp`) so mesh can own the terminal and manage foreground jobs.
- [x] Signal handling: terminal signals target the foreground process group;
      Ctrl-C interrupts it with status 130 while mesh survives, and idle
      Ctrl-Z/Ctrl-\\ do not suspend or terminate mesh. Stopped-job tracking and
      resumption land with the job table below.
- [x] Job table plus `jobs`, `fg`, and `bg` builtins for stopped foreground jobs.
      `N` and `%N` select a job; no operand selects the newest job. Background
      launch with bare `&` registers running commands and pipelines in the same
      table; background stdin defaults to `/dev/null`.
- [x] Hand the terminal to full-screen programs and restore the shell's terminal
      modes cleanly when they exit or stop.

## M3 — The mesh language ✅ (done)

- [x] First typed value: bracketed list literals in assignment
      (`xs = [a "b c"]`), including the distinct empty list (`xs = []`).
- [x] Explicit list spread into command arguments (`puts ...$xs`); using a list
      without `...` is a loud error rather than implicit word splitting.
- [x] Replace the incremental command lexer with the clean-break expression and
      block parser.
  - [x] Fix the parser grammar, precedence, attachment, and completeness contract
        in [`PARSER.md`](PARSER.md).
  - [x] Emit a span-carrying token stream without performing structural parsing.
  - [x] Parse tokens into command, expression, and block AST nodes.
  - [x] Route parser-owned expression errors through `parser::parse` at
        execution entry, including chained comparisons and arithmetic assignment
        syntax, while command words remain compatibility-owned until their AST
        adapter lands below.
  - [x] Add recursive AST execution for `Source`, `Statement`, `AndOr`,
        `Executable`, `Pipeline`, `Command`, and `Expr`; implement sequencing,
        `&&` / `||`, background execution, and control flow from those nodes.
  - [x] Adapt parser-native `Word` / `WordPiece` and redirects directly into the
        existing expansion and process layers without stringifying and reparsing
        the AST through the compatibility lexer.
  - [x] Evaluate expressions as typed values, including variables, member and
        index access, modifiers, lists and spread, unary and binary operators,
        and recursive `if` / `for` bodies; return explicit runtime errors for
        parsed expression forms that are not implemented yet.
  - [x] Store parsed function bodies as `parser::Source` and execute them
        recursively instead of retaining and reparsing raw body text.
  - [x] Remove the raw-text function, `if`, and `for` recognizers and their brace
        scanners; use only `ParseOutcome::Incomplete` to buffer compound input.
  - [x] Retire `lexer::split_line` and compatibility lexer types from the REPL
        execution path once commands and expressions run from the AST; retain
        the old lexer only where a temporary public compatibility surface or its
        tests still require it.
  - [x] Add regression coverage for parser-authoritative errors and completeness,
        stored function ASTs, nested compound bodies, quoting, interpolation,
        globbing, redirects, pipelines, guards, and background commands; verify
        that `repl.rs` has no raw compound recognizers or `lexer::split_line`.
- [x] General list expressions: nested values, indexing/slicing, `+=`, and
      expression-position spread.
  - [x] Exact integer indexing (`$xs[0]`, including negative indices) for the
        current list slice.
  - [x] Clamped range slicing (`...$xs[1..3]`, `...$xs[..=2]`) for the current
        list slice.
  - [x] Append assignment (`+=`) for strings and the current list slice.
  - [x] List-preserving assignment from a variable or slice (`ys = $xs`,
        `ys = $xs[1..]`).
  - [x] List-preserving append from a slice (`xs += $ys[1..]`).
  - [x] Nested values and one-level expression spread (`[$xs]` versus
        `[...$xs]`), including spreading an indexed nested list.
- [x] Builtins and functions as pipeline stages (`puts $x | grep`, `f | sort`,
      `a | f | b`). Each runs in a forked child so the stages are concurrent
      rather than buffered, which is what lets `f | head -3` end early; state a
      stage changes is confined to it, as in every POSIX shell.
- [x] `while cond { … }` and `loop { … }`, with `break` / `continue` / `return`
      behaving as they do in `for`. A condition takes the same forms `if` does.
      Fixes a related gap along the way: a spaced `<` / `>` in condition position
      is a comparison rather than a redirection, so `if $i < 3` reads the way
      `if $i <= 3` already did.
- [x] Ordered, string-keyed maps: literals (including `[:]`), duplicate-key
      replacement, map spread, strict dot/bracket access, `+=` merge, and
      `:keys` / `:values` / `:len` collection modifiers.
- [x] Remaining scalar types (integers and booleans).
- [x] Initial argument-free `:` modifiers: path/string transforms and list
      collection operations, including typed list results and chaining.
- [x] First argument-taking `:` modifiers, `:split(SEP)` / `:join(SEP)`, in value
      expressions (terminator-style trailing-empty trim on split; fail-loud on a
      nested element for join). Deferred: the command-word form and other
      argument-taking modifiers (`:get`, `:has`, `:replaceall`, …).
- [x] `func` — user-defined functions: `func name(params) { body }` with required
      named positionals, multi-line bodies, function-local (lexical) scope, and
      `return`. Resolution is builtins → functions → external.
- [x] Function signature roles — optional positionals (`name = default`), flags
      (boolean switch `--name`, valued `--name = default`), and a trailing rest
      (`...name`), with `--` ending flag parsing and call-time default evaluation.
- [x] Redirecting a function (`f > out`, `f >> log`, `f < in`) — applied to the
      shell's own descriptors around the in-process call, so the body's output
      (including from externals it runs) lands in the target and stdout is
      restored afterward. A redirected **builtin** takes the same route.
- [x] Backgrounding an in-shell command (`f &`, `puts hi &`) — the same fork a
      pipeline stage gets, so the job joins the table like any other. A function
      keeps its typed arguments as a stage and in the background, and a stage
      runs with the status the pipeline started from, which a bare `exit` reads.
- [x] Calling for a value — `f(arg, key: value, ...$spread)` returns the
      function's value (last expression, or an explicit `return`), with `key:`
      options binding the same parameter as `--flag`; command position (`f arg`)
      still streams.
- [x] A lone integer literal is a value, so a block can yield one: `{ 42 }` is 42
      rather than "command not found: 42". Narrow by design — the whole statement
      must be that literal, so `42 foo`, `42 > file`, and `42 | cat` stay commands.
      A bare `-3` still does not qualify (it lexes as the minus operator, not one
      numeric word); `return -3` and `(-3)` carry it. Nothing changes for `3.5`,
      since mesh has no float literals.
- [x] Lambdas — `func(params) { body }` as an expression yielding a function
      value, value-called through the variable it is bound to (`$double(5)`),
      reusing the whole signature grammar. Scope is a `func`'s: parameters and
      globals, with no capture of the enclosing function's locals. A function
      value has no text form and compares by identity.
- [x] The higher-order modifiers `:map` / `:filter` / `:each`, each taking one
      callable and applying it per element, through the same call machinery a
      written call uses — so `return`, arity, a runtime error, an escaped
      `break`, and `exit` all behave identically. `:filter` requires a
      **boolean**, which settles the transform-as-predicate footgun `DESIGN.md`
      raises as open; `:each` yields the empty string, not the list. A list
      subject only: a map is a loud error pointing at `:keys` / `:values`.
- [x] The file modifiers — the scalar **tests** `:exists`, `:type`, `:read`, and
      `:write` (`test -e` / `find -type` / `-r` / `-w`), which map over a list, and
      the **filters** `:files`/`:f`, `:dirs`/`:d`, `:links`/`:l`, and `:exec`/`:x`,
      which keep a list's matching elements and chain for AND (`:f:x` is the
      executable plain files). On one path a filter is the boolean its `test`
      operator gives, so a filter doubles as the predicate `:filter` applies per
      element. All dereference symlinks except `:links` and `:type`, the two that
      exist to ask about the link itself; `:type` is the only one that errors on a
      missing path, since there is no word to report.
- [x] A bare `:mod` reference as a callable value — `$files:filter(:exec)` for
      `$files:filter(func(f) { $f:exec })`, the equivalence `DESIGN.md` states.
      Argument-free **value** modifiers only: `:join` needs a separator, `:map` a
      callable, and `:capture` wraps an invocation, so none is a one-argument
      function and naming one is loud — `:capture` at the point the reference is
      written, or the call it would capture runs first. A
      reference is a function value like any other (no text form, identity
      equality), and a leading `:` is a reference only in expression position, so
      map keys, named arguments, and `$host:$port` are untouched. Applied through
      the same value-sensitive path as the postfix form, so a regex still gets the
      flag names (`:i`, and `:x` as extended rather than the executable filter).
- [x] `f(…):capture` — the channel record (`.value` / `.out` / `.err` /
      `.status`), as an invocation-level modifier that wraps execution, including
      on an external (minus `.value`, positional arguments only). `.out`/`.err`
      are the bytes as written. Deferred: the richer fields `DESIGN.md` leaves
      open (timing, a `pipestatus` list), and true **byte**-strings — mesh has no
      byte-string type yet, so a capture that is not valid UTF-8 is a loud error
      rather than raw bytes.
- [x] First `if` expression slice — command-status and value conditions, brace-delimited
      `else` / `else if`, multiline bodies, typed assignment-position results,
      and conditional list-pattern binding.
- [x] Finish `for` / `match`, in dependency order:
  - [x] First `for` slice over string lists and expanded word expressions, with
        brace-delimited multiline bodies and current-scope bindings.
  - [x] Ordered map iteration with `key, value` binders, bounded integer range
        iteration, `break`, and `continue`.
  - [x] Introduce reusable list-pattern binding for names, `_`, and `...rest`,
        then use it for assignment, conditional binding, loops, and match arms.
  - [x] Implement `match` parsing and evaluation, including ordered first-match
        arms, literal/glob/regex/range/`_` patterns, alternation, list patterns,
        guards, statement position, and expression results.
  - [x] Add loop-control regression coverage for nested loops and for `break` /
        `continue` reached through nested `if` and function calls.
  - [x] Audit the M3 acceptance examples in [`DESIGN.md`](DESIGN.md) and
        [`docs/INTRO.md`](docs/INTRO.md): add end-to-end coverage for examples
        that are in scope, inventory dependencies that remain, and update the
        milestone documentation without silently weakening its acceptance bar.

## Beyond M3 — Interactive completion

- [x] Lazily derive typed flag and subcommand candidates from bounded external
      `--help` probes.
- [x] Cache generated completion specs in memory and under
      `$XDG_CACHE_HOME/mesh/completions/`, keyed by executable path, modification
      time, and subcommand arguments; regenerate stale or corrupt entries.
- [x] Add typed file, directory, and enum values to completion specs.
- [x] Add fuzzy and case-insensitive candidate ranking with `nucleo`.
- [ ] Load curated completion specs, then add man-page-derived specs.
- [ ] Expose static and dynamic completion overrides through `$sh.complete`.

## Beyond M3 — The environment

- [x] `$env.KEY = value` and `$env.KEY += value` write the process environment,
      so children inherit them. Global even inside a function, per `DESIGN.md`.
      Only strings cross: a list or map is a loud error naming `:join`, and an
      embedded NUL is refused rather than truncated.
- [x] Path-type entries (`PATH`, `MANPATH`, `CDPATH`, `INFOPATH`,
      `LD_LIBRARY_PATH`, `PYTHONPATH`) are lists — split on read, `:`-joined on
      write, exactly, so every empty component survives a round trip. This is
      what makes `$env.PATH += /opt/bin` and `$env.PATH:dedup` work.
- [x] `export NAME = value` / `+=` as the other spelling, desugaring to the
      `$env.NAME` write so both carry one set of boundary rules. Bare
      `export NAME` is refused with the spelling that works, since mesh keeps
      shell bindings and the environment in separate namespaces. Deferred:
      `export --list NAME` to opt an arbitrary name into the path-type set.
- [x] `unset name …` (current scope), `global name = value` / `+=`, and
      `global unset name`. All three are contextual keywords, so a variable may
      still be called `global`, `unset`, or `export`. Deferred: deleting a
      collection element (`unset $m.key`, `unset $xs[i]`), which waits on
      general member assignment.
- [x] General member assignment — `$m.key = v`, `$xs[0] = v`, and `+=`, along a
      path mixing members and indices. Local-by-default like any other assignment,
      so a write inside a function shadows rather than reaching through. Nothing
      along the path is auto-created (a missing intermediate key is loud); the one
      exception is a new key at the end of a map. `global $m.key = v` writes into
      the session-global binding instead, the escape hatch that lets a function
      modify a caller's collection. A slice is not a place, and a list is written
      in place, so an out-of-range index is an error. `$env.KEY` keeps its own
      byte-boundary rules and `$sh` stays read-only.
- [x] Deleting a collection element — `unset $m.key`, `unset $xs[i]`, and
      `global unset $m.key` — sharing the assignment's path walker, so a nested
      path, a negative index, a quoted key, the fail-loud rules, and the
      no-stale-shadow guarantee all come across unchanged. Removing from a list
      shifts what follows. Names and places mix in one statement. `$env` and `$sh`
      are not places here either, so removing an environment entry still has no
      spelling.

## Beyond M3 — Invocation

- [x] Run a script named on the command line (`mesh script.mesh a b c`), a
      command string (`-c "…"`), or stdin (`-s`), alongside the existing
      interactive and piped paths. Option parsing stops at the first operand so a
      script's own flags reach it; `--` ends it explicitly. A script is parsed as
      one unit, so a syntax error rejects the whole file; a missing script exits
      `127` and an unreadable one `126`. Shebangs work by way of `#` comments.
- [x] `--help` and `--version`.
- [x] First slice of the read-only `$sh` namespace: `$sh.args` (a real list, not
      `$1` / `$@` / `$#`) and `$sh.name`. `sh` joins `env` as a reserved name.
- [ ] `-i` to force an interactive session when stdin is not a terminal.
- [ ] Mutating positional arguments (`shift` / `set --`), deferred in `DESIGN.md`
      along with system-wide `/etc/mesh/*` startup files.
- [x] A `source` builtin, and the input **origin** (`script` / `sourced` /
      `command` / `stdin` / `interactive`) plus `$sh.source`, resolving the TODO
      block in `DESIGN.md` §"Startup and invocation". `source FILE` runs a file in
      this shell; a startup file reports itself as `sourced` too, so `$sh.source`
      locates a sibling. `$sh.source` reports the **innermost** file, not a stack.
      `return` leaves a sourced file and gives `source` its status (a bare one
      carries the last status); `exit` still ends the shell; a script/`-c`/typed
      top level has no caller, so `return` there stays an error. Missing and
      unreadable files answer `127` / `126`, the statuses `mesh FILE` uses.
      Deferred: arguments for a sourced file, which need `shift` / `set --`.
- [x] **A leading `not` starts a value.** `if not $b { … }` and `while not $b { … }`
      read as conditions rather than a command named `not`, matching the postfix
      guard and an assignment's right-hand side, which already parsed an expression
      directly. Claimed only when what follows itself starts a value, so `not foo`
      is still the command — the same discriminator `if $i < 3` uses against
      `cmd <file`. A bare `true` / `false` counts as a value only after `not`, so
      `if not false` negates while `if true` still runs the command; the one cost is
      reaching a command named `not` as `not true` / `not false`. Claimed only when the
      negation is the **whole statement**, as a lone integer literal is, so `not true
      foo`, `not $x | cat`, and `not false > out.txt` stay the commands they were, with
      the redirect judged after the *complete* operand so a list or a `:mod` operand
      (`not [1 2] > out.txt`) redirects too.
      `not -1` remains a command, since `-1` is not a value start anywhere in command
      position — see the negative-literal item under "Loose ends".
- [x] **A word operand is a value only when it is the whole statement**, and a redirect
      after it is found by scanning to the end of the **command word** — a word plus its
      attached argument-free `:modifier` suffixes. Both halves were wrong for the same
      reason: the check looked one token past the *start* of an operand, which is the
      `:` in `$p:base` — so `$editor file` reported `expected a statement separator`
      while `$editor > log` worked, and `$x:len > out.txt` read the `>` as a comparison
      instead of a redirection. The scan is deliberately not a parse; every parse-based
      version reached too far, since the grammar nests whole expressions inside a
      subscript or a call, which is how `$x + 1 > 1` and `$xs[0 + 0] > 0` briefly became
      commands that truncated a file named after the right operand. `$xs[0] > f` still
      redirects, a literal index being part of the word. `$cmd` with arguments, a spread, a pipeline, `&&` / `||`,
      a backgrounding `&`, a spaced postfix argument (`$e :len` echoes `:len` rather than
      measuring the word), and every redirect spelling (`>`, `>out`, `>>`) now reach the
      command they name — a bare `$cmd || fallback` ran nothing at all before, and
      branched on the truthiness of the *word* rather than the exit status. A negation is
      the other kind of operand, with no command reading, so `not $b && puts x` stays a
      value statement: the *shape* question an operand check asks is kept separate from
      the *statement* question, which is what conflating them broke.
      Unchanged: an operand that *is* the whole statement stays a value (`$xs:len` on
      its own), a spaced comparison in a condition stays one (`if $xs:len > 5`), and a
      derived value stays a non-place (`$xs:dedup = 9` is still a syntax error, not a
      command). `value_is_whole_statement` is shared with the leading-`not` rule, taking
      a parameter for the one way the two operand kinds differ, rather than being
      duplicated per clause. The redirect question is *two* predicates on purpose, since
      it is two questions: for a word operand the operand is itself the command word, so
      only word shapes can take a redirect, while for a negation `not` is the command
      word and the operand is an argument, so any shape can precede one
      (`not [1 2] > out.txt`).
- [x] `$sh.status` (the readable `$?`) and `$sh.pipestatus` (a real list, not
      bash's magic `PIPESTATUS` array). The two always describe the *same* run:
      a compound's status is its body's, so the breakdown stays the body's too —
      which differs from bash and holds only because pipefail is always on. A
      forgiven `SIGPIPE` is the one place they diverge from each other, showing
      as `141` in the list while the status stays 0.
- [x] `$sh.pid` / `$sh.ppid`, `$sh.version`, `$sh.interactive`, and the stream
      handles `$sh.stdin` / `$sh.stdout` / `$sh.stderr` with their `:tty` test.
      A handle is its own value (`Value::Stream`) with **no byte form**, as
      `DESIGN.md`'s rendering table requires, so `puts $sh.stdin` is a loud
      error and the descriptor never reaches argv, arithmetic, or the
      environment; `:tty` is the question it answers, and a bare integer is
      refused. `$sh.interactive` is recorded by the loop that runs, not derived
      from `isatty`, so `mesh -s` on a terminal reports `false`.
- [x] `$sh.jobs`: the live table as an insertion-ordered map of `pid` / `cmd` /
      `state` / `status` records, keyed by job id. Reading polls so a finished
      job reports `done` with its status rather than a stale `running`, but does
      **not** reap — a completed job stays available to `fg`, and reaping still
      reports and removes it at its own time. Deferred: `j = cmd &` binding a
      handle, `kill $j` / `wait $j`, and the `%n` sigil.
- [x] `wait JOB`, taking the same `N` / `%N` reference `fg` and `bg` take. It is
      `fg` without the foreground — no `SIGCONT`, and the terminal stays with the
      shell — so a background job goes on being one and only its status comes
      back. Waiting is what lets backgrounded work outlive the script that
      started it, since the shell hangs its jobs up on the way out. A finished
      job answers from its record, a stopped one reports its stop status rather
      than blocking on a job that will not finish, and SIGINT abandons the wait
      (`130`) while leaving the job listed — which needed a `sigaction` catcher
      around the wait, because the interactive shell ignores SIGINT and a
      background job never receives the keystroke itself.
- [x] The full `%` job reference: `%%` / `%+` for the current job, `%-` for the
      previous one, and `%prefix` for the most recent command starting with it,
      alongside the `%N` and bare-`N` forms already there. The table tracks the
      current and previous job by **id** rather than position, so a job leaving
      cannot silently repoint them: a job takes the current spot when it is
      registered, when it stops, and when `bg` restarts it, and when the current
      job leaves the previous is promoted and the job behind it fills `%-`. An
      id still wins over a prefix, so `%1` is job 1. Deferred with a message that
      names it: `%?string`, the substring match `DESIGN.md` also defers.
- [ ] **The rest of `wait`.** `wait` with **no operand** — bash's "every child,
      with an aggregate status" — is refused rather than guessed at, since `fg`'s
      no-operand default means "the most recent one" and the two would read
      alike. `DESIGN.md` defers the aggregate; deciding it is what unblocks the
      bare form, along with multiple operands (`wait 1 2`). `wait $j` on a job
      handle waits on `j = cmd &`, deferred above.
- [ ] **A `kill` builtin.** `DESIGN.md` lists it among the job builtins, so that
      `kill $j` / `kill %2` signal a *job* while `kill 49001` stays a pid. It
      needs the same job-reference resolution `wait` uses (`JobTable::resolve`),
      which is why the two belong together.
- [ ] The rest of `$sh.*`: `$sh.options` and the hook maps.

## Loose ends

Small items rescued from pull requests that were closed as superseded — the bulk
of each PR had landed by another route, but these pieces had not.

- [x] **FreeBSD compile-check in CI.** `cargo check --workspace --all-targets
      --target x86_64-unknown-freebsd` runs alongside the macOS cross-check, so a
      BSD-only mistake in `mesh-platform` no longer passes both runners
      unnoticed. It needs no cross compiler — nothing here builds C for FreeBSD,
      and `cargo check` never links — so it is a target install and one command.
- [x] **`fork { … }` — the subshell grade of isolation.** The body runs in a forked
      child, so its `cd`, environment writes, and bindings stay there, and an `exit`
      inside ends the child rather than the shell, arriving outside as the block's
      status. Only bytes cross back, as `DESIGN.md` says of a subshell. Contextual
      like `global` / `unset` — `fork` leads a statement only when a `{` follows, so
      a command of that name stays reachable — and the fork/wait itself reuses the
      status conventions `wait_for_job` already encodes rather than restating them.
- [ ] **An inherited `SIGCHLD` of `SIG_IGN` loses every exit status.** A process
      started with `SIGCHLD` ignored has its children auto-reaped, so `waitpid`
      fails with `ECHILD` for a child that exited perfectly well, and mesh inherits
      that disposition rather than resetting it. Every wait path is affected, not
      one: launched that way, `true` reports status 1, `puts hi | cat` reports 1
      after printing `hi`, and `fork { puts inside }` reports 1 after printing
      `inside`. Reproduced by exec'ing mesh from a parent that sets
      `SIGCHLD` to `SIG_IGN`. The fix is at startup — a shell owns its own
      disposition for `SIGCHLD` the way it owns the terminal signals, so reset it
      to `SIG_DFL` before the first child rather than teaching each wait site to
      read `ECHILD` as success, which cannot recover the status anyway.
- [ ] **Forking while the capture readers are running.** Every fork in the shell
      runs interpreter code in the child before any `exec` — a pipeline or background
      stage at `exec.rs:413`, a `fork` block at `exec.rs:1360` — and under `$(…)` or
      `:capture` the shell is multithreaded for the whole body, since both capture
      helpers drain the diverted pipes on scoped reader threads (`repl.rs:3026`,
      `repl.rs:3061`). POSIX allows only async-signal-safe calls between fork and exec
      in that state: a lock held by a thread that does not exist in the child is held
      forever. It is not specific to `fork { … }` — `out = $(f | cat)` on a mesh `f`
      has forked a stage that runs the interpreter since stages were forked at all,
      and `expand.rs:1339` already names the hazard in a test. What keeps it latent is
      narrow: a reader holds no Rust-level lock, reading a raw `File` into a `String`,
      so the only lock it can own is the allocator's, and glibc and musl both
      reinitialize theirs across fork. Anything a reader gains later that takes a
      Rust-level lock — `std`'s stdout lock, the env lock — removes that. The fix
      belongs to the capture side rather than to each fork: drain both pipes from the
      forking thread with `poll` and no thread survives to strand a lock, which clears
      the precondition everywhere at once. The comment at `repl.rs:3009` rules out
      *sequential* reads, which deadlock on the unread channel's buffer; polling both
      descriptors is not that.
- [ ] **Can a subshell return a value?** Written up in `DESIGN.md` beside the
      isolation grades. Short version: "only bytes cross back" is argv's rule —
      about *flattening*, where a list fails for want of a canonical separator —
      borrowed for a boundary whose problem is *reconstruction*, where a list is
      fine because a structured encoding carries its own delimiters. The appealing
      form is that mesh's own literal syntax is the encoding: the child writes the
      value as the text you would have typed, on a pipe of its own, and the parent
      reads it back with the ordinary expression parser. What crosses is then
      exactly what has a literal form. **The writer has landed** as `:repr`, with
      the quoting exact (`42` and `'42'` stay apart, as do `[]` and `[:]`) and the
      formless types refused by name, so "what crosses" is settled and enforced in
      one place. The temp-file fallback listed here turned out **not** to be
      needed: that is `$( … )`'s two-pipe deadlock, and a value channel is one
      pipe the parent can drain before it waits. What is left is the plumbing —
      the pipe itself, a reader that parses exactly one literal (rather than
      running arbitrary source through the statement evaluator), and
      length-prefixing so a grandchild holding the write end open cannot hang the
      read. Decide before `fork func`, since a value call on one is the case that
      needs it.
- [ ] **`i64::MIN` has no readable literal.** `x = -9223372036854775808` fails
      with "expected integer" while `i64::MIN + 1` and `i64::MAX` both work: the
      parser builds a negation over the magnitude `9223372036854775808`, which does
      not fit an `i64`, so the operand is already a string by the time the sign
      would apply (`parser.rs` `prefix`, `expand.rs` `typed_scalar`). The fix is to
      fold the sign into the literal at parse time, as Rust itself does — which
      changes how *every* negative literal parses, so it wants its own change
      rather than riding along with one. Found by the `:repr` round-trip tests,
      where it is the one value the writer can spell and the reader cannot take
      back. `:repr` **refuses** it meanwhile (`NoLiteral::MinInteger`), because it
      is reachable by arithmetic — `-9223372036854775807 - 1` — and a round-trip
      contract with one silent hole is not one the fork value channel could build
      on. Fixing the parser therefore deletes that arm as well: pinned from both
      sides by `the_smallest_integer_is_refused_until_the_reader_can_take_it` in
      `repl.rs`, which fails when this lands.
- [ ] **Two modifier tables, one of them quietly stale.** `lexer::Modifier`
      (`lexer.rs:36`) and `expand::Modifier` (`expand.rs:27`) are separate enums
      with separate `from_name` tables, and the lexer's has not been extended
      since the initial path set: `Keys`, `Values`, `Int`, `Type`, `Exists`,
      `Read`, `Write`, `Files`, `Dirs`, `Links`, `Exec`, `Tty`, and `Repr` — 13
      names — exist only in `expand`. Nothing is broken for the shell, because a
      name the lexer does not know ends its modifier scan and the parser's postfix
      path handles it; that is how `:keys` has always worked. But
      `lexer::split_line` is `pub`, so a consumer tokenizing with it sees
      `$x:keys` as `$x` followed by literal `:keys`, and the two tables have to be
      remembered together every time a modifier is added. Fix by having the lexer
      defer to `expand::Modifier::from_name` — one table — rather than by adding
      13 entries to a second one. Raised by Codex review on #223 against `:repr`,
      which is consistent with the other twelve rather than a new instance.
- [ ] **The rest of `fork` isolation.** Two pieces of the `DESIGN.md` cluster are
      still open. **`fork func name(params) { … }`**, a func whose *body* is a
      subshell, needs a decision first: a subshell returns only bytes, so what does
      a **value** call (`f()`) on one mean — an error, or an empty value? That is a
      grammar-level question rather than a mechanical one. **Backgrounding a
      subshell** (`fork { … } &`) is refused today, since a backgrounded child needs
      a job-table entry to be resumable by `fg`; wiring it into the table is the
      work, and it is also what a *stopped* descendant of a subshell needs — with
      nowhere to record one, Ctrl-Z on a subshell is answered by continuing it
      rather than stranding it. **Piping or redirecting a subshell** (`fork { … } | cat`,
      `fork { … } > log`) is a syntax error today: a `fork` block is a statement
      rather than a pipeline stage, and bash's `( … ) > log` says people will
      expect it. `in DIR { … }`, the third and cheapest grade in `DESIGN.md` (scoped cwd,
      no fork), does not parse yet either.
- [ ] **A redirect after a non-word value operand.** `[1 2] > out.txt` and
      `(1) > out.txt` read the `>` as a comparison and fail with "comparison requires
      two integers or two strings"; `[1 2] >> out.txt` is a syntax error, since `>>` is
      never a comparison. Unlike a word operand there is no second reading to fall back
      on — `[` always opens a list literal, so there is no `[1 2]` *command* the way a
      shell without list values would have one — so the fix is not "route it to the
      command parser" but deciding what a redirect after a value means at all. Options:
      reject it with a message naming the real problem ("a value cannot be
      redirected"), or let a value statement be redirected and write its text form,
      which is the same question as displaying a value at the prompt (below).
- [ ] **The parser has no recursion-depth limit.** Deeply nested input aborts the
      whole shell with `thread 'main' has overflowed its stack` instead of reporting
      a syntax error. Not new — on `main`, before any of #215, both of these already
      abort:

      ```
      x = ((((… 20000 deep … ))))     → stack overflow
      x = [[[[… 20000 deep … ]]]]     → stack overflow
      x = not not not … $b            → stack overflow
      ```

      #215 removed the one case where its own lookahead recursed (a command-shaped
      run of `not`s, now iterative and covered by a test), but a genuine *value*
      chain — `if not not not … $b`, around 1000 deep — still reaches the shared
      expression recursion and dies there, where before #215 that line was
      command-shaped and merely reported `command not found: not`. The fix is a
      depth counter in the expression parser that raises a syntax error at some
      generous limit; it belongs with the general error model rather than with any
      one operator, since parens and lists get there without `not` at all.
- [ ] **Math at the prompt.** The goal (mikelward): type `1 + 2` at the prompt and
      get `3`, so the shell is usable as a calculator without `expr`, `bc`, or
      `$((…))`. Not supported today, and deliberately not part of #215 — recorded
      here as the direction. Two things are missing, and only the second is
      arithmetic at all:

      1. **Display.** `1 + 2` already *evaluates* — the parser reaches it and the
         result becomes the statement's value — but nothing prints it, and
         `status_of` folds the integer into the exit status instead, so
         `1 + 2` then `$sh.status` reports `3` while the terminal stays blank.
         Deciding what an interactive prompt echoes for a value-producing
         statement is the real work: it interacts with the bare-literal rule
         above, with `capture`, and with not printing anything for `ls`.
      2. **Negative literals in command position.** `-1` lexes as the minus
         operator plus `1`, and only the expression parser puts them back
         together, so `x = -1` works while `if -1 { … }`, `while -1`, a bare `-1`,
         and `if not -1 { … }` all report `command not found: -1`. Decided
         (#215) that it should eventually *be* an integer literal there, fixing
         all four at the root rather than widening each lookahead — the
         bare-literal rule above excludes `-3` for this same reason. The question
         it must answer is where a literal ends and a flag begins, since `head -1`
         and `sort -1` stay commands with flags; likely "attached digits after
         `-` in *value* position," not everywhere.
- [ ] **Document bold input in `DESIGN.md`.** Interactive input renders bold
      (`repl.rs`, `input_highlighter`), but the *design* — uniform weight rather
      than token-aware color, live as you type, surviving Enter into scrollback,
      and whether it gets a `$sh.options.bold-input` off switch — is written down
      nowhere.

## Redirection: one source-ordered pass ✅ (done)

Three sides of one change, done together. What each closed is written up in the
entries below, which is where the reasoning now lives.

- [x] **1. Merge `open_paths` and `resolve_sources` into one walk** — as
      `resolve_redirs`: each redirection, where the walk reaches it, does its own
      limit check and then opens its path or **performs** its duplication. The
      per-stage thread is kept, so the seed each stage starts from is built up
      front.
- [x] **2. Resolve a background stage's destinations without opening** — the
      same walk in `Acquire::Deferred` mode, which opens nothing and duplicates
      nothing (those belong to the child) and reports only where each descriptor
      lands. The syntactic `stdout_is_redirected` scan is gone.
- [x] **3. Spawn externals with `fork` + `execvp` instead of `Command`** —
      through the same `fork_stage` an in-shell stage uses, so a stage that
      cannot `exec` reports 126/127 from its own process. Took the
      `--mesh-background-redirect` helper with it.

## Redirection edge cases

Found by review on the descriptors-above-2 work and deliberately deferred: each
needs a descriptor above 2, a resource limit, or a failed `exec` to reach, so
none affects a redirection that worked before that change. Every one below was
reproduced against the built binary and compared with bash.

All but the last two are now fixed; the fixed entries are kept for the
reasoning, and the open ones are at the bottom.

- [x] **A failed `exec` writes into the redirection target** — *fixed*.
      `Command::spawn`'s private close-on-exec error pipe took a low descriptor
      and the hook that installs descriptors above 2 overwrote it, so
      `mesh_no_such_command 4> out` put Rust's binary `NOEX` packet into `out`
      and exited 1. External stages now `fork` and `execvp` themselves through
      the same `fork_stage` an in-shell stage uses, so the child reports 126/127
      from its own process and no private pipe is involved. Two consequences
      worth knowing: the report now goes to *the stage's* stderr, as bash's does
      (`missing 2> log` logs it), and the re-executed
      `--mesh-background-redirect` helper is gone — a forked child can open its
      own targets after the parent has moved on, which is all the helper existed
      to provide. That also lifts the refusal to background a heredoc or
      here-string, whose body no longer has to travel as argv.
- [x] **A backgrounded in-shell stage loses its pipe** — *fixed*.
      `stdout_is_redirected` scanned `cmd.redirs` for anything targeting fd 1, so
      it tripped on the `>` in `f 3>&1 > file 1>&3 | tr a-z A-Z &` and in
      `f 2>&1 > file | tr a-z A-Z &` even though source order leaves the pipe
      held. Both printed `low` past the pipeline where bash pipes `LOW`. A
      background stage is now resolved by the same walk as any other, in
      `Acquire::Deferred` mode: it opens nothing and duplicates nothing (those
      belong to the child) and reports only where each descriptor lands, so the
      shell can ask whether *anything* is still on the pipe. `piped_out` follows
      that answer, so `> file` still makes a `SIGPIPE` real.
- [x] **A duplication that cannot be afforded still lets later targets be
      opened** — *fixed*. `open_paths` and `resolve_sources` are now one
      source-ordered walk (`resolve_redirs`) that acquires every descriptor as it
      reaches it: the limit is checked, then the path is opened or the
      duplication *performed*. At `ulimit -n 5`, `true 3> foo 4>&3 > existing`
      now fails on the `>` it cannot afford and leaves `existing` alone, as bash
      does. The ordering guarantee is structural rather than something each new
      failure mode has to be taught.
- [x] **The descriptor-limit check does not run in source order** — *fixed, and
      the residual difference from bash decided deliberately*. The check moved
      into the walk, so `true > existing 4> later` at `ulimit -n 4` truncates
      `existing` and only then reports `&4`; the earlier redirections happen, as
      the source-order rule says they should. mesh still does **not** create
      `later`: it refuses a descriptor it could never install onto before opening
      a target for it, where bash opens first and fails on the `dup2`. Kept
      deliberately — everything else in this list was a bug because mesh
      destroyed something bash spares, and this is the one place mesh spares
      something bash destroys.
- [ ] **A redirection failure is reported by the shell, not through the
      redirections that already applied.** `sh -c 'echo nope' 2>&1 4>&9 | cat`
      puts `Bad file descriptor` into the pipe in bash, because `2>&1` applied
      before the `4>&9` that failed; mesh writes it to the shell's stderr, in the
      foreground and backgrounded alike. Raised by review on the fork/execvp PR
      and checked against the commit before it — the behavior is the same there,
      so it is a standing divergence rather than something that change caused.
      The cause is that mesh resolves a stage's redirections **completely**
      before installing any of them, so at the moment it reports there is no
      partially-applied stderr to report through: bash applies each redirection
      as it reaches it and is therefore already inside the new stderr when the
      next one fails. Closing it means installing what the walk reached before
      reporting — which the forked child could do for itself, but which a
      foreground stage cannot without moving resolution after the fork and giving
      up the concurrent opens that keep `cat < fifo | cmd > fifo` from
      deadlocking. Worth deciding deliberately, because the tidy answer (every
      stage resolves and installs in its own child) trades a real property for
      message placement.
- [ ] **`3>&0` with stdin closed.** Reported as accepted-and-destructive with fd 0
      closed by mesh's caller. `live_descriptors` no longer assumes the standard
      three are open — it probes all three — but the reported symptom persists
      (`existing` is still truncated, with no error), which suggests mesh has
      something on fd 0 by then, plausibly reopened at startup. Establish what
      fd 0 actually is at that point before deciding whether there is a bug here
      or whether the real question is what mesh should do with a closed
      inherited stdin.

## Decisions made

- **Merge method:** rebase. **Toolchain:** floating `stable`. **Loop autonomy:**
  proceed with best call, documented + overridable; pause only for grammar-level
  design decisions.
- **Working-directory var namespace = `$env.PWD` / `$env.OLDPWD`** (confirms
  `DESIGN.md`; the `$sh.*` alternative was considered and dropped — if a value is
  exported to and inherited by children, it lives under `$env.`).
- **Heredocs interpolate by default; a quoted delimiter is raw.** `<< END … END`
  interpolates (`$var` + the `"…"` escape set); `<< 'END' … END` is raw — no
  interpolation, no escapes — the bash convention. The **quoted-delimiter** heredoc
  is the raw mixed-quote string form (embeds both `'` and `"` with no escaping),
  chosen over a Rust-style `r#"…"#` delimiter. Its value-producing spelling (vs
  command-redirection) is still open below. Implementation lands with the quoting
  task (task 5).
- **Repo license = `MIT OR Apache-2.0`** (the Rust-ecosystem norm, as used by Rust
  itself). `LICENSE-APACHE` and `LICENSE-MIT` live at the repo root and every crate
  declares `license = "MIT OR Apache-2.0"`.
- **Glob no-match → empty** (nullglob-style: the pattern expands to zero words).
  This is *principled*, not a compromise, and fully consistent with "absence is
  loud": specific-element access (`xs[99]`, `$map.key`) errors because you asked
  for one thing that isn't there and there is no null; a glob (`*.txt`) is a
  **collection query** whose result type is a *list*, so zero matches = the empty
  list = a complete, honest answer, not an absence. Rejects bash's literal
  pass-through as a footgun.

## Decisions needed

- [ ] **Regex literal + absolute-path rule** *(direction chosen — see the block in
      [`DESIGN.md`](DESIGN.md) "Quoting and escaping")*. **Keep `/…/`** as the regex
      literal; in a match slot a leading-slash word is a regex only when it is a clean
      `/BODY/` (closing `/` final, no unescaped interior `/`), otherwise it is a
      path/glob — so absolute globs/paths go bare, no `glob("…")` wrapper. Known
      **residual** (accepted): a single segment with a trailing slash (`$p ~ /tmp/`)
      reads as the regex `tmp`; workaround is `$p ~ /tmp`, or `glob(…)`/`==`. Set
      aside (documented as alternatives in DESIGN.md): the `rx'…'` **regex-literal**
      sugar and RHS string→regex coercion. The `r'…'` / `r"…"` **raw strings** are
      *adopted* (Model B, below), not set aside. Still open under this direction:
  - [x] **String→regex coercion on the RHS — decided: no coercion (for now).** A
        plain string / `$var` on the `~`/`match` RHS stays an **error**; a regex must
        be explicit (`/…/` or `re($pat)`). Keeps the no-silent-coercion rule and
        avoids the "quotes mean literal" inversion. Revisitable.
  - [x] **String model — decided: Model B.** `"…"` interpolates + escapes; `'…'` is
        non-interpolating but **escaped** (Python `str`: `\n \t \r \e \\ \'` + `\u{…}`,
        `$` literal, unknown escape is an error); `r'…'` / `r"…"` are **raw** (regex
        source, paths). This retires the original "keep `'…'`'s two escapes or go
        fully raw" question — `'…'` is no longer raw; rawness moved to `r'…'`.
  - [x] **Regex-flag modifiers — decided: coexist.** Regex values take `:` modifiers
        (`re($x):i`, `$re:m`, `:s`) **and** the `--ignore-case` constructor flag
        stays — both spellings supported. A **parse-affecting** flag is *not* a
        post-hoc modifier (`re()` is fail-loud and compiles the unflagged pattern
        first): use `re($x --extended)` for a dynamic pattern, and reserve trailing
        `:x` for a `/…/` literal that folds it in before compilation (`/…/:x`).
        `--literal` stays a constructor argument.
  - [ ] **Value-producing raw heredoc** — the decided both-quote-kinds raw form is a
        heredoc, but the only heredoc specified today is command-redirection (feeds
        bytes to a command; an unquoted delimiter would expand). A raw,
        *value-producing* heredoc spelling still needs defining.
- [ ] **Arithmetic operators** *(direction chosen — see the "Arithmetic" section in
      [`DESIGN.md`](DESIGN.md))*. mesh has `Value::Integer` (i64, already checked —
      `+=` past `i64::MAX` is a loud `numeric overflow`) but no operator beyond
      `+=`, while `DESIGN.md` wrote infix arithmetic in three places before ever
      specifying it: `$m:int + 1` (`:replaceall` callback), `port: $base + 1`
      (named arguments), and `$a:ms / $b:ms` (the time model, whose argument for
      *not* needing a float type rests on integer `/` existing). Decided: nushell's
      two contexts (a statement starting with a number, and parens wherever a value
      is expected), `+ * / %` with Rust/bash truncation and dividend-signed
      remainder, `:pow(n)` rather than `**`, and `0x`/`0o`/`0b` literals with
      Python-rule `_` separators. Still open under this direction:
  - [ ] **Implement it.** The semantics are settled in `DESIGN.md`; no code
        exists yet — `+=` is still the only operator, `puts ($n + 3)` is a
        syntax error, and `1_000` / `0xff` parse as strings. Blocked on the two
        questions below, since binary `-` cannot be deferred past the first
        `$a - $b` and the literal rule has to be decided before any are parsed.
  - [ ] **How subtraction is spelled.** `*`, `/` and `%` are unclaimed, but a
        spaced infix `-` is already **glob exclusion** (`*.txt - *.bak`), and both
        it and arithmetic want value positions, so `$a - $b` is ambiguous on its
        face. Options: type-directed dispatch (ints subtract, globs/lists exclude),
        which is what `+=` already does and what the proposed `-=` is specced to
        do; a modifier form (`$a:minus($b)`, matching `$m:int` / `$a:ms`); or
        leaving it to the parenthesised context, inside which no glob can appear.
        `~` is **not** available — it is mesh's infix match operator. No other
        shell has this collision, since bash's `$((a-b))` and fish's `math` both
        put arithmetic inside a delimiter.
  - [ ] **Whether a leading zero means octal.** `007` parses as `7` today, silently
        dropping the zeros — the one answer that is certainly wrong. Either it is
        octal (C, POSIX `$(( ))`, and the `chmod` tradition), which forces `08` and
        `09` to become **errors** as invalid octal digits, or a multi-digit literal
        with a leading zero is rejected outright and `0o007` is the only octal
        spelling (Python 3's answer). Note the usual argument for the C form does
        not apply here: file modes travel as **command arguments**, which never
        parse as integers, so `chmod 0644 f` is unaffected either way — the octal
        reading would only ever govern `n = 007`, where nobody is writing a mode,
        while `n = 09` breaking is a real cost.
- [x] **What `fg` does with a job that has already finished** — *decided: hand
      back the status the job already carries*, which is the reason a completed
      job is kept in the table at all. `JobTable::info` polls with
      `waitpid(WNOHANG)` to answer `$sh.jobs` and reaps the pid while keeping the
      record, so `fg` signaled a process group with no members left and failed:
      `printf '/bin/true &\nsleep 0.3\nfg\n' | mesh` printed `mesh: fg: No such
      process (os error 3)` and returned 1, every time. Since *every* executable
      refreshes `$sh.jobs`, merely running a command in between was enough, so
      reading the table decided what `fg` did — the one thing keeping the job was
      meant to prevent. `fg` now polls before signaling and, for a finished job,
      reports `[n] Done (status) cmd` and returns that status; the note is what
      keeps it distinguishable from a successful resume, which ruled out simply
      ignoring the `ESRCH` the way the exit-time hangup does. The alternatives —
      reaping first so `fg` says `no such job` like bash after its own
      prompt-time reap, or keeping the failure with a better message — both throw
      away a status that is sitting right there. `wait` was implemented on the
      same rule, so waiting after the fact reports what waiting through it would
      have.
- [x] **Choose a repo license** — *decided: `MIT OR Apache-2.0`* (the
      Rust-ecosystem norm, as used by Rust itself). Nothing constrained the choice:
      all current/planned deps are permissive (`reedline`/`nix`/`crossterm` MIT)
      except `nucleo` **MPL-2.0** (weak, file-level copyleft — compatible with a
      permissive project). `LICENSE-APACHE`/`LICENSE-MIT` are at the repo root.

## Icebox / decide later

- [ ] **`$env.PATH` is one more argument for auto-flattening at the boundary.**
      Now that path-type entries are lists, `puts $env.PATH` needs `...` or a
      `:join` like any other list — consistent, and what makes `+=` append an
      entry rather than concatenate a string. But `PATH` is the list users reach
      for by reflex from other shells, where it is plain text, so it sharpens the
      general question already in this icebox: whether list values should flatten
      automatically at the external-command boundary. Decide it there, as one
      rule, rather than carving out a special case for path-type entries.
- [x] **`return` with no argument — use the last status.** `exit` already does
      this (a bare `exit` leaves the last command's status). Apply the same rule
      to `return` when it lands with function bodies.
- [ ] **Reserve only bare `_` as discard, allow `_name`.** Today a name must
      start with a letter, so a leading underscore is rejected wholesale (`_` and
      `_x` alike) — `_` is the discard pattern (`DESIGN.md`). Reconsider narrowing
      the reservation to **bare `_` only**, letting `_name` (underscore + letters)
      be a valid identifier, the common "intentional / private / unused-but-named"
      convention. Would touch `read_name` (allow a `_` head as long as the whole
      token isn't just `_`) and the `GRAMMAR.md` name rule.
- [ ] **Optional commas + word×list distribution in list literals.** Two related
      list ergonomics, motivated by the bash `mv foo{,bak}` idiom (rename
      `foo` → `foobak` in one word):
  - **Optional commas** — accept `[a, b, c]` as well as `[a b c]`. Decide whether
    *empty* elements are allowed (`[, bak]` → an empty-string first element),
    which is what would make `foo[, bak]` a terse cross-product.
  - **Word × list distribution** — `pre[a b]` → `prea preb` (distribute a prefix
    over a list), the list-native analog of brace expansion, so `mv foo['' bak]`
    or `mv foo[, bak]` → `mv foo foobak`. Blocked on a disambiguation rule versus
    the **glob character class** `[abc]` (already implemented): `foo[a b]` differs
    from the class `foo[ab]` only by a space.
      Note: bash-style **braces are already kept** (`DESIGN.md` "Braces — kept";
      `mv foo{,bak}` is the specced idiom), so this is about whether the list
      syntax should *also* cover it, not a missing capability. Leaning (from
      discussion): keep `{,}` for textual expansion, keep `[]` for real list
      values, maybe add optional commas — but don't overload `foo[…]` for
      brace-style expansion (small payoff, muddies the glob-class / list / index
      story).
- [ ] **Empty-glob warning (optional).** Keep behavior "empty always", but
      consider *warning* on an empty glob expansion while still proceeding — mesh
      is the only party that can detect it (the argv boundary carries bytes, not
      lists, so the emptiness is erased at `execve`; a downstream `grep` can't
      tell `grep foo *.log`-matched-nothing from `grep foo`). Interactively it
      could even prompt what to do. Warn on an empty *glob*, not on a genuinely
      empty `$list`.
- [x] Reading a script file as an argument (`mesh script.mesh`) vs. stdin only —
      *landed*, see "Beyond M3 — Invocation".
- [ ] Allow list values to flatten automatically at the external-command
      boundary, so callers do not have to write the explicit `...` operator.
- [x] `-c "…"` one-shot command flag — *landed*, see "Beyond M3 — Invocation".
- [ ] Whether satellite helpers (`vcs`, prompt) are Rust workspace members or
      standalone (per-helper call; see `DEVELOPMENT.md`)
