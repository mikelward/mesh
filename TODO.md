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
      a `Stdio` slot. Duplicating an unopened descriptor is `EBADF`. Deferred:
      closing one (`n>&-`).
- [x] Descriptor duplication: `2>&1`, `>&2`, `<&0`, and the both-streams forms
      `>& file` and `&> file`. A bare `>&` picks its meaning from the token as
      written, so a computed target (`>&$fd`) is refused rather than guessed at.
      Deferred: closing a descriptor (`n>&-`).
- [x] Heredocs: `<< END … END` interpolates its body (`$…` plus the `"…"` escape
      set, resolved through the command grammar so a heredoc and a string cannot
      disagree); `<< 'END'` is raw. The body reaches the command as an unlinked
      temporary file rather than a pipe, so a body larger than the pipe buffer
      cannot deadlock the shell, and a line-at-a-time reader waits for the
      delimiter directly rather than re-parsing the body per line. Deferred:
      backgrounding a heredoc, and the value-producing spelling (still
      unspecified — see the design entry below).
- [x] Here-strings: `cmd <<< word` feeds the expanded word plus a trailing
      newline, bash's behavior. The word expands like any other argument and
      must come to exactly one, the rule every redirection target follows; it
      travels by the same unlinked temporary file a heredoc body uses. `<<<`
      names no descriptor, and backgrounding one is refused for the reason a
      heredoc's is.
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
- [ ] A `source` builtin, and the input **origin** (`script` / `sourced` /
      `command` / `stdin` / `interactive`) plus `$sh.source` that `DESIGN.md`
      §"Startup and invocation" leaves as a TODO — a file needs to know it is
      being sourced, and where it lives.
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
- [ ] The rest of `$sh.*`: `$sh.options` and the hook maps.

## Loose ends

Small items rescued from pull requests that were closed as superseded — the bulk
of each PR had landed by another route, but these pieces had not.

- [x] **FreeBSD compile-check in CI.** `cargo check --workspace --all-targets
      --target x86_64-unknown-freebsd` runs alongside the macOS cross-check, so a
      BSD-only mistake in `mesh-platform` no longer passes both runners
      unnoticed. It needs no cross compiler — nothing here builds C for FreeBSD,
      and `cargo check` never links — so it is a target install and one command.
- [ ] **Carry `fork` isolation into the build track.** `DESIGN.md` specifies
      `fork { … }` and `fork func name(params) { … }` as the explicit isolation
      forms, but the keyword is absent from `GRAMMAR.md` and `docs/`, is not
      listed among the deferred syntax there, and does not parse — `fork { pwd }`
      is a syntax error today.
- [ ] **Document bold input in `DESIGN.md`.** Interactive input renders bold
      (`repl.rs`, `input_highlighter`), but the *design* — uniform weight rather
      than token-aware color, live as you type, surviving Enter into scrollback,
      and whether it gets a `$sh.options.bold-input` off switch — is written down
      nowhere.

## Redirection edge cases

Found by review on the descriptors-above-2 work and deliberately deferred: each
needs a descriptor above 2, a resource limit, or a failed `exec` to reach, so
none affects a redirection that worked before that change. Every one below was
reproduced against the built binary and compared with bash.

- [ ] **A failed `exec` writes into the redirection target.** `mesh_no_such_command
      4> out` puts Rust's binary `NOEX` packet into `out` and exits 1 instead of
      127, with no `command not found`. `Command::spawn` makes a private
      close-on-exec pipe for the child to report `exec` failures on, it takes a
      low descriptor, and the hook that installs descriptors above 2 overwrites
      it; `std` exposes no way to see or reserve that descriptor, and `pre_exec`
      runs after the pipe already exists. The fix is to `fork` and `execvp`
      directly for these stages, as `fork_in_shell` already does for in-shell
      ones — `execvp` sets `errno`, so the child reports 126/127 itself and no
      private pipe is involved. Carries process groups, the interactive
      signal/terminal hooks and the `Stdio` wiring with it.
- [ ] **A backgrounded in-shell stage loses its pipe.** Two spellings reach it:
      `func f() { puts low }; f 3>&1 > file 1>&3 | tr a-z A-Z &`, where stdout is
      moved away and put back, and `func f() { sh -c 'echo low >&2' };
      f 2>&1 > file | tr a-z A-Z &`, where the pipe ends up held by stderr alone.
      Both print `low` directly where bash pipes `LOW`, and both because
      `stdout_is_redirected` scans `cmd.redirs`
      for anything targeting fd 1 and so trips on the `>`, even
      though source order puts stdout back on the pipe. It has to be based on
      stdout's *resolved* destination — which for a background stage means
      resolving the redirections without opening anything, since the opens are
      deferred to the child. `piped_out` must keep tracking the final
      destination too: if stdout really ends on a file, a `SIGPIPE` is real.
- [ ] **A duplication that cannot be afforded still lets later targets be
      opened.** At `ulimit -n 5`, `true 3> foo 4>&3 > existing` truncates
      `existing` and then fails with `EMFILE`; bash fails while applying `4>&3`
      and leaves the file alone. Validation moved into the source-ordered
      opening walk, but the duplication's actual `dup` still happens afterwards
      in `resolve_sources`, so a duplication can be proved well-formed, a later
      `>` can truncate, and only then does the duplication turn out to be
      unaffordable. The fix is to **perform** each duplication during the walk,
      collapsing `open_paths` and `resolve_sources` into one source-ordered pass
      that acquires every descriptor as it reaches it — which is also what the
      previous item wants, and what would make the ordering guarantee structural
      rather than something each new failure mode has to be taught.
- [ ] **The descriptor-limit check does not run in source order.** It is a
      pre-pass over the whole list, so `true > existing 16> later` at
      `ulimit -n 16` reports `&16` without applying the earlier `>` — mesh leaves
      `existing` intact and never creates `later`, while bash truncates one and
      creates the other before failing. This is the one case in this list where
      mesh is *less* destructive than bash, so it is worth deciding deliberately
      whether to match: everything else here is a bug because mesh destroys
      something bash spares, and the source-order rule the rest of the
      redirection code enforces says the earlier redirections should have
      happened. If it should match, the check moves into the opening walk beside
      the duplication validation, which is where the two items above are heading
      anyway.
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
