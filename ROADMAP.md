# Roadmap

The implementation plan, as milestones. This is the build track that runs **in
parallel with** the language design in [`DESIGN.md`](DESIGN.md): the design
settles *what* mesh should be; the milestones below get a working shell in front
of the keyboard as early as possible and grow it toward that design.

Each milestone is a shippable, testable increment. Scope is deliberately narrow —
the goal is a thing you can *run*, not a feature checklist completed on paper.

> A full language `SPEC.md` is deferred until grammar design resumes. Until then,
> each milestone's **Acceptance** section is the behavioral contract for that
> increment, and `DESIGN.md` remains the source of truth for the eventual
> language.

---

## M0 — It runs `ls` ✅

**Goal:** the smallest shell that reads a line and launches the external command
it names. No mesh language yet.

**Scope**
- Read/tokenize/dispatch loop over stdin (interactive TTY and piped input).
- M0 tokenizer: split on whitespace only — an explicit placeholder for the real
  lexer, with no quoting/expansion.
- Launch external commands with inherited stdio; wait; report status.
- Builtin `exit` (`cd` is deferred — see below).
- Exit-status conventions: `127` not-found, `126` not-executable, `128+signal`
  when signaled; the last command's status becomes the shell's exit code.
- Zero dependencies; workspace + CI (fmt, clippy, test on Linux and macOS).

`cd` was **punted from M0** to keep it minimal: a correct `cd` pulls in
logical-cwd tracking, `CDPATH`, and the `$env.PWD`/`OLDPWD` contract from
`DESIGN.md`, which is more than M0 needs to run `ls`. A basic `cd` (plus `pwd`
and `puts`) subsequently landed in M1.

**Acceptance**
- `echo 'ls' | mesh` lists the directory; an interactive session runs `ls`,
  `pwd`, `echo`, etc.
- Unknown command prints `command not found` and yields status `127`.
- `exit 3` exits `3`; `exit 256` exits `0` (8-bit masking).
- `cargo test --workspace`, `cargo fmt --check`, and `cargo clippy -- -D warnings`
  are all green.

---

## M1 — A shell you'd actually sit in ✅

**Goal:** replace the bare stdin reader with a real interactive line editor, and
the placeholder tokenizer with the first real slice of the mesh lexer.

**Scope**
- `reedline` line editing ✅ landed — interactive TTY input with in-memory
  history and Ctrl-C/Ctrl-D handling; the std byte reader stays for piped input.
- Prefix completion ✅ landed — commands and builtins in command position,
  filesystem paths in argument position, and visible variables including nested
  map keys after `$map.`.
- Lexer v1 ✅ landed — quoting (`'…'`, `"…"`) and escaping, so arguments with
  spaces work.
- Promote the shell internals into a `crates/mesh-core` library ✅ landed; the
  binary is a thin `main` (enables direct unit tests of the lexer).
- `&&` / `||` sequencing and `;` ✅ landed — the smallest useful control flow.
- `cd`/`pwd`/`puts` builtins ✅ landed — basic `cd` (`$HOME`, `cd -`, updates
  `$env.PWD`/`OLDPWD`); `CDPATH` search landed later. Remaining for `cd`:
  `--physical`, autocd, logical cwd.

**Acceptance:** edit and recall lines interactively; `echo "a b"` passes one
argument; `false || echo ok` prints `ok`.

---

## M2 — Pipes, redirection, and job control ✅

**Goal:** the headline feature — real POSIX job control — plus the plumbing that
makes a shell a shell.

**Scope**
- Pipelines (`a | b | c`) and redirection (`>`, `>>`, `<`), including the
  descriptor forms (`2>`, `2>>`, `2>&1`) ✅ landed.
- Process groups, `tcsetpgrp`, foreground signal handling, stopped-job tracking,
  `&` background launch, and `jobs` / `fg` / `bg` / `wait` / `kill` ✅ landed.
  `j = cmd &` binds a job handle, and `fg` / `bg` / `wait` / `kill` take it or a
  `%` reference. `wait` takes no operand (every job in the table) or several,
  reporting the last failure, and `disown` gives a job up ✅ landed.
- **Ctrl-C returns to the prompt** with status `130` (child gets SIGINT, shell
  survives) ✅ landed.
- Hand the terminal to full-screen programs (`vim`) and get it back cleanly ✅
  landed, including restoration of the shell's saved terminal modes.

**Acceptance:** `ls | grep foo > out.txt` works; `Ctrl-Z` a `vim`, `bg`/`fg` it,
run a pipeline alongside.

---

## M3 — The mesh language ✅

**Goal:** turn the central `DESIGN.md` language ideas into a running, typed
language on top of the M2 shell runtime.

**Landed**
- A clean-break, span-carrying parser for commands, expressions, and blocks,
  replacing the incremental lexer path for execution.
- Typed strings, integers, booleans, recursively nested lists, and ordered
  string-keyed maps, with explicit `...` spread and no implicit word splitting.
- Variable, member, index, and slice access; arithmetic, comparisons, boolean
  operators, append/merge assignment, and chainable argument-free modifiers.
- Named functions with lexical local scope and `return`; typed arguments stay
  typed across an in-shell call.
- `if` expressions with command-status or value conditions and conditional
  list-pattern binding.
- `for` over lists, ordered maps, and bounded integer ranges, with reusable list
  patterns plus `break` and `continue` through nested blocks and function calls.
- Glob and regular-expression `~` tests and ordered `match` expressions with
  exact, glob, regex, range, alternative, list-pattern, guarded, and `_` arms.

**Acceptance:** the implemented M3 subset is covered end to end and documented in
[`docs/TOUR.md`](docs/TOUR.md) and [`docs/REFERENCE.md`](docs/REFERENCE.md).
`DESIGN.md` and `docs/INTRO.md` intentionally also preview post-M3 design; examples
that depend on those later features are not an M3 compatibility promise.

---

## Beyond

Work continues past M3 on this track rather than as a new milestone. Landed so
far, each documented in [`docs/REFERENCE.md`](docs/REFERENCE.md):

- **Script invocation** — `mesh script.mesh a b c`, `mesh -c "…"`, `mesh -s`,
  shebangs, and the `$sh.args` / `$sh.name` slice of the `$sh` namespace, so mesh
  is no longer stdin-only.
- **Fuzzy, case-insensitive completion** (`nucleo`), and a four-layer source for
  what it offers: a curated spec file, else the command's manual page, else a
  bounded `--help` probe, else files and directories.
- **The status-sensitive prompt** with composable lifecycle hooks
  (`prompt`, `prompt-hook`), including the **directory pair** `precd` / `postcd`
  that brackets every actual `cd`.
- **Regex and glob `~` tests**, and `match` with regex arms.
- **The environment as values** — `$env.KEY`, `export`, and the path-type entries
  that split on `:` and rejoin exactly.
- **`type`** — the name lookup every shell spells differently, with bash's flags.

Still ahead: `$sh.complete` for programmable completion, session management
(shpool/tmux), and the rest of the `DESIGN.md` surface.

---

The near-term, checkable task list lives in [`TODO.md`](TODO.md); this file is
the stable arc, that file is the working front.
