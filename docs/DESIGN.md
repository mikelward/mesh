# Design

> **Name: mesh.** (Runner-up: smash.) See [Name](#name). This document often
> just says "the shell".

## What this is

A personal, **interactive-first** Unix shell. The goal is a shell that is a
pleasure to *use* at a terminal all day, and a language you are still happy to
be in when the one-liner grows into a script — but not a POSIX-compatible `sh`.

Interactive use sets the priorities, and the same syntax has to hold up when it
is saved to a file: the two are the same language, and neither is a
second-class dialect of the other. That means fixing the two things that make
today's shells worse than they need to be:

- **Safer word expansion.** A bare `$x` never word-splits on whitespace or
  silently glob-expands. A capture is one string until a split is spelled, and lists stay
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
- **Good for scripting too.** What you type at the prompt is what you save to a
  file: funcs, lists, maps, `match`, and the strict/soft error pairs are the
  same language either way. A script gets no extra constructs and pays no
  interactive tax — the features that make a line safe to type (no word
  splitting, loud absence, real values) are exactly the ones that make a script
  safe to leave running unattended.

### Non-goals

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
  `connected_remotely`, and friends. Named here as the `shrc` spells them, since
  the requirement is to replace those 41 guard sites; mesh's own answers are
  [`:kind` / `:where`](#modifiers) and kebab-case funcs like
  `inside-project`.

## Language sketch

Everything below is **decided** unless marked *(open)*.

### Command substitution

A command substitution **captures the command's raw output bytes** and becomes
**one string**, trailing newlines trimmed:

```
$(cmd)          # the output, trailing newlines trimmed -> one string
"$(cmd)"        # the same string; quoting a capture changes nothing
$(cmd):lines    # split on newlines -> list      (alias :ls)
$(cmd):nulls    # split on NUL *only* -> list    (alias :ns; find -print0, newline-safe)
$(cmd):raw      # the raw bytes, unsplit, trailing newline intact
```

**Nothing splits implicitly, captures included.** That is the same promise that
makes a bare `$x` safe, applied at the same boundary, and it is what keeps the
shape a caller wanted readable from the line rather than inferred from what the
command happened to print. It also keeps the common capture — a path, a hostname,
a branch name — free of ceremony: `cd $(git rev-parse --show-toplevel)` is a
string reaching argv like any other.

**A newline-split default was considered and rejected.** It is the dominant Unix
convention and it makes `for line in $(cmd)` correct with no modifier, which is a
real pull. Two things decided against it. First, it taxes the common case: scalar
captures outnumber list-wanting ones several times over in this repo's own docs,
and a list neither interpolates into `"…"` nor reaches an external command
un-spread, so `cd $(…)` and `"$(…)"` would both need ceremony. Second, and
decisively, the loop that motivates it is **not a silent failure here**: a `for`
over a value that is not a list is [refused](#loops-for-while-loop) and names
`:lines`. With the quiet wrong answer loud, the argument for an implicit
split is only ergonomic — and it is outweighed by the ergonomics of the case that
is actually more common.

*(Splitting a capture is therefore always explicit. A split modifier still binds
the **raw** bytes rather than the trimmed value, so `:nulls` sees NULs and nothing
else and a `find -print0` name holding a newline survives; see
[Modifiers](#modifiers).)*

### Modifiers

A **postfix modifier** transforms a value. The operator is `:`, followed by a
readable keyword. This is the zsh history-modifier idea (`:h :t :r :e`) but with
*words instead of cryptic letters*.

There are four kinds of modifier, and the difference matters:

- **Split modifiers** (`:lines :words :nulls :tabs :split`) turn a command
  substitution's **raw byte capture** into a list. They *replace* the bare
  capture's trailing-newline trim and run against the raw bytes — they never run
  *after* it. Each applies to a `$(…)` capture, producing the list. They apply
  equally to a **plain string value** (`$line:split(":")`, `gets():words`) —
  there the string's own bytes are the input and there is no trim to replace;
  the `$(…)` capture is just the most common source. The odd one out is
  **`:raw`**, which lives in the same capture-modifier family but is the
  *no-split* member: it replaces the trim with nothing at all, handing back the
  raw bytes as **one string** rather than a list.
- **Value modifiers** (path and string — `:stem`, `:dir`, `:stripend`, …) transform
  a value, and **map over a list** automatically (applied to each element).
- **Collection modifiers** (`:len :first :last :rest :init :keys :values
  :has :get :join :dedup :prepend :append :extend`) consume a list or map **as a whole** — they do *not* map element-wise
  — and return either a scalar (`:len` → int, `:join` → one byte-string) or a
  derived collection (`:rest`, `:keys`, `:dedup`). This is the category that answers "how
  long," "the last one," and "flatten to a string." `:join(SEP)` is the fold
  that turns a list back into bytes (`$dirs:join(":")`); it stringifies each
  element and errors on a nested list or map (there is no implicit deep
  flattening — spell it out; the modifier to spell it *with* is the open `:flat`
  under [Spread](#spread--flattening)). **`:dedup`** returns the list with duplicate
  elements removed — **keep-first, order-preserving**, equality by value — so
  `$env.PATH:dedup` is the guarded, deduped PATH; unlike Unix `uniq(1)` it drops
  *non-adjacent* duplicates and needs no prior sort. It is **pure** (returns a new
  list — `$env.PATH = $env.PATH:dedup` to store) and lists-only. The full list/map
  surface is in [Arrays](#arrays-lists) and [Maps](#maps-associative-arrays).
- **`:repr`** stands apart from all of these: it does not transform a value, it
  **writes one down** — the mesh source you would have typed to get it back, as a
  string. It takes *any* type rather than a category, and its contract is
  **round-trip, not display**: parsing the result yields **the same value, and of
  the same type**. Both halves are required — equality alone would admit `1.0`
  written as `1`, since [`1 == 1.0`](#arithmetic), and `1` and `1.0` divide
  differently. That is
  what forces a string to be quoted even when it would read as a bare word (`42`
  vs `'42'`) and keeps `[]` and `[:]` apart. That is what distinguishes it from
  [`puts`](#builtins), which *displays* a collection — one element or `key: value`
  per line — and so cannot tell `42` from `'42'` or `[]` from `[:]`. Reach for
  `:repr` when you need to know what you have rather than read it. A collection
  still has no **argv** form, so an external command needs a spread or a
  [`:join`](#spread--flattening). The types with **no** literal form are refused by name rather than
  approximated (a stream handle, a function, a glob — writing the pattern back
  would re-glob it — and, until its flags round-trip, a regex); an approximation
  would read back as a different value, which is the one thing `:repr` must not
  do. It is the writer half of the [subshell value channel](#isolation-and-subshells).
- **`:pretty`** is that same literal **laid out over lines**, two-space indent per
  level, for the sizes where one line stops being readable — a whole `$env`, a
  config map, anything a few levels deep. **Every** collection breaks, with no
  size threshold: "short values stay inline" would mean you cannot tell which form
  you get without counting characters, and the compact form already has a name.
  A scalar and the empty `[]` / `[:]` have nothing to put between the brackets and
  are written as they are. The round-trip contract is **unchanged**, and that is
  what makes the layout safe here — the brackets and commas still say where each
  value starts and ends, so the indentation is decoration over a spelling that
  already parsed, and the refusals are `:repr`'s refusals by the same name.
  [`puts`](#builtins) could not do this: it quotes nothing, so there the layout
  would be the only thing carrying the structure. It is a **separate name** rather
  than a flag on `:repr` because `:repr` keeps meaning *one line* — `$a:repr ==
  $b:repr` compares two values, and a one-line literal is what you paste back. The
  indent matches the one `puts` uses for nesting, so the read-it and read-it-back
  forms of a value line up.
- **Filter modifiers** (`:files`/`:f`, `:dirs`/`:d`, `:links`/`:l`,
  `:exec`/`:x`) keep the list elements matching a **file-type predicate** and
  drop the rest — a subset, not a transform. They **chain for AND** (`:f:x` =
  executable files) and are the `:` spelling of the glob type qualifiers
  (`*:f` ≡ `*(f)`, see [Globbing](#globbing)); on a glob the engine fuses the
  filter into matching, but they work on any path list too (`$paths:files`).

All four kinds:

- **chain**: `$f:stem:stem`, `$(cmd):nulls` then value modifiers over each item,
  `$xs:rest:last` (collection modifiers compose too).
- **No-argument modifiers are bare; arguments are parenthesized.** A modifier that
  takes **no** argument is written bare and chains by adjacency — `$f:stem:dir`,
  `$xs:rest:last`, `:dedup`, `:values` — never `:first()`. A modifier that **takes
  arguments** uses **parentheses**, comma-separated inside like a
  [value call](#calling-for-a-value-and-lambdas): `:split(":")`, `:get(EDITOR, vim)`,
  `:get(99, "-")`, `:match(/re/)`. One form, no exceptions — a **regex** argument is
  just a `/…/` literal sitting inside the parens like any other value — so there is
  no load-bearing whitespace to trip over and chaining is always unambiguous:
  `$host:split("."):first` reads exactly one way.
- **Disambiguation:** `:` is a modifier when immediately followed by a bare
  **identifier** — by shape, not by whether that name is one mesh knows, since
  [declaring a modifier](#modifiers) means the parser cannot hold the list.
  `$host:$port` keeps `:` literal (the token after `:` is an expansion, not an
  identifier), so building `host:port`-style strings from values is unaffected; a
  literal `host:port` word is the case the reservation does claim, and the escapes
  are quoting the word or bracing the name (see
  [Bare words and quoted values](#bare-words-and-quoted-values--decided)).

**Split modifiers** (choose the separator). These bind to a substitution's raw
byte capture, replacing the trim that a bare capture would have applied:

```
$(cmd):lines        # split raw bytes on newlines (the line-loop case)
$(cmd):words        # split on whitespace runs (opt-in; the old IFS behavior)
$(cmd):nulls        # split on NUL *only* (find -print0 / xargs -0; newline-safe)
$(cmd):tabs         # split on tab   (TSV)
$(cmd):raw          # no split; raw bytes including the trailing newline
$(cmd):split(":")    # split on an arbitrary separator
```

The delimiter is a **terminator, not a separator**: **trailing empty fields are
dropped** — any run of delimiters at the very end contributes nothing. So
`find -print0` (which ends every path, including the last, with NUL) yields
exactly the paths — `a\0b\0` → `[a b]` — and a stray blank line at the end of
output never becomes a phantom element. It is the same rule the bare capture's
trailing-newline trim follows. **Interior** empty fields are *kept* (`a\0\0b\0` →
`[a "" b]`), so structure in the middle survives; an **empty capture** — or one
that is nothing but delimiters — is the empty list `[]`. `:words` is the
exception that ignores whitespace entirely — leading, trailing, and runs — so it
never yields empty elements (the classic IFS word-split). `:raw` does not split
at all (it is the [no-split capture member](#modifiers) — one byte-string, with
its trailing newline intact).

*(Implementation status.* The whole family is built: `:split(SEP)`, `:words`,
`:lines`, `:nulls`, `:tabs` and `:raw` — the fixed-separator members carrying the
aliases `:ls`, `:ws`, `:ns`, `:ts` — along with the raw-capture binding that makes
them bind the bytes rather than the trimmed value.

They refuse a **list** subject rather than mapping element-wise, since a split
consumes one string; `$lines:map(:words)` is how a list of lines is taken apart.
Note this contradicts the `$lines:words` written in the [`:flat`](#spread--flattening)
TODO, which assumes the auto-mapping every *value* modifier does. That question is
open for the family as a whole and should be settled once rather than by whichever
member lands next.)*

**Path components** — for `a/b/foo.tar.gz`:

| Modifier | Result | Meaning |
| --- | --- | --- |
| `:dir` | `a/b` | dirname |
| `:base` | `foo.tar.gz` | basename |
| `:ext` | `gz` | last extension (no leading dot) |
| `:exts` | `tar.gz` | **all** extensions (no leading dot) |
| `:stem` | `foo.tar` | basename minus the **last** extension |
| `:bare` | `foo` | basename minus **all** extensions |
| `:real` | *(absolute)* | resolved real path |
| `:url` | *(absolute)* | `file://host/path` URL |
| `:ancestors` | `[a/b/foo.tar.gz a/b a]` | the path, then every directory above it |

Rules:

- `:ext` **excludes the dot** (`txt`, not `.txt`) — better for comparisons
  (`if $f:ext == md`). Rebuild with `($f:stem).png`.
- A **leading** dot is not an extension: `.bashrc:ext` is empty, and `.bashrc:base`,
  `.bashrc:stem`, and `.bashrc:bare` are all `.bashrc` (dotfiles stay whole).
- `:base` splits into `:bare` + `:exts` (first dot); `:base` also splits into
  `:stem` + `:ext` (last dot) — `foo.tar.gz` is `foo`+`tar.gz` or `foo.tar`+`gz`.
- `:url` is for handing a path to something that takes a URL rather than a path —
  `link($report:base, $report:url)`. It carries the **host**, which is what lets a
  terminal tell a local file from one inside an `ssh` session, and percent-encodes
  everything a reader could misread (a space would end the URL; `#` would start a
  fragment). It shares its encoder with `OSC 7`, so the shell and the terminal name
  a file the same way.
- `:url` absolutizes a relative path against the shell's directory without
  resolving symlinks, so it works for a file that does not exist yet — that is the
  split with `:real`, which asks the filesystem and can fail.
- `:url` **refuses a `..`**, because the two ends do not agree on what one names.
  RFC 3986 §5.2.4 has a URL reader remove dot segments *before* opening anything,
  while the kernel follows each symlink first and applies `..` to wherever it
  landed: for `a/link/../report` the reader opens `a/report` and the shell means
  the sibling of `link`'s target. Both can exist, so emitting the path would hand
  out a link to the wrong file. `:real:url` is the spelling that resolves it. A
  `.` needs no refusal — removing it names the same file either way. The empty
  string is refused too, rather than quietly meaning the current directory.
- `:bare` strips *every* dot-suffix, so on a dotted non-extension name like
  `2024.01.report` it yields `2024`. `:stem` (last only) is the safe default;
  reach for `:bare` when you mean "strip it all." Controlled peeling is also
  available via chaining (`$f:stem:stem`). `:bare` is one letter from `:base`
  (basename, extensions **kept**) — the mnemonic is *bare* = stripped down.
- `:ancestors` is the **upward walk** `find_up`, project-root detection and
  `rootdir` each write by hand as a `cd ..`-in-a-subshell loop: `pwd():ancestors`
  is `[/a/b/c /a/b /a /]`, so the search becomes a plain list iteration over
  `pwd()` — the *validated* shell-owned cwd, not the possibly-stale `$env.PWD`.
  It **includes the path itself**, because that is where those searches start
  looking and a list that skipped it would have every caller putting back the path
  they already had; `:rest` is the strict "above me" reading. It includes the `/`
  root for the same reason — a marker file can be there, and stopping short would
  make the walk's own end the one directory it could not answer for. A **relative**
  path stops at its first component (`x/y` → `[x/y x]`) rather than stepping off
  the front into a path with no spelling, and the empty string walks nothing at
  all (`[]`). Like `:dir` and unlike `:real` it is **lexical** — no component has
  to exist, a `..` is a step it reports rather than resolves, and `:real:ancestors`
  is the resolved walk. It takes **one path**, not a list: one path already answers
  with a list, so mapping element-wise would nest a walk per element, and
  `$paths:map(:ancestors)` is the spelling that wants that. The rejected name was
  `:parents`, which reads as excluding the path itself — the one thing the walk
  must include.

*(TODO — a decision surfaced porting real `PATH` / `find_up` code:*
- ***Transform-vs-predicate overlap.*** Keeping directories is the settled
  `:dirs` / `:d` filter modifier; the open question is only the footgun sitting
  next to it — `:dir` is *dirname* (a transform), so `$paths:filter(:dir)` silently
  keeps **everything** (a dirname is always a truthy string) when `$paths:dirs` (the
  directory **filter** modifier) was meant. Decide whether a transform modifier
  surfacing as a predicate's truthy value should be a **loud error** rather than a
  quiet keep-all.)*

This modifier system is the direct answer to
[fish #4002](https://github.com/fish-shell/fish-shell/issues/4002) ("a
dead-simple way to strip a suffix"): it is a first-class language feature, not a
custom function.

**Name resolution** *(proposed — the predicate vocabulary)*. The
[goals](#goals) name `have_command` and friends, and a port of a real `shrc`
finds 41 sites reaching for them — nearly all guarding a tool's setup
(`if have_command fzf`). They are all one question, *what does this name resolve
to*, which is the shape [`:type`](#modifiers) already has for paths, so they get
the same spelling:

```
$name:kind          # keyword | builtin | func | external | false
$name:where         # an external's path, or false
```

One primitive answers the whole family, which is why it is a modifier rather than
five predicates: `have_command` is `$x:kind != false`, `is_builtin` is
`$x:kind == builtin`, `is_function` is `$x:kind == func`, `is_command` is
`$x:kind == external`, and `path` is `$x:where`. Only the first needs the
comparison spelled out — the others already compare — and it is the one the 41
sites use, so it carries the cost of the rewrite. It maps over a list like any
value modifier, and reads the way the guards actually appear:

```
if shpool:kind != false { … }
for e in [vi vim editline] { if $e:kind != false { export EDITOR = $e; break } }
```

The `!= false` is not decoration. A condition is [a bool or a
command](#conditionals), and `:kind` yields a *string* when the name resolves, so
the comparison is what the written contract asks for — and it is now the only form
that works, since condition truthiness settled as **no truthy values**: a bare
`if shpool:kind` is a loud "a string is not a condition" rather than a shorter
spelling. The explicit form is the honest one, and it is the only cost the
spelling carries.

**A bare subject takes a modifier** *(decided; shipped)*, so the guard is written
`if shpool:kind != false` and the quoting this entry argued for through its first
twenty-two rounds is not needed. An attached `:name` binds as a modifier wherever a
value is read — expression and argument context alike — for bare and quoted
subjects equally, and whether or not the modifier takes arguments.

It was a smaller change than it sounded, because half of it had already shipped.
`value_argument_starts` (`parser.rs:2319`) claimed a chain that ended in a `(` and
never asked whether the subject was quoted, so the split was never
bare-versus-quoted — it was argument-taking versus argument-free:

```
puts "a.b":stripend(".b")   # a     — applied before and after
puts abc:stripend("c")      # ab    — applied before and after, on a bare subject
puts "abc":upper            # ABC   — was the text `abc:upper`
puts abc:upper              # ABC   — was the text `abc:upper`
```

A `$`-prefixed subject was never affected at all: it carries its chain on the
`VarRef` and expansion applies it (`expand.rs:863`), so only a *literal* subject
saw the split.

**An attached `:identifier` outranks keyword parsing** *(shipped)*, which neither
rule above implies and the parser did not do. The keyword arms in `primary` return
before the postfix loop, so `if:upper`, `match:upper` and `for:upper` were syntax
errors and `not:upper` was silently `false` — `not` took the negation and `:upper`
folded away. In command position `if` and `unless` also lead a trailing guard, so
`puts if:upper` parsed as a guarded `puts` and printed nothing. `while`, `loop`,
`return`, `break`, `continue`, `global`, `unset`, `export`, `func`, `and` and
`fork` always took the ordinary path, so it was a four-name carve-out.

**A map literal's key is settled before the key is parsed** *(shipped)*. The key
goes through `expression`, whose postfix loop claimed the colon first, so
`[host:upper]` built the string `HOST` and `[host:upper, port:22]` was a hard
"consistent map entries" error — silently, and only for values that happened to
name a modifier. A bare identifier with an attached `:` is now a map key.
Deliberately narrow: `["abc":upper]`, `[$x:upper]` and `[(host:upper)]` are all
still chains, because none of them is a bare word.

**`:` followed by an identifier is reserved by the grammar** *(decided; shipped)* —
not gated on a list of known modifier names. An attached `:name` is a modifier
position wherever a value is read, and a name that is not a modifier is an
*error*, not text.

Half of it was already the rule, which was the main argument for the other half:
expression position claimed the chain outright while argument position fell back to
text. Both now refuse an unknown name, and the diagnostic names both escapes:

```
puts ubuntu:latest    # `:latest` is not a modifier; quote the whole word to keep it
                      # as text (`"x:latest"`), or brace the name when it comes from
                      # a variable (`"${x}:latest"`)
```

**When that error fires has since moved to run time**, though what is reserved has
not. [Declaring a modifier](#modifiers) lets a user add to the vocabulary, so the
parser can no longer hold the whole list and cannot tell a typo from a name declared
elsewhere; `:latest` is diagnosed when the line runs rather than when it is read. The
*grammar* reservation below is untouched — `ubuntu:latest` is still a modifier
position and never text — and the escapes above are still the escapes. That move is
**shipped**: the parser's gate on `MODIFIER_NAMES` is open for a value subject, for
the `:name` reference, and for the regex-literal suffix alike, and
`parser::unknown_modifier_message` carries the wording to its new site.

**The flag suffix on a [regex literal](#operators-and-matching) is opened with the
rest** *(shipped)*, because a regex flag **is** a modifier — one whose subject is a
pattern and whose result is a pattern. That was already the settled reading
([Tests and comparisons](#tests-and-comparisons)), and the shipped vocabulary already
agrees: `i`, `ignorecase`, `m`, `multiline`, `s`, `dotall`, `x` and `extended` are all
in `MODIFIER_NAMES`, and *applying* one already runs through the ordinary modifier
applier — `expand::set_regex_flag`, reached from the same path as any other modifier,
which is why `$r:i` on a pattern in a variable and `$rs:map(:i)` by reference both work
today. Which modifier `:x` is depends on the subject it meets: the extended-syntax flag
on a pattern, the executable filter on a path. The parser's `regex_flag` table is not a
second vocabulary; it answers the narrower question of whether a name may be *folded
into the literal while parsing*, which is the thing call-time resolution removes.

So a `/…/` in a match slot is a pattern **value**, and a `:name` after it is the
ordinary postfix chain, resolved against that subject when it runs. What that buys is
narrow and worth stating exactly: a declared modifier may follow a regex literal. The
flags themselves already reach a pattern by every other route.

The diagnostic survives the move with better grounds, not worse. `/a/:g` reports that
`:g` is not a regex flag today because the parser guessed the subject from syntax; at
run time the subject *is* a pattern, so the same message can be given for a reason the
parser only inferred — and the flag names it lists are still a closed set, since a
built-in modifier name cannot be redeclared.

Underneath both is one rule, the same one the `:ident` reservation above states:
**shape decides, vocabulary does not.** A colon and an identifier is a modifier
position whether or not that modifier exists; equally, a regex literal followed by
`:name` is a regex literal followed by a modifier, whatever the name turns out to be.
Which names exist is a question for run time in both cases.

What that costs is one corner that exists only because the parser was allowed to
answer it from vocabulary. Today a name it knows is a modifier but *not* a flag makes
it **back out** of the regex reading and take the string one, which is why
`"/A/":replaceall(/a/:upper, X)` prints `X` (pinned in `crates/mesh/tests/cli.rs`).
With the rule above there is nothing to back out on: `/a/` is the pattern, and
`:upper` applied to a pattern reports. `"x":i` stays an error from the other side — a
flag needs a pattern subject, and a string is not one.

**One thing the chain must not lose: a parse-affecting flag is construction-time.**
`:x` decides whether the pattern text is *valid*, so `/foo#(/:x` has to be compiled
once, in extended mode, rather than compiled and then modified — which is what
[`re()`](#tests-and-comparisons) already says, and why `re($x, extended: true)` exists.
Making the suffix an ordinary postfix chain does not change that; it constrains *when*
the literal compiles, not what the grammar reads. It used to compile eagerly, before
any postfix ran, and the contract was unmet — `/foo#(/:x` reported `invalid regex` —
which was a bug to fix alongside rather than a behavior to preserve. `Expr::Regex` now
carries the flag, so the literal is built with it.

What folds is the **leading run** of flags, and it closes at the first modifier that is
not one — because that is exactly how far the text still belongs to the literal. Past
it, nothing about the chain changes: a flag applied to a pattern value is the ordinary
dispatch it already is, so `/a/:foo:x` reaches `:x` with whatever `:foo` returned and
means the extended flag if that is a pattern, the executable filter if it is a path.
`$r:x` on a pattern in a variable works today and keeps working.

So the constraint is on **construction, not on chain position**. `/foo#(/:foo:x` fails
because `/foo#(/` cannot be compiled at all without the flag — the failure lands before
`:foo` runs, and it is the same failure `re("foo#(")` gives. `/foo#(/:x:foo` is the fix,
just as `re($x, extended: true)` is for the constructor. Nothing is applied
retroactively and no ordering rule is added; a literal simply has to be constructible
before a chain can run on it.

*(The [`re()`](#tests-and-comparisons) note used to state this more strongly — "never
as a post-hoc modifier on a finished value" — which the implementation has never done:
post-hoc `:x` on a pattern whose text is *valid* works, and is tested. That sentence is
narrowed to what actually holds, so the two sections agree: a post-hoc flag cannot
rescue text that never compiled.)*

**Only a bare identifier after the colon is claimed** — the reservation is of the
shape, not of the colon. `key:2`, `key:/path`, `key:`, `http://x` and `a:$b` all keep
the punctuation reading they had, so the break is narrower than "colons are taken".

A name the vocabulary *does* hold but the engine cannot apply yet (`:sort`) parses
and reports at run time. That was once the *distinguishing* case, an unknown name
having failed earlier; both now report at run time, and the two stay worded
differently because "no such modifier" and "not implemented yet" are different
answers even when they arrive together.

The parser tests `MODIFIER_NAMES` for neither, now that both report at run time. So
reserving `:ident` in the grammar does not introduce a new rule; it makes argument
position agree with expression position, which is where the inconsistency was.

The alternative — keep gating on the name list — was written into an earlier
draft of this entry and is worse in the way that matters. Under it the reserved
vocabulary **grows silently with every modifier added**: `img:raw` is text until
someone adds `:raw`, and then it quietly stops being. This proposal is itself the
first instance, since `kind` and `where` are two new names, and the entry had to
float a deprecation cycle to cope. Reserving the whole shape up front makes that
class of change a non-event: adding a modifier can no longer break argument text,
because `:ident` was never argument text.

It also fails loudly. `ubuntu:latest` becomes "unknown modifier" the day the rule
lands, rather than working for two years and then changing meaning under a
release note nobody read.

The cost is a one-time, fully-known break rather than a creeping one, and it is
real: `docker run ubuntu:latest`, `git show HEAD:file`, `rsync host:src dst` and
`curl -H Accept:application/json` all need quoting — `"ubuntu:latest"`, with the
colon *inside* the quotes, not `"ubuntu":latest`. When the part before the colon
comes from a variable, quoting is not enough and braces are what end the name
(`"${image}:latest"`); see below.

Measured against the `shrc` this vocabulary exists for, that cost is **zero**:
every `word:identifier` in `shrc`, `config/fish/config.fish` and
`config/nushell/config.nu` sits inside a single-quoted `LS_COLORS` string or a
comment, and no unquoted one appears in command-argument position at all. The
pain lands on interactive typing rather than on configuration, which is worth
saying plainly because it is the part a repo survey cannot measure.

**Quoting the subject does not preserve the old reading**, and that is the part a
reader is most likely to get wrong, because quoting is the usual escape from
shell metacharacters. It does not help here — `"abc":upper` is literal `abc:upper`
today and becomes `ABC`, exactly as the bare form does. What the quotes have to
enclose is the **whole token**:

```
puts "abc":upper      # ABC        — the modifier applies; quoting the subject changes nothing
puts "abc:upper"      # abc:upper  — literal, because the colon is inside the string
puts 'abc:upper'      # abc:upper  — likewise
```

So the escape hatch exists and is cheap, but it is `"img:raw"` rather than the
`"img":raw` a reader might reach for first.

**And "quote the whole token" is not the rule when the subject is a variable** —
that phrasing is wrong in the case people will actually hit. A modifier already
binds inside a double-quoted string, so quoting does not stop it:

```
x = "abc";    puts "$x:upper"      # ABC          — quoted, and the modifier applies
x = "abc";    puts "${x}:upper"    # abc:upper    — braces stop it
x = "ubuntu"; puts "$x:latest"     # ubuntu:latest today; an ERROR under this rule
x = "ubuntu"; puts "${x}:latest"   # ubuntu:latest — safe either way
```

Command-word parsing merges a same-quoted suffix into an unbraced variable
access, so `"$image:latest"` is a modifier chain no matter how much of it is
inside the quotes. **The literal spelling is `"${image}:latest"`** — braces, not
quotes — and that is the form a migration note has to give, because dynamically
assembled Docker tags and `rsync` targets are exactly where this bites and they
are already fully quoted today.

The rule, stated once and correctly: quotes stop the colon only when the *whole
token* is literal text (`"img:raw"`); when the subject interpolates, braces are
what end the name (`"${img}:raw"`).

**Any attached `:identifier` outranks keyword parsing** — an unknown name
included, since the grammar reserves the shape rather than a list. Neither rule
above implies this and the parser does not do it. Expression
position already binds a bare subject — `x = abc:upper` gives `ABC` — so the
argument rows are most of what changes. But four receivers are claimed before the
postfix-modifier loop is ever reached, and they fail in *expression* position too:

```
x = while:upper     # WHILE          — the ordinary path, and most keywords take it
x = if:upper        # syntax error: expected `{`     claimed by `primary`
x = match:upper     # syntax error                   likewise
x = for:upper       # syntax error                   likewise
x = not:upper       # false          — worst case: no error, silently wrong
```

`primary` recognizes `if`, `match` and `for` at `parser.rs:3502`, and
`not_expression` consumes `not` at `3078`, all before any modifier is considered.
`not` is the one to fix first: it does not fail, it quietly evaluates to `false`,
so a guard written over it would read as "no such name" forever.

Every other reserved word — `while`, `loop`, `return`, `break`, `continue`,
`global`, `unset`, `export`, `func`, `and`, `fork` — already takes the ordinary
path, so this is a four-name carve-out rather than a general keyword problem.

Resolution order is the one command position uses — **keyword → builtin → func →
external**. mesh has no alias stage, so there is no further answer: what `alias`
defines is a `func`, and it resolves at that step like any other.

The justification usually given for that order — *`:kind` cannot disagree with
what running the name would do* — does not survive contact with the ways a name
can be written, because it never says which one it means. There are four
spellings but only **three** behaviors, since quoting and expanding share one
path (`expand_stage`, `repl.rs:5521`):

| how the name is written | what happens for `if` |
|---|---|
| bare, `if x` | syntax — the parser claims it |
| quoted `"if" x`, or expanded `n = "if"; $n x` | resolves **func → external**: the function if one is defined, else an `if` program if `PATH` has one, else `command not found` |
| value call, `x = if()` | syntax error (a func for 7 of the 13) |

Only the first line yields `keyword`. Quoting does not force external lookup —
with both a `func if` and an `if` executable present, `"if" x` runs the function,
exactly as `$n` does. The order above is the **bare** one, and it
is settled for the 13 command-position words only once the open question below is
answered: option A keeps it exactly and `if:kind` stays `keyword`; option B has
`:kind` follow the resolving order — builtin → func → external, `keyword` only
when nothing is found — which is a different contract, not a tweak. Everything
else in this section holds either way; this paragraph is where the two diverge.

*(Still open, but no longer even-handed: the shipped `type -t` answers `keyword`
for a bare `if`, which is option A's behavior. That is evidence rather than a
decision — `-t` is bash's vocabulary and was not written to settle this — but
"one vocabulary in every form" applies to `:kind` too, so choosing B would put
two answers about the same word in the tree and now costs a change to `type` as
well.)*

`keyword` is the one that is easy to miss, and it is **defined by the grammar,
not by a list of favorites**: a keyword is a word the parser claims *in command
position*, so that a **bare** name of that spelling never reaches resolution —
`if`, `for`, `while`, `match`, `loop`, `func`, `not`, `return`, `break`,
`continue`, `global`, `unset`, `export`. *Bare* is load-bearing, and narrower
than it looks: the parser claims these words only unquoted and unexpanded. A
**quoted or expanded head resolves normally**, and the quoted spelling is this
document's own recommended one:

```
"if" x            # func `if` if defined, else an `if` program, else not found
n = "if"; $n x    # the same path — quoting and expanding agree
```

The value call is *not* a third resolving spelling and should not be described as
one: `x = if()` is a syntax error, and only 7 of the 13 words reach a function
that way at all (`while`, `loop`, `break`, `continue`, `global`, `unset`,
`export`). It is name-dependent, so it belongs beside those two rather than with
them.

That matters here rather than as trivia, because it is the *subject matter* of
the open question below. What it is **not** is a property of how the guard is
written: `:kind` receives a string, and the bare-subject rule above makes
`if:kind`, `"if":kind` and `$n:kind` on a variable holding `"if"` the same call.
Receiver spelling is invisible to the modifier, so it cannot decide which
command-head spelling the answer describes.

**Being reserved somewhere is not enough**, and the reserved list is split almost
evenly on this. Half of it does *not* claim the head of a command:

- **Mid-form syntax.** `in` within `for x in ys`, `and` / `or` as infix, `else`
  and `unless` within their clauses. They are syntax inside a larger form, never
  leading one.
- **`fork`**, which leads a statement only when a block follows.
- **The built-in value names** — `re`, `style`, `link`, `glob`, `files`, `dirs`.
  These are reserved from `func` *definitions* only, because `re(x)` must always
  build a regex rather than call a user function; the parser says so where it
  refuses them, noting the plain command form stays reachable.

For all of those, `:kind` performs ordinary builtin → func → external resolution.
Answering `keyword` would mask something real — a defined, callable function, or
in the case of `link` an actual `/usr/bin/link` — which is the same failure as
answering it for a name nobody reserves at all.

The test is the whole rule, and it is a probe rather than an opinion: type the
bare word and ask **whether resolution ran**, not whether the parse failed.
`command not found`, or the name's own program running, means it ran — the word
is not a keyword. Anything else means the shell claimed the word first, and a
parse error is only one of the ways that shows:

```
if       → syntax error: empty command in a pipeline     claimed
break    → mesh: break: not inside a loop                claimed — parsed fine, complained at run time
return   → mesh: return: not inside a function           claimed, likewise
re       → mesh: command not found: re                   resolution ran
link     → link: missing operand                         resolution ran — /usr/bin/link
```

`break`, `continue` and `return` are why the phrasing matters: they parse
perfectly well and object about *context*, so a "syntax error means keyword" rule
would file all three under resolution and let `:kind` answer `false` for them —
the one thing the invariant forbids.

Enumerating that here would be a third copy, and copies drift — this entry
already got the set wrong twice by trying. The set does exist in the codebase,
mirroring `parser.rs` and kept honest by a test asserting `help` explains every
word in it. But it lives *inside* that test, which is the wrong place for
something a language feature now depends on, and reaching for `help` instead is
not a substitute: `help` deliberately documents *shapes* as well as words, so it
answers for `cmd` — a placeholder for an ordinary command line, reserved by
nobody. A guard asking `cmd:kind` would be told `keyword` and would stop seeing
a real program of that name.

So the prerequisite is **one table outside the test, and three named views over
it** — not one predicate, because the three callers are asking three different
questions and a single answer would have to be wrong for two of them:

*(**Built.** `RESERVED_WORDS` (`crates/mesh-core/src/builtins.rs`) is one row per
reserved word carrying a `Claim`, and the three views are derived from it —
`syntax_words()`, `is_command_keyword`, `is_value_call`. Whoever builds `:kind`
inherits the `keyword` view rather than writing it. One gap remains: the parser's
own word arms are mirrored by the table, not driven by it, so a new keyword is
three edits rather than one. The reasoning below is kept because it is what the
shape has to answer for, not because the work is outstanding.)*

| asks | set |
|---|---|
| `:kind`, for `keyword` | claims command position |
| `func`, for its refusal | three parts, none of them derived from deadness: `func`, `not` and `return`, refused today and kept refused as inherited policy; the value-call names — `re(x)` must build a regex; and **every builtin**, via the existing `is_builtin`, since command position reaches a builtin before a func |
| `help`, for coverage | every reserved word, mid-form ones included |

`and` is the case that separates them: `help and` must answer, `func and()` is
allowed, and `and:kind` is not `keyword`. Collapsing those into one predicate
either misclassifies `and` or stops documenting it.

The whole middle row is a *separate* check the table supplements rather than
replaces, and none of it derives from the reserved-word analysis. Builtins: `pwd`
is not a reserved word and never appears in the table, but `func pwd()` is
refused, and must stay refused. `func`, `not` and `return`: refused at
`repl.rs:1153` today, and kept refused as policy — the probe cannot be run on any
of them, since `func not()` is rejected before `n = "not"; $n x` can be tried,
and for `return` the likely answer runs the other way, since only *bare* `return`
is intercepted as control flow. **No command-position word belongs in this row on
deadness grounds**, because none of them is dead; see the parenthetical below.

What is worth unifying is the *data*, not the answer. Today the same words are
written out three times — the parser inline, `func` from a hardcoded
`func`/`return`/`not` plus builtins, and `help` from a table carrying an extra
placeholder — so a new keyword has to be added in three places and nothing
notices when it is not. One table with a row per word, and each view derived from
it, removes the drift while keeping the three answers distinct. That is exactly
the divergence this modifier exists to avoid, which makes fixing it part of the
work rather than a tidy-up alongside it.

The invariant is what earns the answer, and it has to be stated at exactly this
width: **`:kind` never says `false` for a word the shell claims in command
position.** Not "a word the shell handles" — `and` is handled, as infix syntax,
and `and:kind` is quite properly `false` where no such program exists, because
a guard asking about `and` is asking whether it can *run* one. The wider phrasing
reads better and is wrong, and it would drive an implementation straight back to
calling mid-form words `keyword`. Command position is the boundary throughout:
it is what `:kind` asks about, so it is what the invariant may promise.

**A keyword is syntax, so `:kind` does not promise it is runnable.** Quoted into
command position, every keyword but one falls through to external lookup — when
no function of that name is defined:

```
n = "if"; $n x        # command not found: if   — with no function of that name
n = "break"; $n x     # command not found: break
n = "return"; $n x    # returns — the one keyword also caught at run time
```

The trailing clause on the first line is the whole subtlety, and it is easy to
read past: those answers hold *because nothing named `if` is defined*. Expansion
consults `shell.funcs` (`repl.rs:5529`), so a defined function is found:

```
func if(_x) { puts OK }; n = "if"; $n arg   # OK
```

So a keyword is not runnable **bare**, which is all `:kind` needs to be
right about `/usr/bin/if`: answering `external` or `false` would send a guard
looking for a program that does not exist. But "not runnable in any spelling" is
false, and this document asserted it — see the open question below, which is the
part that is not yet settled.

`global`, `unset` and `export` look contextual and are not, for this purpose:
each claims the word wherever an assignment does not follow, so no *literal*
`global x` reaches a function. (Via an expanded name it does —
`func global(_x) { … }; n = "global"; $n y` runs the function. The qualifier
matters and is not decoration.) They are keywords. `fork` is the one word
that genuinely straddles it — `fork { … }` is syntax, `fork arg` calls a function
— and it lands on the reachable side, because that is where a wrong answer costs
something: an answer of `keyword` hides a real function or program, while an
answer of `func` merely fails to mention a syntax the guard was not asking about.

***(Open — what `keyword` should say when the name resolves to something.)*** The
`fork` decision above rests on a rule: land on the reachable side, because
answering `keyword` for something callable hides it. Three of the four spellings
make that rule apply to *every* command-position word, not just `fork`:

```
func if(_x) { … }; "if" arg    # a real function, hidden by `keyword`
"if" arg                        # a real program,  hidden by `keyword`
```

(Both spellings resolve func → external, so either can be the thing hidden.)

The second is the harder case. An earlier draft of this entry claimed the
bare-subject decision softened it — that once the guard reads `if if:kind` the
subject is bare, and bare is the spelling the parser really does claim. **That
reasoning is wrong**, and the decision above is what makes it wrong: modifiers
take values, so `if:kind`, `"if":kind` and `$n:kind` over a variable holding
`"if"` are one call on one string. How the receiver was spelled never reaches
`:kind`, and therefore cannot pick which reading it reports.

The question is better stated with the receiver left out of it entirely. `:kind`
is handed the name `if`. That name behaves one way as a bare command head — the
parser claims it — and another way through every other route, where it resolves
func → external. Both are true of the name at once. The open question is which of
them the taxonomy is *about*, and it would read identically if the only spelling
in the language were `$n:kind`.

Two consistent answers, and they differ in what `keyword` means:

- **`keyword` is about the word.** `:kind` on the name `if` is always `keyword`,
  and the guard is told the word is syntax. Simple and stable, but wrong whenever a real
  function or program of that name exists — and it reports `keyword` for a name
  the same script could successfully run as `"if" x`.
- **`:kind` reports what would be found.** `:kind` on `if` is `func` or
  `external` when one exists and `keyword` otherwise, matching the "report what you find"
  rule that already governs `pwd:kind == builtin` against `command pwd`.
  Consistent with the rest of the design, at the cost of an answer that changes
  under the guard — and it makes `keyword` mean "nothing else claimed this",
  which is a weaker word than the section above spends its length defining.
  **`return` needs carving out either way**: `run_expanded` intercepts the bare
  string before external lookup (`repl.rs:5814`), so `"return" x` performs shell
  control flow even with a `return` executable on `PATH`. Under this option
  `return:kind` would answer `external` and be wrong about what the same line
  does — so either `return` stays `keyword` as a named exception, or removing
  that interception becomes part of the option.

This is not decided here. Each call form found so far has widened it rather than
closed it, which is itself the argument for settling it before implementation
rather than discovering it during.

*(Related, and pre-existing: `func` refuses `func`, `return`, `not`, the value
names and any builtin, but accepts `func if()`, `func while()`, `func break()`
and the rest. **None of them is dead.** There are three call forms, not two, and
the third reaches every one of them:

```
func while() { return OK }; x = while()        # value call — OK
func if(_x) { puts OK }; n = "if"; $n arg     # expanded name — OK
```

`repl.rs:5529` looks the expanded head up in `shell.funcs` before anything else,
so any definition the `func` statement accepts is callable by that route,
including `if`, `match` and `for`, which are unreachable both as bare commands
and as value calls. The dead-definition premise for this item is therefore empty:
there is no set of names that can be reserved as "already unusable". Reserving
any of them is a deliberate compatibility break, to be argued for on its own
merits — that a function callable only through `$n` is a trap worth closing — and
not as a tidy-up.)*

`command NAME` looks past **all** of it — keyword, builtin and func alike —
because bypassing the wrapper is what it exists for (`func ls() { command ls … }`,
and `command return` inside a function reports not-found and keeps going rather
than returning). `:kind` reports what it *finds* rather than where `command`
would look, so `pwd:kind` is `builtin` while `command pwd` runs `/bin/pwd`. The
two can disagree in the other direction as well: `command` only *looks* for a
program, so `command cd` is `command not found` on a system with no `/bin/cd` —
which is why `:kind` answers about resolution rather than about what `command`
would do with the name.

**A receiver containing `/` is a path, not a name.** `execvp` treats a word with
a slash as a direct path and never consults `PATH` (`exec.rs:1061`), which is why
`./tool` runs today. The modifier binds on such a word already — `./tool:upper`
gives `./TOOL`, the whole word being the receiver — so `:kind` and `:where` will
be asked about these and need an answer.

The keyword, builtin and func layers do not apply: no keyword, builtin or `func`
name can contain a slash, so a slashed receiver is external-or-nothing. That much
is forced.

**What it resolves to is not.** An earlier draft of this entry recorded a second
forced result — that `./tool:kind` must be `external` wherever the file is
executable, since the shell runs it. That is false, and the exec bit is simply
not the predicate. A script with mode 755 whose shebang names a missing
interpreter is executable by every permission test and still does not run:

```
./btool     # mesh: command not found: ./btool     — mode 755, shebang /nonexistent/interp
```

`execve` fails `ENOENT` on the *interpreter*, and the shell reports it as though
the command itself were absent. So "executable" and "runnable" come apart a
second time, in the opposite direction from the `EACCES` rows below: there a file
exists and cannot run; here a file is executable and still cannot run.

**The same ambiguity exists for a slashless receiver, and `PATH` makes it
sharper**, because `execvp` does not stop at the first *name* match — it skips a
candidate it cannot execute and keeps looking. With a non-executable `tool` in
the first `PATH` entry and an executable one in the second:

```
tool          # ran-from-d2   — the first candidate is skipped, not an error
dirtool       # mesh: permission denied: dirtool   — a directory, no later candidate
tool          # mesh: permission denied: tool      — only the non-executable one on PATH
```

The bad-shebang file behaves the same way, and this is what makes the point
general rather than a permissions detail — `execvp` skips it and runs the later
candidate:

```
btool         # ran-from-e2   — e1/btool is mode 755 with a missing interpreter
btool         # mesh: command not found: btool     — only the bad-shebang one on PATH
```

So an implementation that answers with the first `PATH` entry containing a file
of that name names something the shell will never run — and so does one that
answers with the first *executable* file of that name. Neither permission bits
nor name matching is the predicate.

Note the two failures report differently: the non-executable and directory cases
are **`EACCES`** ("permission denied"), the bad-interpreter case is **`ENOENT`**
("command not found"), and `execvp` continues past both.

**The honest position is that exact fidelity is not reachable**, and the earlier
draft's principle — "`:where` may not disagree with what the shell does" — has to
be retracted as an absolute. Knowing whether a file will run means reading its
shebang, resolving that interpreter, and recursing, and the answer can change
between the query and the command anyway. Every POSIX shell has some version of the gap,
but **they do not agree on which**, so `command -v` is not a reference behavior
to copy. With a mode-0644 `ptool` and a mode-0755 `btool2` whose shebang names a
missing interpreter, both on `PATH`:

```
bash 5.2.21   command -v ptool  → prints the path, rc 0     no permission check at all
              command -v btool2 → prints the path, rc 0
dash          command -v ptool  → rc 127                    checks the bit
```

So bash reports a bare name match, dash reports an executable match, and neither
follows the shebang. "Match `command -v`" names two different behaviors depending
on the shell, and the entry must say which it means.

The choice is therefore which *approximation* to specify, and there are **three**
— the bash probe above is what makes the first one real rather than a straw man:

**What each shell actually accepts, measured rather than described.** This entry
has now characterized these rules in prose three times and been wrong three
times, so the table is the specification and the bullets below are derived from
it. Each candidate placed alone on `PATH`; `rc` from `command -v NAME`:

| candidate | bash 5.2.21 | dash |
|---|---|---|
| regular file, mode 644 | 0 | 127 |
| regular file, mode 755 | 0 | 0 |
| **directory**, mode 755 | 1 | 127 |
| **FIFO**, mode 644 | 0 | 127 |
| **FIFO**, mode 755 | 0 | 127 |
| symlink → regular 755 | 0 | 0 |
| broken symlink | 1 | 127 |

Read off the table rather than from intuition:

- **bash** accepts anything that exists and is **not a directory**. Not "regular
  file" — a FIFO passes, at either mode — and there is no permission check at
  all.
- **dash** requires a **regular file** *and* execute permission for the effective
  user. A mode-755 FIFO has the bits and is still rejected, so the regular-file
  test is separate from the permission test and both are needed.
- Both follow symlinks and both reject a broken one.
- Neither reads the shebang, so both accept the bad-interpreter file from above.

The exec bit on a directory means "may be traversed", not "may be run", which is
why every rule has to exclude directories however else it is spelled.

So the three approximations, each stated as the table supports:

- **Name match** — the first candidate of that name that exists and is not a
  directory. Cheapest, and bash's. Wrong for a non-executable file, a FIFO and a
  bad interpreter, and it cannot express the `PATH`-skipping behavior at all,
  since the first match is by definition where it stops.
- **Permission bits** — the first *regular file* of that name that the effective
  user may execute. dash's. Handles the `EACCES` rows and the skip, and is
  knowingly wrong for a bad interpreter.
- **Shebang-following** — read the interpreter and recurse. Strictly better
  answers, unbounded work, still racy, and matches no shell here.

Two phrasings in the middle option are load-bearing, and each has already been
got wrong once: **regular file** (not "not a directory" — a FIFO has exec bits)
and **effective user** (not the owner's bits).

Whichever is chosen, the promise `:where` makes must be written as "what lookup
would select", not "what will run".

What stays open is one question, not two, and it is the same question in both
settings: **when nothing runnable is found but a file of that name exists**, does
`:kind` answer `false` (nothing here can run) or `external` (a file of that name
is present)? The rows it covers:

- a file that exists but is not executable, direct (`./notes.txt`) or on `PATH`;
- a **directory** of that name, which fails identically;
- and, for `:where`, what to return in those cases.

Separately and still open: whether `:where` gives a direct path **as written**
(`./tool`, matching what `command -v ./tool` reports in a POSIX shell) or an
absolutized one.

This is also evidence for the open `:where` question below, in two directions. A
direct path has no `PATH` answer at all, so "`:where` searches `PATH`" is
undefined for every receiver containing a slash. And where it *does* search
`PATH`, "searches `PATH`" is not yet a specification — it has to say "the way
`execvp` does", or it names the wrong file.

**A name that resolves to nothing is `false`, not an error** *(decided)*. This
departs from `:type`, which errors on a missing path because a file that is not
there has no type word — and it is worth stating because the sibling it is
modeled on goes the other way. Here absence is the *question*: "is this
installed?" is the whole point of a guard, so it follows `:exists`, `:get`'s
default, and `gets()` at EOF, all of which answer rather than raise. Erroring
would make all 41 guard sites defensive.

***(Open — is `:where` about resolution, or about `PATH`?)*** I first wrote
"`:where` on a builtin or a func is `false`: it asks for a path, and there isn't
one" as though it settled the matter. It does not, and the gap is not confined to
keywords — **shadowing is the ordinary case, not the exotic one**:

```
pwd:where     # builtin, and /bin/pwd exists
ls:where      # with `func ls() { command ls … }` — the documented wrapper idiom
if:where      # keyword under option A, and /usr/bin/if may exist
```

For every one of those the shell resolves to something with no path, while a real
program of that name sits on `PATH`. Two answers, and the choice is the same one
each time:

- **`:where` follows resolution.** All three are `false`, because that is what
  the shell would actually run. The pair never disagrees, and `:where` cannot
  surface a program `:kind` declined to mention. The cost is that it **stops
  being `path`**: the `shrc` function it replaces searches `PATH` and would
  answer `/bin/pwd`, so a port's `path pwd` changes meaning silently.
- **`:where` answers about the filesystem.** All three give the path, because the
  question is "where is the program", which is what `path` asks and what the
  guard sites want when they ask it. The cost is that `:kind` and `:where` openly
  disagree — `builtin` alongside `/bin/pwd` — which is the divergence the rest of
  this section works to avoid.

This is **independent of the `keyword` question above**: `pwd:kind` is `builtin`
under either option, so the builtin and func rows have to be decided on their own
terms. The keyword row is the only one that also depends on it, and under option B
it mostly dissolves — `if:kind` is already `external` where only the program
exists — leaving `return`, which stays `keyword` by carve-out and so takes
whichever answer is chosen here.

Worth naming plainly: the wrapper idiom this document recommends elsewhere
(`func ls() { command ls … }`) is exactly the case that makes `ls:where` return
`false` under the first option. A vocabulary meant to replace `path` would then
answer `false` for the most common thing a user wraps.

*(Open: the session half of the vocabulary — `connected-remotely`,
`inside-project`, `in-shpool` — needs no new surface. `$sh.interactive`,
`$sh.stdin:tty` and `$env:get(SSH_CLIENT, "")` already answer those, so they are
ordinary `func`s a user writes, not language.)*

*(Implementation note: `:kind` needs the function table, which lives in `Funcs`,
while string interpolation resolves through `expand.rs` with only `&Vars`.
Wiring it into one path and not the other would make `y = $x:kind` and
`"$x:kind"` disagree — the exact split this modifier exists to prevent — so
whichever way the plumbing goes, both paths land together.)*

**String** *(open — initial set)*: `:replaceall(OLD, NEW)` and its anchored/removal
kin (`:replacestart` / `:replaceend` / `:stripstart` / `:stripend`, plus
`:trimstart` / `:trimend` for whitespace), and likely `:upper` / `:lower`. To be
fleshed out.

**Anchored and removal variants** *(decided; lower priority to implement)*. Alongside
the global `:replaceall`, a start/end-anchored
`:replacestart(OLD, NEW)` / `:replaceend(OLD, NEW)` act only on a **leading** /
**trailing** match — their `OLD` is a match slot exactly like `:replaceall`'s (a
string is literal, a `/…/` is a regex, so `$s:replaceend(/\.js$/, ".ts")` works).
`:stripstart(PREFIX)` / `:stripend(SUFFIX)` are the removal
shorthand (`:stripend(x)` == `:replaceend(x, "")`): each drops the affix **once if the
string starts / ends with it**, and is a no-op otherwise — `"report.tar.gz":stripend(".tar.gz")`
is `report`. This is the everyday "drop a known suffix" reach — the spirit of bash's
`basename "$f" .tar.gz`, though a pure string op, not its equal (it doesn't strip the
dirname, and has none of basename's POSIX corner cases) — with no regex escaping and no
interior-match surprise (a global `:replaceall(".tar.gz", "")` would also rewrite
`a.tar.gz.bak`). Separately,
`:trimstart` / `:trimend` peel **whitespace** (or a given **char set**) repeatedly —
the trailing-newline case, not a known suffix.

**Regex substitution is `:replaceall` with a regex `OLD`** *(decided — the "sed
`s///` in a modifier" case)*. There is **no `:s/old/new/` form**. It would fight
three settled decisions at once: **`:s` is already taken** — it is the terse
spelling of the `:dotall` regex flag (see [`re()`](#tests-and-comparisons)), so `$f:s/…/…/` is
ambiguous with a flagged value; **arguments are parenthesized, with no exceptions**
(a regex argument is a `/…/` literal *inside* the parens like any other value — see
[Modifiers](#modifiers)), so a slash-delimited inline argument is the one shape the
grammar deliberately doesn't have; and mesh **already declined sed's `s///`** for
[history substitution](#history-expansion) in favor of the `old=new` mapping form.
Reintroducing `s///` here would make it the sole place slashes delimit a modifier
argument.

Instead, the everyday substitution the user reaches for is the **existing
`:replaceall(OLD, NEW)`** with a **regex** `OLD`:

```
$f:replaceall("foo", "bar")     # literal substring replace
$f:replaceall(/foo/, bar)       # regex replace  — the :s/foo/bar case
$f:replaceall(/foo/:i, bar)     # flags ride on the regex value (case-insensitive)
$line:replaceall(re($pat), $new) # pattern arrives as a string → re()
```

- **The argument type decides**, no second operator: a **string** `OLD` matches
  **verbatim** (metacharacters are literal), a **regex** `OLD` (`/…/` or an `re()`
  value) matches as a pattern. This is the same no-silent-coercion rule as `~` and
  `:int` — a string full of `.`/`*` never quietly becomes a pattern. The **first
  (`OLD`) argument of the replace family** — `:replaceall` and its anchored
  `:replacestart` / `:replaceend` kin — is a [regex match slot](#tests-and-comparisons),
  the fourth, alongside the `~`/`!~` RHS, the `:match` argument, and a `match` arm — so
  a bare `/foo/` there is a regex, not a path. (`NEW` is an ordinary value slot; a
  `/…/` there is a literal string.)
- **Global by default** — the name says so: every occurrence, matching the [history `old=new`](#history-expansion)
  precedent (mesh has no per-line notion here for a `/g` toggle to hang off).
- It is a **value modifier**, so it **maps over a list** element-wise like `:stem`
  — `$paths:replaceall(/\.js$/, .ts)` rewrites each path.
- **Capture backreferences** in `NEW` for a regex `OLD` *(provisional spelling)*:
  `${1}` / `${name}` currently stand in for syntax that splices
  the numbered / named group of *this match* (a replacement-local scope, not an
  outer variable — bare `$1` stays reserved, mesh having no positional `$1`). For a
  **computed** replacement, `NEW` may be a **lambda** taking the match — `:replaceall(/(\d+)/, func(_m) { $_m:int + 1 })` — the callback form, consistent with `:map` / `:filter` / `:each`.

*(Open sub-questions: the exact backref spelling (`${1}` vs `$1` inside the
replacement string), and whether a first-only variant is ever needed — it would be a
separate `:replace`, mirroring JavaScript's `replace` / `replaceAll` split — deferred
until a port needs it.)*

**String→number parse** *(decided — porting `total`, `bisect`)*. Values from argv /
`gets` / `$(…)` captures are **strings**, and numeric operations do not coerce
string operands (`n += "1"` fails when `n` is an int) with `<` / `>` comparing
strings *lexically*. This does not narrow the operators themselves: `+=` also
concatenates strings, extends lists, and merges maps, while `Duration` and
`Instant` have their arithmetic defined below. The
**`:int`** modifier parses a string to an integer, **fail-loud** — the inverse of
the canonical int→decimal rendering, erroring on non-numeric input rather than
silently yielding `0`. So `$line:words:get(0, "0"):int` sums a column and
`$good:int < $bad:int` compares numerically. *(The need appeared, and both landed
in the design together as that note said they should: see **Floats** under
[Arithmetic](#arithmetic) for the `f64` type and the `:num` parse. `:int` keeps its
own job — a string the program means as an integer should fail loud when it is
`3.5`, not quietly widen.)*

**String→boolean parse** *(decided — porting a shell config's `FAILSAFE=1` flag)*.
**`:bool`** is `:int`'s twin and reads `1` / `true` / `0` / `false`, the only four
spellings it accepts. Two vocabularies had to be in and a third had to stay out:
`true` / `false` are what mesh writes for a boolean, so one round-trips, and
`1` / `0` are what every shell flag already uses. `yes`, `on`, `y`, `enabled` are
where a parse turns into a dialect — each synonym admitted forces a ruling on its
opposite, and the ten-entry table that results still has to guess at `maybe`.

**It warns rather than raising**, which is the one place it parts from `:int`, and
the reason is the types rather than a preference: **a boolean has a safe stand-in
and an integer does not**. "I could not read this flag, so it is off" is a real
answer; "I could not read this number, so it is 0" is a fabrication. Raising was
rejected because the motivating caller is a shell rc, where the flag is the escape
hatch — `FAILSAFE=yes mesh` raising *inside* the rc is the failure the flag exists
to escape. Answering `false` silently was rejected for the opposite reason: the
person who typed `yes` meant *on* and would never find out they had not got it.

**`:bool(DEFAULT)` is the quiet form**, and the split is what keeps the argument
from being a synonym. Supplying a default is the statement that an unreadable
value is expected, so mesh stops mentioning it — exactly the bargain
[`:get(KEY, DEFAULT)`](#arrays-lists) makes, which never reports the key that was
absent. If both forms warned, `:bool(false)` would agree with bare `:bool` on
every input and the only useful argument left in the language would be `true`.

This does **not** reopen [truthiness](#conditionals-if-is-an-expression), which
stays settled at *there isn't any*. That rule governs values a condition accepts
implicitly; `:bool` is an asked-for parse at the one boundary where strings arrive
already stringly-typed from outside mesh, and it is refusable — `$sh.options.X =
"false"` is still a type error rather than a coercion.

A **boolean subject is the identity**, which exists to make the composition work:
`$env:get(FLAG, false):bool` can then spell its default as the bare literal, where
a string-only `:bool` would force `"false"` and put back the quotes the modifier
was added to remove. The comparison it replaces —
`$env:get(FLAG, "0") == "1"` — hides a trap those quotes are all that hold shut,
since `1` is an integer literal and equality is type-strict across string and
number, so dropping them makes the test *always* false.

**Declaring a modifier — `func _s:name()`** *(decided)*. The modifier vocabulary is no
longer closed: a user may add to it, but only by **declaring** a modifier, not by
having written a one-argument `func`. The declaration puts the subject where the call
site puts it — left of the colon — so the two read the same:

```
func _s:foo()          { … }   # $x:foo         — one element at a time
func _s:bar(_n)        { … }   # $x:bar(8)      — …with an argument
func ..._xs:baz(_sep)  { … }   # $xs:baz(", ")  — the whole list, with an argument
func ..._xs:qux()      { … }   # $xs:qux        — the whole list
```

The subject is **not a positional parameter** — it sits outside the parens, which is
what lets a list-taking modifier still take arguments; as a leading rest parameter it
would collide with rest-must-be-last, and `$xs:baz(", ")` is not a corner case.
This also describes the built-ins without strain: `:upper` is `func _s:upper()`,
`:replaceall` is `func _s:replaceall(_old, _new)`, `:join` is
`func ..._xs:join(_sep)`.

**Element-wise is the default; `...` takes the collection.** A plain subject parameter
receives *one element*, so a list subject means the modifier is called per element —
the auto-mapping the built-ins already do, and most of what declaring one buys. A rest
subject receives the whole list, once. That is not a second meaning for `...`: the
subject is *spread into* the parameter the same way arguments are, and `...` gathers
many either way — only the source differs.

**Why a declaration rather than any `func`.** Not to keep resolution static — it is
not, see below. The declaration is about **intent**. `func helper(_s)` is a function
that happens to take one argument, and making every such function silently reachable
as `$x:helper` would promote a private helper to public vocabulary by accident. The
declaration says *this is a modifier*, and it is where the subject and its `...` form
live, which an ordinary parameter list has nowhere to put.

**A modifier resolves when it is called**, exactly as command position does. `$x:foo`
finds whatever `:foo` names at the moment that line runs, so redefining a modifier
changes what an already-written use runs, and one arriving from a `source` a statement
earlier is found. There is no pre-pass and nothing is hoisted; a modifier declaration
may sit wherever a `func` may sit, including inside a function body or a branch, and
binds when it executes just as a `func` does.

That makes the reach of a *later* declaration exactly the reach `func` already has, and
it is worth being precise rather than saying "forward references work". A declaration
further down the file is found only when the **use** is delayed past it:

```
func f() { puts $x:foo }     # fine — `f` is called below, after the declaration binds
func _s:foo() { … }
f
```

```
puts $x:bar                  # error — nothing has bound `:bar` at this point yet
func _s:bar() { … }
```

Two separate programs, and separate names: in one file the first block's declaration
would already have bound the modifier, and the second would be a redefinition rather
than the missing one it is meant to show.

Which is the same thing `func f { g }` buys and the same thing it does not: definition
order is irrelevant *between* a function and what its body calls, and entirely relevant
for a call written above the definition. Modifiers get that rule, not a stronger one.

**The cost is that a typo'd modifier fails at run time.** `$x:fop` is no longer a
syntax error; it fails when the line runs, naming the modifier. That is a real loss,
and an earlier revision of this section treated avoiding it as the whole reason to
require a declaration. Two things make it the right trade.

mesh already takes exactly this loss one rung over. A typo'd *read* — `$staus` — is a
run-time unbound-variable error rather than a syntax error, the accepted cost of having
no `let` / `var`. Demanding a parse-time answer for `:name` would hold modifiers to a
stricter standard than variables and commands, which is the inconsistency rather than
the guarantee.

And the shell precedent points the same way. bash's **alias** is its one construct
resolved when a line is *read* rather than when it runs, and it is exactly the one that
surprises people: `alias hi="echo hi"; hi` in a single parse unit reports `hi: command
not found`, and so does an alias defined after a function that uses it. bash's
*functions* have no such problem — `f() { g; }; g() { …; }; f` works — because they are
looked up late. Early resolution is what makes the aliases brittle, and there is no
reason `:name` should be the one construct in mesh that repeats it.

**What this removes.** An earlier revision built three further rules on a load-time
check: declarations **hoisted** so a forward use had a body to call, declarations
**banned below top level** so nothing could bind conditionally, and the **`source`
boundary** left explicitly undecided because a sourced library's modifiers could not be
seen by a text-only pre-pass. None of the three is needed once resolution is late. A
nested or conditional `func _s:name()` is as legal as a nested `func` and binds the same
way; a library's modifiers work in the script that sources them; and textual order
decides which body is live exactly as it does for `func`. The blocked `source` question
is not answered here — it is *not raised* here, and stays where it already lived, with
the static-checker item.

**A built-in modifier name may not be redeclared**, on the principle that already
governs this. `func _s:upper()` is a **loud error at the declaration**, for the reason
mesh already refuses `func puts` and `func cd`: a name the shell resolves first makes
the definition *unreachable*, and silently dead code is the failure mode that rule
exists to prevent.

What it does **not** do is widen the existing command-name reserved set. Modifiers are
their own vocabulary, so the two declarations are independent: `func _s:upper()` is
refused because `:upper` is a built-in **modifier**, while `func upper() { tr a-z A-Z }`
stays perfectly legal — `upper` is not a builtin *command*, and a shipped modifier has
no claim on the command namespace. One principle, applied per namespace; adding modifier
names to the command-side check would break working code.

**An argument reaches a modifier through braces** *(shipped)*. Inside `"…"`, an
attached `(` after `:name` is never literal text — it is the modifier's argument list,
and it always reports, because an unbraced `$…` interpolation cannot pass one. Which
complaint you get is the modifier's to make: `"$x:upper(foo)"` says **`:upper` does
not take arguments** — the same message the braced `"${x:upper(foo)}"` already gives
— while `"$x:bar(8)"`, for a modifier that does take one, points at
`"${x:bar(8)}"`. Bracing advice is given only where bracing is the fix.

That split needs the modifier's arity, so the check lands **where arity is known**,
which for a declared modifier is run time: the name is not resolved when the string
is parsed, and a later declaration can change what it means. Today the parser decides
both cases itself, from a fixed table.

This **breaks shipped behavior**, deliberately. After an argument-free built-in an
attached `(` is ordinary text today — `"$x:upper(foo)"` prints `AB(foo)`, pinned in
`crates/mesh/tests/cli.rs` — and it will report instead. Literal text after a chain
keeps a spelling: end the chain with braces, `"${x:upper}(foo)"`. Only an *abutting*
`(` after `:name` changes; `"$x:upper (1)"`, `"($x:upper)"` and `"$x(foo)"` are
untouched, and the braced form is unaffected throughout.

A **map** subject has no element-wise meaning yet and **errors**, naming `:keys` /
`:values` — see [Open questions](#open-questions), where it is parked rather than
decided here.

### Globbing

- `**` — recursive, **on by default** (no `globstar`-style opt-in).
- `*/`, `**/` — directories (trailing slash, existing muscle memory).
- **Qualifiers are the glob's argument list.** The `(...)` after a glob carries its
  **options**, the same comma grammar as any [value call](#calling-for-a-value-and-lambdas)
  — `*(...)` is sugar for `glob("*", ...)`. The options are **ANDed predicates** of
  three kinds:
  - **`type:`** — the file-type dimension, *mutually exclusive*: `type: file`,
    `type: dir`, `type: symlink`, or an alternation `type: file|dir` for "either." The
    `find -type` **letters are shorthand** — `f` ≡ `type: file`, `d` ≡ `type: dir`,
    `l` ≡ `type: symlink` (and the rarer `p s b c` for fifo/socket/block/char).
  - **boolean predicates** — orthogonal tests: `exec: true` (shorthand `x`),
    `empty: true`. A file can be executable *and* over a size, so these are independent
    booleans, not part of the exclusive `type:`.
  - **comparisons** — real predicate expressions with the type-directed operators,
    `size > 1M`, `age < 1d` (`>` / `<` read better than zsh's `+/-` age codes).

  ```
  *(type: file)             # long form
  *(f)                      # shorthand ≡ type: file
  *(f, x)                   # ≡ type: file, exec: true — executable files
  *(f, size > 1M)           # type + a comparison predicate
  *(type: file|dir)         # either type
  glob("*", type: file, size > 1M)   # the same options, via the function
  ```

  Qualifier arguments are evaluated once **per candidate path** in a dedicated
  predicate context. In that context `size`, `age`, `type`, `exec`, and `empty`
  are properties of the current candidate; they are not ordinary caller-scope
  names or expressions evaluated before `glob` starts. The literal and function
  forms use this same binding rule.

  There is also a terse **`:`-modifier** shorthand for the common single-type filter,
  usable on a glob *or* a plain list — `*:f` / `*:files` / `$paths:files`, so
  `*:f == *(f)` — which the engine **fuses** into matching, so `**:files` never
  materializes non-files.

- **These qualifiers are expansion-only.** `(f)` / `(d)` / `(x)` and the `size` /
  `age` / `empty` predicates all inspect the **filesystem**, so they belong to
  globbing (finding files), never to string matching. A `~` / `match` / `fnmatch`
  pattern uses only the plain glob metacharacters (`* ? [ ] { } **`), which need no
  disk: `$f ~ *.txt` tests the string alone, while `*(f)` / `*(size > 1M)` are
  meaningful only where real files exist to stat.

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

  **In front of a command, the spelling is open.** All three examples above are
  *statements*, and as statements they are consistent with the rest of the language:
  they need glob-led classification and list difference, both unbuilt but both
  tracked. Put one after a command, though, and [arithmetic](#arithmetic) decides
  the other way — operators between argv words are deliberately not operators — so
  `puts *.txt - *.bak` prints `a.txt b.txt - c.bak d.bak`, expanding both globs and
  passing the dash along. Value contexts are fine (`x = *.txt - *.bak` and
  `for f in (* - *.bak)` both reach evaluation), so what has no spelling is the
  interactive case, `rm * - *.bak`. Which spelling it should get — parens, an
  argv-position operator, zsh's unspaced `*~*.bak`, or a `not:` qualifier — is an
  [open question](#open-questions).

- **Braces** — kept (`*.{jpg,png}`); universally understood.
- **ksh extended globs** (`!(…)`, `@(…)`, `+(…)`) — **dropped.** Cryptic, and
  their jobs are covered by braces + exclusion.

**The `glob()` family — globbing expands, matching is separate.** A glob's one job is
to **find files**: `glob(STR)` and the bare literal forms above are **eager** — they
touch the filesystem and hand back a plain [list](#arrays-lists) of matching paths.
There is no lazy "glob value"; a glob is either a **literal you write** or a **list you
got back**.

```
*.txt                     # bare literal → the matching paths (a list)
glob("*.log")             # same, but from a string  → a list
glob("src/**"):files      # recursion, then a type filter on the returned list
```

The two ergonomic wrappers are **expansion** helpers — they match now and return a
plain [list](#arrays-lists), so they read naturally in a `for`. They enumerate a
**directory's** immediate entries (`find -maxdepth 1`) filtered by type — reusing the
`files` / `dirs` words that name the same filter as the `:files` / `(f)`
[qualifiers](#modifiers):

```
files(DIR=.)              # files directly in DIR   (find DIR -mindepth 1 -maxdepth 1 -type f)
dirs(DIR=.)               # subdirectories of DIR   (find DIR -mindepth 1 -maxdepth 1 -type d)

for f in files() { … }    # PWD by default
for d in dirs()  { … }
for f in files(src) { … } # a named directory
```

They are `DIR/*` with the filter applied, so they inherit globbing's policies
rather than growing their own: entries are sorted, a **hidden** entry is skipped
(the `find` comparison above is about depth, not the dotfile rule — `*` is the
authority there), and a missing or unreadable directory is the empty list, as any
pattern that matched nothing is. An entry is prefixed by the directory it was
asked for, `.` adding none — `files(src)` is `src/a.txt`, `files()` is `a.txt`.

**Matching a string is a different operation.** Finding files (touches the disk) and
asking "does this *string* look like this pattern" (no disk at all) split the way
Python splits `glob` from `fnmatch`. The `~` operator carries the match side:
`$f ~ *.txt` is a bool — whole-string fnmatch, **no filesystem access** (see [Tests and
comparisons](#tests-and-comparisons)). A pattern built at runtime is matched by the
predicate directly — [`fnmatch($f, $pat)`](#built-ins) — so no first-class glob value
is needed to test against a computed pattern. (Regex keeps its `re(STR)` *value*
because regexes are complex and reused; a glob stays a literal or an `fnmatch` call.)

**A value never re-globs — and laziness is a thunk.** A pattern stored in a string is
inert; only a literal you *write* or an explicit `glob(…)` call touches the filesystem:

```
p = "*.jpg"               # a plain string — quoted, since a bare *.jpg would expand here
ls $p                     # passes the literal string *.jpg — a value never re-globs
ls ...glob($p)            # expand it now: glob() returns the list, ... splats it to argv
files = glob($p)          # or bind the list and reuse it
```

Because `glob()` is eager, deferring it needs no special lazy type — just wrap it in a
thunk: `later = func() { glob("*.txt") }` stores the *call*, and each `$later()` re-globs
against the **current** filesystem (fresh every time, which is what "lazy" is usually
for).

**Splatting to a command.** A bare literal in argument position splats its matches
straight into argv — `ls *.txt` is N arguments — because you wrote it there. Any glob
result you have **stored** (or got from `glob()` or a wrapper) is an ordinary **list**,
so handing it to an external takes the explicit [`...`](#spread--flattening) every list
does, or you iterate it:

```
ls *.txt                  # literal: splats in place, N argv entries
ls ...glob($pat)          # a runtime list → external: spread, as any list
for f in files(src) { }   # or iterate it — no spread needed
```

Daily globbing is the bare literal and needs no `...`; the spread shows up only for the
same case any stored list does — you stashed the list and want it as separate arguments.

**Functions look like functions.** `glob` / `files` / `dirs` are
[value calls](#calling-for-a-value-and-lambdas) — `files(.)`, parens attached — never
bare `files .`, so at a glance a glob **function** stays distinct from an external
**command** even in statement position.

**Two policies the primitive pins.** `*` matches *everything not hidden* — files,
dirs, and symlinks alike — and is deliberately **not** narrowed to files-only (else
`cp -r * dst` would silently skip subdirectories, a fresh footgun traded for the old
one); the file / dir / special split lives entirely in the `(f)` / `(d)` / `:files`
vocabulary. A hidden (leading-dot) entry matches only when the corresponding **path
component** of the pattern itself begins with a literal `.` — the usual per-component
rule, so `*` skips `.git` while `.*` and `src/.*` match it. **No-match:** an expansion
that matches nothing is the empty list `[]` (programmatic use never throws) — and since
globbing is eager there is no stored pattern to disagree with that; a bare *literal*
matching nothing in command position **warns but does not error** — it expands to
nothing rather than passing the literal through (bash's footgun). *(TODO —
interactively, **prompt** on no match instead of only warning.)*

### Arithmetic

Integers are **`i64`**, signed, and every operation is **checked** — overflow is a
loud error, never a wrap. There is a **float** (`f64`, below) but no unsigned type
and no arbitrary-precision integer; `Duration` and `Instant` carry their own closed
arithmetic, defined with the time model.

> **Not implemented.** The float half of this section is **design only** — `Value`
> has no float variant, so `1.0` lexes as the *string* `'1.0'` and every claim
> below about float behavior describes what is intended, not what runs. Two
> consequences are live defects rather than missing features, because they read as
> working:
>
> ```mesh
> (1.0 + 1)      # error: expected integer — not 2.5
> (10.0 < 2.0)   # true  — decimal text compares lexicographically
> (0.5 < 0.10)   # false — same cause
> ```
>
> Ordering has one numeric arm (int vs int) and otherwise falls through to text,
> so decimal-looking strings sort as strings. `1.0 < 2.0` giving the right answer
> is a coincidence of digit order. Tracked in `TODO.md`; until a float type
> exists, do not read this section as describing behavior.

**Where arithmetic happens** *(decided)*. A bare word is a command, so arithmetic
needs a context that is unambiguously a value. There are two, following nushell and
PowerShell:

- **A statement whose first word is a number** is an expression, not a command, so
  `1 + 2` at the prompt prints `3`. Nothing is given up: no command is named by an
  integer literal, and the parser already uses "is this word numeric" to tell a
  value from a command word.
- **Parentheses**, wherever a value is expected — a command argument included:

  ```
  puts (1 + 2)
  puts ($n + 3)
  retry --sleep ($base * 2)
  ```

`puts 1 + 2` without the parens is deliberately **not** arithmetic. Making `+` an
operator between argv words would either refuse `$n` operands — leaving the case
anyone actually wants unserved — or turn `mycmd $file + $other` into a type error,
and it would break `find . -exec grep foo {} +`. Every shell draws this line
somewhere: bash needs `$(( ))`, fish needs `math`, and nushell and PowerShell put it
exactly where mesh does.

**Operators** *(decided)*. `+`, `-`, `*`, `/`, `%`, with the usual precedence.
**Integer** division and remainder follow **Rust and bash**: the quotient truncates
toward zero and the remainder takes the sign of the **dividend**, so `-7 / 2` is
`-3`, `-7 % 2` is `-1`, and `(a / b) * b + a % b == a` holds. That identity is
integer-only, and stops holding the moment either operand is a
[float](#arithmetic), where `/` is fractional rather than truncating — see there
for how a float remainder is defined. That also agrees with `:ms` / `:secs`, which
already truncate toward zero. (Python, Ruby and Perl floor instead, giving `-4` and
`1` — mesh does not.) Division by zero is a **loud error**.

Exponentiation is a **modifier**, not an operator: `$b:pow(3)`. Rust has no exponent
operator either; `^` would collide with any future bitwise use, `**` needs a
precedence rule of its own, and the modifier form matches `$m:int` / `$a:ms` — while
giving the negative-exponent case somewhere honest to fail, an integer power having
no answer there.

**Binary `-`, and the glob-exclusion collision** *(decided)*. `-` subtracts
numbers and takes the **difference of lists**, dispatching on its operands — one
operator over two operand types, exactly as `+` already concatenates strings,
extends lists, merges maps and adds integers.

There turns out to be no collision to legislate, because bare glob literals are
[eager](#globbing): they touch the filesystem and hand back a plain list. So by
the time `-` evaluates, `* - *.bak` is a *list* minus a *list*, and glob exclusion
**is** list difference rather than a second meaning of the operator. The parse
never forks either — `-` always reads as binary minus, and only evaluation asks
what it was handed.

```mesh
* - *.bak                      # every file except the backups
[a b c] - [b]                  # [a c]
```

Three rules the list form needs, none of them glob-specific:

- **The left order is kept**, and every occurrence of a removed element goes, so
  the result is "the left list with those values taken out" rather than a set.
- **Removing what is not there is fine**, unlike the [`unset`](#variables-and-assignment)
  family's fail-loud rule. `* - *.bak` in a directory with no backups is the
  ordinary case, not a mistake worth reporting — an exclusion names a *filter*,
  where `unset` names a thing the writer believes exists.
- **Elements compare by value**, the same equality `:dedup` and `==` use.

Mixed operands (`[a] - 1`, `5 - [a]`) are a loud error that names both accepted
shapes, since neither reading is recoverable.

**A glob-led statement has to be classified as a value**, and today it is not:
`outranks_a_command` promotes a bare leading scalar only when it is an integer, a
boolean, or quoted, so `* - *.bak` on its own line stays a *command pipeline* and
tries to **run the first matching file** — `command not found: a.txt`, verified.
A bracketed list leads with a non-scalar and is already classified as a value, which
is why `[a b c] - [b]` reaches evaluation and the glob form does not. Making the
headline spelling work therefore means teaching that classification about a
glob-led expression, not only implementing the list operation; inside parentheses
the question does not arise. Raised in review on mikelward/mesh#341.

**Argument position is a second gap, and a quieter one.** The classification above
fixes the statement, and a value context needs nothing beyond the operation itself —
`x = *.txt - *.bak` and `for f in (* - *.bak)` both reach evaluation and stop at the
`expected integer` that says list difference is unbuilt. Neither reaches
`rm * - *.bak`, where by the *where arithmetic happens* rule above the dash is an
argv word and not an operator at all. That form is the one that does not report:
`puts *.txt - *.bak` prints `a.txt b.txt - c.bak d.bak` and `/bin/echo *(f) - *.tmp`
passes the dash through, so a wrong answer arrives wearing the shape of a right one
where the statement forms at least fail loudly. Nor does the
parenthesized form reach an external command yet: `/bin/echo (*.txt)` reports
``a list needs `...` to become command arguments``, and the `...` it names,
`/bin/echo ...(*.txt)`, is itself a syntax error — the `CommandItem::Value` spread
gap tracked with `ls ...glob($p)`. Builtins are unaffected, `puts (*.txt)` taking
the list directly. The candidates are laid out in [Open questions](#open-questions).

What keeps all of this off kebab-case names is the
[operators-need-spaces](#globbing) rule: `a-b` is one name, `a - b` is the
operator, `$a-$b` interpolates with a literal hyphen.

**Floats** *(decided)*. A second number type, **`f64`**. Two things forced it. A
shell gets used as a calculator, where integer-only arithmetic stops paying. And
without it `3.5` is not a number but a *word*, which makes `9.5 < 10.5` answer
**`false`** — `<` compares two strings lexically, exactly as specified, and the
result is still wrong. That is the only place mesh returns a quiet wrong answer
rather than an error, and it is reason enough on its own.

**Take Rust's operation wherever Rust has one.** Integer `/` truncating toward
zero, `%` as `fmod`, and checked overflow are Rust's own semantics, adopted rather
than specified — every numeric rule mesh writes down itself is one it can get
wrong, and the review of this section proved that the hard way. Where an
*arithmetic* edge is unstated below, Rust's answer is the intended one.

**Rendering is excluded from that rule**, because Rust's `{}` never switches to
exponent form — it prints `1e300` as 301 digits. Digit *selection* is Rust's
(shortest round-trip); the exponent switch is mesh's own, below.

**Where mesh diverges, it diverges deliberately.** Each of these buys something
the [fail-loud](#variables-and-assignment) model needs, so none should be
"simplified" back toward Rust. The list is the notable ones rather than a closed
set — an earlier draft claimed "and only three" and was twice wrong, which is
itself the argument against counting:

- **A normalized value space — no NaN, no infinity, no negative zero.** Rust
  yields all three (`-4.0 % 2.0` is `-0` there, and prints that way); mesh raises
  on the first two and folds `-0.0` into `0.0`. That is what keeps `<` a total
  order, `==` an equivalence, and rendering single-valued.
- **Implicit widening.** Rust has none — `1i64 + 1.0f64` does not compile — while
  mesh promotes the integer, because a shell used as a calculator cannot ask for
  casts.
- **Cross-type comparison.** Rust does not offer it at all, so `1 == 1.0` and
  `1 < 1.5` are mesh's own and have to be written by hand; that is the one place
  hand-rolling is unavoidable, and the exactness rule below is why.
- **Checked float-to-integer conversion.** Rust's `as` saturates silently, so
  `1e20:int` would answer `i64::MAX`; mesh raises instead, per `:int` below.

**`/` does not change.** Two integers divide to an integer, truncating toward zero
as above; a float appears only when an operand is one.

```mesh
(10 / 3)                       # 3      — unchanged
(10.0 / 3)                     # 3.3333333333333335
($xs:len / 2)                  # still an index
```

This is C, Go, Java and Rust, and it is the reason **no `//` operator is needed** —
Python only wants one because its `/` always floats. It also means the time model's
`$a:ms / $b:ms` stays ordinary integer division, so the argument made there needs no
revision. Where two integers *should* divide fractionally, multiplying one by `1.0`
is the spelling (`$hits * 1.0 / $total`); no cast function is wanted, since a float
literal already says it.

**Widen freely, compare exactly.** Mixed arithmetic promotes the integer, so
`(1 + 1.5)` is `2.5`. Comparison and equality must **not** go through that
promotion — `i64`→`f64` is lossy above 2⁵³, and nanosecond `Duration`s live well
past it, so comparing by cast would lose precision in the range mesh actually uses.
That is the same silent wrong answer floats are being added to remove. Python
compares the two exactly for this reason; JavaScript before BigInt is the
cautionary tale.

**`1 == 1.0` is true**, and equality stays type-strict against everything else
(`1 == "1"` **reports**, per
[Comparison across types](#comparison-across-types) — it was a silent `false`
when this paragraph was written). The two rules agree rather than conflict: int
and float share a single projection, the number, so respecting it forces no
contradiction, which is exactly the test that entry sets for when a cross-type
equality is allowed at all. This is a **choice**, not something rendering forces:
`42` and `"42"` already display identically and compare unequal, so "renders the
same" has never implied "is the same" here. The reason is that every language that
widens on mixed arithmetic — Python, Lua, Ruby — compares numerically across its
number types, and an arithmetic `1 == 1.0` answering false is a trap in a
calculator. The comparison is exact, per the rule above, not a promotion to `f64`.

`Hash` has to agree, since `:dedup` is a `HashSet<Value>` and `[1 1.0]:dedup` must
collapse to one element: an integral float **that fits `i64`** hashes as that
integer. One that does not (`1e20`) is no integer's equal anyway, so it hashes as
a float — collapsing those onto `i64::MAX` would only manufacture collisions. Rust derives
neither `Hash` nor `Eq` for `f64`, so both are written by hand — which banning NaN
below is what makes legitimate.

**No NaN and no infinity, ever.** Division or remainder by zero, and overflow, are
**loud errors** — the rule checked integer arithmetic already follows. The payoff
is that the value space stays totally ordered: `<` is a total order, `==` an
equivalence, and sorting needs no special case. `-0.0` normalizes to `0.0`.

**Rendering** is shortest round-trip, and switches to exponent form for large and
small magnitudes rather than printing `1e300` as 301 digits — Python's thresholds
are a sane starting point, not a commitment. Display drops a trailing `.0`, so
`(6.0 / 3)` shows `2`; a shell should not announce the type of an answer.

**`:repr` keeps the type that display drops**, writing an integral float as
`1.0`. Its contract is that the output reads back as the *same* value, not merely
an equal one — the reason `42` and `'42'` are already spelled apart there — and
`1` would read back as an integer, which divides differently.

**`%` stays dividend-signed** for floats as for integers, so `(-10 % 3)` and
`(-10.0 % 3.0)` are both `-1`. Python answers `2` here; mesh diverges deliberately
and has since the integer rule was set, and one operator cannot have two sign
conventions. The operation is **`fmod`** — C's `fmod`, Rust's `%` — which is
already dividend-signed; a zero divisor is a loud error, as it is for `/`.

The [integer identity](#arithmetic) `(a / b) * b + a % b == a` is integer-only and
does not carry over, since float `/` does not truncate.

*(The numeric edges — which quotient `fmod` uses, what a naive
`a - trunc(a / b) * b` expansion loses at scale, where the domain stops — are
implementation detail rather than language decisions, and are recorded with their
counterexamples in the `TODO.md` entry. Kept out of here deliberately: writing
them as prose invited three separate corrections in review without changing a
single decision.)*

**`:num`** parses a string to a number — the `:int` twin, fail-loud on non-numeric
input, yielding an integer where the text names one and a float otherwise. `:int`
on a float truncates toward zero, and a result outside `i64` is a **loud range
error**, never a clamp — the same refusal `:int` already gives an out-of-range
string.

*(Deferred, with the reasons, so they are not re-litigated from scratch. **Decimal
and rational** would fix `0.1 + 0.2` and `1/3` respectively; each costs a
dependency, and binary-float rounding is what every language a mesh user already
knows does. **Arbitrary-precision integers** would retire overflow as a category,
but `i64` covers every realistic shell quantity — file sizes to ~9.2 exabytes,
nanosecond timestamps to 2262 — overflow is already *safe* rather than merely
absent, and a bignum does not remove the boundary checks that indices, exit codes
and `Duration` nanos need, it relocates them. If a calculator doing factorials
makes it real, the shape to reach for is promote-on-overflow behind the same
integer face, which changes no user-visible rule except that overflow stops
erroring.)*

**Literals** *(decided)*. Decimal, plus `0x` / `0o` / `0b` prefixes — `0xff`,
`0o755`, `0b1011`. Underscores group digits, following **Python's placement rule**:
exactly one, only between two digits, so `1_000_000` is fine while `_1`, `1_`, and
`1__0` are errors. Group *size* is deliberately **not** checked, which is what makes
non-Western grouping work for nothing — `1_00_00_000` (one crore) and `1_0000_0000`
(一億) are as valid as `1_000_000`. Rust is looser still and accepts `1_` and `1__0`;
no grouping convention wants those.

A literal is a **float** when it carries a `.` or an `e` — `3.5`, `1e10`, `2.5e-3` —
so the exponent form always means float, as in Python. Two lexing rules follow: a
`.` must have a digit on **both** sides, so `1..5` stays a range rather than `1.`
followed by `.5`; and the `0x` / `0o` / `0b` prefixes take neither, an integer being
the only thing a radix literal names.

A leading zero means **neither octal nor decimal**: `007` is the *string* `007`.
The open question here used to be octal-or-decimal, and the note recorded that
`007` silently parsing as `7` was "the one answer that is certainly wrong" — it
was, and the reason generalizes past leading zeros. An integer carries no record
of how it was written, so any spelling that is not the number's own is lost the
moment it binds. A **decimal** literal is therefore an integer only when its text
is that integer's own spelling, which leaves `007`, `08`, `+5` and `-0` as
strings.

The rule is scoped to decimal on purpose, and says nothing about the grouped and
radix forms decided above. `1_0` and `0x10` are integers under those rules and
`1e3` is a float; they are strings *today* only because none of the three is
implemented yet, which is an implementation state rather than a decision. When
they land, each brings its own canonical spelling — the question this rule
answers for them is not "integer or string" but *which* text round-trips, and
`0x10` printing back as `16` would lose a spelling exactly as `007` did.

That keeps the spelling wherever it travels, which is what the bug was: a word
passed through a `func` parameter, `...rest`, `alias`, or an assignment came out
renumbered, while a direct external argument and `$sh.args` kept it — so putting
a mesh function in front of a command changed what the command received. The
cost is that `007 + 1` is an error asking for `$n:int + 1`, the rule every other
string already follows; a numeral whose spelling matters is usually an
identifier (a mode, a version segment, a zero-padded index) rather than a
quantity. Octal is unaffected: `0o755` is the spelling that says what it means,
and it is an integer under the radix rule above once that form is implemented.

One consequence worth stating, since it is what makes this a *language* rule
rather than a binding rule: a bare word that is not a typed literal names a
**command**, so `007` in statement position runs a program of that name where a
bare `7` is a discarded value. The parser's value-or-command test and the
argument-typing rule ask one shared predicate so they cannot answer differently.

**Value positions only.** All of this governs *in-shell* values; the process
boundary stays bytes. `chmod 0644 f` passes `0644` and `ls 1_000` names the
directory `1_000` — integer parsing never reaches argv, which is also why a
C-style octal literal would not buy the `chmod` case anything.

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

- **Scope — two levels, lexical.** There are two *kinds* of variable scope:
  the **session-global** scope (top-level rc and interactive bindings) and a fresh
  **function-local** scope per `func` call. Two is the **depth**, and the decided
  [lambda capture](#calling-for-a-value-and-lambdas) rule keeps it there: a lambda's
  scope holds its parameters plus the values its `with (…)` list copied in, and its
  parent is the session. There is no chain of defining scopes to walk, which is the
  point of capturing values rather than scopes — a captured name is an ordinary
  current-scope binding, so nothing outlives the frame that wrote it. The environment
  (exported names) is a separate axis. Scoping is **lexical**: a function sees its own locals, its
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

  Reading resolves **outward** along that chain (local → global) — and a lambda's
  captured names are in the *local* rung, having been copied there from the scope that
  *wrote* the lambda, never the one that calls it; an **unbound** name is an
  **error**, not empty — the always-on `set -u` that the *no null* rule below
  already implies, so a **typo'd read fails loud** (`$staus` → error). The one
  place a typo is *not* caught is **assignment**, which always creates
  (`staus = 5` binds a new var) — the cost of having no `let`/`var` keyword;
  reads carry the fail-loud guarantee, writes create. The **total read** for a
  maybe-unset name is the same `:get`
  that maps use, because the **environment is a first-class map named `env`**:

  ```
  editor = $env:get(EDITOR, vim)  # total: value, or "vim" if unset — never errors
  $env.EDITOR                   # strict: errors if unset (like any $m.key)
  if $env:has(SSH_AUTH_SOCK) { … }
  ```

  So `$env.EDITOR` (a strict read) errors when unset, and `$env:get(EDITOR, vim)`
  is the safe defaulting form — no new syntax, just the map surface applied to
  the environment.
- **No block scope; `unset` removes a scope's binding.** Control-flow blocks
  (`if` / `for` / `while` / `loop`) do **not** open a new scope, so
  `if c { x = 1 }` then `$x` works — a block adds **no rung**, and depth comes from
  `func` calls alone.

  The **`for` binder is the one exception**, and it is a binder rather than a
  block rule: it belongs to its loop, fresh each iteration and gone at the end
  (see [`for` binding](#calling-for-a-value-and-lambdas)), so `$_i` after the loop
  is an unbound read. That is not a rung either — the binding lives in the
  enclosing scope for the loop's duration and any name it shadows is put back
  afterwards — it is a *lifetime*, chosen so that a lambda written in the body
  cannot silently read one shared slot and see only its last value.
  **`unset name`** removes the binding **in the
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
  $dirs:join(":")`). **The one exception is path-type variables** —
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
  `global`-assign an already-exported name). For a **temporary** env change,
  `with NAME=value … { … }` runs a block with those entries in place and restores
  what was there on the way out, however the block leaves — so scoping and
  restoring the environment is **settled** for a block rather than deferred.
  The **one-command prefix** (`NAME=value cmd`) is settled too, and rides on
  `with`'s mechanism as expected: it binds to a *stage* rather than a statement,
  so `FOO=1 a | FOO=2 b` gives each side its own and `FOO=1 a && b` leaves `b`
  alone. A run of them is one prefix (`TZ=UTC LANG=C date`), `+=` appends, and a
  run with no command after it is the assignment it always was — `x=1` is
  unchanged. Like `export`, the prefix reaches the **environment** rather than
  binding a shell name, since a prefix that wrote a shell binding would give the
  child nothing. More precisely — and this is the authoritative statement, refined
  from an earlier "writes the environment, and deliberately collides with a shell
  binding" — **for the region the name lives on the environment rung and only
  there**: a shell binding of the same name is *masked* for the duration, not
  duplicated, and restored on the way out. See
  [Shadowing, bounded](#variables-and-assignment) for why the masking matters and
  what it means for writes made while the region is live.
  A whole **function** scoping its environment implicitly stays
  the deferred *isolation* question (see [Open questions](#open-questions)) —
  `with` is explicit, which is the property that made it decidable first.
- **Types are inferred, not declared.** `x = foo` is a string, `x = [a b c]` a
  list, `x = [a: 1]` a map. There is no type sigil (`@`, `%`) on the *name* —
  a variable just holds whatever value it was given, and `$x` reads it back.
  Perl-style sigils (`@PATH` a list, `$PATH` a scalar) were considered and
  rejected: a variable's type here is the *value's* business, not the name's, so
  a name-baked sigil would lie the moment a var is reassigned a different shape —
  and Perl's context-varying sigil (where `$foo[0]` indexes the array `@foo`) is
  a notorious footgun. `$name` means one thing everywhere: "read this variable."
- **String interpolation.** Inside `"…"`, unbraced member access and integer
  indexing work exactly as they do outside strings: `"$m.key"` and `"$xs[0]"`.
  Braces remain available for the same references and delimit them when literal
  text could otherwise be consumed as access: `"${m.key}"`, `"${xs[0]}"`,
  `"${file}.txt"`, `"${dir}s"`. General expressions also use `${…}`.
- **No null.** mesh has **no `nil`/`null`/`none`** value — the billion-dollar
  mistake is left out. The consequence is a consistent rule wherever a value
  might be absent: **exact** access fails loud (`$xs[99]`, `$m[absent]` are
  errors), **total** access takes a default (`$xs:get(i, d)`, `$m:get(k, d)`), and
  a **control-flow gap** yields the empty string (a no-`else` `if`). Nothing
  silently returns a null that has to be checked for downstream. *(open — the
  one genuine fork this leaves: is a first-class absent value ever worth adding
  back for, e.g., "key present but unset"? Current answer: no; `:has` +
  `:get(key, default)` cover it.)*

**Special variables live in two namespace maps** — the *(decided)* way to keep
the shell's built-in state out of your variable namespace. The whole lowercase
top-level is **yours**; the built-ins hang off two reserved roots:

- **`$env`** — the process environment, accessed by name: `$env.EDITOR`,
  `$env.HOME`. **`$env.PATH` is a list** — `$env.PATH += /opt/bin`,
  `$env.PATH:dedup`, `$env.PATH:has(/usr/bin)` all just work, which is the
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
  **`$sh.status`** (last exit, the readable replacement for `$?` — a **`Status`**
  value per the [status decision](#open-questions), whose code is `0`–`255`.
  `if $sh.status { … }` is the idiomatic test, and **`$sh.status == 0` is true on
  success** — a status compares to an int by its code, the one declared cross-type
  pair, so the shell reflex reads correctly rather than needing `:code`; see
  [Comparison across types](#comparison-across-types)),
  **`$sh.pipestatus`** (a **list** of the last pipeline's stage statuses, each a
  `Status`, where real lists beat bash's `PIPESTATUS`), `$sh.pid` / `$sh.ppid` (own and parent PID,
  bash's `$$` / `$PPID`), `$sh.uid` (effective user id), `$sh.version`, `$sh.options`,
  `$sh.interactive`, the **stream handles** `$sh.stdin` / `$sh.stdout` / `$sh.stderr`
  (each with a `:tty` test — the `test -t N` replacement), **`$sh.jobs`** (the live
  [job-control](#job-control) map),
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
`$sh.pipestatus`, `$sh.pid`, `$sh.ppid`, `$sh.uid`, `$sh.version`, `$sh.interactive`, the
`$sh.stdin` / `$sh.stdout` / `$sh.stderr` handles, `$sh.jobs` with
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

**Decided, and landed for `$sh.options`.** A setting is written one at a time —
`$sh.options.NAME = false` — and takes effect at once, in the session doing the
writing; there is no whole-map assignment, because a map literal that omitted a
key would have to mean either "leave it" or "reset it". The keys are **fixed** and
the values are **booleans**: an unknown key is refused rather than added (a
settings map that absorbs a typo is a setting silently not applied), a non-boolean
is refused rather than coerced (`"false"` is a *string*, and a truthiness rule
would turn the setting on), and `unset` is refused too — removing a setting would
leave the shell's question unanswered rather than restore a default, and assigning
is the way back. `global` does not apply: `$sh` is the session's, not a scope's,
so a function that writes a setting has written it for the shell. Every setting is
**on by default** and governs an interactive decoration, so turning one off never
changes what a command *does* — see [Terminal control](#hooks-and-the-prompt).

*(TODO: **indirect / by-name variable access.** Real configs reach a value through
a *computed* name — fish's `my_set_color` does `eval "printf \$$arg"` to read the
variable named by `$arg` (`bold`, `blue`, …); bash has `${!var}` and `declare -n`,
zsh the `${(P)var}` flag, ksh namerefs (`typeset -n`). mesh has **no** by-name access
to the variable
namespace, deliberately so far — the intended answer is to put such values in a
**map** and index it (`$colors[$name]`), which is first-class and needs no `eval`.
Because `$env` / `$sh` are already maps, indirect *environment* **reading** fell
out for free (`$env[$name]`); the matching **write** and removal did not, and were
added deliberately so the pair is symmetric — `$env[$name] = value` and
`unset $env[$name]` resolve the subscript through the same resolver the read uses.
That is the whole of by-name access mesh has, and it is confined to the namespace
that is a real table in the process. Open question, still open for the *variable*
namespace: is a map always enough, or is a narrow by-name facility warranted for
genuine metaprogramming? Leaning: maps only — revisit only if a real need survives
the reframe.)*

**Bare environment references, and one scope ladder** *(decided for now — the
strong form, adopted to be tried in real use and reversible if it does not hold
up)*. `$env.NAME` is the only spelling for the environment today,
and in practice nobody wants to write `$env.ANYTHING` for the handful of names
they touch daily. `$PATH` is an unbound-variable error (`expand.rs:279`) and bare
`PATH` is the word `PATH`. The shape that makes both work, taken as a set because
the pieces only hold together as one:

- ***Three rungs, one existing rule.*** Scopes nest **local ⊂ session ⊂
  environment**, and the rule is the one the store already implements: reads fall
  outward (`vars.rs:1198`, innermost local then global), writes stay put
  (`vars.rs:1145`, the active scope), and an outward write names its scope —
  `global x = v` for the session (`vars.rs:1158`), `export X = v` for the
  environment. The environment is a third rung, not a new concept. `export` is
  then required uniformly at *every* scope, which is less special-casing than
  today, not more.
- ***Reads fall out to the environment.*** An unbound `$NAME` resolves
  `$env.NAME`. The trigger is **presence in the environment**, not a
  SCREAMING_CASE convention — `http_proxy` and `no_proxy` are real, common,
  lowercase environment variables, and a case rule cannot see them.
- ***Writes must not reach through, and that is what makes the fallback safe.***
  A shell binding named `PATH` is **inert**: command lookup reads the process
  environment directly (`whence.rs:415`, `env::var_os("PATH")`), never a binding.
  So a read fallback paired with a local-binding write would let
  `PATH = /opt/bin:$PATH` bind a local, let `$PATH` read that local straight back
  and *confirm* the value, and change nothing whatsoever about what runs. The
  fallback manufactures that trap; the next bullet removes it.
- ***No shadowing, at any rung*** — more precisely, **no unbounded, unmarked
  second binding** of a name; rebinding one for a bounded, marked region is
  exactly what `NAME=value cmd` and `with` are for, and is untouched (see
  [Shadowing, bounded](#variables-and-assignment) below, which is the statement to
  read this bullet by). A binding may not be *created* where the name is
  already visible further out — not local over session, not session over
  environment, and (once [lambdas capture](#calling-for-a-value-and-lambdas)) not a
  lambda parameter over a captured local. The list is the rungs that exist, not the
  extent of the ban: a new rung inherits it rather than escaping it. `PATH = /x` is refused, naming `export`, in the same teaching-error
  style as the bare-`export` refusal (`parser.rs:5530`). The payoff is that
  `$NAME` has no precedence question at all: a collision cannot exist, so the
  fallthrough in `vars.rs:1198` can never actually choose. Worth noting Python
  faces this exact situation and binds the local silently, then fails on read —
  `UnboundLocalError` is one of its most-hit footguns. Erroring at the assignment
  is strictly better.
- ***`_`-prefixed locals, enforced rather than styled.*** No-shadow on its own is
  non-modular: `func f { count = 0 }` works until a caller creates a session
  `count`, so a callee breaks on its caller's namespace, detectable only at
  runtime. Requiring a function-local binding to be `_`-prefixed — and making a
  `_` name *always* current-scope, never global or exported — makes collision
  impossible by construction. The namespaces are disjoint, so the check is static
  and no caller can break a callee. Inside a function every assignment is then
  `_local`, `global`, or `export`; there is no fourth case. (`_name` already
  parses: `valid_name` (`parser.rs:5804`) accepts `_private` and rejects bare
  `_`, which stays the discard.)
- ***Parameters carry the underscore too — no exemption.*** A parameter is a
  function-local binding like any other, so it is spelled `_name` in the signature
  and read as `$_name` in the body. This is the stricter of the two options on the
  table, taken deliberately: it makes the rule **exceptionless** — every binding
  visible inside a function body is `_`-prefixed, with no "…unless it is in the
  signature" clause for a reader to carry — and it costs nothing structural,
  because parameters are known when the body is parsed either way.

  Two things fall out that are worth naming, because both *remove* rules rather
  than adding them. **The declaration-site exemption disappears**: there is no
  longer a class of plain-named inner bindings, so a plain name inside a body
  always means an outer one, mechanically, with no signature to consult. And
  **the path-var blacklist becomes unnecessary** — `func f(_PATH)` cannot shadow
  the environment's `PATH`, because `$_PATH` and `$PATH` are simply different
  names, so the one genuinely misleading parameter shadow stops being reachable.
- ***It reaches every binding form — but only inside a function.*** Once
  parameters carry the prefix on the grounds that they are locals, so must every
  other construct that introduces a local name: **lambda parameters**,
  **destructuring**, and **match-arm bindings**. Nothing distinguishes those from
  a parameter — each binds a name visible in a body, each could otherwise shadow
  an outer one — so an exemption for any of them would reopen exactly the hole
  the exceptionless reading closes.

  The scope qualifier is what bounds this, and it bounds it sharply. `_` marks a
  **function**-local, and at top level the session *is* the current scope, so a
  top-level binding is plain-named and unaffected. The same destructure is
  therefore spelled two ways, and the difference is the only thing the reader has
  to track:

  ```
  [user pass uid] = $line:split(":")        # top level — session scope, plain

  func parse(_line) {
    [_user _pass _uid] = $_line:split(":")  # inside a function — local, prefixed
  }
  ```

  So the tax lands on **function bodies that destructure or match**, not on
  interactive or top-level code — which is the bulk of what a shell user types,
  and the reason the [Destructuring](#destructuring) and
  [Matching](#matching-match) examples below stand correct as written rather than
  needing a sweep. It is still the piece to judge first in use: a helper that
  pulls apart its arguments is exactly the code that gains the most underscores,
  and it is also the code mesh most wants people writing instead of `$1`-style
  plumbing.
- ***The call syntax, and the question it turns on.*** The underscore is a
  **scope** marker, and a flag parameter's name is also **public interface** —
  `func deploy(--_region = us-west)` has to decide what a caller types. This
  narrows to *flags* only: a positional is passed by position, so `deploy prod`
  never names `_env` at all, and a rest parameter likewise. Hyphens are already
  legal in names (`valid_name`, `parser.rs:5804`, accepts `MY-VAR`), so
  `--dry-run` can bind `$_dry-run` with no name mangling — the leading underscore
  is the whole question.

  The question underneath is **whether `_` is part of the identifier or a marker
  on it**, and the two readings are not interchangeable:

  - ***Part of the identifier.*** `_region` and `region` are unrelated names. This
    is what makes no-shadow **free** — a local can never collide with an outer
    binding because the namespaces are disjoint, so the check is static and no
    caller can break a callee. It is the reading the bullets above assume.
  - ***A marker on the identifier*** (punctuation, like a sigil). Then `_region`
    *is* `region`, marked local, and the call site strips it for the same reason
    a sigil is not part of a name. Clean at the call site — but it **gives back
    the disjointness**, because `_count = 0` in a function is now the name
    `count`, which a session `count` collides with. No-shadow becomes a runtime
    check again, and a caller can break a callee again.

  **Taken: `_` is part of the identifier** — the strong form, chosen because it is
  the reading that makes the ban free rather than checked.

  That makes "strip the prefix at the call site" circular *if* justified the
  natural way, since the marker reading is exactly what would undo the property
  just bought. So stripping is defined instead as a **derivation rule, not an
  identity claim**: `_region` remains its own identifier, and the flag it
  *exposes* is mechanically its name minus the leading `_`. The derivation is
  total and injective — only a parameter named `_region` can expose `--region` —
  so disjointness survives untouched and callers type `deploy --region=eu-west`.
  The honest cost is two spellings for one parameter, related by a rule the reader
  has to know.

  The property that makes this worth the rule: **call sites do not change at all.**
  `deploy prod --force web1 web2` is byte-identical before and after; only
  signatures and bodies gain underscores. A reversal is therefore confined to
  declarations, which is what keeps the whole block cheap to undo.

  Rejected, and why: **keep it** (`deploy --_region=eu-west`) is one spelling with
  no rule, but leaks a scope marker into every call site of a public interface, and
  would churn every call site in the language. **Name the external form
  explicitly** is honest but adds ceremony to the common case where the two match.
  **Exempt parameters** is the smallest change and the one most languages make —
  it costs only the exceptionless "every binding in a body is `_`-prefixed"
  reading, and it is the fallback if the strong form does not survive use.

*Costs and loose ends, in the order they are likely to bite: `_i` / `_line` /
`_out` throughout every function body is a visible tax and a hard break from
every other shell — this is the piece most likely to be reversed once it has been
lived with, and the reason the whole block is provisional. A read that falls out
to the environment turns what is today an unbound-variable error into an
inherited ambient value — it can only fire where the name is bound nowhere, so it
cannot change a working program's meaning, but it is a real dent in fail-loud.
`$env.PATH` is a **list**, so even with all of the above `echo $PATH` errors
("list value needs `...`") unless a path-type value `:`-joins when a byte context
demands one, which is arguably just the export serialization above applied one
step earlier. Removing an environment entry still has no spelling, so
`unset PATH` would drop a binding rather than the entry.*

**Shadowing, bounded — what the ban actually bans.** The **one-command prefix**
(`NAME=value cmd`) is [settled](#variables-and-assignment), and its whole purpose
is to make a name mean something else for one command: `TZ=UTC date`,
`LANG=C sort`, `PATH=/opt/bin tool` are all overrides of an entry that already
exists. That is *shadowing*, and it is the point — so a rule announced as "no
shadowing" needs saying more precisely, because taken literally it would forbid
the most useful line in the shell.

What the ban forbids is an **unbounded, unmarked second binding** of a name. It
does not forbid rebinding a name for a **bounded, explicitly marked region**,
which is what the prefix and `with NAME=value { … }` are for. Three properties
separate the two, and the prefix has all three:

- **One value, not a second binding.** A bounded override *replaces* what the name
  already means rather than adding a candidate beside it, so `$NAME` still has
  exactly one answer and the precedence question the ban exists to close stays
  closed. Getting this right takes care, and the next paragraph spells it out —
  "the prefix writes the environment" is not sufficient on its own.
- **Bounded.** The value is restored on the way out, however the command or block
  leaves — a stage's prefix does not outlive the stage.
- **Marked.** You wrote the prefix, or the `with`. Nothing is implicit.

So the ladder's rule is better stated as: **a name has one binding at a time, and
any name may be rebound for a bounded, explicitly marked region.** That is not a
restriction on shadowing's *use*; it is a requirement that shadowing be scoped and
visible. mesh bans the accident, not the capability.

**A prefix overrides the name, not a rung.** "The prefix writes the environment"
is the right rule for what a *child* inherits and the wrong rule for what the
override *means*, because a name need not be on the environment rung when the
prefix runs. Take `FOO` uninherited, so a plain `FOO=bar` is permitted and binds
a session name; then `FOO=baz f`. If the prefix only wrote an environment entry,
both rungs would hold `FOO`, and a **mesh function** `f` reading `$FOO` would
resolve outward to the session binding and see `bar` — while an **external** `f`
would see `baz` from its environment. Same line, two meanings, chosen by
something the writer cannot see at the call site. That is the precedence question
back again, and it would make bounded overrides unreliable in exactly the case
they are reached for.

So a prefix (and `with`) puts the name on **the environment rung and only there**
for the region, masking rather than duplicating, and restores on the way out:

- on the **environment** rung — replace the entry for the region;
- on the **session** rung — **mask** the shell binding and install the
  environment entry, so there is still exactly one live binding;
- **nowhere** — install the environment entry for the region, and drop it after.

Masking rather than keeping a matched pair is the whole point. A pair would be a
*second binding* of the name — the very thing the "one value" property above says
a bounded override must not create — and two copies can diverge the moment
anything writes one of them. One live binding cannot.

The **local** rung is absent from that list because a prefix cannot reach it. A
prefix names something a *child* will inherit, and a `_` name is by definition
never exported, so `_FOO=baz g` is not an unspecified case but a **syntax error**
— refused where it is written, with the diagnostic saying that `_` names do not
cross into the environment. There is nothing to define for the local rung; the
existing rule already forecloses it. (This is one more place the disjoint-name
reading pays: the refusal is decidable from the name's spelling alone, with no
lookup.)

The invariant is one sentence, and it is what the "one value" property above
promises: **for the region, `$NAME` reads `value` and a child sees
`NAME=value`** — whichever rung the name was on, and whether the command is a
function or an external. The alternative, rejecting the prefix when a shell
binding exists, was considered and dropped: it fails a common line for a reason
invisible at the call site (`FOO` happens to also be a session variable), and it
buys nothing the masking does not.

The invariant underneath all of this is worth stating on its own, because it is
what the ban *is* once the bounded case is admitted: **a name is on at most one
rung at a time** — never two; zero is just an unbound name. No-shadow is that
sentence; masking preserves it through a
region; and every question about writes during a region answers itself from it.

**Writes made while the region is live** therefore need no new rule — only the
existing ban, applied to the state the region has established:

- **`export FOO = qux`** writes the one live binding, so `$FOO` reads `qux` and
  the next child sees `qux`. Still one rung, and the region restores what it
  saved on the way out.
- **`global FOO = qux`** may *update* a masked session binding, but may not
  *create* one — because during the region `FOO` is live on the environment rung,
  and creating a session binding there is exactly the shadow the ban forbids. The
  test is whether a session binding **exists** under the enclosing masks, not
  which region put the mask on: a nested `with` must not change whether an
  explicit session write is legal, and helpers that add an override would
  otherwise silently break their callers' `global`. So
  `FOO=bar; with FOO=one { with FOO=two { global FOO = qux } }` updates the
  session `FOO`, exactly as the same statement one level out would.
- **`global unset FOO`** reaches the same masked binding and **removes** it, and
  the removal survives the region — an explicit deletion is not something to
  silently undo. Afterwards `FOO` is on no rung at all, which the invariant is
  fine with: *at most* one, never two.
- **plain `FOO = qux`** (and bare `FOO += qux`) takes the *same* create-vs-update
  test, not a blanket refusal. At top level the session **is** the current scope,
  so a plain assignment names the rung `global` names, and the two must agree: it
  **updates** a masked session binding, and is **refused** only where there is
  none to update and it would therefore create a binding beside the live
  environment entry. Anything stricter would make adding a `with` silently change
  what an ordinary top-level assignment means, which is the writes-stay-put rule
  broken from the other side. (Inside a function a plain assignment names the
  *local* rung and must be `_`-prefixed, so a bare `FOO = qux` there is already a
  different error and never reaches this test.)

The split is create-vs-update throughout, in other words, and it belongs to the
*write*, not to the spelling that names the rung: `global`, a top-level plain
assignment, and the bare append are one rule seen three ways. Updating a masked
binding is always fine — it cannot make a second binding, and the region restores
to one rung by construction. Creating one beside a live environment entry is
always refused, whatever spelling asks for it.

**What a region restores is what the region itself installed** — its environment
entry, and the mask — not the masked binding's value. That is what lets `global`
and `global unset` mean something durable from inside a region, and it fixes the
restoration order for nesting: masks lift innermost-first, each region putting
back the environment entry it saved, and no region rewrites a session binding it
did not write.

Both halves of the `global` rule matter. Permitting the *creating* form would
leave `with FOO=baz { global FOO = qux }` — with `FOO` originally only in the
environment — populating both rungs once the original entry is restored, so
`$FOO` would read `qux` while children inherited the old value: a collision
manufactured by the region and outliving it. Forbidding the *updating* form would
break `global`'s promise for no gain, since that case restores to one rung by
construction.

Had the region kept a matched pair instead of masking, none of this would follow:
`export FOO = qux` would have left `$FOO` reading `baz` while children saw `qux`,
breaking the invariant mid-region with no rule able to say which copy was right.

**`+=` reads its own target.** An append reads and writes the same place, so the
scope qualifier that picks the target picks the source too — there is no separate
rule for where the old value comes from:

- **the prefix `FOO+=baz cmd`** targets the live binding, which is also what
  `$FOO` reads. With `FOO` uninherited and a session `FOO=bar` in place it appends
  to `bar`, so `$FOO` reads `barbaz` and the child sees `barbaz`. This is a
  **change** from the environment-only reading, worth stating because the two
  disagree: that reading would have appended to an absent entry and passed `baz`.
  Appending to the entry while `$FOO` read something else would break the
  one-value promise mid-region.
- **a bare `FOO += baz`** is *not* the same thing, and must not be read as the
  prefix without its command. It is an ordinary assignment — neither bounded nor
  marked — so it takes the ordinary create-vs-update test, exactly as plain
  `FOO = baz` does: it appends to a shell binding where one exists (including one
  a region has masked), and is **refused** where `FOO` is on the environment rung
  with no binding to extend, pointing at `export FOO += baz`. Reading it as the
  prefix instead would let a bare append mutate a live environment entry with no
  `export` and no region — the unbounded, unmarked write this whole section
  exists to forbid.
- **`global FOO += qux`** targets the session rung, so it reads the session
  binding — *through* any masks. In `FOO=bar; with FOO=baz { global FOO += qux }`
  that is `bar`, giving `barqux` on the session rung, which survives the region
  like any other `global` write. It is **not** `bazqux`: `baz` is the live
  environment value, and `global` did not name the environment.
- **`export FOO += qux`** targets the environment rung. Where an entry exists it
  reads and extends it — inside a region, the region's own value. Where the name
  is instead a **shell binding**, `export` **migrates** it: the name moves to the
  environment rung rather than gaining an entry beside the binding, so
  `FOO=bar; export FOO += qux` leaves `FOO=barqux` in the environment and no
  session binding. Migration is what the one-rung invariant requires of `export`
  generally, not a rule invented for the appending form — it is also what makes
  the documented copy-it-in idiom `export NAME = $NAME` (`parser.rs:5530`) land on
  one rung instead of two.

"What `$NAME` reads" is therefore the right source only for the prefix, where the
target *is* the live binding. A qualifier moves both ends together.

This also settles the collision the prefix rule flags as deliberate — that
`NAME=value cmd` writes the environment while a plain `FOO=bar` binds a shell
name. The two are different constructs, not two spellings of one, and the ladder
makes the distinction enforceable: where `FOO` is already an environment entry, a
plain `FOO=bar` is **refused**, and the diagnostic names all three things the
writer might have meant — `export FOO = bar` to change it for real, `FOO=bar cmd`
to override it for one command, `with FOO=bar { … }` to override it for a block.
Where no entry exists, a plain `FOO=bar` binds a shell name exactly as before.
The prefix's **scoping and stage-binding** are untouched — it still binds to a
stage, so `FOO=1 a | FOO=2 b` gives each side its own and `FOO=1 a && b` leaves
`b` alone. Its **`+=`** is the one thing this does change, as set out above.

### Quoting and escaping

mesh has a few string forms — a bare word, three quote kinds (`"…"`, `'…'`, `r'…'`),
and a heredoc — plus the backslash escape, chosen so the common cases need no
ceremony and the rules stay few.

**Bare words are literal** (`x = foo` binds `"foo"`), and a single **backslash
escapes the next character** so one metacharacter can go literal without reaching
for quotes: `cp a\ b dst` (a literal space keeps it one argument), `\*` (a literal
star, not a glob), `\$`, `\#`, `\!`, `\-`. A `\` at end of line is **line
continuation**.

**Single quotes `'…'` don't interpolate but do escape** — they are `"…"` minus `$`
interpolation (Python's `str`). The escape set is the double-quote set with the quote
swapped: `\n \t \r \e \a \b \f \v \\ \'` and `\u{…}`; `$` is always literal (no
`\$` needed), and an **unknown escape is an error** (`'\d'` is a mistake, not a
literal backslash-d). `\a` is `BEL`, there because it terminates the OSC sequence a
title-setting prompt carries — `"\e]0;mesh\a mesh$ "` — which is the form such a
prompt is copied in as from another shell; `\b \f \v` come with it so the set has no
arbitrary hole. **`\0` is deliberately not one of them**: a NUL cannot cross `execve`
or the environment, both of which mesh refuses it at, so the escape would only build
values that fail later. Because an unknown escape is an error rather than a literal,
another one can always be added without changing what an existing script means.
So `'can\'t'` → `can't`, `'a\nb'` is two lines, and no variable expands.

**Raw strings `r'…'` / `r"…"` take no escapes at all** — every byte is literal and
the delimiter is the only special character — so they are the home for regex source
and paths: `r'\d+\.txt'` is exactly those bytes and `r'C:\x'` is a Windows path. Pick
the delimiter that avoids your content's quote — `r"can't \d+"` holds an apostrophe
freely — and a string needing **both** quote kinds uses the quoted-delimiter
[heredoc](#redirection).

**Double quotes `"…"` interpolate and escape.** `$name` / `${…}`
[interpolate](#variables-and-assignment), and a **modern C-style escape set**
applies — `\n \t \r \e \a \b \f \v \\ \" \$` and `\u{1F600}` for Unicode — so
`"a\nb"` is two lines and `"\$5"` is a literal dollar. This is a deliberate break from bash (where
`"\n"` is a backslash-n and you reach for `$'\n'`): mesh needs no `$'…'` form
because double quotes already interpret escapes.

**Adjacent pieces concatenate** into one word — `"$dir"/'sub'/$file` fuses into a
single path and `--flag='some value'` is one argument — so literals and expansions
compose without a `+`.

*(decided: the raw form that can itself hold *both* quote kinds — for the rare
string embedding `'` and `"` with no escaping — is a **quoted-delimiter heredoc**
(`<< 'END' … END`; the bare `<< END` interpolates, see [Redirection](#redirection)),
chosen over an `r#"…"#` delimiter; see [`TODO.md`](TODO.md). Its *value-producing*
spelling is still unspecified — today's heredoc is command-redirection only —
tracked in TODO.md.)*

**Regex literals stay `/…/`; absolute paths are disambiguated by word shape**
*(decided direction — the raw-string alternative is recorded under "Alternatives
considered" below)*. mesh keeps the familiar `/…/` regex literal and resolves the one
real problem it creates — an absolute path or glob also begins with `/` — with a
**word-shape rule**, replacing the blunt "any leading slash in a match slot is a
regex."

In a **match slot** (the `~` / `!~` RHS, a `:match` argument, the replace family's
`OLD` argument — `:replaceall` / `:replacestart` / `:replaceend` — a `match` arm), a word
beginning with `/` is a **regex** *only* when its **base** — the word stripped of any
trailing `:` modifier chain — is a clean `/BODY/`: the closing `/` is the final
character of the base and `BODY` has no unescaped interior `/`. So `/\d+/:i` is a
regex (base `/\d+/`, then `:i`). Every other leading-`/` word is a **path or glob**:

*(The chain is stripped by **shape**, not by vocabulary — this rule once said
"recognized flag modifiers", which [declaring a modifier](#modifiers) makes impossible
to know while parsing, and which the parser never did anyway: `$p ~ /tmp/:foo` reports
about `:foo` today rather than reading as the path `/tmp/:foo`. No row of the table
moves, because the base test — clean `/BODY/`, no unescaped interior `/` — is
unchanged, and it is the base test that separates a path from a pattern.)*

| RHS word | reads as | why |
| --- | --- | --- |
| `/error/`, `/^\d+$/`, `/a\|b/` | **regex** | clean `/BODY/`, no interior `/` |
| `/a\/b/` | **regex** `a/b` | interior slash escaped |
| `/usr/bin` | **path** | interior `/` before the end |
| `/usr/*/bin` | **glob** (absolute) | interior `/` ⇒ path shape |
| `/tmp/*` | **glob** | the closing-looking `/` isn't final |
| `/tmp` | **path** | no closing `/` |
| `/*.txt` | **glob** at root | leading `/`, no closing `/` |

The win over the old rule: **absolute globs and paths need no wrapper** —
`$p ~ /tmp/*` and `$p ~ /usr/bin` just work, where before *every* absolute pattern
had to be wrapped.

**The one residual.** A single segment with a trailing slash still reads as a regex:
`$p ~ /tmp/` is the regex `tmp`, not the path. Three teachable outs — drop the slash
(`$p ~ /tmp`, the path, and the more usual spelling anyway), add structure
(`$p ~ /tmp/*`), or force it (`fnmatch($p, "/tmp/")` / `== "/tmp/"`). That is the entire
residual, versus the old rule's blanket wrapper requirement.

**Recognized only in match slots.** Everywhere else a `/…/` word stays a path or
string — `cd /tmp/`, `grep /usr/bin`, `p = /etc/hosts`. In particular an
**assignment** `x = /…/` binds the **path string**, not a regex: extending
regex-literal recognition into general value position was considered and **not
chased** — it would split `x = /tmp/` from `cd /tmp/` inconsistently, and buys only
sugar over `re("…")`. To bind a **regex value** to a name, use the constructor with a
raw string, `pat = re(r'\d+')` (a plain `'\d+'` is a Model B error — `\d` is an
unknown escape).

**Settled independent of the literal syntax:** regex flags are `:` modifiers on the
regex value — `/\d+/:i`, `:m`, `:s`, `:x` (see the note by `re()`; parse-affecting
flags like `:x` are construction-time).

**Alternatives considered (explored, not taken).** Sketched while hunting for a rule
with *zero* edge cases; the word-shape rule above accepts one narrow residual
instead. Kept as the record and as possible future sugar:

- **`rx'…'` as a regex literal replacing `/…/`.** The Python-shaped string trio —
  `"…"`, `'…'` (non-interpolating but escaped), `r'…'` / `r"…"` (raw) — **was
  adopted** (see [Quoting](#quoting-and-escaping) above); what was *not* taken is
  spelling the **regex literal** as `rx'…'` (raw body → regex value,
  `rx'\d+' ≡ re(r'\d+')`) with `/` then always a path/glob. `/…/` is kept instead.
  Still, `rx'…'` remains the clean way to write a regex *value* in a non-match
  position (`pat = rx'\d+'`: no `$`-anchor issue, no path ambiguity), so it may
  return as sugar for `re(r'…')`.
- **`~` / `match` RHS coercion** *(decided: no coercion, for now)*. A plain string on
  the RHS stays an **error**; a regex must be explicit (`/…/` or `re($pat)`) — the
  no-silent-coercion rule (below) holds. The two coercion flavors were weighed and
  neither adopted: *string → regex* ("like `match`": terse, but inverts the universal
  "quotes mean literal" and risks `$x ~ 'a.b'` matching `axb`), and
  *quotes-mean-literal* (`'…'` inert, regex only via `re` / `/…/`). Revisitable.
- **Removing the two single-quote escapes.** The thread's original question — the old
  design made `'…'` raw with only `\'` / `\\`, and asked whether to drop those to make
  it *fully* raw. Overtaken by adopting Model B: `'…'` is now the *escaped*
  non-interpolating string (so `\'` is simply part of a full escape set), and rawness
  lives in `r'…'`. No longer open.

#### Bare words and quoted values — decided

**A bare word is a command; a quoted word is a string literal.** One sentence, and it
holds in every position a statement is read — including the tail of a block, which is
where it used to stop holding.

```mesh
x = if true { pwd }        # runs pwd, yields its output
x = if true { "pwd" }      # the string "pwd"
"foo"                      # a string statement — nothing is run
```

Three bare spellings escape and are **literals everywhere**, in statement position as
much as in a block tail: an **integer literal** and **`true`** / **`false`**.

```mesh
func answer() { 42 }       # the integer
func no() { false }        # the boolean — not /usr/bin/false's status
if true { … }              # a literal condition; nothing is forked
```

For a numeral the reason is that it could never name a command. `true` and `false` are
different — a program of each name exists on every system — so this is a genuine
choice rather than a fact about the words. It was taken because `if true` and
`while true` are written far more often than `true` is invoked for its exit status,
and reading `true` as a boolean surprises nobody; the shell also stops forking
`/usr/bin/true` to learn what it already knows. The program stays reachable by any
spelling that is not a lone bare word — `./true`, `command -- true` — exactly as `./42`
still runs a file called that.

Getting here took two steps. The bare/quoted rule below settled the *block tail*, which
left `true`/`false` split by position: `x = if true { false }` was the boolean while
`func no() { false }` ran the program and resulted in `1`, because
`parser::outranks_a_command` excused only integers and quoted words while the block-tail
rule also excused booleans. Both are falsy, so no condition could tell them apart and
the difference showed only in the *value*. The two carve-outs now match.

What this replaced was a **single-bare-word block-tail coercion**: a one-word block was
read as a scalar literal, so `{ pwd }` was the string `"pwd"` while `{ pwd . }` ran.
Three footguns came out of that, and the worst was silent — `x = if true { pwd }` bound
`"pwd"` with no error to show for it. Adding an argument flipped a literal into an
execution, and the same block text meant different things in statement and expression
position. Quoting was inert in the tail (`{ pwd }` and `{ "pwd" }` agreed), so there was
no reliable way to *ask* for either reading.

The cost is one spelling: a lone quoted word no longer runs, so a program whose path
needs quoting is reached through **`command -- "/opt/my program"`**. Quoting a command
name that *takes arguments* is unaffected — `"if" x` still resolves func → external, as
[Command resolution](#command-resolution-and-help) specifies, because that is a
multi-word command rather than a lone scalar.

### Arrays (lists)

The list is mesh's core value — a [split capture](#command-substitution) produces
one and value modifiers map over it. This section pins down the *literal*,
*indexing*, and *slicing* surface.

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
| `$xs:get(i, default)` | returns `default` | total, never errors — the safe accessor when absence is expected |

So `$xs[99]` on a 4-element list is an error that names the index, but
`$xs:get(99, "-")` yields `"-"`, and `$xs[1..99]` just runs to the end. Fail loud
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

*(TODO: with `:append` / `:extend` below saying by **name** what this says by
type, `xs += $ys` and `$xs:extend($ys)` are one operation with two spellings —
and they disagree about what a list on the right means. Open: drop `:extend`,
make `+=` append-only so no type dispatch survives anywhere, make it extend-only,
or keep both and accept the overlap. Whichever wins, the table above and this
paragraph are what change with it.)*

**`:prepend(e)` / `:append(e)` / `:extend(ys)`** are the **pure** counterparts of
`+=` — the list builds above written as a chain. Each returns a *new* list rather
than writing one, so they compose where a statement cannot:

```
$env.PATH = $env.PATH:prepend(/opt/bin):dedup
xs = $xs:append($ys)      # [...$xs $ys]   — one element, $ys nested whole
xs = $xs:extend($ys)      # [...$xs ...$ys] — $ys's own elements
```

**None of them reads its argument by type.** `:append` adds exactly one element
whatever it is, and `:extend` adds a list's elements; which one you meant is in
the **name**, decided at the call site rather than inferred from the value's
shape. That is the point of having three: `+=` dispatches on the right-hand type
because a statement has only one spelling to work with, and that dispatch is
deliberately **the one place the shell flattens by type rather than by an
explicit `...`** — a second one here would cost the rule its meaning. So
`:extend` requires a list and says so, naming `:append` when it is handed
something else.

`:prepend` and `:append` are named for the end they add to rather than `:add`,
since a list has two of them and a name that does not say which is a coin toss at
the call site. `:extend` has no front-loading twin: `[...$ys ...$xs]` is the
spelling for that, and a name for it would be worse than the spread it replaces.
All three are **lists only** — a map has no front or back to add to (its `+=` is
a *merge*, a different operation under a name that would not say so), and a
string has `+=` and interpolation already. The element is **stored rather than
read as text**, so a styled value keeps its attributes in the list, as it does in
the `[...$xs $e]` build these are the chain spelling of.

*(TODO: a **`:remove(e)`** to match, the pure counterpart of the proposed `-=` —
`$env.PATH:remove(/usr/games):dedup` — waiting on the same open questions the
`-=` note below raises: first match or every match, and what a list argument
means.)*

*(TODO: consider a symmetric **`-=`** that removes an element — `$hosts -= web3`
deleting the matching element, mirroring how `+=` appends one. Open: remove the
first match or every match; equality by value; whether the right-hand type
dispatches like `+=` (a list RHS removing each of its elements → set-difference,
a scalar removing one), and what a map LHS means (`-= key` dropping that entry,
overlapping with `unset $m.key`). Note this is a value-level remove-by-content,
distinct from `unset $xs[i]`, which deletes by index.)*

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

Brackets stay for dynamic or non-identifier keys (`$m[$k]`, `$m["a b"]`). Dot
access has the same meaning inside and outside a double-quoted string, so
`"$m.key"` reads the map member. Use braces when a dot starts literal text:
`"${file}.txt"`.

| Form | Result | Meaning |
| --- | --- | --- |
| `$m:keys` | list | keys (insertion order preserved) |
| `$m:values` | list | values |
| `$m:len` | int | entry count (same word as lists) |
| `$m:has(KEY)` | bool | membership — the decided spelling |
| `$m:get(KEY, default)` | value | total lookup — `default` when absent |

**Membership is `:has`.** The terser `?` postfix (`$m[key]?`) was considered and
dropped — it fights the "words, not punctuation" grain the modifiers are built
on, and spends a `?` symbol that optional/error-handling will likely want. *(to
do: consider an infix `in` operator — `if https in $ports { … }` — as an
additional, English-reading spelling alongside `:has`; familiar from Python, but
it adds a second way to phrase the same test, so weigh it before adding.)*

**Missing keys** follow the same strict/total split as list access, since mesh
has no null: `$m[absent]` is an **error** (a bad key is usually a typo in
config, and should fail loud, not silently yield `""`), while `$m:get(key,
default)` is the total form that returns `default` when the key is absent, and
`if $m:has(key) { … }` is the guard. So a dynamic lookup that may legitimately
miss is written `$m:get($name, unknown)`, never a bare `$m[$name]`.

Insertion order is **preserved** (like Python dict / a `Vec<(K,V)>` behind the
scenes) so `for k in $m:keys` is deterministic — important for an rc file that
builds, say, an ordered alias table.

*(open — **`[key=value]` as the literal spelling**, in place of `key: value`. The
draw is that it would retire the one collision this section has to legislate
around: the pair `:` versus the modifier `:`, which today costs a quoting rule
(`["http:" 80]`) and a lookahead to keep `[:]` apart from `[:stem]`. `=` has no
such clash. Three things have to land before it is a candidate:*

- ***The `env FOO=1` case is narrower than it first appears.*** The pair syntax
  is already **bracket-scoped** — a bare `key: value` in space-separated command
  position is not map grammar (see
  [Calling for a value](#calling-for-a-value-and-lambdas)) — so `env FOO=1 cmd`
  never enters the map rule under either spelling. What changes is only
  `[CFLAGS=-O2]`, which would flip from a one-element list of an argv token to a
  one-entry map.
- ***Map spread would want a canonical argv encoding after all,*** which the
  bullet below currently refuses. `--key=value` is the wanted rendering; note it
  is *not* the same as the literal's own spelling, so the tidy "the literal is
  the wire format" argument (the `KEY=bytes` environment table being the one map
  every process already carries) does **not** close the case, and picking
  `--k=v` over `k=v` is back to a guess between plausible encodings.
- ***Whether named options follow.*** A value call's options **are** a map
  literal, so `deploy(prod, region: us-west)` would become `region=us-west` —
  which then collides with the dashed `--region=us-west` spelling *and* with a
  literal `f(CC=gcc)` positional. Deciding maps without deciding options would
  split the two shapes that are deliberately one.)*

### Spread / flattening

`...` is the one operator that moves between "a list" and "several arguments,"
in both directions:

- **At a call site**, `...$xs` **explodes** a **list** into separate positional
  arguments — or a **map** into named options, each `key: value` pair binding the `key`
  option (the two shapes a call takes; see
  [Calling for a value](#calling-for-a-value-and-lambdas)). A **list** spread reaches an
  **external** command as plain argv tokens, but a **map** spread binds *named options*
  and so needs a signature — spreading a map to an external is an **error** (a map has
  no canonical argv encoding — mesh will not guess `--k=v` vs `--k v` vs `k=v`), the
  same bytes-boundary rule that rejects an un-spread list at the process edge.
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
  spread it with ...$flags or join it with $flags:join"*). The two
  explicit outs are `...$flags` (one argv entry per element) and
  `$flags:join(SEP)` (one byte-string). Note that this is a **stricter** rule
  than the bytes boundary alone forces — a flat list *does* have a canonical argv
  form — so it is argued on its own terms under "Implicit flattening at argv"
  below rather than inherited from the no-word-splitting promise.

**Two boundaries, not one.** It is tempting to justify all of this with "mesh
will not guess a separator," but that reason only holds at *one* of the two
places a value turns into bytes, and conflating them muddies both:

| Boundary | Shape it lands in | Does a list need a *guess*? |
| --- | --- | --- |
| **argv** (`cmd $x`) | a **vector** of byte-strings | **No.** argv is already a sequence of independent strings, so a list maps onto it losslessly — one element per slot, no separator anywhere |
| **interpolation** (`"$x"`) | **one** byte-string | **Yes.** Collapsing many elements into one string requires picking a separator (space? tab? `,`?), and there is no right answer |

So the "no canonical separator" argument is an **interpolation** argument. It is
the whole story for `"$xs"`, and it is *not* why a bare `$xs` is refused at
argv — a list has a perfectly canonical argv form. The argv rule rests on two
different legs, spelled out under "Implicit flattening at argv" at the end of
this section: **nesting** (a list *element* that is itself a list has no argv
form) and **decidability** (whether a line works should be readable from the
line, not dependent on what a variable happens to hold at run time). Both are
good reasons; neither is about separators.

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
| **list** | **error** — spread or `:join` | *not* a separator problem here (see above): a **nested** element has no argv form, and implicitly flattening the flat case would make a line's meaning depend on run-time contents |
| **map** | **error** — render it explicitly | here the guess is real — `--k=v` vs `--k v` vs `k=v` are all plausible and mesh will not pick |
| Duration | its canonical spelling (`3s`, `1m30s`) | it has a canonical form |
| `Status` (under the [status decision](#open-questions)) | decimal digits — `cmd status(5)` passes `5` | it wraps an integer, so decimal is canonical exactly as for an int; the type governs projection and dispatch, not the byte form |
| **Instant / regex / stream handle** | **error** — no canonical byte form | an Instant needs `:iso`/`:epoch`/`:format`; a regex (it carries flags) and a stream handle have no byte form at all |

String interpolation uses this same rendering table, and reaches the same verdict
on every row — but for a **list** it gets there by the separator argument above
rather than by the argv one. Interpolating a list, map, Instant, regex, stream
handle, embedded-NUL string, or any future value without a canonical byte form is
a loud error; `${…}` is not an alternate serialization mechanism. The practical
consequence of the split is that `...` and `:join` are **not** interchangeable
fixes: `cmd ...$xs` passes `$xs:len` separate arguments and needs no separator,
while `$xs:join(",")` makes one string and must name one.

An embedded NUL (which a `$(cmd):raw` capture can hold) is the one place a
*string* fails to cross — argv, like the environment, is NUL-terminated, so it
is a hard error at both boundaries, never a silent truncation.

So `echo $xs:len` prints `4` and `echo $found` prints `true`, but `echo $xs`
(a list) and `echo $m` (a map) are errors that name the fix. The dividing line
is "is there one obviously-right rendering?" — ints and bools have one, and a
map's shape does not. A **list** is the row where the answer differs by
boundary, per the split above: no for interpolation, yes for argv, where it is
refused on the separate grounds below.

**Implicit flattening at argv.**
*(open — the rule above is **not** in question for maps or for interpolation;
what is open is whether a **flat list** should reach argv without a written
`...`.)*

**What is settled.** Implicit flattening here is **not** the bash footgun, and
the goals' "no implicit word splitting, ever" should not be read as settling it
by itself. bash's bug is *string → many words*: arity is inferred from the
**bytes** of a value at run time, on `IFS`, which is why `rm $file` breaks on a
space. A mesh list carries its arity in its **type** — nothing is inferred from
whitespace, and `$file` (a string) stays one argv entry under every option
below. "Does a **string** split?" is closed — no, permanently. "Does a **list**
flatten?" is a separate question that the first one does not answer.

**The live costs.** The `...` requirement is not free. A
[capture](#command-substitution) is **one string**, so the plain `cd $(…)` case
pays nothing and the cost falls only on captures that are *deliberately* split:
`wc -l ...$(ls):lines`, `grep foo ...$(find . -name '*.rs'):lines`. That is the
common list-producing idiom, and it pays a token per use — but the line already
says it wants a list, which is the part that makes the spread read as redundant.

The options, cheapest-to-change first:

| Option | `cmd $xs` where `xs = [a b]` | Nested `[a [b c]]` | Trade |
| --- | --- | --- | --- |
| **A. Status quo** — always `...` | **error**, names the fix | error either way | Rule is **syntactic**: readable from the line, never data-dependent. Costs a token on every capture |
| **B. Flatten flat, error on nested** | `cmd a b` | **error** | Terse where it matters; puts the loud failure exactly where the real ambiguity is. But whether a line works now depends on **run-time contents** — the regression that matters |
| **C. Deep flatten** | `cmd a b` | `cmd a b c` | Never errors, never surprises with an error. Silently erases the distinction `+=` is built to preserve (`xs += [$ys]` appends whole vs `xs += $ys` extends), so a value's argv arity stops being predictable from its structure. This is **Perl's** auto-flattening list wart, which is why Perl needs `\@a` refs to nest at all |
| **D. Flatten only a split `$( )` capture** | `cmd a b` | n/a — split captures are flat | Buys back the whole ergonomic cost above with no nesting question, since a split always yields a flat list of strings. But it makes the rule depend on a value's **provenance**, not its type — `xs = $(ls):lines` then `cmd $xs` would have to decide whether the property survives the binding, and "it does not" is a nasty wrinkle |

**Leaning A or D.** B's data-dependence is a real regression over a rule you can
check by reading, and C is a known wart in the one language that shipped it. D is
the interesting one precisely because it is *not* about lists at all — it is
about whether a split `$(…)`-in-argument-position should keep bash's shape while
keeping mesh's safety, and it needs the binding question answered before it is a
real candidate. The ergonomic cost D buys back is confined to captures the author
already chose to split, which is what keeps A cheap.

*(TODO — **a `:flat` modifier**, which is wanted under **every** option above and
is a gap today: [`:join`](#modifiers) already promises "there is no implicit deep
flattening — spell it out" and then supplies no spelling. Nesting arises in mesh
more readily than in most languages, because value modifiers **auto-map over a
list**: a split applied to a list of lines (`$lines:words`) or any
`:map` whose lambda returns a list yields a list of lists, and there is currently
no way back.*

- ***Depth default — one level, not deep.*** The split across languages is even —
  one-level in **JavaScript** (`flat()`, depth defaulting to `1`), **Rust**
  (`flatten`), **Haskell** (`concat`), **Clojure** (`mapcat`), and **nushell**
  (`flatten`, with `--all` for deep); deep in **Ruby** (`flatten`), **jq**,
  **Elixir**, and Clojure's own `flatten` — which the Clojure community warns off
  for exactly the structure-loss reason. One level is the better fit here because
  it matches the level `...` already spreads and the level `+=` already
  distinguishes, and because the shell cases that produce nesting produce exactly
  one level of it. **Python** is worth noting for having *no* built-in at all
  (`itertools.chain.from_iterable`), which is a mild vote that this is less
  load-bearing than it looks.
- ***Name and depth argument.*** `:flat` matches the terse modifier vocabulary
  (`:len`, `:keys`, `:dedup`) and JavaScript's spelling; `:flat(n)` for an
  explicit depth follows `:get(i, default)` / `:split(SEP)`. Whether the deep
  case earns a spelling at all (`:flat(999)` is ugly; nushell's `--all` needs
  option syntax on a modifier) is open.
- ***Skip `:flatmap`.*** Rust and JavaScript ship `flat_map` / `flatMap` largely
  for iterator fusion, which a shell does not care about; `$xs:map(f):flat` says
  the same thing in one more token and composes with the existing chain grammar
  rather than adding a second name for `map`.)*

### Destructuring

Binding several names from a list in one step reuses the **list-literal syntax on
the left**. So splitting a string into variables — bash's `read a b c` — is just
*split then destructure*, and there is no monolithic `read` built-in:

```
[user pass uid gid home shell] = $line:split(":")   # a passwd line into fields
[k v]           = gets():split("=")                 # read a line, split on =, bind two
[first ...rest] = $args                            # ...rest absorbs the remainder as a list
[a b ...mid z]  = $xs                              # ends pinned; mid is everything between
[_ _ uid]       = $line:split(":")                  # _ discards a field
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

*(The examples here bind at **top level**, where the session is the current scope,
so the names are plain. Inside a `func` the same patterns bind function-locals and
carry the `_` prefix the [scope ladder](#variables-and-assignment) requires —
`[_user _pass _uid] = $_line:split(":")`. The grammar is identical either way; only
the names differ.)*

**The pattern grammar is shared with [`match`](#matching-match).** A bare
destructuring assignment is the *unconditional* use ("I know the shape — bind it");
a **`match` arm** is the *conditional* use — branch on shape or length and bind in
the same step:

```
match $args {
  []            => usage()                 # empty
  [cmd]         => run($cmd)               # exactly one, bound as cmd
  [cmd ...rest] => run($cmd, ...$rest)     # one-or-more; rest bound
}
```

So destructuring isn't *owned* by `match` — it is one list-pattern grammar, used
bare for the simple case and in a `match` arm when you need to branch.

**Regex captures.** The right-hand side is any list, and `:split` is not the only
way to build one — **`:match`** runs a regex against a string and hands back its
capture groups, so destructuring names them in one step. Like `~`, it is
**unanchored** — the first match anywhere in the string, so `[ip] =
$line:match(/\d+\.\d+\.\d+\.\d+/)` pulls an address out of the middle of a line; anchor with
`^…$` when you mean the whole string:

```
[one two]      = $str:match(/(.*) (.*)/)          # two groups → two names
[year mon day] = $date:match(/(\d+)-(\d+)-(\d+)/)  # an ISO date into fields
[ip]           = $line:match(/\d+\.\d+\.\d+\.\d+/) # no group → the whole match, one element
```

- **Positional groups** come back as a **list**, in order — the parenthesized
  sub-matches only, *not* the whole match — so `[one two] = …:match(/(.*) (.*)/)`
  binds exactly the two groups. A pattern with **no** group yields the whole match
  as a one-element list, so `[ip] = …:match(/re/)` still binds.
- **An unmatched group keeps its slot as `""`** — a group that didn't participate
  (an optional `(a)?(b)` against `"b"`) contributes an **empty string**, never a
  dropped position, so the list length equals the group count and the following
  bindings don't shift. mesh has no null, so `""` is the placeholder (a group that
  matched empty and one that didn't both read as `""` — distinguish with an
  explicit optional-group guard if you must).
- **Named groups** `(?<name>…)` come back as a **map** keyed by name
  (`m = $str:match(/(?<user>\w+)@(?<host>\S+)/)` then `$m.user`); an unmatched
  named group is present with value `""`. This pairs with map destructuring once
  that lands (deferred below). **Name all the groups or none** — a pattern that
  *mixes* named and unnamed groups is a **loud error** for the MVP (list or map is
  ambiguous); a later map-keyed-by-both-name-and-position rule is deferred until the
  need is real.
- **No match yields `false`**, not an empty collection. Matching is a pass/fail
  operation, so on a miss `:match` returns the bool **`false`** (status `1`) —
  keeping the model's rule that failure is signaled by a `false`, never by the
  *shape* of a value. On a match it returns the capture list (or map).
- **Test with `~`, capture with an `if`-binding.** A match returns a list/map, and
  a bare collection is *not* a condition (the [condition
  contract](#conditionals-if-is-an-expression) is a bool or a command, and a
  list has status `0` whether or not it matched). So use `~` for a pure yes/no, and
  put the assignment *in* the condition — the `if let` shape — to test **and**
  capture in one step, pattern written **once**, names in scope for the block:

  ```
  if $str ~ /(.*) (.*)/  { … }                          # yes/no only
  if [one two] = $str:match(/(.*) (.*)/)  { puts "$one / $two" }
  if m = $str:match(/(?<user>\w+)@(?<host>\S+)/)  { puts $m.user }
  ```

  As a *condition*, `lhs = rhs` is true iff the RHS is **truthy** (a `false` — the
  no-match, or `gets()` at EOF — fails it) **and** its shape fits `lhs`; on true the
  names bind for the block, on false it skips and binds nothing. A shape mismatch in
  the condition (`[a b]` against a three-element list) is a **soft false → skip** —
  deliberately unlike the bare statement below. This isn't regex-specific:
  `if line = gets() { … }` falls out of the same rule. The longer
  `match`-with-destructuring form is there when you want to branch on more than one
  shape:

  ```
  match $line:match(/(\w+): (.*)/) {
    [key val] => …       # matched — key/val bound
    false     => …       # no match
  }
  ```

- **A bare, unconditional bind is an assertion.** `[a b] = $str:match(/…/)` with
  no `if` says "I know this matches" — so a miss (`false`, not a two-element list)
  is a **loud error**, the [no-null](#variables-and-assignment) rule again: an
  unconditional bind that silently yielded `a = ""` would bury the bug. (The same
  mismatch *inside* an `if` condition is the quiet skip above — that contrast is the
  point of the `if let` form.) Reach for the `if` form when a miss is expected; the
  bare form when it isn't.

This makes `/re/` mesh's one regex story on the *value* side too: `~`
([Tests](#tests-and-comparisons)) answers yes/no, `:match` extracts the
captures — no `=~`-then-`$BASH_REMATCH` dance.

Named **`:match`** (not `:matches`), the unanchored scripting-world sense — Ruby
`String#match`, JS, Perl `=~`, bash `[[ =~ ]]`, grep — *not* Python's anchored
`re.match`. `:groups` / `:captures` were considered and dropped: `:match` pairs
with the [`match`](#matching-match) statement and the `~` test, one regex story
under one word.

*(**Decided — keep both, split by job** *(resolving the earlier "consolidate?"
open, settled alongside the [`match`](#matching-match) `~`-alignment law)*. They
overlap — `:match` is falsey on a miss, so `if $str:match(/re/)` covers `~`'s yes/no
— but the division is deliberate and worth two spellings: **`~` (and a bare `/re/`
`match` arm) answer *whether*; `:match` extracts *what*.** `~` reads as a bare
predicate and binds nothing; `:match` is the single capture path. Defining `~` as
literal sugar for `:match`-truthiness is a fine mental model and costs nothing, but
neither is dropped — a predicate that quietly returned a capture list, or a capture
call you had to read as a bool, would blur the whether/what line this keeps crisp.)*

**Regex is a first-class value** *(decided — porting `fromto`, `filter`, `he`,
`untar`)*. `/re/` is a **regex literal** evaluating to a regex **value**, and `~`
and `:match` **consume a regex value** — so `$str ~ /re/` and `$str:match(/re/)` are
the literal case. A `/…/` literal is **raw and does not interpolate** — like `r'…'`
but for a single lexical exception: **`\/` is the delimiter escape** (a literal slash
in the pattern, since `/` bounds the literal), and the lexer strips only that
backslash. Every *other* backslash reaches the regex engine verbatim (`\d`, `\.`,
`\\`), and `$` inside it is always the anchor; build a regex with a variable hole via
`re("…$var…")` (see the interpolation note below). A regex literal is recognized **only in the match slots** — the
`~`/`!~` RHS, the `:match` argument, the replace family's **first** (`OLD`) argument
(`:replaceall` / `:replacestart` / `:replaceend`), and a
`match` arm — and there a leading-slash
word is a regex **only when its base is a clean `/BODY/`** (the base is the word minus
any trailing `:` modifier chain, so `/\d+/:i` qualifies; the closing `/` is the base's
final character and `BODY` has no unescaped interior `/`); every other leading-`/`
word is a **path or glob** (full rule and cases in [Quoting](#quoting-and-escaping)).
The `~` RHS *also* takes a **glob**: a **relative** one is bare (`*.txt`, `src/**`),
and an **absolute** one now also goes bare — `$p ~ /usr/*/bin`, `$p ~ /tmp/*` — with
`$p ~ /usr/bin` reading as the path. The one residual is a single segment with a
trailing slash: `$p ~ /tmp/` is the regex `tmp`; write `$p ~ /tmp` for the path (or
`fnmatch($p, "/tmp/")` / `== "/tmp/"`).
**Everywhere else a `/…/` word is a path or string** — `cd /tmp/`, `grep /usr/bin`,
`$env.PATH:has(/usr/bin)`, `p = /etc/hosts` are all unaffected (a `/…/` is a regex
only in the enumerated match slots above — including the `:match` and replace-family
(`:replaceall` / `:replacestart` / `:replaceend`) `OLD` argument slots — never in a
plain argument or any *other* modifier slot, so
`:has(/usr/bin)` stays a path). To
hold a regex as a **value** anywhere else — a variable, a list, another argument — or
to turn a pattern that arrives as a **string** (`fromto $from $to`, any `grep`-like)
into one, you use the constructor **`re($str)`**: `$line ~ re($to)`,
`$line:match(re($to))`. `re` is a
**[built-in](#built-ins)** (a rich value can't come from an external) and
**fail-loud** (a malformed pattern errors at the call, not silently), carries flags
on the value (`re($x, ignore-case: true)`), and `re($s, literal: true)` quotes the string to
match **verbatim** (Perl's `\Q…\E`) — the common "match exactly what the user typed"
case. A **bare string is never auto-converted** *(decided — no RHS coercion, for
now)*: `$s ~ "a.b"` is an **error** pointing at `re("a.b")` or `/a.b/`, so a string
full of metacharacters never silently becomes a pattern — the same no-silent-coercion
rule as `:int`.

*(Settled — regex flags are `:` modifiers* (independent of the quoting exploration
above). Flags are set with the ordinary
[`:` modifier](#modifiers) machinery rather than a constructor flag: `re($x):i` /
`:ignorecase`, `:m` / `:multiline`, `:s` / `:dotall`, `:x` / `:extended` —
chainable, and carrying the readable-or-terse dual spelling used elsewhere. This
applies to `re(…)` and to the `/…/` literal (`/\d+/:i`). *(Decided: the `:` modifiers
**coexist** with the `ignore-case:` constructor argument — both spellings are
supported.)* `literal:` stays a
**constructor** argument regardless, since it
changes how the string becomes a pattern rather than being a post-hoc flag on a
finished regex. Match-behavior flags (`:i` `:m` `:s`) work as post-hoc modifiers on
any regex value, and so does a **parse-affecting** flag like `:x` when the pattern
compiles without it — `re("a b"):x` is a finished value, extended after the fact. What
a post-hoc flag cannot do is **rescue source that never compiled**: `re()` is fail-loud
and compiles the *unflagged* pattern first, so `re('foo # (')` errors before a trailing
`:x` could make it valid in extended mode. A parse-affecting flag must therefore be
known at construction *whenever the unflagged text is invalid*: folded in pre-compile on
a `/…/` literal (`/foo#(/:x`, compiled once; `#(` is ignored only in extended mode) or
passed as a constructor argument (`re($x, extended: true)`).)*

*(decided: **`/…/` does not interpolate** — it is a **raw** regex literal (raw except
the `\/` delimiter escape; see the regex-value section above), so a `$` inside `/…/`
is always the anchor/metacharacter, with no splice-vs-anchor ambiguity. To build a regex with a variable hole, use
**`re("…$var…")`**: the `"…"` string does the interpolation (its settled `$`-splice /
`\$`-literal rules apply), then `re()` compiles. So there is **one** interpolation
path — the `"…"` string — and no `/$var/` special case; the earlier deferred sugar is
dropped.*

*An interpolated hole is **regex source** by default (metacharacters live — building a
pattern from parts is what `re()`-from-a-string means). To splice a value as a
**literal** (match it verbatim, the regex-injection-safe case), quote it with the
**`:quotemeta`** modifier — `re("^${user:quotemeta}@")` — Perl's `\Q…\E` / Python's
`re.escape` as an ordinary modifier. It is the per-value cousin of `re($s, literal: true)`
(which quotes a whole string); use `:quotemeta` when only the hole is literal and the
skeleton is a real pattern.)*

*(deferred: **map destructuring** — `[name: n, age: a] = $m` binding by key — a
natural extension of the same idea; and nested patterns (`[a [b c]] = …`).)*

### Functions

```
func greet(_name) {
  echo "hi, $_name"
}

greet world               # -> hi, world
```

Paren-delimited, `func name(params) { … }` — C/Go/JS muscle memory, and unlike
Elvish's `{|a b| … }` or Nushell's `def f [a b] { … }` it puts the signature
where a reader already looks for it. Parameters are **named**: inside the body
you reference `$_name`, never `$1`. This is the fish `--argument-names` idea
promoted to the declaration itself.

Parameters carry the `_` prefix because they *are* function-locals, and the
[scope ladder](#variables-and-assignment) spells every function-local that way —
there is no exemption, so every binding visible inside a body reads as local
without consulting the signature. A **flag** parameter exposes its name minus the
leading `_`, so `--_region` is passed as `--region`; call sites are therefore
unchanged by the prefix, and positionals and rest parameters never name themselves
at a call site at all.

The signature borrows Nushell's/Elvish's proven vocabulary — *positional*,
*optional-with-default*, *flag*, and *rest*:

```
func deploy(_env, --_region = us-west, --_force, --_tag = latest, ..._hosts) {
  # $_env     required positional
  # $_region  valued flag,   defaults to us-west   (passed as --region)
  # $_force   boolean switch: true iff --force was passed
  # $_tag     valued flag,   defaults to latest    (passed as --tag)
  # $_hosts   list of any remaining positionals    (rest / "flattening")
}

deploy prod --force web1 web2
#   _env=prod  _region=us-west  _force=true  _tag=latest  _hosts=[web1 web2]

deploy prod --region=eu-west --tag=v9 ...$fleet
#   _env=prod  _region=eu-west  _tag=v9  _hosts = the spread-in elements of $fleet
```

`_region` is a **flag**, not an optional positional, on purpose — with a
`..._hosts` rest parameter present, an optional *positional* `_region` could not
be skipped (the first host would silently bind to it). That is the general
rule below. An optional positional is fine when it is the last non-rest
parameter and can just be omitted from the right:

```
func tag(_image, _version = latest) {        # optional positional, no rest
  docker tag $_image $_image:$_version
}
tag app          # _version defaults to latest
tag app v9       # _version = v9
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
- **Flags** are declared with a leading `--` on the parameter name, so `--_force`
  (no `=`) is a boolean **switch**, false unless passed, and `--_tag = default` is
  a **valued flag**. Each exposes its name minus the leading `_`, which is what
  callers write. At the call
  site each has the two equivalent spellings from
  [Calling for a value](#calling-for-a-value-and-lambdas): the dashed sugar
  (`--force`, `--tag=v2`) and the value-mode `key: value` pair (`force: true`,
  `tag: v2`) — `--force` ≡ `force: true`, `--tag=v2` ≡ `tag: v2`. A valued flag in
  dashed form is **attached only** (`--tag=v2`, never a separate `--tag v2` that
  consumes the next token), so every argument stays **self-contained** — which matters
  because a value-mode call's arguments are comma-separated. Neither a switch nor a
  valued flag ever swallows a following argument: `--force web1` is the switch `--force`
  plus a positional `web1`, and a bare `--tag` with **no `=value`** is a missing-value
  **error**, not a consume-the-next-token. (An **external** command still accepts the
  separate `--tag v2` getopt form — mesh does not parse its flags, it only passes the
  tokens through.)
  Flags may appear in any order at the call site and are *not* consumed as
  positionals — this is why a shell wants real flag parsing in the signature
  rather than hand-rolled `case $1` juggling. An argument that begins with `--`
  but names **no declared flag** is an **error**, not a silently-forwarded
  positional — a typo'd flag should fail loudly, not vanish into `...rest`.
  When a flag is given **more than once** (directly or via a spread), the
  **last occurrence wins** for a valued flag (`--tag=v1 --tag=v2` binds `v2`, the
  universal CLI convention that makes a forwarded default overridable), and a
  repeated switch is simply still true (idempotent) — neither repeat is an error.
  *(TODO — flag-grammar extensions the settled `--long` grammar doesn't yet cover,
  surfaced porting `recent`/`shift_options`/`homepkg`/`setup`:*
  - ***Short & numeric flags.*** Interactive use leans on `-N` counts (`recent -20`,
    the `head -20` idiom), single-letter switches (`-v`), bundles (`-abc`), and
    attached values (`-ffile`). Decide whether a function can declare short aliases
    (`--verbose | -v`) and a numeric-count form, or whether short/numeric flags stay
    an external-tool-only convention and in-shell functions are `--long`-only.
  - ***Enum / choice-constrained values.*** `homepkg --backend=mamba|conda|github`
    has no parse-time validation — "enum" exists only as a *completion* value type.
    Let a flag or positional declare an allowed-value set that validates at the call
    and feeds completion.
  - ***Mutually-exclusive switch groups.*** `setup`'s `--kde`/`--hypr`/`--sway` are
    three separate switches where at most one is allowed — a *different* requirement
    from a single enum value (a plain allowed-set check would still pass
    `setup --kde --sway`). Either steer such interfaces toward one enum-valued option
    (`--desktop=kde|hypr|sway`) or grow a mutex-group constraint in the signature.
  - ***Negatable / tri-state flags.*** `setup`'s `--gui`/`--no-gui` auto/yes/no
    pairs have no expression: a switch is binary, false-unless-passed, with no
    `--no-` negation. Allow a switch to auto-derive a `--no-` form (a
    enum-valued `auto`/`yes`/`no` binding), or a first-class three-valued flag.
    The omitted case must bind `auto`; it cannot be represented by an unbound or
    unset value because mesh has no absent value and omitted switches are bound.
  The `--`-mid-stream that `shift_options` relies on is already covered by the
  terminator rule below.)*
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
  (`...$xs`, one argv entry per element) or join it (`$xs:join(",")`, one
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
- **Value and status are separate channels** *(shipped — and
  [revised](#open-questions): the channels collapse into one once a **status is a
  value**. `status(N)` is an ordinary builtin — so `status 5`
  bare and `status(5)` in an expression are its two spellings, by the usual
  mode rule — and bare `return X` keeps meaning the value `X`. What follows
  describes the shipped two-channel behavior, which the revision leaves intact
  except that `fail N` becomes sugar for `return status(N)`.)* A function has
  three outputs, not two: the **bytes** it writes to stdout, the **value** it
  returns, and its **exit status**. `return` fills the value channel; `fail`
  fills the status channel. Neither is derived from the other:

  | Form | Value | Status |
  | --- | --- | --- |
  | body ends in a command | `Status(n)` — that command's status *as a value*, per the [status decision](#open-questions) | the command's own |
  | `return $v` | `$v` | `0` — or `1` when `$v` is `false`; or `n` when `$v` is a `Status(n)` |
  | `return status N` | `Status(N)` | `N` |
  | `return true` / `return false` | the bool | `0` / `1` |
  | bare `return` | the result so far | the **last** status |
  | `fail` / `fail 123` | `Status(1)` / `Status(123)` | `1` / `123` |

  **Only `false` fails** *(and, under the [status decision](#open-questions), a
  `Status(n)` — which projects to its own `n`, so `fail 123` and
  `return status 123` both leave 123. The invariant below is otherwise unchanged:
  what that decision adds is a second value that can carry a failure, not a
  reason for any other value to.)* `false` is mesh's "no result" — what `gets()`
  yields at
  EOF, what a failing predicate returns, what `if x = f() { … }` tests for — so it
  is the one value whose status is worth reporting as nonzero. Every other value
  *is* a result, and producing a result is success, which is why `return 5` carries
  the integer five with status `0` rather than claiming exit code 5. A returned
  string, list, map or zero is likewise a success.

  That keeps a session predicate like `connected-remotely` usable in command
  position — `if connected-remotely { … }` reads correctly whether the body ends
  in a test, a `return $cond`, or a `fail`.

  **`fail` is the status channel's verb**, spelled apart from `exit` because
  `exit` tears down the whole shell from wherever it is called, while `fail`
  leaves only the current function — the same unit `return` leaves. It takes a
  nonzero code only: bare `fail` is `1`, `fail 123` names a code, and `fail 0` is
  refused, because a `fail` that succeeds is always a mistake and the spelling for
  "leave with success" is `return true`. It is a reserved name, as `return` is.

  Bare `return` is *not* a synonym for `return true`: it means "stop here, as if
  the body ended at this line", so it propagates a **failure** as readily as a
  success — the one place the word `return` does not imply success.

  A status is the OS's **8-bit** process status. The value channel is not: the
  full integer survives as the function's value (`n = f()`), because it was never
  a status to begin with.

  *(Why not one channel. Deriving the status from the value — `return 5` meaning
  both "the number five" and "exit code 5" — conflates two unrelated things and
  makes every integer-returning function a landmine. The nullable `false | T`
  encoding is a real and useful duality and is kept; an integer's coincidental
  resemblance to a status is not. **This argument was reviewed and holds** — see
  [Open questions](#open-questions). The review's conclusion is that the missing
  piece was never a second channel but a *spelling*: with `status(N)` as a value,
  `status 5` and `return status(5)` name a status explicitly, and `return 1` can
  go on meaning the integer one.)*
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

*(TODO: **wrappers, forwarding, and dynamic definition.** [No alias
mechanism](#built-ins) is *decided* — what `alias ll` defines is a `func`. But
real configs still need things a plain `func` doesn't yet give cleanly; these are
open:*
  - *A **terse forwarding wrapper.** ~~Open.~~ **Settled, and built:
    [`wrapper func`](#functions).** Even `func co(..._args) { vcs checkout ...$_args }`
    was not a fully transparent baseline: under the settled function rules an
    **undeclared long flag** (`co --amend`) is rejected before `...args` can collect
    it, so the caller needed an explicit `--` — the same trap nushell hits, where
    a plain `def` wrapper rejects `co -m msg` as an "unknown flag" unless it uses
    `def --wrapped`. *Decided (porting the ssh/vcs wrappers): a
    wrapper **cannot** validate the flags it forwards — it does not know the callee's
    grammar — so a passthrough wrapper forwards unknown flags **verbatim** and
    validity is enforced at the **wrapped call**: the wrapped in-shell `func`'s own
    signature rejects a bad flag (a loud error* there*), or the external program
    rejects it itself. Disabling the wrapper's flag parsing therefore does not drop
    the check, it **relocates** it to where the grammar is known.*

    *The surface is a **prefix marker**, `wrapper func name(…) { … }`, for the same
    reason `fork func` is spelled that way: a word before `func` is already how a
    definition's properties are marked here, so nushell's `--wrapped` would have been
    importing a spelling that follows from `def` being a command rather than a
    keyword. `wrapped` was rejected on its own terms too — it is an adjective
    describing the **callee**, and the callee is not what the marker sits on.*

    *The **shorthand** is settled and built too: **`alias co = vcs checkout`**,
    sugar over the marker rather than a competing mechanism. `wrapper func
    co(...args) { vcs checkout ...$args }` is transparent but not terse, and the
    everyday case is a name plus a command prefix. The word `alias` is reused
    because it is the one every shell user reaches for; the "No aliases" text
    under [Built-ins](#built-ins) is reworded to say what is actually dropped —
    the **mechanism** (parse-time textual expansion, its own resolution stage) —
    rather than the name. A self-naming alias desugars through `command`, so
    `alias grep = grep --color=auto` reaches the program instead of recursing,
    and the right-hand side is a **command, not a string**: bash's quoted
    `alias ll='ls -l'` is diagnosed rather than left to become one word naming no
    program.*
  - ***Running a wrapper under `sudo` / `xargs` / `watch`.** Because mesh commands
    are functions, not aliases or `PATH` binaries, `sudo ll` can't see `ll` — bash
    papers over this with the invisible `alias sudo='sudo '` trailing-space trick.
    mesh should offer a deliberate way to say "expand this command's first argument
    as a mesh command" instead.*
  - *Whether to expose **dynamic definition** (a function whose name is computed —
    the `set_up_ssh_aliases` `eval` loop) at all; a dynamically-defined function
    still [completes](#completion) like any other once it exists, so the cost is
    **readability and static analysis** (you can't tell from reading the config
    which commands are defined), not completion. The wrapper shorthand may cover
    the real need; if a general escape hatch is wanted, prefer a **scoped**
    primitive over bash's
    string-concatenating `eval`. Leaning: a forwarding-wrapper shorthand with
    transparent flag passthrough, defer general dynamic definition.*

    *Deferred, and noted only so the constraint is written down: a name
    containing a **dot** cannot be defined at all — `func a.b()` is refused in
    every spelling, quoted included. The refusal now names the reason rather than
    pointing at the dot (it used to be the bare ``expected `(` ``), and it is a
    runtime error against that one definition rather than a syntax error against
    the file, but the rule itself is unchanged. bash, zsh and fish all accept one,
    which is how their `set_up_ssh_aliases` loops give an FQDN `Host` entry a
    command. Command position looks unambiguous (a bare word there is already a
    command name, and dotted program names are ordinary), so the parser change
    would be narrow; the question is value-call position against member access.
    Low priority — the motivating case, FQDN ssh aliases, is not actually
    wanted, and the config that raised it filters those names out deliberately.)*

### Isolation and subshells

**A plain `func` does not isolate process state.** cwd, umask, and the `env`
map are OS process state, not mesh values, so a `func` runs *in the current
process* and its `cd` (or `export`) **persists after return** — exactly like
bash, and exactly what navigation helpers want:

```
func proj(_name) { cd ~/work/$_name }   # moving your shell is the point
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
fork { cd build; make }                 # subshell: forks; cwd/env/umask/vars
                                        #   isolated, nonzero exit can't kill
                                        #   the outer shell
fork func build() { cd build; make }    # a func whose *body* is a subshell — the
                                        #   `fork` prefix (vs a plain `func`) is the
                                        #   isolation flag
in dist { rm -rf * }                    # scoped cwd: run the block there, restore
                                        #   after — NO fork (cheaper than subshell)
```

A **subshell forks**, so — like `export` — only **bytes** cross back out (its
stdout); rich list/map values do not survive the process boundary. `in DIR { }`
does not fork: it is the lightweight "do this over there without stranding me,"
covering the common `pushd`/`popd` pattern with a block.

*(open — **can a subshell return a value?** "Only bytes cross back" is written
above as if it were a law of the boundary, but it is really argv's rule borrowed
for a different problem, and the two have different requirements.*

*The argv rule is about **flattening**: one value has to become bytes that a
program will read as one or more arguments, and a list fails it because there is
no canonical separator to pick — that is the table in §"Values and the bytes
boundary", and it is right. Returning a value across a fork is about
**reconstruction**: the child has to write something the parent can turn back into
the same value. A separator problem does not arise, because a structured encoding
carries its own delimiters. So a list crosses a process boundary perfectly well
while still — correctly — failing to cross into argv.*

*The appealing form of this is that mesh already has the encoding: its own literal
syntax. The child writes the value as the text you would have typed for it, on a
pipe of its own (not stdout, which must keep streaming), and the parent reads it
back with the ordinary expression parser. No new format, no serialization
dependency, and a wire form that is debuggable by looking at it. What crosses is
then exactly the values that **have a literal form** — string, int, bool, list,
map, Duration — and what does not is exactly the values with no form at all: a
stream handle (a descriptor means nothing in another process), a function (a
closure over bindings that did not cross), an Instant or regex until their
spellings round-trip. That is a rule with a reason rather than a list.*

*The **writer** now exists, and is [`:repr`](docs/REFERENCE.md): a value written
as the source you would have typed for it, with the quoting exact — `42` and
`"42"` do not both come out as `42`, and `[]` and `[:]` stay apart. It refuses
the values with no literal form by name rather than approximating them, so the
"what crosses" rule above is enforced in one place rather than restated at the
boundary. What crosses is therefore settled; what remains for the channel is the
plumbing.*

*One thing recorded here as missing turned out not to be. A value larger than a
pipe buffer was said to need the unlinked-temp-file trick heredocs use, or the
child blocks writing while the parent blocks waiting — but that is `$( … )`'s
problem, where **two** pipes are drained and reading them in sequence deadlocks
on whichever is not being read. A value channel is **one** pipe: the parent
drains it to the end and only then waits, so no temp file and no reader thread
are involved. The hazard that does remain is a grandchild inheriting the write
end and holding it open past the child's exit — bash's `$(sleep 10 &)` hang —
which length-prefixing the payload answers directly, since the parent then stops
at the value's end instead of waiting for EOF.*

*Worth noting what it would cost: `fork` stops being purely "a process boundary"
and starts being a value channel, which is a bigger promise to keep — every future
value type has to answer whether it crosses. The alternative is to keep the rule
as it stands and let `fork func` value calls be an error, which is what they are
today.)*

*(open, deferred cluster: whether a `func` defined inside a `func` is visible
only there. Also a **TODO — dynamic scope**: the same "extract a chunk into a
subfunction" goal that motivates persist would be served further for *variables*
by letting an extracted helper see the caller's locals; worth weighing dynamic —
or opt-in dynamic — scope against the lexical default decided above.)*

### Calling for a value, and lambdas

A `func` has two outputs — the **bytes** it writes to stdout (composes in pipes,
like any command) and the **value** it returns (last expression / `return val`,
a rich list/map/scalar). Which you get is chosen by **how you write the call**,
and that choice is really a choice of **mode**:

| Mode | Form | You get | Idiomatic args |
| --- | --- | --- | --- |
| **command** — run it | `f arg --flag` (bare), or `$(f arg)` | stdout streams (status is the result); `$(…)` captures the bytes | **space**-separated positionals, `--flag` / `--flag=value` |
| **value** — call for its return | `f(arg, key: value)` (parens attached) | the mesh value | **comma**-separated positionals, `key: value` options |

The split is by **mode, not callee**. A function *run* in command position looks
like a command on purpose — that is how you use it at the prompt (`co main --amend`,
bare, no ceremony) — and the *same* function *called for a value* looks like a
function (`x = co(main, amend: true)`). Command position is unchanged from any shell;
the comma grammar appears **only** inside `f(...)`, so the prompt stays all spaces and
commas live in expressions. (The `f(...)` marker is required at all because a bare word
on an assignment RHS is already a [literal string](#variables-and-assignment) —
`x = greet` binds `"greet"`, so reaching a function's value needs the parens.)

**Options have two equivalent spellings, one idiomatic per mode.** The `--force` you
type at the prompt and the `force: true` you write in a value call are the *same
option*:

- **Value mode — `key: value`**, the [map literal](#maps-associative-arrays) shape, so a
  call's options *are* a little map — and one can be **spread**: `deploy(prod, ...$opts)`
  where `opts = [region: us-west, force: true]`. Values compose (`port: $base + 1`).
- **Command mode / dashed sugar — `--flag` / `--flag=value`**, with a bare `--flag` ≡
  `flag: true` (`--region=us-west` ≡ `region: us-west`; `--force` ≡ `force: true`). An
  explicit **false** is the `force: false` pair; there is no `--no-flag` negation sugar
  (whether a switch auto-derives one is left open under [Functions](#functions)).
- The two are **interchangeable** — you *may* write `--flag` inside `f(...)`, it is just
  clumsier than the `key: value` it equals; and `key: value` is **value-mode only** (a
  bare `key: value` in space-separated command position tokenizes awkwardly, and maps
  need `[...]` anyway).
- A bare `key=value` (no colon, no `--`) stays a **literal string** positional — that is
  the `env FOO=1` / `make CC=gcc` / `git commit --author=me` case — so `=` is never an
  option separator on its own; it only appears attached to a `--flag`.

```
config = load-env($path)                     # value call, one arg
n      = add($a, $b)                         # positionals comma-separated
deploy(prod, region: us-west, force: true)   # value mode: key: value options
deploy prod --region=us-west --force         # command mode: the same options as --flags
deploy(prod, ...$opts)                       # opts = [region: us-west, force: true]
config = load-config()                       # zero args still needs () — a bare name is a string
```

Rules:

- **Positionals are positional-only** — passed by position, never by name
  (`cp(a, b)`, not `cp(dest: b, src: a)`), exactly like a shell command's
  positional arguments. A parameter's *name* is therefore never part of the
  positional call surface, so `f(help)` is unambiguously the string `"help"` in
  first position, and a `--help` **option** is told apart by its leading `--` — the
  same way a shell already separates flags from arguments.
- **A signature declares options with the `--name` spelling**
  (`func deploy(_env, --_force, --_region = us-west, --_out = -) { … }`) and
  positionals as bare names (`_env`); either call spelling (`--region=us-west` or `region: us-west`)
  binds the same parameter. `...spread` works in both modes — a list of positionals or
  a map of options.
- **The channels are independent.** During `x = f(…)`, whatever `f` writes to
  stdout still goes wherever stdout goes — the value call reads the *return*
  value, it does not capture or suppress output. A well-behaved value function
  simply does not print; one that legitimately does both streams *and* returns.
- **Both channels at once — `:capture`.** When you genuinely need more than one,
  `f(…):capture` runs the call and returns a **record of every channel**: `.value`
  (the return value), `.out` and `.err` (its stdout / stderr, as **raw byte-strings**
  — split them with the usual [`:lines`](#modifiers) / `:split` / `:nulls` modifiers
  as needed, so the record bakes in no split policy), and `.status` (the exit
  **int** — becoming a **`Status`** under the [status decision](#open-questions),
  which is the "richer status value" this line used to leave as a TODO. It has to
  move with `$sh.status`: `.status` is the *only* result channel for an external
  capture, so leaving it an int would make `return $r.status` forward a failure as
  successful data — the very bug that decision types `$sh.status` to prevent). Read them with ordinary field
  access — `r = f(x):capture` binds `r`, then `$r.value` / `$r.out:lines` read it. It is an
  *invocation-level* modifier, not a plain value [modifier](#modifiers) — it has to
  wrap execution, since by the time a value modifier saw the return value the stdout
  would already have streamed away, the same reason `$(…)` is a wrapper rather than a
  postfix. The **same `cmd(…):capture` spelling works on an external** — and is the
  single exception to the value-call error below *(which the
  [status decision](#open-questions) removes: an external's result **is** a
  `Status`, so `grep(foo)` returns `Status(1)` rather than erroring, and `f` /
  `$(f)` / `f()` come to mean the same three things for an external as for a
  function)*: a bare `grep(foo)` errors **today** because it
  asks for a return value the command lacks, but `grep(foo):capture` asks for the
  channel record, so it is allowed and comes back the same **minus `.value`** (there
  is none — accessing it is a loud no-such-field error). External captures accept
  positional arguments only. A direct `key: value`, a
  dashed option interpreted through a mesh signature, or a map spread is an error
  because an external has no signature or canonical named-option encoding; pass
  the intended argv tokens as positionals instead (for example, `"--color=never"`).
  Reaching for `:capture` is
  the sign a function is doing two jobs at once; a single-channel function needs none
  of it. *(TODO — further fields such as timing and a `pipestatus` list; today it is
  the four above.)*
- **Externals have no return value**, so a bare `grep(foo)` is a **runtime error**
  that points you at `$(grep foo)` for stdout, or `grep(foo):capture` for the full
  channel record. Rich values stay in-shell — the same bytes-only boundary as
  `export` and subshells. (`f` resolves at call time, so this is a runtime, not parse,
  distinction.)

**Lambdas** are then just anonymous functions — the `func` declaration minus the
name, reusing its whole signature grammar (defaults, `--flags`, `...rest`) — and
they are value-called the same way:

```
double = func(x) { $x * 2 }       # a function value bound to a variable
y = $double(5)                    # value-call it through the variable

evens = $xs:filter(func(x) { $x % 2 == 0 })
stems = $files:map(func(f) { $f:stem })    # :map / :filter / :each take a lambda
```

`func(params) { … }` (over an Elvish-style `{|params| …}`) keeps **one parameter
syntax** for named and anonymous functions, and the transform modifiers
(`:map` / `:filter` / `:each` / `:sort …`) are where lambdas earn their keep,
complementing the auto-mapping value modifiers for the cases a bare modifier
can't express.

A **bare modifier reference is itself a callable value**, so where a predicate or
mapper is wanted you can hand a modifier directly instead of wrapping it in a
lambda: `$files:filter(:exec)` *is* `$files:filter(func(f) { $f:exec })`, and
`$paths:map(:stem)` *is* `$paths:map(func(p) { $p:stem })`. A `:mod` in argument
position denotes "the function that applies `:mod`"; the lambda form remains for
anything a single modifier can't say.

**Referencing a command or function by name — `&name`** *(decided)*. The two namespaces
[coexist for free](#variables-and-assignment) — `f = 5` and `func f()` both bind, and
`$f` and `f` tell them apart — but the split is one-directional: a variable can *hold*
a function, while a named `func` has no value spelling at all. A bare word in value
position is a [string literal](#bare-words-and-quoted-values--decided), so
`$xs:map(up)` hands `:map` the string `"up"` and it reports `argument must be a
function, got a string`. Only two things produce a function value otherwise — a lambda
and a `:mod` reference — and neither can name a `func` you already wrote.

`&name` is that spelling: a prefix `&` in value position denotes the **callable** of
that name — a `func` you wrote, a builtin, or an external on `PATH`. The gap it fills
is the named `func`, but the reference is over the **command namespace**, not a
`func`-only lookup; that is what lets a hook slot hold `&reload-config` without the
slot caring which of the three it is, and it falls out of late resolution rather than
being a second rule.

**Keywords are not referenceable.** Command position's full order is **keyword →
builtin → func → external**; `&name` takes that path *minus the first step* — **builtin
→ func → external** — so `&if` and `&return` are not references to control flow but
**errors**, reported at *parse* time, since the reserved words are known statically.
That is not a carve-out invented for `&`: a quoted or expanded name already skips the
keyword step (`"if" x` resolves func → external), so the keyword step belongs to bare
command position alone, and `&` is one more spelling that is not it.

```
$xs:map(&up)                    # the func `up`, as a value
$sh.preprompt.git = &git-info   # the same reference, in a hook slot
double = func(_x) { $_x * 2 }
$double(5)                      # a lambda lives in the *variable* namespace: `$`, not `&`
```

`&name` and `$name` are therefore different callables — `&g` is whatever `g` names in
the command namespace, `$g` is whatever the variable `g` holds — which falls out of `$` meaning "the value namespace"
everywhere else, rather than being a rule of its own.

**`&name` is a late-bound name reference, not a captured function object.** It resolves
when it is *called*, against the command namespace as it stands then — the rule
[command position already uses](#variables-and-assignment) — so redefining `up` changes
what an already-registered `&up` runs. That is what lets the spelling be shared with
[hooks](#hooks-and-the-prompt), whose re-source-safety *is* late binding: a handler
registered as `&git-info` picks up a redefined `git-info` on the next prompt.

**Whether the reference is legal and what the *call* yields are separate questions.**
`&name` is well-formed for anything the callable path above resolves — any builtin,
`func`, or external. *Today* not everything it resolves *returns* something:
externals have no return value, and neither do the **effect-only builtins**, so
`r = puts(1 + 2)` reports `a command has no return value` and `&puts` is no more
usable in a value slot than `&grep` is — the division being **returns a value
versus runs for effect** rather than builtin-versus-external.

*(The [status decision](#open-questions) removes that division: every call yields
a value, and a command-shaped one yields a `Status`, so `puts(1 + 2)` binds
`Status(0)` and `&puts` becomes usable in a value slot. The cost is a lost
diagnostic — using `puts` for its value is a real mistake and the error caught
it — accepted because the alternative is carving effect-only builtins out of
"every call yields a value," which would put the null back that the decision
exists to remove.)*

A reference to an effect-only callable is fine in a slot that calls its handler for
**effect** (a `$sh.preprompt` entry, a `$sh.signal.<NAME>` handler) and, *today*,
fails *when called* in a slot that needs a **value** (`:map`, a prompt segment that
must return a piece). That failure is about the call producing nothing to use, not
about the reference being ill-formed, and it lands at call time for the same reason
the bare-`grep(foo)` error does.

*(Under the [status decision](#open-questions) that second half goes away with
the rest of the division: the call yields `Status(0)`, so `:map(&puts)` produces
a list of statuses and a prompt segment gets a piece rendering as `0`. Neither
is *useful*, and both are still mistakes — but they are mistakes the value
system no longer has a way to catch, which is the diagnostic cost the decision
accepts above rather than a second rule. What stays is the slot's own
requirement: a segment that must return a **string** still refuses a `Status` on
its own terms, not on "the call produced nothing.")*

Late *dispatch* does not by itself decide whether a slot may hold a reference to a name
that does not exist **yet**. Command position accepts one — `func f { g }` resolves `g`
when `f` runs, so definition order is irrelevant there — while a hook slot validates
eagerly at registration today, and whether it should keep doing so is its own question
(`docs/HOOKS.md` D3). `&name` fixes when a reference *resolves*, not when it is
*checked*; the two are easy to conflate and only the first is decided here.

An early-bound capture (Perl's and Raku's `&foo` is a code object)
would make `&` the one construct in mesh that snapshots the command namespace,
reintroducing exactly the definition-order sensitivity call-time resolution exists to
avoid. Elvish's `$f~` — the other two-namespace shell — is likewise a live lookup.

**Why `&`.** In value position it is unclaimed, and it cannot collide with
backgrounding, which is *postfix* in statement position (`make -j8 &`); a prefix `&`
where a value is expected is unambiguous. The costs are named rather than dodged: `&`
is the one glyph in a shell that already says "background", so `:map(&up)` misreads for
a beat, and a future infix bitwise-and is spent — the worry [arithmetic](#arithmetic)
already records for `^`. Both survivable, since prefix and infix are different
positions, as `-` already demonstrates.

*Rejected: `\name`.* Perl's `\&foo` is the shape people half-remember, and it works
there because `\` is Perl's reference operator. Here `\` is the escape and the line
continuation — `x = \up` already binds `"up"` — so it would silently change an existing
spelling rather than add one.

*Not a second spelling: `:name` for a user's own.* `:upper` is already a one-argument
function reference in value position, and [it has since been decided](#modifiers) that
a user may add to that vocabulary — by **declaring a modifier**, `func _s:name()`, and
only that way. An ordinary `func helper(_s)` is *not* callable as `$x:helper`; the
declaration is what makes a modifier, which is the whole of that decision. Either way,
this does not make `&name` and `:name` alternatives for the same job. The line between them
is by **shape**, not by who wrote the name: `:name` is the postfix, auto-mapping
modifier form, applying to the subject on its left; `&name` is the general reference,
any arity, any slot, usable wherever a value goes. A reader can still predict which
applies without knowing whether a name shipped with the shell.

**A lambda captures by an explicit list — `with (…)`** *(decided — a change from what
runs today, and a reversal of what an earlier revision of this section decided)*. The
body's scope parent is the *session*, so a lambda sees session and global bindings but
not the function-locals beside it, even when it is called immediately, in the same
scope:

```
func f() { _n = 41
  _g = func() { puts $_n }
  $_g() }                       # today: `_n: unbound variable`
```

That makes lambdas and [`_`-prefixed locals](#variables-and-assignment) mutually
unusable in exactly the place a lambda earns its keep — `$xs:filter(func(_p) { $_p:ext
== $_want })` cannot reach the `_want` bound on the line above it. The same text works
at top level and fails inside a function, which is the sharpest statement of the bug.

The fix is a **capture list**, written after the parameters:

```
func pick(_want) {
  return $_xs:filter(func(_p) with ($_want) { $_p:ext == $_want }) }
```

`with ($_want)` is evaluated **where the lambda is written**, in the frame that can see
`_want`, and the value is copied into the function value. Reading an unbound name there
is the usual loud error, at the point of capture rather than at the call.

*Two decisions, not one.* They are usually run together and should not be, because
different things decide them.

- **By value, not by live binding — decided by the missing garbage collector.**
  Capturing a *scope* means a frame outliving its call, which means a shared,
  reference-counted scope; a lambda stored into the scope it captured is then a cycle
  that is never freed. Capturing *values* has no such problem: they are copied into the
  existing function value, nothing outlives its frame, and a self-referential
  `_g = func() with ($_g) { … }` is an unbound-variable error at the point of capture,
  because `_g` is not bound until the assignment completes. This is the axis where
  having no GC actually forces the answer. Rust is the counterexample worth knowing:
  the other GC-less language with closures, it requires no capture list and leans on
  the borrow checker instead — so the absence of a collector does not dictate a *list*,
  only that the lifetime question gets answered somewhere.
- **Explicit, not implicit — decided by the `_` rule, and it is a readability call.**
  A `_` name is [always current-scope](#variables-and-assignment), which is what makes
  collision impossible by construction. Implicit capture works against exactly that: it
  would make `$_want` in a body resolve to *some other frame's* scope, silently. A
  capture list keeps the invariant, because the captured value arrives as a binding in
  the lambda's **own** current scope — `with ($_want)` reads as "bind my `_want` from
  the enclosing one", a copy in rather than a reach out. Note what this bullet does
  *not* rest on: implicit capture **by value** would have been cycle-free too, so the
  GC argument above does not reach this choice. PHP's arrow functions capture by value
  implicitly, and immutable languages such as Erlang and Elixir get it for nothing.

*Where the spelling comes from.* PHP is the near-exact precedent — `function ($p) use
($want) { … }` — a garbage-collected scripting language that took an explicit list for
readability rather than for memory, which is mesh's reason too. C++ (capture lists in
C++11, init-capture in C++14) and Swift's `[weak self]` are the other explicit ones, and
both are mainly about object lifetime. Everything else in common use captures
implicitly. The keyword is `with` rather than `use` because mesh already spells
"establish these bindings for the following block" that way in the
[`with FOO=1` prefix](#the-environment).

*What it costs, stated plainly.* A lambda cannot read a local it did not name, so
adding a name to a body means adding it to the list — the compile-time-ish nuisance
that is the price of the lifetime being visible. And a captured value is a **copy**, so
a lambda can never accumulate into an enclosing local; `global` remains the way to
mutate something a lambda can see. For a shell that is a fair trade — `:map` /
`:filter` / `:len` cover most accumulation — but it is a real limit and not a temporary
one.

*What this reverses.* An earlier revision decided that a lambda captures its **defining
scope**, with "by binding or by value" left open as a sub-question, and asked for the
scope rung to be built as a parent link. That is withdrawn. Three things follow from
the withdrawal, all of them simplifications:

- **Scope depth stays two.** A lambda's scope is its parameters plus its captured
  copies, and its parent is the session. There is no chain to walk and no parent link
  to build, so [§Scope](#variables-and-assignment)'s "two is the current depth, not a
  cap" is no longer under pressure from this decision.
- **The by-binding / by-value sub-question is answered by removal.** Capture is always
  by value, and only of what is named. Reading a *session* variable from a body stays
  late, because the session outlives every frame — which is the principle the whole
  rule rests on: **you may read late only from a scope that outlives you.** A frame
  that is going away has to hand its values over, and handing over is a copy.
- **The shadowing question dissolves.** The previous revision reasoned that a lambda
  parameter may not shadow a captured local, since a captured scope is a rung. With an
  explicit list the enclosing frame is *not* a rung — it is not in scope at all — so
  there is nothing to shadow. What remains is an ordinary duplicate-binding error when
  a parameter and a captured name collide, which is the same check a repeated parameter
  already gets.

It also retires a justification used elsewhere: the decision that a flag's value is
captured at assignment rejected the late alternative as "a closure in disguise, which
mesh has nothing else like." A capture list is not that closure — it captures values,
not scopes — so the flag decision keeps its ground rather than losing it: a *value*
should not carry unevaluated work, and that is now the only argument it needs.

**The same list goes on a definition** *(decided — `func name(…) with (…) { … }` and
`alias NAME with (…) = COMMAND`)*. A `func` or `alias` body is syntax evaluated at call
time, so a definition written in a loop cannot bake that pass's value — the loop that
wants one alias per host reads `$h` when the alias *runs* and finds nothing, and the
only way out was to generate a file and source it. That is the second half of the "no
`eval`, no dynamically-named `func`" edge, the half `alias $name = …` left open when it
made the *name* computable but not the body.

The capture list already means precisely the right thing, so this is a spelling
decision rather than a semantic one: the names are read where the definition runs and
copied into the stored function, and bound into the fresh call scope beside the
parameters. Nothing about an existing definition changes, because a definition with no
list captures nothing and reads its names late exactly as before.

The alternative was making an alias bake its body's interpolations *implicitly*. That
is a semantic change to every alias already written — `alias gh = grep "$env.HOME/.history"`
would freeze `$HOME` at definition — and it offers no way to say "not this one," where
an explicit list is opt-in per definition and reuses a rule readers already know. On an
`alias` the list goes **before the `=`**, because after the `=` every word belongs to
the command being aliased; on a `func` it sits where a lambda's does.

*Open — capturing under another name.* The list above captures a name as itself. The
obvious extension is `with (_w = $_want)`, letting the body use a shorter name or
capture a computed value, which would also make the list read like the `with FOO=1`
[block prefix](#the-environment) it shares a keyword with. Nothing is blocked by
leaving it out — the shorthand covers the motivating case — so it stays unbuilt until
something wants it.

**A `for` binding belongs to its loop** *(decided — a change from what runs today)*.
The loop variable is currently an ordinary assignment into the surrounding scope, so it
outlives the loop:

```
for _i in [1 2 3] { }
puts $_i                        # today: 3
```

That is the classic capture footgun in the making: every lambda written in the body
would read one shared binding and see only its final value, which is Go before 1.22 and
JavaScript's `var`. Both fixed it in the *loop* rather than in the closure, and so does
this: **the binding is fresh for each iteration and is gone when the loop ends**, so the
`puts` above is an unbound-variable error and a body that wants the value must capture
it — `func() with ($_i) { … }`, which then sees that iteration's value because the list
is evaluated per iteration.

A name the loop shadows is restored afterwards rather than clobbered, so a loop cannot
quietly overwrite a binding around it. The two decisions are independent: fixing the
loop is worth it on its own, and it is what keeps the capture list from being the only
thing standing between a reader and a wrong answer.

### Conditionals: `if` is an expression

`if` **yields a value** — it is an expression, not just a statement (Rust,
Kotlin, Nix). So the same construct that branches control flow also *produces*
the branch's value, which is what lets a value-returning function (the
[structured-return TODO](#functions) above) have a natural body and kills a
whole category of `x = $(if … )` scaffolding.

```
# statement position — run a branch for effect
if fzf:kind != false {
  bind-key ctrl-r fzf-history
} else if atuin:kind != false {
  atuin init mesh | source
}

# expression position — the taken branch's value becomes the result
glyph = if connected-remotely { "⇄" } else { "•" }
tag   = if $root { "[root]" } else { "" }
```

Decisions:

- **The condition is a bool or a command — and nothing else** *(decided;
  shipped)*. A boolean value (`$root`, a comparison like `$n > 0`, a `:has` test)
  branches on its truth; a bare command branches on its **exit status** (`0` →
  true), preserving the `if grep -q foo file { … }` reflex. *(Under the
  [status decision](#open-questions) a **`Status`** joins them, true iff its code
  is `0` — not a new truthiness, but the command arm's own subject named as a
  value, so `if grep(foo) { … }` and the bare form ask one question.)* Every other type is a
  **loud error** naming the comparison to write instead. The [predicate
  vocabulary](#requirements-carried-over-from-existing-configs) splits across
  both: the session predicates (`connected-remotely`, `inside-project`, …) are
  ordinary functions that slot straight into `if` with no `[ … ]` / `test`, while
  name resolution is the [`:kind` modifier](#modifiers) and so yields a value,
  compared explicitly (`if fzf:kind != false`).

  **Truthiness is settled, and the answer is that there isn't any.** Which world
  a condition branches on is decided by **where the subject is written** — command
  position means exit status — not by what type it evaluates to. So no value is
  coerced into a truth:

  ```
  if 0 { … }          # error: an int is not a condition; compare it (`… > 0`)
  if "" { … }         # error: a string is not a condition; compare it (`… != ""`)
  if $xs { … }        # error: a list is not a condition; test its length (`…:len > 0`)
  if $xs:len > 0 { … }  # the comparison, which is what you meant
  ```

  What this replaces was three different rules wearing one name, and they
  disagreed with each other: an **int** read as an exit status (`0` true — the
  inversion of every other language), a **string** read for emptiness *and*
  sniffed against the literal texts `"false"` and `"0"`, a **collection** read for
  emptiness. Together they made `if 0` true while `if "0"` was false — the same
  number, opposite answers, decided by type — and since `$(…)` yields strings,
  `if $(echo 0)` disagreed with `if 0` too.

  The case that forced it is `:len`. It returns a count, counts are ints, and ints
  read as statuses, so **`if $xs:len` fired on the empty list and stayed quiet on a
  full one**. No local fix helps: any rule that makes counts work breaks statuses,
  and vice versa, because they are different things that happen to share a type.
  Refusing both is what makes `$xs:len > 0` the thing you write.

  `and`, `or` and `not` ask the same question and refuse the same values, since
  they are boolean operators rather than a second truthiness system.
- **An assignment may *be* the condition** — `if lhs = rhs { … }`, the `if let`
  shape. The condition is true iff the RHS is **truthy** (a `false` / failed
  command / nonzero `Status` fails it — *a nonzero **int** does not, under the
  [status decision](#open-questions), since `return 5` is data with status `0`*)
  **and** its shape **fits** `lhs`; on true the
  names bind for the block, on false it skips and binds nothing. `lhs` may be a
  name (always fits) or a `[…]` [destructuring](#destructuring) pattern, so
  `if [one two] = $s:match(/…/) { … }` and `if line = gets() { … }` both test-and-bind
  in one step, RHS written once. Crucially, **pattern-fit is part of the test**: a
  shape or length mismatch (`[a b]` against a three-element list) makes the
  condition *false and skips* — it does **not** error. That is the deliberate
  contrast with a bare `lhs = rhs` statement, where the same mismatch is a loud
  assertion failure — the conditional form is "bind if it fits," the statement form
  is "it must fit."
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
  interactive brevity wins here.) ***Reopened*** — see
  [Open questions](#open-questions), which holds that the interactive-brevity
  reason is weak for a construct that lives in configuration files, and that the
  same question governs [`match`](#matching-match) totality. Lenient remains the
  shipped behavior until that resolves.
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
  *.md | *.markdown => markdown     # glob patterns, alternation with `|`
  *.txt             => text
  /^README/         => readme       # a /regex/ arm (slash-delimited)
  .git              => special      # a literal
  _                 => other        # `_` is the default (the old `*)` )
}

match $sig {                        # statement position; a block arm runs commands
  int  => { cleanup; exit 130 }
  term => { cleanup; exit 143 }
  hup  => { reload-config }
}
```

**Arm syntax — decided.** An arm is `pattern [if guard] => body`, arms are
separated by a **statement terminator** — a newline, or `;` on one line, the same
interchangeable pair as everywhere else in mesh, and **never a comma** — and the `=>` is
**mandatory**. The **body is either a value or a
`{ }` block**:

- `=> value` is a **value context** — a bare word is a **string** (`=> markdown` is
  `"markdown"`), `f()` is a call taken for its return value, `$v` / `[a b]` / `42` are
  themselves.
- `=> { … }` is a **block** — ordinary **statement context**, so a bare word *runs*
  (`=> { markdown }` executes `markdown`; note the *current* single-bare-word block
  rule below still reads that shape as a scalar in expression position), commands
  stream, and several statements are
  fine.

The `=>` is what terminates the pattern-and-guard, which is why it is required rather than
inferred: without it a guard expression would swallow the body
(`[verb ...rest] if $verb == "quit" 130` has no parseable boundary). One consequence,
accepted: adding braces around a **bare** word changes it from a string to a command
(`=> markdown` vs `=> { markdown }`) — a divergence from Rust, where the two agree.
Whether a *quoted* value is identical in both (`=> "md"` ≡ `=> { "md" }`) depends on the
still-open block-value rule: it holds under an implicit-tail rule, but under an explicit
value keyword the block form would be `=> { result "md" }`.

**`if` keeps block-only branches** — `if c { … } else { … }`, no arrow — so the terse
value form (`=> markdown`) exists only on arms. That asymmetry is deliberate and is
**exactly Rust's and nushell's**: branches are blocks of work, arms are a pattern→result
mapping, and the syntax reflects the difference. The mesh-specific wrinkle is that here the
two forms disagree on a bare word (`=> markdown` is a string, `{ markdown }` runs), where
Rust's `=> 1` and `{ 1 }` agree — so a bare-word *value* is arm-only; in an `if` branch you
quote it (`if $root { "[root]" }`). Both constructs share the same residual block-value
rule, so neither is worse off than the other for multi-statement bodies.

*(This form is **implemented**: `=>` is a token, an arm body is a value expression or a
block, and a missing arrow or a missing separator between arms is a syntax error. How a
`=> { … }` block yields a value in expression position is still the open value-production
question, so that part keeps its existing behavior — see the notes below.)*

Arm patterns, in one vocabulary:

| Pattern | Matches | Notes |
| --- | --- | --- |
| `foo`, `42` | a literal value | exact |
| `*.txt`, `foo*` | a **glob** | fnmatch — the string metacharacters of [Globbing](#globbing) (`* ? [] {} **`); the filesystem qualifiers (`(f)`, `size`, `age`) are expansion-only |
| `/re/` | a **regex** | slash-delimited; this is mesh's whole regex story (no separate `=~`) |
| `a \| b` | either | alternation |
| `1..=9` | a **range** | the `..` / `..=` from slices |
| `--force`, `--n=2` | a **flag** | exact, payload type included — `--n=2` and `--n='2'` are different arms |
| `--verb=n` | a flag, **binding** the payload | `n` is a binder, not a literal; `--verb=_` takes any payload — see the flag-pattern rule below |
| `_` | anything | the default; put it last |

Rules:

- **First match wins**, top to bottom; `_` is the catch-all and conventionally
  last. Whether non-`_`-exhaustive matches must be total is *(open)*; lenient
  was the position here until the [`if` question](#open-questions) reopened, and
  is the shipped behavior meanwhile — a `match` with no arm hit yields `""`, like
  a no-`else` `if`. ***Coupled***, with no independent lean left: the reopened
  [`if` question](#open-questions) is the same one wearing another keyword —
  must a value-producing construct cover every path? — so whichever way that
  lands, this lands with it, **for matches whose result is used**. A
  statement-position `match` discards its value, so nothing downstream can
  receive an empty and it stays outside the coupling either way.
- **It is an expression**: `x = match … { … }` binds the winning arm's value;
  in statement position the value is discarded and arms run for effect.
- **A literal arm compares totally, even where `==` refuses.** An arm is
  dispatch machinery, like `:dedup` and list `-`: under first-match traversal
  it needs an answer for every pair, so it uses the total equality those
  share rather than the `==` operator's refusals. The `Flag` type is where the
  two visibly part: `$x == "--help"` refuses on a flag, while a `match` with
  both a `--help` arm and a `"--help"` arm keeps working and takes the right
  one, since naming both arms is someone deliberately telling them apart.
  Stated here so the `(==)` in the table below is not read as importing the
  refusal.
- **A status matches an int arm** *(decided, not yet built)*, because that pair
  is genuinely equal rather than merely tolerated — see [Comparison across
  types](#comparison-across-types). So `match $sh.status { 0 => … }`, the
  spelling shell reflex reaches for, takes its arm instead of falling silently
  through to `_`. This is the one place the total equality above *gains* a pair
  rather than keeping one the operator refuses, and arms need no rule of their
  own for it: they already use `Value::eq`, so they follow automatically.
- **Regex captures**: on the *value* side this is **settled** — `str:match(/re/)`
  returns the groups (positional → list, named → map); see
  [Destructuring](#destructuring). A `/re/` **arm** does **not** *auto*-bind its
  groups *(decided — resolving the earlier open)*: a bare `/re/` arm is a pure
  yes/no predicate exactly like the `~` it mirrors (see the `~`/`match` note below),
  and to *capture* you go through `:match` explicitly — an `if`-binding
  `if [a b] = $x:match(/re/) { … }`, or a match over the capture result,
  `match $x:match(/re/) { [a b] => … ; false => … }` (a bare `[a b] = …` is *not*
  itself an arm — an arm is a pattern, an optional guard, `=>`, then a value or block).
  Auto-binding would smuggle invisible, position-fragile names into the arm body
  (Perl's `$1` / bash's `BASH_REMATCH`), the one implicit-value habit mesh's error
  model exists to refuse; keeping capture explicit leaves a single obvious rule and a
  clean split — `~`/`/re/`-arm answer *whether*, `:match` extracts *what*.
- **List-shape patterns** *(settled — see [Destructuring](#destructuring))*: a
  `match` arm may be a list pattern that **binds by position** — a bare element is
  always a **binder** (never a literal to match), with `_` to discard and `...rest`
  for the tail (`[a b]`, `[cmd ...rest]`). Note this differs from a *top-level* arm,
  where a bare word is a literal: inside `[ ]` you are destructuring, so `[start arg]`
  binds both. To *match* a specific element, use an arm **guard**
  (`[verb ...rest] if $verb == "quit"`). Richer element sub-patterns (a literal /
  glob / `/re/` element, or nesting) and **map-shape** patterns (`[k: v]`) stay
  **deferred** until the need is real.
- **Flag patterns bind their payload** *(decided, not yet built)*. A flag has three
  states and each gets its own spelling, so none of them has to be inferred:

  ```mesh
  match $a {
    --verb       => { set-verbosity 1 }     # the bare switch, no payload
    --verb="max" => { set-verbosity 9 }     # that exact payload
    --verb=n     => { set-verbosity $n }    # any other payload, bound as `n`
  }
  ```

  Arms are first-win, so the exact payload precedes the binder — a binder catches
  every valued flag and would shadow anything below it. `--verb=_` is the fourth
  spelling, the binder's discard form, for when the payload's presence is the
  whole question:

  ```mesh
  match $a { --verb=_ => { set-verbosity 1 } ; --verb => { usage() } }
  ```

  A bare `--verb` matches **only** the bare switch: `--force` and `--force=true`
  are different flags and stay so (see the `Flag` entry in `TODO.md`), so nothing
  here collapses the two. The bound payload **keeps its type**: the pattern
  `--n=v` binds the integer `2` against a subject written `--n=2`, and the string
  `"2"` against `--n='2'`. That is the point of binding rather than reading text
  back off the flag. (`--n=2` as a *pattern* is a literal, per the rule below —
  only the binder position is being described here.)

  **A bare word is a literal in a whole-value position and a binder in a
  sub-pattern position.** That one sentence covers both this slot and `[ ]`,
  replacing what would otherwise be two special cases: `--tag=main` binds for the
  same reason `[start arg]` binds both elements, and a top-level `main` arm is a
  string for the same reason it always was.

  **The binder takes only the slot's *string* case.** A bare payload word is typed
  exactly as any bare word is — `true` and `false` are booleans, a canonical
  integer is an int, everything else is a string — and **only that last case
  binds**. So `--n=2` stays the integer literal and `--force=true` the boolean;
  `--tag=main` is the one that becomes a binder. A quoted word and `_` keep their
  own readings, unchanged.

  That line is what keeps every typed payload spellable, and it is not a
  refinement for tidiness. **Quoting is a real escape for a string but not for a
  typed payload**: `--tag="main"` is the same value as the bare form, while
  `--force="true"` is a *string*-payload flag — a different value from
  `--force=true`, as `:repr` shows (`--force='true'` against `--force=true`). A
  rule that swallowed `true` into the binder would therefore leave an exact
  boolean-payload arm with no spelling at all. The string case is the only one
  quoting can give back, so it is the only one the binder may take.

  **Quoting is otherwise the escape mesh already teaches.** The [word-shape
  rule](#tests-and-comparisons) says a bare word in a match slot may
  be read as a pattern and quoting forces literal text; `--tag="main"` is that
  rule applied one level in. So the binder costs only the *unquoted* spelling of a
  literal **string** payload, never the ability to match one.

  The value slot takes a binder, `_`, or a literal for now. A glob, a regex or an
  alternation there (`--out=*.txt`, `--level=/^\d+$/`) is **deferred** under the
  same entry that defers richer element sub-patterns inside `[ ]` — and when that
  is lifted, `["quit" ...rest]` should become the literal element this slot
  already accepts, rather than the two growing separate rules.

  Binding in the arm is what makes this work on a subject that has no name:
  `match $args:get(0) { --verb=n => … }` needs nowhere to hang an extractor. For a
  flag held *outside* a match, **`:name` and `:value`** read the two halves
  (`--tag=v2` gives `"tag"` and `"v2"`) — `FlagValue` has carried both fields
  since the type landed with no way to read either, and `:flag` builds one with
  nothing to take it apart.

  **`:value` on a bare switch is an error**, not `""` and not a sentinel. A bare
  flag genuinely has no payload — that is the two-state distinction the type
  exists to keep — and [mesh has no null](#variables-and-assignment), so inventing
  an empty string here would be the silent-absence answer the language refuses
  everywhere else. Branching on the two states is the arm vocabulary above
  (`--verb` against `--verb=_`), which is why `:value` does not also need a
  soft form: by the time you are reading a payload you have already established
  there is one.

  **Reserving these two names retires any user modifier of the same name**, which
  is a real cost rather than a free pick. `func _s:name()` and `func _s:value()`
  are legal today; `modifier_definition_name_problem` refuses every name
  `is_builtin_modifier` knows (`repl.rs`:1566), so both declarations start
  reporting `` `:value` is a built-in modifier and cannot be redeclared `` the day
  this lands. That is the general cost of adding to the modifier vocabulary, not
  something specific to these two — recorded so the choice is made knowing it.

**`~` and `match` share one pattern vocabulary, but `~` is a strict subset** *(current
M3 behavior)*. For a **string** subject and a **glob or regex** pattern,
`match $x { P => … }` takes the `P` arm iff `$x ~ P` — that shared core is learned
once. But an arm does strictly more than a `~` RHS:

| Pattern | `match` arm | `~` RHS |
| --- | --- | --- |
| glob `*.txt`, regex `/re/` (string subject) | ✔ | ✔ |
| literal on any type (`match 7 { 7 => … }`) | ✔ (`==`) | ✗ — `~` needs a **string** left operand |
| range `1..=9` | ✔ | ✗ |
| alternation `a \| b` | ✔ | ✗ — `~`'s RHS is one glob/regex value |
| list-binding `[a b]`, `[cmd ...rest]` | ✔ | ✗ — `~` is a bool, binds nothing |

So `~` is the scalar, string-only slice of the arm grammar; `match` adds literal-on-any-
type, ranges, alternation, and destructuring.

**Which to reach for.** `if $x ~ P { … }` and a one-arm `match $x { P => { … } }` do look
alike, but the resemblance is confined to a **single test on the shared glob/regex
subset**. Both are expressions; the difference is what they produce and where they can sit:
**`~` is an infix operator that always yields a `bool` and nests anywhere an expression
goes** (a condition, an `or` chain, a postfix guard, a `match` arm's own guard), while
**`match` is a braced multi-arm construct that yields the taken arm's value** — of any
type — and can bind.

| Situation | Form |
| --- | --- |
| One test — or a bool to store, negate, or combine | **`~`**: `is_src = $f ~ *.rs`, `$a ~ *.x or $b ~ *.y`, `continue if $f ~ *.tmp`, `while $line ~ /^\s/ { … }` — and inside a `match` arm's own guard (`[cmd ...rest] if $cmd ~ git-* => …`) |
| Heterogeneous conditions | **`if`**: `if $p:exists and $n > 5 { … }` — arms all test one subject, so a chain of unrelated tests belongs in `if` |
| Several patterns against **one** subject | **`match`**: names the subject once, tests in order, has a default, and yields a value |
| Need to **bind** parts of the subject | **`match`** list arms (`[cmd ...rest] => …`), or an [`if`-binding](#conditionals-if-is-an-expression) |

Hence **no single-arm sugar** — but only for the patterns the two share: where `P` is a
glob or `/re/` against a string, `if $x ~ P` is the shorter spelling, so a one-arm `match`
buys nothing. For the patterns `~` **cannot express at all** — a literal on a non-string,
a range, or a list-binding — a one-arm `match` is exactly right (`match $xs
{ [cmd ...rest] => … }`), as is the corresponding comparison (`$n >= 1 and $n <= 9`) or
[`if`-binding](#conditionals-if-is-an-expression). The overlap is the same one every
language with both `if` and `match`/`switch` has.

**How an arm body yields a value** *(current behavior)*. A **`=> value`** arm is settled
and simple: the expression is evaluated in value context, so a bare word is a scalar
literal (`=> markdown` is `"markdown"`, `=> 7` is integer `7`), and in statement position
its value is discarded, reporting the value's status view.

A **`=> { … }`** block is the part still governed by the open value-production question,
and it behaves as it always has — by position, exactly like an `if` branch:

- **Statement position** — `match $x { … }` on its own line — runs the block as an
  ordinary block: commands execute and stream, *no* value, *no* capture. `*.x => { ls }`
  runs `ls`.
- **Expression position** — `y = match $x { … }`, or nested in another value expression
  — resolves the block to a value by its tail (`eval_value_body`): (1) a
  **value-expression tail** (`=> { "text" }`, `{ $v }`, `{ [a b] }`, nested `if`/`match`)
  yields that value; (2) a body ending in a **command** (`{ wc -l < $f }`, and
  `{ markdown }` — see the bare/quoted rule below) **runs and streams**, exactly as
  it does in statement position, and yields the **status** that command left — there
  is no implicit capture. To yield a string, quote it: `{ "text" }`; to yield the
  bytes, capture them explicitly: `{ $(wc -l < $f) }`.
  *(A function's value-return is **not** yet an expression context — a `match` as a
  function's last statement runs in statement position and the value is discarded;
  structured value-return / value-calls beyond `re(…)` are unbuilt.)*

**Spelling and arm grammar — decided.** The exploration weighed four levers; three are now
settled and one stays open:

1. **Shape: prefix `match $x { … }`** *(decided)*. Subject-first `$x match { … }` (Scala,
   C#) was the runner-up — it aligns with the infix, subject-first `~` and `:mod` — but
   `if` is mesh's own precedent for an expression-that-branches and it is prefix, and the
   two references for this construct (Rust, **nushell**) are prefix. The `~`/`match`
   infix-vs-prefix "asymmetry" then just reflects operator-vs-keyword, as with `==` vs `if`.
2. **Keyword: `match`** *(decided)*. `case $x { … }` was genuinely viable — Ruby's
   `case`/`when` is a value-returning expression, and reusing the shell keyword with brace
   grammar is the same "keep the word, fix the grammar" move mesh already made for
   `if`/`for`/`while`, so the "false familiarity" objection is weak. `match` wins on:
   mesh's arms are *patterns* (which even Ruby spells `case`/**`in`**), `match` is the
   cross-language pattern keyword (Rust, Scala, nushell, Python), it pairs with `~`, and
   with `=>` arms the whole construct then reads as Rust/nushell do. `switch`
   (statement-flavored) and `~~` (Perl **smartmatch**, deprecated for its type-dispatched
   unpredictability) are declined.
3. **`~` scope** *(**open** — the one lever still undecided)*. Keep `~` narrow (string vs
   glob/regex) or widen it toward the arm grammar. *Lean: narrow*, revisiting only
   **alternation** on the RHS (`$f ~ *.a|*.b`) as the extension that pays for itself. Full
   type-dispatch parity (Ruby's `===`) is rejected — it re-creates the smartmatch trap.
4. **Arm grammar: mandatory `=>`, body is a value or a block** *(decided — see
   "Arm syntax" above)*. Alternation is **`|`** (Rust, nushell, Python's `match`, Scala,
   OCaml, bash `case`, and regex all use it; it reads as *or*, where comma reads as a list
   and is already glob-internal alternation in `*.{md,markdown}`). Arms are
   separated by a **terminator** (newline or `;`), **not** comma-separated — a separator
   between arms is required, so `a => {} b => {}` does not parse. Declined alternatives: today's
   tail-coercion (the sharp-edge source), an arrow-free `pattern value` form (no boundary
   for guards), and an explicit `result`/`return` in *place* of `=>`.

   **Residual, and it is not match-specific:** a `=> { … }` block is statement context, so
   *how a statement-context block produces a value* — implicit tail expression, or an
   explicit `result`/`return` — is the same open question as for `func` bodies, and is
   tracked there (see [Functions](#functions) and the value-production item in
   [Open questions](#open-questions)). Whatever `func` does, arms do.

**`int → status` is gone — superseded twice over.** This paragraph used to record
"keep it": a bare int read as an exit code rather than data, its truthiness following the
status view rather than the number. That is **no longer true of the implementation or of
the design.** `0b107f6` dropped the `Integer` arm from `status_of`, so a returned int has
projected to status `0` since; and the [status decision](#open-questions) settles the
question deliberately, giving a status its own type and spelling (`status(N)`,
`return status N`) so that `return 5` is the integer five, successfully, with no residual
about "an int whose masked status is nonzero."

What the old paragraph got right is worth keeping, because it is the reason the
projection stays *total* rather than being abolished: external commands exit `0` for
success with no typed value to consult, so for `if X { … }` to mean "did X succeed"
whether `X` is `grep -q …` or a mesh function, success must be truthy on both sides. That
interchangeability is preserved — it is just carried by `Status` and `false` now, rather
than by every integer. Two live scraps this left, both pointed at their canonical homes:

- **Empty `""` / `[]` truthiness** — **closed** by
  [condition truthiness](#conditionals-if-is-an-expression) settling as *no truthy
  values*: a bare `if $xs` is an error whether the list is empty or not, so there
  is no emptiness rule left to decide. The question survives only for the
  **assignment-condition RHS** (`if xs = f() { … }`), which tests *presence* rather
  than truth — and there the answer follows from `false` being mesh's "no result":
  only `false` is absent, so `""`, `[]` and `0` all bind and take the branch. That
  also keeps `gets()`'s pinned contract, where a blank line must not end a read
  loop. A **`Status`** is the one addition the [status
  decision](#open-questions) makes here, and it is not an exception to the
  presence reading: a nonzero status is a *value-level failure* like `false`, so
  it takes `else` — but unlike `false` it still binds, being a result rather than
  an absence, so the `else` branch can read the code.
- **An explicit coded-failure spelling** *(deferred)* — any such value must stay a
  **channel-1** failure (a testable value) and so **cannot** reuse the name "error"
  (channel-2: fail-loud, no value, aborts); defining it touches the two-channel
  [error model](#error-handling). Not pursued here.

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
  `[[ $s =~ re ]]`, unified. The regex form is **unanchored** (first match
  anywhere, as bash `=~` and grep are); anchor with `^…$`. A glob, by contrast,
  matches the **whole string** (fnmatch), the same as a `/re/` wrapped in `^…$` —
  and `:match` shares the regex rule. On the RHS a leading-slash word is the regex
  only when its base (minus trailing `:` modifiers, so `/\d+/:i` counts) is a clean
  `/BODY/` (closing `/` final, no unescaped interior `/`); otherwise it is a path or
  glob, so both **relative** (`*.txt`) and **absolute** (`/usr/*/bin`, `/tmp/*`) globs
  are bare (full rule in [Quoting](#quoting-and-escaping)).
- **File tests** are the scalar cousins of the `:files`/`:f` filter modifiers.
  The type/permission axis is words: `$p:type` yields the `find -type` word
  (`file`/`dir`/`link`/…) so `$p:type == dir` is `-d`; `$p:exists` is `-e`;
  `$p:exec` / `$p:read` / `$p:write` are `-x` / `-r` / `-w`. (`-z`/`-n` are just
  `$s == ""` / `$s:len > 0`.) The **binary** file relations `-nt` / `-ot` / `-ef`
  (newer / older / same-inode) are the same comparison family as the
  [predicate qualifiers](#globbing) (`age < 1d`), spelled `$a:mtime > $b:mtime` and
  `$a:same($b)` rather than cryptic digraphs. Like `test`, these **dereference
  symlinks** — `:mtime`/`:atime`/`:ctime` and `:same` act on the link *target*, so a
  symlink and its target share an mtime and are `:same`; `:type == link` is how you
  ask about the link itself. A raw `$a:mtime > $b:mtime` requires **both** files to
  exist (strict absence errors on a missing operand); `-nt`'s quirk of treating a
  *missing* target as older is the rebuild idiom, written explicitly as
  `$a:exists and (not $b:exists or $a:mtime > $b:mtime)`. These ride on the **time model**
  *(decided, porting `age()`)*: `now()` and the file-time modifiers
  (`:mtime`/`:atime`/`:ctime`) return an **`Instant`**, and `Instant - Instant` is
  a **`Duration`** (`age = now() - $f:mtime`). A `Duration` is written with **suffix
  literals** — `500ms`, `3s`, `5m`, `2h`, `7d`, units up through **days** (no week or
  year — not fixed-length), compounding as `2h30m` — and **prints canonically**, so
  the prompt timer is `took $elapsed` with no `/1000`. Arithmetic is the closed set
  `Duration ± Duration`, `Duration × n`, `Instant ± Duration → Instant`, and
  `Instant - Instant → Duration` (`Instant + Instant` is an error). Division is
  **not** in the set — for a ratio, drop to an integer first with `:ms` / `:secs`,
  which **truncate toward zero** (`(now() - $t):ms` drops any sub-millisecond
  remainder toward zero); then
  `$a:ms / $b:ms` is ordinary integer division, so the time model needs no
  non-integer type of its own. (A [float](#arithmetic) exists for other reasons,
  and keeping `/` integer on two integers is what leaves this argument standing;
  `$a:ms * 1.0 / $b:ms` is the spelling when a fractional ratio is what you
  want.) A `Duration`
  is **signed** — `Instant - Instant` goes negative for a future instant (so a
  future-dated file's `age` is just negative, not an error or a saturated zero),
  rendering with a leading `-` (`-3s`). `Instant` and `Duration` are
  **nanosecond**-resolution internally, so sub-millisecond file-time differences
  still compare correctly (`$a:mtime > $b:mtime`, the `-nt` replacement); literals
  only reach down to `ms`, and canonical rendering stops at `ms` — any finer
  remainder is dropped from the *printed* form but kept for comparison and
  arithmetic. A `Duration`'s **canonical spelling** uses the largest units that fit
  with no zero components (`90s` → `1m30s`, `3000ms` → `3s`), bottoms out at `ms`,
  writes zero as `0s`, and prefixes a negative value's whole form with `-`
  (`-1m30s`). Any magnitude that rounds below the `ms` floor — including a wholly
  sub-millisecond duration like `500µs` — renders as `0s` too, and there is **no
  negative zero**: a value that renders as zero is always `0s`, never `-0s` or `0ms`.
  An **`Instant` has no canonical text form**: interpolating, `puts`-ing,
  or passing one to argv is a **loud error** — epoch-vs-ISO and the timezone are a
  guess, the same no-guess-at-the-boundary rule as an un-spread list — so render it
  explicitly with `$t:epoch` (integer seconds), `$t:iso` (UTC ISO-8601 with a
  literal `Z` suffix and exactly nine fractional-second digits), or
  `$t:format(…)`. A bare
  integer is **not** a
  Duration (the ms-vs-s footgun mesh kills), but the process boundary stays bytes, so
  an external `sleep 2` still passes `"2"` — the type governs only *in-shell* values.
  One literal grammar then unifies the glob `age < 1d` predicate, file-time
  comparisons, `retry --sleep 2s`, and the prompt's `took 3s`. *(TODO — **timezone /
  calendar handling** deferred: `Instant` parse and format (`$t:format("%F %T")`,
  `"…":datetime`, and the tz conversion behind `tz2tz`/`udate`/`utc2`) delegate to
  `date` for now; consider a native tz-aware datetime later, weighed against simply
  shelling out.)*
- **Combine** bools with the words `and` / `or` / `not` (`if $a:exists and not
  $b:exists { … }`). These join *values*; the byte-stream **command** chains
  `&&` / `||` (run-next-on-success/failure, by exit status) are kept separately
  and unchanged — two different jobs that bash blurs.

So `case` → `match`, and the everyday `[[ … ]]` jobs map to a comparison, a `~`
pattern-match, a file-test modifier, or an `and`/`or`/`not` of those — no
special `[[` context, and none of its word-splitting quirks. The binary file
relations (`-nt`/`-ot`/`-ef`) are settled above as `$a:mtime > $b:mtime` and
`$a:same($b)`. Regex **captures** (bash's `BASH_REMATCH`) are settled too: they go
through the value-side `:match` extractor, and a `/re/` `match` arm does **not**
auto-bind (see [Matching](#matching-match)) — so `~` stays a pure predicate.

#### Comparison across types

`==` and `!=` **refuse** when their two operands are different types, rather than
answering `false` — with one **declared** cross-type pair, a status against an
int:

```mesh
1 == "1"           # error: cannot compare an int with a string
status(0) == true  # error: cannot compare a status with a bool;
                   #        a status is already a condition, so test it directly (`if … { }`)

$sh.status == 0    # true when the command succeeded — the one pair that compares
```

A silent `false` is unfalsifiable at the call site: the writer cannot tell "these
types do not meet" from "these values are genuinely unequal", so the answer looks
like data when it is really a category error. [`if 0`](#conditions) already
sets the loud precedent for a question mesh declines to guess the meaning of.

`$sh.status == 0` is the case that forced the shape of the rule. It is the
spelling shell reflex reaches for; a quiet `false` there is *always* wrong and
*never* says so, and an earlier draft answered that by making it an **error**
naming `:code`. Comparing the code is the better answer — the reflex is not a
category error, it is the obvious reading of a value whose canonical projection
is exactly that integer, so mesh takes the reading instead of teaching a longer
spelling for it. Where a pair genuinely has no reading, the refusal stands. See
[Why `Status` compares to an int](#why-status-compares-to-an-int-and-to-nothing-else)
for why exactly one pair opens and the other stays shut.

A **styled value and a plain string are one type** here, the single grouping the
rule keeps, because a styled value has to behave exactly as its text. The kinds
are the diagnostic's own type names, so the message can never read "cannot
compare a string with a string".

**The refusal lives in the operator, not in equality.** This is forced, not
stylistic. `Value`'s equality is what [`:dedup`](#modifiers) (a hash set), `in`,
and [`match`](#matching-match) literal
arms are built on, and each of those can only accept a bool — a fallible equality
would have nothing to hand them. (Map keys used to be listed here as a fourth
case "whose `Hash` must agree". They are not one: a map's keys are **text**, so
they never consult `Value::eq` at all — see the note below.) So the refusal is scoped to the **top-level
operand pair of `==` / `!=`**, and everything beneath it stays total:

```mesh
[1] == ["1"]                # false — nested, not an error
1 in [1, "a"]               # true;  "x" in [1] is false
[1, 1, "1"]:dedup:len       # 2 — `1` and `"1"` are distinct, not a report
match "x" { 0 => … }        # skipped, not an error — the arm just does not match
```

**The status/int pair is not scoped that way, and that is the whole difference.**
It is not a refusal the operator declines to propagate — it is an *equality*, so
it lives in `Value::eq` and everything built on that agrees with it for free:

```mesh
$sh.status == 0                                  # true on success
match $sh.status { 0 => "ok" ; _ => "failed" }   # takes the `0` arm — same equality
[status(0) 0]:dedup:len                          # 1 — one value
0 in $sh.pipestatus                              # true if any stage succeeded
```

**This is why comparing beat refusing.** The seam this section used to record was
that `$s == 0` reported while a `0` arm against the same status quietly did not
match, and the obvious repair was to make the arm report too. It was the wrong
repair, for a reason worth keeping written down: an arm is dispatch under
first-match traversal, so a refusal there **aborts the whole `match`** at the arm
it reaches, including arms a later subject needs. A status is an ordinary value —
`status(N)` is a public constructor — so a collection may hold statuses beside
ints, and

```mesh
for x in [status(2) 1] {
  match $x { status(1) => … ; 1 => … ; _ => … }
}
```

runs today and would have died on its first iteration, at the `1` arm the
*second* iteration needs. Refusing more places is not the free tightening it
looks like: it accepts strictly less, which means no *new* program becomes valid
and existing ones stop working. Making the pair *equal* closes the seam from the
other side, and closes it everywhere at once, with nothing foreclosed.

A `Flag` is the contrast that shows the rule is about readings and not about
strictness. It keeps refusing every non-flag comparison, because a flag has no
projection into a string — `--tag=v2` and `"--tag=v2"` are the same bytes with
different meanings, which is the asymmetry the type exists to preserve. And arms
stay total there for the reason above: flags arrive interleaved with strings in
argv, so

```mesh
for a in [--force out.txt] {
  match $a { --force => … ; "out.txt" => … }   # both arms wanted; both reached
}
```

is the intended use, and both arms have to stay reachable.

##### Why `Status` compares to an int, and to nothing else

A `Status` **admits two projections**, and both are reachable as values today.

| projection | spelling | yields |
|---|---|---|
| its code | `$s:code` | an int |
| its success | `not not $s` | a bool |

Equality cannot respect **both**, because `==` is transitive and holding both
gives

```
0 == status(0) == true    ⟹    0 == true
```

which is exactly the proposition `if 0` refuses to let you even ask. An earlier
draft concluded from this that equality must respect **neither**. That does not
follow, and it was the wrong call: respecting exactly **one** breaks the chain
just as effectively, because the link that would bridge the two ends stays
refused. `0 == true` is never derivable if `status(0) == true` is never true.

So **equality respects the code**:

```mesh
$sh.status == 0    # true when the command succeeded
status(5) == 5     # true
status(0) == true  # error: cannot compare a status with a bool;
                   #        a status is already a condition, so test it directly (`if … { }`)
```

**Which projection is not a matter of taste — only one of them is legal.** The
code is **injective**: distinct statuses have distinct codes, so equating a status
with its code equates nothing that was not already equal. Success is **lossy** —
all 255 failing codes collapse to a single `false`. Respecting *that* would give

```
status(1) == false == status(2)    ⟹    status(1) == status(2)
```

and `status(1) == status(2)` is **`false`** today, correctly: they are different
statuses. So the success reading does not merely raise a cross-type awkwardness,
it breaks equality *inside* the type. It is disqualified, not deprioritized.

That generalizes past `Status`, and is the rule to apply to any type added later.
It has to be stated as **equivalence classes**, not as pairs, because `==` is
transitive and a pairwise rule cannot stay so:

> **Equality partitions the types into classes. A type may join at most one
> class, and only through a *lossless* projection into it — one mapping distinct
> values to distinct results. Within a class `==` compares by that projection and
> is total; across classes it refuses.**

A lossy projection can never carry equality, because collapsing two values into
one image makes them equal to each other by transitivity. That is the same
argument that rules out respecting *both* of a status's projections, applied one
level down, and it is why the rule needs no judgment about which reading is
"canonical".

**Why classes and not pairs.** "A status compares to an int" is a pair, and pairs
do not close under transitivity. Today the **numeric** class has two members,
`Integer` and `Status`, so the two formulations agree — but a pairwise rule
breaks the moment a third type joins, because `a == b` and `b == c` would then
force `a == c` while the pair rule leaves it refused. Non-transitive equality is
not merely untidy: it cannot satisfy the `Eq`/`Hash` contract
[`:dedup`](#modifiers) is built on, since `:dedup` would keep or drop elements
depending on the order it met them. Stating the rule as a class makes any later
member fall out instead of needing a patch, and says what a candidate has to
show: a lossless projection into the class.

Membership is what a new type must argue for, and the bar is the projection: a
type joins the numeric class by having a lossless projection **to a number**, and
otherwise starts a class of its own. A `Flag` has none — its text form is not a
projection but a rendering, and `--tag=v2` against `"--tag=v2"` is the very
distinction the type exists to keep — so it is alone in its class and refuses
everything, which is exactly what it does today.

**Equality is one relation; the seam it used to leave is gone.** Because
[`match`](#matching-match) arms, [`:dedup`](#modifiers) and `in` are all built on
the same total equality, making `status(0) == 0` true makes all of them agree at
once:

```mesh
match $sh.status { 0 => "ok" ; _ => "failed" }   # takes the `0` arm on success
[status(0) 0]:dedup:len                          # 1 — one value
```

That is the whole point of choosing this over refusing more places. The reflex
spelling `match $sh.status { 0 => … }` stops being a silent miss by *working*,
not by reporting — so no diagnostic has to be written, no heterogeneous `match`
is foreclosed, and no program that runs today stops running.

**The cost, stated rather than discovered:** `Value::eq` and `Hash` must agree
with the operator or the seam merely relocates, so a `Status` hashes as its code
and `[status(0) 0]:dedup` yields one element. That is a wider change than the
operator alone, and it is the part of this entry that is not confined to `==`.
**Map keys are not affected**, and are not evidence either way: a map's keys are
**text** (`Value::Map` is `Vec<(String, Value)>`, and `[status(5): "a"]` reprs as
`['5': 'a']`), so a status and its code already key the same entry and never
consulted `Value::eq` to do it.

**The residual oddity, named rather than hidden.** `if $s { true }` yields the
bool `true`, yet `$s == true` reports — a status can be *turned into* a bool but
not *compared* to one. That reads strangely at first, and it is the honest price
of the rule above rather than an oversight. The resolution is that **truthiness
is a question, not a projection of the data**: `if $s { … }` asks whether the
status succeeded and throws the code away, which is exactly the lossiness that
disqualifies it from equality. Converting explicitly is always allowed — `not not
$s`, or the `if` above — because a conversion the writer spells out cannot make
two distinct statuses silently equal; an implicit one inside `==` would.

mesh already splits a value's readings this way, and deliberately: `if n` runs
`n` and branches on its **status**, while `if n()` evaluates the call and reports
that an int is not a condition — the same function, two answers, chosen by
[position](#conditions) rather than by type.

**Comparing anything to `true` is a smell in any case.** `$x == true` on a bool
is the redundant idiom every linter flags; `if $x` is the spelling. `true` and
`false` earn their keep as *values* in the places a condition cannot go — a
stored predicate result (`is_src = $f ~ *.rs`), a flag payload (`--force=true`,
which is a `flag<bool>` and distinct from bare `--force`), a `func` that returns
one — and none of those want an equality test against a literal `true`. So the
refused half of this rule costs a spelling nobody reaches for, which is why the
diagnostic points at `if … { }` instead of naming a comparison.

*TODO — ordering (`<` `<=` `>` `>=`) is not settled by this entry.* It still
errors across types, which leaves equality and ordering answering type mismatch
in the same voice for the first time, but the fall-through to lexicographic text
comparison is a separate defect (see the TODO on numeric text).

### Error handling

mesh keeps **two distinct failure channels** and deliberately does not merge them
the way bash does (into "empty string, exit 1"):

- **Value-level failure** — a `false`, a nonzero **`Status`**, or a command's exit
  status. *(It was "a nonzero `int`" before the
  [status decision](#open-questions); an int is data now, and a status has its own
  type.)* This is *not* an interruption: it is a **value** you branch on (`if`,
  `while`, `&&` / `||`, `and` / `or` / `not`). It is the whole of the
  [result/status model](#functions) — failure here is signalled by a `false` /
  nonzero `Status` / command-status, **never** by the *shape* of a value.
- **Errors ("fail loud")** — a value the code *required* is absent or ill-typed:
  a destructure length mismatch (`[a b c] = two_items`), an out-of-range index
  (`$xs[99]`), a bare [`:match`](#destructuring) miss, undecodable text where text
  is required, a type error. These produce **no value** — they **abort the current
  statement** and surface loudly. They live *outside* the value/status model: not a
  `false` you might accidentally test as truthy, but an interruption you can't miss.

The split exists because "the command found nothing" (channel 1 — normal, testable)
and "the code asked for something that isn't there" (channel 2 — a bug) are
genuinely different, and collapsing them is the source of a whole class of silent
shell bugs.

**Strict by default, soft by opt-in.** Fail-loud is the *default*; every strict
operation that can be legitimately "maybe absent" has a **soft twin**, and *which
construct you write* is how you declare whether absence is a bug or expected:

| Intent | Strict — errors (channel 2) | Soft — yields a value (channel 1) |
| --- | --- | --- |
| bind N names from a list | `[a b] = xs` | `if [a b] = xs { … }` — a miss skips |
| a captured group | `[x] = s:match(/re/)` | `if [x] = s:match(/re/) { … }` |
| index an element | `$xs[i]` | `$xs:get(i, default)` — total, never errors |
| a map value | `$m.key` | `$m:get(key, default)` |
| read a line | — | `gets()` → `false` at EOF |
| a branch's value | — | `if cond { v }` → `""` when false |

So absence is loud when you **asserted** the value is there (a bare bind, a direct
`[i]`) and quiet when you **asked whether** it is (`if`-binding, `:get`, `gets`, a
no-`else` `if`). You never get bash's silent-empty-*by-default*; softness is
explicit. The soft index accessor is the existing two-arg [`$xs:get(i,
default)`](#arrays-lists) rather than a `:get(i)` that returns a bare `false` or a
`:get():default()` chain — deliberately, because the two-arg form does the bounds
check *internally* and so can still distinguish "element `i` is genuinely `false` /
`""`" from "there is no element `i`," which a returned-sentinel chain cannot. That
is the same no-null reasoning as everywhere else: don't let one value stand in for
both "empty" and "absent."

**`if` with no `else` is a soft form, not a suppressed error.** A false condition
is a normal outcome, not a failure, so `tag = if $root { "[root]" }` yielding `""`
when not root is the *soft channel producing the "nothing" value* — exactly
parallel to `gets()` producing `false` — and is consistent with fail-loud, which
governs only *required* positions. The residual edge is stated honestly: `""`-as-
nothing is indistinguishable from a real empty string and flows downstream under
[no-null](#variables-and-assignment), so a no-`else` `if` hands you a silent empty
that a destructure would refuse. That is the accepted cost
of the terse one-liner ([Conditionals](#conditionals-if-is-an-expression),
"Decided: lenient"); the only lever to close it — requiring `else` in *binding*
position — was weighed and declined for ergonomics. ***Reopened***, and this
paragraph used to say "the one place", which was wrong: an unmatched
[`match`](#matching-match) yields `""` too, and so does a function with
[no expression to yield](#functions). See [Open questions](#open-questions).

**An ambiguous spelling is an error.** Where one spelling has two genuinely
plausible readings, mesh refuses rather than picking a winner. The standard the
diagnostic aims for is to name the spelling that says each reading outright —
`if $xs` errors naming `$xs:len > 0` — though not every shipped message meets
it yet: the option-value report below states its requirement without suggesting
the rewrites, which is diagnostic polish still owed, not a design change. The
rule itself is the refusal. It keeps being reached independently
rather than having been laid down up front — it is why a condition must be a
bool or a command ([Conditionals](#conditionals-if-is-an-expression): `if $xs`
on a list is an error naming `$xs:len > 0`, not a length test, alongside `if 0`
and `if ""`); why an option value that evaluates to anything other than one
string is reported rather than joined or dropped (`f(--tag=*.txt)` — raised on
mikelward/mesh#361); and why comparing a flag to its own text form refuses
rather than answering `false` *(built — the `Flag` type entry in `TODO.md`)*:
the string was written *because* someone believed it was the
flag, so a quiet `false` reads as "not that flag" when the truth is "wrong
question."

Two lookalikes are **not** instances, named so the rule is not overclaimed.
`007` is not one: mesh *picks* — it is the string `007`, and it binds, travels,
and runs as a command ([Arithmetic](#arithmetic)); the only error is `007 + 1`,
which is the ordinary "a string is not a number" rule every string already
follows. A glob that matches nothing is not one either: it is `[]`, a chosen
answer rather than a refusal.

The rule has a cost, and making it explicit is the point of writing the rule
down: every refusal is a spelling somebody has to write differently, and the
flag-equality decision shows the sharper version, where an operator that could
not previously fail becomes fallible. So the rule earns its place only where
the two readings are genuinely both plausible. It is not a license to refuse
anything merely unusual — refusing the unusual is just a smaller language.

One scoping note, so this principle and the flag decision are not read as
contradicting. The rule decides *whether* a refusal exists; it does not fix
where the line is drawn once one does. The flag decision refuses **every**
flag-against-non-flag comparison — `$x == 7` as readily as the text-form
pair — not because `7` is confusable with a flag but because "a flag compares
to flags" needs no per-pair confusability judgment, where a narrower refusal
would have to remake that judgment for every type added later. The genuinely
ambiguous pair is what *earns* the refusal; the type boundary is where the
line is cheapest to hold.

**Recovery — the shell contains errors at interactive boundaries.** A channel-2
error has to land somewhere; the rule is where:

- **Interactive line** — the error aborts that line, prints, and returns to a fresh
  prompt. The session never dies.
- **`source FILE`** — a *parse* error rejects the whole file (none of it runs, so a
  bad rc can't leave a half-defined config); a *runtime* error aborts the file at
  that point. Whether that error is then **contained or propagated depends on
  interactivity**, not on `source` itself: in an **interactive** shell it is
  contained — surfaced, and the shell keeps running so a broken `rc.mesh` never
  bricks your session — whereas in a **non-interactive** shell it **propagates** as
  an uncaught channel-2 error and follows the batch rule below (the sourcing
  script fails hard; subsequent deploy/mutation commands do *not* run). Containment
  is an interactive affordance, never a blanket swallow.
- **Prompt / hook / completion callback** — the shell **catches** the error at the
  dispatch boundary, reports it (above the fresh prompt), and continues with a
  degraded result — that one prompt segment is dropped, not the whole prompt. A
  buggy config *shows* its bug without bricking interactivity: fail-loud and
  keep-running at once. (This boundary-catch is interactive-only for the same
  reason; a hook firing in a non-interactive run propagates like any other error.)
- **Script / `-c` / non-interactive** — an uncaught error exits nonzero (the batch
  contract), so automation still fails hard. This is the rule a propagated
  sourced-file or hook error lands in.

*(Open — the catch question: whether mesh also exposes a **user-facing** recovery
form — a `try` / `catch`, or an Elvish-style `?(…)` capture that converts a
channel-2 error into a channel-1 value — for the cases with no soft twin (a type
error, div-by-zero, undecodable text), or whether the strict/soft pairs plus the
boundary-catch above suffice for the MVP. Leaning: ship the boundary-catch and the
soft twins, **no** user `try` / `catch` in the MVP, since interactive use rarely
needs to programmatically recover from a genuine bug; revisit for scripting.)*

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
for line in $(git status --porcelain):lines {   # the split is spelled — safe
  …
}
for host, addr in $known_hosts {           # a map yields key, value pairs
  puts "$host is $addr"
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
- A **here-document** `<< END … END` **interpolates** by default — `$var` and the
  `"…"` escape set apply, as inside double quotes — and a **quoted delimiter**
  `<< 'END' … END` makes it **raw** (no interpolation, no escapes), the bash
  convention. The quoted-delimiter form is mesh's raw **both-quote-kinds** string: it
  holds `'` and `"` freely with no escaping. Using a heredoc as a **value**
  (`re(<< 'END' … END)`, `x = << END … END`) rather than a command's stdin is still
  to be specified (see [`TODO.md`](TODO.md)); the interpolate-unless-quoted rule
  applies to both uses.

**The last stage of a pipeline runs in the current shell** *(decided — bash's
opt-in `lastpipe`, unconditional here)*. Every stage but the last runs in its own
forked process, which is what makes a pipeline concurrent. The **last** one, when
it is something the shell runs itself — a builtin, a function — runs in the shell
instead, with the incoming pipe on its stdin for the length of the stage and the
shell's own descriptors put back after. So a binding it makes **outlives the
pipeline**:

```mesh
cmd | gets line     # `line` is set afterwards, not lost to a subshell
```

That is the fix for the bash defect in [What mesh avoids](#what-mesh-avoids):
`seq 3 | while read x; do n=$((n+1)); done` leaves `n` at `0` in bash because the
loop ran in a subshell. Automatic rather than an opt-in `shopt`, since a
shell-visible binding is the behavior people expect and the subshell is the
surprise.

**Not under job control**, which is the condition bash puts on `lastpipe` too. An
interactive pipeline puts its forked stages in a process group of their own and
hands that group the terminal — the shell is not in it. Reading the pipe in the
shell would then leave `cat | gets line` unstoppable: Ctrl-Z stops `cat` and not
mesh, and mesh sits blocked on a pipe that will never reach EOF, with no prompt
and no stopped-job record. So the last stage runs here exactly when the shell
keeps the terminal, and forks as it always did when it does not. The two are the
same condition, so they cannot drift apart.

Two stages keep their own process. A **backgrounded** pipeline has no
foreground last stage — the shell is not waiting for it, so there is nothing to
run here — and **`exec`** is asking to spend a process on its replacement, which
must not be the shell's: `cmd | exec prog` is observably `cmd | prog`, so
replacing the shell there would end the session for nothing. A **function
wrapping** `exec` cannot be spotted that way — a body cannot be asked in advance
what it will do — so reaching `exec` while standing in for a stage is a **loud
refusal**, the same answer `exec` gives inside a `$(…)` capture and for the same
reason: the process is already committed to being something else. An **external** last
stage forks as it always did; it needs a process to `exec` into. Status and
[`$sh.pipestatus`](#variables-and-assignment) are unaffected either way, and an
`exit` in the last stage is still that *stage's* exit, reported as a status.

*(open: `noclobber` and the `>|` override; whether `&>>` append-both is worth a
spelling.)*

**`exec` replaces the process image** *(decided — porting `autosession`, `logexec`)*.
`exec CMD …` replaces the current shell process with the command — the standard
`exec(2)` hand-off — so a dispatcher/wrapper (`autosession` → `exec autotmux …`,
`logexec` → `exec "$0".distrib`) leaves no parent shell behind: ordinary invocation
of an **external executable** runs a child, while `exec` *becomes* that external.
`exec` accepts only external executables; functions and built-ins have no process
image with which to replace the shell. (`exec` with only redirections and no
command applies them to the current shell, bash's `exec >log`.)

**Per-stream tty tests** *(decided — porting `confirm`)*. `$sh.interactive` answers
"is this an interactive shell," but a function sometimes needs "is *this stream* a
terminal" — `confirm` guards on `test -t 0 && test -t 2`. That is **`$sh.stdin:tty` /
`$sh.stdout:tty` / `$sh.stderr:tty`** — each a bool, the `test -t N` replacement,
under the `sh` namespace (a bare `$stdin` is an ordinary user variable under the
two-reserved-names rule).

*(TODO — **output process substitution `>(cmd)`**. The input form `<(cmd)` and
explicit fds / dup / close are settled above; the output form (`filter`'s
`3> >(cmd)`) is not — decide whether to add it.)*

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
$sh.jobs:values:filter(func(j) { $j.state == running })
```

`state` is `running` / `stopped` / `done`; `status` fills in when a job finishes
— a **`Status`**, for the same reason `$sh.status` is one (see the [status
decision](#open-questions)): `wait $j; return $j.status` is the natural way to
forward a job's failure, and an int there forwards the *number*, successfully.
It is `""` until the job finishes, which is the empty-value rule, not a null.

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

A **`jobdone` hook** fires alongside that notice, once per finished job, taking
`id`, `command`, and `status` — a **`Status`**, per the [status
decision](#open-questions), so `if not $status { … }` is the test rather than
`$status != 0` — see `docs/REFERENCE.md`. It runs where the notice
is printed rather than the instant the job ends, so it carries the same timing:
a job that finishes while a line is being typed is reported once that line is
submitted.

*(deferred past the spike: the fuzzy `%?string` (substring) reference, and
per-stage `pipestatus` on a backgrounded pipeline. Terminal plumbing — process
groups, `tcsetpgrp`, `SIGTSTP`/`SIGCONT` — is implementation, not surface.)*

### Signals

**Interactive defaults** — the shell owns these at the prompt. The *keyboard*
signals never end your session; only a lost terminal (SIGHUP) does:

- **`Ctrl-C` / SIGINT** — at the prompt, **abandon the current input** and draw a
  fresh prompt (never exits the shell). While a foreground command runs, SIGINT
  goes to *that* [job](#job-control)'s process group; the shell stays up and the
  next prompt shows its interrupted [status](#variables-and-assignment).
- **`Ctrl-D` / EOF** — `delete-char` on a non-empty line, as in bash: it deletes
  the character under the cursor, and at the end of a line there is none, so
  nothing happens. It means EOF only on an **empty** line, and even then exits
  only when the input buffer behind that line is empty too — with a block or
  heredoc still open it does nothing, so a stray `Ctrl-D` can't drop you
  mid-construct. Discarding a buffer is `Ctrl-C`'s job. An
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
  (as bash does). (`disown` exempts a job from this HUP; `disown -h`
  keeps the job in the table and exempts only the hangup.)

**User handlers are keyed hook maps, not bash's `trap`.** `$sh.signal.<NAME>` is an
insertion-ordered map of named callables — the *same shape* as `$sh.preprompt` and
the other [hooks](#hooks-and-the-prompt), so it is re-source-safe and composable,
with no new `trap` builtin:

```
$sh.signal.INT.note  = func() { puts "interrupted" }
$sh.signal.TERM.save = &save-state                 # by name
$sh.signal.USR1.reload = &reload-config            # a command/function, late-bound
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

Two read-only entries describe **what is being evaluated**, which `$sh.name`
(bash's `$0`) cannot answer because it never changes on `source`. **`$sh.origin`**
is the input's origin — `script` / `sourced` / `command` (`-c`) / `stdin` (`-s`) /
`interactive` — kept **orthogonal to interactivity**, since `mesh -i script.mesh`
is interactive *and* a script and that stays [`$sh.interactive`](#variables-and-assignment).
**`$sh.source`** is the path of the file being evaluated, defined for the file
origins (`script` / `sourced`) and empty for the rest. Together they replace bash's
`${BASH_SOURCE[0]}` and its `[[ "${BASH_SOURCE[0]}" != "$0" ]]` idiom, which
becomes the direct `if $sh.origin == script { … }`.

*(decided) `$sh.source` reports the **innermost** file rather than a stack.* "Where
am I" has one answer, which is what locating a sibling needs; a startup file is a
sourced file and reports itself the same way. A stack, if it is ever wanted, is a
separate `$sh.sources` and not a reinterpretation of this one.

*(decided) `return` leaves a sourced file; `exit` leaves the shell.* A `return`
ends the innermost unit with an **invoker to return to** — a function, or a sourced
file, whose `source` then reports the returned value's status; a bare `return`
carries the last status, as a bare `exit` does. A script, a `-c` string, and a
typed line have no caller, so `return` there stays an error. `exit` always ends the
shell, from a sourced file included, because `source` runs in *this* shell. Without
the `return` half there is no way to leave a sourced file early — `exit` would take
the session with it — which is exactly what a config-file guard needs.

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
  - **`pwd`** — the working directory. The shell **maintains the logical cwd
    itself** (updated by `cd` / autocd), so `pwd` reports *that* shell-owned value —
    validated against the real directory and recomputed if a stale or forged
    `$env.PWD` has diverged, so `pwd` can't lie. Run bare it **prints** the path; the
    **value call `pwd()` returns** the same validated cwd as a string value — so
    `pwd():ancestors` and `style(pwd(), fg: blue)` read the authoritative path, never
    the raw `$env.PWD`. **`--physical` / `-P`** calls `getcwd` for the symlink-resolved
    path.
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
    `$sh.options.autocd = false` disables it.
- **I/O**
  - **`puts [args…]`** — one order-preserving rule: **render each argument to
    text** — a scalar as itself, a **list** as its elements joined by newlines (a
    list *is* a sequence of lines), a **map** as `key: value` entries joined by
    newlines; a value with **no canonical byte form** — an `Instant`, a `regex`, a
    stream handle — is a **loud error** here, exactly as at the argv boundary above,
    never a guessed rendering — then **join the arguments with a single space** and append a trailing
    newline. A **nested** collection keeps that rule and moves down a level: under
    a map key it goes on the lines below, indented two spaces, and as a list element
    it takes a `- ` bullet. The bullet goes only where the ambiguity is — a scalar
    element needs no marker because the line break already separates it, while a
    nested collection's line breaks are *inside* it, so `[[1 2] [3 4]]` would
    otherwise print exactly as the flat `[1 2 3 4]`. Depth is **not capped**;
    indentation is what makes depth readable. So `puts $env` prints as an ordinary
    map, its path-type entries as indented blocks, with no rule of its own. The
    result reads like YAML and is deliberately **not** YAML: nothing here quotes or
    escapes, so a scalar holding a newline — or one that starts with `- ` — renders
    ambiguously. That is the standing trade for output meant to be *read*;
    [`:repr`](#modifiers) is the form that survives a round trip. So `puts a b` → `a b`, `puts $(ls)` → one file per line, and a mixed
    `puts head $xs tail` is fully defined by that rule. `puts` can render rich values
    because it is a **built-in** on real values — an *external* command still needs
    bytes (spread or [`:join`](#spread--flattening)). It takes **no flags** — none of
    `echo`'s `-e` / `-n` reinterpretation, since escapes are resolved by the
    [string literal](#quoting-and-escaping).
  - **`print [args…]`** — identical, but with **no trailing newline** — for partial
    lines and hand-built prompts. The `puts` / `print` pair replaces `echo -n`,
    keeping both flag-free.
  - **`gets [--nulls] [var]`** — read one line from stdin into `var` (trailing newline
    stripped) and return that line as its value. **At EOF it returns `false`**
    (whose [status](#variables-and-assignment) is `1`) and leaves `var` unchanged,
    so `while gets line { … }` terminates. An empty line still reads as a truthy
    `""` — only EOF is `false` — so blank lines don't end the loop. With no `var`
    it just yields the line (or `false`). **`--nulls` reads a NUL-terminated item
    instead** — the read a `find -print0` stream needs, where a newline inside a name
    is data and a line read would tear the name in half. The separator is *named*
    rather than passed as a character because `\0` is deliberately not one of the
    [escapes](#quoting-and-escaping) — a NUL crosses neither `execve` nor the
    environment — so a general `--delimiter=CHAR` could not spell the one delimiter
    this is for; `--nulls` is what the [`:nulls`](#modifiers) split modifier already
    calls it, and the two stay one vocabulary. The delimiter is a *terminator* either
    way, so a final item without one is still an item. Both spellings take the flag —
    `gets --nulls name` and `name = gets(--nulls)` — since the value form is the
    composable one, and withholding it there would make the spelling you reach for
    the one that cannot read the stream the flag exists for.
- **Formatting** — **`style(text, fg: name, bg: name, bold: bool)`** produces a
  [styled value](#hooks-and-the-prompt) — for the prompt, and for `puts`/`print`,
  the other renderers. It must be a built-in because a structured return value
  can't come from an external command, and a **value call** (`style(…)`, parens
  attached) because a command position yields a status: a bare `style …` runs it as
  a command. Colors are the sixteen ANSI names (`red`, `bright-blue`, `grey`/`gray`
  for bright black); 256-color and truecolor wait on a spelling for the value and a
  downgrade rule for terminals that can't show them.
- **Vars / env** — `export`, `unset`, `global`, and `source FILE` to (re-)load a
  file — re-sourcing your rc is safe because [hooks are keyed](#hooks-and-the-prompt).
- **Jobs** — `fg`, `bg`, `jobs`, `kill`, `wait` ([Job control](#job-control)).
- **History** — `history` (list past commands; `history | grep` is the MVP search —
  see [Interactive history](#interactive-history)).
- **Process** — **`exec CMD …`** replaces the shell process with the command (the
  `exec(2)` hand-off; ordinary invocation runs a child instead). `CMD` resolves as
  an **external executable** — function and built-in lookup is bypassed, since there
  is no process image to replace the shell with otherwise, so a name that is only a
  function or built-in (`exec cd`, `exec my-wrapper`) is an **error**. With only
  redirections and no command it applies them to the current shell (bash's `exec >log`).
- **Values** — **`re(STR)`** builds a [regex value](#tests-and-comparisons) from a
  string — a built-in constructor, since a rich value can't come from an external —
  with `re(STR, literal: true)` for verbatim matching. **`glob(STR)`** is *not* a value
  constructor — it **expands** a (runtime-built or absolute) pattern to its matching
  **paths**, a [list](#arrays-lists), since globbing is filesystem expansion, not a
  pattern object; its match-side twin **`fnmatch(STR, PAT)`** returns a bool for
  "does this string match this glob pattern" with no filesystem access. **`files(DIR=.)`**
  and **`dirs(DIR=.)`** are the [wrapper](#globbing) expansions — `glob` over a
  directory's immediate entries preset to `type: file` / `type: dir` — returning a
  path [list](#arrays-lists). `style` (above) is the styled-value constructor.
- **Discovery** — **`type [-t|-P|-a|--quiet] NAME …`** says what a name *is*:
  a keyword, a built-in, a function, or the executable `PATH` finds — and, because
  mesh keeps bindings in a namespace of their own, the variable or `$env` entry of
  that name alongside it. Bare, it reports the **winner** — what a bare `NAME`
  would run — and names what that shadows (`git is a function (shadowing
  /usr/bin/git)`), which is the interactive question; **`-a` / `--all`** lists
  every match in resolution order instead. **`--quiet`** prints nothing at all and leaves only
  the status — `0` found, `1` not — so `if type --quiet fzf { … }` is mesh's
  `command -v fzf >/dev/null`. A name is given **without a sigil**: `type xs`
  asks about the name, where `$xs` would expand before the built-in ever saw it.
  A word with a `/` in it is a **path operand**, read as command resolution reads
  it — the file, not a `PATH` search. Because it is the search `execvp` performs,
  an **unset `PATH`** falls back to the platform's default (`confstr(_CS_PATH)`)
  rather than reporting nothing, exactly as the exec would.

  **Described is not usable**, and where they part the line still prints and the
  status still fails, so the report explains it: a path that exists but could not
  be run (a directory, no execute bit, or a fifo carrying one — all `126`), and a
  shape the parser does not claim in command position (`127`). Only a word it
  claims *unconditionally* resolves — `if`, `for`, `func` — because the rest are
  **contextual**: `fork` is the subshell keyword only before a block, `unless` a
  postfix guard after a statement, `and` an operator between values, so a bare one
  is an ordinary command word and a legal function name. A contextual word is
  therefore reported *beside* the function or executable that a bare one reaches,
  never as shadowing it.

  **Two flags carry the shapes a script consumes**, and both are bash's, because
  their output is compared rather than read. **`-t`** prints one word —
  `function`, `builtin`, `file`, `keyword` or `variable` — which is what a guard
  wants instead of matching prose: a port that writes
  `case "$(type -t "$1")" in function)` keeps working, where matching
  `*" shell builtin"` against the sentence breaks the moment the wording moves.
  **`-P`** prints only the path a `PATH` search finds, ignoring functions and
  built-ins, and retires the hand-rolled `for d in $PATH` loop an `shrc` carries
  because `type -P` is not portable. Both print nothing and exit `1` when there is
  nothing to print. `variable` is the one word bash has no use for, since its
  `type` does not see bindings.

  **`-t` answers only about something the name can actually be used as.** It
  looks past the command race — it has to, since `variable` is not in it — and
  that is also where the findings that *describe without resolving* are kept: a
  contextual syntax word (`and`, `fork`), and a path operand that exists but
  cannot be run. Neither is what a bare name reaches, so neither is a `-t`
  answer: `type -t and` prints nothing and exits `1`, the same as the sentence
  form, while `type and` still describes the word from the side. Define
  `func and()` and `-t` says `function`, because the contextual word never
  outranked it.

  **One vocabulary, bash's, everywhere.** The prose says `if is a shell keyword`,
  never "is syntax", so the sentence and `-t` cannot disagree about what a thing
  is, and it follows bash's wording wherever there is no reason to differ — `cd is
  a shell builtin`, `ls is /usr/bin/ls`. Where mesh has a reason it says more: the
  detail line under each finding, the variable row, and naming what a winner
  shadows are all things bash has nothing to say about. A sentence is *read* while
  `-t` and `-P` are *consumed*, so only the consumed shapes owe anyone byte
  compatibility.

  It takes **bash's spelling**. `whence` is ksh's and stays reachable, as do
  `what` and `where`, which no shell defines as this; none of the three is
  reserved, so a user function may still take those names. **`which` is left
  alone** — in bash it is an external program that cannot see built-ins or
  functions, and mesh keeps that rather than shadowing a binary, so `which cd`
  finds nothing here exactly as it finds nothing there. The earlier objection to
  `type` — that mesh has real value types and [`:type`](#modifiers) already asks a
  path's — does not survive contact: `type foo` is a command and `$p:type` a
  modifier on a value, and neither can be written where the other is meant. The
  **value**-side question is [`:repr`](#modifiers), which already answers it.
- **Session** — `exit [status]`.

**No alias *mechanism*.** What mesh drops is the machinery — parse-time textual
expansion and a resolution stage of its own — not the familiar name. A bare word
that is neither a function nor a built-in is a command-not-found error, never a
silently-expanded alias, and there is no second half-language of "short names":
what `alias` defines is a [`wrapper func`](#functions), so it composes, scopes,
and takes arguments properly, and `type` reports it as the function it is.

The word is kept because it is the one every shell user already reaches for, and
[`alias ll = ls -l --color`](#functions) is exactly as terse as bash's while
being real syntax rather than a stored string.

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

*(TODO — gap surfaced porting a vi NORMAL/INSERT prompt indicator
(`keymap_character`): the [prompt segment map](#hooks-and-the-prompt) is evaluated
**once before** the editor runs, but the vi keymap changes **during** editing, and
mesh exposes neither the live keymap as a value nor a redraw hook when it changes.
zsh solves this with a `zle-keymap-select` widget that redraws a mode indicator
reactively. Decide how to surface the active keymap (e.g. a `$sh.keymap` a segment
can read) plus the on-mode-change **redraw** a reactive indicator needs.)*

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
   `/usr/bin/tool`'s page), which instead falls through to the probe. The
   association is the **install prefix**: the page is looked for beside the
   executable — `<prefix>/bin/tool` is documented under `<prefix>/share/man` —
   rather than through `MANPATH` or `$PATH`, since those say where pages *are*
   and the question is which page belongs to *this* binary. Formatting is left to
   **`man -l <path>`**, which decompresses the page and handles every roff dialect;
   read through a pipe it returns plain text, so nothing has to be unescaped. That
   makes this layer "runs a **formatter over a data file**" rather than literally
   no execution — a different bet from running the user's command, and the reason
   it still outranks the probe. A page yields **options only**; subcommands are
   left to the probe, since a page documents them in prose with none of the table
   structure a help listing has;
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

A **curated file** is named the way the manual names the same thing — `git`,
`git-commit` — so a subcommand's spec sits beside its command's, and a command
word is a file name rather than a path. One candidate per line, with a value type
spelled out rather than inferred:

```text
# mesh completions for demo
--verbose
--output file            # file, dir, page, or a | list of literal values
--color auto|always|never
build
positional dir
```

The generated sources read whatever a program happens to print, so they are
heuristic by nature; a curated spec exists for when that guess was wrong, which is
why it *says* its types instead of having them deduced from a metavar's name. It
is read before anything is resolved or run — so one holds for a command that is
not on `PATH` at all — and read afresh each time rather than cached, so editing one
takes effect at the next Tab. A file that says nothing (empty, or only comments)
falls through to the generated spec rather than answering with an empty one.

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

**One flag rule, for built-ins and functions alike** *(decided)*. Built-ins parse
flags — several must, since `kill -9`, `disown -a`, `prompt --reset` and
`on --remove` are their spellings — and `puts` "takes no flags" means it has
none *of its own*, not that its `--help` is data. Two consequences, and they are the
same for both kinds of command.

**A builtin is not a third kind of command.** It works the way a function and an
external do, and any place it does not is a bug until it has been argued for —
`puts` is mesh's own code with a documented signature, not a special case. The one
split that *is* principled runs elsewhere: an **external** takes bytes, so
`curl $url` must pass `--foo` if that is what `$url` holds, while everything mesh
owns can know more than the bytes. Two rules with a stated reason, never three.

- **A word that *is* `--help` asks for help, wherever it came from.** `x = --help;
  puts $x` prints the usage, and so does `f $x` on a function. mesh's expansion
  safety is about never *splitting* or *globbing* a value — it was never a promise to
  launder a word that is a flag, and a shell in which `$x` could smuggle one past
  option parsing would be the surprising one.

  *(This reading is under revision. It is right that a flag stays a flag through a
  variable, and wrong about how that is known: today `x = --help` and
  `a = "--help"` both bind the string `'--help'`, so "is this a flag" can only be
  re-derived from the characters — and a consumer that sniffs text cannot tell the
  two apart. That is what let `f $w` bind an option because `$w` happened to hold
  `--sleep=0`, which is the data-decides-the-call reading ruled out everywhere
  else. The direction is a `Flag` **value**, decided where it is written, so the
  word carries what it is instead of being guessed at; `x = --help` is a flag and
  `a = "--help"` is text. Decidability then sits at the assignment, as it already
  does for `x = 7` against `x = 007` and for `g = *.md`. `TODO.md` carries the
  design and its open questions.)*
- **`--` ends the options and is consumed** *(under revision — see the `Flag`
  value entry in [`TODO.md`](TODO.md))*. Who consumes it depends on who has
  options to end: a command **with** options owns its terminator, because only it
  knows where its options stop (`kill -- -9 %1` looks for a job named `-9`;
  `prompt -- --reset` sets that text), while for a command with **no** options there
  is nothing to end, so the terminator is removed before it is reached. Either way
  only the *first* `--` goes, so a literal one stays writable after it.

  *Two parts of that are being replaced. "Who has options to end" describes the
  two **built-in** cases and does not reach functions: an ordinary `func`
  consumes the terminator whenever flag scanning is on, which it is even for a
  signature declaring no flags. And a **`wrapper func`** — which has no options
  and so would be "removed before it is reached" under the rule above — does the
  opposite: it never consumes the terminator and binds it positionally, because
  a wrapper does not interpret argument syntax at all. The rule as written is
  right for built-ins and wrong for both kinds of function.*

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
  off switch **`$sh.options.complete.probe = false`** for anyone who wants *zero*
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

**The store is SQLite** at `$XDG_STATE_HOME/mesh/history.sqlite3` (`$XDG_STATE_HOME`
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
- **`!^`** / **`!$`** / **`!*`** — word designators on the previous command
  *line*: `!^` its first argument, `!$` its last, `!*` all of them (joined by
  spaces). An empty argument list leaves `!*` empty but makes `!^` / `!$` an error,
  as does having no previous command. Because expansion reads the stored history
  (not the current input), they refer to a *separately submitted* line: run
  `mkdir foo`, then on the next line `cd !$` → `foo` (not the same-line
  `mkdir foo; cd !$`, where `mkdir foo` isn't in history yet).
  (`!n` / `!-n` by index are natural extensions — deferred.)
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
immediately followed by a **supported designator** — `!` (→ `!!`), `^` (→ `!^`),
`$` (→ `!$`), `*` (→ `!*`), or a word character (→ `!string`). A digit or `-` does
**not** activate expansion in the MVP (they are reserved for the deferred `!n` /
`!-n`), and neither do `=` / `~` (the operators `!=` / `!~`) or a lone `!` — all
left literal. Two safety wins over bash: expansion happens **only unquoted** —
*both* single and double quotes make `!` literal (bash expanding `!` inside double
quotes is a classic footgun) — with `\!` to escape and a
**`$sh.options.histexpand = false`** switch to turn it off entirely.

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

A handler value is a **callable**: an
[`&name` reference](#calling-for-a-value-and-lambdas) to a command or function —
resolved late, so a redefinition is picked up on the next event — or a
`func(){ … }` lambda for inline logic.

**The `&` is required here** *(decided)*, rather than a hook slot accepting a bare
word as a callable and reserving quotes for a literal string. Two reasons. A hook
slot could only ever have that rule because `$sh.*` is a **fixed, shell-owned
shape** that knows which slots are function-typed; a user's own `func` has no
typed parameters, so `my-retry(&attempt)` would need the sigil regardless — and a
reader would have to know which slots are magic. That is the same
position-dependent bare-word meaning that
[bare words and quoted values](#bare-words-and-quoted-values--decided) removed from
block tails. Second, the hook slot was the one place in the language where quoting
*changed* meaning; with `&` carrying the reference, quoting goes inert again
everywhere. The cost is a sigil on lines you write once in an rc file, and it is
worth it to keep higher-order user functions first-class.

**Event hooks** run for effect at named events, in symmetric `pre`/`post` pairs
plus the singletons — `preprompt` (before each prompt), the command pair
**`preexec`** (before a command runs, given the command line) / **`postexec`**
(after it finishes, given the command, its **exit status** — a
[`Status`](#open-questions), like every status channel — and **duration**),
the directory pair **`precd`** (before the cwd changes, still in the old dir,
given the target) / **`postcd`** (after, now in the new dir, given the previous
dir), and `exit`:

```
$sh.preprompt.jobs   = &publish-jobs                   # by name, late-bound
$sh.postcd.fetch  = func() { vcs auto-fetch & }     # arrived in a new dir — the PWD-gate is now the event itself
$sh.precd.save    = func(to) { save-dir-state }     # about to leave: act while still in the old dir
$sh.preexec.timer = func(cmd) { timer-start }       # start the clock…
$sh.postexec.timer = func(cmd, status, elapsed) { global last-cmd-time = $elapsed }   # …stop it; a Duration — `global` so it survives to feed the prompt
unset $sh.preprompt.jobs                               # remove one
```

The `pre`/`post` split (rather than a single after-the-fact hook) is what lets a
handler run *before* the transition — save state before leaving a dir, start a
timer before a command — separately from the after-work. The `preexec` /
`postexec` pair in particular is how the prompt's **last-exit status** and
**command timing** (both required dashboard fields) get fed without special
casing.

*(TODO — **terminal control: escapes & OSC features**. Surfaced porting
`title`/`set_title`/`init_title_sequences`, broadened to the whole surface. mesh
owns the line editor and prompt, so it should decide first-class handling — a hook,
a builtin, or automatic — for the escape/OSC features a modern interactive shell is
expected to drive, rather than leaving each to a hand-emitted `print "\e…"`:*
- ***Window/tab title*** *(OSC 0/1/2)* — **decided: automatic.** `user@host: dir` at
  the prompt and the command line while one runs, with the sequence chosen from
  `$env.TERM` (xterm `\e]0;…\a`, screen/tmux `\ek…`, nothing for a terminal not known
  to take one — an allowlist, since a terminal wrongly assumed to take a title
  prints it instead). **Off switch `$sh.options.osc-title`**, which silences the
  title without silencing the *clear* on the way out: that one is owed to any title
  the session actually wrote, since a shell that stops updating the title still has
  to stop owning it. A session that never titled anything owes nothing and stays
  silent to the last byte. A *replacement* for the text — a `$sh.title` hook — is
  still open, and is a different question from turning it off.
- ***Bracketed paste*** *(`\e[?2004h/l`)* — **decided: on, always.** Pasted input is
  inserted, not executed line by line, and a lone newline in a paste doesn't submit.
- ***Shell integration / semantic prompt marks*** *(OSC 133 `A`/`B`/`C`/`D`)* —
  **decided: automatic, off switch `$sh.options.shell-integration`.** The line
  editor emits `A` and `B`, the prompt's own
  boundaries; the shell emits `C` before the output and `D` with the status after,
  from outside the `preexec`/`postexec` dispatch so a printing hook falls inside the
  region the marks bracket. A line abandoned with Ctrl-C is closed with a bare `D`
  and no status; a blank submission is not a command, so it is neither marked nor
  hooked. The setting silences **both** halves — a terminal given `A` and `B` with
  no `C`/`D` reads everything after the prompt as still being input, which is worse
  than a stream with no marks at all — and is read once per command, before it
  runs, so a command that changes it cannot leave a `C` without its `D`.
  Under `$env.TERM_PROGRAM == vscode` the marks are **`OSC 633`**, VS Code's dialect:
  the same boundaries under a different number, plus `E`, which hands over the
  command line so the terminal can label and re-run it rather than reading the text
  back out of the echo. One dialect, never both — VS Code parses `133` as well and
  would count every command twice. `633;P;Cwd=` is left out, since `OSC 7` already
  reports the directory.
- ***cwd reporting*** *(OSC 7)* — **decided: automatic, once per prompt** (after the
  `preprompt` hooks), **off switch `$sh.options.cwd-report`**, which covers both the
  startup report a fresh remote shell owes
  a new tab/split and every later move, whatever caused it — a `cd`, a `func` that
  cds internally, a startup file — without a `postcd` hook of its own.
- ***Bold input*** — **decided: on, off switch `$sh.options.bold-input`.** What is
  being typed is drawn in bold: uniform weight rather than token-aware color, live
  as you type, and surviving Enter into scrollback, so a command stays
  distinguishable from its own output after the fact. Weight and not color because
  color would be a syntax claim the shell would then have to keep true, and because
  it has to read on any theme.
- ***Hyperlinks*** *(OSC 8)* — **decided: `link(text, url)`**, a `style` sibling
  rather than a raw escape, for the same reason color is data: the shell measures the
  visible width from the text and can drop the link where it cannot be followed,
  neither of which it can do with an opaque `\e]8;;…` inside a string. It builds the
  same [styled value](#hooks-and-the-prompt) `style` does — each sets the attributes
  it names — so the two compose in either order. The URL is percent-encoded outside
  printable ASCII, which is both what the sequence asks for and what stops an `ESC`
  in a URL from ending mesh's own sequence; a **scheme is required**, since a
  terminal needs an absolute URI and guessing `file://` would need a hostname to be
  right over `ssh`. Unlike color it survives **`NO_COLOR`** — that silences the
  palette, and dropping a link would lose the URL rather than make output plainer —
  but it does want a terminal known to parse an `OSC`, the same allowlist the title
  and the notification use.
- ***Clipboard*** *(OSC 52)* — **decided: a builtin, `clip`.** `clip TEXT …` or
  `… | clip`, copying the bytes it was handed to the terminal's clipboard, which is
  what makes it work over `ssh` where no local clipboard tool can be reached. The
  sequence goes to `/dev/tty` rather than stdout, since it is a message to the
  terminal and not output — that is also what lets a script copy. Reading the
  clipboard back stays out: it needs a query and a reply, so it can block on a
  terminal that never answers.
- ***Notifications*** *(OSC 9)* — **decided: automatic, plus a `notify` builtin.** A
  command that takes longer than ten seconds notifies when it finishes, with its
  outcome: a failure that completed while you were away is the case worth a
  notification. The threshold stands in for whether anyone is watching, which mesh
  cannot ask — terminals report focus, but the line editor owns the input, so those
  events never reach the shell. `notify TEXT …` sends one explicitly, taking
  arguments or stdin like `clip`, and `$sh.options.command-notify` turns the
  automatic one off. Inside tmux the sequence needs the `DCS tmux;` envelope, since a
  multiplexer consumes an `OSC` it does not implement rather than forwarding it.
  Making the *threshold* configurable wants a `$sh.options` that holds values and
  not only flags. OSC 777 stays out: its `notify;title;body` form would double up on
  terminals that support both.
- ***Cursor shape per mode*** *(DECSCUSR `\e[…q`)* — block in vi NORMAL, bar in
  INSERT; driven by the same mode-change event as the keymap-indicator TODO in the
  line-editor section.
- ***Synchronized output*** *(DEC private mode 2026, `CSI ?2026 h/l`)* — wrap the prompt / multi-line redraw so it
  updates atomically without flicker.

  Decide per feature: automatic, a hook/builtin, or out of scope (left to a
  hand-emitted `print "\e…"`). The six marked **decided** above have landed;
  `TODO.md` §"Beyond M3 — Terminal integration" tracks the rest.
  Everything **automatic** here is interactive-only and, bracketed paste aside,
  carries a `$sh.options` off switch — the decoration is the default because it
  should be pleasant out of the box, and the switch is for the terminal,
  multiplexer, or taste it does not suit. Bracketed paste has none deliberately:
  with the guard off a pasted newline arrives as Enter and every line but the last
  runs before it can be read, which is data loss rather than a decoration. A
  builtin needs none either — it writes only when called.)*

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

**The `exit` hook fires however the session ends** — `exit`, Ctrl-D, the end of
a script or a `-c` string, an `exit` from a startup file. It is where a session
tears down what it set up, and a script cleaning up after itself is that case as
much as an interactive session is, so tying it to the prompt loop would miss the
half that needs it most. It is handed the status the shell is leaving with (the
argument to `exit N`, or the last command's status otherwise) — bash's `$?`
inside a `trap … EXIT`, and a [`Status`](#open-questions) like the rest. A `fork { … }` subshell leaving is *not* the session
ending and runs no handler.

*(TODO — **exiting because of a signal**. bash runs its EXIT trap for the
catchable fatal signals and re-raises afterwards, so the parent still sees
`128 + N`; only `SIGKILL` escapes. mesh runs nothing there yet. Open with it:
whether the handler should be **told** it was a signal. bash's answer is no —
`$?` in the trap is the last command's status, not `128 + N`, so a handler
cannot tell a clean finish from a kill — and mesh copies that for now. Passing
`128 + N` would let it tell them apart and would match what the caller waits
for, at the cost of that encoding meaning two things: today it says a **child**
died on a signal.)*

**Status is snapshotted across hook dispatch.** The submitted command's exit
status (and pipeline stage statuses) are captured before `postexec` / `preprompt`
run, and **`$sh.status` and `$sh.pipestatus` are restored** to them for the
prompt segments — so a segment always sees the *interactive command's* status,
never the status of some command a handler happened to run. (`postexec` also
gets the status as an explicit `status` argument.)

**The prompt** is a named, insertion-ordered map where **each top-level entry is
one line**, rendered top to bottom. A line's value — a callable is evaluated to
produce it — is one of:

- a **renderable**: a plain string or a `style(…)` value (or `""` to contribute
  nothing → its line is skipped);
- a **flat list of renderables**: the inline pieces of the line, **space-joined,
  empties dropped** — the *same rule `puts` uses* for its arguments, so `[&host-info
  &dir-info &auth-info]` reads like `puts host dir auth` and an empty middle piece
  never leaves a double space. Each piece **keeps its own style** (the pieces stay
  separate *values*; fold them into a string — `"$a$b"` — and the attributes flatten,
  since a string has nowhere to store per-piece color). *Tight* joining (`user@host`,
  no space) is not a list job: build it **inside a segment** as a string where you
  control every character — or, when the tight unit is also multi-color, as a
  `style([…])` [span](#hooks-and-the-prompt) (post-MVP). Line list = space-joined
  fields; segment string = character-level control;
- a **keyed sub-map** (`[host: …, dir: …]` — a map literal, `[ ]` not `{ }`): the
  *same* inline line, but each piece **named** so you can replace or `unset` it
  individually;
- a **structural piece**: `rule` (a full-width line) or `newline` (a blank line) —
  each a **whole** line; or **`fill`**, the *inline* structural piece, used *within*
  a line's list (below). `rule` and `newline` are **zero-argument** callables;
  `fill` takes **one optional argument**, the character to repeat (spaces by
  default). All three are referenced `&rule` / `&newline` / `&fill` like any other
  segment, and `fill("─")` is a *call* that produces the piece directly and needs
  no `&`, being a value rather than a reference. What `fill` never takes as an
  argument is the renderer's measured **slack** — that stays the renderer's job.
  The reading *not* taken is exactly that one: a `fill` handed the slack would have
  made `&fill("─")` a partial application, which mesh has no other instance of —
  see [Open questions](#open-questions).

A segment slot holds an [**`&name` reference**](#calling-for-a-value-and-lambdas) —
late-bound, so re-sourcing rebinds it, the same rule the hooks use — or a
`func(){ … }` lambda. A **bare word is an ordinary string**, exactly as it is in
every other value position, so the slot no longer inverts the quoting rule
(`&host` calls the `host` segment; `host` and `"host"` both render the text). And
**multiple lines are multiple entries** — a list is always the pieces of *one*
line, never several lines. So there are no separator entries to name:

```
$sh.prompt.status = &status-info               # a line — the status-info segment, by name
$sh.prompt.rule   = &rule                      # a full-width line on its own
$sh.prompt.line1  = [&host-info &dir-info &auth-info]   # ONE line: host (red) dir (blue) auth (yellow), each its own color
$sh.prompt.jobs   = &job-info                  # its own line — skipped when empty
$sh.prompt.char   = func() { "> " }            # a func literal is fine too

# `fill` is the inline right-align / trailing-bar piece, when you want it:
$sh.prompt.line1  = [&host-info &dir-info &fill &clock-info]   # host dir on the left, clock flush-right
$sh.prompt.line1  = [&host-info &dir-info fill("─")]           # …or a bar to the right edge (`rule` ≡ a whole-line [fill("─")])

# named variant — same line, pieces individually addressable:
$sh.prompt.line1     = [host: &host-info, dir: &dir-info, auth: &auth-info]
$sh.prompt.line1.dir = &my-dir-info            # swap ONE piece by name
unset $sh.prompt.line1.auth                    # drop the auth warning

func host-info() { style("$(hostname)", fg: red) }     # `style` (not styled); comma-separated args; parens on the func
func dir-info()  { if inside-project() { "$(vcs prompt-info)" } else { style(tilde-pwd(), fg: blue) } }
func auth-info() { if ssh-id-missing() { style("SSH", fg: yellow) } }   # no else → "" → omitted
```

(Segments use `if` *expressions* to pick a string — not `and`/`or`, which combine
bools, not values — and the `auth` segment leans on the decided
no-`else`-yields-`""` rule so "not applicable" is just an empty contribution. The
`nl1` / `nl2` separator keys an earlier draft needed are gone: lines come from the
map's shape, and the only structural entries — `rule`, a deliberate blank
`newline` — carry *meaningful* names, never a positional filler like `nl3`.)

**Color comes from a `style` helper, not raw escapes.** The value call
`style("no-ssh-id", fg: yellow, bold: true)` returns a **styled value** — text and
style attributes kept apart — rather than baked-in ANSI. It is an ordinary value
call, so it takes attached parens and `--flag` arguments like any other; a *bare*
`style …` would run it in command position and yield a status, not the value
(hence the parens in the example above).

This falls out of the general [`$(…)`-vs-`()` split](#calling-for-a-value-and-lambdas):
**`()` yields a structured value, `$(…)` yields raw output.** A **renderable** is
therefore one of two things:

- a **styled value** (from a `()` call to `style`) — text and attributes kept
  separate, so the shell measures display width from the text *and* can strip or
  re-theme the styling (needed for the later transient/collapsed form). Because the
  attributes are data, `style` is also where **color downgrade** lives: it drops the
  styling automatically when output is not a color-capable tty or when **`NO_COLOR`**
  is set, so there is no config-visible `$color` flag or capability probe to manage; or
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
**text** is empty contributes nothing — a plain `""` or `style("", fg: yellow)`
alike, since emptiness is judged by the payload text (not emitted as bare control
codes). `style` is the one styling primitive in the MVP (color + bold).

A styled value is **not a new scalar type** — it is a **string carrying display
attributes**. Everywhere *except* rendering it behaves exactly as its
text: the same [argv](#spread--flattening) rule (its text crosses, an
embedded NUL is the same hard error), the same [`+=`](#arrays-lists) (it
concatenates as its text, yielding a plain string — attributes are
rendering-only and don't survive), the same comparisons and string
interpolation — and, for the same reason, a **modifier** transforms it as its text
and yields a plain result. **Only a renderer reads the attributes** — the prompt
renderer, and [`puts` / `print`](#builtins) writing to a color-capable terminal;
every other context sees a string. So `style` adds presentation metadata to a
string without minting a type that must be defined at each boundary. *(A richer
per-fragment "styled spans" value — where concatenation preserves each fragment's
own style — is a possible later iteration; the MVP keeps one attribute set per
string.)*

**Styling a styled value adds to it.** `style(style(x, fg: red), bold: true)` is
red *and* bold: a named argument overrides only the attribute it names, so a caller
can emphasize a segment someone else produced without knowing its color. And a call
that names **no** attribute is a plain string, not a styled value with nothing to
render — one representation per meaning, so `style(x)` and `x` are the same value by
type as well as by comparison.

**Line structure is the map — newlines are not in-band.** Because each top-level
entry is a line, line breaks come from the **map's shape**, never from an in-band
`\n` a callable printed, and **never from a list** — a list is the space-joined
*pieces of one line*, so **multiple lines are multiple entries** (a list element
that is itself a list is an error — no guessed flatten, no lines-from-nesting).
That is what makes the per-line features well-defined: a "line" is a map entry,
stable and addressable, not a function of what a callable happened to print. A
segment renders its **return value**, consistent with the
[value-vs-stream split](#calling-for-a-value-and-lambdas) — you *return* your
prompt, you don't `puts` it. (The one exception is raw external output, below,
whose `\n`s are honored — you can't dictate an external tool's line count.)

**Empty entries take no line.** An entry — or a grouped inline segment — that
renders `""` contributes **no line**, so the common "nothing to show" case (an
empty `vcs` / `jobs` / auth) simply collapses: no blank gap, and no separator to
suppress. A *deliberate* blank line is an explicit **`newline`** entry (named, e.g.
`gap`), so blank lines are opt-in, never an accident of an empty segment.

**External output is the one place `\n` is honored.** A value that *is* the raw
output of an external capture — `"$(vcs prompt-info)"` returned **directly** — may
carry `\n`, since you can't dictate an external tool's output; the shell honors
those as **dumb** breaks that the structural entries (`fill` / `rule`) don't align
across. Provenance rides the **value**, not the map slot: passing that output
through `style(…)` or string concatenation re-imports it as an ordinary mesh string
(back under the single-line rule), so a genuinely multi-line external renderer must
be returned raw, not wrapped. So a drop-in external renderer (starship, `vcs
prompt-info`) still works. The renderer measures width **per line**, tracks how many
lines the prompt occupies, and places input after the last one so redraw,
completion, and resize stay correct; there is **no line-count knob**.

**`fill` — right-align and trailing bars.** Within a line's list, **`fill`** is an
inline piece that **expands to consume the remaining width of its line**, pushing
whatever follows it to the right edge — the right-alignment primitive.
`[&left-info &fill &right-info]` puts `left-info` flush-left and `right-info`
flush-right; **multiple `fill`s on a line split the slack evenly** (even columns). It
fills with **spaces** by default; give it a character to repeat that instead —
`fill("─")` draws a bar to
the edge, so `[&host-info &dir-info fill("─")]` renders `host dir───────────────` out
to the right margin. **`rule` is the whole-line case of `fill`** — a line whose only
piece is `fill("─")` — so the two are one mechanism: `fill` fills the *rest of a
line*, `rule` fills a *whole line*. `fill` measures against the same per-line width
the renderer already tracks, and its own width is the slack (zero when the line is
already full).

The payoff is the requirement, met directly: **the external base renderer is
just one named segment** (`$(vcs prompt-info)`), sitting among peers, so
`[root]`, the auth warning, and the session nag compose *around* it rather than
being swallowed by it — the failure mode of "set `$PROMPT` to one big external
command." This is exactly the hand-rolled `preprompt` / `prompt_line` /
`host_info` / `auth_info` structure of today's config, promoted to first-class,
keyed, re-source-safe segments — with its *side effects* (a background fetch)
moving to the `$sh.preprompt` event hook and its *rendering* to this segment map.

*(MVP: keyed **line entries**, `style` color, an entry yielding a renderable **or a
space-joined flat list of pieces** (empties dropped, `puts`-style; each keeps its
own style), an optional keyed **sub-map** so the pieces are individually named, a
deliberate-blank **`newline`** entry, the full-width **`rule`** entry, and the
inline **`fill`** piece (right-align / trailing bar — consumes a line's slack,
multiple `fill`s split it evenly, an optional repeat-char draws a bar; `rule` ≡ a
whole-line `[fill("─")]`). Line structure is the **map** — a list is one line's
pieces, multiple lines are multiple entries — never in-band `\n` (raw external
output excepted, above). The one thing layered *past* the MVP is **transient
collapse** of past prompts in scrollback. The event set — `preprompt`,
`preexec`/`postexec`, `precd`/`postcd`, `exit` — is settled.)*

## Footguns we avoid

mesh's surface is partly *reactive*: many decisions exist to remove a specific,
well-known way an existing shell surprises people. This section collects the ones
that most shaped the design, grouped by the shell they're most associated with,
each paired with the mesh decision that defuses it. Several are drawn from real
workarounds in the author's own `bash` / `fish` / `nushell` configs — where a
comment in those files documents a hack, that hack marks the footgun.

Most of these defenses are **settled** decisions elsewhere in this document. A few
rely on mechanisms still being designed; those are marked ***(planned)*** and link
to the open TODO, so this section reads as "things we avoid" and "things we *intend*
to avoid" rather than promising the latter as done.

### bash / POSIX

- **A pipeline's `while read` silently loses its variables.**
  `n=0; seq 3 | while read x; do n=$((n+1)); done; echo "$n"` prints `0` in bash —
  the loop ran in a forked subshell, so `n` never escaped. mesh's **settled**
  answer is that the pipe keeps them. The **last stage of a `|` pipeline runs in
  the current shell** rather than a forked subshell — bash's opt-in `lastpipe`,
  automatic here where job control is not active — and a **compound statement**
  (`if`, `match`, `for`, `while`, `loop`) is a stage, so both `cmd | gets line`
  and `cmd | while line = gets() { … }` leave their bindings behind. A binding a
  stage makes is the shell's. A compound cannot *lead* a pipeline
  (`while … { } | cat` is a syntax error, since the statement dispatcher takes the
  `while` before a pipeline is read), but piping **into** a loop is the direction
  that matters. See [Redirection](#redirection).

  [Splitting a capture](#command-substitution) is the other spelling, and the
  better one when the whole list is wanted at once rather than a line at a time:
  `for line in $(cmd):lines { n += 1 }` iterates *in the current scope* with no
  pipeline involved. Its split is spelled rather than implied, which is a defense
  in its own right — bash's alternative to the pipe, `for x in $(cmd)`, re-splits
  each line on `IFS` and globs it, so escaping the subshell reintroduces the
  word-splitting bug. Before the last stage ran in the shell this was the *only*
  spelling that kept what the loop counted; it is now a choice rather than a
  workaround.
- **Unquoted `$var` word-splits and globs.** `rm $file` breaks on a space; `[ $x =
  y ]` becomes a parse error when `$x` is empty. The single most common bash bug.
  mesh has **no word splitting and no implicit globbing of a value** — `$x` is
  exactly one value; splitting is opt-in (`:words` / `:split`) and exploding a list
  into arguments is the explicit `...`. See [Spread](#spread--flattening).
- **`!` in double quotes fires history expansion.** Interactive bash expands `!`
  inside double quotes — `echo "hello!world"` fails with `!world: event not found`
  (a trailing `!` before a space or end-of-line is safe, but `!` before a word is
  not, which is the trap). mesh's [history expansion](#history-expansion) is
  **quote-safe and lexically narrow**: `!` is a designator only directly before a
  ref character *and never inside quotes*, and `!=` / `!~` are excluded — so
  `"hello!world"` is plain text.
- **`[ ]` / `[[ ]]` operator quirks** — `-a`/`-o` precedence, empty-operand parse
  errors, `-lt` vs `<`, `=` vs `==`. mesh has no `[ ]`: value
  [tests](#tests-and-comparisons) are type-directed (`==` / `<`), `~` matches
  patterns, and `:exists` / `:exec` are the file tests.

### zsh

- **Over-complexity.** zsh's power is a very large, mutable surface: dozens of
  `setopt`s that silently change core semantics (whether `$x` splits, how globs
  behave, prompt parsing), plus a completion system that is its own programming
  language. mesh keeps a **small, non-optional core** — no option flips whether a
  value splits — and derives [completion](#completion) mechanically from `--help` /
  man pages rather than a bespoke DSL. Behavior you can read off the page.
- **Job-control edge cases.** zsh has a long tail of job-control surprises. mesh
  makes [jobs first-class values](#job-control) with a specified lifecycle and
  defined [signal](#signals) semantics (SIGHUP-then-SIGCONT-to-stopped on terminal
  close, Ctrl-Z ignored at an idle prompt, status snapshotted across handlers) —
  behavior that is *specified*, not emergent. The author's configs hand-roll
  `%1`…`%9` job aliases (`for i in (seq 0 9) { alias %$i = fg %$i }`); mesh's `%n`
  job refs are built in.
- **1-based indexing.** zsh (and fish) index from 1. mesh is
  [zero-based](#arrays-lists), always — matching bash/Python/Rust — so a ported
  `$xs[1]` doesn't silently shift by one.

### fish

- **Splitting and the empty-vs-scalar trap.** fish splits every command
  substitution into a list and has changed those rules over time; the standard
  defense is `| string collect`, which appears dozens of times in the author's
  `config.fish` purely to keep a result (e.g. an empty `projectroot`) a *string*
  rather than an empty list that breaks the next comparison. mesh makes splitting
  **explicit and stable**, and makes the **list-vs-scalar choice part of the
  capture** rather than a post-hoc rescue: `$(cmd)` is [one
  string](#command-substitution) and a list is what you ask for by spelling the
  split (`:lines` / `:words` / `:nulls` / `:tabs` / `:split`, with a defined
  [trailing-empty-field rule](#modifiers)); `$(cmd):raw` is the variant that also
  keeps the trailing newline. Nothing split the scalar in the first place, so
  there is nothing for a `string collect` to undo. The empty cases are each clean
  and stated ([Modifiers](#modifiers)): an empty capture is `""` and an empty
  split is `[]` — [no null](#variables-and-assignment) either way, so neither
  needs a guard.
- **Non-POSIX breaks muscle memory.** fish dropped `$(...)`, `&&` / `||` (for
  years), `export`, and more, so familiar reflexes stop working. mesh keeps the
  POSIX **spine** — `$()`, `&&` / `||`, `~`, redirection — so those reflexes
  transfer; the ergonomics are additive, not a dialect you relearn. This is about
  *syntax familiarity only*: running existing sh/bash **code** stays a
  [non-goal](#non-goals), so `source` reads mesh grammar, not POSIX. A `brew
  shellenv`-style integration (whose output is POSIX shell) therefore needs a
  mesh-native path or an adapter here just as it does in nushell (whose `config.nu`
  reimplements it by hand) — mesh's win is that the *language* stays familiar, not
  that foreign snippets run.
- **`switch` / `case` is glob-only.** fish's `case` has no regex — the author's
  config notes "fish wildcards have no `[0-9]` character class" and falls back to
  `string match -rq '^-[0-9]+$'`. mesh's [`match`](#matching-match) takes `/re/`
  arms directly.
- **`eval` for dynamic definition and indirect variables.** fish resorts to
  `eval "function $alias; ssh_to $alias \$argv; end"` to synthesize per-host
  functions, and `eval "printf ... \$$arg"` for indirect variable access. mesh's
  direction is to make both first-class rather than string-`eval`, but ***(planned)***
  — neither is settled: dynamic definition is the wrapper/forwarding TODO in
  [Functions](#functions), and by-name variable access is its own open question in
  [Variables](#variables-and-assignment) (the intended answer is a
  [map](#maps-associative-arrays) indexed by the computed name, `$colors[$name]`,
  rather than reaching into the variable namespace — but that reframe isn't yet a
  settled feature).

### elvish / nushell (rich-value shells)

- **Everything is an exception.** Elvish raises on every nonzero command (you reach
  for `?(...)` to tolerate failure), which is heavy for interactive use. mesh keeps
  the Unix **status model** — a nonzero status is a [value, not a thrown
  exception](#functions) — so `grep x f; echo done` just runs, while you can still
  branch on the status.
- **Static (parse-time) command resolution.** nushell resolves `def`→`def` calls
  at parse time, so you *cannot* redefine a command and have existing callers pick
  it up (the author's `config.nu` documents this and routes overridable hooks
  through `$env.*` closures invoked with `do`). mesh resolves function calls at
  **call time** (see [Isolation](#isolation-and-subshells)), so a later
  redefinition or a hook override is visible to callers — no closure-in-a-variable
  workaround.
- **No exit hook.** nushell has none, so the author's job-publish file can't be
  cleaned up on shell exit. mesh's `exit` hook — with the full `preprompt` /
  `preexec` / `postexec` / `precd` / `postcd` set — is part of the core
  ([Hooks](#hooks-and-the-prompt)).
- **Rich-value ↔ byte-stream friction.** Elvish/nushell's structured values don't
  flow cleanly into byte-oriented Unix tools; you convert at every boundary. mesh
  draws the [bytes-vs-values line explicitly](#command-substitution) at the
  external-command edge (argv rendering rules; `puts` renders, externals take
  `...` / `:join`), so you always know which side you're on — rich values inside,
  bytes at the process boundary.
- **Unfamiliar syntax tax.** Elvish's `{|a b| … }` lambdas and data literals are a
  real relearn. mesh puts signatures where readers already look
  (`func name(params)`), keeps `$var`, and borrows the *semantics* (rest / flag /
  default params) not the syntax — see [Functions](#functions).

## Open questions

- **Name — decided: mesh** ([Name](#name)); smash was the runner-up.
- **Bash spellings for renamed builtins** — whether `echo` / `read` should be
  the names for [`puts`](#builtins) / `gets` (or live alongside them) is **open**.
  The argument for is that mesh already keeps the bash name wherever it can —
  `cd`, `export`, `source`, `exec`, `jobs`, `fg`, `kill` — and these two are the
  reflexes people type most. The argument against is that they are also the two
  bash builtins carrying the most baggage, which is *why* they were renamed:
  `echo -n` / `-e` would print as text under flag-free `puts` (silently wrong, in
  a language that is otherwise fail-loud), and `read -r` / `read a b c` have no
  `gets` equivalent, while `gets` *returns* the line so it composes
  (`gets():words`, `if line = gets() { … }`) in a way bash's `read` never does.
  Two spellings for one operation is the worst of the three, so this is a rename
  or nothing. **MVP answer, not a decision:** `command not found` names mesh's
  spelling for a renamed bash builtin (`read` → `gets`, `local` → `x = 5`), which
  buys discoverability without spending the name — see
  [Reference](docs/REFERENCE.md#commands). `echo` stays unintercepted so an
  external `echo -n` keeps working.
- **Exclusion `~` alias** — the *spaced* alias was resolved by elimination: `~` /
  `!~` is the **pattern-match** operator ([Tests and comparisons](#tests-and-comparisons)),
  so a spaced `~` between two globs cannot also mean exclusion. zsh's **unspaced**
  `*~*.bak` is a different spelling and is not settled by that, which is why the
  question is reopened below as *Exclusion in argument position* — where the
  spaced infix `-` turns out not to work at all.
- **String modifier set** — `:replaceall` (global substitution) with decided but
  lower-priority anchored/removal kin (`:replacestart` / `:replaceend` /
  `:stripstart` / `:stripend`, plus `:trimstart` / `:trimend` for whitespace).
  Substitution is settled: a **regex `OLD` in `:replaceall`** (`:replaceall(/foo/, bar)`),
  **not** a `:s/old/new/` form (`:s` is the `:dotall` flag; arguments stay
  parenthesized) — see [Modifiers](#modifiers). Remaining: backref spelling and
  whether a first-only `:replace` is ever needed.
- **Member access inside string interpolation — decided:** `$map.field` has the
  same meaning inside and outside `"…"`. Use `${file}.bak` when a dot begins a
  literal suffix rather than member access ([Variables and assignment](#variables-and-assignment)).
- **Predicate qualifier syntax** — confirm `size >` / `age <` / `mtime <` forms.
- **History expansion — decided** ([History expansion](#history-expansion)):
  interactive-only, quote-safe `!!` / `!string` / `!^` / `!$` / `!*` (with `!n`
  by index deferred);
  the `!` clash resolved lexically (a designator must follow, so `!=` / `!~` and a
  lone `!` are untouched); both quotes make `!` literal, `\!` escapes, and
  `$sh.options.histexpand = false` disables it. Substitution is a chainable,
  **global** **`:old=new`** modifier on any history reference (`!git:foo=bar:x=y`;
  quote each side or backslash-escape for spaces / specials), with **`^old^new`**
  as shorthand for `!!:old=new`.
- **Interactive history (store & recall) — decided**
  ([Interactive history](#interactive-history)): a **SQLite** store at
  `$XDG_STATE_HOME/mesh/history.sqlite3` with rich per-entry columns
  (command / cwd / tty / session / start / duration / status) populated by
  `preexec` / `postexec`; recall via up/down and `Ctrl-R`; a `history` built-in
  plus `history | grep` as the MVP search. Remaining: fuzzy search, a
  `$sh.history` accessor, cross-session sync, dedup policy, and secret redaction.
- **Interactive signals — decided** ([Signals](#signals)): interactive defaults
  (`Ctrl-C` abandons the line / interrupts the foreground job but never kills the
  shell; `Ctrl-D` EOFs on an empty input buffer; `Ctrl-Z` suspends; `SIGWINCH` redraws;
  `SIGHUP` exits, `SIGTERM` ignored). User handlers are the keyed **`$sh.signal.<NAME>`**
  hook maps (no bash `trap`), with `$sh.exit` as the EXIT trap. Remaining: whether
  a handler may suppress a default, and mid-pipeline SIGINT delivery.
- **Core surface** (arrays / maps / functions / `if` / `match` / loops / scope /
  tests / isolation) — sketched above. Remaining sub-questions: an infix **`in`**
  operator as a second membership spelling alongside `:has`; whether non-`_` `match`
  must be **exhaustive** (was leaning lenient → `""`; now ***coupled*** to the
  reopened `if` question in this section, for value-producing uses only); and the **`~` scope** lever (keep it
  the narrow string-vs-glob/regex predicate, or widen it toward the arm grammar — see
  [Matching](#matching-match)). *(Decided: the `match` **spelling** — prefix
  `match $x { … }`, arms `pattern [if guard] => value | { block }`, mandatory `=>`,
  `|` alternation, terminator-separated (newline or `;`, never comma); and a `/re/` arm
  does **not** auto-bind its
  captures — capture goes through the value-side `:match` extractor. See
  [Matching](#matching-match) and [Destructuring](#destructuring). This spelling is
  **implemented**; what a `=> { … }` block yields in expression position is still the
  open value-production question above.)*
  **Tests**
  replace `[[ ]]` (`~`/`!~` pattern-match, type-directed
  comparisons, `$p:type`/`:exists`/`:exec` file tests, `and`/`or`/`not` vs command
  `&&`/`||`); the **postfix guard** `stmt if/unless cond` is the one-line form;
  **isolation** is explicit — plain `func` persists cwd/state, `fork { }` /
  `fork func f() { }` subshell-isolate, `in DIR { }` scopes cwd without forking.
- **Value calls & lambdas — decided** ([section](#calling-for-a-value-and-lambdas)):
  `f(arg)` (parens attached, comma-separated args) takes a function's **return
  value**, `$(f arg)` its **stdout**, bare `f arg` runs it; stdout streams during
  a value call (independent channels); externals have no return value (runtime
  error → `$(…)`). Lambdas are `func(params) { … }` (anonymous, one param
  grammar), passed to `:map` / `:filter` / `:each`.
- **Function references — decided: `&name`**
  ([Calling for a value](#calling-for-a-value-and-lambdas)). A named `func` had no
  value spelling, so `$xs:map(up)` passed the *string* `"up"`; `&name` is the
  reference, and it is **late-bound** (resolved when called, matching call-time
  command resolution) rather than a captured function object. The lookup is the
  **command namespace**, so `&name` reaches a builtin or an external too — whether the
  *call* then yields a usable value is a separate question, and an `&external` works in
  an effect slot while failing in a value slot. It is **required in
  hook and prompt slots**, which retires the bare-word-is-a-callable rule there —
  the one place quoting changed meaning. Rejected: `\name` (`\` is the escape;
  `x = \up` already binds `"up"`). The **`:name` question this left open is now
  answered** — a user may add to the modifier vocabulary, but only by *declaring* a
  modifier (`func _s:name()`), never by writing an ordinary `func`. That is not a
  second spelling for `&name`: the line between them is by **shape** (`:name` postfix
  on its subject and auto-mapping, `&name` general — any arity, any slot) rather than
  by who wrote the name.
- **Partial application — open, and deliberately unanswered.** `&name` names a
  function but cannot pre-supply any of its arguments, so every higher-order slot
  that wants an existing function with one choice already made takes a lambda
  wrapper instead (`$xs:map(func(_x) { pad($_x, width: 8) })`). The leading spelling
  if mesh ever grows one is **`&f(key: value)`** — the reference sigil kept, so it
  is still a *value* rather than a call, with arguments bound by **keyword** only,
  which sidesteps "which positional did you mean" and matches how a flag parameter
  already derives its call-site name. Two costs, neither weighed yet:
  `&pad(width: 8)` and `pad(width: 8)` would differ only by the sigil while
  meaning "later" versus "now" — a
  distinction the prompt slots already carry (`&fill` vs `fill("─")`), but not yet
  one where *both* forms are legal in the same slot — and it inherits the capture
  question from lambdas, since a bound argument could be snapshotted when the
  partial is written or re-evaluated on every call. **Nothing is blocked:** a
  lambda expresses every case already, at the cost of naming the parameter, so
  this is sugar and is recorded to be thought about rather than scheduled. It
  surfaced from the prompt `fill` reading *not* taken — a `fill` that received the
  renderer's measured slack would have made `&fill("─")` a partial application,
  which is why the slack stays the renderer's job and `fill`'s only argument is the
  optional repeat character ([Hooks and the prompt](#hooks-and-the-prompt)).
- **User-defined modifiers — decided: a *declared* modifier, `func _s:name()`.**
  `:ident` is reserved by the grammar, so the ambiguity was already paid for and a
  user modifier was always *possible*; the vocabulary was otherwise closed forever.
  What was open is whether to spend it, and on what terms. A modifier is a **postfix
  function on its subject** that auto-maps over a list, and the declaration is what
  marks one — an ordinary one-argument `func` is not reachable as `:name`, so a
  private helper is never promoted to public vocabulary by accident, and the subject
  and its `...` form get somewhere to live. **Resolution is at call time**, the same
  rule as command position, which costs the parse-time unknown-modifier error and buys
  consistency with every other name in the language. An earlier revision kept a
  load-time check instead and grew three rules to support it — hoisting, top-level-only
  declarations, and a blocked `source` boundary — all since removed. See
  [Modifiers](#modifiers).
- **Interpolation shape — open, for later.** Requiring braces around a modifier's
  argument (above) is a step in that direction, not the answer to it. `${…}` accepts
  **two grammars** today — `"${xs:join("-")}"` (bare name) and `"${$xs:join("-")}"`
  (a `$` expression) both work — and the second is the one worth building on:
  `"{$x:foo(bar)}"`, with the `$` always present and the braces purely a delimiter,
  would leave one grammar inside them instead of two. What it costs is the `{`
  immediately followed by `$`, which is literal text today (`"{$x}"` prints `{5}`).
  No other literal brace is affected — mesh has no brace expansion, so `"a{b}c"` is
  `a{b}c` and a JSON object pasted into a string stays JSON text. **Not decided, and
  not for now**; the braces-for-arguments rule holds either way.
- **Element-wise over a map — open.** `$m:name` where the subject is a **map** has no
  answer here. Loop iteration is already settled and does *not* decide it: `for host,
  addr in $known_hosts` binds key and value as two names, which is a **binding form**,
  not a pair *value* — and a modifier receives one subject, so it has nothing to bind
  two names to. Over-values and over-pairs are therefore both live, and over-pairs
  would need a pair value the language does not have. `$m:name` **errors for now**,
  naming `:keys` / `:values`, which decides nothing; the error can lift once the
  question is answered, since nothing could have depended on it working. Related but
  separate: [map destructuring](#destructuring) is deferred on its own terms.
- **Bare environment references (`$PATH`) — decided for now, and reversible**
  ([Variables and assignment](#variables-and-assignment)). The environment becomes
  a third scope rung below local and session; reads fall outward to it on
  presence; no name may take an unbounded second binding — rebinding one for a
  bounded, marked region is what `NAME=value cmd` and `with` are for, and stays
  untouched; and `_`-prefixed
  function-locals — **parameters included, no exemption** — keep that ban static
  and modular. Adopted to be lived with rather than settled on paper: the
  underscore tax on every function body is the piece most likely to be reversed.
  `_` is **part of the identifier**, which is what keeps the ban free and static;
  a flag parameter therefore *derives* its call-site name by dropping the leading
  `_` (`--_region` is passed as `--region`) rather than being identified with it,
  so call sites are unchanged and only declarations gain the prefix. The prefix
  reaches every binding form — lambda parameters, destructuring, match arms — but
  only **inside a function**: at top level the session is the current scope, so
  interactive and top-level code stays plain-named. The cost therefore lands on
  function bodies that destructure or match, which is the first thing to judge in
  use.
- **Remaining function questions** — whether a **`func` defined inside a `func`**
  is visible only there; a **TODO — dynamic scope**: the "extract a chunk
  into a subfunction" goal that fixed cwd as *persist* would be served further by
  letting an extracted helper see the caller's locals — weigh dynamic (or opt-in
  dynamic) scope against the lexical default. *(A named helper still cannot see its
  caller's locals — that is this question and stays open. A **lambda** now can see
  the locals of the scope that **defined** it, which is
  [lexical capture](#calling-for-a-value-and-lambdas), decided separately and not an
  answer to this one; the two are easily conflated.)* And an **open value-production
  question** *(from the match-syntax exploration — see [Matching](#matching-match))*:
  whether functions/blocks should require an **explicit value keyword** instead of the
  settled implicit **last-expression** rule. Language-wide — it
  touches every value-producing block: `if`, `match`, `for`, and `func` alike.
  *(The **single-bare-word block-tail coercion** this used to be bundled with is
  **gone** — settled independently by
  [Bare words and quoted values](#bare-words-and-quoted-values--decided), which did not
  need a value keyword to get there. The general assignment-RHS rule stays either way.)*
  *(**Spelling, if one is ever needed: `result`, not `yield`.** The two are not
  interchangeable names for one thing. `yield` means **generator** in every language a
  reader is likely to arrive from — Python, JavaScript, Ruby, C# — where it emits *many*
  values lazily and suspends between them. That is a real feature a shell could want,
  and one mesh may eventually want at the value channel: a `func` that emits a stream of
  values into a pipe is the typed analogue of a stage emitting lines. Spending the word
  on "send back one value" trades a feature's natural name for a synonym of `return`.
  `result` is already mesh's own vocabulary for exactly this — "a function's **result**
  is its last expression", above — so it names the thing the language already calls it,
  and it carries no suspension baggage. Runners-up: `give` (unclaimed, no baggage, but
  no precedent either) and `value` (reads as a noun, weak as a verb). Whichever wins is
  **contextual**, like `fork` / `global` / `unset`, so a program or variable of that
  name stays reachable.)*

  *(**Implicit stdout capture in value position is gone** *(decided; shipped)*. A
  value-position block used to run its body under a capture and yield the bytes,
  gated on exit 0. It was never intended — `func` never did it, and the rule was
  always that a block streams unless something explicitly captures or calls it — and
  three sharp edges came out of it. The **same block text** either streamed or was
  silently eaten depending on whether anyone bound the result, so
  `x = if true { echo hi }` swallowed `hi` while the bare statement printed it. The
  capture took **every** statement's stdout rather than the tail's, so
  `{ puts a; some-cmd }` yielded the `a` too. And the exit-0 gate failed **silently**:
  `x = if true { grep -q zzz f }` left `x` unbound, surfacing as an "unbound variable"
  on a later line with nothing to say why. `eval_value_body` (repl.rs) now routes
  `if` and `match` through the same `eval_body` a `func` body uses, so the three
  agree: output streams, and the value is the last thing that produced one. `$(…)` is
  the thing that means "capture", and it still does.)*
- **A `proc` / `func` split — open; leaning add `proc` only and leave `func`
  alone.** The split already exists; it is at the **call site** rather than the
  declaration. [Calling for a value](#calling-for-a-value-and-lambdas) chooses by
  mode — `f arg` takes words, streams stdout, and its result is a status; `f(arg)`
  takes expressions and its result is the return value; `:capture` is there for
  when you genuinely want both channels. The open question is whether that choice
  belongs to the **caller** or to the **declaration**. YSH (Oils) puts it at the
  declaration: a `proc` takes words and answers with a status, a `func` takes typed
  arguments and answers with a value. Tcl's `proc` is already cited
  [above](#functions) for its signature vocabulary, so the word arrives with the
  right connotation — a procedure, run for its effect.

  Two shapes land on opposite sides, which is the argument in one screen: `ips()`
  in [`INTRO.md`](INTRO.md) writes with `puts` and has no return value anyone
  wrote, while a string-building helper returns a value and writes nothing.

  ```
  proc ips() {                          # words in, bytes out, a status back
    for line in $(ip -o a sh up primary scope global):lines {
      [_ _iface _afam _addr ..._rest] = $line:words
      puts $_iface $_addr if $_afam ~ inet*
    }
  }

  func path-string() {                  # values in, a value out
    return $env.PATH:join(":")
  }
  ```

  **"No return value anyone wrote" is not the same as none, and the gap is itself
  an argument for the split.** `ips()` ends in a `for`, and a `for` collects a
  value per completed pass into a list — `eval_for_passes` and
  `run_ast_for_passes` in `repl.rs`, deliberately, so that a `for`'s result is the
  aggregate rather than its last pass. Calling it for a value therefore hands back
  a list of per-pass results, not nothing. Nobody wrote that aggregate and nothing
  names it. Under the union **every** function carries a value channel whether or
  not its author meant to fill one, so "what does this return?" has no answer
  short of reading the body down to its last statement — which is precisely what a
  declaration keyword would answer in one word.

  | Option | For | Against |
  | --- | --- | --- |
  | **Keep mode-at-call-site** (today) | One definition serves both — `co main --amend` at the prompt and `x = co(main, amend: true)` in a script; nothing to rename; the caller asks for the channel it needs | The two argument grammars stay a *mode*, which is exactly the comma question [below](#open-questions); the `:capture` "doing two jobs at once" smell is a lint a reader notices, not something the language knows |
  | **Split at the declaration** (both keywords narrow) | Argument grammar becomes a property of the callee — decided once, printable by `help`; `return` / `fail` divide cleanly along the two channels; a func with no byte channel is the tractable subset for the [static checks](../TODO.md) a resolver pass currently cannot do | `func` **narrows**, so most existing `func`s are procs — a rename across `TOUR.md`, `REFERENCE.md`, this file, and every ported config; loses the one-definition-two-ways affordance; two keywords where there was one, against *concise* |
  | **Add `proc`, leave `func` the union** *(leaning)* | Purely additive — nothing existing breaks and no doc has to be renamed on day one; names the majority case (writes bytes, returns no value) and every external; `func` narrows by attrition as value-returning ones get written deliberately; if the split does not earn its keep the cost was one keyword, not a restructuring | For a while two spellings overlap, which is the redundancy [the `echo` / `read` question](#open-questions) calls the worst of its three outcomes — the difference is that this one is a *migration* with an end state, not a permanent pair |

  *Rejected spelling: `sub` / `fun`.* `sub` is Perl's and Raku's word for the
  **value-returning** thing, so it inverts the intended meaning for a reader
  arriving from either.

  **"Purely additive" holds only if `proc` is a *contextual* keyword.** Today the
  word is an ordinary name: `func proc(..._args) { … }` is a legal definition and
  `proc x` a legal call, so claiming it unconditionally the way `func` is claimed
  would turn that call into declaration syntax or an error — a compatibility break,
  which is exactly what the option is chosen for avoiding. mesh already has the
  mechanism, and **`fork` is the precedent to copy** — the subshell keyword only
  before a block, an ordinary command word otherwise, so `func fork() { … }` stays
  reachable and `type fork` reports the function beside the keyword. `global` /
  `unset` / `export` are *not* precedents here, though they read like ones: as
  [`:kind`](#modifiers) sets out above, each claims the word wherever an
  assignment does not follow, so no literal `global x` ever reaches a function.
  They are keywords that merely look contextual. `proc` has to work the way `fork`
  does — recognizing a complete declaration shape rather than the bare word — and
  that requirement belongs in the option rather than being assumed by it.

  **An argument for the split that does not survive, recorded so it is not made
  again: "an external would simply be a proc, so `grep(foo)` stops being an
  exception."** The [status decision](#open-questions) already got there without
  any split — an external's result *is* a `Status`, so `grep(foo)` answers
  `Status(1)` rather than erroring, and `f` / `$(f)` / `f()` mean the same three
  things for an external as for a function
  ([Calling for a value](#calling-for-a-value-and-lambdas)). That removes a reason
  to split. **The uniformity is on the *result* side only, though, and the limit
  is the interesting part:** a function takes a bare list as one typed positional
  where an external still needs it spread or joined, so an external's *arguments*
  are words in a way a func's are not. So "an external is a proc you did not
  write" is a fair shorthand for the result channel and an overstatement anywhere
  else — and what survives the correction is an **argument-grammar** asymmetry,
  which is the axis the split is actually about.

  **The wrinkle to resolve before `func` could ever narrow: hook slots hold both
  kinds.** `$sh.prompt.dir = func() { … }` returns a styled string — a genuine
  func. `$sh.postcd.fetch = func(_previous) { vcs auto-fetch & }` runs for effect
  and has no return value anyone wrote — the same incidental channel as `ips()`
  above, since a bare background statement records its launch `Status` as the
  result (`run_recorded` in `repl.rs`: "for anything else — a command, a
  background statement — the status *is* the result"). A proc by nature, stored in
  a variable. *(The parameter is
  not decoration: `postcd` supplies the previous directory and the binder is
  exact, so a zero-parameter handler is rejected at dispatch — see
  [`HOOKS.md`](HOOKS.md), which works through that same line.)* So either procs are
  first-class values the way funcs are, or each hook point declares which kind its
  slot takes and `$sh.prompt.*` and `$sh.postcd.*` answer differently. That is the
  sharpest question the split raises. It does **not** block adding `proc`, which is
  why the leaning is to add it and defer the rest.
- **Destructuring in a signature — open; leaning yes, positionals only.**
  [Destructuring](#destructuring) binds a list's elements to names at an
  assignment and the *same* pattern grammar drives a [`match`](#matching-match)
  arm — but a signature cannot use it, so a function taking a pair opens with
  bookkeeping:

  ```
  func connect(_pair) {
    [_host _port] = $_pair
    ...
  }

  func connect([_host _port]) { ... }        # the proposal
  ```

  Raku's subsignatures (`sub handle([$host, $port])`) are the closest precedent;
  Rust destructures in a parameter pattern, and Erlang/Elixir match in the clause
  head. It is the pattern grammar in a third position rather than a new one.

  | Option | For | Against |
  | --- | --- | --- |
  | **Skip it** | Nothing new to learn; the unpack line is one line, and it is already the strict form, so a wrong shape errors either way | The most common small function opens with bookkeeping; the signature says `_pair` where every caller thinks in host and port, and that useless name is what `help` prints |
  | **Positionals only** *(leaning)* | Reuses the [destructure](#destructuring) pattern verbatim — same brackets, same `...rest`, same `_` discard, same atomic all-or-nothing binding; no new grammar to specify; `help` can print the shape a caller actually passes | One more way to write a parameter; a defaulted or flag parameter cannot carry a pattern, so the restriction has to be stated rather than inferred |
  | **Positionals and flags** | Uniform across the whole signature | A flag whose value arrives destructured is rare, and at the call site `--pair=…` gives the reader nothing to match the pattern against |

  Consequences worth stating:

  - **The shape error moves to the call.** `connect(host)` is refused naming the
    parameter and the shape it wanted, before the body runs — where today it is
    refused on the unpack line, one statement in. It is a *shape* check, not an
    arity one: one argument arrives for one parameter, so the count is right and
    what fails is that the argument is not a two-element list. Same fail-loud
    rule, one step earlier, which is the same argument the
    [static checks](../TODO.md) item makes.
  - **Two separator conventions meet in one line.** Parameters are
    comma-separated (`func f(_a, _b)`) and list patterns are space-separated
    (`[_host _port]`), so `func connect([_host _port], _timeout)` mixes both. It is
    consistent — brackets are pattern grammar, parens are signature grammar — but
    it is the first place the two sit side by side, and it will read oddly before
    it reads obviously.
  - **A signature destructure is strict, with no soft twin.** The
    [strict/soft pairs](#error-handling) work because the soft form has somewhere
    to put the "no" — an `if` to skip, a default to return. A signature has
    neither, so a shape mismatch is an error, full stop. That is the right answer
    for a *required* positional and it is worth writing down rather than leaving
    to be inferred.
  - **Every binder inside a pattern joins the signature's name checks.** A
    signature's parameter names must be distinct and cannot be `env`, and a
    nested pattern has to flatten into that rule rather than sit beside it — so
    `func f([_x _y], _x)` is refused for the duplicate exactly as `func f(_x, _x)`
    is. Reusing the pattern grammar does not settle this on its own: an ordinary
    destructure only ever validates duplicates as part of binding, because it has
    no signature to collide with. `_` stays exempt, being a discard rather than a
    binder.
  - **Map patterns are out of scope here** — `[name: _n] = $m` is
    [deferred on its own terms](#destructuring), and a signature can only follow
    wherever that lands.
- **A flat soft bind — open; leaning a bare bind with a *distinct* fallback word,
  `[a b] = expr otherwise { … }`.** The
  [strict/soft pairs](#error-handling) give a soft bind, but only as a
  **condition**, so each one costs a level of nesting and the body drifts right:

  ```
  for line in $(cmd):lines {
    if [_key _val] = $line:match(/(\w+): (.*)/) {
      if [_host _port] = $_val:match(/(.*):(\d+)/) {
        ...                                        # two levels in, and the real work starts here
      }
    }
  }

  for line in $(cmd):lines {                       # the proposal
    [_key _val]   = $line:match(/(\w+): (.*)/) otherwise { continue }
    [_host _port] = $_val:match(/(.*):(\d+)/)  otherwise { continue }
    ...                                            # flat, and both binds are live
  }
  ```

  The borrow is Swift's `guard let` and Rust's `let … else`: a bind that escapes
  into the **current** scope on success, where the failure block must *diverge*.

  **Two independent axes, which an earlier draft of this entry collapsed into
  one.** Swift and Rust each write a prefix *and* spell the fallback `else`, so
  copying either wholesale hides that these are separate choices:

  | Axis | Options |
  | --- | --- |
  | **Prefix** | a `guard` keyword, or nothing |
  | **Fallback word** | `else`, or a word of its own |

  Everything difficult about this construct lives on the **second** axis, and the
  first matters only through it: with a distinct fallback word the prefix is a
  readability preference and nothing more, while with `else` a
  *mandatory*-fallback prefix becomes one of the ways to disambiguate — at a cost
  set out below.

  | Fallback word | For | Against |
  | --- | --- | --- |
  | **A distinct word — `otherwise`** *(leaning)* | The construct is unambiguous everywhere: no association rule, no restriction on what the right-hand side may be, no reversal of a decided call. Costs one keyword in the language and **nothing at the use site**, since it is typed exactly where `else` would have been | One more word to learn, and a longer one, on a construct meant to be terse |
  | **`else`** | No new keyword — it is already in the language, and this is Rust's `let … else` minus the `let` mesh has not got; reads as "bind these, else do that" | Collides with `if`'s own `else`, and the collision has to be paid for somewhere — see the four ways out below |

  On the **prefix** axis, `guard` buys one thing — it announces the diverging form
  before a long right-hand side rather than letting it arrive at the end of the
  line — and costs two: a keyword for something the bare form already spells, and
  a second meaning for a word this document already uses for the postfix `if` /
  `unless` modifier. Once the fallback word is distinct the bare form is already
  unambiguous, so the keyword is paying for readability alone. *Lean: no prefix.*

  **The dangling `else`, which is the whole of the second axis.** A lone `if` is a
  valid expression and yields `""` when false
  ([Conditionals](#conditionals-if-is-an-expression) — the shipped behavior, and
  [reopened](#open-questions) below, though the ambiguity here holds either way:
  it needs a lone `if` to be *legal*, which requiring `else` would end), so if the
  fallback is *also* spelled `else`:

  ```
  [x] = if $cond { [v] } else { continue }
  ```

  the `else` can complete the right-hand `if` **or** be the soft-bind fallback,
  and both readings parse. They are not equivalent: under the first the bind is
  strict, so a wrong shape is a hard error; under the second a wrong shape quietly
  runs `continue`. Identical text, error against silent skip. That is precisely
  the shape [*an ambiguous spelling is an error*](#error-handling) refuses to
  resolve by picking a winner.

  **A prefix does not help *by itself***, which is worth writing down because
  assuming it does is the natural mistake:
  `guard [x] = if $cond { [v] } else { continue }` reads both ways too, since a
  leading keyword marks where a construct *starts*, not where its right-hand side
  *ends*. Making the fallback **mandatory** after `guard` does settle it — the
  competing parse would leave the guard without its required fallback, so it is
  invalid, and the form becomes syntactically determined. But that is
  disambiguation bought with an association rule, and the rule silently captures
  an `else` a reader wrote for the inner `if`. The ambiguity turns into a wrong
  answer rather than a refusal, which is the trade the distinct word never has to
  make.

  Keeping `else` therefore means choosing one of these, every one of which the
  distinct word avoids outright:

  | Way out | Cost |
  | --- | --- |
  | **Refuse the combination** — `[x] = if … else …` is an error naming both rewrites | Most in keeping with the house rule, and nearly free since splitting the statement is always available; but it is a rule that fires rarely and so is easily forgotten |
  | **Restrict the right-hand side** (Rust's answer) — parens when it could take its own `else` | The parens are not grouping, they are a parser hint wearing grouping's clothes, against this document's position that parens keep meaning grouping; and the same expression is legal bare one line up, so they appear because of what *follows* it |
  | **An association rule** — the trailing `else` always belongs to the bind | Silently takes the `else` a reader wrote for the inner `if` |
  | **Require `else` on every value-position `if`** | Disambiguates by making the lone `if` illegal, but that is a language-wide call about silent empties, tracked as its own question below — and the legal form becomes `… else { [w] } else { continue }`, which parses only if you already know the rule |

  **Which word, if it is distinct.** `otherwise` is the lean. `missing` is shorter
  and echoes this document's own *absence is loud* framing, but it names the wrong
  condition for one of the two failures it would catch: a length mismatch
  (`[a b] = $three_items`) is a wrong shape, not an absence, and a keyword that
  lies about what it detects is worse than a longer honest one. **`or` is out**
  despite reading well: it already combines values and statuses, so
  `[x] = foo() or { continue }` re-creates the same ownership question one axis
  over.

  Consequences worth stating:

  - **The fallback must diverge** — `return`, `fail`, `break`, `continue`,
    `exit`. If it can fall through, execution reaches the next line with the names
    unbound, which is the silent empty mesh refuses everywhere else. Swift and Rust
    both require it. **But checking the *form* is not the guarantee it looks
    like**, and the gap has to be closed before this is safe to build: `exit
    status` with no code and `fail 0` both parse as diverging forms and both are
    refused at run time *without leaving* — `exit status` "does not end the shell",
    and `fail 0` is refused outright. A fallback built from either reports a
    recoverable error and then falls through to exactly the unbound state the rule
    exists to prevent. So either the fallback is restricted to forms that cannot
    fail that way, or a fallback that *completes* is itself an error. That choice
    is part of the proposal, not an implementation detail under it.
  - **It catches exactly what the `if`-bind catches, and no more.** That test is
    *truthy* **and** *shape fits*
    ([Conditionals](#conditionals-if-is-an-expression)), so the fallback fires on
    either half: a **value-level failure** — a `false`, a failed command, a
    nonzero `Status` — or a **shape miss**, including a non-list right-hand side,
    which `pattern_bindings` already treats as a plain no-match rather than an
    error, and a missed [`:match`](#destructuring).

    It inherits the binding asymmetry with it: a `false` binds nothing, while a
    nonzero `Status` **does** bind, since a status is a result rather than an
    absence. So `s = build() otherwise { puts "build failed: $s" }` can read `$s`
    in the fallback, exactly as the `else` of `if s = build() { … }` can.

    What it does **not** catch is a failure in *evaluating* the right-hand side:
    `[a b] = 1 + "x" otherwise { … }` aborts, because the type error happens
    before there is anything to test, and a type error is a channel-2
    interruption with [no soft twin](#error-handling), listed there beside
    div-by-zero and undecodable text. The line is *the test failed* against *the
    right-hand side never produced a value*. The flat form is the *flat spelling
    of the nested one*, so the two must catch the same set; if they diverged there
    would be two soft binds with different semantics, which is worse than the
    two-spellings problem this construct is trying to avoid.
  - **It applies to binding forms, not to lookups.** `[k v] = $s:match(…)` and a
    plain list destructure are the candidates. `$xs[i]` and `$m.key` already have
    `:get(…, default)` as their soft twin, and giving them a second one would be
    the two-spellings problem for no gain.
  - **It does not overlap the postfix guard.** `return unless $args:len > 0`
    *tests* and skips a statement; this *binds* and guards the rest of the block.
    Same instinct, different scale — and keeping the fallback word off `else` and
    off `guard` leaves both existing spellings meaning exactly what they mean now.
  - It adds a third column to the strict/soft table in
    [Error handling](#error-handling): strict (errors), soft-nested (`if`-bind),
    soft-flat (fallback-bind).
- **Requiring `else` where an `if` yields a value — reopened; no lean, and the
  obstacle is not the one on record.**
  [Conditionals](#conditionals-if-is-an-expression) settled this *lenient*: a lone
  `if` is a valid expression and yields `""` when false.
  [Error handling](#error-handling) then concedes what that costs — `""`-as-nothing
  "is indistinguishable from a real empty string and flows downstream under
  no-null, so a no-`else` `if` is the one place mesh hands you a silent empty that
  a destructure would refuse." A language whose headline is that
  [absence is loud](INTRO.md) manufacturing a quiet empty string at all is the
  case for reopening, and it is the document's own words.

  **"The one place" is wrong, and this document names two more.**
  [Matching](#matching-match) records that a `match` with no arm hit yields `""`
  too — "like a no-`else` `if`" — with totality for non-`_`-exhaustive matches
  left *(open)*; lenient was its position before this, and is now coupled to the
  question here rather than standing as a current lean of its own.
  [Functions](#functions) adds a third: a function with **no expression to
  yield** — an empty body, or a bare `return` *before anything ran* — results in
  `""` with status `0`, "the same 'nothing produced, nothing failed' answer a
  no-`else` `if` gives." A bare `return` after something has run is not a case:
  it carries the result so far, so `func f() { 42; return }` answers `42`.

  **Two of the three are one question; the third is not, and the difference is
  what the rule should key on.** The `if` and the `match` produce their empty on a
  path *the author did not write* — a condition that failed, an arm that did not
  hit. That is one question wearing two keywords: *must a value-producing
  construct cover every path?* Requiring `else` on `if` while leaving `match`
  lenient would close half of it and leave the language with two rules for one
  idea, so whatever answer this gets, the `match` totality question should get the
  same one and the two are best decided together.

  **The coupling is over *value-producing* uses only.** A statement-position
  `match` discards its value and runs arms for effect
  ([Matching](#matching-match)), so an unmatched one produces no empty for
  anything downstream to receive — exactly as a statement-position lone `if`
  produces none, which every option here still permits. Coupling the whole
  totality question would make effect-only dispatch exhaustive as a side effect,
  which nothing in the silent-empty argument asks for. The rule keys on *is this
  value used*, in both constructs, or it over-reaches in both.

  The function producer is a different animal, and it splits in two. An **empty
  body** is visible at the definition rather than hiding on an unwritten branch,
  so it is already the *asking* half of the
  [strict/soft pairs](#error-handling) — `func f() { }` is a stub, not a missed
  case. A **bare `return` before anything ran** is less clear-cut: the author
  wrote the exit, but "the result so far" is implicit, so the *path* is theirs
  while the *value* on it is not. `func f(_c) { if $_c { return }; 1 }` answers
  `""` on one path and `1` on the other, and nothing in the source says the first
  was intended.

  Neither belongs in *this* fix, and for the second one the reason is not the
  stub argument: **a totality rule over `if` and `match` would not catch it
  anyway.** The enclosing `if` there is statement-position, which every option in
  the table still permits. Catching it needs a different rule — *every path of a
  value-returning function must produce a value* — which is return-type analysis
  rather than branch totality, and a materially larger question than the one
  reopened here. Worth its own entry if it is ever wanted; not a reason to widen
  this one.

  **The recorded reason for declining is the weak part.** It was interactive
  brevity — the cost of the terse `tag = if $root { "[root]" }`. But conditional
  assignment is not really an interactive construct: it lives in configuration
  files, which are written once and read constantly. On the [goals](#goals)
  tiebreaker of whichever is better *to use interactively*, this shape barely
  registers, so brevity should not have carried the decision on those grounds.

  **The real obstacle is that "value position" is not a clean category in mesh.**
  The rule needs a boundary, and the obvious one — wherever the value is used —
  is not a syntactic thing here. A `func` body's tail produces the return value, a
  [`match`](#matching-match) arm's does, and a `for` body's does too, since the
  loop collects a value per completed pass. So a bare `if` at the end of a loop
  body is in value position by value-flow, and the rule either fires nearly
  everywhere or gets scoped syntactically to assignment right-hand sides, where it
  misses the func-tail and loop-body cases — which is where a silent `""` actually
  flows.

  The concrete casualty is in [`INTRO.md`](INTRO.md), and it is load-bearing:

  ```
  $sh.prompt.auth = func() { if not ssh-id-loaded() { style("SSH", fg: yellow) } }
  ```

  The prompt design *depends* on a lone `if` yielding nothing — that is how a
  segment omits itself. Under a broad rule every conditional prompt segment grows
  an `else { "" }`; under a narrow one func tails are carved out and the hole
  stays open exactly where it matters most.

  | Option | For | Against |
  | --- | --- | --- |
  | **Leave it lenient** (today) | The terse one-liner survives; nothing to migrate | Keeps both manufactured silent empties — the no-`else` `if` and the unmatched `match` — in the language whose pitch is that absence is loud |
  | **Require `else` wherever the value is used** | Closes the `if` half of it — the `match` half needs the totality question answered the same way | "Wherever the value is used" includes func tails, `match` arms and `for` bodies, so it fires far beyond the case it was aimed at, and breaks the documented prompt-segment idiom |
  | **Require `else` in binding position only** | Narrow, syntactic, and checkable at parse time | Already weighed and declined once under [Error handling](#error-handling); and it misses the func-tail case, so it buys explicitness without closing the hole |

  **What it buys is smaller than it first looks.**
  `tag = if $root { "[root]" } else { "" }` still produces an empty string
  indistinguishable from a real one, so nothing downstream improves. The gain is
  that `""` can no longer be reached by *forgetting a branch* — which is the same
  shape as the rest of the [strict/soft pairs](#error-handling), where softness is
  always opt-in. It does not make emptiness reachable only by asking: an empty
  function body still yields one, and that is fine, since writing an empty body
  *is* the asking. Real, but it is explicitness at the write site rather than
  absence-safety.

  **Independent of the flat soft bind above.** That construct is unambiguous with
  a distinct fallback word whether or not this changes, so the two should not be
  bundled: deciding a language-wide question about silent empties in order to tidy
  a corner of one new feature would be the tail wagging the dog. Requiring `else`
  *would* also disambiguate a fallback spelled `else` — that is why it appears in
  the other entry's list of ways out — but it should stand or fall on the `""`
  question alone.
- **Hook API — decided** ([Hooks and the prompt](#hooks-and-the-prompt)): hook
  points are insertion-ordered maps of named callables (the key is the handler's
  identity → re-source-safe, individually removable). Events `preprompt`,
  `preexec`/`postexec`, `precd`/`postcd`, `exit`; the prompt is a named, ordered
  segment map with the external renderer as one peer segment. Prompt MVP: **each
  top-level entry is a line** (implicit breaks between entries — no separator keys),
  an entry yields a renderable **or a space-joined flat list of pieces**
  (`puts`-style, empties dropped, each keeping its own style), with a keyed
  **sub-map** variant to name the pieces; `style` color; a deliberate-blank
  **`newline`** entry; the full-width **`rule`** entry; and the inline **`fill`**
  piece (right-align / trailing bar, multiple `fill`s split slack evenly, optional
  repeat-char; `rule` ≡ a whole-line `[fill("─")]`). A list is one line's pieces —
  **multiple lines are multiple entries** — and line structure is the map, not
  in-band `\n` (raw external output excepted, as dumb breaks). A segment slot holds
  an `&name` reference (late-bound) or a lambda; a bare word is an ordinary string.
  Remaining: transient collapse.
- **Structured prompt — direction decided** ([Hooks and the prompt](#hooks-and-the-prompt)):
  line structure is the **map**, not in-band newlines — **each top-level entry is a
  line** (implicit breaks; no `nl1`/`nl2` separator keys), a line's pieces are a
  **space-joined flat list** (or a keyed **sub-map** to name them), a deliberate
  blank line is a named **`newline`** entry, and **`fill`** is the inline
  right-align / trailing-bar piece (`rule` ≡ a whole-line `[fill("─")]`). A list is
  one line's pieces, so **multiple lines are multiple entries** — the keyed-map
  shape won over a whole-prompt list-of-lines (which would have made rows positional,
  not keyed). `rule`, `fill`, and `newline` are all in the MVP. **Remaining:**
  **transient collapse** of past prompts, now that lines are explicit and
  addressable.

**Foundational specification work.** The entries above settle *surface* features;
these five are the deeper contracts an implementation needs before code. They
are called out together because tooling, error recovery, and the Rust data
representation all depend on them; contracts still marked as needing a decision
remain under-specified.

- **Grammar and precedence — decided.** [`GRAMMAR.md`](GRAMMAR.md) is the parser
  contract: it covers adjacency/concatenation, modifier arguments, value calls,
  ranges, redirects, backgrounding, pipelines, conditional chains, postfix
  guards, and termination. In particular, `a | b && c &` backgrounds the whole
  `&&` list, while a redirect attaches to the nearest simple command. It
  describes what the parser accepts today, so what is still only design lives
  here rather than there.
- **Status lifetime.** Define exactly when `$sh.status` changes. Provisional: a
  pipeline's status is its **last stage**, every stage retained in
  [`$sh.pipestatus`](#variables-and-assignment); decide whether a **`pipefail`**
  option is in the MVP (leaning: available, default off). Specify the status after
  a plain assignment, a value expression, a parse error vs a runtime error, a
  background launch (`&`), and hook dispatch (already snapshotted/restored around
  hooks). mesh adds **no implicit `errexit`**; interactive and `source`d
  configuration errors therefore need an explicit recovery rule (see failure
  classes below) rather than unpredictable termination.
- **Condition truthiness — needs a table or a narrowing.** Ordinary `if` / `while`
  accept a bool or a command status; the [assignment-condition](#conditionals-if-is-an-expression)
  additionally calls the RHS "truthy," which needs a per-type table or should be
  narrowed. Leaning: **narrow it to the status view** — bool `false`, a failed
  command, and a nonzero `int` are false; everything else (including `""`, `[]`,
  `[:]`, and any non-empty value) is true — so truthiness is never
  content-emptiness, and pattern-fit stays the separate gate. That keeps it
  consistent with the result/status model and `gets`'s truthy `""`, and avoids
  inventing collection-truthiness. Write the table out explicitly for every value
  type.
- **Text vs bytes — the encoding model.** Decide whether a mesh string is an
  arbitrary **byte string** or guaranteed **UTF-8**; how undecodable filenames and
  command output are represented (leaning: bytes that round-trip losslessly, so a
  non-UTF-8 path survives capture → argv unharmed); what a **"character" index**
  means (byte / scalar value / grapheme); and which operations require text
  (case-fold, display width, parsing) versus bytes (pipes, captures, argv, paths).
  Leaning: a string is a **byte string that is usually UTF-8** — byte operations
  never decode, text operations decode on demand and **fail loud** on an invalid
  sequence. This must precede the Rust representation and is essential on Unix,
  where paths are not guaranteed UTF-8.
- **Failure classes — mostly settled** ([Error handling](#error-handling)). The
  execution model is now written up: **two channels** (value-level failure vs
  fail-loud errors), **strict-by-default / soft-by-opt-in** with a strict/soft table,
  the reconciliation that a no-`else` `if` is a *soft* form (so it is consistent
  with fail-loud), and the **boundary-catch** recovery rule (interactive line,
  `source`, prompt/hook/completion, script). **Remaining open:** whether to expose a
  **user-facing** `try` / `catch` or `?(…)` capture for channel-2 errors with no
  soft twin, or ship only the boundary-catch + soft twins for the MVP (leaning: no
  user catch in the MVP).
- **Status as a projection of the value — reopened, then decided: `status` becomes
  a *value*. Implemented.** *(Resolution at the end of this entry; the exploration
  that led there is kept because it is what rules the alternatives out. The
  checklist at the end of the entry has landed in full — the type, the builtin,
  the channel words, the typed channels, and the end of "no value" — so the
  present-tense claims about the implementation below are describing it as it now
  is, except where one says otherwise.)*
  [Value and status are separate channels](#functions) is marked
  *decided; shipped*, and the objection it records is that deriving one from the
  other "makes every integer-returning function a landmine." Reopened on the
  report that the separation is **too complicated for what it buys**, with the
  shell reflex as the motivating case:

  ```mesh
  func f() { return 0 }
  if f { puts true }                          # want: true

  func g() { return 1 }
  if g { puts true } else { puts false }      # want: false
  ```

  Today the second prints **`true`**. `return 0` and `return 1` are both
  successes, because only `false` fails — so the two spellings of failure
  disagree (`return false` fails, `return 1` does not) and `return` cannot
  **name** a status. Bare `return` does *propagate* one (the row above: it carries
  the last status, so `func g() { sh -c "exit 3"; return }` reports 3); what has no
  spelling is choosing the code an operand-bearing `return` leaves.

  Seven facts constrain the choice, each checked against `main` (`88fa209`) rather
  than assumed:

  - **The projection is still there; it lost one arm.** `status_of`
    (`crates/mesh-core/src/repl.rs`) maps `false` → `1` and everything else → `0`.
    Restoring the old model is restoring `Value::Integer(n) => n.rem_euclid(256)`,
    dropped in `0b107f6`. This is not a new mechanism, it is a one-arm revert plus
    whatever fallout the arm has.
  - **The landmine is real and a test caught it once.** `$x:split("-"):len || puts
    three` fired *because* a length of 3 read as a failing status (`0b107f6`). Any
    restoration re-arms exactly that case.
  - **The value channel is unaffected either way.** Under the projection
    `return 5` still binds `5` at `n = f()`; the derived status mostly shows up
    where the value was being discarded anyway — bare command position, `&&` /
    `||`, `$sh.status`. That narrows the blast radius considerably and is the
    strongest argument for restoring it.
  - **`:capture` is the exception, and it is an API.** A
    [capture record](#calling-for-a-value-and-lambdas) reports `.value` and
    `.status` *together*, and `capture_call` derives that status through the same
    `status_of` (`crates/mesh-core/src/repl.rs`), so restoring the arm changes
    records that keep their value: `func f() { return 3 }; f():capture` reports
    `value=3 status=0` today and would report `value=3 status=3`. Any restoration
    has to say what a capture record means for an int — the one place the two
    channels are read side by side, where a derived status is most visibly
    redundant with the value sitting next to it.
  - **`source` is a second API affected.** `return` at the top level of a
    [sourced file](#startup-and-invocation) is a shipped contract, and
    `make_return` derives its code through the same `status_of` before
    `run_sourced_text` publishes it as the `source` command's status. So a file
    containing `return 3` leaves status `0` today and would leave `3` after the
    revert. Together with `:capture` that makes three fallout sites, not one:
    the arm is small, its reach is not.
  - **Position already decides which channel is read.** `if f() { … }` on an int
    errors (*an int is not a condition*) while `if f { … }` reads the status —
    true today, and true under the projection. So the projection does **not**
    create a value-position/status-position disagreement: the value position
    refuses to branch at all. The inversion only appears once a comparison is
    written to read the int back as data (`if v = f(); $v != 0`, option E), where
    `0` is the false-ish one and `if f` treats that same `0` as true. Narrow, but
    it is the shape the [truthiness rule](#conditionals-if-is-an-expression) spent
    its budget removing, so E buys its escape hatch at that price.
  - **`0`–`255` is the whole status range.** `return 256` would be
    indistinguishable from `return 0`, and `return -1` from `return 255`, because
    a status is 8-bit and the value channel is not.
  - **`if v = f(); $v != 0 { … }` does not parse** — `syntax error: expected {`.
    The [if-binding](#conditionals-if-is-an-expression) has only the
    `if lhs = rhs { … }` form, which tests presence-and-fit, not an arbitrary
    condition over the bound value.

  | Option | `return N` | For | Against |
  | --- | --- | --- | --- |
  | **A. Restore the full projection** | value `N`, status `N` mod 256 | Matches the POSIX-shell reflex (sh/bash/zsh — though *not* the rich-value shells: nushell and PowerShell both return `1` as data); `return 0` / `return 1` do the obvious thing; removes the `return false` / `return 1` asymmetry | Re-arms the `:len` case above — `count \|\| warn` fires on a *non-empty* result; `return 256` ≡ `return 0`; changes `:capture` records and the status of a sourced `return N` |
  | **B. Keep the channels, close the known gaps** | value `N`, status `0` | No landmine; `fail` already names statuses; status quo plus [`ok`](#open-questions) and the `not` fix | Leaves `return 1` succeeding — does not answer the report |
  | **C. Project a distinct status *type*** | value `N`, status `0`; `fail N` / a `status` value projects | Total and unambiguous — no type collision to arbitrate | `return 1` still succeeds, exactly as today — an int operand cannot name a status, so the asymmetry the report is about survives |
  | **D. Narrow projection — only `0` and `1`** | `0`/`1` project; other ints are data | Covers the literals people actually write | Worse than uniform: a count of 1 fails while a count of 3 succeeds |
  | **E. A + the Go-shaped `if init; cond`** | as A | Gives the landmine a *spelling* rather than a warning — `if v = f(); $v != 0 { … }` reads the int back as data | New grammar; does nothing for `&&` / `\|\|` chains |

  **`fail` survives option A** — it is not made redundant by it. `fail N` leaves
  the value `false` where `return N` leaves the integer `N`, so even with equal
  statuses the two stay distinguishable at a value call and at
  `f():capture`'s `.value`. "Fail with code N and produce no result" would still
  need its own verb unless A also proposed changing `fail`'s value semantics,
  which it does not.

  **A and E were the leaning, and are now declined.** The POSIX reflex is real,
  but the counterexample is a one-liner nobody would call exotic:

  ```mesh
  func g(_n) { $_n + 1 }
  g 1                    # value 2 — and under A, status 2, i.e. a failed command
  ```

  Any function that computes a number and is also run bare fails whenever the
  number is nonzero. That is the `:len` case again, arriving through arithmetic
  rather than a modifier, and it is enough on its own — it is a defect in the
  *design*, not a migration cost. The knock-on effects on `||` chains,
  `:capture` and sourced `return N` are further evidence of the same thing
  rather than separate objections; that two of them touch already-written code
  is beside the point while the language has no users.

  ### Decision: a status is a value, so naming one needs no new syntax

  What the reopening actually surfaced is that the two channels were never the
  problem — the problem is that **a status has no spelling**, so `return`'s
  operand has to be read as one by type. Give it a spelling and the channels
  collapse into one without any of A's fallout.

  - **`status(N)` is a builtin returning a `Status` value.** It is an ordinary
    value: bindable, passable, returnable. `file-not-found = status(5)` then
    `return $file-not-found` works, which is the case that settles this as a
    *type* rather than as syntax.
  - **`return status N` is syntax, and sugar for `return status(N)`.** Both ship
    in the MVP: the builtin is the mechanism, the channel word is the spelling.
    `status` and `value` are **channel words** recognized only directly after
    `return`, which is not an exception to the
    [mode rule](#calling-for-a-value-and-lambdas) — `return` is not a call site,
    it is a control keyword, and mesh's control keywords already carry their own
    small grammars (`fail 5`, `exit 5`, `break`). A channel word belongs to that
    family.

    The **channel words** are positional, so they reserve nothing on their own:
    `return status` with no operand is an error naming the missing code, rather
    than the string `"status"` it binds today.

    **But `status` the builtin does take the name.** Builtins are already
    reserved against `func` — `func puts` is refused today with *"`puts` is a
    reserved name and cannot be a function name"* — so adding `status(N)` makes
    `func status` illegal by the existing rule, with no special case needed. That
    is the price of the builtin, and it is worth stating plainly rather than
    leaving as an open question: a fairly ordinary word leaves the user's
    namespace. (`value` is a channel word only, with no builtin behind it, so
    `func value` stays legal.)

    **An attached `(` is a call, never a channel word** — the lookahead rule
    that keeps `func value` legal from making `return value(5)` ambiguous. On
    its face that line reads two ways: the channel word applied to a
    parenthesized `5`, or a call to the user's `value`. It is the **call**, and
    the discrimination is one mesh already trains everywhere else — `f arg` and
    `f(arg)` are different things, separated by exactly that attached paren.
    (Not the [mode rule](#calling-for-a-value-and-lambdas) itself, since
    `return` is no call site; the same *shape*, reused as a one-token
    lookahead.) So:

    ```mesh
    return value 5      # channel word — the value 5
    return value(5)     # a call to whatever `value` names; an error if nothing does
    ```

    Reserving `value` would also close the ambiguity, and is declined: the
    lookahead costs one token and takes no name. It falls out consistently on
    the other side too — `return status(5)` is likewise a plain call, to the
    builtin, which is *why* it and `return status 5` agree without a special
    case. The two spellings coincide there only because a builtin backs the
    name; nothing requires that of `value`, and adding an identity `value(X)`
    builtin to force the symmetry would buy nothing and cost the name.
  - **`status(N)` takes `0`–`255` and errors outside it.** `fail` already sets
    the precedent — `fail 300` and `fail 0` both fail with *"status must be
    between 1 and 255"* — so construction **rejects** rather than normalizing.
    Silent truncation is exactly the `return 256` ≡ `return 0` trap this entry
    declines elsewhere. `status(0)` is legal where `fail 0` is not: zero is a
    perfectly good status to name, while a `fail` that succeeds is a mistake.
  - **Bare `return X` means the value `X`, and does not warn.** This is the
    load-bearing rule. `func f() { 5 }` and `func f() { return 5 }` must be the
    same thing, so if bare `return 5` meant a status, writing `return` in front of
    a tail expression would silently change its meaning — the exact trap being
    removed.

    **`return value X` is the explicit spelling of the same thing**, carrying no
    semantics of its own. It costs nothing as a channel word — there is no
    `value(…)` builtin to squat the name — so it is kept for the reader who wants
    to say it out loud, while `return status` is the half that does the work.

    **And `return 5` does not warn.** A diagnostic is justified where a spelling
    is *ambiguous*, and after the channel words each spelling says what it is.
    Warning would also have to fire on `func f() { 5 }` to be consistent, since
    it is the same program, and it would fire on the common case — returning a
    count, a port, an index — which is how a diagnostic gets trained away. mesh
    has no advisory-warning precedent either; its diagnostics are errors that
    name the fix. The residual cost is accepted and stated rather than papered
    over: someone arriving from bash writes `return 1` for failure and gets
    success.
  - **`$sh.status` stays a projection, now a total one**: `Status(n)` → `n`,
    `false` → `1`, `true` → `0`, everything else → `0`. That is
    `status_of` (`crates/mesh-core/src/repl.rs`, today
    `u8::from(matches!(value, Value::Boolean(false)))`) plus a `Status` arm —
    still no `Integer` arm, which is what keeps `g 1` above succeeding.
  - **`fail N` survives as a *validating* wrapper** over `return status(N)` — not
    exact sugar, and the difference is load-bearing. `status(N)` accepts `0`,
    since naming a zero status is reasonable; `fail 0` is refused, since a `fail`
    that succeeds is always a mistake. So `fail` is `return status(N)` **plus the
    constraint `N ≥ 1`**, and calling it plain sugar would silently delete that
    diagnostic. It stops being a separate *mechanism* while keeping its own
    precondition.

  | written | value | `$sh.status` |
  | --- | --- | --- |
  | `return 5` / `return value 5`, or tail expression `5` | `5` | `0` |
  | `return status 5` ≡ `return status(5)`, or tail `status 5` | `Status(5)` | `5` |
  | `fail 5` — now sugar for the same | `Status(5)` | `5` |
  | `return false` | `false` | `1` |
  | `return` (bare) | result so far | last status |

  #### Match arms: what the channel words cannot reach

  The two [arm forms](#matching-match) are affected in opposite ways, and only
  one of them is a problem. All of the following measured on `main`.

  - **`=> value` needs no change.** A bare word is a scalar literal, so
    `match $v { a => markdown }` binds `"markdown"`. That is right and stays.
    The consequence is that this form can *never* be reached by a channel word:
    `=> status 5` is not a `return`, so there is nothing to attach the word to.
    If an arm is ever to yield a status **as the match's value**, the only
    possible spelling is the value form, `=> status(5)`.
  - **`=> { block }` already leaks a status as data, and that is the real
    problem.** In expression position an arm ending in a command yields that
    command's status as a bare **int**:

    ```mesh
    x = match $v { a => { /bin/false } }   # x is the int 1
    puts ($x + 1)                          # 2 — arithmetic on an exit status
    ```

    That is the command-tail defect reached through an arm, and the rule below
    settles it the same way: the arm yields `Status(1)`, so `$x + 1` becomes a
    type error rather than `2`. Arms were already documented to yield a command's
    status in expression position — what was wrong is only that they yielded it as
    a bare **int**.
  - **`fail` in an arm unwinds the whole function, not the arm.** `func f() { …
    match $v { a => { fail 2 } }; puts after }` never reaches `after`, and `f`
    leaves status 2. So `fail N` and `return status N` are **function-level
    verbs**: neither can let an arm *yield* a failure while the function
    continues.
  - **The builtin closes exactly that gap, and this is why both spellings ship.**
    `status 5` in a block arm is an ordinary call in command position, so it
    evaluates rather than unwinding:

    ```mesh
    x = match $kind {
      missing => { status 5 }     # yields Status(5) — the arm produces it
      _       => { status 0 }
    }
    ```

    The `=> value` form is served the same way, by `=> status(5)`. So the channel
    word serves `func` bodies and the builtin serves arms; neither covers both,
    which is the argument for having them together rather than in sequence.

    **Both arm spellings work, and both are kept.** `=> status(5)` and
    `=> { status 5 }` yield the same `Status(5)`, and neither needs a rule of its
    own to do so — the first is an ordinary value call, the second an ordinary
    command-position call at a block's tail. That is the same two-spelling
    pattern mesh already has for `f arg` / `f(arg)` and `--flag` / `flag: true`,
    so it is consistency rather than redundancy; a `status` that had needed a
    carve-out to work in an arm would have been the warning sign. The value form
    is strictly more general, since it also fits the `=> value` arm where no
    block exists.
  - **A bare inner call discards the callee's value, and should keep doing so.**
    `func outer() { inner }` does not reach `inner`'s `42`. That is the
    [mode rule](#calling-for-a-value-and-lambdas) working as designed: `inner`
    runs it, `inner()` reaches its value. Forwarding the value instead would make
    bare command position mean something different for a mesh function than for
    an external, which is the uniformity this entry is trying to protect. What it
    yields *instead* is `Status(0)` per the rule below — not the bare int `0` it
    produces today.

  **Cross-type `==` reports rather than answering `false`, and `Status` joins the
  numeric class** — settled under
  [Comparison across types](#comparison-across-types), which is canonical for
  this. In short:

  ```mesh
  $s == 0            # true on success — a status compares to an int by its code
  $s == status(0)    # the same question, spelled with a status
  $s:code == 0       # and the same again, spelled with the integer
  $s == true         # error; a status is a condition, so `if $s { }` is the test
  $s > 1             # still an error; ordering is not settled by that entry
  ```

  An earlier draft of this line read "`Status` carves no exception in comparison"
  and made `$s == 0` an error naming the other two spellings. The exception is
  real, and it is not special-casing: a status has a lossless projection to a
  number, which is what class membership takes.

  Two earlier drafts of this entry tried to fix the ergonomics locally — first by
  erroring on `Status`-vs-int alone, then by **making the two compare equal**.
  Both were set aside on the grounds that a `Status` admits **two** projections,
  its code and its success, both reachable as values (`$s:code` and `not not $s`),
  so an equality respecting both would force `0 == true` by transitivity.

  **The second draft was right and the reasoning against it was incomplete**, which
  is why the [comparison entry](#comparison-across-types) has since revived it.
  "Respecting both is contradictory" does not imply "respect neither" — respecting
  exactly one breaks the chain too, since `0 == true` is underivable while
  `status(0) == true` stays refused. And the choice between the two is forced
  rather than arbitrary: the code is injective while success collapses 255 failing
  codes into one `false`, and a lossy projection cannot carry equality without
  making `status(1) == status(2)`. So the general rule is stated as **equivalence
  classes** — a type joins at most one, through a lossless projection into it —
  rather than as pairs, which do not close under transitivity. `Status` joins
  the numeric class alongside `Integer`; whatever joins later inherits the same
  equality rather than needing a fresh ruling.

  The cost the compare-equal draft carried is real and is now accepted rather than
  avoided: `Value`'s hand-written `PartialEq` and `Hash` must agree, so a `Status`
  hashes as its code and `[status(0) 0]:dedup` collapses to one element. What that
  buys is the seam closing everywhere at once — `match f() { 0 => … }` against a
  status now *takes* its arm, because arms are built on the same equality, so no
  separate arm rule is needed and no heterogeneous `match` is foreclosed.

  **Rendering is the bare number** — `status(5)` displays as `5`. A "`status 5`"
  rendering was considered on the grounds that `5` is confusable with the
  integer, and rejected: mesh **already** loses type at the byte boundary for
  every value, since the int `5` and the string `"5"` both render `5`. The
  confusion this entry removes is in the language, not in the text, so a status
  needs no special treatment on the way out. "status 5" phrasing belongs to a
  diagnostic's formatter, not to the value.

  **[`:repr`](#modifiers) gives `status(5)`, and that is forced rather than
  chosen.** Its contract is round-trip — *parsing the result yields the same
  value, and of the same type* — so `5` is inadmissible there, since it would
  read back as the integer. It is the same rule that writes a string as `'42'`
  rather than `42`. `status 5` fails it too, being the command-position spelling
  rather than a value literal. So display and `:repr` diverge here exactly as
  they already do for `42` / `'42'`, by a rule that predates this decision.

  Argv and interpolation take the same verdict, and `Status` is listed in the
  [byte-boundary table](#spread--flattening) beside `int` for it:
  `cmd status(5)` passes `5`. Nothing here is left to the implementer.

  *(The display form is listed under **Still open** below; `:repr` and argv are
  both pinned and are not part of that question.)*

  **`$sh.status` is itself a `Status`, not an int.** The first draft of this
  entry kept it an int on the grounds that prompts and existing configuration
  would be untouched — a compatibility argument, and worth nothing while the
  language has no users. The long-term answer is the consistent one, and there is
  a concrete reason beyond tidiness:

  ```mesh
  func wrapper() { some-cmd; return $sh.status }
  ```

  Forwarding the last status is a natural thing to write, and with `$sh.status`
  an **int** it returns the *number*, successfully — the exact failure this entry
  exists to remove, sitting at the likeliest place to meet it. As a `Status` it
  forwards correctly, and a prompt segment still prints `5` by the rendering rule
  above. This used to cost `$sh.status == 0`, which read silently false and then,
  under a later draft, reported. It costs nothing now: a status compares to an int
  by its code, so the reflex spelling is simply **true on success** — see
  [Comparison across types](#comparison-across-types). `if $sh.status { … }`
  remains the idiomatic test.
  **`$sh.pipestatus`** becomes a list of `Status` for the same reason.

  #### A `Status` is a condition — naming what was already allowed

  [Condition truthiness](#conditionals-if-is-an-expression) admits "a bool or a
  command — and nothing else," so as written this decision would make
  `if $sh.status { … }` a **loud error**, a `Status` being neither. That is
  incoherent, and the rule needs a third arm:

  ```mesh
  if grep -q foo file { … }   # allowed: a command, branched on its status
  if grep(foo) { … }          # the same status, now named as a value
  ```

  A command is admitted in a condition *precisely because* its result is a
  status. Once a status is a first-class value those two lines are the same
  question, and refusing the second would split the uniformity this decision
  exists to create. So **a `Status` is a condition, true iff its code is `0`.**

  This is not a truthiness exception. The truthiness rule's point is that no
  value is *coerced* into a truth — an int, a string, a list have no truth to
  read. A `Status` is different in kind: success and failure are the whole of
  what it encodes, exactly as for a `bool`. Admitting it names something the
  rule already permitted through the command arm rather than widening what
  counts as true.

  It also matters ergonomically: `if $sh.status { … }` is the natural short
  spelling for "did that work," and without this arm the only spellings would be
  `$sh.status == status(0)` or `$sh.status:code == 0`. That mattered most when
  `$sh.status == 0` did not work; it stays the idiomatic form now that it does,
  since asking a condition directly beats comparing it to a literal.

  **`and` / `or` / `not` follow automatically**, and that is forced rather than
  chosen: they are documented to "ask the same question and refuse the same
  values" as a condition, being boolean operators rather than a second
  truthiness system. Since `if` now admits a `Status`, so do they — `status(1)
  or true` is well-formed. Leaving it open would contradict a rule the document
  already states by reference.

  **`&&` / `||` need no rule at all**, which is worth saying since their absence
  here reads as an omission next to `and` / `or` / `not`. They are the *command*
  chains, branching on exit status rather than on a value, and every statement
  already has a status — a bare `status(1)` leaves `1` through the projection
  above, so `status(1) || puts fallback` runs the fallback with nothing added.
  The two kinds stay [separate](#tests-and-comparisons) exactly as before; this
  decision touches only the value side.

  One consequence this entry does *not* decide: whether **`exit`** accepts a
  `Status` beside the int it takes today, since `exit $sh.status` becomes the
  obvious way to leave with the last status. *(In the implementation the question
  did not arise: `exit` reads its operand as a **word**, and a `Status` renders as
  decimal there, so `exit $sh.status` keeps working without `exit` knowing the
  type. `fail` does read a value, and takes a `Status` beside the int — recorded
  in `TODO.md` under decisions needing review.)*

  #### "No value" stops existing, which is what unifies the rest

  `val = f()` has to do *something* for any `f`, and mesh has no null type to
  reach for. Today it does four different things, measured on `main`:

  | case | `val = f()` |
  | --- | --- |
  | `func e() { }` — empty body | `""`, per *"there is no null to invent"* above |
  | `func p() { /bin/false }` — command tail | the **int** `1` |
  | `grep(zzz)` — external value call | **error**: *a command has no return value* |
  | `grep(zzz):capture` | `.value` is **absent from the record** |

  Four answers to one question. The `Status` type collapses them, because a
  command's result is not *missing* — it is **how the command went**:

  - **Every call yields a value.** There is no valueless call and therefore no
    null to invent.
  - **For anything command-shaped, that value is a `Status`.** A command tail
    yields `Status(n)`; `func p() { /bin/false }; p()` is `Status(1)`, not the
    int `1` and not nothing.
  - **`grep(zzz)` returns `Status(1)` instead of erroring**, so `f` / `$(f)` /
    `f()` finally mean the same three things for an external as for a function —
    the uniformity goal this whole area is chasing. This is why "do externals
    gain `f()`?" is **not** an optional extra: it is forced by every call having
    a value.
  - **`:capture`'s `.value` is always present**, so the record has a fixed shape
    rather than one that depends on what was called.
  - **An empty body still yields `""`.** Nothing ran, so there is no status to
    report, and the existing answer stands unchanged.

  That also retires a naming problem: `.value` on a capture record and the value
  `return value X` fills stop being two ideas that happen to share a word. They
  are the same channel, read two ways.

  *(Honest history: an earlier draft asserted the command-tail change on the
  weak grounds that it "types an int correctly," was rightly challenged — an
  external genuinely has no mesh value of its own — and was withdrawn. It returns
  here on the stronger footing that a call with no value is unrepresentable
  without a null. The bug was always the **int**, not the existence of a value.)*

  [Match arms](#matching-match) were already documented to yield a command's
  status in expression position, so they agree with the command-tail rule
  without changing. The [function table](#functions) had a third answer —
  "none" for a command-tailed body — and this entry updates that row to
  `Status(n)` rather than leaving the document holding two.

  **What changes, as an implementation checklist.** Not a compatibility
  analysis — the language has no users, nothing here is owed backward
  compatibility, and "is this additive?" is the wrong question to judge a design
  by. What follows is simply the list of places that have to move, and the places
  worth re-reading afterwards to confirm the result is coherent.

  `return` itself is unaffected: `func f() { return 3 }; f():capture` reports
  `value=3 status=0` before and after — the same *code*, though `.status` holds
  it as a `Status` per the typing below — and a sourced `return 3` still leaves `0`
  — `return status 3` there is new reach rather than a change. Everything that
  *does* move is below; an implementation that does only part of it leaves the
  document's rules disagreeing with each other, which is the failure this list
  exists to prevent.

  **New surface:** the `status(N)` builtin, the `Status` value type with its
  `:code` modifier and its `:repr`, the `status` / `value` channel words after
  `return` with the attached-`(` lookahead, and `Status` as a third arm of the
  condition rule (which `and` / `or` / `not` inherit).

  - **`fail N`** leaves `Status(N)` where it leaves `false` today, since it
    becomes a wrapper over `return status(N)`. Only the value moves: the `N ≥ 1`
    check it validates is unchanged, so no program that ran before stops running.
    This is the one value change the decision actually makes, and it is
    deliberate content rather than fallout.
  - **Every status channel becomes a `Status`** rather than an int, for the
    forwarding reason given above — the type lands on all of them or on none,
    since one left an int is one more place a handler forwarding it reports
    success. Enumerated, so the list can be checked rather than trusted:

    | channel | where |
    | --- | --- |
    | `$sh.status`, `$sh.pipestatus` | [`$sh`](#variables-and-assignment) |
    | `:capture`'s `.status` | [the capture record](#calling-for-a-value-and-lambdas) |
    | a finished job's `$j.status` | [job control](#job-control) |
    | **`jobdone`**'s third argument | [job control](#job-control) |
    | **`postexec`**'s second argument | [event hooks](#hooks-and-the-prompt) |
    | the **`exit`** hook's argument | [event hooks](#hooks-and-the-prompt) |

    The three **hook arguments** are the easiest to miss and the worst to leave
    behind: a handler is exactly where someone writes `if $status != 0`, and an
    int there **mis-forwards** — the argument is passed on as data rather than as
    a failure, which is the defect this whole entry exists to remove. The
    comparison is *not* part of the argument: under [Comparison across
    types](#comparison-across-types) a status and an int compare by the code, so
    `if $status != 0` reads correctly either way. (An earlier draft of this
    paragraph claimed an int made that test "always true", inherited from the
    superseded refusal; it never applied to `!= 0` against an int at all.) The
    shipped examples still get updated: `docs/PROMPT.md`'s status line,
    `docs/REFERENCE.md`'s `jobdone` handler, and `docs/COMPARISON.md`'s syntax
    sample now read the `Status` directly (`if not $status { … }`,
    `if $sh.status { … }`) instead of comparing against `0` — not because the
    comparison is broken, but because the condition rule makes it unnecessary,
    and the shorter form is the one to meet first.

  - **Every call yields a value, so the evaluator stops erroring** in the three
    places it does today. `grep(zzz)` returns `Status(1)` instead of *a command
    has no return value*; `puts(1 + 2)` binds `Status(0)` instead of the same
    error; and a command-tailed body yields `Status(n)` where a value call
    currently sees a bare int. These are the substance of ["no value" stops
    existing](#open-questions) above, not separate cleanups — an implementation
    that adds the `Status` type and the channel words but leaves these on the
    old contract has the type without the unification it exists for.
  - **A capture record's `.value` is always present**, including for an
    external, where it is absent today. That is what gives the record a fixed
    shape rather than one that depends on what was called, and it follows from
    the bullet above rather than being an extra decision.

  The command-tail *bug* — `p():capture` reporting `value=1 status=0` while bare
  `p` leaves `$sh.status` 1 — is listed under the defects above and wants fixing
  either way. What the decision adds is the **type**: once fixed, the value is
  `Status(1)`, not the int `1`.

  Checked against `main` (`199d4ef`) before the work: `status` was a free name —
  `func status(_n) { $_n }` defined and called cleanly — so the builtin needed no
  keyword and the channel word needed no reservation. `return status 5` was a
  syntax error and `return status` bound the **string** `"status"`, which is
  exactly why the channel word needed parser support rather than falling out of
  existing rules. It has it now, and `func status` is refused by the existing
  builtin rule.

  **It does not do the motivating example, and that is intended.** `func g()
  { return 1 }; if g` takes the *true* branch, because `1` is data; the failing
  spellings are `return status 1` (≡ `return status(1)`) and `return false`. The report that opened this
  entry asked for the opposite, and the reversal is deliberate — `g 1` above is
  the reason.

  #### `.value` and `.status` now say the same thing — TODO

  For anything command-shaped or status-returning, a [capture
  record](#calling-for-a-value-and-lambdas)'s two result fields carry the same
  information:

  ```mesh
  func f() { return status 3 }
  r = f():capture     # .value = Status(3)   .status = Status(3)
  grep(zzz):capture   # .value = Status(1)   .status = Status(1)
  ```

  The invariant that makes the redundancy safe is that **`.status` is exactly
  the `Status` whose code is `status_of(.value)`** — a derived view, never an
  independent channel, the same projection `$sh.status` uses. The wrapping is
  load-bearing rather than pedantic: `status_of` yields the bare **`u8` code**
  (`Status(3)` → `3`), so reading the invariant as plain equality would type
  `.status` as an int and reinstate the forwarding bug the typing exists to
  prevent. `status_of` stays the code extractor — it is what the OS is handed on
  exit — and the *fields* hold `status(status_of(v))`. The defect recorded above (`value=1` beside
  `status=0`) is precisely those two disagreeing, which the invariant forbids.

  **`.status` is a `Status`**, for the same reason `$sh.status` is: it holds a
  status. An earlier draft of this paragraph hesitated over that, on the grounds
  that `$r.status == 0` would then read silently false — but that objection
  applied **identically to `$sh.status`**, which the entry types without
  hesitation, so using it to block one field and not the other was simply
  inconsistent. The objection has since been removed at the root rather than
  answered: `$r.status == 0` is **true** on success, since a status compares to
  an int by its code (see [Comparison across
  types](#comparison-across-types)).

  What remains genuinely worth flagging is the ergonomic risk you would expect: a
  reader reaches for `.value` expecting data and finds a status, or checks
  `.status` when the result they wanted is in `.value`. The two fields are one
  channel with two views, not two answers, and the record's documentation should
  say so plainly rather than listing them as peers.

  #### Still open

  Everything below is genuinely undecided. None of it blocks the syntax or the
  builtin, and each is cheap to change later; they are listed together so they
  are visible rather than scattered through the prose above.

  - **The display form.** `status(5)` shows as `5` for now. `status 5` and
    `status(5)` stay defensible for `puts` and want re-testing once statuses are
    printed in anger — prompts, diagnostics, logs — where a bare `5` may read as
    noise. Pinned either way: `:repr` is `status(5)` by round-trip, and argv is
    `5` by the byte-boundary table.
  - **What else a `Status` carries.** `:code` gives the integer. Whether it
    also gets a bool view — and how that relates to the [`ok`](#open-questions)
    status-to-bool word below — is unsettled.
  - **Reserved names in general.** `status` becomes reserved automatically, which
    is accepted — but every builtin consumes an ordinary English word, and
    `status` is a good example of how ordinary. Whether that stays a flat
    reservation or gains an escape (a namespace, a shadowing rule, an explicit
    `builtin` prefix) is its own pass.

  Two **defects** were found while checking this entry, neither created by the
  decision. `1 == 1.0` is `false` on `main` while the [`:repr`](#modifiers)
  rationale and §"Floats" both say it is true. The *design* was never in doubt
  and the comparison entry does not reopen it — it only records the consequence
  that an integral float then equals the corresponding status, both being in the
  numeric class. The defect has its own cause: there is no float type yet, so
  `1.0` lexes as the **string** `'1.0'` and the comparison is a string one.
  Tracked with the float entry in `TODO.md`. The second one is
  **fixed**: `p():capture` reported
  `value=1 status=0` for a function whose command tail failed while bare `p` left
  `$sh.status` 1, and typing the command tail made the two agree
  (`value=Status(1) status=Status(1)`).

  #### Settled along the way

  - **Cross-type comparison across the whole language**, of which `Status` was
    just one instance. Whether `$r.status == 0` and `$sh.status == 0` work at all
    hung on it, which is why this sat in the open list. **Settled** by
    [Comparison across types](#comparison-across-types): mismatched types refuse,
    except within an equivalence class joined through a lossless projection, and
    a status's code is one. Both of those read **true** on success.
  - **Should `return` evaluate its operand as a command line** — so that
    `return status 5` runs `status 5` — rather than as a value expression?
    *Declined.* `return`'s operand is value context, where a bare word is a
    [string literal](#variables-and-assignment); that is the same rule that makes
    `x = greet` bind `"greet"` and that pins `=> markdown` to the string
    `"markdown"` in [match arms](#matching-match). Flipping it would make
    `return markdown` a command lookup and `return some-file.txt` an attempt to
    execute a filename. The parens already distinguish the two, so nothing is
    gained for the cost.
  - **Does `status` need reserving?** No special case — builtins are already
    refused as `func` names, so `func status` becomes illegal the moment the
    builtin exists, and the repo owner has accepted that. (The general question
    is listed above as still open.)
  - **Does a `Status` render to bytes?** Yes, as decimal digits, and it has a row
    in the [byte-boundary table](#spread--flattening) beside `int`: `cmd
    status(5)` passes `5`. It wraps an integer, so decimal is canonical exactly
    as for an int; the type governs projection and dispatch, not the byte form.
    Handle-like variants with no byte form (`Stream`, `Job`) were the alternative
    model and do not fit — a status has an obvious number.

  **The *status-to-bool word* below is largely subsumed** — though not by the
  projection question, which is what an earlier draft of this paragraph claimed.
  It is subsumed by the two rules above: a command-tailed function now returns
  `Status(n)`, and a `Status` is a condition, so the case `ok` was invented for
  writes itself:

  ```mesh
  func inside-project() { git rev-parse --git-dir }   # returns Status(n)
  if inside-project { … }                            # works — command position
  if inside-project() { … }                           # works — Status is a condition
  ```

  What `ok` was for was a predicate whose condition is a **command**, which had
  no spelling and had to write out `if … { return true } / return false`. That
  branch is now unnecessary.

  What survives is narrower still, now that `and` / `or` / `not` admit a
  `Status` as well: storing, negating and combining all work without a bool. So
  `ok` is left wanting a **bool specifically** — for an API that demands one, or
  for a reader who prefers `true` to `Status(0)` — which is a preference rather
  than a gap. *(`return
  CMD` remains unrelated either way: `return /bin/false` yields the string
  `"/bin/false"`, since a bare word in value position is a literal.)*

  Two adjacent questions from the same report, both answered against `main`:

  - **`match f() { … }` works and needs nothing.** It matches on the **value** —
    `func f() { return 7 }; match f() { 7 => … }` takes the `7` arm. `match` is a
    value construct, so it reads the value channel, which is right under either
    model. (`switch` is not a mesh keyword; `match` was
    [decided](#matching-match) and `switch` explicitly declined.)
  - **`match f { … }` is a trap.** A bare word in expression position is a
    [string literal](#variables-and-assignment), so the subject is the string
    `"f"` — the function never runs and the arm falls to `_`. Same shape as
    `x = greet` binding `"greet"`, but here nothing forces the parens, so it is
    silent. Worth a diagnostic: a `match` subject that is a bare word naming a
    function in scope almost certainly meant `f()`.
- **Is `f()` the right call spelling, or would `(f)` be? — open; leaning keep
  `f()`, question the comma instead.** `f()` is forced today because a bare word
  on an RHS is a [literal string](#variables-and-assignment) (`x = greet` binds
  `"greet"`), so reaching a function's value needs a marker; `(f)` is plain
  grouping and evaluates to the string `"f"` on `main`.

  | Option | For | Against |
  | --- | --- | --- |
  | **Keep `f()`** | The universal call spelling (Python, JS, Rust, Go); the name stays attached to its arguments; chains cleanly (`f(x):capture`, `load-env($path)`); parens keep meaning grouping | A *call's* arguments are written two ways depending on mode — `f(a, b)` against `f a b` — which is the split worth questioning (the comma itself is shared with modifiers and glob qualifiers, so it is not unique to calls) |
  | `(f arg)` | Unifies the argument grammar *for calls* — `f arg` runs it, `$(f arg)` takes the bytes, `(f arg)` takes the value, all space-separated — so `--flag` becomes the one option spelling | `(f)` collides with grouping a bare-word string, needing a "bare word at the head of parens is a call" rule; `("f")` becomes the only way to group a literal; unfamiliar outside Lisp/Tcl; **does not remove comma grammar from the language** |

  The bracket is not the questionable part — `f()` earns its keep from the
  string-literal rule alone. What is worth revisiting is the **comma**: `f(a, b)`
  against `f a b` is the only place mesh writes the *same* arguments two ways, and
  `(f arg)` is the coherent endpoint if that ever goes.

  That endpoint is narrower than it first looks. Comma-separated parenthesized
  arguments are not exclusive to value calls — [modifiers](#modifiers) specify
  the same grammar (`:get(EDITOR, vim)`, `:split(":")`), explicitly "like a value
  call," and glob qualifiers follow it. So `(f arg)` would remove the *function-call*
  instance of the comma, not the comma from the language, and it would cost the
  consistency the modifier rule currently borrows from the call syntax — which
  weakens the main argument for making the switch at all.
- **A status-to-bool word — open; leaning `ok`, but much reduced.** *(The
  [status decision](#open-questions) above shrinks this from a gap to a
  convenience: a command-tailed function returns `Status(n)` and a `Status` is a
  condition, so a command-conditioned predicate no longer needs the written-out
  branch. `and` / `or` / `not` admit a `Status` too, so storing, negating and
  combining need no bool either. What is left is wanting a **bool**
  specifically, which is a preference rather than a gap.)* A
  predicate whose condition is a
  *value* collapses to `return $cond`, because
  [value and status are separate channels](#functions) and `return expr` fails only
  on `false`. A predicate whose condition is a **command** has no such spelling and
  must write the branch out:

  ```mesh
  func is-ssh-valid() {
      if quiet ssh-add -L { return true }
      return false
  }
  ```

  What is missing is a word that reads a command's status and answers with the
  **bool**, so the body becomes one line. Three facts constrain the choice, each
  checked against `main` rather than assumed:

  - **`not` over a command yields an int, not a bool** — the inverted status.
    `not command -- /bin/true` results in `1` and `not command -- /bin/false` in
    `0`, so `not not CMD` hands back the original status, still an int, and the
    predicate is unusable as a condition: `if f() { … }` answers *an int is not a
    condition; compare it (`… > 0`), or use `fail` to report a status*. Double
    negation is therefore not a workaround today, and `not`'s own behavior here is
    a defect to fix whichever word wins (it is already flagged
    [for a different reason](#modifiers) — modifiers are considered after
    `not_expression` consumes it).
  - **A bare prefix word does reach the position that matters.** `not CMD` parses
    as a block tail, which is where all of a real config's session predicates end,
    so the candidate needs no new bracket to be useful.
  - **`return` is the exception.** `return not command -- /bin/true` is a syntax
    error, because `return` takes a value expression and a command form is not one.
    Any bare word inherits that limit and would need separate grammar work to reach
    `return`. `ok`, `status` and `bool` are all free names today.

  | Candidate | For | Against |
  | --- | --- | --- |
  | **`ok CMD`** | Inverse of `fail`, the status channel's verb — `fail` writes a failure, `ok` reads one back as a bool. Short, and reads cleanly ahead of a modifier: `ok quiet ssh-add -L`. | Says nothing about the *type* it produces. |
  | `status CMD` | Names the channel it reads; the most literal of the three. | Says "status" while producing a bool, and the word is the obvious one for a future pipeline- or `$sh.status` accessor. |
  | `bool CMD` | Names the result type, echoing the `:bool` cast. | mesh's casts are `:`-prefixed modifiers **on values**; a bare `bool` taking a *command* is a different shape wearing a cast's name. |
  | `is CMD` | Shortest, reads as English. | `is quiet ssh-add -L` scans as "is quiet" — it collides with the modifier following it — and `is` is wanted for type tests and the if-binding. |
  | `not not CMD` | No new name at all. | Does not work (above), and two negations to express a *conversion* reads worse than one word that names it. |
  | `$?(CMD)` | Bracketed sibling of `$(…)` — bytes vs status — and works in `return` position with no grammar change. | Spends a sigil where a word will do, now that `not` shows a prefix word already reaches the block tail. |
- **Flag forwarding into an option-less command — open; leaning keep the
  terminator.** A command that declares no matching option *reports* a flag
  handed to it rather than printing or passing it, so a wrapper that forwards
  arguments which might include a flag has to say `--` in its definition:

  ```mesh
  func show(...rest) { puts -- ...$rest }      # prints; without the `--` it reports
  alias co = puts checkout --                  # `co --force x` → checkout --force x
  ```

  The rule itself is settled — a builtin is not a third kind of command, and
  `puts $x` is genuinely ambiguous between *print this* and *pass this option*.
  What is open is whether that `--` is an acceptable standing cost, or whether
  the forwarding case deserves to work unannotated. Four facts constrain the
  choice, each checked against `main` rather than assumed:

  - **It is 12 of the 22 builtins, not a `puts` quirk.** Of the table in
    `crates/mesh-core/src/builtins.rs`, thirteen declare no option — but
    *twelve* refuse a flag: `cd`, `pwd`, `puts`, `print`, `clip`, `notify`,
    `exit`, `fg`, `bg`, `jobs`, `source`, `help`. Nine own their option
    parsing and decide for themselves: `gets`, `wait`, `disown`, `kill`,
    `prompt`, `on`, `command`, `exec`, `type` — `gets` among them, reporting
    its own unknown flag in wording identical to the general one, so it looks
    from outside like a refusal it does not go through.

    `timeout` is the thirteenth, and it belongs in neither column: its
    operands are a duration and *a command*, so a flag in the first position
    is a bad duration (`timeout --force …`) and one in the second is a
    command name (`timeout 5s --force` → `command not found: --force`). The
    generic refusal never runs for it; a flag *behind* the command reaches
    that command, which is why `timeout 5s puts --force` is refused by
    `puts`. Counting it in would overstate the surface this question is
    about.
  - **A `func` answers identically — but a `wrapper func` does not.**
    `func f(a) { puts $a }` with `f --force` reports `unknown flag`, so this
    is not a rule about builtins that a plain `func` happens to share. The
    exception is the form built for forwarding: `wrapper func f(a) { … }`
    parses no flags, so `f --force` binds `a` and runs the body. That is why
    an alias — which desugars to exactly this — reports at the *target* and
    not at the call: `alias co = puts checkout` lets `--force` through `co`
    and `puts` refuses it. So the language already has a command form that
    doesn't refuse, which is the honest state of the "one rule" claim: two
    of the three forms refuse, and the third exists because forwarding
    needed one that doesn't.
  - **The mark decides, not the spelling.** `x = --force; puts $x` reports;
    `x = "--force"; puts $x` prints; `x = [--force]; puts $x` prints, because
    a flag inside a collection is data one element down.
  - **A spread already carries marks, and forwarding depends on it.**
    `func g(...r) { f ...$r }` with `f` declaring `--tag`: `g -- --tag=x` binds
    `tag=x`, while the same text arriving as a string element
    (`args = ["--tag=x"]; f ...$args`) is data and lands as a positional. So
    the marks surviving a spread is what makes wrapper forwarding work at all
    — it is not an obstacle to it.

  | Candidate | For | Against |
  | --- | --- | --- |
  | **Keep the terminator** | One rule wherever a command parses its own options — the exception, `wrapper func`, is written at the definition and exists to forward, so it is a declared opt-out rather than a hole. The escape is already spelled, already documented, and costs nothing at runtime: the `--` is removed before dispatch, so calls carrying no flag are unaffected. | You learn it by hitting the error: a forwarding definition looks correct until the day a caller passes a flag. |
  | A spread passes flags as data | `puts ...$rest` is the common forwarding shape and would just work, and `[--force]` printing gives "a collection being emptied is data" a precedent. | Breaks fact 4 — `g -- --tag=x` forwarding into a `func` that declares `--tag` binds it *because* the spread keeps the mark. Making a spread mean data would retire the working case to fix the reporting one. |
  | Implicit terminator when the command declares no options — **applied to every command**, `func` included | `puts --force` prints, `func f(a)` takes `f --force` as a positional, the question disappears with no new spelling, and every form then behaves as `wrapper func` already does. | It is the refusal, deleted. Wherever the rule fires today it fires because nothing there can match the flag, which is exactly the set this would silence: `func f(a)` called as `f --frce` reports `unknown flag` now and would bind `a = --frce` instead. That is the guess between *pass this option* and *pass this text* that the rule exists not to make, and the caller who meant an option learns about it downstream rather than at the call. |
  | Implicit terminator for **builtins only** | Smaller blast radius; the option-less builtins are the shapes people actually forward into. And by fact 2 the language already tolerates a non-refusing form — `wrapper func` — so a second one is not the precedent it looks like. | A plain `func` with no such parameter would go on refusing, so the two disagree on identical-looking calls. Weaker than it first reads, given `wrapper func`, but the difference is that `wrapper` is *written* at the definition: you can see which reading you get. A builtins-only rule is invisible at the call. |
  | Diagnostic only | Cheapest, and it decides nothing. The message already names `puts -- --force`; it could name the forwarding spelling too, since a caller who hit this from inside a wrapper needs `puts -- ...$rest` rather than the scalar escape. | Changes how fast the cost is learned, not what it is. And the improvement has to be **unconditional**, which is a weaker message than a targeted one: by the time the refusal runs, a spread-delivered flag is indistinguishable from a written one — `expand::Written` is `Data`/`Flag`/`Terminator` with no provenance, and `Argv` keeps only the words and those marks, so nothing records that an element arrived via `...$rest`. Naming the forwarding form only when it applies would mean carrying a spread-origin bit through expansion, which is real work for a message. |
  | **Spread of an expression** at a command boundary, `...$r:map(func(e) { "$e" })` | The conversion itself already exists, for a list of strings and flags: quoting is the scalar half (`x = --force; puts "$x"` prints `--force`, one of the three ways `docs/REFERENCE.md` lists) and `:map` distributes it — `x = $r:map(func(e) { "$e" }); puts ...$x` prints `a --force b`, leaving plain strings unchanged. What the direct form needs is not a flag feature at all: `CommandItem::Value` had no spread variant, the *same* gap tracked for `puts ...$x:split(":")` and `ls ...glob($p)` — all three gave the identical syntax error. **That gap has since closed on its own**, which spends this candidate's best argument: it no longer pays for itself elsewhere, because the two entries it would have closed as a side effect are closed. What survives is that it is compatible — it accepts programs that were errors and retires nothing — and that `...$r:map(func(e) { "$e" })` now parses, so the spelling is available to judge on its own merits rather than blocked. | Correspondingly not the small change the "just a spelling" reading suggests — it is the general spread-of-expression feature, with a parser change behind it. And for *this* question it is not even a substitute, let alone a shorter one. Quoting each element replaces it with a string, so the forwarded list arrives stripped of every type it carried, and a **collection** does not survive at all: for `s("a", [1 2], "b")`, `puts -- ...$r` renders the list where `$r:map(func(e) { "$e" })` fails with `$e: list value needs \`...\` in command arguments`. So it converts flags at the cost of everything else a rest list can hold, where the terminator preserves all of it — and it is written per call site, and longer than the `--`. Judge it on its own merits now that the two entries it would have closed are closed without it. Not a use of `:flag`, which runs the other way and is the identity on a flag. |

  Leaning: **keep the terminator and improve the diagnostic**, which is one
  candidate plus the null one — and the compatibility argument reaches only
  half the table, so it should be said where it applies rather than as a
  blanket.

  Two of the candidates change what accepted programs do. The refusal reports
  and *skips the body*, so under an implicit terminator a call that errors
  today runs tomorrow: `func f(a) { … }` given `f --frce` never enters `f`,
  where it would bind `a = --frce` and run it — silently, succeeding with a
  value nobody checked. Making a spread mean data is the same shape one step
  over, retiring the forwarding case fact 4 describes. Both are behavior
  changes to existing programs, not additions to them, and neither can be
  undone once written against.

  The other two are compatible and cost nothing to defer, so both stay
  available whatever is decided — which is the actual reason to take the
  diagnostic now and leave the rest open. A better diagnostic changes no
  accepted behavior at all; it just has to say both escapes unconditionally,
  since the refusal cannot see whether the flag came through a spread.
  Spread-of-expression only ever accepts programs that are errors today; it
  is not on the critical path for this question either way, and it should be
  decided on the two entries it closes rather than as an answer here.
- **Exclusion in argument position — open; leaning a `not:` qualifier beside the
  operator.** [Globbing](#globbing) spells exclusion as a spaced infix `-`, and as
  an operator that is right: a glob is [eager](#globbing), so exclusion is list
  difference and needs no glob-specific meaning. Its examples are *statements*, and
  they will work as written once glob-led classification and list difference land —
  both tracked. What the design never spells is exclusion **in front of a command**,
  which is where anyone would actually type it, and [arithmetic](#arithmetic) rules
  the operator out there deliberately: operators between argv words are not
  operators, so `find . -exec grep foo {} +` keeps working and
  `mycmd $file + $other` does not become a type error. So the open question is
  narrow — not which operator, but what `rm` and `ls` get:

  ```mesh
  rm * - *.bak         # today: rm is handed a literal `-` and every .bak right back
  rm ...(* - *.bak)    # what the arithmetic rule allows — and an external needs the spread
  rm *(not: *.bak)     # a qualifier: one word, so it needs neither
  ```

  Six facts constrain the choice, each checked against `main` rather than assumed:

  - **Only the argument-position form is inert; the others report.** As a statement,
    `*.txt - *.bak` says `command not found: a.txt` (the classification gap above),
    and in a value context `x = *.txt - *.bak` says `expected integer` (list
    difference unbuilt) — both loud. Put a command in front and it goes quiet:
    `puts *.txt - *.bak` prints `a.txt b.txt - c.bak d.bak` and
    `/bin/echo *(f) - *.tmp` passes the dash through, so the `.bak` files come back
    rather than being removed. That asymmetry is the whole problem — the one form
    with no diagnostic is the one people type.
  - **The parenthesized form does not reach an external command yet.**
    `/bin/echo (*.txt)` reports ``a list needs `...` to become command arguments``,
    and the spelling it points at, `/bin/echo ...(*.txt)`, is itself a syntax error:
    `CommandItem::Value` had no spread variant, the same gap tracked for
    `ls ...glob($p)` and `puts ...$x:split(":")`. **That gap has since closed** —
    `CommandItem::Value` carries a `spread` marker and all three parse, so
    `/bin/echo ...(*.txt)` runs. What remains here is the *bare* parenthesized form,
    which still reports and still points at the spread. Note what closing the gap
    bought:
    it made `rm ...(* - *.bak)` *parse*, not `rm (* - *.bak)` work — a
    parenthesized list is one list-valued argument either way, so the spread is
    part of this candidate's spelling rather than a temporary workaround. So it is
    not self-contained, and the case people care about is still behind this entry.
    A builtin is unaffected: `puts (*.txt)` takes the list directly.
  - **List difference is unbuilt, so nothing is written against either spelling.**
    `([a b c] - [b])` and `(*.txt - *.bak)` both answer `expected integer`; `-`
    evaluates for integers only. Whatever is decided is a first implementation
    rather than a migration, and it is additive to the operator rather than a
    replacement for it: §Globbing's statement examples stand under every candidate
    here, since none of them touches what `-` means in a value context.
  - **A qualified glob is one word, and one word reaches argv.** `/bin/echo *(d)`
    prints the directory and `*(f)` omits it, in front of an external, with no
    parens and no spread — the type qualifiers are implemented (`TODO.md` said
    otherwise; that entry was stale). The `:`-modifier form does *not* have this
    property: `puts *:f` in the same position is an ordinary glob word that matches
    nothing, and only `(*:f)` filters. Of the predicates, the **boolean** ones are
    built and filter correctly (`*(x)`, `*(f, exec: true)`, and `*(f, empty: false)`
    picking out the one non-empty file); it is the **comparisons** that are not, so
    `*(f, size > 1M)` is a syntax error naming the accepted set. `not` is refused by
    that same message, so a `not:` option would accept text that is an error today
    rather than change what any program means.
  - **`~` mid-word is inert text, so zsh's spelling is available — at a price.**
    `puts x~y` prints `x~y` and `puts a*~/tmp` globs without home-expanding, because
    `+ - * / % ~` are tokens **only with a boundary on each side** (`GRAMMAR.md`
    §Words). Taking `*~*.bak` means carving `~` out of that rule — the rule that
    keeps `a-b` one kebab-case name and `--flag=x` one argument. And `*~` matches
    `foo.txt~` today, so the `rm *~` backup idiom is a working program that the zsh
    reading turns into an exclusion with an empty right-hand side.
  - **Spacing would then decide the result type, silently.** With `f = a.txt`,
    `($f ~ *.bak)` is `false` — a bool from the match operator — and `($f~*.bak)`
    is the empty list. Neither errors. Everywhere else in mesh the unspaced reading
    of an operator character is inert filename text; this would make it a second
    operator with a different type. In argument position the spaced form is not the
    match operator at all: `puts * ~ *.bak` home-expands the bare tilde and prints a
    home path in the middle of the file list.

  | Candidate | For | Against |
  | --- | --- | --- |
  | **A `not:` glob qualifier** — `rm *(not: *.bak)` | Reaches argument position by fact 4, with no new character, no lexer change and no spread. It is the option grammar that already exists, ANDed with the others (`*(f, not: *.tmp)`), and exclusion genuinely *is* a filter, which is what a qualifier names. Additive: `not` is a syntax error today. And it is one of the two **pattern-level** candidates, so it *could* **prune** — `**/*.js(not: **/node_modules)` skipping the subtree instead of walking it and subtracting after, which is the `.gitignore` case and the one place the operator is not merely longer but slower. (The unspaced `~` shares this; the operator forms do not, since `-` is handed two already-expanded lists. See both rows.) | Two spellings for one idea: a list you already hold is not a glob, so `$paths - $skip` still wants the operator, and the qualifier does not apply to it. It would also be the first qualifier that touches no filesystem, softening the "these qualifiers are expansion-only" line — though that section's own frame is "the glob's argument list", and a name is as much a property of a candidate as its size. Leaves a name to pick and a plural form to settle (`not: *.bak\|*.tmp` or `not: [*.bak *.tmp]`). On the name, `not` is the weakest of the three: it is already a live prefix operator — `if not false { … }` — so it would carry two unrelated jobs, negating a command's status and excluding paths from a match. Nothing is ambiguous, the two sitting in different grammar positions, but the reader meets one word meaning two things; `skip:` and `except:` carry no other job. Written up as `not:` throughout below because it is the spelling this was raised under, not because the name is settled. **And the pruning is not free with the qualifier** — `expand_word` takes every result from `glob_matches` and applies qualifiers afterwards with `retain`, which is the same after-the-walk shape the *Fuse `**:files` into the match* entry records for all three existing filter paths. Rejecting a directory before recursing needs a traversal mesh does not have, so pruning is an argument for where this belongs, not a property it arrives with. **And it needs a semantic, not just a faster walk** — two pieces of one, both checked against the matcher. First, the exclusion has to name the subtree *root*: `node_modules ~ **/node_modules/**` is `false` (only descendants match) while `node_modules ~ **/node_modules` is `true`, so a predicate handed the operator's own `**/node_modules/**` keeps the directory and has nothing to prune on. Second — and this is the part that makes it a new evaluation model rather than a filter — **the directory it must reject is not one of the pattern's candidates.** `**/*.js` yields `node_modules/pkg/index.js` and `src/a.js`; the directory `node_modules` never appears, since bare `**` yields directories and `**/*` yields both, but neither is what a `.js` pattern asks for. So the exclusion has to be evaluated against the intermediate directories the *walk visits*, not against the candidates the pattern *produces*, and "a rejected directory takes its subtree with it" has to be defined on top — post-walk it would drop a directory entry, when it appears at all, and leave every file under it. That is `.gitignore`'s rule, and it is the part to cost. |
  | **Parens, operator unchanged** — `rm ...(* - *.bak)` | No new syntax at all, and the [Binary `-`](#arithmetic) argument stands as written: one operator dispatching on operand type. Covers lists and globs with one spelling, and a builtin needs no spread at all — `puts (* - *.bak)` works the moment list difference is built. | The external case needs the **spread**, not just the parens: a parenthesized list is one list-valued argument, so `rm (* - *.bak)` hits ``a list needs `...` to become command arguments`` however much of it is implemented, and closing the `CommandItem::Value` gap (fact 2) makes `rm ...(* - *.bak)` parse rather than making the bare parens work. That is five characters and a nesting level around what zsh writes as one, on something typed interactively — and it is behind another open entry, for *the* case: `rm`, `ls`, `cp`. It also leaves fact 1 standing: `rm * - *.bak` goes on quietly not excluding. |
  | `-` as an operator **between argv words** | The Globbing examples work exactly as written, and it is the spelling a reader already expects from that section. | Reverses the arithmetic decision for one operator and not the rest, so `-` would bind between argv words while `+ * / %` do not. A dash is also the worst character to pick for it: every option starts with one, a lone `-` means stdin to dozens of commands (`cat -`, `diff - file`), and the words either side of it are exactly where option parsing already looks. |
  | zsh's unspaced `~` — `rm *~*.bak` | One word, so it reaches argument position like a qualifier does, and it is muscle memory for zsh users. It can desugar to the same list difference, being a spelling that binds inside a word rather than a second meaning for the operator — but desugaring is a choice, not a limit: because `~` binds *inside the glob word*, the whole exclusion reaches the matcher as one pattern, so this form can prune exactly as the qualifier can, on the same terms (a traversal that rejects a directory before descending, plus testing the exclusion against directories the walk *visits* rather than the candidates the pattern *produces*). The pruning argument therefore does not separate these two candidates; it separates both of them from the operator forms. | Costs the boundary-on-each-side rule, spacing then picks between two operators with different result types and neither spelling errors (facts 5, 6), and `rm *~` stops meaning what it means today. Third job for a character that already carries home expansion and matching; zsh itself keeps it behind `extendedglob`. |
  | `^`-prefixed negation (zsh's `^*.bak`) | Shortest for the whole-pattern case, and unspaced, so it reaches argv too. | Does not compose — `*.js ^node_modules/**` is two words, i.e. an argv operator again in disguise — and `^` is the character [arithmetic](#arithmetic) is already holding for a future bitwise use. ksh's `!(…)` is the same job and is [dropped](#globbing). |
  | **Report the argv-position dash** (compatible with any of the above) | Fact 1 is silent today. A bare `-` between two glob words is almost certainly meant as exclusion, and naming it — with the chosen spelling in the message — turns the wrong answer into a diagnostic. | Decides nothing on its own, and it is **not** free: an unquoted `-` between two globs is accepted and passed through today, so reporting it rejects a program that runs — §Globbing's "quote it as `'-'`" is a convention, not something enforced. The narrowness is therefore load-bearing: too wide and it reports a legitimate stdin operand or separator, too narrow and it misses the case it was written for. |

  Leaning: **a `not:` qualifier for globs, the operator kept for lists, and the
  argv dash reported** — which is one candidate plus the null one, the same shape
  the flag-forwarding question landed on. The duplication objection is the honest
  cost, and it is smaller than it looks: the two forms answer different questions
  (a filter the glob applies to its own candidates, versus a set operation on values
  you already hold), which is the same split `:files` and `(f)` already live with.
  The pruning that makes the first one worth having is a traversal change *and* a
  semantic on top, and should be costed as both — the walk is the same work the
  `**:files` fusion entry describes, and the semantic is that the exclusion is
  tested against the directories the walk *visits* rather than the candidates the
  pattern *produces*, with a rejected directory taking its subtree with it. That is
  `.gitignore`'s rule rather than a filter's, and it is a different evaluation model
  from every qualifier mesh has: `exec:` and `empty:` ask a question about a path
  that is already a result. Neither piece is a side effect of adding an option, and
  the second is the one that decides what the feature *is*.

  **Pruning does not pick between the two pattern-level candidates**, though — the
  unspaced `~` binds inside the glob word, so its exclusion reaches the matcher as
  one pattern too and could prune on exactly those terms. What pruning separates is
  the pattern-level pair from the operator forms, where `-` is handed two lists that
  have already been expanded and there is nothing left to skip. So it argues for
  *where* exclusion belongs, not for which of the two spellings gets it; the case for
  the qualifier over `~` rests on facts 5 and 6 — the lexer rule, `rm *~`, and the
  silent type flip — rather than on this.

  Only two of the six are strictly additive: `not:` accepts text that is a syntax
  error today, and parens accept an operation that is unbuilt. Every other
  candidate takes something back, in descending order of cost. An argv-position `-`
  retires the bare dash operand outright. The unspaced `~` retires `rm *~` and makes
  a dropped space change a result's type. `^` spends a character that is ordinary
  glob text today, so a pattern for a literal `^`-leading name stops meaning that —
  a rare filename rather than a live idiom, but not nothing. And the report,
  narrowest of the four, still rejects a program that runs: the unquoted dash is
  accepted today, so "already quoted per §Globbing" describes the convention and not
  the implementation.

  That last one is worth making on its own merits — fact 1 is a wrong answer nobody
  is told about — but it has to be argued as a behavior change rather than counted
  as free, and its blast radius is whatever the narrowing rule turns out to be. The
  qualifier is the only piece here that costs nothing to take now, which is why it
  leads the leaning; the report is a second decision, and the operator question can
  stay where it is either way.

## Name

**mesh.** No other shell claims the name — the cleanest option on that axis. Two
tradeoffs accepted: the word is heavily overloaded in infra (service mesh, mesh
networking, WiFi mesh), and it sits one letter from `mosh` (mobile shell), an
adjacent tool, so there is a real read-alike / typo risk.

Runner-up: **smash** — distinctive and unconfusable, but with soft collisions
(abandoned toy shells; HPE's unrelated SMASH server-management standard).
Rejected along the way: `lish`, `lsh`, `sish`, `ish`, `bish`, `sash` (all taken
by real or well-known tools).
