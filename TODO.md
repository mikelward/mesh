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
      redirections on individual pipeline stages. Deferred: descriptor/stderr
      redirection, redirected builtins, and redirection without a command.
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

## M3 — The mesh language (in progress)

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
- [ ] Maps and remaining scalar types.
- [x] Initial argument-free `:` modifiers: path/string transforms and list
      collection operations, including typed list results and chaining.
- [x] `func` — user-defined functions: `func name(params) { body }` with required
      named positionals, multi-line bodies, function-local (lexical) scope, and
      `return`. Resolution is builtins → functions → external. Deferred:
      flags/optionals/rest parameters, functions in pipelines/redirections, and
      calling for a value (`f(arg)`) vs running (`f arg`).
- [x] First `if` expression slice — command-status conditions, brace-delimited
      `else` / `else if`, multiline bodies, and assignment-position string/list
      results. Deferred with the general expression parser: boolean/comparison
      conditions and conditional destructuring.
- [ ] `for` / `match`.
  - [x] First `for` slice over string lists and expanded word expressions, with
        brace-delimited multiline bodies and current-scope bindings.
  - [ ] Map/range iteration, destructured binders, `break`, and `continue`.

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
- **Repo license = decide later** (leave unlicensed for now; revisit before any
  real release). Recorded in "Decisions needed" below for visibility.
- **Glob no-match → empty** (nullglob-style: the pattern expands to zero words).
  This is *principled*, not a compromise, and fully consistent with "absence is
  loud": specific-element access (`xs[99]`, `$map.key`) errors because you asked
  for one thing that isn't there and there is no null; a glob (`*.txt`) is a
  **collection query** whose result type is a *list*, so zero matches = the empty
  list = a complete, honest answer, not an absence. Rejects bash's literal
  pass-through as a footgun.

## Decisions needed

- [ ] **Double-quoted interpolation of structured values.** Decide how lists,
      maps, and other non-scalar values interpolate inside `"…"`: whether they
      remain a loud error, use a canonical rendering, or require an explicit
      conversion such as `:join` or serialization.
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
- [ ] **Choose a repo license** — *decided: later* (revisit before any real
      release). Nothing constrains the choice today: all current/planned deps are
      permissive (`reedline`/`nix`/`crossterm` MIT) except `nucleo` **MPL-2.0**
      (weak, file-level copyleft — compatible with a permissive project). Likely
      landing spot: `MIT OR Apache-2.0` (the Rust-ecosystem norm).

## Icebox / decide later

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
- [ ] Reading a script file as an argument (`mesh script.msh`) vs. stdin only
- [ ] `-c "…"` one-shot command flag
- [ ] Whether satellite helpers (`vcs`, prompt) are Rust workspace members or
      standalone (per-helper call; see `DEVELOPMENT.md`)

## Review Comments

Review threads swept from the project's PRs (open and merged) that still need
attention. Each entry is the full reviewer comment plus a link to the PR and the
specific thread. Threads the maintainer already replied to as fixed — or
declined outright — are excluded; a couple where the maintainer agreed the issue
is real but the fix is still pending are kept and marked as such. PRs 1–18 were
not swept in this pass.

### PR #24 — Design: settle regex-as-value/re(), :int, exec, per-stream tty tests ([#24](https://github.com/mikelward/mesh/pull/24))

- `DESIGN.md` — [thread](https://github.com/mikelward/mesh/pull/24#discussion_r3612864631)
  **[P2] Preserve trailing-slash absolute globs on match RHS** — The remaining
  counterexample after the earlier `/usr/*/bin` fix is an absolute glob or path
  that ends in `/`: `$p ~ /tmp/` and `$p ~ /usr/*/` are both complete `/…/`
  atoms under this rule, so they are interpreted as unanchored regexes rather
  than whole-string globs. For example, `/tmp/` becomes the regex `tmp` and
  unexpectedly matches `/var/tmp/file`. Add a disambiguation or escape mechanism
  that preserves these valid trailing-slash absolute patterns.
  _Maintainer reply:_ Valid — `$p ~ /tmp/` reading as the unanchored regex `tmp`
  (matching `/var/tmp/…`) is a real footgun. This is the third distinct edge
  case from the same root cause — the `~` RHS overloads glob and regex with
  colliding `/…/` syntax — so rather than patch the lexer rule a fourth time,
  I'm raising the disambiguation as a design decision with the maintainer
  instead of guessing. Will update here once it's resolved.
### PR #30 — Add the real lexer: quoting, escapes, and expansion suppression ([#30](https://github.com/mikelward/mesh/pull/30))

- `crates/mesh/src/expand.rs:64` — [thread](https://github.com/mikelward/mesh/pull/30#discussion_r3616268047)
  **[P2] Keep quoted class characters from becoming glob syntax** — When quoted
  text occurs inside an unquoted bracket expression, context-free
  `Pattern::escape` does not prevent every literal character from acquiring class
  syntax. For example, `[a'-'z]` passes the new structure guard as `[aaz]`, but
  escaping `-` leaves it unchanged, so the final pattern is `[a-z]` and
  unexpectedly matches a file named `m` rather than limiting the class to literal
  `a`, `-`, and `z`. Pattern construction needs to account for whether each
  literal fragment is inside an active character class. *(no maintainer reply;
  related `['*'` case fixed in 4148687, this class-context corner left open)*

### PR #38 — Add pipes and redirection ([#38](https://github.com/mikelward/mesh/pull/38))

- `crates/mesh/src/lexer.rs:356` — [thread](https://github.com/mikelward/mesh/pull/38#discussion_r3617704138)
  **[P2] Stop the descriptor-prefix scan at preceding operators** — When a
  descriptor redirect begins immediately after another operator, the backward
  scan crosses that operator and incorrectly decides the prefix is not numeric.
  For example, `true;2>f` or `echo x|2>f` treats `2` as a command/argument and
  creates `f` instead of raising the documented unsupported-descriptor syntax
  error. Fresh evidence after the earlier descriptor fixes is that the scan stops
  only at whitespace, even though `finish_segment`/the pipe handler has already
  started a new word after `;`, `&&`, `||`, or `|`. *(no maintainer reply)*

### PR #40 — Split core into `crates/mesh-core` and add stderr redirection ([#40](https://github.com/mikelward/mesh/pull/40))

- `crates/mesh-core/src/exec.rs:125` — [thread](https://github.com/mikelward/mesh/pull/40#discussion_r3617863908)
  **[P2] Redirect spawn diagnostics through the requested stderr file** — When
  the executable cannot be spawned, such as `optional-tool 2>/dev/null`, this
  configures only the prospective child's stderr; `spawn_error_code` later prints
  the command-not-found or permission diagnostic from the parent, so the redirect
  file remains empty and the message leaks to the shell's stderr. Since `2>` is
  documented as redirecting the command's stderr and redirection is otherwise
  bash-like, the spawn-failure diagnostic must also use the opened stderr
  destination. *(no maintainer reply)*
- `crates/mesh-core/src/lexer.rs:342` — [thread](https://github.com/mikelward/mesh/pull/40#discussion_r3617863911)
  **[P2] Accept stderr redirects after an unspaced operator** — The
  whitespace-only boundary check misclassifies a valid `2>` when the stage begins
  immediately after another operator. For example, `true;2>err sh -c 'echo x >&2'`
  or `producer|2>err consumer` treats `2` as the command/argument and `>` as
  stdout redirection instead of recognizing stderr redirection. Separators and
  redirection operators are explicitly allowed without surrounding spaces, so
  operator boundaries such as `;`, `&&`, `||`, and `|` must also qualify as the
  start of the descriptor prefix. *(no maintainer reply)*

### PR #42 — Add functions with named positionals and local scope ([#42](https://github.com/mikelward/mesh/pull/42))

- `crates/mesh-core/src/lexer.rs:560` — [thread](https://github.com/mikelward/mesh/pull/42#discussion_r3618697563)
  **[P1] Keep unmatched parameter lists quarantined** — When an opener contains
  the body `{` but omits the signature's `)`, this searches the entire buffered
  definition and can consume a `)` from a later body line. The latest `p < b`
  guard does not cover this unmatched-`(` case: piped input
  `func f(x {\nputs )\nputs LEAKED\n}\n` stops buffering at `puts )`, rejects the
  partial definition, then executes `puts LEAKED` at top level. Limit the close
  search to the header (before the body `{`) so malformed definitions remain
  quarantined through their matching body close. *(no maintainer reply)*

### PR #57 — Run foreground commands in process groups ([#57](https://github.com/mikelward/mesh/pull/57))

- `crates/mesh-core/src/exec.rs:166` — [thread](https://github.com/mikelward/mesh/pull/57#discussion_r3626714504)
  **[P1] Hand off the terminal before children can read** — When an interactive
  pipeline whose first stages read fd 0, every child is spawned before the
  terminal is transferred here. A scheduled child can therefore read while its
  new process group is still in the background and be stopped by `SIGTTIN`; this
  is readily reproducible with a sufficiently long `cat | cat | ...` pipeline.
  Because the subsequent waits neither observe stopped children nor send
  `SIGCONT`, mesh then hangs indefinitely. Coordinate startup with the terminal
  handoff, or resume the group after transferring it. *(no maintainer reply)*
- `crates/mesh-core/src/exec.rs:113` — [thread](https://github.com/mikelward/mesh/pull/57#discussion_r3626714507)
  **[P1] Preserve signal delivery for non-TTY executions** — When mesh reads a
  script from a file or pipe, this unconditionally moves each command into a
  process group separate from mesh, while the later `is_terminal()` check does
  not hand that group the terminal or otherwise forward signals. Consequently,
  Ctrl-C delivered to the invoking foreground group — or a supervisor signal sent
  to mesh's group — kills mesh but not the running command, which continues as an
  orphan. Keep non-TTY children in mesh's group until signal forwarding is
  implemented, or explicitly relay termination signals. *(no maintainer reply)*

### PR #58 — Handle foreground job signals ([#58](https://github.com/mikelward/mesh/pull/58))

- `crates/mesh-core/src/repl.rs:365` — [thread](https://github.com/mikelward/mesh/pull/58#discussion_r3626735482)
  **[P2] Preserve the background-startup stop before ignoring SIGTTIN** — When
  `mesh` is launched from another interactive shell as a background job
  (`mesh &`), this ignore prevents the kernel from suspending it on its first
  terminal read. In a PTY with another foreground process group, the new code
  instead makes reedline report `line editor error` and mesh exits with status 1,
  so the parent shell cannot later resume it with `fg`. Keep SIGTTIN at its
  default disposition — or explicitly stop until `tcgetpgrp` matches the shell's
  group — during startup, and add PTY regression coverage for this flow. *(no
  maintainer reply)*
- `crates/mesh-core/src/exec.rs:125` — [thread](https://github.com/mikelward/mesh/pull/58#discussion_r3626775540)
  **[P2] Preserve inherited ignored signals in noninteractive mode** — When mesh
  is run noninteractively by a caller that intentionally ignores one of these
  signals, this unconditional hook resets that inherited disposition for every
  external command. For example, with the parent ignoring SIGINT,
  `sh -c 'kill -INT $$; echo survived'` exits with 130 through mesh instead of
  printing `survived`, unlike direct execution through `sh` or `bash`. Apply the
  reset only when the interactive REPL installed these ignores, and add
  noninteractive regression coverage. *(no maintainer reply)*
- `crates/mesh-core/src/exec.rs:186` — [thread](https://github.com/mikelward/mesh/pull/58#discussion_r3626775550)
  **[P2] Avoid sending SIGCONT to every newly launched job** — When a foreground
  program installs a SIGCONT handler or deliberately stops during startup before
  the parent reaches this call, the unconditional group signal either invokes an
  unsolicited handler or resumes an intentional stop. Newly launched jobs that
  never stopped on SIGTTIN should not receive SIGCONT; synchronize the terminal
  handoff before allowing exec, or continue only a group confirmed to have
  stopped, with PTY regression coverage. *(no maintainer reply)*

### PR #66 — Add stopped-job tracking and job-control builtins (jobs, fg, bg) ([#66](https://github.com/mikelward/mesh/pull/66))

- `crates/mesh-core/src/exec.rs:61` — [thread](https://github.com/mikelward/mesh/pull/66#discussion_r3626898554)
  **[P1] Restore each job's terminal modes before continuing it** — When a
  foreground program switches the terminal to raw/no-echo mode and then stops,
  this path gives it the terminal and sends `SIGCONT` without reinstalling the
  terminal attributes it left behind. The stored `shell_modes` value is the
  pre-launch shell snapshot, so programs such as editors resume under the shell's
  canonical settings and can mis-handle input; capture the job's modes when it
  stops and apply them before continuing it. Add a PTY regression that changes
  terminal modes, stops, and verifies the resumed process sees those modes.
  *(no maintainer reply)*
- `crates/mesh-core/src/exec.rs:384` — [thread](https://github.com/mikelward/mesh/pull/66#discussion_r3626898559)
  **[P1] Persist reaped stage statuses across job resumes** — When an earlier
  pipeline stage exits before another stage is stopped, the initial
  `wait_outcomes` call reaps that child but leaves its outcome as `Running`.
  After `fg`, this code waits for the same PID again, gets `ECHILD`, and
  substitutes status 1, so even `true | sleep 3` followed by Ctrl-Z and `fg`
  finishes as a failure; background polling similarly loses statuses observed on
  earlier polls. Persist completed outcomes and their codes instead of re-waiting
  for reaped children, with a stopped multi-stage PTY regression. *(no maintainer
  reply)*
- `crates/mesh/tests/cli.rs:857` — [thread](https://github.com/mikelward/mesh/pull/66#discussion_r3626898563)
  **[P2] Return failed prompts after recognizing them** — When a PTY command
  leaves a nonzero status — particularly after Ctrl-Z, which produces `mesh!` —
  this new condition stops reading at that prompt, but the final predicate still
  returns `Some` only for `mesh$`. The helper therefore reports a timeout/failure
  after successfully finding `mesh!`, preventing it from covering the newly added
  stopped-job flow; accept both prompt forms in the return predicate as well.
  *(no maintainer reply)*
- `crates/mesh-core/src/exec.rs:25` — [thread](https://github.com/mikelward/mesh/pull/66#discussion_r3626928169)
  **[P1] Hang up retained jobs when the shell exits** — When a user stops a
  long-running command, resumes it with `bg`, and then runs `exit` or loses the
  terminal, this new table is discarded without signaling the job's process
  group; terminal-loss SIGHUP also terminates mesh via the default disposition
  before it can forward anything. I reproduced `sleep 1000` remaining alive and
  reparented to PID 1 after `bg` followed by `exit`, contrary to the
  no-persistence-until-`disown` behavior in `DESIGN.md:2255-2256` and the
  explicit HUP/CONT shutdown sequence in `DESIGN.md:2280-2284`. Add an orderly
  shutdown and SIGHUP path that sends SIGHUP to every retained group and SIGCONT
  to stopped groups, plus a PTY regression covering `bg` followed by shell exit.
  *(no maintainer reply)*

### PR #67 — Add background command launch (bare `&`) ([#67](https://github.com/mikelward/mesh/pull/67))

- `crates/mesh-core/src/exec.rs:225` — [thread](https://github.com/mikelward/mesh/pull/67#discussion_r3627015317)
  **[P1] Keep terminal stdin usable after `fg`** — In an interactive session,
  assigning `/dev/null` to every background job's stdin is permanent, so `cat &`
  followed by `fg` immediately receives EOF even after the job owns the terminal.
  Terminal job control already prevents a background process group from consuming
  prompt input via `SIGTTIN`; use `/dev/null` only where that protection is
  unavailable, otherwise foregrounding input-driven jobs is unusable. *(no
  maintainer reply)*
- `crates/mesh-core/src/exec.rs:360` — [thread](https://github.com/mikelward/mesh/pull/67#discussion_r3627015322)
  **[P1] Retain statuses reaped on earlier prompts** — When stages of a
  registered background pipeline finish between different prompt iterations,
  `poll_outcomes` reaps each stage but never stores its status in `Outcome`; its
  local `status` is discarded whenever another stage remains running. For
  example, `sh -c 'exit 7' | sleep 1 &` can be reported as `Done (0)` after an
  intervening prompt, and foregrounding a partially reaped pipeline can similarly
  return a spurious status, violating the documented pipefail result. *(no
  maintainer reply)*
- `crates/mesh-core/src/exec.rs:352` — [thread](https://github.com/mikelward/mesh/pull/67#discussion_r3627015323)
  **[P2] Defer blocking redirects for background jobs** — This background branch
  is reached only after all `open_redirs` threads have been joined, so a command
  such as `cat < fifo & puts ready` blocks the shell while opening the FIFO and
  never launches `puts ready` until an external writer appears. A background
  launch must return control before potentially blocking redirection setup, or
  otherwise perform those opens in the child. *(no maintainer reply)*

### PR #70 — Preserve background job input and outcomes ([#70](https://github.com/mikelward/mesh/pull/70))

- `crates/mesh-core/src/exec.rs:507` — [thread](https://github.com/mikelward/mesh/pull/70#discussion_r3627151268)
  **[P2] Avoid requiring PATH-resolved sh for redirects** — When `PATH` does not
  contain `sh`, every background command with a redirection now fails before
  running, even if the requested executable uses an absolute path. For example,
  with `PATH=/definitely-missing`, `/bin/echo ok > out &` reports the original
  command as not found and never creates `out`; foreground redirection still
  works. Resolve the wrapper independently of the user's command-search path or
  avoid introducing this extra executable dependency. *(no maintainer reply)*
- `crates/mesh-core/src/exec.rs:503` — [thread](https://github.com/mikelward/mesh/pull/70#discussion_r3627151270)
  **[P2] Preserve Mesh's redirection-failure status** — When a background
  redirect cannot be opened, delegating the operation to `sh` changes the result
  from Mesh's established status 1 to a shell-dependent value. On this workspace,
  `/bin/echo ok > missing/dir/out &` is later reported as `Done (2)`, while the
  same foreground command exits 1; the diagnostic also leaks `mesh-redir` instead
  of using Mesh's normal error format. Normalize wrapper-side open failures so
  foreground and background execution retain the same status semantics. *(no
  maintainer reply)*

### PR #76 — Preserve empty strings in list literals ([#76](https://github.com/mikelward/mesh/pull/76))

- `crates/mesh-core/src/lexer.rs:480` — [thread](https://github.com/mikelward/mesh/pull/76#discussion_r3627370806)
  **[P2] Preserve tilde expansion across trailing empty quotes** — For inputs
  such as `puts ~""` or `puts ~""/child`, this zero-length literal becomes the
  next entry in `pieces`; `apply_tilde` then refuses to expand because it only
  accepts a lone `~` or a slash in the immediately following piece. The commands
  consequently emit `~`/`~/child` instead of paths under `$HOME`, even though the
  empty suffix contributes no characters and does not quote the preceding tilde.
  Make tilde adjacency ignore zero-length pieces and add regression coverage for
  these forms. *(no maintainer reply)*

### PR #77 — Preserve background redirection semantics ([#77](https://github.com/mikelward/mesh/pull/77))

- `crates/mesh-core/src/exec.rs:506` — [thread](https://github.com/mikelward/mesh/pull/77#discussion_r3627400544)
  **[P2] Decouple the redirect helper from the embedding executable** — When
  another frontend uses the advertised `mesh_core::run` entry point,
  `current_exe()` names that frontend rather than the `mesh` binary, and the
  frontend is not required to implement `--mesh-background-redirect`. For
  example, a frontend whose `main` only calls `mesh_core::run()` handles
  `/bin/echo ok > out &` by restarting itself, entering its REPL with
  `/dev/null`, and reporting `Done (0)` without executing `echo` or creating
  `out`. The helper must not assume every consumer of `mesh-core` has the private
  dispatch added specifically to `crates/mesh/src/main.rs`. *(no maintainer
  reply)*

### PR #78 — Support member access in quoted interpolation ([#78](https://github.com/mikelward/mesh/pull/78))

- `DESIGN.md:654` — [thread](https://github.com/mikelward/mesh/pull/78#discussion_r3627423330)
  **[P2] Update all docs for the new interpolation rule** — This newly documented
  rule conflicts with two remaining authoritative descriptions:
  `DESIGN.md:1043-1046` still says `"$file.txt"` appends a literal suffix, and
  `docs/REFERENCE.md:96-99` gives users the same now-invalid behavior. With this
  commit, that expression instead attempts member access and fails for a string
  variable, so users following either passage will get runtime errors; update
  those sections to require `"${file}.txt"` and describe unbraced member access.
  *(no maintainer reply)*

### PR #79 — Align quoted interpolation documentation and coverage ([#79](https://github.com/mikelward/mesh/pull/79))

- `GRAMMAR.md:115` — [thread](https://github.com/mikelward/mesh/pull/79#discussion_r3627447406)
  **[P2] Add braced access forms to the grammar** — The new production documents
  only unbraced `$xs[N]`, even though this commit's lexer test and prose
  explicitly support `${xs[N]}` and `${name.member}`. Because `GRAMMAR.md`
  defines the grammar currently parsed, consumers following this EBNF would
  reject valid forms such as `"${items[-1]}"`; add braced member and index
  alternatives, or factor the optional access suffix into both braced and
  unbraced forms. *(no maintainer reply)*

### PR #81 — Add clamped list slicing and spread support ([#81](https://github.com/mikelward/mesh/pull/81))

- `crates/mesh-core/src/expand.rs:215` — [thread](https://github.com/mikelward/mesh/pull/81#discussion_r3627480804)
  **[P2] Apply inclusivity before clamping the end bound** — For inclusive slices
  whose negative end lies before the list (for example, `...$xs[..=-5]` on
  `[a b c d]`), clamping first changes the end to index 0 and then increments it,
  incorrectly returning `a`; every end below `-len` therefore behaves like
  exactly `-len` instead of producing the empty intersection required by clamped
  slicing. Resolve the inclusive end to an exclusive position by adding one
  before clamping (using the existing wider integer representation to avoid
  overflow). *(no maintainer reply; the inclusive-slice clamping order is the
  subject of PRs #83/#85 — verify and resolve)*

### PR #82 — Add append assignment (`+=`) for strings and lists ([#82](https://github.com/mikelward/mesh/pull/82))

- `crates/mesh-core/src/repl.rs:429` — [thread](https://github.com/mikelward/mesh/pull/82#discussion_r3627499447)
  **[P2] Support list slices as append operands** — When the RHS is a list slice
  such as `xs += $more[1..]`, the `access.is_none()` condition sends it through
  `scalar_value`, where expansion rejects the slice with "list value needs `...`"
  and leaves `xs` unchanged. `GRAMMAR.md:241-247` defines a slice as a list value
  and says list-to-list `+=` extends the target, so this path should materialize
  the selected slice as `Value::List` just as a whole-list variable does. *(no
  maintainer reply; addressed by PR #84 "Support sliced RHS for `+=`" — verify
  and resolve)*

### PR #83 — Apply inclusive slice ends before clamping ([#83](https://github.com/mikelward/mesh/pull/83))

- `crates/mesh-core/src/expand.rs:210` — [thread](https://github.com/mikelward/mesh/pull/83#discussion_r3627504423)
  **[P1] Preserve the relative meaning of an inclusive -1 end** — For any
  non-empty list, an inclusive end of `-1` should include the final element, but
  adding the inclusive offset before resolving negative indices changes `-1` to
  absolute `0`; consequently, `$xs[..=-1]` now produces an empty slice instead of
  the entire list. Resolve the original signed endpoint relative to `len` in
  `i128`, then add one and clamp, and add a `..=-1` regression case. *(no
  maintainer reply; addressed by PR #85 "Resolve negative inclusive slice ends
  correctly" — verify and resolve)*

### PR #84 — Support sliced RHS for `+=` and preserve inclusive `..=-1` semantics ([#84](https://github.com/mikelward/mesh/pull/84))

- `GRAMMAR.md:248` — [thread](https://github.com/mikelward/mesh/pull/84#discussion_r3627546056)
  **[P1] Support a spread immediately before `]`** — When a spread is the last or
  only list item (for example, `ys = [first ...$xs]` or `ys = [...$xs]`),
  `list_literal` removes `]` but leaves an empty trailing `Piece::Text`;
  `expand::spread_var` then rejects the word because it requires exactly the
  `"..."` and variable pieces, so the assignment fails with `list value needs
  ...`. This makes the newly documented whole-word spread incomplete and breaks
  the canonical concatenation forms in `DESIGN.md`; discard empty edge pieces
  after removing the brackets and add a final-position regression test. *(no
  maintainer reply; addressed by PR #86 "Support spreads before list closing
  brackets" — verify and resolve)*

### PR #88 — Preserve lists in variable assignments ([#88](https://github.com/mikelward/mesh/pull/88))

- `crates/mesh-core/src/repl.rs:400` — [thread](https://github.com/mikelward/mesh/pull/88#discussion_r3627794224)
  **[P2] Preserve quote context before copying list values** — When the RHS is
  solely a double-quoted interpolation, such as `ys = "$xs"` or
  `ys = "${xs[1..]}"`, the lexer produces the same single-`Piece::Var` shape as a
  bare reference, so this fast path silently binds a list. That contradicts the
  double-quoted string behavior in `DESIGN.md:755-764` and is inconsistent with
  `ys = "x$xs"`, which correctly rejects rendering the list as text. Retain
  whether the reference was quoted or restrict this path to bare references, and
  cover the quoted whole-list and slice cases with a regression test. *(addressed
  by e2ad505 "Preserve quote context for list assignments" — verify and resolve)*

### PRs with no unresolved review threads

37, 39, 41, 45, 47–56, 59, 62, 63, 68, 69, 71, 72, 80, 85, 86, 87, 89, 90.
