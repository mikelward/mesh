# Design

> **Name is not final.** Working title is **smash** or **mesh**. This document
> uses "the shell" to stay name-neutral. See [Name](#name) for the tradeoff.

## What this is

A personal, **interactive-first** Unix shell. The goal is a shell that is a
pleasure to *use* at a terminal all day — not a general-purpose scripting
language, and not a POSIX-compatible `sh`. Where nontrivial logic is needed
(prompt rendering, VCS info), the shell leans on small external binaries (the
`vcs`-style split) rather than growing a heavy scripting layer.

### Goals

- Excellent interactive ergonomics: completion, history, line editing, prompt.
- **Byte-stream pipes** — external commands and coreutils work exactly as they
  do everywhere else. No structured-data pipeline (that is the one thing that
  rules out a nushell-style model here).
- **Real arrays / lists** with **no word-splitting footguns**.
- A **clean-break syntax**: keep the muscle memory that is worth keeping, fix
  the parts that are genuinely bad, and do not carry POSIX warts forward.
- First-class prompt hooks, session management, and job control.

### Non-goals

- Being a scripting language. Interactive use comes first; big logic goes into
  binaries.
- Running existing `sh`/`bash` scripts verbatim. External *programs* run
  normally; the shell *language* is new.
- A structured-data pipeline. Pipes carry bytes.

## Core decisions

| Area | Decision |
| --- | --- |
| Implementation language | **Rust** (best line-editor / TUI ecosystem — reedline, nucleo, crossterm; strong POSIX job-control via `nix`). Satellite helpers (prompt, VCS) may be any language, e.g. Go. |
| Pipe model | **Byte streams.** Coreutils and external programs are first-class. |
| Values | **Real arrays / lists.** No implicit word splitting, ever. |
| Syntax | **Clean break** from POSIX. |
| Config / logic | Written in the shell's own language, with an escape hatch to external binaries for anything heavy or perf-sensitive. |

### Why Rust

The two subsystems that make or break an interactive shell both favor Rust
decisively:

- **Line editing / completion** — `reedline` (multiline, vi+emacs keymaps,
  hinting, history backends), `nucleo` (fzf-grade fuzzy matching, as used by
  Helix), `crossterm`. This is almost exactly the interactive feature set we
  want, already built.
- **Job control** — `nix` exposes the full POSIX surface (`setpgid`,
  `tcsetpgrp`, `WUNTRACED`, signalfd) needed for real `Ctrl-Z` / `fg` / `bg`
  and handing the terminal to a full-screen program like `vim`. This is the
  headline feature ("run vim and a shell/tail in the same shell"), and it is
  the area where Go actively fights the runtime.

Go's genuine wins (goroutines, effortless static builds) land on the *satellite*
work, which stays available: helper binaries can be written in anything.

## Requirements carried over from existing configs

These are treated as settled requirements, drawn from the author's current
bash/zsh/fish/nushell setup:

- **Prompt as a status dashboard** — two-line, full-width, showing host,
  session, VCS/dir, auth, jobs, last-exit status, and timing; a **transient**
  old prompt that collapses in scrollback. The prompt glyph signals which
  shell/mode you are in.
- **Composable prompt hooks** — the prompt may be rendered by an external
  binary, *provided* override hooks (e.g. the `ssh-add` "no identity" warning,
  a `[root]` tag, the session nag) can layer on top. Hooks compose; they do not
  replace each other.
- **Session management** baked in — attach-or-create on login, per-project
  sessions, job publishing to the status bar. shpool preferred, tmux fallback.
- **Emacs keys layered over vi mode** — both keymaps active; two grades of word
  motion; Esc/Alt disambiguation.
- **Fuzzy + case-insensitive completion.**
- **Job control** — the headline feature.
- **Idempotent, guarded PATH** — a single source of truth, deduped, applied
  once per process tree.
- **A predicate vocabulary** — `have_command`, `inside_project`,
  `connected_remotely`, and friends.

## Language sketch

Everything below is **decided** unless marked *(open)*.

### Command substitution

A command substitution **captures the command's raw output bytes.** What you get
back depends on the split that is applied to that capture:

```
$(cmd)          # default: split raw bytes on newlines, trim trailing blank -> list
"$(cmd)"        # one string (trailing newline trimmed)
$(cmd):nulls    # split the raw bytes on NUL -> list  (newline-safe)
$(cmd):raw      # the raw bytes, unsplit, trailing newline intact
```

Newline-splitting is the **default** because it is the dominant Unix convention
(`ls`, `find`, `grep`, `ps`) and never breaks on spaces in filenames — the
classic word-splitting footgun. But it is only the default: a split modifier
**replaces** it and runs against the raw capture (see [Modifiers](#modifiers)),
so the default split never destroys bytes that an explicit splitter needs. In
particular, splitting is applied *once*, not layered on top of the newline
split — `:nulls` sees the raw output (so `find -print0` filenames containing
newlines survive), and `:raw` keeps the trailing newline the default would trim.

### Modifiers

A **postfix modifier** transforms a value. The operator is `:`, followed by a
readable keyword. This is the zsh history-modifier idea (`:h :t :r :e`) but with
*words instead of cryptic letters*.

There are two kinds of modifier, and the difference matters:

- **Split modifiers** (`:lines :words :nulls :tabs :raw :split`) turn a command
  substitution's **raw byte capture** into a list. They *replace* the default
  newline split and run against the raw bytes — they never run *after* it. Each
  applies to a `$(…)` capture, producing the list.
- **Value modifiers** (path and string — `:stem`, `:dir`, `:strip`, …) transform
  a value, and **map over a list** automatically (applied to each element).

Both kinds:

- **chain**: `$f:stem:stem`, `$(cmd):nulls` then value modifiers over each item.
- **Disambiguation:** `:` is a modifier only when immediately followed by a
  known modifier keyword. `$dir:$PATH` keeps `:` literal (the token after `:`
  is an expansion, not a keyword), so the classic `PATH` construction is
  unaffected.

**Split modifiers** (choose the separator). These bind to a substitution's raw
byte capture and replace the default newline split:

```
$(cmd):lines        # split raw bytes on newlines (explicit form of the default)
$(cmd):words        # split on whitespace runs (opt-in; the old IFS behavior)
$(cmd):nulls        # split on NUL   (find -print0 / xargs -0; newline-safe)
$(cmd):tabs         # split on tab   (TSV)
$(cmd):raw          # no split; raw bytes including the trailing newline
$(cmd):split ":"    # split on an arbitrary separator
```

**Path components** — for `a/b/foo.tar.gz`:

| Modifier | Result | Meaning |
| --- | --- | --- |
| `:dir` | `a/b` | dirname |
| `:base` | `foo.tar.gz` | basename |
| `:ext` | `gz` | last extension (no leading dot) |
| `:stem` | `foo.tar` | basename minus the **last** extension |
| `:root` | `foo` | basename minus **all** extensions |
| `:real` | *(absolute)* | resolved real path |

Rules:

- `:ext` **excludes the dot** (`txt`, not `.txt`) — better for comparisons
  (`if $f:ext == md`). Rebuild with `($f:stem).png`.
- A **leading** dot is not an extension: `.bashrc:ext` is empty, `.bashrc:base`
  and `.bashrc:stem` are both `.bashrc` (dotfiles stay whole).
- `:root` strips *every* dot-suffix, so on a dotted non-extension name like
  `2024.01.report` it yields `2024`. `:stem` (last only) is the safe default;
  reach for `:root` when you mean "strip it all." Controlled peeling is also
  available via chaining (`$f:stem:stem`).

This modifier system is the direct answer to
[fish #4002](https://github.com/fish-shell/fish-shell/issues/4002) ("a
dead-simple way to strip a suffix"): it is a first-class language feature, not a
custom function.

**String** *(open — initial set)*: `:strip PREFIX/SUFFIX`, `:replace OLD NEW`,
and likely `:upper` / `:lower`. To be fleshed out.

### Globbing

- `**` — recursive, **on by default** (no `globstar`-style opt-in).
- `*/`, `**/` — directories (trailing slash, existing muscle memory).
- **Type qualifiers** in `(...)`, borrowed from `find -type` rather than zsh
  punctuation, so "plain file" is the logical `(f)` and not an arbitrary `(.)`:

  ```
  *(f)              plain files            (find -type f)
  *(fx) == *(f x)   executable files       (single letters may concatenate)
  *(d)              directories
  *(l)              symlinks
  ```

  Qualifiers are a **space-separated list, ANDed** together; bare single-letter
  type codes may be run together as sugar. Type letters follow `find -type`
  (`f d l p s b c`) plus `x` (executable), with more predicates to come.

- **Predicate qualifiers** *(open — direction)*: space-separated, comparison
  based, e.g. `*(f size>1M)`, `*(f age<1d)`, `*(f empty)`. Comparisons
  (`>` / `<`) read better than zsh's `+/-` age codes.

- **Exclusion** — a spaced infix `-`:

  ```
  *.txt - *.bak                     # everything but .bak
  **/*.js - **/node_modules/**      # recurse, skip a subtree (.gitignore case)
  *(f) - *.tmp                      # combine with qualifiers
  ```

  **Spaces are required.** Without them, `-` is ambiguous with the dashes that
  fill real filenames and globs (`*-min.js`, `2024-*-report`, `*-backup`).
  Requiring spaces removes that whole class, since nobody writes `foo - bar`
  with spaces in a filename. The only casualty is a lone stdin `-` sitting
  between globs, which is quoted as `'-'`. (This "operators need surrounding
  space" rule is general — every punctuation operator collides with something
  in filenames.)

- **Braces** — kept (`*.{jpg,png}`); universally understood.
- **ksh extended globs** (`!(…)`, `@(…)`, `+(…)`) — **dropped.** Cryptic, and
  their jobs are covered by braces + exclusion.

## Open questions

- **Name** — smash vs mesh (below).
- **`~` as a terse alias for exclusion `-`?** Or keep `-` only.
- **String modifier set** — beyond `:strip` / `:replace`.
- **Predicate qualifier syntax** — confirm `size>` / `age<` / `mtime<` forms.
- **Arrays** — literal syntax, indexing, slicing, append.
- **Functions / variables / export** — declaration and scoping syntax.
- **Hook API** — how override hooks compose over a base (possibly external)
  prompt renderer.

## Name

Not final. Candidates:

- **smash** — distinctive, memorable, unconfusable. Soft collisions only
  (abandoned toy shells; HPE's unrelated SMASH server-management standard).
- **mesh** — no other *shell* claims it (cleaner than smash on that axis), but
  the word is heavily overloaded in infra (service mesh, mesh networking, WiFi
  mesh) and sits one letter away from `mosh` (mobile shell), an adjacent tool —
  a real read-alike / typo risk.

Rejected along the way: `lish`, `lsh`, `sish`, `ish`, `bish`, `sash` (all taken
by real or well-known tools).
