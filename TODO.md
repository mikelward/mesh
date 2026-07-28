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
      `prompt-hook --remove` are their spellings — and `DESIGN.md`'s "`puts` takes no
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

## Beyond M3 — External tool integration

The hooks and surfaces a bash/zsh user's toolbox plugs into — starship, atuin,
fzf, carapace, zoxide, direnv, mise. The full write-up, tool by tool, is
[`docs/INTEGRATION.md`](docs/INTEGRATION.md); this is the checkable list it
produced. Several entries overlap with items elsewhere in this file
(`$sh.complete`, `precd`/`postcd`, the keybinding surface); they are named again
here because the integration case is what makes them urgent rather than merely
designed, and the cross-references say where the fuller note lives.

- [ ] **`precd` / `postcd` hooks.** Specified in `DESIGN.md`
      ("Hooks and the prompt"), unbuilt. The highest-value missing hook of the
      set: zoxide's directory recording, direnv, mise, autoenv, and a background
      fetch on arrival all want it. Unlike the keybinding gap it has **no
      workaround** — a function cannot shadow a builtin (`func cd` is refused as
      a reserved name, `whence.rs:348`), so the zsh trick of wrapping `cd` is
      unavailable by design. The closest thing today is a `preprompt` hook
      comparing `$env.PWD` against a stashed global, which is exactly the
      hand-rolled check `DESIGN.md` cites `postcd` as replacing, and which misses
      a `cd` that returns to where it started.
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
      is a JSON env diff, `mise env --json` the same, and atuin's search output
      likewise. mesh cannot parse any of it. Decide between a JSON reader and a
      mesh-defined line format that upstreams are asked to emit; the former needs
      no upstream cooperation, which is most of its case. Doing each bridge in
      Rust instead sidesteps the parser at the cost of building per-tool
      knowledge into the shell.
      **Cost: none in dependency terms.** `serde_json` is already in the tree —
      reedline depends on it, so it is compiled into every build today
      (`cargo tree -i serde_json`). No new crate, no added build time, no
      measurable binary growth. What is left to decide is the *mapping*: how a
      JSON null, a nested array, and a non-integer number become mesh values,
      which is the same question a `:json` modifier or a `from-json` builtin
      would have to answer anyway.
- [ ] **`$env` writes under a computed key, and a bulk env-diff apply.** Only a
      literal `$env.KEY` is an assignment target today (§"The environment" in
      `docs/REFERENCE.md`), so a loop over a computed diff cannot write it — the
      narrowest blocker in the integration document and probably the easiest to
      lift. The tool-facing shape worth considering is applying a **map** as one
      transaction (a key set, a null unset), so a malformed payload is a
      diagnostic rather than a half-applied environment.
- [ ] **Decide the stance on generated code.** Every tool ships
      `eval "$(tool init zsh)"`. mesh has no `eval`, and `source` takes exactly
      one file operand — no pipe, no string, no `-` — so the published install
      line cannot work. `DESIGN.md` sketches `atuin init mesh | source`
      (§"Conditionals"), which would need `source -` or a `run TEXT` builtin.
      The three options and the argument for the third (exchange **data**, not
      code — which needs no `eval` and no upstream change for direnv, mise, and
      carapace) are written up in `docs/INTEGRATION.md`. This decision gates the
      one below.
- [ ] **Publish an integration contract**, so an upstream can add a `mesh`
      target to `atuin init` / `starship init` / `zoxide init` / `direnv hook` /
      `carapace`. Deliberately *after* the hooks and the decision above: a
      `tool init mesh` emitting registrations mesh cannot parse is worse than no
      target at all, since it looks supported and is not.
- [ ] **A name cannot start with `_`** (`parser.rs:1440`, `:4651` require an
      alphabetic first character), so the private-global convention every shell
      config uses is a syntax error in exactly the variables these hooks ask
      users to create. Already tracked — see "Reserve only bare `_` as discard,
      allow `_name`" under "Icebox / decide later", where the integration case
      and the two follow-ons (the diagnostic, and the design-doc examples that do
      not parse) are recorded.
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

- [ ] **A bare `/…/` literal cannot hold a space or an unbalanced paren.** A regex
      literal is not lexed as a unit — it is an ordinary word that
      `match_operand` recognizes *after* tokenization, by its shape — so it ends
      where any word ends. `/foo #bar/:x` is two words and `/foo(/` opens a group,
      both "unexpected end of input", exactly as `puts a b` and `puts a(b` are.
      Pre-existing on `main`, and it applies to every match slot alike: the `~`
      right-hand side, a `match` arm, and the replace family.

      It bites `:x` hardest, since extended mode exists to let a pattern be spaced
      out and commented, and the spelling that most wants a space is the one that
      cannot have one. `#` itself is fine — `/foo#bar/:x` works, so a comment with
      no space before it is writable — and `re(r"…")` takes anything.

      The fix would be lexing `/…/` as a unit, which is exactly what the
      after-the-fact recognition avoids: a leading `/` is far more often a path
      than a pattern, so the lexer cannot know. Any change here has to keep
      `ls /usr/bin` a path and `cat a(b` whatever it is today. Worth a design
      entry rather than a patch.

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

## Loose ends

Small items rescued from pull requests that were closed as superseded — the bulk
of each PR had landed by another route, but these pieces had not.

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
- [ ] **`$( … )` around a value-producing statement fails with the value as a status.**
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
- [ ] **Three flaky tests, all pre-existing and all timing-dependent.** None is
      caused by the depth work, and each was checked against an unmodified tree;
      recorded here rather than fixed because each needs its own change. Two of the
      three surfaced only in CI, which is the slower and more loaded machine — the
      suite's PTY and subprocess-probe tests race real deadlines against real
      output, so a busy runner is where they show.
  - [ ] `ctrl_c_cancels_an_interactive_gets` fails around 10% of the time, on
        `main` as much as on a branch — measured by interleaving the two binaries
        over 60 alternating runs, 7 failures against 6. It times out at 11s waiting
        for the shell to come back to a prompt after the Ctrl-C. Beware measuring
        this by running one build and then the other: the rate drifts with machine
        conditions enough to invent a difference that is not there.
  - [ ] `notify_reaches_the_terminal_and_a_quick_command_does_not` failed once in
        CI with harness code 150 — `pty_read_until_command_done` timing out after
        `prompt-hook --remove`. Not reproducible locally at all: 30 interleaved
        pairs against an unmodified tree failed 0 times on both sides, so this one
        appears to need a loaded runner. It is built on real sleeps (`sleep 0.2`,
        a `preprompt` handler that sleeps) raced against prompt output, which is
        the thing to fix rather than the deadline.
  - [ ] `separated_typed_completion_probes_the_option_context_first` failed once in
        CI, expecting `["auto", "always"]` and getting a filename. The `--help`
        probe it depends on has a 2-second deadline (`completion.rs:993`, `:1268`);
        exceeding it falls back to file completion. The fix is to stop the test
        depending on wall-clock — inject the deadline rather than raise it.
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
- [ ] **Make `GRAMMAR.md` the current grammar.** Its header says it is a
      task-by-task record of the pre-M3 language and "**not** the current
      execution grammar", pointing at `PARSER.md` for the parser and
      `docs/REFERENCE.md` for the user-facing surface. That leaves a file named
      after the grammar that nobody should read for the grammar, and it catches
      people out — two changes in this area have now documented current behavior
      into it by mistake. Either rewrite it as the M3 grammar it is named for,
      folding in what `PARSER.md` and the reference already state, or retire it to
      `docs/history/` so the name stops promising something it does not deliver.
      Rewriting is the better end state: a single EBNF for the implemented
      language is the thing neither of the other two documents provides.
- [ ] **A glob inside a list literal nests instead of contributing elements.**
      `GRAMMAR.md` says each scalar element uses the word-expansion rules, "so a
      glob can contribute zero or more elements", but `xs = [*.txt]` is a list of
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
- [ ] **Reserve only bare `_` as discard, allow `_name`.** Today a name must
      start with a letter, so a leading underscore is rejected wholesale (`_` and
      `_x` alike) — `_` is the discard pattern (`DESIGN.md`). Reconsider narrowing
      the reservation to **bare `_` only**, letting `_name` (underscore + letters)
      be a valid identifier, the common "intentional / private / unused-but-named"
      convention. Would touch `read_name` (allow a `_` head as long as the whole
      token isn't just `_`) and the `GRAMMAR.md` name rule.

      **A motivating case, from `docs/INTEGRATION.md`:** every hook-based
      integration with an external tool needs a private global to carry state
      from `preexec` to `postexec` or across a prompt — atuin's row id,
      starship's command duration, a `cd` tracker's previous `$PWD` — and
      `_atuin_id` is what every bash/zsh config in the world calls that. So this
      is not only a "unused-but-named" nicety; it is the naming convention users
      arrive with for exactly the variables mesh's hook API asks them to create.
      Two further notes:
  - [ ] **The diagnostic hides the rule.** `_x = 1` parses as a *command* and
        reports `command not found: _x`; `global _x = 1` reports
        `syntax error: expected a name`. Neither says a name cannot start with
        `_`, so the reader looks for a missing program or a typo. Worth fixing
        even if the reservation stands — the rule should name itself.
  - [ ] **Two design docs already assume it works.** `docs/PROMPT.md` and
        `docs/INTRO.md` both write `global _cmd_time = 0s`, which today's parser
        rejects. PROMPT.md is labeled a design target, but the naming is not one
        of the things it flags as unbuilt, so the example reads as writable and
        is not. Either fix the examples or allow the name.
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
