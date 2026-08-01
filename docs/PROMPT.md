# A prompt in mesh

> **This is the design target, not what runs today.** The `$sh.prompt` map,
> styled segments, and `rule` below are not implemented yet. What works now is
> the `prompt` builtin and named functions registered with `on` for the `preprompt`,
> `preexec`, `postexec`, `precd`, `postcd`, `jobdone`, and `exit` events — see
> [Custom prompts and hooks](REFERENCE.md#custom-prompts-and-hooks) in the
> reference, which shows the same context line built with today's API.

In mesh your prompt is a **map of named pieces**, one per line — not a single
cryptic `$PS1` string. Each piece is a small function returning text (optionally
with color). Pieces that have nothing to show simply disappear. Here's a complete,
real prompt.

## What it renders

```text
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

```mesh
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

---

# What it takes to not *need* starship

The segment map above is the **layout** question, and it is settled
(`DESIGN.md` §"Hooks and the prompt"). It is not the reason people install
starship. Three other things are, and none of them is a map:

| | starship gives you | mesh today |
| --- | --- | --- |
| **Facts** | ~50 modules that already know the git branch, dirtiness, ahead/behind, stash, language versions, duration | `$sh.status`, `$sh.pipestatus`, `$sh.jobs`, `postexec`'s `elapsed` — and for everything else, a subprocess |
| **Speed** | One binary, modules evaluated in parallel, a per-module timeout | Each segment is mesh code that forks, in sequence, on the critical path |
| **A default** | Looks good with an empty config | `mesh$ ` |

Everything below is about those three. The map is how you *arrange* the answer;
these are how you *have* one.

## 1. Facts, not scrapes

The example at the top of this file is honest about today and quietly bad as an
argument. `"$(git branch --show-current)"` and `"$(git log -1 --format='%h %s')"`
are two forks per prompt to learn two things git already knew, and they are
*text* — a segment that wants "am I ahead of upstream, and by how many" gets to
parse for it.

That sits badly beside the claim three bullets up: **you read real values, not
scraped text.** It is true of `$sh.status` and `$sh.jobs` and false of everything
else, and the gap is exactly where starship earns its place.

So the missing primitive is a **fact map**, computed once per prompt and read by
as many segments as care:

```mesh
$sh.prompt.git = func() {
  if $sh.vcs:len == 0 { return "" }                            # not in a working copy
  dirt   = if $sh.vcs.dirty      { "*" }                 else { "" }
  ahead  = if $sh.vcs.ahead > 0  { "↑${sh.vcs.ahead}" }  else { "" }
  behind = if $sh.vcs.behind > 0 { "↓${sh.vcs.behind}" } else { "" }
  style("${sh.vcs.branch}$dirt$ahead$behind", fg: green)
}
```

(`if` *expressions*, not postfix guards: a guard
[may not follow an assignment](REFERENCE.md#postfix-guards--if-and-unless), which
is exactly the shape a segment wants.)

`branch`, `dirty`, `ahead`, `behind`, `stash`, `state` (merging / rebasing /
bisecting) — the fields every prompt in the world reconstructs by scraping. As a
map they compose with the modifiers the language already has, and a segment that
wants a different presentation writes different mesh rather than a different
parse.

**Where the facts come from is a separate question from what shape they take**,
and it should be decided on measurement rather than taste. Three options, with
their real costs:

- **Shell out to `git`** — 2–4 forks per prompt (~10–40 ms in a small repo, far
  worse in a large one, and worst on a cold cache or a network filesystem). No
  new dependency. This is what today's example does and what most shell configs
  do.
- **A helper binary** — one fork, and the one this author's config already uses
  (`vcs prompt-info`, "one fork" as its comment notes). It keeps mesh out of the
  git-plumbing business and works for hg and jj through the same interface. Cost:
  one process per prompt (~5–15 ms), plus a dependency the shell does not own —
  if it is missing the segment has to degrade, not error.
- **Read the repository in-process** (`gix` / `git2`) — no fork at all, the
  fastest by a wide margin, and the only option that can cache against index
  mtime cheaply. The cost is real and worth stating plainly: `gix` pulls in
  dozens of crates, adds tens of seconds to a clean build and megabytes to the
  binary, and buys a correctness surface — worktrees, submodules, sparse
  checkouts, `.git` files — that mesh would then own. There is no per-use dollar
  cost; the price is build time, binary size, and maintenance.

**Recommendation: the helper or `git`, behind the cached fact map, and native
only if measurement demands it.** The map is the part that matters, because it is
the part segments are written against — the source behind it can change later
without a single segment changing.

## 2. Speed — and the thing starship structurally cannot do

Count the forks in a realistic prompt: hostname, cwd, git branch, git status,
ahead/behind, an `ssh-add -L` for the auth warning. That is five or six
processes, in sequence, between your Enter and your next prompt. The existing
config already fights this by hand — its `prompt_line` captures `auth_info` once
"so `ssh-add -L` only runs one time per prompt", which is a cache written in
shell because the shell offered nowhere to put one.

The map can fix that particular bug — but *only* if the segments are evaluated
once and the result snapshotted, and that is a requirement to write down rather
than a property the map has on its own. reedline calls a `Prompt`'s render
methods on **every repaint**: each edit, each menu movement, a resize, the
submission itself. A `$sh.prompt` implementation that evaluated segments inside
those methods — the tempting design, since rendering is what they are for — would
run `git` and `ssh-add` again on every keystroke, which is worse than the problem
it set out to solve.

Today's prompt already gets this right, and the shape is worth copying exactly:
`MeshPrompt` carries a `custom: Option<String>` cloned from `shell.prompt.text`
*before* `editor.read_line`, and `render_prompt_indicator` only borrows it. The
segment map's version is the same boundary with more behind it — **evaluate the
map, snapshot the rendered lines, then hand the snapshot to the editor.** It also
turns out to be exactly what §2's async repaint needs: an async segment landing
means *replacing the snapshot* and redrawing, which is only coherent because
there is a snapshot to replace.

With that boundary explicit, the map does end the double-running, but it does not
make the work cheaper. Three things do, in increasing order of what they buy:

**Cache the facts, keyed by something that actually changes.** The fact map is
computed once per prompt; better, it is computed once per *directory* and
invalidated by `postcd` — which now exists — plus after any command that could
have changed the repository. A prompt in a directory you have not left and a
command that touched nothing needs no work at all.

Those two triggers are not sufficient, and the gap is worth naming because it is
the one a cache like this always gets wrong. **A repository can change without a
command of yours finishing.** `git fetch &` returns to the prompt before it
updates a single ref; a rebase in another terminal, or an editor's own git
integration, never touches this shell at all. Post-command invalidation fires
too early for the first and never for the rest, so the cache would sit on stale
`ahead`/`behind` values indefinitely while you stayed put. Two more triggers
close it:

- **`jobdone`** — but not yet, and the reason matters. The hook exists, and it
  fires from the `reap` at the *top of the REPL loop*, before `read_line`. So a
  fetch that finishes while you are sitting at the prompt is not noticed until
  you submit the next line, which for a stale cache is barely better than
  invalidating after a command. `TODO.md` already records this for the `[N] Done`
  notice — "a job that ends while a line is being typed is announced only once
  that line is submitted" — and the hook inherits that timing exactly. Making it
  fire *at completion* means waking the line editor on a child's state change,
  which is the same wake §2 needs for the async repaint. **One mechanism, three
  features**: the job notice, async segments, and this.
- **Metadata — for the fields it can answer for.** Nothing in this shell observes
  another terminal's commit, so the cache has to notice by looking. But the map
  does not share one invalidation story, and that split is worth designing in
  rather than discovering:
  - **Ref-derived** — `branch`, `ahead`, `behind`, `state`, `stash` — change only
    when something under `.git/` does, so stat'ing `.git/HEAD`, the index, and
    the refs is exact and cheap.
  - **Worktree-derived** — `dirty` — has no cheap trigger at all. An editor
    writing a tracked file changes that file's mtime and nothing under `.git/`;
    creating an untracked file changes a directory's mtime and nothing under
    `.git/`. Metadata will say "still valid" while `dirty` is simply wrong.
    Watching the worktree means a recursive filesystem watcher over a tree of
    unknown size — inotify limits, network filesystems, a class of failure a
    prompt has no business owning — so the honest answer is an unconditional
    time-to-live for this one field.

  The two properties point the same way, which is the useful part: `dirty` is
  also the *most expensive* fact — a full worktree scan, where the others are a
  couple of ref reads. The field that cannot be invalidated cheaply is the field
  that most wants to arrive late, which makes it the first candidate for an async
  segment rather than an argument against caching the rest.

**Give every fact source a timeout.** starship has `command_timeout` for a
reason: a git command on a dead NFS mount should cost you a missing segment, not
a hung terminal. Any fact source mesh calls needs a bounded wait and a defined
answer when it expires — the same bet the completion probe already makes with its
two-second budget.

**Render asynchronously — and this is the structural argument.** starship is a
one-shot subprocess: it is handed a moment, it prints, it exits. It cannot come
back and revise. mesh *owns the line editor*, so it can draw the prompt
immediately from what is cheap (status, jobs, cwd — all in-process, all free) and
repaint the slow segments when they land. The user starts typing at once, and the
git segment fades in 30 ms later.

That is the thing no external prompt can do and no shell plugin does well —
zsh needs a whole async-worker library to fake it. It is also the answer to "how
do you get starship's information without starship's latency": you stop paying
for the information before the prompt appears.

**The mechanism is not `external_printer`, despite the obvious guess.** That is
reedline's way of printing a *line above* the prompt — which is exactly right for
the background-job notice, and wrong here. Reading the locked 0.49.0: the repaint
is gated on there being a message to print (`if !messages.is_empty()` →
`print_external_message` → `repaint`), and a message of `""` contributes nothing
(`"".lines()` yields no items) and is discarded without repainting. So every
resolved segment would either leave a stray line in your scrollback or not
repaint at all. A prompt repaint needs a **silent wake-and-redraw**, which is a
different thing.

reedline does not expose one — `repaint` is private — but it already *performs*
one, for exactly this shape of problem:

```rust
// reedline 0.49.0, engine.rs — background completions
if completer_pending && self.completer.check_pending() {
    if let Some(menu) = self.menus.iter_mut().find(|m| m.is_active()) {
        menu.update_values(…);
        self.repaint(prompt)?;
    }
}
```

An async producer finishes, the editor notices on its poll, and the display is
redrawn with no line printed. That is the whole requirement, already implemented
for the completer and not generalized. So the ask — upstream, or in a fork — is
narrow and well-shaped: **let something other than the completer say "I have new
material, redraw."**

The same code says something careful about the cost, and the distinction is worth
keeping straight because the two async paths are not treated alike. `needs_polling`
is recomputed every iteration, and the completer's contribution is *conditional* —
`result |= completer_pending` — so that path polls only while a completion is
outstanding and blocks otherwise. The printer's is **not**: `if
self.external_printer.is_some() { result = true }`, so a printer attached for the
life of the editor polls for the life of the editor. The warning already in
`TODO.md` is therefore accurate for the printer exactly as written.

What the completer shows is not that reedline already avoids the cost, but that
it is willing to scope polling to outstanding work where the producer says so.
Attaching and detaching around pending work stays mesh's to implement — the
completer is the precedent for the shape, not an existing implementation of it.

Two rules the repaint needs, neither of them obvious:

- **A repaint must never move the user's cursor or eat a keystroke.** It rewrites
  the prompt region above the input and leaves the buffer alone.
- **A segment that resolves after the command has been submitted is discarded.**
  It is answering a question about a prompt that is now scrollback.

## 3. A default worth keeping

starship's headline is that it looks good with an empty config file. mesh's
current default is `mesh$ `, and every good thing in this document is available
only to someone who sat down and wrote a config.

So the default prompt should *be* the dashboard the requirements ask for — host,
directory, VCS, jobs, status, timing — written in mesh, shipped as the default
`$sh.prompt` map, and printed by `prompt --show` so you can read the thing you
are about to edit. Not a builtin that renders a prompt: **the same map a user
writes**, pre-populated. That is what makes "replace one segment" the first thing
you learn instead of "throw it away and start over."

The two remaining pieces the requirements name are then layout, and the map
already has room for them: **`fill`** for the right-aligned half, and the
**transient prompt** — the collapse-to-one-line rewrite of the previous prompt.

The transient prompt is the cheapest thing in this document, and worth doing
early for that reason alone: reedline already has it. `with_transient_prompt`
takes a second `Prompt`, and `submit_buffer` repaints with it the moment a line
is accepted. It needs none of §2 — there is no async producer and nothing to wake
for, because submission is already an event the editor handles. What it needs
from mesh is a second prompt to hand over, which the segment map makes natural:
the transient form is a map too, and usually a shorter one.

## What this adds up to

In order, because each one makes the next worth having:

1. **The `$sh.prompt` map, `fill` / `rule` / `newline`** — the layout, already
   designed, currently unbuilt. Without it there is one string and no segments.
2. **A fact map** (`$sh.vcs` first), cached per directory and invalidated by
   `postcd`, behind whichever source measures best. This is the one that ends the
   scraping.
3. **The repaint mechanism**, shared with the background-job notice and with
   `jobdone`-at-completion, and with it async segments. *Not* the transient
   prompt, which reedline already provides and which can land any time after (1).
4. **A default map that is the dashboard**, so none of the above needs a config
   file to reach anyone.

Steps 1 and 4 make mesh's prompt *competitive* with starship for a user who is
willing to configure. Steps 2 and 3 are what make it better — not because the
segments are prettier, but because the shell is the only thing in the terminal
that can answer a slow question after it has already drawn the prompt.
