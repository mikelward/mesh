# Worked example: a real prompt in mesh

This ports a real, hand-rolled interactive prompt (from a `config.fish`) to
mesh's prompt model — the `$sh.prompt` segment map, `style(…)` styled values,
the `rule` / `newline` structural segments, and `$sh.jobs`. It's a companion to
the [Hooks and the prompt](../DESIGN.md#hooks-and-the-prompt) and
[Error handling](../DESIGN.md#error-handling) sections of `DESIGN.md`; the syntax
here follows what those sections settle.

## What it renders

```
took 3s
─────────────────────────────────────────────────────────────
host ~/src/mesh [main] SSH
2 unmerged
%1 vim  %2 tail -f log
>
```

The `took …` line, the `unmerged` line, the jobs line, and the `SSH` warning are
all **optional** — each is present only when it has something to say, and its line
is skipped otherwise (see *empty lines are skipped* in the design).

## `rc.mesh`

```
# ── hooks ────────────────────────────────────────────────────────────
# The fish version's maybe_background_fetch hand-gated on "did PWD change?".
# postcd only fires on an actual cd, so the event *is* the gate.
$sh.postcd.fetch   = func() { vcs auto-fetch & }

# postexec hands you the command's duration directly — no preexec timer needed.
$sh.postexec.timer = func(cmd, status, ms) { global _cmd_ms = $ms }

# No log_history: the built-in SQLite history records
# command / cwd / tty / session / start / duration / status for you.

# ── prompt segments (rendered in key order) ──────────────────────────

# fish's postexec `last_job_info`: the previous command's error + how long it took
$sh.prompt.status = func() {
  parts = []
  if $sh.status != 0 { parts += style("status $sh.status" --fg red) }   # nonzero → show it
  if $_cmd_ms > 1000 { parts += style("took $(fmt-duration $_cmd_ms)" --fg yellow) }
  $parts:join " "                                      # "" when empty → line skipped
}
# (Want per-signal wording? `match $sh.status { 130 { "interrupted" } 148 { } _ { … } }`
#  — reach for match only when you branch on specific statuses, not just zero/nonzero.)

$sh.prompt.gap  = newline          # blank line above the rule
$sh.prompt.rule = rule             # full-width ─── (replaces `bar $COLUMNS`)
$sh.prompt.nl1  = newline

# main line — host_info + dir_info + auth_info, as three keyed segments
$sh.prompt.host = func() {
  h    = short-hostname()
  h    = if on-production-host() { style($h --fg red) } else { $h }
  root = if is-root() { style("[root] " --fg red) } else { "" }
  "$root$h $(session-tag)"
}
$sh.prompt.dir  = func() {
  style(if inside-project() { "$(vcs prompt-info)" } else { tilde-pwd() } --fg blue)
}
$sh.prompt.auth = func() { if not ssh-id-loaded() { style("SSH" --fg yellow) } }  # no else → "" → dropped

$sh.prompt.nl2  = newline
$sh.prompt.vcs  = func() { "$(vcs unmerged)" }         # own line; empty → line skipped
$sh.prompt.nl3  = newline
$sh.prompt.jobs = func() {                             # was the `jobs | sed` tab-parser
  $sh.jobs:values:map(func(j) { "%$j.id $j.cmd" }):join "  "
}
$sh.prompt.nl4  = newline

# the prompt character — red when root, doubling as a which-shell-am-I cue
$sh.prompt.char = func() { if is-root() { style("> " --fg red) } else { "> " } }

# ── helpers (the fish helpers that don't become built-ins) ───────────
# Illustrative — string-modifier spellings track DESIGN.md's modifier set.
func is-root()            { $(id -u):raw == "0" }
func ssh-id-loaded()      { ssh-add -L >/dev/null }              # status → bool
func short-hostname()     { $env.HOSTNAME:split "." :first }
func on-production-host() { not on-my-machine() and not on-test-host() }
```

## What falls away vs the fish version

- **`| string collect` — gone everywhere.** No auto-split, so `ssh-add -L`'s status
  is just a bool, `$(id -u):raw` is one string, and an empty `vcs prompt-info`
  doesn't collapse a variable to an empty list. That was dozens of `string collect`s
  in the original.
- **`bar $COLUMNS` → `rule`.** No width loop and no `$COLUMNS` — the structural
  segment is full-width by construction.
- **`job_info`'s tab-split / skip-optional-CPU-column / `sed` parser →
  `$sh.jobs:values:map(…)`.** The careful "don't index past field 1" comment is
  gone; jobs are structured records.
- **History logging, `_session_name` warming, and `my_set_color 'normal'` resets —
  gone.** History is built-in; styled values are scoped per segment, so there's no
  color to reset; session info is a lookup, not a memoized `tmux display-message`
  fork.
- **`maybe_background_fetch`'s PWD-gate → `postcd`.** The hook fires only on a real
  `cd`, so the hand-rolled `_LAST_BG_FETCH_PWD` check vanishes.
- **`auth_info`'s "nothing when fine"** is the decided **no-`else` → `""` → segment
  omitted** rule, directly.
- **The external renderer (`vcs prompt-info`) is one keyed segment (`dir`)** among
  peers — the reason `[root]`, the auth warning, and the session tag compose
  *around* it instead of being swallowed by one big `$PROMPT` string.

## The mechanic that makes the optional lines clean

`status`, `vcs`, and `jobs` are optional whole lines: each returns `""` most of the
time. Because a `newline` structural segment is a **no-op when the current line is
empty**, bracketing those segments with `newline`s costs a line only when the
segment actually produces content — so there are no blank gaps, without the
conditional-`printf` dance the fish version needed.
