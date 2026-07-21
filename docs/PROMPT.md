# A prompt in mesh

In mesh your prompt is a **map of named pieces**, one per line — not a single
cryptic `$PS1` string. Each piece is a small function returning text (optionally
with color). Pieces that have nothing to show simply disappear. Here's a complete,
real prompt.

## What it renders

```
took 3s
──────────────────────────────────────────────────────────────
mikel@host ~/src/mesh main
9f3c2a1 Initial commit
%1 vim  %2 tail -f log
❯
```

The `took …`, commit, jobs, and status lines are each shown only when they have
something to say.

## `rc.mesh`

```
# Record each command's run time — the shell measures it and hands a `Duration`
# to postexec; we just stash it so the prompt can say "took 3s". Side effects like
# this live in hooks; the prompt segments below stay pure renderers.
global _cmd_time = 0s
$sh.postexec.record-time = func(cmd, status, elapsed) { global _cmd_time = $elapsed }

# The prompt is a map — one entry per line, rendered top to bottom in the order
# you add them (maps preserve insertion order).

# how the last command went: an error in red, or a slow time in yellow (else nothing)
$sh.prompt.status = func() {
  if $sh.status != 0      { style("✗ ${sh.status}", fg: red) }
  else if $_cmd_time > 1s { style("took $_cmd_time", fg: yellow) }
}

$sh.prompt.rule = rule                                   # a full-width rule

# where you are — one line: user@host, the path in blue, the git branch in green
$sh.prompt.head = [
  who:  func() { h = $env.HOSTNAME:split("."):first; "${env.USER}@$h" },
  path: func() { style(pwd(), fg: blue) },
  git:  func() { style("$(git branch --show-current)", fg: green) },   # empty off a repo → hidden
]

# the current commit: short hash + subject line (nothing outside a repo)
$sh.prompt.commit = func() { "$(git log -1 --format='%h %s' 2>/dev/null)" }

# background jobs, read straight from the live job table
$sh.prompt.jobs = func() { $sh.jobs:values:map(func(job) { "%${job.id} ${job.cmd}" }):join("  ") }

$sh.prompt.char = func() { "❯ " }
```

## Why this is nice

- **Your prompt is named pieces, not one big string.** Restyle one, reorder them,
  or drop one — `unset $sh.prompt.commit` — without touching the rest. Re-sourcing
  your config replaces pieces by name instead of duplicating them.
- **Color is data, not escape codes.** `style("main", fg: green)` — no
  `\e[32m…\e[0m` to hand-balance. The shell knows the real text width, and can even
  recolor a piece later.
- **Empty pieces vanish.** A piece with nothing to show returns `""` and its whole
  line disappears — the branch off a repo, the timer after a fast command, the error
  after a success — with no `if`-guards wrapped around your layout.
- **You read real values, not scraped text.** `$sh.status` is the last exit code,
  `$sh.jobs` is the live job table, and `postexec` hands you a command's runtime —
  so the jobs line is `$sh.jobs:values:map(…)`, never a parse of `jobs` output.
- **Side effects stay in hooks.** Timing here — or a background `git fetch` on `cd`
  — lives in `postexec` / `postcd`, keeping every segment a pure, predictable
  renderer.
- **Drop-in external prompts are just one piece.** A tool like starship sits among
  your own segments, framed by your `[root]` and git bits — not a black box that
  owns the whole line.
