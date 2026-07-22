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
- [ ] Replace the incremental command lexer with the clean-break expression and
      block parser.
  - [x] Fix the parser grammar, precedence, attachment, and completeness contract
        in [`PARSER.md`](PARSER.md).
  - [x] Emit a span-carrying token stream without performing structural parsing.
  - [x] Parse tokens into command, expression, and block AST nodes.
  - [x] Route parser-owned expression errors through `parser::parse` at
        execution entry, including chained comparisons and arithmetic assignment
        syntax, while command words remain compatibility-owned until their AST
        adapter lands below.
  - [ ] Add recursive AST execution for `Source`, `Statement`, `AndOr`,
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
  - [ ] Retire `lexer::split_line` and compatibility lexer types from the REPL
        execution path once commands and expressions run from the AST; retain
        the old lexer only where a temporary public compatibility surface or its
        tests still require it.
  - [ ] Add regression coverage for parser-authoritative errors and completeness,
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

Open review threads swept from the project's PRs that still need attention. In
the latest pass (2026-07-22) every unreplied thread on PRs updated in the last
24 hours was verified against `main` (@ d0485f4): threads whose fix is already
present on `main` were replied to on GitHub and are **omitted here**, and only
findings that still reproduce on `main` are kept below, each with a one-line note.
Entries for PRs #24–#58 are carried over from the earlier sweep and were not
re-verified in this pass. PRs 1–18 were not swept.

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

### PR #65 — Normalize TIOCSCTTY for BSD ioctl ABI ([#65](https://github.com/mikelward/mesh/pull/65))

- `crates/mesh-platform/src/lib.rs` — [thread](https://github.com/mikelward/mesh/pull/65#discussion_r3626885654)
  **[P2] Keep the ioctl request type target-specific for musl** — `mesh-platform`
  only cfg-splits macOS vs. not, forcing `TIOCSCTTY: libc::c_ulong` for every
  other target. On `*-linux-musl`, `libc::ioctl`'s request parameter is `c_int`,
  so the `c_ulong` constant fails to type-check and `cargo test` no longer builds
  on musl. Add a `target_env = "musl"` arm (or otherwise narrow the widening) so
  the constant matches each target's `ioctl` ABI. *(still open on main)*

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

### PR #92 — Document grammar simplification opportunities ([#92](https://github.com/mikelward/mesh/pull/92))

- `GRAMMAR.md:20` — [thread](https://github.com/mikelward/mesh/pull/92#discussion_r3627954324)
  **[P2] Treat trailing connectors as incomplete input** — `GRAMMAR.md` flatly
  calls a trailing `&&` / `||` a syntax error, but the line-editor contract
  requires the interactive parser to report incompleteness and read a
  continuation line. Reserve the syntax error for EOF without a right-hand
  operand and preserve the incomplete result during editing. *(GRAMMAR.md still
  calls them syntax errors)*

### PR #96 — Track structured string interpolation decision ([#96](https://github.com/mikelward/mesh/pull/96))

- `TODO.md` (this file, "Decisions needed") — [thread](https://github.com/mikelward/mesh/pull/96#discussion_r3628103538)
  **[P2] Remove the already-settled interpolation decision** — `DESIGN.md`
  already specifies that lists/maps have no canonical byte representation and that
  interpolating them is a loud error requiring explicit rendering (`:join`).
  Leaving "Double-quoted interpolation of structured values" under "Decisions
  needed" above contradicts the settled design. Resolve/remove that item.
  *(self-referential; the entry is still present in this file)*

### PR #97 — Keep quoted glob hyphens literal ([#97](https://github.com/mikelward/mesh/pull/97))

- `crates/mesh-core/src/expand.rs:165` — [thread](https://github.com/mikelward/mesh/pull/97#discussion_r3628143486)
  **[P2] Keep multiple quoted hyphens out of range positions** — A class with two
  quoted hyphens still lets the first act as a range operator: with files `-`,
  `a`, `z`, `puts [a'--'z]` expands to only `a`. Place deferred hyphens where they
  cannot become the middle of a range and add coverage for multiple quoted
  hyphens. *(still reproduces on main)*

### PR #102 — Add string-list for loops ([#102](https://github.com/mikelward/mesh/pull/102))

- `crates/mesh-core/src/repl.rs:520` — [thread](https://github.com/mikelward/mesh/pull/102#discussion_r3630644981)
  **[P2] Reject `env` as a loop binding** — `for env in [a b] { ... }` is accepted
  at parse time, but every `$env` read inside the body fails with
  `$env: not supported yet` because `env` is the reserved environment namespace.
  Assignments and function parameters already reject `env`; loops should reject it
  at parse time too. *(still accepted on main)*

### PR #104 — Add argument-free postfix modifiers ([#104](https://github.com/mikelward/mesh/pull/104))

- `crates/mesh-core/src/expand.rs:420` — [thread](https://github.com/mikelward/mesh/pull/104#discussion_r3630783302)
  **[P2] Preserve a directory value for paths without a parent** — `$file:dir`
  returns `""` for a bare filename (`report.txt`) and for `/`, conflicting with
  the `dirname` semantics (a dirname is always truthy). Normalize to `.` for a
  relative leaf and `/` for the root. *(still returns empty on main)*
- `crates/mesh-core/src/expand.rs:442` — [thread](https://github.com/mikelward/mesh/pull/104#discussion_r3630783308)
  **[P2] Keep extensions after a dotfile's leading dot** — For `.config.toml`,
  `:exts` is empty and `:bare` returns `.config.toml`, even though `:ext` returns
  `toml` and `:stem` returns `.config`. Only the initial dot should be excluded
  when searching for extension delimiters, so `:base` decomposes into both
  `:stem`+`:ext` and `:bare`+`:exts`. *(still reproduces on main)*

### PR #106 — Add clean-break parser contract and nested list values ([#106](https://github.com/mikelward/mesh/pull/106))

- `crates/mesh-core/src/expand.rs:376` — [thread](https://github.com/mikelward/mesh/pull/106#discussion_r3631193878)
  **[P2] Recurse value modifiers through nested lists** — A nested list passed to
  a value modifier fails instead of mapping over each value: `x = [[a b] c]; y = $x:upper`
  reports `:upper: cannot map over a nested list`. Value modifiers are defined to
  map automatically over lists (unlike `:join`, whose nested-value error is
  explicit); apply the modifier recursively and add coverage. *(still reproduces
  on main; the three PARSER.md findings on #106 — `unless` guards, guards on
  ordinary commands, and `if lhs = rhs` conditional assignment — are now resolved
  in the contract.)*

### PR #107 — Define M3 parser contract, add PARSER.md ([#107](https://github.com/mikelward/mesh/pull/107))

- `PARSER.md:101` — [thread](https://github.com/mikelward/mesh/pull/107#discussion_r3631390301)
  **[P2] Use the decided brace syntax for match arms** — `match-arm` is still
  `pattern "=>" (value-expression | block)`, but the settled Matching section in
  `DESIGN.md` defines arms as `pattern { ... }`, with literal/glob/regex/range
  patterns. Reserve the documented arm syntax even if evaluation stays deferred.
- `PARSER.md:74` — [thread](https://github.com/mikelward/mesh/pull/107#discussion_r3631390309)
  **[P2] Preserve the separator after a background assignment** — `background-job`
  consumes the `&`, but `statement-list` still requires a `list-sep` before the
  next `and-or`, so `j = make -j8 & puts ready` cannot parse even though `&` is
  declared to be both the background marker and a list separator. Model the marker
  so it terminates the assignment RHS and separates the following item.
- `PARSER.md:165` — [thread](https://github.com/mikelward/mesh/pull/107#discussion_r3631390313)
  **[P2] Require an endpoint for inclusive ranges** — `range-expression` makes the
  right `additive` optional for both `..` and `..=`, so `..=` and `1..=` are
  accepted as complete. Split the half-open and inclusive productions so `..=`
  always has a right operand.
- `PARSER.md:22` — [thread](https://github.com/mikelward/mesh/pull/107#discussion_r3631390318)
  **[P2] Reserve the here-string redirect token** — For `cmd <<< "text"`, the
  longest-match set reserves `<<` but not `<<<`, and there is no here-string
  redirect production. Add a distinct `<<<` token and operand shape.

### PR #108 — Add span-carrying M3 tokenizer and syntax parser ([#108](https://github.com/mikelward/mesh/pull/108))

- `crates/mesh-core/src/parser.rs:1356` — [thread](https://github.com/mikelward/mesh/pull/108#discussion_r3631613772)
  **[P1] Represent command substitutions as capture expressions** — `x = $(echo hi)`
  is not captured: `echo` writes straight to stdout and `x` receives the exit
  status (`0`). Add a capture AST node that runs the contents as a statement list
  and captures output. *(same root cause as #114's capture finding below; still
  reproduces on main)*
- `crates/mesh-core/src/parser.rs:1346` — [thread](https://github.com/mikelward/mesh/pull/108#discussion_r3631613777)
  **[P1] Capture a command-running branch's value in value position** — A value
  form whose selected branch runs a command, such as
  `greeting = if $french { echo bonjour } else { hi }`, runs the command directly
  and leaves `greeting` empty. (Literal-value branches like `if true { "x" }`
  already work; capturing a branch that produces its value via a command does
  not.) *(still reproduces on main)*

### PR #109 — Use parser to decide compound-input completeness ([#109](https://github.com/mikelward/mesh/pull/109))

- `crates/mesh-core/src/repl.rs:512` — [thread](https://github.com/mikelward/mesh/pull/109#discussion_r3631789889)
  **[P1] Keep malformed function bodies quarantined** — A body line such as
  `puts xr'\' }` exposes a lexer disagreement: the M3 parser reads the mid-word
  `r'…'` as raw and reports `Complete`, while `lexer::scan_braces` treats the
  quote as escaped and still considers the body open. The buffer is dispatched,
  `define_func` reports a missing `}`, and a following `puts LEAKED` runs at top
  level. Preserve the incomplete result whenever the executor's scanner still
  considers the body open. *(still leaks on main)*

### PR #111 — Route expression errors through the parser ([#111](https://github.com/mikelward/mesh/pull/111))

- `crates/mesh-core/src/repl.rs:91` — [thread](https://github.com/mikelward/mesh/pull/111#discussion_r3632004520)
  **[P2] Keep comparison errors out of command redirections** — When a command
  word is interpolated/quoted, the parser classifies multiple redirections such as
  `cmd=cat; $cmd < first < second` as a `ChainedComparison` and returns status 2
  before the compatibility lexer can run the command. Only honor the comparison
  error after establishing that the input is actually in expression position.
  *(still reproduces on main)*

### PR #113 — Replace compatibility command lexer with parser-driven AST execution ([#113](https://github.com/mikelward/mesh/pull/113))

- `crates/mesh-core/src/repl.rs:83` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632167181)
  **[P1] Keep background conditional lists in one job** — A backgrounded `and-or`
  is launched per-executable, so `&` binds tighter than `&&`. `false && touch marker &`
  still creates `marker` because `false`'s status-0 background launch is observed
  before short-circuiting. The whole chain must background as one unit (or be
  rejected until supported).
- `crates/mesh-core/src/repl.rs:192` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632167202)
  **[P1] Surface errors raised while evaluating guards** — `puts BAD if $missing`
  prints the unbound-variable diagnostic but still exits 0 and silently skips the
  command (`guard_allows` converts `Err(_)` to `false`). Guard evaluation must
  return an error-bearing result so callers report the failure. *(same root cause
  as #120's guard finding.)*
- `crates/mesh-core/src/repl.rs:247` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632167206)
  **[P2] Honor stderr-pipe edges in parsed pipelines** — `sh -c 'echo error >&2' |& cat`
  leaves the error on mesh's stderr with `cat` empty (exit 0); `|&` executes as a
  plain `|`. Propagate the `pipe_stderr` edge into the executor or reject `|&`.
  *(same root cause as #120's `|&` finding.)*
- `crates/mesh-core/src/repl.rs:199` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632196451)
  **[P1] Propagate errors from `if` conditions** — `if $missing { puts wrong } else { puts ELSE }`
  reports the unbound variable but still runs the `else` branch and can finish
  successfully. A fail-loud condition error must abort the conditional, not select
  `else`.
- `crates/mesh-core/src/repl.rs:664` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632196468)
  **[P1] Reprocess input after an incomplete header is invalidated** — Piped
  `func f()` then `puts after` reports the header error but discards `puts after`
  with `std::mem::take`, so the valid second command never runs. Report the header
  error and reprocess the new line as its own input unit.
- `crates/mesh-core/src/repl.rs:169` — [thread](https://github.com/mikelward/mesh/pull/113#discussion_r3632196473)
  **[P2] Reject `break` and continue outside loops** — A top-level `break` is
  reported, but inside a function `func f() { break; puts unreachable }; f` prints
  `unreachable` — the control step escapes the body without aborting it. Track
  loop context and abort/report when no loop is active.

### PR #114 — Execute parsed syntax trees recursively ([#114](https://github.com/mikelward/mesh/pull/114))

- `crates/mesh-core/src/repl.rs:588` — [thread](https://github.com/mikelward/mesh/pull/114#discussion_r3632264055)
  **[P1] Capture command output instead of returning its status** — In an
  AST-evaluated expression, `E::Capture` runs the source with inherited stdout and
  substitutes the exit-status string: `answer = $(printf 20) + 22; puts $answer`
  leaks `20` to stdout and prints `2022` instead of `42`. *(same root cause as
  #108's capture finding; still reproduces on main)*

### PR #116 — Evaluate parsed expressions as typed runtime values ([#116](https://github.com/mikelward/mesh/pull/116))

- `crates/mesh-core/src/repl.rs:868` — [thread](https://github.com/mikelward/mesh/pull/116#discussion_r3632440491)
  **[P2] Distinguish remainder overflow from division by zero** — `i64::MIN % -1`
  makes `checked_rem` return `None` (overflow), but the branch reports
  `division by zero`. Check the divisor first and report numeric overflow for the
  remaining `None`, mirroring `checked_div`. *(still mislabeled on main)*

### PR #118 — Use parser completeness for compound input ([#118](https://github.com/mikelward/mesh/pull/118))

- `crates/mesh-core/src/repl.rs:1216` — [thread](https://github.com/mikelward/mesh/pull/118#discussion_r3632636867)
  **[P1] Quarantine malformed compound bodies through their closing brace** — A
  malformed header that has already opened a body, `func f(x {\nputs LEAKED\n}`,
  parses the first line to an error rather than `Incomplete`, so the reader runs
  the subsequent body lines at top level and `puts LEAKED` executes. Preserve the
  unit through its closing delimiter. *(still leaks on main; the sibling
  "incomplete compound after earlier executables" finding is now fixed.)*

### PR #120 — Execute REPL from parser AST and remove lexer::split_line ([#120](https://github.com/mikelward/mesh/pull/120))

- `crates/mesh-core/src/repl.rs:80` — [thread](https://github.com/mikelward/mesh/pull/120#discussion_r3632755710)
  **[P1] Propagate postfix-guard evaluation failures** — `guard_allows` converts
  `Err(_)` to `false` and returns the previous status, so
  `puts skipped if $missing && puts ran` reports the unbound variable but still
  runs `puts ran` and exits 0. Preserve the evaluation error's `Step` so `&&` does
  not continue. *(same root cause as #113's guard finding.)*
- `crates/mesh-core/src/repl.rs:80` — [thread](https://github.com/mikelward/mesh/pull/120#discussion_r3632755717)
  **[P1] Preserve stderr piping for `|&`** — `run_ast_pipeline` ignores
  `Pipeline::pipe_stderr`, so `sh -c 'echo err >&2' |& cat` writes `err` to mesh's
  stderr and leaves `cat` empty. Carry the per-connector stderr flag into
  execution. *(same root cause as #113's `|&` finding.)*
