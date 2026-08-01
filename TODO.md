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
      `\a` (`BEL`) joined the set later, for the title-setting prompt idiom
      `"\e]0;mesh\a mesh$ "` that every other shell spells that way, with
      `\b \f \v` alongside it so the set has no arbitrary hole. `\0` stays out: it
      would build a value `execve` and the environment both refuse. Adding an
      escape can never change what an existing script means, since an unknown one
      is an error rather than a literal — which is what keeps this open rather than
      a one-time decision.
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
      rejects surplus operands. `CDPATH` search landed later — see
      "Beyond M3 — Navigation". Still deferred: `--physical`, autocd, logical cwd.
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
        in [`GRAMMAR.md`](GRAMMAR.md).
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
- [x] Read the help real programs print: git's sentence-shaped table caption,
      cargo's `build, b` aliases and next-line value lists, docker's starred
      plugin commands, and the punctuation usage lines wrap their flags and
      operands in. Parser tests run against `--help` output captured verbatim
      under `crates/mesh-core/tests/help/`.
- [x] Complete a `PAGE` operand from the installed manual — `man l<Tab>` offers
      pages, not the current directory's files.
- [x] Load curated completion specs — `$XDG_DATA_HOME/mesh/completions/`, named
      the way the manual names the same thing (`git`, `git-commit`), so a
      subcommand's spec sits beside its command's. Read before anything is
      resolved or run, so one holds for a command that is not on `PATH` at all,
      and read afresh each time rather than cached, so editing one takes effect
      at the next Tab. Its own line-based format rather than the `--help` parser:
      a curated spec exists for when the heuristic guess was wrong, so it says
      its value types instead of having them inferred from a metavar. A file that
      says nothing falls through to the generated spec rather than answering with
      an empty one. A command word is a file name, never a path — one with a
      separator in it is refused rather than joined.
- [x] Add man-page-derived specs, the layer between curated and the `--help`
      probe. The page is looked for beside the *executable* — `<prefix>/bin/tool`
      is documented under `<prefix>/share/man` — rather than through `MANPATH` or
      `$PATH`, which is what makes a system page untrusted for a `PATH`-shadowing
      local binary: `./tool` is documented beside itself or not at all. Cached on
      the page's own path, size and mtime plus `MANPATH`, so a docs-only package
      update re-parses. A parse that finds nothing is not cached and does not
      answer, so an unreadable page falls through to the probe rather than
      replacing it with less.

      Declarations are taken only *outside* an `.RS` block. A page cites options
      in its prose constantly, each on a line of its own, which opens with an
      option exactly the way a declaration does; the block is what tells them
      apart. Subcommands are left to the probe — a page documents them in
      whatever prose shape its author chose, with none of the table structure
      `command_names` keys on. Both roff dialects are covered by fixtures under
      `crates/mesh-core/tests/man/`.
      Formatting is `man`'s job, not mesh's: `man -l <path>` decompresses the
      page, picks its macro package, and — read through a pipe rather than a
      terminal — returns plain text with no escapes or overstrike to strip. That
      is one process per page against a roff implementation here, and it is what
      makes every dialect and every compression scheme work alike. It also means
      this layer is not quite "runs nothing" any more; it runs a *formatter over a
      data file*, which is a different bet from running the user's command.

      A `man` that is absent, or is the advisory stub a minimized image ships
      (which prints its notice and exits 0), yields no options and falls through
      to the probe. A zero exit status does not mean a page was rendered, so
      nothing keys on it.
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
- [ ] **Exporting a function to a child mesh (`export -f`).** No spelling today.
      The cases that already work need none: a `fork` block, a pipeline stage, and
      a backgrounded function all inherit the func table as a memory copy, so the
      gap is only a *new* mesh process — `mesh -c`, a `#!/usr/bin/env mesh`
      script, `find -exec mesh -c`, `xargs mesh -c`, `sudo mesh`. Bash's answer is
      `export -f name`, which puts the body in the environment as
      `BASH_FUNC_name%%=() { … }` for every child bash to reparse. Weigh three
      channels rather than copying that one. **(a) The environment**, bash's:
      inherited by every process rather than just mesh, so a definition rides into
      programs with no use for it; it spends the `ARG_MAX` budget argv shares; and
      reparsing environment text at startup is where Shellshock lived — the bug
      was the *parse*, not the export, but the parse existed only because the
      environment was the channel. Taking it means the reader accepts a
      **definition and nothing else**, never arbitrary source, the same discipline
      the value channel's one-literal reader needs. **(b) A startup file**, which
      needs no new mechanism at all: a func in `env.mesh` is already in every
      mesh, so "export" may just be "put it where every mesh reads it" — paid for
      by parsing it on every invocation, including the ones that never call it.
      **(c) An explicit flag or fd** (`mesh --with f -c …`), where the definition
      crosses only where asked and nothing inherits it silently. Two questions any
      of them must answer: a func's source text becomes a **compatibility
      surface** between mesh versions, which a fork never had since it shares the
      binary; and a func closes over bindings that do not cross, which is exactly
      why `:repr` refuses to write one and why a func cannot ride the value
      channel either. No new dependency, so the cost is startup parse time and
      environment size rather than build or binary size.

## Beyond M3 — Invocation

- [x] Run a script named on the command line (`mesh script.mesh a b c`), a
      command string (`-c "…"`), or stdin (`-s`), alongside the existing
      interactive and piped paths. Option parsing stops at the first operand so a
      script's own flags reach it; `--` ends it explicitly. A script is parsed as
      one unit, so a syntax error rejects the whole file; a missing script exits
      `127` and an unreadable one `126`. Shebangs work by way of `#` comments.
- [x] `--help` and `--version`.
- [x] A `help` builtin, bash's `help` in mesh's shape. Bare `help` prints mesh in
      one screen: every builtin with its usage, then every keyword and operator
      with the shape it is written in, each with a one-line summary. `help NAME …`
      explains one — a builtin's entry is exactly what `NAME --help` prints, and a
      keyword's shows its syntax, which is the only way to ask (`if --help` is an
      `if` whose condition is a command called `--help`). Every reserved word the
      parser knows and every operator a line can carry answers, asked for as it is
      typed (`help unless`, `help '+='`); where several share a row that row
      explains the family, so `help else` explains `if`, and the tests hold both
      lists against the table. The
      builtin side reads the one table `is_builtin`, completion, and each
      builtin's `--help` already answer from, so a builtin cannot be dispatchable
      but undiscoverable; each builtin's `--help` now opens with that summary, as
      clap's generated help does. A name that is neither is an error rather than a
      lookup elsewhere: an external command's help is its own, and `NAME --help`
      asks it.
- [x] A `whence` builtin — the name lookup every shell spells differently
      (bash's and fish's `type`, nushell's `which`, ksh's `whence`, which is the
      spelling taken, `-a` / `-q` included as `--all` / `--quiet`). It reports
      what a bare word *is* in resolution order — syntax, builtin, function, then
      the executables `PATH` holds — leading with the winner and naming what that
      shadows, or listing every match under `--all`. Because mesh keeps bindings
      in a namespace of their own, a variable or `$env` entry of the same name is
      reported alongside rather than shadowed, and is asked for **without a
      sigil**: `$xs` would expand before the builtin saw the name. A word with a
      `/` is a path operand, read as command resolution reads it. `--quiet`
      leaves only the status (`0` found, `1` not), which is `command -v`.
      `type` is deliberately not taken: mesh has value types and `:type` already
      asks a path's, so it stays free for the value question — which is `:repr`.
- [x] **Rename `whence` to `type`, with `-t` / `-P` / `-a`** *(landed)*. Bash's
      name, bash's flags, bash's words. `whence` stays reachable as a rename
      pointer, as do `what` and `where`; none is reserved, so a user function may
      take any of them (`func what()` works today and must keep working). **Do not
      claim `which`** — in bash it is an external program that cannot see builtins
      or functions, and mesh keeps that, so `which cd` finds nothing here exactly
      as it finds nothing there. It is also the only one of the five with a real
      binary on disk, so claiming it would mean shadowing a program rather than
      improving a not-found message.

  - [x] **`-t` prints one word**: `function`, `builtin`, `file`, `keyword`,
        `variable`. Bash's tokens, because this output is *compared*, not read —
        `case "$(type -t "$1")" in function)` is the shape a port carries over.
        `variable` is the one addition; bash's `type` does not see bindings.
        Nothing printed and status `1` when the name is not found.
  - [x] **`-P` prints only a `PATH` hit**, ignoring functions and builtins, and
        nothing with status `1` otherwise. This retires the hand-rolled
        `for d in $PATH` loop an `shrc` carries because `type -P` is not portable.
  - [x] **`-a` as the short form of the existing `--all`.** `--quiet` stays a mesh
        convenience over `>/dev/null`.
  - [x] **One vocabulary in every form** — the prose says `if is a shell keyword`,
        never "is syntax", so the sentence and `-t` cannot disagree. Follow bash's
        wording wherever there is no reason to differ (`cd is a shell builtin`,
        `ls is /usr/bin/ls`); keep what mesh has a reason for — the detail line,
        the variable row, and naming what a winner shadows.
  - [x] `func type()` stops being definable once `type` is a builtin, the way
        `func whence()` is refused today. Worth a test pinning that, and one
        pinning that `func what()` / `func where()` still work.
  - [ ] **This likely closes the `:kind` / `:where` question** in the predicate
        vocabulary: `type -t` *is* `:kind` and `type -P` *is* `:where`. What a
        modifier would still add is use in expression position without a capture,
        which `type(NAME)` returning a map already covers. Re-read that item
        once this lands.

- [ ] **`type(NAME)` as a value call**, returning the report as a map
      (`[kind: external, path: /usr/bin/git, shadows: […]]`) rather than text, so
      a script can branch on the kind instead of matching prose — the shape
      nushell's `which` returns as a table. It retires the last `command -v`
      idiom: `type --quiet` answers "is it there", but not "where, and as
      what". Needs the map's key set settled (one entry per `Finding` kind, and
      whether a name in two namespaces yields a list) and builtins to reach
      value-call position, which today only `style` / `link` / `re` do.
- [x] First slice of the read-only `$sh` namespace: `$sh.args` (a real list, not
      `$1` / `$@` / `$#`) and `$sh.name`. `sh` joins `env` as a reserved name.
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
- [ ] **Should `logout.mesh` run for a *non-interactive* login shell?** It does
      today — the gate is `options.login` alone (`repl.rs`, in the function that
      runs on the way out) — and bash's equivalent does not: `~/.bash_logout` is
      read by an **interactive** login shell. Checked both ways rather than
      assumed:

      ```
      mesh -l --norc -c 'puts body'   → body, then logout.mesh runs
      bash --login    -c 'echo body'  → body, and .bash_logout does not
      bash --login -i                 → .bash_logout runs on the way out
      ```

      `DESIGN.md` §"Startup and invocation" says only "on login-shell exit", so
      it does not settle the interactivity half either way, and nothing chose
      this — it is what the one-condition gate happened to do. Worth deciding
      deliberately, in either direction: a `mesh -l -c …` in a cron entry or an
      `ssh host command` runs a teardown file written for a human leaving a
      terminal, which is the argument for bash's rule; against it, `logout.mesh`
      is the file for "this login session is over" and a non-interactive login
      session is still over.

      Surfaced asking a nearby design question — whether an **`on logout`**
      event is worth having for interactive login shells. The lean is no: `on
      exit` registered from `login.mesh` already fires only for login shells,
      because `login.mesh` only runs in one, so the registration site does the
      filtering and `logout` would be a second hook on the same instant with an
      ordering question attached. The one case it would serve is a handler in a
      **shared** `rc.mesh` that self-gates, and that wants one bit of state —
      there is no `$sh.login` today — rather than a new event.
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
      reports and removes it at its own time. The handle binding, `kill`, and the
      `%` sigils it deferred have since landed; indexing the table now yields a
      handle rather than a copy of the record.
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
- [x] **Reaping moved behind one owner, fed by `SIGCHLD`.** `waitpid` used to be
      called wherever an answer was wanted, which made job state only as fresh as
      the last command boundary and left changes noticed in one pass without an
      order between them. `reaper::drain` now walks the pids the shell owns, asks
      about each by name, and files what it gets stamped with the order it
      arrived; everything that used to call `waitpid` reads from there.
      This keeps the two properties that ruled out sprinkling
      `waitpid(-1, WNOHANG | WUNTRACED)` at the call sites. Nothing steals: a
      transition the drain takes on one job's behalf is *stored* rather than
      discarded, so a blocking wait for another finds it waiting. And the handler
      never touches the table — it does no work at all — so looking still cannot
      change what the shell does.
      Asking by name rather than discovering with `waitid(P_ALL, …, WNOWAIT)` is
      what keeps the drain unobstructable, and what keeps it away from children
      that are not the shell's: the completion helper waits on its own with
      `Child::wait`, and a child inherited across `exec` belongs to whoever
      spawned it. Discovering instead deadlocks, because an unowned pid's
      transition stays pending and every probe answers with it.
      A wait sleeps in `sigsuspend` with `SIGCHLD` held by `WaitCatcher`, which
      hands the signal over atomically — a blocking `waitpid` leaves a window
      where a state change is handled and forgotten, and the waiter then sleeps
      with an undrained transition behind it. The handler forwards to the waiting
      thread with `pthread_kill`, since mesh runs a reader thread for `$(…)` and
      `:capture` and a process-directed signal reaches whichever thread does not
      block it.
      A self-pipe did this first and cost seven review findings, all of them the
      same shape: a pipe is a descriptor, mesh lets a script name any descriptor,
      and the endpoint kept turning up where a script had addressed it — including
      by path, through `/dev/fd/100`, which no care about descriptor *names* can
      cover. `pthread_kill` needs no namespace at all.
- [ ] **Stops inside the same sub-millisecond window are still ordered
      arbitrarily.** Recency follows the order the drain *took* each transition,
      which is the order they happened as long as the shell drains between them.
      Two that are both pending before a drain runs are ordered by whatever the
      drain's walk over its owned pids reaches first, which is a `HashSet` and
      therefore nothing.
      Measured, with two jobs stopped from outside: at a 1ms gap — one `fork`
      between the signals — `bg %+` names the job that stopped last 12 times out
      of 12, as it does at 5ms, 20ms, 50ms and 150ms. Issued back to back with
      nothing between them it is 8 of 15, which is a coin flip and the honest
      answer: two signals in one syscall burst *are* concurrent, and there is no
      order to recover. For contrast the same script before this work was 0 of 15
      at every gap, because table position always won.
      This is the residue of the older "ordered by the table, not by when they
      happened" entry, and what remains of it is genuinely unavailable:
      `SIGCHLD` does not queue, so a handler cannot count what coalesced, and no
      portable interface records *when* a child stopped — `/proc/pid/stat` has a
      start time and nothing else. See the `SA_SIGINFO` entry below for the one
      move that would narrow it further without leaving POSIX.
- [ ] **Record `si_pid` from the handler to narrow the ordering window.**
      `SA_SIGINFO` hands the handler a `siginfo_t` naming the child that caused
      *this* delivery. Recording those in arrival order — a preallocated ring and
      atomics, which is async-signal-safe — would move the ordering boundary from
      "between drains" to "between signal deliveries", and a delivery is much
      cheaper than a drain, so the window above gets smaller.
      It cannot close it. `SIGCHLD` is a standard signal, so two arriving before
      the handler runs still collapse into one delivery and one `si_pid`; the
      second child's place in the order is gone before any code can see it. So
      this is a narrowing, not a fix, and worth doing only if the window turns
      out to matter in practice — it costs a handler that writes state, which the
      current one deliberately does not.
      The alternatives all leave POSIX and none is worth it: `signalfd` still
      delivers the coalesced signal; `pidfd_open` plus `epoll` orders *exits*
      precisely but has no stop notification, and costs a descriptor per job;
      `kqueue`'s `EVFILT_PROC` genuinely queues and is the closest thing to a
      real answer, but only on the BSDs; the netlink proc connector has no
      stop event and wants `CAP_NET_ADMIN`; and `ptrace` would work by making the
      shell a debugger, which changes signal delivery and locks out real ones.
- [ ] **Notify about a finished job when it finishes, not at the next prompt.**
      The table is now current the moment anything drains, but the *notice* is
      still printed by `reap` at the top of the REPL loop, so a job that ends
      while a line is being typed is announced only once that line is submitted.
      Bash is the same by default and prints immediately under `set -b`; doing it
      here means the line editor waking on a child's state change and redrawing
      the prompt around the notice, which is a reedline integration question
      rather than a job-control one — reedline owns the terminal read, and
      `SIGCHLD` carries `SA_RESTART` outside a wait precisely so it does *not*
      disturb that read. The `jobdone` hook now fires from that same `reap`, so
      it inherits the same timing and moving the notice moves the hook with it —
      neither could ever run *from* the handler, being arbitrary mesh code where
      the handler may only forward.

      What reedline offers is its `external_printer` feature, which prints above
      the prompt and repaints around it — the redraw half, solved. The cost is
      the other half: attaching a printer switches reedline's terminal read from
      blocking to `poll`ing on a 100ms interval, and it is attached for the life
      of the editor, so an idle prompt with no jobs at all wakes ten times a
      second forever. That is the decision this is really waiting on, and the
      shapes are: accept the polling; make it opt-in as bash does with `set -b`
      (a `notify` option), so only sessions that ask pay for it; or rebuild the
      editor as the job table goes empty and non-empty, so the polling lasts
      exactly as long as there is something to report. The last is closest to
      free at the prompt and the most machinery, and needs the editor's history
      and session state to survive a rebuild.
- [x] **A `mesh-core` unit test occasionally hangs the whole suite.** Seen three
      times in roughly a dozen `cargo test --workspace` runs, always after the
      CLI tests have passed, and in a *different* test each time —
      `exec::tests::spawn_failure_reclaims_the_terminal` once,
      `repl::tests::named_prompt_hooks_replace_in_place_and_run_before_the_prompt`
      another — each reported as "has been running for over 60 seconds" and never
      finishing.

      It was the reaper's lock, and the suspected mechanism was the right one.
      The tests `fork()` from the multi-threaded harness; `fork` copies the lock
      but not the thread holding it, so a child that inherits a held store waits
      on a thread that does not exist in it. The first thing a forked child calls
      is `reaper::forget_all`, which locks — after which the parent's blocking
      `waitpid` never returns.

      Reproducing it by running the suite was still hopeless: 25 more clean
      rounds, on top of the three earlier attempts. Holding the store from
      another thread and forking makes it deterministic instead, which is what
      `a_child_forked_while_the_store_is_held_can_still_reach_it` does — it hung
      every time before the fix.

      Fixed with `pthread_atfork`: the store is taken in the `prepare` handler
      and released in both the parent and the child, so the copy a child gets is
      never held. A `fork` now waits for whoever holds the store, which costs
      nothing — every critical section in `reaper.rs` is short, and `drain`
      releases before its `waitpid` loop.

      Note the allocator was the other suspect and is not implicated: user
      `prepare` handlers run before `fork` locks the malloc arenas. If a hang is
      ever seen again, the remaining process-global that a forked child reaches
      is the `OnceLock` in `reaper()` itself, whose initialization is not covered
      by the handler it registers.
- [ ] **A stopped job killed from outside can still be reported stopped.**
      `wait` reports a stopped job's cached stop rather than blocking, since a
      stopped job does not finish on its own. Our own `kill -KILL` clears that
      mark so the wait blocks for the real status, but a `kill -9` typed in
      another terminal cannot be intercepted that way: if the drain has not yet
      run when `wait` is asked, the stop is still what it knows. Closing it means
      the wait consulting the kernel about whether the process is still stopped,
      which has no portable answer, or treating any pending nudge as a reason to
      re-drain before trusting the mark.
- [x] **Job handles.** `j = cmd &` binds the job rather than the status of
      launching it, as a distinct `Value::Job` carrying the id. Reading a member
      resolves it against the live table, so `$j.state` moves on with the job
      instead of freezing as a record captured at bind time would; a bare `$j`
      has **no byte form**, which is what keeps `kill $j` a job where
      `kill 49001` is a pid. `$sh.jobs[2]` is a handle too, per `DESIGN.md`, so
      the published record carries its `id`. The job builtins take either
      spelling by expanding their arguments as *values*, with a handle arriving
      as `%id` — the sigil form rather than a bare id, so it can never be read
      as a pid.
- [x] **`kill`**, taking the same job references `fg` / `bg` / `wait` take plus a
      bare pid. A job signals its whole **process group**, since a pipeline is
      several processes and signalling only the leader leaves the rest running; a
      pid signals just that process. `-9`, `-KILL`, `-SIGKILL` and `-s KILL` all
      name a signal, defaulting to `TERM`, and each target is signalled
      independently so one bad name does not stop the rest.
- [ ] **What a handle means once its job has left the table.** Waiting for a job
      removes it, so `$j.status` afterwards reports `job 1 is no longer in the
      job table` rather than the status it exited with — and `wait $j` is exactly
      when you would want to ask. `wait`'s own result carries it (`$sh.status`),
      so nothing is unreachable, but the handle going blind at the moment the job
      finishes is a sharp edge on the `$!` replacement.
      Retaining the final record is the obvious answer and needs three decisions:
      **which** jobs to keep (only those a handle still names is unknowable, so
      it is a cap or nothing), **how many** before the oldest is dropped, and
      **where the record comes from** — the last published snapshot is not it,
      since `wait` removes a job without publishing a `done` record first, so a
      naive retain-on-disappearance would keep a stale `running`.
- [x] **The rest of `wait`, and `disown`.** A bare `wait` takes **every job in
      the table** and several operands wait for each in turn; either way the
      status is **the last job to fail, or 0** — the pipefail rule mesh already
      applies to a pipeline, applied to the other place where several things
      finish at once. bash returns 0 regardless, discarding the one thing the
      caller waited to find out.
      "Every job in the table" rather than "every child the shell owns" is what
      makes `disown` sufficient: a disowned job is gone from the table precisely
      so nothing waits for it, and a second opt-out would otherwise be needed. A
      forked stage's background children are owned by the reaper — so they cannot
      become zombies — but were deliberately never jobs.
      `disown` drops the job from the table and from the exit hangup while
      leaving it reaped, which is the `abandon` state the reaper already had;
      `-h` keeps the job and buys only the hangup exemption `DESIGN.md` promises.
- [ ] **Should `wait` be able to hand back a list?** The status answers "did
      anything fail", which is what a script usually branches on, and loses which
      job failed and with what. `$sh.pipestatus` exists for exactly that reason
      on the pipeline side, so the shape is already in the language — the
      question is whether a bare `wait` should fill something like
      `$sh.jobstatus`, what it holds for jobs that were stopped rather than
      finished, and whether it is keyed by job id rather than positional (a
      pipeline's stages have an order; a set of jobs has ids). Worth deciding
      before anything depends on the scalar being the whole answer.
- [ ] The rest of `$sh.*`: the hook maps, `$sh.complete`, and `$sh.signal`.
      `$sh.options` has landed, along with the per-key mutability the rest of the
      configuration half needs.

      For the hook maps specifically, the options are worked out in
      [`docs/HOOKS.md`](docs/HOOKS.md) — what `on` does today, what `DESIGN.md`
      promises that is not built, and six decisions with their trade-offs.
      **None of the six gates another**, so they can be taken in any order; two
      are merely cheaper early. The map wants to be a **view over the one
      store** rather than a second one beside it, which gets harder with each
      new direct reader of `shell.prompt.hooks`. And **arity is exact in both
      directions** for a handler whose positionals are all required, so every one
      written before prefix binding lands is one that breaks if an event ever
      gains an argument — a `...rest` parameter or an optional trailing
      positional already absorbs the surplus, which makes this a preference
      rather than a deadline. The rest — callable handler values, an
      optional hook name derived from the function, whether registration keeps
      its eager existence check, and how signals are spelled — are independent.
- [ ] **Signal handling, end to end.** Nothing user-facing exists yet. What the
      shell does with signals today is entirely for its own account, and the
      boundary is the **terminal-owning loop** rather than interactivity:
      `ignore_interactive_signals` is called only from `run_interactive`, where
      it **ignores** INT/QUIT/TSTP/TTOU/TERM. A session interactive by *flag* —
      `mesh -i script.mesh`, `mesh -i -c …`, piped `mesh -i` — goes through
      `run_batch` or `run_piped` and keeps every default disposition, as a plain
      non-interactive run does. **HUP** is handled nowhere. The one piece of catch-and-resume
      machinery is `exec.rs`'s `SigintCatcher`, which exists so a blocking wait
      can be interrupted — the flag-and-check shape the rest of this should
      follow.

      Four pieces, roughly in dependency order:
  - [ ] **The registry, and its spelling.** `DESIGN.md` §"Signals" specifies
        `$sh.signal.<NAME>` insertion-ordered maps (`INT`, `TERM`, `HUP`, …,
        without the `SIG` prefix); `on int NAME FUNC` should work too. **Both, over
        one store** — the reason that is cheap right now is that the map surface
        does not exist yet, so there is nothing to reconcile: hooks live in a
        single flat `Vec<Hook>` keyed by `(event, name)` on `PromptConfig`, and
        whoever builds `$sh.signal.*` can make it a *view* over that rather than
        a second registry. Two stores that drift is the failure mode to design
        out, and today there is only one to preserve.
  - [ ] **Whether signal names share the event namespace.** `HookEvent` is a
        closed enum parsed from one flat set of lowercase words (`preprompt`,
        `precd`, `jobdone`, `exit`); adding `int` / `term` / `hup` to it puts two
        different kinds of thing in one namespace. The collision to settle first
        is **`exit`**: bash's `trap` conflates the EXIT pseudo-signal with real
        ones, while `DESIGN.md` keeps them apart — `$sh.exit` is called "the
        EXIT-pseudo-signal trap … already defined with the hooks", i.e. a
        lifecycle moment that happens to have a signal-shaped name. If signals
        share the namespace, `on exit` must keep meaning the lifecycle event.
        A prefix (`on signal INT`, `on sig:int`) is the alternative.
  - [ ] **Exiting because of a signal** — recorded in full with the `exit` hook
        entry under "External tool integration": bash runs its EXIT trap for the
        catchable fatal signals and re-raises so the parent still sees `128 + N`,
        and mesh runs nothing. Depends on the plumbing here, not on the registry.
  - [ ] **The three things `DESIGN.md` explicitly defers**: whether a handler
        may **suppress** a default (swallow Ctrl-C), exact SIGINT delivery
        mid-pipeline, and per-signal masking while a handler runs.

      Constraints that hold whatever is chosen: a handler cannot run *in* the
      signal handler (nothing mesh executes is async-signal-safe), so it is
      record-and-dispatch — a flag or self-pipe the main loop notices, with the
      wait loops in `exec.rs` made interruptible rather than `SA_RESTART`.
      **SIGKILL and SIGSTOP cannot be caught**, an OS rule, so registering for
      them is an error rather than a handler that never fires. And per
      `DESIGN.md`, `$sh.status` / `$sh.pipestatus` are snapshotted and restored
      across a handler, as `run_hooks` already does for every other hook.
- [ ] **`$sh.options` values are booleans, and its keys are flat.** Both hold for
      every setting that exists, and neither is a decision against the alternatives
      — they are simply what was needed. Two things would force the shape open, and
      it is worth knowing which:
  - [ ] A setting with a **third state**. The `auto`/`yes`/`no` shape `DESIGN.md`
        discusses is about *function* flags (§"Functions", the negatable/tri-state
        TODO), not about settings, so nothing here is waiting on it — but a
        settings-side case is easy to imagine (`ls --color=auto`'s question:
        decorate when the terminal wants it, not merely when interactive). Today
        `Options` is an array of `AtomicBool` and `assign` accepts only
        `Value::Boolean`; a third state means a per-setting value *type*, and the
        strict "`true` or `false`" message becomes per-setting too.
  - [ ] A **nested** setting. `DESIGN.md` already writes one —
        `$sh.options.complete.probe` — which the flat map cannot hold: the write
        path stops at one key past `options` and says a setting has nothing inside
        it. Deciding whether that key is spelled `complete.probe` (flat, with a dot
        in the name) or a real submap is what unblocks it.

## Beyond M3 — Rich output

`DESIGN.md` gives the output builtins a rendering rule richer than argv's, and
gives the prompt styled values. Both need the builtin to see a **value** rather
than the string the argv boundary would have produced.

- [x] **`puts` and `print` render values, not words.** Per `DESIGN.md` §"I/O": a
      scalar as itself, a list as its elements joined by newlines, a map as
      `key: value` lines, and a value with no byte form (a job or stream handle, a
      function, a pattern) as a loud error. `print` is the same builtin without the
      trailing newline. Both are dispatched with typed arguments by
      `repl::output_builtin_words`, which every command path goes through, so
      `puts $xs`, `puts $xs > out` and `puts $xs | cat` agree. Only a word ordinary
      expansion *cannot* render takes the typed route — value typing reads a bare
      literal as a number, and `puts 007` has to print `007`.
- [x] **One flag rule for builtins and functions.** Settled: builtins parse flags —
      several must, since `kill -9`, `disown -a`, `prompt --reset` and
      `on --remove` are their spellings — and `DESIGN.md`'s "`puts` takes no
      flags" means none *of its own*, never that its `--help` is data. The two
      sentences were not in tension. A word that **is** `--help` asks for help
      wherever it came from, which is what functions already did (`x = --help; f $x`
      prints usage), because expansion safety is about not splitting or globbing a
      value rather than laundering a flag. The real defect was `--`: detected and then
      left in, so `puts -- --help` printed `-- --help`. Now a builtin with **no**
      options has the first `--` removed centrally (`builtins::reads_options`, read off
      the usage line so it cannot go stale), while one **with** options keeps it —
      only that builtin knows where its options end, and stripping it centrally made
      `kill -- -9 %1` send SIGKILL and `prompt -- --reset` reset. `prompt` gained the
      handling `kill` already had.
- [x] **Styled values and `style()`.** `Value::Styled` carries text plus a `Style`
      (foreground, background, bold — the MVP set). It behaves as its text at every
      boundary that wants bytes: `Value`'s `PartialEq`/`Hash` are hand-written so a
      styled value *equals and hashes as* its text, ordering and `in` read the text,
      `+=` and every modifier flatten to a plain result, and argv, the environment,
      interpolation and `:repr` all see the text. Attributes are read in one place,
      `builtins::rendered_for_output`, and emitted only when the command's own
      stdout is a color-capable terminal — `redirects_stdout` plus the last-stage
      test make that a per-command answer rather than the shell's, since words are
      rendered before a redirection is opened. `NO_COLOR` (non-empty, per
      no-color.org) and `TERM=dumb` drop it. Re-styling is additive.
- [ ] **256-color and truecolor.** `Color` is the sixteen ANSI names, which is what
      every color-capable terminal agrees on. Going further needs a spelling for the
      value (`fg: 33`? `fg: "#8be9fd"`? both?) and a **downgrade** rule to the
      nearest of the sixteen for terminals that cannot show it — otherwise the
      capability question `style` was designed to avoid comes straight back.
- [x] **`link(text, url)`** (OSC 8) landed — see §"Terminal integration", where it
      is tracked with the other terminal sequences.
- [ ] **A `file://` helper for a path.** `link` requires a scheme, so linking a local
      path today means building `file://$host$path` by hand — and getting the host
      right matters over `ssh`, which is exactly why `link` will not guess it. A
      modifier (`$path:url`) reusing `cwd_url`'s encoder is the obvious shape; `OSC 7`
      already builds the same string for the working directory.

## Beyond M3 — Terminal integration

The escape/OSC surface a modern interactive shell drives, from the TODO block in
`DESIGN.md` §"terminal control". Everything here is **interactive-only** —
`set_interactive` is recorded by the interactive loop, so `mesh -s` on a terminal
and every piped run stay byte-exact — and **failure-ignoring**, since a decoration
that could change a command's status would be worse than a missing decoration.

Each landed decoration is on by default with a `$sh.options` off switch, except
bracketed paste, which is deliberately not optional. A new one here should arrive
with its switch: add an `Opt` variant in `options.rs` and read it through
`repl::decoration`, which pairs the setting with the interactive test.

- [x] **Shell integration / semantic prompt marks** (OSC 133). reedline emits `A`
      and `B`, the prompt's own boundaries, since it draws the prompt; the shell
      emits `C` before the output and `D` with the status after, from outside the
      `preexec` / `postexec` dispatch so a printing hook's output falls inside the
      region the marks bracket. A line abandoned with Ctrl-C gets a bare `D` with
      no status: nothing ran, so there is no outcome to report, but the input
      region reedline opened at `B` still has to be closed. A blank submission gets
      no marks and fires no hooks — it is not a command, for the same reason
      history keeps no row for it. `$sh.options.shell-integration` turns the marks
      off, reedline's `A`/`B` along with the shell's `C`/`D`: half the marks leaves
      a terminal reading the output as input, which is worse than none.
- [x] **Bracketed paste** (`CSI ?2004 h`). On, always: reedline's guard defaults
      to off, so without asking for it a paste's newlines each arrive as Enter and
      every line but the last runs before it can be read.
- [x] **cwd reporting** (OSC 7). `file://host/path`, percent-encoded per RFC 3986,
      with the path's bytes encoded as they are so a directory whose name is not
      UTF-8 still reports. Written once per prompt, after the `preprompt` hooks:
      one call site covers both halves `DESIGN.md` asks for — the first prompt is
      the startup report a fresh remote shell owes a new split, and any later move
      reaches the next prompt whatever caused it. `$sh.options.cwd-report` turns it
      off.
- [x] **Bold input.** The line being typed is drawn in bold — uniform weight rather
      than token-aware color, so it makes no syntax claim and reads on any theme —
      and survives Enter into scrollback, which is what keeps a command
      distinguishable from its own output after the fact.
      `$sh.options.bold-input` turns it off, read per repaint so the change lands
      on the next keystroke rather than the next session.
- [x] **Window/tab title** (OSC 0, screen's `\ek`). Automatic: `user@host: dir` at
      the prompt, the command line while a command runs, so a row of tabs reads at
      a glance. The sequence follows `$env.TERM` — `\ek…\e\\` inside screen or tmux,
      where the name belongs to the pane rather than to the outer window, OSC 0 for
      a terminal on the `OSC_TERMS` allowlist, and *nothing* for anything else.
      An allowlist because the two ways of being wrong are not equal: an unlisted
      terminal quietly has no title, while one wrongly assumed to take a title
      *prints* it at every prompt — `TERM=linux` reads `ESC ]` as a palette sequence
      and abandons it at the first non-hex byte, leaving the text on screen and the
      `BEL` ringing, and `ansi` and `sun` have no title at all. It is what bash, zsh
      and fish all do too. A name matches a family exactly or up to a `-` or `.`,
      the separators terminfo uses for variants, so `xterm-kitty` and
      `screen.xterm-256color` come along while `st52` — an Atari VT52 — does not
      get in on the strength of starting with `st`.
      Terminated with `BEL` rather than the `ST` mesh uses elsewhere, since every
      shell's `PS1` idiom spells it that way and terminals exist that answer only
      to that spelling. Control characters in the text become spaces and the title
      is cut at 96 characters: both the command line and the directory carry text
      mesh did not choose, and a filename holding an `ESC` would otherwise end
      mesh's sequence and start one of its own.
      `$sh.options.osc-title` turns it off. The clear on the way out is gated on
      having *written* a title rather than on the setting, so turning it off
      mid-session still cleans up after the titles already written, and a session
      that never titled anything emits nothing at all — not even the clear.
- [x] **Asking terminfo which sequence the terminal takes — investigated, and it
      does not work.** The plan was to read `hs`, `tsl` and `fsl` and retire the
      `OSC_TERMS` allowlist along with the multiplexer special case. The database
      does not carry the answer. Measured against ncurses 6.4.20240113: of 41 local
      entries, **five** declare `hs` — `tmux`, `tmux-256color`, `cygwin`,
      `rxvt-unicode`, `rxvt-unicode-256color` — and `xterm`, `xterm-256color`,
      `kitty`, `alacritty`, `foot`, `st` and `screen` declare none of the three.
      `xterm+sl`, the building block that would supply them, is not installed.
      Driving the title from terminfo would therefore mean **no title on almost
      every terminal anyone uses**, which is worse than the list by a wide margin.
      Where `tsl` does exist the values disagree about what a "status line" even
      is: `rxvt-unicode` says `\E]2;` (window title), `tmux` says `\E]0;`, `cygwin`
      says `\E];`. Note that tmux's answer is OSC 0 — adopting it would *replace*
      the `ESC k` mesh sends, which renames the tmux window in the status bar. That
      is a different target, not a better spelling, so terminfo would also cost the
      behavior we chose deliberately.
      The reason is historical rather than an oversight: `hs`/`tsl`/`fsl` model a
      *hardware status line*, and a title bar set by `OSC` was never retrofitted
      into that shape. Terminfo answers "how do I address a status line", not "does
      this terminal parse an `OSC`", which is the question mesh is actually asking.
      Left as-is: the allowlist is the accurate mechanism, and this entry exists so
      nobody spends the effort again. Capability *reporting* — `XTVERSION`,
      `DA1`, the `TERM_PROGRAM` variables — is the direction with something left in
      it, and would need a reply mesh currently never waits for.
- [x] **The prompt width scan counted escape sequences as columns.** It
      recognized only SGR, so an OSC title in a custom prompt — `prompt
      "\e]0;mesh\a mesh$ "`, the reachable case — was measured as if it printed and
      the continuation indicator came out that much too long. `escape_stripped_width`
      (renamed, since the old name was the wrong claim) now discounts OSC to either
      terminator and CSI with any final byte. Cursor motion is deliberately not
      modeled: reedline lays out the same line with every escape at zero width, and
      the number has to agree with the editor rather than with the terminal.
- [x] **OSC 633** — VS Code's dialect, chosen from `$env.TERM_PROGRAM == vscode`,
      which is what VS Code and its forks set. The same `A`/`B`/`C`/`D` boundaries
      under a different number — reedline ships the markers for its half — plus `E`,
      which hands over the command line. `E` is the reason to bother: VS Code parses
      plain `133` too, but only from `633;E` does it learn what the command *was*,
      which its re-run and command-label features need; left to `133` it reads the
      text back out of the echo and gets it wrong whenever the prompt or the editor
      is interesting.
      One dialect, never both — VS Code understands `133` as well, so sending both
      makes it count every command twice. `E` goes out before `C`, the order VS
      Code's own integrations use, so the terminal knows what is about to run before
      any output arrives. The command line is escaped as VS Code requires: `;` as
      `\x3b` since it delimits the payload (`sleep 1; puts hi` is ordinary to type),
      the backslash that introduces the escape as `\\`, and control characters as
      `\xXX` — a pasted two-line command carries a newline, and an `ESC` would start
      a sequence of its own.
      Read once per session, like `$env.TERM`: a dialect that changed mid-session
      would close a region in a language it was not opened in. Both are snapshotted
      at one point, *after* the startup files and before the first prompt, so an
      `rc.mesh` that sets either variable is honored — the marks ask for the dialect
      when they draw rather than holding one from before `rc.mesh` ran.
      `$sh.options.shell-integration` gates it, as it does `133` — one setting for
      the feature, not one per dialect. `633;P;Cwd=` is deliberately absent: `OSC 7`
      already reports the directory and VS Code reads it. The nonce `633;E` can
      carry is absent too — it exists for a script VS Code injects into a shell it
      launched, and mesh is not that.
- [x] **Hyperlinks** (OSC 8) — `link(text, url)`, a `style` sibling on the same
      `Value::Styled`, so the two compose in either order and everything that made a
      styled value safe to compute with holds for a link too. `Style` gained a `link`
      attribute rather than a value kind gaining a variant. The url is percent-encoded
      wherever RFC 3986 forbids it raw — which covers the sequence's own guard against
      an `ESC` ending it early *and* the ordinary case of a space in a path, an
      invalid URI a terminal may reject the link over — and a **scheme is required**, since
      a terminal needs an absolute URI and guessing `file://` would need a hostname to
      be right over `ssh`. `LINK_LIMIT` is 2083 encoded bytes, refused loudly, because
      past a terminal's own limit the whole sequence is dropped and the link text goes
      with it. `Decoration` now carries two bits instead of one: a link is **kept**
      under `NO_COLOR` (that silences the palette, and dropping a link would lose the
      url) but wants a terminal on the `OSC` allowlist, since `TERM=linux` would print
      the url. `takes_osc` is that question, now shared with the notification.
      Inside a multiplexer the sequence goes **raw**, unlike `OSC 9`: measured against
      tmux 3.4, raw is parsed and stored per cell and re-emitted (so the link survives
      a repaint tmux does itself), while the `DCS tmux;` envelope forwards its payload
      instead of drawing it and the link *text* never reaches the pane at all — with
      or without `allow-passthrough`. `OSC 9` is a point event with nothing on screen,
      which is why passthrough is right there and wrong here.
- [x] **Clipboard** (OSC 52) — the `clip` builtin. `clip TEXT …` joins its
      arguments with a space as `puts` does; with no arguments it reads stdin, so
      `puts hi | clip` works. It copies exactly the bytes it was handed — a pipe's
      trailing newline included — because "copy what you were given" needs no
      exception list, and base64 is written out rather than taken as a dependency.
      The sequence goes to `/dev/tty`, not stdout: it is a message to the terminal,
      so `clip x > file` would otherwise put escapes in the file and nothing on the
      clipboard, and `clip` in a pipeline would corrupt the stream. Writing to the
      terminal also lets a *script* copy, which is the point of the builtin over a
      hand-emitted escape. Refused above 74,994 bytes of base64, the smallest of the
      common terminal limits, since past it a terminal drops the sequence without
      saying so. Whether the copy lands is the terminal's business — xterm wants
      `allowWindowOps`, tmux `set-clipboard on` — and there is no reply, so success
      means "asked", not "copied".
      Deferred: **reading** the clipboard back, which needs a query and a response
      and so can block on a terminal that never answers; and the `p` (primary)
      selection.
- [x] **Notifications** (OSC 9) — the `notify` builtin, plus an automatic one when
      a command takes longer than ten seconds: `mesh: cargo build — done in 1m15s`,
      or `— exit 2 in …` when it failed, since a failure that finished while you
      were away is the case worth a notification at all. `notify TEXT …` takes
      arguments or stdin and writes to `/dev/tty`, exactly as `clip` does; the
      automatic one writes to stdout, where the other automatic sequences go, since
      an interactive session's stdout *is* the terminal.
      The threshold stands in for the question mesh cannot answer — whether anyone
      is watching. Terminals report focus (`CSI ?1004 h`), but the line editor owns
      the input, so those events never reach the shell; a command long enough to
      walk away from is the usable proxy. Ten seconds is long enough that the news
      is news and short enough to catch a build.
      Gated by the same `OSC_TERMS` allowlist as the title (renamed from
      `TITLE_TERMS`, since it now decides two sequences): the question it answers is
      "will this terminal parse an `OSC` rather than print it", and a terminal that
      parses one it does not implement discards it. Which terminals actually *raise*
      notifications is unaskable — iTerm2, WezTerm, Ghostty, kitty and ConEmu do,
      xterm and Alacritty discard, tmux swallows without `allow-passthrough` — and
      there is no reply either way, so success means "asked".
      Off switch: `$sh.options.command-notify`, alongside the others — for anyone who
      would rather not have a notification daemon told what they are running.
      Inside **tmux** the sequence is wrapped in the `DCS tmux;` envelope with its
      `ESC`s doubled, since a multiplexer consumes an `OSC` it does not implement
      rather than forwarding it; tmux passes the envelope on when
      `allow-passthrough` is set and discards it otherwise, which is the same
      silence as sending nothing. `$TMUX` and `$STY` decide which multiplexer is in
      the way, because `$env.TERM` cannot: tmux is commonly configured to set
      `TERM=screen-256color`.
      Deferred: **screen's** passthrough, whose payload limit and quirks mesh has no
      way to test against here (and terminfo cannot supply it either — see the
      entry above) — and a wrong envelope *prints*, which is the failure
      the allowlist exists to avoid; **OSC 777**, whose `notify;title;body` split
      would double up on the terminals that support both; a **configurable
      threshold**, which is a value rather than a flag and so wants more of
      `$sh.options` than exists; **OSC 9;4 progress**; and skipping the notification
      when the terminal has focus, which needs focus events the line editor does not
      surface.
- [ ] **Cursor shape per mode** (DECSCUSR) — blocked on vi mode; the line editor
      is Emacs-only today.
- [ ] **Synchronized output** (DEC 2026) — belongs around reedline's repaint
      rather than in mesh.
- [ ] **Input arriving in the same burst as a submission is dropped, not queued.**
      Two command lines written to the terminal together run only the first; the
      rest of the burst is discarded rather than left for the next prompt.
      Measured boundary: with no gap between the two writes the second line never
      runs, and with a 10ms gap it always does, so this is about bytes already in
      the buffer when the `Enter` is processed — not about how long the first
      command takes. It predates the bracketed-paste work (the pre-`OSC 7`
      binary drops it identically) and reproduces with builtins alone, so no
      child and no terminal handoff are involved.
      Bracketed paste covers the common case, since a paste from a terminal that
      supports it arrives wrapped and becomes one buffer to edit; what is left
      exposed is a paste from a terminal without it and anything feeding mesh a
      scripted burst.
      Ruled out so far: mesh never flushes terminal input (the foreground
      handoff at `exec.rs:1598` restores modes with `TCSADRAIN`, which does not
      discard), and crossterm's filtered read — the `ESC[6n` cursor-position
      wait, the obvious suspect for eating queued keys — puts every skipped
      event back. What is left is reedline's batch loop, which stops collecting
      at the `Enter` (`engine.rs:943`) and hands the batch off; where the bytes
      after it go needs instrumenting to say, and the fix may belong upstream.
      What it is *not*: input typed while a command runs survives and runs at the
      next prompt, which is the case that would matter most — as long as that
      command does not read the terminal itself. One that does (`cat`, a REPL)
      consumes what is typed at it, which is the foreground process group owning
      the terminal working correctly and is not this bug; a regression test for
      this has to use a command that leaves stdin alone.

## Beyond M3 — The prompt

The layout half is designed (`DESIGN.md` §"Hooks and the prompt") and unbuilt.
What is *not* designed is everything that makes a prompt worth arranging — where
its facts come from and what they cost. The reasoning is written up in
[`docs/PROMPT.md`](docs/PROMPT.md) §"What it takes to not need starship"; this is
the checkable list.

- [ ] **The `$sh.prompt` segment map**, with `rule`, `newline`, and the inline
      `fill`. Designed in full; nothing of it is built, so a prompt is one string
      today. Everything below assumes it.
  - [ ] **`prompt TEXT` writes one entry, not the map.** Decide it now rather
        than at implementation time, because the wrong answer is a footgun: if
        `prompt` replaced the map, the *beginner* command would silently destroy
        the *advanced* config. It should be sugar for `$sh.prompt.char = TEXT`,
        `prompt --reset` for `unset $sh.prompt.char`, and bare `prompt` prints
        what `prompt TEXT` sets — the char — leaving `prompt --show` to render
        the whole map. This is what the builtin already does rather than a new
        rule: it feeds `render_prompt_indicator` while every line above comes
        from `preprompt` hooks, which `docs/REFERENCE.md` already spells out as
        "controls only the input indicator". Writing one key also keeps one
        meaning per key — `$sh.prompt.char = func() { … }` and `prompt "$ "` are
        the same slot, last write wins, with no second mechanism to reason about.
        The continuation prompt derives from `char` as it derives from the custom
        text today.
  - [ ] **Evaluate and snapshot the map before `read_line`** — an explicit
        requirement, not something the map gives you. reedline calls a `Prompt`'s
        render methods on **every repaint**: each edit, each menu movement, a
        resize, the submission itself. Evaluating segments inside those methods —
        the tempting design, since rendering is what they are for — would re-run
        `git` and `ssh-add` on every keystroke, which is worse than the
        double-running it was meant to fix. Today's code already has the right
        shape: `MeshPrompt` carries a `custom: Option<String>` cloned from
        `shell.prompt.text` before `editor.read_line` (`repl.rs:8655`), and
        `render_prompt_indicator` only borrows it. The async repaint needs the
        same boundary from the other side — a segment landing means *replacing
        the snapshot* and redrawing, which is only coherent because one exists.
- [ ] **A fact map, starting with `$sh.vcs`.** The prompt design claims "you read
      real values, not scraped text", and that is true of `$sh.status` and
      `$sh.jobs` and false of everything else: `docs/PROMPT.md`'s own example
      forks `git` twice per prompt and then parses the text. `branch`, `dirty`,
      `ahead`, `behind`, `stash`, `state` as a map is what ends that, and it is
      the piece segments get written against — so the **shape** matters more than
      the source, which can change underneath it.
  - [ ] Decide the source on measurement, not taste. `git` (2–4 forks, no new
        dependency), a helper binary (one fork, and this author's config already
        uses `vcs prompt-info`, plus hg/jj through one interface), or in-process
        `gix`/`git2` (no fork, fastest, but dozens of crates, tens of seconds of
        clean build, megabytes of binary, and mesh then owns worktrees,
        submodules, and sparse checkouts). Leaning: helper or `git` behind the
        cached map, native only if measurement demands it.
  - [ ] **Cache it per directory**, invalidated by `postcd` (which now exists)
        and after a command that could have touched the repository. A prompt in a
        directory you have not left should do no work. Those two are **not
        sufficient**: `git fetch &` returns to the prompt before it updates a ref,
        and a rebase in another terminal never touches this shell at all, so the
        cache would sit on stale `ahead`/`behind` indefinitely. Two more
        triggers, and the first is **not** free today:
    - [ ] **`jobdone`**, once it fires at completion. The hook exists but runs
          from the `reap` at the top of the REPL loop, before `read_line`, so a
          fetch finishing while you sit at the prompt is not noticed until the
          next line is submitted — the same timing the `[N] Done` notice has, as
          "Notify about a finished job when it finishes" above records. Fixing
          that means waking the editor on a child's state change, which is the
          **same wake** the async repaint needs: one mechanism, three features.
    - [ ] **Metadata**, but only for the fields it can answer for — **the map
          does not share one invalidation story**, and splitting it by class is
          the design decision here:
      - [ ] **Ref-derived** (`branch`, `ahead`, `behind`, `state`, `stash`)
            change only when something under `.git/` does, so stat'ing
            `.git/HEAD`, the index, and the refs is exact and cheap. This is what
            covers a rebase in *another* terminal, which fires nothing in this
            shell at all.
      - [ ] **Worktree-derived** (`dirty`) has **no cheap trigger**. An editor
            writing a tracked file changes that file's mtime and nothing under
            `.git/`; creating an untracked file changes a directory's mtime and
            nothing under `.git/`. So metadata says "still valid" while `dirty` is
            wrong. Watching the worktree means a recursive filesystem watcher over
            a tree of unknown size — inotify watch limits, network filesystems,
            and a whole class of failure a prompt should not own — so the
            realistic answer is an **unconditional TTL** for this field, with an
            explicit refresh for anyone who wants certainty.
      - [ ] Note the two properties point the same way: `dirty` is also the
            **most expensive** fact (a full worktree scan, where the others are
            a couple of ref reads). The field that cannot be invalidated cheaply
            is the field that most wants to arrive late — which makes it the
            first candidate for an async segment rather than an argument against
            caching the rest.
  - [ ] **Every fact source needs a timeout**, the bet the completion probe
        already makes: a `git` call on a dead mount should cost a missing segment,
        not a hung terminal.
- [ ] **Async segments, via a prompt repaint.** Draw immediately from what is
      in-process (status, jobs, cwd — free) and repaint the slow segments when
      they land. This is the one thing an external prompt structurally cannot do:
      starship is handed a moment, prints, and exits; mesh owns the editor and can
      revise. It is also the honest answer to "starship's information without
      starship's latency".
  - [ ] **Not `external_printer`.** That prints a *line above* the prompt, which
        is right for the background-job notice and wrong here: in the locked
        0.49.0 the repaint is gated on having a message to print
        (`if !messages.is_empty()` → `print_external_message` → `repaint`), and an
        empty message is discarded without repainting (`"".lines()` yields no
        items). Every resolved segment would either leave a stray scrollback line
        or not repaint. A prompt needs a **silent wake-and-redraw**.
  - [ ] **The shape already exists upstream, ungeneralized.** reedline's
        `repaint` is private, but `engine.rs` does exactly this for background
        completions — `if completer_pending && self.completer.check_pending() { …
        self.repaint(prompt) }` — an async producer finishing, noticed on the
        poll, redrawn with nothing printed. The ask, upstream or in a fork, is
        narrow: let something other than the completer say "I have new material,
        redraw."
  - [ ] **The polling cost recorded under "Beyond M3 — Terminal integration"
        stands** — reedline's two async paths are not treated alike, and only one
        of them is scoped. `needs_polling` is recomputed each iteration, and the
        completer's contribution is conditional (`result |= completer_pending`),
        so that path polls only while a completion is outstanding. The printer's
        is unconditional — `if self.external_printer.is_some() { result = true }`
        — so a printer attached for the life of the editor polls for the life of
        the editor, exactly as that entry warns. What the completer shows is that
        reedline is willing to scope polling where the producer says so; the
        attach/detach shape remains **mesh's to implement**, with the completer as
        the precedent rather than an existing implementation.
  - [ ] Two rules that are not obvious: a repaint must never move the cursor or
        eat a keystroke (it rewrites the prompt region, not the buffer), and a
        segment that resolves *after* the command was submitted is discarded — it
        is answering about a prompt that is now scrollback.
- [ ] **A default prompt map that is the dashboard.** starship's headline is that
      it looks good with an empty config; mesh's default is `mesh$ `, so every
      good thing in the design is available only to someone who already sat down
      and wrote a config. The default should be the *same map a user writes*,
      pre-populated and printable (`prompt --show`), so "replace one segment" is
      the first thing learned rather than "throw it away and start over".
- [ ] **The transient prompt** — the collapse-to-one-line rewrite of the previous
      prompt, named in the carried-over requirements. **Independent of the async
      repaint work**, and the cheapest item here: reedline already implements it.
      `Reedline::with_transient_prompt` takes a second `Prompt` and `submit_buffer`
      repaints with it the moment a line is accepted (`engine.rs:605`, `:2340`),
      so there is no async producer and nothing to wake for — submission is
      already an event the editor handles. What it needs from mesh is a second
      prompt to hand over, which the segment map makes natural: the transient form
      is a map too, usually a shorter one. Sequence it any time after the map.
- [ ] **Display width in the segment renderer** — reusing what is already there,
      not adding it. `unicode-width` is already a dependency
      (`crates/mesh-core/Cargo.toml:24`) and already measures the prompt:
      `escape_stripped_width` (`repl.rs:9710`) strips escapes and runs the rest
      through the width table, with a CJK regression test asserting `日本> ` is
      six columns (`repl.rs:11861`). What is open is the segment-level use —
      `fill` splitting slack across pieces, and a `rule` that reaches the margin,
      both need a per-piece width rather than one for the finished line — and the
      policy question the table cannot answer: a nerd-font glyph or an emoji ZWJ
      sequence whose width the *terminal* disagrees about, where being right by
      the standard still leaves the cursor in the wrong column.

## Beyond M3 — Navigation

- [x] **`CDPATH` search in `cd`** — *landed*. A plain relative operand is looked
      for in each `$env.CDPATH` entry in order, first hit wins; a miss falls back
      to the current directory, so setting it never breaks a plain `cd subdir`.
      An empty entry is the current directory, and a hit through a **non-empty**
      entry prints where it landed (POSIX, and the same rule `cd -` already
      followed). `.`, `..`, `./x`, `../x`, and an absolute path never search — the
      POSIX dot exemption — and neither does an empty operand, since `entry/""` is
      the entry itself and would turn `cd ''` into a jump.

      This closed a real inconsistency rather than adding a feature: `CDPATH` was
      already one of mesh's **path-type** names (`environ.rs:21`), so it split on
      `:`, took `+=`, round-tripped exactly, and was exported — everything except
      being *read*. Setting it configured every shell except this one.
- [ ] **Should `CDPATH` be exported at all?** Now that `cd` reads it, where it
      lives is a real question. In bash it is a **shell** variable, not an
      environment one, and deliberately: exported, it changes what `cd src` means
      inside every script the shell starts, which is the classic footgun — a
      script that means `./src` can land in a `CDPATH` entry. mesh has no
      unexported-but-special namespace today: `$env.X` *is* the environment
      (that is the point of the namespace split, `docs/REFERENCE.md`
      §"The environment"), and an ordinary binding is invisible to `cd`. The
      shapes:
  - [ ] **Keep `$env.CDPATH`** — one place, inherits from a parent shell, passes
        to children, interoperates with bash/zsh either way. Keeps the footgun.
  - [ ] **Move it to `$sh`** (`$sh.cdpath`, or a settings entry) — not exported,
        so a script's `cd` is never redirected by the parent's convenience. Costs
        the inheritance, and forces `$sh.options` open to non-boolean values,
        which is the nested/typed-settings TODO under "Beyond M3 — The
        environment".
  - [ ] **Read `$sh` first, `$env` as the fallback** — inherit when wanted,
        override locally. Two sources for one answer, which is the shape mesh
        avoids elsewhere.

## Beyond M3 — External tool integration

The hooks and surfaces a bash/zsh user's toolbox plugs into — starship, atuin,
fzf, carapace, zoxide, direnv, mise. The full write-up, tool by tool, is
[`docs/INTEGRATION.md`](docs/INTEGRATION.md); this is the checkable list it
produced. Several entries overlap with items elsewhere in this file
(`$sh.complete`, `precd`/`postcd`, the keybinding surface); they are named again
here because the integration case is what makes them urgent rather than merely
designed, and the cross-references say where the fuller note lives.

- [x] **`precd` / `postcd` hooks** — *landed*. `on precd NAME FUNC`
      runs before the move, still in the old directory, given the target;
      `postcd` runs after, in the new one, given the previous directory. They
      were the highest-value missing hook of the set — zoxide's directory
      recording, direnv, mise, autoenv, and a background fetch on arrival all
      want them — and the only one with **no workaround**, since a function
      cannot shadow a builtin (`func cd` is refused as a reserved name,
      `whence.rs:348`) so the zsh trick of wrapping `cd` is unavailable by
      design.

      Three contracts, each from `DESIGN.md` §"Hooks and the prompt", each
      covered by a test in `crates/mesh/tests/cli.rs`:
  - **Per actual move**, a `cd` inside a function included. Deferring to
        function return would run `precd` somewhere other than the directory it
        promises to run in, which is the whole reason for the `pre`/`post` split.
  - **The target is resolved before `precd`**, so a handler that `cd`s away
        itself cannot make a *relative* outer `cd` land somewhere unintended.
        Resolution is `canonicalize`, which also means a destination that does
        not exist is reported before any hook runs for a move that was never
        going to happen; `$env.OLDPWD` is captured on the same side of the hooks,
        so `cd -` is unaffected by a wandering handler.
  - **A handler's own `cd` does not re-dispatch** (`Shell::in_cd_hooks`),
        or `$sh.postcd.track = func(from) { cd $from }` would recurse until the
        stack ran out. A failed move owes no `postcd`.

      Mechanism: `cd` moved out of `builtins::dispatch` into the REPL's
      shell-aware match beside `source` / `gets` / `type`, since the hooks are
      the shell's; `builtins::cd` split into `cd_target` (resolve, no move) and
      `cd_change` (move) so the hooks can run between them.
- [x] **`exit` hook on every exit path** — *landed*. `on exit NAME FUNC` had
      been dispatched from the interactive loop's `Step::Exit` arm alone, so a
      script, a `-c` string, piped stdin, and an `exit` from a startup file all
      left without running it — the cleanup case the hook exists for was the
      case it did not cover. Dispatch moved into `run_logout`, the one function
      every exit path already arrives at (it is where the `jobdone` drain and
      the title reset live for the same reason). Ordering is unchanged: the
      drain still precedes the handler, and the handler still precedes
      `logout.mesh`.

      The status handed to the handler is **bash's `$?` in a `trap … EXIT`** —
      the argument to `exit N`, the last command's status for a bare `exit` or
      an end of input. Verified against bash rather than assumed.

      Two follow-ups, both deliberately out of that change:
  - [ ] **Exiting because of a signal.** bash runs its EXIT trap for the
        catchable fatal signals — SIGTERM, SIGHUP, SIGINT — and then re-raises
        so the parent still sees `128 + N`; only SIGKILL escapes it. mesh runs
        nothing: only the terminal-owning `run_interactive` loop ignores
        INT/QUIT/TSTP/TTOU/TERM (`repl.rs:ignore_interactive_signals`), while a
        script, a `-c` string, piped stdin, and even a flag-forced `mesh -i`
        over any of those keep their default dispositions; HUP is handled
        nowhere.
        Wants a handler that records the signal and lets the main loop leave
        through `run_logout`, then re-raises — the flag-and-check shape
        `exec.rs`'s `SigintCatcher` already uses for a wait it must interrupt.
        A blocking `waitpid` on a foreground child is the case to get right:
        bash does not wait for the child, it goes promptly.
  - [ ] **Whether that handler should be told it was a signal.** bash says
        no — `$?` inside the EXIT trap of a script killed by SIGTERM is `0`,
        not `143`, so a bash handler cannot tell "finished cleanly" from "was
        killed", and the `128 + N` exists only in what the parent waits for.
        mesh copies bash for now. Worth revisiting: passing `128 + N` would let
        a handler distinguish the two and would match the number the caller
        goes on to see, at the cost of giving that encoding a second meaning
        (today it says *a child* died on a signal, not that this shell did).
- [ ] **Keybindings from `rc.mesh`.** Deferred in `DESIGN.md` (§"Line editing"),
      and the reason reedline was chosen. Nothing binds Ctrl-R to atuin, Ctrl-T
      to fzf, or anything to anything.
- [ ] **A line-buffer API, and the widget concept it implies.** Required
      *together with* the binding above: a binding that cannot touch the line
      being edited is useless to every tool that wants one. fzf's Ctrl-T
      *inserts at the cursor* and runs nothing; atuin's Ctrl-R *replaces the
      buffer* and leaves it editable. Needs deciding: how a mesh function runs
      **during** line editing rather than as a command, and what it may do —
      read the buffer and cursor, replace them, insert at point, accept the
      line, and force a redraw after a full-screen program has scribbled on the
      screen. Related: the `$sh.keymap` gap for a vi-mode indicator
      (§"Beyond M3 — Terminal integration") is the same missing "the editor's
      live state is not a value" hole seen from the other side.
- [ ] **`$sh.complete`, with the parts a *bridge* needs.** The map itself is
      already tracked under "Beyond M3 — Interactive completion"; carapace adds
      three requirements the design does not state:
  - [ ] a **fallback key** (`*`, or a documented default entry) — a bridge
        answers for every command, not one named one;
  - [ ] a defined **callable contract**: the words so far, the cursor's word
        index, the partial word, and the cwd (bash's `COMP_WORDS` / `COMP_CWORD`
        in mesh shapes), plus where a dynamic provider sits in the four-layer
        resolver — the argument for carapace is *between* the curated file and
        the man page, since it is curated data but must never outrank the user's
        own file;
  - [ ] **descriptions alongside candidates**, since a description is most of
        what carapace is for and the menu shows bare candidates today. A dynamic
        provider must also be exempt from the mtime-keyed spec cache, which has
        no key for a per-word answer.
- [ ] **Reading structured output.** carapace exports JSON, `direnv export json`
      is a JSON env diff, `mise env --json` prints JSON too (though see the apply
      entry below — it looks like a *target state* rather than a diff), and atuin's
      search output likewise. mesh cannot parse any of it. Decide between a JSON
      reader and a mesh-defined line format that upstreams are asked to emit; the
      former needs no upstream cooperation, which is most of its case. Doing each
      bridge in Rust instead sidesteps the parser at the cost of building per-tool
      knowledge into the shell.
      **Cost: none in dependency terms.** `serde_json` is already in the tree —
      reedline depends on it, so it is compiled into every build today
      (`cargo tree -i serde_json`). No new crate, no added build time, no
      measurable binary growth. What is left to decide is the *mapping*: how a
      JSON null, a nested array, and a non-integer number become mesh values,
      which is the same question a `:json` modifier or a `from-json` builtin
      would have to answer anyway.

      **One of those three is no longer forced by the environment.** The env-diff
      apply below owns its own `null` handling inside the builtin, so a general
      reader does not have to invent a mesh value for one just to unblock direnv.
      (It does **not** finish mise, which additionally needs a source that reports
      removals — see the apply entry below.) What remains is what carapace and
      atuin need, and for those refusing a null is a defensible answer. A
      **non-integer number** is the harder one and is nobody else's to absorb:
      `Value` is `Integer(i64)` with no float variant, and both of those tools
      emit floats.
- [ ] **A bulk env-diff apply — `if json = "$(direnv export json)" { env-apply
      --json $json }`.** The
      **writes** this entry was mostly about have landed: `$env[$name] = value`
      writes under a computed key and `unset $env[$name]` removes an entry, so a
      loop over a computed diff applies it today (§"The environment" in
      `docs/REFERENCE.md`, and rough edge 5). What is left is the **transaction**:
      one builtin that parses the payload, splits it, validates the whole thing,
      and only then touches the environment, so a payload malformed half way
      through is a diagnostic rather than a half-applied environment.

      **Shape decided** (see §"direnv, mise, nvm" in `docs/INTEGRATION.md` for the
      reasoning). A removal is spelled `unset $env[$name]` — the verb the language
      already has — and there is **no sentinel value** meaning "remove": mesh's
      no-null rule is load-bearing, and inventing a `none` to carry one tool's wire
      convention would pay a language-wide cost for an integration. The split
      between writes and removals therefore lives **inside the builtin**, before
      any mesh value exists, since a map that cannot hold a null cannot carry the
      distinction into mesh code.

      The alternative — a reader handing both halves back for mesh to apply — was
      rejected on two counts: it needs a value-function call, because
      `[sets removes] = $(cmd)` does not bind — a capture is **one string**, not a
      pair to destructure — and decisively **it is not a transaction**, since a
      second loop failing partway leaves exactly the half-applied environment this
      is meant to prevent. The payload `env-apply` receives is therefore a string,
      which is what it parses.

      **The payload arrives as an argument, not through a pipe.** A `|` runs every
      stage in its own process, a builtin included, so
      `direnv export json | env-apply --json` would write the environment in a
      forked child and lose all of it on exit — verified, not reasoned about: a
      function writing `$env` on the right of a `|` leaves the parent unchanged.
      `DESIGN.md` marks "the last stage of a pipeline runs in the current shell" as
      *planned*, and the argument form owes nothing to it.

      **And the payload is bound before it is applied**, not interpolated straight
      in, and it is **quoted**: a capture is one string today, but `DESIGN.md` has
      `$(cmd)` becoming a newline-split list with `"$(cmd)"` as the one-string
      form, so the quotes keep this spelling correct across that change. Only an
      assignment keeps a capture's status (§"Pipelines and sequencing" in
      `docs/REFERENCE.md`), so `env-apply --json "$(direnv export json)"` would
      discard direnv's exit code and apply the output of a *failed* hook that still
      printed parseable JSON — the one outcome a transaction exists to prevent.
      Both points raised in review on mikelward/mesh#341.

      **mise may not fit the same bridge.** `direnv export json` is a diff whose
      `null` means "unset this"; `mise env --json` is documented as exporting the
      vars that activate mise once, which is a **target state**, and a target state
      cannot express a removal by omission. If that holds, mise needs a stateful
      source of its own. Unconfirmed here — neither tool is installed in this
      checkout — so confirm against a real `mise` before building that half. Also
      from the #341 review.

      What this buys the entry above: `from-json` no longer has to answer the null
      question on the environment's behalf.
- [ ] **Decide the stance on generated code.** Every tool ships
      `eval "$(tool init zsh)"`. mesh has no `eval`, and `source` takes exactly
      one file operand — no pipe, no string, no `-` — so the published install
      line cannot work. `DESIGN.md` sketches `atuin init mesh | source`
      (§"Conditionals"), which would need `source -` or a `run TEXT` builtin.
      The three options and the argument for the third (exchange **data**, not
      code — which needs no `eval` and no upstream change for direnv and
      carapace, nor for mise beyond a removal-reporting source) are written up in
      `docs/INTEGRATION.md`. This decision gates the
      one below.
- [ ] **Publish an integration contract**, so an upstream can add a `mesh`
      target to `atuin init` / `starship init` / `zoxide init` / `direnv hook` /
      `carapace`. Deliberately *after* the hooks and the decision above: a
      `tool init mesh` emitting registrations mesh cannot parse is worse than no
      target at all, since it looks supported and is not.
- [x] **A name cannot start with `_`** (`parser.rs:1440`, `:4651` require an
      alphabetic first character), so the private-global convention every shell
      config uses is a syntax error in exactly the variables these hooks ask
      users to create. Already tracked — see "Reserve only bare `_` as discard,
      allow `_name`" under "Icebox / decide later", where the integration case
      and the two follow-ons (the diagnostic, and the design-doc examples that do
      not parse) are recorded.

      **Done**, along with both follow-ons — `valid_name` takes a `_` head as
      long as something follows it. `docs/INTEGRATION.md` said this was a wrinkle
      every integration hits and stashed its starship timing in `cmd-elapsed` to
      dodge it; it now uses `_cmd_elapsed`, the name the convention asks for.
- [ ] **Hint and highlighter hooks.** Not external tools, but the
      zsh-autosuggestions / syntax-highlighting experience users arrive
      expecting. reedline supports both and mesh exposes neither.
- [ ] **The history question atuin forces.** mesh's SQLite store already carries
      most of atuin's schema, so "integrate atuin" splits in two: *atuin's UI
      over mesh's store* (needs `$sh.history` or a documented on-disk contract)
      versus *atuin as the store* (needs the recall motions — Up, Ctrl-R, `!$` —
      to read a pluggable backend, a much deeper change nobody has asked for).
      Decide before either is built: the answer determines whether
      `--no-save-history` is the integration point or a workaround. Adjacent and
      already deferred: importing bash/zsh/atuin history, and secret redaction.
- [ ] **`$sh.options.complete.probe`** — a session with carapace probably wants
      mesh's own `--help` probe off. Blocked on nested keys in the flat settings
      map, tracked under "Beyond M3 — The environment".
- [ ] **The prompt-segment items starship exercises**, all tracked under the
      prompt design and listed here for the integration case: the `$sh.prompt`
      map (so an external renderer is *one* segment rather than the whole line),
      multi-line raw external output, `fill` for a right prompt, and a redraw
      hook for a transient prompt. What works today is
      `prompt "$(starship prompt …)"` from a `preprompt` hook, with `$sh.status`,
      `$sh.jobs:len`, and `postexec`'s `elapsed` supplying its arguments.

## Beyond M3 — Modifier arguments and `gets` ✅ (landed)

- [x] `:get(KEY, DEFAULT)` — the total accessor, on maps and lists, plus a bare
      `$env` resolving to the whole table as a map so `$env:get(EDITOR, vim)`
      needs no rule of its own. This is the mesh spelling of `${VAR:-default}`,
      by a wide margin the most-used thing a real shell rc reaches for.
- [x] The affix family: `:stripstart(P)` / `:stripend(S)` drop an affix once,
      `:trimstart` / `:trimend` peel whitespace (or a given char set) repeatedly.
- [x] The replace family: `:replaceall(OLD, NEW)` and the anchored
      `:replacestart` / `:replaceend`. `${x//a/b}` and its anchored kin, with a
      pattern that is a **match slot**: a string matches verbatim, a `/…/` is a
      regex. See the regex entry below for what that slot has to get right.
- [x] Argument-taking modifiers in **command-argument** position
      (`puts $env:get(EDITOR, vim)`), which used to be a syntax error: a command
      word stops in front of the `(`, so the arguments arrived glued to it.
- [x] `gets [var]`, reading descriptor 0 a byte at a time so it cannot swallow
      input belonging to the next command.

- [ ] **The spread of an argument-taking modifier at a command boundary.**
      `puts ...$x:split(":")` is still a syntax error. `CommandItem::Value` has
      no spread variant — `UnaryOp::Spread` is produced by the parser and
      consumed by nothing — so routing the run through the expression parser
      would pass one list where the reader asked for its elements. Deliberately
      left loud rather than made silently wrong; bind it first for now.
- [ ] **Should `gets` return the line, so `while line = gets()` works?**
      *(mikelward)* The command form built here binds a variable and reports a
      status, which is what `while gets line { … }` needs and nothing more.
      `DESIGN.md` §"Builtins" also wants `gets` to **return** the line as its
      value, which is the part that composes:

      ```mesh
      while line = gets() { puts $line }     # the spelling to consider
      [k v] = gets():split("=")              # read and destructure in one
      if line = gets() { … }
      ```

      The pieces are mostly already decided elsewhere, which is what makes this
      worth doing rather than reopening:
      - The **assignment-as-condition** rule is settled (`DESIGN.md` §"Tests and
        comparisons"): `lhs = rhs` is true iff the RHS is truthy and its shape
        fits, so `while line = gets()` needs no new grammar — it is the same rule
        `if [one two] = $s:match(/…/)` already uses.
      - The **EOF value** is pinned: `gets()` yields `false`, *not* `""`, so a
        blank line (truthy `""`) cannot end a loop. That is already how the
        command form's status behaves, so the two cannot disagree.
      - What is missing is the **value-call route** — the one `style()` and
        `link()` take, where parens attached to a name yield a value rather than a
        status. `gets` would be the first builtin to have *both* spellings.

      Open sub-questions to settle first:
      - Does bare `gets` (no parens, no variable) stay the "consume and discard a
        line" form, or does the value form make it redundant?
      - `gets line` and `line = gets()` would both bind `line`. Two spellings for
        one act is the thing `DESIGN.md` usually declines — is the command form
        still worth keeping once the value form exists, or does it retire?
      - A value-returning `gets` in a **pipeline stage** runs in a forked process,
        so the binding does not outlive it. Same as any builtin, but worth a line
        in the reference, since `cmd | while gets line { … }` is the shape people
        will reach for (`DESIGN.md`:3462 already flags that form as *planned*).
- [x] **A regex pattern for the replace family** — the `/…/` slot `DESIGN.md`
      §"String" specifies. Split out of the modifier-arguments change because it
      is where the difficulty lives; each item below was a real bug caught in
      review before it was got right:
  - [x] **Anchoring, not filtering.** `find_iter` reports non-overlapping
        leftmost-first matches, so an earlier match eats the bytes a later
        trailing one needed and `re("ab|bc")` against `abc` never offers the `bc`
        that ends the string.
  - [x] **`\A` / `\z`, not `^` / `$`.** The latter move to *line* edges under
        `:m`, and a subject's edge is not a line's.
  - [x] **The subject stays whole.** Testing truncated slices to find a longer
        match fabricates context for a look-around: `re(r"a\b")` has no match in
        `ab` but passes against the slice `a`, whose cut end reads as a word
        boundary.
  - [x] **Extended mode cannot be read off the flags.** `(?x)` turns it on from
        inside the pattern, and a trailing `#` comment then swallows any
        generated `)` and anchor. Retrying the wrap with a closing newline works
        and cannot mask a broken pattern, since a swallowed `)` always leaves the
        group unclosed.
  - [x] **A flagged literal is a chain, not a bare regex.** `/a/:i` parses as `:i`
        applied to an `Expr::Regex`, so a slot conversion that inspects only the
        top of the tree drops it and leaves `:i` on a string.
  - [x] **The match contract is the engine's** *(decided — mikelward)*. Anchor,
        and document what anchoring gives rather than promising more. At the
        *trailing* edge that is already the longest match, free: every candidate
        finishes at `\z`, so the leftmost start wins. At the *leading* edge every
        candidate starts at 0, so `regex`'s leftmost-**first** rule picks the
        alternative written first (`a|ab` takes `a`) — write the one you want
        first, as in any regex. True leftmost-longest at both edges is
        deliberately **not** promised: it needs `regex-automata` for
        anchored-at-offset search on an intact haystack, and every attempt to
        fake it on top of `regex::Regex` produced a worse bug than the asymmetry
        it removed.
  - [ ] **Capture backreferences in a replacement.** `NEW` is literal text —
        `regex`'s own `$1` expansion is suppressed — because `DESIGN.md` still
        calls the spelling (`${1}` vs `$1`) provisional.
- [ ] **`:has(VALUE)`.** The parser knows the name; the engine does not.

- [x] **`postfix` consumes a *spaced* call suffix, so a following group is
      stolen** *(landed — narrowed to `Expr::Modifier`, so `f (1)` still calls
      `f` and the language decision this was blocked on was not needed)*. `y = $x:upper (1)` reports "modifier :upper does not take
      arguments" and `puts $x:split (":") (1)` reports "value is not callable",
      because `postfix` eats a `(` whether or not it abuts. Pre-existing — both
      reproduce on `origin/main` — but argument-taking modifiers in command
      position give it one more spelling to show up in
      (`puts $x:split(":") (1)`). The documented rule is that spacing decides an
      attached call from an argument (`docs/REFERENCE.md` §Commands), so the fix
      is for `postfix` to require adjacency the way `value_argument_starts` now
      does. Not done here because it changes call syntax in *expression* position
      too — `f (1)` currently calls `f` and would stop — which is a language
      decision rather than a bug fix.

- [x] **A bare `/…/` literal cannot hold a space or an unbalanced paren.** Fixed
      by having the three **slots** read the literal themselves, which is what
      makes the ambiguity decidable: a leading `/` is far more often a path than a
      pattern and the lexer cannot know, but the right-hand side of `~`, a `match`
      arm, and a replace's pattern are three places where the shape is a pattern
      or nothing. `Parser::regex_literal` scans from the opening `/` to its closer
      and takes the text from the source, so `[`, `(`, `{`, `|`, `,`, `:` and a
      space all sit inside one. Command position never reaches it, so
      `ls /usr/bin` is untouched, and so is `cat a(b`.

      `/usr/bin` stays a glob because **the closer has to end the word**: the
      slash before `bin` has a word character after it, so it closes nothing and
      the existing reading answers as before. A pattern that wants an interior
      slash still writes `\/`.

      Three cases are left. A slash inside a character class (`/a[/]b/`) closes
      the literal, since the scan is not bracket-aware — `/a[\/]b/` works and is
      a valid class either way.

      The other four are the **lexer's**, and together they are why the scan
      being a parser-side reading is a trade rather than a free win: it runs on
      the source *after* tokenization, so anything the lexer resolves first is
      inside the literal in the source and already decided in the tokens.

      - A ` #` comment. Reported now as `a /…/ literal cannot contain a
        comment`, where it used to decline and leave the leading `/a` to read as
        a glob and answer false.
      - An unmatched `'` or `"`, rejected as an unclosed quote — so the class
        `/['"]/`, which is not an exotic thing to want, has to be written
        `re("[\"']")`.
      - `<<`, rejected as an unterminated heredoc before the parser sees
        anything, so the message names a delimiter nobody wrote.
      - An **unbalanced** `}` or `)` inside a `${…}` or `$(…)` body, whose
        closing delimiter the lexer finds first: `"${ "a}b" ~ /a}b/ }"` ends the
        body at the pattern's `}` and renders `falseb/ }`, while the same test
        at top level is `true`. Silent, and the inconsistency is new — the top
        level only started accepting `}` with this change. A *balanced* pair is
        fine in both places, which covers what patterns actually use
        (`/a{1,2}/`, `/a(b)c/`).

      Closing all four means teaching the **lexer** the slots — it knows the
      token before the one it is about to scan, so `~` and `!~` are reachable
      there, but a `match` arm and a replace's argument are not without more
      context. Doing it for one slot and not the others would be worse than the
      limit. Four instances now argue for doing it properly; all four were
      raised by review on mikelward/mesh#318.

- [ ] **How `puts` should render a nested structure.** *(mikelward)* A collection
      inside a collection has no rendering today, so `puts $m` on
      `[a: [1 2]]` is a loud error, and so is `puts $env` — the path-type names
      are lists, which is what keeps `$env.PATH` and `$env:get(PATH, …)` the same
      value. The error is honest (better than a guessed flattening), but it means
      the one obvious way to *look at* a nested value is unavailable, and `$env`
      made that reachable by accident rather than by choice.

      `puts` renders for **reading** — a scalar as itself, a list one element per
      line, a map as `key: value` lines — so the question is what the nested case
      reads as. Some shapes to weigh:
  - [ ] An **indented** rendering, one level per depth, which is what a reader
        drawing it by hand would do. Needs a rule for how deep before it stops
        being readable, and whether a list of scalars stays on one line.
  - [ ] Defer to **`:repr`**, which already writes any nested value down and is
        defined round-trip. That answers "show me what I have" without inventing
        a second format — but `:repr` quotes for round-tripping, which is exactly
        what `puts` exists not to do.
  - [ ] Render only the **outer** level, with a nested element shown by its shape
        (`[3 elements]`, `[2 entries]`) rather than its contents. Keeps output one
        line per entry, at the cost of not showing the value.

      Whatever it becomes, `$env` should print under it rather than needing a
      rule of its own — a listing whose entries are typed the way every other read
      types them is the point of it being an ordinary map.

- [x] **A modifier chain in `"…"` is dropped when punctuation abuts it**
      *(landed — the name now stops at the first character that cannot be in one;
      the command-word half was already fixed by the binding work, so
      `puts $x:split` reports the missing separator)*.
      `puts "$x:upper"` renders `AB`, but `puts "[$x:upper]"` renders
      `[ab:upper]` — the closing bracket is scanned into the modifier name, the
      name then matches nothing, and the whole chain silently reverts to literal
      text. The **command-word** path drops a modifier the same silent
      way when it is one that needs an argument: `puts $x:split` prints the
      subject unchanged rather than saying `:split` requires a separator, where
      the same chain in `x = $y:split` reports it. Pre-existing, and not specific to the new modifiers. It is the bad
      kind of failure: no error, just the wrong string. The scanner should stop a
      modifier name at the first character that cannot be in one, and only then
      decide whether the name it read is a modifier.

## Beyond M3 — The `glob` family ✅ (landed)

- [x] **`glob(PATTERN)`, `files(DIR=.)` and `dirs(DIR=.)`.** The expansion side of
      `DESIGN.md` §"Globbing", as **value calls** answering with a path list — so
      `for d in dirs() { … }` works, where before `dirs` fell through to
      "a command has no return value". The value-return machinery was already
      there (a `func` called for its value has iterated since M3); these three
      names simply had no implementation behind them. `expand::glob_paths` is the
      word path's own matcher with the pattern arriving as a string, so hidden
      entries, sort order, and no-match-is-empty are inherited rather than
      restated; the wrappers are `entries_pattern` (`DIR/*`, `.` unprefixed, the
      directory escaped because it is a path) plus the `:files` / `:dirs` filter.
      The names join `re` / `style` / `link` as reserved, and `parser::value_builtin`
      now backs the `:capture` routing that only `re` had been spelled into —
      `style(x):capture` reported a command-not-found before.
- [ ] **A spread value call at a command boundary.** `ls ...glob($p)` — the
      `DESIGN.md` spelling — is still a syntax error, alongside the
      `puts ...$x:split(":")` it shares a cause with: `CommandItem::Value` has no
      spread variant, so the expression path would build a `UnaryOp::Spread`
      nothing consumes. Workaround is a binding (`found = glob($p)`, `ls ...$found`),
      which is why this is ergonomics rather than a hole.
- [ ] **A `.`-leading path in value-argument position.** `dirs(.)` and
      `files(./src)` are syntax errors — `.` is the member operator and `..` the
      range one — so an explicit current or parent directory is quoted
      (`dirs(".")`, `files("../src")`). `dirs()` covers the common case, and `..`
      cannot be recovered without colliding with ranges, so the quote may simply be
      the answer.
- [ ] **A pattern whose first component starts with `.` matches nothing useful.**
      `.*` yields `./.` and `./..` rather than the dotfiles — the `glob` crate's
      reading of a pattern that opens with `.`, so the bare word and `glob(".*")`
      are wrong in exactly the same way. There is no working way to list hidden
      entries today. Pre-existing and shared, but the `glob` family makes it easy
      to reach for.
- [ ] **The qualifier argument list.** `*(f)`, `*(size > 1M)` and
      `glob("*", type: file)` — the ANDed predicate options — are unimplemented;
      the type half is reachable through the `:files` / `:dirs` modifiers and the
      `files()` / `dirs()` wrappers, the size/age/`exec`/`empty` half not at all.

## Beyond M3 — `command` ✅ (landed)

- [x] `command [--] NAME [ARG …]` — run the **program** `NAME`, past the builtin
      or function the bare name would reach. This is what makes a wrapper
      writable: `func ls() { command ls --color=auto }` calls the program rather
      than itself, and there was no other way to say it, since a function is
      looked up before an external and cannot be spelled around. Only the words in
      **front of** the program are `command`'s own — everything from the program
      name on is the program's, so `command ls --help` asks `ls` for its help
      instead of printing mesh's, which is exactly the interception the builtin
      exists to escape. `command` owns its `--` for the same reason. The prefix is
      taken off *before* a stage is built, so a piped, redirected or backgrounded
      one is the program's own process rather than a forked shell that then runs
      it, and a job listing names the program. A builtin's name finds no program
      and says so, rather than reporting a bare "command not found" about a name
      `help` lists. A flag-looking word in **front of** the program is a usage
      error rather than a program name, which is what keeps `command -v ls` from
      answering "command not found: -v" — a true statement about the wrong
      question — and keeps the option space free for the entry below. Completion
      follows the same resolution: `command <Tab>` offers `$PATH` alone, and
      `command NAME --<Tab>` asks the program, not the function wrapping it.

- [ ] **`command -v NAME` / `-V` — what would this name run?** POSIX's other half
      of the builtin, and the answer to "is this a function, a builtin, or which
      file on `$PATH`". It is a different job from running something, and it wants
      a decision first: mesh has typed values, so the useful form may be a
      `$sh.which(…)`-style **value** (a map naming the kind and the path) rather
      than a flag that prints a line. Left out rather than half-built — but
      `command -v` is already refused rather than read as a program name, so
      building it later cannot change what a working line meant.

## Beyond M3 — Interpolation

- [x] **A call, and any expression, in `${…}`** — *landed*. `DESIGN.md`
      §"Variables and assignment" already said "General expressions also use
      `${…}`"; only a variable access parsed there, so `"${host-info()}"` was a
      syntax error and every segment had to bind its calls to names first. It
      surfaced porting a real zsh prompt (`docs/PROMPT.md`), where composing three
      helper calls into one line is the whole job.

      Mechanism: the braced body keeps the cheap path when it is a plain access —
      [`valid_variable_access`], resolved by `expand` with only `&Vars` — and is
      otherwise lexed and parsed as an expression and carried as a
      `WordPiece::Value`, the same piece a `$(…)` capture rides in. `Lexer::lex`
      grew a closing-delimiter parameter so a `${…}` body can stop at its `}` the
      way a capture body stops at its `)`.

      `WordPiece::Value` also grew a `quote`, and that is the part worth keeping in
      mind: inside `"…"` the quotes mean *make this text*, so the value is rendered
      by the rule `"$xs"` obeys — a scalar renders, a collection is a loud error.
      Without it a call returning a list smuggled the list out through a pair of
      quotes and quoting stopped meaning "one string".
- [ ] **`${…}` in a bare word still rejects an expression.** `"${f()}"` works;
      `${f()}` unquoted reports "expected a variable name or access". Held back
      deliberately rather than forgotten: a value piece in a *bare* word raises the
      whole-argument rule `DESIGN.md` states for captures — `pre$(x)post` and
      `f()x` are syntax errors, not three arguments — and honoring that needs a
      decision rather than an accident.
- [x] **A modifier's arguments flip the reading of a braced body** — *fixed*.
      `${xs:len}` was the *reference* (sigil-less, resolved as the binding) while
      `${xs:join(" ")}` was not a valid access, so it fell to the expression path
      where a bare `xs` is the **word** `xs`. `:join` then reported "requires a
      list" — and `${xs[0]:join(" ")}` was worse, producing nothing at all rather
      than saying so. Adding an argument silently changed what the body meant.

      The question is asked of the **parsed body**, not of its text.
      `has_modifier_arguments` walks the expression the body already parsed to, and
      `head_as_variable` then reads its sigil-less head as the binding it names.
      That last step is what makes `${xs:join(" ")}` and `${$xs:join(" ")}` agree.

      The first attempt scanned the source for the closing `)` and `}` instead, and
      that is the part worth remembering: a scan has to re-derive the lexer's idea
      of what counts as *text*, and review found four separate places the two
      disagreed — bare escapes (`:join(a\)b)`), raw strings whose content ends in a
      backslash, a `#` that is only a comment at a word's head (`"a"#b` is text),
      and nested interpolations inside a quoted argument. Each fix exposed the
      next. The parse has already applied every one of those rules, so asking it
      costs nothing and cannot drift.

      The *path* still splits, because `expand`'s `Modifier` is a bare enum with
      nowhere to put an argument: an argument-free chain keeps the cheap
      `&Vars`-only route, and one with arguments goes through the expression parser.

      Left as it was: `expand` still cannot carry modifier arguments itself. Giving
      `Modifier` an argument list would let the cheap path take the whole shape and
      retire the split, and is the change to make if the two paths start to drift.

- [x] **The reference path dropped every modifier it could not apply** — *fixed*.
      `expand` implements 35 of the 83 modifiers the parser accepts, and
      `expansion_variable` dropped a `from_name` miss rather than reporting it, on
      the grounds that the miss meant "implemented elsewhere" rather than
      "unimplemented". The cost was a chain that quietly lost a step:
      `"${s:lines:len}"` answered **3** — the length of the string — for a `:len`
      that had been asked of the lines, where every other spelling of the same thing
      said `modifier :lines is not implemented yet`. A wrong answer, not a missing
      one, and silent.

      It now reports, in the words `apply_argument_free_modifier` would have used:
      `requires an argument` for one that takes them, the call-specific message for
      `:capture` (implemented, but for a call — the mislabel the silence was avoiding
      is now avoided by naming it), and `is not implemented yet` otherwise. That
      message is shared as `CAPTURE_NEEDS_A_CALL` rather than written out three
      times.

      Still true, and still the deeper fix: the two paths implement modifiers
      separately, and this only makes the weaker one *honest* about what it cannot
      do. Unifying them — one dispatcher both reach — is what stops the sets
      diverging again.

- [x] **One dispatcher for both spellings of a modifier chain** — *done*. `${x:m}`
      and `$x:m` are the same chain written two ways, and each carried its own
      implementation. They drifted apart four times, every one found singly and in
      review: which names are flags, which table wins on a pattern (`:x` is
      `extended` there and `Modifier::Exec` everywhere else), whether a flag change
      is validated at all, and what a value that cannot take a modifier is told.

      Now `repl::modifier_step` builds a step — folding in the two things only that
      layer knows, which names take arguments and which are implemented past a
      boundary `expand` cannot cross — and `expand::apply_modifier_step` applies it.
      One builder, one applier; `apply_argument_free_modifier` is a call to both.
      The last divergence went with it: every `expand`-known modifier on a pattern
      (`:upper`, `:len`, `:dir`, `:base`, `:stem`, `:keys`) reported a type-generic
      message through the reference path and `not valid for a regex` through the
      other.

      Guarded by the *property* rather than by instances:
      `both_spellings_of_a_modifier_chain_agree` runs 25 modifiers across a pattern,
      a string, a list, a map and a path, and asserts the two spellings produce
      identical output — 125 pairs. It fails on the parent commit.

      Not covered, and a real limit: this unifies the **argument-free** chain.
      `:join(…)`, `:map(…)` and the rest are still applied only on the expression
      side, because `expand` resolves with `&Vars` and no shell while the
      higher-order ones call lambdas. A sigil-less `${xs:join(" ")}` gets there by
      being *routed* to the expression path (see the entry above), not by this
      dispatcher growing arguments.

## Beyond M3 — The predicate vocabulary

- [ ] **`:kind` and `:where`** — the name-resolution half of the predicate
      vocabulary, spelled out in `DESIGN.md` §"Name resolution". `$name:kind`
      gives `keyword` / `builtin` / `func` / `external` / `false` and
      `$name:where` gives an
      external's path, which between them are `have_command` (`$x:kind != false`),
      `is_builtin`, `is_function`, `is_command` and `path` — 41 guard sites in the
      `shrc` this is for, nearly all `if have_command X`.

  - [ ] **First: does this need to exist at all, now that `type` has shipped?**
        Everything below is downstream of the answer, including both items marked
        "open and blocking".

        `type --quiet` already answers the 41 guard sites, and answers them as a
        *command condition*, which is mesh's natural form — no comparison, no
        quoting, no taxonomy:

        ```
        if type --quiet fzf { … }          # against  if have_command fzf
        if shpool:kind != false { … }      # what this item proposes
        ```

        It also settles, by reporting **both** rather than choosing, the two
        questions this item cannot: what to say about a keyword that is also a
        program, and whether a path or a kind is the answer. And it already builds
        the prerequisite this item asked for — `COMMAND_KEYWORDS` split from
        `SYNTAX` / `SYNTAX_WORDS` (`builtins.rs:473`).

        What a modifier would still add is **structured output in expression
        position**: `$x:kind == builtin` branches on a value where `type` writes
        a report and sets a status. That gap is already tracked as `type(NAME)`
        returning a map, which would close it without a new modifier.

        So the live question is whether `:kind` / `:where` earn a second surface
        over the name lookup, or whether this item should be closed as answered by
        `type` plus its value-call follow-up. Not decided.

  - [ ] Decide the plumbing first. `:kind` needs the function table from `Funcs`,
        but string interpolation resolves through `expand.rs`, which is handed
        only `&Vars` — so a naive implementation works in `y = $x:kind` and not
        in `"$x:kind"`. Either thread the funcs through `resolve`/`resolve_value`
        (9 call sites) or give `Vars` a view of the defined names. Both paths
        have to land together: a modifier whose answer depends on where it is
        written is the failure this one exists to prevent.
  - [ ] **Prerequisite: one table, three views — not one predicate.** The three
        callers ask three different questions, and `and` separates all of them:
        `help and` must answer, `func and()` is allowed, `and:kind` is not
        `keyword`. So the views stay distinct:

        | asks | set |
        |---|---|
        | `:kind`, for `keyword` | claims command position |
        | `func`, for its refusal | three parts, none derived from deadness — `func`/`not`/`return` as inherited policy, the value-call names, **and every builtin** via `is_builtin` |
        | `help`, for coverage | every reserved word, mid-form included |

        That whole row is a separate check the table *supplements*, not
        replaces, and none of it derives from the reserved-word analysis.
        Builtins: `pwd` is not a reserved word and has no row, but `func pwd()`
        is refused today and must stay refused. `func` / `not` / `return`:
        refused at `repl.rs:1153` today, kept refused as policy — the probe
        cannot be run on them, and for `return` the likely answer runs the other
        way, since only bare `return` is intercepted as control flow. No
        command-position word belongs here on deadness grounds; none is dead.

        What to unify is the *data*. The same words are written out three times
        today — `parser.rs` inline via its `self.word("…")` arms plus the
        value-call reservation, `RESERVED_FUNCTION_NAMES` at `parser.rs:1869`
        and enforced in `Parser::function` at `parser.rs:2457`; `repl.rs:1153` from a hardcoded
        `func` / `return` / `not` plus `is_builtin`; and `builtins.rs`'s `SYNTAX`
        table, whose coverage is asserted by
        `every_keyword_the_parser_reserves_is_explained`, with the list itself
        living inside that test. Give it one table with a row per word and derive
        each view from it, so a new keyword is added once. Demote the test to a
        consumer. Do **not** collapse the views into a single predicate: that
        either misclassifies `and` as `keyword` or stops documenting it.
  - [ ] **Do not use `syntax_help(name).is_some()` as the oracle.** It is off by
        exactly one: `cmd` is registered as a documentation placeholder for an
        ordinary command line (`builtins.rs:203`) and is reserved by nobody, so
        `cmd:kind` would answer `keyword` and mask a real program of that name.
        `help` documents *shapes* as well as words; reservation is the narrower
        question. Worth a test pinning `cmd:kind` to whatever it resolves to on
        the machine, not `keyword`.
  - [ ] Do **not** re-type the list into the implementation either — this design
        entry got it wrong twice, missing `not` on one pass and
        `in` / `and` / `or` / `re` / the value names on the next. Ask the owning
        predicate. The invariant to test is narrow, and the exact width matters:
        **`:kind` never answers `false` for a word the shell claims in command
        position** — *not* "a word the shell handles", which is wider and false.
        `and` is handled as infix syntax, and `and:kind` is correctly `false`
        where no such program exists. Writing the invariant the wide way drives
        the implementation back to calling mid-form words `keyword`, so assert
        the narrow one: `if:kind` is never `false`, `and:kind` may be.
  - [ ] **Do not "fix" the expanded path to match.** Quoted into command
        position, every keyword but `return` falls through to external lookup on
        `main` today:

        ```
        n = "if"; $n x        # command not found: if     — only with no func of that name
        n = "break"; $n x     # command not found: break  — likewise
        n = "return"; $n x    # returns — `run_expanded` intercepts this one
        ```

        **Every one of those answers is conditional on no function of that name
        being defined.** Expanded-name lookup hits `shell.funcs` first
        (`repl.rs:5529`), so all ten accepted command-position names — `if`,
        `match`, `for`, `while`, `loop`, `break`, `continue`, `global`, `unset`,
        `export` — run the function instead when one exists. Only `func`, `not`
        and `return` are refused as definitions and so cannot.

        The fall-through is not a divergence to close *in the engine*: the
        expanded path behaves correctly, so nothing here needs changing.

        **What `:kind` should answer about it is the open question below, and
        this item must not pre-empt it.** An earlier draft said "`external` /
        `false` would be worse than `keyword`, not more precise" — that is option
        A's conclusion stated as though it were settled, and option B has `:kind`
        report `external` for exactly this case. Both readings are live; do not
        let a test written from this item encode either one. `return`'s
        interception is the odd one out; whether it should exist is a separate
        question. Pin the fall-through with a test **in the no-function
        case**, and a second one showing the function wins when defined — pinning
        only the first would encode the wrong behavior for ten names.
  - [ ] **`keyword` means claimed in *command position*, not reserved anywhere.**
        The reserved list splits almost evenly, and only the claimed half is
        `keyword`. Probe by typing the bare word and asking whether *resolution
        ran*:

        ```
        if      → syntax error              claimed, so `keyword`
        break   → break: not inside a loop  claimed — parsed fine, complained at run time
        return  → return: not inside a func claimed, likewise
        fork    → command not found         syntax only before a block
        else    → command not found         mid-form syntax; same for in/and/or/unless
        re      → command not found         reserved from `func` definitions only
        link    → link: missing operand     resolution ran — /usr/bin/link
        ```

        Claimed: `func`, `if`, `match`, `for`, `while`, `loop`, `not`, `return`,
        `break`, `continue`, `global`, `unset`, `export`. Everything else in the
        reserved list resolves normally, and `:kind` must too — answering
        `keyword` would hide a callable function or, for `link`, a real program.
        Probe by asking **whether resolution ran**, not whether the parse failed:
        `command not found` (or the name's own program running) means it did.
        `break`, `continue` and `return` parse fine and object about *context*
        (`break: not inside a loop`), so a syntax-error test would file them
        under resolution and let `:kind` answer `false` — the one thing the
        invariant forbids.
        Build the `:kind` view from command-position claims — not from
        `RESERVED_FUNCTION_NAMES` (wider by the value names) nor from the
        parser's full word list (wider by the mid-form words) — and make this
        probe the test.
  - [ ] *(Adjacent, pre-existing.)* `Function` in `repl.rs` refuses `func`,
        `return`, `not`, the value names and builtins, but accepts `func if()`,
        `func while()`, `func break()` and the rest. **Command position is not
        the test for this one** — reachability has two call forms, and a name the
        parser claims as a statement can still be called as a value:

        ```
        func while() { return OK }; x = while(); puts $x     # OK
        ```

        Same for `loop`, `break`, `continue`, `global`, `unset`, `export`. And
        there is a **third** call form that reaches all the rest — an expanded
        name, which `repl.rs:5529` looks up in `shell.funcs` before anything
        else:

        ```
        func if(x) { puts OK }; n = "if"; $n arg            # OK
        ```

        `if`, `match` and `for` go this way too, so **no** accepted definition is
        dead. The dead-definition premise for this item is empty: reserving any
        of these names is a deliberate compatibility break — argue it as "a
        function callable only through `$n` is a trap worth closing", not as a
        cleanup. Test the expanded-name call on `if` so the third form is pinned.
  - [ ] **Open, and blocking: what `keyword` says when the name resolves to
        something.** The parser claims these words only **bare**. A quoted or
        expanded head resolves normally:

        ```
        "if" x           # func `if` if defined, else an `if` program, else not found
        n = "if"; $n x   # same path — quoting and expanding agree
        ```

        The value call is not a third resolving spelling: `x = if()` is a syntax
        error, and only 7 of the 13 (`while`, `loop`, `break`, `continue`,
        `global`, `unset`, `export`) reach a func that way. Name-dependent — do
        not fold it into the quoted/expanded path, or an implementation will make
        reserved value syntax callable.

        So `if:kind == keyword` can hide a real program or function — the
        failure this design uses to argue mid-form words must resolve normally.
        **State the choice without reference to the receiver.** Modifiers take
        values, so `if:kind`, `"if":kind` and `$n:kind` over a variable holding
        `"if"` are one call on one string; how the receiver was spelled never
        reaches `:kind` and cannot pick which reading it reports. The name `if`
        is claimed as a bare command head *and* resolves func → external through
        every other route; the question is which of those the taxonomy is about. Either `keyword` is about the word (always
        `keyword`; stable, wrong when something real exists) or `:kind` reports
        what would be found (`func`/`external` when one exists, `keyword`
        otherwise — matching `pwd:kind == builtin` against `command pwd`, and
        making `keyword` mean "nothing else claimed this"). Either way `return`
        needs carving out: `run_expanded` intercepts it before external lookup
        (`repl.rs:5814`), so `"return" x` is control flow even with a `return`
        executable on `PATH` — so it stays `keyword` as a named exception, or
        removing that interception is part of the option. Settle before
        implementing; it changes what the modifier means.

        **`type` has since answered this with a third option neither bullet
        offered: report both.** `type if` gives `if is syntax (shadowing
        /usr/bin/if)` and `type --all if` lists the two findings separately, so
        it never picks between "the word is syntax" and "a program exists". The
        either/or was an artifact of `:kind` returning a single value, not of the
        question. It also draws the contextual distinction this entry argued for:
        with a real program on `PATH`, `and` / `fork` / `else` / `in` / `unless`
        report the program and no "syntax" at all, while `if` reports syntax plus
        the shadow.

        So this survives **only if `:kind` exists as a single-valued modifier**,
        which is itself open below. Do not settle it before that.
  - [ ] **Open, and blocking: is `:where` about resolution or about `PATH`?**
        Separate from the `keyword` question — shadowing is ordinary, not exotic:
        `pwd:where` (builtin, `/bin/pwd` exists), `ls:where` with the
        documented `func ls() { command ls … }` wrapper, `if:where` under
        option A. Either `:where` follows resolution (all `false`; the pair never
        disagrees, but it **stops being `path`**, which searches `PATH` and would
        answer `/bin/pwd`) or it answers about the filesystem (all give the path;
        `:kind` and `:where` then openly disagree). `pwd:kind` is `builtin`
        under both `keyword` options, so the builtin and func rows must be
        decided on their own terms. Test whichever is chosen on all three rows,
        since the wrapper idiom makes `ls:where` the common case.

        **`type` answers this the same way — both at once.** `type pwd` gives
        `pwd is a builtin (shadowing /usr/bin/pwd)`: the kind and the path in one
        report, so "resolution or `PATH`" never arises. Its `PATH` scan follows
        `execvp` (skipping a candidate it cannot execute rather than stopping at
        the first name match), which is the behavior this item's probes established
        was required. Like the `keyword` question, it survives only if a
        single-valued `:kind` / `:where` pair exists.
  - [ ] Resolution order is command position's — keyword → builtin → func →
        external — **for the bare form, and pending the open question above**.
        "Cannot disagree with what running the name would do" does not name a
        single behavior: bare `if` is syntax, while `"if" x` and `$n` share one
        path that resolves func → external. Option A keeps this order as written; option B makes `:kind`
        follow the resolving order (builtin → func → external, `keyword` only
        when nothing is found). Do not implement this item until that is settled
        — the two are different contracts. Note that
        `command NAME` looks past **all** of it — keyword, builtin and func —
        since bypassing the wrapper is the point (`func ls() { command ls … }`),
        and that it only *looks*: `pwd:kind` is `builtin` while `command pwd`
        runs `/bin/pwd`; `command return` inside a function reports not-found and
        keeps going; and `command cd` is `command not found` where no `/bin/cd`
        exists. `:kind` reports what it finds, which is why it is defined on
        resolution rather than on what `command` would do — `command` can fail on
        a name that resolves perfectly well. Worth a test for each direction.
  - [ ] **A receiver containing `/` is a path, not a name.** `execvp` never
        consults `PATH` for a slashed word (`exec.rs:1061`), so `./tool` runs
        today, and the modifier already binds on such a word (`./tool:upper` →
        `./TOOL`). Keyword/builtin/func cannot apply — none of those names can
        contain a slash — so it is external-or-nothing. That much is forced.
        **What it resolves to is not, and the exec bit is not the predicate.** A
        mode-755 script whose shebang names a missing interpreter does not run —
        `./btool` gives `command not found`, because `execve` fails `ENOENT` on
        the *interpreter*. An earlier draft recorded "`external` wherever the file
        is executable" as forced; that is retracted.
        **`PATH` lookup does not stop at the first name match** — a candidate it
        cannot execute is skipped and the search continues. Verified:

        ```
        tool      # ran-from-d2  — non-exec d1/tool skipped, exec d2/tool runs
        dirtool   # permission denied — a directory, no later candidate
        tool      # permission denied — only the non-executable one on PATH
        btool     # ran-from-e2  — e1/btool is mode 755 with a missing interpreter
        btool     # command not found — only the bad-shebang one on PATH
        ```

        So neither "first name match" nor "first *executable* file" is the
        predicate — both name files the shell will never run. Note the failures
        differ: `EACCES` for non-exec and directories, `ENOENT` for a bad
        interpreter, and `execvp` continues past both.

        **Exact fidelity is not reachable**, so retract the principle "`:where`
        may not disagree with what the shell does". Knowing whether a file runs
        means reading the shebang, resolving that interpreter and recursing, and
        it can change before the command runs. Every POSIX shell has this gap —
        but **shells disagree about which**, so `command -v` is not a reference
        behavior to copy — verified with a mode-0644 `ptool` and a mode-0755
        `btool2` with a missing interpreter, both on `PATH`:

        ```
        bash 5.2.21   command -v ptool  → path, rc 0    no permission check
                      command -v btool2 → path, rc 0
        dash          command -v ptool  → rc 127        checks the bit
        ```

        **Use the measured table, not a prose summary** — this has been
        characterized wrongly three times. Each candidate alone on `PATH`, `rc`
        from `command -v NAME`:

        | candidate | bash 5.2.21 | dash |
        |---|---|---|
        | regular file 644 | 0 | 127 |
        | regular file 755 | 0 | 0 |
        | directory 755 | 1 | 127 |
        | FIFO 644 | 0 | 127 |
        | FIFO 755 | 0 | 127 |
        | symlink → reg 755 | 0 | 0 |
        | broken symlink | 1 | 127 |

        bash: exists and **not a directory** — a FIFO passes at either mode, and
        there is no permission check. dash: **regular file** *and* execute
        permission for the **effective** user — a mode-755 FIFO has the bits and
        is still rejected, so both tests are needed. Both follow symlinks, both
        reject a broken one, neither reads the shebang.

        Choose an approximation and name the shell: name-match (bash — exists,
        not a directory), permission bits (dash — regular file the effective user
        may execute; wrong for a bad interpreter), or shebang-following (better,
        unbounded, racy, matches neither). Test every row of the table under
        whichever is chosen — directory and FIFO especially, since "has the exec
        bit" and "is not a directory" each admit one of them wrongly. Write `:where`'s
        promise as "what lookup would select", not "what will run".

        **Open — one question, not two, and the same in both settings:** when
        nothing runnable is found but a file of that name exists, is `:kind`
        `false` (nothing here can run) or `external` (a file is present)? Covers
        non-executable files and directories, direct and via `PATH`, plus what
        `:where` returns in those cases. Separately open: whether `:where` gives a
        direct path as written (`./tool`, as both shells above report it) or
        absolutized.
        Test executable, non-executable, directory, missing **and bad-interpreter**
        — relative, absolute, and via `PATH`, including both skip-to-later-candidate
        cases (`EACCES` and `ENOENT`).
        Bears on the `:where` question above in two directions: a direct path has
        no `PATH` answer at all, and where it does search `PATH`, "searches
        `PATH`" is not a specification until it says "the way `execvp` does".
  - [ ] Absent is **`false`**, not an error, unlike `:type`. Worth a test that
        pins it beside `:type`'s error, since the two siblings deliberately
        differ.

- [x] **Claim both: a modifier chain binds in expression *and* argument context,
      argument-free or not, on bare and quoted subjects alike** *(landed)*. On `main` today the four cases split
      two ways, and the axis is not the one the old note assumed:

      ```
      puts "a.b":stripend(".b")   # a          — applied
      puts abc:stripend("c")      # ab         — applied, and the subject is BARE
      puts "abc":upper            # abc:upper  — literal text
      puts abc:upper              # abc:upper  — literal text
      ```

      So the divergence is *not* bare vs quoted. `value_argument_starts`
      (`parser.rs:2319`) only claims a chain ending in `(`, so argument-taking
      binds on both spellings and argument-free binds on neither. **The bare-word
      colon grab has therefore already happened** — this item completes its
      remaining half rather than opening a new one.

  - [x] **`:` + identifier is reserved by the grammar, not gated on a name list**
        *(decided; shipped)*. An unknown modifier is an **error**, not text. Half of this
        already holds — expression position claims the chain outright
        (`x = ubuntu:latest` is a syntax error today), and `modifier_name`
        (`parser.rs:4562`) consults `MODIFIER_NAMES` only to decide the
        *argument*-position fallback. So this makes argument position agree with
        expression position rather than adding a rule.

        Rejected alternative — keep the name-list gate — because the reserved
        vocabulary then grows silently with every modifier added: `img:raw` is
        text until someone adds `:raw`. This proposal is the first instance
        (`kind`, `where`), which is why an earlier draft had to float a
        deprecation cycle. Reserving the shape up front makes adding a modifier a
        non-event for argument text.

  - [ ] **The cost:** a one-time, fully-known break. `docker run ubuntu:latest`,
        `git show HEAD:file`, `rsync host:src dst`, `curl -H Accept:application/json`
        need quoting — `"ubuntu:latest"`, colon inside the quotes, not
        `"ubuntu":latest`. **Braces, not quotes, when the subject interpolates**
        (`"${image}:latest"`); see the escape-hatch item below. Measured on the `shrc` this exists for it is **zero** —
        every `word:identifier` in `shrc`, `config/fish/config.fish` and
        `config/nushell/config.nu` is inside a single-quoted `LS_COLORS` string or
        a comment, none unquoted in command-argument position. The pain is on
        interactive typing, which a repo survey cannot measure.
  - [ ] **Quoting the subject does not preserve the old reading**, and that is the
        migration trap: `"abc":upper` is literal today and becomes `ABC`, exactly
        as the bare form does. The escape hatch is quoting the **whole token**:

        ```
        puts "abc":upper      # ABC        — modifier applies
        puts "abc:upper"      # abc:upper  — literal, colon inside the string
        ```

        Worth calling out in any migration note, since `"img":raw` is the form a
        reader reaches for first and it is the one that changes.

        **The whole-token rule does not cover an interpolated subject**, which is
        the case that will actually bite. A modifier already binds inside a
        double-quoted string:

        ```
        x = "abc";    puts "$x:upper"      # ABC        — quoted, still applies
        x = "abc";    puts "${x}:upper"    # abc:upper  — braces stop it
        x = "ubuntu"; puts "$x:latest"     # text today; an ERROR under the rule
        x = "ubuntu"; puts "${x}:latest"   # safe either way
        ```

        So `"$image:latest"` breaks despite being fully quoted, and the literal
        spelling is `"${image}:latest"` — **braces, not quotes**. That is the form
        the migration note must give: dynamically assembled Docker tags and
        `rsync` targets are where this lands, and they are already quoted.
        (Checked against the target `shrc`: zero occurrences of `$var:ident`, so
        the measured cost there stays zero — but the pattern-based survey would
        not have caught them, and this one had to be looked for separately.)
        Test both spellings, known and unknown modifier name.
  - [ ] Tests for the grammar rule:
    - [ ] An **unknown** modifier errors in *both* expression and argument
          position — `x = ubuntu:latest` and `puts ubuntu:latest` alike. The
          second is the one that changes; do **not** pin it as text, which would
          reinstate the name-list fallback this design rejects.
    - [ ] The same for a quoted subject (`puts "ubuntu":latest`), since receiver
          spelling is not the axis.
    - [ ] Whole-token quoting stays literal (`puts "ubuntu:latest"`) — the only
          row that remains text.
    - [ ] A **known** modifier applies in both positions, argument-free and
          argument-taking (`puts abc:upper`, `puts abc:stripend("c")`).
    - [ ] The error names the offending modifier — `unknown modifier: latest`,
          not a bare syntax error — **including on a keyword receiver**
          (`x = if:latest`, `x = not:latest`), which today reports `expected {`
          and `expected a value expression` from the keyword parser instead. That message is the entire migration story for
          anyone who hits it, and today the equivalent expression-position failure
          is only `syntax error: expected a statement separator`.
  - [ ] **Any attached `:identifier` must outrank keyword parsing** — the
        binding rule above does not imply it and the parser does not do it.
        Expression position already binds a bare subject (`x = abc:upper` → `ABC`),
        so the argument rows are most of the change, but four receivers are
        claimed before the postfix-modifier loop and fail in expression position
        too: `if`, `match` and `for` via `primary` (`parser.rs:3502`), and `not`
        via `not_expression` (`3078`).

        ```
        x = while:upper     # WHILE          — the ordinary path
        x = if:upper        # syntax error: expected `{`
        x = match:upper     # syntax error
        x = for:upper       # syntax error
        x = not:upper       # false          — no error, silently wrong
        ```

        **Not just *recognized* modifiers.** The grammar rule reserves every
        attached `:identifier`, so the lookahead has to yield to an unknown name
        too, or `x = if:latest` is consumed as `if` syntax instead of reporting
        `unknown modifier: latest`. All four already error on an unknown name,
        but with messages from the keyword parser rather than the modifier one:

        ```
        x = if:latest       # syntax error: expected `{`
        x = not:latest      # syntax error: expected a value expression
        x = for:latest      # syntax error: expected a name
        x = match:latest    # syntax error: expected a value expression
        ```

        So the fix is one change, not two: make the lookahead yield to any
        attached identifier, and the known and unknown cases both fall out.

        `not` first among the four: with a *known* name it does not fail at all —
        `x = not:upper` evaluates to `false` — so a guard over it reads as "no
        such name" forever. With an unknown name it errors, so the two halves of
        `not` misbehave differently and both need pinning. Note this also breaks the entry's own
        recommended guard — `if if:kind != false { … }` is currently
        `unexpected end of input`. Every other reserved word (`while`, `loop`,
        `return`, `break`, `continue`, `global`, `unset`, `export`, `func`, `and`,
        `fork`) already takes the ordinary path, so this is a four-name carve-out,
        not a general keyword problem. Cover a keyword receiver in the parser
        tests.

- [ ] **The session half needs no language surface.** `connected-remotely`,
      `inside-project`, `in-shpool`, `is-interactive` and friends are ordinary
      `func`s over `$sh.interactive`, `$sh.stdin:tty` and `$env:get(NAME, "")`,
      all of which exist. Worth an `rc.mesh` example rather than builtins.

## Rough edges found porting a real config

Thirty findings from porting a ~1800-line bash/zsh config to mesh
(`mikelward/conf#226`), the first thing of that size written against the
language. Each is worked around in that config, so none of them blocks a port —
what an entry records is what the workaround *costs*, which is what decides
whether the edge is worth closing. The numbering is the PR's, so a finding can be
matched back to the discussion. Eighteen have since been fixed; two are tracked
elsewhere in this file and are cross-referenced rather than restated. Every entry
was re-checked against `main` rather than taken from the PR text.

*Keep the count above in step with the checkboxes below — it went stale once
already, reading `fourteen` against sixteen boxes, which is exactly the sort of
thing a reader takes on trust.*

- [x] **1. A `...rest` function refused an unknown long flag.** A plain `func`
      scanned every `--`-leading argument against its signature, which broke both
      kinds of rest parameter — one holding a delegated command's options
      (`setx curl --location URL`), one holding data that merely looks like an
      option (`bak --weird-name`, `error "--x is unset"`). Neither is a flag
      *that* function owns. Fixed by `wrapper func` (mikelward/mesh#286) and the
      terser `alias NAME = COMMAND` (mikelward/mesh#289); the config is 130
      aliases and 53 wrappers on the far side of it.
- [x] **2. `VAR=value cmd` is a syntax error.** No one-command environment
      prefix, so every occurrence becomes `fork { $env.VAR = …; cmd }` — three
      lines and a process for what is one word in every other shell. The config's
      `ssh-to` (`LC_CLIENT_HOST`) and `xr` (`DISPLAY`) both pay it.

      **Half fixed:** `with NAME=value … { … }` runs a block with those
      environment entries in place and restores them on the way out, however the
      body leaves. It takes as many bindings as a prefix would, spells each of
      them the same way (unspaced `NAME=value` / `NAME+=value`), and costs no
      process — so the `fork` is gone and the three lines are one header. See
      §`with` in `docs/REFERENCE.md`.

      **Fixed, the other half:** `VAR=value cmd` is the prefix form, binding to
      one **stage** — `FOO=1 a | FOO=2 b` gives each side its own, `FOO=1 a && b`
      leaves `b` alone, a function or builtin gets it applied and restored around
      the call, and a name that was unset goes back to unset. It shares `with`'s
      apply/restore, so the two spellings cannot drift.

      The collision it creates was accepted knowingly and is written down rather
      than hidden: `FOO=bar cmd` writes the **environment**, while `FOO=bar` alone
      binds a shell variable no child sees. A prefix that wrote a shell binding
      would do nothing for the child, so it has to mean the environment. Whether a
      bare `FOO=bar` should mean it too is its own entry under Loose ends.

      The historical note, kept because it was wrong for a while: this entry once
      argued the prefix would cost `x=1` and `x=1 cmd` being one construct, and
      that `x=1 y=2` — a clean `expected a statement separator` today — would
      become a command-not-found. Neither happened. A binding run is parsed and
      **given back** unless a command follows it, so `x=1` and `x=1 y=2` read
      exactly as they did.

      **The one bash behavior not copied**, and it is POSIX's rule rather than
      bash's own: under `--posix` (and in dash), a prefix on a *special builtin*
      leaks into the shell — `FOO=qux :` leaves `FOO` as `qux`. Default bash does
      not do this. Verified both ways rather than taken from folklore.

      **Superseded, kept for the record:** the prefix form as an open question. It
      is
      available — mesh requires a separator between statements, so a `NAME=value`
      followed by another word on the same line has exactly one possible reading,
      and the parser can take it syntactically. What it costs is that `x=1` and
      `x=1 cmd` stop being the same construct, and `x=1 y=2` — today a clean
      `expected a statement separator` — becomes a command-not-found for `y=2`.

      Open questions if it lands: does a prefix on a **function** or a builtin
      scope the same way (they run in-shell, so it is `with`'s push/pop rather
      than the child's environment); per-stage in a pipeline
      (`FOO=1 a | FOO=2 b`); and per-job when backgrounded. Whatever is decided,
      the mechanism is `with`'s — the prefix would be a second surface on it, not
      a second implementation. One bash behavior to **not** copy: its special-
      builtin rule, where `FOO=bar export …` leaks `FOO` into the shell
      permanently.
- [x] **3. Negating a *command's* status had no spelling.** `if not cmd` was
      ``syntax error: expected `{` `` — `not` starts a **value**, deliberately, so
      it never claims `not foo`. Together with 15 below that left no direct way
      to branch on "this command failed": the command had to run as a statement,
      `$sh.status` be read into a name, and the `if` test that. Predicates written
      as value functions (`if not have-command(x)`) were fine; it was the external
      command that had nowhere to go.

      **Fixed:** a `not` with no value after it negates the operand's *status*, so
      `not test -f $config` reads as it does in every other shell. Which of the two
      readings applies is decided by the same `value_start_in` the position already
      uses, asked one token later — `not $ready` and `not have(x)` are values and
      rewind to the expression parser, `not ls` is a command — so nothing that parsed
      as a value before parses as a command now. `command_negations` is that one
      test, and both an `if` / `while` condition and a plain statement call it, so
      the word has one meaning wherever it is written. A run of `not`s folds by
      parity as it already did for values, and a pipeline negates as a whole. A
      command needs a **word** to name it, which is what keeps `not = 5` the syntax
      error it already was rather than `command not found: =`.

      **In a condition the negation is a reading**, not a second run, so it sits
      outside the operand (`Executable::Not`) rather than inside the pipeline: the
      command still publishes the code it really exited with, and `$sh.pipestatus`
      keeps its per-stage breakdown. `if not sh -c 'exit 130' { puts $sh.status }`
      answers `130` where bash's `!` has flattened it to `1` — which is 15's
      guarantee, kept rather than undone one level up.

      **As a statement the negated code is the result**, since there is no branch to
      carry it: `not sh -c 'exit 3'` exits `0`. That is the rule a value statement
      already followed — bare `false` exits `1`, and so does `1 == 2` — so `not` is
      not manufacturing a kind of status mesh lacked. `$sh.pipestatus` follows
      `$sh.status` here and reports one stage, by `run_recorded`'s existing invariant
      that the breakdown always describes the run that produced the status: a `0`
      explained by a `3` would break it. The two positions differing is the point
      rather than a wart — the same split a value *condition* (publishes nothing) and
      a value *statement* (publishes its truthiness) already have.

      Where the two positions disagreed before `not` existed, the negation inherits
      the disagreement rather than papering over it: a spaced `>` is a comparison in
      a condition and a redirection in a statement, so `not $n > 2` negates a
      comparison in one and a redirected command in the other — exactly what each
      does without the `not`.

      **A guard that skipped its statement is exempt.** `not cmd if false` ran
      nothing, so there is no status to negate and the previous command's still
      stands — `Produced::Nothing` says which, and it is the same exemption every
      other guard already has. Without it the inherited failure flipped to success:
      `false; not puts BAD if false && puts RAN` ran `RAN` where the un-negated line
      does not. A **list-pattern binding** needed asking for too: `value_start_in`
      answers for the `[` alone, so `not [head ...tail] = $xs` rewound and lost the
      negation while the un-negated binding worked. The exemption then needed a
      **baseline of its own**: a `while` header starts at `Produced::Nothing` and the
      list-pattern arm answers without touching it, so the loop's own first test
      looked like a skipped guard and `while not [a] = [x]` ran backwards both ways.
      The answer is to ask the **tree**, not the field: a pattern test always ran, and
      `Produced::Nothing` describes only the shapes that reach the funnel that sets
      it. Writing the field instead — a baseline set before recursing — fixed the
      direction and then cost the loop its no-pass `""`, since a completed test
      looked like a *pass*. All four raised in review.

      **Only two positions gained the command reading**, because they are the two
      where a command can be written: a condition and a statement. A postfix guard
      and an assignment's right-hand side parse a value expression and nothing else,
      so `not` there is the value operator alone — `puts ok if not test -e /` passes
      those words to `puts`, exactly as the un-negated guard does.

      **Not backgroundable.** `not cmd &` is refused: the status to invert arrives
      when the job is waited on, not when it is launched, so inverting at launch
      would report the negation of "started successfully" and leave the job's real
      code un-negated in the table.

      **One property worth naming**, since a value operand wins: the *programs*
      `true` and `false` are not reachable through `not`. Nothing is lost by it — a
      boolean and a command exiting `0` mean the same thing, so `not true` and
      `not /bin/true` answer alike, as do `not false` and `not /bin/false`.
- [x] **4. A `/…/` regex literal ends at the first `[`, `(`, `{`, `|`, `:`, `,`,
      `;`, `<`, `>` or `&`.** Fixed with the "Loose ends" entry it shares — see
      *A bare `/…/` literal cannot hold a space or an unbalanced paren* — so
      `/[A-Za-z]/` is now the pattern it looks like and `re("[A-Za-z]")` is a
      choice rather than the workaround.

      The **silence** this entry added is worth keeping in view, because the fix
      removed the symptom without touching the cause. `if $x ~ /[A-Za-z]/` did not
      report because an `if` condition that fails to parse as a value is re-read
      as a *command*, so the shell ran `$x` and took the `else` branch. That
      fallback is deliberate — a partial command has to stay buffered rather than
      erroring — but it swallows the diagnostic for any condition that was clearly
      meant as a value. Nothing else in the file makes a wrong answer this quiet.
- [x] **5. `$env[$name] = value` — no dynamically-named environment write.**
      `$env:get(NAME, default)` reads by computed name and had no writing twin, so
      a generic "parse this tool's `shellenv` output and apply it" helper could not
      be written at all; every tool's variables had to be named literally, which is
      why `setup-fnm` and `setup-brew` are each hand-written.

      **Fixed.** The asymmetry was narrower than this entry said: `$env[$name]`
      already *read* by computed name, falling out of a bare `$env` being the whole
      table — exactly as `DESIGN.md` predicted when it said indirect environment
      access comes for free. What was missing was that the same spelling was not a
      **place**. It is now, for both a write and a removal, and both resolve the
      subscript through the one `subscript_key` a read goes through, so the pair
      cannot drift into two notions of what a computed key is.

      **`unset $env.KEY` landed with it**, and that half was a gap nothing had
      recorded: there was no way to remove an environment entry at all, only to
      empty one, so a child could not be made to see a name as unset — the
      distinction `${VAR-default}` turns on in every POSIX shell. It is the
      loud-when-missing removal every other `unset` target already is, and it takes
      no `global`, since the environment is the process's rather than a scope's.

      **A run-time key needed a run-time check.** `set_var` and `remove_var`
      *panic* on an empty name, a `=` in one, or a NUL — `EINVAL` at the syscall,
      which `std` turns into an abort. A literal `$env.KEY` could never carry one,
      because the parser proved it a name first; a computed key is the first
      environment name a user supplies that the parser never sees, so
      `environ::check_key` reports those three instead. It checks **only** those
      three rather than re-applying `valid_name`: `$env:keys` answers with every
      name the process really has, so a round trip over the listing has to be able
      to write back a name mesh's own grammar could not spell.

      **Still one access, deliberately.** `$env.PATH[0] = …`, `$env.PATH:dedup = …`
      and `$env[0..2] = …` stay syntax errors, for a write and an `unset` alike — a
      slice or a modifier names a derived value, and an entry is bytes with nothing
      inside it to reach into. `export`, `with`, and the `NAME=value` prefix keep
      taking a spelled-out name only, since each is a header whose names are read at
      parse time; `$env[…]` in the body covers the computed case.

      **The payoff is wider than the `shellenv` helper this entry was about.**
      `docs/INTEGRATION.md` called the missing write "the narrowest blocker in the
      whole document" — it is what direnv and nvm need, since their contract is a
      computed diff applied in a loop with a `null` meaning "unset this". Those now
      want only a JSON reader. **mise needs one thing more**: its output looks like
      a target state, which cannot express a removal, so it also wants a source
      that reports them — see the apply entry under "External tool integration".
- [x] **6. A syntax error carried no line or column, and there was no way to
      check a file without running it.** A config that *generates* mesh source (see 26) had no way to check
      the generated file before sourcing it, so its only test was whether sourcing
      it broke the shell.

      **Fixed, the second half:** `-n` / `--no-execute` parses the input and runs
      nothing — silent on success, `2` and a located diagnostic on a syntax error,
      so `mesh -n generated.mesh && source generated.mesh` is the check that was
      missing. It skips the startup files: `env.mesh` is ordinary mesh code, and
      sourcing it to check an unrelated file would run arbitrary commands.

      **Fixed, the first half:** a syntax error used to report
      `syntax error: unexpected end of input` and nothing else, so locating one in
      an 1800-line config meant bisecting it. Diagnostics now carry
      `file:line:column`, and an unclosed delimiter is reported **at the
      delimiter** rather than at the end of the file — the innermost one, which is
      what has to be closed first. The spans were already on `ParseError`; what
      was missing was that `ParseOutcome::Incomplete` discarded the error rather
      than carrying it, so the one case a config hits most had nothing left to
      report.

      **Still unlocated: heredocs.** An unterminated heredoc takes the
      `IncompleteHeredoc` arm, and a malformed interpolation inside a body is
      found by a hand-written scan in `repl.rs` rather than by the parser. Neither
      carries a span, so both report the message alone. The scan would need to
      know the body's offset within the file to say more. Raised in review on the
      PR that located the rest.

      **One residual gap in the piped line count**, from the same review. A piped
      session counts the lines `gets` takes off descriptor 0, since they are lines
      of the same stream the commands come from. In a *forked pipeline stage* —
      `gets x | cat` — the count happens in the child's copy of `Vars` and dies
      with the stage, so the parent's later diagnostics are short by the lines
      that stage consumed. Getting it back means the child reporting to the
      parent, which is a lot of machinery for a line number in a shape that is
      already rare (`gets` in a pipeline, in a piped script, before a syntax
      error). Left as is deliberately; if it is ever worth closing, the count
      would ride back on whatever channel a forked stage gets for reporting.
- [x] **7. No terminal width.** `$sh` had no `width`, so the prompt's rule cost
      one `tput cols` **fork per prompt** — measured at 2.6ms against 2.0ms for
      the whole prompt composition path, so the decoration cost more than what it
      decorated.

      **Fixed** by `$sh.width`, a `TIOCGWINSZ` read. No `SIGWINCH` refresh was
      needed after all: the entry is read at each access rather than cached, and
      the ioctl is current the instant the window changes — the signal is only the
      notification that it did, so a cache would need it to stay honest where a
      live read cannot be stale. One `ioctl` against a fork, or up to three when the
      fallback below walks past a redirected stdout. It asks stdout's
      terminal, then stderr's, then stdin's — the width that matters is the one
      being looked at, and a redirected stdout answers `ENOTTY` rather than the
      terminal behind it, so `mesh script.mesh | less` reaches the real width
      through stderr. With no terminal anywhere it answers `0`, which is not a
      width, rather than a made-up 80.
- [ ] **8. No way to set the window title.** OSC 0 landed as an automatic
      `user@host: dir` (§"Beyond M3 — Terminal integration") with
      `$sh.options.osc-title` to turn it off, but there is no way for a config to
      *supply* the text — so the choice is mesh's title or no title.
- [x] **9. A newline inside an unclosed `(…)` ends the expression** unless it
      follows an operator, so a two-line `x = (1` / `+ 2)` is
      ``syntax error: expected `)` `` where `docs/REFERENCE.md` reads as
      continuing. Either the parser or the reference is wrong; decide which.

      **Fixed: the parser was.** Three of the four newline positions in a group
      already worked — after the `(`, after an operator, before the `)` — and only
      an operator *opening* the next line was refused, which is the spelling a
      wrapped sum is usually written in. So this was one missing skip rather than
      a rule anyone had chosen: `primary` stepped over newlines at the edges of a
      group and `binary` stepped over them after taking an operator, but nothing
      looked past one to find the operator in the first place.

      A group and a `${ … }` body hold **one expression**, so a newline in them can
      only be layout. `Parser::grouped` counts those, `Parser::wraps` steps over a
      newline when what follows continues the expression, and `Parser::source`
      clears the count — a block or a `$( … )` written inside a group is back to
      statements, where a newline separates again.

      The skip is **speculative**: the newlines go back unless the operator is
      really there, so an unclosed group still reports where it runs out instead of
      eating the lines after it. `${ … }` had the identical gap and the identical
      fix, and its own comment already claimed it wrapped "the way a `( … )` group
      does" — true of both, in three positions out of four.

      A `[ … ]` list is unchanged and deliberately differs: it holds several things,
      so `[1` / `2]` is two elements. The reference now says which bracket means
      which rather than leaving "line breaks continue the statement" to imply both.

      The rule is handed off in **one** place, `Parser::postfix`, rather than at
      each construct that separates items. Everything a wrapped expression can
      descend into is reached from there — the `[ … ]` and `{ … }` a `primary`
      opens, the `( … )` argument lists the postfix loop reads — and clearing at
      each was how `([1` / `+ 2])` came to be the one-element `[3]` while the
      identical bare list was a syntax error. A group written *inside* one of them
      turns wrapping back on, so `[1, (2` / `+ 3)]` is two elements with the second
      wrapped.
- [ ] **10. `"$var.suffix"` in a string is member access, not text.**
      `"$file.bak"` looks up `bak` in `$file` and fails with `value is not a map`
      — at **runtime**, so a rarely-taken branch carries it silently until it is
      taken. `"${file}.bak"` is the fix, and the brace is easy to forget precisely
      because every other shell makes it optional here.
- [x] **11. An argument-taking modifier cannot be interpolated.**
      `"$env:get(HOME, none)"` does not call the modifier. Walked into four
      separate times in one port, which is the usual sign that the diagnostic
      should name the rule.

      **Fixed, and the premise was wrong in mesh's favor:** the value does *not*
      have to be bound first. `"${env:get(HOME, none)}"` works — a braced body
      takes arguments. What failed was the bare `$…` form, which is scanned by its
      characters: the scan stops at the `(`, so the arguments stayed behind as
      literal text and the modifier ran with **none**.
      `"$env:get(HOME, none)"` therefore answered the whole environment and failed
      with ``$env: list value needs `...` ``, naming neither the mistake nor the
      fix.

      Now a syntax error that names both, reported where the scan is
      (`variable_access_prefix`) so a `"…"` string and a heredoc body agree.

      The braced form takes its head **sigil-less**, as the argument-free
      `${file:stem}` does, so adding an argument does not change how the head
      reads. That was not true when this landed — the message and the reference
      both said the `$` inside the braces was required, which
      "Take a modifier's arguments in a sigil-less `${…}`" (a17ab27) made false
      two commits later. Corrected since; `$` there is now optional rather than
      necessary, and the test asserts both spellings so the advice cannot drift
      from what works again.

      **Found alongside, since fixed:** the bare chain was read in *command*
      position only. `puts "$x:upper"` was `AB`, while `y = "$x:upper"` bound the
      literal `ab:upper` — the merge step that attaches a chain to a preceding
      reference ran in `command_word` and nowhere else, so the same string meant
      two things depending on where it sat and the value-position reading was
      silent. A bug against the **documented** behavior rather than a design
      choice: §Modifiers says they work in double-quoted interpolation without
      qualifying by position. Raised in review as a P2.

      **Fixed** by ending `word_run` — the value-position counterpart of
      `command_word` — with the same `merge_command_variable_access`, so the two
      paths that build a word agree on what a word is. Access folds too, not just
      modifiers: `"$m.a"` and `"$xs[1]"` in value position were the same silence.

      What it changes is bounded by what the *shape* claims — `"$h:$p"`, `"$h:2"`
      and `"$h:/path"` keep reading as text, since none of those is an identifier.
      An identifier after the colon is claimed whether or not it names a modifier
      (see "`:name` is reserved by shape" under Decisions made), so `"$h:nope"` is
      now reported rather than the text it used to be. A value string that changes
      meaning is one holding a `$name:identifier` chain — which command position
      already read that way, so the strings at risk are the ones that were only ever
      *written* in value position. Every case that moved went from a silent wrong
      answer to the right answer or a named error: `"$x:sort"` now says
      `not implemented yet` where it rendered `abc:sort`, `"$x:split(-)"` names the
      braced spelling where it rendered `a-b:split(-)`, and `"$n:upper"` on an
      integer says `requires a string` where it rendered `5:upper`. The whole
      existing suite passed unchanged.

      **Also found alongside, also not fixed: an interpolation drops a modifier
      `expand::Modifier::from_name` does not know.** `expansion_variable`
      (`repl.rs`) pushes the modifier only `if let Some(…)`, so `"$x:sort"`
      renders its subject unchanged with no error where the bare `$x:sort` says
      `modifier :sort is not implemented yet`, and `"$x:match(/a/)"` renders
      `abc(/a/)`. Raised in review as a P2.

      The tempting one-line fix — map the miss to "unimplemented" — is **wrong**,
      and two review rounds proved it: a miss does not mean unimplemented, only
      that `expand` is not where the name lives. The regex flags (`:i`, `:m`,
      `:s`, `:x` and their long spellings) and `:capture` are implemented in
      `repl::apply_argument_free_modifier`, on the other side of a layer the
      expansion path cannot reach, so the fallback reported `:i: is not
      implemented yet` for a flag that works and replaced `:capture`'s actionable
      ``applies to a call — write `f(…):capture` `` with a false one.

      The real fix is to stop having **two** modifier vocabularies: expansion
      knows `expand::Modifier`, while the expression path knows that plus the
      regex flags, `:capture`, and the argument-taking set. Unify them — one
      entry point that takes a name and a value and answers the same way
      wherever the chain was written — and the silent drop, the wrong messages,
      and the position-dependence above all close together.
- [x] **12. No whitespace-run tokenizer.** `:split(SEP)` takes a literal separator
      and keeps interior empties (`"a   b":split(" "):len` is 4), so every
      column-padded output — `getent`, `ip -o`, `df`, `stat` — needed a
      hand-written `fields()` helper that split and dropped empties. It was the
      most-copied helper in the port. **Fixed** by `:words`, the name `DESIGN.md`
      had specified for it all along; the config's helper is gone and its six
      call sites are a modifier chain.
- [x] **13. A function whose last statement was a `match` swallowed its own
      earlier stdout when called for a value.** `confirm("ok")` returned the right
      answer with no prompt printed. Fixed in 77dca06, "Stop `if` and `match`
      capturing a value block's stdout".
- [x] **14. `$sh.uid`.** The root check that picks the prompt's `#` / `$`
      glyph runs on every render, and bash, zsh and fish each answer it from their
      own `$UID` for free. Mesh now exposes the effective user id directly, captured
      with the shell's process identity and kept stable in a forked stage, so a
      prompt does not need to fork `id -u` or keep its own cache.
- [x] **15. `$sh.status` was cleared while an `if` condition was evaluated**, so it
      read `0` in *both* arms and a command used as a condition could not have its
      status inspected afterwards — `if sh -c "exit 3" { … } else { … }` reported
      `status=0` in the else branch. The cost was not the branch but the detail: a
      `130` from Ctrl-C flattened to a generic failure, so `trydiff`, `applydiff`
      and `isort` each ran their command as a plain statement and captured
      `$sh.status` before branching.

      **Fixed:** a command condition publishes its status like any other command,
      so the branch it picks reads the real code. The publishing happens in
      `condition_status`, which had been bypassing the `run_recorded` funnel that
      normally does it — a condition is not the statement's result, so it never
      passed through. A *value* condition is exempt: a bool is not a command and
      has no status to report, so it leaves the previous command's standing, as a
      skipped guard does. A pipeline condition keeps its per-stage breakdown.
- [x] **16. No path-resolving modifier.** `:type` reported `link` but nothing
      resolved one, so `realdir` shelled out to `readlink -f` — a fork for
      something the shell already has the syscall for.

      **Fixed** by `:real`, the name `DESIGN.md` had specified for it all along
      (§"Path components"). It resolves every symlink, `.` and `..` and answers
      an absolute path, and maps over a list like the other path modifiers. It
      **errors** on a path it cannot resolve rather than inventing one: the kernel
      has to be able to follow every component, so there is no partial answer to
      give — the same reason `:type` errors where the yes/no file tests answer
      `false`.
- [ ] **17. A value call scans an argument by its *runtime* value.** `f($word)`
      reports ``unknown flag `--sleep` `` when `$word` happens to hold
      `--sleep=0`, so data that merely looks like a flag cannot be passed to a
      plain `func` at all. This is 1 again, one level down: `wrapper func` fixed
      the *command* position, and a value call still has no equivalent.
- [ ] **18. A bare `...$list` in command position runs nothing.** `xs = [echo hi]`
      followed by `...$xs` produces no output and no error — the head has to be
      bound out and used as the command word. The condition half of this —
      `if ...$rest` taking the branch without invoking anything — was fixed in
      03c22a9, "Make a condition a bool or a command, with no truthy values", and
      is now a loud `a list is not a condition`; command position is what remains.
- [ ] **19. "Parse my own leading option, forward everything after" has no
      spelling.** A `wrapper func` may declare no flags at all —
      ``a `wrapper func` parses no flags, so it cannot declare `--times` `` — and a
      plain `func` scans a declared flag against the **whole** argument list, so
      `retry --times=2 curl --fail URL` is rejected for curl's `--fail`. `retry`,
      `body` and `recent` each read their own option off the front by hand, in all
      three spellings (`-N`, `--opt N`, `--opt=N`) — the shape bash gets from a
      `case $1 in` and a `shift`.
- [ ] **20. A bare `-2` argument arrives as the integer `-2`**, not the string
      that was typed, so string modifiers refuse it — `:len` answers
      `requires a string or collection` — and `~` errors outright, saying its
      left operand must be a string. `body`, `recent` and `shift-options` each
      classify a text copy and then forward the original argument.

      **Re-checked, and mostly working as designed.** `f 2` binding an integer is
      the documented rule — types come from the value, not the name — and `:len`
      refusing an integer is that rule being consistent rather than a gap. The
      workaround is also milder than this entry claims: `"$x"` gives the text
      back in one interpolation, and `"$x" ~ re("^-[0-9]+\$")` matches fine. No
      "classify a copy and forward the original" is needed.

      What is worth keeping is narrower. The typing is **not uniform across the
      shapes a reader would treat as one category**: `-2` is an integer but
      `-2.5` is a string, because there is no float type, so "a negative numeric
      option" is not one thing to test for. And `--2` is neither — it is
      ``unknown flag `--2` ``. A function taking `-N`-style options has to handle
      three readings of what its caller thinks is one.

      The real bug found while re-checking this is separate and worse — a
      function argument does not keep the text it was given. See "A numeric-looking
      argument loses its spelling" below.
- [ ] **21. `files` is a reserved value-call name, so the shortcut cannot be
      written.** `alias files = package files` is refused — `files` is a built-in
      value call and cannot be a function name — rather than shadowing, and `re`,
      `style`, `link`, `glob` and `dirs` are the same. The other three shells in
      that config all define this shortcut; mesh has no spelling for it, which
      makes it the one name the port had to drop rather than translate.

      *Narrowed by 27, not closed by it.* This used to be a **syntax error**,
      which cost the whole file the alias sat in; it is now a runtime error
      against that one definition, so the rest of a config survives it. What this
      entry is actually about — that there is no spelling for the shortcut — is
      untouched.
- [ ] **22. An alias cannot be tab-completed.** `co --` offers nothing:
      completion builds a spec from a function's generated help, which a wrapper
      leaves empty by design, and it cannot fall back to probing because the name
      is a function rather than a program on `PATH`. Since `alias` exists to make
      forwarding terse, every alias in a config is a name the shell can run and
      cannot complete. Related to the open `$sh.complete` item under "Beyond M3 —
      Interactive completion", though the likelier fix is that a wrapper's spec
      should be the spec of whatever it forwards to.
- [x] **23. `'…'` is not literal, which surprises on paste.** mesh processes
      escapes inside single quotes as well as double, so a pasted sed/awk/grep
      program is a *syntax error* (`invalid escape \(`) rather than a working
      command. `r'…'` is the right answer and works — the edge is that the failure
      arrives on paste, which is exactly when the reader is least likely to know
      the raw form exists, and that the diagnostic does not mention it.

      **Fixed, and the entry's second claim was wrong.** It said "a mesh string is
      also single-line, so a multi-line sed script still has to be split across
      `-e` expressions". Not so: a script file has always taken one, because the
      whole file is parsed as a unit. What was single-line was the **line-at-a-time
      reader** — piped stdin and interactive — which returned a hard error for an
      unclosed quote while buffering an unclosed `{`. So the two readers disagreed
      about the same source, and the disagreement bit exactly where paste happens.

      The tokenizer's "ran out of input" errors — an unclosed quote, `$(` or `${`
      — now reach the reader as `Incomplete`, the same signal an open brace gives,
      and the continuation prompt that already existed shows while it waits. At
      true end of input every caller still converts an incomplete parse back into
      its error, so a quote that never closes is reported rather than swallowed;
      what changes is *when*, and that the following line is string content.

      The diagnostic names the raw form now: ``invalid escape \(; for text holding
      its own backslashes (a sed or awk program, a Windows path) use a raw string,
      `r'…'` ``.

      **Only the quote characters continue.** A bare `${x` also ends the input,
      but nothing later can complete it: `variable_end`'s `valid_variable_access`
      rejects the newline, so buffering it consumed a following `}` into a
      reference that still could not parse, and with no `}` at all swallowed
      every command after it through EOF. An unclosed `$(` or `${` *inside* a
      string keeps its old hard error for the same reason — continuing it is a
      separate question from continuing the string around it. Both raised in
      review.
- [x] **Decide what Ctrl-D should do with pending input.** `Signal::CtrlD =>
      Some(Step::Exit(last))` (`repl.rs`) never looked at `pending`, so an
      interactive session with a half-typed construct exited silently with the
      *previous* status. Deliberate for a half-typed `func` — the comment said
      "abandoning any in-progress `func`" — and reedline only emits Ctrl-D on an
      empty editor line, so it read as "I mean to leave" rather than "tell me
      what is wrong". Raised in review against the entry above, which extends the
      same treatment to a half-typed string.

      What made it worth a decision rather than a shrug: the *other* readers
      disagree. Piped EOF, a script, and `-n` all convert an incomplete parse
      back into its error and exit 2. So the same unclosed quote was a reported
      syntax error through three doors and a silent exit 0 through the fourth —
      the exact class of reader disagreement edge 23 was about.

      **Resolved as neither of the two options the entry framed.** Reporting and
      exiting 2 makes Ctrl-D destroy the buffer to complain about it, and
      documenting abandonment blesses losing it. The rule that came out instead
      is about the *buffer*, not about constructs: **Ctrl-D exits only when the
      input buffer is completely empty, and does nothing at all otherwise** — no
      special case for a continuation line, for a `func`, for a heredoc, or for
      an unclosed string. It keeps the gestures distinct: Ctrl-D leaves, Ctrl-C
      discards. Press Ctrl-C then Ctrl-D and you get the old behavior,
      deliberately, in two keystrokes.

      Scope, since the rule is easy to overstate (and the first draft of these
      docs did): this governs only the *signal*, which reedline emits solely on
      an empty editor line. With characters on the line reedline never signals
      at all — it runs `EditCommand::Delete`, so Ctrl-D is `delete-char` there,
      as in bash, and at the end of a line it finds nothing to delete. Raised by
      Codex against wording that claimed Ctrl-D "does nothing at all" on a
      non-empty line; the behavior was right, the claim was not.

      The reader disagreement is still real but no longer a *silent* exit: the
      other three readers hit a genuine end of input, where there is no more to
      come, so converting the incomplete parse to an error is right for them.
      Ctrl-D at a prompt is not an end of input — the session is still there and
      the buffer is still in hand — so it now declines to answer for it.
      `repl.rs` `handle_signal`; `docs/REFERENCE.md` key table + paragraph;
      `DESIGN.md` signals.
- [ ] **24. No NUL-delimited read.** `gets` takes no delimiter and `"\0"` is
      `invalid escape`, so `find -print0 | while read -d ''` has no translation.
      `each0` delegates to `xargs -0` instead, which means it can only run
      **programs** where its sibling `each` can call a mesh function — the one
      capability gap between the pair.
- [x] **25. A value function's exit status cannot be reached, and `$(f)` around
      one captures nothing.** `y = $(v)` on a value function binds the empty
      string, so bash's `find_up x && …` has no mesh spelling; a falsy return
      value stands in, which is why `find-up` answers `""` on a miss rather than a
      status. `"$(f(x))"` inside a string silently yields nothing too. Related to
      the open *`$( … )` around a value-producing statement* item under "Loose
      ends".

      **Most of this was already stale.** The two-channel model `DESIGN.md`
      §"Result and `return`" records as *decided; shipped* really is shipped, so a
      value function's status is reachable four ways: `return false` reports `1`
      and `return true` / `return 5` / `return "s"` report `0`; `fail` and `fail
      123` report `1` and `123`; `f():capture` hands back a record with `.status`
      beside `.value`; and because only `false` fails, `find-up(x) && …` chains
      exactly as the bash idiom does.

      `$(f)` capturing nothing is **correct**, not a gap. `$( … )` is a *stdout*
      capture and a well-behaved value function does not print, so there is
      nothing there to take — the same reason `m = $(5 + 0)` binds `""`. Three
      spellings already read the value channel: `x = f()`, `"${f()}"` inside a
      string, and a bare `f()` as an argument. Nothing to build.

      **What was actually broken was the test, not the function.** An assignment
      condition over a value never asked about the value: a `Name` pattern fell
      through to the command path and reported the *assignment statement's*
      status, which is "the binding worked" and so always `0`. Every `if x = …`
      was true, whatever it bound. Only the list-pattern arm did a real test.

      Worse than a wrong branch, it broke termination — the `while gets line { …
      }` shape `DESIGN.md` pins its contract on:

      ```
      while n = nxt($n) { puts "n=$n" }
      n=1 / n=2 / n=3 / n=false
      mesh: comparison requires two integers or two strings
      ```

      Fixed by giving `condition_status` an arm of its own for a value
      right-hand side, ahead of the command fall-through: evaluate, and answer
      `1` when the value is `false` and `0` otherwise — the presence test
      `DESIGN.md` §"Empty `\"\"` / `[]` truthiness" already specifies, where
      `""`, `[]` and `0` are all results and only `false` is absent. Absent binds
      nothing, matching the two neighbors that already say so: a list-pattern
      mismatch "selects `else` without changing any bindings", and `gets` at end
      of input leaves `var` unchanged.

      Scoped by `capture_tail`, the same syntax-only test the assignment
      *statement* uses to pick between its own `0` and the capture's status, so
      `if out = $(diff a b)` keeps branching on the diff. And the presence test
      belongs to the **condition** only — a plain `x = false` statement still
      reports `0`, so a following `&&` is not silently skipped.
      `repl.rs` `condition_status`; `docs/REFERENCE.md` §Conditionals.
- [ ] **26. No `eval` and no dynamically-named `func`**, so "define one function
      per name in this list" — what a VCS-subcommand loop and an ssh-host alias
      loop both do — has to write a file and source it. That turns a private
      in-memory definition into **shared mutable state on disk**: the port had to
      handle a concurrent second shell, a partial write, a stale generation, an
      unreadable input, a directory sitting at the target path, and a 0600 file
      mode for definitions derived from an ssh config — none of which `eval` has.
      A `func` bound to a computed name, or a way to define into the function
      table from a value, would retire all of it.
- [x] **27. Three separate rules decide what can be a function name, and only one
      of them is a runtime error.** A leading underscore is a syntax error
      (`expected a name`, so `_exit` became `safe-exit`), a dot is a syntax error
      (recorded in `DESIGN.md`, mikelward/mesh#293), a built-in value call is a
      third (``syntax error: `files` is a built-in value call``), and a reserved
      word is the milder runtime one (``func: `puts` is a reserved name``).
      Because the first three are *parse* errors, one bad name in a generated file
      (26) costs **every** definition in it — which is why the ssh-host generator
      filters names through `type -t` before emitting rather than emitting and
      hoping. The underscore rule is the icebox item *Reserve only bare `_` as
      discard, allow `_name`* reached from a second direction.

      **One rule was already gone.** `_exit` defines and calls fine on `main`: the
      icebox item landed, so a leading underscore is a name and only the bare `_`
      — the discard — is refused. The entry above is the state at the port, kept
      for the record; `safe-exit` is no longer the workaround it was.

      **Fixed, the rest.** `func` and `alias` now read the name **without judging
      it** and hand the text to one runtime check, so every rule reports the same
      way, at the same moment, with the same blast radius — the definition itself
      and nothing else. Reading it unjudged means gluing back what the lexer
      splits (`a.b` arrives as `a` `.` `b`), which is also what lets `alias a.b =
      …` reach the check instead of answering `command not found: alias`. The four
      reasons, one message each:

      | Name | Reported as |
      | --- | --- |
      | `puts` | `` `puts` is a reserved name and cannot be a function name`` |
      | `files` | `` `files` is a built-in value call and cannot be a function name`` |
      | `a.b` | `` `a.b` cannot be a function name: a `.` reads as member access …`` |
      | `_` | `` `_` is the discard name and cannot be a function name`` |
      | `2x` | `` `2x` is not a name: a name starts with a letter or `_` …`` |

      A generated file is no longer all-or-nothing, which is the cost this entry
      was really about: the bad definition reports and every other one in the file
      still defines. So the ssh-host generator can emit and let the shell say,
      rather than filtering through `type -t` first. `a.b` also gained a message
      that names the problem — it used to answer ``expected `(` ``, pointing at the
      dot without saying what was wrong with it.

      **What did not change:** which names are refused. A dotted name is still not
      definable — `DESIGN.md` §"Functions" defers that on its own merits (the
      question is value-call position against member access), and this entry was
      about the *shape* of the refusal, not its content. A quoted `func "files"()`
      is still the parse-time `expected a name`, because that is a rule about a
      name being a name rather than about which names are taken.
- [x] **28. A failing capture did not bind the name, and execution continued.**
      The single most costly edge in the port. `x = $(sh -c "echo x; exit 3")`
      left `x` **unbound** and discarded the output, so the next `$x` failed with
      `unbound variable` — and then carried on, so the symptom was a confusing
      second error rather than a stop. In a `for` head it silently iterated
      nothing, with no error at all.

      **Fixed:** a capture now yields its bytes whatever the command exited with,
      and an assignment takes its right-hand side's capture status, so
      `if out = $(diff old new) { … } else { puts $out }` reads as it does in
      bash — the status picks the branch and the output is there on the one that
      needs it. Discarding was never right: a nonzero exit is routinely a
      *result*, not an error (`diff` exits 1 with the diff on stdout, `timeout`
      124 over what it printed first), so the bytes thrown away were the answer.
      With several captures in one right-hand side the last decides, as in bash.
      `capture-or-empty` is no longer needed for this.
- [x] **29. `source` reports failure only through the status, and a file's status
      is its last statement's.** So a sequence of sources has to be gated by hand
      — an `env.mesh` that stopped parsing followed by an `rc.mesh` that still
      does would report success over a shell holding the old `PATH` — *and* a
      failure nested inside a sourced file has to be reported where it happens,
      because by the time control returns every later statement has overwritten
      the evidence. Both local-override files in the config carry a report at the
      point of failure for that reason.

      **Fixed:** a sourced file now reports **that it broke**, whatever it went on
      to end with. The distinction is one mesh already draws internally and had
      never surfaced: a command's nonzero *status* is a **result** — `grep` finding
      nothing, `diff` finding a difference — and stays exactly as it was, while an
      unhandled **evaluation error** (a syntax error, an unbound variable, a bad
      definition name) is a **breakage**, and the first one a file raises becomes
      that `source`'s status even if forty later lines succeed. So
      `source local-env.mesh && source local-rc.mesh` now does the gating those
      files were reporting by hand.

      **Handled means handled.** Only errors that reach the file's own top level
      count, so `f || fallback` — including an `f` that broke several frames down —
      leaves nothing behind and the file reports success. Without that, `||` inside
      a config would be unusable.

      **The startup set keeps the same promise.** Its status was the *last* file's,
      so a `login.mesh`/`rc.mesh` that ran fine reported success over an `env.mesh`
      that gave up before it finished setting `PATH` — the case this entry names.
      The first file that broke is now what the set reports, and it is published,
      so `$sh.status` at the first prompt says so. Later files still **run** after
      an earlier one fails, which was never the complaint.

      **Not changed:** a script's own statements. `mesh script.mesh` still exits on
      its last statement's status, as bash does; this is about what a *caller*
      learns from `source`, which had no other channel.
- [x] **30. No force-interactive flag.** `mesh -i -s` was `unknown option -i`.
      The port added what it cost a config: nothing past
      `return unless $sh.interactive` could be exercised without a pty, so
      everything with behavior worth asserting had to be a named function defined
      *above* that line, with the interactive section reduced to calling them.

      **Fixed:** `-i` makes the session interactive whatever its input is —
      `$sh.interactive` is true and `rc.mesh` joins the startup set — while the
      invocation still decides where the commands come from, which is the
      orthogonality `DESIGN.md` §Invocation asks for (`mesh -i script.mesh` is a
      script *and* interactive). It does not conjure a terminal: without one there
      is nothing to run a line editor on, so `mesh -i` off a terminal reads stdin
      as it always did, just as an interactive session. Nothing a prompt would
      decorate with leaks into a piped run, so output stays byte-exact.

## Loose ends

Small items rescued from pull requests that were closed as superseded — the bulk
of each PR had landed by another route, but these pieces had not.

- [ ] **Control flow raised inside a deferred stage never reaches the parent.**
      A stage carrying a value expands in its **own fork** — that is what keeps a
      call's writes out of the shell and stops `cmd $(sleep 2) &` holding the
      prompt — but a `break`, `continue` or `return` raised while expanding there
      dies with the fork:

      ```mesh
      for i in [1 2] { puts (if true { break }) | cat
        puts AFTER }        # AFTER twice; unpiped, the loop exits
      ```

      Not new, and not about the `NAME=value` prefix, though that is where it was
      noticed: a prefix now defers exactly as a word does, so the two behave
      alike, and `loop_control_in_a_prefix_stops_the_stage` asserts them side by
      side so they cannot drift apart. The stage does not *launch* either way —
      what is lost is only the effect on the enclosing loop or function.

      Fixing it means a forked stage reporting its control outcome back, which is
      the same missing channel the piped `gets` line count needs (rough edge 6).
      Worth doing once, for all of them — because there is a third.

      **An evaluation *error* raised in a fork is lost the same way**, and rough
      edge 29 gave it teeth: a sourced file now reports a breakage its statements
      never answered for, and that record is process-local, so `fork { $nope }`,
      a background job, and a stage that really must defer all leave the parent
      with a *status* where an error happened. A status is a result — that is the
      whole distinction 29 turns on — so the file reads as fine. What crosses the
      fork today is one exit code, and no code can mean "this was invalid input"
      without stealing it from the programs that already use it.

      Narrowed rather than fixed, in the commit that closed 29: a prefix whose
      value is a bare `$x` runs no code, so it is read in the parent like the
      quoted `"$x"` beside it instead of deferring — `A=$nope true | cat` reports
      now (`env-prefix-in-a-stage`). That removes the case a reviewer hit and an
      inconsistency between two spellings of the same thing; it does not touch the
      general hole, which needs the channel above.

- [ ] **Consider making a bare `FOO=bar` an environment assignment, and `$FOO`
      an environment reference.** Raised when the `NAME=value cmd` prefix landed,
      because the prefix creates a deliberate collision: `FOO=bar cmd` writes the
      **environment**, since what a child inherits is the entire point, while
      `FOO=bar` alone binds a **shell** variable that no child ever sees. Same
      spelling, two namespaces, told apart only by whether a command follows.

      That was accepted knowingly — bash has one namespace and the prefix has to
      mean the environment to be worth having — but it is worth asking whether
      the collision should be removed from the other side instead: let a bare
      `FOO=bar` mean `$env.FOO = bar` and `$FOO` mean `$env.FOO`, so the two
      spellings agree.

      Against it, and why it is a question rather than a plan:

      - It would give mesh two ways to say the same thing (`FOO=bar` and
        `$env.FOO = bar`) where `DESIGN.md` keeps shell bindings and the
        environment deliberately separate.
      - Every child would inherit anything a script assigns in passing, which is
        the leak the separation exists to prevent.
      - `$FOO` reading the environment collides with local bindings — a function's
        `PATH = …` local shadow would stop being local.
      - Types: `x = 1` binds an integer, and only strings cross into the
        environment, so the two spellings would not accept the same values.

      Worth deciding together with `export`, which is already the ingrained
      spelling for exactly this, and with `x=1 y=2` — two bindings on one line,
      which bash reads as two assignments and mesh reports as a missing
      separator.

- [ ] **A numeric-looking argument loses its spelling through a mesh binding.**
      Found while re-checking rough edge 20, which is about something else. A
      word that parses as a decimal integer is bound as one and re-rendered from
      the number, so the text the caller wrote does not survive:

      ```
      $ mesh -c 'wrapper func w(...rest) { printf "[%s]\n" ...$rest }
                 w 007'
      [7]
      $ mesh -c 'printf "[%s]\n" 007'
      [007]
      ```

      `007` → `7`, `08` → `8`, `+5` → `5`, `-0` → `0`. `1_0`, `0x10` and `1e3`
      are unaffected, since they stay strings.

      What makes it a bug rather than the type rule being consistent is **where
      it does not happen**. A direct external argument keeps its spelling, and so
      does `$sh.args` — a script run with `007 +5 -2 08` sees all four as
      written. It is `func` parameters, `...rest`, `alias`, and `x = 007` that
      lose it. So putting a mesh function in front of a command changes what the
      command receives, silently, which is the one thing a wrapper must not do —
      and that config is 130 aliases and 53 wrappers, every one of them a place
      this can bite.

      `chmod 0755` survives it by luck, because chmod re-parses octal either way.
      A zero-padded identifier, a version segment, or anything else where the
      digits are text does not.

      Worth deciding as one question with `$sh.args`, which already keeps the
      spelling: either an argument is text until something asks it to be a
      number, or the integer binding keeps the source text alongside the value.
      The second is what makes `$x + 1` and `"$x"` both answer correctly.

- [ ] **Decide whether a non-interactive shell should start process groups for
      its pipelines.** `run_pipeline` takes its job-control decision from
      `shell_stdin_is_terminal()` (`exec.rs`) — an `isatty` on the shell's saved
      stdin — rather than from anything about the session. So `mesh -c 'sleep 30'`
      launched from a terminal puts the child in a **group of its own**, though
      that shell never acquired the terminal: it did not take the foreground group
      and did not configure the terminal signals.

      The consequence is that a `SIGINT` sent to the invocation's process group
      reaches mesh but *misses* the child, killing the shell and orphaning what it
      launched. Measured under a pty, printing the child's group: the shell's own
      is `26730`, and both `mesh -c` and `mesh -i -c` answer a fresh group
      (`26737`, `26742`).

      The narrower case — a `fork` block — was fixed by asking
      `Vars::owns_terminal` instead of `Vars::interactive` (mikelward/mesh#307),
      which is the shape the answer here probably takes too. It is separate work
      because it changes behavior for **every** batch invocation on a terminal,
      not just a forced-interactive one, and because "should a script's children
      be interruptible as a unit" is a question worth answering deliberately
      rather than as a side effect. Raised by review on that PR.

- [x] **Two pty tests fail under load.** The one the harness caused is fixed: it
      typed the next line while the shell was still finishing the previous
      command.

      `decoration_settings_harness` synchronized on a file its command created,
      because with `shell-integration` off there is no `D` mark to wait for. But
      `touch` creates the file and *then* exits, so when the path appeared the
      shell was still mid-command — the terminal back in cooked mode, reedline
      not yet re-entered. The line the harness wrote next was echoed by the line
      discipline and read back by reedline as it started, which the session
      transcript shows plainly: the typed text arrives between the `?2004l` that
      ends one read and the `?2004h` that begins the next, rather than in a
      repaint. Whether that survives is timing, and phase 123 —
      `wait_for_path(&restored)` giving up after 30 seconds — is the losing side.

      Two mechanisms, both removed:

      - **Type only when the editor is reading.** Reedline turns bracketed paste
        off as it leaves a read and on as it enters the next, whatever the marks
        under test are set to, so `?2004l` then `?2004h` is the prompt-ready
        signal a session with its marks off still has.
        `pty_read_until_the_prompt_returns` waits for that pair and hands back
        what it read; the file stays as proof the command *ran*.
      - **Never leave the pty unread.** The harness had deliberately left two
        commands' output unread so both would be in the buffer it examined at the
        end. Meanwhile the cursor-position query reedline writes at each prompt
        went unanswered until it timed out — after which the reply the next
        reader sent arrived with nothing waiting for it and was taken as typed
        input. Reading throughout and joining the windows gets the same bytes
        without the stall.

      `pty_read_until_one_of` also answered *every* `ESC[6n` in its accumulated
      buffer on every read — three replies to one query in an idle run, 26 when
      reads are small — the mistake its sibling's comment already warned about.
      One reply per query now, from `answer_cursor_queries`.

      Reading a failure: each harness returns a distinct code per phase —
      `abandoned_line_harness` 90–96, `decoration_settings_harness` 110–132
      (renumbered; 123 is now "the prompt did not come back after `touch`").
      `await_pty_harness` prints that code directly, so a failure reads
      `PTY harness failed at phase 123` rather than the encoded wait status
      `0x7b00` that reached this file.

      Reproducing it needed no load in the end. With only the cursor-query fix
      applied, phase 123 failed on the *first* full run of `cli` on an idle
      machine, in 55s against the usual 30s — the same slow-run signature as
      both sightings above. The spurious replies had been masking the unread
      window: they left one sitting in the input queue for reedline's next query
      to consume, so cutting them to one per query is what let the shell stall
      where it had only been slow. Both fixes, in that order, is why the two
      land together.

      `an_abandoned_line_is_closed_without_a_status` is a weaker claim. It went
      through the same `pty_read_until_one_of`, via `start_pty_shell`, so the
      over-answering reached it too — but its failure was never reproduced, so
      what fixed it is inference rather than a measurement. Worth watching in
      CI.

      To reproduce under contention, `taskset -c 0 cargo test --workspace` is
      the sharpest form, and is what surfaced the three below.

- [ ] **The pty suite is flaky in CI, in more than one place.** Six distinct
      failures now, and the pattern that matters is that they are *different
      tests each time* — so this is one property of the harness rather than six
      bugs, and it will keep blocking merges until it is taken as its own piece
      of work.

      Seen on a single CPU (`taskset -c 0 cargo test --workspace`, which is the
      sharpest reproduction):

      - ~~`a_jobdone_hook_fires_where_the_done_notice_prints`, phase 155~~ —
        **fixed**. Both jobs are gated on files now rather than staggered by
        sleeps, so the order the case needs is stated instead of raced: the
        first job waits for a gate the harness opens once the shell is idle at
        its prompt, and the second waits for one the handler opens before it
        sleeps. Six full-suite runs pinned to one CPU, all green, against four
        failures in six before.

        Both waits end on the job being **reapable**, not on it having begun to
        leave. End of file is the second of those, not the first: the kernel
        closes a process's descriptors in `exit_files` and publishes its wait
        status later in `exit_notify`, and between the two the shell cannot yet
        reap it. Probed over 5000 runs that gap was open about 1% of the time,
        and *flat* whether the reader did nothing afterwards or five thousand
        syscalls' worth — a child preempted mid-teardown is waiting for a
        scheduling slot, not for work to finish, so waiting longer does not
        help. `/proc/<pid>/stat` reporting state `Z` is the answer, and where
        there is no `/proc` the fifo answers alone.
      - `the_title_setting_turns_the_title_off_and_back_on`, phase 140.
      - `vs_code_gets_its_own_dialect_and_the_command_line`, phase 170.

      The CI sightings below are all fixed: the decorations one by the
      type-ahead fix, `new_foreground_job_does_not_receive_sigcont` by spelling
      the teardown `exit 0`, and the notify one by answering the terminal while
      the session leaves (mikelward/mesh#321) — which was the whole family's
      clearest statement. Kept as the record of what each turned out to be.

      Seen in **CI**, on a two-core runner at ordinary speed, each once and each
      a different test — mikelward/mesh#318, across three pushes:

      - `settings_turn_the_interactive_decorations_off_and_back_on`, phase 110 —
        `start_pty_shell` giving up. The same phase the pinned runs produced, so
        it is not only a starved machine.
      - `new_foreground_job_does_not_receive_sigcont`, phase 27 — `exit` written,
        and the shell did not leave cleanly.
      - `notify_reaches_the_terminal_and_a_quick_command_does_not`.

      The job-done one had the only cause already named — a 400ms sleep
      expecting a `sleep 0.2` job to have ended and a `sleep 0.7` job not to
      have, which on a loaded machine is not a safe bet — and it is fixed.

      **`start_pty_shell` giving up is the one thing left**, and it now reports
      what it saw. The answer is `got 0 bytes` with the shell still running:
      nothing written at all, so it is a session that has not been scheduled far
      enough to speak rather than one that stopped mid-paint. It survives a full
      30-second budget, and at the oversubscription that reproduces it — 24 test
      threads pinned to one CPU — unrelated non-pty cases
      (`a_line_gets_consumes_counts_toward_later_locations`) fail in half the
      runs too, which is the signal that the machine is what is being measured.
      At eight threads on one CPU, the level that surfaced every other flake
      here, it is green.

      So what is unexplained is narrow: the same failure was seen **once in real
      CI**, on a two-core runner at ordinary speed, and starvation is a poor fit
      for that. Left open for the next occurrence, which will say which of the
      two it was.

      The phase codes now name themselves (`await_pty_harness`, and
      `pty_start_failed` for the six ways a session fails to start), so the next
      occurrences should be cheaper to read than these were.

      **Two more were seen in CI rather than under `taskset`. Both are fixed**,
      and they are kept here because of how they turned out rather than because
      anything is outstanding — one bug, found late, wearing two phase numbers.

      Note what this does *not* close: the same notify test has a separate
      still-open entry below at harness code **150**, a
      `pty_read_until_command_done` timeout after `on --remove`. That is a
      different failure of the same test, and nothing here touches it.

      `notify_reaches_the_terminal_and_a_quick_command_does_not` (phase 163) —
      **fixed** in 036a7f5, and it was not load at all. `stop_pty_shell` wrote
      `exit 0` and then blocked in `waitpid` with nobody reading the pty, so the
      farewell prompt's cursor-position query went unanswered, reedline gave up
      with `line editor error: The cursor position could not be read within a
      normal duration`, and mesh left with **1** — an `exit 0` that named its own
      status not having it. The harness reads to end of file before it waits now.

      Two things that hid it, both worth remembering. It read as flaky because it
      failed on unrelated branches — a modifier diagnostic, a `with` block —
      neither of which can reach the notify path, which is exactly the shape that
      argues "not this branch's fault, therefore load". And the harness's own
      explanation was being thrown away: `pty_start_failed` / `pty_stop_failed`
      run in the **forked** child, where the test runner's thread-local stderr
      sink is a copy that `_exit` discards, so `PTY harness failed at phase 163`
      arrived with nothing beside it. 296b98a writes to descriptor 2 directly.
      A phase number with no line under it is what made a diagnosable failure
      look like weather.

      `spawn_failure_returns_terminal_to_interactive_shell` (phase 38) **shared
      the same cause and is fixed with it**, and the next sighting is what said so — it arrived on the
      docs-only branch that first wrote this paragraph, as phase 27 of
      `new_foreground_job_does_not_receive_sigcont`. Both phases are the
      `WEXITSTATUS(status) != 0` check in `spawn_failure_harness` and
      `sigcont_harness`, the only two harnesses left writing `exit 0` and then
      waiting with nobody reading the pty. Their own comment named them —
      "these two harnesses predate it" — while explaining the `exit 0` spelling
      they had copied without the drain that makes it work. Both drain now.

      Worth stating because it changes what the word covers: **four of the five
      teardown failures recorded here were one bug wearing different phase
      numbers**, across
      different tests, on unrelated branches. What made them look like weather
      was that each sighting was a phase code with no line under it, on a diff
      that plainly could not have caused it. Neither observation was wrong;
      together they suggested load, and load was never it.

      Not reproduced locally — not under `taskset -c 0` per test, and not with a
      full single-CPU suite run. The argument is structural: the same pattern was
      proven to cause exactly this in `stop_pty_shell` (036a7f5), only these two
      still had it, and the two phase numbers CI reported are precisely their
      status checks. CI is the test.

- [ ] **If the syntactic capture-status rule proves too narrow, carry the status
      as evaluation metadata instead.** An assignment takes its right-hand side's
      capture status only when that side syntactically *is* a capture
      (`capture_tail` in `repl.rs`), which is what makes the rule leak-proof: there
      is no shared state, so nothing can smuggle a status out of an expression that
      merely ran a capture along the way.

      The first attempt did use shared state — an `Option<u8>` on `Shell`, cleared
      and consumed by whoever needed it — and review found **five** separate escape
      routes for it in a row: a callee's body, `$env.K =` / `$m.k =`, a capture
      interpolated into a command, a list-pattern condition, and an argument to a
      command-form `:capture`. Each fix was another clear site, and the next probe
      found the next one. That is the record worth keeping: the mechanism was the
      bug, not the sites.

      The alternative, if the syntactic rule ever needs to answer for an expression
      it currently reports `0` for, is to thread the status through `eval_expr` as
      part of the evaluation result (`{ value, capture }`) so it follows the value
      rather than the shell. That is the general answer and it costs ~46 call sites;
      the syntactic rule is one function and covers every case anyone has wanted so
      far. Do not go back to a slot on `Shell` — that is the shape that failed.

- [x] **A value expression can be a command argument.** `puts (1 + 2)`, `puts $(pwd)`,
      `ls $(pwd)`, `puts style(x, fg: red)`, `puts f()` and `puts pwd():capture` all
      work; `DESIGN.md` wrote the first two in its own examples. `parser::command` gains
      a `CommandItem::Value` when `value_argument_starts` sees a shape with no word
      spelling — `$(`, `(`, an attached `name(`/`$f(`, an attached `:name(` — which is
      why nothing could break: every one of them was a syntax error. `[` and `..` are
      deliberately **not** in that set, since in an argument they are already a glob
      character class and a literal word.

      The value rides into expansion as `Piece::Value`, evaluated in
      `run_ast_pipeline` where the shell is (a `$(…)` launches a command, a call runs a
      function) and literal thereafter — never re-split, never re-globbed, exactly as
      an interpolated variable is. It carries the **value**, not its text, so
      `whole_value` hands it over typed: `puts f()` on a list renders per line and
      `puts style(…)` keeps its attributes, while argv gets `value_argument_text` and
      the same loud refusals a bare list already meets.
- [x] **A stage evaluates its own value arguments, in its fork.** Raised in review on
      the value-argument PR. Value arguments used to run while the stage was assembled
      in `run_ast_pipeline`; a backgrounded or piped stage runs in a *fork* that had
      not happened yet, so the work and any side effects landed in the parent —
      `puts change() | cat` left `n=MUTATED` where `docs/REFERENCE.md` promises the
      fork keeps it, and `puts $(sleep 10) &` was refused outright because it would
      have spent the ten seconds at the prompt.

      A stage whose words carry a value now travels **unexpanded** to its own process
      as `StageBody::Deferred`, and `run_stage_in_shell` expands it there. What the
      words come to is also what decides, that late, how the stage ends: a function
      call, a builtin, or this process replaced by a program through the new
      `exec::exec_stage`. `exec::Cmd::in_shell` is true for every deferred stage,
      which is what keeps the parent from building a `Program` for argv it does not
      have yet.

      Two consequences worth knowing:

      - A job listing shows an unevaluated value as `$(…)` (`repl::display_words`),
        since printing it properly would mean running it in the shell — the very thing
        being deferred.
      - A deferred stage does not refresh the job table unless its command word is
        literally `jobs` (`repl::run_stages`). Treating it like a *function* stage —
        conservatively, since what it runs is equally unknowable — reaped for every
        `puts $(x) | cat` as well, and a reap removes finished jobs: a later
        `wait $j` reported "no current job" instead of the status. Also from review.
      - A refusal that depends on what the words come to now arrives from the stage.
        `c = return` then `$c 7 &` is refused at the prompt with status 2; `$c $(x) &`
        starts a job that reports the same message and exits 2, which `wait` gives
        back. That is the shape every other failing background command already has
        (`nosuchcmd &` starts and reports 127), so it stays.

      Reporting an unknown command does **not** change, which is worth writing down
      because it looks like it should: `Program::new` only converts argv to
      `CString`s, so the PATH lookup was always `execvp`'s, in the stage's own
      process. `nosuchcmd $(x) 2> log` and `nosuchcmd foo 2> log` both put the message
      in `log` and report 127, piped or not, before this change and after.

      A stage that **redirects** does not defer (`repl::can_defer`), and backgrounding
      a value in one stays refused (`repl::carries_a_value`). The shell resolves every
      stage's targets before it forks any of them, concurrently across stages, which is
      what keeps `cat < fifo | cmd > fifo` from deadlocking — so deferring the words
      while the targets stayed behind would expand the targets *first*, reversing the
      documented order. Raised in review, where it had done exactly that: a failing
      target stopped the words from running, and `f * $(x) > summary | cat` globbed the
      `summary` its own redirection had just created.

- [x] **A value in a word is evaluated when that word is expanded.** Raised in review
      on the PR above. Value arguments used to run while the stage was assembled in
      `run_ast_pipeline`, with the words expanded later as a batch, so a value that
      mutated shell state was observed by words written *earlier* on the line:

      ```
      cmd = /bin/echo
      func g() { global cmd = /bin/false; return x }
      $cmd g()            # ran /bin/false, not the /bin/echo that was selected first
      ```

      A `Stage` now carries its words **parsed**, and `repl::expand_stage` walks them
      one at a time — evaluating each word's values as it reaches that word, then
      expanding it — so `puts $n g() $n` reads `first x second`. Word zero is expanded
      once and first, since it decides how every other word expands and a value in it
      (`"$(pick)" arg`) must not run again for each question asked.

      A value argument became a *word* with a value piece in it on the way, which is
      what let one code path replace three: `expand_stage` is now the single expansion
      for `run_command`, `run_single` and `run_multi`, and the whole-command
      `typed_builtin_words` / `output_builtin_words` / `job_builtin_words` collapsed
      into the per-word `stage_argument`.

      Redirect targets follow the same rule one level down: they stay parsed too, and
      `expand_redirs` evaluates each as it reaches it — after **all** the words, which
      is what keeps `f * > summary` from globbing the file the redirection is about to
      create. Evaluated at assembly time they ran *before* every word, so
      `puts $n > "$(g)"` wrote what `g` had just assigned.
- [x] **`$( … )` around a value-producing statement fails with the value as a status.**
      Pre-existing, and surfaced by review on the value-argument PR. The capture reads
      the inner statement's status, and an expression statement's status is derived from
      its *value*, so any non-zero one is read as a failure:

      ```
      m = $(0 + 0)      # status 0, m is ''
      m = $(5 + 0)      # status 5, and `m` is never bound
      ```

      So `$(5 + 0)` is unusable while `$(0 + 0)` works, which is the value/status
      confusion `DESIGN.md` §"Result and `return`" otherwise keeps apart —
      `capture_source` wants "did the body fail", and `status_of` on a value is not that
      question. Nothing to do with argument position: `m = $(5 + 0)` has always done
      this. It shows up more now only because the form is reachable in more places.

      **Gone.** Re-measured while closing edge 25: both lines now leave status `0`
      and bind `m`, so no value is read as a failure any more. What is left is not
      a bug — `$( … )` is a *stdout* capture, an expression statement prints
      nothing, so `m` is `""` in both cases. Reading the value channel is `x = 5 +
      0`, or `"${f()}"` in a string. Closed on measurement rather than on a fix
      in this branch; the status half went with whatever settled `status_of`.
- [ ] **Text glued to a *bare* value argument.** `pre$(x)post` and `f()x` are a loud
      syntax error pointing at `"pre$(…)post"`, the quoted spelling that works, because
      handing over three arguments where one was written would be silently wrong. The
      value piece the quoted form needs now exists (`parser::WordPiece::Value`), so what
      is left is parser-side: `command_word` gluing an adjacent `$(` into the word it
      touches, and the value-argument branch doing the same for text that follows.

- [x] **`$(…)` interpolates inside `"…"`.** `puts "at $(pwd) now"` substitutes, and
      `DESIGN.md`'s prompt segment `func host-info() { style("$(hostname)", fg: red) }`
      styles the host name rather than the *string* `$(hostname)`. The piece scanner
      turns `$(` inside a double-quoted string into a `WordPiece::Value`, whose extent
      is found by lexing the body — bounded at the `)` that closes it, since scanning
      characters would close `"$(puts "a)b")"` on the wrong one — and whose body is
      parsed there and then, so a syntax error inside stays a parse error.

      `repl::expansion_word` evaluates the piece, which is what makes it shell-aware
      (a `$(…)` launches a command); the value rides into expansion as the same
      `Piece::Value` a value *argument* uses, so it is literal thereafter — never
      re-split, never re-globbed. Only `"…"`: `'…'`, `r"…"` and `\$(` stay text.

- [ ] **A capture does not interpolate in a heredoc body.** `<< END` interpolates
      `$var` and the `"…"` escape set, but `$(cmd)` stays as written — the body is
      interpolated from its *text* by `repl::interpolate_heredoc`, which resolves references
      against `Vars` and has no shell, rather than by the word machinery a string
      goes through. `docs/REFERENCE.md` §"Heredocs" says so now. Worth closing for
      consistency, and it wants the same shell-aware treatment `expansion_word` got.

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
- [ ] **Two modifier tables, one of them quietly stale.** *(Stale as written:
      `lexer.rs` no longer exists, so `lexer::Modifier` is gone. The live pair is
      now `parser::MODIFIER_NAMES` and `expand::Modifier`, and the reservation
      change raised the stakes — `MODIFIER_NAMES` decides a **syntax error**, so a
      name implemented but missing from it would be refused outright. Checked: no
      implemented modifier is missing from it today. Re-scope or close.)*
      `lexer::Modifier`
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
- [x] **The parser has no recursion-depth limit.** Deeply nested input aborted the
      whole shell with `thread 'main' has overflowed its stack` instead of reporting
      a syntax error — a dead shell from a paste. Fixed by a counter shared between
      the parser and the lexer, reporting `nested too deeply` past 100 levels. The
      counter has to sit at each place the grammar *descends*, not on the way in:
      several shapes (`else if` chains, `-` / `...` prefix chains, and the trailer
      loop's call arguments, index expressions and modifier arguments) recurse only
      after `primary` has given its level back, and each bypassed the guard until
      counted where it descends. Counting on entry instead would charge every
      operand a level and halve what the limit means. The
      `not` chain the entry originally named turned out to be iterative already and
      parses at 20000 deep. Measured per shape on a debug build, the most expensive
      (a chain of `$( … )` captures) overflows at 253 levels on the usual 8 MiB, so
      100 leaves room to spare on the stack a shell actually starts with.
- [x] **A stack overflow aborts instead of reporting.** `crates/mesh-core/src/stack.rs`
      now catches `SIGSEGV`/`SIGBUS` on an alternate stack and exits 70 with
      `mesh: fatal: out of stack …`, where before the process died on Rust's
      `has overflowed its stack` abort. This is the net under the limit above, not
      a replacement for it: it covers the cases a parse-time counter cannot see —
      the evaluator below, and a stack so small (`ulimit -s 512`) that the parser's
      own ceiling falls under `MAX_DEPTH`. Deliberately not a *recovery*: the shell
      still exits, since unwinding out of a signal handler is not safe.
- [ ] **Evaluating a long operator chain still overflows the stack.** The parser's
      depth limit does not cover this one, and the distinction is worth keeping
      straight: `1 + 1 + …` parses *iteratively* into a left-leaning `Expr::Binary`
      spine, so nothing is deeply nested at parse time — it is `eval_expr` walking
      that spine that recurses. Around 1000 terms is enough:

      ```
      x = 1 + 1 + … x1000 …           → mesh: fatal: out of stack (was an abort)
      x = 1 * 1 * … x1000 …           → mesh: fatal: out of stack (was an abort)
      if false { x = 1 + … x20000 … } → fine, because it is never evaluated
      ```

      That last line is the proof it is evaluation and not parsing or `Drop`. The
      fault handler makes this legible but it still ends the shell, which for an
      interactive session is not good enough. Two ways out: a depth counter in
      `eval_expr` raising a runtime error, or making the walk over a binary spine
      iterative so a chain of any length just works. The second is the better answer
      for `+` specifically — the spine is left leaning, so it unrolls into a loop —
      but a counter is what covers the general case, since a deep tree can be built
      by nesting rather than chaining.
- [x] **Two of three flaky tests fixed; one still unexplained.** All three were
      pre-existing and timing-dependent. Both fixes attack the *dependence on
      timing*, not the deadline — a bigger number would have moved the failure
      rate without changing what the test rests on.
  - [x] `ctrl_c_cancels_an_interactive_gets` — failed around 10% of the time, on
        `main` as much as on a branch. The cause was in the test, and the comment
        asserting otherwise was wrong: waiting for `puts BLOCKING` proved the
        command *before* the read had ended, which is the near side of a gap, not
        the far one. An interactive shell ignores SIGINT except where it has armed
        itself to catch it, so a keystroke landing between the two commands is
        discarded by design and `gets` then blocks forever — the 11s failures were
        `QUIET` expiring on a shell that was never going to answer. Nothing the
        shell writes marks the moment a read begins, so the keystroke now repeats
        until it lands (`pty_interrupt_until_command_done`). What repeats is the
        stimulus; the assertions on status and variable are untouched, and a shell
        that ignored a properly-blocked Ctrl-C still fails on the deadline.
        0 failures in 120 runs, from ~10%.
  - [x] `separated_typed_completion_probes_the_option_context_first` — the `--help`
        probe's 2-second budget, confirmed by slowing the fake tool to 3s and
        reproducing the CI failure byte for byte (`["Cargo.toml"]` against
        `["auto", "always"]`). The budget is now a thread-local so a test can say
        how long it is willing to wait: this one allows 60s, because it asserts on
        what the probe *found* and should not also be asserting that a loaded
        machine ran it in time. `times_out_nonterminating_help` sets 200ms instead,
        which turns a 2x margin into 25x and takes ~1.3s off the unit suite.
        Beware the obvious disproof: shortening the budget does *not* make the
        probe fail, because killing the child does not discard what it already
        wrote to the pipe.
  - [ ] `notify_reaches_the_terminal_and_a_quick_command_does_not` — **still open,
        and not reproduced.** Failed once in CI with harness code 150,
        `pty_read_until_command_done` timing out after `on --remove`.
        75 local runs found nothing: 30 interleaved pairs against an unmodified
        tree, 25 with the machine loaded to a load average of 4.7 on 4 cores, and
        20 pinned to a single contended core. Deliberately left alone rather than
        given a longer deadline — without a reproduction that would only move the
        rate. Worth knowing for whoever picks it up: the harness writes the next
        command while a `preprompt` hook is still sleeping 0.6s, so type-ahead
        crossing a hook is the first thing to suspect, and `QUIET` is 10s, so a
        code-150 failure means a full ten seconds of silence rather than a near
        miss.
- [ ] **Unit tests write to the real `$HOME/.cache`.** Found while chasing the
      above, and unrelated to any of them. Nothing in the suite sets
      `XDG_CACHE_HOME`, so `cache_directory` (`completion.rs:1051`) falls back to
      `$HOME/.cache/mesh/completions` and the completion tests leave `.spec` files
      in the developer's own cache — 147 of them on this machine. They are keyed by
      a hash of the executable's path, which for these tests contains the process
      id, so a reused pid could match a stale entry; the stored fingerprint
      (mtime and size) saves it in practice, which is luck rather than isolation.
      The fix is to point `XDG_CACHE_HOME` at a temporary directory for the test
      run.
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
- [ ] **Finish moving the job-synchronizing tests onto `wait`.** A good number of
      tests need to say "the background job has finished". Sleeping at it is a
      live flake source, since no interval is long enough on a machine that is
      busy enough, and the failure is not merely a late result: the shell hangs
      up its jobs on the way out, so a shell that exits first *kills* the job it
      was waiting for. Now that `wait` exists the tightest cases use it, and the
      rest — the scattered `sleep 0.2`–`sleep 0.4` in the job-control and
      redirection tests, all with margins of 250ms or more — are still on a
      guessed interval. Three details decide what each one can use:
  - [ ] `wait` **removes the job from the table**, so any test asserting on the
        `[N] Done (…)` notice cannot use it and needs `while $sh.jobs[N].state
        == running` instead. `background_pipeline_retains_statuses_reaped_on_
        earlier_prompts` and `a_failed_background_redirect_reports_mesh_status_
        one` are both in this class.
  - [ ] `wait` takes an explicit job reference, several of them, or none — the
        bare form waits for every job in the table and reports the last failure.
        A test that wants one job's status should say `wait 1`; a test that wants
        "everything finished" can now say `wait` instead of sleeping.
  - [ ] Some sleeps guard nothing at all — `a_function_stage_keeps_its_typed_
        arguments` slept 0.1s for a *foreground* stage the shell already waits
        for. Check whether there is a job to wait for before reaching for a
        primitive.

- [ ] **A bare `return` reached through a variable loses the result so far.**
      `func f() { r = return; false; $r }` reports **0**, where the written
      `func f() { false; return }` reports 1. Both build the same
      `Step::Return(shell.result.clone())`, so the operand is not the problem —
      expanding word zero is. `$r` is evaluated before the word is known to name
      `return`, and evaluating it overwrites `shell.result`, so "the result so
      far" is the expansion's own rather than the `false` before it. Pre-existing
      and narrow (the operand forms agree since the typed-`return` fix; only the
      *bare* one diverges), but it is the same word meaning two things depending
      on how it is spelled. The fix is to snapshot the result before expanding a
      stage's words rather than to special-case `return`, since any command word
      that carries a value has the same effect on what follows it.

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

## Parser asymmetries found writing `GRAMMAR.md`

Reading `parser.rs` production by production to write the grammar turned up rules
that look accidental rather than chosen. Each was **documented as it behaves**,
because a docs consolidation is the wrong place to change the language — but each
is a candidate for a parser fix instead, at which point the grammar entry becomes
a one-line edit. Every claim below was checked against the built shell.

- [x] **A range chain parsed and then failed at run time.** `Parser::binary`
      `continue`d its operator loop after building an `Expr::Range`, so
      `1 .. 2 .. 3` was `(1..2)..3` — accepted by `mesh -n`, then refused by the
      engine with `range endpoints must be integers`, which named neither the
      operator nor the line's real problem. Fixed by giving the range tier the
      guard the comparison tier already had: `ChainedRange`, reported at the
      second `..`. It covers the two spellings that reach the same shape through
      an *operand* rather than through the loop — `1 .. ..3`, whose end `primary`
      reads whole, and `..1 .. 2`, which arrives with the first operand already a
      range — so every way of writing it answers alike. Group to say it
      (`(1 .. 2) .. 3` parses, and is the engine's problem from there).

- [ ] **A parameter list refuses the newline an argument list accepts.**
      `g(1` ⏎ `, 2)` parses; `func f(x = 1` ⏎ `, y)` is ``expected `,` or `)` ``.
      `Parser::arguments` takes `NL*` before the comma and `Parser::parameters`
      does not. Nothing seems to depend on the difference, and a signature is
      exactly the thing long enough to want breaking across lines.

- [ ] **A repeated `;` is refused by a check that only looks one token ahead.**
      `Parser::source` tests `same(Semi) && tokens[position + 1] is Semi` once,
      right after the statement, and then lets `terminators()` swallow any run.
      So `puts x;; puts y` is `an empty command` while `puts x;` ⏎ `;; puts y`
      is fine. The narrow rule is hard to state as anything but "what the code
      happens to do"; either check the whole run or drop the check.

- [ ] **`$env.PATH[0] = …` reports `expected a statement separator`.** Not a
      target (an env entry is bytes, with nothing inside to reach into), so it
      falls through to an ordinary expression and the error lands on the `=`,
      naming neither the entry nor why it was refused. That is the same shape of
      complaint that `member_target` already answers for `$sh` by *accepting* the
      parse and reporting at run time.

- [ ] **`/a/:i:g` says ``:g` is not a modifier`.** `Parser::regex_literal`
      requires every link in the chain to be a regex flag and abandons the whole
      regex reading on the first that is not — correct, but the message then comes
      from the string path and never mentions flags. Naming the flag vocabulary
      would say what is actually wrong.

- [ ] **Newline layout inside a group covers binary operators but not postfix
      access.** `Parser::wraps` fires for a continuing binary or range operator,
      so `(1` ⏎ `+ 2)` parses, while `($m` ⏎ `.a)` is `expected )`. Likewise
      `not` takes no newline after it (`x = not` ⏎ `true`) where every binary
      operator does. Both may be deliberate; neither is written down anywhere as
      a decision.

- [ ] **A bracketed `match` arm cannot be an exact list.** A leading `[` goes
      straight to `binding_pattern`, so `[1] =>` is `expected a name` and there is
      no way to spell "matches the one-element list `[1]`" other than binding and
      comparing in a guard. Compare `1 =>` and `"x" =>`, which do compare.

## Decisions made

- **`:name` is reserved by *shape*, not by the name list** (mikelward, decided).
  What follows the colon is claimed when it is an identifier; whether that
  identifier names a modifier decides only whether it *works*, never whether it was
  claimed. The alternative — ask `MODIFIER_NAMES` and fall back to literal text —
  makes the reading depend on the list's contents, so implementing a new modifier
  would silently change scripts that never mentioned it. Introducing `:port` must
  not be able to reinterpret `"$h:port"`.

  Unquoted and braced spellings already reserved the shape; bare-in-string asked
  the list, so `"$h:nope"` was the text `host:nope` while `$h:nope` and
  `"${h:nope}"` were both `` `:nope` is not a modifier ``. Fixed in
  `variable_access_prefix`, which now claims any identifier and reports an unknown
  one. A leading digit is not an identifier, which is what keeps `"$h:2"`,
  `"$h:8080"`, `"$h:/path"`, `"$h:$port"` and a bare `"$h:"` reading as the text
  they always were.

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

- [ ] **Should a `"…"` string require braces to introduce an interpolation?**
      Raised by mikelward as "the interpolation syntax is ugly and special-casey",
      then narrowed over a long back-and-forth. Bare `$var` **outside** quotes is not
      in question and stays, chains included — `puts $p:base`, `if $xs:len > 5`,
      `cd $dir:real` are untouched by everything below. The question is only what a
      `$` means *inside* a `"…"` string.

      **The proposal.** Inside a string, only a braced form interpolates. A `$`
      outside those braces is an ordinary character.

      ```
      "{$foo}"          the value          "$foo"          not an interpolation
      "{$foo:upper}"    a chain            "cost: $5.00"   text
      "{$file}.bak"     value, then text   "awk '$1'"      text
      "{$m.a}"          member access
      "{1 + 2}"         any expression
      ```

      **What it settles.** The complaint that started this is `${foo:upper}` against
      `${foo}:upper`: the brace delimits a *name* in one reading and an *expression*
      in the other, so the rule learned from `$foo:upper` — the chain attaches to the
      reference — is contradicted by the third spelling. With no bare-in-string form
      there is no competing rule to learn, and the brace has one job. That is also
      why JS `${…}` and Ruby `#{…}` do not feel illogical despite the identical
      inside/outside distinction: their opener is atomic and neither has a bare form.

      It also disposes of three entries in this file rather than fixing them:
      rough edge 10 (`"$file.bak"` is member access, not text) cannot arise, the
      command-vs-value position divergence cannot arise, and the shape-not-list rule
      under "Decisions made" becomes moot — nothing after a name is ever scanned, so
      no future modifier can reinterpret an existing string. Forward compatibility
      stops being a rule to enforce and becomes a property of the grammar.
      `variable_access_prefix` and its whole bug class are deleted, not shrunk.

      **The bracket is a separate, smaller question.** `{$x}` versus `${x}`: putting
      the sigil inside removes the name-delimiting reading structurally, since
      `{foo}` cannot mean "the variable foo, delimited" when it has no `$`. But once
      the bare-in-string form is gone, `${x}` is unambiguous too — so `{$x}` buys
      visible grouping and costs an escape for `{` in every string. Note mesh already
      has `'…'` (no interpolation) and `r'…'` (raw), so JSON, awk programs and format
      strings have somewhere to live that pays neither escape. Decide it separately;
      it is not what makes the design work.

      **The footgun, and why it should be an error rather than a warning.** The
      danger is `"$HOME/bin"` quietly becoming literal text. A warning is the worst
      answer, because it means the code still runs while nagging. Make `$name`
      inside a `"…"` string a **permanent syntax error** — "write `{$name}`, or
      `\$name` for a literal" — and there is never a version in which the string
      silently means something new. The escape cost does not rise: writing a literal
      `$name` in a string already costs `\$` today. `"$5.00"`, `"awk '$1'"` and a
      bare `"$ "` stay untouched, since a `$` before a non-identifier was never an
      interpolation.

      **The open question is muscle memory, and it splits in two.**

      | | what happens | cost |
      |---|---|---|
      | the reflex `"$var"` | loud error naming both fixes | one-time, nothing silent |
      | composition `"$a/$b"` | `"{$a}/{$b}"` | real, unavoidable |

      Only the second is a standing cost, and mesh has an unusual reason to think it
      is small: **unquoted is already safe here.** There is no word splitting and no
      re-globbing, so `f = "two words.txt"; puts $f` and `mkdir -p $d` and
      `g = "*"; puts $g` all do the right thing without quotes. The bash reflex to
      quote defends against a hazard mesh does not have, so most `"$var"` a shell
      user would write becomes bare `$var` here — not `{$var}`. Quoting in mesh is
      for *composition*, which is the only case that pays.

      **What would settle it.** How often real mesh code does genuine composition
      inside a string. A crude count of this repo's docs gives ~54 interpolations
      inside `"…"` against ~321 bare, which points the right way but is weak evidence
      — documentation demonstrates features rather than reflecting normal use. The
      corpus that would answer it is `mikelward/conf#226`, the ~1800-line port: count
      quoted interpolations that genuinely compose against those that are a bare name
      standing alone. Not yet done — mikelward asked to think about footguns and
      simplicity first.

- [ ] **Format string + positional arguments, instead of or beside interpolation?**
      Raised by mikelward alongside the bracket question: what about Python's
      `str.format`, absl `Substitute`, or `printf` — a format string taking
      arguments, rather than a string that references values itself.

      **As a replacement: no, and the reason is that mesh already banked the win.**
      The case for parameterized-over-interpolated — in SQL, in logging, in shell —
      is that the data can never be re-read as syntax. mesh gets that from typed
      values and no implicit word splitting: `$user` holding `; rm -rf /` is
      already one argument. So the safety argument is spent before the syntax
      question starts, and what remains is aesthetics paid for on the most common
      operation in the language: `"$user@$host:$port/$path"` becomes four holes and
      four fillers held apart, with positional mismatch as a new error class. The
      direction of travel elsewhere is *toward* interpolation for the same reason —
      Python has `%`, `.format` and f-strings and f-strings won; C# added `$"…"`
      on top of `String.Format`; every shell has `printf` **beside** interpolation,
      never instead of it.

      It is worth recording that it *would* be the largest simplification of the
      three options: the string becomes inert data, so `variable_access_prefix` and
      the machinery for re-parsing an expression nested inside a string token both
      go, not just the former. That is a real argument, and it loses to ergonomics
      rather than to correctness.

      **As an addition: yes, and there is a gap here already.** Width, precision
      and alignment (`%5.2f`, column padding) cannot be expressed by interpolation
      in any language without a mini format language inside the braces — Python's
      `{:>10.2f}` is a format string relocated, not avoided. Nor can a *reusable*
      format string: one bound to a name, used at several call sites, or swapped
      for i18n. Interpolation is single-use by construction. mesh has neither:
      `:format` is in the reserved modifier list but `DESIGN.md` scopes it to
      `Instant` (`$t:format("%F %T")`) and it is unimplemented, so there is no
      general facility. Worth adding as a builtin beside interpolation; decide its
      placeholder spelling with the bracket question above, since a `{}` hole in a
      format string and a `{…}` interpolation would want to agree.
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
  - [ ] **Finish implementing it.** The **operators** landed since this was
        written: `+ - * / %` all evaluate, in both contexts, with the decided
        signs — `(-10 / 3)` is `-3` and `(-10 % 3)` is `-1`, so truncation toward
        zero and a dividend-signed remainder are both real, and `(1 / 0)` and an
        overflowing `+` are loud errors.

        What is still missing is the **literals** and `:pow`. `0xff`, `0o755`,
        `0b1011` and `1_000` all still parse as **strings**, so `x = 1_000` binds
        text and `$x + 1` fails with `expected integer` — the shape most likely to
        be read as a bug, since the literal looks typed. `$b:pow(3)` is not a
        modifier yet. The leading-zero question below still gates the literals.
  - [x] **How subtraction is spelled.** *Decided: type-directed dispatch* — see
        §"Binary `-`, and the glob-exclusion collision" in `DESIGN.md`. It is what
        `+` already does across strings, lists, maps and integers, so `-` doing it
        is symmetric rather than novel.

        The collision this entry worried about turns out not to exist. Bare glob
        literals are **eager** — verified: `g = *` binds a list of paths, and
        `$g:type` answers per element — so by the time `-` evaluates, `* - *.bak`
        is a list minus a list. Glob exclusion **is** list difference, not a second
        meaning, and the parse never forks: `-` always reads as binary minus and
        only evaluation asks what it was handed. The modifier form and the
        parenthesis-only form are both unnecessary.
  - [ ] **Implement floats.** The model is decided — see §"Floats" under
        *Arithmetic* in `DESIGN.md`. `f64`; `/` unchanged on two integers, so no
        `//`; widen on mixed arithmetic but **compare exactly**, never through an
        `i64`→`f64` cast; `1 == 1.0` with `Hash` agreeing, since `:dedup` is a
        `HashSet<Value>`; no NaN and no infinity, float `/0` and overflow being
        loud errors as the integer ones already are; shortest round-trip rendering
        with exponent form beyond some large/small magnitude — Python's ±(10¹⁶,
        10⁻⁴) are a reasonable starting point, deliberately **not** settled here;
        `%` being `fmod`; and `:num` as the string→number parse.

        **The governing rule is to take Rust's operation wherever Rust has one** —
        integer `/`, `%` as `fmod`, checked overflow — so an unstated *arithmetic*
        edge means "whatever Rust does". Rendering is **excluded**: Rust's `{}`
        never uses exponent form, printing `1e300` as 301 digits, so the digit
        selection is Rust's and the exponent switch is mesh's. The notable
        divergences — not a closed list — are a **normalized value space** (no
        NaN, no infinity, no negative zero; Rust yields all three, `-4.0 % 2.0`
        being `-0` there), implicit widening, cross-type comparison (which Rust
        does not provide, so it is the one unavoidable hand-roll), and checked
        float-to-integer conversion (Rust's `as` saturates; `:int` raises).

        **Two traps to watch for when building it**, both found in review. Float `%`
        is `fmod` — the *truncating* remainder, explicitly not IEEE 754's
        `remainder`, which rounds to nearest-even and answers `-1` where
        `fmod(3, 2)` is `1` — computed directly, never `a - trunc(a / b) * b`: above 2⁵³ the
        rounded quotient loses what the subtraction needs (`1e20 % 3.0` is `1`, the
        expansion gives `0`), and `1e308 % 1e-308` overflows its intermediate to
        infinity for a finite answer. `%` by a zero divisor is a loud error like
        `/` is, since `fmod(x, 0.0)` is `NaN` — integer `5 % 0` already reports
        `division by zero`, and the float case must not diverge. And `:int` on a float outside `i64` is a loud
        range error — Rust's `as` cast saturates silently, which is exactly the
        wrong answer the checked model exists to prevent.

        **`:repr` is a separate channel from display.** Display drops a trailing
        `.0`, but `:repr` must keep it — an integral float writes as `1.0`, since
        `:repr`'s contract is that its output reads back as the same value, and `1`
        would read back an integer where `1 / 2` is `0` and `1.0 / 2` is `0.5`. The
        same reason `42` and `'42'` are already spelled apart there. Also from the
        #341 review.

        The bug this closes: `9.5 < 10.5` answers **`false`** today, because `3.5`
        is a word rather than a number and `<` compares two strings lexically. It
        is the one place mesh returns a quiet wrong answer instead of an error.

        Sequencing note — the float literal and the still-unbuilt `0x` / `0o` /
        `0b` / `_` forms are the same lexer path, and the leading-zero question
        gates both, so they want doing together rather than twice.
  - [ ] **Implement `-` and its list form.** Subtraction on two integers works
        today; nothing else does. `(* - *.bak)`, `($xs - $ys)` and `([a b] - 1)`
        all answer `expected integer`, so the list difference is unbuilt and the
        error message needs to name both accepted shapes rather than only the
        numeric one. Nothing depends on the current behavior, so this is additive.

        **Two pieces, not one.** Besides the operation, a **glob-led statement has
        to be classified as a value**. `outranks_a_command` (`parser.rs:6022`)
        promotes a bare leading scalar only when it is an integer, a boolean or
        quoted, so a bare `* - *.bak` stays a command pipeline and tries to run the
        first match — `command not found: a.txt`, verified. `[a b c] - [b]` leads
        with a non-scalar and is already classified as a value, which is why it
        reaches evaluation instead. Implementing only the list difference would
        leave the documented headline spelling still executing a file. Raised in
        review on mikelward/mesh#341.
  - [ ] **The spacing rule is not enforced as written.** `DESIGN.md` says `-` is an
        operator "*only* with surrounding spaces" — the rule that keeps kebab-case
        names like `last-cmd-time` safe. The implementation requires only the
        **leading** space: `(5 -3)` answers `2` and `($a -$b)` answers `2`, while
        `(5- 3)` is a syntax error. Harmless while `-` is integer-only, and sharper
        once `-*.bak` could plausibly read as one word. Settle it deliberately —
        either enforce both sides or write down that the leading space is what
        matters — rather than letting the two drift further apart.
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
      Now that path-type entries are lists, `$env.PATH` needs `...` or a `:join`
      to reach an external command like any other list — consistent, and what
      makes `+=` append an entry rather than concatenate a string. (`puts
      $env.PATH` prints one entry per line; the boundary in question is argv.)
      But `PATH` is the list users reach
      for by reflex from other shells, where it is plain text, so it sharpens the
      general question already in this icebox: whether list values should flatten
      automatically at the external-command boundary. Decide it there, as one
      rule, rather than carving out a special case for path-type entries.
- [x] **`return` with no argument — use the last status.** `exit` already does
      this (a bare `exit` leaves the last command's status). Apply the same rule
      to `return` when it lands with function bodies.
- [x] **Glob qualifiers — the type and boolean halves.** `*(f)` / `*(d)` / `*(l)`
      / `*(p s b c)`, the long `type: file` names with their `file|dir`
      alternation, and the `x` / `exec:` / `empty:` tests, in both command and
      value position. An attached `(` opens qualifiers only after a word carrying
      bare glob syntax, which is what keeps `style(x)` a call. Types read `lstat`
      so `l` means the link; `exec` and `empty` follow it, since a symlink's own
      mode is `0777` and an `lstat` reading would make `*(x)` list every link.
- [ ] **Glob qualifiers — the comparison predicates.** `size > 1M` and
      `age < 1d` (`DESIGN.md` §"Globbing") are the part still missing. They need
      more than the two above: a size/duration literal grammar (`1M`, `1d`) and a
      per-candidate predicate context in which `size` and `age` are properties of
      the path being tested rather than caller-scope names. The qualifier parser
      is where they attach — `Parser::qualifier` — and `expand::qualifies` is
      where they would be evaluated.
- [ ] **Fuse `**:files` into the match rather than filtering after it.**
      `glob()` / `files()` / `dirs()` and the `:files` / `:dirs` modifiers landed
      separately, and the qualifiers here are a third path to the same question,
      each filtering paths the walk has already produced. `DESIGN.md` §"Globbing"
      wants the type filter *fused* into matching so `**:files` never materializes
      the non-files, which none of the three does. Worth folding the three type
      filters (`expand::qualifies`, `matches_file_filter`, `directory_entries`)
      into one while doing it.
- [x] **Make `GRAMMAR.md` the current grammar.** Its header said it was a
      task-by-task record of the pre-M3 language and "**not** the current
      execution grammar", pointing at `PARSER.md` for the parser and
      `docs/REFERENCE.md` for the user-facing surface. That left a file named
      after the grammar that nobody should read for the grammar, and it caught
      people out — two changes in this area documented current behavior into it
      by mistake. Rewritten as a single EBNF for the implemented language, with
      `PARSER.md` folded in and deleted.
- [ ] **A glob inside a list literal nests instead of contributing elements.**
      A list element is a word, and a word containing a glob metacharacter
      expands to zero or more matches everywhere else, but `xs = [*.txt]` is a list of
      **one** element that is itself the match list — `$xs:len` is `1`, and
      `puts $xs` reports `a list inside a list has no rendering`. `[z *.txt]`
      measures `2` the same way. Either the element expansion should splice its
      matches, as it does in command arguments, or the nesting is deliberate and
      the doc sentence is wrong; decide which before writing the test.
- [ ] **Two space-separated globs in a list literal do not parse.** `xs = [* *]`
      is `expected a value expression`: list elements are space-separated, so the
      second lone `*` lands in binary-operator position and reads as
      multiplication against the first. The lone-`*` fix deliberately did not
      touch this — inside `[…]` the two readings are genuinely ambiguous under
      the current spacing rule, so it needs a decision, not a parser tweak.
      `[*.txt *.txt]` is unaffected, its stars being inside words.
- [ ] **`$(…)` does not split on newlines.** `capture_source`
      (`crates/mesh-core/src/repl.rs:4315`) returns one `Value::String` with
      trailing newlines trimmed, but `DESIGN.md` §"Modifiers" specifies the
      default capture as a **newline split → list**, and §"Loops" writes
      `for line in $(git status --porcelain)` as the safe line-by-line idiom.
      Today that loop runs **once** with the whole blob bound to `line` — a quiet
      wrong answer rather than an error, which makes this worse than the missing
      `:lines` / `:nulls` / `:raw` modifiers beside it (those at least say
      `modifier :lines is not implemented yet`). Landing the split is what makes
      `:raw` mean anything, since `:raw` is defined as the member that turns it
      off.

      **The default was re-opened and confirmed: newlines.** The alternative on
      the table was *no* split — `$(cmd)` stays one string, lists only ever on
      request — which the scalar-heavy evidence genuinely supports: across this
      repo's docs the scalar captures (`$(pwd)`, `$(hostname)`,
      `$(vcs prompt-info)`, `$(id -un)`, `$(git branch --show-current)`)
      outnumber the list-wanting ones (`$(ls)`, `$(seq …)`) by several times over,
      and today a one-element list is not a near-miss for a
      string but a cliff: `xs = ["/tmp"]; puts "at $xs now"` is an error
      (``list value needs `...` in command arguments``) and `$xs == "/tmp"` is
      `false`. That cost is knowingly accepted. What decides it is the asymmetry
      of the failure, not the frequency: wanting a scalar and getting a list is
      *loud* and the fix is two characters (`"$(cmd)"`), while wanting lines and
      getting a blob is *silent* — exactly the `for line in $(…)` wrong answer
      this entry is about. See `DESIGN.md` §"Command substitution" for the
      written-up decision.
  - [ ] **`:nulls` splits on NUL only — never on both.** Recorded here because
        it is the easy thing to get wrong when the split lands: the tempting
        implementation applies the default newline split first and then hands the
        pieces to the modifier, which tears exactly the filenames `find -print0`
        exists to protect. A split modifier **replaces** the default and runs
        against the raw capture bytes; the same holds for `:tabs`, `:words` and
        `:split(SEP)`. `:lines` is the explicit spelling of the default, not a
        second pass over it.
- [x] **Reserve only bare `_` as discard, allow `_name`.** Today a name must
      start with a letter, so a leading underscore is rejected wholesale (`_` and
      `_x` alike) — `_` is the discard pattern (`DESIGN.md`). Reconsider narrowing
      the reservation to **bare `_` only**, letting `_name` (underscore + letters)
      be a valid identifier, the common "intentional / private / unused-but-named"
      convention. Would touch `read_name` (allow a `_` head as long as the whole
      token isn't just `_`) and the `GRAMMAR.md` name rule.

      **Done, and the entry above is the state when it was written.** `valid_name`
      takes a `_` head as long as something follows it, so `_x = 1`,
      `global _cmd_time = 0s`, `func f(_a)` and `func _exit()` all work, and only
      the bare `_` is reserved. Both sub-items below went with it: there is no rule
      left to hide, and the `docs/PROMPT.md` / `docs/INTRO.md` examples parse as
      written.

      **A motivating case, from `docs/INTEGRATION.md`:** every hook-based
      integration with an external tool needs a private global to carry state
      from `preexec` to `postexec` or across a prompt — atuin's row id,
      starship's command duration, a `cd` tracker's previous `$PWD` — and
      `_atuin_id` is what every bash/zsh config in the world calls that. So this
      is not only a "unused-but-named" nicety; it is the naming convention users
      arrive with for exactly the variables mesh's hook API asks them to create.
      Two further notes:
  - [x] **The diagnostic hides the rule.** `_x = 1` parses as a *command* and
        reports `command not found: _x`; `global _x = 1` reports
        `syntax error: expected a name`. Neither says a name cannot start with
        `_`, so the reader looks for a missing program or a typo. Worth fixing
        even if the reservation stands — the rule should name itself. **Moot:**
        both spellings bind now.
  - [x] **Two design docs already assume it works.** `docs/PROMPT.md` and
        `docs/INTRO.md` both write `global _cmd_time = 0s`, which today's parser
        rejects. PROMPT.md is labeled a design target, but the naming is not one
        of the things it flags as unbuilt, so the example reads as writable and
        is not. Either fix the examples or allow the name. **Moot:** the examples
        run as written.
  - [x] **The two spellings disagree about the bare `_`.** Found re-checking the
        above, and the one piece still open: `_ = 1` reports
        `command not found: _` — the discard read as a command word, the old
        diagnostic complaint surviving on the one name that is still reserved —
        while `global _ = 1` **silently succeeds**, binding nothing and answering
        `0`. One of the two is wrong and neither says what the rule is. The
        discard should presumably refuse a value in both spellings, and say so.

        **Fixed, that way.** `_` discards a *position*, so it has one to discard
        only inside a pattern; on its own there is nothing to assign to. All four
        spellings — `_ =`, `_ +=`, `global _ =`, `global _ +=` — now say that, and
        `if _ = f() { … }` with them. A discarded position is untouched: `[_ x]`,
        `match`'s `_`, `for _ in`, and `_name` are all unaffected.

        They disagreed because the two paths asked different questions. The bare
        spelling never reached an assignment path at all — those are gated on
        `word_text_at`, which answers only for a *name* — so it fell through to a
        command word. `global` asks `binding_pattern` directly, which answers
        `Ignore`, and an `Ignore` assignment bound nothing quietly.

        *The road not taken*, in case it is the better one: `_ = f()` could have
        been **allowed** as an explicit discard, the way Rust's `let _ =` is, which
        would have made the two spellings agree in the other direction and given a
        function tail a way to suppress its value. Refused instead because a
        statement's value is already discarded in statement position, so it buys
        nothing that `f()` on its own does not — and a form that binds nothing and
        reports success is the shape this entry was filed against.
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
