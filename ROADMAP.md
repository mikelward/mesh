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

*(Everything below is planned; scope will firm up as each milestone begins.)*

---

## M1 — A shell you'd actually sit in

**Goal:** replace the bare stdin reader with a real interactive line editor, and
the placeholder tokenizer with the first real slice of the mesh lexer.

**Scope**
- `reedline` line editing ✅ landed — interactive TTY input with in-memory
  history and Ctrl-C/Ctrl-D handling; the std byte reader stays for piped input.
- Lexer v1: quoting (`'…'`, `"…"`) and escaping, so arguments with spaces work.
- Promote the shell internals into a `crates/mesh-core` library ✅ landed; the
  binary is a thin `main` (enables direct unit tests of the lexer).
- `&&` / `||` sequencing and `;` — the smallest useful control flow.
- `cd`/`pwd`/`puts` builtins ✅ landed — basic `cd` (`$HOME`, `cd -`, updates
  `$env.PWD`/`OLDPWD`). Remaining for `cd`: `CDPATH`, `--physical`, autocd,
  logical cwd.

**Acceptance:** edit and recall lines interactively; `echo "a b"` passes one
argument; `false || echo ok` prints `ok`.

---

## M2 — Pipes, redirection, and job control

**Goal:** the headline feature — real POSIX job control — plus the plumbing that
makes a shell a shell.

**Scope**
- Pipelines (`a | b | c`) and basic redirection (`>`, `>>`, `<`) ✅ landed.
  Descriptor/stderr redirection (`2>`) remains planned.
- Process groups, `tcsetpgrp`, foreground signal handling, stopped-job tracking,
  `&` background launch, and `jobs` / `fg` / `bg` ✅ landed.
- **Ctrl-C returns to the prompt** with status `130` (child gets SIGINT, shell
  survives) ✅ landed.
- Hand the terminal to full-screen programs (`vim`) and get it back cleanly ✅
  landed, including restoration of the shell's saved terminal modes.

**Acceptance:** `ls | grep foo > out.txt` works; `Ctrl-Z` a `vim`, `bg`/`fg` it,
run a pipeline alongside.

---

## M3 — The mesh language 🚧

**Goal:** start turning `DESIGN.md` into a running language — the point where the
build track and the design track converge.

**Scope (indicative — driven by `DESIGN.md` as it settles):** parser for the
clean-break grammar; real values (lists, maps) with no word-splitting;
`$`-expansion and the `...` spread; `:`-modifiers; `if`/`for`/`match`; `func`.

The first vertical slice has landed: assignments accept string-list literals
(`xs = [a "b c"]`, including `[]`), `...$xs` spreads one into command
arguments, and exact integer indexing reads an element. A bare `$xs` in
argument position fails loudly, preserving the design's
explicit-spread/no-word-splitting rule. Nested values, slicing, and general
expression parsing remain ahead. List-valued assignment also preserves a whole
list or slice instead of coercing it to command words.

Named functions and the first conditional slice have also landed. `if` executes
brace-delimited branches from command status, supports `else if`, and can yield a
string or string-list value in assignment position (with `""` for a missing
`else`). General boolean/comparison expressions and conditional destructuring
remain tied to the general expression parser.

The first `for` slice iterates string lists and expanded word expressions with
brace-delimited bodies. Map/range iteration, destructured binders, `break`, and
`continue` remain ahead.

**Acceptance:** the `DESIGN.md`/`docs/INTRO.md` examples run as written.

---

## Beyond

Fuzzy + case-insensitive completion (`nucleo`), the status-dashboard prompt with
composable hooks, session management (shpool/tmux), regex/`~`, and the rest of
the `DESIGN.md` surface. Sequenced when the milestones above make them reachable.

---

The near-term, checkable task list lives in [`TODO.md`](TODO.md); this file is
the stable arc, that file is the working front.
