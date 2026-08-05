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
decisively, the loop that motivated it is **no longer the silent failure it was**:
a `for` over a value that is not a list is [refused](#loops-for-while-loop) and
names `:lines`. Once the quiet wrong answer is loud, the argument for an implicit
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
  substitution's **raw byte capture** into a list. They *replace* the default
  capture's trim and run against the raw bytes — they never run *after* it. Each
  applies to a `$(…)` capture, producing the list. They apply equally to a
  **plain string value** (`$line:split(":")`, `gets():words`) — there the string's
  own bytes are the input and there is no default split to override; the `$(…)`
  capture is just the most common source. The odd one out is **`:raw`**,
  which lives in the same capture-modifier family but is the *no-split* member:
  it yields the raw bytes as **one string**, not a list (it is what turns the
  default newline-splitting off). So every split modifier produces a list
  *except* `:raw`, whose whole job is to hand back a single byte-string.
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
- **Disambiguation:** `:` is a modifier only when immediately followed by a
  known modifier keyword. `$host:$port` keeps `:` literal (the token after `:`
  is an expansion, not a keyword), so building `host:port`-style strings — or
  any `a:b` construction — is unaffected.

**Split modifiers** (choose the separator). These bind to a substitution's raw
byte capture, replacing the trim that a bare capture would have applied:

```
$(cmd):lines        # split raw bytes on newlines (explicit form of the default)
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
output never becomes a phantom element. This generalizes the default newline
split's trailing trim. **Interior** empty fields are *kept* (`a\0\0b\0` →
`[a "" b]`), so structure in the middle survives; an **empty capture** — or one
that is nothing but delimiters — is the empty list `[]`. `:words` is the
exception that ignores whitespace entirely — leading, trailing, and runs — so it
never yields empty elements (the classic IFS word-split). `:raw` does not split
at all (it is the [no-split capture member](#modifiers), one byte-string).

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

*(TODO — decisions surfaced porting real `PATH` / `find_up` code:*
- ***Transform-vs-predicate overlap.*** Keeping directories is the settled
  `:dirs` / `:d` filter modifier; the open question is only the footgun sitting
  next to it — `:dir` is *dirname* (a transform), so `$paths:filter(:dir)` silently
  keeps **everything** (a dirname is always a truthy string) when `$paths:dirs` (the
  directory **filter** modifier) was meant. Decide whether a transform modifier
  surfacing as a predicate's truthy value should be a **loud error** rather than a
  quiet keep-all.
- ***Upward path walk — `:ancestors` / `:parents`.*** `find_up`, project-root
  detection, and `rootdir` all want `pwd():ancestors` → `[/a/b/c /a/b /a /]`, turning
  a `cd ..`-in-a-subshell loop into a plain list iteration — `pwd()`, the *validated*
  shell-owned cwd, not the possibly-stale `$env.PWD`. Decide the name and whether it
  includes the path itself and the `/` root.)*

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
puts ubuntu:latest    # syntax error: `:latest` is not a modifier; quote the whole
                      # word to keep it as text (`"x:latest"`), or brace the name
                      # when it comes from a variable (`"${x}:latest"`)
```

**Only a bare identifier after the colon is claimed** — the reservation is of the
shape, not of the colon. `key:2`, `key:/path`, `key:`, `http://x` and `a:$b` all keep
the punctuation reading they had, so the break is narrower than "colons are taken".

A name the vocabulary *does* hold but the engine cannot apply yet (`:sort`) parses
and reports at run time, which is a different failure from an unknown one and stays
worded that way.

`modifier_name` (`parser.rs:4562`) tests `MODIFIER_NAMES` only to decide that
fallback. So reserving `:ident` in the grammar does not introduce a new rule; it
makes argument position agree with expression position, which is where the
inconsistency was.

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
(`1 == "1"` is still false). This is a **choice**, not something rendering forces:
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

- **Scope — two levels today, lexical.** There are two *kinds* of variable scope:
  the **session-global** scope (top-level rc and interactive bindings) and a fresh
  **function-local** scope per `func` call. Two is the current **depth**, not a
  cap: the decided [lambda capture](#calling-for-a-value-and-lambdas) rule gives a
  lambda a local scope whose parent is the scope that *defined* it, so a lambda
  called from elsewhere — or outliving that frame — resolves through the captured
  scope before reaching the session, and nested lambdas chain further. Build the
  rung as a **parent link**, not a two-slot lookup. The environment (exported
  names) is a separate axis. Scoping is **lexical**: a function sees its own locals, its
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

  Reading resolves **outward** along that chain (local → any captured defining
  scopes → global) — capture is lexical too, so it reaches the scope that *wrote*
  the lambda, never the one that calls it; an **unbound** name is an
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
  `if c { x = 1 }` then `$x` works and a loop binder is an ordinary binding in
  the enclosing scope (readable after the loop, holding the last value) — a block
  adds **no rung**. Depth comes from `func` calls and, once
  [lambdas capture](#calling-for-a-value-and-lambdas), from a captured defining
  scope; never from a block. **`unset name`** removes the binding **in the
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
  **`$sh.status`** (last exit, int `0`–`255`, the readable replacement for `$?`),
  **`$sh.pipestatus`** (a **list** of the last pipeline's stage statuses, where
  real lists beat bash's `PIPESTATUS`), `$sh.pid` / `$sh.ppid` (own and parent PID,
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
trailing recognized `:` flag modifiers — is a clean `/BODY/`: the closing `/` is the
final character of the base and `BODY` has no unescaped interior `/`. So `/\d+/:i` is
a regex (base `/\d+/`, then `:i`). Every other leading-`/` word is a **path or
glob**:

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

**The live costs.** The `...` requirement is not free, though it costs less than
it once did. When [command substitution](#command-substitution) newline-split by
default, every capture reaching argv owed a spread — `wc -l ...$(ls)` — which was
a large surface for a rule whose stated justification (the separator guess) turns
out not to apply at argv. A capture is now **one string**, so the plain `cd $(…)`
case pays nothing and the cost falls only on captures that are *deliberately*
split: `wc -l ...$(ls):lines`, `grep foo ...$(find . -name '*.rs'):lines`. That is
still the common list-producing idiom, and it still pays a token per use — but the
line already says it wants a list, which is the part that makes the spread read as
redundant.

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
real candidate. A capture becoming one string weakened D's case on its own: the
ergonomic cost it buys back is now confined to captures the author already chose
to split, so A costs less than it did when this was first weighed.

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
any trailing `:` flag modifiers, so `/\d+/:i` qualifies; the closing `/` is the base's
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
any regex value; a **parse-affecting** flag like `:x` cannot, because `re()` is
fail-loud and compiles the *unflagged* pattern first — `re('foo # (')` errors before
a trailing `:x` could make it valid in extended mode. Parse-affecting flags must
therefore be known at construction: folded in pre-compile on a `/…/` literal
(`/foo#(/:x`, compiled once; `#(` is ignored only in extended mode) or passed as a constructor argument
(`re($x, extended: true)`), never as a post-hoc modifier on a finished value.)*

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
- **Value and status are separate channels** *(decided; shipped)*. A function has
  three outputs, not two: the **bytes** it writes to stdout, the **value** it
  returns, and its **exit status**. `return` fills the value channel; `fail`
  fills the status channel. Neither is derived from the other:

  | Form | Value | Status |
  | --- | --- | --- |
  | body ends in a command | none | the command's own |
  | `return $v` | `$v` | `0` — or `1` when `$v` is `false` |
  | `return true` / `return false` | the bool | `0` / `1` |
  | bare `return` | the result so far | the **last** status |
  | `fail` / `fail 123` | `false` | `1` / `123` |

  **Only `false` fails.** `false` is mesh's "no result" — what `gets()` yields at
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
  resemblance to a status is not.)*
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
  as needed, so the record bakes in no split policy), and `.status` (the exit **int**;
  *TODO — a richer status value if one is wanted later*). Read them with ordinary field
  access — `r = f(x):capture` binds `r`, then `$r.value` / `$r.out:lines` read it. It is an
  *invocation-level* modifier, not a plain value [modifier](#modifiers) — it has to
  wrap execution, since by the time a value modifier saw the return value the stdout
  would already have streamed away, the same reason `$(…)` is a wrapper rather than a
  postfix. The **same `cmd(…):capture` spelling works on an external** — and is the
  single exception to the value-call error below: a bare `grep(foo)` errors because it
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
`func`, or external — but not everything it resolves *returns* something. **Externals
have no return value** (above), and neither do the **effect-only builtins**: `r =
puts(1 + 2)` already reports `a command has no return value`, so `&puts` is no more
usable in a value slot than `&grep` is. What divides the callables is therefore not
builtin-versus-external but **returns a value versus runs for effect**.

A reference to an effect-only callable is fine in a slot that calls its handler for
**effect** (a `$sh.preprompt` entry, a `$sh.signal.<NAME>` handler) and fails *when
called* in a slot that needs a **value** (`:map`, a prompt segment that must return a
piece). That failure is about the call producing nothing to use, not about the
reference being ill-formed, and it lands at call time for the same reason the
bare-`grep(foo)` error does.

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

*Not taken (yet): widening `:name`.* `:upper` is already a one-argument function
reference in value position, so extending it to user functions would need no new sigil
at all. That is a real option, but it is the [user-defined modifier
question](#open-questions), not a spelling choice: it merges the modifier vocabulary
with the function namespace and gives up the parse-time unknown-modifier error. The
line kept here is by **shape**, not by who wrote the name — `:name` is the
argument-free, auto-mapping modifier form; `&name` is the general reference, any arity,
any slot — so a reader can predict which applies without knowing whether a name shipped
with the shell.

**A lambda closes over the scope that created it** *(decided — a change from what runs
today)*. The body's scope parent is currently the *session*, so a lambda sees session
and global bindings but not the function-locals beside it, even when it is called
immediately, in the same scope:

```
func f() { _n = 41
  _g = func() { puts $_n }
  $_g() }                       # today: `_n: unbound variable`
```

That makes lambdas and [`_`-prefixed locals](#variables-and-assignment) mutually
unusable in exactly the place a lambda earns its keep — `$xs:filter(func(_p) { $_p:ext
== $_want })` cannot reach the `_want` bound on the line above it. So a lambda captures
its defining scope.

This is mesh's **first closure**, which is a commitment rather than a scope tweak: a
lambda that outlives its defining frame needs that frame's locals to outlive the call
too — as live bindings, or as a snapshot taken at capture, which is the sub-question
below — so either way locals stop being a pure stack discipline. It also retires a justification used elsewhere — the
decision that a flag's value is captured at assignment rejected the late alternative as
"a closure in disguise, which mesh has nothing else like." mesh now has one, so that
decision needs its own reasoning; it stands on the simpler ground that a *value* should
not carry unevaluated work, but the supporting argument is gone and should not be cited
again.

*Open — capture by binding or by value.* Reading a **session** variable
from a lambda is late today (`x = 1; g = func() { puts $x }; x = 2; $g()` prints `2`),
and capturing the **binding** rather than a snapshot is what keeps a captured local
consistent with that. One question follows and is not answered here: whether a
captured local is *writable* through the lambda, which is the same answer under a
different name.

*Not open — shadowing a captured local.* An earlier revision listed this as a second
sub-question, on the grounds that the [no-shadow rule](#variables-and-assignment) was
stated over locals versus outer *session* bindings and said nothing about two nested
locals. That misread it: the rule is *"no shadowing, **at any rung**"* — one name, one
rung at a time — and its "not local over session, not session over environment" is an
enumeration of the rungs that existed, not the extent of the ban. A captured defining
scope is a rung, so a lambda parameter may not shadow a captured local, for exactly the
reason a local may not shadow a session binding. Nothing here argues for a carve-out,
and one would need its own justification.

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
  true), preserving the `if grep -q foo file { … }` reflex. Every other type is a
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
  command / nonzero int fails it) **and** its shape **fits** `lhs`; on true the
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
| `_` | anything | the default; put it last |

Rules:

- **First match wins**, top to bottom; `_` is the catch-all and conventionally
  last. Whether non-`_`-exhaustive matches must be total is *(open)* — leaning
  lenient (a `match` with no arm hit yields `""`, like a no-`else` `if`).
- **It is an expression**: `x = match … { … }` binds the winning arm's value;
  in statement position the value is discarded and arms run for effect.
- **A literal arm compares totally, even where `==` refuses.** An arm is
  dispatch machinery, like `:dedup` and list `-`: under first-match traversal
  it needs an answer for every pair, so it uses the total equality those
  share rather than the `==` operator's refusals. Today the two agree
  everywhere; the decided-but-unbuilt `Flag` type (`TODO.md`) is the first
  divergence — `$x == "--help"` will refuse on a flag, while a `match` with
  both a `--help` arm and a `"--help"` arm keeps working and takes the right
  one, since naming both arms is someone deliberately telling them apart.
  Stated here so the `(==)` in the table below is not read as importing the
  refusal.
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

**Explored, kept the settled model — `0` = success is correct** *(not a change)*. The
exploration questioned `int → status` — a bare int read as an exit code rather than data,
its truthiness following the status view, not the number. Resolution: **keep it.**
External commands exit `0` for success with no typed value to consult, so for `if X { }`
to mean "did X succeed" whether `X` is `grep -q …` or a mesh function, a function's
`0`/success must be truthy too — that interchangeability is the point, and it just works.
The residual (an int whose masked status is nonzero can't be returned as successful data)
is narrow and accepted. Two live scraps this left, both pointed at their canonical homes:

- **Empty `""` / `[]` truthiness** — **closed** by
  [condition truthiness](#conditionals-if-is-an-expression) settling as *no truthy
  values*: a bare `if $xs` is an error whether the list is empty or not, so there
  is no emptiness rule left to decide. The question survives only for the
  **assignment-condition RHS** (`if xs = f() { … }`), which tests *presence* rather
  than truth — and there the answer follows from `false` being mesh's "no result":
  only `false` is absent, so `""`, `[]` and `0` all bind and take the branch. That
  also keeps `gets()`'s pinned contract, where a blank line must not end a read
  loop.
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

### Error handling

mesh keeps **two distinct failure channels** and deliberately does not merge them
the way bash does (into "empty string, exit 1"):

- **Value-level failure** — a `false`, a nonzero `int`, or a command's exit
  status. This is *not* an interruption: it is a **value** you branch on (`if`,
  `while`, `&&` / `||`, `and` / `or` / `not`). It is the whole of the
  [result/status model](#functions) — failure here is signalled by a `false` /
  nonzero-int / command-status, **never** by the *shape* of a value.
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
[no-null](#variables-and-assignment), so a no-`else` `if` is the one place mesh
hands you a silent empty that a destructure would refuse. That is the accepted cost
of the terse one-liner ([Conditionals](#conditionals-if-is-an-expression),
"Decided: lenient"); the only lever to close it — requiring `else` in *binding*
position — was weighed and declined for ergonomics.

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
rather than answering `false` *(decided, not yet built — the `Flag` type entry
in `TODO.md`)*: the string was written *because* someone believed it was the
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

A **`jobdone` hook** fires alongside that notice, once per finished job, taking
`id`, `command`, and `status` — see `docs/REFERENCE.md`. It runs where the notice
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
(after it finishes, given the command, its **exit status**, and **duration**),
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
inside a `trap … EXIT`. A `fork { … }` subshell leaving is *not* the session
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
  the loop ran in a forked subshell, so `n` never escaped. mesh's **settled** answer is to not pipe into a loop at all:
  [split the capture](#command-substitution) and iterate the list *in the current
  scope* — `for line in $(cmd):lines { n += 1 }` leaves `n` set, no subshell
  involved. The split is spelled rather than implied, which is the other half of
  the defense: bash's alternative to the pipe, `for x in $(cmd)`, re-splits each
  line on `IFS` and globs it, so the escape from the subshell reintroduces the
  word-splitting bug. ***(planned)*** for the literal `cmd | while gets line { … }` form to
  persist too, the **last stage of a `|` pipeline** would run in the current shell
  rather than a forked subshell — bash's opt-in `lastpipe`, intended as mesh's
  unconditional default; not yet written into [Redirection](#redirection).
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
  capture** rather than a post-hoc rescue: `$(cmd)` is a list (default newline
  split, opt-in `:words` / `:nulls` / `:tabs` / `:split`, a defined
  [trailing-empty-field rule](#modifiers)), and `"$(cmd)"` is one string — quoting,
  not a rescue pipeline, is how you ask for a scalar, with `$(cmd):raw` the
  variant that also keeps the trailing newline. You ask for the shape you want up
  front, so a value is never auto-split against your intent and then un-split with
  `string collect`. The empty cases are each clean and stated
  ([Modifiers](#modifiers)): an empty list capture is `[]`, and an empty scalar
  capture is `""` — [no null](#variables-and-assignment) either way, so neither
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
- **Exclusion `~` alias** — resolved by elimination: `~` / `!~` is now the
  **pattern-match** operator ([Tests and comparisons](#tests-and-comparisons)),
  so glob exclusion keeps the spaced infix `-` only.
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
  must be **exhaustive** (leaning lenient → `""`); and the **`~` scope** lever (keep it
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
  `x = \up` already binds `"up"`). **Remaining:** whether `:name` should widen to
  user-defined functions, which is the *user-defined modifier* question below, not
  a second spelling — the line held for now is by **shape** (`:name` argument-free
  and auto-mapping, `&name` general) rather than by who wrote the name.
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
- **User-defined modifiers — open.** `:ident` is reserved by the grammar, so the
  ambiguity is already paid for and a user modifier is *possible*; the vocabulary
  is otherwise closed forever. The cost is that `:name` moves from **parse**-time
  to **run**-time resolution, so a typo'd modifier stops being a syntax error. That
  trade is the whole decision.
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

## Name

**mesh.** No other shell claims the name — the cleanest option on that axis. Two
tradeoffs accepted: the word is heavily overloaded in infra (service mesh, mesh
networking, WiFi mesh), and it sits one letter from `mosh` (mobile shell), an
adjacent tool, so there is a real read-alike / typo risk.

Runner-up: **smash** — distinctive and unconfusable, but with soft collisions
(abandoned toy shells; HPE's unrelated SMASH server-management standard).
Rejected along the way: `lish`, `lsh`, `sish`, `ish`, `bish`, `sash` (all taken
by real or well-known tools).
