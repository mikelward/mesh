# Design

> **Name: mesh.** (Runner-up: smash.) See [Name](#name). This document often
> just says "the shell".

## What this is

A personal, **interactive-first** Unix shell. The goal is a shell that is a
pleasure to *use* at a terminal all day — not a general-purpose scripting
language, and not a POSIX-compatible `sh`. Where nontrivial logic is needed
(prompt rendering, VCS info), the shell leans on small external binaries (the
`vcs`-style split) rather than growing a heavy scripting layer.

The emphasis is interactive use, but fixing the two things that make today's
interactive shells worse than they need to be:

- **Safer word expansion.** A bare `$x` never word-splits on whitespace or
  silently glob-expands. The default capture splits on newlines and lists stay
  whole (see [Command substitution](#command-substitution) /
  [Spread](#spread--flattening)) — the footgun is opt-*in*, spelled `...`, not
  opt-out via quoting.
- **No backwards-compatibility contortions.** bash arrays are the cautionary
  tale: a genuinely useful feature bolted onto a word-splitting, POSIX-compatible
  base until it takes `"${arr[@]}"` incantations to use without getting burned.
  mesh starts from a clean base instead, so arrays, maps, expansion, and quoting
  are *boring and safe by default* — the point of the [clean
  break](#core-decisions).

### Goals

The overriding goal is **ergonomics** — interactive use comes first (the *e* in
mesh is for *ergonomic*). In service of that, syntax aims to be **familiar,
consistent, and concise** at once: reuse what people already know, make it
compose the same way everywhere, and keep it short. These are *facets* of good
ergonomics, not a ranked checklist — when they pull apart, the tie-breaker is
whichever is better **to use interactively**, decided case by case, not a fixed
precedence among the three.

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

There are four kinds of modifier, and the difference matters:

- **Split modifiers** (`:lines :words :nulls :tabs :split`) turn a command
  substitution's **raw byte capture** into a list. They *replace* the default
  newline split and run against the raw bytes — they never run *after* it. Each
  applies to a `$(…)` capture, producing the list. They apply equally to a
  **plain string value** (`$line:split ":"`, `gets():words`) — there the string's
  own bytes are the input and there is no default split to override; the `$(…)`
  capture is just the most common source. The odd one out is **`:raw`**,
  which lives in the same capture-modifier family but is the *no-split* member:
  it yields the raw bytes as **one string**, not a list (it is what turns the
  default newline-splitting off). So every split modifier produces a list
  *except* `:raw`, whose whole job is to hand back a single byte-string.
- **Value modifiers** (path and string — `:stem`, `:dir`, `:strip`, …) transform
  a value, and **map over a list** automatically (applied to each element).
- **Collection modifiers** (`:len :first :last :rest :init :keys :values
  :has :get :join :dedup`) consume a list or map **as a whole** — they do *not* map element-wise
  — and return either a scalar (`:len` → int, `:join` → one byte-string) or a
  derived collection (`:rest`, `:keys`, `:dedup`). This is the category that answers "how
  long," "the last one," and "flatten to a string." `:join SEP` is the fold
  that turns a list back into bytes (`$dirs:join ":"`); it stringifies each
  element and errors on a nested list or map (there is no implicit deep
  flattening — spell it out). **`:dedup`** returns the list with duplicate
  elements removed — **keep-first, order-preserving**, equality by value — so
  `$env.PATH:dedup` is the guarded, deduped PATH; unlike Unix `uniq(1)` it drops
  *non-adjacent* duplicates and needs no prior sort. It is **pure** (returns a new
  list — `$env.PATH = $env.PATH:dedup` to store) and lists-only. The full list/map
  surface is in [Arrays](#arrays-lists) and [Maps](#maps-associative-arrays).
- **Filter modifiers** (`:files`/`:f`, `:dirs`/`:d`, `:links`/`:l`,
  `:exec`/`:x`) keep the list elements matching a **file-type predicate** and
  drop the rest — a subset, not a transform. They **chain for AND** (`:f:x` =
  executable files) and are the `:` spelling of the glob type qualifiers
  (`*:f` ≡ `*(f)`, see [Globbing](#globbing)); on a glob the engine fuses the
  filter into matching, but they work on any path list too (`$paths:files`).

All four kinds:

- **chain**: `$f:stem:stem`, `$(cmd):nulls` then value modifiers over each item,
  `$xs:rest:last` (collection modifiers compose too).
- **Disambiguation:** `:` is a modifier only when immediately followed by a
  known modifier keyword. `$host:$port` keeps `:` literal (the token after `:`
  is an expansion, not a keyword), so building `host:port`-style strings — or
  any `a:b` construction — is unaffected.

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

The delimiter is a **terminator, not a separator**: **trailing empty fields are
dropped** — any run of delimiters at the very end contributes nothing. So
`find -print0` (which ends every path, including the last, with NUL) yields
exactly the paths — `a\0b\0` → `[a b]` — and a stray blank line at the end of
output never becomes a phantom element. This generalizes the default newline
split's trailing trim. **Interior** empty fields are *kept* (`a\0\0b\0` →
`[a "" b]`), so structure in the middle survives; an **empty capture** — or one
that is nothing but delimiters — is the empty list `[]`. `:words` is the
exception that ignores whitespace entirely — leading, trailing, and runs — so it
never yields empty elements (the classic IFS word-split). `:raw` does not split
at all (it is the [no-split capture member](#modifiers), one byte-string).

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
- **Type qualifiers** — **two equivalent spellings**. The `:`-modifier form
  `*:files` / `*:f` (a readable word *or* the terse letter, exactly like every
  other `:` modifier) is the idiom for the common single-type filter; the
  `(...)` form `*(f)` (from `find -type`, not zsh punctuation) is retained and
  is the general form for ANDed sets and arg-carrying predicates. They coincide
  for a single type:

  ```
  *:f       ==  *(f)         # plain files             (find -type f)
  *:files   ==  *(f)         #   ...spelled out
  *:d       ==  *(d)         # directories
  *:l       ==  *(l)         # symlinks
  *:f:x     ==  *(f x)       # chain for AND: executable files
  **:files  ==  **(f)        # recursive, files only
  ```

  Type letters follow `find -type` (`f d l p s b c`) plus `x` (executable),
  each with a word alias (`files dirs links … exec`); in `(...)` they are a
  space-separated ANDed list (bare letters may run together, `*(fx)`). The `:`
  forms are **filter modifiers** (see [Modifiers](#modifiers)) — they select a
  path list by a file-type predicate, so they also work on a plain list
  (`$paths:files`), and on a glob the engine **fuses** the filter into matching,
  so `**:files` never materializes non-files.

- **Predicate qualifiers** *(open — direction)*: the arg-carrying predicates
  (`size>1M`, `age<1d`, `empty`) stay in `(...)` since they do not fit a bare
  `:word` — `*(f size>1M)`, `*(f age<1d)`. Comparisons (`>` / `<`) read better
  than zsh's `+/-` age codes; whether these also grow `:word arg` modifier
  spellings is folded into this open question.

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

### Variables and assignment

Assignment is `name=value`, the **bash spelling** — the most ingrained shell
reflex, kept. A bare `name=value` (a statement that is *just* that) binds a
variable, unspaced, exactly like bash. The identical `word=value` token as an
**argument** — anywhere after a command word — stays an ordinary literal
(`git commit --author=me`, `env FOO=1 cmd`), so **position** separates
assignment from data, precisely as shell users already expect. No
`set` / `let` / `var` keyword needed.

A **spaced** `name = value` is also accepted, and is the form to reach for when
the value has internal spaces — a list, a glob, an `if` — where the unspaced
form would be awkward to read. Two things mesh does *not* fold in, to stay
unambiguous: bash's prefix-env form (`FOO=1 cmd` in one breath) is written
`env FOO=1 cmd` here, and a bare leading `name=value` is always a *shell*
binding, never a one-command temporary.

```
foo=bar                   # assignment — bash-style, unspaced
n=42
env FOO=1 cmd             # NOT assignment: FOO=1 is a literal arg to `env`
git commit --author=me    # NOT assignment: a k=v arg after the command word

xs = [a b c]              # spaced form for a compound value (list)
files = *.txt             # a glob result (list)
greeting = if $french { bonjour } else { hi }
```

**`$` reads, bare binds or runs.** A leading `$` means *read this variable*
(`$x`, `$f:stem`). A **bare** name is either being *bound* — the left of `=`, a
`for` binder, a function parameter — or, in command position, is a *command or
function to run*. So the same name changes form with what you do to it:

```
f = report.txt            # bind f        (bare, LHS of =)
echo $f                   # read f        ($)
for f in *(f) { … $f … }  # bind f, then read $f  (same as = / $x)

if ready { … }            # run the `ready` command/predicate, branch on status
if $ready { … }           # read the variable `ready`, branch on its bool
```

This is the familiar shell split, kept deliberately: the only names *without* a
`$` are the ones you are defining or the commands you are calling. Its one
hazard — forgetting the `$` and running a command by accident — is softened
because an unknown bareword is a **command-not-found error**, not a silent
misread.

**Names are kebab-case.** Identifiers — variables *and* command/function names
alike — may contain hyphens (`last-cmd-time`, `auto-fetch`, `host-seg`), matching
Unix command names (`ssh-add`, `docker-compose`) and the Lisp tradition. There is
no clash with the minus operator because of the [operators-need-spaces](#globbing)
rule: `-` is subtraction / exclusion *only* with surrounding spaces. So `a-b` is
one name, `a - b` subtracts, and `$a-$b` interpolates the two with a literal
hyphen between — the third payoff of that one spacing rule.

- **Scope — two levels, lexical.** There are exactly two variable scopes: the
  **session-global** scope (top-level rc and interactive bindings) and a fresh
  **function-local** scope per `func` call. The environment (exported names) is
  a separate axis. Scoping is **lexical**: a function sees its own locals, its
  parameters, and the globals — never its *caller's* locals (no dynamic scope,
  the classic shell footgun). Inside a function, `x = 5` binds a **local by
  default**, shadowing any global rather than clobbering it — the deliberate
  inverse of bash's assign-to-global default. To write a session-global from
  within a function, say so explicitly:

  ```
  count = 0                 # global (top level)
  func tick() {
    n = 1                   # a NEW function-local, gone on return
    global count = $count + 1   # explicitly updates the session-global
  }
  ```

  Reading resolves **outward** (local → global); an **unbound** name is an
  **error**, not empty — the always-on `set -u` that the *no null* rule below
  already implies, so a **typo'd read fails loud** (`$staus` → error). The one
  place a typo is *not* caught is **assignment**, which always creates
  (`staus = 5` binds a new var) — the cost of having no `let`/`var` keyword;
  reads carry the fail-loud guarantee, writes create. The **total read** for a
  maybe-unset name is the same `:get`
  that maps use, because the **environment is a first-class map named `env`**:

  ```
  editor = $env:get EDITOR vim  # total: value, or "vim" if unset — never errors
  $env.EDITOR                   # strict: errors if unset (like any $m.key)
  if $env:has SSH_AUTH_SOCK { … }
  ```

  So `$env.EDITOR` (a strict read) errors when unset, and `$env:get EDITOR vim`
  is the safe defaulting form — no new syntax, just the map surface applied to
  the environment.
- **No block scope; `unset` removes a scope's binding.** Control-flow blocks
  (`if` / `for` / `while` / `loop`) do **not** open a new scope, so
  `if c { x = 1 }` then `$x` works and a loop binder is an ordinary binding in
  the enclosing scope (readable after the loop, holding the last value) — the
  model stays two levels, no more. **`unset name`** removes the binding **in the
  current scope**: inside a function it drops the local, and if that local was
  shadowing a global the global becomes visible again (reads resolve outward as
  usual) — so plain `unset` never reaches through to mutate a global, matching
  the `global`-to-escape rule. To remove a session-global from within a function,
  **`global unset name`** (symmetric with `global name = value`). A read errors
  only when the name is unbound in *every* visible scope. `unset x` differs from
  `x = ""`: the latter is *bound to the empty string*, the former *unbound* — the
  two states that stand in for a missing null. **`unset` also deletes a
  collection element** — `unset $m[key]` / `unset $m.key` removes that map entry
  (and `unset $xs[i]` removes the element and closes the gap); deleting a missing
  key is a **no-op**, not an error, so `unset $sh.prompt.auth` is idempotent whether
  or not the segment was registered.
- **Command/function names resolve at call time** — a separate namespace from
  variables. A bare word in command position (`g` inside `func f { g }`) is a
  *command or function* looked up **when `f` runs**, not when `f` is defined. So
  definition order is irrelevant: define helpers in any order, forward-reference
  freely, mutual recursion just works, and an rc file reads top-to-bottom with no
  forward declarations. If `g` is still undefined when `f` actually runs, that is
  the ordinary command-not-found **error** at that point. Only *variable* scope
  is lexical; the value namespace and the command namespace are distinct, as in
  every shell.
- **Export.** `export NAME = value` puts a name in the process environment for
  children. **Only byte-strings can be exported** — the environment is a flat
  `KEY=bytes` table, so a list or map cannot cross an `exec` boundary. Exporting
  a list is an error with a clear message (join it first: `export P =
  $dirs:join ":"`). **The one exception is path-type variables** —
  `$env.PATH` and friends are lists *by design* and the shell **auto-`:`-joins**
  them on export (splitting on read); that is a defined serialization for the
  known `:`-delimited path vars, not a general "lists become strings" rule, so an
  arbitrary list still errors. The path-type set is a **fixed built-in list** —
  `PATH`, `MANPATH`, `CDPATH`, `INFOPATH`, `LD_LIBRARY_PATH`, `PYTHONPATH`, and
  the like — plus an **opt-in** for any other name: **`export --list NAME`** marks
  a name as a `:`-delimited list, so it is split-on-import and joined-on-export
  just like the built-ins (`export --list MY_TOOL_PATH` reclassifies an inbound
  value in place; `export --list MY_TOOL_PATH = [/a /b]` declares and sets). The
  separator is fixed to `:`. *(TODO: consider a dedicated `declare --list NAME`
  spelling instead — it reads as its own statement, at the cost of adding a
  builtin; `export --list` is chosen for now because it needs no new builtin and
  lives exactly where the join-on-export exception already does.)* One further
  restriction: environment entries are
  **NUL-terminated**, so a byte-string containing an embedded NUL (which a
  `$(cmd):raw` capture can) **cannot** be exported either — that too is a hard
  error, not a silent truncation. This keeps the rich types honest: they live
  *in* the shell, and the boundary to external programs is always
  (NUL-free) bytes.

  **Export is a global effect on the `env` map**, not a local-by-default
  binding: `export NAME = value` (even inside a function) writes the session
  environment and **persists after return** — export exists precisely to change
  what *children* inherit, so scoping it locally would defeat the point. This is
  the one deliberate exception to local-by-default, and it is explicit (you typed
  `export`). A plain **local shadow does not touch the environment**: inside a
  function, `PATH = …` binds an in-shell local that only that function sees;
  children still inherit the *exported* `env[PATH]` until you `export` (or
  `global`-assign an already-exported name). For a **temporary** env change
  around a single command, `env NAME=val cmd` stays the idiom; a whole function
  scoping-and-restoring the environment is the deferred *isolation* question
  (see [Open questions](#open-questions)).
- **Types are inferred, not declared.** `x = foo` is a string, `x = [a b c]` a
  list, `x = [a: 1]` a map. There is no type sigil (`@`, `%`) on the *name* —
  a variable just holds whatever value it was given, and `$x` reads it back.
  Perl-style sigils (`@PATH` a list, `$PATH` a scalar) were considered and
  rejected: a variable's type here is the *value's* business, not the name's, so
  a name-baked sigil would lie the moment a var is reassigned a different shape —
  and Perl's context-varying sigil (where `$foo[0]` indexes the array `@foo`) is
  a notorious footgun. `$name` means one thing everywhere: "read this variable."
- **String interpolation.** Inside `"…"`, a bare `$name` interpolates just the
  **variable** — the following `.`/`[` is *literal text*, so `"$file.txt"` is
  `$file` then `.txt` and `"$m.key"` is `$m` then `.key` (the shell reflex). Any
  **member access, indexing, or expression** in a string uses the braced
  **`${…}`** form, which also delimits where it ends: `"${m.key}"`, `"${xs[0]}"`,
  `"${dir}s"`. One rule — unbraced `$name` is the variable, `${…}` is everything
  more — so the two never parse ambiguously. (Outside strings there is no
  ambiguity: `$m.key` / `$xs[0]` are ordinary access.)
- **No null.** mesh has **no `nil`/`null`/`none`** value — the billion-dollar
  mistake is left out. The consequence is a consistent rule wherever a value
  might be absent: **exact** access fails loud (`$xs[99]`, `$m[absent]` are
  errors), **total** access takes a default (`$xs:get i d`, `$m:get k d`), and
  a **control-flow gap** yields the empty string (a no-`else` `if`). Nothing
  silently returns a null that has to be checked for downstream. *(open — the
  one genuine fork this leaves: is a first-class absent value ever worth adding
  back for, e.g., "key present but unset"? Current answer: no; `:has` +
  `:get default` cover it.)*

**Special variables live in two namespace maps** — the *(decided)* way to keep
the shell's built-in state out of your variable namespace. The whole lowercase
top-level is **yours**; the built-ins hang off two reserved roots:

- **`$env`** — the process environment, accessed by name: `$env.EDITOR`,
  `$env.HOME`. **`$env.PATH` is a list** — `$env.PATH += /opt/bin`,
  `$env.PATH:dedup`, `$env.PATH:has /usr/bin` all just work, which is the
  "guarded, deduped PATH" requirement. Because the OS environment is bytes, a
  path-type entry is `:`-joined on the way out and split on the way in (see the
  [export exception](#variables-and-assignment) below); the other built-in path
  vars (`MANPATH`, `CDPATH`, `INFOPATH`, `LD_LIBRARY_PATH`, `PYTHONPATH`, …) are
  lists too, and `export --list NAME` opts any other name in. Path-var splitting is
  **exact** — it keeps *every* empty component (leading, interior, trailing),
  *not* the trailing-empty-trimming [capture split](#modifiers), because an empty
  component is meaningful (`PATH=/usr/bin:` means "…and the cwd") and a
  split→join round-trip must be byte-faithful.
- **`$sh`** — everything else the shell owns, **flat**: runtime values —
  **`$sh.status`** (last exit, int `0`–`255`, the readable replacement for `$?`),
  **`$sh.pipestatus`** (a **list** of the last pipeline's stage statuses, where
  real lists beat bash's `PIPESTATUS`), `$sh.pid`, `$sh.version`, `$sh.options`,
  `$sh.interactive`, **`$sh.jobs`** (the live [job-control](#job-control) map),
  and **`$sh.args`** / **`$sh.name`** (script/positional args as a list, and the
  shell-or-script name — see [Startup](#startup-and-invocation)); **and the
  hooks** — `$sh.prompt`, `$sh.preprompt`,
  `$sh.preexec` / `$sh.postexec`, `$sh.precd` / `$sh.postcd`, `$sh.exit`
  ([Hooks and the prompt](#hooks-and-the-prompt)), the **`$sh.complete`**
  [completion-override](#completion) map, and the **`$sh.signal`**
  [signal-handler](#signals) map.

So there are exactly **two reserved names** (`env`, `sh`); every other lowercase
name is entirely yours — a var called `status`, `prompt`, or `path` never
clashes. Access is strict [map access](#maps-associative-arrays), so `$sh:keys`
lists the whole surface and a mistyped key fails loud.

**Read-only vs. writable within `$sh`.** The **runtime** entries (`$sh.status`,
`$sh.pipestatus`, `$sh.pid`, `$sh.version`, `$sh.interactive`, `$sh.jobs` with
its records, and `$sh.args` / `$sh.name`) are the shell's authoritative state —
**read-only**: assigning or `unset`ting one is an error, so config can't corrupt
an invariant. (`$sh.jobs` changes only through `&` / `fg` / `bg` / `kill` and job
completion, never by mutating the map directly — you still *read* it freely, e.g.
`$sh.jobs:len`.) The **configuration** entries are yours to
write: the hook maps (`$sh.prompt`, `$sh.preprompt`, …), the `$sh.options`
settings map, the `$sh.complete` [completion-override](#completion) map, and the
`$sh.signal` [signal-handler](#signals) map.
(This is the one place the general map rules are constrained — individual keys
carry a mutability flag.)

### Quoting and escaping

mesh has three ways to write a literal plus one escape character, chosen so the
common cases need no ceremony and the rules stay few.

**Bare words are literal** (`x = foo` binds `"foo"`), and a single **backslash
escapes the next character** so one metacharacter can go literal without reaching
for quotes: `cp a\ b dst` (a literal space keeps it one argument), `\*` (a literal
star, not a glob), `\$`, `\#`, `\!`, `\-`. A `\` at end of line is **line
continuation**.

**Single quotes `'…'` are raw** — no interpolation and almost no escapes — so they
are the natural home for regex source and paths (`'\d+\.txt'` is exactly those
bytes). The **only** two escapes are **`\'`** (a literal quote — `'can\'t'` →
`can't`) and **`\\`** (a literal backslash — `'C:\\'` → `C:\`); the `\\` escape is
what lets you put a backslash *before* a quote (`'\\\''` → `\'`) or at the very end
of a string. *Every other* backslash — `\n`, `\d`, `\.` — stays literal, so regex
and path rawness holds.

**Double quotes `"…"` interpolate and escape.** `$name` / `${…}`
[interpolate](#variables-and-assignment), and a **modern C-style escape set**
applies — `\n \t \r \e \\ \" \$` and `\u{1F600}` for Unicode — so `"a\nb"` is two
lines and `"\$5"` is a literal dollar. This is a deliberate break from bash (where
`"\n"` is a backslash-n and you reach for `$'\n'`): mesh needs no `$'…'` form
because double quotes already interpret escapes.

**Adjacent pieces concatenate** into one word — `"$dir"/'sub'/$file` fuses into a
single path and `--flag='some value'` is one argument — so literals and expansions
compose without a `+`.

*(open: a raw form that can itself hold *both* quote kinds — a heredoc or an
`r#"…"#`-style delimiter — for the rare string needing embedded `'` and `"` with
no escaping.)*

### Arrays (lists)

The list is mesh's core value — command substitutions already produce lists
(see [Command substitution](#command-substitution)) and value modifiers already
map over them. This section pins down the *literal*, *indexing*, and *slicing*
surface.

```
xs = [a b c d]            # literal: space-separated, like nushell / elvish
empty = []
one = [solo]             # a 1-element list, never collapsed to a scalar
```

**Zero-based**, always — matching bash/Python/Rust and rejecting zsh's
1-based indexing (the single biggest cross-shell gotcha). Negative indices
count from the end.

```
$xs[0]                    # a           first
$xs[-1]                   # d           last  (negative index)
$xs[1]                    # b
```

**Ergonomic length and ends** are *words*, consistent with the modifier system
— no `${#arr[@]}` and no `$#arr`:

| Form | Result | Notes |
| --- | --- | --- |
| `$xs:len` | `4` | element count |
| `$xs:first` | `a` | same as `$xs[0]` |
| `$xs:last` | `d` | same as `$xs[-1]`; the two spellings coexist on purpose |
| `$xs:rest` | `[b c d]` | all but the first |
| `$xs:init` | `[a b c]` | all but the last |

`last` gets **two spellings** deliberately: `$xs[-1]` for anyone with the
Python/zsh reflex, `$xs:last` for readability and for the case where `$xs` is
itself an expression you don't want to index twice.

**Slices** use ranges. mesh is written in Rust, so it adopts Rust's range
spelling directly — `..` is **half-open** (end-exclusive), `..=` is inclusive:

```
$xs[1..3]                 # [b c]       indices 1,2   (half-open)
$xs[1..=3]                # [b c d]     indices 1,2,3 (inclusive)
$xs[..2]                  # [a b]       first two
$xs[2..]                  # [c d]       from 2 to end
$xs[-2..]                 # [c d]       last two
```

Half-open is the default because `[..n]` then reads as "the first `n`", and
`[i..j]` has length `j - i` — the two properties that make off-by-one bugs
rare. Reach for `..=` when you literally mean "up to and including."

**Empty and out-of-range** — mesh has **no null value**, so every accessor has a
defined result rather than a silent `nil`. The rule follows Python/Rust: exact
access is **strict** (fail loud), range access is **lenient** (clamp), and a
**total** accessor with a default is the ergonomic safe path.

| Access | On empty / out of range | Rationale |
| --- | --- | --- |
| `$xs[i]` (exact index) | **error** | asking for element `i` that isn't there is a bug, not a `""` |
| `$xs:first` / `$xs:last` | **error** on empty | no first/last element exists |
| `$xs:rest` / `$xs:init` | **`[]`** | "all but one" of a 0- or 1-element list is genuinely empty — total, no error |
| `$xs[a..b]` (slice) | **clamped** | `$xs[2..99]` → to the end; `$xs[5..]` on a short list → `[]` (a range is a request, a partial answer is fine) |
| `$xs:get i default` | returns `default` | total, never errors — the safe accessor when absence is expected |

So `$xs[99]` on a 4-element list is an error that names the index, but
`$xs:get 99 "-"` yields `"-"`, and `$xs[1..99]` just runs to the end. Fail loud
where a missing element means a mistake; stay total where absence is normal.

**Build** goes through the spread operator `...` (see
[Spread](#spread--flattening) below), so there is one primitive for assembling
lists:

```
xs = [...$xs e]           # append e
xs = [pre ...$xs]         # prepend
both = [...$a ...$b]      # concatenate
```

**Append in place** is `+=`, terse in the common cases, with no `push` verb and
no unfamiliar operator (a `<<`-style shovel was considered and rejected — not
widely known, and it collides with heredocs). It is defined by **both operands —
the left-hand type first, then the right** — so every combination has one
answer:

| LHS | RHS | `+=` does | Note |
| --- | --- | --- | --- |
| list | list | **extend** by its elements | Python/Ruby `+=` |
| list | scalar or map | **append** as one element | a list may hold any value |
| map | map | **merge** (right side wins on key clash) | |
| map | non-map | **error** | no key to merge a bare value under |
| string | string | **concatenate** | a [styled value](#hooks-and-the-prompt) counts as its text here → plain-string concatenate |
| int | int | **add** | |
| bool | bool | **error** | `+=` has no meaning on bools — use `or` / `and` |
| scalar | mismatched scalar type | **error** | no coercion (`n += "x"` fails) |

```
hosts += web3             # list  += scalar : append one   -> [...$hosts web3]
xs    += [d e f]          # list  += list   : extend by three
xs    += $more            # list  += list   : extend by a list
m     += [key: value]     # map   += map    : insert / update
greeting += "!"           # string += string: concatenate
n += 1                    # int   += int    : add
```

For the common **list** LHS this is the ergonomic rule you'd expect — a list on
the right extends, anything else appends as one element. Why it is safe and not
a bash-style "word or list?" trap: mesh values
are **typed with no coercion** — a scalar `x` and the one-element list `[x]`
are distinct and stay that way — so the dispatch is *determinate and knowable*,
never inferred from whitespace. Two properties follow:

- **The single-append case has no wrong answer.** For a scalar `e`, `xs += e`
  (append) and `xs += [e]` (extend-by-one) both yield `[...$xs e]`. They only
  diverge when the RHS is genuinely a list — which is exactly when you mean
  extend.
- **Nesting stays expressible** by bracketing: `xs += [$ys]` is a one-element
  list whose element is `$ys`, so it appends `$ys` *whole* (one nested
  element), while `xs += $ys` extends and `xs += [...$ys]` forces extend. The
  bracket is the explicit control when a variable's arity is unknown.

This is the **one place the shell flattens by type rather than by an explicit
`...`** — confined to the `+=` right-hand side, type-directed not
whitespace-directed, so it does not reintroduce word-splitting.

*(TODO: consider a symmetric **`-=`** that removes an element — `$hosts -= web3`
deleting the matching element, mirroring how `+=` appends one. Open: remove the
first match or every match; equality by value; whether the right-hand type
dispatches like `+=` (a list RHS removing each of its elements → set-difference,
a scalar removing one), and what a map LHS means (`-= key` dropping that entry,
overlapping with `unset $m.key`). Note this is a value-level remove-by-content,
distinct from `unset $xs[i]`, which deletes by index.)*

*(TODO: consider modifier-form **`:add`** / **`:remove`** (or similar names) as
the **pure** counterparts to the mutating `+=` / `-=` — `$xs:add e` returning a
new list with `e` appended and `$xs:remove e` returning one with the matching
element gone, so they compose in a modifier chain (`$env.PATH:remove /usr/games:dedup`)
and read as expressions rather than statements. Open: the exact names, whether
they mirror `+=`'s type-directed dispatch, and how they line up with the existing
`:map` / `:filter` transforms.)*

### Maps (associative arrays)

A map literal is a bracket literal whose entries are **`key: value` pairs**,
comma-separated. The discriminator between a map and a list is the **pair
syntax**, not the comma — so a singleton `[a: 1]` is unambiguously a map. The
comma is merely the separator *between* entries; the space separates *list*
elements.

```
ports = [http: 80, https: 443, ssh: 22]
one   = [a: 1]            # a map: the `key: value` pair makes it one
empty = [:]               # the empty map  (`[]` is the empty list)
```

Precisely: a `[...]` literal is a **map** iff it contains at least one
`key: value` pair **or is the empty-map form `[:]`**, and then **every** entry
must be a pair — mixing pair and bare-value entries (`[a: 1 lone]`) is an error,
not a hybrid. `[:]` is the sole zero-entry map (a bare `:` standing in for "the
pairs that would be here"); `[]` is the empty list. A list element
that needs a literal colon is quoted (`["http:" 80]`), which also keeps this
rule from colliding with the modifier `:` (only a modifier *keyword* after `:`
triggers a modifier; `key: value` has a value, so it stays a pair).

**Keys are byte-strings**, always — the same type the environment and argv use,
so there is no key-equality question to answer and no list/map keys to compare
structurally. A key in a literal is a bareword or quoted string (`http`,
`"a b"`); a numeric-looking key is just those bytes (`[200: ok]` keys on the
string `"200"`, and `$m[200]` looks up the same); and an interpolation in key
position uses its **string value** (`[$name: 1]`, `$m[$k]`). A non-string value
used as a key — a list or map — is an **error**, not silently stringified. This
keeps maps to the one job an rc file needs: string-keyed lookup tables.

**Duplicate keys** in one literal (`[a: 1, a: 2]`, or interpolated keys that
collide) resolve **last-value-wins, first-position** — the later value is kept
(`2`), and the key stays at the position of its first appearance. That is the
same "right side wins" as `+=` merge, and it keeps insertion order stable so map
iteration is unaffected by a later overwrite. It is never an error, so building
a map by overriding earlier defaults just works.

Access mirrors list indexing exactly — `$m[key]` for a string key is the same
shape as `$arr[0]` for an integer index:

```
$ports[https]             # 443
$ports[https] = 8443      # set / update
```

**Dot sugar.** When the key is a bareword identifier, `$m.key` is sugar for
`$m[key]` — the record-style access every language has, and much nicer for
config-shaped maps and the [hook maps](#hooks-and-the-prompt) below:

```
$ports.https              # == $ports[https]
$config.editor = vim
```

Brackets stay for dynamic or non-identifier keys (`$m[$k]`, `$m["a b"]`). The
dot is an **expression-position** operator only: inside a double-quoted string a
bare `"$file.txt"` is still interpolate-then-literal (the shell reflex), so reach
for `$m[key]` or `${m.key}` when you need a map access *inside* a string.

| Form | Result | Meaning |
| --- | --- | --- |
| `$m:keys` | list | keys (insertion order preserved) |
| `$m:values` | list | values |
| `$m:len` | int | entry count (same word as lists) |
| `$m:has KEY` | bool | membership — the decided spelling |
| `$m:get KEY default` | value | total lookup — `default` when absent |

**Membership is `:has`.** The terser `?` postfix (`$m[key]?`) was considered and
dropped — it fights the "words, not punctuation" grain the modifiers are built
on, and spends a `?` symbol that optional/error-handling will likely want. *(to
do: consider an infix `in` operator — `if https in $ports { … }` — as an
additional, English-reading spelling alongside `:has`; familiar from Python, but
it adds a second way to phrase the same test, so weigh it before adding.)*

**Missing keys** follow the same strict/total split as list access, since mesh
has no null: `$m[absent]` is an **error** (a bad key is usually a typo in
config, and should fail loud, not silently yield `""`), while `$m:get key
default` is the total form that returns `default` when the key is absent, and
`if $m:has key { … }` is the guard. So a dynamic lookup that may legitimately
miss is written `$m:get $name unknown`, never a bare `$m[$name]`.

Insertion order is **preserved** (like Python dict / a `Vec<(K,V)>` behind the
scenes) so `for k in $m:keys` is deterministic — important for an rc file that
builds, say, an ordered alias table.

### Spread / flattening

`...` is the one operator that moves between "a list" and "several arguments,"
in both directions:

- **At a call site**, `...$xs` **explodes** a list into separate arguments.
- **In a signature**, `...name` **collects** trailing arguments into a list.

```
git log ...$flags         # each element of $flags becomes its own argv entry
cp ...$srcs $dest         # spread in the middle is fine
```

This is the crux of mesh's **no-word-splitting** promise: a bare `$xs` passed
to a command stays **one value, a list** — flattening into argv only happens
where you *write* `...`. That inverts the bash default (everything splits unless
you fight it with quotes) into opt-in — the footgun becomes a deliberate
keystroke.

What "stays a list" means depends on where the value lands, because argv for an
external program is bytes, not mesh values:

- **To an in-shell `func`**, the list arrives intact as one parameter — the
  callee sees a real list and can index it, `:len` it, spread it onward.
- **To an external program**, there is no list-shaped argv slot, so passing an
  un-spread list is a **hard error** (`git log $flags` → *"$flags is a list;
  spread it with ...$flags or join it with $flags:join"*). mesh refuses to
  silently pick a separator — that guess is exactly the bash footgun. The two
  explicit outs are `...$flags` (one argv entry per element) and `$flags:join
  SEP` (one byte-string).

The general rule at the bytes boundary — **a value renders to argv iff it has a
*canonical* byte form; if rendering it would require a *guess*, that is an
error**:

| Value | Crosses to argv as | Why |
| --- | --- | --- |
| string (NUL-free) | itself | already bytes |
| int (`$xs:len`, `n = 42`) | decimal digits — `echo $xs:len` → `4` | decimal is canonical, not a choice |
| bool (a switch, a comparison) | `true` / `false` | two fixed spellings, unambiguous |
| styled value (from `style`) | its **text** (attributes dropped), then the string rows apply | a styled value *is* a string with display metadata, so an embedded NUL in its text is the same hard error as below |
| **string with embedded NUL** | **error** | argv entries are NUL-terminated; the OS cannot carry it (same limit as `export`) |
| **list** | **error** — spread or `:join` | no canonical separator (space? tab? `,`?) |
| **map** | **error** — render it explicitly | no canonical flattening at all |

An embedded NUL (which a `$(cmd):raw` capture can hold) is the one place a
*string* fails to cross — argv, like the environment, is NUL-terminated, so it
is a hard error at both boundaries, never a silent truncation.

So `echo $xs:len` prints `4` and `echo $found` prints `true`, but `echo $xs`
(a list) and `echo $m` (a map) are errors that name the fix. The dividing line
is "is there one obviously-right rendering?" — ints and bools have one, a list's
separator and a map's shape do not.

### Destructuring

Binding several names from a list in one step reuses the **list-literal syntax on
the left**. So splitting a string into variables — bash's `read a b c` — is just
*split then destructure*, and there is no monolithic `read` built-in:

```
[user pass uid gid home shell] = $line:split ":"   # a passwd line into fields
[k v]           = gets():split "="                 # read a line, split on =, bind two
[first ...rest] = $args                            # ...rest absorbs the remainder as a list
[a b ...mid z]  = $xs                              # ends pinned; mid is everything between
[_ _ uid]       = $line:split ":"                  # _ discards a field
```

- **`...rest`** absorbs the remaining elements as a list (possibly empty) — the
  variable-length case; it may sit anywhere, with fixed names on either side.
- **`_`** discards that position — the same wildcard [`match`](#matching-match) uses.
- **A length mismatch is an error** unless a `...rest` is present, consistent with
  [no null](#variables-and-assignment): a missing field is a bug, not a silent
  empty. This is cleaner than bash's `read`, where the last variable silently soaks
  up the leftover — here you write `...rest` when you mean it.
- **A failed destructure binds nothing** — shape and length are validated against
  the RHS *before* any name is committed, so `[a b c] = $two_items` errors with
  `a`/`b`/`c` left at their prior values (or unbound), never half-updated. The
  assignment is atomic: all names take their new values or none do.

**The pattern grammar is shared with [`match`](#matching-match).** A bare
destructuring assignment is the *unconditional* use ("I know the shape — bind it");
a **`match` arm** is the *conditional* use — branch on shape or length and bind in
the same step:

```
match $args {
  []            { usage() }                # empty
  [cmd]         { run($cmd) }              # exactly one, bound as cmd
  [cmd ...rest] { run($cmd ...$rest) }     # one-or-more; rest bound
}
```

So destructuring isn't *owned* by `match` — it is one list-pattern grammar, used
bare for the simple case and in a `match` arm when you need to branch.

**Regex captures.** The right-hand side is any list, and `:split` is not the only
way to build one — **`:matches`** runs a regex against a string and hands back its
capture groups, so destructuring names them in one step:

```
[one two]      = $str:matches /(.*) (.*)/          # two groups → two names
[year mon day] = $date:matches /(\d+)-(\d+)-(\d+)/  # an ISO date into fields
[ip]           = $line:matches /\d+\.\d+\.\d+\.\d+/ # no group → the whole match, one element
```

- **Positional groups** come back as a **list**, in order — the parenthesized
  sub-matches only, *not* the whole match — so `[one two] = …:matches /(.*) (.*)/`
  binds exactly the two groups. A pattern with **no** group yields the whole match
  as a one-element list, so `[ip] = …:matches /re/` still binds.
- **An unmatched group keeps its slot as `""`** — a group that didn't participate
  (an optional `(a)?(b)` against `"b"`) contributes an **empty string**, never a
  dropped position, so the list length equals the group count and the following
  bindings don't shift. mesh has no null, so `""` is the placeholder (a group that
  matched empty and one that didn't both read as `""` — distinguish with an
  explicit optional-group guard if you must).
- **Named groups** `(?<name>…)` come back as a **map** keyed by name
  (`m = $str:matches /(?<user>\w+)@(?<host>\S+)/` then `$m.user`); an unmatched
  named group is present with value `""`. This pairs with map destructuring once
  that lands (deferred below).
- **No match yields `false`**, not an empty collection. Matching is a pass/fail
  operation, so on a miss `:matches` returns the bool **`false`** (status `1`) —
  keeping the model's rule that failure is signalled by a `false`, never by the
  *shape* of a value. On a match it returns the capture list (or map).
  That makes the result a **valid condition** on its own:

  ```
  if $str:matches /(.*) (.*)/ { … }      # matched? — false on a miss
  ```

- **Bind and test in one step** by using the assignment *as* the condition — the
  `if let` shape, so the pattern is written **once** and the names are in scope for
  the block:

  ```
  if [one two] = $str:matches /(.*) (.*)/  { puts "$one / $two" }
  if m = $str:matches /(?<user>\w+)@(?<host>\S+)/  { puts $m.user }
  ```

  As a *condition*, `lhs = rhs` tests the RHS's status: a match binds `lhs` and
  enters the block; a miss (`false`) skips it and binds nothing. This isn't
  regex-specific — `if line = gets() { … }` falls out of the same rule, since
  `gets()` returns `false` at EOF. The longer `match`-with-destructuring form is
  there when you want to branch on more than one shape:

  ```
  match $line:matches /(\w+): (.*)/ {
    [key val] { … }      # matched — key/val bound
    false     { … }      # no match
  }
  ```

- **A bare, unconditional bind is an assertion.** `[a b] = $str:matches /…/` with
  no `if` says "I know this matches" — so a miss (`false`, not a two-element list)
  is a **loud error**, the [no-null](#variables-and-assignment) rule again: an
  unconditional bind that silently yielded `a = ""` would bury the bug. Reach for
  the `if` form when a miss is expected; the bare form when it isn't.

This makes `/re/` mesh's one regex story on the *value* side too: `~`
([Tests](#tests-and-comparisons)) answers yes/no, `:matches` extracts the
captures — no `=~`-then-`$BASH_REMATCH` dance.

*(TODO: **name this modifier** before first release. `:matches` reads well in
`[a b] = $s:matches /…/`, but `:match`, `:groups`, and `:captures` are all in the
running.)*

*(deferred: **map destructuring** — `[name: n, age: a] = $m` binding by key — a
natural extension of the same idea; and nested patterns (`[a [b c]] = …`).)*

### Functions

```
func greet(name) {
  echo "hi, $name"
}

greet world               # -> hi, world
```

Paren-delimited, `func name(params) { … }` — C/Go/JS muscle memory, and unlike
Elvish's `{|a b| … }` or Nushell's `def f [a b] { … }` it puts the signature
where a reader already looks for it. Parameters are **named**: inside the body
you reference `$name`, never `$1`. This is the fish `--argument-names` idea
promoted to the declaration itself.

The signature borrows Nushell's/Elvish's proven vocabulary — *positional*,
*optional-with-default*, *flag*, and *rest*:

```
func deploy(env, --region = us-west, --force, --tag = latest, ...hosts) {
  # $env     required positional
  # $region  valued flag,   defaults to us-west
  # $force   boolean switch: true iff --force was passed
  # $tag     valued flag,   defaults to latest
  # $hosts   list of any remaining positionals   (rest / "flattening")
}

deploy prod --force web1 web2
#   env=prod  region=us-west  force=true  tag=latest  hosts=[web1 web2]

deploy prod --region=eu-west --tag=v9 ...$fleet
#   env=prod  region=eu-west  tag=v9  hosts = the spread-in elements of $fleet
```

`region` is a **flag**, not an optional positional, on purpose — with a
`...hosts` rest parameter present, an optional *positional* `region` could not
be skipped (the first host would silently bind to it). That is the general
rule below. An optional positional is fine when it is the last non-rest
parameter and can just be omitted from the right:

```
func tag(image, version = latest) {          # optional positional, no rest
  docker tag $image $image:$version
}
tag app          # version defaults to latest
tag app v9       # version = v9
```

Rules:

- **Positionals** bind left to right. A parameter with `= default` is optional
  and may be **omitted only from the right** — you cannot skip an optional
  positional while still supplying a later positional or a rest element. When
  you need to set a later value but default an earlier one, make the earlier
  one a `--flag`; that skip-ability is the main reason to prefer a flag over an
  optional positional. It follows that an optional positional and a `...rest`
  do **not** usefully coexist (the rest would swallow anything meant for the
  optional), so a signature with `...rest` keeps its positionals required.
- **Flags** are declared with a leading `--`. `--force` (no `=`) is a boolean
  **switch**, false unless passed. `--tag = default` is a **valued flag**, and at
  the call site it accepts **both spellings** — attached `--tag=v2` and separate
  `--tag v2` (the flag consumes the next argument) — the two getopt forms every
  shell user knows. A valued flag with **no value to consume** (nothing follows,
  or the next token is `--`/another flag) is an **error** — a missing value fails
  loud rather than silently swallowing an unrelated token. A **switch** never
  consumes a following argument (`--force web1` leaves `web1` a positional).
  Flags may appear in any order at the call site and are *not* consumed as
  positionals — this is why a shell wants real flag parsing in the signature
  rather than hand-rolled `case $1` juggling. An argument that begins with `--`
  but names **no declared flag** is an **error**, not a silently-forwarded
  positional — a typo'd flag should fail loudly, not vanish into `...rest`.
  When a flag is given **more than once** (directly or via a spread), the
  **last occurrence wins** for a valued flag (`--tag=v1 --tag=v2` binds `v2`, the
  universal CLI convention that makes a forwarded default overridable), and a
  repeated switch is simply still true (idempotent) — neither repeat is an error.
- **`--` ends flag parsing** (the universal Unix terminator, kept). Everything
  after a bare `--` is positional/rest, even if it begins with `--`. This is
  how a value that literally looks like a flag reaches a rest parameter:

  ```
  run --verbose -- --force ./x    # --verbose is run's flag;
                                  # ["--force" "./x"] are positionals -> ...rest
  wrap -- ...$argv                # forward argv verbatim, flags and all
  ```

  A single `--` element produced by a spread (`...$argv` where `$argv` contains
  `--`) terminates parsing the same way; to pass a *literal* `--` as data,
  place it after an earlier `--`.
- **Rest** (`...name`, at most one, last) collects the leftover positionals
  into a list. This is the "flattening" you asked about — the same slurpy/`@rest`
  concept as Raku's `*@rest`, Elvish's `@rest`, Nushell's `...rest`, Tcl's
  `args`.
- **Arguments do not word-split.** A bare list argument passes to an **in-shell
  function** as one list value. External programs take **bytes only**, so an
  un-spread list handed to an external command is an **error** — spread it
  (`...$xs`, one argv entry per element) or join it (`$xs:join ","`, one
  string). The shell never guesses a serialization (see
  [Spread](#spread--flattening)).
- **Result and `return`.** A function's **result is its last expression** —
  evaluated like any block, the same rule as [`if`](#conditionals-if-is-an-expression).
  No explicit `return` is needed to produce it. `return` on its own exits the
  function **early**, carrying the result so far; `return val` exits early
  **with a value**. That is the whole return mechanism — implicit last
  expression, `return`/`return val` for early exit. A function with **no
  expression to yield** — an empty body, or a bare `return` before anything
  ran — results in the **empty string with status `0`**, the same "nothing
  produced, nothing failed" answer a no-`else` `if` gives; there is no null to
  invent.
- **Exit status is a view of the result** — not a separate channel — and it is
  defined for *every* result type, so a function used in command position
  (`if f { … }`) always has one:

  | Result type | Exit status |
  | --- | --- |
  | command | its own exit status |
  | int | the integer itself — `0` success (the shell `return N`) |
  | bool | `true` → `0`, `false` → `1` (the Unix inversion) |
  | string / list / map / styled value (incl. empty) | `0` — producing a value *is* success |

  So `have_command` ends in a test whose bool becomes the status and
  `if have_command fzf { … }` reads correctly; `return $cond` exits `0`/`1`;
  `return 2` exits `2`; and a function that returns a string or a list is a
  success (`0`) when its status is observed. Failure is only ever signalled by a
  command's own status, a `false`, or an explicit nonzero `int` — never by the
  mere *shape* of a returned value.

  A status is the OS's **8-bit** process status, so an out-of-range int is
  **masked to `0`–`255`** (`return 256` → `0`, `return -1` → `255`, matching
  `exit`) — an in-process call and a process-backed one then report the *same*
  status. The full integer survives as the function's **value** (`n = f()`);
  only the *status view* is 8-bit.
- **Output is stdout.** Independently of its result, whatever a `func` writes to
  stdout *is* its output stream, exactly like an external command, so functions
  compose in byte-stream pipes with everything else.

  **Value vs stream — resolved** (see [Calling for a value, and
  lambdas](#calling-for-a-value-and-lambdas)). `return val` / last-expression
  settle how a function *produces* a value; the caller chooses which channel it
  reads **by syntax**: `f(arg)` (parens attached) takes the **return value**,
  `$(f arg)` takes the **stdout bytes**, bare `f arg` runs it. No declaration
  modifier and no context magic — the parens are forced anyway, since a bare RHS
  word is a literal string.

**Prior art surveyed** (all shell-adjacent, all validate the same four
signature roles): Elvish `{|a b &opt=default @rest|}`, Nushell
`def f [a, b?, --sw, --n = d, ...rest]`, fish `function f --argument-names …`,
Raku signatures (`$x = 5`, `*@rest`), Tcl `proc` (`{b 5}`, `args`),
PowerShell `param()` with `[Parameter(ValueFromRemainingArguments)]`. mesh
takes the *semantics* these agree on and dresses them in the `func name(...)`
syntax above.

### Isolation and subshells

**A plain `func` does not isolate process state.** cwd, umask, and the `env`
map are OS process state, not mesh values, so a `func` runs *in the current
process* and its `cd` (or `export`) **persists after return** — exactly like
bash, and exactly what navigation helpers want:

```
func proj(name) { cd ~/work/$name }     # moving your shell is the point
```

The decisive reason to keep persist as the default (over auto-restoring cwd the
way local-by-default does for variables): **it keeps the *process-state*
boundary refactor-safe.** Lift a run of lines out of a function body into a
helper `func` and the `cd`/`export`/umask effects behave identically at the new
call edge — an auto-restoring boundary would silently restore cwd there instead.
(This is only about process state; extracting lines that read a caller-*local*
variable would still break under lexical scope — that is exactly what the
dynamic-scope TODO below is about — and moving a `return`/`break` retargets it,
as in any language.) Isolation is therefore **explicit**, in three grades:

```
( cd build; make )                      # subshell: forks; cwd/env/umask/vars
                                        #   isolated, nonzero exit can't kill
                                        #   the outer shell
func build() ( cd build; make )         # a func whose *body* is a subshell — the
                                        #   `( )` body (vs `{ }`) is the isolation
                                        #   flag (bash/POSIX spell it this way)
in dist { rm -rf * }                    # scoped cwd: run the block there, restore
                                        #   after — NO fork (cheaper than subshell)
```

A **subshell forks**, so — like `export` — only **bytes** cross back out (its
stdout); rich list/map values do not survive the process boundary. `in DIR { }`
does not fork: it is the lightweight "do this over there without stranding me,"
covering the common `pushd`/`popd` pattern with a block.

*(open, deferred cluster: whether a `func` defined inside a `func` is visible
only there. Also a **TODO — dynamic scope**: the same "extract a chunk into a
subfunction" goal that motivates persist would be served further for *variables*
by letting an extracted helper see the caller's locals; worth weighing dynamic —
or opt-in dynamic — scope against the lexical default decided above.)*

### Calling for a value, and lambdas

A `func` has two outputs — the **bytes** it writes to stdout (composes in pipes,
like any command) and the **value** it returns (last expression / `return val`,
a rich list/map/scalar). Which one you get is chosen by **how you write the
call**, not by context — and it *has* to be syntactic, because a bare word on an
assignment RHS is already a [literal string](#variables-and-assignment)
(`x = greet` binds `"greet"`), so reaching a function's value needs an explicit
marker. That marker is **parens attached to the name** (the C/JS/Python call
shape):

| Form | Purpose | Yields |
| --- | --- | --- |
| `f arg` (bare, command-style) | **run it** — for effect or in a pipe | stdout streams; exit status = result-as-status |
| `$(f arg)` | **capture its stdout** (bytes) | a list (or `:raw`, one string) — works on externals too |
| `f(arg)` (parens, attached) | **use its return value** (rich) | the mesh value |

```
config = load-env($path)          # value call: the returned map
n      = add($a $b)               # args are SPACE-separated, exactly like a
                                  #   command call — parens only mean "value call"
deploy(prod --force ...$hosts)    # flags and ... spread work the same way
config = load-config()            # zero args still needs () — bare name is a string
```

Rules:

- **Args inside `f(…)` use the same space-separated grammar as a command call** —
  positionals, `--flags`, `...spread`. The parens add nothing but "take the
  return value"; there is no second argument syntax to learn.
- **The channels are independent.** During `x = f(…)`, whatever `f` writes to
  stdout still goes wherever stdout goes — the value call reads the *return*
  value, it does not capture or suppress output. A well-behaved value function
  simply does not print; one that legitimately does both streams *and* returns.
- **Externals have no return value**, so `grep(foo)` is a **runtime error** that
  points you at `$(grep foo)`. Rich values stay in-shell — the same bytes-only
  boundary as `export` and subshells. (`f` resolves at call time, so this is a
  runtime, not parse, distinction.)

**Lambdas** are then just anonymous functions — the `func` declaration minus the
name, reusing its whole signature grammar (defaults, `--flags`, `...rest`) — and
they are value-called the same way:

```
double = func(x) { $x * 2 }       # a function value bound to a variable
y = $double(5)                    # value-call it through the variable

evens = $xs:filter func(x) { $x % 2 == 0 }
stems = $files:map func(f) { $f:stem }     # :map / :filter / :each take a lambda
```

`func(params) { … }` (over an Elvish-style `{|params| …}`) keeps **one parameter
syntax** for named and anonymous functions, and the transform modifiers
(`:map` / `:filter` / `:each` / `:sort …`) are where lambdas earn their keep,
complementing the auto-mapping value modifiers for the cases a bare modifier
can't express.

### Conditionals: `if` is an expression

`if` **yields a value** — it is an expression, not just a statement (Rust,
Kotlin, Nix). So the same construct that branches control flow also *produces*
the branch's value, which is what lets a value-returning function (the
[structured-return TODO](#functions) above) have a natural body and kills a
whole category of `x = $(if … )` scaffolding.

```
# statement position — run a branch for effect
if have_command fzf {
  bind-key ctrl-r fzf-history
} else if have_command atuin {
  atuin init mesh | source
}

# expression position — the taken branch's value becomes the result
glyph = if connected_remotely { "⇄" } else { "•" }
tag   = if $root { "[root]" } else { "" }
```

Decisions:

- **The condition is a bool or a command.** A boolean value (`$root`, a
  comparison like `$n > 0`, a `:has` test) branches on its truth; a bare
  command branches on its **exit status** (`0` → true), preserving the
  `if grep -q foo file { … }` reflex. This is why the [predicate
  vocabulary](#requirements-carried-over-from-existing-configs)
  (`have_command`, `inside_project`, …) is just commands/functions — they slot
  straight into `if` with no `[ … ]` / `test`.
- **An assignment may *be* the condition** — `if lhs = rhs { … }`, the `if let`
  shape. It tests the RHS's status (a `false` / nonzero fails), and on success
  binds `lhs` for the block; `lhs` may be a name or a `[…]`
  [destructuring](#destructuring) pattern, so `if [one two] = $s:matches /…/ { … }`
  and `if line = gets() { … }` both test-and-bind in one step, with the RHS written
  once. A miss binds nothing. This is the conditional counterpart to a bare
  `lhs = rhs` statement, which stays a loud assertion (a shape/length mismatch
  errors rather than yielding false).
- **No `then` / `fi`.** Brace-delimited blocks, same as `func` bodies; chain
  with `else if`. The POSIX `then`/`elif`/`fi` scaffolding is dropped (clean
  break).
- **The value is the taken branch's trailing expression.** A block evaluates to
  its last expression — a bare value, a `[…]` literal, a `$(…)` capture, a
  value-function call, or a nested `if`. In *statement* position that value is
  simply discarded and any commands in the branch stream to stdout exactly as
  today; the expression behavior is a superset, not a mode switch.
- **A missing `else` yields the empty string.** In expression position, a false
  condition with no `else` produces **`""`** — one concrete value, not a
  context-dependent "empty string or empty list." mesh infers types and does not
  carry a contextual target type back into the branch, so there is nothing to
  pick an empty *list* from; the empty string is the universal shell "nothing"
  that a prompt fragment wants — `tag = if $root { "[root]" }` then `"$tag…"`
  reads a plain empty string when not root (interpolate the *bound value*, not a
  `$(…)` stdout capture, which a statement-position `if` would not feed).
  Both branches (when both exist) are expected to yield the same *shape*; mesh
  does not coerce one to match the other. **Decided: lenient** — a lone `if` is
  a valid expression and the no-`else` case is `""`. (The stricter Rust-style
  alternative — *require* `else` in expression position, lone `if` as statement
  only — was considered and dropped: it buys parse-time "you forgot the else"
  safety but costs the terse `tag = if $root { "[root]" }` one-liner, and
  interactive brevity wins here.)
- **`match`** is the multi-way companion — its own section below.

**Postfix guard.** A single statement may carry a trailing `if` (or `unless`)
guard — the Ruby/Perl statement modifier — for the very common one-line skip:

```
continue if $f ~ *.tmp
release $tag if $tag ~ /^v[0-9]+/
return unless $args:len > 0
```

This is the shortest guarded form. It is deliberately limited to a **single
statement** — no `else`, no block — so the block `if cond { … }` stays the form
for anything larger; the two do not overlap (guard for one-liners, block for
bodies). It pairs naturally with `~` (`continue if $f ~ *.tmp`) and the file-test
modifiers (`skip $p unless $p:exists`).

The deep seam — what a branch's value *is* when its tail is a byte-streaming
external command rather than a mesh value — is the same bytes-vs-values
question as the structured-return TODO, and is tracked there rather than
re-litigated here.

### Matching: `match`

`match` is a pattern-matching switch and, like `if`, an **expression** — it
tests a value against patterns top to bottom, runs the first arm that matches,
and yields that arm's value. It **replaces bash `case`** with less ceremony (no
`in` / `)` / `;;` / `esac`) and it returns a value:

```
kind = match $file {
  *.md | *.markdown   { markdown }     # glob patterns, alternation with `|`
  *.txt               { text }
  /^README/           { readme }       # a /regex/ arm (slash-delimited)
  .git                { special }      # a literal
  _                   { other }        # `_` is the default (the old `*)` )
}
```

Arm patterns, in one vocabulary:

| Pattern | Matches | Notes |
| --- | --- | --- |
| `foo`, `42` | a literal value | exact |
| `*.txt`, `foo*` | a **glob** | fnmatch, same syntax as [Globbing](#globbing) |
| `/re/` | a **regex** | slash-delimited; this is mesh's whole regex story (no separate `=~`) |
| `a \| b` | either | alternation |
| `1..=9` | a **range** | the `..` / `..=` from slices |
| `_` | anything | the default; put it last |

Rules:

- **First match wins**, top to bottom; `_` is the catch-all and conventionally
  last. Whether non-`_`-exhaustive matches must be total is *(open)* — leaning
  lenient (a `match` with no arm hit yields `""`, like a no-`else` `if`).
- **It is an expression**: `x = match … { … }` binds the winning arm's value;
  in statement position the value is discarded and arms run for effect.
- **Regex captures**: on the *value* side this is **settled** — `str:matches /re/`
  returns the groups (positional → list, named → map); see
  [Destructuring](#destructuring). What stays *(open)* is only whether a `/re/`
  **arm** *auto*-binds its groups into the arm body, or whether you reach for
  `:matches` explicitly there too.
- **List-shape patterns** *(settled — see [Destructuring](#destructuring))*: a
  `match` arm may be a list pattern that **binds by position** — a bare element is
  always a **binder** (never a literal to match), with `_` to discard and `...rest`
  for the tail (`[a b]`, `[cmd ...rest]`). Note this differs from a *top-level* arm,
  where a bare word is a literal: inside `[ ]` you are destructuring, so `[start arg]`
  binds both. To *match* a specific element, use an arm **guard**
  (`[verb ...rest] if $verb == "quit"`). Richer element sub-patterns (a literal /
  glob / `/re/` element, or nesting) and **map-shape** patterns (`[k: v]`) stay
  **deferred** until the need is real.

### Tests and comparisons

This is the surface that replaces bash `[[ … ]]` — the pieces a condition needs,
each a plain value expression (usable in `if`, `while`, `match` guards, or bound
to a bool):

- **Compare** with `==` `!=` `<` `<=` `>` `>=`. Comparison is **type-directed**:
  on ints it is numeric, on strings lexical — so mesh needs no `-lt`-vs-`<`
  split (`$n > 5` numeric, `$a < $b` lexical, decided by the operands' types).
- **Pattern-match** with `~` / `!~`: `$f ~ *.txt` is a bool "does the string
  match this glob," and `$f ~ /re/` the regex form — the one-line boolean twin
  of a `match` arm (`!~` negates). This is bash's `[[ $f == *.glob ]]` and
  `[[ $s =~ re ]]`, unified.
- **File tests** are the scalar cousins of the `:files`/`:f` filter modifiers.
  The type/permission axis is words: `$p:type` yields the `find -type` word
  (`file`/`dir`/`link`/…) so `$p:type == dir` is `-d`; `$p:exists` is `-e`;
  `$p:exec` / `$p:read` / `$p:write` are `-x` / `-r` / `-w`. (`-z`/`-n` are just
  `$s == ""` / `$s:len > 0`.) The **binary** file relations `-nt` / `-ot` / `-ef`
  (newer / older / same-inode) are the same comparison family as the
  [predicate qualifiers](#globbing) (`age<`) and are *(open)* alongside them —
  likely `$a:mtime > $b:mtime` and a `$a:same $b` rather than cryptic digraphs.
- **Combine** bools with the words `and` / `or` / `not` (`if $a:exists and not
  $b:exists { … }`). These join *values*; the byte-stream **command** chains
  `&&` / `||` (run-next-on-success/failure, by exit status) are kept separately
  and unchanged — two different jobs that bash blurs.

So `case` → `match`, and the everyday `[[ … ]]` jobs map to a comparison, a `~`
pattern-match, a file-test modifier, or an `and`/`or`/`not` of those — no
special `[[` context, and none of its word-splitting quirks. The stragglers are
tracked, not hand-waved: the binary file relations (`-nt`/`-ot`/`-ef`) sit with
the predicate-qualifier open question, and regex **captures** (bash's
`BASH_REMATCH`) with the `match`-arm capture question above.

### Loops (`for`, `while`, `loop`)

Same brace-delimited shape as `func` and `if` — **no `do` / `done`**. The header
carries no parentheses, Go-style:

```
for f in * {
  …
}
```

Take the loop that motivated this section — "walk a directory, skip the
subdirectories":

```bash
# bash
for f in *; do
  test -d "$f" && continue
  process "$f"
done
```

Two things make that fussier than it should be, and both are things mesh already
fixed elsewhere:

1. `*` **word-splits**, so `$f` *must* be quoted or a filename with a space
   breaks the loop.
2. There is no way to say "only files," so you filter by hand with
   `test -d … && continue`.

`*` is a real list and `$f` is one element that never splits, so the quotes just
go away:

```
# mesh — direct translation, no quoting needed
for f in * {
  if $f:type == dir { continue }
  process $f
}
```

…and the **idiomatic** version deletes the guard, because the glob already
*types* its matches — `(f)` is "plain files," straight from `find -type`
([Globbing](#globbing)):

```
# mesh — filter at the source; the loop body has nothing to skip
for f in *(f) {
  process $f
}
```

That is the ergonomic payoff: the most common reason for a `continue` at the top
of a shell loop (wrong file type) is gone, because filtering lives in the glob.
`continue` and `break` are still there for the cases that need them — kept
as-is, familiar.

**Iterating other things** — anything that is a list, plus maps and ranges,
reusing syntax already defined:

```
for line in $(git status --porcelain) {   # a capture: splits on newlines — safe
  …
}
for k, v in $aliases {                     # a map yields key, value pairs
  alias $k $v
}
for i in 1..=5 {                           # a range: same .. / ..= as slices
  echo $i
}
```

The map form (`k, v`) and the range form need nothing new — they are the `[k:
v]` maps and `..`/`..=` ranges from earlier, showing up where a loop expects a
list.

**Reach for a modifier before a loop when you are *transforming*.** A `for` loop
is for side effects; to *derive* a list you usually do not need one, because
value modifiers already map over a list:

```
stems = $files:stem       # not: stems = []; for f in $files { stems += [$f:stem] }
```

**`while`** is the same shape, with an `if`-style condition (a bool or a
command's exit status); **`loop`** is the infinite form, exited with `break`
(clearer than `while true`, borrowed from Rust):

```
while $queue:len > 0 {
  handle ($queue:first)
  queue = $queue:rest
}

loop {
  if deploy-succeeded { break }   # run until a condition breaks out
  sleep 5
}
```

mesh deliberately keeps a **separate `while`** rather than folding it into `for`
the way Go does: `while` is muscle memory every shell user already has, and
familiarity outranks shaving a keyword. `loop` fills Go's bare-`for {}` niche
without overloading `for`. So three keywords, each doing one obvious thing —
`for` iterates, `while` tests, `loop` repeats.

The one-line skip idiom is the **postfix guard** (`continue if $f:type == dir`),
now decided — see [Conditionals](#conditionals-if-is-an-expression). The
file-test modifiers it leans on (`$f:type` / `:exists` / `:exec`) are settled in
[Tests and comparisons](#tests-and-comparisons).

### Redirection

Redirection is **basically bash** — the operators are too familiar and too
ergonomic to reinvent, and they plumb a command's byte streams, which is
orthogonal to mesh's value model. The same set:

```
cmd > file          # stdout, truncate
cmd >> file         # stdout, append
cmd < file          # stdin
cmd 2> file         # stderr
cmd 2>&1            # stderr to wherever stdout currently goes
cmd &> file         # both stdout and stderr (>& also accepted)
cmd 2>> file        # stderr, append
cmd > /dev/null     # discard
a | b               # pipe: a's stdout to b's stdin (the byte-stream pipe)
a |& b              # pipe stdout AND stderr (shorthand for a 2>&1 | b)
cmd << END … END    # here-document
cmd <<< "text"      # here-string
cmd 3< file         # explicit fd; n>&m dup, n>&- close
diff <(a) <(b)      # process substitution (a filename/fd, bash-compatible)
```

Two mesh notes, neither a behavior change:

- A redirection operator is its **own lexical token**, so it is **exempt from the
  [operators-need-spaces](#globbing) rule** — `cmd 2>&1` and `cmd >file` both
  parse as in bash; the spacing rule is only about word operators like `-`.
- Redirection moves **bytes to/from files and fds** — it does *not* interact with
  the rich value channel. A list or map is not "redirected"; you print it
  (`puts $xs > file`) and the command's stdout is what lands. This is the same
  bytes-vs-values split as [command substitution](#command-substitution) and
  [export](#variables-and-assignment).

*(open: `noclobber` and the `>|` override; whether `&>>` append-both is worth a
spelling.)*

### Job control

Job control is table stakes for an interactive shell, and mesh's one improvement
over bash/zsh is that **jobs are first-class values**, not an opaque table you
reach only through the `%n` sigil and scrape out of `jobs` text.

**`$sh.jobs` is an insertion-ordered map keyed by a small stable job id**, each
value a record:

```
$sh.jobs
# [ 1: [pid: 48213, cmd: "make -j8", state: running, status: ""],
#   2: [pid: 49001, cmd: "vim notes", state: stopped, status: ""] ]

$sh.jobs:len              # 2   — this is `publish-jobs`, now one word in a prompt segment
$sh.jobs[2].state          # stopped
$sh.jobs:values:filter func(j) { $j.state == running }
```

`state` is `running` / `stopped` / `done`; `status` fills in when a job finishes
(the same 8-bit view as [`$sh.status`](#variables-and-assignment)).

**`&` backgrounds and yields a job handle.** `j = make -j8 &` binds the record,
so `$j.pid` is mesh's replacement for bash's `$!` and `$j` is the thing you
`fg` / `kill` / `wait`. A bare `make &` just registers the job in `$sh.jobs`.

**The interactive verbs are the familiar ones:**

| Action | Spelling |
| --- | --- |
| suspend the foreground job | Ctrl-Z → a `stopped` job |
| foreground | `fg` (most recent) · `fg 2` · `fg %2` · `fg $j` |
| resume in background | `bg` · `bg 2` · `bg %2` |
| list | `jobs` (pretty-prints `$sh.jobs`) |
| signal | `kill $j` · `kill $sh.jobs[2]` · `kill %2` — but `kill 49001` is still a **PID** |
| wait for it | `wait $j` |

**Job references — three ways, no ambiguity.** `fg` / `bg` only ever take a job,
so a **bare id** there (`fg 2`) is unambiguous. The **handle** (`$sh.jobs[2]`, or
`$j` from `j = cmd &`) is the value-model reference and is what disambiguates
`kill` from a PID. And the **`%n` sigil is kept as sugar** for muscle memory —
`%2` (by id), **`%+`** / **`%%`** (current job), **`%-`** (previous job), and
`%string` (most recent whose command starts with `string`).

**Completion is reported before the next prompt** (like bash's `[2]+ Done`), and
the finished job's record carries its final `status` at that point before leaving
`$sh.jobs`.

*(deferred past the spike: `disown` / nohup-style persistence past shell exit;
`wait` with no args / multiple jobs and its aggregate status; the fuzzy
`%?string` (substring) reference; per-stage `pipestatus` on a backgrounded
pipeline; and a `jobdone` hook to fire on completion. Terminal plumbing —
process groups, `tcsetpgrp`, `SIGTSTP`/`SIGCONT` — is implementation, not
surface.)*

### Signals

**Interactive defaults** — the shell owns these at the prompt. The *keyboard*
signals never end your session; only a lost terminal (SIGHUP) does:

- **`Ctrl-C` / SIGINT** — at the prompt, **abandon the current input** and draw a
  fresh prompt (never exits the shell). While a foreground command runs, SIGINT
  goes to *that* [job](#job-control)'s process group; the shell stays up and the
  next prompt shows its interrupted [status](#variables-and-assignment).
- **`Ctrl-D` / EOF** — on an **empty** line, exit the shell; on a non-empty line it
  does nothing, so a stray `Ctrl-D` can't drop you mid-command. An
  **`$sh.options.ignore-eof`** setting can require a second press.
- **`Ctrl-Z` / SIGTSTP** — suspend the foreground job to a **stopped**
  [job](#job-control); at an idle prompt (no foreground job) it is **ignored** —
  the interactive shell never suspends itself.
- **`Ctrl-\` / SIGQUIT** — ignored at the prompt; delivered to the foreground job.
- **SIGWINCH** (resize) — the [line editor](#line-editing) reflows and redraws the
  (possibly multi-line) prompt.
- **SIGHUP** (terminal closed) — the shell exits, **SIGHUPs its jobs, then sends
  SIGCONT to any that are *stopped*** (a stopped job can't act on the HUP until it's
  continued; a running job just gets the HUP); **SIGTERM** is ignored interactively
  (as bash does). (A `disown` exemption from the HUP arrives with `disown` itself,
  which is [deferred](#job-control).)

**User handlers are keyed hook maps, not bash's `trap`.** `$sh.signal.<NAME>` is an
insertion-ordered map of named callables — the *same shape* as `$sh.preprompt` and
the other [hooks](#hooks-and-the-prompt), so it is re-source-safe and composable,
with no new `trap` builtin:

```
$sh.signal.INT.note  = func() { puts "interrupted" }
$sh.signal.TERM.save = save-state                 # by name
$sh.signal.USR1.reload = reload-config             # a command/function, late-bound
unset $sh.signal.INT.note                          # remove one
```

Names drop the `SIG` prefix (`INT`, `TERM`, `HUP`, `USR1`, …). **`$sh.exit`** is
the EXIT-pseudo-signal trap (bash's `trap … EXIT`), already defined with the
[hooks](#hooks-and-the-prompt). **`SIGKILL` and `SIGSTOP` can't be trapped** (an OS
rule); assigning a handler for them is an error. A user handler runs *in addition
to* the shell's interactive default where both apply — the shell keeps terminal
control (the line-cancel / redraw still happens) and the handler runs for its
effect. **The handler runs first and the shell's terminal redraw is its final
step** — so any output a handler writes (`puts "interrupted"`) appears *before* the
fresh prompt is drawn, never stranded after it, and the line editor's displayed
buffer / cursor stay consistent (a WINCH handler's output likewise precedes the
reflow). Handlers fire for signals delivered while a script, function, or command
is running, matching where bash traps fire. And — as with `postexec` / `preprompt`
dispatch — **`$sh.status` and `$sh.pipestatus` are snapshotted and restored** across
a handler, so a command the handler runs (that `puts`) can't overwrite the
interrupted foreground status the next prompt reports.

*(deferred: whether a handler may **suppress** a default (e.g. swallow `Ctrl-C`);
exact SIGINT delivery mid-pipeline; and per-signal masking during handler
execution.)*

### Startup and invocation

**Config files** live under `$XDG_CONFIG_HOME/mesh` (default `~/.config/mesh/`),
sourced in order by shell kind — the zsh split, XDG-located and mesh-named:

- **`env.mesh`** — *every* mesh, including non-interactive scripts: environment
  and `$env.PATH` setup. Kept small and fast, because scripts pay for it on
  every run.
- **`login.mesh`** — login shells only, after `env.mesh`: once-per-login setup.
- **`rc.mesh`** — interactive shells, after the above: the *interactive* rc where
  prompt segments, hooks, keybindings, and functions live. This is the file the
  whole design has been targeting.
- **`logout.mesh`** — on login-shell exit.

Order: `env` → (login) `login` → (interactive) `rc`, and `logout` on the way out.

**Invocation & flags** are the familiar surface:

```
mesh                       # interactive shell when stdin is a tty
mesh script.mesh a b c     # run a script; a b c become $sh.args
mesh -c "puts hi" a b      # run a command string; a b become $sh.args
mesh -s                    # read commands from stdin
mesh -i                    # force interactive
mesh -l / --login          # login shell (also sources login.mesh)
mesh --rcfile FILE         # use FILE instead of rc.mesh
mesh --norc                # skip rc.mesh
mesh --version / --help
```

Script and positional args are a **real list**, **`$sh.args`** (`$sh.args:len`
for the count, `$sh.args[0]` for the first — none of `$1` / `$@` / `$#`), and
**`$sh.name`** is the shell-or-script name (bash's `$0`). Both are read-only
runtime entries.

*(deferred: system-wide `/etc/mesh/*` files; mutating positional args
(`shift` / `set --`); and whether a non-login, non-interactive script should skip
`env.mesh` for speed.)*

### Built-ins

The MVP built-in set is deliberately small — most "commands" are external
programs or user functions:

- **Navigation**
  - **`cd [DIR]`** — change directory. No arg → `$env.HOME`; **`cd -`** → the
    previous dir (`$env.OLDPWD`); a *relative* `DIR` that does **not** begin with
    `./` or `../` is searched in `CDPATH`. A **dot-relative** operand (`./child`,
    `../sib`) always resolves from the current directory, never through `CDPATH` —
    the conventional POSIX exemption, so `cd ../` can't jump to a `CDPATH` entry. It
    updates `$env.PWD` / `$env.OLDPWD` and fires the
    [`precd` / `postcd`](#hooks-and-the-prompt) hooks. Logical by default;
    **`--physical` / `-P`** resolves symlinks first. The block form `in DIR { }` is
    the scoped `pushd` / `popd`.
  - **`pwd`** — print the working directory. The shell **maintains the logical cwd
    itself** (updated by `cd` / autocd), so `pwd` reports *that* shell-owned value —
    validated against the real directory and recomputed if a stale or forged
    `$env.PWD` has diverged, so `pwd` can't lie. **`--physical` / `-P`** calls
    `getcwd` for the symlink-resolved path.
  - **Autocd** — a bare word in command position that is a **directory path ending
    in `/`** (`src/`, `../`, `/tmp/`) is a `cd` into it, no `cd` keyword needed. The
    **trailing slash is the signal** — and it's what makes this safe where zsh's
    slashless autocd isn't: a slashless `src` stays an ordinary command lookup (so a
    command that shares a directory's name is never shadowed), and only the explicit
    `src/` means "go there." Because it *is* a `cd`, a relative target honors
    [`CDPATH`](#variables-and-assignment) — `proj/` resolves through `CDPATH`
    exactly as `cd proj` would, and the same **dot-path exemption** applies, so
    `../` and `./sub/` resolve from the current directory rather than a `CDPATH`
    entry. It fires for a **lone** word only (`src/ x` runs
    `src/` as a command); a trailing-slash word whose target isn't a directory is a
    *no-such-directory* error, not command-not-found. On by default —
    `$sh.options.autocd = off` disables it.
- **I/O**
  - **`puts [args…]`** — one order-preserving rule: **render each argument to
    text** — a scalar as itself, a **list** as its elements joined by newlines (a
    list *is* a sequence of lines), a **map** as `key: value` entries joined by
    newlines — then **join the arguments with a single space** and append a trailing
    newline. So `puts a b` → `a b`, `puts $(ls)` → one file per line, and a mixed
    `puts head $xs tail` is fully defined by that rule. `puts` can render rich values
    because it is a **built-in** on real values — an *external* command still needs
    bytes (spread or [`:join`](#spread--flattening)). It takes **no flags** — none of
    `echo`'s `-e` / `-n` reinterpretation, since escapes are resolved by the
    [string literal](#quoting-and-escaping).
  - **`print [args…]`** — identical, but with **no trailing newline** — for partial
    lines and hand-built prompts. The `puts` / `print` pair replaces `echo -n`,
    keeping both flag-free.
  - **`gets [var]`** — read one line from stdin into `var` (trailing newline
    stripped) and return that line as its value. **At EOF it returns `false`**
    (whose [status](#variables-and-assignment) is `1`) and leaves `var` unchanged,
    so `while gets line { … }` terminates. An empty line still reads as a truthy
    `""` — only EOF is `false` — so blank lines don't end the loop. With no `var`
    it just yields the line (or `false`).
- **Formatting** — `style` (produce a [styled value](#hooks-and-the-prompt) for
  the prompt); it must be a built-in because a structured return value can't come
  from an external command.
- **Vars / env** — `export`, `unset`, `global`, and `source FILE` to (re-)load a
  file — re-sourcing your rc is safe because [hooks are keyed](#hooks-and-the-prompt).
- **Jobs** — `fg`, `bg`, `jobs`, `kill`, `wait` ([Job control](#job-control)).
- **History** — `history` (list past commands; `history | grep` is the MVP search —
  see [Interactive history](#interactive-history)).
- **Session** — `exit [status]`.

**No aliases.** mesh drops the alias mechanism entirely: a **function** is just
as terse (`func ll(...args) { ls -l --color ...$args }`), and it composes, scopes,
and takes arguments properly, so there's no second half-language of "short
names." A bare word that is neither a function nor a built-in is a
command-not-found error, never a silently-expanded alias.

### Line editing

The interactive read loop — cursor motion, kill/yank, multi-line editing, history
recall, completion — is built on a **line-editor library**, not hand-rolled,
chosen so the keybinding and completion model stays configurable later. The pick
is **reedline** (nushell's editor, **MIT-licensed**): it already models swappable
keybinding maps (emacs *and* vi), completion menus, hints/autosuggestions, a
syntax-highlight hook, multi-line validation, and pluggable history — so mesh's
future "configure your keys from `rc.mesh`" surface is mostly a matter of exposing
what reedline already has. A deciding factor is **word-boundary editing** — good
word motions and word-kills (`Ctrl-W`, `Alt-B`/`Alt-F`, `Alt-D`) are exactly the
everyday ergonomics that matter, and reedline handles them well where **libedit
is poor** and **readline is workable but not ergonomic**. Both viable candidates
are permissively licensed (reedline and the fallback **rustyline** are MIT); GNU
readline is avoided as GPL.

**MVP: bindings are hardcoded emacs/friendly** — `Ctrl-A`/`Ctrl-E` for line ends,
`Ctrl-B`/`Ctrl-F` and arrows to move, `Ctrl-W` / `Alt-Backspace` word-kill,
`Ctrl-U`/`Ctrl-K` line-kill, `Ctrl-Y` yank, `Alt-.` (Esc + `.`) to insert the
**last argument** of the previous command (repeat to walk earlier commands' last
args; it obeys the same [session selection rule](#interactive-history) as the other
recall motions), `Ctrl-R` reverse history search, up/down for **prefix** history search (a
typed prefix filters the walk; see [Interactive history](#interactive-history)),
`Tab` to complete, `Ctrl-L` to
clear. **Multi-line
continuation** is driven by **parser incompleteness** — the editor asks the
parser whether the buffer is a complete command and, if not, reads a continuation
line — so *every* unfinished form is covered uniformly rather than by an
enumerated token list: an unclosed `{` / `[` / `(` / quote, or a trailing binary
connector (`|`, `|&`, `&&`, `||`) or line-continuation `\`. The editor owns
rendering the [prompt](#hooks-and-the-prompt) segment map and its multi-line
redraw.

*(deferred: exposing the **keybinding config** from `rc.mesh` — the whole reason
for the library choice — plus a vi mode, custom widgets, fish-style
autosuggestions, and syntax highlighting.)* Completion runs *through* the editor's
menu; its model is the next section.

### Completion

Completion has three targets — **files, directories, and command arguments** —
and the distinctive choice is that command-argument completion is
**auto-generated, never hand-written**: no bash/zsh-style completion scripts to
maintain, in the spirit of fish's `--help`/man-page scraping.

**One spec per command, generated for you.** There is a single notion of a
per-command **spec** — its subcommands, flags, and which arguments expect a
file / dir / enum value. A spec is found by a layered resolver:

1. a **curated spec file** if one exists (a drop-in override) —
   `$XDG_DATA_HOME/mesh/completions/` (`$XDG_DATA_HOME` defaulting to
   `~/.local/share`);
2. else a spec **parsed from the command's man page** — *when that page can be
   associated with the resolved executable* (same package / install). It needs
   *no execution*, so it is preferred; but a system page is **not** trusted for a
   `PATH`-shadowing local binary (a project-local `./tool` must not inherit
   `/usr/bin/tool`'s page), which instead falls through to the probe;
3. else a spec **auto-generated from `cmd --help`** — the executing probe, for
   external commands only;
4. else plain **file / dir** completion — the universal fallback.

Both generated specs are **cached** under `$XDG_CACHE_HOME/mesh/completions/`
(`$XDG_CACHE_HOME` defaulting to `~/.cache`), keyed by **the source that produced
them** so each regenerates when *its own* input changes: a `--help` spec by the
binary's path + mtime, a man-page spec by the **selected page's path + mtime**
(plus the `MANPATH` / locale that selected it) — so a docs-only package update or
a `MANPATH`/locale change re-parses rather than serving a stale spec.

Files and dirs are not a separate mechanism; they are the built-in *value types* a
spec's arguments point at (`cd` completes dirs; a `--output FILE` flag completes
files). Every source — curated file, man page, `--help` — writes a spec of the
**same shape**, so there is one format and one resolver.

**In command position (word 0)** completion offers PATH executables, functions,
and built-ins. After that the spec drives it: subcommands, flags (`-x` / `--long`),
a flag's value (file / dir / enum), or a positional file / dir.

**Only external executables are ever run.** The `--help` probe applies solely to a
resolved external binary; the shell never executes a **function** or **built-in**
to learn its arguments — it introspects them. In fact mesh gives **every function
a canned `--help`**, auto-generated from its declared **parameter signature** (its
positionals, `--switch` / `--flag`s, and `...rest`, see [Functions](#functions))
and emitted in the *same format the `--help` parser reads* — so `ll --help` prints
a real usage message **and** completion reads that same spec, both without running
the function. A function extends the generated help with a **docstring** (a
leading string in its body) for per-argument descriptions; the signature alone is
the zero-effort default. Built-ins ship their specs the same way. This is why the
[command-position](#completion) sources — functions and built-ins — need no probe.

The canned help never overrides the function's own contract: it is synthesized
**only when the signature does not itself claim `--help`** (a function that
declares a `--help` switch keeps it), and the `--` terminator still wins — a
literal `--help` after `--` reaches the body as data (`ll -- --help`), never the
auto-help. So the synthesized help fills the gap only where the function hasn't
spoken for the name.

**Generation is lazy.** A spec is generated the first time you complete
*arguments* for a command with no spec yet, then cached, so later Tabs never
regenerate. The man-page parse is tried first because it runs nothing; the
`--help` probe is the executing fallback.

**On executing `--help`:** it fires only at *argument* completion — after you have
already typed the command name and a space — so you have signaled intent to run
that command, and reading its `--help` is within that intent (you would have run
`cmd --help` yourself otherwise), not a surprise execution. It is still bounded:

- **stdin from `/dev/null`**, so a command that reads input can't hang the prompt;
- a **short timeout** with kill, and an **output-size cap**;
- an **opt-out denylist** for commands whose `--help` isn't safe, plus a global
  off switch **`$sh.options.complete.probe = off`** for anyone who wants *zero*
  implicit execution (curated specs, man pages, and file / dir still work);
- **conservative parsing** — recognize the `-x` / `--long` / `--long=VAL` /
  subcommand-table shapes; if parsing yields nothing, silently fall back to
  file / dir.

(`--help` is side-effect-free by near-universal convention, and clap / cobra /
argparse output is regular enough to parse — the bet fish makes; the
man-page-first order and the off switch cover the rest.)

**Override hook.** The **`$sh.complete`** map — keyed by command, each value a spec
*or* a callable returning candidates — overrides or augments the auto-generated
spec, matching the keyed-map pattern used for [hooks](#hooks-and-the-prompt).
Auto-generation stays the zero-config default; this is where a *dynamic* completer
(git branches, a live PID list) goes.

*(deferred: the exact spec-file format; the function-docstring format; dynamic
value providers; recursive per-subcommand `--help` probing; and shared/remote spec
repos. The match/menu UI itself is the [line editor](#line-editing)'s.)*

### Interactive history

This is the history **store and recall**; the history *expansion* syntax
(`!!` / `^old^new`) is specified in [History expansion](#history-expansion) below.

**The store is SQLite** at `$XDG_STATE_HOME/mesh/history.db` (`$XDG_STATE_HOME`
defaulting to `~/.local/state` — history is per-machine *state*, not cache or
config). A flat history *file* would force `grep` for everything; a small database
gives structured columns now and real search later, without committing to a query
UI yet.

**Every entry is rich, and the [hooks](#hooks-and-the-prompt) already populate it**
— history is just a built-in consumer of `preexec` / `postexec`, no new machinery:

| Column | Filled at | From |
| --- | --- | --- |
| `command` | `preexec` | the command line **after history expansion** — what actually ran, so `!!` never stores literally `!!` |
| `cwd` | `preexec` | `$env.PWD` at submit |
| `tty` | `preexec` | the session's terminal |
| `session` | `preexec` | the interactive session id |
| `start` | `preexec` | submit timestamp |
| `duration` | `postexec` | how long it ran |
| `status` | `postexec` | the [exit status](#variables-and-assignment) |

**Recall** is the [line editor](#line-editing)'s, reading from this store, with two
motions: **`Ctrl-R`** does reverse *substring* search, and **up/down do prefix
search** — with a prefix already typed, `Up` walks the most recent commands that
*start with* it (an empty buffer just steps chronologically). So typing `git ` then
`Up` cycles your recent `git …` lines — the friendly default.

**Recall and expansion draw from your session plus finished history.** `Up`,
`Ctrl-R`, `Alt-.`, and the `!!` / `!$` / `!string` expansions all select from one
view: **this session's own rows together with every completed row from sessions
that are no longer live** — the full persisted history, *minus* the in-flight
commands of other **currently-live** sessions. So a fresh session still recalls
everything earlier sessions saved, while a command running *right now* in another
terminal never becomes your "previous" command. (Once that terminal exits its rows
become finished history and join the view; a mode that also pulls in *live* peers'
commands is a deferred opt-in.) The store stays **shared** — `history` lists and
searches across every session regardless.

**The MVP surface is a `history` built-in** that lists entries (newest last), and
**`history | grep foo`** is the MVP search — the whole point of a real store is
that richer queries (by cwd, by exit status, by time) can come later without
changing how entries are written. So `list | grep` is enough to ship. Only the **current session's own in-flight command** is excluded from what
`history` lists: its row is *recorded* at `preexec` (to capture `start` / `cwd` /
`tty`) but hidden until it completes, so `history | grep foo` never matches its own
pipeline. A row left incomplete — its owning session no longer live — is
**finalized at startup** (a null `status` / `duration`) rather than hidden forever,
so no real command is lost. **Liveness** is tracked by a per-session **lock
file** — `$XDG_STATE_HOME/mesh/sessions/<id>.lock` — on which the session holds an
**exclusive OS advisory lock** for its lifetime; the `sessions` record stores that
path plus the session's `pid` + boot time (an identity a recycled PID can't
counterfeit). A session is *live* iff its lock file's lock is still held, so startup
recovery finalizes an incomplete row only when the owning session's lock is unheld —
a still-running session's in-flight row is never mistaken for a crash.

*(deferred: an atuin-style fuzzy / interactive search over the columns; a
`$sh.history` value accessor for scripting; cross-session and cross-host sync;
the dedup policy; secret redaction; and import from bash/zsh history files.)*

### History expansion

For quick keyboard recall mesh keeps bash's `!` history expansion — but
**interactive-only and quote-safe**. It is a pre-parse pass that rewrites the input
line *before* parsing and runs **only in an interactive shell** (a script never
expands `!`), so it can never surprise non-interactive code. It reads from the
**same selection view** as the other [recall motions](#interactive-history) — this
session's rows plus finished (non-live) sessions' — so a fresh session's `!!` still
finds your last command, while another *live* terminal's commands never become your
`!!`.

- **`!!`** — the previous command line.
- **`!string`** — the most recent command that *starts with* `string`
  (`!git` → your last `git …`).
- **`!$`** — the last argument of the previous command *line*. Because expansion
  reads the stored history (not the current input), it refers to a *separately
  submitted* line: run `mkdir foo`, then on the next line `cd !$` → `foo` (not the
  same-line `mkdir foo; cd !$`, where `mkdir foo` isn't in history yet).
  (`!*` for all args, and `!n` / `!-n` by index, are natural extensions — deferred.)
- **Substitution** — two spellings: the terse **`^old^new`** for the everyday
  "fix my last command" (line-start; previous command), and a general
  **`:old=new`** modifier on *any* history reference (`!!:foo=bar`,
  `!git:foo=bar`). The `old=new` form reads as a *mapping* rather than importing
  sed's `s///` (which mesh uses nowhere else), and it **chains** like every other
  mesh `:` modifier — `!git:foo=bar:x=y` applies both in order. Replacement is
  **global** — every occurrence. The separator is the first *unquoted* `=`; for a
  pattern with spaces or a literal `=` / `:`, **quote each side**
  (`!git:"old thing"="new thing"`) or **backslash-escape**
  (`!git:old\ thing=new\ thing`). `^old^new` is just shorthand for `!!:old=new`.

**The `!` clash is resolved lexically:** `!` introduces an expansion only when
immediately followed by a **supported designator** — `!` (→ `!!`), `$` (→ `!$`), or
a word character (→ `!string`). A digit, `-`, or `*` does **not** activate
expansion in the MVP (they are reserved for the deferred `!n` / `!-n` / `!*`), and
neither do `=` / `~` (the operators `!=` / `!~`) or a lone `!` — all left literal. Two safety wins over bash: expansion happens **only unquoted** —
*both* single and double quotes make `!` literal (bash expanding `!` inside double
quotes is a classic footgun) — with `\!` to escape and a
**`$sh.options.histexpand = off`** switch to turn it off entirely.

### Hooks and the prompt

The requirement (from [Requirements](#requirements-carried-over-from-existing-configs)):
the prompt may be rendered by an external binary, *provided* override hooks — the
`ssh-add` "no identity" warning, a `[root]` tag, the session nag — can **layer
on top**, and **hooks compose, they do not replace each other**.

mesh models a hook point as an **insertion-ordered [map](#maps-associative-arrays)
of named callables** — the key is the handler's *identity*. That one choice
solves the composition requirement and the worst hook footgun at once:

- **Re-source-safe by construction.** `$sh.preprompt.git = …` is *keyed*, so running
  your rc file again **replaces** the `git` handler instead of stacking a
  duplicate — the bane of bash `PROMPT_COMMAND` (which appends) and zsh's
  `add-zsh-hook` (which needs manual dedup). The identity is what lets you
  re-source freely.
- **Update or drop one by name** — reassign `$sh.preprompt.git`, or `unset $sh.preprompt.git`
  — without touching the others; `$sh.preprompt:keys` introspects.
- **Deterministic order** — maps preserve insertion order, so handlers run
  (and segments render) in the order registered.
- **Compose, never replace** — adding a key leaves every other handler intact.

A handler value is a **command name or a callable**: a bareword is a string that
names a command/function run late-bound (matching the [command
namespace](#variables-and-assignment)), or a `func(){ … }` lambda for inline
logic.

**Event hooks** run for effect at named events, in symmetric `pre`/`post` pairs
plus the singletons — `preprompt` (before each prompt), the command pair
**`preexec`** (before a command runs, given the command line) / **`postexec`**
(after it finishes, given the command, its **exit status**, and **duration**),
the directory pair **`precd`** (before the cwd changes, still in the old dir,
given the target) / **`postcd`** (after, now in the new dir, given the previous
dir), and `exit`:

```
$sh.preprompt.jobs   = publish-jobs                    # by name
$sh.postcd.fetch  = func() { vcs auto-fetch & }     # arrived in a new dir — the PWD-gate is now the event itself
$sh.precd.save    = func(to) { save-dir-state }     # about to leave: act while still in the old dir
$sh.preexec.timer = func(cmd) { timer-start }       # start the clock…
$sh.postexec.timer = func(cmd, status, ms) { global last-cmd-time = $ms }   # …stop it; `global` so it survives to feed the prompt
unset $sh.preprompt.jobs                               # remove one
```

The `pre`/`post` split (rather than a single after-the-fact hook) is what lets a
handler run *before* the transition — save state before leaving a dir, start a
timer before a command — separately from the after-work. The `preexec` /
`postexec` pair in particular is how the prompt's **last-exit status** and
**command timing** (both required dashboard fields) get fed without special
casing.

**Command hooks fire for the outer interactive command only.** `preexec` /
`postexec` fire once for the command line you submit at the prompt — *not* for
commands run inside a function, a script, a `$(…)`, or a hook handler itself, and
a handler's own commands don't re-fire them. Without this, `$sh.preexec.timer`'s
`timer-start` would dispatch `preexec` again forever.

**Directory hooks fire around each actual `cd`** — `precd` *before* the
`chdir` (so it genuinely runs in the old dir, even for a `cd` inside a navigation
`func`), `postcd` *after* (in the new dir) — with the same guard that a `cd`
performed *by a hook handler* doesn't re-dispatch. A `func` that `cd`s internally
therefore fires them per change; if a handler only cares about net movement it
gates on `$env.PWD` itself (the one-line `precd`/`postcd` PWD-check that today's
config hand-rolls). Per-`cd` is the right default because `precd`'s "old dir"
contract can't hold if the hooks are deferred to function return. The pending
`cd` target is **resolved to an absolute path *before* `precd` runs**, so a
handler that itself `cd`s elsewhere (allowed — its change just doesn't
re-dispatch) can't make a *relative* outer `cd` land somewhere unintended.

**Status is snapshotted across hook dispatch.** The submitted command's exit
status (and pipeline stage statuses) are captured before `postexec` / `preprompt`
run, and **`$sh.status` and `$sh.pipestatus` are restored** to them for the
prompt segments — so a segment always sees the *interactive command's* status,
never the status of some command a handler happened to run. (`postexec` also
gets the status as an explicit `status` argument.)

**The prompt** is the same shape — a named, insertion-ordered segment map — but
each segment is a callable that **returns a renderable** — a plain string *or* a
styled value (below) — or `""` to contribute nothing; the shell renders the
non-empty ones in key order:

```
$sh.prompt.host = host-seg                     # named, ordered segments
$sh.prompt.dir  = func() { if inside-project() { "$(vcs prompt-info)" } else { tilde-pwd() } }
$sh.prompt.auth = func() { if ssh-id-missing() { style("no-ssh-id" --fg yellow) } }   # no else → "" → omitted
$sh.prompt.dir  = my-dir-seg                   # swap ONE segment by name
unset $sh.prompt.auth                          # drop the auth warning
```

(The segments use `if` *expressions* to pick a string — not `and`/`or`, which
combine bools, not values — and the `auth` segment leans on the decided
no-`else`-yields-`""` rule so "not applicable" is just an empty contribution.)

**Color comes from a `style` helper, not raw escapes.** The value call
`style("no-ssh-id" --fg yellow --bold)` returns a **styled value** — text and
style attributes kept apart — rather than baked-in ANSI. It is an ordinary value
call, so it takes attached parens and `--flag` arguments like any other; a *bare*
`style …` would run it in command position and yield a status, not the value
(hence the parens in the example above).

This falls out of the general [`$(…)`-vs-`()` split](#calling-for-a-value-and-lambdas):
**`()` yields a structured value, `$(…)` yields raw output.** A **renderable** is
therefore one of two things:

- a **styled value** (from a `()` call to `style`) — text and attributes kept
  separate, so the shell measures display width from the text *and* can strip or
  re-theme the styling (needed for the later transient/collapsed form); or
- a **plain string** — which may carry its own ANSI escapes, as an external
  renderer captured with `$(vcs prompt-info)` does (externals have no return
  value, so the renderer necessarily comes in through the output lane). The shell
  measures visible width by **skipping SGR (color/style) sequences** — the
  `ESC [ … m` family, which are genuinely zero-width — treating them as opaque and
  un-restylable. A plain string that emits **cursor-positioning or other non-SGR
  control** sequences is *outside* the width contract: those move the cursor, so
  the shell can't treat them as zero-width, and a prompt segment is expected to
  produce styled text, not drive the cursor.

So width is accurate either way for the styling (SGR) case — the reason to prefer
`style` is that structured attributes stay *restylable*, which raw escapes are
not. A renderable whose
**text** is empty contributes nothing — a plain `""` or `style("" --fg yellow)`
alike, since emptiness is judged by the payload text (not emitted as bare control
codes). `style` is the one styling primitive in the MVP (color + bold).

A styled value is **not a new scalar type** — it is a **string carrying display
attributes**. Everywhere *except* prompt rendering it behaves exactly as its
text: the same [argv](#spread--flattening) rule (its text crosses, an
embedded NUL is the same hard error), the same [`+=`](#arrays-lists) (it
concatenates as its text, yielding a plain string — attributes are
rendering-only and don't survive), the same comparisons and string
interpolation. **Only the prompt renderer reads the attributes**; every other
context sees a string. So `style` adds presentation metadata to a string without
minting a type that must be defined at each boundary. *(A richer per-fragment
"styled spans" value — where concatenation preserves each fragment's own style —
is a possible later iteration; the MVP keeps one attribute set per string.)*

**A segment may render more than one line.** The shell assembles the segments
into a single prompt buffer and treats a **newline as a line break wherever it
appears** — so one callable can emit an entire multi-line prompt (a
`preprompt`-style blob is just a segment that returns a string with newlines in
it), and there is **no line-count setting**: the line structure emerges from the
newlines in the output. The renderer therefore measures width **per line** and
tracks how many lines the prompt occupies, placing the input after the last one
so redraw, completion, and resize stay correct.

The payoff is the requirement, met directly: **the external base renderer is
just one named segment** (`$(vcs prompt-info)`), sitting among peers, so
`[root]`, the auth warning, and the session nag compose *around* it rather than
being swallowed by it — the failure mode of "set `$PROMPT` to one big external
command." This is exactly the hand-rolled `preprompt` / `prompt_line` /
`host_info` / `auth_info` structure of today's config, promoted to first-class,
keyed, re-source-safe segments — with its *side effects* (a background fetch)
moving to the `$sh.preprompt` event hook and its *rendering* to this segment map.

*(MVP is the above: keyed segments, `style` color, and multi-line output.
Deferred to a later iteration — all layered on the same per-line width the styled
values already give the shell: a full-width **rule**, a **`fill`** spacer for
right-aligned segments, **transient collapse** of past prompts in scrollback, and
whether `newline` / `fill` / `rule` get a **structural-segment** spelling. Line
structure itself stays emergent from newlines, not a line-count knob. The event
set — `preprompt`, `preexec`/`postexec`, `precd`/`postcd`, `exit` — is settled.)*

## Open questions

- **Name — decided: mesh** ([Name](#name)); smash was the runner-up.
- **Exclusion `~` alias** — resolved by elimination: `~` / `!~` is now the
  **pattern-match** operator ([Tests and comparisons](#tests-and-comparisons)),
  so glob exclusion keeps the spaced infix `-` only.
- **String modifier set** — beyond `:strip` / `:replace`.
- **Predicate qualifier syntax** — confirm `size>` / `age<` / `mtime<` forms.
- **History expansion — decided** ([History expansion](#history-expansion)):
  interactive-only, quote-safe `!!` / `!string` / `!$` (with `!*` / `!n` deferred);
  the `!` clash resolved lexically (a designator must follow, so `!=` / `!~` and a
  lone `!` are untouched); both quotes make `!` literal, `\!` escapes, and
  `$sh.options.histexpand = off` disables it. Substitution is a chainable,
  **global** **`:old=new`** modifier on any history reference (`!git:foo=bar:x=y`;
  quote each side or backslash-escape for spaces / specials), with **`^old^new`**
  as shorthand for `!!:old=new`.
- **Interactive history (store & recall) — decided**
  ([Interactive history](#interactive-history)): a **SQLite** store at
  `$XDG_STATE_HOME/mesh/history.db` with rich per-entry columns
  (command / cwd / tty / session / start / duration / status) populated by
  `preexec` / `postexec`; recall via up/down and `Ctrl-R`; a `history` built-in
  plus `history | grep` as the MVP search. Remaining: fuzzy search, a
  `$sh.history` accessor, cross-session sync, dedup policy, and secret redaction.
- **Interactive signals — decided** ([Signals](#signals)): interactive defaults
  (`Ctrl-C` abandons the line / interrupts the foreground job but never kills the
  shell; `Ctrl-D` EOFs on an empty line; `Ctrl-Z` suspends; `SIGWINCH` redraws;
  `SIGHUP` exits, `SIGTERM` ignored). User handlers are the keyed **`$sh.signal.<NAME>`**
  hook maps (no bash `trap`), with `$sh.exit` as the EXIT trap. Remaining: whether
  a handler may suppress a default, and mid-pipeline SIGINT delivery.
- **Core surface** (arrays / maps / functions / `if` / `match` / loops / scope /
  tests / isolation) — sketched above. Remaining sub-questions: an infix **`in`**
  operator as a second membership spelling alongside `:has`; whether a `/re/`
  `match` **arm** auto-binds its **captures** (the value-side `:matches` extractor
  is settled — see [Destructuring](#destructuring)); and whether non-`_` `match`
  must be **exhaustive**
  (leaning lenient → `""`). Decided this pass: **`match`** replaces `case`
  (literal/glob/`/regex/`/range/`_` arms; no single-arm sugar — `~` covers the
  one-test case); **tests** replace `[[ ]]` (`~`/`!~` pattern-match, type-directed
  comparisons, `$p:type`/`:exists`/`:exec` file tests, `and`/`or`/`not` vs command
  `&&`/`||`); the **postfix guard** `stmt if/unless cond` is the one-line form;
  **isolation** is explicit — plain `func` persists cwd/state, `( )` /
  `func f() ( )` subshell-isolate, `in DIR { }` scopes cwd without forking.
- **Value calls & lambdas — decided** ([section](#calling-for-a-value-and-lambdas)):
  `f(arg)` (parens attached, space-separated args) takes a function's **return
  value**, `$(f arg)` its **stdout**, bare `f arg` runs it; stdout streams during
  a value call (independent channels); externals have no return value (runtime
  error → `$(…)`). Lambdas are `func(params) { … }` (anonymous, one param
  grammar), passed to `:map` / `:filter` / `:each`.
- **Remaining function questions** — whether a **`func` defined inside a `func`**
  is visible only there; and a **TODO — dynamic scope**: the "extract a chunk
  into a subfunction" goal that fixed cwd as *persist* would be served further by
  letting an extracted helper see the caller's locals — weigh dynamic (or opt-in
  dynamic) scope against the lexical default.
- **Hook API — decided** ([Hooks and the prompt](#hooks-and-the-prompt)): hook
  points are insertion-ordered maps of named callables (the key is the handler's
  identity → re-source-safe, individually removable). Events `preprompt`,
  `preexec`/`postexec`, `precd`/`postcd`, `exit`; the prompt is a named, ordered
  segment map with the external renderer as one peer segment. Prompt MVP: `style`
  color plus multi-line output (a newline is a line break wherever it appears, so
  one callable may emit the whole prompt). Remaining: the frame surface layered on
  per-line width — a full-width rule, a `fill` right-align spacer, transient
  collapse, and an optional structural-segment spelling for `newline`/`fill`/`rule`.
- **Structured prompt — explore next** ([Hooks and the prompt](#hooks-and-the-prompt)):
  the MVP renders a flat segment map with newlines-as-line-breaks. The next
  iteration is the *structured* model — **structural segments** (`newline` /
  `fill` / `rule`) as keyed peers of the content segments, giving the shell
  first-class line boundaries for per-line **right-alignment** (`fill` spacer), a
  **full-width rule**, and **transient collapse** — all without a line-count knob
  (structure stays emergent from the segments). Weigh this keyed-structural-segment
  shape against a list-of-lines (which buys explicit lines at the cost of
  positional, non-keyed rows).

## Name

**mesh.** No other shell claims the name — the cleanest option on that axis. Two
tradeoffs accepted: the word is heavily overloaded in infra (service mesh, mesh
networking, WiFi mesh), and it sits one letter from `mosh` (mobile shell), an
adjacent tool, so there is a real read-alike / typo risk.

Runner-up: **smash** — distinctive and unconfusable, but with soft collisions
(abandoned toy shells; HPE's unrelated SMASH server-management standard).
Rejected along the way: `lish`, `lsh`, `sish`, `ish`, `bish`, `sash` (all taken
by real or well-known tools).
