# Worked example: a real prompt in mesh

This ports a real, hand-rolled interactive prompt (from a `config.fish`) to
mesh's prompt model — the `$sh.prompt` map (each top-level entry is a **line**),
`style(…)` styled values, the `rule` / `newline` structural entries, keyed inline
**groups**, and `$sh.jobs`. It's a companion to the
[Hooks and the prompt](../DESIGN.md#hooks-and-the-prompt) and
[Error handling](../DESIGN.md#error-handling) sections of `DESIGN.md`; the syntax
here follows what those sections settle.

## What it renders

```
took 3s
─────────────────────────────────────────────────────────────
host ~/src/mesh [main] SSH
9f3c2a1 Pull fill into the MVP
%1 vim  %2 tail -f log
>
```

The `took …` line, the commit line, the jobs line, and the `SSH` warning are
all **optional** — each is present only when it has something to say, and its line
is skipped otherwise (an entry that renders `""` contributes no line).

## `rc.mesh`

```
# ── hooks ────────────────────────────────────────────────────────────
# The fish version's maybe_background_fetch hand-gated on "did PWD change?".
# postcd only fires on an actual cd, so the event *is* the gate.
$sh.postcd.fetch   = func() { vcs auto-fetch & }

# Time each command. Initialize _cmd_ms so the first prompt — rendered before any
# command has run, so before postexec fires — reads a bound value, not an error.
global _cmd_ms = 0
$sh.postexec.timer = func(cmd, status, ms) { global _cmd_ms = $ms }

# No log_history: the built-in SQLite history records
# command / cwd / tty / session / start / duration / status for you.

# ── prompt: each top-level entry is one line ─────────────────────────

# fish's postexec `last_job_info`: the previous command's error + how long it took
$sh.prompt.status = func() {
  parts = []
  if $sh.status != 0 { parts += style("status ${sh.status}" --fg red) }   # nonzero → show it
  if $_cmd_ms > 1000 {
    d = fmt-duration($_cmd_ms)                        # value call — its return value, not its stdout
    parts += style("took $d" --fg yellow)
  }
  $parts:join " "                                    # "" when empty → line skipped
}
# (Want per-signal wording? `match $sh.status { 130 { "interrupted" } 148 { } _ { … } }`
#  — reach for match only when you branch on specific statuses, not just zero/nonzero.)

$sh.prompt.gap  = newline          # a deliberate blank line
$sh.prompt.rule = rule             # full-width ─── (replaces `bar $COLUMNS`)

# The header — one line, built from keyed inline segments so each keeps its own
# color (concatenating styled values into a single string would flatten them). The
# renderer space-joins the pieces (empties dropped, puts-style), so no piece carries
# its own spacing.
$sh.prompt.head = [                            # a MAP literal — [ ], not { } (braces are blocks)
  root: func() { if is-root() { style("[root]" --fg red) } },               # "" when not root
  host: func() {
    h = short-hostname()
    if on-production-host() { style($h --fg red) } else { $h }
  },
  sess: func() { session-tag() },                                            # value call — the styled session tag
  dir:  func() {
    if inside-project() { "$(vcs prompt-info)" }     # external renderer — returned RAW (own color, own newlines)
    else                { style(tilde-pwd() --fg blue) }   # mesh fallback — we color it
  },
  auth: func() { if not ssh-id-loaded() { style("SSH" --fg yellow) } },      # no else → "" → dropped
]

$sh.prompt.commit = func() {                         # own line: short SHA + commit subject; empty outside a repo → skipped
  if inside-project() { "$(git log -1 --format='%h %s')" }
}
$sh.prompt.jobs = func() {                           # was the `jobs | sed` tab-parser
  $sh.jobs:values:map(func(j) { "%${j.id} ${j.cmd}" }):join "  "
}

# the prompt character — red when root, doubling as a which-shell-am-I cue
$sh.prompt.char = func() { if is-root() { style("> " --fg red) } else { "> " } }

# ── helpers (the fish helpers that don't become built-ins) ───────────
# Illustrative — string-modifier spellings track DESIGN.md's modifier set.
func is-root()            { $(id -u):raw == "0" }
func ssh-id-loaded()      { ssh-add -L >/dev/null }              # status → bool
func short-hostname()     { $env.HOSTNAME:split "." :first }
func on-production-host() { not on-my-machine() and not on-test-host() }
```

## Variations with `fill`

`fill` is the inline right-align / trailing-bar piece. A couple of common tweaks:

```
# a bar on the SAME line as the header, instead of a separate rule line:
$sh.prompt.head = [host-info dir-info auth-info fill("─")]   # host dir auth───────────  to the right edge

# a clock pinned to the right edge of the header:
$sh.prompt.head = [host-info dir-info fill clock-info]        # host dir …………………… 14:23
```

`fill` eats the slack (spaces by default, or the repeat-char you give it); multiple
`fill`s on a line split the slack evenly. The full-width `rule` entry above is just
the whole-line case — `rule ≡ a line that is [fill("─")]`.

## What falls away vs the fish version

- **`| string collect` — gone everywhere.** No auto-split, so `ssh-add -L`'s status
  is just a bool, `$(id -u):raw` is one string, and an empty `vcs prompt-info`
  doesn't collapse a variable to an empty list. That was dozens of `string collect`s
  in the original.
- **`bar $COLUMNS` → `rule`.** No width loop and no `$COLUMNS` — the structural
  entry is full-width by construction.
- **The `nl1`/`nl2` separator keys are gone.** Each top-level entry is a line; the
  only structural entries (`gap`, `rule`) carry meaningful names.
- **`job_info`'s tab-split / skip-optional-CPU-column / `sed` parser →
  `$sh.jobs:values:map(…)`.** The "don't index past field 1" comment is gone; jobs
  are structured records.
- **History logging, `_session_name` warming, and `my_set_color 'normal'` resets —
  gone.** History is built-in; styled values are scoped per segment, so there's no
  color to reset; session info is a lookup, not a memoized `tmux display-message`
  fork.
- **`maybe_background_fetch`'s PWD-gate → `postcd`.** The hook fires only on a real
  `cd`, so the hand-rolled `_LAST_BG_FETCH_PWD` check vanishes.
- **`auth_info`'s "nothing when fine"** is the decided **no-`else` → `""` → segment
  omitted** rule, directly.
- **The external renderer (`vcs prompt-info`) is one inline segment (`dir`)** among
  peers — returned **raw** so its own coloring (and any newlines) survive; the
  mesh `tilde-pwd` fallback is the branch we `style`. Passing external output
  through `style(…)` would re-import it as an ordinary mesh string (single line), so
  a multi-line external renderer is returned unwrapped.

## The mechanic that makes the optional lines clean

`status`, `commit`, and the `jobs` line are optional: each returns `""` most of the
time, and an entry that renders `""` **contributes no line** — so there are no
blank gaps and nothing to suppress, without the conditional-`printf` dance the fish
version needed. Note `${sh.status}` / `${j.id}` / `${j.cmd}` use **braced**
interpolation: an unbraced `$sh.status` in a string would stop at `$sh` and append
literal `.status`, so member access inside a string is always braced.
