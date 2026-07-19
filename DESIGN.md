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

There are three kinds of modifier, and the difference matters:

- **Split modifiers** (`:lines :words :nulls :tabs :raw :split`) turn a command
  substitution's **raw byte capture** into a list. They *replace* the default
  newline split and run against the raw bytes — they never run *after* it. Each
  applies to a `$(…)` capture, producing the list.
- **Value modifiers** (path and string — `:stem`, `:dir`, `:strip`, …) transform
  a value, and **map over a list** automatically (applied to each element).
- **Collection modifiers** (`:len :first :last :rest :init :keys :values
  :has :get :join`) consume a list or map **as a whole** — they do *not* map element-wise
  — and return either a scalar (`:len` → int, `:join` → one byte-string) or a
  derived collection (`:rest`, `:keys`). This is the category that answers "how
  long," "the last one," and "flatten to a string." `:join SEP` is the fold
  that turns a list back into bytes (`$dirs:join ":"`); it stringifies each
  element and errors on a nested list or map (there is no implicit deep
  flattening — spell it out). The full list/map surface is in
  [Arrays](#arrays-lists) and [Maps](#maps-associative-arrays).

All three kinds:

- **chain**: `$f:stem:stem`, `$(cmd):nulls` then value modifiers over each item,
  `$xs:rest:last` (collection modifiers compose too).
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

### Variables and assignment

Assignment is `name = value`, **with surrounding spaces** — the same
"operators need space" rule that governs glob exclusion (see
[Globbing](#globbing)). That single rule pays off again here: the spaces are
what separate an *assignment* from a *command with a `k=v` argument*, so mesh
needs no `set` / `let` / `var` keyword for the common case.

```
x = hello                 # assignment: bind x
env FOO=1 cmd             # NOT assignment: `env` runs with a literal `FOO=1` arg
files = *.txt             # binds the glob result (a list) to `files`
n = 42
```

- **Scope.** Bindings are **function-local by default.** A name assigned
  inside a `func` body does not leak out. Top-level (rc-file) bindings are
  session-global. *(open: an explicit `local` / block-scoped form, if we ever
  want a name narrower than the enclosing function.)*
- **Export.** `export NAME = value` puts a name in the process environment for
  children. **Only byte-strings can be exported** — the environment is a flat
  `KEY=bytes` table, so a list or map cannot cross an `exec` boundary. Exporting
  a list is an error with a clear message (join it first: `export P =
  $dirs:join ":"`). One further restriction: environment entries are
  **NUL-terminated**, so a byte-string containing an embedded NUL (which a
  `$(cmd):raw` capture can) **cannot** be exported either — that too is a hard
  error, not a silent truncation. This keeps the rich types honest: they live
  *in* the shell, and the boundary to external programs is always
  (NUL-free) bytes.
- **Types are inferred, not declared.** `x = foo` is a string, `x = [a b c]` a
  list, `x = [a: 1]` a map. There is no type sigil (`@`, `%`) on the *name* —
  a variable just holds whatever value it was given, and `$x` reads it back.
- **No null.** mesh has **no `nil`/`null`/`none`** value — the billion-dollar
  mistake is left out. The consequence is a consistent rule wherever a value
  might be absent: **exact** access fails loud (`$xs[99]`, `$m[absent]` are
  errors), **total** access takes a default (`$xs:get i d`, `$m:get k d`), and
  a **control-flow gap** yields the empty string (a no-`else` `if`). Nothing
  silently returns a null that has to be checked for downstream. *(open — the
  one genuine fork this leaves: is a first-class absent value ever worth adding
  back for, e.g., "key present but unset"? Current answer: no; `:has` +
  `:get default` cover it.)*

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

**Append in place** is `+=`, and it **dispatches on the right-hand side's
type** — the two common cases both stay terse, with no `push` verb and no
unfamiliar operator (a `<<`-style shovel was considered and rejected — not
widely known, and it collides with heredocs):

- **scalar RHS → append** it as one element (the `.append` you reach for
  interactively);
- **list RHS → extend** by its elements (Python/Ruby `+=`);
- **map RHS → merge** into a map (right side wins on key collisions).

```
hosts += web3             # scalar: append one       -> [...$hosts web3]
xs    += [d e f]          # list:   extend by three
xs    += $more            # list:   extend by a list
m     += [key: value]     # map:    insert / update
```

Why this is safe here and not a bash-style "word or list?" trap: mesh values
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
`key: value` pair, and then **every** entry must be a pair — mixing pair and
bare-value entries (`[a: 1 lone]`) is an error, not a hybrid. A list element
that needs a literal colon is quoted (`["http:" 80]`), which also keeps this
rule from colliding with the modifier `:` (only a modifier *keyword* after `:`
triggers a modifier; `key: value` has a value, so it stays a pair).

Access mirrors list indexing exactly — `$m[key]` for a string key is the same
shape as `$arr[0]` for an integer index:

```
$ports[https]             # 443
$ports[https] = 8443      # set / update
```

| Form | Result | Meaning |
| --- | --- | --- |
| `$m:keys` | list | keys (insertion order preserved) |
| `$m:values` | list | values |
| `$m:len` | int | entry count (same word as lists) |
| `$m:has KEY` | bool | membership *(open: `:has` vs a `?` postfix)* |
| `$m:get KEY default` | value | total lookup — `default` when absent |

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
  **switch**, false unless passed. `--tag = default` is a **valued flag**.
  Flags may appear in any order at the call site and are *not* consumed as
  positionals — this is why a shell wants real flag parsing in the signature
  rather than hand-rolled `case $1` juggling. An argument that begins with `--`
  but names **no declared flag** is an **error**, not a silently-forwarded
  positional — a typo'd flag should fail loudly, not vanish into `...rest`.
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
- **Exit status.** A function has an exit status just like an external command,
  and it is the **status of the last command the function ran** (kept from shell
  tradition — familiar, and exactly what makes a predicate work: `have_command`
  is a `func` whose last command is the existence test, so `if have_command fzf
  { … }` reads its status straight out). `return` exits the function early with
  the current status; `return N` (or `return $cond`) exits with an explicit
  status, `0` = success. This status channel is **separate** from the
  value/output channels below — a function can stream bytes *and* carry a
  success/fail status, which is why predicates need no special declaration.
- **Return / output.** A plain `func`'s *stdout is its output*, exactly like an
  external command, so functions compose in byte-stream pipes with everything
  else.

  **TODO — structured return.** Some functions want to hand a real list/map
  back to an in-shell caller without a bytes round-trip (`config = load-env()`).
  The current lean is to make this a **function-declaration modifier** rather
  than an in-body `return <value>` statement — i.e. the *signature* declares
  "this returns a mesh value," so the two kinds of function are distinguishable
  at the call site and by the reader:

  ```
  func fetch(url) { curl -sS $url }          # streams bytes (a command)
  <mod> func load-env(path) { … a map … }    # returns a mesh value
  ```

  Open sub-points: (a) the modifier keyword — `raw func` was floated, but `raw`
  already means *unsplit bytes* as a modifier (`$(cmd):raw`), i.e. the opposite
  of "rich value," so it would need a different word (`val`/`pure`/`fn`/`ret`
  are candidates); (b) whether a value-function may still stream to stdout or is
  value-only; (c) how its value-returning body reads — which is where
  [`if` as an expression](#conditionals-if-is-an-expression) below does a lot
  of the work.

**Prior art surveyed** (all shell-adjacent, all validate the same four
signature roles): Elvish `{|a b &opt=default @rest|}`, Nushell
`def f [a, b?, --sw, --n = d, ...rest]`, fish `function f --argument-names …`,
Raku signatures (`$x = 5`, `*@rest`), Tcl `proc` (`{b 5}`, `args`),
PowerShell `param()` with `[Parameter(ValueFromRemainingArguments)]`. mesh
takes the *semantics* these agree on and dresses them in the `func name(...)`
syntax above.

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
  and is what an interpolation like `prompt = "$(if $root { "[root]" })…"` wants.
  Both branches (when both exist) are expected to yield the same *shape*; mesh
  does not coerce one to match the other. **Decided: lenient** — a lone `if` is
  a valid expression and the no-`else` case is `""`. (The stricter Rust-style
  alternative — *require* `else` in expression position, lone `if` as statement
  only — was considered and dropped: it buys parse-time "you forgot the else"
  safety but costs the terse `tag = if $root { "[root]" }` one-liner, and
  interactive brevity wins here.)
- **`match` later, not now.** A `match`/`case` expression is the obvious
  companion but is deferred — `if`/`else if` covers the rc-file need first.

The deep seam — what a branch's value *is* when its tail is a byte-streaming
external command rather than a mesh value — is the same bytes-vs-values
question as the structured-return TODO, and is tracked there rather than
re-litigated here.

## Open questions

- **Name** — smash vs mesh (below).
- **`~` as a terse alias for exclusion `-`?** Or keep `-` only.
- **String modifier set** — beyond `:strip` / `:replace`.
- **Predicate qualifier syntax** — confirm `size>` / `age<` / `mtime<` forms.
- **Arrays / maps / functions / `if`** — core surface now sketched above.
  Remaining sub-questions: `:has` vs a `?` membership postfix; and a `local`
  binding narrower than function scope. (Decided: expression-`if` with no
  `else` yields `""`; in-place append/merge is `+=`.)
- **Structured return** *(TODO, leaning decided)* — a plain `func` outputs
  stdout bytes; a **function-declaration modifier** (keyword TBD — not `raw`,
  which is taken) marks a function that returns a rich list/map to an in-shell
  caller without a bytes round-trip. Sub-points tracked in
  [Functions](#functions).
- **`match` expression** — the companion to expression-`if`; deferred until the
  rc-file need is real.
- **Hook API** — how override hooks compose over a base (possibly external)
  prompt renderer.

## Name

**mesh.** No other shell claims the name — the cleanest option on that axis. Two
tradeoffs accepted: the word is heavily overloaded in infra (service mesh, mesh
networking, WiFi mesh), and it sits one letter from `mosh` (mobile shell), an
adjacent tool, so there is a real read-alike / typo risk.

Runner-up: **smash** — distinctive and unconfusable, but with soft collisions
(abandoned toy shells; HPE's unrelated SMASH server-management standard).
Rejected along the way: `lish`, `lsh`, `sish`, `ish`, `bish`, `sash` (all taken
by real or well-known tools).
